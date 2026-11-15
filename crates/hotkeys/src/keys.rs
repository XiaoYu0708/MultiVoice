//! 按鍵名稱解析與產生（移植自 QuickExit `hotkey.rs`，改成不依賴 Win32 API）

use std::fmt;
use std::str::FromStr;
use std::sync::Mutex;

use crate::Recorded;

pub const VK_SHIFT: u16 = 0x10;
pub const VK_CONTROL: u16 = 0x11;
pub const VK_MENU: u16 = 0x12; // Alt
pub const VK_ESCAPE: u16 = 0x1B;
pub const VK_LWIN: u16 = 0x5B;
pub const VK_RWIN: u16 = 0x5C;
pub const VK_LSHIFT: u16 = 0xA0;
pub const VK_RSHIFT: u16 = 0xA1;
pub const VK_LCONTROL: u16 = 0xA2;
pub const VK_RCONTROL: u16 = 0xA3;
pub const VK_LMENU: u16 = 0xA4;
pub const VK_RMENU: u16 = 0xA5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Trigger {
    Key(u16),
    MouseMiddle,
    MouseX1,
    MouseX2,
}

/// 具名按鍵表：同一個鍵碼的第一個名稱是「正式名稱」，其餘是別名
const NAMED_KEYS: &[(&str, u16)] = &[
    ("Ctrl", VK_CONTROL),
    ("Control", VK_CONTROL),
    ("LCtrl", VK_LCONTROL),
    ("LControl", VK_LCONTROL),
    ("RCtrl", VK_RCONTROL),
    ("RControl", VK_RCONTROL),
    ("Alt", VK_MENU),
    ("LAlt", VK_LMENU),
    ("RAlt", VK_RMENU),
    ("Shift", VK_SHIFT),
    ("LShift", VK_LSHIFT),
    ("RShift", VK_RSHIFT),
    ("LWin", VK_LWIN),
    ("Win", VK_LWIN),
    ("RWin", VK_RWIN),
    ("Space", 0x20),
    ("Tab", 0x09),
    ("Enter", 0x0D),
    ("Return", 0x0D),
    ("Esc", VK_ESCAPE),
    ("Escape", VK_ESCAPE),
    ("Backspace", 0x08),
    ("Insert", 0x2D),
    ("Ins", 0x2D),
    ("Delete", 0x2E),
    ("Del", 0x2E),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PgUp", 0x21),
    ("PageDown", 0x22),
    ("PgDn", 0x22),
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    ("Pause", 0x13),
    ("ScrollLock", 0x91),
    ("CapsLock", 0x14),
    ("PrintScreen", 0x2C),
    ("NumLock", 0x90),
    ("NumMultiply", 0x6A),
    ("Multiply", 0x6A),
    ("NumAdd", 0x6B),
    ("Add", 0x6B),
    ("NumSubtract", 0x6D),
    ("Subtract", 0x6D),
    ("NumDecimal", 0x6E),
    ("Decimal", 0x6E),
    ("NumDivide", 0x6F),
    ("Divide", 0x6F),
    (";", 0xBA),
    ("Semicolon", 0xBA),
    ("=", 0xBB),
    ("Equals", 0xBB),
    (",", 0xBC),
    ("Comma", 0xBC),
    ("-", 0xBD),
    ("Minus", 0xBD),
    (".", 0xBE),
    ("Period", 0xBE),
    ("/", 0xBF),
    ("Slash", 0xBF),
    ("`", 0xC0),
    ("Grave", 0xC0),
    ("Backtick", 0xC0),
    ("[", 0xDB),
    ("LBracket", 0xDB),
    ("\\", 0xDC),
    ("Backslash", 0xDC),
    ("]", 0xDD),
    ("RBracket", 0xDD),
    ("'", 0xDE),
    ("Quote", 0xDE),
];

const NAMED_MOUSE: &[(&str, Trigger)] = &[
    ("Mouse3", Trigger::MouseMiddle),
    ("MButton", Trigger::MouseMiddle),
    ("Middle", Trigger::MouseMiddle),
    ("Mouse4", Trigger::MouseX1),
    ("XButton1", Trigger::MouseX1),
    ("Mouse5", Trigger::MouseX2),
    ("XButton2", Trigger::MouseX2),
];

/// 解析單一按鍵名稱（不含修飾鍵組合）
pub fn parse_key(name: &str) -> Option<Trigger> {
    let n = name.trim().to_ascii_lowercase();

    if let Some(i) = n.strip_prefix('f').and_then(|s| s.parse::<u16>().ok()) {
        if (1..=24).contains(&i) {
            return Some(Trigger::Key(0x70 + i - 1));
        }
    }
    for prefix in ["numpad", "num"] {
        if let Some(i) = n.strip_prefix(prefix).and_then(|s| s.parse::<u16>().ok()) {
            if i <= 9 {
                return Some(Trigger::Key(0x60 + i));
            }
        }
    }
    if n.len() == 1 {
        let c = n.as_bytes()[0];
        if c.is_ascii_lowercase() {
            return Some(Trigger::Key(c.to_ascii_uppercase() as u16));
        }
        if c.is_ascii_digit() {
            return Some(Trigger::Key(c as u16));
        }
    }
    NAMED_KEYS
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(&n))
        .map(|&(_, vk)| Trigger::Key(vk))
        .or_else(|| NAMED_MOUSE.iter().find(|(k, _)| k.eq_ignore_ascii_case(&n)).map(|&(_, t)| t))
}

/// 按鍵的正式名稱（parse_key 的反向）
pub fn key_name(t: Trigger) -> Option<String> {
    let vk = match t {
        Trigger::Key(vk) => vk,
        mouse => return NAMED_MOUSE.iter().find(|(_, m)| *m == mouse).map(|(n, _)| n.to_string()),
    };
    match vk {
        0x70..=0x87 => Some(format!("F{}", vk - 0x70 + 1)),
        0x60..=0x69 => Some(format!("Numpad{}", vk - 0x60)),
        0x30..=0x39 | 0x41..=0x5A => Some((vk as u8 as char).to_string()),
        _ => NAMED_KEYS.iter().find(|(_, v)| *v == vk).map(|(n, _)| n.to_string()),
    }
}

pub fn is_modifier(vk: u16) -> bool {
    matches!(
        vk,
        VK_CONTROL | VK_LCONTROL | VK_RCONTROL | VK_MENU | VK_LMENU | VK_RMENU | VK_SHIFT | VK_LSHIFT
            | VK_RSHIFT | VK_LWIN | VK_RWIN
    )
}

/// 同一個修飾鍵的左右版本都算（例如設定 Ctrl 時 LCtrl / RCtrl 都可以）
pub(crate) fn variants(t: Trigger) -> Vec<Trigger> {
    let k = Trigger::Key;
    match t {
        Trigger::Key(VK_CONTROL) => vec![k(VK_CONTROL), k(VK_LCONTROL), k(VK_RCONTROL)],
        Trigger::Key(VK_MENU) => vec![k(VK_MENU), k(VK_LMENU), k(VK_RMENU)],
        Trigger::Key(VK_SHIFT) => vec![k(VK_SHIFT), k(VK_LSHIFT), k(VK_RSHIFT)],
        Trigger::Key(VK_LWIN) => vec![k(VK_LWIN), k(VK_RWIN)],
        t => vec![t],
    }
}

/// 快捷鍵：觸發鍵 + 必須同時按住的修飾鍵。
/// 判斷「按住」時只要求設定的修飾鍵有按，**多按其他修飾鍵也算**
/// （遊戲中常按著 Shift 跑步，PTT 不能因此失效）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub trigger: Trigger,
}

impl Hotkey {
    pub fn single(trigger: Trigger) -> Self {
        Hotkey { ctrl: false, alt: false, shift: false, win: false, trigger }
    }

    /// 目前是否按住（需要 hook 已啟動）
    pub fn held(&self) -> bool {
        if !crate::is_down(self.trigger) {
            return false;
        }
        let (c, a, s, w) = crate::modifiers();
        (!self.ctrl || c) && (!self.alt || a) && (!self.shift || s) && (!self.win || w)
    }
}

impl FromStr for Hotkey {
    type Err = String;

    /// 解析 "Ctrl+Alt+F9"、"Mouse4"、"LCtrl" 之類的字串
    fn from_str(s: &str) -> Result<Self, String> {
        let text = s.trim();
        if text.is_empty() {
            return Err("按鍵不可為空".into());
        }
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        if parts.iter().any(|p| p.is_empty()) {
            return Err(format!("按鍵格式錯誤：\"{text}\""));
        }
        let (last, mods) = parts.split_last().unwrap();
        let mut hk = Hotkey::single(parse_key(last).ok_or_else(|| format!("看不懂的按鍵名稱：\"{last}\""))?);
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => hk.ctrl = true,
                "alt" => hk.alt = true,
                "shift" => hk.shift = true,
                "win" => hk.win = true,
                other => return Err(format!("\"{other}\" 不是修飾鍵（只能用 Ctrl / Alt / Shift / Win）")),
            }
        }
        Ok(hk)
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [(self.ctrl, "Ctrl+"), (self.alt, "Alt+"), (self.shift, "Shift+"), (self.win, "Win+")] {
            if on {
                f.write_str(name)?;
            }
        }
        match key_name(self.trigger) {
            Some(n) => f.write_str(&n),
            None => match self.trigger {
                Trigger::Key(vk) => write!(f, "VK{vk:02X}"),
                _ => unreachable!(),
            },
        }
    }
}

// ---------------------------------------------------------------- 錄製

/// 錄製中按下、尚未有其他鍵的修飾鍵（放開時就把它本身當成按鍵）
static PENDING_MOD: Mutex<Option<u16>> = Mutex::new(None);

pub(crate) fn reset_record_state() {
    *PENDING_MOD.lock().unwrap() = None;
}

/// 錄製模式的一步；回傳 Some 表示錄製結束
pub(crate) fn record_step(t: Trigger, down: bool) -> Option<Recorded> {
    let mut pending = PENDING_MOD.lock().unwrap();
    match (t, down) {
        (Trigger::Key(VK_ESCAPE), true) => Some(Recorded::Cancelled),
        (Trigger::Key(vk), true) if is_modifier(vk) => {
            pending.get_or_insert(vk);
            None
        }
        (Trigger::Key(vk), false) if is_modifier(vk) => {
            (*pending == Some(vk)).then(|| Recorded::Key(Hotkey::single(Trigger::Key(vk))))
        }
        (t, true) => {
            let (ctrl, alt, shift, win) = crate::modifiers();
            Some(Recorded::Key(Hotkey { ctrl, alt, shift, win, trigger: t }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_display() {
        let h: Hotkey = " ctrl + Alt+q ".parse().unwrap();
        assert!(h.ctrl && h.alt && !h.shift);
        assert_eq!(h.trigger, Trigger::Key(b'Q' as u16));
        assert_eq!(h.to_string(), "Ctrl+Alt+Q");
        assert_eq!("Mouse5".parse::<Hotkey>().unwrap().trigger, Trigger::MouseX2);
        assert_eq!("Numpad0".parse::<Hotkey>().unwrap().trigger, Trigger::Key(0x60));
        for bad in ["", "Ctrl+", "Foo", "Q+F9", "F25"] {
            assert!(bad.parse::<Hotkey>().is_err(), "{bad}");
        }
    }

    #[test]
    fn key_name_roundtrip() {
        for vk in 0u16..=0xFF {
            if let Some(name) = key_name(Trigger::Key(vk)) {
                assert_eq!(parse_key(&name), Some(Trigger::Key(vk)), "vk {vk:#x} -> {name}");
            }
        }
        for t in [Trigger::MouseMiddle, Trigger::MouseX1, Trigger::MouseX2] {
            assert_eq!(parse_key(&key_name(t).unwrap()), Some(t));
        }
    }
}
