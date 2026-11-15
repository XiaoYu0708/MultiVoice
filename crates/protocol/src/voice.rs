//! 語音 datagram（PLAN.md §4.3），手寫二進位標頭、小端序。
//!
//! 成員 → 房主（6 bytes 標頭）：
//! ```text
//! u8  kind        0x01 voice | 0x02 end-of-stream
//! u8  target_id   預先註冊的 talk target（M1：0 = 所在的所有頻道）
//! u32 seq         每幀 +1；換房主後不重置
//! [..] opus payload
//! ```
//! 房主 → 成員（15 bytes 標頭）：
//! ```text
//! u8  kind
//! u16 sender_idx
//! u32 seq
//! u64 via_mask    接收端連線的頻道槽位遮罩
//! [..] opus payload（原封不動）
//! ```

use crate::SessionIdx;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Voice = 0x01,
    EndOfStream = 0x02,
}

impl Kind {
    fn from_u8(b: u8) -> Option<Self> {
        match b {
            0x01 => Some(Kind::Voice),
            0x02 => Some(Kind::EndOfStream),
            _ => None,
        }
    }
}

pub const UP_HEADER: usize = 6;
pub const DOWN_HEADER: usize = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoiceUp<'a> {
    pub kind: Kind,
    pub target_id: u8,
    pub seq: u32,
    pub payload: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoiceDown<'a> {
    pub kind: Kind,
    pub sender_idx: SessionIdx,
    pub seq: u32,
    pub via_mask: u64,
    pub payload: &'a [u8],
}

impl<'a> VoiceUp<'a> {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.clear();
        out.reserve(UP_HEADER + self.payload.len());
        out.push(self.kind as u8);
        out.push(self.target_id);
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(self.payload);
    }

    pub fn parse(b: &'a [u8]) -> Option<Self> {
        if b.len() < UP_HEADER {
            return None;
        }
        Some(Self {
            kind: Kind::from_u8(b[0])?,
            target_id: b[1],
            seq: u32::from_le_bytes(b[2..6].try_into().ok()?),
            payload: &b[UP_HEADER..],
        })
    }

    /// 房主轉發用：把上行封包改寫成下行封包
    pub fn to_down(&self, sender_idx: SessionIdx, via_mask: u64) -> VoiceDown<'a> {
        VoiceDown { kind: self.kind, sender_idx, seq: self.seq, via_mask, payload: self.payload }
    }
}

impl<'a> VoiceDown<'a> {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.clear();
        out.reserve(DOWN_HEADER + self.payload.len());
        out.push(self.kind as u8);
        out.extend_from_slice(&self.sender_idx.to_le_bytes());
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.via_mask.to_le_bytes());
        out.extend_from_slice(self.payload);
    }

    pub fn parse(b: &'a [u8]) -> Option<Self> {
        if b.len() < DOWN_HEADER {
            return None;
        }
        Some(Self {
            kind: Kind::from_u8(b[0])?,
            sender_idx: u16::from_le_bytes(b[1..3].try_into().ok()?),
            seq: u32::from_le_bytes(b[3..7].try_into().ok()?),
            via_mask: u64::from_le_bytes(b[7..15].try_into().ok()?),
            payload: &b[DOWN_HEADER..],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_roundtrip_and_header_size() {
        let p = [0xAB; 80];
        let up = VoiceUp { kind: Kind::Voice, target_id: 3, seq: 0xDEADBEEF, payload: &p };
        let mut buf = Vec::new();
        up.encode(&mut buf);
        assert_eq!(buf.len(), UP_HEADER + 80);
        assert_eq!(VoiceUp::parse(&buf), Some(up));
    }

    #[test]
    fn host_rewrite_to_down() {
        let p = [1, 2, 3];
        let up = VoiceUp { kind: Kind::EndOfStream, target_id: 0, seq: 42, payload: &p };
        let down = up.to_down(7, 0b101);
        let mut buf = Vec::new();
        down.encode(&mut buf);
        assert_eq!(buf.len(), DOWN_HEADER + 3);
        let d = VoiceDown::parse(&buf).unwrap();
        assert_eq!((d.kind, d.sender_idx, d.seq, d.via_mask, d.payload), (Kind::EndOfStream, 7, 42, 0b101, &p[..]));
    }

    #[test]
    fn rejects_short_and_unknown_kind() {
        assert_eq!(VoiceUp::parse(&[1, 0, 0, 0, 0]), None);
        assert_eq!(VoiceUp::parse(&[9, 0, 0, 0, 0, 0]), None);
        assert_eq!(VoiceDown::parse(&[1; 14]), None);
    }
}
