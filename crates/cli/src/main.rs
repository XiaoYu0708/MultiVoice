//! MultiVoice 命令列客戶端（除錯、自動測試用；一般使用請用桌面版）。
//!
//!   multivoice-cli host [選項]              建立房間（自己當房主），印出邀請票
//!   multivoice-cli join <邀請票> [選項]     加入房間
//!
//! 選項：
//!   --name <名稱>        顯示名稱（預設：Windows 使用者名稱）
//!   --profile <名稱>     身分設定檔（同一台電腦開兩個實例測試時用不同名稱，預設 default）
//!   --relay <URL|off>    只用指定 relay；off = 不用 relay（只走直連）。預設：自架 relay + n0 備援
//!   --in / --out <名稱片段>  音訊裝置
//!   --open-mic           不用 VAD，一直送出
//!   --no-denoise         關閉降噪
//!   --mix <0..1>         降噪時人聲混回原音比例（預設 0.3）
//!   --bitrate <bps>      Opus 位元率（預設 32000）
//!   --buffer <ms>        輸出緩衝目標（預設 30）
//!   --volume <倍數>      輸出音量（預設 1.0；0 = 靜音，用於自動測試）
//!   --seconds <秒>       時間到自動離開（自動測試用）
//!   --channel <名稱>     建立這個頻道並在裡面一直開麥，大廳改為只聽（可重複；自動測試用）
//!
//! 執行中：輸入 m + Enter 切換靜音，q + Enter 離開。

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use session::{AudioOpts, ChannelCfg, ChannelMix, Relays, Session, Snapshot, TalkMode};

struct Opts {
    mode: String,
    ticket: Option<String>,
    name: String,
    profile: String,
    relays: Relays,
    audio: AudioOpts,
    seconds: Option<u64>,
    channels: Vec<String>,
}

fn parse_args() -> Result<Opts> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().context("用法：multivoice-cli host | join <邀請票>（詳見 --help）")?;
    if mode == "-h" || mode == "--help" {
        println!("見 crates/cli/src/main.rs 開頭的說明");
        std::process::exit(0);
    }
    let mut o = Opts {
        mode,
        ticket: None,
        name: std::env::var("USERNAME").unwrap_or_else(|_| "玩家".into()),
        profile: "default".into(),
        relays: Relays::Default,
        audio: AudioOpts::default(),
        seconds: None,
        channels: Vec::new(),
    };
    while let Some(a) = args.next() {
        let mut val = || args.next().with_context(|| format!("{a} 需要一個值"));
        match a.as_str() {
            "--name" => o.name = val()?,
            "--profile" => o.profile = val()?,
            "--relay" => {
                let v = val()?;
                o.relays = if v == "off" { Relays::Disabled } else { Relays::Custom(vec![v.parse()?]) };
            }
            "--in" => o.audio.input = Some(val()?),
            "--out" => o.audio.output = Some(val()?),
            "--open-mic" => o.audio.open_mic = true,
            "--no-denoise" => o.audio.denoise = false,
            "--mix" => o.audio.mix = val()?.parse()?,
            "--bitrate" => o.audio.bitrate = val()?.parse()?,
            "--buffer" => o.audio.buffer_ms = val()?.parse()?,
            "--volume" => o.audio.volume = val()?.parse()?,
            "--seconds" => o.seconds = Some(val()?.parse()?),
            "--channel" => o.channels.push(val()?),
            _ if o.ticket.is_none() && !a.starts_with("--") => o.ticket = Some(a),
            other => bail!("未知選項 {other}"),
        }
    }
    Ok(o)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        // net_report 的「QAD 位址因目的地而異」在家中經 NAT loopback 連自架 relay 時必然出現，不顯示
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,iroh::net_report=error".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let o = parse_args()?;

    let t = Instant::now();
    let connected = match o.mode.as_str() {
        "host" => session::host_room(&o.profile, &o.name, o.relays.clone()).await?,
        "join" => {
            println!("加入中…");
            session::join_room(o.ticket.as_deref().context("請提供邀請票")?, &o.profile, &o.name, o.relays.clone()).await?
        }
        other => bail!("未知模式 {other}（host 或 join）"),
    };
    let s = Session::start(connected, &o.audio, &tokio::runtime::Handle::current())?;
    let snap = s.snapshot();
    if snap.is_host {
        println!("\n======== 邀請票（傳給要加入的人）========\n{}\n", snap.ticket);
    } else {
        println!("已加入（{} ms）", t.elapsed().as_millis());
    }
    println!("你是 #{}「{}」。輸入：{}｜輸出：{}", snap.me.idx, snap.me.name, s.input_name, s.output_name);
    println!("輸入 m + Enter 切換靜音，q + Enter 離開。\n");

    if !o.channels.is_empty() {
        setup_channels(&s, &o.channels).await?;
    }
    run_ui(&s, o.seconds).await;
    s.leave().await;
    Ok(())
}

async fn run_ui(s: &Session, seconds: Option<u64>) {
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        let mut line = String::new();
        while std::io::stdin().read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
            if cmd_tx.send(line.trim().to_lowercase()).is_err() {
                break;
            }
            line.clear();
        }
    });

    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut stdin_open = true;
    let mut log_seen = 0;
    let deadline = seconds.map(|s| Instant::now() + Duration::from_secs(s));
    loop {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        tokio::select! {
            cmd = cmd_rx.recv(), if stdin_open => match cmd.as_deref() {
                Some("m") => {
                    let now = !s.snapshot().muted;
                    s.set_muted(now);
                    println!("{}", if now { "🔇 已靜音" } else { "🎤 已取消靜音" });
                }
                Some("q") => break,
                // 標準輸入關閉（背景執行）：不再讀指令，但繼續執行
                None => stdin_open = false,
                Some(_) => println!("指令：m = 靜音切換，q = 離開"),
            },
            _ = tick.tick() => {
                let snap = s.snapshot();
                for line in snap.log.iter().skip(log_seen) {
                    println!("· {line}");
                }
                log_seen = snap.log.len();
                if snap.disconnected.is_some() {
                    break;
                }
                print_status(&snap);
            }
        }
    }
}

fn print_status(s: &Snapshot) {
    let speaking: Vec<&str> = s.members.iter().filter(|m| m.speaking).map(|m| m.name.as_str()).collect();
    let path = match s.path {
        Some((true, r)) => format!("relay {r:.0}ms"),
        Some((false, r)) => format!("直連 {r:.0}ms"),
        None => "?".into(),
    };
    let me = if s.muted { "🔇" } else if s.tx_active { "🎤送出中" } else { "🎤" };
    let j = &s.jitter;
    println!(
        "{me}｜成員 {}｜說話中：{}｜{path}｜ping {}｜緩衝 {}｜FEC {} PLC {} 跳過 {} 遲到 {}｜欠載 {}",
        s.members.len() + 1,
        if speaking.is_empty() { "—".into() } else { speaking.join("、") },
        s.ping.map(|p| format!("{p:.0}ms")).unwrap_or_else(|| "—".into()),
        j.target,
        j.fec,
        j.plc,
        j.skipped,
        j.late,
        s.underruns
    );
}

/// 自動測試用：建立頻道並在裡面一直開麥，大廳改為只聽
async fn setup_channels(s: &Session, names: &[String]) -> Result<()> {
    let open = ChannelCfg { listen: true, talk: TalkMode::Open, mix: ChannelMix::default(), ptt_key: None };
    s.set_channel(0, Some(ChannelCfg { talk: TalkMode::Off, ..open }))?;
    for name in names {
        s.create_channel(name);
        let id = 'wait: {
            for _ in 0..100 {
                if let Some(c) = s.snapshot().channels.iter().find(|c| &c.name == name) {
                    break 'wait c.id;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            bail!("頻道「{name}」沒有建立");
        };
        s.set_channel(id, Some(open))?;
        println!("已建立頻道「{name}」並一直開麥");
    }
    Ok(())
}
