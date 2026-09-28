//! The winit application: one window that shows the launcher or a session,
//! with the egui UI painted over the remote picture.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nya_proto::pb::{self, cursor_msg, input_msg::Ev};
use nya_transport::{Fingerprint, Identity};
use nya_ui::Gui;
use nya_win::d3d::D3dDevice;
use nya_win::topology::Topology;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::platform::windows::MonitorHandleExtWindows;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{CursorGrabMode, CursorIcon, CustomCursor, Fullscreen, Window, WindowId};

use crate::config::{ClientConfig, Defaults, HostEntry};
use crate::events::{ConnectDone, Hotkey, Ui, UiEvent};
use crate::net::{self, Link, PairPrompt, Params};
use crate::render::{fit, Renderer};
use crate::session::{Session, SessionOptions};
use crate::ui::{self, Action, LauncherState, Notice, PairingDialog};
use crate::{caps, input};

/// A connection attempt in progress.
struct Pending {
    address: String,
    label: Option<String>,
    reverify: bool,
}

pub struct App {
    rt: tokio::runtime::Handle,
    ui_tx: Ui,
    data_dir: PathBuf,
    cfg: ClientConfig,
    identity: Identity,
    client_name: String,
    auto_connect: Option<(String, Option<String>)>,

    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    gui: Option<Gui>,
    adapter_luid: u64,

    launcher: LauncherState,
    session: Option<Session>,
    pending: Option<Pending>,
    attempt: u64,
    connect_task: Option<tokio::task::JoinHandle<()>>,
    pair_reply: Option<std::sync::mpsc::Sender<Option<String>>>,
    verify_link: Option<Box<Link>>,

    focused: bool,
    fullscreen: bool,
    toolbar_open: bool,
    mods: winit::keyboard::ModifiersState,
    cursor_over_ui: bool,
    /// Buttons pressed on the remote side (their releases must follow).
    remote_buttons: u8,
    repaint_at: Option<Instant>,
    exit: bool,
    hovering_file: bool,
    /// Files dropped on the window, sent together once the drop is complete.
    dropped: Vec<PathBuf>,
    /// Window resized: resize the host's virtual display at this time.
    vd_resize_at: Option<Instant>,
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

fn parse_codec(s: &str) -> pb::Codec {
    match s.to_ascii_lowercase().as_str() {
        "h264" | "avc" => pb::Codec::H264,
        "hevc" | "h265" => pb::Codec::Hevc,
        "av1" => pb::Codec::Av1,
        _ => pb::Codec::Unspecified,
    }
}

fn parse_chroma(s: &str) -> pb::Chroma {
    match s {
        "420" => pb::Chroma::Yuv420,
        "444" => pb::Chroma::Yuv444,
        _ => pb::Chroma::Unspecified,
    }
}

/// "不限制": the top of the encoder's range; static office content still only
/// uses what it needs.
const UNLIMITED_KBPS: u32 = 80_000;

fn parse_display_mode(s: &str) -> pb::DisplayMode {
    match s {
        "virtual" => pb::DisplayMode::Virtual,
        "private" => pb::DisplayMode::Private,
        _ => pb::DisplayMode::Physical,
    }
}

/// Virtual display sizes: width a multiple of 8, height even, at least 640x480.
fn vd_dims(w: u32, h: u32) -> (u32, u32) {
    ((w & !7).max(640), (h & !1).max(480))
}

fn start_request(d: &Defaults, vd: Option<pb::VirtualDisplay>) -> pb::StartStream {
    let game = d.mode.eq_ignore_ascii_case("game");
    pb::StartStream {
        // A virtual display is streamed as the host's primary display.
        display_id: if vd.is_some() { 0 } else { d.display },
        virtual_display: vd,
        config: Some(pb::StreamConfig {
            codec: parse_codec(&d.codec) as i32,
            chroma: parse_chroma(&d.chroma) as i32,
            width: 0,
            height: 0,
            fps: 0,
            bitrate_kbps: if d.unlimited_bitrate { UNLIMITED_KBPS } else { d.bitrate_kbps },
            mode: if game { pb::StreamMode::Game } else { pb::StreamMode::Office } as i32,
            bitrate_policy: crate::ui::parse_policy(&d.bitrate_policy) as i32,
        }),
        encoder_preference: d.encoder.clone(),
    }
}

impl App {
    pub fn new(
        rt: tokio::runtime::Handle,
        ui_tx: Ui,
        data_dir: PathBuf,
        cfg: ClientConfig,
        identity: Identity,
        auto_connect: Option<(String, Option<String>)>,
    ) -> Self {
        Self {
            rt,
            ui_tx,
            data_dir,
            cfg,
            identity,
            client_name: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "nya-client".into()),
            auto_connect,
            window: None,
            renderer: None,
            gui: None,
            adapter_luid: 0,
            launcher: LauncherState::default(),
            session: None,
            pending: None,
            attempt: 0,
            connect_task: None,
            pair_reply: None,
            verify_link: None,
            focused: false,
            fullscreen: false,
            toolbar_open: false,
            mods: Default::default(),
            cursor_over_ui: false,
            remote_buttons: 0,
            repaint_at: None,
            exit: false,
            hovering_file: false,
            dropped: Vec::new(),
            vd_resize_at: None,
        }
    }

    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    // ------------------------------------------------------------------ devices

    fn create_renderer(&mut self, dev: D3dDevice) -> anyhow::Result<()> {
        let window = self.window.clone().unwrap();
        let h = hwnd(&window).ok_or_else(|| anyhow::anyhow!("no HWND"))?;
        let size = window.inner_size();
        self.adapter_luid = dev.luid;
        match self.gui.as_mut() {
            Some(g) => g.set_device(&dev)?,
            None => self.gui = Some(Gui::new(&window, &dev)?),
        }
        self.renderer = Some(Renderer::new(dev, h, size.width, size.height)?);
        Ok(())
    }

    fn recreate_device(&mut self) {
        self.renderer = None;
        let Some(w) = self.window.clone() else { return };
        match device_for_window(&w) {
            Ok(dev) => {
                if let Some(s) = &mut self.session {
                    s.current = None;
                    s.set_device(&dev);
                }
                if let Err(e) = self.create_renderer(dev) {
                    tracing::error!("renderer: {e:#}");
                }
            }
            Err(e) => tracing::error!("D3D device: {e:#}"),
        }
    }

    /// Virtual display request for `mode` from the settings and this window.
    fn vd_request(&self, mode: pb::DisplayMode, fullscreen: bool) -> Option<pb::VirtualDisplay> {
        if mode == pb::DisplayMode::Physical {
            return None;
        }
        let d = &self.cfg.defaults;
        let w = self.window.as_ref()?;
        let monitor = w.current_monitor();
        let monitor_size = monitor.as_ref().map(|m| (m.size().width, m.size().height)).unwrap_or((1920, 1080));
        let (width, height) = match d.vd_size.as_str() {
            "fixed" => (d.vd_width, d.vd_height),
            "screen" => monitor_size,
            // Follow the window; fullscreen means the whole monitor.
            _ if fullscreen => monitor_size,
            _ => (w.inner_size().width, w.inner_size().height),
        };
        let (width, height) = vd_dims(width, height);
        let refresh_hz = monitor.and_then(|m| m.refresh_rate_millihertz()).map(|mhz| (mhz + 500) / 1000).unwrap_or(60);
        Some(pb::VirtualDisplay {
            mode: mode as i32,
            width,
            height,
            refresh_hz,
            scale_percent: if d.vd_scale { (w.scale_factor() * 100.0).round() as u32 } else { 0 },
        })
    }

    /// The window size settled: fit the virtual display to it.
    fn fit_virtual_display(&mut self) {
        let Some(s) = self.session.as_ref() else { return };
        if !s.vd_follow_window || s.display_mode() == pb::DisplayMode::Physical {
            return;
        }
        if self.window.as_ref().is_some_and(|w| w.is_minimized() == Some(true) || w.inner_size().width == 0) {
            return;
        }
        let vd = self.vd_request(s.display_mode(), self.fullscreen);
        if let Some(s) = self.session.as_mut() {
            s.set_virtual_display(vd);
        }
    }

    // --------------------------------------------------------------- connecting

    fn connect(&mut self, target: String, name: Option<String>) {
        let entry = self.cfg.find(&target).cloned();
        let address = entry.as_ref().map(|e| e.address.clone()).unwrap_or(target);
        let label = name.or_else(|| entry.as_ref().map(|e| e.name.clone()));
        self.start_connect(Pending { address, label, reverify: false });
    }

    fn start_connect(&mut self, p: Pending) {
        let pinned = if p.reverify {
            None
        } else {
            self.cfg
                .hosts
                .iter()
                .find(|h| h.address == p.address)
                .and_then(|h| Fingerprint::from_hex(&h.fingerprint))
        };
        self.attempt += 1;
        let attempt = self.attempt;
        let (id, name, ui, address) = (self.identity.clone(), self.client_name.clone(), self.ui_tx.clone(), p.address.clone());
        let ui2 = ui.clone();
        let prompt: PairPrompt = Arc::new(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            ui2.send(UiEvent::NeedPairing(tx));
            rx.recv().ok().flatten()
        });
        let task = self.rt.spawn(async move {
            let res = async {
                let addr = nya_transport::endpoint::resolve(&address, nya_proto::DEFAULT_PORT)?;
                net::connect(addr, &id, pinned, &name, Some(prompt)).await
            }
            .await;
            let (result, pin_mismatch) = match res {
                Ok(l) => (Ok(Box::new(l)), false),
                Err(e) => {
                    let m = format!("{e:#}");
                    let pm = pinned.is_some() && m.contains(nya_transport::tls::PIN_MISMATCH);
                    (Err(m), pm)
                }
            };
            ui.send(UiEvent::ConnectDone(ConnectDone { attempt, result, pin_mismatch }));
        });
        self.launcher.connecting = Some(p.label.clone().unwrap_or_else(|| p.address.clone()));
        self.launcher.notice = None;
        self.pending = Some(p);
        self.connect_task = Some(task);
        self.request_redraw();
    }

    fn cancel_connect(&mut self) {
        if let Some(t) = self.connect_task.take() {
            t.abort();
        }
        if let Some(tx) = self.pair_reply.take() {
            let _ = tx.send(None);
        }
        self.pending = None;
        self.verify_link = None;
        self.launcher.connecting = None;
        self.launcher.pairing = None;
        self.launcher.pin_changed = false;
        self.launcher.verify_fingerprint = None;
    }

    fn on_connect_done(&mut self, done: ConnectDone) {
        if done.attempt != self.attempt || self.pending.is_none() {
            return; // cancelled
        }
        self.connect_task = None;
        self.launcher.connecting = None;
        self.launcher.pairing = None;
        match done.result {
            Ok(link) => {
                let reverify = self.pending.as_ref().is_some_and(|p| p.reverify);
                if reverify && !link.welcome.needs_pairing {
                    self.launcher.verify_fingerprint = Some(link.server_fp.to_string());
                    self.verify_link = Some(link);
                } else {
                    self.finish_connect(link);
                }
            }
            Err(msg) if done.pin_mismatch => {
                tracing::warn!("{msg}");
                self.launcher.pin_changed = true;
            }
            Err(msg) => {
                self.pending = None;
                self.launcher.notice = Some(Notice::Error(msg));
            }
        }
    }

    fn finish_connect(&mut self, link: Box<Link>) {
        let Some(p) = self.pending.take() else { return };
        let label = p.label.unwrap_or_else(|| link.welcome.server_name.clone());
        self.cfg.upsert(HostEntry { name: label.clone(), address: p.address, fingerprint: link.server_fp.to_hex() });
        if let Err(e) = self.cfg.save(&self.data_dir) {
            tracing::warn!("save config: {e:#}");
        }
        let Some(dev) = self.renderer.as_ref().map(|r| r.dev.clone()) else { return };
        let d = self.cfg.defaults.clone();
        let monitor_fps = self
            .window
            .as_ref()
            .and_then(|w| w.current_monitor())
            .and_then(|m| m.refresh_rate_millihertz())
            .map(|mhz| mhz.div_ceil(1000))
            .unwrap_or(60);
        let caps = caps::detect(&dev, d.hw_decode, if d.max_fps > 0 { d.max_fps } else { monitor_fps });
        tracing::info!("decoders: {:?}", caps.decoders.iter().map(|c| (c.codec, c.chroma, c.hardware)).collect::<Vec<_>>());
        let vd_supported = link.neg.has(pb::Feature::VirtualDisplay);
        let vd = if vd_supported { self.vd_request(parse_display_mode(&d.display_mode), d.fullscreen) } else { None };
        let params = Params {
            addr: link.conn.remote_address(),
            pinned: link.server_fp,
            identity: self.identity.clone(),
            name: self.client_name.clone(),
            caps,
            start: start_request(&d, vd),
        };
        let opts = SessionOptions { hw_decode: d.hw_decode, audio: d.audio, clipboard: d.clipboard };
        let mut session = Session::start(&self.rt, *link, params, &dev, &opts, self.ui_tx.clone(), label);
        session.vd_supported = vd_supported;
        session.vd_follow_window = d.vd_size == "window";
        self.session = Some(session);
        self.vd_resize_at = None;
        self.toolbar_open = false;
        self.launcher.notice = None;
        if d.fullscreen {
            self.set_fullscreen(true);
        }
        self.update_no_hotkeys();
        self.update_title();
    }

    fn end_session(&mut self, message: Option<Notice>) {
        let Some(s) = self.session.take() else { return };
        s.quit();
        drop(s);
        self.set_fullscreen(false);
        if let Some(w) = &self.window {
            let _ = w.set_cursor_grab(CursorGrabMode::None);
            w.set_cursor(CursorIcon::Default);
            w.set_cursor_visible(true);
        }
        self.remote_buttons = 0;
        self.launcher.notice = message;
        self.update_no_hotkeys();
        self.update_title();
        self.request_redraw();
    }

    // ------------------------------------------------------------------ session

    fn set_fullscreen(&mut self, on: bool) {
        self.fullscreen = on;
        if let Some(w) = &self.window {
            w.set_fullscreen(on.then(|| Fullscreen::Borderless(None)));
        }
    }

    fn set_relative(&mut self, on: bool) {
        let Some(s) = self.session.as_mut() else { return };
        s.relative = on;
        if let Some(w) = &self.window {
            if on {
                let _ = w.set_cursor_grab(CursorGrabMode::Confined).or_else(|_| w.set_cursor_grab(CursorGrabMode::Locked));
                w.set_cursor_visible(false);
            } else {
                let _ = w.set_cursor_grab(CursorGrabMode::None);
                w.set_cursor_visible(s.cursor_visible);
            }
        }
    }

    /// While focused and grabbed, keep shell hotkeys (Win+D, …) from acting locally.
    fn update_no_hotkeys(&self) {
        if let Some(h) = self.window.as_ref().and_then(|w| hwnd(w)) {
            input::set_no_hotkeys(h, self.focused && input::grabbed() && self.session.is_some());
        }
    }

    fn update_title(&self) {
        let Some(w) = &self.window else { return };
        let t = match &self.session {
            None => "NyaRemoteControl".to_string(),
            Some(s) => {
                let mut t = format!("{} — NyaRemoteControl", s.label);
                if let Some(st) = &s.stream {
                    let c = st.config.clone().unwrap_or_default();
                    t += &format!(" — {}x{}@{} {} {} kbps", c.width, c.height, s.summary.fps, st.encoder_name, s.summary.kbps);
                }
                if !input::grabbed() {
                    t += " [键盘未捕获]";
                }
                t
            }
        };
        w.set_title(&t);
    }

    fn hotkey(&mut self, h: Hotkey) {
        tracing::info!("hotkey {h:?}");
        if self.session.is_none() {
            return;
        }
        match h {
            Hotkey::ToggleGrab => self.set_grab(!input::grabbed()),
            Hotkey::ToggleStats => {
                if let Some(s) = &mut self.session {
                    s.show_stats = !s.show_stats;
                }
            }
            Hotkey::ToggleMode => {
                if let Some(s) = &mut self.session {
                    let g = !s.game;
                    s.set_game_mode(g);
                }
            }
            Hotkey::ToggleRelative => {
                let on = !self.session.as_ref().is_some_and(|s| s.relative);
                self.set_relative(on);
            }
            Hotkey::ToggleFullscreen => self.set_fullscreen(!self.fullscreen),
            Hotkey::CtrlAltDel => {
                if let Some(s) = &self.session {
                    s.ctrl_alt_del();
                }
            }
            Hotkey::ToggleToolbar => self.toolbar_open = !self.toolbar_open,
            Hotkey::Display(n) => {
                if let Some(s) = &mut self.session {
                    s.select_display_index(n);
                }
            }
            Hotkey::Quit => self.end_session(Some(Notice::Info("已断开连接".into()))),
        }
        self.update_title();
        self.request_redraw();
    }

    fn set_grab(&mut self, on: bool) {
        input::set_grab(on);
        if !on {
            if let Some(s) = &self.session {
                s.release_all();
            }
        }
        self.update_no_hotkeys();
        self.update_title();
    }

    fn apply(&mut self, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::Connect { target, name } => self.connect(target, name),
                Action::CancelConnect => self.cancel_connect(),
                Action::PairCode(code) => {
                    self.launcher.pairing = None;
                    if let Some(tx) = self.pair_reply.take() {
                        let _ = tx.send(code);
                    }
                }
                Action::PinChanged(yes) => {
                    self.launcher.pin_changed = false;
                    match self.pending.take() {
                        Some(mut p) if yes => {
                            p.reverify = true;
                            self.start_connect(p);
                        }
                        _ => self.pending = None,
                    }
                }
                Action::FingerprintOk(yes) => {
                    self.launcher.verify_fingerprint = None;
                    match self.verify_link.take() {
                        Some(link) if yes => self.finish_connect(link),
                        _ => {
                            self.pending = None;
                            self.launcher.notice = Some(Notice::Error("证书指纹未确认，已取消连接".into()));
                        }
                    }
                }
                Action::DeleteHost(i) => {
                    if i < self.cfg.hosts.len() {
                        self.cfg.hosts.remove(i);
                        let _ = self.cfg.save(&self.data_dir);
                    }
                }
                Action::RenameHost(i, name) => match self.cfg.rename(i, &name) {
                    Ok(()) => {
                        self.launcher.rename = None;
                        if let Err(e) = self.cfg.save(&self.data_dir) {
                            self.launcher.notice = Some(Notice::Error(format!("保存失败：{e:#}")));
                        }
                    }
                    Err(e) => self.launcher.rename_error = Some(e.into()),
                },
                Action::SaveConfig => {
                    if let Err(e) = self.cfg.save(&self.data_dir) {
                        self.launcher.notice = Some(Notice::Error(format!("保存失败：{e:#}")));
                    }
                }
                Action::Hotkey(h) => self.hotkey(h),
                Action::SetGameMode(g) => {
                    if let Some(s) = &mut self.session {
                        s.set_game_mode(g);
                    }
                }
                Action::SetDisplayMode(mode) => {
                    let vd = self.vd_request(mode, self.fullscreen);
                    if let Some(s) = &mut self.session {
                        s.set_virtual_display(vd);
                    }
                }
                Action::SelectDisplay(id) => {
                    if let Some(s) = &mut self.session {
                        s.select_display(id);
                    }
                }
                Action::SetGrab(on) => self.set_grab(on),
                Action::ToggleUsb => {
                    let open = self.session.as_ref().is_some_and(|s| !s.usb_open);
                    if let Some(s) = &mut self.session {
                        s.usb_open = open;
                    }
                    if open {
                        self.refresh_usb();
                    }
                }
                Action::RefreshUsb => self.refresh_usb(),
                Action::InstallUsbipd => {
                    if let Some(s) = &mut self.session {
                        s.usbipd_install = Some((true, "准备安装包…".into()));
                    }
                    let ui = self.ui_tx.clone();
                    std::thread::spawn(move || {
                        let r = crate::usb::install_usbipd(&mut |m| ui.send(UiEvent::UsbipdInstall(true, m)));
                        ui.send(UiEvent::UsbipdInstall(
                            false,
                            match r {
                                Ok(()) => "安装完成".into(),
                                Err(e) => format!("安装失败：{e:#}"),
                            },
                        ));
                    });
                }
                Action::UsbAttach { busid, description, bound } => {
                    if let Some(s) = &mut self.session {
                        s.usb_busy.insert(busid.clone());
                        let (net, ui) = (s.net_tx.clone(), self.ui_tx.clone());
                        std::thread::spawn(move || {
                            // Share it with usbipd first (UAC prompt), then ask the host to attach.
                            let shared = if bound { Ok(()) } else { crate::usb::bind(&busid) };
                            match shared {
                                Ok(()) => {
                                    let m = pb::ControlMsg {
                                        msg: Some(pb::control_msg::Msg::UsbAttach(pb::UsbAttach { busid, description })),
                                    };
                                    let _ = net.send(crate::events::NetCmd::Control(m));
                                }
                                Err(e) => ui.send(UiEvent::UsbStatus(pb::UsbStatus {
                                    busid,
                                    attached: false,
                                    message: format!("{e:#}"),
                                })),
                            }
                        });
                    }
                }
                Action::UsbDetach(busid) => {
                    if let Some(s) = &mut self.session {
                        s.usb_detach(&busid);
                    }
                }
                Action::SetMic(on) => {
                    if let Some(s) = &mut self.session {
                        s.set_mic(on);
                    }
                }
                Action::SetPolicy(p) => {
                    if let Some(s) = &mut self.session {
                        s.set_bitrate_policy(p);
                    }
                }
                Action::Disconnect => self.end_session(Some(Notice::Info("已断开连接".into()))),
                Action::PickFiles => {
                    if let Some(paths) = rfd::FileDialog::new().set_title("选择要发送到被控端的文件").pick_files() {
                        if let Some(s) = &mut self.session {
                            s.send_files(paths);
                        }
                    }
                }
                Action::AcceptOffer(id) => {
                    if let Some(s) = &mut self.session {
                        s.accept_offer(id);
                    }
                }
                Action::DismissOffer(id) => {
                    if let Some(s) = &mut self.session {
                        s.dismiss_offer(id);
                    }
                }
                Action::DismissTransfer(id) => {
                    if let Some(s) = &mut self.session {
                        s.dismiss_transfer(id);
                    }
                }
                Action::OpenFolder(p) => {
                    let _ = std::process::Command::new("explorer").arg(p).spawn();
                }
            }
        }
    }

    // ------------------------------------------------------------------ drawing

    fn draw(&mut self) {
        let Some(window) = self.window.clone() else { return };
        let Some(mut gui) = self.gui.take() else { return };
        let mut actions = Vec::new();
        let (toolbar_open, fullscreen, hovering_file) = (self.toolbar_open, self.fullscreen, self.hovering_file);
        let (session, launcher, cfg) = (&mut self.session, &mut self.launcher, &mut self.cfg);
        let frame = gui.run(&window, |ctx| match session.as_mut() {
            Some(s) => ui::session_overlay(ctx, s, toolbar_open, fullscreen, hovering_file, &mut actions),
            None => ui::launcher(ctx, launcher, cfg, &mut actions),
        });

        let mut fresh = false;
        if let Some(s) = &mut self.session {
            let (slot, f) = s.store.take();
            if slot.is_some() {
                s.current = slot;
            }
            fresh = f;
        }
        let game = self.session.as_ref().is_some_and(|s| s.game);
        let t = Instant::now();
        let mut failed = false;
        if let Some(r) = self.renderer.as_mut() {
            let res = (|| -> anyhow::Result<()> {
                let rtv = r.begin()?;
                if let Some(slot) = self.session.as_ref().and_then(|s| s.current.clone()) {
                    r.draw_video(&slot);
                }
                gui.paint(&rtv, (r.width, r.height), &frame)?;
                r.present(game)
            })();
            if let Err(e) = res {
                tracing::warn!("render failed ({e:#}); recreating device");
                failed = true;
            }
        }
        self.gui = Some(gui);
        if failed {
            self.recreate_device();
        }

        if fresh {
            if let Some(s) = &self.session {
                let render_ms = t.elapsed().as_secs_f32() * 1000.0;
                let lat = s.current.as_ref().map(|c| s.stats.latency_ms(c.capture_ts));
                s.stats.with(|st| {
                    if st.total_rendered == 0 {
                        tracing::info!("first frame rendered ({render_ms:.1} ms)");
                    }
                    st.total_rendered += 1;
                    st.render_ms.push(render_ms);
                    st.frames_rendered += 1;
                    if let Some(l) = lat {
                        st.latency_ms.push(l);
                    }
                });
            }
        }
        self.repaint_at = (frame.repaint_after < Duration::from_secs(1)).then(|| Instant::now() + frame.repaint_after);
        if !actions.is_empty() {
            self.apply(actions);
            self.request_redraw();
        }
    }

    fn refresh_usb(&mut self) {
        if let Some(s) = &mut self.session {
            s.usb_devices = None;
        }
        let ui = self.ui_tx.clone();
        std::thread::spawn(move || ui.send(UiEvent::UsbDevices(crate::usb::list().map_err(|e| format!("{e:#}")))));
    }

    fn map_mouse(&self, x: f64, y: f64) -> Option<(u32, u32)> {
        let (vw, vh) = self.session.as_ref()?.video_size()?;
        let r = self.renderer.as_ref()?;
        let rect = fit(r.width, r.height, vw, vh);
        let nx = ((x - rect.x) / rect.w).clamp(0.0, 1.0);
        let ny = ((y - rect.y) / rect.h).clamp(0.0, 1.0);
        Some(((nx * 65535.0).round() as u32, (ny * 65535.0).round() as u32))
    }

    /// Switch between the remote cursor and a normal arrow over the toolbar.
    fn update_cursor_over_ui(&mut self, over: bool) {
        if over == self.cursor_over_ui {
            return;
        }
        self.cursor_over_ui = over;
        let (Some(w), Some(s)) = (&self.window, &self.session) else { return };
        if over {
            w.set_cursor(CursorIcon::Default);
            w.set_cursor_visible(true);
        } else if !s.relative {
            if let Some(c) = s.cursors.get(&s.cursor_shape) {
                w.set_cursor(c.clone());
            }
            w.set_cursor_visible(s.cursor_visible);
        }
    }

    fn on_cursor(&mut self, el: &ActiveEventLoop, m: pb::CursorMsg) {
        let Some(s) = self.session.as_mut() else { return };
        match m.msg {
            Some(cursor_msg::Msg::Shape(sh)) => {
                let src = CustomCursor::from_rgba(
                    sh.rgba,
                    sh.width.min(u16::MAX as u32) as u16,
                    sh.height.min(u16::MAX as u32) as u16,
                    sh.hot_x.clamp(0, sh.width as i32 - 1) as u16,
                    sh.hot_y.clamp(0, sh.height as i32 - 1) as u16,
                );
                match src {
                    Ok(src) => {
                        s.cursors.insert(sh.id, el.create_custom_cursor(src));
                    }
                    Err(e) => tracing::debug!("cursor shape: {e}"),
                }
            }
            Some(cursor_msg::Msg::State(st)) => {
                let Some(w) = &self.window else { return };
                if st.shape_id != s.cursor_shape {
                    if let Some(c) = s.cursors.get(&st.shape_id) {
                        if !self.cursor_over_ui {
                            w.set_cursor(c.clone());
                        }
                        s.cursor_shape = st.shape_id;
                    }
                }
                if st.visible != s.cursor_visible {
                    s.cursor_visible = st.visible;
                    if !s.relative && !self.cursor_over_ui {
                        w.set_cursor_visible(st.visible);
                    }
                }
            }
            None => {}
        }
    }

    fn hotkey_from_key(&self, event: &winit::event::KeyEvent) -> Option<Hotkey> {
        use winit::keyboard::{KeyCode, PhysicalKey};
        let m = self.mods;
        if event.state != ElementState::Pressed || !(m.control_key() && m.alt_key() && m.shift_key()) {
            return None;
        }
        let PhysicalKey::Code(c) = event.physical_key else { return None };
        Some(match c {
            KeyCode::KeyQ => Hotkey::ToggleGrab,
            KeyCode::KeyS => Hotkey::ToggleStats,
            KeyCode::KeyM => Hotkey::ToggleMode,
            KeyCode::KeyR => Hotkey::ToggleRelative,
            KeyCode::KeyF => Hotkey::ToggleFullscreen,
            KeyCode::KeyD => Hotkey::CtrlAltDel,
            KeyCode::KeyT => Hotkey::ToggleToolbar,
            KeyCode::KeyX => Hotkey::Quit,
            KeyCode::Digit1 => Hotkey::Display(1),
            KeyCode::Digit2 => Hotkey::Display(2),
            KeyCode::Digit3 => Hotkey::Display(3),
            KeyCode::Digit4 => Hotkey::Display(4),
            _ => return None,
        })
    }

    /// Mouse / keyboard events while a session is active.
    fn session_input(&mut self, event: &WindowEvent) {
        let over_ui = self.gui.as_ref().is_some_and(|g| g.ctx.is_pointer_over_area() || g.ctx.is_using_pointer());
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.update_cursor_over_ui(over_ui);
                let relative = self.session.as_ref().is_some_and(|s| s.relative);
                if !over_ui && !relative && self.focused {
                    if let Some((x, y)) = self.map_mouse(position.x, position.y) {
                        if let Some(s) = &self.session {
                            s.send_input(Ev::MouseAbs(pb::MouseAbs { x, y }));
                        }
                    }
                }
                // Keep the toolbar handle responsive near the top edge.
                if position.y < 60.0 || over_ui {
                    self.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let (b, bit) = match button {
                    MouseButton::Left => (pb::MouseButton::Left, 1),
                    MouseButton::Right => (pb::MouseButton::Right, 2),
                    MouseButton::Middle => (pb::MouseButton::Middle, 4),
                    MouseButton::Back => (pb::MouseButton::X1, 8),
                    MouseButton::Forward => (pb::MouseButton::X2, 16),
                    MouseButton::Other(_) => return,
                };
                let down = *state == ElementState::Pressed;
                // Presses on the toolbar stay local; a release always follows its press.
                let forward = if down { !over_ui } else { self.remote_buttons & bit != 0 };
                if forward {
                    if down {
                        self.remote_buttons |= bit;
                    } else {
                        self.remote_buttons &= !bit;
                    }
                    if let Some(s) = &self.session {
                        s.send_input(Ev::MouseButton(pb::MouseButtonEv { button: b as i32, down }));
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } if !over_ui => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => ((x * 120.0) as i32, (y * 120.0) as i32),
                    MouseScrollDelta::PixelDelta(p) => (p.x as i32, p.y as i32),
                };
                if dx != 0 || dy != 0 {
                    if let Some(s) = &self.session {
                        s.send_input(Ev::Wheel(pb::Wheel { dx, dy }));
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let Some(s) = &mut self.session {
                    s.winit_keys += 1;
                }
                if let Some(h) = self.hotkey_from_key(event) {
                    self.hotkey(h);
                    return;
                }
                // Keys the hook forwarded were swallowed and never reach the window, so
                // anything arriving here still has to go to the host (cloud desktops
                // deliver no keys to low-level hooks at all).
                if input::grabbed() && self.focused {
                    use winit::platform::scancode::PhysicalKeyExtScancode;
                    if let (Some(sc), Some(s)) = (event.physical_key.to_scancode(), &self.session) {
                        let (scancode, extended) = (sc & 0xff, sc & 0xff00 == 0xe000);
                        if scancode != 0 {
                            s.send_input(Ev::Key(pb::Key { scancode, extended, down: event.state == ElementState::Pressed }));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

impl ApplicationHandler<UiEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title("NyaRemoteControl").with_inner_size(LogicalSize::new(1100.0, 760.0));
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                crate::fatal(&format!("无法创建窗口：{e}"));
                el.exit();
                return;
            }
        };
        self.window = Some(window.clone());
        let dev = match device_for_window(&window) {
            Ok(d) => d,
            Err(e) => {
                crate::fatal(&format!("无法创建 D3D11 设备：{e:#}"));
                el.exit();
                return;
            }
        };
        if let Err(e) = self.create_renderer(dev) {
            crate::fatal(&format!("无法初始化渲染：{e:#}"));
            el.exit();
            return;
        }
        tracing::info!("renderer on adapter luid {:#x}", self.adapter_luid);
        if let Some(h) = hwnd(&window) {
            input::install(h, self.ui_tx.clone());
        }
        if let Some((target, name)) = self.auto_connect.take() {
            self.connect(target, name);
        }
        self.draw();
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else { return };
        // Keyboard goes to egui only in the launcher (in a session it belongs to the host).
        let keyboard = matches!(event, WindowEvent::KeyboardInput { .. } | WindowEvent::ModifiersChanged(_) | WindowEvent::Ime(_));
        if self.session.is_none() || !keyboard {
            if let Some(g) = self.gui.as_mut() {
                let r = g.on_event(&window, &event);
                if r.repaint && self.session.is_none() {
                    window.request_redraw();
                }
            }
        }
        match &event {
            WindowEvent::CloseRequested => {
                if let Some(s) = &self.session {
                    s.quit();
                }
                self.exit = true;
                el.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    if let Err(e) = r.resize(size.width, size.height) {
                        tracing::warn!("resize: {e:#}");
                    }
                }
                if self.session.as_ref().is_some_and(|s| s.vd_follow_window) {
                    // Wait until resizing stops: a size the driver does not offer yet restarts it.
                    self.vd_resize_at = Some(Instant::now() + Duration::from_millis(800));
                }
                self.draw();
            }
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Focused(f) => {
                self.focused = *f;
                self.update_no_hotkeys();
                if let Some(g) = self.session.as_ref().and_then(|s| s.gamepads.as_ref()) {
                    g.set_active(*f);
                }
                if !*f {
                    input::reset_modifiers();
                    if let Some(s) = &self.session {
                        s.release_all();
                    }
                    self.remote_buttons = 0;
                    if self.session.as_ref().is_some_and(|s| s.relative) {
                        self.set_relative(false);
                    }
                }
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::HoveredFile(_) if self.session.is_some() => {
                self.hovering_file = true;
                window.request_redraw();
            }
            WindowEvent::HoveredFileCancelled => {
                self.hovering_file = false;
                window.request_redraw();
            }
            WindowEvent::DroppedFile(p) if self.session.is_some() => {
                self.hovering_file = false;
                self.dropped.push(p.clone());
            }
            WindowEvent::Moved(_) => {
                // Moving to a monitor on another GPU: follow it (design doc §3.5, client side).
                if let (Some(m), Ok(topo)) = (window.current_monitor(), Topology::enumerate()) {
                    if let Some(a) = topo.adapter_for_monitor(m.hmonitor()) {
                        if a.luid != self.adapter_luid && self.adapter_luid != 0 {
                            tracing::info!("window moved to GPU {}; recreating device", a.name);
                            self.recreate_device();
                        }
                    }
                }
            }
            _ => {}
        }
        if self.session.is_some() {
            self.session_input(&event);
        }
    }

    fn device_event(&mut self, _el: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = event {
            if let Some(s) = &self.session {
                if s.relative && self.focused {
                    let (dx, dy) = (delta.0.round() as i32, delta.1.round() as i32);
                    if dx != 0 || dy != 0 {
                        s.send_input(Ev::MouseRel(pb::MouseRel { dx, dy }));
                    }
                }
            }
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, event: UiEvent) {
        match event {
            UiEvent::Frame => return self.draw(),
            UiEvent::Cursor(m) => return self.on_cursor(el, m),
            UiEvent::ConnectDone(d) => self.on_connect_done(d),
            UiEvent::NeedPairing(tx) => {
                self.pair_reply = Some(tx);
                self.launcher.pairing = Some(PairingDialog { code: String::new() });
            }
            UiEvent::Hotkey(h) => self.hotkey(h),
            UiEvent::Disconnected(msg) => self.end_session(Some(Notice::Error(msg))),
            UiEvent::FileOffer(o) => {
                if let Some(s) = &mut self.session {
                    s.on_offer(o);
                }
            }
            UiEvent::FileResult(r) => {
                if let Some(s) = &mut self.session {
                    s.on_file_result(r);
                }
            }
            UiEvent::Transfer(u) => {
                if let Some(s) = &mut self.session {
                    s.on_transfer(u);
                }
            }
            UiEvent::UsbStatus(st) => {
                if let Some(s) = &mut self.session {
                    s.on_usb_status(st);
                }
            }
            UiEvent::UsbipdInstall(running, msg) => {
                let done = !running;
                if let Some(s) = &mut self.session {
                    s.usbipd_install = Some((running, msg));
                }
                if done {
                    self.refresh_usb();
                }
            }
            UiEvent::UsbDevices(r) => {
                if let Some(s) = &mut self.session {
                    s.usb_devices = Some(r);
                }
            }
            UiEvent::ClipboardImage(dib) => {
                if let Some(s) = &self.session {
                    s.clipboard_image_from_host(dib);
                }
            }
            other => {
                let Some(s) = self.session.as_mut() else { return };
                match other {
                    UiEvent::Connected => s.status.clear(),
                    UiEvent::SessionInfo(i) => s.on_session_info(i, self.focused),
                    UiEvent::GamepadRumble(r) => {
                        if let Some(g) = &s.gamepads {
                            g.rumble(&r);
                        }
                    }
                    UiEvent::StreamStarted(st) => {
                        tracing::info!(
                            "stream: {} {:?} cross_gpu={}",
                            st.encoder_name,
                            st.config.as_ref().map(|c| (c.width, c.height, c.fps, c.bitrate_kbps)),
                            st.cross_gpu
                        );
                        s.game = st.config.as_ref().is_some_and(|c| c.mode == pb::StreamMode::Game as i32);
                        s.stream = Some(st);
                        if s.status_until.is_none() {
                            s.status.clear();
                        }
                        s.cursor_shape = 0;
                    }
                    UiEvent::StreamError(e) if e.starts_with("虚拟显示器") => {
                        tracing::error!("{e}");
                        s.virtual_display_failed(&e);
                    }
                    UiEvent::StreamError(e) => {
                        tracing::error!("stream error: {e}");
                        s.status = format!("被控端无法开始推流：{e}");
                    }
                    UiEvent::ServerStats(st) => s.server_stats = Some(st),
                    UiEvent::Clipboard(t) => s.clipboard_from_host(t),
                    UiEvent::Reconnecting(msg) => {
                        s.status = format!("连接中断，正在重连…（{msg}）");
                        s.release_all();
                    }
                    _ => {}
                }
                self.update_title();
            }
        }
        self.request_redraw();
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.exit {
            el.exit();
            return;
        }
        let mut redraw = false;
        // One drop delivers one DroppedFile event per file; send them as a batch.
        if !self.dropped.is_empty() {
            let files = std::mem::take(&mut self.dropped);
            if let Some(s) = &mut self.session {
                s.send_files(files);
            }
            redraw = true;
        }
        if let Some(s) = &mut self.session {
            if s.tick() {
                redraw = s.show_stats;
                self.update_title();
            }
        }
        if self.repaint_at.is_some_and(|t| Instant::now() >= t) {
            self.repaint_at = None;
            redraw = true;
        }
        if self.vd_resize_at.is_some_and(|t| Instant::now() >= t) {
            self.vd_resize_at = None;
            self.fit_virtual_display();
            redraw = true;
        }
        if redraw {
            self.request_redraw();
        }
        let next = Instant::now() + Duration::from_millis(250);
        let wake = self.repaint_at.map_or(next, |t| t.min(next));
        let wake = self.vd_resize_at.map_or(wake, |t| t.min(wake));
        el.set_control_flow(ControlFlow::WaitUntil(wake));
    }
}
