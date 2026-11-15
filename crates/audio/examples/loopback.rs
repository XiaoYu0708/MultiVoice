//! M0 第 2 步：本機語音迴路。
//!
//!   麥克風 → mono 48 kHz → Opus 編碼 →（模擬掉包）→ Opus 解碼（FEC / PLC）→ 喇叭
//!
//! **請戴耳機**，否則喇叭聲會被麥克風收進去形成回授。
//! 對著麥克風「拍手」或「彈一下」，程式會實測端到端延遲（麥克風取樣 → 喇叭播放）。
//!
//! cargo run -p audio --example loopback --release -- [選項]
//!   --in <名稱片段>      輸入裝置（預設：系統預設）
//!   --out <名稱片段>     輸出裝置（預設：系統預設）
//!   --bitrate <bps>      Opus 位元率（預設 32000）
//!   --loss <百分比>      模擬隨機掉包（預設 0）
//!   --buffer <ms>        輸出緩衝目標（預設 20）
//!   --gain <倍數>        輸出增益（預設 1.0；0 = 靜音，仍會量測）
//!   --seconds <秒>       執行秒數（預設：按 Enter 結束）

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use audio::codec::{Decoder, Encoder, FRAME_SAMPLES, MAX_PACKET, SAMPLE_RATE};
use audio::device::{self, OnsetDetector, StreamStats};

struct Opts {
    input: Option<String>,
    output: Option<String>,
    bitrate: i32,
    loss_pct: f32,
    buffer_ms: u32,
    gain: f32,
    seconds: Option<u64>,
}

fn parse_args() -> Opts {
    let mut o = Opts {
        input: None,
        output: None,
        bitrate: 32_000,
        loss_pct: 0.0,
        buffer_ms: 20,
        gain: 1.0,
        seconds: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| panic!("{a} 需要一個值"));
        match a.as_str() {
            "--in" => o.input = Some(val()),
            "--out" => o.output = Some(val()),
            "--bitrate" => o.bitrate = val().parse().expect("--bitrate"),
            "--loss" => o.loss_pct = val().parse().expect("--loss"),
            "--buffer" => o.buffer_ms = val().parse().expect("--buffer"),
            "--gain" => o.gain = val().parse().expect("--gain"),
            "--seconds" => o.seconds = Some(val().parse().expect("--seconds")),
            "-h" | "--help" => {
                println!("見 crates/audio/examples/loopback.rs 開頭的說明");
                std::process::exit(0);
            }
            other => panic!("未知選項 {other}"),
        }
    }
    o
}

/// 不引入 rand：xorshift64，只用來模擬掉包
struct Rng(u64);
impl Rng {
    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

#[derive(Default)]
struct PipeStats {
    frames: u64,
    lost: u64,
    fec: u64,
    plc: u64,
    bytes: u64,
    drift_drops: u64,
    codec_us_max: u64,
}

fn dbfs(x: f32) -> String {
    if x <= 1e-6 { "  -∞".into() } else { format!("{:4.0}", 20.0 * x.log10()) }
}

fn main() -> Result<(), device::Error> {
    let o = parse_args();
    let in_dev = device::find_device(true, o.input.as_deref())?;
    let out_dev = device::find_device(false, o.output.as_deref())?;

    // 環形緩衝：輸入 200 ms、輸出 200 ms 容量
    let cap = SAMPLE_RATE as usize / 5;
    let (in_prod, mut in_cons) = rtrb::RingBuffer::<f32>::new(cap);
    let (mut out_prod, out_cons) = rtrb::RingBuffer::<f32>::new(cap);
    let (in_onset_tx, mut in_onset_rx) = rtrb::RingBuffer::<u64>::new(64);
    let (out_onset_tx, mut out_onset_rx) = rtrb::RingBuffer::<u64>::new(64);

    let in_stats = Arc::new(StreamStats::default());
    let out_stats = Arc::new(StreamStats::default());

    let target = (SAMPLE_RATE * o.buffer_ms / 1000) as usize;
    // 先預填靜音到目標水位，避免一開始就欠載
    for _ in 0..target {
        let _ = out_prod.push(0.0);
    }

    let (_in_stream, in_ch) = device::open_input(
        &in_dev, in_prod, in_stats.clone(), Some(OnsetDetector::new(in_onset_tx)))?;
    let (_out_stream, out_ch) = device::open_output(
        &out_dev, out_cons, out_stats.clone(), Some(OnsetDetector::new(out_onset_tx)), o.gain)?;

    println!("libopus {}", opus::version());
    println!("輸入：{}（{in_ch} ch）", device::device_name(&in_dev));
    println!("輸出：{}（{out_ch} ch）", device::device_name(&out_dev));
    println!(
        "位元率 {} kbps｜模擬掉包 {}%｜輸出緩衝目標 {} ms｜增益 {}",
        o.bitrate / 1000, o.loss_pct, o.buffer_ms, o.gain
    );
    println!("請戴耳機。對麥克風拍手或彈一下（間隔 0.5 秒以上）即可量測端到端延遲。");
    match o.seconds {
        Some(s) => println!("將執行 {s} 秒。\n"),
        None => println!("按 Enter 結束。\n"),
    }

    let stop = Arc::new(AtomicBool::new(false));
    if o.seconds.is_none() {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::stdin().read_line(&mut s);
            stop.store(true, Ordering::Relaxed);
        });
    }

    let mut enc = Encoder::new(o.bitrate)?;
    let mut dec = Decoder::new()?;
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut frame = Vec::with_capacity(FRAME_SAMPLES);
    let mut pcm_out = vec![0f32; FRAME_SAMPLES];
    // 有模擬掉包時延後一幀解碼，才能用下一包的 FEC 復原遺失的那包
    let mut pending: Option<Option<Vec<u8>>> = None;
    let mut ps = PipeStats::default();
    // 串流啟動時輸出端會比第一幀解碼資料早開始，那段欠載不算
    let mut warmed_up = false;

    let mut in_onsets: std::collections::VecDeque<u64> = Default::default();
    let mut latencies: Vec<f64> = Vec::new();

    let started = Instant::now();
    let mut last_report = Instant::now();
    let fill_of = |p: &rtrb::Producer<f32>| cap - p.slots();

    while !stop.load(Ordering::Relaxed)
        && o.seconds.is_none_or(|s| started.elapsed() < Duration::from_secs(s))
    {
        // 1. 取麥克風資料，湊滿一幀就處理
        while let Ok(s) = in_cons.pop() {
            frame.push(s);
            if frame.len() < FRAME_SAMPLES {
                continue;
            }
            let t = Instant::now();
            let mut pkt = [0u8; MAX_PACKET];
            let n = enc.encode(&frame, &mut pkt)?;
            frame.clear();
            ps.frames += 1;
            ps.bytes += n as u64;

            let lost = o.loss_pct > 0.0 && rng.next_f32() * 100.0 < o.loss_pct;
            if lost {
                ps.lost += 1;
            }
            let this = (!lost).then(|| pkt[..n].to_vec());

            // 2. 解碼：無掉包模擬時直接解；有掉包模擬時解「上一幀」
            let decoded = if o.loss_pct <= 0.0 {
                dec.decode(this.as_deref().unwrap(), &mut pcm_out)?;
                true
            } else {
                let prev = pending.replace(this.clone());
                match prev {
                    None => false, // 第一幀，還沒有上一幀
                    Some(Some(p)) => {
                        dec.decode(&p, &mut pcm_out)?;
                        true
                    }
                    Some(None) => {
                        match this.as_deref() {
                            Some(_) => ps.fec += 1,
                            None => ps.plc += 1,
                        }
                        dec.conceal(this.as_deref(), &mut pcm_out)?;
                        true
                    }
                }
            };
            ps.codec_us_max = ps.codec_us_max.max(t.elapsed().as_micros() as u64);

            // 3. 推到輸出緩衝；兩個裝置時脈不同會慢慢漂移，水位太高就丟掉多出來的部分
            if decoded {
                let fill = fill_of(&out_prod);
                let high = target + FRAME_SAMPLES * 2;
                let skip = if fill > high { (fill - target).min(FRAME_SAMPLES) } else { 0 };
                if skip > 0 {
                    ps.drift_drops += 1;
                }
                for &s in &pcm_out[skip..] {
                    let _ = out_prod.push(s);
                }
                if !warmed_up {
                    warmed_up = true;
                    out_stats.xruns.store(0, Ordering::Relaxed);
                    in_stats.xruns.store(0, Ordering::Relaxed);
                }
            }
        }

        // 4. 配對延遲量測：每個輸出起點，對應到它之前 1 秒內最早的輸入起點
        while let Ok(t) = in_onset_rx.pop() {
            in_onsets.push_back(t);
        }
        while let Ok(t_out) = out_onset_rx.pop() {
            while in_onsets.front().is_some_and(|&t_in| t_out.saturating_sub(t_in) > 1_000_000_000) {
                in_onsets.pop_front();
            }
            if let Some(&t_in) = in_onsets.front() {
                if t_in < t_out {
                    in_onsets.pop_front();
                    let ms = (t_out - t_in) as f64 / 1e6;
                    latencies.push(ms);
                    let avg = latencies.iter().sum::<f64>() / latencies.len() as f64;
                    println!("  ▶ 端到端延遲 {ms:6.1} ms（平均 {avg:5.1} ms，n={}）", latencies.len());
                }
            }
        }

        // 5. 每秒狀態列
        if last_report.elapsed() >= Duration::from_secs(1) {
            last_report = Instant::now();
            let kbps = ps.bytes as f64 * 8.0 / (ps.frames.max(1) as f64 * 0.02) / 1000.0;
            println!(
                "麥克風 {} dBFS｜輸入裝置 {:4.1} ms｜輸出緩衝 {:4.1} ms｜輸出裝置 {:4.1} ms｜欠載 {}｜溢位 {}｜漂移修正 {}｜{:4.1} kbps｜掉包 {}（FEC {}，PLC {}）",
                dbfs(in_stats.take_peak()),
                in_stats.device_latency_us.load(Ordering::Relaxed) as f64 / 1000.0,
                fill_of(&out_prod) as f64 * 1000.0 / SAMPLE_RATE as f64,
                out_stats.device_latency_us.load(Ordering::Relaxed) as f64 / 1000.0,
                out_stats.xruns.load(Ordering::Relaxed),
                in_stats.xruns.load(Ordering::Relaxed),
                ps.drift_drops,
                kbps,
                ps.lost, ps.fec, ps.plc,
            );
            let _ = out_stats.take_peak();
        }

        std::thread::sleep(Duration::from_millis(1));
    }

    println!("\n=== 結果 ===");
    println!(
        "幀數 {}｜編+解碼單幀最長 {} µs｜欠載 {}｜漂移修正 {}",
        ps.frames,
        ps.codec_us_max,
        out_stats.xruns.load(Ordering::Relaxed),
        ps.drift_drops
    );
    if latencies.is_empty() {
        println!("沒有量到延遲（沒有偵測到拍手等明顯的聲音起點）。");
    } else {
        let mut v = latencies.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let avg = v.iter().sum::<f64>() / v.len() as f64;
        println!(
            "端到端延遲：平均 {avg:.1} ms｜最小 {:.1}｜中位數 {:.1}｜最大 {:.1}｜n={}",
            v[0], v[v.len() / 2], v[v.len() - 1], v.len()
        );
    }
    Ok(())
}
