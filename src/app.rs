//! The winit application: window, input mapping, cursor, rendering, hotkeys.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use nya_proto::frame::AudioPacket;
use nya_proto::pb::{self, control_msg::Msg, cursor_msg, input_msg::Ev};
use nya_win::d3d::D3dDevice;
use nya_win::topology::Topology;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::platform::windows::MonitorHandleExtWindows;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{CursorGrabMode, CustomCursor, Fullscreen, Window, WindowId};

use crate::events::{Hotkey, NetCmd, Ui, UiEvent};
use crate::net::{self, Link, Params, Sinks};
use crate::overlay::{self, OverlayImage};
use crate::render::{fit, Renderer};
use crate::stats::{Shared, Summary};
use crate::video::{FrameStore, Slot, VideoIn, VideoThread};
use crate::{caps, input};

/// Everything created before the window exists.
pub struct Startup {
    pub link: Link,
    pub params: Params,
    pub runtime: tokio::runtime::Handle,
    pub net_rx: UnboundedReceiver<NetCmd>,
    pub video_rx: Receiver<VideoIn>,
    pub audio_rx: Receiver<AudioPacket>,
    pub audio_tx: Sender<AudioPacket>,
    pub audio: bool,
    pub clipboard: bool,
    pub max_fps: u32,
}

pub struct App {
    startup: Option<Startup>,
    ui: Ui,
    net_tx: UnboundedSender<NetCmd>,
    video_tx: Sender<VideoIn>,
    stats: Arc<Shared>,
    store: Arc<FrameStore>,
    hw_decode: bool,
    host_label: String,
    start: pb::StartStream,

    window: Option<Window>,
    renderer: Option<Renderer>,
    adapter_luid: u64,
    clip_tx: Option<Sender<String>>,

    status: String,
    session: Option<pb::SessionInfo>,
    stream: Option<pb::StreamStarted>,
    server_stats: Option<pb::ServerStats>,
    current: Option<Arc<Slot>>,
    cursors: HashMap<u32, CustomCursor>,
    cursor_shape: u32,
    cursor_visible: bool,
    relative: bool,
    focused: bool,
    overlay_on: bool,
    game: bool,
    fullscreen: bool,
    overlay_img: Option<OverlayImage>,
    overlay_version: u64,
    last_tick: Instant,
    summary: Summary,
    status_log: Instant,
    last_rendered_total: u64,
    exit_message: Option<String>,
}

fn ctl(m: Msg) -> NetCmd {
    NetCmd::Control(pb::ControlMsg { msg: Some(m) })
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        startup: Startup,
        ui: Ui,
        net_tx: UnboundedSender<NetCmd>,
        video_tx: Sender<VideoIn>,
        hw_decode: bool,
        host_label: String,
        fullscreen: bool,
    ) -> Self {
        let start = startup.params.start.clone();
        let game = start.config.as_ref().is_some_and(|c| c.mode == pb::StreamMode::Game as i32);
        Self {
            startup: Some(startup),
            ui,
            net_tx,
            video_tx,
            stats: Arc::new(Shared::new()),
            store: Arc::new(FrameStore::default()),
            hw_decode,
            host_label,
            start,
            window: None,
            renderer: None,
            adapter_luid: 0,
            clip_tx: None,
            status: "连接中".into(),
            session: None,
            stream: None,
            server_stats: None,
            current: None,
            cursors: HashMap::new(),
            cursor_shape: 0,
            cursor_visible: true,
            relative: false,
            focused: false,
            overlay_on: false,
            game,
            fullscreen,
            overlay_img: None,
            overlay_version: 0,
            last_tick: Instant::now(),
            summary: Summary::default(),
            status_log: Instant::now(),
            last_rendered_total: 0,
            exit_message: None,
        }
    }

    pub fn exit_message(&self) -> Option<&str> {
        self.exit_message.as_deref()
    }

    fn send_input(&self, ev: Ev) {
        let _ = self.net_tx.send(NetCmd::Input(pb::InputMsg { ev: Some(ev) }));
    }

    fn hwnd(window: &Window) -> Option<windows::Win32::Foundation::HWND> {
        match window.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _)),
            _ => None,
        }
    }

    /// Device on the GPU that drives the monitor the window is on.
    fn device_for_window(window: &Window) -> anyhow::Result<D3dDevice> {
        if let (Some(m), Ok(topo)) = (window.current_monitor(), Topology::enumerate()) {
            if let Some(a) = topo.adapter_for_monitor(m.hmonitor()) {
                if let Ok(d) = D3dDevice::for_adapter(&a.adapter) {
                    return Ok(d);
                }
            }
        }
        D3dDevice::default_adapter()
    }

    fn create_renderer(&mut self, dev: D3dDevice) -> anyhow::Result<()> {
        let window = self.window.as_ref().unwrap();
        let hwnd = Self::hwnd(window).ok_or_else(|| anyhow::anyhow!("no HWND"))?;
        let size = window.inner_size();
        self.adapter_luid = dev.luid;
        self.renderer = Some(Renderer::new(dev, hwnd, size.width, size.height)?);
        Ok(())
    }

    fn title(&self) -> String {
        let mut t = format!("{} — NyaRemoteControl", self.host_label);
        if let Some(s) = &self.stream {
            let c = s.config.clone().unwrap_or_default();
            let chroma = if c.chroma == pb::Chroma::Yuv444 as i32 { "4:4:4" } else { "4:2:0" };
            t += &format!(" — {}x{}@{} {} {} {} kbps", c.width, c.height, self.summary.fps, s.encoder_name, chroma, self.summary.kbps);
        }
        if !self.status.is_empty() {
            t += &format!(" — {}", self.status);
        }
        if !input::grabbed() {
            t += " [键盘未捕获 Ctrl+Alt+Shift+Q]";
        }
        t
    }

    fn overlay_text(&self) -> String {
        let s = &self.summary;
        let mut lines = vec![format!("{}  {}", self.host_label, self.status)];
        if let Some(st) = &self.stream {
            let c = st.config.clone().unwrap_or_default();
            lines.push(format!(
                "编码 {} {} {}x{}@{}  {}{}",
                st.encoder_name,
                if c.chroma == pb::Chroma::Yuv444 as i32 { "4:4:4" } else { "4:2:0" },
                c.width,
                c.height,
                c.fps,
                if c.mode == pb::StreamMode::Game as i32 { "游戏模式" } else { "办公模式" },
                if st.cross_gpu { format!("  跨显卡 [{}]→[{}]", st.capture_gpu_index, st.encode_gpu_index) } else { String::new() }
            ));
        }
        let (sfps, skbps, enc_ms, xfer_ms) = self
            .server_stats
            .as_ref()
            .map(|x| (x.fps, x.bitrate_kbps, x.encode_ms_p50, x.transfer_ms_p50))
            .unwrap_or_default();
        lines.push(format!("帧率 被控端 {sfps} / 本机 {}  丢帧 {}  码率 {:.1} Mbps", s.fps, s.dropped, skbps.max(s.kbps) as f32 / 1000.0));
        lines.push(format!("延迟 端到端 {:.1} ms  RTT {:.1} ms", s.latency_ms, s.rtt_ms));
        lines.push(format!(
            "耗时 编码 {enc_ms:.1} ms  跨显卡 {xfer_ms:.1} ms  解码 {:.1} ms  渲染 {:.1} ms",
            s.decode_ms, s.render_ms
        ));
        lines.push(format!(
            "解码器 {}  鼠标 {}  键盘 {}",
            s.decoder,
            if self.relative { "相对" } else { "绝对" },
            if input::grabbed() { "已捕获" } else { "未捕获" }
        ));
        lines.push("Ctrl+Alt+Shift: Q 键盘 S 统计 M 模式 R 相对鼠标 F 全屏 D Ctrl+Alt+Del 1-9 显示器 X 退出".into());
        lines.join("\n")
    }

    fn refresh_overlay(&mut self) {
        if self.overlay_on {
            self.overlay_version += 1;
            self.overlay_img = Some(overlay::render_text(&self.overlay_text(), self.overlay_version));
        }
    }

    fn draw(&mut self) {
        let (slot, fresh) = self.store.take();
        if slot.is_some() {
            self.current = slot;
        }
        let Some(r) = self.renderer.as_mut() else { return };
        let overlay = if self.overlay_on { self.overlay_img.as_ref() } else { None };
        let t = Instant::now();
        if let Err(e) = r.render(self.current.as_deref(), overlay, self.game) {
            tracing::warn!("render failed ({e:#}); recreating device");
            self.recreate_device();
            return;
        }
        let render_ms = t.elapsed().as_secs_f32() * 1000.0;
        if fresh && self.stats.with(|s| s.total_rendered == 0) {
            tracing::info!("first frame rendered ({:.1} ms)", render_ms);
        }
        if fresh {
            self.stats.with(|s| s.total_rendered += 1);
        }
        if fresh {
            let lat = self.current.as_ref().map(|s| self.stats.latency_ms(s.capture_ts));
            self.stats.with(|s| {
                s.render_ms.push(render_ms);
                s.frames_rendered += 1;
                if let Some(l) = lat {
                    s.latency_ms.push(l);
                }
            });
        }
    }

    fn recreate_device(&mut self) {
        self.renderer = None;
        self.current = None;
        let Some(w) = self.window.as_ref() else { return };
        match Self::device_for_window(w) {
            Ok(dev) => {
                let _ = self.video_tx.send(VideoIn::Device(dev.clone()));
                if let Err(e) = self.create_renderer(dev) {
                    tracing::error!("renderer: {e:#}");
                }
            }
            Err(e) => tracing::error!("D3D device: {e:#}"),
        }
    }

    fn video_size(&self) -> Option<(u32, u32)> {
        if let Some(s) = &self.current {
            return Some((s.width, s.height));
        }
        let c = self.stream.as_ref()?.config.clone()?;
        Some((c.width, c.height))
    }

    fn map_mouse(&self, x: f64, y: f64) -> Option<(u32, u32)> {
        let (vw, vh) = self.video_size()?;
        let r = self.renderer.as_ref()?;
        let rect = fit(r.width, r.height, vw, vh);
        let nx = ((x - rect.x) / rect.w).clamp(0.0, 1.0);
        let ny = ((y - rect.y) / rect.h).clamp(0.0, 1.0);
        Some(((nx * 65535.0).round() as u32, (ny * 65535.0).round() as u32))
    }

    fn set_relative(&mut self, on: bool) {
        self.relative = on;
        if let Some(w) = &self.window {
            if on {
                let _ = w.set_cursor_grab(CursorGrabMode::Confined).or_else(|_| w.set_cursor_grab(CursorGrabMode::Locked));
                w.set_cursor_visible(false);
            } else {
                let _ = w.set_cursor_grab(CursorGrabMode::None);
                w.set_cursor_visible(self.cursor_visible);
            }
        }
    }

    fn hotkey(&mut self, el: &ActiveEventLoop, h: Hotkey) {
        match h {
            Hotkey::ToggleGrab => {
                input::set_grab(!input::grabbed());
                if !input::grabbed() {
                    self.send_input(Ev::ReleaseAll(pb::ReleaseAll {}));
                }
            }
            Hotkey::ToggleStats => {
                self.overlay_on = !self.overlay_on;
                self.refresh_overlay();
                self.draw();
            }
            Hotkey::ToggleMode => {
                self.game = !self.game;
                let mode = if self.game { pb::StreamMode::Game } else { pb::StreamMode::Office } as i32;
                self.start.config.get_or_insert_with(Default::default).mode = mode;
                let _ = self.net_tx.send(ctl(Msg::SetMode(pb::SetMode { mode })));
                self.status = if self.game { "切换到游戏模式" } else { "切换到办公模式" }.into();
            }
            Hotkey::ToggleRelative => self.set_relative(!self.relative),
            Hotkey::ToggleFullscreen => {
                self.fullscreen = !self.fullscreen;
                if let Some(w) = &self.window {
                    w.set_fullscreen(self.fullscreen.then(|| Fullscreen::Borderless(None)));
                }
            }
            Hotkey::CtrlAltDel => {
                let _ = self.net_tx.send(ctl(Msg::SendSas(pb::SendSas {})));
            }
            Hotkey::Display(n) => {
                let id = self.session.as_ref().and_then(|s| s.displays.get(n as usize - 1)).map(|d| d.id);
                if let Some(id) = id {
                    self.start.display_id = id;
                    let _ = self.net_tx.send(ctl(Msg::StartStream(self.start.clone())));
                    self.status = format!("切换到显示器 {n}");
                }
            }
            Hotkey::Quit => self.quit(el, None),
        }
        if let Some(w) = &self.window {
            w.set_title(&self.title());
        }
    }

    fn quit(&mut self, el: &ActiveEventLoop, msg: Option<String>) {
        let _ = self.net_tx.send(NetCmd::Quit);
        self.exit_message = msg;
        el.exit();
    }

    fn on_cursor(&mut self, el: &ActiveEventLoop, m: pb::CursorMsg) {
        match m.msg {
            Some(cursor_msg::Msg::Shape(s)) => {
                let src = CustomCursor::from_rgba(
                    s.rgba,
                    s.width.min(u16::MAX as u32) as u16,
                    s.height.min(u16::MAX as u32) as u16,
                    s.hot_x.clamp(0, s.width as i32 - 1) as u16,
                    s.hot_y.clamp(0, s.height as i32 - 1) as u16,
                );
                match src {
                    Ok(src) => {
                        self.cursors.insert(s.id, el.create_custom_cursor(src));
                    }
                    Err(e) => tracing::debug!("cursor shape: {e}"),
                }
            }
            Some(cursor_msg::Msg::State(st)) => {
                let Some(w) = &self.window else { return };
                if st.shape_id != self.cursor_shape {
                    if let Some(c) = self.cursors.get(&st.shape_id) {
                        w.set_cursor(c.clone());
                        self.cursor_shape = st.shape_id;
                    }
                }
                if st.visible != self.cursor_visible {
                    self.cursor_visible = st.visible;
                    if !self.relative {
                        w.set_cursor_visible(st.visible);
                    }
                }
            }
            None => {}
        }
    }

    fn tick(&mut self) {
        let secs = self.last_tick.elapsed().as_secs_f32();
        if secs < 1.0 {
            return;
        }
        if self.stream.is_some() && self.status_log.elapsed() >= Duration::from_secs(5) {
            self.status_log = Instant::now();
            let (f, b, d, r) = self.stats.with(|s| (s.total_rx_frames, s.total_rx_bytes, s.total_decoded, s.total_rendered));
            if r == self.last_rendered_total {
                tracing::warn!("no new picture in 5 s: received {f} frames / {} KB, decoded {d}, rendered {r}", b / 1024);
            }
            self.last_rendered_total = r;
        }
        self.last_tick = Instant::now();
        self.summary = self.stats.take_summary(secs);
        let s = &self.summary;
        let _ = self.net_tx.send(ctl(Msg::ClientStats(pb::ClientStats {
            decode_ms_p50: s.decode_ms,
            render_ms_p50: s.render_ms,
            frames_dropped: s.dropped,
            fps: s.fps,
        })));
        if let Some(w) = &self.window {
            w.set_title(&self.title());
        }
        if self.overlay_on {
            self.refresh_overlay();
            self.draw();
        }
    }
}

impl ApplicationHandler<UiEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        let Some(st) = self.startup.take() else { return };
        let attrs = Window::default_attributes()
            .with_title(format!("{} — NyaRemoteControl", self.host_label))
            .with_inner_size(PhysicalSize::new(1600u32, 900u32))
            .with_fullscreen(self.fullscreen.then(|| Fullscreen::Borderless(None)));
        let window = match el.create_window(attrs) {
            Ok(w) => w,
            Err(e) => return self.quit(el, Some(format!("无法创建窗口：{e}"))),
        };
        let hwnd = Self::hwnd(&window);
        let dev = match Self::device_for_window(&window) {
            Ok(d) => d,
            Err(e) => return self.quit(el, Some(format!("无法创建 D3D11 设备：{e:#}"))),
        };
        let monitor_fps = window
            .current_monitor()
            .and_then(|m| m.refresh_rate_millihertz())
            .map(|mhz| mhz.div_ceil(1000))
            .unwrap_or(60);
        self.window = Some(window);
        if let Err(e) = self.create_renderer(dev.clone()) {
            return self.quit(el, Some(format!("无法初始化渲染：{e:#}")));
        }
        tracing::info!("renderer on adapter luid {:#x}", self.adapter_luid);
        // Paint black right away instead of leaving the window uninitialised.
        self.draw();

        let max_fps = if st.max_fps > 0 { st.max_fps } else { monitor_fps };
        let caps = caps::detect(&dev, self.hw_decode, max_fps);
        tracing::info!("decoders: {:?}", caps.decoders.iter().map(|d| (d.codec, d.chroma, d.hardware)).collect::<Vec<_>>());

        VideoThread {
            hw_allowed: self.hw_decode,
            caps: caps.clone(),
            store: self.store.clone(),
            ui: self.ui.clone(),
            net: self.net_tx.clone(),
            stats: self.stats.clone(),
        }
        .spawn(dev, st.video_rx);
        if st.audio {
            crate::audio::spawn(st.audio_rx);
        }
        if st.clipboard {
            let (tx, rx) = crossbeam_channel::unbounded();
            crate::clipboard::spawn(rx, self.net_tx.clone());
            self.clip_tx = Some(tx);
        }
        if let Some(h) = hwnd {
            input::install(h, self.net_tx.clone(), self.ui.clone());
        }

        let mut params = st.params;
        params.caps = caps;
        let sinks = Sinks { ui: self.ui.clone(), video: self.video_tx.clone(), audio: st.audio_tx, stats: self.stats.clone() };
        st.runtime.spawn(net::supervise(st.link, params, st.net_rx, sinks));
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.quit(el, None),
            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    if let Err(e) = r.resize(size.width, size.height) {
                        tracing::warn!("resize: {e:#}");
                    }
                }
                self.draw();
            }
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Focused(f) => {
                self.focused = f;
                if !f {
                    input::reset_modifiers();
                    self.send_input(Ev::ReleaseAll(pb::ReleaseAll {}));
                    if self.relative {
                        self.set_relative(false);
                    }
                }
            }
            WindowEvent::Moved(_) => {
                // Moving to a monitor on another GPU: follow it (design doc §3.5, client side).
                if let Some(w) = &self.window {
                    if let (Some(m), Ok(topo)) = (w.current_monitor(), Topology::enumerate()) {
                        if let Some(a) = topo.adapter_for_monitor(m.hmonitor()) {
                            if a.luid != self.adapter_luid && self.adapter_luid != 0 {
                                tracing::info!("window moved to GPU {}; recreating device", a.name);
                                self.recreate_device();
                            }
                        }
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if !self.relative && self.focused {
                    if let Some((x, y)) = self.map_mouse(position.x, position.y) {
                        self.send_input(Ev::MouseAbs(pb::MouseAbs { x, y }));
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let b = match button {
                    MouseButton::Left => pb::MouseButton::Left,
                    MouseButton::Right => pb::MouseButton::Right,
                    MouseButton::Middle => pb::MouseButton::Middle,
                    MouseButton::Back => pb::MouseButton::X1,
                    MouseButton::Forward => pb::MouseButton::X2,
                    MouseButton::Other(_) => return,
                };
                self.send_input(Ev::MouseButton(pb::MouseButtonEv { button: b as i32, down: state == ElementState::Pressed }));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => ((x * 120.0) as i32, (y * 120.0) as i32),
                    MouseScrollDelta::PixelDelta(p) => (p.x as i32, p.y as i32),
                };
                if dx != 0 || dy != 0 {
                    self.send_input(Ev::Wheel(pb::Wheel { dx, dy }));
                }
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _el: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = event {
            if self.relative && self.focused {
                let (dx, dy) = (delta.0.round() as i32, delta.1.round() as i32);
                if dx != 0 || dy != 0 {
                    self.send_input(Ev::MouseRel(pb::MouseRel { dx, dy }));
                }
            }
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, event: UiEvent) {
        let refresh_title = !matches!(event, UiEvent::Frame | UiEvent::Cursor(_));
        match event {
            UiEvent::Frame => self.draw(),
            UiEvent::Cursor(m) => self.on_cursor(el, m),
            UiEvent::Connected { server_name } => {
                self.status = String::new();
                if self.host_label.is_empty() {
                    self.host_label = server_name;
                }
            }
            UiEvent::SessionInfo(i) => self.session = Some(i),
            UiEvent::StreamStarted(s) => {
                tracing::info!(
                    "stream: {} {:?} cross_gpu={}",
                    s.encoder_name,
                    s.config.as_ref().map(|c| (c.width, c.height, c.fps, c.bitrate_kbps)),
                    s.cross_gpu
                );
                self.game = s.config.as_ref().is_some_and(|c| c.mode == pb::StreamMode::Game as i32);
                self.stream = Some(s);
                self.status = String::new();
                self.cursor_shape = 0;
            }
            UiEvent::StreamError(e) => {
                tracing::error!("stream error: {e}");
                self.status = format!("被控端无法开始推流：{e}");
            }
            UiEvent::ServerStats(s) => self.server_stats = Some(s),
            UiEvent::Clipboard(t) => {
                if let Some(tx) = &self.clip_tx {
                    let _ = tx.send(t);
                }
            }
            UiEvent::Reconnecting(msg) => {
                self.status = format!("连接中断，正在重连…（{msg}）");
                self.send_input(Ev::ReleaseAll(pb::ReleaseAll {}));
            }
            UiEvent::Disconnected(msg) => return self.quit(el, Some(msg)),
            UiEvent::Hotkey(h) => self.hotkey(el, h),
        }
        if refresh_title {
            if let Some(w) = &self.window {
                w.set_title(&self.title());
            }
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.tick();
        el.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(250)));
    }
}
