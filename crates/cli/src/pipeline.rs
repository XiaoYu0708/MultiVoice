//! 音訊管線：
//!
//! ```text
//! 麥克風 → [擷取執行緒] 降噪 → VAD 閘門 → Opus 編碼 → client.send_voice
//! client.recv_voice → [tokio task] 每位說話者的 jitter buffer
//! [播放執行緒] 各 jitter buffer → 各自的 Opus 解碼器 → 混音 → 耳機
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use audio::codec::{Decoder, Encoder, FRAME_SAMPLES, MAX_PACKET, SAMPLE_RATE};
use audio::denoise::Denoiser;
use audio::device::{self, StreamStats};
use client_core::{Client, VoiceDown};
use mixer_core::jitter::{JitterBuffer, Pull};
use mixer_core::mix::{mix_into, soft_clip};
use protocol::SessionIdx;
use protocol::voice::Kind;

#[derive(Clone)]
pub struct AudioOpts {
    pub input: Option<String>,
    pub output: Option<String>,
    pub open_mic: bool,
    pub denoise: bool,
    pub mix: f32,
    pub bitrate: i32,
    pub buffer_ms: u32,
    pub volume: f32,
}

impl Default for AudioOpts {
    fn default() -> Self {
        Self { input: None, output: None, open_mic: false, denoise: true, mix: 0.3, bitrate: 32_000, buffer_ms: 30, volume: 1.0 }
    }
}

/// VAD：RNNoise 人聲機率 + 降噪後音量（PLAN.md：人聲機率不能單獨使用）
const VAD_PROB: f32 = 0.6;
const VAD_MIN_DBFS: f32 = -50.0;
/// 講完後繼續送出的時間
const HANGOVER_FRAMES: u32 = 15; // 300 ms

pub struct Shared {
    pub stop: AtomicBool,
    pub muted: AtomicBool,
    pub tx_active: AtomicBool,
    epoch: Instant,
    jitter: Mutex<HashMap<SessionIdx, JitterBuffer>>,
    last_heard: Mutex<HashMap<SessionIdx, Instant>>,
    out_stats: Arc<StreamStats>,
    underrun_base: AtomicU64,
}

#[derive(Default)]
pub struct JitterSummary {
    pub target: String,
    pub fec: u64,
    pub plc: u64,
    pub skipped: u64,
    pub late: u64,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            stop: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            tx_active: AtomicBool::new(false),
            epoch: Instant::now(),
            jitter: Default::default(),
            last_heard: Default::default(),
            out_stats: Arc::new(StreamStats::default()),
            underrun_base: AtomicU64::new(0),
        }
    }

    fn now_us(&self) -> u64 {
        self.epoch.elapsed().as_micros() as u64
    }

    /// 最近 `within` 內有收到聲音的成員
    pub fn speaking(&self, within: Duration) -> Vec<SessionIdx> {
        let lh = self.last_heard.lock().unwrap();
        let mut v: Vec<_> = lh.iter().filter(|(_, t)| t.elapsed() < within).map(|(i, _)| *i).collect();
        v.sort();
        v
    }

    pub fn forget(&self, idx: SessionIdx) {
        self.jitter.lock().unwrap().remove(&idx);
        self.last_heard.lock().unwrap().remove(&idx);
    }

    pub fn jitter_summary(&self) -> JitterSummary {
        let jbs = self.jitter.lock().unwrap();
        let mut s = JitterSummary::default();
        let mut targets = Vec::new();
        for jb in jbs.values() {
            s.fec += jb.stats.fec;
            s.plc += jb.stats.plc;
            s.skipped += jb.stats.skipped;
            s.late += jb.stats.late;
            targets.push(format!("{}ms", jb.target_ms()));
        }
        s.target = if targets.is_empty() { "—".into() } else { targets.join("/") };
        s
    }

    pub fn underruns(&self) -> u64 {
        self.out_stats.xruns.load(Ordering::Relaxed) - self.underrun_base.load(Ordering::Relaxed)
    }
}

/// 持有音訊串流與執行緒；drop 時停止
pub struct AudioHandle {
    _in: cpal_stream::Stream,
    _out: cpal_stream::Stream,
    pub input_name: String,
    pub output_name: String,
}

mod cpal_stream {
    /// 只是為了不在這個 crate 直接依賴 cpal：audio::device 回傳的串流型別
    pub type Stream = Box<dyn std::any::Any>;
}

pub fn start(client: Client, shared: Arc<Shared>, o: &AudioOpts) -> Result<AudioHandle> {
    let in_dev = device::find_device(true, o.input.as_deref()).map_err(anyhow::Error::from_boxed)?;
    let out_dev = device::find_device(false, o.output.as_deref()).map_err(anyhow::Error::from_boxed)?;

    let cap = SAMPLE_RATE as usize / 5; // 200 ms
    let (in_prod, in_cons) = rtrb::RingBuffer::<f32>::new(cap);
    let (out_prod, out_cons) = rtrb::RingBuffer::<f32>::new(cap);

    let (in_stream, _) = device::open_input(&in_dev, in_prod, Arc::new(StreamStats::default()), None)
        .map_err(anyhow::Error::from_boxed)?;
    let (out_stream, _) = device::open_output(&out_dev, out_cons, shared.out_stats.clone(), None, o.volume)
        .map_err(anyhow::Error::from_boxed)?;

    spawn_capture(client.clone(), shared.clone(), in_cons, o.clone())?;
    spawn_receive(client, shared.clone());
    spawn_playback(shared, out_prod, cap, o.buffer_ms);

    Ok(AudioHandle {
        _in: Box::new(in_stream),
        _out: Box::new(out_stream),
        input_name: device::device_name(&in_dev),
        output_name: device::device_name(&out_dev),
    })
}

fn spawn_capture(client: Client, shared: Arc<Shared>, mut ring: rtrb::Consumer<f32>, o: AudioOpts) -> Result<()> {
    let mut enc = Encoder::new(o.bitrate)?;
    let mut denoiser = o.denoise.then(|| {
        let mut d = Denoiser::new();
        d.set_speech_mix(o.mix);
        d
    });
    std::thread::Builder::new().name("capture".into()).spawn(move || {
        let mut frame = Vec::with_capacity(FRAME_SAMPLES);
        let mut pkt = [0u8; MAX_PACKET];
        let mut prev: Option<Vec<u8>> = None; // 前一幀（開口時補送，避免切掉字頭）
        let mut seq: u32 = 0;
        let mut hang = 0u32;
        let mut active = false;
        while !shared.stop.load(Ordering::Relaxed) {
            while let Ok(s) = ring.pop() {
                frame.push(s);
                if frame.len() < FRAME_SAMPLES {
                    continue;
                }
                let vad = denoiser.as_mut().map(|d| d.process(&mut frame)).unwrap_or(1.0);
                let rms = (frame.iter().map(|x| x * x).sum::<f32>() / FRAME_SAMPLES as f32).sqrt();
                let db = 20.0 * rms.max(1e-9).log10();
                // 一直編碼，讓編碼器狀態連續；只是決定要不要送
                let n = match enc.encode(&frame, &mut pkt) {
                    Ok(n) => n,
                    Err(_) => {
                        frame.clear();
                        continue;
                    }
                };
                frame.clear();

                let voiced = o.open_mic || (vad >= VAD_PROB && db >= VAD_MIN_DBFS);
                hang = if voiced { HANGOVER_FRAMES } else { hang.saturating_sub(1) };
                let want = hang > 0 && !shared.muted.load(Ordering::Relaxed);

                if want {
                    if !active {
                        if let Some(p) = prev.take() {
                            client.send_voice(Kind::Voice, 0, seq, &p);
                            seq = seq.wrapping_add(1);
                        }
                    }
                    client.send_voice(Kind::Voice, 0, seq, &pkt[..n]);
                    seq = seq.wrapping_add(1);
                } else if active {
                    client.send_voice(Kind::EndOfStream, 0, seq, &[]);
                    seq = seq.wrapping_add(1);
                }
                active = want;
                shared.tx_active.store(active, Ordering::Relaxed);
                prev = Some(pkt[..n].to_vec());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })?;
    Ok(())
}

fn spawn_receive(client: Client, shared: Arc<Shared>) {
    tokio::spawn(async move {
        while let Ok(d) = client.recv_voice().await {
            let Some(v) = VoiceDown::parse(&d) else { continue };
            let now = shared.now_us();
            let mut jbs = shared.jitter.lock().unwrap();
            let jb = jbs.entry(v.sender_idx).or_default();
            match v.kind {
                Kind::Voice => {
                    jb.push(v.seq, v.payload.to_vec(), now);
                    drop(jbs);
                    shared.last_heard.lock().unwrap().insert(v.sender_idx, Instant::now());
                }
                Kind::EndOfStream => jb.push_eos(v.seq, now),
            }
        }
    });
}

fn spawn_playback(shared: Arc<Shared>, mut out: rtrb::Producer<f32>, cap: usize, buffer_ms: u32) {
    let target = (SAMPLE_RATE * buffer_ms / 1000) as usize;
    std::thread::Builder::new()
        .name("playback".into())
        .spawn(move || {
            let mut decoders: HashMap<SessionIdx, Decoder> = HashMap::new();
            let mut mix = vec![0f32; FRAME_SAMPLES];
            let mut tmp = vec![0f32; FRAME_SAMPLES];
            let mut warmed = false;
            while !shared.stop.load(Ordering::Relaxed) {
                if cap - out.slots() >= target {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                mix.fill(0.0);
                let now = shared.now_us();
                {
                    let mut jbs = shared.jitter.lock().unwrap();
                    decoders.retain(|k, _| jbs.contains_key(k));
                    for (idx, jb) in jbs.iter_mut() {
                        let pull = jb.pull(now);
                        if pull == Pull::Silent {
                            continue;
                        }
                        let dec = match decoders.entry(*idx) {
                            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                            std::collections::hash_map::Entry::Vacant(e) => match Decoder::new() {
                                Ok(d) => e.insert(d),
                                Err(_) => continue,
                            },
                        };
                        let ok = match pull {
                            Pull::Decode(p) => dec.decode(&p, &mut tmp),
                            Pull::Fec(p) => dec.conceal(Some(&p), &mut tmp),
                            Pull::Plc => dec.conceal(None, &mut tmp),
                            Pull::Silent => unreachable!(),
                        };
                        if ok.is_ok() {
                            mix_into(&mut mix, &tmp, 1.0);
                        }
                    }
                }
                soft_clip(&mut mix);
                for &s in &mix {
                    let _ = out.push(s);
                }
                if !warmed {
                    // 串流剛開始時輸出端比第一幀早開始，那段欠載不算
                    warmed = true;
                    shared.underrun_base.store(shared.out_stats.xruns.load(Ordering::Relaxed), Ordering::Relaxed);
                }
            }
        })
        .expect("無法建立播放執行緒");
}
