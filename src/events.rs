//! Messages between the network task, worker threads and the UI thread.

use nya_proto::pb;
use winit::event_loop::EventLoopProxy;

use crate::net::Link;

/// Commands for the network task.
pub enum NetCmd {
    Input(pb::InputMsg),
    Control(pb::ControlMsg),
    /// Upload files to the host's Downloads folder.
    SendFiles(Vec<std::path::PathBuf>),
    /// Local clipboard image (CF_DIB) for the host.
    SendImage(Vec<u8>),
    /// Encoded MIC datagram.
    Mic(Vec<u8>),
    Quit,
}

/// Progress / outcome of one file transfer batch.
#[derive(Debug, Clone)]
pub struct TransferUpdate {
    pub id: u64,
    pub upload: bool,
    /// Current file name (or a summary).
    pub name: String,
    pub done: u64,
    pub total: u64,
    /// Some(Ok(message)) / Some(Err(message)) when finished.
    pub finished: Option<Result<String, String>>,
    /// Folder the files ended up in (downloads).
    pub folder: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hotkey {
    ToggleGrab,
    ToggleStats,
    ToggleMode,
    ToggleRelative,
    ToggleFullscreen,
    CtrlAltDel,
    ToggleToolbar,
    Display(u8),
    Quit,
}

/// Result of a connection attempt started from the launcher.
pub struct ConnectDone {
    pub attempt: u64,
    pub result: Result<Box<Link>, String>,
    /// The failure was a changed server certificate.
    pub pin_mismatch: bool,
}

/// Events delivered to the winit event loop.
pub enum UiEvent {
    Connected,
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
    ConnectDone(ConnectDone),
    FileOffer(pb::FileOffer),
    FileResult(pb::FileResult),
    Transfer(TransferUpdate),
    /// Clipboard image from the host.
    ClipboardImage(Vec<u8>),
    /// The host wants a pairing code; answer through the sender (None = cancel).
    NeedPairing(std::sync::mpsc::Sender<Option<String>>),
}

#[derive(Clone)]
pub struct Ui(pub EventLoopProxy<UiEvent>);

impl Ui {
    pub fn send(&self, ev: UiEvent) {
        let _ = self.0.send_event(ev);
    }
}
