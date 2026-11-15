//! 房主邏輯（PLAN.md §3、§4）：加入驗證、成員管理、語音扇出。
//!
//! M1：單一頻道，所有成員互相收聽；語音原封不動轉發（不解碼、不混音）。
//! 同一個 NodeId 重新連線時沿用原本的 `session_idx`（為 M3 房主轉移鋪路）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use iroh::Endpoint;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use protocol::auth::verify_join_proof;
use protocol::control::{ClientMsg, ErrorCode, HostMsg, MemberInfo};
use protocol::ticket::{RoomId, RoomSecret};
use protocol::voice::VoiceUp;
use protocol::{NodeId, PROTOCOL_VERSION, SessionIdx};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// 加入流程（Hello → Challenge → Auth → Welcome）的逾時
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// M1 的 via_mask：唯一的預設頻道佔槽位 0
const DEFAULT_VIA_MASK: u64 = 1;
const MAX_NAME_CHARS: usize = 32;

#[derive(Clone, Debug)]
pub struct HostConfig {
    pub room_id: RoomId,
    pub secret: RoomSecret,
    pub term: u64,
    pub max_members: usize,
}

struct Member {
    info: MemberInfo,
    conn: Connection,
    ctrl: mpsc::UnboundedSender<HostMsg>,
}

#[derive(Default)]
struct State {
    members: HashMap<SessionIdx, Member>,
    /// NodeId → session_idx，整個房間生命週期內固定
    idx_of: HashMap<NodeId, SessionIdx>,
    next_idx: SessionIdx,
}

/// 轉發統計
#[derive(Default, Debug, Clone, Copy)]
pub struct HostStats {
    pub datagrams_in: u64,
    pub datagrams_out: u64,
    pub bad_datagrams: u64,
}

#[derive(Clone)]
pub struct Host {
    cfg: Arc<HostConfig>,
    state: Arc<RwLock<State>>,
    stats: Arc<Mutex<HostStats>>,
}

impl Host {
    pub fn new(cfg: HostConfig) -> Self {
        Self { cfg: Arc::new(cfg), state: Default::default(), stats: Default::default() }
    }

    pub fn members(&self) -> Vec<MemberInfo> {
        let st = self.state.read().unwrap();
        let mut v: Vec<_> = st.members.values().map(|m| m.info.clone()).collect();
        v.sort_by_key(|m| m.session_idx);
        v
    }

    pub fn stats(&self) -> HostStats {
        *self.stats.lock().unwrap()
    }

    /// 接受連線直到端點關閉
    pub async fn run(&self, ep: Endpoint) {
        while let Some(incoming) = ep.accept().await {
            let host = self.clone();
            tokio::spawn(async move {
                let conn = match incoming.await {
                    Ok(c) => c,
                    Err(e) => {
                        debug!("連線失敗：{e}");
                        return;
                    }
                };
                let peer = conn.remote_id();
                if let Err(e) = host.handle(conn).await {
                    info!("成員 {} 連線結束：{e:#}", peer.fmt_short());
                }
            });
        }
    }

    async fn handle(&self, conn: Connection) -> Result<()> {
        let (mut send, mut recv) = tokio::time::timeout(HANDSHAKE_TIMEOUT, conn.accept_bi())
            .await
            .context("等待控制串流逾時")??;
        let info = match tokio::time::timeout(HANDSHAKE_TIMEOUT, self.handshake(&conn, &mut send, &mut recv)).await {
            Ok(Ok(info)) => info,
            Ok(Err(e)) => {
                // 讓錯誤訊息送出去再關
                let _ = send.finish();
                tokio::time::sleep(Duration::from_millis(200)).await;
                conn.close(1u32.into(), b"join rejected");
                return Err(e);
            }
            Err(_) => {
                conn.close(1u32.into(), b"handshake timeout");
                bail!("加入流程逾時");
            }
        };
        let idx = info.session_idx;
        info!("成員加入：#{idx} {}", info.display_name);

        // 控制訊息寫入：所有廣播都經過這個 channel，由單一 task 依序寫入
        let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel::<HostMsg>();
        let writer = tokio::spawn(async move {
            while let Some(m) = ctrl_rx.recv().await {
                if transport::write_msg(&mut send, &m).await.is_err() {
                    break;
                }
            }
        });

        {
            let mut st = self.state.write().unwrap();
            // 同一個 NodeId 的舊連線（例如網路切換後重連）直接取代
            if let Some(old) = st.members.remove(&idx) {
                old.conn.close(2u32.into(), b"replaced by new connection");
            }
            for m in st.members.values() {
                let _ = m.ctrl.send(HostMsg::MemberJoined(info.clone()));
            }
            st.members.insert(idx, Member { info: info.clone(), conn: conn.clone(), ctrl: ctrl_tx.clone() });
        }

        let result = self.serve(&conn, idx, &mut recv, &ctrl_tx).await;

        {
            let mut st = self.state.write().unwrap();
            // 只移除「這條」連線（可能已被新連線取代）
            if st.members.get(&idx).is_some_and(|m| m.conn.stable_id() == conn.stable_id()) {
                st.members.remove(&idx);
                for m in st.members.values() {
                    let _ = m.ctrl.send(HostMsg::MemberLeft { session_idx: idx });
                }
                info!("成員離開：#{idx} {}", info.display_name);
            }
        }
        writer.abort();
        result
    }

    async fn handshake(&self, conn: &Connection, send: &mut SendStream, recv: &mut RecvStream) -> Result<MemberInfo> {
        let reject = |code, msg: &str| HostMsg::Error { code, message: msg.to_string() };

        match transport::read_msg::<ClientMsg>(recv).await? {
            ClientMsg::Hello { protocol_version, room_id } => {
                if protocol_version != PROTOCOL_VERSION {
                    transport::write_msg(send, &reject(ErrorCode::ProtocolVersion, "版本不相容，請更新 MultiVoice")).await?;
                    bail!("協定版本不符：{protocol_version}");
                }
                if room_id != self.cfg.room_id {
                    transport::write_msg(send, &reject(ErrorCode::WrongRoom, "這裡不是你要找的房間")).await?;
                    bail!("房間 ID 不符");
                }
            }
            other => bail!("預期 Hello，收到 {other:?}"),
        }

        let nonce: [u8; 32] = transport::random_bytes();
        transport::write_msg(send, &HostMsg::Challenge { nonce }).await?;

        let client = transport::node_id(&conn.remote_id());
        let display_name = match transport::read_msg::<ClientMsg>(recv).await? {
            ClientMsg::Auth { proof, display_name } => {
                if !verify_join_proof(&self.cfg.secret, &self.cfg.room_id, &nonce, &client, &proof) {
                    transport::write_msg(send, &reject(ErrorCode::AuthFailed, "邀請票無效或已失效")).await?;
                    bail!("加入證明錯誤");
                }
                sanitize_name(&display_name)
            }
            other => bail!("預期 Auth，收到 {other:?}"),
        };

        // 鎖只在這個區塊內持有，不能跨 await
        let assigned = {
            let mut st = self.state.write().unwrap();
            let existing = st.idx_of.get(&client).copied();
            if existing.is_none() && st.members.len() >= self.cfg.max_members {
                None
            } else {
                let idx = match existing {
                    Some(i) => i,
                    None => {
                        let i = st.next_idx;
                        st.next_idx = st.next_idx.checked_add(1).context("session_idx 用盡")?;
                        st.idx_of.insert(client, i);
                        i
                    }
                };
                let others: Vec<_> =
                    st.members.values().filter(|m| m.info.session_idx != idx).map(|m| m.info.clone()).collect();
                Some((idx, others))
            }
        };
        let Some((idx, members)) = assigned else {
            transport::write_msg(send, &reject(ErrorCode::RoomFull, "房間已滿")).await?;
            bail!("房間已滿");
        };

        let info = MemberInfo { session_idx: idx, node_id: client, display_name };
        transport::write_msg(send, &HostMsg::Welcome { session_idx: idx, term: self.cfg.term, members }).await?;
        Ok(info)
    }

    async fn serve(
        &self,
        conn: &Connection,
        idx: SessionIdx,
        recv: &mut RecvStream,
        ctrl: &mpsc::UnboundedSender<HostMsg>,
    ) -> Result<()> {
        let mut out = Vec::with_capacity(1500);
        loop {
            tokio::select! {
                msg = transport::read_msg::<ClientMsg>(recv) => match msg? {
                    ClientMsg::Ping { t } => { let _ = ctrl.send(HostMsg::Pong { t }); }
                    ClientMsg::Leave => {
                        conn.close(0u32.into(), b"bye");
                        return Ok(());
                    }
                    other => warn!("#{idx} 送了不該在這時送的訊息：{other:?}"),
                },
                d = conn.read_datagram() => {
                    let d = d.context("連線中斷")?;
                    self.forward(idx, &d, &mut out);
                }
            }
        }
    }

    /// 單一頻道：轉給除了說話者以外的所有人
    fn forward(&self, from: SessionIdx, datagram: &[u8], out: &mut Vec<u8>) {
        let mut stats = self.stats.lock().unwrap();
        stats.datagrams_in += 1;
        let Some(up) = VoiceUp::parse(datagram) else {
            stats.bad_datagrams += 1;
            return;
        };
        up.to_down(from, DEFAULT_VIA_MASK).encode(out);
        let pkt = Bytes::copy_from_slice(out);
        let st = self.state.read().unwrap();
        for (&i, m) in &st.members {
            if i != from && m.conn.send_datagram(pkt.clone()).is_ok() {
                stats.datagrams_out += 1;
            }
        }
    }
}

/// 顯示名稱：去掉控制字元、限制長度、不可為空
fn sanitize_name(s: &str) -> String {
    let n: String = s.chars().filter(|c| !c.is_control()).take(MAX_NAME_CHARS).collect();
    let n = n.trim().to_string();
    if n.is_empty() { "（未命名）".into() } else { n }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_sanitizing() {
        assert_eq!(sanitize_name("  小明\n\t "), "小明");
        assert_eq!(sanitize_name("\u{7}"), "（未命名）");
        assert_eq!(sanitize_name(&"長".repeat(100)).chars().count(), MAX_NAME_CHARS);
    }
}
