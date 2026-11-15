//! 降噪 + 人聲機率（RNNoise 的純 Rust 移植：nnnoiseless）。
//!
//! - 每 480 取樣（10 ms）處理一次；一個 Opus 幀（960）切兩段
//! - 會多出約 10 ms 的演算法延遲（重疊相加視窗）
//! - 回傳的人聲機率（0..1）之後可直接作為 VAD 開麥判斷

use nnnoiseless::DenoiseState;

use crate::codec::FRAME_SAMPLES;

const SUB: usize = DenoiseState::FRAME_SIZE; // 480
const _: () = assert!(FRAME_SAMPLES % SUB == 0);

/// nnnoiseless 預期 16-bit PCM 的數值刻度
const SCALE: f32 = 32768.0;

pub struct Denoiser {
    st: Box<DenoiseState<'static>>,
    inp: [f32; SUB],
    out: [f32; SUB],
}

impl Default for Denoiser {
    fn default() -> Self {
        Self::new()
    }
}

impl Denoiser {
    pub fn new() -> Self {
        Self { st: DenoiseState::new(), inp: [0.0; SUB], out: [0.0; SUB] }
    }

    /// 就地降噪一幀（`FRAME_SAMPLES` 個取樣，範圍 -1..1），回傳這幀的人聲機率（兩段取大者）。
    pub fn process(&mut self, pcm: &mut [f32]) -> f32 {
        debug_assert_eq!(pcm.len(), FRAME_SAMPLES);
        let mut vad = 0f32;
        for chunk in pcm.chunks_exact_mut(SUB) {
            for (d, &s) in self.inp.iter_mut().zip(chunk.iter()) {
                *d = s * SCALE;
            }
            vad = vad.max(self.st.process_frame(&mut self.out, &self.inp));
            for (d, &s) in chunk.iter_mut().zip(self.out.iter()) {
                *d = (s / SCALE).clamp(-1.0, 1.0);
            }
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
}
