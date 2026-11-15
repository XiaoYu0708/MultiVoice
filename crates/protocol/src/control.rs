//! 控制訊息（PLAN.md §4.2）。
//!
//! 傳輸：iroh 雙向 stream，每則訊息為 `u32 LE 長度` + `postcard(訊息)`。
//! M1 只實作加入、成員變動與 ping；多頻道（M2）、房主轉移（M3）的訊息之後加。
//! 新增變體一律加在列舉**最後面**，postcard 以變體序號編碼，插在中間會破壞相容性。

use serde::{Deserialize, Serialize};

use crate::auth::{Nonce, Proof};
use crate::ticket::RoomId;
use crate::{NodeId, SessionIdx};

/// 單則控制訊息上限，防止惡意長度造成大量配置
pub const MAX_FRAME: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberInfo {
    pub session_idx: SessionIdx,
    pub node_id: NodeId,
    pub display_name: String,
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
}

/// 房主 → 成員
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostMsg {
    /// 加入流程第 2 步
    Challenge { nonce: Nonce },
    /// 加入流程第 4 步
    Welcome { session_idx: SessionIdx, term: u64, members: Vec<MemberInfo> },
    MemberJoined(MemberInfo),
    MemberLeft { session_idx: SessionIdx },
    Pong { t: u64 },
    /// 不是房主時，告訴對方目前的房主（M3）
    Redirect { host: NodeId, term: u64 },
    Error { code: ErrorCode, message: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    ProtocolVersion,
    WrongRoom,
    AuthFailed,
    Banned,
    RoomFull,
    BadMessage,
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
        });
        roundtrip(HostMsg::Error { code: ErrorCode::AuthFailed, message: "x".into() });
    }

    #[test]
    fn rejects_oversized_and_garbage() {
        assert_eq!(frame_len(((MAX_FRAME + 1) as u32).to_le_bytes()), Err(FrameError::TooLarge(MAX_FRAME + 1)));
        assert_eq!(decode_body::<HostMsg>(&[0xFF, 0xFF, 0xFF]), Err(FrameError::Decode));
    }
}
