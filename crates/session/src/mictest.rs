//! 麥克風測試（不用進房）：開麥克風、跑跟通話一樣的前端處理（降噪、說話判斷、自動音量），
//! 把音量、是否判定為說話、環境噪音給 UI 顯示；可以選擇把處理後的聲音播到耳機（聽自己的聲音）。
//!
//! cpal 串流不能跨執行緒，所以裝置在測試專用的執行緒裡開；drop [`MicTest`] 就停止。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;
use audio::codec::{FRAME_SAMPLES, SAMPLE_RATE};
use audio::device::{self, StreamStats};
use audio::frontend::{Frontend, FrontendCfg};
use audio::resample::Resampler;

/// 測試結果（UI 每幀讀取）
#[derive(Default)]
pub struct MicStats {
    /// 原始峰值（0..1）
    peak: AtomicU32,
    /// 降噪後、增益前的音量（dBFS）
    level_db: AtomicU32,
    noise_db: AtomicU32,
    gain_db: AtomicU32,
    voiced: AtomicBool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MicReading {
    pub peak: f32,
    pub level_db: f32,
    pub noise_db: f32,
    pub gain_db: f32,
    pub voiced: bool,
}

fn st(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

fn ld(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

/// 測試中可以即時調整的設定
#[derive(Default)]
struct Live {
    denoise: AtomicBool,
    speech_mix: AtomicU32,
    gate_dbfs: AtomicU32,
    agc: AtomicBool,
    strong: AtomicBool,
    monitor: AtomicBool,
}

impl Live {
    fn set(&self, cfg: FrontendCfg, monitor: bool) {
        self.denoise.store(cfg.denoise, Ordering::Relaxed);
        st(&self.speech_mix, cfg.speech_mix);
        st(&self.gate_dbfs, cfg.gate_dbfs);
        self.agc.store(cfg.agc, Ordering::Relaxed);
        self.strong.store(cfg.strong, Ordering::Relaxed);
        self.monitor.store(monitor, Ordering::Relaxed);
    }
}

pub struct MicTest {
    stats: Arc<MicStats>,
    live: Arc<Live>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MicTest {
    /// 開始測試。`input` / `output` 是裝置名稱片段（None = 系統預設）；`monitor` = 播放自己的聲音
    pub fn start(input: Option<String>, output: Option<String>, cfg: FrontendCfg, monitor: bool) -> Result<MicTest> {
        let stats = Arc::new(MicStats::default());
        let live = Arc::new(Live::default());
        let stop = Arc::new(AtomicBool::new(false));
        live.set(cfg, monitor);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        let thread = std::thread::Builder::new().name("mic-test".into()).spawn({
            let (stats, live, stop) = (stats.clone(), live.clone(), stop.clone());
            move || {
                if let Err(e) = run(input, output, stats, live, stop, &ready_tx) {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })?;
        let t = MicTest { stats, live, stop, thread: Some(thread) };
        // 開不起來時 drop t 會停掉執行緒
        ready_rx.recv().map_err(|_| anyhow::anyhow!("麥克風測試異常結束"))??;
        Ok(t)
    }

    /// 即時調整（降噪、門檻、自動音量、聽自己的聲音）
    pub fn set(&self, cfg: FrontendCfg, monitor: bool) {
        self.live.set(cfg, monitor);
    }

    pub fn reading(&self) -> MicReading {
        let s = &self.stats;
        MicReading {
            peak: ld(&s.peak),
            level_db: ld(&s.level_db),
            noise_db: ld(&s.noise_db),
            gain_db: ld(&s.gain_db),
            voiced: s.voiced.load(Ordering::Relaxed),
        }
    }
}

impl Drop for MicTest {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// 試聽提示音（調整提示音音量時用，不用進房）：在背景執行緒開輸出裝置播一次「開始按鍵說話」的提示音。
/// `gain` = 輸出音量 × 提示音音量。開不起來就算了（只是試聽）
pub fn preview_cue(output: Option<String>, gain: f32) {
    let _ = std::thread::Builder::new().name("cue-preview".into()).spawn(move || {
        let _ = play_cue(output, gain);
    });
}

fn play_cue(output: Option<String>, gain: f32) -> Result<()> {
    use crate::talk::{Cue, TonePlayer};
    let err = anyhow::Error::from_boxed;
    let out_dev = device::find_device(false, output.as_deref()).map_err(err)?;
    let out_rate = device::default_rate(&out_dev, false).map_err(err)?;
    let mut tones = TonePlayer::default();
    tones.play(Cue::PttOn(0));
    // 前面墊 50 ms 靜音，輸出剛開始時不會被吃掉開頭
    let mut stereo = vec![0f32; SAMPLE_RATE as usize / 20 * 2];
    let mut buf = vec![0f32; FRAME_SAMPLES * 2];
    while tones.playing() {
        buf.fill(0.0);
        tones.mix_into(&mut buf, gain);
        stereo.extend_from_slice(&buf);
    }
    let mut rs = Resampler::new(SAMPLE_RATE, out_rate, 2).map_err(err)?;
    let mut out = Vec::new();
    rs.process(&stereo, &mut out);
    // 尾巴也墊一點，讓重取樣器吐完
    rs.process(&vec![0f32; FRAME_SAMPLES * 2], &mut out);
    let (mut prod, cons) = rtrb::RingBuffer::<f32>::new(out.len() + 16);
    for &v in &out {
        let _ = prod.push(v);
    }
    let (_stream, _) =
        device::open_output(&out_dev, cons, Arc::new(StreamStats::default()), None, 1.0, None).map_err(err)?;
    let secs = out.len() as f32 / 2.0 / out_rate as f32;
    std::thread::sleep(Duration::from_secs_f32(secs + 0.15));
    Ok(())
}

fn run(
    input: Option<String>,
    output: Option<String>,
    stats: Arc<MicStats>,
    live: Arc<Live>,
    stop: Arc<AtomicBool>,
    ready: &std::sync::mpsc::Sender<Result<()>>,
) -> Result<()> {
    let err = anyhow::Error::from_boxed;
    let in_dev = device::find_device(true, input.as_deref()).map_err(err)?;
    let out_dev = device::find_device(false, output.as_deref()).map_err(err)?;
    let in_rate = device::default_rate(&in_dev, true).map_err(err)?;
    let out_rate = device::default_rate(&out_dev, false).map_err(err)?;
    let (in_prod, mut in_cons) = rtrb::RingBuffer::<f32>::new(in_rate as usize / 5);
    // 監聽用的輸出緩衝小一點（約 100 ms），延遲才不會太大
    let (mut out_prod, out_cons) = rtrb::RingBuffer::<f32>::new(out_rate as usize / 10 * 2);
    let (_in_stream, _) =
        device::open_input(&in_dev, in_prod, Arc::new(StreamStats::default()), None, None).map_err(err)?;
    let (_out_stream, _) =
        device::open_output(&out_dev, out_cons, Arc::new(StreamStats::default()), None, 1.0, None).map_err(err)?;
    let mut rs_in = Resampler::new(in_rate, SAMPLE_RATE, 1).map_err(err)?;
    let mut rs_out = Resampler::new(SAMPLE_RATE, out_rate, 2).map_err(err)?;
    let _ = ready.send(Ok(()));

    let mut frontend = Frontend::new();
    let (mut raw, mut res, mut frame, mut stereo, mut out) =
        (Vec::new(), Vec::new(), Vec::with_capacity(FRAME_SAMPLES), Vec::new(), Vec::new());
    while !stop.load(Ordering::Relaxed) {
        raw.clear();
        while let Ok(s) = in_cons.pop() {
            raw.push(s);
        }
        res.clear();
        rs_in.process(&raw, &mut res);
        for &s in &res {
            frame.push(s);
            if frame.len() < FRAME_SAMPLES {
                continue;
            }
            let cfg = FrontendCfg {
                denoise: live.denoise.load(Ordering::Relaxed),
                speech_mix: ld(&live.speech_mix),
                gate_dbfs: ld(&live.gate_dbfs),
                agc: live.agc.load(Ordering::Relaxed),
                strong: live.strong.load(Ordering::Relaxed),
            };
            let info = frontend.process(&mut frame, &cfg);
            st(&stats.peak, info.raw_peak);
            st(&stats.level_db, info.level_db);
            st(&stats.noise_db, info.noise_db);
            st(&stats.gain_db, info.gain_db);
            stats.voiced.store(info.voiced, Ordering::Relaxed);
            if live.monitor.load(Ordering::Relaxed) {
                stereo.clear();
                stereo.extend(frame.iter().flat_map(|&v| [v, v]));
                out.clear();
                rs_out.process(&stereo, &mut out);
                for &v in &out {
                    let _ = out_prod.push(v);
                }
            }
            frame.clear();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}
