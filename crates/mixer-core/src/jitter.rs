//! 單一說話者的自適應 jitter buffer。
//!
//! - `push`：網路收到封包時呼叫（亂序、重複、遲到都可以）
//! - `pull`：播放端每 20 ms 呼叫一次，告訴解碼器這一幀該怎麼產生
//!
//! 緩衝深度依 RFC 3550 式的到達抖動估計自動調整，只在一段話開始時套用，避免中途變動造成破音。

use std::collections::BTreeMap;

use crate::FRAME_US;

/// 播放端這一幀該做什麼
#[derive(Debug, PartialEq, Eq)]
pub enum Pull {
    /// 沒有在說話（或還在累積緩衝），輸出靜音、不需解碼
    Silent,
    /// 正常解碼這個封包
    Decode(Vec<u8>),
    /// 這一幀遺失，但下一包已到：用下一包內含的 FEC 復原這一幀
    Fec(Vec<u8>),
    /// 這一幀遺失且無法復原：讓解碼器做封包遺失補償
    Plc,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub decoded: u64,
    pub fec: u64,
    pub plc: u64,
    /// 到得太晚（該幀已播放過）而丟棄
    pub late: u64,
    /// 緩衝過深而跳過（延遲累積或時脈漂移）
    pub skipped: u64,
    pub duplicates: u64,
}

#[derive(Debug)]
enum State {
    Idle,
    Buffering { since_us: u64 },
    Playing { next: u32, missing_run: u32 },
}

/// 最少 / 最多緩衝幾幀
const MIN_TARGET: u32 = 2;
const MAX_TARGET: u32 = 8;
/// 緩衝超過 target + 這麼多幀時跳過
const OVERFLOW_SLACK: u32 = 4;
/// 連續缺這麼多幀（100 ms）且沒有後續封包，視為對方已停止說話
const MAX_MISSING_RUN: u32 = 5;
/// 保護上限，防止惡意或異常的大量封包
const MAX_PACKETS: usize = 64;

#[derive(Debug)]
pub struct JitterBuffer {
    packets: BTreeMap<u32, Vec<u8>>,
    eos: Option<u32>,
    state: State,
    target: u32,
    /// 到達抖動估計（µs）
    jitter_us: f64,
    last_arrival: Option<(u64, u32)>,
    pub stats: Stats,
}

impl Default for JitterBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl JitterBuffer {
    pub fn new() -> Self {
        Self {
            packets: BTreeMap::new(),
            eos: None,
            state: State::Idle,
            target: MIN_TARGET,
            jitter_us: 0.0,
            last_arrival: None,
            stats: Stats::default(),
        }
    }

    /// 目前的目標緩衝深度（ms）
    pub fn target_ms(&self) -> u64 {
        self.target as u64 * FRAME_US / 1000
    }

    pub fn jitter_ms(&self) -> f64 {
        self.jitter_us / 1000.0
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    pub fn push(&mut self, seq: u32, payload: Vec<u8>, now_us: u64) {
        if matches!(self.state, State::Idle) {
            // 新的一段話：說話之間的停頓不是網路抖動，重設到達時間的基準
            self.last_arrival = Some((now_us, seq));
        } else {
            self.note_arrival(seq, now_us);
        }
        if let State::Playing { next, .. } = self.state {
            if seq < next {
                self.stats.late += 1;
                return;
            }
        }
        if self.packets.insert(seq, payload).is_some() {
            self.stats.duplicates += 1;
        }
        while self.packets.len() > MAX_PACKETS {
            self.packets.pop_first();
        }
        if matches!(self.state, State::Idle) {
            self.state = State::Buffering { since_us: now_us };
        }
    }

    /// 對方送出 end-of-stream：`seq` 這一幀之後不再有聲音（直到下一段話）
    pub fn push_eos(&mut self, seq: u32, now_us: u64) {
        let _ = now_us;
        self.eos = Some(seq);
    }

    fn note_arrival(&mut self, seq: u32, now_us: u64) {
        if let Some((t0, s0)) = self.last_arrival {
            if seq > s0 {
                let expected = (seq - s0) as f64 * FRAME_US as f64;
                let actual = now_us.saturating_sub(t0) as f64;
                let d = (actual - expected).abs();
                self.jitter_us += (d - self.jitter_us) / 16.0;
            } else {
                return; // 亂序的舊封包不更新基準
            }
        }
        self.last_arrival = Some((now_us, seq));
    }

    /// 依抖動估計計算目標深度：約 3 倍抖動 + 5 ms 安全邊際
    fn desired_target(&self) -> u32 {
        let need_us = 3.0 * self.jitter_us + 5_000.0;
        let frames = (need_us / FRAME_US as f64).ceil() as u32 + 1;
        frames.clamp(MIN_TARGET, MAX_TARGET)
    }

    pub fn pull(&mut self, now_us: u64) -> Pull {
        match self.state {
            State::Idle => Pull::Silent,
            State::Buffering { since_us } => {
                self.target = self.desired_target();
                let waited = now_us.saturating_sub(since_us);
                let enough = self.packets.len() as u32 >= self.target;
                if !(enough || waited >= self.target as u64 * FRAME_US) {
                    return Pull::Silent;
                }
                let Some((&first, _)) = self.packets.first_key_value() else {
                    self.state = State::Idle;
                    return Pull::Silent;
                };
                self.state = State::Playing { next: first, missing_run: 0 };
                self.pull_playing()
            }
            State::Playing { .. } => self.pull_playing(),
        }
    }

    fn pull_playing(&mut self) -> Pull {
        let State::Playing { mut next, mut missing_run } = self.state else { unreachable!() };

        // 一段話結束
        if self.eos.is_some_and(|e| next >= e) && !self.packets.contains_key(&next) {
            let e = self.eos.take().unwrap();
            self.packets.retain(|&s, _| s > e);
            self.state = if self.packets.is_empty() { State::Idle } else { State::Buffering { since_us: 0 } };
            return Pull::Silent;
        }

        // 緩衝過深：跳到只留 target 幀
        if let Some((&last, _)) = self.packets.last_key_value() {
            let depth = last.saturating_sub(next) + 1;
            if depth > self.target + OVERFLOW_SLACK {
                let new_next = last + 1 - self.target;
                let before = self.packets.len();
                self.packets.retain(|&s, _| s >= new_next);
                self.stats.skipped += (before - self.packets.len()) as u64;
                next = new_next;
            }
        }

        let out = if let Some(p) = self.packets.remove(&next) {
            missing_run = 0;
            self.stats.decoded += 1;
            Pull::Decode(p)
        } else if let Some(p) = self.packets.get(&(next + 1)) {
            missing_run += 1;
            self.stats.fec += 1;
            Pull::Fec(p.clone())
        } else {
            missing_run += 1;
            if missing_run > MAX_MISSING_RUN {
                // 對方停止說話但 end-of-stream 遺失了；若後面還有封包就重新對齊
                match self.packets.first_key_value() {
                    Some((&s, _)) => {
                        self.state = State::Playing { next: s, missing_run: 0 };
                        return self.pull_playing();
                    }
                    None => {
                        self.state = State::Idle;
                        return Pull::Silent;
                    }
                }
            }
            self.stats.plc += 1;
            Pull::Plc
        };
        self.state = State::Playing { next: next + 1, missing_run };
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: u64 = FRAME_US;

    fn pkt(seq: u32) -> Vec<u8> {
        seq.to_le_bytes().to_vec()
    }

    fn seq_of(p: &Pull) -> Option<(char, u32)> {
        match p {
            Pull::Decode(v) => Some(('D', u32::from_le_bytes(v[..4].try_into().unwrap()))),
            Pull::Fec(v) => Some(('F', u32::from_le_bytes(v[..4].try_into().unwrap()))),
            Pull::Plc => Some(('P', 0)),
            Pull::Silent => None,
        }
    }

    /// 封包每 20 ms 準時到達，播放端每 20 ms 拉一次
    fn run(jb: &mut JitterBuffer, arrivals: &[(u64, u32)], pulls: u64) -> Vec<Option<(char, u32)>> {
        let mut out = Vec::new();
        let mut a = arrivals.iter().peekable();
        for i in 0..pulls {
            let now = i * F;
            while let Some(&&(t, s)) = a.peek() {
                if t <= now {
                    jb.push(s, pkt(s), t);
                    a.next();
                } else {
                    break;
                }
            }
            out.push(seq_of(&jb.pull(now)));
        }
        out
    }

    #[test]
    fn in_order_plays_all_after_buffering() {
        let mut jb = JitterBuffer::new();
        let arrivals: Vec<_> = (0..10).map(|s| (s as u64 * F, 100 + s)).collect();
        let out = run(&mut jb, &arrivals, 14);
        let decoded: Vec<_> = out.iter().flatten().filter(|(k, _)| *k == 'D').map(|(_, s)| *s).collect();
        assert_eq!(decoded, (100..110).collect::<Vec<_>>());
        // 開頭先靜音累積 target 幀
        assert_eq!(out[0], None);
        assert_eq!(jb.stats.decoded, 10);
    }

    #[test]
    fn reorder_is_fixed() {
        let mut jb = JitterBuffer::new();
        for s in [0u32, 2, 1, 3] {
            jb.push(s, pkt(s), 0);
        }
        let got: Vec<_> = (0..4).map(|i| seq_of(&jb.pull(i * F)).unwrap()).collect();
        assert_eq!(got, vec![('D', 0), ('D', 1), ('D', 2), ('D', 3)]);
    }

    #[test]
    fn single_loss_uses_fec_double_loss_uses_plc() {
        let mut jb = JitterBuffer::new();
        let arrivals: Vec<_> = [0u32, 1, 3, 6, 7].iter().map(|&s| (s as u64 * F, s)).collect();
        let got = run(&mut jb, &arrivals, 9);
        // 2 遺失 → 用 3 的 FEC；4 遺失且 5 也沒到 → PLC；5 遺失 → 用 6 的 FEC
        let want = [None, Some(('D', 0)), Some(('D', 1)), Some(('F', 3)), Some(('D', 3)), Some(('P', 0)), Some(('F', 6)), Some(('D', 6)), Some(('D', 7))];
        assert_eq!(got, want);
        assert_eq!((jb.stats.fec, jb.stats.plc), (2, 1));
    }

    #[test]
    fn late_packet_is_dropped() {
        let mut jb = JitterBuffer::new();
        for s in [0u32, 1, 3] {
            jb.push(s, pkt(s), 0);
        }
        for i in 0..3 {
            jb.pull(i * F);
        }
        jb.push(2, pkt(2), 3 * F); // 2 已經用 FEC 播過
        assert_eq!(jb.stats.late, 1);
    }

    #[test]
    fn end_of_stream_then_new_talkspurt() {
        let mut jb = JitterBuffer::new();
        for s in 0..3u32 {
            jb.push(s, pkt(s), 0);
        }
        jb.push_eos(3, 0);
        let first: Vec<_> = (0..5).map(|i| seq_of(&jb.pull(i * F))).collect();
        assert_eq!(first, vec![Some(('D', 0)), Some(('D', 1)), Some(('D', 2)), None, None]);
        assert!(!jb.is_active());
        // 下一段話從 seq 3 繼續（送端只在有送出時才遞增）
        for s in 3..6u32 {
            jb.push(s, pkt(s), 10 * F);
        }
        let second: Vec<_> = (10..13).map(|i| seq_of(&jb.pull(i * F))).flatten().collect();
        assert_eq!(second, vec![('D', 3), ('D', 4), ('D', 5)]);
        assert_eq!(jb.target, MIN_TARGET, "說話之間的停頓不應被當成抖動");
    }

    #[test]
    fn missing_tail_without_eos_goes_idle() {
        let mut jb = JitterBuffer::new();
        for s in 0..2u32 {
            jb.push(s, pkt(s), 0);
        }
        let out: Vec<_> = (0..10).map(|i| seq_of(&jb.pull(i * F))).collect();
        assert_eq!(out[..2], [Some(('D', 0)), Some(('D', 1))]);
        assert_eq!(out[2..7].iter().filter(|p| **p == Some(('P', 0))).count(), 5);
        assert!(out[7..].iter().all(|p| p.is_none()));
        assert!(!jb.is_active());
    }

    #[test]
    fn burst_is_trimmed_to_bound_latency() {
        let mut jb = JitterBuffer::new();
        for s in 0..2u32 {
            jb.push(s, pkt(s), 0);
        }
        jb.pull(0);
        // 突然一次到達 20 個（例如網路卡住後一起送達）
        for s in 2..22u32 {
            jb.push(s, pkt(s), F);
        }
        let p = seq_of(&jb.pull(F)).unwrap();
        assert!(p.1 >= 22 - jb.target - 1, "應跳到接近最新：{p:?}");
        assert!(jb.stats.skipped > 0);
    }

    #[test]
    fn target_grows_with_jitter() {
        let mut calm = JitterBuffer::new();
        let mut shaky = JitterBuffer::new();
        let mut x = 12345u64;
        for s in 0..200u32 {
            calm.push(s, pkt(s), s as u64 * F);
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
            let wobble = (x >> 33) % 40_000; // 0–40 ms 隨機延遲
            shaky.push(s, pkt(s), s as u64 * F + wobble);
        }
        calm.pull(0);
        shaky.pull(0);
        assert_eq!(calm.target, MIN_TARGET);
        assert!(shaky.target > calm.target, "抖動 {:.1} ms → target {}", shaky.jitter_ms(), shaky.target);
    }
}
