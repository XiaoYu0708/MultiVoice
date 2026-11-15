//! 錄 N 秒環境音，分析底噪的頻段分布（判斷是風扇低頻、人聲頻段，還是電子嘶聲）。
//! cargo run -p audio --example mic_probe --release -- [秒數，預設 3]

use std::sync::Arc;
use std::time::{Duration, Instant};

use audio::codec::SAMPLE_RATE;
use audio::device::{self, StreamStats};

/// RBJ 二階 Butterworth biquad
struct Biquad { b: [f32; 3], a: [f32; 2], z: [f32; 2] }
impl Biquad {
    fn new(kind: &str, f0: f32) -> Self {
        let w = 2.0 * std::f32::consts::PI * f0 / SAMPLE_RATE as f32;
        let (c, s) = (w.cos(), w.sin());
        let alpha = s / (2.0 * std::f32::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        let b = match kind {
            "lp" => [(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0],
            _ => [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0],
        };
        Self { b: b.map(|x| x / a0), a: [-2.0 * c / a0, (1.0 - alpha) / a0], z: [0.0; 2] }
    }
    fn run(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

fn db(x: f64) -> f64 { 10.0 * x.max(1e-12).log10() }

fn main() -> Result<(), device::Error> {
    let secs: u64 = std::env::args().nth(1).map(|s| s.parse().unwrap()).unwrap_or(3);
    let dev = device::find_device(true, None)?;
    let (prod, mut cons) = rtrb::RingBuffer::<f32>::new(SAMPLE_RATE as usize);
    let stats = Arc::new(StreamStats::default());
    let (_s, _) = device::open_input(&dev, prod, stats, None)?;
    println!("錄音 {secs} 秒：{}（請保持安靜）", device::device_name(&dev));

    // 頻段：< 250 Hz（風扇、冷氣、桌面震動）、250–4000 Hz（人聲主要頻段）、> 4000 Hz（嘶聲）
    let mut low = Biquad::new("lp", 250.0);
    let (mut mid_hp, mut mid_lp) = (Biquad::new("hp", 250.0), Biquad::new("lp", 4000.0));
    let mut high = Biquad::new("hp", 4000.0);
    let (mut e_all, mut e_low, mut e_mid, mut e_high, mut n, mut peak) = (0f64, 0f64, 0f64, 0f64, 0u64, 0f32);

    let t = Instant::now();
    let mut skip = SAMPLE_RATE as u64 / 5; // 丟掉前 200 ms（串流啟動）
    while t.elapsed() < Duration::from_secs(secs) + Duration::from_millis(200) {
        while let Ok(x) = cons.pop() {
            let (l, m, h) = (low.run(x), mid_lp.run(mid_hp.run(x)), high.run(x));
            if skip > 0 { skip -= 1; continue; }
            e_all += (x * x) as f64; e_low += (l * l) as f64; e_mid += (m * m) as f64; e_high += (h * h) as f64;
            peak = peak.max(x.abs());
            n += 1;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let nf = n.max(1) as f64;
    println!("整體：RMS {:6.1} dBFS｜峰值 {:6.1} dBFS", db(e_all / nf), 20.0 * (peak.max(1e-6) as f64).log10());
    for (name, e) in [("  < 250 Hz（風扇/冷氣/震動）", e_low), ("  250–4000 Hz（人聲頻段）", e_mid), ("  > 4000 Hz（嘶聲）", e_high)] {
        println!("{name:<28} RMS {:6.1} dBFS｜佔 {:5.1}%", db(e / nf), 100.0 * e / e_all.max(1e-12));
    }
    Ok(())
}
