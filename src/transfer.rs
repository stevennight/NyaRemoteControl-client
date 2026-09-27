//! Client side of file transfer: uploads (drag & drop / "发送文件"),
//! downloads of files offered by the host, and clipboard images.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nya_proto::pb;
use nya_transport::files;
use nya_transport::quinn::{Connection, RecvStream};

use crate::events::{TransferUpdate, Ui, UiEvent};

/// Rate-limits progress events to the UI.
struct Progress {
    ui: Ui,
    update: TransferUpdate,
    last: Instant,
}

impl Progress {
    fn new(ui: Ui, id: u64, upload: bool, total: u64) -> Self {
        Self {
            ui,
            update: TransferUpdate { id, upload, name: String::new(), done: 0, total, finished: None, folder: None },
            last: Instant::now() - Duration::from_secs(1),
        }
    }

    fn add(&mut self, n: u64) {
        self.update.done += n;
        if self.last.elapsed() >= Duration::from_millis(100) {
            self.last = Instant::now();
            self.ui.send(UiEvent::Transfer(self.update.clone()));
        }
    }

    fn finish(mut self, r: Result<String, String>, folder: Option<PathBuf>) {
        self.update.finished = Some(r);
        self.update.folder = folder;
        self.ui.send(UiEvent::Transfer(self.update));
    }
}

pub fn new_id() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    nya_proto::now_us() ^ (N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) << 48)
}

/// Upload files; the host confirms with a FileResult when the batch is saved.
pub async fn upload(conn: Connection, paths: Vec<PathBuf>, ui: Ui) {
    let files: Vec<(PathBuf, u64)> = paths
        .into_iter()
        .filter_map(|p| match std::fs::metadata(&p) {
            Ok(m) if m.is_file() => Some((p, m.len())),
            _ => {
                tracing::info!("skipping {} (folder or unreadable)", p.display());
                None
            }
        })
        .collect();
    let id = new_id();
    let total: u64 = files.iter().map(|f| f.1).sum();
    let mut prog = Progress::new(ui.clone(), id, true, total);
    if files.is_empty() {
        prog.finish(Err("没有可发送的文件（暂不支持文件夹）".into()), None);
        return;
    }
    let count = files.len() as u32;
    for (i, (p, size)) in files.iter().enumerate() {
        prog.update.name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let h = pb::FileHeader {
            transfer_id: id,
            name: prog.update.name.clone(),
            size: *size,
            purpose: pb::FilePurpose::Save as i32,
            index: i as u32,
            count,
        };
        if let Err(e) = files::send_file(&conn, h, p, |n| prog.add(n)).await {
            prog.finish(Err(format!("发送 {} 失败：{e:#}", p.display())), None);
            return;
        }
    }
    // Sent; the host's FileResult reports where it was saved.
    prog.update.name = format!("{count} 个文件，等待被控端确认…");
    ui.send(UiEvent::Transfer(prog.update));
}

pub async fn send_image(conn: Connection, dib: Vec<u8>) {
    let h = pb::FileHeader {
        transfer_id: new_id(),
        name: "clipboard.dib".into(),
        size: dib.len() as u64,
        purpose: pb::FilePurpose::ClipboardImage as i32,
        index: 0,
        count: 1,
    };
    if let Err(e) = files::send_bytes(&conn, h, &dib).await {
        tracing::debug!("clipboard image: {e:#}");
    }
}

/// Files of a download batch received so far.
#[derive(Default)]
pub struct Downloads {
    batches: Mutex<HashMap<u64, (Vec<PathBuf>, u64)>>,
}

/// A FILE stream from the host (after the type varint).
pub async fn receive(mut r: RecvStream, ui: Ui, downloads: Arc<Downloads>, files_on: bool, images_on: bool) {
    let h = match files::read_header(&mut r).await {
        Ok(h) => h,
        Err(e) => return tracing::warn!("file header: {e:#}"),
    };
    match pb::FilePurpose::try_from(h.purpose).unwrap_or(pb::FilePurpose::Unspecified) {
        pb::FilePurpose::ClipboardImage if images_on => match files::receive_to_vec(&mut r, &h, files::MAX_IMAGE_BYTES).await {
            Ok(dib) => ui.send(UiEvent::ClipboardImage(dib)),
            Err(e) => tracing::debug!("clipboard image: {e:#}"),
        },
        pb::FilePurpose::Save if files_on => {
            let dir = nya_win::shell::receive_dir(None).unwrap_or_else(|| std::env::temp_dir().join("NyaRemoteControl"));
            let already = downloads.batches.lock().unwrap().get(&h.transfer_id).map(|b| b.1).unwrap_or(0);
            let mut prog = Progress::new(ui.clone(), h.transfer_id, false, 0);
            prog.update.name = h.name.clone();
            prog.update.done = already;
            let res = files::receive_to_dir(&mut r, &h, &dir, |n| prog.add(n)).await;
            match res {
                Ok(p) => {
                    let done = {
                        let mut b = downloads.batches.lock().unwrap();
                        let e = b.entry(h.transfer_id).or_default();
                        e.0.push(p);
                        e.1 += h.size;
                        if h.index + 1 >= h.count {
                            b.remove(&h.transfer_id).map(|x| x.0)
                        } else {
                            None
                        }
                    };
                    if let Some(paths) = done {
                        let n = paths.len();
                        let clip = tokio::task::spawn_blocking(move || nya_win::clipboard::set_files(&paths)).await;
                        let note = if matches!(clip, Ok(Ok(()))) { "，已放入剪贴板，可直接粘贴" } else { "" };
                        prog.finish(Ok(format!("已下载 {n} 个文件{note}")), Some(dir));
                    }
                }
                Err(e) => {
                    downloads.batches.lock().unwrap().remove(&h.transfer_id);
                    prog.finish(Err(format!("下载 {} 失败：{e:#}", h.name)), None);
                }
            }
        }
        _ => {
            let _ = r.stop(0u32.into());
        }
    }
}
