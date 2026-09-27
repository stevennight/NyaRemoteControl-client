//! NyaRemoteControl Windows client.

mod app;
mod audio;
mod caps;
mod clipboard;
mod config;
mod diag;
mod events;
mod input;
mod launcher;
mod net;
mod overlay;
mod render;
mod stats;
mod video;

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use nya_proto::pb;
use nya_transport::{Fingerprint, Identity};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use winit::event_loop::EventLoop;

use crate::config::{ClientConfig, HostEntry};
use crate::events::{Ui, UiEvent};

#[derive(Parser)]
#[command(name = "nya-client", version, about = "NyaRemoteControl 客户端")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// 连接被控端（已保存的名称或地址）
    Connect {
        target: String,
        /// 保存时使用的名称
        #[arg(long)]
        name: Option<String>,
        /// office | game
        #[arg(long)]
        mode: Option<String>,
        /// 被控端显示器编号（0 = 主显示器）
        #[arg(long)]
        display: Option<u32>,
        #[arg(long)]
        fullscreen: bool,
        /// auto | nvenc | qsv | amf | software
        #[arg(long)]
        encoder: Option<String>,
        /// auto | h264 | hevc | av1
        #[arg(long)]
        codec: Option<String>,
        /// auto | 420 | 444
        #[arg(long)]
        chroma: Option<String>,
        /// 码率 kbit/s（0 = 自动）
        #[arg(long)]
        bitrate: Option<u32>,
        /// 禁用硬件解码
        #[arg(long)]
        sw_decode: bool,
    },
    /// 列出 / 删除保存的被控端
    Hosts {
        #[arg(long)]
        remove: Option<String>,
    },
    /// 诊断：显卡、硬件解码能力、音频
    Diag,
}

fn init_logging(dir: &std::path::Path) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter = tracing_subscriber::EnvFilter::try_from_env("NYA_LOG").unwrap_or_else(|_| "info".into());
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("client")
        .filename_suffix("log")
        .max_log_files(7)
        .build(dir.join("logs"))
        .ok();
    let (file, guard) = match appender {
        Some(a) => {
            let (nb, g) = tracing_appender::non_blocking(a);
            (Some(tracing_subscriber::fmt::layer().with_writer(nb).with_ansi(false)), Some(g))
        }
        None => (None, None),
    };
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(file)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .try_init();
    guard
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

fn main() {
    if let Err(e) = real_main() {
        eprintln!("错误：{e:#}");
        wait_key();
        std::process::exit(1);
    }
}

/// Keep the console open when launched by double-click.
fn wait_key() {
    if std::env::args().len() <= 1 {
        eprintln!("按回车键退出");
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let dir = config::data_dir();
    std::fs::create_dir_all(&dir)?;
    let _log = init_logging(&dir);
    nya_media::check_runtime_versions()?;
    nya_media::init_log_level();
    let mut cfg = ClientConfig::load(&dir)?;

    let (target, name) = match cli.cmd {
        Some(Cmd::Diag) => return diag::run(),
        Some(Cmd::Hosts { remove }) => {
            if let Some(r) = remove {
                cfg.hosts.retain(|h| h.name != r && h.address != r);
                cfg.save(&dir)?;
            }
            for h in &cfg.hosts {
                println!("{}  {}  {}", h.name, h.address, if h.fingerprint.is_empty() { "未配对" } else { "已配对" });
            }
            return Ok(());
        }
        Some(Cmd::Connect { target, name, mode, display, fullscreen, encoder, codec, chroma, bitrate, sw_decode }) => {
            let d = &mut cfg.defaults;
            if let Some(m) = mode {
                d.mode = m;
            }
            if let Some(x) = display {
                d.display = x;
            }
            d.fullscreen |= fullscreen;
            if let Some(x) = encoder {
                d.encoder = x;
            }
            if let Some(x) = codec {
                d.codec = x;
            }
            if let Some(x) = chroma {
                d.chroma = x;
            }
            if let Some(x) = bitrate {
                d.bitrate_kbps = x;
            }
            if sw_decode {
                d.hw_decode = false;
            }
            (target, name)
        }
        None => (launcher::choose(&cfg).ok_or_else(|| anyhow!("没有选择被控端"))?, None),
    };

    let identity = Identity::load_or_create(&dir)?;
    let entry = cfg.find(&target).cloned();
    let address = entry.as_ref().map(|e| e.address.clone()).unwrap_or_else(|| target.clone());
    let addr = nya_transport::endpoint::resolve(&address, nya_proto::DEFAULT_PORT)?;
    let pinned = entry.as_ref().and_then(|e| Fingerprint::from_hex(&e.fingerprint));
    let client_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "nya-client".into());

    let rt = tokio::runtime::Runtime::new()?;
    println!("正在连接 {addr} …");
    let prompt: net::PairPrompt = Arc::new(launcher::pairing_code);
    let link = match rt.block_on(net::connect(addr, &identity, pinned, &client_name, Some(prompt.clone()))) {
        Ok(l) => l,
        Err(e) if pinned.is_some() && format!("{e:#}").contains(nya_transport::tls::PIN_MISMATCH) => {
            println!();
            println!("警告：被控端的证书和上次保存的不一样。");
            println!("  常见原因：被控端从开发模式改成了服务模式，或者重装过；也可能有人在冒充被控端。");
            if !launcher::confirm("是否重新验证这台被控端？(y/N)：") {
                return Err(anyhow!("已取消连接"));
            }
            let l = rt.block_on(net::connect(addr, &identity, None, &client_name, Some(prompt)))?;
            if l.welcome.needs_pairing {
                println!("已通过配对码验证了新的被控端证书。");
            } else {
                // The host already knows us, so no pairing code proved its identity: compare by eye.
                println!("新的证书指纹：{}", l.server_fp);
                println!("请在被控端运行 `nya-server pair`，核对其中显示的“证书指纹”。");
                if !launcher::confirm("两边的指纹一致吗？(y/N)：") {
                    return Err(anyhow!("指纹未确认，已取消连接"));
                }
            }
            l
        }
        Err(e) => return Err(e),
    };
    let server_name = link.welcome.server_name.clone();
    println!("已连接到 {server_name}（证书 {}）", link.server_fp);

    let label = name.or(entry.as_ref().map(|e| e.name.clone())).unwrap_or_else(|| server_name.clone());
    cfg.upsert(HostEntry { name: label.clone(), address: address.clone(), fingerprint: link.server_fp.to_hex() });
    cfg.save(&dir).context("save client.toml")?;

    let d = cfg.defaults.clone();
    let game = d.mode.eq_ignore_ascii_case("game");
    let start = pb::StartStream {
        display_id: d.display,
        config: Some(pb::StreamConfig {
            codec: parse_codec(&d.codec) as i32,
            chroma: parse_chroma(&d.chroma) as i32,
            width: 0,
            height: 0,
            fps: 0,
            bitrate_kbps: d.bitrate_kbps,
            mode: if game { pb::StreamMode::Game } else { pb::StreamMode::Office } as i32,
        }),
        encoder_preference: d.encoder.clone(),
    };

    let event_loop = EventLoop::<UiEvent>::with_user_event().build()?;
    let ui = Ui(event_loop.create_proxy());
    let (net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
    let (video_tx, video_rx) = crossbeam_channel::bounded(16);
    let (audio_tx, audio_rx) = crossbeam_channel::bounded(64);

    let params = net::Params {
        addr,
        pinned: link.server_fp,
        identity,
        name: client_name,
        caps: pb::ClientCaps::default(),
        start,
    };
    let startup = app::Startup {
        link,
        params,
        runtime: rt.handle().clone(),
        net_rx,
        video_rx,
        audio_rx,
        audio_tx,
        audio: d.audio,
        clipboard: d.clipboard,
        max_fps: d.max_fps,
    };
    let mut app = app::App::new(startup, ui, net_tx, video_tx, d.hw_decode, label, d.fullscreen);
    event_loop.run_app(&mut app)?;
    // Let the Bye go out.
    rt.block_on(async { tokio::time::sleep(std::time::Duration::from_millis(200)).await });
    if let Some(msg) = app.exit_message() {
        return Err(anyhow!("{msg}"));
    }
    Ok(())
}
