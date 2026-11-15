//! 串流重取樣：裝置取樣率不是 48 kHz（例如 44.1 kHz 耳機）時，在擷取／播放執行緒裡轉換。
//!
//! 包裝 rubato 的 `Async`（多項式插值）：一次可以餵任意長度的交錯資料，湊滿一塊就轉一塊。
//! 取樣率相同時直接原樣傳過，不經過 rubato。
//! **不要在音訊回呼裡用**（會配置記憶體）；放在擷取／播放執行緒。

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, FixedAsync, PolynomialDegree, Resampler as _};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub struct Resampler {
    inner: Option<Async<f32>>,
    channels: usize,
    /// 還沒湊滿一塊的輸入（交錯）
    pending: Vec<f32>,
    /// 單塊輸出的暫存
    chunk_out: Vec<f32>,
}

impl Resampler {
    /// `from` → `to` Hz，`channels` 聲道交錯
    pub fn new(from: u32, to: u32, channels: usize) -> Result<Self, Error> {
        let inner = if from == to {
            None
        } else {
            // 每塊 10 ms 的輸入
            let chunk = (from as usize / 100).max(32);
            let r = Async::<f32>::new_poly(
                to as f64 / from as f64,
                1.0,
                PolynomialDegree::Septic,
                chunk,
                channels,
                FixedAsync::Input,
            )?;
            Some(r)
        };
        let max_out = inner.as_ref().map(|r| r.output_frames_max()).unwrap_or(0);
        Ok(Self { inner, channels, pending: Vec::new(), chunk_out: vec![0.0; max_out * channels] })
    }

    /// 是否真的有在轉換（取樣率不同）
    pub fn active(&self) -> bool {
        self.inner.is_some()
    }

    /// 餵入交錯資料，轉換好的部分接到 `out` 後面
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let Some(r) = self.inner.as_mut() else {
            out.extend_from_slice(input);
            return;
        };
        let ch = self.channels;
        self.pending.extend_from_slice(input);
        let mut used = 0;
        loop {
            let need = r.input_frames_next();
            if (self.pending.len() - used) / ch < need {
                break;
            }
            let src = &self.pending[used..used + need * ch];
            let n_out = r.output_frames_next();
            let (Ok(a), Ok(mut b)) = (
                InterleavedSlice::new(src, ch, need),
                InterleavedSlice::new_mut(&mut self.chunk_out[..n_out * ch], ch, n_out),
            ) else {
                break;
            };
            match r.process_into_buffer(&a, &mut b, None) {
                Ok((read, written)) => {
                    out.extend_from_slice(&self.chunk_out[..written * ch]);
                    used += read * ch;
                }
                Err(_) => {
                    // 不該發生；丟掉這塊避免卡住
                    used += need * ch;
                }
            }
        }
        self.pending.drain(..used);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f32, secs: f32, ch: usize) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n)
            .flat_map(|i| {
                let v = (i as f32 / rate as f32 * hz * std::f32::consts::TAU).sin() * 0.5;
                std::iter::repeat_n(v, ch)
            })
            .collect()
    }

    /// 用零交越估計頻率
    fn freq(x: &[f32], rate: u32, ch: usize) -> f32 {
        let mono: Vec<f32> = x.chunks_exact(ch).map(|f| f[0]).collect();
        let crossings = mono.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        crossings as f32 * rate as f32 / mono.len() as f32
    }

    #[test]
    fn passthrough_when_same_rate() {
        let mut r = Resampler::new(48_000, 48_000, 2).unwrap();
        assert!(!r.active());
        let x = sine(48_000, 1000.0, 0.1, 2);
        let mut out = Vec::new();
        r.process(&x, &mut out);
        assert_eq!(out, x);
    }

    #[test]
    fn converts_44k_to_48k_and_back() {
        for (from, to, ch) in [(44_100, 48_000, 1), (48_000, 44_100, 2), (96_000, 48_000, 1)] {
            let mut r = Resampler::new(from, to, ch).unwrap();
            assert!(r.active());
            let x = sine(from, 1000.0, 1.0, ch);
            let mut out = Vec::new();
            // 用不規則的塊大小餵，模擬環形緩衝每次取到的量不同
            for piece in x.chunks(50 * ch) {
                r.process(piece, &mut out);
            }
            let frames = out.len() / ch;
            let expect = to as usize; // 1 秒
            assert!(
                frames.abs_diff(expect) <= expect / 50,
                "{from}->{to}：輸出 {frames} 幀，預期約 {expect}"
            );
            // 頻率不變（跳過開頭延遲）
            let f = freq(&out[out.len() / 4..], to, ch);
            assert!((f - 1000.0).abs() < 10.0, "{from}->{to}：頻率 {f}");
            // 振幅大致保持
            let peak = out[out.len() / 4..].iter().fold(0f32, |m, v| m.max(v.abs()));
            assert!((peak - 0.5).abs() < 0.03, "{from}->{to}：峰值 {peak}");
        }
    }
}
