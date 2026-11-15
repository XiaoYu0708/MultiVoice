//! MultiVoice 協定定義（PLAN.md §4）。
//!
//! 這個 crate 不依賴 iroh、tokio 或作業系統 API，原生與 WASM 共用：
//! - [`ticket`]：邀請票
//! - [`control`]：控制訊息（iroh reliable stream 上，長度前綴 + postcard）
//! - [`voice`]：語音 datagram 標頭
//! - [`auth`]：加入房間的挑戰／證明

pub mod auth;
pub mod control;
pub mod ticket;
pub mod voice;

/// 協定版本。不相容的變更就 +1；房主會拒絕版本不同的客戶端。
pub const PROTOCOL_VERSION: u16 = 1;

/// iroh 連線的 ALPN
pub const ALPN: &[u8] = b"multivoice/1";

/// iroh 的 EndpointId（Ed25519 公鑰）原始位元組。protocol 不依賴 iroh，所以用陣列表示。
pub type NodeId = [u8; 32];

/// 房間內的成員索引，由房主分配；換房主後不變（PLAN.md §4.3）
pub type SessionIdx = u16;

/// 頻道 ID。M1 只有一個預設頻道 [`DEFAULT_CHANNEL`]。
pub type ChannelId = u32;
pub const DEFAULT_CHANNEL: ChannelId = 0;
