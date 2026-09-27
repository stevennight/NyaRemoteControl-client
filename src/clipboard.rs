//! Clipboard sync (local side): text and images. Files are not synced
//! automatically (they can be large); they go through drag & drop / "发送文件".

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError};
use nya_proto::pb::{self, control_msg::Msg};
use nya_win::clipboard;
use tokio::sync::mpsc::UnboundedSender;

use crate::events::NetCmd;

const MAX_TEXT: usize = 1 << 20;
const MAX_IMAGE: usize = 64 << 20;

/// Clipboard content received from the host.
pub enum ClipIn {
    Text(String),
    Image(Vec<u8>),
}

fn hash(b: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

pub fn spawn(remote: Receiver<ClipIn>, net: UnboundedSender<NetCmd>) {
    std::thread::Builder::new()
        .name("nya-clipboard".into())
        .spawn(move || {
            let mut last_seq = clipboard::sequence_number();
            let mut last_text: Option<String> = None;
            let mut last_image: Option<u64> = None;
            loop {
                let applied = match remote.recv_timeout(Duration::from_millis(300)) {
                    Ok(ClipIn::Text(text)) => {
                        if last_text.as_deref() == Some(text.as_str()) {
                            false
                        } else {
                            let ok = clipboard::set_text(&text).is_ok();
                            last_text = Some(text);
                            ok
                        }
                    }
                    Ok(ClipIn::Image(dib)) => {
                        last_image = Some(hash(&dib));
                        clipboard::set_dib(&dib).is_ok()
                    }
                    Err(RecvTimeoutError::Timeout) => false,
                    Err(RecvTimeoutError::Disconnected) => return,
                };
                if applied {
                    last_seq = clipboard::sequence_number();
                }
                let seq = clipboard::sequence_number();
                if seq == last_seq {
                    continue;
                }
                last_seq = seq;
                let cmd = if clipboard::has_text() {
                    match clipboard::get_text() {
                        Ok(Some(text)) if text.len() <= MAX_TEXT && last_text.as_deref() != Some(text.as_str()) => {
                            last_text = Some(text.clone());
                            Some(NetCmd::Control(pb::ControlMsg { msg: Some(Msg::ClipboardText(pb::ClipboardText { text })) }))
                        }
                        _ => None,
                    }
                } else if clipboard::has_image() && !clipboard::has_files() {
                    match clipboard::get_dib() {
                        Ok(Some(dib)) if dib.len() <= MAX_IMAGE && last_image != Some(hash(&dib)) => {
                            last_image = Some(hash(&dib));
                            Some(NetCmd::SendImage(dib))
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(cmd) = cmd {
                    if net.send(cmd).is_err() {
                        return;
                    }
                }
            }
        })
        .expect("spawn clipboard thread");
}
