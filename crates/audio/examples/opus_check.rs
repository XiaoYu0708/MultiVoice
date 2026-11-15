//! M0：確認 libopus 已正確連結，印出版本與編碼統計。
//! cargo run -p audio --example opus_check

use audio::codec::*;

fn main() {
    println!("libopus: {}", opus::version());
    let mut enc = Encoder::new(32_000).expect("encoder");
    let mut dec = Decoder::new().expect("decoder");
    let mut pkt = [0u8; MAX_PACKET];
    let mut out = vec![0f32; FRAME_SAMPLES];
    let frames = 250; // 5 秒
    let mut total = 0usize;
    let t0 = std::time::Instant::now();
    for f in 0..frames {
        let pcm: Vec<f32> = (0..FRAME_SAMPLES)
            .map(|i| {
                let t = (f * FRAME_SAMPLES + i) as f32 / SAMPLE_RATE as f32;
                0.5 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()
            })
            .collect();
        let n = enc.encode(&pcm, &mut pkt).expect("encode");
        dec.decode(&pkt[..n], &mut out).expect("decode");
        total += n;
    }
    let el = t0.elapsed();
    println!(
        "{frames} 幀（5 秒音訊）：平均封包 {:.1} bytes，編+解碼共 {:.2} ms（每幀 {:.3} ms）",
        total as f32 / frames as f32,
        el.as_secs_f64() * 1000.0,
        el.as_secs_f64() * 1000.0 / frames as f64
    );
}
