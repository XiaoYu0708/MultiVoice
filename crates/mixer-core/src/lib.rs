//! 混音與 jitter buffer 的純邏輯（PLAN.md §6.2）。不碰 I/O、不依賴時鐘，時間一律由呼叫端傳入。

pub mod denoise;
pub mod jitter;
pub mod mix;
pub mod route;
pub mod talk;

/// 一幀的長度（與 Opus 幀一致）
pub const FRAME_MS: u64 = 20;
pub const FRAME_US: u64 = FRAME_MS * 1000;
