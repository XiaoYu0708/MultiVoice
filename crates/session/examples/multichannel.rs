//! 驗證 session 層的多頻道：每頻道說話模式、via 對應的「在哪個頻道說話」、目前送出的頻道。
//! cargo run -p session --example multichannel --release

use std::time::Duration;

use session::{AudioOpts, ChannelCfg, ChannelMix, Relays, Session, TalkMode};

fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let quiet = AudioOpts { open_mic: true, volume: 0.0, ..Default::default() };

    let a = rt.block_on(session::host_room("mc-a", "A", Relays::Default))?;
    let a = Session::start(a, &quiet, rt.handle())?;
    let b = rt.block_on(session::join_room(&a.snapshot().ticket, "mc-b", "B", Relays::Default))?;
    let b = Session::start(b, &quiet, rt.handle())?;

    a.create_channel("指揮部");
    let wait = |s: &Session, f: &dyn Fn(&session::Snapshot) -> bool, what: &str| {
        for _ in 0..100 {
            if f(&s.snapshot()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("等不到：{what}");
    };
    wait(&b, &|s| s.channels.iter().any(|c| c.name == "指揮部"), "B 看到指揮部");
    let cmd = b.snapshot().channels.iter().find(|c| c.name == "指揮部").unwrap().id;

    let listen_only = ChannelCfg { listen: true, talk: TalkMode::Off, mix: ChannelMix::default(), ptt_key: None };
    let open = ChannelCfg { listen: true, talk: TalkMode::Open, mix: ChannelMix::default(), ptt_key: None };
    a.set_channel(0, Some(listen_only))?; // A 在大廳只聽不說
    a.set_channel(cmd, Some(open))?; // A 在指揮部常開
    b.set_channel(0, None)?; // B 離開大廳
    b.set_channel(cmd, Some(listen_only))?; // B 只聽指揮部

    std::thread::sleep(Duration::from_secs(3));
    let (sa, sb) = (a.snapshot(), b.snapshot());
    let in_ch = |s: &session::Snapshot, ch: u32, who: &str| {
        s.channels.iter().find(|c| c.id == ch).and_then(|c| c.members.iter().find(|m| m.name == who)).cloned()
    };
    let a_in_cmd = in_ch(&sb, cmd, "A").expect("B 看得到 A 在指揮部");
    let a_in_lobby = in_ch(&sb, 0, "A").expect("B 看得到 A 在大廳");
    println!("A 目前送出：{:?}", sa.talking_to);
    println!("B 看到 A 在指揮部：listen={} talk={} 說話中={}", a_in_cmd.listen, a_in_cmd.talk, a_in_cmd.speaking);
    println!("B 看到 A 在大廳：  listen={} talk={} 說話中={}", a_in_lobby.listen, a_in_lobby.talk, a_in_lobby.speaking);
    println!("B 是否還在大廳：{}", in_ch(&sb, 0, "B").is_some());
    println!("B 的 jitter：{:?}", sb.jitter);
    let ok = sa.talking_to == ["指揮部"]
        && a_in_cmd.speaking
        && a_in_cmd.talk
        && !a_in_lobby.speaking
        && !a_in_lobby.talk
        && in_ch(&sb, 0, "B").is_none();
    println!("\n結果：{}", if ok { "通過" } else { "失敗" });

    rt.block_on(async {
        b.leave().await;
        a.leave().await;
    });
    Ok(())
}
