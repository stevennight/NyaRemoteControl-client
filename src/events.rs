//! Messages between the network task, worker threads and the UI thread.

use nya_proto::pb;
use winit::event_loop::EventLoopProxy;

/// Commands for the network task.
pub enum NetCmd {
    Input(pb::InputMsg),
    Control(pb::ControlMsg),
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hotkey {
    ToggleGrab,
    ToggleStats,
    ToggleMode,
    ToggleRelative,
    ToggleFullscreen,
    CtrlAltDel,
    Display(u8),
    Quit,
}

/// Events delivered to the winit event loop.
pub enum UiEvent {
    Connected { server_name: String },
    SessionInfo(pb::SessionInfo),
    StreamStarted(pb::StreamStarted),
    StreamError(String),
    Cursor(pb::CursorMsg),
    ServerStats(pb::ServerStats),
    Clipboard(String),
    /// A new decoded frame is ready in the frame store.
    Frame,
    Reconnecting(String),
    Disconnected(String),
    Hotkey(Hotkey),
}

#[derive(Clone)]
pub struct Ui(pub EventLoopProxy<UiEvent>);

impl Ui {
    pub fn send(&self, ev: UiEvent) {
        let _ = self.0.send_event(ev);
    }
}
