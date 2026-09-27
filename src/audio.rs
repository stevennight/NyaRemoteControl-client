//! Audio playback: Opus decode → jitter buffer → WASAPI.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError};
use nya_media::audio::OpusDecoder;
use nya_proto::frame::AudioPacket;
use nya_win::audio::AudioRenderer;

const SAMPLES_PER_MS: usize = 48 * 2;
/// Buffer this much before (re)starting playback.
const PREBUFFER_MS: usize = 30;
/// Beyond this, drop old audio to keep latency bounded.
const MAX_BUFFER_MS: usize = 120;
/// Keep roughly this much queued in the device.
const DEVICE_TARGET_MS: u32 = 20;

pub fn spawn(rx: Receiver<AudioPacket>) {
    std::thread::Builder::new()
        .name("nya-audio".into())
        .spawn(move || run(rx))
        .expect("spawn audio thread");
}

fn run(rx: Receiver<AudioPacket>) {
    nya_win::com_init();
    nya_win::mmcss_boost("Pro Audio");
    let mut decoder = match OpusDecoder::new() {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("opus decoder: {e:#}");
            return;
        }
    };
    let mut renderer: Option<AudioRenderer> = None;
    let mut retry_at = Instant::now();
    let mut buf: VecDeque<f32> = VecDeque::new();
    let mut scratch = Vec::new();
    let mut last_seq: Option<u32> = None;
    let mut playing = false;

    loop {
        match rx.recv_timeout(Duration::from_millis(3)) {
            Ok(p) => {
                // Conceal short losses with silence so timing stays right.
                if let Some(prev) = last_seq {
                    let gap = p.seq.wrapping_sub(prev).wrapping_sub(1);
                    if (1..=5).contains(&gap) {
                        buf.extend(std::iter::repeat(0.0).take(gap as usize * 10 * SAMPLES_PER_MS));
                    }
                }
                last_seq = Some(p.seq);
                scratch.clear();
                if let Err(e) = decoder.decode(&p.data, &mut scratch) {
                    tracing::debug!("opus decode: {e:#}");
                }
                buf.extend(scratch.iter().copied());
                for p in rx.try_iter() {
                    last_seq = Some(p.seq);
                    scratch.clear();
                    let _ = decoder.decode(&p.data, &mut scratch);
                    buf.extend(scratch.iter().copied());
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        if buf.len() > MAX_BUFFER_MS * SAMPLES_PER_MS {
            let excess = buf.len() - PREBUFFER_MS * SAMPLES_PER_MS;
            buf.drain(..excess - excess % 2);
        }

        if renderer.is_none() {
            if Instant::now() < retry_at {
                continue;
            }
            match AudioRenderer::new() {
                Ok(r) => renderer = Some(r),
                Err(e) => {
                    tracing::warn!("audio output unavailable: {e:#}");
                    retry_at = Instant::now() + Duration::from_secs(3);
                    continue;
                }
            }
        }
        if !playing {
            if buf.len() >= PREBUFFER_MS * SAMPLES_PER_MS {
                playing = true;
            } else {
                continue;
            }
        }
        let r = renderer.as_mut().unwrap();
        let queued = match r.queued_frames() {
            Ok(q) => q,
            Err(_) => {
                renderer = None; // device changed
                continue;
            }
        };
        let target = DEVICE_TARGET_MS * 48;
        if queued >= target {
            continue;
        }
        let want = ((target - queued) as usize * 2).min(buf.len());
        if want == 0 {
            if queued == 0 {
                playing = false; // underrun: prebuffer again
            }
            continue;
        }
        let chunk: Vec<f32> = buf.drain(..want - want % 2).collect();
        match r.write(&chunk) {
            Ok(n) => {
                // Put back what didn't fit.
                for &s in chunk[n * 2..].iter().rev() {
                    buf.push_front(s);
                }
            }
            Err(_) => renderer = None,
        }
    }
}
