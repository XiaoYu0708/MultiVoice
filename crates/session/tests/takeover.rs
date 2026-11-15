//! 房主轉移（M3）整合測試：同一行程內，關閉 relay、只走本機直連。
//!
//! 每位成員都照 session 的做法接上 [`Takeover`]：MigrateTo → on_migrate、HostLost → on_host_lost。

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use client_core::{Client, ClientEvent, VoiceDown};
use host_core::{Host, HostConfig};
use iroh::Endpoint;
use protocol::DEFAULT_CHANNEL;
use protocol::control::Subscription;
use protocol::ticket::{RoomId, RoomSecret, Ticket};
use protocol::voice::Kind;
use session::takeover::Takeover;
use tokio::sync::mpsc;
use tokio::time::timeout;
use transport::RelaySetup;

const T: Duration = Duration::from_secs(25);

struct Room {
    host_ep: Endpoint,
    host: Host,
    ticket: Ticket,
}

struct M {
    _ep: Endpoint,
    standby: Option<Endpoint>,
    c: Client,
    t: Arc<Takeover>,
    /// Takeover 以外的事件（Reconnected、Disconnected…）
    ev: mpsc::UnboundedReceiver<ClientEvent>,
}

async fn start_room() -> Result<(Room, M)> {
    let host_ep = transport::bind(transport::generate_secret(), RelaySetup::Disabled, true).await?;
    let ticket = Ticket {
        room_id: RoomId(transport::random_bytes()),
        secret: RoomSecret(transport::random_bytes()),
        bootstrap: vec![transport::node_id(&host_ep.id())],
    };
    let owner_key = transport::generate_secret();
    let host = Host::new(HostConfig {
        room_id: ticket.room_id,
        secret: ticket.secret,
        term: 1,
        max_members: 100,
        owner: Some(transport::node_id(&owner_key.public())),
        seed: None,
    });
    tokio::spawn({
        let (host, ep) = (host.clone(), host_ep.clone());
        async move { host.run(ep).await }
    });
    let room = Room { host_ep, host: host.clone(), ticket };
    let owner = connect(&room, "房主", owner_key, Some(host), false).await?;
    Ok((room, owner))
}

/// 加入並接上 Takeover（`standby` = 可以當候補）
/// `react = false`：不處理 MigrateTo／HostLost（模擬跟房主同時當掉的人）
async fn connect(room: &Room, name: &str, key: iroh::SecretKey, host: Option<Host>, standby: bool) -> Result<M> {
    connect_with(room, name, key, host, standby, true).await
}

async fn connect_with(
    room: &Room,
    name: &str,
    key: iroh::SecretKey,
    host: Option<Host>,
    standby: bool,
    react: bool,
) -> Result<M> {
    let ep = transport::bind(key, RelaySetup::Disabled, false).await?;
    let addr = transport::loopback_addr(&room.host_ep);
    let (c, mut raw) = timeout(T, Client::join_addr(&ep, addr, &room.ticket, name)).await??;
    let standby = if standby { Some(transport::bind(transport::generate_secret(), RelaySetup::Disabled, true).await?) } else { None };
    let t = Takeover::new(c.clone(), standby.clone(), host, Arc::new(|_| {}));
    t.start_standby(false);
    let (tx, ev) = mpsc::unbounded_channel();
    let t2 = t.clone();
    tokio::spawn(async move {
        while let Some(e) = raw.recv().await {
            match e {
                ClientEvent::MigrateTo { .. } | ClientEvent::HostLost(_) if !react => {}
                ClientEvent::MigrateTo { session_idx, host, term } => {
                    tokio::spawn(t2.clone().on_migrate(session_idx, host, term));
                }
                ClientEvent::HostLost(_) => {
                    tokio::spawn(t2.clone().on_host_lost());
                }
                ClientEvent::Rtt(_) => {}
                e => {
                    let _ = tx.send(e);
                }
            }
        }
    });
    Ok(M { _ep: ep, standby, c, t, ev })
}

async fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let ok = timeout(T, async {
        while !f() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(ok.is_ok(), "等待逾時：{what}");
}

async fn wait_reconnected(m: &mut M, term: u64) {
    let r = timeout(T, async {
        loop {
            match m.ev.recv().await {
                Some(ClientEvent::Reconnected { term: t }) if t == term => return,
                Some(ClientEvent::Disconnected(why)) => panic!("放棄了：{why}"),
                Some(_) => continue,
                None => panic!("事件通道關閉"),
            }
        }
    })
    .await;
    assert!(r.is_ok(), "等 Reconnected(term {term}) 逾時");
}

/// 收到某位說話者的語音（略過 end-of-stream）
async fn hears(listener: &Client, speaker: u16) -> bool {
    timeout(Duration::from_secs(5), async {
        loop {
            let d = listener.recv_voice().await.ok()?;
            if let Some(v) = VoiceDown::parse(&d)
                && v.kind == Kind::Voice
                && v.sender_idx == speaker
            {
                return Some(());
            }
        }
    })
    .await
    .ok()
    .flatten()
    .is_some()
}

/// 一直送語音直到停止（模擬擷取執行緒）
fn talk(c: Client) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut seq = 0u32;
        loop {
            c.send_voice(Kind::Voice, 1, seq, &[0xAB; 20]);
            seq += 1;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
}

/// 三人房：房主 A + 候補 B、C（B 排第一），B、C 都在大廳收聽與說話
async fn three_member_room() -> Result<(Room, M, M, M)> {
    three_member_room_with(true).await
}

/// `b_reacts = false`：B 不會接手（模擬 B 跟房主同時當掉）
async fn three_member_room_with(b_reacts: bool) -> Result<(Room, M, M, M)> {
    let (room, a) = start_room().await?;
    let b = connect_with(&room, "B", transport::generate_secret(), None, true, b_reacts).await?;
    let c = connect(&room, "C", transport::generate_secret(), None, true).await?;
    for m in [&b, &c] {
        m.c.subscribe(DEFAULT_CHANNEL, true, true)?;
    }
    let (bi, ci) = (b.c.session_idx(), c.c.session_idx());
    wait_until("候補名單 [B, C]", || {
        let s = c.c.successors();
        s.len() == 2 && s[0].session_idx == bi && s[1].session_idx == ci
    })
    .await;
    wait_until("B、C 的訂閱生效", || {
        let subs = room.host.subscriptions();
        [bi, ci].iter().all(|&i| subs.contains(&Subscription { session_idx: i, channel: DEFAULT_CHANNEL, listen: true, talk: true }))
    })
    .await;
    Ok((room, a, b, c))
}

/// 轉移後的共同檢查：B 是新房主（任期 2），C 的編號、訂閱不變，語音互通
async fn check_after(b: &M, c: &M, ci: u16) {
    let host = b.t.host().expect("B 應該是新房主");
    assert_eq!(host.term(), 2);
    assert_eq!(c.c.term(), 2);
    assert_eq!(c.c.session_idx(), ci, "session_idx 要沿用");
    assert_eq!(c.c.host_id(), transport::node_id(&b.standby.as_ref().unwrap().id()), "C 要連到 B 的備用端點");
    let bi = b.c.session_idx();
    wait_until("新房主上 B、C 的訂閱都重新登記", || {
        let subs = host.subscriptions();
        [bi, ci].iter().all(|&i| subs.contains(&Subscription { session_idx: i, channel: DEFAULT_CHANNEL, listen: true, talk: true }))
    })
    .await;
    let speaker = talk(c.c.clone());
    assert!(hears(&b.c, ci).await, "B 要聽得到 C");
    speaker.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn planned_migration_to_first_successor() -> Result<()> {
    let (room, a, mut b, mut c) = three_member_room().await?;
    let ci = c.c.session_idx();
    let started = Instant::now();

    // 房主要離開：交給 B，然後關掉（照 Session::leave 的順序）
    let next = room.host.migrate().expect("有候補");
    assert_eq!(next.session_idx, b.c.session_idx());
    tokio::time::sleep(Duration::from_millis(300)).await;
    a.c.leave().await;
    room.host_ep.close().await;

    wait_reconnected(&mut b, 2).await;
    wait_reconnected(&mut c, 2).await;
    println!("計畫性轉移完成：{:?}", started.elapsed());
    check_after(&b, &c, ci).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn host_crash_first_successor_takes_over() -> Result<()> {
    let (room, _a, mut b, mut c) = three_member_room().await?;
    let ci = c.c.session_idx();
    let started = Instant::now();

    // 房主當機：沒有 MigrateTo，連線直接斷
    room.host_ep.close().await;

    wait_reconnected(&mut b, 2).await;
    wait_reconnected(&mut c, 2).await;
    let took = started.elapsed();
    println!("非預期轉移完成：{took:?}");
    assert!(took < Duration::from_secs(8), "要在幾秒內恢復，實際 {took:?}");
    check_after(&b, &c, ci).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn second_successor_takes_over_when_first_is_gone() -> Result<()> {
    let (room, _a, b, mut c) = three_member_room_with(false).await?;
    let started = Instant::now();

    // 房主和第一候補同時當掉：B 不會接手（react = false）；只關房主端點，
    // 否則房主會先發現 B 離開、更新候補名單，C 就直接排第一了
    room.host_ep.close().await;
    b.c.give_up("模擬當機");

    wait_reconnected(&mut c, 2).await;
    let took = started.elapsed();
    println!("第二候補接手：{took:?}");
    assert!(c.t.host().is_some(), "C 應該成為房主");
    assert!(took >= Duration::from_secs(3), "第二候補要等第一候補的機會（k × 3 秒），實際 {took:?}");
    let host = c.t.host().unwrap();
    wait_until("C 在自己的房主上重新登記訂閱", || {
        host.subscriptions().iter().any(|s| s.session_idx == c.c.session_idx() && s.channel == DEFAULT_CHANNEL)
    })
    .await;
    Ok(())
}
