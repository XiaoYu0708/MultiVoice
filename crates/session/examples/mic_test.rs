//! 麥克風測試（不連網路）：印出幾秒的音量、是否判定為說話、環境噪音、自動音量
//!
//! cargo run -p session --example mic_test --release -- [秒數] [nodenoise]
//! （MIC=名稱片段 指定麥克風；STRONG=1 用強力降噪）

use std::time::Duration;

use session::FrontendCfg;
use session::mictest::MicTest;

fn main() -> anyhow::Result<()> {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(4);
    // 第二個參數 nodenoise：關掉降噪（排查用）
    let denoise = std::env::args().nth(2).as_deref() != Some("nodenoise");
    // 環境變數 MIC=名稱片段：指定麥克風
    let t = MicTest::start(std::env::var("MIC").ok(), None, FrontendCfg { denoise, strong: std::env::var("STRONG").is_ok(), ..FrontendCfg::default() }, false)?;
    for _ in 0..secs * 4 {
        std::thread::sleep(Duration::from_millis(250));
        let r = t.reading();
        println!(
            "原始峰值 {:.4}｜音量 {:>6.1} dB｜環境噪音 {:>6.1} dB｜自動音量 {:+5.1} dB｜{}",
            r.peak,
            r.level_db,
            r.noise_db,
            r.gain_db,
            if r.voiced { "說話" } else { "安靜" }
        );
    }
    Ok(())
}
