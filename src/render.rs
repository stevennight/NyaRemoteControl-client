//! Presentation: flip-model swap chain, YUV→RGB shaders and letterboxing.
//! The UI is painted between [`Renderer::draw_video`] and [`Renderer::present`].

use anyhow::{anyhow, Result};
use windows::core::Interface;
use windows::Win32::Foundation::{BOOL, HWND};
use windows::Win32::Graphics::Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

use crate::video::{Slot, SlotKind};
use nya_media::decoder::Matrix;
use nya_win::d3d::{compile_shader, D3dDevice};

const HLSL: &str = include_str!("shaders/render.hlsl");

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Cb {
    dst: [f32; 4],
    m0: [f32; 4],
    m1: [f32; 4],
    m2: [f32; 4],
    off: [f32; 4],
    scale: [f32; 4],
}

/// Rectangle in window pixels.
#[derive(Debug, Clone, Copy, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Fit `vw`×`vh` into the window keeping the aspect ratio.
pub fn fit(win_w: u32, win_h: u32, vw: u32, vh: u32) -> Rect {
    if vw == 0 || vh == 0 || win_w == 0 || win_h == 0 {
        return Rect { x: 0.0, y: 0.0, w: win_w as f64, h: win_h as f64 };
    }
    let s = (win_w as f64 / vw as f64).min(win_h as f64 / vh as f64);
    let (w, h) = ((vw as f64 * s).round(), (vh as f64 * s).round());
    Rect { x: ((win_w as f64 - w) / 2.0).floor(), y: ((win_h as f64 - h) / 2.0).floor(), w, h }
}

fn yuv_params(matrix: Matrix, full_range: bool) -> ([f32; 4], [f32; 4], [f32; 4], [f32; 4], [f32; 4]) {
    let (m0, m1, m2) = match matrix {
        Matrix::Bt709 => ([1.0, 0.0, 1.5748, 0.0], [1.0, -0.187324, -0.468124, 0.0], [1.0, 1.8556, 0.0, 0.0]),
        Matrix::Bt601 => ([1.0, 0.0, 1.402, 0.0], [1.0, -0.344136, -0.714136, 0.0], [1.0, 1.772, 0.0, 0.0]),
    };
    let (off, scale) = if full_range {
        ([0.0, 128.0 / 255.0, 128.0 / 255.0, 0.0], [1.0, 1.0, 1.0, 0.0])
    } else {
        ([16.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, 0.0], [255.0 / 219.0, 255.0 / 224.0, 255.0 / 224.0, 0.0])
    };
    (m0, m1, m2, off, scale)
}

pub struct Renderer {
    pub dev: D3dDevice,
    swap: IDXGISwapChain1,
    rtv: Option<ID3D11RenderTargetView>,
    pub width: u32,
    pub height: u32,
    vs: ID3D11VertexShader,
    ps_nv12: ID3D11PixelShader,
    ps_ayuv: ID3D11PixelShader,
    ps_planar: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    cbuf: ID3D11Buffer,
    tearing_supported: bool,
}

impl Renderer {
    pub fn new(dev: D3dDevice, hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        let d = &dev.device;
        unsafe {
            let dxgi_dev: IDXGIDevice1 = d.cast()?;
            let _ = dxgi_dev.SetMaximumFrameLatency(1);
            let adapter = dxgi_dev.GetAdapter()?;
            let factory: IDXGIFactory2 = adapter.GetParent()?;
            let tearing_supported = factory
                .cast::<IDXGIFactory5>()
                .map(|f5| {
                    let mut allow = BOOL(0);
                    f5.CheckFeatureSupport(
                        DXGI_FEATURE_PRESENT_ALLOW_TEARING,
                        &mut allow as *mut _ as *mut _,
                        std::mem::size_of::<BOOL>() as u32,
                    )
                    .is_ok()
                        && allow.as_bool()
                })
                .unwrap_or(false);
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width.max(1),
                Height: height.max(1),
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_UNSPECIFIED,
                Flags: if tearing_supported { DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32 } else { 0 },
                ..Default::default()
            };
            let swap = factory.CreateSwapChainForHwnd(d, hwnd, &desc, None, None)?;
            let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);

            let vs_code = compile_shader(HLSL, "vs_quad", "vs_4_0")?;
            let mut vs = None;
            d.CreateVertexShader(&vs_code, None, Some(&mut vs))?;
            let ps = |e: &str| -> Result<ID3D11PixelShader> {
                let code = compile_shader(HLSL, e, "ps_4_0")?;
                let mut p = None;
                d.CreatePixelShader(&code, None, Some(&mut p))?;
                p.ok_or_else(|| anyhow!("pixel shader {e}"))
            };
            let sd = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            let mut sampler = None;
            d.CreateSamplerState(&sd, Some(&mut sampler))?;
            let bd = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of::<Cb>() as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            let mut cbuf = None;
            d.CreateBuffer(&bd, None, Some(&mut cbuf))?;

            Ok(Self {
                swap,
                rtv: None,
                width: width.max(1),
                height: height.max(1),
                vs: vs.unwrap(),
                ps_nv12: ps("ps_nv12")?,
                ps_ayuv: ps("ps_ayuv")?,
                ps_planar: ps("ps_planar")?,
                sampler: sampler.unwrap(),
                cbuf: cbuf.unwrap(),
                tearing_supported,
                dev,
            })
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == (self.width, self.height) {
            return Ok(());
        }
        self.rtv = None;
        unsafe {
            self.dev.context.OMSetRenderTargets(None, None);
            self.dev.context.Flush();
            let flags = if self.tearing_supported { DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING } else { DXGI_SWAP_CHAIN_FLAG(0) };
            self.swap.ResizeBuffers(0, width, height, DXGI_FORMAT_UNKNOWN, flags)?;
        }
        self.width = width;
        self.height = height;
        Ok(())
    }

    fn ensure_rtv(&mut self) -> Result<ID3D11RenderTargetView> {
        if let Some(r) = &self.rtv {
            return Ok(r.clone());
        }
        unsafe {
            let back: ID3D11Texture2D = self.swap.GetBuffer(0)?;
            let mut rtv = None;
            self.dev.device.CreateRenderTargetView(&back, None, Some(&mut rtv))?;
            let rtv = rtv.ok_or_else(|| anyhow!("CreateRenderTargetView"))?;
            self.rtv = Some(rtv.clone());
            Ok(rtv)
        }
    }

    fn ndc(&self, r: Rect) -> [f32; 4] {
        let (w, h) = (self.width as f64, self.height as f64);
        [
            (r.x / w * 2.0 - 1.0) as f32,
            (1.0 - r.y / h * 2.0) as f32,
            ((r.x + r.w) / w * 2.0 - 1.0) as f32,
            (1.0 - (r.y + r.h) / h * 2.0) as f32,
        ]
    }

    /// Bind and clear the back buffer; returns its view for further drawing (UI).
    pub fn begin(&mut self) -> Result<ID3D11RenderTargetView> {
        let rtv = self.ensure_rtv()?;
        unsafe {
            self.dev.context.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            self.dev.context.ClearRenderTargetView(&rtv, &[0.0, 0.0, 0.0, 1.0]);
        }
        Ok(rtv)
    }

    /// Draw a decoded frame letterboxed into the window.
    pub fn draw_video(&mut self, s: &Slot) {
        let ctx = self.dev.context.clone();
        let r = fit(self.width, self.height, s.width, s.height);
        let (m0, m1, m2, off, scale) = yuv_params(s.matrix, s.full_range);
        let cb = Cb { dst: self.ndc(r), m0, m1, m2, off, scale };
        let ps = match s.kind {
            SlotKind::Nv12 => &self.ps_nv12,
            SlotKind::Ayuv => &self.ps_ayuv,
            SlotKind::Planar => &self.ps_planar,
        };
        let views: Vec<Option<ID3D11ShaderResourceView>> = s.srvs.iter().cloned().map(Some).collect();
        unsafe {
            let vp = D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: self.width as f32,
                Height: self.height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            ctx.RSSetViewports(Some(&[vp]));
            ctx.RSSetState(None);
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            ctx.VSSetShader(&self.vs, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(self.cbuf.clone())]));
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cbuf.clone())]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.OMSetBlendState(None, None, 0xffff_ffff);
            ctx.UpdateSubresource(&self.cbuf, 0, None, &cb as *const _ as *const _, 0, 0);
            ctx.PSSetShader(ps, None);
            ctx.PSSetShaderResources(0, Some(&views));
            ctx.Draw(4, 0);
            ctx.PSSetShaderResources(0, Some(&[None, None, None]));
        }
    }

    pub fn present(&mut self, allow_tearing: bool) -> Result<()> {
        let tear = allow_tearing && self.tearing_supported;
        let hr = unsafe { self.swap.Present(0, if tear { DXGI_PRESENT_ALLOW_TEARING } else { DXGI_PRESENT(0) }) };
        if hr.is_err() {
            return Err(anyhow!("Present: {hr:?}"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_letterboxes() {
        let r = fit(1920, 1200, 1920, 1080);
        assert_eq!((r.x, r.y, r.w, r.h), (0.0, 60.0, 1920.0, 1080.0));
        let r = fit(1000, 1000, 2000, 1000);
        assert_eq!((r.w, r.h), (1000.0, 500.0));
    }

    #[test]
    fn shaders_compile() {
        for (e, t) in [("vs_quad", "vs_4_0"), ("ps_nv12", "ps_4_0"), ("ps_ayuv", "ps_4_0"), ("ps_planar", "ps_4_0")] {
            compile_shader(HLSL, e, t).unwrap();
        }
    }

    /// Limited-range BT.709 grey and white map to the expected RGB.
    #[test]
    fn yuv_matrix() {
        let (m0, m1, m2, off, scale) = yuv_params(Matrix::Bt709, false);
        let conv = |y: f32, u: f32, v: f32| {
            let yuv = [(y - off[0]) * scale[0], (u - off[1]) * scale[1], (v - off[2]) * scale[2]];
            let d = |m: [f32; 4]| m[0] * yuv[0] + m[1] * yuv[1] + m[2] * yuv[2];
            [d(m0), d(m1), d(m2)]
        };
        let white = conv(235.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0);
        for c in white {
            assert!((c - 1.0).abs() < 1e-3);
        }
        let black = conv(16.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0);
        for c in black {
            assert!(c.abs() < 1e-3);
        }
    }
}
