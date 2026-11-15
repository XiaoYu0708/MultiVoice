//! M0 第 3 步：iroh datagram 網路探測。
//!
//! 模擬語音流量（每 20 ms 一個 100 bytes 的 datagram），對方原封不動送回，
//! 統計來回時間（RTT）、抖動、掉包率，以及目前走直連還是 relay。
//!
//!   netprobe listen  [--relay-only]
//!   netprobe connect <EndpointId> [--relay-only] [--seconds N]
//!   netprobe local   [--relay-only] [--seconds N]     同一行程內兩端互測
//!
//! `--relay-only`：關閉直連（IP transport），強制所有封包經 relay。
//! `--relay <URL>`：只用指定的 relay（例如自架的 https://relay.xiaoyu0708.ip-ddns.com/），不用 n0 公共 relay。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, RelayUrl};

const ALPN: &[u8] = b"multivoice/netprobe/0";
const PACKET_SIZE: usize = 100; // ≈ 32 kbps Opus 20 ms 封包 + 我們的標頭
const INTERVAL: Duration = Duration::from_millis(20);

struct Opts {
    mode: String,
    peer: Option<String>,
    relay_only: bool,
    relay: Option<RelayUrl>,
    seconds: u64,
}

fn parse_args() -> Result<Opts> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().context("用法：netprobe listen|connect <id>|local [--relay-only] [--seconds N]")?;
    let mut o = Opts { mode, peer: None, relay_only: false, relay: None, seconds: 30 };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--relay-only" => o.relay_only = true,
            "--relay" => o.relay = Some(args.next().context("--relay 需要 URL")?.parse()?),
            "--seconds" => o.seconds = args.next().context("--seconds 需要值")?.parse()?,
            _ if o.peer.is_none() && !a.starts_with("--") => o.peer = Some(a),
            other => bail!("未知選項 {other}"),
        }
    }
    Ok(o)
}

async fn bind(o: &Opts, accept: bool) -> Result<Endpoint> {
    let mut b = Endpoint::builder(presets::N0);
    if let Some(url) = &o.relay {
        b = b.relay_mode(RelayMode::Custom(RelayMap::from(url.clone())));
    }
    if accept {
        b = b.alpns(vec![ALPN.to_vec()]);
    }
    if o.relay_only {
        b = b.clear_ip_transports();
    }
    let ep = b.bind().await?;
    let t = Instant::now();
    ep.online().await;
    println!("端點上線（連上 relay）耗時 {} ms", t.elapsed().as_millis());
    Ok(ep)
}

fn print_addr(ep: &Endpoint) {
    let addr = ep.addr();
    println!("EndpointId：{}", addr.id);
    for ip in addr.ip_addrs() {
        println!("  直連位址：{ip}");
    }
    for r in addr.relay_urls() {
        println!("  Relay：{r}");
    }
}

/// 回聲端：把收到的 datagram 原封不動送回
async fn serve(ep: Endpoint) -> Result<()> {
    while let Some(incoming) = ep.accept().await {
        tokio::spawn(async move {
            let conn = match incoming.await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("連線失敗：{e}");
                    return;
                }
            };
            println!("對方已連線：{}", conn.remote_id());
            while let Ok(d) = conn.read_datagram().await {
                let _ = conn.send_datagram(d);
            }
            println!("對方已離線：{}", conn.remote_id());
        });
    }
    Ok(())
}

fn describe_paths(conn: &Connection) -> String {
    let paths = conn.paths();
    let mut parts = Vec::new();
    for p in paths.iter() {
        let kind = if p.is_relay() { "relay" } else if p.is_ip() { "直連" } else { "其他" };
        parts.push(format!(
            "{}{} {:?} rtt {:.1}ms",
            if p.is_selected() { "★" } else { "" },
            kind,
            p.remote_addr(),
            p.rtt().as_secs_f64() * 1000.0
        ));
    }
    if parts.is_empty() { "（沒有路徑）".into() } else { parts.join("｜") }
}

fn selected_kind(conn: &Connection) -> &'static str {
    conn.paths()
        .iter()
        .find(|p| p.is_selected())
        .map(|p| if p.is_relay() { "relay" } else { "直連" })
        .unwrap_or("?")
}

fn pct(v: &mut [f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * q).round() as usize]
}

/// 撥號端：送模擬語音流量並統計
async fn probe(ep: &Endpoint, target: EndpointAddr, seconds: u64) -> Result<()> {
    let t0 = Instant::now();
    let conn = ep.connect(target, ALPN).await.context("連線失敗")?;
    println!("連線建立耗時 {} ms，路徑：{}", t0.elapsed().as_millis(), describe_paths(&conn));
    if let Some(m) = conn.max_datagram_size() {
        println!("datagram 上限 {m} bytes");
    }

    let total = (seconds * 1000 / INTERVAL.as_millis() as u64) as usize;
    let mut sent_at: Vec<Option<Instant>> = vec![None; total];
    let mut got = vec![false; total];
    let mut rtts_all: Vec<f64> = Vec::new();
    let mut rtts_sec: Vec<f64> = Vec::new();
    let mut lost_sec_base = 0usize;
    let mut first_direct: Option<Duration> = None;
    let mut path_switches: VecDeque<String> = VecDeque::new();
    let mut last_kind = selected_kind(&conn);

    let mut tick = tokio::time::interval(INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut report = tokio::time::interval(Duration::from_secs(1));
    report.tick().await;
    let start = Instant::now();
    let mut seq = 0usize;
    let mut buf = vec![0u8; PACKET_SIZE];

    loop {
        tokio::select! {
            _ = tick.tick(), if seq < total => {
                buf[..4].copy_from_slice(&(seq as u32).to_le_bytes());
                sent_at[seq] = Some(Instant::now());
                if let Err(e) = conn.send_datagram(buf.clone().into()) {
                    eprintln!("送出失敗：{e}");
                }
                seq += 1;
            }
            d = conn.read_datagram() => {
                let d = d.context("連線中斷")?;
                if d.len() >= 4 {
                    let s = u32::from_le_bytes(d[..4].try_into().unwrap()) as usize;
                    if s < total && !got[s] {
                        got[s] = true;
                        if let Some(t) = sent_at[s] {
                            let ms = t.elapsed().as_secs_f64() * 1000.0;
                            rtts_all.push(ms);
                            rtts_sec.push(ms);
                        }
                    }
                }
            }
            _ = report.tick() => {
                let kind = selected_kind(&conn);
                if kind == "直連" && first_direct.is_none() {
                    first_direct = Some(t0.elapsed());
                }
                if kind != last_kind {
                    path_switches.push_back(format!("{:.1}s：{last_kind} → {kind}", start.elapsed().as_secs_f64()));
                    last_kind = kind;
                }
                // 給最後送出的封包 500 ms 回來，算「1 秒前那一秒」的掉包
                let window_end = seq.saturating_sub(25);
                let lost = (lost_sec_base..window_end).filter(|&i| !got[i]).count();
                let n = window_end.saturating_sub(lost_sec_base).max(1);
                lost_sec_base = window_end;
                println!(
                    "[{:>3}s] {kind:<5}｜RTT 中位數 {:6.1} ms  p95 {:6.1} ms  最大 {:6.1} ms｜掉包 {:4.1}%｜{}",
                    start.elapsed().as_secs(),
                    pct(&mut rtts_sec.clone(), 0.5),
                    pct(&mut rtts_sec.clone(), 0.95),
                    rtts_sec.iter().cloned().fold(f64::NAN, f64::max),
                    lost as f64 * 100.0 / n as f64,
                    describe_paths(&conn),
                );
                rtts_sec.clear();
                if seq >= total && start.elapsed() > Duration::from_secs(seconds) + Duration::from_millis(1000) {
                    break;
                }
            }
        }
    }

    let lost = got.iter().filter(|g| !**g).count();
    // 抖動：相鄰封包 RTT 差的平均
    let jitter = if rtts_all.len() > 1 {
        rtts_all.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>() / (rtts_all.len() - 1) as f64
    } else {
        f64::NAN
    };
    let mut v = rtts_all.clone();
    println!("\n=== 結果（{seconds} 秒，{total} 個封包）===");
    println!(
        "RTT：最小 {:.1}｜中位數 {:.1}｜p95 {:.1}｜p99 {:.1}｜最大 {:.1} ms｜抖動 {:.1} ms",
        pct(&mut v, 0.0), pct(&mut v, 0.5), pct(&mut v, 0.95), pct(&mut v, 0.99), pct(&mut v, 1.0), jitter
    );
    println!("掉包：{lost}/{total}（{:.2}%）", lost as f64 * 100.0 / total as f64);
    match first_direct {
        Some(d) => println!("切換到直連：連線後 {:.1} 秒內", d.as_secs_f64()),
        None => println!("全程沒有直連（一直走 relay）"),
    }
    for s in path_switches {
        println!("路徑切換：{s}");
    }
    conn.close(0u32.into(), b"done");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // 預設只顯示 iroh 的警告（例如被 relay 限流）；可用 RUST_LOG 調整
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let o = parse_args()?;
    match o.mode.as_str() {
        "listen" => {
            let ep = bind(&o, true).await?;
            print_addr(&ep);
            println!(
                "\n在另一台電腦執行：netprobe connect {}{}{}",
                ep.id(),
                if o.relay_only { " --relay-only" } else { "" },
                o.relay.as_ref().map(|u| format!(" --relay {u}")).unwrap_or_default()
            );
            println!("等待連線中（Ctrl+C 結束）…");
            serve(ep).await?;
        }
        "connect" => {
            let id: EndpointId = o.peer.as_deref().context("請提供對方的 EndpointId")?.parse()?;
            let ep = bind(&o, false).await?;
            // 只給公鑰：位址由 iroh 的位址查詢（DNS / pkarr）解析
            probe(&ep, EndpointAddr::from(id), o.seconds).await?;
            ep.close().await;
        }
        "local" => {
            let server = bind(&o, true).await?;
            let addr = server.addr();
            let client = bind(&o, false).await?;
            tokio::spawn(serve(server.clone()));
            println!("本機互測{}\n", if o.relay_only { "（強制 relay）" } else { "" });
            probe(&client, addr, o.seconds).await?;
            client.close().await;
            server.close().await;
        }
        other => bail!("未知模式 {other}"),
    }
    Ok(())
}
