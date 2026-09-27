//! Text clipboard sync (local side).

use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError};
use nya_proto::pb::{self, control_msg::Msg};
use nya_win::clipboard;
use tokio::sync::mpsc::UnboundedSender;

use crate::events::NetCmd;

const MAX_TEXT: usize = 1 << 20;

/// `remote` delivers text received from the host.
pub fn spawn(remote: Receiver<String>, net: UnboundedSender<NetCmd>) {
    std::thread::Builder::new()
        .name("nya-clipboard".into())
        .spawn(move || {
            let mut last_seq = clipboard::sequence_number();
            let mut last_text: Option<String> = None;
            loop {
                match remote.recv_timeout(Duration::from_millis(300)) {
                    Ok(text) => {
                        if last_text.as_deref() != Some(text.as_str()) && clipboard::set_text(&text).is_ok() {
                            last_text = Some(text);
                            last_seq = clipboard::sequence_number();
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
                let seq = clipboard::sequence_number();
                if seq == last_seq {
                    continue;
                }
                last_seq = seq;
                if let Ok(Some(text)) = clipboard::get_text() {
                    if text.len() <= MAX_TEXT && last_text.as_deref() != Some(text.as_str()) {
                        last_text = Some(text.clone());
                        let msg = pb::ControlMsg { msg: Some(Msg::ClipboardText(pb::ClipboardText { text })) };
                        if net.send(NetCmd::Control(msg)).is_err() {
                            return;
                        }
                    }
                }
            }
        })
        .expect("spawn clipboard thread");
}
