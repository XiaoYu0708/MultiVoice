//! 邀請票（Room Ticket，PLAN.md §4.1）：`mv1` + base32(postcard(Ticket))

use data_encoding::BASE32_NOPAD;
use serde::{Deserialize, Serialize};

use crate::NodeId;

const PREFIX: &str = "mv1";

/// 房間 ID：建房時隨機產生，永久不變
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RoomId(pub [u8; 32]);

/// 房間密鑰：證明「我有被邀請」。重新產生即可讓舊邀請票失效。
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomSecret(pub [u8; 32]);

impl std::fmt::Debug for RoomSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoomSecret(…)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    pub room_id: RoomId,
    pub secret: RoomSecret,
    /// 最近已知的成員（含當下房主與候補房主），加入時依序撥號
    pub bootstrap: Vec<NodeId>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TicketError {
    Prefix,
    Base32,
    Decode,
    NoBootstrap,
}

impl std::fmt::Display for TicketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TicketError::Prefix => "邀請票格式錯誤（應以 mv1 開頭）",
            TicketError::Base32 => "邀請票內容不是有效的 base32",
            TicketError::Decode => "邀請票內容損毀",
            TicketError::NoBootstrap => "邀請票裡沒有可連線的成員",
        })
    }
}

impl std::error::Error for TicketError {}

/// 使用者貼上的文字 → 邀請票字串：去掉空白，網頁版邀請連結（`https://…/#mv1…`）只取 `#` 後面
pub fn clean_input(s: &str) -> String {
    let s: String = s.split_whitespace().collect();
    match s.rsplit_once('#') {
        Some((_, t)) => t.to_string(),
        None => s,
    }
}

impl std::fmt::Display for Ticket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let bytes = postcard::to_allocvec(self).expect("Ticket 序列化不會失敗");
        write!(f, "{PREFIX}{}", BASE32_NOPAD.encode(&bytes).to_lowercase())
    }
}

impl std::str::FromStr for Ticket {
    type Err = TicketError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let body = s.trim().strip_prefix(PREFIX).ok_or(TicketError::Prefix)?;
        let bytes = BASE32_NOPAD
            .decode(body.to_uppercase().as_bytes())
            .map_err(|_| TicketError::Base32)?;
        let t: Ticket = postcard::from_bytes(&bytes).map_err(|_| TicketError::Decode)?;
        if t.bootstrap.is_empty() {
            return Err(TicketError::NoBootstrap);
        }
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Ticket {
        Ticket {
            room_id: RoomId([1; 32]),
            secret: RoomSecret([2; 32]),
            bootstrap: vec![[3; 32], [4; 32]],
        }
    }

    #[test]
    fn roundtrip() {
        let t = sample();
        let s = t.to_string();
        assert!(s.starts_with("mv1"));
        assert!(s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        assert_eq!(s.parse::<Ticket>().unwrap(), t);
        // 大小寫不敏感、容許前後空白（從聊天軟體複製時常見）
        assert_eq!(format!("  {}\n", s.to_uppercase().replacen("MV1", "mv1", 1)).parse::<Ticket>().unwrap(), t);
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!("xx1abc".parse::<Ticket>(), Err(TicketError::Prefix));
        assert_eq!("mv1!!!".parse::<Ticket>(), Err(TicketError::Base32));
        assert_eq!("mv1aaaa".parse::<Ticket>(), Err(TicketError::Decode));
        let mut t = sample();
        t.bootstrap.clear();
        assert_eq!(t.to_string().parse::<Ticket>(), Err(TicketError::NoBootstrap));
    }

    #[test]
    fn clean_web_link() {
        let s = sample().to_string();
        assert_eq!(clean_input(&format!("https://voice.example.com/#{s}")), s);
        assert_eq!(clean_input(&format!(" {} 
{}", &s[..10], &s[10..])), s, "聊天軟體斷行");
        assert_eq!(clean_input(&s), s);
    }

    #[test]
    fn debug_does_not_leak_secret() {
        assert!(!format!("{:?}", sample()).contains("2, 2"));
    }
}
