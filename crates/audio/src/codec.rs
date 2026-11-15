//! Opus 編解碼的薄包裝：48 kHz、mono、20 ms 幀。

pub const SAMPLE_RATE: u32 = 48_000;
pub const FRAME_MS: u32 = 20;
/// 每幀取樣數（mono）：48000 × 0.02 = 960
pub const FRAME_SAMPLES: usize = (SAMPLE_RATE * FRAME_MS / 1000) as usize;
/// 單一 Opus 封包上限；語音在 32 kbps 下約 80 bytes，留足餘裕
pub const MAX_PACKET: usize = 1276;

pub use opus::Error;

pub struct Encoder {
    inner: opus::Encoder,
}

impl Encoder {
    pub fn new(bitrate_bps: i32) -> Result<Self, Error> {
        let mut inner =
            opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Voip)?;
        inner.set_bitrate(opus::Bitrate::Bits(bitrate_bps))?;
        inner.set_inband_fec(true)?;
        inner.set_packet_loss_perc(10)?;
        Ok(Self { inner })
    }

    /// 編碼一幀（必須剛好 `FRAME_SAMPLES` 個取樣），回傳寫入 `out` 的位元組數。
    pub fn encode(&mut self, pcm: &[f32], out: &mut [u8]) -> Result<usize, Error> {
        debug_assert_eq!(pcm.len(), FRAME_SAMPLES);
        self.inner.encode_float(pcm, out)
    }
}

pub struct Decoder {
    inner: opus::Decoder,
}

impl Decoder {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            inner: opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono)?,
        })
    }

    /// 解碼一個封包，回傳取樣數。
    pub fn decode(&mut self, packet: &[u8], out: &mut [f32]) -> Result<usize, Error> {
        self.inner.decode_float(packet, out, false)
    }

    /// 封包遺失時：有下一包就用它的 FEC 復原（`fec_from = Some(next)`），否則做 PLC。
    pub fn conceal(&mut self, fec_from: Option<&[u8]>, out: &mut [f32]) -> Result<usize, Error> {
        let out = &mut out[..FRAME_SAMPLES];
        match fec_from {
            Some(next) => self.inner.decode_float(next, out, true),
            None => self.inner.decode_float(&[], out, false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 440 Hz 正弦波，振幅 0.5
    fn sine_frame(frame_idx: usize) -> Vec<f32> {
        (0..FRAME_SAMPLES)
            .map(|i| {
                let t = (frame_idx * FRAME_SAMPLES + i) as f32 / SAMPLE_RATE as f32;
                0.5 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()
            })
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn roundtrip_preserves_energy_and_packet_size_is_small() {
        let mut enc = Encoder::new(32_000).unwrap();
        let mut dec = Decoder::new().unwrap();
        let mut pkt = [0u8; MAX_PACKET];
        let mut out = vec![0f32; FRAME_SAMPLES];
        let mut sizes = Vec::new();

        for f in 0..50 {
            let pcm = sine_frame(f);
            let n = enc.encode(&pcm, &mut pkt).unwrap();
            sizes.push(n);
            let m = dec.decode(&pkt[..n], &mut out).unwrap();
            assert_eq!(m, FRAME_SAMPLES);
        }
        // 解碼器起始有延遲，用最後一幀比較能量（輸入 RMS ≈ 0.354）
        let r = rms(&out);
        assert!((0.25..0.45).contains(&r), "decoded rms = {r}");

        let avg = sizes.iter().sum::<usize>() as f32 / sizes.len() as f32;
        // 32 kbps × 20 ms = 80 bytes
        assert!(avg < 120.0, "avg packet = {avg} bytes");
    }

    #[test]
    fn plc_and_fec_produce_full_frames() {
        let mut enc = Encoder::new(32_000).unwrap();
        let mut dec = Decoder::new().unwrap();
        let mut pkt = [0u8; MAX_PACKET];
        let mut out = vec![0f32; FRAME_SAMPLES];

        let n = enc.encode(&sine_frame(0), &mut pkt).unwrap();
        dec.decode(&pkt[..n], &mut out).unwrap();

        assert_eq!(dec.conceal(None, &mut out).unwrap(), FRAME_SAMPLES);

        let n = enc.encode(&sine_frame(2), &mut pkt).unwrap();
        assert_eq!(dec.conceal(Some(&pkt[..n]), &mut out).unwrap(), FRAME_SAMPLES);
    }
}
