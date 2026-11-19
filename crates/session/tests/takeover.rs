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
static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

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
        local: Some(transport::node_id(&owner_key.public())),
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

/// 每次加入的人對 MigrateTo 的反應延遲（模擬網路慢的成員）；測試用全域設定，依加入順序取用
static REACT_DELAY: std::sync::Mutex<Vec<(String, Duration)>> = std::sync::Mutex::new(Vec::new());

fn react_delay(name: &str) -> Duration {
    REACT_DELAY.lock().unwrap().iter().find(|(n, _)| n == name).map(|x| x.1).unwrap_or_default()
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
    let tag = format!("{name}#{}", c.session_idx());
    let t0 = *EPOCH.get_or_init(Instant::now);
    let t = Takeover::new(
        c.clone(),
        standby.clone(),
        host,
        Arc::new(move |m| {
            if std::env::var("TAKEOVER_LOG").is_ok() {
                println!("{:>6.2}s [{tag}] {m}", t0.elapsed().as_secs_f64());
            }
        }),
    );
    t.start_standby(false);
    t.start_merge_loop();
    let (tx, ev) = mpsc::unbounded_channel();
    let t2 = t.clone();
    let delay = react_delay(name);
    tokio::spawn(async move {
        while let Some(e) = raw.recv().await {
            match e {
                ClientEvent::MigrateTo { .. } | ClientEvent::HostLost(_) | ClientEvent::MoveTo { .. } if !react => {}
                ClientEvent::MoveTo { host } => {
                    tokio::spawn(t2.clone().on_move(host));
                }
                ClientEvent::MigrateTo { session_idx, host, term } => {
                    let t = t2.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(delay).await;
                        t.on_migrate(session_idx, host, term).await
                    });
                }
                ClientEvent::HostLost(_) => {
                    tokio::spawn(t2.clone().on_host_lost());
                }
                ClientEvent::Rtt(_) => {}
                ClientEvent::Kicked => {
                    t2.on_kicked();
                    let _ = tx.send(ClientEvent::Kicked);
                }
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

#[tokio::test(flavor = "multi_thread")]
async fn split_brain_merges_back_into_bigger_room() -> Result<()> {
    let (room, a, b, c) = three_member_room().await?;
    let started = Instant::now();
    let original = transport::node_id(&room.host_ep.id());

    // 模擬「B 自己斷網、以為房主不見了」：B 自己升任房主，於是有兩個房主：
    // 原房主（A、C，任期 1）與 B（只有 B，任期 2）
    b.t.promote(2).await?;
    assert!(b.t.host().is_some());
    wait_until("原房主看到 B 離開", || room.host.member_count() == 2).await;

    // B 的房主每 3 秒探測，發現原房主人比較多 → 讓位，B 合併回原房間
    wait_until("B 合併回原房間", || b.t.host().is_none() && b.c.host_id() == original).await;
    wait_until("原房間恢復三個人", || room.host.member_count() == 3).await;
    println!("雙房主合併完成：{:?}", started.elapsed());
    assert!(a.t.host().is_some(), "原房主不該讓位");
    assert_eq!(c.c.host_id(), original, "C 不受影響");
    // B 的訂閱也重新登記了
    let bi = b.c.session_idx();
    wait_until("B 的訂閱回到原房主", || {
        room.host.subscriptions().contains(&Subscription { session_idx: bi, channel: DEFAULT_CHANNEL, listen: true, talk: true })
    })
    .await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn old_host_with_fewer_members_yields() -> Result<()> {
    let (room, a, b, c) = three_member_room().await?;
    let started = Instant::now();

    // 模擬「原房主網路閃斷，B、C 已經轉到 B 當房主」：B 升任，C 改連 B；原房主只剩自己（A）
    b.t.promote(2).await?;
    let b_addr = transport::loopback_addr(b.standby.as_ref().unwrap());
    c.c.reconnect(b_addr).await?;
    let b_host = b.t.host().unwrap();
    wait_until("B 那邊有 B、C", || b_host.member_count() == 2).await;

    // 原房主（1 人）探測到 B（2 人）→ 讓位，A 也改連到 B
    let b_id = transport::node_id(&b.standby.as_ref().unwrap().id());
    wait_until("A 合併到 B", || a.t.host().is_none() && a.c.host_id() == b_id).await;
    wait_until("B 那邊三個人", || b_host.member_count() == 3).await;
    println!("舊房主讓位完成：{:?}", started.elapsed());
    assert!(b.t.host().is_some(), "B 不該讓位");
    // 停掉後成員是非同步移除的（舊連線還會保留一下：先建後拆），用等的
    wait_until("原房主停掉後沒有人", || room.host.member_count() == 0).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn host_hands_over_and_stays_as_member() -> Result<()> {
    let (room, a, b, c) = three_member_room().await?;
    let started = Instant::now();

    // A（房主）讓出房主：B 接手，A 自己留在房間
    assert!(a.t.hand_over());
    let b_id = transport::node_id(&b.standby.as_ref().unwrap().id());
    wait_until("B 成為房主", || b.t.host().is_some()).await;
    wait_until("A、C 都改連到 B", || a.c.host_id() == b_id && c.c.host_id() == b_id).await;
    let b_host = b.t.host().unwrap();
    wait_until("B 那邊三個人", || b_host.member_count() == 3).await;
    wait_until("A 不再是房主、舊房主停掉", || a.t.host().is_none() && room.host.member_count() == 0).await;
    println!("讓出房主完成：{:?}", started.elapsed());
    assert_eq!(b_host.term(), 2);
    Ok(())
}

/// 計畫性轉移期間的語音空白：C 一直說話（每 20 ms 一包）、B 一直聽，
/// 照 `Session::leave` 的順序（migrate → 等一下 → 關掉舊房主），量 B 收到 C 的最長間隔與遺失包數
#[tokio::test(flavor = "multi_thread")]
async fn planned_migration_audio_gap() -> Result<()> {
    audio_gap("Cslow", Duration::ZERO).await
}

/// C 晚 400 ms 才改連（網路慢）：B 先換到新房主，兩人短暫分在兩個房主上
#[tokio::test(flavor = "multi_thread")]
async fn planned_migration_audio_gap_slow_member() -> Result<()> {
    audio_gap("Cslow2", Duration::from_millis(400)).await
}

async fn audio_gap(c_name: &str, c_delay: Duration) -> Result<()> {
    REACT_DELAY.lock().unwrap().push((c_name.to_string(), c_delay));
    let (room, a) = start_room().await?;
    let mut b = connect(&room, "B", transport::generate_secret(), None, true).await?;
    let mut c = connect(&room, c_name, transport::generate_secret(), None, true).await?;
    for m in [&b, &c] {
        m.c.subscribe(DEFAULT_CHANNEL, true, true)?;
    }
    let bi = b.c.session_idx();
    wait_until("候補名單 B 第一", || c.c.successors().first().is_some_and(|s| s.session_idx == bi)).await;
    wait_until("B、C 的訂閱生效", || room.host.subscriptions().len() >= 2).await;
    let ci = c.c.session_idx();
    let speaker = talk(c.c.clone());
    // B 收聽：記錄每個 C 的封包（序號、到達時間）
    let listener = b.c.clone();
    let rec = tokio::spawn(async move {
        let mut got: Vec<(u32, Instant)> = Vec::new();
        let end = Instant::now() + Duration::from_secs(6);
        while Instant::now() < end {
            let Ok(Ok(d)) = timeout(Duration::from_millis(200), listener.recv_voice()).await else { continue };
            if let Some(v) = VoiceDown::parse(&d)
                && v.kind == Kind::Voice
                && v.sender_idx == ci
            {
                got.push((v.seq, Instant::now()));
            }
        }
        got
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    // 照 Session::leave：migrate → 自己離開 → 等成員改連過去（沒人或 DRAIN）→ 關掉
    room.host.migrate().expect("有候補");
    a.c.leave().await;
    let start = Instant::now();
    tokio::time::sleep(Duration::from_millis(300)).await;
    while room.host.member_count() > 0 && start.elapsed() < client_core::DRAIN {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    room.host_ep.close().await;
    wait_reconnected(&mut b, 2).await;
    wait_reconnected(&mut c, 2).await;
    let got = rec.await?;
    speaker.abort();

    let mut seqs: Vec<u32> = got.iter().map(|g| g.0).collect();
    seqs.sort();
    seqs.dedup();
    let (first, last) = (seqs[0], *seqs.last().unwrap());
    let lost = (last - first + 1) as usize - seqs.len();
    let max_gap = got.windows(2).map(|w| w[1].1 - w[0].1).max().unwrap();
    println!("轉移期間：最長間隔 {max_gap:?}，遺失 {lost} 包（共 {} 包）", last - first + 1);
    assert!(max_gap < Duration::from_millis(150), "最長間隔 {max_gap:?}");
    assert!(lost <= 2, "遺失 {lost} 包");
    Ok(())
}

/// 關掉一個成員的所有端點（模擬當機／斷網）；原始房主還要關掉房間的房主端點
async fn kill(m: &M, room_ep: Option<&Endpoint>) {
    // 當機的程式整個不在了：換房主流程也要停（否則測試裡會留下還在嘗試接手的「殭屍」）
    m.t.shutdown();
    m._ep.close().await;
    if let Some(s) = &m.standby {
        s.close().await;
    }
    if let Some(ep) = room_ep {
        ep.close().await;
    }
}

/// 房主照 `Session::leave` 的順序離開：migrate → 自己離開 → 等成員改連（沒人或 DRAIN）→ 關掉。回傳被指定接手的人
async fn host_leaves(m: &M, room_ep: Option<&Endpoint>, before_close: impl AsyncFnOnce(u16)) -> u16 {
    let host = m.t.host().expect("要是房主");
    let next = host.migrate().expect("有候補").session_idx;
    before_close(next).await;
    m.c.leave().await;
    let start = Instant::now();
    tokio::time::sleep(Duration::from_millis(300)).await;
    while host.member_count() > 0 && start.elapsed() < client_core::DRAIN {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    kill(m, room_ep).await;
    next
}

/// 收斂：活著的人裡只有一個房主、所有人連到同一個房主、任期一致、房主那邊的人數正確，而且語音互通
async fn converged(what: &str, ms: &[M], alive: &[usize]) -> u64 {
    let dump = || {
        for &i in alive {
            let m = &ms[i];
            println!(
                "  [{i}] 自己 {:02x?} 備用 {:02x?} #{} host_id {:02x?} term {} 我是房主 {:?} 找房主中 {} 候補 {:?}",
                &m.c.node_id()[..3],
                m.standby.as_ref().map(|e| transport::node_id(&e.id())[..3].to_vec()),
                m.c.session_idx(),
                &m.c.host_id()[..3],
                m.c.term(),
                m.t.host().map(|h| (h.term(), h.member_count())),
                m.t.recovering(),
                m.c.successors().iter().map(|s| s.session_idx).collect::<Vec<_>>()
            );
        }
    };
    let ok = timeout(T, async {
        loop {
            let hosts: Vec<Host> = alive.iter().filter_map(|&i| ms[i].t.host()).collect();
            let first = &ms[alive[0]].c;
            if hosts.len() == 1
                && alive.iter().all(|&i| ms[i].c.host_id() == first.host_id() && ms[i].c.term() == hosts[0].term())
                && hosts[0].member_count() == alive.len()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    if ok.is_err() {
        println!("  所有成員（含已關掉的）：");
        for (i, m) in ms.iter().enumerate() {
            println!(
                "  [{i}] 自己 {:02x?} 備用 {:02x?} #{} 我是房主 {:?}",
                &m.c.node_id()[..3],
                m.standby.as_ref().map(|e| transport::node_id(&e.id())[..3].to_vec()),
                m.c.session_idx(),
                m.t.host().map(|h| (h.term(), h.member_count()))
            );
        }
        println!("  活著的：");
        dump();
        panic!("等待逾時：{what}");
    }
    let term = ms[alive[0]].c.term();
    // 最後兩個人：一個說、一個聽（訂閱要在新房主上重新登記好）
    let (x, y) = (&ms[alive[alive.len() - 2]], &ms[alive[alive.len() - 1]]);
    let speaker = talk(x.c.clone());
    assert!(hears(&y.c, x.c.session_idx()).await, "{what}：語音要互通");
    speaker.abort();
    println!("{what}：收斂，任期 {term}，{} 人", alive.len());
    term
}

/// 混沌測試：8 人房依序經歷 計畫性轉移 → 房主當機 → 房主與第一候補同時當機 → 轉移途中接手的人當機，
/// 每一步之後都要收斂成單一房主、大家連到他、語音互通
#[tokio::test(flavor = "multi_thread")]
async fn chaos_sequence_keeps_single_host() -> Result<()> {
    let started = Instant::now();
    let (room, a) = start_room().await?;
    let mut ms = vec![a];
    for i in 1..8 {
        let m = connect(&room, &format!("成員{i}"), transport::generate_secret(), None, true).await?;
        m.c.subscribe(DEFAULT_CHANNEL, true, true)?;
        ms.push(m);
    }
    wait_until("7 個候補都登記", || ms[1].c.successors().len() == 7).await;
    let mut alive: Vec<usize> = (0..8).collect();
    let pos = |ms: &[M], alive: &[usize], idx: u16| alive.iter().position(|&i| ms[i].c.session_idx() == idx).unwrap();
    let host_pos = |ms: &[M], alive: &[usize]| alive.iter().position(|&i| ms[i].t.host().is_some()).unwrap();

    // 1. 計畫性轉移：原始房主離開
    host_leaves(&ms[0], Some(&room.host_ep), async |_| {}).await;
    alive.remove(0);
    let t1 = converged("1 計畫性轉移", &ms, &alive).await;

    // 2. 房主當機
    let h = alive.remove(host_pos(&ms, &alive));
    kill(&ms[h], None).await;
    let t2 = converged("2 房主當機", &ms, &alive).await;
    assert!(t2 > t1);

    // 3. 房主與第一候補同時當機
    let h = alive.remove(host_pos(&ms, &alive));
    let first = ms[alive[0]].c.successors()[0].session_idx;
    let s = alive.remove(pos(&ms, &alive, first));
    kill(&ms[h], None).await;
    kill(&ms[s], None).await;
    let t3 = converged("3 房主與第一候補同時當機", &ms, &alive).await;
    assert!(t3 > t2);

    // 4. 計畫性轉移途中，被指定接手的人當機
    let h = alive.remove(host_pos(&ms, &alive));
    let mut victim = None;
    let next = host_leaves(&ms[h], None, async |next| {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let i = ms.iter().position(|m| m.c.session_idx() == next).unwrap();
        kill(&ms[i], None).await;
        victim = Some(i);
    })
    .await;
    let v = victim.expect("接手的人");
    assert_eq!(ms[v].c.session_idx(), next);
    alive.retain(|&i| i != v);
    let t4 = converged("4 轉移途中接手的人當機", &ms, &alive).await;
    assert!(t4 > t3);
    println!("混沌測試完成：{:?}", started.elapsed());
    Ok(())
}

/// 房主（管理權）跟著開房的人：A 讓出轉發給 B 之後，B 那邊的房主還是 A
#[tokio::test(flavor = "multi_thread")]
async fn owner_stays_with_creator_after_hand_over() -> Result<()> {
    let (_room, a, b, c) = three_member_room().await?;
    let a_node = a.c.node_id();
    assert!(a.c.is_owner() && !b.c.is_owner());
    assert!(a.t.hand_over());
    wait_until("B 成為轉發者", || b.t.host().is_some()).await;
    let b_host = b.t.host().unwrap();
    wait_until("B 那邊三個人", || b_host.member_count() == 3).await;
    assert_eq!(b_host.owner(), Some(a_node), "新的轉發者沿用原本的房主");
    wait_until("大家都看到房主還是 A", || [&a, &b, &c].iter().all(|m| m.c.term() == 2 && m.c.owner() == Some(a_node)))
        .await;
    assert!(a.c.is_owner() && !b.c.is_owner());
    Ok(())
}

/// 房主移出目前的轉發者 B：其他人先改連下一位候補 C，B 收到 Kicked 後停掉自己的房主
#[tokio::test(flavor = "multi_thread")]
async fn owner_kicks_the_relay() -> Result<()> {
    let (room, a, mut b, c) = three_member_room().await?;
    let old_secret = room.ticket.secret;
    let a_node = a.c.node_id();
    assert!(a.t.hand_over());
    wait_until("B 成為轉發者", || b.t.host().is_some()).await;
    let b_host = b.t.host().unwrap();
    wait_until("B 那邊三個人、候補只剩 C", || {
        let s = a.c.successors();
        b_host.member_count() == 3 && s.len() == 1 && s[0].session_idx == c.c.session_idx()
    })
    .await;

    a.c.kick(b.c.session_idx());
    let r = timeout(T, async {
        loop {
            if let Some(ClientEvent::Kicked) = b.ev.recv().await {
                return;
            }
        }
    })
    .await;
    assert!(r.is_ok(), "B 要收到 Kicked");
    wait_until("C 接手轉發", || c.t.host().is_some()).await;
    let c_host = c.t.host().unwrap();
    let c_id = transport::node_id(&c.standby.as_ref().unwrap().id());
    wait_until("A 改連到 C、C 那邊兩個人", || a.c.host_id() == c_id && c_host.member_count() == 2).await;
    wait_until("B 的轉發停掉", || b.t.host().is_none()).await;
    assert_eq!(c_host.owner(), Some(a_node), "房主還是 A");
    assert!(c_host.secret() != old_secret && c_host.secret() == a.c.ticket().secret, "C 用新的密鑰");
    assert!(b.c.ticket().secret != c_host.secret(), "B 拿不到新密鑰");
    assert!(b.c.recv_voice().await.is_err());
    Ok(())
}
