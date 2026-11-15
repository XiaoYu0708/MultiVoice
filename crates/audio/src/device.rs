//! 音訊裝置 I/O（cpal / WASAPI）。
//!
//! - 輸入：任意聲道數 → 混成 mono f32 → 推入 `rtrb` 環形緩衝
//! - 輸出：從 `rtrb` 取 mono f32 → 複製到每個聲道；不足時補靜音並計一次欠載
//! - 回呼內不配置記憶體、不上鎖、不印 log；統計值一律走 atomic
//!
//! 目前只支援 48 kHz 的裝置（與 Opus 一致）。其他取樣率會回傳錯誤，重取樣之後再加。

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, StreamConfig};

use crate::codec::SAMPLE_RATE;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

const NANOS_PER_SAMPLE: f64 = 1e9 / SAMPLE_RATE as f64;

/// 串流的即時統計，由音訊回呼寫入、其他執行緒讀取。
#[derive(Default)]
pub struct StreamStats {
    /// 輸出裝置延遲（µs）：callback→預計播放時間。
    /// 輸入端不提供：cpal/WASAPI 的 callback 時間取自 IAudioClock 的快取值，
    /// 常常早於 GetBuffer 給的擷取時間，相減沒有意義（擷取時間本身是可靠的）。
    pub device_latency_us: AtomicU64,
    /// 輸入：環形緩衝滿而丟棄的取樣數；輸出：發生欠載的回呼次數
    pub xruns: AtomicU64,
    /// 裝置回報的 xrun（WASAPI 資料不連續旗標）次數
    pub device_xruns: AtomicU64,
    /// 自上次讀取以來的峰值（f32 bits；非負浮點數的 bit 順序與數值順序一致，可直接 fetch_max）
    peak_bits: AtomicU32,
    /// 回呼被呼叫的次數
    pub callbacks: AtomicU64,
}

impl StreamStats {
    fn note_peak(&self, peak: f32) {
        self.peak_bits.fetch_max(peak.to_bits(), Ordering::Relaxed);
    }

    /// 取出並歸零峰值
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak_bits.swap(0, Ordering::Relaxed))
    }
}

/// 聲音起點偵測（門檻隨底噪自動調整）：
/// 包絡線低於 `底噪 × 3` 持續 200 ms 後，第一個超過 `max(底噪 × 6, 0.1)` 的取樣即為起點，
/// 把該取樣的時間（ns，與 cpal StreamInstant 同一時鐘）送出。
/// 用來量測端到端延遲：同一個拍手聲在輸入端與輸出端各被偵測一次，兩者相減即延遲。
pub struct OnsetDetector {
    /// 峰值包絡線（約 30 ms 衰減）
    env: f32,
    env_decay: f32,
    /// 底噪估計：往下立即跟隨，往上緩慢爬升（前 0.5 秒時間常數 50 ms 以快速收斂，之後 1 秒）
    floor: f32,
    floor_rise_fast: f32,
    floor_rise: f32,
    warmup_left: u32,
    rearm: u32,
    quiet: u32,
    armed: bool,
    out: rtrb::Producer<u64>,
}

impl OnsetDetector {
    pub fn new(out: rtrb::Producer<u64>) -> Self {
        let sr = SAMPLE_RATE as f32;
        Self {
            env: 0.0,
            env_decay: (-1.0 / (0.030 * sr)).exp(),
            floor: 0.01,
            floor_rise_fast: 1.0 / (0.05 * sr),
            floor_rise: 1.0 / (1.0 * sr),
            warmup_left: SAMPLE_RATE / 2,
            rearm: SAMPLE_RATE / 5, // 200 ms
            quiet: 0,
            armed: false,
            out,
        }
    }

    #[inline]
    fn feed(&mut self, s: f32, t_ns: u64) {
        let a = s.abs();
        self.env = a.max(self.env * self.env_decay);
        if self.env < self.floor {
            self.floor = self.env.max(1e-4);
        } else {
            let k = if self.warmup_left > 0 { self.floor_rise_fast } else { self.floor_rise };
            self.floor += (self.env - self.floor) * k;
        }
        self.warmup_left = self.warmup_left.saturating_sub(1);
        let on = (self.floor * 6.0).max(0.1);
        let off = (self.floor * 3.0).max(0.005);

        if self.armed {
            if a >= on {
                let _ = self.out.push(t_ns);
                self.armed = false;
                self.quiet = 0;
            }
        } else if self.env < off {
            self.quiet += 1;
            if self.quiet >= self.rearm {
                self.armed = true;
            }
        } else {
            self.quiet = 0;
        }
    }
}

/// 以名稱子字串挑選裝置；`None` 時使用系統預設裝置。
pub fn find_device(input: bool, name_contains: Option<&str>) -> Result<cpal::Device, Error> {
    let host = cpal::default_host();
    match name_contains {
        None => if input { host.default_input_device() } else { host.default_output_device() }
            .ok_or_else(|| "找不到預設音訊裝置".into()),
        Some(sub) => {
            let sub = sub.to_lowercase();
            let devices = if input { host.input_devices()? } else { host.output_devices()? };
            for d in devices {
                if device_name(&d).to_lowercase().contains(&sub) {
                    return Ok(d);
                }
            }
            Err(format!("找不到名稱包含「{sub}」的裝置").into())
        }
    }
}

pub fn device_name(d: &cpal::Device) -> String {
    d.description().map(|x| x.to_string()).unwrap_or_else(|_| "(未知裝置)".into())
}

fn check_rate(rate: u32) -> Result<(), Error> {
    if rate == SAMPLE_RATE {
        Ok(())
    } else {
        Err(format!("裝置取樣率為 {rate} Hz，目前只支援 {SAMPLE_RATE} Hz（重取樣尚未實作）").into())
    }
}

/// 開啟輸入串流。回傳的 `cpal::Stream` 被 drop 時串流就會停止。
pub fn open_input(
    dev: &cpal::Device,
    ring: rtrb::Producer<f32>,
    stats: Arc<StreamStats>,
    onset: Option<OnsetDetector>,
) -> Result<(cpal::Stream, u16), Error> {
    let sc = dev.default_input_config()?;
    check_rate(sc.sample_rate())?;
    let config: StreamConfig = sc.config();
    let ch = config.channels;
    let stream = match sc.sample_format() {
        SampleFormat::F32 => build_input::<f32>(dev, &config, ring, stats, onset)?,
        SampleFormat::I16 => build_input::<i16>(dev, &config, ring, stats, onset)?,
        SampleFormat::I32 => build_input::<i32>(dev, &config, ring, stats, onset)?,
        f => return Err(format!("不支援的取樣格式 {f:?}").into()),
    };
    stream.play()?;
    Ok((stream, ch))
}

fn build_input<T>(
    dev: &cpal::Device,
    config: &StreamConfig,
    mut ring: rtrb::Producer<f32>,
    stats: Arc<StreamStats>,
    mut onset: Option<OnsetDetector>,
) -> Result<cpal::Stream, Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let ch = config.channels as usize;
    let inv = 1.0 / ch as f32;
    let on_error = error_handler(stats.clone(), "輸入");
    let stream = dev.build_input_stream(
        *config,
        move |data: &[T], info: &cpal::InputCallbackInfo| {
            let ts = info.timestamp();
            stats.callbacks.fetch_add(1, Ordering::Relaxed);
            // capture = 這批資料第一個取樣的擷取時間（WASAPI GetBuffer 的 QPC 位置）
            let t0 = ts.capture.as_nanos() as f64;
            let mut peak = 0f32;
            let mut dropped = 0u64;
            for (i, frame) in data.chunks_exact(ch).enumerate() {
                let s = frame.iter().map(|&x| f32::from_sample(x)).sum::<f32>() * inv;
                peak = peak.max(s.abs());
                if let Some(o) = onset.as_mut() {
                    o.feed(s, (t0 + i as f64 * NANOS_PER_SAMPLE) as u64);
                }
                if ring.push(s).is_err() {
                    dropped += 1;
                }
            }
            stats.note_peak(peak);
            if dropped > 0 {
                stats.xruns.fetch_add(dropped, Ordering::Relaxed);
            }
        },
        on_error,
        None,
    )?;
    Ok(stream)
}

/// xrun 只計數（串流剛啟動時 WASAPI 常回報一次），其他錯誤才印出來。
fn error_handler(stats: Arc<StreamStats>, label: &'static str) -> impl FnMut(cpal::Error) + Send + 'static {
    move |e: cpal::Error| {
        if e.kind() == cpal::ErrorKind::Xrun {
            stats.device_xruns.fetch_add(1, Ordering::Relaxed);
        } else {
            eprintln!("{label}串流錯誤：{e}");
        }
    }
}

/// 開啟輸出串流。`gain` 為輸出增益（0 = 靜音，用於不出聲的自動測試）。
pub fn open_output(
    dev: &cpal::Device,
    ring: rtrb::Consumer<f32>,
    stats: Arc<StreamStats>,
    onset: Option<OnsetDetector>,
    gain: f32,
) -> Result<(cpal::Stream, u16), Error> {
    let sc = dev.default_output_config()?;
    check_rate(sc.sample_rate())?;
    let config: StreamConfig = sc.config();
    let ch = config.channels;
    let stream = match sc.sample_format() {
        SampleFormat::F32 => build_output::<f32>(dev, &config, ring, stats, onset, gain)?,
        SampleFormat::I16 => build_output::<i16>(dev, &config, ring, stats, onset, gain)?,
        SampleFormat::I32 => build_output::<i32>(dev, &config, ring, stats, onset, gain)?,
        f => return Err(format!("不支援的取樣格式 {f:?}").into()),
    };
    stream.play()?;
    Ok((stream, ch))
}

fn build_output<T>(
    dev: &cpal::Device,
    config: &StreamConfig,
    mut ring: rtrb::Consumer<f32>,
    stats: Arc<StreamStats>,
    mut onset: Option<OnsetDetector>,
    gain: f32,
) -> Result<cpal::Stream, Error>
where
    T: SizedSample + FromSample<f32>,
{
    let ch = config.channels as usize;
    let on_error = error_handler(stats.clone(), "輸出");
    let stream = dev.build_output_stream(
        *config,
        move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
            let ts = info.timestamp();
            stats.callbacks.fetch_add(1, Ordering::Relaxed);
            stats.device_latency_us.store(
                ts.playback.duration_since(ts.callback).as_micros() as u64,
                Ordering::Relaxed,
            );
            let t0 = ts.playback.as_nanos() as f64;
            let mut peak = 0f32;
            let mut underrun = false;
            for (i, frame) in data.chunks_exact_mut(ch).enumerate() {
                let s = match ring.pop() {
                    Ok(s) => s,
                    Err(_) => {
                        underrun = true;
                        0.0
                    }
                };
                peak = peak.max(s.abs());
                // 偵測用的是增益前的訊號，所以 gain = 0 時仍可量測延遲
                if let Some(o) = onset.as_mut() {
                    o.feed(s, (t0 + i as f64 * NANOS_PER_SAMPLE) as u64);
                }
                let v = T::from_sample(s * gain);
                frame.fill(v);
            }
            stats.note_peak(peak);
            if underrun {
                stats.xruns.fetch_add(1, Ordering::Relaxed);
            }
        },
        on_error,
        None,
    )?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 在峰值約 -22 dBFS 的噪音中放兩下「拍手」，兩下都要在正確時間被偵測到
    #[test]
    fn onset_detector_finds_claps_in_noise() {
        let (tx, mut rx) = rtrb::RingBuffer::<u64>::new(16);
        let mut det = OnsetDetector::new(tx);
        let mut seed = 0x1234_5678_9abc_def0u64;
        let mut noise = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            ((seed >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0) * 0.08
        };
        let sr = SAMPLE_RATE as usize;
        let claps = [sr, sr * 2]; // 第 1 秒、第 2 秒
        for i in 0..sr * 3 {
            let mut s = noise();
            if claps.iter().any(|&c| (c..c + 96).contains(&i)) {
                s = if i % 2 == 0 { 0.8 } else { -0.8 };
            }
            det.feed(s, (i as f64 * NANOS_PER_SAMPLE) as u64);
        }
        let got: Vec<u64> = std::iter::from_fn(|| rx.pop().ok()).collect();
        let want: Vec<u64> = claps.iter().map(|&c| (c as f64 * NANOS_PER_SAMPLE) as u64).collect();
        assert_eq!(got, want, "偵測到的起點（ns）");
    }
}
