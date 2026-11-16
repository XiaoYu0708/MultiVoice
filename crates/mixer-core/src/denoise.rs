//! 降噪 + 人聲機率（RNNoise 的純 Rust 移植：nnnoiseless）。桌面版（`audio::denoise`）與網頁版共用。
//!
//! - 48 kHz；每 480 取樣（10 ms）處理一次；一個 Opus 幀（960）切兩段
//! - 會多出約 10 ms 的演算法延遲（重疊相加視窗）
//! - 回傳的人聲機率（0..1）可作為 VAD 的參考，但不能單獨使用（風扇類噪音也可能被判高）
//!
//! **人聲時混回原音（speech mix）**：RNNoise 在訊噪比低時會連小聲的人聲一起壓掉。
//! 判定有人聲時，把延遲對齊後的原始訊號以 `speech_mix` 的比例混回去；
//! 安靜時不混，所以背景噪音仍然完全被壓掉，人聲時混回的噪音則被人聲遮蔽。

use nnnoiseless::DenoiseState;

const SAMPLE_RATE: f32 = 48_000.0;
/// 一個 Opus 幀（20 ms）
pub const FRAME_SAMPLES: usize = 960;
const SUB: usize = DenoiseState::FRAME_SIZE; // 480
const _: () = assert!(FRAME_SAMPLES % SUB == 0);

/// nnnoiseless 預期 16-bit PCM 的數值刻度
const SCALE: f32 = 32768.0;

/// RNNoise 的輸入→輸出延遲剛好是一段（480 取樣）：這次的輸出對應上一段的輸入
/// （2026-11-15 以互相關實測為 479～480 取樣）。
const VAD_ON: f32 = 0.25;

pub struct Denoiser {
    st: Box<DenoiseState<'static>>,
    inp: [f32; SUB],
    out: [f32; SUB],
    /// 上一段的原始訊號（-1..1），與這次的輸出對齊
    prev_dry: [f32; SUB],
    prev_vad: f32,
    speech_mix: f32,
    /// 目前的混音比例（平滑後）
    k: f32,
    release: f32,
}

impl Default for Denoiser {
    fn default() -> Self {
        Self::new()
    }
}

impl Denoiser {
    pub fn new() -> Self {
        Self {
            st: DenoiseState::new(),
            inp: [0.0; SUB],
            out: [0.0; SUB],
            prev_dry: [0.0; SUB],
            prev_vad: 0.0,
            speech_mix: 0.3,
            k: 0.0,
            // 講完話後約 150 ms 收回
            release: (-1.0 / (0.150 * SAMPLE_RATE)).exp(),
        }
    }

    /// 判定有人聲時混回原音的比例（0 = 純 RNNoise；預設 0.3 ≈ -10.5 dB）
    pub fn set_speech_mix(&mut self, mix: f32) {
        self.speech_mix = mix.clamp(0.0, 1.0);
    }

    /// 就地降噪一幀（`FRAME_SAMPLES` 個取樣，範圍 -1..1），回傳這幀的人聲機率（兩段取大者）。
    pub fn process(&mut self, pcm: &mut [f32]) -> f32 {
        debug_assert_eq!(pcm.len(), FRAME_SAMPLES);
        let mut vad = 0f32;
        for chunk in pcm.chunks_exact_mut(SUB) {
            for (d, &s) in self.inp.iter_mut().zip(chunk.iter()) {
                *d = s * SCALE;
            }
            let p = self.st.process_frame(&mut self.out, &self.inp);
            vad = vad.max(p);
            // 輸出對應上一段；用「上一段或這一段」有人聲來判斷 → 多看 10 ms，不會切掉字頭
            let target = if p.max(self.prev_vad) > VAD_ON { self.speech_mix } else { 0.0 };
            for i in 0..SUB {
                self.k = if target >= self.k { target } else { target + (self.k - target) * self.release };
                chunk[i] = (self.out[i] / SCALE + self.k * self.prev_dry[i]).clamp(-1.0, 1.0);
            }
            for (d, &s) in self.prev_dry.iter_mut().zip(self.inp.iter()) {
                *d = s / SCALE;
            }
            self.prev_vad = p;
        }
        vad
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    /// 風扇類（偏低頻）的穩定噪音，在 -25 dBFS RMS 下應壓低 20 dB 以上。
    ///
    /// 實測（2026-11-15）：風扇類噪音可壓低 35～50 dB；但寬頻白噪音在 -25 dBFS 以上
    /// 只能壓低 1～3 dB，而且風扇類噪音的人聲機率高達 0.9，所以人聲機率不能單獨當 VAD。
    #[test]
    fn suppresses_fan_like_noise() {
        let mut dn = Denoiser::new();
        // 合成的風扇噪音會被判成人聲（見上），這裡驗證的是 RNNoise 本身，關掉混音
        dn.set_speech_mix(0.0);
        let mut seed = 0xDEAD_BEEF_1234_5678u64;
        let mut lp = 0f32;
        let target_rms = 0.055; // ≈ -25 dBFS
        let (mut before, mut after) = (Vec::new(), Vec::new());
        for f in 0..150 {
            let mut frame: Vec<f32> = (0..FRAME_SAMPLES)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    let w = (seed >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0;
                    lp = 0.9 * lp + 0.1 * w;
                    lp
                })
                .collect();
            let g = target_rms / rms(&frame);
            frame.iter_mut().for_each(|v| *v *= g);
            if f >= 50 {
                before.extend_from_slice(&frame);
            }
            dn.process(&mut frame);
            assert!(frame.iter().all(|v| v.is_finite()));
            if f >= 50 {
                after.extend_from_slice(&frame);
            }
        }
        let reduction_db = 20.0 * (rms(&before) / rms(&after).max(1e-9)).log10();
        assert!(reduction_db > 20.0, "只壓低了 {reduction_db:.1} dB");
    }

    /// 類人聲訊號：混音開啟時保留的能量要比純 RNNoise 多，且延遲對齊（不應因相位抵銷而變小）
    #[test]
    fn speech_mix_preserves_more_voice() {
        let voice = |i: usize| {
            let t = i as f32 / 48000.0;
            let env = (t * 4.0 * std::f32::consts::TAU).sin().abs();
            env * (1..8)
                .map(|h| (t * 180.0 * h as f32 * std::f32::consts::TAU).sin() / h as f32)
                .sum::<f32>()
                * 0.05
        };
        let run = |mix: f32| {
            let mut dn = Denoiser::new();
            dn.set_speech_mix(mix);
            let mut e = 0f64;
            for f in 0..100 {
                let mut fr: Vec<f32> = (0..FRAME_SAMPLES).map(|i| voice(f * FRAME_SAMPLES + i)).collect();
                dn.process(&mut fr);
                if f >= 20 {
                    e += fr.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
                }
            }
            e
        };
        let (pure, mixed) = (run(0.0), run(0.3));
        assert!(mixed > pure * 1.05, "pure {pure:.3} vs mixed {mixed:.3}");
    }
}
