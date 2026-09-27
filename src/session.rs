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

use crate::events::{NetCmd, Ui};
use crate::net::{self, Link, Params, Sinks};
use crate::stats::{Shared, Summary};
use crate::video::{FrameStore, Slot, VideoIn, VideoThread};
use crate::input;

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
    clip_tx: Option<Sender<String>>,
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

    pub fn ctrl_alt_del(&self) {
        let _ = self.net_tx.send(ctl(Msg::SendSas(pb::SendSas {})));
    }

    pub fn clipboard_from_host(&self, text: String) {
        if let Some(tx) = &self.clip_tx {
            let _ = tx.send(text);
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
        let (sfps, skbps, enc_ms, xfer_ms) = self
            .server_stats
            .as_ref()
            .map(|x| (x.fps, x.bitrate_kbps, x.encode_ms_p50, x.transfer_ms_p50))
            .unwrap_or_default();
        lines.push(format!("帧率  被控端 {sfps} / 本机 {}   丢帧 {}", s.fps, s.dropped));
        lines.push(format!("码率  {:.1} Mbps", skbps.max(s.kbps) as f32 / 1000.0));
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
        input::set_session(None);
        let _ = self.net_tx.send(NetCmd::Quit);
    }
}
