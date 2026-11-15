//! 說話遮罩與提示音：純邏輯在 `mixer_core::talk`（網頁版也用）

pub use mixer_core::talk::*;

const _: () = assert!(SLOTS == protocol::control::MAX_SLOTS as usize);
