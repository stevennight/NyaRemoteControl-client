//! One connection to a host: worker threads, network supervisor, stream
//! state and statistics. Created when a connection succeeds, dropped when it
//! ends (the window then returns to the launcher).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use nya_proto::pb::{self, control_msg::Msg, input_msg::Ev};
use nya_win::d3d::D3dDevice;
use tokio::sync::mpsc::UnboundedSender;
use winit::window::CustomCursor;

use crate::clipboard::ClipIn;
use crate::events::{NetCmd, TransferUpdate, Ui};
use crate::net::{self, Link, Params, Sinks};
use crate::stats::{Shared, Summary};
use crate::video::{FrameStore, Slot, VideoIn, VideoThread};
use crate::input;

#[derive(Debug, Clone)]
pub enum TransferState {
    Running,
    Done(String),
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct TransferView {
    pub id: u64,
    pub upload: bool,
    pub name: String,
    pub done: u64,
    pub total: u64,
    pub state: TransferState,
    /// Local folder with the downloaded files.
    pub folder: Option<std::path::PathBuf>,
}

pub struct SessionOptions {
    pub hw_decode: bool,
    pub audio: bool,
    pub clipboard: bool,
}

pub struct Session {
    pub label: String,
    pub net_tx: UnboundedSender<NetCmd>,
    video_tx: Sender<VideoIn>,
    pub stats: Arc<Shared>,
    pub store: Arc<FrameStore>,
    clip_tx: Option<Sender<ClipIn>>,
    pub transfers: Vec<TransferView>,
    mic_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    pub offers: Vec<pb::FileOffer>,
    /// Current stream request (display, mode …), replayed on reconnect.
    pub start: pb::StartStream,
    pub status: String,
    pub info: Option<pb::SessionInfo>,
    pub stream: Option<pb::StreamStarted>,
    pub server_stats: Option<pb::ServerStats>,
    pub current: Option<Arc<Slot>>,
    pub cursors: HashMap<u32, CustomCursor>,
    pub cursor_shape: u32,
    pub cursor_visible: bool,
    pub relative: bool,
    pub game: bool,
    pub show_stats: bool,
    pub summary: Summary,
    last_tick: Instant,
    status_log: Instant,
    last_rendered_total: u64,
    pub winit_keys: u64,
    logged_keys: (u64, u64),
}

fn ctl(m: Msg) -> NetCmd {
    NetCmd::Control(pb::ControlMsg { msg: Some(m) })
}

impl Session {
    pub fn start(
        rt: &tokio::runtime::Handle,
        link: Link,
        params: Params,
        dev: &D3dDevice,
        opts: &SessionOptions,
        ui: Ui,
        label: String,
    ) -> Self {
        let (net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
        let (video_tx, video_rx) = crossbeam_channel::bounded(16);
        let (audio_tx, audio_rx) = crossbeam_channel::bounded(64);
        let stats = Arc::new(Shared::new());
        let store = Arc::new(FrameStore::default());
        let game = params.start.config.as_ref().is_some_and(|c| c.mode == pb::StreamMode::Game as i32);

        VideoThread {
            hw_allowed: opts.hw_decode,
            caps: params.caps.clone(),
            store: store.clone(),
            ui: ui.clone(),
            net: net_tx.clone(),
            stats: stats.clone(),
        }
        .spawn(dev.clone(), video_rx);
        if opts.audio {
            crate::audio::spawn(audio_rx);
        }
        let clip_tx = opts.clipboard.then(|| {
            let (tx, rx) = crossbeam_channel::unbounded();
            crate::clipboard::spawn(rx, net_tx.clone());
            tx
        });
        input::set_session(Some(net_tx.clone()));

        let start = params.start.clone();
        let sinks = Sinks { ui, video: video_tx.clone(), audio: audio_tx, stats: stats.clone() };
        rt.spawn(net::supervise(link, params, net_rx, sinks));

        Self {
            label,
            net_tx,
            video_tx,
            stats,
            store,
            clip_tx,
            transfers: Vec::new(),
            mic_stop: None,
            offers: Vec::new(),
            start,
            status: "连接中".into(),
            info: None,
            stream: None,
            server_stats: None,
            current: None,
            cursors: HashMap::new(),
            cursor_shape: 0,
            cursor_visible: true,
            relative: false,
            game,
            show_stats: false,
            summary: Summary::default(),
            last_tick: Instant::now(),
            status_log: Instant::now(),
            last_rendered_total: 0,
            winit_keys: 0,
            logged_keys: (0, 0),
        }
    }

    pub fn send_input(&self, ev: Ev) {
        let _ = self.net_tx.send(NetCmd::Input(pb::InputMsg { ev: Some(ev) }));
    }

    pub fn release_all(&self) {
        self.send_input(Ev::ReleaseAll(pb::ReleaseAll {}));
    }

    pub fn set_device(&self, dev: &D3dDevice) {
        let _ = self.video_tx.send(VideoIn::Device(dev.clone()));
    }

    pub fn set_game_mode(&mut self, game: bool) {
        if self.game == game {
            return;
        }
        self.game = game;
        let mode = if game { pb::StreamMode::Game } else { pb::StreamMode::Office } as i32;
        self.start.config.get_or_insert_with(Default::default).mode = mode;
        let _ = self.net_tx.send(ctl(Msg::SetMode(pb::SetMode { mode })));
        self.status = if game { "正在切换到游戏模式…" } else { "正在切换到办公模式…" }.into();
    }

    /// Switch to the host display with this id.
    pub fn select_display(&mut self, id: u32) {
        if self.stream.as_ref().is_some_and(|s| s.display_id == id) {
            return;
        }
        self.start.display_id = id;
        let _ = self.net_tx.send(ctl(Msg::StartStream(self.start.clone())));
        self.status = "正在切换显示器…".into();
    }

    /// 1-based index into the host's display list.
    pub fn select_display_index(&mut self, n: u8) {
        let id = self.info.as_ref().and_then(|s| s.displays.get(n as usize - 1)).map(|d| d.id);
        if let Some(id) = id {
            self.select_display(id);
        }
    }

    pub fn bitrate_policy(&self) -> i32 {
        self.start.config.as_ref().map(|c| c.bitrate_policy).unwrap_or(0)
    }

    /// Restarts the stream with the new policy.
    pub fn set_bitrate_policy(&mut self, p: pb::BitratePolicy) {
        if self.bitrate_policy() == p as i32 {
            return;
        }
        self.start.config.get_or_insert_with(Default::default).bitrate_policy = p as i32;
        let _ = self.net_tx.send(ctl(Msg::StartStream(self.start.clone())));
        self.status = "正在切换码率策略…".into();
    }

    pub fn mic_on(&self) -> bool {
        self.mic_stop.is_some()
    }

    /// Name of the host device that receives the microphone, if any.
    pub fn host_mic_device(&self) -> Option<&str> {
        self.info.as_ref().map(|i| i.mic_device.as_str()).filter(|n| !n.is_empty())
    }

    pub fn set_mic(&mut self, on: bool) {
        if on == self.mic_on() {
            return;
        }
        if let Some(stop) = self.mic_stop.take() {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if on {
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            crate::mic::spawn(self.net_tx.clone(), stop.clone());
            self.mic_stop = Some(stop);
        }
    }

    pub fn ctrl_alt_del(&self) {
        let _ = self.net_tx.send(ctl(Msg::SendSas(pb::SendSas {})));
    }

    pub fn clipboard_from_host(&self, text: String) {
        if let Some(tx) = &self.clip_tx {
            let _ = tx.send(ClipIn::Text(text));
        }
    }

    pub fn clipboard_image_from_host(&self, dib: Vec<u8>) {
        if let Some(tx) = &self.clip_tx {
            let _ = tx.send(ClipIn::Image(dib));
        }
    }

    pub fn send_files(&mut self, paths: Vec<std::path::PathBuf>) {
        if !paths.is_empty() {
            let _ = self.net_tx.send(NetCmd::SendFiles(paths));
        }
    }

    /// Download the files of a host offer.
    pub fn accept_offer(&mut self, id: u64) {
        let Some(i) = self.offers.iter().position(|o| o.transfer_id == id) else { return };
        let o = self.offers.remove(i);
        let total = o.files.iter().map(|f| f.size).sum();
        self.transfers.push(TransferView {
            id,
            upload: false,
            name: format!("{} 个文件", o.files.len()),
            done: 0,
            total,
            state: TransferState::Running,
            folder: None,
        });
        let _ = self.net_tx.send(ctl(Msg::FileRequest(pb::FileRequest { transfer_id: id })));
    }

    pub fn dismiss_offer(&mut self, id: u64) {
        self.offers.retain(|o| o.transfer_id != id);
    }

    pub fn dismiss_transfer(&mut self, id: u64) {
        self.transfers.retain(|t| t.id != id);
    }

    pub fn on_offer(&mut self, o: pb::FileOffer) {
        self.offers.retain(|x| x.transfer_id != o.transfer_id);
        self.offers.push(o);
        // Only the latest few matter.
        while self.offers.len() > 3 {
            self.offers.remove(0);
        }
    }

    pub fn on_transfer(&mut self, u: TransferUpdate) {
        let t = match self.transfers.iter_mut().find(|t| t.id == u.id) {
            Some(t) => t,
            None => {
                self.transfers.push(TransferView {
                    id: u.id,
                    upload: u.upload,
                    name: String::new(),
                    done: 0,
                    total: u.total,
                    state: TransferState::Running,
                    folder: None,
                });
                self.transfers.last_mut().unwrap()
            }
        };
        if !u.name.is_empty() {
            t.name = u.name;
        }
        t.done = t.done.max(u.done);
        if u.total > 0 {
            t.total = u.total;
        }
        if u.folder.is_some() {
            t.folder = u.folder;
        }
        match u.finished {
            Some(Ok(m)) => {
                t.state = TransferState::Done(m);
                t.done = t.total.max(t.done);
            }
            Some(Err(m)) => t.state = TransferState::Failed(m),
            None => {}
        }
    }

    /// The host's verdict on an upload (or a failed download).
    pub fn on_file_result(&mut self, r: pb::FileResult) {
        let state = if r.ok {
            TransferState::Done(if r.saved_to.is_empty() { r.message.clone() } else { format!("{}，位置：{}", r.message, r.saved_to) })
        } else {
            TransferState::Failed(r.message.clone())
        };
        match self.transfers.iter_mut().find(|t| t.id == r.transfer_id) {
            Some(t) => {
                t.state = state;
                if r.ok {
                    t.done = t.total.max(t.done);
                }
            }
            None => self.transfers.push(TransferView {
                id: r.transfer_id,
                upload: true,
                name: String::new(),
                done: 0,
                total: 0,
                state,
                folder: None,
            }),
        }
    }

    pub fn video_size(&self) -> Option<(u32, u32)> {
        if let Some(s) = &self.current {
            return Some((s.width, s.height));
        }
        let c = self.stream.as_ref()?.config.clone()?;
        Some((c.width, c.height))
    }

    /// Per-second bookkeeping: statistics, periodic diagnostics.
    pub fn tick(&mut self) -> bool {
        let secs = self.last_tick.elapsed().as_secs_f32();
        if secs < 1.0 {
            return false;
        }
        if self.stream.is_some() && self.status_log.elapsed() >= Duration::from_secs(5) {
            self.status_log = Instant::now();
            let (f, b, d, r) =
                self.stats.with(|s| (s.total_rx_frames, s.total_rx_bytes, s.total_decoded, s.total_rendered));
            if r == self.last_rendered_total {
                tracing::warn!("no new picture in 5 s: received {f} frames / {} KB, decoded {d}, rendered {r}", b / 1024);
            }
            self.last_rendered_total = r;
            let keys = (input::hook_key_count(), self.winit_keys);
            if keys != self.logged_keys {
                tracing::info!(
                    "keys so far: hook {} (hook calls {}) / window {} (grab {})",
                    keys.0,
                    input::hook_call_count(),
                    keys.1,
                    input::grabbed()
                );
                self.logged_keys = keys;
            }
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
        true
    }

    /// Lines for the statistics window.
    pub fn stats_lines(&self) -> Vec<String> {
        let s = &self.summary;
        let mut lines = Vec::new();
        if let Some(st) = &self.stream {
            let c = st.config.clone().unwrap_or_default();
            lines.push(format!(
                "编码 {} {} {}x{}@{}{}",
                st.encoder_name,
                if c.chroma == pb::Chroma::Yuv444 as i32 { "4:4:4" } else { "4:2:0" },
                c.width,
                c.height,
                c.fps,
                if st.cross_gpu { format!("  跨显卡 [{}]→[{}]", st.capture_gpu_index, st.encode_gpu_index) } else { String::new() }
            ));
        }
        let (sfps, skbps, enc_ms, xfer_ms, target, note) = self
            .server_stats
            .as_ref()
            .map(|x| (x.fps, x.bitrate_kbps, x.encode_ms_p50, x.transfer_ms_p50, x.target_kbps, x.bitrate_note.clone()))
            .unwrap_or_default();
        lines.push(format!("帧率  被控端 {sfps} / 本机 {}   丢帧 {}", s.fps, s.dropped));
        lines.push(format!("码率  实际 {:.1} Mbps   上限 {:.1} Mbps", skbps.max(s.kbps) as f32 / 1000.0, target as f32 / 1000.0));
        if !note.is_empty() {
            lines.push(format!("策略  {note}"));
        }
        lines.push(format!("延迟  端到端 {:.1} ms   RTT {:.1} ms", s.latency_ms, s.rtt_ms));
        lines.push(format!("耗时  编码 {enc_ms:.1}  跨显卡 {xfer_ms:.1}  解码 {:.1}  渲染 {:.1} ms", s.decode_ms, s.render_ms));
        lines.push(format!("解码器  {}", s.decoder));
        lines
    }

    pub fn quit(&self) {
        let _ = self.net_tx.send(NetCmd::Quit);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.set_mic(false);
        input::set_session(None);
        let _ = self.net_tx.send(NetCmd::Quit);
    }
}
