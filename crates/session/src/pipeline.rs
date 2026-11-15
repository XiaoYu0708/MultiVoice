//! 音訊管線：
//!
//! ```text
//! 麥克風 → [擷取執行緒] 降噪 → VAD 閘門 → Opus 編碼 → client.send_voice
//! client.recv_voice → [tokio task] 每位說話者的 jitter buffer
//! [播放執行緒] 各 jitter buffer → 各自的 Opus 解碼器 → 混音 → 耳機
//! ```
//!
//! 進房後可調整：
//! - 開麥方式、降噪、人聲保留、輸出音量：存在 [`Shared`]，執行緒每幀讀取，立即生效
//! - 換裝置：丟掉舊的 [`AudioHandle`]（停止並等待執行緒結束）再 [`start`] 新的；
//!   語音序號存在 [`Shared`]，新的擷取執行緒接續編號，對方的 jitter buffer 不會把它當成遲到封包
//!
//! 接收語音的 task（[`spawn_receive`]）整個工作階段只開一次，換裝置不影響。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
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

#[derive(Clone, Debug, PartialEq)]
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
        Self {
            input: None,
            output: None,
            open_mic: false,
            denoise: true,
            mix: 0.3,
            bitrate: 32_000,
            buffer_ms: 30,
            volume: 1.0,
        }
    }
}

/// VAD：RNNoise 人聲機率 + 降噪後音量（PLAN.md：人聲機率不能單獨使用）
const VAD_PROB: f32 = 0.6;
const VAD_MIN_DBFS: f32 = -50.0;
/// 講完後繼續送出的時間
const HANGOVER_FRAMES: u32 = 15; // 300 ms

fn load_f32(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

fn store_f32(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

pub struct Shared {
    /// 整個工作階段結束
    pub stop: AtomicBool,
    pub muted: AtomicBool,
    /// 拒聽：照常消耗 jitter buffer，只是輸出靜音（取消時不會冒出累積的舊聲音）
    pub deafened: AtomicBool,
    pub tx_active: AtomicBool,
    // 進房後可即時調整的設定
    open_mic: AtomicBool,
    denoise: AtomicBool,
    speech_mix: AtomicU32,
    volume: AtomicU32,
    /// 下一個語音封包的序號（重啟音訊後接續）
    seq: AtomicU32,
    /// 麥克風原始峰值（降噪前），音量表用；每幀更新
    mic_peak: AtomicU32,
    /// 最近一幀的人聲機率
    mic_vad: AtomicU32,
    epoch: Instant,
    jitter: Mutex<HashMap<SessionIdx, JitterBuffer>>,
    last_heard: Mutex<HashMap<SessionIdx, Instant>>,
    out_stats: Arc<StreamStats>,
    underrun_base: AtomicU64,
}

#[derive(Default, Clone, Debug)]
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
            deafened: AtomicBool::new(false),
            tx_active: AtomicBool::new(false),
            open_mic: AtomicBool::new(false),
            denoise: AtomicBool::new(true),
            speech_mix: AtomicU32::new(0.3f32.to_bits()),
            volume: AtomicU32::new(1.0f32.to_bits()),
            seq: AtomicU32::new(0),
            mic_peak: AtomicU32::new(0),
            mic_vad: AtomicU32::new(0),
            epoch: Instant::now(),
            jitter: Default::default(),
            last_heard: Default::default(),
            out_stats: Arc::new(StreamStats::default()),
            underrun_base: AtomicU64::new(0),
        }
    }

    /// 套用可即時調整的設定（不含裝置）
    pub fn apply(&self, o: &AudioOpts) {
        self.open_mic.store(o.open_mic, Ordering::Relaxed);
        self.denoise.store(o.denoise, Ordering::Relaxed);
        store_f32(&self.speech_mix, o.mix);
        store_f32(&self.volume, o.volume);
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
        self.out_stats.xruns.load(Ordering::Relaxed).saturating_sub(self.underrun_base.load(Ordering::Relaxed))
    }

    /// (麥克風峰值 0..1, 人聲機率 0..1)
    pub fn mic_level(&self) -> (f32, f32) {
        (load_f32(&self.mic_peak), load_f32(&self.mic_vad))
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }
}

impl Default for Shared {
    fn default() -> Self {
        Self::new()
    }
}

/// 一組音訊裝置與其擷取／播放執行緒。drop 時停止並等待執行緒結束。
pub struct AudioHandle {
    streams: Vec<Box<dyn std::any::Any>>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    pub input_name: String,
    pub output_name: String,
}

impl Drop for AudioHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        self.streams.clear();
    }
}

/// 開啟裝置並啟動擷取與播放執行緒
pub fn start(client: Client, shared: Arc<Shared>, o: &AudioOpts) -> Result<AudioHandle> {
    let in_dev = device::find_device(true, o.input.as_deref()).map_err(anyhow::Error::from_boxed)?;
    let out_dev = device::find_device(false, o.output.as_deref()).map_err(anyhow::Error::from_boxed)?;

    let cap = SAMPLE_RATE as usize / 5; // 200 ms
    let (in_prod, in_cons) = rtrb::RingBuffer::<f32>::new(cap);
    let (out_prod, out_cons) = rtrb::RingBuffer::<f32>::new(cap);

    let (in_stream, _) = device::open_input(&in_dev, in_prod, Arc::new(StreamStats::default()), None)
        .map_err(anyhow::Error::from_boxed)?;
    let (out_stream, _) = device::open_output(&out_dev, out_cons, shared.out_stats.clone(), None, 1.0)
        .map_err(anyhow::Error::from_boxed)?;

    shared.apply(o);
    let stop = Arc::new(AtomicBool::new(false));
    let threads = vec![
        spawn_capture(client, shared.clone(), stop.clone(), in_cons, o.bitrate)?,
        spawn_playback(shared, stop.clone(), out_prod, cap, o.buffer_ms)?,
    ];

    Ok(AudioHandle {
        streams: vec![Box::new(in_stream), Box::new(out_stream)],
        stop,
        threads,
        input_name: device::device_name(&in_dev),
        output_name: device::device_name(&out_dev),
    })
}

fn spawn_capture(
    client: Client,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    mut ring: rtrb::Consumer<f32>,
    bitrate: i32,
) -> Result<JoinHandle<()>> {
    let mut enc = Encoder::new(bitrate)?;
    let mut denoiser = Denoiser::new();
    let h = std::thread::Builder::new().name("capture".into()).spawn(move || {
        let mut frame = Vec::with_capacity(FRAME_SAMPLES);
        let mut pkt = [0u8; MAX_PACKET];
        let mut prev: Option<Vec<u8>> = None; // 前一幀（開口時補送，避免切掉字頭）
        let mut hang = 0u32;
        let mut active = false;
        let running = || !shared.stop.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed);
        while running() {
            while let Ok(s) = ring.pop() {
                frame.push(s);
                if frame.len() < FRAME_SAMPLES {
                    continue;
                }
                let raw_peak = frame.iter().fold(0f32, |m, v| m.max(v.abs()));
                let denoise = shared.denoise.load(Ordering::Relaxed);
                let vad = if denoise {
                    denoiser.set_speech_mix(load_f32(&shared.speech_mix));
                    denoiser.process(&mut frame)
                } else {
                    // 沒有降噪就沒有人聲機率，VAD 只看音量
                    1.0
                };
                store_f32(&shared.mic_peak, raw_peak);
                store_f32(&shared.mic_vad, vad);
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

                let voiced = shared.open_mic.load(Ordering::Relaxed) || (vad >= VAD_PROB && db >= VAD_MIN_DBFS);
                hang = if voiced { HANGOVER_FRAMES } else { hang.saturating_sub(1) };
                let want = hang > 0 && !shared.muted.load(Ordering::Relaxed);

                if want {
                    if !active {
                        if let Some(p) = prev.take() {
                            client.send_voice(Kind::Voice, 0, shared.next_seq(), &p);
                        }
                    }
                    client.send_voice(Kind::Voice, 0, shared.next_seq(), &pkt[..n]);
                } else if active {
                    client.send_voice(Kind::EndOfStream, 0, shared.next_seq(), &[]);
                }
                active = want;
                shared.tx_active.store(active, Ordering::Relaxed);
                prev = Some(pkt[..n].to_vec());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        // 停止時正在送出：補一個 end-of-stream，讓對方立刻結束這段話
        if active {
            client.send_voice(Kind::EndOfStream, 0, shared.next_seq(), &[]);
            shared.tx_active.store(false, Ordering::Relaxed);
        }
    })?;
    Ok(h)
}

/// 接收語音放進 jitter buffer。整個工作階段只開一次（換裝置不受影響）。
/// `rt`：呼叫端可能是沒有 tokio context 的 GUI 執行緒
pub fn spawn_receive(client: Client, shared: Arc<Shared>, rt: &tokio::runtime::Handle) {
    rt.spawn(async move {
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

fn spawn_playback(
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    mut out: rtrb::Producer<f32>,
    cap: usize,
    buffer_ms: u32,
) -> Result<JoinHandle<()>> {
    let target = (SAMPLE_RATE * buffer_ms / 1000) as usize;
    let h = std::thread::Builder::new().name("playback".into()).spawn(move || {
        let mut decoders: HashMap<SessionIdx, Decoder> = HashMap::new();
        let mut mix = vec![0f32; FRAME_SAMPLES];
        let mut tmp = vec![0f32; FRAME_SAMPLES];
        let mut warmed = false;
        while !shared.stop.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) {
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
            if shared.deafened.load(Ordering::Relaxed) {
                mix.fill(0.0);
            } else {
                let vol = load_f32(&shared.volume);
                if vol != 1.0 {
                    mix.iter_mut().for_each(|s| *s *= vol);
                }
                soft_clip(&mut mix);
            }
            for &s in &mix {
                let _ = out.push(s);
            }
            if !warmed {
                // 串流剛開始時輸出端比第一幀早開始，那段欠載不算
                warmed = true;
                shared.underrun_base.store(shared.out_stats.xruns.load(Ordering::Relaxed), Ordering::Relaxed);
            }
        }
    })?;
    Ok(h)
}
