//! 整合測試：同一行程內 1 個房主 + 成員，關閉 relay、只走本機直連（不依賴外部網路）。

use std::time::Duration;

use anyhow::Result;
use client_core::{Client, ClientEvent, VoiceDown};
use host_core::{Host, HostConfig};
use iroh::{Endpoint, EndpointAddr, SecretKey};
use protocol::ticket::{RoomId, RoomSecret, Ticket};
use protocol::voice::Kind;
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

async fn member(room: &Room, name: &str, key: SecretKey) -> Result<(Endpoint, Client, UnboundedReceiver<ClientEvent>)> {
    let ep = transport::bind(key, RelaySetup::Disabled, false).await?;
    let (c, rx) = timeout(T, Client::join_addr(&ep, room.addr.clone(), &room.ticket, name)).await??;
    Ok((ep, c, rx))
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

#[tokio::test(flavor = "multi_thread")]
async fn join_talk_leave() -> Result<()> {
    let room = start_room().await?;
    let (_ea, a, mut a_ev) = member(&room, "小明", transport::generate_secret()).await?;
    let (_eb, b, _b_ev) = member(&room, "阿華", transport::generate_secret()).await?;

    // 成員名單與加入事件
    assert_ne!(a.session_idx(), b.session_idx());
    assert_eq!(b.members().iter().map(|m| m.display_name.as_str()).collect::<Vec<_>>(), ["小明"]);
    match next_event(&mut a_ev).await {
        ClientEvent::MemberJoined(m) => assert_eq!((m.display_name.as_str(), m.session_idx), ("阿華", b.session_idx())),
        e => panic!("預期 MemberJoined，收到 {e:?}"),
    }
    assert_eq!(room.host.members().len(), 2);

    // A 說話 → B 收到；A 不會收到自己的聲音
    for seq in 0..5u32 {
        a.send_voice(Kind::Voice, 0, seq, &[seq as u8; 80]);
    }
    a.send_voice(Kind::EndOfStream, 0, 5, &[]);
    for seq in 0..6u32 {
        let d = timeout(T, b.recv_voice()).await??;
        let v = VoiceDown::parse(&d).expect("下行封包格式");
        assert_eq!((v.sender_idx, v.seq, v.via_mask), (a.session_idx(), seq, 1));
        if seq < 5 {
            assert_eq!((v.kind, v.payload), (Kind::Voice, &[seq as u8; 80][..]));
        } else {
            assert_eq!(v.kind, Kind::EndOfStream);
        }
    }
    assert!(timeout(Duration::from_millis(300), a.recv_voice()).await.is_err(), "不該收到自己的回音");

    // B 離開 → A 收到 MemberLeft
    let b_idx = b.session_idx();
    b.leave().await;
    assert_eq!(next_event(&mut a_ev).await, ClientEvent::MemberLeft(b_idx));
    assert_eq!(room.host.members().len(), 1);
    room.host_ep.close().await;
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
    let (_e1, first, _r1) = member(&room, "小明", key.clone()).await?;
    let idx = first.session_idx();
    first.leave().await;
    // 另一個人先加入，確保 idx 不是單純「下一個號碼」碰巧相同
    let (_e2, other, _r2) = member(&room, "阿華", transport::generate_secret()).await?;
    let (_e3, again, _r3) = member(&room, "小明", key).await?;
    assert_eq!(again.session_idx(), idx);
    assert_ne!(other.session_idx(), idx);
    Ok(())
}
