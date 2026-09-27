//! Connection, handshake, pairing, and the long-running session with
//! automatic reconnection.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use crossbeam_channel::Sender;
use nya_proto::frame::{datagram_type, stream_type, AudioPacket, VideoFrameHeader};
use nya_proto::framing::{encode_varint, expect_msg, read_msg, read_varint, write_msg};
use nya_proto::negotiate::{self, LocalVersion, Negotiated};
use nya_proto::pb::{self, control_msg::Msg, Feature};
use nya_proto::{MAX_MESSAGE_LEN, MAX_VIDEO_FRAME_LEN};
use nya_transport::identity::peer_fingerprint;
use nya_transport::pairing::{self, PairingKey, Transcript};
use nya_transport::quinn::{Connection, Endpoint, RecvStream, SendStream};
use nya_transport::{Fingerprint, Identity};
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::events::{NetCmd, Ui, UiEvent};
use crate::stats::Shared;
use crate::video::VideoIn;

pub type PairPrompt = Arc<dyn Fn() -> Option<String> + Send + Sync>;

pub struct Link {
    pub endpoint: Endpoint,
    pub conn: Connection,
    pub send: SendStream,
    pub recv: RecvStream,
    pub neg: Negotiated,
    pub welcome: pb::Welcome,
    pub server_fp: Fingerprint,
}

fn ctl(m: Msg) -> pb::ControlMsg {
    pb::ControlMsg { msg: Some(m) }
}

/// Connect and complete the handshake (and pairing if the host asks for it).
pub async fn connect(
    addr: SocketAddr,
    id: &Identity,
    pinned: Option<Fingerprint>,
    client_name: &str,
    prompt: Option<PairPrompt>,
) -> Result<Link> {
    let endpoint = nya_transport::endpoint::client_endpoint(addr)?;
    let conn = timeout(Duration::from_secs(8), nya_transport::endpoint::connect(&endpoint, addr, id, pinned))
        .await
        .map_err(|_| anyhow!("连接 {addr} 超时（检查组网是否连通、被控端是否运行、防火墙 UDP 端口）"))??;
    let server_fp = peer_fingerprint(&conn).ok_or_else(|| anyhow!("被控端没有证书"))?;
    let (mut send, mut recv) = conn.open_bi().await?;

    let me = LocalVersion::current();
    write_msg(&mut send, &negotiate::hello(&me, client_name, env!("CARGO_PKG_VERSION"))).await?;
    let reply: pb::HelloReply = timeout(Duration::from_secs(10), expect_msg(&mut recv, MAX_MESSAGE_LEN))
        .await
        .context("等待被控端响应超时")??;
    let welcome = match reply.reply {
        Some(pb::hello_reply::Reply::Welcome(w)) => w,
        Some(pb::hello_reply::Reply::Reject(r)) => bail!("被控端拒绝连接：{}", r.message),
        None => bail!("被控端响应无法识别（版本差异过大？）"),
    };
    let neg = negotiate::accept_welcome(&welcome, &me).map_err(|e| anyhow!(e))?;

    if welcome.needs_pairing {
        let challenge: pb::ControlMsg = expect_msg(&mut recv, MAX_MESSAGE_LEN).await?;
        let Some(Msg::AuthChallenge(ch)) = challenge.msg else { bail!("expected AuthChallenge") };
        let Some(prompt) = prompt else {
            bail!("被控端要求重新配对（可能被控端重装或移除了本机），请重新运行并输入配对码");
        };
        let code = tokio::task::spawn_blocking(move || prompt()).await?.ok_or_else(|| anyhow!("已取消配对"))?;
        let key = PairingKey::from_code(&code).ok_or_else(|| anyhow!("配对码格式不正确"))?;
        let client_nonce = pairing::nonce();
        let t = Transcript {
            server_nonce: &ch.server_nonce,
            client_nonce: &client_nonce,
            server_fp,
            client_fp: id.fingerprint(),
        };
        write_msg(&mut send, &ctl(Msg::AuthResponse(pb::AuthResponse { client_nonce: client_nonce.to_vec(), mac: t.client_mac(&key) })))
            .await?;
        let res: pb::ControlMsg = expect_msg(&mut recv, MAX_MESSAGE_LEN).await?;
        let Some(Msg::AuthResult(r)) = res.msg else { bail!("expected AuthResult") };
        if !r.ok {
            bail!("配对失败：{}", r.message);
        }
        if !t.verify_server(&key, &r.server_mac) {
            bail!("被控端无法证明它知道配对码，已中止（可能存在中间人）");
        }
    } else if pinned.is_none() {
        tracing::warn!("被控端已认识本机但本机没有保存它的指纹；将信任并保存 {server_fp}");
    }
    Ok(Link { endpoint, conn, send, recv, neg, welcome, server_fp })
}

/// Everything needed to (re)start the session.
pub struct Params {
    pub addr: SocketAddr,
    pub pinned: Fingerprint,
    pub identity: Identity,
    pub name: String,
    pub caps: pb::ClientCaps,
    pub start: pb::StartStream,
}

pub struct Sinks {
    pub ui: Ui,
    pub video: Sender<VideoIn>,
    pub audio: Sender<AudioPacket>,
    pub stats: Arc<Shared>,
}

enum End {
    UserQuit,
    Fatal(String),
    Lost(String),
}

/// Run the session; reconnect for up to two minutes when the link drops.
pub async fn supervise(first: Link, mut p: Params, mut cmds: mpsc::UnboundedReceiver<NetCmd>, sinks: Sinks) {
    let mut link = Some(first);
    let mut lost_since: Option<Instant> = None;
    loop {
        let l = match link.take() {
            Some(l) => l,
            None => match connect(p.addr, &p.identity, Some(p.pinned), &p.name, None).await {
                Ok(l) => l,
                Err(e) => {
                    let since = *lost_since.get_or_insert_with(Instant::now);
                    if since.elapsed() > Duration::from_secs(120) {
                        sinks.ui.send(UiEvent::Disconnected(format!("无法重新连接：{e:#}")));
                        return;
                    }
                    sinks.ui.send(UiEvent::Reconnecting(format!("{e:#}")));
                    // Drain commands meanwhile; honour Quit.
                    let deadline = tokio::time::sleep(Duration::from_secs(2));
                    tokio::pin!(deadline);
                    loop {
                        tokio::select! {
                            _ = &mut deadline => break,
                            c = cmds.recv() => match c {
                                Some(NetCmd::Quit) | None => return,
                                Some(NetCmd::Control(m)) => track(&mut p, &m),
                                _ => {}
                            }
                        }
                    }
                    continue;
                }
            },
        };
        lost_since = None;
        let _ = sinks.video.send(VideoIn::Reset);
        match run(l, &mut p, &mut cmds, &sinks).await {
            End::UserQuit => return,
            End::Fatal(msg) => {
                sinks.ui.send(UiEvent::Disconnected(msg));
                return;
            }
            End::Lost(msg) => {
                tracing::warn!("connection lost: {msg}");
                sinks.ui.send(UiEvent::Reconnecting(msg));
            }
        }
    }
}

/// Keep the replayable state current.
fn track(p: &mut Params, m: &pb::ControlMsg) {
    match &m.msg {
        Some(Msg::StartStream(s)) => p.start = s.clone(),
        Some(Msg::SetMode(m)) => p.start.config.get_or_insert_with(Default::default).mode = m.mode,
        Some(Msg::ClientCaps(c)) => p.caps = c.clone(),
        _ => {}
    }
}

async fn run(link: Link, p: &mut Params, cmds: &mut mpsc::UnboundedReceiver<NetCmd>, sinks: &Sinks) -> End {
    let Link { endpoint: _endpoint, conn, mut send, mut recv, neg, .. } = link;
    sinks.ui.send(UiEvent::Connected);

    let setup = async {
        write_msg(&mut send, &ctl(Msg::ClientCaps(p.caps.clone()))).await?;
        write_msg(&mut send, &ctl(Msg::StartStream(p.start.clone()))).await?;
        let mut input = conn.open_uni().await?;
        input.set_priority(20)?;
        let mut prelude = Vec::new();
        encode_varint(stream_type::INPUT, &mut prelude);
        input.write_all(&prelude).await?;
        anyhow::Ok(input)
    };
    let mut input = match setup.await {
        Ok(i) => i,
        Err(e) => return End::Lost(format!("{e:#}")),
    };

    let files_on = neg.has(Feature::FileTransfer);
    let images_on = neg.has(Feature::ClipboardImage);
    let uni = tokio::spawn(accept_uni(
        conn.clone(),
        sinks.video.clone(),
        sinks.ui.clone(),
        sinks.stats.clone(),
        (files_on, images_on),
    ));
    let usb_on = neg.has(Feature::UsbRedirect);
    let bidi = tokio::spawn({
        let conn = conn.clone();
        async move {
            while let Ok((send, mut recv)) = conn.accept_bi().await {
                tokio::spawn(async move {
                    match (read_varint(&mut recv).await, read_varint(&mut recv).await) {
                        (Ok(Some(stream_type::TUNNEL)), Ok(Some(port))) if usb_on => {
                            if let Err(e) = crate::usb::tunnel(send, recv, port).await {
                                tracing::debug!("usb tunnel: {e:#}");
                            }
                        }
                        _ => {
                            let _ = recv.stop(0u32.into());
                        }
                    }
                });
            }
        }
    });
    let dgram = tokio::spawn(read_datagrams(conn.clone(), sinks.audio.clone(), neg.has(Feature::Audio)));
    let mut ping = tokio::time::interval(Duration::from_secs(1));
    let clipboard = neg.has(Feature::ClipboardText);

    let end = loop {
        tokio::select! {
            m = read_msg::<pb::ControlMsg, _>(&mut recv, MAX_MESSAGE_LEN) => {
                let m = match m {
                    Ok(Some(m)) => m,
                    Ok(None) => break End::Lost("被控端关闭了连接".into()),
                    Err(e) => break End::Lost(format!("{e}")),
                };
                match m.msg {
                    Some(Msg::SessionInfo(i)) => sinks.ui.send(UiEvent::SessionInfo(i)),
                    // The decoder notices the new stream id itself; frames may arrive first.
                    Some(Msg::StreamStarted(s)) => sinks.ui.send(UiEvent::StreamStarted(s)),
                    Some(Msg::StreamError(e)) => sinks.ui.send(UiEvent::StreamError(e.message)),
                    Some(Msg::DisplayChanged(d)) => tracing::info!("host displays changed: {} displays", d.displays.len()),
                    Some(Msg::ServerStats(s)) => sinks.ui.send(UiEvent::ServerStats(s)),
                    Some(Msg::ClipboardText(c)) if clipboard => sinks.ui.send(UiEvent::Clipboard(c.text)),
                    Some(Msg::Pong(p)) => sinks.stats.on_pong(p.t_us, p.server_t_us),
                    Some(Msg::FileOffer(o)) if files_on => sinks.ui.send(UiEvent::FileOffer(o)),
                    Some(Msg::FileResult(r)) => sinks.ui.send(UiEvent::FileResult(r)),
                    Some(Msg::UsbStatus(u)) => sinks.ui.send(UiEvent::UsbStatus(u)),
                    Some(Msg::Bye(b)) => break End::Fatal(format!("被控端断开：{}", b.reason)),
                    Some(other) => tracing::debug!("ignoring {other:?}"),
                    None => tracing::debug!("ignoring unknown control message"),
                }
            }
            c = cmds.recv() => match c {
                Some(NetCmd::Input(m)) => {
                    if let Err(e) = write_msg(&mut input, &m).await {
                        break End::Lost(format!("input: {e}"));
                    }
                }
                Some(NetCmd::Control(m)) => {
                    track(p, &m);
                    if matches!(m.msg, Some(Msg::ClipboardText(_))) && !clipboard {
                        continue;
                    }
                    if let Err(e) = write_msg(&mut send, &m).await {
                        break End::Lost(format!("control: {e}"));
                    }
                }
                Some(NetCmd::SendFiles(paths)) => {
                    if files_on {
                        tokio::spawn(crate::transfer::upload(conn.clone(), paths, sinks.ui.clone()));
                    } else {
                        sinks.ui.send(UiEvent::FileResult(pb::FileResult {
                            ok: false,
                            message: "被控端版本不支持文件传输，请升级被控端".into(),
                            ..Default::default()
                        }));
                    }
                }
                Some(NetCmd::Mic(d)) => {
                    if neg.has(Feature::Microphone) {
                        let _ = conn.send_datagram(d.into());
                    }
                }
                Some(NetCmd::SendImage(dib)) => {
                    if images_on {
                        tokio::spawn(crate::transfer::send_image(conn.clone(), dib));
                    }
                }
                Some(NetCmd::Quit) | None => {
                    let _ = write_msg(&mut send, &ctl(Msg::Bye(pb::Bye { reason: "用户断开".into() }))).await;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    conn.close(0u32.into(), b"bye");
                    break End::UserQuit;
                }
            },
            _ = ping.tick() => {
                let _ = write_msg(&mut send, &ctl(Msg::Ping(pb::Ping { t_us: nya_proto::now_us() }))).await;
            }
            e = conn.closed() => break End::Lost(format!("{e}")),
        }
    };
    uni.abort();
    bidi.abort();
    dgram.abort();
    end
}

async fn accept_uni(conn: Connection, video: Sender<VideoIn>, ui: Ui, stats: Arc<Shared>, (files_on, images_on): (bool, bool)) {
    let downloads = Arc::new(crate::transfer::Downloads::default());
    while let Ok(mut r) = conn.accept_uni().await {
        let (video, ui, stats, downloads) = (video.clone(), ui.clone(), stats.clone(), downloads.clone());
        tokio::spawn(async move {
            match read_varint(&mut r).await {
                Ok(Some(stream_type::FILE)) => crate::transfer::receive(r, ui, downloads, files_on, images_on).await,
                Ok(Some(stream_type::VIDEO)) => {
                    let Ok(Some(stream_id)) = read_varint(&mut r).await else { return };
                    tracing::info!("video stream {stream_id} opened by host");
                    loop {
                        let mut len = [0u8; 4];
                        if r.read_exact(&mut len).await.is_err() {
                            return;
                        }
                        let len = u32::from_le_bytes(len) as usize;
                        if len < VideoFrameHeader::LEN_V1 || len > MAX_VIDEO_FRAME_LEN {
                            tracing::warn!("bad video frame length {len}");
                            return;
                        }
                        let mut buf = vec![0u8; len];
                        if r.read_exact(&mut buf).await.is_err() {
                            return;
                        }
                        let first = stats.with(|s| {
                            s.bytes += len as u64;
                            s.total_rx_bytes += len as u64;
                            s.total_rx_frames += 1;
                            s.total_rx_frames == 1
                        });
                        if first {
                            tracing::info!("first video frame received: stream {stream_id}, {len} bytes");
                        }
                        if video.try_send(VideoIn::Frame { stream_id, buf }).is_err() {
                            tracing::warn!("decoder queue full; dropping frame");
                        }
                    }
                }
                Ok(Some(stream_type::CURSOR)) => {
                    while let Ok(Some(m)) = read_msg::<pb::CursorMsg, _>(&mut r, MAX_MESSAGE_LEN).await {
                        ui.send(UiEvent::Cursor(m));
                    }
                }
                Ok(Some(other)) => {
                    tracing::debug!("unknown stream type {other}");
                    let _ = r.stop(0u32.into());
                }
                _ => {}
            }
        });
    }
}

async fn read_datagrams(conn: Connection, audio: Sender<AudioPacket>, enabled: bool) {
    while let Ok(d) = conn.read_datagram().await {
        if !enabled || d.is_empty() {
            continue;
        }
        if d[0] == datagram_type::AUDIO {
            if let Some(p) = AudioPacket::decode(&d) {
                let _ = audio.try_send(p);
            }
        }
    }
}
