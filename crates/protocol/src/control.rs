//! 控制訊息（PLAN.md §4.2）。
//!
//! 傳輸：iroh 雙向 stream，每則訊息為 `u32 LE 長度` + `postcard(訊息)`。
//! 新增變體一律加在列舉**最後面**，postcard 以變體序號編碼，插在中間會破壞相容性
//! （不相容的變更要提高 [`crate::PROTOCOL_VERSION`]）。
//!
//! ## 頻道與槽位（M2）
//! 成員訂閱頻道時**自己挑一個槽位**（0..64）告訴房主。之後：
//! - 上行語音的 `talk_mask` 用「自己的槽位」表示要對哪些頻道說話
//! - 下行語音的 `via_mask` 用「接收者自己的槽位」表示經由哪些頻道聽到
//!
//! 槽位由成員決定，所以不需要等房主回覆就能開始說話，也沒有登記訊息與語音封包的先後問題。

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::auth::{Nonce, Proof};
use crate::ticket::{RoomId, RoomSecret};
use crate::{ChannelId, NodeId, SessionIdx};

/// 單則控制訊息上限，防止惡意長度造成大量配置
pub const MAX_FRAME: usize = 64 * 1024;
/// 每位成員最多訂閱的頻道數（= 槽位數 = u64 遮罩的 bit 數）
pub const MAX_SLOTS: u8 = 64;
/// 房間最多頻道數
pub const MAX_CHANNELS: usize = 64;
pub const MAX_CHANNEL_NAME_CHARS: usize = 24;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberInfo {
    pub session_idx: SessionIdx,
    pub node_id: NodeId,
    pub display_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub id: ChannelId,
    pub name: String,
}

/// 某位成員在某頻道的狀態（所有人都看得到，PLAN.md §2.2：不允許隱形竊聽）。
/// `listen` 與 `talk` 都是 false 代表已離開該頻道。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    pub session_idx: SessionIdx,
    pub channel: ChannelId,
    pub listen: bool,
    pub talk: bool,
}

/// 一個 iroh 端點的位址（M3：備用房主端點）。protocol 不依賴 iroh，所以 relay 用字串。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostAddr {
    pub id: NodeId,
    pub addrs: Vec<SocketAddr>,
    pub relay: Option<String>,
}

/// 房主候補（M3）：成員與他的備用房主端點
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Successor {
    pub session_idx: SessionIdx,
    pub host: HostAddr,
}

/// 成員 → 房主
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMsg {
    /// 加入流程第 1 步
    Hello { protocol_version: u16, room_id: RoomId },
    /// 加入流程第 3 步：回應房主的 [`HostMsg::Challenge`]
    Auth { proof: Proof, display_name: String },
    Ping { t: u64 },
    Leave,
    /// 加入或更新某頻道的訂閱。`slot` 由成員自己挑（同一成員的各頻道不可重複）。
    /// `talk = false` 的頻道不會收到這位成員的聲音，即使 talk_mask 有設。
    Subscribe { channel: ChannelId, slot: u8, listen: bool, talk: bool },
    Unsubscribe { channel: ChannelId },
    CreateChannel { name: String },
    RenameChannel { channel: ChannelId, name: String },
    DeleteChannel { channel: ChannelId },
    /// 我可以當房主：這是我預先建好的備用房主端點（M3）
    Standby(HostAddr),
    /// 不加入，只問「你是不是這個房間的房主、有幾個人」（雙房主合併用；取代 Hello 當第一則訊息）
    HostProbe { room_id: RoomId },
    /// 管理者：把這位成員移出房間（同時更換邀請，舊的邀請票失效）（v5）
    Kick { session_idx: SessionIdx },
    /// 管理者：更換邀請（新的房間密鑰，舊的邀請票失效）（v5）
    RotateSecret,
    /// 管理者：把管理權交給這位成員（他要能當轉發者，也就是有宣告備用端點）（v5）
    TransferOwner { session_idx: SessionIdx },
    /// 管理者：把這位成員移進（`join = true`，只收聽）或移出某個頻道（v6）。
    /// 房主只轉告本人（[`HostMsg::Moved`]），由他自己照一般流程訂閱／退出（槽位由成員自己挑）
    MoveMember { session_idx: SessionIdx, channel: ChannelId, join: bool },
}

/// 房主 → 成員
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostMsg {
    /// 加入流程第 2 步
    Challenge { nonce: Nonce },
    /// 加入流程第 4 步：完整的房間狀態
    Welcome {
        session_idx: SessionIdx,
        term: u64,
        members: Vec<MemberInfo>,
        channels: Vec<ChannelInfo>,
        subscriptions: Vec<Subscription>,
        /// 管理者（開房的人，或被他轉移的人）的成員身分；None = 不限制（v5）
        owner: Option<NodeId>,
        /// 目前轉發者（房主）自己的成員編號；還沒連上時 None（v5）
        relay: Option<SessionIdx>,
    },
    MemberJoined(MemberInfo),
    MemberLeft { session_idx: SessionIdx },
    Pong { t: u64 },
    /// 不是房主時，告訴對方目前的房主（M3）
    Redirect { host: NodeId, term: u64 },
    Error { code: ErrorCode, message: String },
    ChannelAdded(ChannelInfo),
    ChannelRemoved { channel: ChannelId },
    ChannelRenamed { channel: ChannelId, name: String },
    /// 有人進出頻道或改變收聽／說話狀態（含自己，作為確認）
    SubscriptionChanged(Subscription),
    /// 房主候補名單，依接手順序（M3）。房主失聯時成員依序找他們
    Successors(Vec<Successor>),
    /// 計畫性轉移（房主要離開）：`session_idx` 這位成員以任期 `term` 接手，其他人改連 `host`（M3）
    MigrateTo { session_idx: SessionIdx, host: HostAddr, term: u64 },
    /// 回應 [`ClientMsg::HostProbe`]：我是房主，任期與連線中的人數
    HostInfo { term: u64, members: u32 },
    /// 雙房主合併：這個房主要讓位，所有人改連 `host`（沒有人要接手）
    MoveTo { host: HostAddr },
    /// 邀請更換了：之後加入（含換房主後重連）都要用新的密鑰（v5）
    SecretChanged { secret: RoomSecret },
    /// 你被管理者移出房間：不要重連，也不要接手當房主（v5）
    Kicked,
    /// 管理權換人（v5）
    OwnerChanged { owner: NodeId },
    /// 管理者把你移進（只收聽）或移出這個頻道（v6）；移出後可以自己再加回去
    Moved { channel: ChannelId, join: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    ProtocolVersion,
    WrongRoom,
    AuthFailed,
    Banned,
    RoomFull,
    BadMessage,
    NoSuchChannel,
    TooManyChannels,
    BadSlot,
    /// 預設頻道不能刪除
    Forbidden,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    TooLarge(usize),
    Decode,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooLarge(n) => write!(f, "控制訊息過大（{n} bytes）"),
            FrameError::Decode => f.write_str("控制訊息格式錯誤"),
        }
    }
}

impl std::error::Error for FrameError {}

/// 編碼成一個完整的 frame（含長度前綴）
pub fn encode_frame<T: Serialize>(msg: &T) -> Vec<u8> {
    let body = postcard::to_allocvec(msg).expect("控制訊息序列化不會失敗");
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// 解析長度前綴；超過上限即回錯，呼叫端應中斷連線
pub fn frame_len(prefix: [u8; 4]) -> Result<usize, FrameError> {
    let n = u32::from_le_bytes(prefix) as usize;
    if n > MAX_FRAME { Err(FrameError::TooLarge(n)) } else { Ok(n) }
}

pub fn decode_body<'a, T: Deserialize<'a>>(body: &'a [u8]) -> Result<T, FrameError> {
    postcard::from_bytes(body).map_err(|_| FrameError::Decode)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T: Serialize + for<'a> Deserialize<'a> + PartialEq + std::fmt::Debug>(m: T) {
        let f = encode_frame(&m);
        let n = frame_len(f[..4].try_into().unwrap()).unwrap();
        assert_eq!(n, f.len() - 4);
        assert_eq!(decode_body::<T>(&f[4..]).unwrap(), m);
    }

    #[test]
    fn messages_roundtrip() {
        roundtrip(ClientMsg::Hello { protocol_version: 1, room_id: RoomId([7; 32]) });
        roundtrip(ClientMsg::Auth { proof: [1; 32], display_name: "小明".into() });
        roundtrip(HostMsg::Welcome {
            session_idx: 3,
            term: 1,
            members: vec![MemberInfo { session_idx: 1, node_id: [9; 32], display_name: "會長".into() }],
            channels: vec![ChannelInfo { id: 0, name: "大廳".into() }, ChannelInfo { id: 5, name: "指揮部".into() }],
            subscriptions: vec![Subscription { session_idx: 1, channel: 5, listen: true, talk: false }],
            owner: Some([9; 32]),
            relay: Some(1),
        });
        roundtrip(ClientMsg::Kick { session_idx: 4 });
        roundtrip(HostMsg::SecretChanged { secret: RoomSecret([5; 32]) });
        roundtrip(HostMsg::OwnerChanged { owner: [6; 32] });
        roundtrip(ClientMsg::Subscribe { channel: 5, slot: 63, listen: true, talk: true });
        roundtrip(HostMsg::SubscriptionChanged(Subscription { session_idx: 2, channel: 0, listen: false, talk: false }));
        roundtrip(HostMsg::Error { code: ErrorCode::AuthFailed, message: "x".into() });
    }

    #[test]
    fn rejects_oversized_and_garbage() {
        assert_eq!(frame_len(((MAX_FRAME + 1) as u32).to_le_bytes()), Err(FrameError::TooLarge(MAX_FRAME + 1)));
        assert_eq!(decode_body::<HostMsg>(&[0xFF, 0xFF, 0xFF]), Err(FrameError::Decode));
    }
}
