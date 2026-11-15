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
/// v3：房主轉移（Standby、Successors、MigrateTo）
/// v4：雙房主合併（HostProbe、HostInfo、MoveTo）
pub const PROTOCOL_VERSION: u16 = 4;

/// iroh 連線的 ALPN。**固定不變**：版本差異由 Hello 的 `protocol_version` 處理，
/// 這樣舊版客戶端才會收到「請更新」的錯誤訊息，而不是在 TLS 握手就被拒絕。
pub const ALPN: &[u8] = b"multivoice/1";

/// iroh 的 EndpointId（Ed25519 公鑰）原始位元組。protocol 不依賴 iroh，所以用陣列表示。
pub type NodeId = [u8; 32];

/// 房間內的成員索引，由房主分配；換房主後不變（PLAN.md §4.3）
pub type SessionIdx = u16;

/// 頻道 ID，由房主分配。[`DEFAULT_CHANNEL`]（大廳）建房時就存在，不能刪除。
pub type ChannelId = u32;
pub const DEFAULT_CHANNEL: ChannelId = 0;
pub const DEFAULT_CHANNEL_NAME: &str = "大廳";
