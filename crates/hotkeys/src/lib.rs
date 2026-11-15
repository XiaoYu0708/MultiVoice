//! 全域按鍵：按鍵名稱解析（"F9"、"Ctrl+Q"、"Mouse4"…）與按住狀態查詢。
//!
//! Windows 上 [`start`] 會開一條執行緒安裝 `WH_KEYBOARD_LL` / `WH_MOUSE_LL`，
//! 只記錄「目前按住哪些鍵」，**不攔截**（遊戲照樣收得到）。
//! 其他平台 [`start`] 回傳錯誤，[`Hotkey::held`] 永遠是 false。

mod keys;
#[cfg(windows)]
mod hook;

pub use keys::{Hotkey, Trigger, VK_CONTROL, VK_MENU, VK_SHIFT, key_name, parse_key};

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// 0..=255：虛擬鍵碼；256..：滑鼠按鍵
const SLOTS: usize = 5;
static DOWN: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
/// hook 收到的事件總數（UI 用來判斷「遊戲在前景時是否還收得到按鍵」）
static EVENTS: AtomicU64 = AtomicU64::new(0);

/// 切換型按鍵（例如全域靜音）的按下次數；hook 執行緒在「按下」的瞬間判斷，短按也不會漏
static WATCHES: Mutex<Vec<(Hotkey, Arc<AtomicU32>)>> = Mutex::new(Vec::new());

static RECORDING: AtomicBool = AtomicBool::new(false);
static RECORDED: Mutex<Option<Recorded>> = Mutex::new(None);

/// 錄製模式的結果
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    Key(Hotkey),
    Cancelled,
}

fn bit(t: Trigger) -> usize {
    match t {
        Trigger::Key(vk) => (vk & 0xFF) as usize,
        Trigger::MouseMiddle => 256,
        Trigger::MouseX1 => 257,
        Trigger::MouseX2 => 258,
    }
}

fn set_down(t: Trigger, down: bool) {
    let b = bit(t);
    let m = 1u64 << (b % 64);
    if down {
        DOWN[b / 64].fetch_or(m, Ordering::Relaxed);
    } else {
        DOWN[b / 64].fetch_and(!m, Ordering::Relaxed);
    }
}

fn raw_down(t: Trigger) -> bool {
    let b = bit(t);
    DOWN[b / 64].load(Ordering::Relaxed) & (1 << (b % 64)) != 0
}

/// 某個觸發鍵是否按住（Ctrl/Alt/Shift/Win 不分左右）
pub fn is_down(t: Trigger) -> bool {
    keys::variants(t).iter().any(|&v| raw_down(v))
}

/// 目前按住的修飾鍵 (ctrl, alt, shift, win)
pub fn modifiers() -> (bool, bool, bool, bool) {
    use keys::*;
    (
        is_down(Trigger::Key(VK_CONTROL)),
        is_down(Trigger::Key(VK_MENU)),
        is_down(Trigger::Key(VK_SHIFT)),
        is_down(Trigger::Key(VK_LWIN)),
    )
}

pub fn event_count() -> u64 {
    EVENTS.load(Ordering::Relaxed)
}

/// 開始計算某個快捷鍵被按下的次數（不含自動重複）。丟掉回傳的計數器就停止計算。
pub fn watch_presses(hk: Hotkey) -> Arc<AtomicU32> {
    let c = Arc::new(AtomicU32::new(0));
    let mut w = WATCHES.lock().unwrap();
    w.retain(|(_, c)| Arc::strong_count(c) > 1);
    w.push((hk, c.clone()));
    c
}

/// 進入錄製模式：下一個按下的鍵（可帶修飾鍵；單按修飾鍵後放開也算；Esc 取消）
/// 由 [`take_recorded`] 取回
pub fn start_recording() {
    *RECORDED.lock().unwrap() = None;
    keys::reset_record_state();
    RECORDING.store(true, Ordering::SeqCst);
}

pub fn cancel_recording() {
    RECORDING.store(false, Ordering::SeqCst);
}

pub fn is_recording() -> bool {
    RECORDING.load(Ordering::SeqCst)
}

pub fn take_recorded() -> Option<Recorded> {
    RECORDED.lock().unwrap().take()
}

/// hook 收到一個按下／放開事件（hook 執行緒呼叫；測試也直接呼叫）
pub(crate) fn on_input(t: Trigger, down: bool) {
    EVENTS.fetch_add(1, Ordering::Relaxed);
    let pressed = down && !raw_down(t); // 自動重複的 keydown 不算新的按下
    set_down(t, down);
    if pressed {
        let mut w = WATCHES.lock().unwrap();
        w.retain(|(_, c)| Arc::strong_count(c) > 1);
        for (hk, c) in w.iter() {
            if keys::variants(hk.trigger).contains(&t) && hk.held() {
                c.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    if RECORDING.load(Ordering::SeqCst) {
        if let Some(r) = keys::record_step(t, down) {
            RECORDING.store(false, Ordering::SeqCst);
            *RECORDED.lock().unwrap() = Some(r);
        }
    }
}

/// 從其他來源補送按鍵事件。
///
/// 本程式自己的視窗在前景時，全域 hook 收不到大部分鍵盤事件（實測：中文輸入法會處理的鍵，
/// 例如字母、空白、Alt 都收不到；F19、滑鼠側鍵收得到），所以 UI 要把自己收到的按鍵也送進來。
/// 跟 hook 重複送同一個事件沒關係（重複的按下不算新的按下）。
pub fn feed(t: Trigger, down: bool) {
    on_input(t, down);
}

/// 啟動全域 hook（整個程式只需呼叫一次；重複呼叫直接回傳 Ok）
pub fn start() -> Result<(), String> {
    #[cfg(windows)]
    {
        hook::start()
    }
    #[cfg(not(windows))]
    {
        Err("全域按鍵目前只支援 Windows".into())
    }
}

/// 本程式是否以系統管理員身分執行（遊戲是管理員時，非管理員程式收不到它的按鍵）
pub fn is_admin() -> bool {
    #[cfg(windows)]
    {
        hook::is_admin()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 以系統管理員身分重新啟動自己（帶原本的參數）；成功送出要求回傳 true
pub fn relaunch_as_admin() -> bool {
    #[cfg(windows)]
    {
        hook::relaunch_as_admin()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keys::*;

    // 按鍵狀態是全域的，測試要依序執行
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn held_with_modifiers() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let hk: Hotkey = "Ctrl+F7".parse().unwrap();
        assert!(!hk.held());
        on_input(Trigger::Key(0x76), true);
        assert!(!hk.held(), "沒按 Ctrl");
        on_input(Trigger::Key(VK_RCONTROL), true);
        assert!(hk.held(), "右 Ctrl 也算 Ctrl");
        on_input(Trigger::Key(0x76), false);
        assert!(!hk.held());
        on_input(Trigger::Key(VK_RCONTROL), false);

        let m: Hotkey = "Mouse4".parse().unwrap();
        on_input(Trigger::MouseX1, true);
        assert!(m.held());
        on_input(Trigger::MouseX1, false);
        assert!(!m.held());
    }

    #[test]
    fn press_counter() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let c = watch_presses("Alt+F8".parse().unwrap());
        on_input(Trigger::Key(0x77), true);
        on_input(Trigger::Key(0x77), false);
        assert_eq!(c.load(Ordering::Relaxed), 0, "沒按 Alt");
        on_input(Trigger::Key(VK_LMENU), true);
        on_input(Trigger::Key(0x77), true);
        on_input(Trigger::Key(0x77), true); // 自動重複
        on_input(Trigger::Key(0x77), false);
        on_input(Trigger::Key(0x77), true);
        on_input(Trigger::Key(0x77), false);
        on_input(Trigger::Key(VK_LMENU), false);
        assert_eq!(c.load(Ordering::Relaxed), 2);
        drop(c);
    }

    #[test]
    fn recording() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        start_recording();
        on_input(Trigger::Key(VK_LSHIFT), true);
        assert!(take_recorded().is_none(), "只按修飾鍵還不算");
        on_input(Trigger::Key(0x48), true); // H
        assert_eq!(take_recorded(), Some(Recorded::Key("Shift+H".parse().unwrap())));
        on_input(Trigger::Key(0x48), false);
        on_input(Trigger::Key(VK_LSHIFT), false);

        start_recording();
        on_input(Trigger::Key(VK_LMENU), true);
        on_input(Trigger::Key(VK_LMENU), false);
        assert_eq!(take_recorded(), Some(Recorded::Key("LAlt".parse().unwrap())), "單按修飾鍵放開");

        start_recording();
        on_input(Trigger::MouseX2, true);
        assert_eq!(take_recorded().map(|r| match r {
            Recorded::Key(h) => h.to_string(),
            _ => String::new(),
        }), Some("Mouse5".into()));
        on_input(Trigger::MouseX2, false);

        start_recording();
        on_input(Trigger::Key(VK_ESCAPE), true);
        assert_eq!(take_recorded(), Some(Recorded::Cancelled));
        on_input(Trigger::Key(VK_ESCAPE), false);
        assert!(!is_recording());
    }
}
