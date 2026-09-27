//! Statistics overlay rendered with GDI into a BGRA bitmap.

use windows::core::w;
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Graphics::Gdi::*;

pub struct OverlayImage {
    pub version: u64,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

const LINE_H: i32 = 20;

pub fn render_text(text: &str, version: u64) -> OverlayImage {
    let lines: Vec<Vec<u16>> = text.lines().map(|l| l.encode_utf16().collect()).collect();
    let width: i32 = 640;
    let height: i32 = lines.len() as i32 * LINE_H + 12;
    let mut out = vec![0u8; (width * height * 4) as usize];
    unsafe {
        let hdc = CreateCompatibleDC(None);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let Ok(dib) = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(hdc);
            return OverlayImage { version, width: width as u32, height: height as u32, bgra: out };
        };
        let old = SelectObject(hdc, dib);
        let font = CreateFontW(
            -15,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            ANTIALIASED_QUALITY.0 as u32,
            0,
            w!("Microsoft YaHei UI"),
        );
        let old_font = SelectObject(hdc, font);
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(0x00ff_ffff));
        for (i, l) in lines.iter().enumerate() {
            let _ = TextOutW(hdc, 10, 6 + i as i32 * LINE_H, l);
        }
        let _ = GdiFlush();
        std::ptr::copy_nonoverlapping(bits as *const u8, out.as_mut_ptr(), out.len());
        SelectObject(hdc, old_font);
        SelectObject(hdc, old);
        let _ = DeleteObject(font);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(hdc);
    }
    // GDI leaves alpha at 0: derive it from the (white) glyph coverage over a
    // translucent black background.
    for px in out.chunks_exact_mut(4) {
        let cov = px[0].max(px[1]).max(px[2]);
        px[3] = cov.max(170);
    }
    OverlayImage { version, width: width as u32, height: height as u32, bgra: out }
}
