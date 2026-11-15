//! 成員端（PLAN.md §4.1–4.3）：加入房間、房間狀態（成員、頻道、訂閱）、語音收送。不含音訊與 UI。
//!
//! 槽位：訂閱頻道時由這裡自動分配（0..64），上行 `talk_mask` 與下行 `via_mask` 都用它。

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr};
use protocol::auth::join_proof;
use protocol::control::{ChannelInfo, ClientMsg, ErrorCode, HostMsg, MAX_SLOTS, MemberInfo, Subscription};
use protocol::ticket::Ticket;
use protocol::voice::{Kind, VoiceUp};
use protocol::{ALPN, ChannelId, NodeId, PROTOCOL_VERSION, SessionIdx};
use tokio::sync::mpsc;
use tracing::{debug, warn};

pub use protocol::voice::VoiceDown;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// 定期 ping：量 RTT，並避免長時間沒說話時 QUIC 連線因閒置而關閉
const PING_INTERVAL: Duration = Duration::from_secs(5);

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

#[derive(Clone)]
pub struct Client {
    conn: Connection,
    session_idx: SessionIdx,
    term: u64,
    room: Arc<Mutex<Room>>,
    ctrl: mpsc::UnboundedSender<ClientMsg>,
}

enum Outcome {
    Joined(Client, mpsc::UnboundedReceiver<ClientEvent>),
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
            match Self::join_addr_inner(ep, addr, ticket, name).await {
                Ok(Outcome::Joined(c, rx)) => return Ok((c, rx)),
                Ok(Outcome::Redirect(host)) => queue.insert(0, host),
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
        match Self::join_addr_inner(ep, addr, ticket, name).await? {
            Outcome::Joined(c, rx) => Ok((c, rx)),
            Outcome::Redirect(_) => bail!("對方不是房主"),
        }
    }

    async fn join_addr_inner(ep: &Endpoint, addr: EndpointAddr, ticket: &Ticket, name: &str) -> Result<Outcome> {
        tokio::time::timeout(HANDSHAKE_TIMEOUT, Self::handshake(ep, addr, ticket, name))
            .await
            .context("加入逾時")?
    }

    async fn handshake(ep: &Endpoint, addr: EndpointAddr, ticket: &Ticket, name: &str) -> Result<Outcome> {
        let conn = ep.connect(addr, ALPN).await.context("無法連線")?;
        let (mut send, mut recv) = conn.open_bi().await.context("無法開啟控制串流")?;
        transport::write_msg(&mut send, &ClientMsg::Hello { protocol_version: PROTOCOL_VERSION, room_id: ticket.room_id })
            .await?;

        let nonce = match transport::read_msg::<HostMsg>(&mut recv).await? {
            HostMsg::Challenge { nonce } => nonce,
            HostMsg::Redirect { host, .. } => {
                conn.close(0u32.into(), b"redirected");
                return Ok(Outcome::Redirect(host));
            }
            HostMsg::Error { code, message } => bail!("房主拒絕（{code:?}）：{message}"),
            other => bail!("預期 Challenge，收到 {other:?}"),
        };
        let me = transport::node_id(&ep.id());
        let proof = join_proof(&ticket.secret, &ticket.room_id, &nonce, &me);
        transport::write_msg(&mut send, &ClientMsg::Auth { proof, display_name: name.to_string() }).await?;

        let room = match transport::read_msg::<HostMsg>(&mut recv).await? {
            HostMsg::Welcome { session_idx, term, members, channels, subscriptions } => {
                let room = Room {
                    members: members.into_iter().map(|m| (m.session_idx, m)).collect(),
                    channels: channels.into_iter().map(|c| (c.id, c.name)).collect(),
                    subs: subscriptions.into_iter().map(|s| ((s.channel, s.session_idx), (s.listen, s.talk))).collect(),
                    my_slots: HashMap::new(),
                };
                (session_idx, term, room)
            }
            HostMsg::Error { code, message } => bail!("房主拒絕（{code:?}）：{message}"),
            other => bail!("預期 Welcome，收到 {other:?}"),
        };
        let (session_idx, term, room) = room;
        let room = Arc::new(Mutex::new(room));
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel::<ClientMsg>();

        // 寫入 task
        tokio::spawn(async move {
            while let Some(m) = ctrl_rx.recv().await {
                if transport::write_msg(&mut send, &m).await.is_err() {
                    break;
                }
            }
        });

        // 讀取 task：房間狀態變動、Pong
        let started = Instant::now();
        {
            let room = room.clone();
            let ev_tx = ev_tx.clone();
            let conn = conn.clone();
            tokio::spawn(async move {
                let reason = loop {
                    match transport::read_msg::<HostMsg>(&mut recv).await {
                        Ok(HostMsg::Pong { t }) => {
                            let rtt = started.elapsed().as_micros() as f64 / 1000.0 - t as f64 / 1000.0;
                            let _ = ev_tx.send(ClientEvent::Rtt(rtt));
                        }
                        Ok(msg) => {
                            let ev = room.lock().unwrap().apply(&msg);
                            match ev {
                                Some(ev) => {
                                    let _ = ev_tx.send(ev);
                                }
                                None => debug!("忽略控制訊息：{msg:?}"),
                            }
                        }
                        Err(e) => break format!("{e:#}"),
                    }
                };
                let reason = conn.close_reason().map(|r| r.to_string()).unwrap_or(reason);
                let _ = ev_tx.send(ClientEvent::Disconnected(reason));
            });
        }

        // ping task
        {
            let ctrl = ctrl_tx.clone();
            let conn = conn.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(PING_INTERVAL);
                loop {
                    tokio::select! {
                        _ = tick.tick() => {
                            let t = started.elapsed().as_micros() as u64;
                            if ctrl.send(ClientMsg::Ping { t }).is_err() { break; }
                        }
                        _ = conn.closed() => break,
                    }
                }
            });
        }

        Ok(Outcome::Joined(Client { conn, session_idx, term, room, ctrl: ctrl_tx }, ev_rx))
    }

    pub fn session_idx(&self) -> SessionIdx {
        self.session_idx
    }

    pub fn term(&self) -> u64 {
        self.term
    }

    /// 其他成員（不含自己）
    pub fn members(&self) -> Vec<MemberInfo> {
        self.room.lock().unwrap().members.values().cloned().collect()
    }

    pub fn member_name(&self, idx: SessionIdx) -> Option<String> {
        self.room.lock().unwrap().members.get(&idx).map(|m| m.display_name.clone())
    }

    pub fn channels(&self) -> Vec<ChannelInfo> {
        self.room.lock().unwrap().channels.iter().map(|(&id, n)| ChannelInfo { id, name: n.clone() }).collect()
    }

    /// 所有人（含自己）的訂閱，依頻道、成員排序
    pub fn subscriptions(&self) -> Vec<Subscription> {
        self.room
            .lock()
            .unwrap()
            .subs
            .iter()
            .map(|(&(channel, session_idx), &(listen, talk))| Subscription { session_idx, channel, listen, talk })
            .collect()
    }

    /// 我在各頻道使用的槽位
    pub fn my_slots(&self) -> HashMap<ChannelId, u8> {
        self.room.lock().unwrap().my_slots.clone()
    }

    /// 加入或更新頻道訂閱；第一次訂閱時自動分配槽位
    pub fn subscribe(&self, channel: ChannelId, listen: bool, talk: bool) -> Result<u8> {
        if !listen && !talk {
            self.unsubscribe(channel);
            bail!("listen 與 talk 都是 false，等同取消訂閱");
        }
        let slot = {
            let mut room = self.room.lock().unwrap();
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
        self.ctrl.send(ClientMsg::Subscribe { channel, slot, listen, talk }).map_err(|_| anyhow!("連線已中斷"))?;
        Ok(slot)
    }

    pub fn unsubscribe(&self, channel: ChannelId) {
        if self.room.lock().unwrap().my_slots.remove(&channel).is_some() {
            let _ = self.ctrl.send(ClientMsg::Unsubscribe { channel });
        }
    }

    pub fn create_channel(&self, name: &str) {
        let _ = self.ctrl.send(ClientMsg::CreateChannel { name: name.to_string() });
    }

    pub fn rename_channel(&self, channel: ChannelId, name: &str) {
        let _ = self.ctrl.send(ClientMsg::RenameChannel { channel, name: name.to_string() });
    }

    pub fn delete_channel(&self, channel: ChannelId) {
        let _ = self.ctrl.send(ClientMsg::DeleteChannel { channel });
    }

    /// 送出一幀語音（或 end-of-stream）。`talk_mask` 用自己的槽位。datagram 不可靠，送不出去就丟掉。
    pub fn send_voice(&self, kind: Kind, talk_mask: u64, seq: u32, payload: &[u8]) {
        let mut buf = Vec::with_capacity(protocol::voice::UP_HEADER + payload.len());
        VoiceUp { kind, talk_mask, seq, payload }.encode(&mut buf);
        if let Err(e) = self.conn.send_datagram(Bytes::from(buf)) {
            warn!("語音送出失敗：{e}");
        }
    }

    /// 等下一個收到的語音 datagram（原始位元組，用 [`VoiceDown::parse`] 解析）
    pub async fn recv_voice(&self) -> Result<Bytes> {
        self.conn.read_datagram().await.map_err(|e| anyhow!("連線中斷：{e}"))
    }

    /// 目前選用的路徑：(是否經 relay, QUIC 估計的 RTT ms)
    pub fn path(&self) -> Option<(bool, f64)> {
        self.conn
            .paths()
            .iter()
            .find(|p| p.is_selected())
            .map(|p| (p.is_relay(), p.rtt().as_secs_f64() * 1000.0))
    }

    pub async fn leave(&self) {
        let _ = self.ctrl.send(ClientMsg::Leave);
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.conn.close(0u32.into(), b"bye");
    }
}
