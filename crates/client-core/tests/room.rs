//! 整合測試：同一行程內 1 個房主 + 成員，關閉 relay、只走本機直連（不依賴外部網路）。
//! 多頻道語意見 PLAN.md §2.3–2.4、§4.4。

use std::time::Duration;

use anyhow::Result;
use client_core::{Client, ClientEvent, VoiceDown};
use host_core::{Host, HostConfig};
use iroh::{Endpoint, EndpointAddr, SecretKey};
use protocol::control::{ErrorCode, Subscription};
use protocol::ticket::{RoomId, RoomSecret, Ticket};
use protocol::voice::Kind;
use protocol::{ChannelId, DEFAULT_CHANNEL};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::timeout;
use transport::RelaySetup;

const T: Duration = Duration::from_secs(10);

struct Room {
    host_ep: Endpoint,
    host: Host,
    addr: EndpointAddr,
    ticket: Ticket,
}

async fn start_room() -> Result<Room> {
    let host_ep = transport::bind(transport::generate_secret(), RelaySetup::Disabled, true).await?;
    let ticket = Ticket {
        room_id: RoomId(transport::random_bytes()),
        secret: RoomSecret(transport::random_bytes()),
        bootstrap: vec![transport::node_id(&host_ep.id())],
    };
    let host = Host::new(HostConfig { room_id: ticket.room_id, secret: ticket.secret, term: 1, max_members: 100 });
    tokio::spawn({
        let host = host.clone();
        let ep = host_ep.clone();
        async move { host.run(ep).await }
    });
    let addr = transport::loopback_addr(&host_ep);
    Ok(Room { host_ep, host, addr, ticket })
}

struct M {
    _ep: Endpoint,
    c: Client,
    ev: UnboundedReceiver<ClientEvent>,
}

async fn member(room: &Room, name: &str, key: SecretKey) -> Result<M> {
    let ep = transport::bind(key, RelaySetup::Disabled, false).await?;
    let (c, ev) = timeout(T, Client::join_addr(&ep, room.addr.clone(), &room.ticket, name)).await??;
    Ok(M { _ep: ep, c, ev })
}

/// 等到某個事件（略過 Rtt）
async fn next_event(rx: &mut UnboundedReceiver<ClientEvent>) -> ClientEvent {
    loop {
        match timeout(T, rx.recv()).await.expect("等事件逾時").expect("事件通道關閉") {
            ClientEvent::Rtt(_) => continue,
            e => return e,
        }
    }
}

/// 等房主端的訂閱狀態符合預期（訂閱是非同步生效的）
async fn wait_subs(host: &Host, want: &[Subscription]) {
    let ok = timeout(T, async {
        loop {
            let have = host.subscriptions();
            if want.iter().all(|w| have.contains(w)) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(ok.is_ok(), "訂閱沒有生效：想要 {want:?}，實際 {:?}", host.subscriptions());
}

async fn wait_channel(host: &Host, name: &str) -> ChannelId {
    timeout(T, async {
        loop {
            if let Some(c) = host.channels().into_iter().find(|c| c.name == name) {
                return c.id;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("頻道沒有建立")
}

fn sub(idx: u16, channel: ChannelId, listen: bool, talk: bool) -> Subscription {
    Subscription { session_idx: idx, channel, listen, talk }
}

/// 收集 `within` 內收到的語音封包：(kind, sender, seq, via_mask)
async fn collect(c: &Client, within: Duration) -> Vec<(Kind, u16, u32, u64)> {
    let mut v = Vec::new();
    let end = tokio::time::Instant::now() + within;
    while let Ok(Ok(d)) = tokio::time::timeout_at(end, c.recv_voice()).await {
        let p = VoiceDown::parse(&d).expect("下行封包格式");
        v.push((p.kind, p.sender_idx, p.seq, p.via_mask));
    }
    v
}

fn mask(slots: &[u8]) -> u64 {
    slots.iter().fold(0, |m, s| m | 1u64 << s)
}

#[tokio::test(flavor = "multi_thread")]
async fn join_talk_leave_in_default_channel() -> Result<()> {
    let room = start_room().await?;
    let mut a = member(&room, "小明", transport::generate_secret()).await?;
    let b = member(&room, "阿華", transport::generate_secret()).await?;

    assert_eq!(b.c.members().iter().map(|m| m.display_name.as_str()).collect::<Vec<_>>(), ["小明"]);
    assert_eq!(b.c.channels().iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["大廳"]);
    match next_event(&mut a.ev).await {
        ClientEvent::MemberJoined(m) => assert_eq!(m.display_name, "阿華"),
        e => panic!("預期 MemberJoined，收到 {e:?}"),
    }

    let sa = a.c.subscribe(DEFAULT_CHANNEL, true, true)?;
    let sb = b.c.subscribe(DEFAULT_CHANNEL, true, true)?;
    let (ia, ib) = (a.c.session_idx(), b.c.session_idx());
    wait_subs(&room.host, &[sub(ia, 0, true, true), sub(ib, 0, true, true)]).await;

    for seq in 0..5u32 {
        a.c.send_voice(Kind::Voice, mask(&[sa]), seq, &[seq as u8; 80]);
    }
    a.c.send_voice(Kind::EndOfStream, 0, 5, &[]);
    let got = collect(&b.c, Duration::from_millis(500)).await;
    let want: Vec<_> = (0..5).map(|s| (Kind::Voice, ia, s, mask(&[sb]))).chain([(Kind::EndOfStream, ia, 5, 0)]).collect();
    assert_eq!(got, want);
    assert!(collect(&a.c, Duration::from_millis(200)).await.is_empty(), "不該收到自己的回音");

    // 雙方都看得到彼此的訂閱
    assert!(a.c.subscriptions().contains(&sub(ib, 0, true, true)));

    b.c.leave().await;
    loop {
        if let ClientEvent::MemberLeft(i) = next_event(&mut a.ev).await {
            assert_eq!(i, ib);
            break;
        }
    }
    assert!(!a.c.subscriptions().iter().any(|s| s.session_idx == ib), "離開的人的訂閱要清掉");
    room.host_ep.close().await;
    Ok(())
}

/// PLAN §2.4：A 同時對 1、2 說話，同時聽 1、2 的 B 只收到一份；只聽 2 的 C 也是一份；沒聽的 D 收不到
#[tokio::test(flavor = "multi_thread")]
async fn multi_channel_fanout_is_deduplicated() -> Result<()> {
    let room = start_room().await?;
    let a = member(&room, "A", transport::generate_secret()).await?;
    let b = member(&room, "B", transport::generate_secret()).await?;
    let c = member(&room, "C", transport::generate_secret()).await?;
    let d = member(&room, "D", transport::generate_secret()).await?;
    a.c.create_channel("指揮部");
    a.c.create_channel("後勤");
    let (c1, c2) = (wait_channel(&room.host, "指揮部").await, wait_channel(&room.host, "後勤").await);
    // 等成員端也收到頻道
    timeout(T, async {
        while [&a, &b, &c, &d].iter().any(|m| m.c.channels().len() < 3) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;

    let (a1, a2) = (a.c.subscribe(c1, false, true)?, a.c.subscribe(c2, false, true)?);
    // 刻意讓 B 的槽位順序跟 A 不同，驗證 via_mask 用的是「接收者自己的」槽位
    let b2 = b.c.subscribe(c2, true, false)?;
    let b1 = b.c.subscribe(c1, true, false)?;
    let cc2 = c.c.subscribe(c2, true, false)?;
    d.c.subscribe(DEFAULT_CHANNEL, true, false)?;
    let (ia, ib, ic, id) = (a.c.session_idx(), b.c.session_idx(), c.c.session_idx(), d.c.session_idx());
    wait_subs(
        &room.host,
        &[sub(ia, c1, false, true), sub(ia, c2, false, true), sub(ib, c1, true, false), sub(ib, c2, true, false), sub(ic, c2, true, false), sub(id, 0, true, false)],
    )
    .await;

    for seq in 0..3u32 {
        a.c.send_voice(Kind::Voice, mask(&[a1, a2]), seq, &[7; 60]);
    }
    let (gb, gc, gd) = tokio::join!(
        collect(&b.c, Duration::from_millis(500)),
        collect(&c.c, Duration::from_millis(500)),
        collect(&d.c, Duration::from_millis(500)),
    );
    assert_eq!(gb, (0..3).map(|s| (Kind::Voice, ia, s, mask(&[b1, b2]))).collect::<Vec<_>>(), "B 每幀只收一份，via 標出兩個頻道");
    assert_eq!(gc, (0..3).map(|s| (Kind::Voice, ia, s, mask(&[cc2]))).collect::<Vec<_>>());
    assert!(gd.is_empty(), "D 沒有收聽 A 說話的頻道：{gd:?}");
    Ok(())
}

/// 在某頻道只有收聽權（talk = false）時，即使 talk_mask 設了也送不出去
#[tokio::test(flavor = "multi_thread")]
async fn talk_requires_talk_flag() -> Result<()> {
    let room = start_room().await?;
    let a = member(&room, "A", transport::generate_secret()).await?;
    let b = member(&room, "B", transport::generate_secret()).await?;
    let sa = a.c.subscribe(DEFAULT_CHANNEL, true, false)?;
    b.c.subscribe(DEFAULT_CHANNEL, true, true)?;
    wait_subs(&room.host, &[sub(a.c.session_idx(), 0, true, false), sub(b.c.session_idx(), 0, true, true)]).await;
    a.c.send_voice(Kind::Voice, mask(&[sa]), 0, &[1; 40]);
    assert!(collect(&b.c, Duration::from_millis(300)).await.is_empty());
    Ok(())
}

/// A 從對 {1,2} 說話改成只對 {2}：只聽 1 的 B 要立刻收到 end-of-stream
#[tokio::test(flavor = "multi_thread")]
async fn eos_when_listener_drops_out_of_talk_set() -> Result<()> {
    let room = start_room().await?;
    let a = member(&room, "A", transport::generate_secret()).await?;
    let b = member(&room, "B", transport::generate_secret()).await?;
    a.c.create_channel("一隊");
    a.c.create_channel("二隊");
    let (c1, c2) = (wait_channel(&room.host, "一隊").await, wait_channel(&room.host, "二隊").await);
    timeout(T, async {
        while a.c.channels().len() < 3 || b.c.channels().len() < 3 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let (a1, a2) = (a.c.subscribe(c1, false, true)?, a.c.subscribe(c2, false, true)?);
    let b1 = b.c.subscribe(c1, true, false)?;
    let ia = a.c.session_idx();
    wait_subs(&room.host, &[sub(ia, c1, false, true), sub(ia, c2, false, true), sub(b.c.session_idx(), c1, true, false)]).await;

    a.c.send_voice(Kind::Voice, mask(&[a1, a2]), 0, &[1; 40]);
    a.c.send_voice(Kind::Voice, mask(&[a1, a2]), 1, &[1; 40]);
    a.c.send_voice(Kind::Voice, mask(&[a2]), 2, &[1; 40]);
    a.c.send_voice(Kind::Voice, mask(&[a2]), 3, &[1; 40]);
    let got = collect(&b.c, Duration::from_millis(500)).await;
    assert_eq!(
        got,
        vec![(Kind::Voice, ia, 0, mask(&[b1])), (Kind::Voice, ia, 1, mask(&[b1])), (Kind::EndOfStream, ia, 2, 0)],
        "B 不再聽得到時只補一次 end-of-stream"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn channel_lifecycle() -> Result<()> {
    let room = start_room().await?;
    let mut a = member(&room, "A", transport::generate_secret()).await?;
    let mut b = member(&room, "B", transport::generate_secret()).await?;
    let _ = next_event(&mut a.ev).await; // B 加入

    a.c.create_channel("  第一小隊\n");
    let ch = match next_event(&mut b.ev).await {
        ClientEvent::ChannelAdded(c) => {
            assert_eq!(c.name, "第一小隊", "名稱要去掉控制字元與空白");
            c.id
        }
        e => panic!("預期 ChannelAdded，收到 {e:?}"),
    };
    let _ = next_event(&mut a.ev).await;

    b.c.subscribe(ch, true, true)?;
    assert_eq!(next_event(&mut a.ev).await, ClientEvent::SubscriptionChanged(sub(b.c.session_idx(), ch, true, true)));
    let _ = next_event(&mut b.ev).await; // 自己的確認

    a.c.rename_channel(ch, "突擊隊");
    assert_eq!(next_event(&mut b.ev).await, ClientEvent::ChannelRenamed(ch, "突擊隊".into()));

    a.c.delete_channel(DEFAULT_CHANNEL);
    loop {
        match next_event(&mut a.ev).await {
            ClientEvent::HostError(code, _) => {
                assert_eq!(code, ErrorCode::Forbidden);
                break;
            }
            ClientEvent::ChannelRenamed(..) => continue,
            e => panic!("預期 HostError，收到 {e:?}"),
        }
    }

    a.c.delete_channel(ch);
    assert_eq!(next_event(&mut b.ev).await, ClientEvent::ChannelRemoved(ch));
    assert!(b.c.my_slots().get(&ch).is_none(), "頻道刪除後槽位要釋放");
    assert!(b.c.subscriptions().iter().all(|s| s.channel != ch));
    assert!(room.host.subscriptions().iter().all(|s| s.channel != ch));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_secret_is_rejected() -> Result<()> {
    let mut room = start_room().await?;
    room.ticket.secret = RoomSecret([0; 32]);
    let err = match member(&room, "壞人", transport::generate_secret()).await {
        Ok(_) => panic!("錯誤的密鑰不該能加入"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains("AuthFailed"), "{err}");
    assert_eq!(room.host.members().len(), 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_keeps_session_idx() -> Result<()> {
    let room = start_room().await?;
    let key = transport::generate_secret();
    let first = member(&room, "小明", key.clone()).await?;
    let idx = first.c.session_idx();
    first.c.leave().await;
    // 另一個人先加入，確保 idx 不是單純「下一個號碼」碰巧相同
    let other = member(&room, "阿華", transport::generate_secret()).await?;
    let again = member(&room, "小明", key).await?;
    assert_eq!(again.c.session_idx(), idx);
    assert_ne!(other.c.session_idx(), idx);
    Ok(())
}
