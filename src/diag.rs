//! `nya-client diag`: GPUs, hardware decoders, audio output.

use nya_win::d3d::D3dDevice;
use nya_win::topology::Topology;

pub fn run() -> anyhow::Result<()> {
    nya_win::com_init();
    println!("== NyaRemoteControl 客户端诊断 ==");
    println!("版本 {} / 协议 {}.{}", env!("CARGO_PKG_VERSION"), nya_proto::PROTO_MAJOR, nya_proto::PROTO_MINOR);
    let (c, u) = nya_media::ffmpeg_versions();
    println!("FFmpeg avcodec {c} / avutil {u}");
    let topo = Topology::enumerate()?;
    for a in &topo.adapters {
        println!("\n[{}] {} vendor={:04x}{}", a.index, a.name, a.vendor_id, if a.software { " (软件)" } else { "" });
        for o in topo.outputs.iter().filter(|o| o.adapter_index == a.index) {
            println!("    显示器 {} {}x{} {}Hz", o.device_name, o.width(), o.height(), o.refresh_hz);
        }
        match D3dDevice::for_adapter(&a.adapter) {
            Ok(d) => {
                let hw = crate::caps::hardware_decoders(&d);
                println!("    硬件解码：{hw:?}");
            }
            Err(e) => println!("    !! D3D11 设备：{e:#}"),
        }
    }
    match nya_win::audio::AudioRenderer::new() {
        Ok(_) => println!("\n音频输出：OK"),
        Err(e) => println!("\n音频输出：!! {e:#}"),
    }
    Ok(())
}
