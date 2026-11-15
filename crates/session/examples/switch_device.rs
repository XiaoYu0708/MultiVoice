//! 驗證進房後換裝置：連線不中斷、語音序號接續（對方不會把新封包當成遲到）。
//! cargo run -p session --example switch_device --release -- <另一支麥克風的名稱片段>

use std::time::Duration;

use session::{AudioOpts, Relays, Session};

fn main() -> anyhow::Result<()> {
    let other_mic = std::env::args().nth(1).unwrap_or_else(|| "Barracuda".into());
    let rt = tokio::runtime::Runtime::new()?;
    let quiet = AudioOpts { open_mic: true, volume: 0.0, ..Default::default() };

    let a = rt.block_on(session::host_room("switch-a", "A", Relays::Default))?;
    let mut a = Session::start(a, &quiet, rt.handle())?;
    let ticket = a.snapshot().ticket;
    let b = rt.block_on(session::join_room(&ticket, "switch-b", "B", Relays::Default))?;
    let b = Session::start(b, &quiet, rt.handle())?;

    let check = |label: &str, a: &Session| {
        std::thread::sleep(Duration::from_secs(3));
        let s = b.snapshot();
        let heard = s.members.iter().any(|m| m.name == "A" && m.speaking);
        println!(
            "{label:<24} A 麥克風：{:<45} B 聽到 A：{}｜遲到 {}｜跳過 {}｜PLC {}",
            a.input_name, if heard { "是" } else { "否" }, s.jitter.late, s.jitter.skipped, s.jitter.plc
        );
        heard
    };

    let mut ok = check("切換前", &a);
    let mut o = quiet.clone();
    o.input = Some(other_mic.clone());
    a.apply_audio(&o)?;
    ok &= check(&format!("切到「{other_mic}」"), &a);
    a.apply_audio(&quiet)?;
    ok &= check("切回預設", &a);
    let late = b.snapshot().jitter.late;
    println!("\n結果：{}", if ok && late == 0 { "通過" } else { "失敗" });

    rt.block_on(async {
        b.leave().await;
        a.leave().await;
    });
    Ok(())
}
