//! 成員端（PLAN.md §4.1–4.3）：加入房間、控制事件、語音收送。不含音訊與 UI。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr};
use protocol::auth::join_proof;
use protocol::control::{ClientMsg, HostMsg, MemberInfo};
use protocol::ticket::Ticket;
use protocol::voice::{Kind, VoiceUp};
use protocol::{ALPN, NodeId, PROTOCOL_VERSION, SessionIdx};
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
    /// 控制串流來回時間（ms）
    Rtt(f64),
    Disconnected(String),
}

#[derive(Clone)]
pub struct Client {
    conn: Connection,
    session_idx: SessionIdx,
    term: u64,
    members: Arc<Mutex<BTreeMap<SessionIdx, MemberInfo>>>,
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

        let (session_idx, term, members) = match transport::read_msg::<HostMsg>(&mut recv).await? {
            HostMsg::Welcome { session_idx, term, members } => (session_idx, term, members),
            HostMsg::Error { code, message } => bail!("房主拒絕（{code:?}）：{message}"),
            other => bail!("預期 Welcome，收到 {other:?}"),
        };

        let members = Arc::new(Mutex::new(members.into_iter().map(|m| (m.session_idx, m)).collect::<BTreeMap<_, _>>()));
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

        // 讀取 task：成員變動、Pong
        let started = Instant::now();
        {
            let members = members.clone();
            let ev_tx = ev_tx.clone();
            let conn = conn.clone();
            tokio::spawn(async move {
                let reason = loop {
                    match transport::read_msg::<HostMsg>(&mut recv).await {
                        Ok(HostMsg::MemberJoined(m)) => {
                            members.lock().unwrap().insert(m.session_idx, m.clone());
                            let _ = ev_tx.send(ClientEvent::MemberJoined(m));
                        }
                        Ok(HostMsg::MemberLeft { session_idx }) => {
                            members.lock().unwrap().remove(&session_idx);
                            let _ = ev_tx.send(ClientEvent::MemberLeft(session_idx));
                        }
                        Ok(HostMsg::Pong { t }) => {
                            let rtt = started.elapsed().as_micros() as f64 / 1000.0 - t as f64 / 1000.0;
                            let _ = ev_tx.send(ClientEvent::Rtt(rtt));
                        }
                        Ok(other) => debug!("忽略控制訊息：{other:?}"),
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

        Ok(Outcome::Joined(Client { conn, session_idx, term, members, ctrl: ctrl_tx }, ev_rx))
    }

    pub fn session_idx(&self) -> SessionIdx {
        self.session_idx
    }

    pub fn term(&self) -> u64 {
        self.term
    }

    /// 其他成員（不含自己）
    pub fn members(&self) -> Vec<MemberInfo> {
        self.members.lock().unwrap().values().cloned().collect()
    }

    pub fn member_name(&self, idx: SessionIdx) -> Option<String> {
        self.members.lock().unwrap().get(&idx).map(|m| m.display_name.clone())
    }

    /// 送出一幀語音（或 end-of-stream）。datagram 不可靠，送不出去就丟掉。
    pub fn send_voice(&self, kind: Kind, target_id: u8, seq: u32, payload: &[u8]) {
        let mut buf = Vec::with_capacity(protocol::voice::UP_HEADER + payload.len());
        VoiceUp { kind, target_id, seq, payload }.encode(&mut buf);
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
