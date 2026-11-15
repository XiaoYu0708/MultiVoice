//! MultiVoice 命令列客戶端（M1：單頻道房主模式）。
//!
//!   multivoice host [選項]              建立房間（自己當房主），印出邀請票
//!   multivoice join <邀請票> [選項]     加入房間
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
//!
//! 執行中：輸入 m + Enter 切換靜音，q + Enter 離開。

mod identity;
mod pipeline;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use client_core::{Client, ClientEvent};
use host_core::{Host, HostConfig};
use protocol::ticket::{RoomId, RoomSecret, Ticket};
use transport::RelaySetup;

use crate::pipeline::{AudioOpts, Shared};

struct Opts {
    mode: String,
    ticket: Option<String>,
    name: String,
    profile: String,
    relays: RelaySetup,
    audio: AudioOpts,
    seconds: Option<u64>,
}

fn parse_args() -> Result<Opts> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().context("用法：multivoice host | join <邀請票>（詳見 --help）")?;
    if mode == "-h" || mode == "--help" {
        println!("見 crates/cli/src/main.rs 開頭的說明");
        std::process::exit(0);
    }
    let mut o = Opts {
        mode,
        ticket: None,
        name: std::env::var("USERNAME").unwrap_or_else(|_| "玩家".into()),
        profile: "default".into(),
        relays: RelaySetup::Default,
        audio: AudioOpts::default(),
        seconds: None,
    };
    while let Some(a) = args.next() {
        let mut val = || args.next().with_context(|| format!("{a} 需要一個值"));
        match a.as_str() {
            "--name" => o.name = val()?,
            "--profile" => o.profile = val()?,
            "--relay" => {
                let v = val()?;
                o.relays = if v == "off" { RelaySetup::Disabled } else { RelaySetup::Custom(vec![v.parse()?]) };
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
    let key = identity::load_or_create(&o.profile)?;

    // 端點必須活到程式結束：被 drop 時 iroh 會直接中止底層 socket，連線就死了。
    // `endpoints` 收集所有端點，離開時逐一正常關閉。
    let mut endpoints = Vec::new();
    let (client, events, _host) = match o.mode.as_str() {
        "host" => {
            // 房主端點用一次性的金鑰；自己的成員身分用 profile 的永久金鑰
            let host_ep = transport::bind(transport::generate_secret(), o.relays.clone(), true).await?;
            let ticket = Ticket {
                room_id: RoomId(transport::random_bytes()),
                secret: RoomSecret(transport::random_bytes()),
                bootstrap: vec![transport::node_id(&host_ep.id())],
            };
            let host = Host::new(HostConfig { room_id: ticket.room_id, secret: ticket.secret, term: 1, max_members: 100 });
            tokio::spawn({
                let (host, ep) = (host.clone(), host_ep.clone());
                async move { host.run(ep).await }
            });
            if !matches!(o.relays, RelaySetup::Disabled) {
                let t = Instant::now();
                if tokio::time::timeout(Duration::from_secs(10), host_ep.online()).await.is_err() {
                    eprintln!("警告：10 秒內沒有連上任何 relay，其他人可能無法連進來");
                } else {
                    println!("房主上線（連上 relay）：{} ms", t.elapsed().as_millis());
                }
            }
            println!("\n======== 邀請票（傳給要加入的人）========\n{ticket}\n");
            println!("加入指令：multivoice join {ticket} --name <名稱>\n");

            let ep = transport::bind(key, o.relays.clone(), false).await?;
            let (c, rx) = Client::join_addr(&ep, transport::loopback_addr(&host_ep), &ticket, &o.name).await?;
            endpoints.push(ep);
            endpoints.push(host_ep);
            (c, rx, Some(host))
        }
        "join" => {
            let ticket: Ticket = o.ticket.as_deref().context("請提供邀請票")?.parse()?;
            let ep = transport::bind(key, o.relays.clone(), false).await?;
            println!("加入中…");
            let t = Instant::now();
            let (c, rx) = Client::join(&ep, &ticket, &o.name).await?;
            println!("已加入（{} ms）", t.elapsed().as_millis());
            endpoints.push(ep);
            (c, rx, None)
        }
        other => bail!("未知模式 {other}（host 或 join）"),
    };

    println!("你是 #{}「{}」。成員：{}", client.session_idx(), o.name, member_list(&client));
    println!("輸入 m + Enter 切換靜音，q + Enter 離開。\n");

    let shared = Arc::new(Shared::new());
    let audio = pipeline::start(client.clone(), shared.clone(), &o.audio)?;
    println!("輸入：{}｜輸出：{}", audio.input_name, audio.output_name);

    run_ui(&client, events, &shared, o.seconds).await;

    shared.stop.store(true, Ordering::Relaxed);
    client.leave().await;
    drop(audio);
    for ep in endpoints {
        ep.close().await;
    }
    Ok(())
}

fn member_list(c: &Client) -> String {
    let m = c.members();
    if m.is_empty() {
        "（目前只有你）".into()
    } else {
        m.iter().map(|m| format!("#{} {}", m.session_idx, m.display_name)).collect::<Vec<_>>().join("、")
    }
}

async fn run_ui(
    client: &Client,
    mut events: tokio::sync::mpsc::UnboundedReceiver<ClientEvent>,
    shared: &Shared,
    seconds: Option<u64>,
) {
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
    let mut rtt = f64::NAN;
    let mut stdin_open = true;
    let deadline = seconds.map(|s| Instant::now() + Duration::from_secs(s));
    loop {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        tokio::select! {
            ev = events.recv() => match ev {
                Some(ClientEvent::MemberJoined(m)) => println!("→ #{} {} 加入了", m.session_idx, m.display_name),
                Some(ClientEvent::MemberLeft(i)) => {
                    println!("← #{i} 離開了");
                    shared.forget(i);
                }
                Some(ClientEvent::Rtt(r)) => rtt = r,
                Some(ClientEvent::Disconnected(why)) => { println!("連線中斷：{why}"); break; }
                None => break,
            },
            cmd = cmd_rx.recv(), if stdin_open => match cmd.as_deref() {
                Some("m") => {
                    let now = !shared.muted.load(Ordering::Relaxed);
                    shared.muted.store(now, Ordering::Relaxed);
                    println!("{}", if now { "🔇 已靜音" } else { "🎤 已取消靜音" });
                }
                Some("q") => break,
                // 標準輸入關閉（背景執行）：不再讀指令，但繼續執行
                None => stdin_open = false,
                Some(_) => println!("指令：m = 靜音切換，q = 離開"),
            },
            _ = tick.tick() => print_status(client, shared, rtt),
        }
    }
}

fn print_status(client: &Client, shared: &Shared, rtt: f64) {
    let speaking: Vec<String> = shared
        .speaking(Duration::from_millis(300))
        .into_iter()
        .map(|i| client.member_name(i).unwrap_or_else(|| format!("#{i}")))
        .collect();
    let path = match client.path() {
        Some((true, r)) => format!("relay {r:.0}ms"),
        Some((false, r)) => format!("直連 {r:.0}ms"),
        None => "?".into(),
    };
    let me = if shared.muted.load(Ordering::Relaxed) {
        "🔇"
    } else if shared.tx_active.load(Ordering::Relaxed) {
        "🎤送出中"
    } else {
        "🎤"
    };
    let j = shared.jitter_summary();
    println!(
        "{me}｜說話中：{}｜{path}｜ping {:.0}ms｜緩衝 {}｜FEC {} PLC {} 跳過 {} 遲到 {}｜欠載 {}",
        if speaking.is_empty() { "—".into() } else { speaking.join("、") },
        rtt,
        j.target,
        j.fec,
        j.plc,
        j.skipped,
        j.late,
        shared.underruns()
    );
}
