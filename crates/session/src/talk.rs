//! 每幀決定要對哪些頻道說話（純邏輯，方便測試），以及開／關麥提示音。

use crate::pipeline::SLOTS;

/// 講完後繼續送出的時間（偵測說話）
pub const VAD_HANGOVER_FRAMES: u8 = 15; // 300 ms
/// 放開 PTT 後繼續送出的時間，避免句尾被切
pub const PTT_HANGOVER_FRAMES: u8 = 8; // 160 ms

/// 這一幀的輸入（遮罩都是「我的槽位」）
#[derive(Clone, Copy, Debug, Default)]
pub struct TalkInputs {
    /// 說話模式「一直開麥」的頻道
    pub open: u64,
    /// 說話模式「偵測說話」的頻道
    pub vad: u64,
    /// 所有會說話（talk != Off）的頻道，全域廣播鍵用
    pub all: u64,
    /// 這一幀偵測到人聲
    pub voiced: bool,
    /// 目前按住的 PTT 鍵所對應的頻道
    pub ptt_held: u64,
    /// 按住全域廣播鍵
    pub broadcast: bool,
    pub muted: bool,
}

/// 跨幀的狀態：各槽位的 PTT 延長計數、VAD 延長計數
#[derive(Clone, Debug)]
pub struct TalkState {
    ptt_hang: [u8; SLOTS],
    vad_hang: u8,
}

impl Default for TalkState {
    fn default() -> Self {
        Self { ptt_hang: [0; SLOTS], vad_hang: 0 }
    }
}

impl TalkState {
    /// 算出這一幀的 talk_mask
    pub fn step(&mut self, i: &TalkInputs) -> u64 {
        self.vad_hang = if i.voiced { VAD_HANGOVER_FRAMES } else { self.vad_hang.saturating_sub(1) };
        // PTT 與廣播鍵不經過 VAD；每個槽位各自延長，放開其中一個鍵不影響其他
        let held = i.ptt_held | if i.broadcast { i.all } else { 0 };
        let mut ptt = 0u64;
        for (s, h) in self.ptt_hang.iter_mut().enumerate() {
            if held & (1 << s) != 0 {
                *h = PTT_HANGOVER_FRAMES;
            } else {
                *h = h.saturating_sub(1);
            }
            if *h > 0 {
                ptt |= 1 << s;
            }
        }
        if i.muted {
            return 0;
        }
        let mut mask = i.open | ptt;
        if self.vad_hang > 0 {
            mask |= i.vad;
        }
        mask
    }
}

// ---------------------------------------------------------------- 提示音

/// 提示音種類
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cue {
    /// 開始按鍵說話；音高依頻道（槽位）不同
    PttOn(u8),
    PttOff(u8),
    /// 全域廣播
    BroadcastOn,
    BroadcastOff,
    Muted,
    Unmuted,
}

impl Cue {
    /// 兩個音符的頻率（Hz）：上升 = 開，下降 = 關
    fn notes(self) -> [f32; 2] {
        // 各頻道相差全音，8 個一循環
        let base = |slot: u8| 523.25 * 2f32.powf((slot % 8) as f32 * 2.0 / 12.0);
        match self {
            Cue::PttOn(s) => [base(s), base(s) * 1.26],
            Cue::PttOff(s) => [base(s) * 1.26, base(s)],
            Cue::BroadcastOn => [1046.5, 1318.5],
            Cue::BroadcastOff => [1318.5, 1046.5],
            Cue::Muted => [440.0, 330.0],
            Cue::Unmuted => [330.0, 440.0],
        }
    }
}

const NOTE_SAMPLES: usize = 48_000 * 45 / 1000; // 45 ms
const FADE: usize = 48_000 * 5 / 1000;
const TONE_GAIN: f32 = 0.12;

/// 正在播放的提示音（播放執行緒持有）
#[derive(Default)]
pub struct TonePlayer {
    cur: Option<([f32; 2], usize)>,
    phase: f32,
}

impl TonePlayer {
    /// 開始播放（打斷正在播的）
    pub fn play(&mut self, cue: Cue) {
        self.cur = Some((cue.notes(), 0));
        self.phase = 0.0;
    }

    /// 混進交錯立體聲的 buffer
    pub fn mix_into(&mut self, stereo: &mut [f32], gain: f32) {
        let Some((notes, pos)) = self.cur.as_mut() else { return };
        for frame in stereo.chunks_exact_mut(2) {
            if *pos >= NOTE_SAMPLES * 2 {
                self.cur = None;
                return;
            }
            let (note, i) = (*pos / NOTE_SAMPLES, *pos % NOTE_SAMPLES);
            let env = (i.min(NOTE_SAMPLES - 1 - i) as f32 / FADE as f32).min(1.0);
            self.phase = (self.phase + notes[note] / 48_000.0).fract();
            let v = (self.phase * std::f32::consts::TAU).sin() * env * TONE_GAIN * gain;
            frame[0] += v;
            frame[1] += v;
            *pos += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(st: &mut TalkState, i: TalkInputs, frames: usize) -> u64 {
        let mut m = 0;
        for _ in 0..frames {
            m = st.step(&i);
        }
        m
    }

    #[test]
    fn open_vad_and_mute() {
        let mut st = TalkState::default();
        let i = TalkInputs { open: 0b001, vad: 0b010, all: 0b111, ..Default::default() };
        assert_eq!(st.step(&i), 0b001, "沒說話：只有常開");
        assert_eq!(st.step(&TalkInputs { voiced: true, ..i }), 0b011);
        // 講完後延長
        assert_eq!(run(&mut st, i, VAD_HANGOVER_FRAMES as usize - 1), 0b011);
        assert_eq!(st.step(&i), 0b001);
        assert_eq!(st.step(&TalkInputs { voiced: true, muted: true, ..i }), 0, "靜音全部不送");
    }

    #[test]
    fn ptt_hangover_per_slot() {
        let mut st = TalkState::default();
        let both = TalkInputs { ptt_held: 0b1100, all: 0b1111, ..Default::default() };
        assert_eq!(st.step(&both), 0b1100, "PTT 不經過 VAD");
        // 放開槽位 2 的鍵，槽位 3 還按著
        let one = TalkInputs { ptt_held: 0b1000, ..both };
        assert_eq!(run(&mut st, one, PTT_HANGOVER_FRAMES as usize - 1), 0b1100, "放開後短暫延長");
        assert_eq!(st.step(&one), 0b1000);
        let none = TalkInputs { ptt_held: 0, ..both };
        assert_eq!(run(&mut st, none, PTT_HANGOVER_FRAMES as usize), 0);
    }

    #[test]
    fn broadcast_and_mute() {
        let mut st = TalkState::default();
        let i = TalkInputs { vad: 0b01, all: 0b111, broadcast: true, ..Default::default() };
        assert_eq!(st.step(&i), 0b111, "廣播鍵：所有會說話的頻道");
        assert_eq!(st.step(&TalkInputs { muted: true, ..i }), 0, "靜音時廣播也不送");
        let released = TalkInputs { broadcast: false, ..i };
        assert_eq!(st.step(&released), 0b111, "放開後延長");
        assert_eq!(run(&mut st, released, PTT_HANGOVER_FRAMES as usize), 0);
    }

    #[test]
    fn tone_ends() {
        let mut t = TonePlayer::default();
        t.play(Cue::PttOn(3));
        let mut buf = vec![0f32; 960 * 2];
        let mut peak = 0f32;
        for _ in 0..5 {
            buf.fill(0.0);
            t.mix_into(&mut buf, 1.0);
            peak = peak.max(buf.iter().fold(0f32, |m, v| m.max(v.abs())));
        }
        assert!(peak > 0.1 && peak <= TONE_GAIN + 1e-6);
        assert!(t.cur.is_none(), "90 ms 後結束");
    }
}
