//! 麥克風前端處理（每幀 20 ms、48 kHz mono）：降噪 → 說話判斷（VAD）→ 自動增益（AGC）→ 防爆音。
//! 正式通話（session 的擷取執行緒）與麥克風測試共用。
//!
//! - VAD：RNNoise 人聲機率 ≥ 0.6 **而且** 降噪後音量 ≥ 門檻（可調；PLAN：人聲機率不能單獨使用，
//!   風扇類噪音也會被判成人聲）。音量是 AGC **之前**的，門檻數值才不會被 AGC 影響
//! - AGC：只在說話時調整，把說話音量拉向 -20 dBFS（最多放大 15 dB、縮小 6 dB）；
//!   太大聲時快速壓（約 0.2 秒）、太小聲時慢慢放大（約 2 秒）；安靜時維持原增益，不會把背景噪音放大
//! - 防爆音：增益後峰值超過 0.95 就整幀縮回
//! - 底噪估計：音量的慢速下包絡（往下立即跟、往上約 10 秒），給「依環境噪音設定門檻」用

use crate::codec::FRAME_SAMPLES;
use crate::denoise::Denoiser;

/// 人聲機率門檻
pub const VAD_PROB: f32 = 0.6;
/// 預設的說話音量門檻
pub const DEFAULT_GATE_DBFS: f32 = -50.0;
const AGC_TARGET_DBFS: f32 = -20.0;
const AGC_MAX_GAIN_DB: f32 = 15.0;
const AGC_MIN_GAIN_DB: f32 = -6.0;
/// 每幀往目標靠近的比例：壓（約 0.2 秒）、放大（約 2 秒）
const AGC_ATTACK: f32 = 0.1;
const AGC_RELEASE: f32 = 0.01;
const LIMIT: f32 = 0.95;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrontendCfg {
    pub denoise: bool,
    /// 降噪時人聲混回原音的比例
    pub speech_mix: f32,
    /// 說話音量門檻（dBFS）
    pub gate_dbfs: f32,
    pub agc: bool,
}

impl Default for FrontendCfg {
    fn default() -> Self {
        Self { denoise: true, speech_mix: 0.3, gate_dbfs: DEFAULT_GATE_DBFS, agc: true }
    }
}

/// 一幀的分析結果
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameInfo {
    /// 原始峰值（降噪前，0..1）
    pub raw_peak: f32,
    /// 降噪後、增益前的音量（dBFS RMS）
    pub level_db: f32,
    /// 人聲機率（沒開降噪時固定 1）
    pub vad_prob: f32,
    /// 這幀判定為說話
    pub voiced: bool,
    /// 目前的 AGC 增益（dB）
    pub gain_db: f32,
    /// 估計的環境噪音（dBFS）
    pub noise_db: f32,
}

pub struct Frontend {
    denoiser: Denoiser,
    gain_db: f32,
    noise_db: Option<f32>,
}

impl Default for Frontend {
    fn default() -> Self {
        Self::new()
    }
}

pub fn rms_db(x: &[f32]) -> f32 {
    let rms = (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt();
    20.0 * rms.max(1e-9).log10()
}

fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

impl Frontend {
    pub fn new() -> Self {
        Self { denoiser: Denoiser::new(), gain_db: 0.0, noise_db: None }
    }

    /// 處理一幀（就地改寫 `frame`）
    pub fn process(&mut self, frame: &mut [f32], cfg: &FrontendCfg) -> FrameInfo {
        debug_assert_eq!(frame.len(), FRAME_SAMPLES);
        let raw_peak = frame.iter().fold(0f32, |m, v| m.max(v.abs()));
        let vad_prob = if cfg.denoise {
            self.denoiser.set_speech_mix(cfg.speech_mix);
            self.denoiser.process(frame)
        } else {
            // 沒有降噪就沒有人聲機率，只看音量
            1.0
        };
        let level_db = rms_db(frame);
        let voiced = vad_prob >= VAD_PROB && level_db >= cfg.gate_dbfs;

        // 底噪：往下立即跟，往上很慢（說話時不更新）
        let noise = self.noise_db.get_or_insert(level_db);
        if level_db < *noise {
            *noise = level_db;
        } else if !voiced {
            *noise += (level_db - *noise) * 0.002;
        }
        let noise_db = *noise;

        // AGC
        let prev = self.gain_db;
        if !cfg.agc {
            self.gain_db = 0.0;
        } else if voiced {
            let want = (AGC_TARGET_DBFS - level_db).clamp(AGC_MIN_GAIN_DB, AGC_MAX_GAIN_DB);
            let k = if want < self.gain_db { AGC_ATTACK } else { AGC_RELEASE };
            self.gain_db += (want - self.gain_db) * k;
        }
        if prev != 0.0 || self.gain_db != 0.0 {
            // 幀內線性過渡，避免增益跳動造成雜音
            let (g0, g1) = (db_to_lin(prev), db_to_lin(self.gain_db));
            let n = frame.len() as f32;
            for (i, s) in frame.iter_mut().enumerate() {
                *s *= g0 + (g1 - g0) * (i as f32 / n);
            }
        }
        // 防爆音
        let peak = frame.iter().fold(0f32, |m, v| m.max(v.abs()));
        if peak > LIMIT {
            let k = LIMIT / peak;
            frame.iter_mut().for_each(|s| *s *= k);
        }
        FrameInfo { raw_peak, level_db, vad_prob, voiced, gain_db: self.gain_db, noise_db }
    }
}

/// 依環境噪音建議的門檻：比底噪高 12 dB，限制在 -70..-20
pub fn suggest_gate(noise_db: f32) -> f32 {
    (noise_db + 12.0).clamp(-70.0, -20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amp: f32, phase: &mut f32) -> Vec<f32> {
        (0..FRAME_SAMPLES)
            .map(|_| {
                *phase += 300.0 / 48_000.0 * std::f32::consts::TAU;
                phase.sin() * amp
            })
            .collect()
    }

    // 不開降噪（人聲機率固定 1），只測音量門檻、AGC、底噪
    const CFG: FrontendCfg = FrontendCfg { denoise: false, speech_mix: 0.0, gate_dbfs: -40.0, agc: true };

    #[test]
    fn gate_decides_voiced() {
        let mut f = Frontend::new();
        let mut ph = 0.0;
        let quiet = f.process(&mut tone(0.003, &mut ph), &CFG); // 約 -53 dBFS
        assert!(!quiet.voiced, "{quiet:?}");
        let loud = f.process(&mut tone(0.1, &mut ph), &CFG); // 約 -23 dBFS
        assert!(loud.voiced, "{loud:?}");
    }

    #[test]
    fn agc_raises_quiet_speech_and_limits_loud() {
        let mut f = Frontend::new();
        let mut ph = 0.0;
        // 小聲說話（約 -33 dBFS）：幾秒後應被拉近 -20 dBFS
        let mut out = Vec::new();
        for _ in 0..300 {
            out = tone(0.032, &mut ph);
            f.process(&mut out, &CFG);
        }
        let after = rms_db(&out);
        assert!((after - AGC_TARGET_DBFS).abs() < 2.0, "放大後 {after} dBFS");

        // 突然很大聲：不爆音，而且很快壓下來
        let mut peak = 0f32;
        for _ in 0..25 {
            let mut x = tone(0.9, &mut ph);
            let info = f.process(&mut x, &CFG);
            peak = peak.max(x.iter().fold(0f32, |m, v| m.max(v.abs())));
            assert!(info.gain_db <= AGC_MAX_GAIN_DB);
        }
        assert!(peak <= LIMIT + 1e-4, "峰值 {peak}");
        assert!(f.gain_db < 0.0, "大聲時增益要降到 0 dB 以下：{}", f.gain_db);
    }

    #[test]
    fn agc_holds_gain_in_silence() {
        let mut f = Frontend::new();
        let mut ph = 0.0;
        for _ in 0..200 {
            f.process(&mut tone(0.032, &mut ph), &CFG);
        }
        let g = f.gain_db;
        for _ in 0..200 {
            f.process(&mut tone(0.001, &mut ph), &CFG); // 安靜
        }
        assert_eq!(f.gain_db, g, "安靜時不改增益");
    }

    #[test]
    fn noise_floor_tracks_background() {
        let mut f = Frontend::new();
        let mut ph = 0.0;
        let mut info = FrameInfo::default();
        for _ in 0..100 {
            info = f.process(&mut tone(0.002, &mut ph), &CFG); // 約 -57 dBFS 的背景
        }
        assert!((info.noise_db - rms_db(&tone(0.002, &mut 0.0))).abs() < 1.5, "{info:?}");
        // 中間有人說話，底噪不能被拉上去太多
        for _ in 0..50 {
            info = f.process(&mut tone(0.1, &mut ph), &CFG);
        }
        assert!(info.noise_db < -50.0, "{info:?}");
        assert_eq!(suggest_gate(-90.0), -70.0, "下限");
        assert_eq!(suggest_gate(-10.0), -20.0, "上限");
        assert_eq!(suggest_gate(-50.0), -38.0);
    }
}
