//! 音訊管線：
//!
//! ```text
//! 麥克風 → [擷取執行緒] 降噪 → VAD ＋ 按鍵說話（PTT）→ 依各頻道說話模式算出 talk_mask → Opus 編碼 → client.send_voice
//! client.recv_voice → [tokio task] 每位說話者的 jitter buffer（記下 via_mask）
//! [播放執行緒] 各 jitter buffer → 各自的 Opus 解碼器 → 依頻道混音規則（音量、定位、優先 ducking）→ 立體聲 ＋ 提示音 → 耳機
//! ```
//!
//! 進房後可調整：
//! - 開麥預設、降噪、人聲保留、輸出音量、各頻道設定：存在 [`Shared`]，執行緒每幀讀取，立即生效
//! - 換裝置：丟掉舊的 [`AudioHandle`]（停止並等待執行緒結束）再 [`start`] 新的；
//!   語音序號存在 [`Shared`]，新的擷取執行緒接續編號，對方的 jitter buffer 不會把它當成遲到封包
//!
//! 接收語音的 task（[`spawn_receive`]）整個工作階段只開一次，換裝置不影響。
//!
//! 裝置熱插拔（[`AudioRunner`]）：音訊串流放在專屬執行緒（cpal 串流不能跨執行緒，放在 UI 執行緒的話，
//! 視窗縮到系統匣時就沒人處理），每秒檢查：串流出錯（裝置被拔掉）就重開；指定的裝置不見時先用系統預設，
//! 插回來再切回去；設定為「系統預設」時，Windows 換了預設裝置就跟著換。
//!
//! 沒有麥克風（或打不開）也能進房只聽：只開耳機，不開擷取執行緒；之後插上麥克風會自動重開。

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use audio::codec::{Decoder, Encoder, FRAME_SAMPLES, MAX_PACKET, SAMPLE_RATE};
use audio::frontend::{DEFAULT_GATE_DBFS, Frontend, FrontendCfg};
use audio::device::{self, StreamStats};
use audio::resample::Resampler;
use client_core::{Client, VoiceDown};
use mixer_core::jitter::{JitterBuffer, Pull};
use mixer_core::mix::{mix_into_stereo, soft_clip};
use mixer_core::route::{ChannelMix, DUCK_GAIN, Route, route};
use protocol::voice::Kind;
use protocol::{ChannelId, SessionIdx};

use hotkeys::Hotkey;

use crate::talk::{Cue, TalkInputs, TalkState, TonePlayer};

#[derive(Clone, Debug, PartialEq)]
pub struct AudioOpts {
    pub input: Option<String>,
    pub output: Option<String>,
    /// 加入頻道時預設的說話模式：true = 常開，false = 偵測說話
    pub open_mic: bool,
    pub denoise: bool,
    pub mix: f32,
    /// 「偵測說話」的音量門檻（dBFS，降噪後、增益前）
    pub gate_dbfs: f32,
    /// 自動音量（AGC）
    pub agc: bool,
    /// 強力降噪（DeepFilterNet3；多 20 ms 延遲）
    pub strong_denoise: bool,
    pub bitrate: i32,
    pub buffer_ms: u32,
    pub volume: f32,
    /// 開／關麥提示音
    pub cues: bool,
    /// 進房時麥克風先靜音（桌面版預設開；要說話時自己打開）
    pub start_muted: bool,
}

impl Default for AudioOpts {
    fn default() -> Self {
        Self {
            input: None,
            output: None,
            open_mic: false,
            denoise: true,
            mix: 0.3,
            gate_dbfs: DEFAULT_GATE_DBFS,
            agc: true,
            strong_denoise: false,
            bitrate: 32_000,
            buffer_ms: 30,
            volume: 1.0,
            cues: true,
            start_muted: false,
        }
    }
}

/// 在某個頻道的說話方式（PLAN.md §2.2）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TalkMode {
    /// 不對這個頻道說話
    Off,
    /// 偵測到說話才送出
    Voice,
    /// 一直開麥
    Open,
    /// 按住 [`ChannelCfg::ptt_key`] 才說話（不經過 VAD）
    Ptt,
}

/// 我在某個已加入頻道的設定
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelCfg {
    pub listen: bool,
    pub talk: TalkMode,
    pub mix: ChannelMix,
    /// 說話模式為 [`TalkMode::Ptt`] 時用的按鍵；沒設定就不會說話
    pub ptt_key: Option<Hotkey>,
}

pub(crate) const SLOTS: usize = protocol::control::MAX_SLOTS as usize;

fn load_f32(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

fn store_f32(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

/// 依槽位查頻道與混音設定（播放端每幀用）
#[derive(Clone, Copy, Default)]
struct SlotInfo {
    channel: Option<ChannelId>,
    mix: Option<ChannelMix>,
}

pub struct Shared {
    /// 整個工作階段結束
    pub stop: AtomicBool,
    /// 使用者自己按的靜音（不含拒聽造成的）；要看實際有沒有送出用 [`Shared::is_muted`]
    muted: AtomicBool,
    /// 拒聽：不聽也不說。照常消耗 jitter buffer，只是輸出靜音（取消時不會冒出累積的舊聲音）
    deafened: AtomicBool,
    pub tx_active: AtomicBool,
    /// 目前有沒有麥克風（沒有就只能聽）
    pub has_mic: AtomicBool,
    // 進房後可即時調整的設定
    denoise: AtomicBool,
    speech_mix: AtomicU32,
    gate_dbfs: AtomicU32,
    agc: AtomicBool,
    strong_denoise: AtomicBool,
    volume: AtomicU32,
    /// 各已加入頻道的設定；變更後呼叫 [`Shared::rebuild`]
    cfgs: Mutex<BTreeMap<ChannelId, ChannelCfg>>,
    slots: Mutex<[SlotInfo; SLOTS]>,
    /// 說話模式為「偵測說話」／「常開」的頻道（我的槽位）
    vad_mask: AtomicU64,
    open_mask: AtomicU64,
    /// PTT 鍵 → 對應的頻道遮罩
    ptt_keys: Mutex<Vec<(Hotkey, u64)>>,
    cues: AtomicBool,
    /// 待播的提示音（播放執行緒取走；新的蓋掉舊的）
    cue: Mutex<Option<Cue>>,
    /// 目前實際送出的 talk_mask
    tx_mask: AtomicU64,
    /// 下一個語音封包的序號（重啟音訊後接續）
    seq: AtomicU32,
    /// 麥克風原始峰值（降噪前），音量表用；每幀更新
    mic_peak: AtomicU32,
    /// 最近一幀的人聲機率
    mic_vad: AtomicU32,
    epoch: Instant,
    jitter: Mutex<HashMap<SessionIdx, JitterBuffer>>,
    /// 每位說話者最近一次收到的時間與 via_mask
    last_heard: Mutex<HashMap<SessionIdx, (Instant, u64)>>,
    /// 我聽某位成員的音量倍率（右鍵調整；沒有 = 1.0）
    user_gain: Mutex<HashMap<SessionIdx, f32>>,
    out_stats: Arc<StreamStats>,
    underrun_base: AtomicU64,
}

#[derive(Default, Clone, Debug)]
pub struct JitterSummary {
    pub target: String,
    /// 正常解碼的幀數（算遺失率用）
    pub decoded: u64,
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
            has_mic: AtomicBool::new(true),
            denoise: AtomicBool::new(true),
            speech_mix: AtomicU32::new(0.3f32.to_bits()),
            gate_dbfs: AtomicU32::new(DEFAULT_GATE_DBFS.to_bits()),
            agc: AtomicBool::new(true),
            strong_denoise: AtomicBool::new(false),
            volume: AtomicU32::new(1.0f32.to_bits()),
            cfgs: Mutex::new(BTreeMap::new()),
            slots: Mutex::new([SlotInfo::default(); SLOTS]),
            vad_mask: AtomicU64::new(0),
            open_mask: AtomicU64::new(0),
            ptt_keys: Mutex::new(Vec::new()),
            cues: AtomicBool::new(true),
            cue: Mutex::new(None),
            tx_mask: AtomicU64::new(0),
            seq: AtomicU32::new(0),
            mic_peak: AtomicU32::new(0),
            mic_vad: AtomicU32::new(0),
            epoch: Instant::now(),
            jitter: Default::default(),
            last_heard: Default::default(),
            user_gain: Default::default(),
            out_stats: Arc::new(StreamStats::default()),
            underrun_base: AtomicU64::new(0),
        }
    }

    /// 套用可即時調整的音訊設定（不含裝置與頻道）
    pub fn apply(&self, o: &AudioOpts) {
        self.denoise.store(o.denoise, Ordering::Relaxed);
        store_f32(&self.speech_mix, o.mix);
        store_f32(&self.gate_dbfs, o.gate_dbfs);
        self.agc.store(o.agc, Ordering::Relaxed);
        self.strong_denoise.store(o.strong_denoise, Ordering::Relaxed);
        store_f32(&self.volume, o.volume);
        self.cues.store(o.cues, Ordering::Relaxed);
    }

    /// 實際上是否不送出聲音：自己靜音，或拒聽中（拒聽一定連麥克風一起關）
    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed) || self.deafened.load(Ordering::Relaxed)
    }

    pub fn is_deafened(&self) -> bool {
        self.deafened.load(Ordering::Relaxed)
    }

    /// 設定靜音。取消靜音時若在拒聽中，連拒聽一起取消（否則按了也還是沒聲音）
    pub fn set_muted(&self, v: bool) {
        self.muted.store(v, Ordering::Relaxed);
        if !v {
            self.deafened.store(false, Ordering::Relaxed);
        }
    }

    /// 設定拒聽。取消拒聽後，靜音回到拒聽前自己設定的狀態
    pub fn set_deafened(&self, v: bool) {
        self.deafened.store(v, Ordering::Relaxed);
    }

    /// 切換靜音（系統匣選單用）：目前沒聲音（靜音或拒聽）→ 全部打開；否則靜音
    pub fn toggle_mute(&self) {
        self.set_muted(!self.is_muted());
    }

    fn cue(&self, c: Cue) {
        if self.cues.load(Ordering::Relaxed) {
            *self.cue.lock().unwrap() = Some(c);
        }
    }

    pub fn channel_cfgs(&self) -> BTreeMap<ChannelId, ChannelCfg> {
        self.cfgs.lock().unwrap().clone()
    }

    /// 設定（Some）或移除（None）某頻道的本機設定；之後要呼叫 [`Shared::rebuild`]
    pub fn set_cfg(&self, channel: ChannelId, cfg: Option<ChannelCfg>) {
        let mut c = self.cfgs.lock().unwrap();
        match cfg {
            Some(cfg) => c.insert(channel, cfg),
            None => c.remove(&channel),
        };
    }

    /// 依頻道設定與我的槽位，重算槽位表與說話遮罩
    pub fn rebuild(&self, my_slots: &HashMap<ChannelId, u8>) {
        let cfgs = self.cfgs.lock().unwrap();
        let mut slots = [SlotInfo::default(); SLOTS];
        let (mut vad, mut open) = (0u64, 0u64);
        let mut ptt: Vec<(Hotkey, u64)> = Vec::new();
        for (&ch, &slot) in my_slots {
            let Some(cfg) = cfgs.get(&ch) else { continue };
            slots[slot as usize] = SlotInfo { channel: Some(ch), mix: Some(cfg.mix) };
            let bit = 1u64 << slot;
            match cfg.talk {
                TalkMode::Voice => vad |= bit,
                TalkMode::Open => open |= bit,
                TalkMode::Ptt => match cfg.ptt_key {
                    Some(k) => match ptt.iter_mut().find(|(kk, _)| *kk == k) {
                        Some((_, m)) => *m |= bit,
                        None => ptt.push((k, bit)),
                    },
                    None => {}
                },
                TalkMode::Off => {}
            }
        }
        *self.slots.lock().unwrap() = slots;
        *self.ptt_keys.lock().unwrap() = ptt;
        self.vad_mask.store(vad, Ordering::Relaxed);
        self.open_mask.store(open, Ordering::Relaxed);
    }

    /// 遮罩（我的槽位）→ 頻道
    pub fn channels_of(&self, mask: u64) -> Vec<ChannelId> {
        let slots = self.slots.lock().unwrap();
        (0..SLOTS).filter(|&s| mask & (1 << s) != 0).filter_map(|s| slots[s].channel).collect()
    }

    /// 目前正在送出的頻道
    pub fn talking_to(&self) -> Vec<ChannelId> {
        if self.tx_active.load(Ordering::Relaxed) { self.channels_of(self.tx_mask.load(Ordering::Relaxed)) } else { Vec::new() }
    }

    fn now_us(&self) -> u64 {
        self.epoch.elapsed().as_micros() as u64
    }

    /// 最近 `within` 內有收到聲音的成員，以及經由哪些頻道
    pub fn speaking(&self, within: Duration) -> Vec<(SessionIdx, Vec<ChannelId>)> {
        let lh: Vec<(SessionIdx, u64)> = self
            .last_heard
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, (t, _))| t.elapsed() < within)
            .map(|(&i, &(_, via))| (i, via))
            .collect();
        let mut v: Vec<_> = lh.into_iter().map(|(i, via)| (i, self.channels_of(via))).collect();
        v.sort_by_key(|(i, _)| *i);
        v
    }

    /// 設定我聽各成員的音量倍率（整份換掉；沒列到的人 = 1.0）
    pub fn set_user_gains(&self, gains: HashMap<SessionIdx, f32>) {
        let mut g = self.user_gain.lock().unwrap();
        if *g != gains {
            *g = gains;
        }
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
            s.decoded += jb.stats.decoded;
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
    in_stats: Arc<StreamStats>,
    out_stats: Arc<StreamStats>,
    /// 指定的裝置找不到，暫時用系統預設（麥克風：系統預設也沒有時也算，插回來會重開）
    in_fallback: bool,
    out_fallback: bool,
    /// 麥克風打不開的原因（None = 有麥克風）
    pub in_error: Option<String>,
}

impl AudioHandle {
    /// 串流出錯了（裝置被拔掉等）
    pub fn failed(&self) -> bool {
        self.in_stats.failed.load(Ordering::Relaxed) || self.out_stats.failed.load(Ordering::Relaxed)
    }
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
/// 指定的裝置找不到時改用系統預設（回傳 (裝置, 是否為替代)）
fn find_or_default(input: bool, name: Option<&str>) -> Result<(device::Device, bool)> {
    match device::find_device(input, name) {
        Ok(d) => Ok((d, false)),
        Err(_) if name.is_some() => {
            let d = device::find_device(input, None).map_err(anyhow::Error::from_boxed)?;
            Ok((d, true))
        }
        Err(e) => Err(anyhow::Error::from_boxed(e)),
    }
}

pub fn start(client: Client, shared: Arc<Shared>, o: &AudioOpts) -> Result<AudioHandle> {
    // 裝置的取樣率不一定是 48 kHz（例如 44.1 kHz 耳機）：環形緩衝裡是裝置取樣率，
    // 擷取／播放執行緒負責轉換
    // 測試用：MULTIVOICE_TEST_RATES=輸入,輸出（例如 44100,44100）強制用其他取樣率開裝置，
    // 在只有 48 kHz 裝置的電腦上也能跑重取樣路徑（輸出由 Windows 轉換；輸入要裝置原生支援）
    let forced: Vec<u32> = std::env::var("MULTIVOICE_TEST_RATES")
        .ok()
        .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_default();
    let (force_in, force_out) = (forced.first().copied(), forced.get(1).copied());
    // 測試用：MULTIVOICE_TEST_NO_MIC=1 假裝沒有麥克風
    let no_mic_test = std::env::var_os("MULTIVOICE_TEST_NO_MIC").is_some();

    // 麥克風可有可無：打不開就只聽。
    // 記下試過的裝置名稱：裝置在但打不開時，熱插拔檢查不會每秒重開（會打斷耳機的聲音）
    let in_stats = Arc::new(StreamStats::default());
    let mut input_name = String::new();
    let mut in_fallback = o.input.is_some();
    let input = (|| -> Result<_> {
        if no_mic_test {
            // 記成目前的預設裝置，熱插拔檢查才不會一直重開
            input_name = device::default_device_name(true).unwrap_or_default();
            in_fallback = false;
            anyhow::bail!("找不到預設音訊裝置（測試）");
        }
        let (dev, fb) = find_or_default(true, o.input.as_deref())?;
        input_name = device::device_name(&dev);
        in_fallback = fb;
        let rate = match force_in {
            Some(r) => r,
            None => device::default_rate(&dev, true).map_err(anyhow::Error::from_boxed)?,
        };
        let (prod, cons) = rtrb::RingBuffer::<f32>::new(rate.max(SAMPLE_RATE) as usize / 5); // 200 ms
        let (stream, _) =
            device::open_input(&dev, prod, in_stats.clone(), None, force_in).map_err(anyhow::Error::from_boxed)?;
        Ok((stream, cons, rate))
    })();

    let (out_dev, out_fallback) = find_or_default(false, o.output.as_deref())?;
    let out_rate = match force_out {
        Some(r) => r,
        None => device::default_rate(&out_dev, false).map_err(anyhow::Error::from_boxed)?,
    };
    let out_cap = out_rate.max(SAMPLE_RATE) as usize / 5;
    // 輸出為左右交錯的立體聲
    let (out_prod, out_cons) = rtrb::RingBuffer::<f32>::new(out_cap * 2);
    shared.out_stats.failed.store(false, Ordering::Relaxed);
    let (out_stream, _) = device::open_output(&out_dev, out_cons, shared.out_stats.clone(), None, 1.0, force_out)
        .map_err(anyhow::Error::from_boxed)?;

    shared.apply(o);
    let stop = Arc::new(AtomicBool::new(false));
    let mut streams: Vec<Box<dyn std::any::Any>> = vec![Box::new(out_stream)];
    let mut threads = vec![spawn_playback(shared.clone(), stop.clone(), out_prod, out_cap, o.buffer_ms, out_rate)?];
    let in_error = match input {
        Ok((stream, cons, rate)) => {
            streams.push(Box::new(stream));
            threads.push(spawn_capture(client, shared.clone(), stop.clone(), cons, o.bitrate, rate)?);
            None
        }
        Err(e) => {
            shared.tx_active.store(false, Ordering::Relaxed);
            shared.tx_mask.store(0, Ordering::Relaxed);
            store_f32(&shared.mic_peak, 0.0);
            store_f32(&shared.mic_vad, 0.0);
            Some(format!("{e:#}"))
        }
    };
    shared.has_mic.store(in_error.is_none(), Ordering::Relaxed);

    Ok(AudioHandle {
        streams,
        stop,
        threads,
        input_name,
        output_name: device::device_name(&out_dev),
        in_stats,
        out_stats: shared.out_stats.clone(),
        in_fallback,
        out_fallback,
        in_error,
    })
}

/// 給 UI 或 log 看的訊息
pub type AudioLog = Arc<dyn Fn(String) + Send + Sync>;

enum Cmd {
    Apply(AudioOpts, std::sync::mpsc::Sender<Result<()>>),
    /// 測試用：假裝裝置出錯
    SimulateFailure,
    Stop,
}

/// 在專屬執行緒持有音訊裝置，並處理熱插拔（見模組說明）。drop 時停止
pub struct AudioRunner {
    tx: std::sync::mpsc::Sender<Cmd>,
    thread: Option<JoinHandle<()>>,
    names: Arc<Mutex<(String, String)>>,
}

impl AudioRunner {
    /// 開啟裝置（等第一次開啟的結果才返回）
    pub fn start(client: Client, shared: Arc<Shared>, opts: &AudioOpts, log: AudioLog) -> Result<AudioRunner> {
        let (tx, rx) = std::sync::mpsc::channel::<Cmd>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        let names = Arc::new(Mutex::new((String::new(), String::new())));
        let opts = opts.clone();
        let names2 = names.clone();
        let thread = std::thread::Builder::new().name("audio-owner".into()).spawn(move || {
            let mut owner = Owner { client, shared, opts, log, names: names2, h: None, last_try: Instant::now() };
            match owner.open() {
                Ok(()) => {
                    let _ = ready_tx.send(Ok(()));
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            }
            owner.run(rx);
        })?;
        ready_rx.recv().map_err(|_| anyhow::anyhow!("音訊執行緒異常結束"))??;
        Ok(AudioRunner { tx, thread: Some(thread), names })
    }

    /// 換裝置（會重開音訊）；新裝置開不起來時退回原本的裝置並回傳錯誤
    pub fn apply(&self, opts: &AudioOpts) -> Result<()> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx.send(Cmd::Apply(opts.clone(), tx)).map_err(|_| anyhow::anyhow!("音訊執行緒已結束"))?;
        rx.recv().map_err(|_| anyhow::anyhow!("音訊執行緒已結束"))?
    }

    /// 目前使用的 (輸入, 輸出) 裝置名稱
    pub fn names(&self) -> (String, String) {
        self.names.lock().unwrap().clone()
    }

    /// 測試用：假裝裝置出錯（驗證會自動重開）
    pub fn simulate_failure(&self) {
        let _ = self.tx.send(Cmd::SimulateFailure);
    }
}

impl Drop for AudioRunner {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// 熱插拔檢查間隔
const HEALTH_INTERVAL: Duration = Duration::from_secs(1);
/// 裝置開不起來時多久重試一次
const RETRY_INTERVAL: Duration = Duration::from_secs(2);

struct Owner {
    client: Client,
    shared: Arc<Shared>,
    opts: AudioOpts,
    log: AudioLog,
    names: Arc<Mutex<(String, String)>>,
    h: Option<AudioHandle>,
    last_try: Instant,
}

impl Owner {
    fn open(&mut self) -> Result<()> {
        // 先停掉舊的（等執行緒結束），避免兩組擷取執行緒同時送出
        self.h = None;
        self.last_try = Instant::now();
        let h = start(self.client.clone(), self.shared.clone(), &self.opts)?;
        let input = if h.in_error.is_some() { String::new() } else { h.input_name.clone() };
        *self.names.lock().unwrap() = (input, h.output_name.clone());
        if let Some(e) = &h.in_error {
            (self.log)(format!("沒有可用的麥克風，只能收聽（插上麥克風後會自動開啟）：{e}"));
        } else if h.in_fallback {
            (self.log)(format!("找不到麥克風「{}」，暫時改用系統預設", self.opts.input.as_deref().unwrap_or_default()));
        }
        if h.out_fallback {
            (self.log)(format!("找不到耳機「{}」，暫時改用系統預設", self.opts.output.as_deref().unwrap_or_default()));
        }
        self.h = Some(h);
        Ok(())
    }

    fn run(&mut self, rx: std::sync::mpsc::Receiver<Cmd>) {
        loop {
            match rx.recv_timeout(HEALTH_INTERVAL) {
                Ok(Cmd::Apply(o, reply)) => {
                    let old = std::mem::replace(&mut self.opts, o);
                    let r = match self.open() {
                        Ok(()) => Ok(()),
                        Err(e) => {
                            // 退回原本的裝置
                            self.opts = old;
                            let _ = self.open();
                            Err(e.context("無法開啟新的音訊裝置，已改回原本的裝置"))
                        }
                    };
                    let _ = reply.send(r);
                }
                Ok(Cmd::SimulateFailure) => {
                    if let Some(h) = &self.h {
                        h.in_stats.failed.store(true, Ordering::Relaxed);
                    }
                }
                Ok(Cmd::Stop) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    self.h = None;
                    return;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => self.health(),
            }
        }
    }

    /// 熱插拔檢查
    fn health(&mut self) {
        if self.shared.stop.load(Ordering::Relaxed) {
            return;
        }
        let reason = match &self.h {
            // 上次重開失敗（例如唯一的耳機被拔掉）：定期重試
            None if self.last_try.elapsed() >= RETRY_INTERVAL => Some(String::new()),
            None => None,
            Some(h) if h.failed() => Some("音訊裝置中斷".into()),
            Some(h) => {
                let changed = |input: bool, want: &Option<String>, cur: &str, fallback: bool| match want {
                    // 跟著系統預設：預設換了（沒有麥克風時 cur 是空的：插上就重開）
                    None => device::default_device_name(input).is_some_and(|d| d != cur),
                    // 指定的裝置插回來了
                    Some(name) => fallback && device::has_device(input, name),
                };
                if changed(true, &self.opts.input, &h.input_name, h.in_fallback)
                    || changed(false, &self.opts.output, &h.output_name, h.out_fallback)
                {
                    Some("音訊裝置有變動".into())
                } else {
                    None
                }
            }
        };
        let Some(reason) = reason else { return };
        let was_down = self.h.is_none();
        match self.open() {
            Ok(()) => {
                let (i, o) = self.names.lock().unwrap().clone();
                let i = if i.is_empty() { "沒有".into() } else { i };
                (self.log)(format!("{}，已重新開啟（麥克風：{i}｜耳機：{o}）", if was_down { "音訊裝置恢復" } else { &reason }));
            }
            Err(e) => {
                if !was_down {
                    (self.log)(format!("{reason}，找不到可用的裝置（插回後會自動恢復）：{e:#}"));
                }
            }
        }
    }
}

fn spawn_capture(
    client: Client,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    mut ring: rtrb::Consumer<f32>,
    bitrate: i32,
    in_rate: u32,
) -> Result<JoinHandle<()>> {
    let mut enc = Encoder::new(bitrate)?;
    let mut rs = Resampler::new(in_rate, SAMPLE_RATE, 1).map_err(anyhow::Error::from_boxed)?;
    let mut frontend = Frontend::new();
    let h = std::thread::Builder::new().name("capture".into()).spawn(move || {
        let mut frame = Vec::with_capacity(FRAME_SAMPLES);
        let mut pkt = [0u8; MAX_PACKET];
        let mut prev: Option<Vec<u8>> = None; // 前一幀（開口時補送，避免切掉字頭）
        let mut talk = TalkState::default();
        let mut active = false;
        // 提示音用：上一幀按住的 PTT 頻道、靜音狀態
        let mut prev_ptt = 0u64;
        let mut prev_muted = shared.is_muted();
        let running = || !shared.stop.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed);
        let (mut raw, mut res) = (Vec::with_capacity(4096), Vec::with_capacity(4096));
        while running() {
            // 裝置取樣率 → 48 kHz
            raw.clear();
            while let Ok(s) = ring.pop() {
                raw.push(s);
            }
            res.clear();
            rs.process(&raw, &mut res);
            for &s in &res {
                frame.push(s);
                if frame.len() < FRAME_SAMPLES {
                    continue;
                }
                // 降噪 → 說話判斷 → 自動音量 → 防爆音（audio::frontend）
                let cfg = FrontendCfg {
                    denoise: shared.denoise.load(Ordering::Relaxed),
                    speech_mix: load_f32(&shared.speech_mix),
                    gate_dbfs: load_f32(&shared.gate_dbfs),
                    agc: shared.agc.load(Ordering::Relaxed),
                    strong: shared.strong_denoise.load(Ordering::Relaxed),
                };
                let info = frontend.process(&mut frame, &cfg);
                store_f32(&shared.mic_peak, info.raw_peak);
                store_f32(&shared.mic_vad, info.vad_prob);
                // 一直編碼，讓編碼器狀態連續；只是決定要不要送、送給哪些頻道
                let n = match enc.encode(&frame, &mut pkt) {
                    Ok(n) => n,
                    Err(_) => {
                        frame.clear();
                        continue;
                    }
                };
                frame.clear();

                let muted = shared.is_muted();
                let ptt_held = shared
                    .ptt_keys
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(k, _)| k.held())
                    .fold(0u64, |m, (_, b)| m | b);
                let mask = talk.step(&TalkInputs {
                    open: shared.open_mask.load(Ordering::Relaxed),
                    vad: shared.vad_mask.load(Ordering::Relaxed),
                    voiced: info.voiced,
                    ptt_held,
                    muted,
                });

                // 提示音：靜音切換；沒靜音時 PTT 按下與放開
                if muted != prev_muted {
                    shared.cue(if muted { Cue::Muted } else { Cue::Unmuted });
                } else if !muted && (ptt_held == 0) != (prev_ptt == 0) {
                    let slot = |m: u64| m.trailing_zeros() as u8;
                    shared.cue(if ptt_held != 0 { Cue::PttOn(slot(ptt_held)) } else { Cue::PttOff(slot(prev_ptt)) });
                }
                (prev_ptt, prev_muted) = (ptt_held, muted);

                let want = mask != 0;

                if want {
                    if !active {
                        if let Some(p) = prev.take() {
                            client.send_voice(Kind::Voice, mask, shared.next_seq(), &p);
                        }
                    }
                    client.send_voice(Kind::Voice, mask, shared.next_seq(), &pkt[..n]);
                } else if active {
                    client.send_voice(Kind::EndOfStream, 0, shared.next_seq(), &[]);
                }
                active = want;
                shared.tx_mask.store(mask, Ordering::Relaxed);
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
                    shared.last_heard.lock().unwrap().insert(v.sender_idx, (Instant::now(), v.via_mask));
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
    out_rate: u32,
) -> Result<JoinHandle<()>> {
    // 以「幀」（左右一對）為單位，裝置取樣率
    let target = (out_rate * buffer_ms / 1000) as usize;
    // 混音是 48 kHz 立體聲 → 裝置取樣率
    let mut rs = Resampler::new(SAMPLE_RATE, out_rate, 2).map_err(anyhow::Error::from_boxed)?;
    let h = std::thread::Builder::new().name("playback".into()).spawn(move || {
        let mut decoders: HashMap<SessionIdx, Decoder> = HashMap::new();
        let mut voices: HashMap<SessionIdx, Vec<f32>> = HashMap::new();
        let mut routed: Vec<(SessionIdx, Route)> = Vec::new();
        let mut mix = vec![0f32; FRAME_SAMPLES * 2];
        let mut tones = TonePlayer::default();
        let mut res: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 4);
        let mut warmed = false;
        while !shared.stop.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) {
            if (cap * 2 - out.slots()) / 2 >= target {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            mix.fill(0.0);
            routed.clear();
            let now = shared.now_us();
            {
                let mut jbs = shared.jitter.lock().unwrap();
                decoders.retain(|k, _| jbs.contains_key(k));
                voices.retain(|k, _| jbs.contains_key(k));
                let heard = shared.last_heard.lock().unwrap().clone();
                let slots = *shared.slots.lock().unwrap();
                for (&idx, jb) in jbs.iter_mut() {
                    let pull = jb.pull(now);
                    if pull == Pull::Silent {
                        continue;
                    }
                    let dec = match decoders.entry(idx) {
                        std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                        std::collections::hash_map::Entry::Vacant(e) => match Decoder::new() {
                            Ok(d) => e.insert(d),
                            Err(_) => continue,
                        },
                    };
                    let buf = voices.entry(idx).or_insert_with(|| vec![0f32; FRAME_SAMPLES]);
                    // 一定要解碼（讓解碼器狀態連續），即使這位說話者所在頻道都被調成靜音
                    let ok = match pull {
                        Pull::Decode(p) => dec.decode(&p, buf),
                        Pull::Fec(p) => dec.conceal(Some(&p), buf),
                        Pull::Plc => dec.conceal(None, buf),
                        Pull::Silent => unreachable!(),
                    };
                    if ok.is_err() {
                        continue;
                    }
                    // 經由哪些頻道聽到 → 音量／定位／是否優先；未知槽位（例如設定還沒同步）用預設
                    let via = heard.get(&idx).map(|&(_, v)| v).unwrap_or(0);
                    let default = ChannelMix::default();
                    let mixes = (0..SLOTS)
                        .filter(|&s| via & (1 << s) != 0)
                        .map(|s| slots[s].mix.as_ref().unwrap_or(&default));
                    let r = if via == 0 { route([&default]) } else { route(mixes) };
                    if let Some(r) = r {
                        routed.push((idx, r));
                    }
                }
            }
            // 優先頻道有人說話：其他人的聲音壓低
            let ducking = routed.iter().any(|(_, r)| r.priority);
            let user_gain = shared.user_gain.lock().unwrap().clone();
            for (idx, r) in &routed {
                let g = if ducking && !r.priority { r.gain * DUCK_GAIN } else { r.gain };
                let g = g * user_gain.get(idx).copied().unwrap_or(1.0);
                mix_into_stereo(&mut mix, &voices[idx], g, r.pan);
            }
            let vol = load_f32(&shared.volume);
            if shared.is_deafened() {
                mix.fill(0.0);
            } else if vol != 1.0 {
                mix.iter_mut().for_each(|s| *s *= vol);
            }
            // 提示音：拒聽時也播（自己的狀態提示），跟著輸出音量
            if let Some(c) = shared.cue.lock().unwrap().take() {
                tones.play(c);
            }
            tones.mix_into(&mut mix, vol);
            soft_clip(&mut mix);
            res.clear();
            rs.process(&mix, &mut res);
            for &s in &res {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deafen_also_mutes() {
        let s = Shared::new();
        s.set_deafened(true);
        assert!(s.is_muted(), "拒聽 = 不聽也不說");
        s.set_deafened(false);
        assert!(!s.is_muted(), "取消拒聽：回到原本沒靜音");

        s.set_muted(true);
        s.set_deafened(true);
        s.set_deafened(false);
        assert!(s.is_muted(), "拒聽前就靜音：取消拒聽後仍靜音");

        s.set_muted(false);
        s.set_deafened(true);
        s.set_muted(false);
        assert!(!s.is_muted() && !s.is_deafened(), "拒聽中取消靜音：連拒聽一起取消");

        s.set_deafened(true);
        s.toggle_mute();
        assert!(!s.is_muted() && !s.is_deafened(), "拒聽中按靜音鍵：全部打開");
        s.toggle_mute();
        assert!(s.is_muted() && !s.is_deafened());
    }
}
