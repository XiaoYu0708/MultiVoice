//! 房主邏輯（PLAN.md §3、§4）：加入驗證、成員與頻道管理、語音扇出。
//!
//! 語音原封不動轉發（不解碼、不混音）：
//! 1. 依說話者的 `talk_mask`（說話者自己的槽位）找出他要說話的頻道；只算他 `talk = true` 的訂閱
//! 2. 這些頻道裡 `listen = true` 的成員都要收到；同一人在多個頻道也**只送一份**，
//!    `via_mask` 用接收者自己的槽位標出經由哪些頻道
//! 3. 上一幀還聽得到、這一幀不再聽得到的人（說話者換了頻道組合、或停止說話），補送 end-of-stream
//!
//! 同一個 NodeId 重新連線時沿用原本的 `session_idx`（為 M3 房主轉移鋪路）。
//!
//! 權限：設定了 [`HostConfig::owner`] 時，只有房主本人（以 NodeId 判斷）能改名／刪除頻道；
//! 建立頻道所有人都可以。
//!
//! 房主轉移（M3，PLAN.md §5）：
//! - 成員用 `Standby` 告知自己的備用房主端點；房主依 `session_idx`（越早進房越前面）排出候補名單，
//!   有變動就廣播 `Successors`（房主自己不在名單裡）
//! - [`Host::migrate`]：計畫性轉移，廣播 `MigrateTo` 給第一候補接手
//! - 新房主用 [`HostConfig::seed`]（自己客戶端看到的頻道與成員）建立初始狀態，成員重連後 `session_idx` 不變
//! - 雙房主合併：回應 `HostProbe`（任期、人數）；讓位時 [`Host::yield_to`] 廣播 `MoveTo` 後 [`Host::stop`]

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use iroh::Endpoint;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use protocol::auth::verify_join_proof;
use protocol::control::{
    ChannelInfo, ClientMsg, ErrorCode, HostAddr, HostMsg, MAX_CHANNEL_NAME_CHARS, MAX_CHANNELS, MAX_SLOTS,
    MemberInfo, Subscription, Successor,
};
use protocol::ticket::{RoomId, RoomSecret};
use protocol::voice::{Kind, VoiceDown, VoiceUp};
use protocol::{ChannelId, DEFAULT_CHANNEL, DEFAULT_CHANNEL_NAME, NodeId, PROTOCOL_VERSION, SessionIdx};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// 加入流程（Hello → Challenge → Auth → Welcome）的逾時
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_NAME_CHARS: usize = 32;

#[derive(Clone, Debug)]
pub struct HostConfig {
    pub room_id: RoomId,
    pub secret: RoomSecret,
    pub term: u64,
    pub max_members: usize,
    /// 房主自己的成員身分（NodeId）。Some 時只有他能改名／刪除頻道；None = 不限制（測試用）
    pub owner: Option<NodeId>,
    /// 接手時的初始狀態（M3）；None = 新房間
    pub seed: Option<Seed>,
}

/// 新房主接手時的初始狀態：頻道（含 ID）與成員的 NodeId → session_idx
#[derive(Clone, Debug, Default)]
pub struct Seed {
    pub channels: Vec<ChannelInfo>,
    pub members: Vec<(NodeId, SessionIdx)>,
}

#[derive(Clone, Copy, Debug)]
struct Sub {
    slot: u8,
    listen: bool,
    talk: bool,
}

struct Member {
    info: MemberInfo,
    conn: Connection,
    ctrl: mpsc::UnboundedSender<HostMsg>,
    subs: HashMap<ChannelId, Sub>,
    /// 上一幀語音送給了誰（用來對「不再聽得到」的人補送 end-of-stream）
    last_recipients: Mutex<Vec<SessionIdx>>,
    /// 備用房主端點（M3）
    standby: Option<HostAddr>,
}

struct State {
    members: HashMap<SessionIdx, Member>,
    /// NodeId → session_idx，整個房間生命週期內固定
    idx_of: HashMap<NodeId, SessionIdx>,
    next_idx: SessionIdx,
    channels: BTreeMap<ChannelId, String>,
    next_channel: ChannelId,
    /// 最近一次廣播的候補名單
    successors: Vec<Successor>,
}

impl State {
    fn new(seed: Option<&Seed>) -> Self {
        let mut st = Self {
            members: HashMap::new(),
            idx_of: HashMap::new(),
            next_idx: 0,
            channels: BTreeMap::from([(DEFAULT_CHANNEL, DEFAULT_CHANNEL_NAME.to_string())]),
            next_channel: DEFAULT_CHANNEL + 1,
            successors: Vec::new(),
        };
        if let Some(seed) = seed {
            for c in &seed.channels {
                st.channels.insert(c.id, c.name.clone());
            }
            st.channels.entry(DEFAULT_CHANNEL).or_insert_with(|| DEFAULT_CHANNEL_NAME.to_string());
            st.next_channel = st.channels.keys().max().map_or(DEFAULT_CHANNEL, |&m| m) + 1;
            for &(node, idx) in &seed.members {
                st.idx_of.insert(node, idx);
            }
            st.next_idx = seed.members.iter().map(|&(_, i)| i).max().map_or(0, |m| m + 1);
        }
        st
    }

    /// 重算候補名單（有備用端點、不是房主本人；越早進房越前面），有變動就廣播
    fn update_successors(&mut self, owner: Option<NodeId>) {
        let mut v: Vec<Successor> = self
            .members
            .values()
            .filter(|m| Some(m.info.node_id) != owner)
            .filter_map(|m| m.standby.clone().map(|host| Successor { session_idx: m.info.session_idx, host }))
            .collect();
        v.sort_by_key(|s| s.session_idx);
        if v != self.successors {
            self.successors = v.clone();
            self.broadcast(HostMsg::Successors(v));
        }
    }

    fn broadcast(&self, msg: HostMsg) {
        for m in self.members.values() {
            let _ = m.ctrl.send(msg.clone());
        }
    }

    fn channel_list(&self) -> Vec<ChannelInfo> {
        self.channels.iter().map(|(&id, name)| ChannelInfo { id, name: name.clone() }).collect()
    }

    fn subscription_list(&self) -> Vec<Subscription> {
        let mut v: Vec<_> = self
            .members
            .values()
            .flat_map(|m| {
                m.subs.iter().map(|(&channel, s)| Subscription {
                    session_idx: m.info.session_idx,
                    channel,
                    listen: s.listen,
                    talk: s.talk,
                })
            })
            .collect();
        v.sort_by_key(|s| (s.channel, s.session_idx));
        v
    }
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
    /// 停止接受連線（讓位）
    stop: Arc<tokio::sync::Notify>,
}

impl Host {
    pub fn new(cfg: HostConfig) -> Self {
        let state = State::new(cfg.seed.as_ref());
        Self {
            cfg: Arc::new(cfg),
            state: Arc::new(RwLock::new(state)),
            stats: Default::default(),
            stop: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// 連線中的人數（含房主自己的客戶端）
    pub fn member_count(&self) -> usize {
        self.state.read().unwrap().members.len()
    }

    /// 雙房主合併時讓位：通知所有人改連 `host`
    pub fn yield_to(&self, host: HostAddr) {
        info!("讓位給另一個房主");
        self.state.read().unwrap().broadcast(HostMsg::MoveTo { host });
    }

    /// 停止當房主：不再接受連線，關掉所有成員的連線
    pub fn stop(&self) {
        self.stop.notify_one();
        let st = self.state.read().unwrap();
        for m in st.members.values() {
            m.conn.close(4u32.into(), b"host stopped");
        }
    }

    pub fn term(&self) -> u64 {
        self.cfg.term
    }

    /// 目前的候補名單
    pub fn successors(&self) -> Vec<Successor> {
        self.state.read().unwrap().successors.clone()
    }

    /// 計畫性轉移：通知所有人由第一候補以 `term + 1` 接手。回傳接手的人（沒有候補時 None，什麼都不做）
    pub fn migrate(&self) -> Option<Successor> {
        let st = self.state.read().unwrap();
        let next = st.successors.first()?.clone();
        info!("房主轉移給 #{}（任期 {}）", next.session_idx, self.cfg.term + 1);
        st.broadcast(HostMsg::MigrateTo {
            session_idx: next.session_idx,
            host: next.host.clone(),
            term: self.cfg.term + 1,
        });
        Some(next)
    }

    pub fn members(&self) -> Vec<MemberInfo> {
        let st = self.state.read().unwrap();
        let mut v: Vec<_> = st.members.values().map(|m| m.info.clone()).collect();
        v.sort_by_key(|m| m.session_idx);
        v
    }

    pub fn channels(&self) -> Vec<ChannelInfo> {
        self.state.read().unwrap().channel_list()
    }

    pub fn subscriptions(&self) -> Vec<Subscription> {
        self.state.read().unwrap().subscription_list()
    }

    pub fn stats(&self) -> HostStats {
        *self.stats.lock().unwrap()
    }

    /// 接受連線直到端點關閉
    pub async fn run(&self, ep: Endpoint) {
        loop {
            let incoming = tokio::select! {
                i = ep.accept() => match i { Some(i) => i, None => return },
                _ = self.stop.notified() => return,
            };
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
                for &ch in old.subs.keys() {
                    st.broadcast(HostMsg::SubscriptionChanged(Subscription {
                        session_idx: idx,
                        channel: ch,
                        listen: false,
                        talk: false,
                    }));
                }
            }
            st.broadcast(HostMsg::MemberJoined(info.clone()));
            st.members.insert(
                idx,
                Member {
                    info: info.clone(),
                    conn: conn.clone(),
                    ctrl: ctrl_tx.clone(),
                    subs: HashMap::new(),
                    last_recipients: Mutex::new(Vec::new()),
                    standby: None,
                },
            );
            // 新加入的人也要拿到目前的候補名單
            let _ = ctrl_tx.send(HostMsg::Successors(st.successors.clone()));
            st.update_successors(self.cfg.owner);
        }

        let result = self.serve(&conn, idx, &mut recv, &ctrl_tx).await;

        {
            let mut st = self.state.write().unwrap();
            // 只移除「這條」連線（可能已被新連線取代）
            if st.members.get(&idx).is_some_and(|m| m.conn.stable_id() == conn.stable_id()) {
                st.members.remove(&idx);
                st.broadcast(HostMsg::MemberLeft { session_idx: idx });
                st.update_successors(self.cfg.owner);
                info!("成員離開：#{idx} {}", info.display_name);
            }
        }
        writer.abort();
        result
    }

    async fn handshake(&self, conn: &Connection, send: &mut SendStream, recv: &mut RecvStream) -> Result<MemberInfo> {
        let reject = |code, msg: &str| HostMsg::Error { code, message: msg.to_string() };

        match transport::read_msg::<ClientMsg>(recv).await? {
            // 另一個房主來探測（雙房主合併）：回報任期與人數就結束
            ClientMsg::HostProbe { room_id } => {
                if room_id == self.cfg.room_id {
                    let members = self.state.read().unwrap().members.len() as u32;
                    transport::write_msg(send, &HostMsg::HostInfo { term: self.cfg.term, members }).await?;
                }
                bail!("探測");
            }
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
                sanitize(&display_name, MAX_NAME_CHARS, "（未命名）")
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
                let members: Vec<_> =
                    st.members.values().filter(|m| m.info.session_idx != idx).map(|m| m.info.clone()).collect();
                let subscriptions: Vec<_> =
                    st.subscription_list().into_iter().filter(|s| s.session_idx != idx).collect();
                Some((idx, members, st.channel_list(), subscriptions))
            }
        };
        let Some((idx, members, channels, subscriptions)) = assigned else {
            transport::write_msg(send, &reject(ErrorCode::RoomFull, "房間已滿")).await?;
            bail!("房間已滿");
        };

        let info = MemberInfo { session_idx: idx, node_id: client, display_name };
        transport::write_msg(
            send,
            &HostMsg::Welcome { session_idx: idx, term: self.cfg.term, members, channels, subscriptions },
        )
        .await?;
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
                msg = transport::read_msg::<ClientMsg>(recv) => {
                    let msg = msg?;
                    if matches!(msg, ClientMsg::Leave) {
                        conn.close(0u32.into(), b"bye");
                        return Ok(());
                    }
                    if let Err((code, message)) = self.on_control(idx, msg, ctrl) {
                        let _ = ctrl.send(HostMsg::Error { code, message: message.into() });
                    }
                }
                d = conn.read_datagram() => {
                    let d = d.context("連線中斷")?;
                    self.forward(idx, &d, &mut out);
                }
            }
        }
    }

    /// 房主才能做的操作
    fn require_owner(&self, st: &State, idx: SessionIdx, why: &'static str) -> Result<(), (ErrorCode, &'static str)> {
        match self.cfg.owner {
            None => Ok(()),
            Some(owner) if st.members.get(&idx).is_some_and(|m| m.info.node_id == owner) => Ok(()),
            Some(_) => Err((ErrorCode::Forbidden, why)),
        }
    }

    fn on_control(
        &self,
        idx: SessionIdx,
        msg: ClientMsg,
        ctrl: &mpsc::UnboundedSender<HostMsg>,
    ) -> Result<(), (ErrorCode, &'static str)> {
        let mut st = self.state.write().unwrap();
        match msg {
            ClientMsg::Ping { t } => {
                let _ = ctrl.send(HostMsg::Pong { t });
            }
            ClientMsg::Subscribe { channel, slot, listen, talk } => {
                if !st.channels.contains_key(&channel) {
                    return Err((ErrorCode::NoSuchChannel, "頻道不存在"));
                }
                if slot >= MAX_SLOTS {
                    return Err((ErrorCode::BadSlot, "槽位超出範圍"));
                }
                let me = st.members.get_mut(&idx).ok_or((ErrorCode::BadMessage, "成員不存在"))?;
                if me.subs.iter().any(|(&c, s)| c != channel && s.slot == slot) {
                    return Err((ErrorCode::BadSlot, "槽位已被其他頻道使用"));
                }
                if listen || talk {
                    me.subs.insert(channel, Sub { slot, listen, talk });
                } else {
                    me.subs.remove(&channel);
                }
                st.broadcast(HostMsg::SubscriptionChanged(Subscription { session_idx: idx, channel, listen, talk }));
            }
            ClientMsg::Unsubscribe { channel } => {
                let me = st.members.get_mut(&idx).ok_or((ErrorCode::BadMessage, "成員不存在"))?;
                if me.subs.remove(&channel).is_some() {
                    st.broadcast(HostMsg::SubscriptionChanged(Subscription {
                        session_idx: idx,
                        channel,
                        listen: false,
                        talk: false,
                    }));
                }
            }
            ClientMsg::CreateChannel { name } => {
                if st.channels.len() >= MAX_CHANNELS {
                    return Err((ErrorCode::TooManyChannels, "頻道數已達上限"));
                }
                let id = st.next_channel;
                st.next_channel += 1;
                let name = sanitize(&name, MAX_CHANNEL_NAME_CHARS, "新頻道");
                st.channels.insert(id, name.clone());
                st.broadcast(HostMsg::ChannelAdded(ChannelInfo { id, name }));
            }
            ClientMsg::RenameChannel { channel, name } => {
                self.require_owner(&st, idx, "只有房主可以改頻道名稱")?;
                let name = sanitize(&name, MAX_CHANNEL_NAME_CHARS, "新頻道");
                let slot = st.channels.get_mut(&channel).ok_or((ErrorCode::NoSuchChannel, "頻道不存在"))?;
                *slot = name.clone();
                st.broadcast(HostMsg::ChannelRenamed { channel, name });
            }
            ClientMsg::DeleteChannel { channel } => {
                self.require_owner(&st, idx, "只有房主可以刪除頻道")?;
                if channel == DEFAULT_CHANNEL {
                    return Err((ErrorCode::Forbidden, "大廳不能刪除"));
                }
                if st.channels.remove(&channel).is_none() {
                    return Err((ErrorCode::NoSuchChannel, "頻道不存在"));
                }
                for m in st.members.values_mut() {
                    m.subs.remove(&channel);
                }
                st.broadcast(HostMsg::ChannelRemoved { channel });
            }
            ClientMsg::Standby(addr) => {
                let me = st.members.get_mut(&idx).ok_or((ErrorCode::BadMessage, "成員不存在"))?;
                me.standby = Some(addr);
                st.update_successors(self.cfg.owner);
            }
            other => {
                warn!("#{idx} 送了不該在這時送的訊息：{other:?}");
                return Err((ErrorCode::BadMessage, "非預期的訊息"));
            }
        }
        Ok(())
    }

    /// 語音扇出（見模組說明）
    fn forward(&self, from: SessionIdx, datagram: &[u8], out: &mut Vec<u8>) {
        let mut stats = self.stats.lock().unwrap();
        stats.datagrams_in += 1;
        let Some(up) = VoiceUp::parse(datagram) else {
            stats.bad_datagrams += 1;
            return;
        };
        let st = self.state.read().unwrap();
        let Some(sender) = st.members.get(&from) else { return };

        // 說話者實際要送達的頻道：talk_mask 有設、且他在該頻道 talk = true
        let talking: Vec<ChannelId> = if up.kind == Kind::Voice {
            sender
                .subs
                .iter()
                .filter(|(_, s)| s.talk && up.talk_mask & (1u64 << s.slot) != 0)
                .map(|(&c, _)| c)
                .collect()
        } else {
            Vec::new()
        };

        // 每位接收者的 via_mask（用接收者自己的槽位）；同一人只送一份
        let mut recipients: Vec<(SessionIdx, u64)> = Vec::new();
        if !talking.is_empty() {
            for (&ri, r) in &st.members {
                if ri == from {
                    continue;
                }
                let mask = talking
                    .iter()
                    .filter_map(|c| r.subs.get(c))
                    .filter(|s| s.listen)
                    .fold(0u64, |m, s| m | 1u64 << s.slot);
                if mask != 0 {
                    recipients.push((ri, mask));
                }
            }
        }

        for &(ri, mask) in &recipients {
            up.to_down(from, mask).encode(out);
            if st.members[&ri].conn.send_datagram(Bytes::copy_from_slice(out)).is_ok() {
                stats.datagrams_out += 1;
            }
        }

        // 上一幀聽得到、這一幀聽不到的人：補送 end-of-stream，讓他們立刻結束這段話
        let mut last = sender.last_recipients.lock().unwrap();
        for &ri in last.iter() {
            if recipients.iter().any(|&(r, _)| r == ri) {
                continue;
            }
            if let Some(r) = st.members.get(&ri) {
                VoiceDown { kind: Kind::EndOfStream, sender_idx: from, seq: up.seq, via_mask: 0, payload: &[] }
                    .encode(out);
                if r.conn.send_datagram(Bytes::copy_from_slice(out)).is_ok() {
                    stats.datagrams_out += 1;
                }
            }
        }
        *last = recipients.into_iter().map(|(r, _)| r).collect();
    }
}

/// 名稱：去掉控制字元、限制長度、不可為空
fn sanitize(s: &str, max: usize, empty: &str) -> String {
    let n: String = s.chars().filter(|c| !c.is_control()).take(max).collect();
    let n = n.trim().to_string();
    if n.is_empty() { empty.into() } else { n }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_sanitizing() {
        assert_eq!(sanitize("  小明\n\t ", MAX_NAME_CHARS, "x"), "小明");
        assert_eq!(sanitize("\u{7}", MAX_NAME_CHARS, "（未命名）"), "（未命名）");
        assert_eq!(sanitize(&"長".repeat(100), MAX_NAME_CHARS, "x").chars().count(), MAX_NAME_CHARS);
    }
}
