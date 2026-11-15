//! 成員端（PLAN.md §4.1–4.3）：加入房間、房間狀態（成員、頻道、訂閱）、語音收送。不含音訊與 UI。
//!
//! 槽位：訂閱頻道時由這裡自動分配（0..64），上行 `talk_mask` 與下行 `via_mask` 都用它。
//!
//! ## 換房主（M3，PLAN.md §5）
//! [`Client`] 可以在不重建的情況下換到新房主（[`Client::reconnect`]）：底層連線換掉，
//! 我的槽位與訂閱保留並自動重新登記，`send_voice` / `recv_voice` 的呼叫端（音訊執行緒）不用知道。
//! 要連去哪裡由上層決定（session）：
//! - 收到 [`ClientEvent::MigrateTo`]：房主要離開，指定了接手的人
//! - 收到 [`ClientEvent::HostLost`]：與房主的連線意外中斷；上層依 [`Client::successors`] 找新房主，
//!   找不到就呼叫 [`Client::give_up`]（之後才會有 [`ClientEvent::Disconnected`]）
//!
//! 計時與背景工作用 `n0_future`（原生走 tokio、瀏覽器走 wasm-bindgen-futures），網頁版（`crates/web`）也共用這個 crate。

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr};
use n0_future::time::{Duration, Instant};
use protocol::auth::join_proof;
use protocol::control::{
    ChannelInfo, ClientMsg, ErrorCode, HostAddr, HostMsg, MAX_SLOTS, MemberInfo, Subscription, Successor,
};
use protocol::ticket::{RoomId, Ticket};
use protocol::voice::{Kind, VoiceUp};
use protocol::{ALPN, ChannelId, NodeId, PROTOCOL_VERSION, SessionIdx};
use tokio::sync::{mpsc, watch};
use tracing::{debug, warn};

pub use protocol::voice::VoiceDown;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// 定期 ping：量 RTT、偵測房主失聯，並避免長時間沒說話時 QUIC 連線因閒置而關閉
const PING_INTERVAL: Duration = Duration::from_secs(1);
/// 這麼久沒收到房主任何控制訊息（含 Pong）就當成失聯（PLAN §5.3）。
/// 不能等 QUIC 自己判斷：對方當機時 iroh 會改走 relay 繼續試，要 30 秒以上才放棄
const HOST_SILENCE: Duration = Duration::from_millis(3500);

#[derive(Debug, Clone, PartialEq)]
pub enum ClientEvent {
    MemberJoined(MemberInfo),
    MemberLeft(SessionIdx),
    ChannelAdded(ChannelInfo),
    ChannelRemoved(ChannelId),
    ChannelRenamed(ChannelId, String),
    /// 有人（含自己）進出頻道或改變收聽／說話狀態
    SubscriptionChanged(Subscription),
    /// 房主拒絕了某個操作
    HostError(ErrorCode, String),
    /// 控制串流來回時間（ms）
    Rtt(f64),
    /// 房主要離開：`session_idx` 以任期 `term` 接手，其他人改連 `host`
    MigrateTo { session_idx: SessionIdx, host: HostAddr, term: u64 },
    /// 雙房主合併：目前的房主讓位，改連 `host`
    MoveTo { host: HostAddr },
    /// 與房主的連線意外中斷（上層要找新房主，或呼叫 [`Client::give_up`]）
    HostLost(String),
    /// 已連上新房主
    Reconnected { term: u64 },
    /// 連線結束，不會再恢復
    Disconnected(String),
}

/// 房間狀態（由控制訊息維護）
#[derive(Default)]
struct Room {
    members: BTreeMap<SessionIdx, MemberInfo>,
    channels: BTreeMap<ChannelId, String>,
    /// (頻道, 成員) → (listen, talk)；含自己
    subs: BTreeMap<(ChannelId, SessionIdx), (bool, bool)>,
    /// 我在各頻道使用的槽位
    my_slots: HashMap<ChannelId, u8>,
}

impl Room {
    fn from_welcome(members: Vec<MemberInfo>, channels: Vec<ChannelInfo>, subscriptions: Vec<Subscription>) -> Self {
        Room {
            members: members.into_iter().map(|m| (m.session_idx, m)).collect(),
            channels: channels.into_iter().map(|c| (c.id, c.name)).collect(),
            subs: subscriptions.into_iter().map(|s| ((s.channel, s.session_idx), (s.listen, s.talk))).collect(),
            my_slots: HashMap::new(),
        }
    }

    fn apply(&mut self, msg: &HostMsg) -> Option<ClientEvent> {
        Some(match msg.clone() {
            HostMsg::MemberJoined(m) => {
                self.members.insert(m.session_idx, m.clone());
                ClientEvent::MemberJoined(m)
            }
            HostMsg::MemberLeft { session_idx } => {
                self.members.remove(&session_idx);
                self.subs.retain(|&(_, i), _| i != session_idx);
                ClientEvent::MemberLeft(session_idx)
            }
            HostMsg::ChannelAdded(c) => {
                self.channels.insert(c.id, c.name.clone());
                ClientEvent::ChannelAdded(c)
            }
            HostMsg::ChannelRemoved { channel } => {
                self.channels.remove(&channel);
                self.subs.retain(|&(c, _), _| c != channel);
                self.my_slots.remove(&channel);
                ClientEvent::ChannelRemoved(channel)
            }
            HostMsg::ChannelRenamed { channel, name } => {
                self.channels.insert(channel, name.clone());
                ClientEvent::ChannelRenamed(channel, name)
            }
            HostMsg::SubscriptionChanged(s) => {
                if s.listen || s.talk {
                    self.subs.insert((s.channel, s.session_idx), (s.listen, s.talk));
                } else {
                    self.subs.remove(&(s.channel, s.session_idx));
                }
                ClientEvent::SubscriptionChanged(s)
            }
            HostMsg::Error { code, message } => ClientEvent::HostError(code, message),
            _ => return None,
        })
    }
}

/// 目前與房主的連線
struct Link {
    conn: Connection,
    ctrl: mpsc::UnboundedSender<ClientMsg>,
    session_idx: SessionIdx,
    term: u64,
    host: EndpointAddr,
    /// 第幾條連線（每次換房主 +1）
    generation: u64,
}

struct Inner {
    ep: Endpoint,
    ticket: Ticket,
    name: String,
    link: Mutex<Link>,
    room: Mutex<Room>,
    ev_tx: mpsc::UnboundedSender<ClientEvent>,
    /// 連線代數；換連線或放棄時更新，`recv_voice` 等它
    generation: watch::Sender<u64>,
    successors: Mutex<Vec<Successor>>,
    /// 見過的所有候補備用端點（累積；雙房主合併時要探測，名單更新後也不能忘）
    seen_standbys: Mutex<Vec<HostAddr>>,
    standby: Mutex<Option<HostAddr>>,
    /// 收到 MigrateTo：舊連線結束時不要當成「房主失聯」
    migrating: AtomicBool,
    /// 自己離開或已放棄
    dead: AtomicBool,
    started: Instant,
}

#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

/// 握手結果
enum Handshake {
    Joined { conn: Connection, send: SendStream, recv: RecvStream, session_idx: SessionIdx, term: u64, room: Room },
    Redirect(NodeId),
}

impl Client {
    /// 依邀請票加入：依序撥號 bootstrap，遇到 Redirect 就改連它指示的房主
    pub async fn join(ep: &Endpoint, ticket: &Ticket, name: &str) -> Result<(Client, mpsc::UnboundedReceiver<ClientEvent>)> {
        let mut errors = Vec::new();
        let mut queue: Vec<NodeId> = ticket.bootstrap.clone();
        let mut tried = Vec::new();
        while let Some(node) = queue.first().copied() {
            queue.remove(0);
            if tried.contains(&node) {
                continue;
            }
            tried.push(node);
            let addr = EndpointAddr::from(transport::endpoint_id(&node)?);
            match timed_handshake(ep, addr.clone(), ticket, name).await {
                Ok(Handshake::Redirect(host)) => queue.insert(0, host),
                Ok(hs) => return Ok(Self::start(ep, ticket, name, addr, hs)),
                Err(e) => errors.push(format!("{e:#}")),
            }
        }
        bail!("無法加入房間：{}", errors.join("；"))
    }

    /// 直接連到指定位址（測試、或已知房主位址時）
    pub async fn join_addr(
        ep: &Endpoint,
        addr: EndpointAddr,
        ticket: &Ticket,
        name: &str,
    ) -> Result<(Client, mpsc::UnboundedReceiver<ClientEvent>)> {
        match timed_handshake(ep, addr.clone(), ticket, name).await? {
            Handshake::Redirect(_) => bail!("對方不是房主"),
            hs => Ok(Self::start(ep, ticket, name, addr, hs)),
        }
    }

    fn start(
        ep: &Endpoint,
        ticket: &Ticket,
        name: &str,
        addr: EndpointAddr,
        hs: Handshake,
    ) -> (Client, mpsc::UnboundedReceiver<ClientEvent>) {
        let Handshake::Joined { conn, send, recv, session_idx, term, room } = hs else { unreachable!() };
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let (ctrl, ctrl_rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            ep: ep.clone(),
            ticket: ticket.clone(),
            name: name.to_string(),
            link: Mutex::new(Link { conn: conn.clone(), ctrl: ctrl.clone(), session_idx, term, host: addr, generation: 0 }),
            room: Mutex::new(room),
            ev_tx,
            generation: watch::channel(0).0,
            successors: Mutex::new(Vec::new()),
            seen_standbys: Mutex::new(Vec::new()),
            standby: Mutex::new(None),
            migrating: AtomicBool::new(false),
            dead: AtomicBool::new(false),
            started: Instant::now(),
        });
        spawn_link_tasks(&inner, conn, send, recv, ctrl, ctrl_rx, 0);
        (Client { inner }, ev_rx)
    }

    /// 換到新房主（M3）。成功後我的槽位與訂閱、備用端點會自動重新登記。
    /// 遇到 Redirect 會跟過去（最多 3 次）
    pub async fn reconnect(&self, addr: EndpointAddr) -> Result<()> {
        let inner = &self.inner;
        if inner.dead.load(Ordering::SeqCst) {
            bail!("已離開房間");
        }
        let mut addr = addr;
        let mut hops = 0;
        let hs = loop {
            match timed_handshake(&inner.ep, addr.clone(), &inner.ticket, &inner.name).await? {
                Handshake::Redirect(host) if hops < 3 => {
                    hops += 1;
                    addr = EndpointAddr::from(transport::endpoint_id(&host)?);
                }
                Handshake::Redirect(_) => bail!("被轉址太多次"),
                hs => break hs,
            }
        };
        let Handshake::Joined { conn, send, recv, session_idx, term, room } = hs else { unreachable!() };

        // 保留我的槽位與訂閱狀態，換掉其他房間狀態（以新房主為準）
        let resubscribe: Vec<(ChannelId, u8, bool, bool)> = {
            let mut r = inner.room.lock().unwrap();
            let old_idx = inner.link.lock().unwrap().session_idx;
            let mine: Vec<(ChannelId, u8, bool, bool)> = r
                .my_slots
                .iter()
                .filter(|(ch, _)| room.channels.contains_key(ch))
                .map(|(&ch, &slot)| {
                    let (l, t) = r.subs.get(&(ch, old_idx)).copied().unwrap_or((true, true));
                    (ch, slot, l, t)
                })
                .collect();
            let mut room = room;
            room.my_slots = mine.iter().map(|&(ch, slot, ..)| (ch, slot)).collect();
            *r = room;
            mine
        };
        let (ctrl, ctrl_rx) = mpsc::unbounded_channel();
        let generation = {
            let mut l = inner.link.lock().unwrap();
            let generation = l.generation + 1;
            let old = std::mem::replace(
                &mut *l,
                Link { conn: conn.clone(), ctrl: ctrl.clone(), session_idx, term, host: addr, generation },
            );
            old.conn.close(0u32.into(), b"host changed");
            generation
        };
        inner.migrating.store(false, Ordering::SeqCst);
        spawn_link_tasks(inner, conn, send, recv, ctrl.clone(), ctrl_rx, generation);
        for (channel, slot, listen, talk) in resubscribe {
            let _ = ctrl.send(ClientMsg::Subscribe { channel, slot, listen, talk });
        }
        if let Some(sb) = inner.standby.lock().unwrap().clone() {
            let _ = ctrl.send(ClientMsg::Standby(sb));
        }
        inner.generation.send_replace(generation);
        let _ = inner.ev_tx.send(ClientEvent::Reconnected { term });
        Ok(())
    }

    /// 找不到新房主：結束連線（發出 [`ClientEvent::Disconnected`]）
    pub fn give_up(&self, reason: &str) {
        if !self.inner.dead.swap(true, Ordering::SeqCst) {
            self.inner.generation.send_modify(|g| *g += 1);
            let _ = self.inner.ev_tx.send(ClientEvent::Disconnected(reason.to_string()));
        }
    }

    pub fn session_idx(&self) -> SessionIdx {
        self.inner.link.lock().unwrap().session_idx
    }

    pub fn term(&self) -> u64 {
        self.inner.link.lock().unwrap().term
    }

    /// 我的端點（探測其他房主時用）
    pub fn endpoint(&self) -> &Endpoint {
        &self.inner.ep
    }

    /// 我的 NodeId（成員身分）
    pub fn node_id(&self) -> NodeId {
        transport::node_id(&self.inner.ep.id())
    }

    /// 目前房主的端點 ID
    pub fn host_id(&self) -> NodeId {
        transport::node_id(&self.inner.link.lock().unwrap().host.id)
    }

    /// 目前房主的位址（撥號用）
    pub fn host_addr(&self) -> EndpointAddr {
        self.inner.link.lock().unwrap().host.clone()
    }

    pub fn ticket(&self) -> &Ticket {
        &self.inner.ticket
    }

    /// 房主候補名單（依接手順序）
    pub fn successors(&self) -> Vec<Successor> {
        self.inner.successors.lock().unwrap().clone()
    }

    /// 見過的所有候補備用端點（累積，包含已經不在名單上的）
    pub fn seen_standbys(&self) -> Vec<HostAddr> {
        self.inner.seen_standbys.lock().unwrap().clone()
    }

    /// 告訴房主「我可以當房主」：這是我預先建好的備用房主端點。換房主後會自動重送
    pub fn announce_standby(&self, addr: HostAddr) {
        *self.inner.standby.lock().unwrap() = Some(addr.clone());
        let _ = self.inner.link.lock().unwrap().ctrl.send(ClientMsg::Standby(addr));
    }

    /// 其他成員（不含自己）
    pub fn members(&self) -> Vec<MemberInfo> {
        self.inner.room.lock().unwrap().members.values().cloned().collect()
    }

    pub fn member_name(&self, idx: SessionIdx) -> Option<String> {
        self.inner.room.lock().unwrap().members.get(&idx).map(|m| m.display_name.clone())
    }

    pub fn channels(&self) -> Vec<ChannelInfo> {
        self.inner.room.lock().unwrap().channels.iter().map(|(&id, n)| ChannelInfo { id, name: n.clone() }).collect()
    }

    /// 所有人（含自己）的訂閱，依頻道、成員排序
    pub fn subscriptions(&self) -> Vec<Subscription> {
        self.inner
            .room
            .lock()
            .unwrap()
            .subs
            .iter()
            .map(|(&(channel, session_idx), &(listen, talk))| Subscription { session_idx, channel, listen, talk })
            .collect()
    }

    /// 我在各頻道使用的槽位
    pub fn my_slots(&self) -> HashMap<ChannelId, u8> {
        self.inner.room.lock().unwrap().my_slots.clone()
    }

    fn send_ctrl(&self, m: ClientMsg) -> bool {
        self.inner.link.lock().unwrap().ctrl.send(m).is_ok()
    }

    /// 加入或更新頻道訂閱；第一次訂閱時自動分配槽位
    pub fn subscribe(&self, channel: ChannelId, listen: bool, talk: bool) -> Result<u8> {
        if !listen && !talk {
            self.unsubscribe(channel);
            bail!("listen 與 talk 都是 false，等同取消訂閱");
        }
        let slot = {
            let mut room = self.inner.room.lock().unwrap();
            if !room.channels.contains_key(&channel) {
                bail!("頻道不存在");
            }
            match room.my_slots.get(&channel) {
                Some(&s) => s,
                None => {
                    let used: Vec<u8> = room.my_slots.values().copied().collect();
                    let s = (0..MAX_SLOTS).find(|s| !used.contains(s)).ok_or_else(|| anyhow!("最多同時訂閱 {MAX_SLOTS} 個頻道"))?;
                    room.my_slots.insert(channel, s);
                    s
                }
            }
        };
        if !self.send_ctrl(ClientMsg::Subscribe { channel, slot, listen, talk }) {
            bail!("連線已中斷");
        }
        Ok(slot)
    }

    pub fn unsubscribe(&self, channel: ChannelId) {
        if self.inner.room.lock().unwrap().my_slots.remove(&channel).is_some() {
            self.send_ctrl(ClientMsg::Unsubscribe { channel });
        }
    }

    pub fn create_channel(&self, name: &str) {
        self.send_ctrl(ClientMsg::CreateChannel { name: name.to_string() });
    }

    pub fn rename_channel(&self, channel: ChannelId, name: &str) {
        self.send_ctrl(ClientMsg::RenameChannel { channel, name: name.to_string() });
    }

    pub fn delete_channel(&self, channel: ChannelId) {
        self.send_ctrl(ClientMsg::DeleteChannel { channel });
    }

    /// 送出一幀語音（或 end-of-stream）。`talk_mask` 用自己的槽位。datagram 不可靠，送不出去就丟掉。
    pub fn send_voice(&self, kind: Kind, talk_mask: u64, seq: u32, payload: &[u8]) {
        let mut buf = Vec::with_capacity(protocol::voice::UP_HEADER + payload.len());
        VoiceUp { kind, talk_mask, seq, payload }.encode(&mut buf);
        let conn = self.inner.link.lock().unwrap().conn.clone();
        if let Err(e) = conn.send_datagram(Bytes::from(buf)) {
            debug!("語音送出失敗：{e}");
        }
    }

    /// 等下一個收到的語音 datagram（原始位元組，用 [`VoiceDown::parse`] 解析）。
    /// 換房主期間會等新連線；只有離開或放棄後才回傳錯誤
    pub async fn recv_voice(&self) -> Result<Bytes> {
        let inner = &self.inner;
        loop {
            if inner.dead.load(Ordering::SeqCst) {
                bail!("已離開房間");
            }
            let mut gen_rx = inner.generation.subscribe();
            let (conn, generation) = {
                let l = inner.link.lock().unwrap();
                (l.conn.clone(), l.generation)
            };
            match conn.read_datagram().await {
                Ok(b) => return Ok(b),
                Err(_) => {
                    // 等換上新連線（或放棄）
                    while *gen_rx.borrow_and_update() == generation && !inner.dead.load(Ordering::SeqCst) {
                        if gen_rx.changed().await.is_err() {
                            bail!("連線中斷");
                        }
                    }
                }
            }
        }
    }

    /// 目前選用的路徑：(是否經 relay, QUIC 估計的 RTT ms)
    pub fn path(&self) -> Option<(bool, f64)> {
        let conn = self.inner.link.lock().unwrap().conn.clone();
        conn.paths().iter().find(|p| p.is_selected()).map(|p| (p.is_relay(), p.rtt().as_secs_f64() * 1000.0))
    }

    pub async fn leave(&self) {
        let inner = &self.inner;
        inner.dead.store(true, Ordering::SeqCst);
        let conn = {
            let l = inner.link.lock().unwrap();
            let _ = l.ctrl.send(ClientMsg::Leave);
            l.conn.clone()
        };
        inner.generation.send_modify(|g| *g += 1);
        n0_future::time::sleep(Duration::from_millis(100)).await;
        conn.close(0u32.into(), b"bye");
    }
}

/// 探測某個端點是不是這個房間的房主（雙房主合併用）。是的話回傳 (任期, 連線人數)
pub async fn probe_host(ep: &Endpoint, addr: EndpointAddr, room_id: RoomId) -> Result<(u64, u32)> {
    n0_future::time::timeout(Duration::from_secs(2), async {
        let conn = ep.connect(addr, ALPN).await.context("無法連線")?;
        let (mut send, mut recv) = conn.open_bi().await?;
        transport::write_msg(&mut send, &ClientMsg::HostProbe { room_id }).await?;
        let r = match transport::read_msg::<HostMsg>(&mut recv).await? {
            HostMsg::HostInfo { term, members } => Ok((term, members)),
            other => Err(anyhow!("不是房主：{other:?}")),
        };
        conn.close(0u32.into(), b"probe done");
        r
    })
    .await
    .context("探測逾時")?
}

async fn timed_handshake(ep: &Endpoint, addr: EndpointAddr, ticket: &Ticket, name: &str) -> Result<Handshake> {
    n0_future::time::timeout(HANDSHAKE_TIMEOUT, handshake(ep, addr, ticket, name)).await.context("加入逾時")?
}

async fn handshake(ep: &Endpoint, addr: EndpointAddr, ticket: &Ticket, name: &str) -> Result<Handshake> {
    let conn = ep.connect(addr, ALPN).await.context("無法連線")?;
    let (mut send, mut recv) = conn.open_bi().await.context("無法開啟控制串流")?;
    transport::write_msg(&mut send, &ClientMsg::Hello { protocol_version: PROTOCOL_VERSION, room_id: ticket.room_id })
        .await?;

    let nonce = match transport::read_msg::<HostMsg>(&mut recv).await? {
        HostMsg::Challenge { nonce } => nonce,
        HostMsg::Redirect { host, .. } => {
            conn.close(0u32.into(), b"redirected");
            return Ok(Handshake::Redirect(host));
        }
        HostMsg::Error { code, message } => bail!("房主拒絕（{code:?}）：{message}"),
        other => bail!("預期 Challenge，收到 {other:?}"),
    };
    let me = transport::node_id(&ep.id());
    let proof = join_proof(&ticket.secret, &ticket.room_id, &nonce, &me);
    transport::write_msg(&mut send, &ClientMsg::Auth { proof, display_name: name.to_string() }).await?;

    match transport::read_msg::<HostMsg>(&mut recv).await? {
        HostMsg::Welcome { session_idx, term, members, channels, subscriptions } => Ok(Handshake::Joined {
            conn,
            send,
            recv,
            session_idx,
            term,
            room: Room::from_welcome(members, channels, subscriptions),
        }),
        HostMsg::Error { code, message } => bail!("房主拒絕（{code:?}）：{message}"),
        other => bail!("預期 Welcome，收到 {other:?}"),
    }
}

/// 一條連線的寫入、讀取、ping task
fn spawn_link_tasks(
    inner: &Arc<Inner>,
    conn: Connection,
    mut send: SendStream,
    mut recv: RecvStream,
    ctrl: mpsc::UnboundedSender<ClientMsg>,
    mut ctrl_rx: mpsc::UnboundedReceiver<ClientMsg>,
    generation: u64,
) {
    // 寫入
    n0_future::task::spawn(async move {
        while let Some(m) = ctrl_rx.recv().await {
            if transport::write_msg(&mut send, &m).await.is_err() {
                break;
            }
        }
    });

    // 最後一次收到房主控制訊息的時間（相對 inner.started，ms）
    let last_rx = Arc::new(std::sync::atomic::AtomicU64::new(inner.started.elapsed().as_millis() as u64));

    // 讀取：房間狀態變動、Pong、候補名單、轉移
    {
        let inner = inner.clone();
        let conn = conn.clone();
        let last_rx = last_rx.clone();
        n0_future::task::spawn(async move {
            let reason = loop {
                let msg = transport::read_msg::<HostMsg>(&mut recv).await;
                last_rx.store(inner.started.elapsed().as_millis() as u64, Ordering::Relaxed);
                match msg {
                    Ok(HostMsg::Pong { t }) => {
                        let rtt = inner.started.elapsed().as_micros() as f64 / 1000.0 - t as f64 / 1000.0;
                        let _ = inner.ev_tx.send(ClientEvent::Rtt(rtt));
                    }
                    Ok(HostMsg::Successors(list)) => {
                        let mut seen = inner.seen_standbys.lock().unwrap();
                        for s in &list {
                            match seen.iter_mut().find(|h| h.id == s.host.id) {
                                Some(h) => *h = s.host.clone(),
                                None => seen.push(s.host.clone()),
                            }
                        }
                        drop(seen);
                        *inner.successors.lock().unwrap() = list;
                    }
                    Ok(HostMsg::MigrateTo { session_idx, host, term }) => {
                        inner.migrating.store(true, Ordering::SeqCst);
                        let _ = inner.ev_tx.send(ClientEvent::MigrateTo { session_idx, host, term });
                    }
                    Ok(HostMsg::MoveTo { host }) => {
                        inner.migrating.store(true, Ordering::SeqCst);
                        let _ = inner.ev_tx.send(ClientEvent::MoveTo { host });
                    }
                    Ok(msg) => {
                        let ev = inner.room.lock().unwrap().apply(&msg);
                        match ev {
                            Some(ev) => {
                                let _ = inner.ev_tx.send(ev);
                            }
                            None => debug!("忽略控制訊息：{msg:?}"),
                        }
                    }
                    Err(e) => break format!("{e:#}"),
                }
            };
            // 已經換成新連線、自己離開、或正在照計畫轉移：不算失聯
            let current = inner.link.lock().unwrap().generation == generation;
            if current && !inner.dead.load(Ordering::SeqCst) && !inner.migrating.load(Ordering::SeqCst) {
                let reason = conn.close_reason().map(|r| r.to_string()).unwrap_or(reason);
                warn!("與房主的連線中斷：{reason}");
                let _ = inner.ev_tx.send(ClientEvent::HostLost(reason));
            }
        });
    }

    // ping + 失聯偵測
    {
        let inner = inner.clone();
        n0_future::task::spawn(async move {
            let mut tick = n0_future::time::interval(PING_INTERVAL);
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        let now = inner.started.elapsed();
                        let silent = now.saturating_sub(Duration::from_millis(last_rx.load(Ordering::Relaxed)));
                        if silent > HOST_SILENCE {
                            // 關掉連線，讀取 task 就會結束並回報 HostLost
                            conn.close(3u32.into(), b"host silent");
                            break;
                        }
                        if ctrl.send(ClientMsg::Ping { t: now.as_micros() as u64 }).is_err() { break; }
                    }
                    _ = conn.closed() => break,
                }
            }
        });
    }
}
