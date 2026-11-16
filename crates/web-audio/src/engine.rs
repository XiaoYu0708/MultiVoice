//! 音訊引擎：每次 AudioWorklet `process()` 呼叫一次（128 取樣），48 kHz。
//!
//! - 擷取：湊滿 20 ms（960 取樣）→ 降噪（RNNoise，與桌面版相同的 [`mixer_core::denoise`]）→
//!   說話判斷（RNNoise 人聲機率 + 降噪後音量門檻）→ Opus 編碼 →
//!   依說話遮罩（[`mixer_core::talk::TalkState`]，與桌面版相同）決定送給哪些頻道 → 放進 outbox 給 JS 送出
//! - 播放：每 20 ms 從各說話者的 jitter buffer 取一幀解碼（含 FEC／PLC），依經由的頻道套用音量／定位／優先，
//!   混成立體聲
//! - 時鐘：播放的取樣數（不依賴 JS 計時器，背景分頁也準）
//!
//! 回音消除、自動增益交給瀏覽器的 getUserMedia（手機常用喇叭，瀏覽器的回音消除正好補上）。
//! 降噪開著時 JS 會關掉瀏覽器自己的降噪（避免降兩次讓人聲變悶）；關掉降噪時改回用瀏覽器的。

use std::collections::BTreeMap;

use mixer_core::denoise::Denoiser;
use mixer_core::jitter::{JitterBuffer, Pull};
use mixer_core::mix::{mix_into_stereo, soft_clip};
use mixer_core::route::{ChannelMix, DUCK_GAIN, route};
use mixer_core::talk::{Cue, SLOTS, TalkInputs, TalkState, TonePlayer};
use protocol::SessionIdx;
use protocol::voice::{Kind, VoiceDown, VoiceUp};

use crate::FRAME_SAMPLES;
use crate::opus::{Decoder, Encoder, MAX_PACKET};

/// 人聲機率門檻（同桌面版 audio::frontend）
const VAD_PROB: f32 = 0.6;
pub const DEFAULT_GATE_DBFS: f32 = -50.0;
const BITRATE: i32 = 32_000;
/// 「正在說話」的顯示：最近這麼久內收到過聲音
const SPEAKING_US: u64 = 300_000;
/// 說話者這麼久沒聲音就清掉（解碼器、jitter buffer）
const FORGET_US: u64 = 60_000_000;

pub fn rms_db(x: &[f32]) -> f32 {
    let rms = (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt();
    20.0 * rms.max(1e-9).log10()
}

struct Speaker {
    jb: JitterBuffer,
    dec: Option<Decoder>,
    buf: Vec<f32>,
    /// 最近一包的 via_mask（經由我的哪些槽位聽到）
    via: u64,
    last_us: u64,
}

#[derive(Default, Clone, Copy)]
pub struct Totals {
    pub decoded: u64,
    pub fec: u64,
    pub plc: u64,
    pub late: u64,
}

pub struct Engine {
    // 設定（JS 呼叫 set_* 改）
    pub slots: [Option<ChannelMix>; SLOTS],
    pub open_mask: u64,
    pub vad_mask: u64,
    pub ptt_mask: u64,
    pub muted: bool,
    pub deafened: bool,
    pub cues: bool,
    pub volume: f32,
    pub gate_db: f32,
    /// 降噪（RNNoise）：開著時送出降噪後的聲音；關掉時仍用 RNNoise 算人聲機率，但聲音不改
    pub denoise: bool,
    /// 我聽某位成員的音量倍率（長按／右鍵調整；沒有 = 1.0）
    pub user_gain: BTreeMap<SessionIdx, f32>,
    // 擷取
    enc: Option<Encoder>,
    denoiser: Denoiser,
    /// 關掉降噪時給 RNNoise 算人聲機率用的副本
    scratch: Vec<f32>,
    cap: Vec<f32>,
    pkt: Vec<u8>,
    prev: Option<Vec<u8>>,
    talk: TalkState,
    active: bool,
    seq: u32,
    prev_ptt: u64,
    prev_muted: bool,
    /// 第一幀只記下目前狀態，不播提示音（進房時預設靜音，不要一進來就響）
    primed: bool,
    /// 要送出的封包：重複的 `[u16 長度][上行封包]`
    pub outbox: Vec<u8>,
    // 播放
    samples: u64,
    speakers: BTreeMap<SessionIdx, Speaker>,
    mix: Vec<f32>,
    mix_pos: usize,
    tones: TonePlayer,
    /// 已移除的說話者累計的統計（讓總數不會因為清掉說話者而變小）
    gone: Totals,
    // 狀態
    pub level_db: f32,
    pub voiced: bool,
    pub tx_mask: u64,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn new() -> Self {
        Self {
            slots: [None; SLOTS],
            open_mask: 0,
            vad_mask: 0,
            ptt_mask: 0,
            muted: false,
            deafened: false,
            cues: true,
            volume: 1.0,
            gate_db: DEFAULT_GATE_DBFS,
            denoise: true,
            user_gain: BTreeMap::new(),
            enc: Encoder::new(BITRATE),
            denoiser: Denoiser::new(),
            scratch: Vec::with_capacity(FRAME_SAMPLES),
            cap: Vec::with_capacity(FRAME_SAMPLES),
            pkt: vec![0; MAX_PACKET],
            prev: None,
            talk: TalkState::default(),
            active: false,
            seq: 0,
            prev_ptt: 0,
            prev_muted: false,
            primed: false,
            outbox: Vec::new(),
            samples: 0,
            speakers: BTreeMap::new(),
            mix: vec![0.0; FRAME_SAMPLES * 2],
            mix_pos: FRAME_SAMPLES,
            tones: TonePlayer::default(),
            gone: Totals::default(),
            level_db: -100.0,
            voiced: false,
            tx_mask: 0,
        }
    }

    fn now_us(&self) -> u64 {
        self.samples * 1_000_000 / 48_000
    }

    /// 處理一段音訊：`input` = 麥克風（None = 沒開麥克風），輸出寫進 `out_l` / `out_r`（同長度）
    pub fn process(&mut self, input: Option<&[f32]>, out_l: &mut [f32], out_r: &mut [f32]) {
        if let Some(inp) = input {
            for &s in inp {
                self.cap.push(s);
                if self.cap.len() == FRAME_SAMPLES {
                    self.capture_frame();
                    self.cap.clear();
                }
            }
        }
        for (l, r) in out_l.iter_mut().zip(out_r.iter_mut()) {
            if self.mix_pos == FRAME_SAMPLES {
                self.render_frame();
                self.mix_pos = 0;
            }
            *l = self.mix[self.mix_pos * 2];
            *r = self.mix[self.mix_pos * 2 + 1];
            self.mix_pos += 1;
            self.samples += 1;
        }
    }

    /// 麥克風關掉：正在送出就補一個 end-of-stream
    pub fn mic_stopped(&mut self) {
        self.cap.clear();
        if self.active {
            self.send(Kind::EndOfStream, 0, None);
            self.active = false;
            self.tx_mask = 0;
        }
    }

    fn send(&mut self, kind: Kind, mask: u64, payload: Option<&[u8]>) {
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        let mut b = Vec::new();
        VoiceUp { kind, talk_mask: mask, seq, payload: payload.unwrap_or(&[]) }.encode(&mut b);
        self.outbox.extend_from_slice(&(b.len() as u16).to_le_bytes());
        self.outbox.extend_from_slice(&b);
    }

    fn cue(&mut self, c: Cue) {
        if self.cues {
            self.tones.play(c);
        }
    }

    fn capture_frame(&mut self) {
        let prob = if self.denoise {
            self.denoiser.process(&mut self.cap)
        } else {
            self.scratch.clear();
            self.scratch.extend_from_slice(&self.cap);
            self.denoiser.process(&mut self.scratch)
        };
        self.level_db = rms_db(&self.cap);
        self.voiced = prob >= VAD_PROB && self.level_db >= self.gate_db;
        // 一直編碼，讓編碼器狀態連續；只是決定要不要送
        let Some(enc) = self.enc.as_mut() else { return };
        let Some(n) = enc.encode(&self.cap, &mut self.pkt) else { return };

        let muted = self.muted || self.deafened;
        let ptt = self.ptt_mask;
        let mask = self.talk.step(&TalkInputs {
            open: self.open_mask,
            vad: self.vad_mask,
            voiced: self.voiced,
            ptt_held: ptt,
            muted,
        });
        if !self.primed {
            self.primed = true;
        } else if muted != self.prev_muted {
            self.cue(if muted { Cue::Muted } else { Cue::Unmuted });
        } else if !muted && (ptt == 0) != (self.prev_ptt == 0) {
            let slot = |m: u64| m.trailing_zeros() as u8;
            let c = if ptt != 0 { Cue::PttOn(slot(ptt)) } else { Cue::PttOff(slot(self.prev_ptt)) };
            self.cue(c);
        }
        (self.prev_ptt, self.prev_muted) = (ptt, muted);

        let pkt = self.pkt[..n].to_vec();
        if mask != 0 {
            // 開口時補送前一幀，避免切掉字頭
            if !self.active
                && let Some(p) = self.prev.take()
            {
                self.send(Kind::Voice, mask, Some(&p));
            }
            self.send(Kind::Voice, mask, Some(&pkt));
        } else if self.active {
            self.send(Kind::EndOfStream, 0, None);
        }
        self.active = mask != 0;
        self.tx_mask = mask;
        self.prev = Some(pkt);
    }

    /// 收到一個下行語音封包
    pub fn push_packet(&mut self, data: &[u8]) {
        let Some(v) = VoiceDown::parse(data) else { return };
        let now = self.now_us();
        let sp = self.speakers.entry(v.sender_idx).or_insert_with(|| Speaker {
            jb: JitterBuffer::new(),
            dec: None,
            buf: vec![0.0; FRAME_SAMPLES],
            via: 0,
            last_us: now,
        });
        match v.kind {
            Kind::Voice => {
                sp.jb.push(v.seq, v.payload.to_vec(), now);
                sp.via = v.via_mask;
                sp.last_us = now;
            }
            Kind::EndOfStream => sp.jb.push_eos(v.seq, now),
        }
    }

    fn render_frame(&mut self) {
        self.mix.fill(0.0);
        let now = self.now_us();
        // 很久沒聲音的說話者清掉（統計先累加起來）
        let gone = &mut self.gone;
        self.speakers.retain(|_, sp| {
            let keep = now.saturating_sub(sp.last_us) < FORGET_US || sp.jb.is_active();
            if !keep {
                gone.decoded += sp.jb.stats.decoded;
                gone.fec += sp.jb.stats.fec;
                gone.plc += sp.jb.stats.plc;
                gone.late += sp.jb.stats.late;
            }
            keep
        });
        let mut routed = Vec::new();
        for (&idx, sp) in self.speakers.iter_mut() {
            let pull = sp.jb.pull(now);
            if pull == Pull::Silent {
                continue;
            }
            if sp.dec.is_none() {
                sp.dec = Decoder::new();
            }
            let Some(dec) = sp.dec.as_mut() else { continue };
            // 一定要解碼（讓解碼器狀態連續），即使所在頻道都被調成靜音
            let ok = match pull {
                Pull::Decode(p) => dec.decode(&p, &mut sp.buf),
                Pull::Fec(p) => dec.fec(&p, &mut sp.buf),
                Pull::Plc => dec.plc(&mut sp.buf),
                Pull::Silent => false,
            };
            if !ok {
                continue;
            }
            let default = ChannelMix::default();
            let via = sp.via;
            let r = if via == 0 {
                route([&default])
            } else {
                route((0..SLOTS).filter(|&s| via & (1 << s) != 0).map(|s| self.slots[s].as_ref().unwrap_or(&default)))
            };
            if let Some(r) = r {
                routed.push((idx, r));
            }
        }
        let ducking = routed.iter().any(|(_, r)| r.priority);
        for (idx, r) in &routed {
            let g = if ducking && !r.priority { r.gain * DUCK_GAIN } else { r.gain };
            let g = g * self.user_gain.get(idx).copied().unwrap_or(1.0);
            mix_into_stereo(&mut self.mix, &self.speakers[idx].buf, g, r.pan);
        }
        if self.deafened {
            self.mix.fill(0.0);
        } else if self.volume != 1.0 {
            let v = self.volume;
            self.mix.iter_mut().for_each(|s| *s *= v);
        }
        // 提示音：拒聽時也播，跟著輸出音量
        self.tones.mix_into(&mut self.mix, self.volume);
        soft_clip(&mut self.mix);
    }

    /// 最近有聲音的說話者：(成員, via_mask)
    pub fn speaking(&self) -> Vec<(SessionIdx, u64)> {
        let now = self.now_us();
        self.speakers
            .iter()
            .filter(|(_, sp)| now.saturating_sub(sp.last_us) < SPEAKING_US)
            .map(|(&i, sp)| (i, sp.via))
            .collect()
    }

    pub fn totals(&self) -> Totals {
        let mut t = self.gone;
        for sp in self.speakers.values() {
            t.decoded += sp.jb.stats.decoded;
            t.fec += sp.jb.stats.fec;
            t.plc += sp.jb.stats.plc;
            t.late += sp.jb.stats.late;
        }
        t
    }

    pub fn active(&self) -> bool {
        self.active
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::voice::VoiceUp;

    fn tone(phase: &mut f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|_| {
                *phase += 220.0 / 48_000.0 * std::f32::consts::TAU;
                phase.sin() * amp
            })
            .collect()
    }

    /// outbox → 上行封包清單
    fn packets(e: &mut Engine) -> Vec<(Kind, u64, u32, Vec<u8>)> {
        let mut out = Vec::new();
        let mut b = &e.outbox[..];
        while b.len() >= 2 {
            let n = u16::from_le_bytes([b[0], b[1]]) as usize;
            let v = VoiceUp::parse(&b[2..2 + n]).unwrap();
            out.push((v.kind, v.talk_mask, v.seq, v.payload.to_vec()));
            b = &b[2 + n..];
        }
        e.outbox.clear();
        out
    }

    /// 上行 → 下行（模擬房主轉發）
    fn down(sender: u16, via: u64, kind: Kind, seq: u32, payload: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        VoiceDown { kind, sender_idx: sender, seq, via_mask: via, payload }.encode(&mut b);
        b
    }

    #[test]
    fn open_mic_sends_and_eos_on_stop() {
        let mut e = Engine::new();
        e.open_mask = 0b10;
        let (mut l, mut r) = (vec![0.0; 128], vec![0.0; 128]);
        let mut ph = 0.0;
        for _ in 0..30 {
            e.process(Some(&tone(&mut ph, 0.2, 128)), &mut l, &mut r);
        }
        let p = packets(&mut e);
        // 30 × 128 = 3840 取樣 = 4 幀
        assert_eq!(p.len(), 4, "{:?}", p.iter().map(|x| x.0).collect::<Vec<_>>());
        assert!(p.iter().all(|x| x.0 == Kind::Voice && x.1 == 0b10));
        assert_eq!(p.iter().map(|x| x.2).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
        e.mic_stopped();
        let p = packets(&mut e);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].0, Kind::EndOfStream);
    }

    #[test]
    fn muted_sends_nothing_and_ptt_works() {
        let mut e = Engine::new();
        let (mut l, mut r) = (vec![0.0; 960], vec![0.0; 960]);
        let mut ph = 0.0;
        e.open_mask = 1;
        e.muted = true;
        e.process(Some(&tone(&mut ph, 0.2, 960)), &mut l, &mut r);
        assert!(packets(&mut e).is_empty());
        e.muted = false;
        e.open_mask = 0;
        e.ptt_mask = 0b100;
        e.process(Some(&tone(&mut ph, 0.2, 960)), &mut l, &mut r);
        let p = packets(&mut e);
        // 開口時補送前一幀（靜音時編好的那幀）＋ 這一幀
        assert_eq!(p.len(), 2);
        assert!(p.iter().all(|x| x.1 == 0b100));
    }

    /// 降噪：風扇類噪音開著降噪時音量明顯變小；關掉時不改聲音
    #[test]
    fn denoise_toggle() {
        let run = |on: bool| {
            let mut e = Engine::new();
            e.denoise = on;
            let (mut l, mut r) = (vec![0.0; 960], vec![0.0; 960]);
            let (mut seed, mut lp) = (0x1234_5678_9ABC_DEF0u64, 0f32);
            let mut level = 0.0;
            for _ in 0..100 {
                let x: Vec<f32> = (0..960)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        lp = 0.9 * lp + 0.1 * ((seed >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0);
                        lp * 0.4
                    })
                    .collect();
                e.process(Some(&x), &mut l, &mut r);
                level = e.level_db;
            }
            level
        };
        let (off, on) = (run(false), run(true));
        assert!(off > -40.0, "沒降噪時是原本的音量：{off}");
        // 合成的風扇噪音會被 RNNoise 判成人聲，人聲時混回 30% 原音（約 -10.5 dB），所以這裡只壓低約 8 dB
        assert!(on < off - 6.0, "降噪後應變小：{on} vs {off}");
    }

    /// 兩個引擎：A 一直開麥送給槽位 0 → 模擬房主轉發 → B 播出聲音；B 把那個頻道調成靜音就沒聲音
    #[test]
    fn loopback_through_two_engines() {
        let mut a = Engine::new();
        a.open_mask = 1;
        let mut b = Engine::new();
        b.slots[3] = Some(ChannelMix { volume: 1.0, pan: -1.0, priority: false });
        let (mut l, mut r) = (vec![0.0; 128], vec![0.0; 128]);
        let (mut bl, mut br) = (vec![0.0; 128], vec![0.0; 128]);
        let mut ph = 0.0;
        let mut energy = (0f32, 0f32);
        for i in 0..400 {
            a.process(Some(&tone(&mut ph, 0.3, 128)), &mut l, &mut r);
            for (kind, _, seq, payload) in packets(&mut a) {
                b.push_packet(&down(7, 1 << 3, kind, seq, &payload));
            }
            b.process(None, &mut bl, &mut br);
            if i > 200 {
                energy.0 += bl.iter().map(|x| x * x).sum::<f32>();
                energy.1 += br.iter().map(|x| x * x).sum::<f32>();
            }
        }
        assert!(energy.0 > 1.0, "左聲道要有聲音：{energy:?}");
        assert!(energy.1 < energy.0 * 0.01, "定位在最左，右聲道幾乎無聲：{energy:?}");
        assert_eq!(b.speaking(), vec![(7, 1 << 3)]);
        // 400 × 128 取樣 ≈ 53 幀，扣掉緩衝
        assert!(b.totals().decoded > 40, "{}", b.totals().decoded);

        // 我把這個人調成 0：不出聲（頻道音量不變）
        b.user_gain.insert(7, 0.0);
        let mut e = 0f32;
        for i in 0..50 {
            a.process(Some(&tone(&mut ph, 0.3, 128)), &mut l, &mut r);
            for (kind, _, seq, payload) in packets(&mut a) {
                b.push_packet(&down(7, 1 << 3, kind, seq, &payload));
            }
            b.process(None, &mut bl, &mut br);
            if i >= 10 {
                e += bl.iter().map(|x| x * x).sum::<f32>();
            }
        }
        assert!(e < 1e-6, "個人音量 0：{e}");
        b.user_gain.clear();

        // 頻道音量 0：不出聲
        b.slots[3] = Some(ChannelMix { volume: 0.0, pan: 0.0, priority: false });
        let mut e = 0f32;
        for i in 0..50 {
            a.process(Some(&tone(&mut ph, 0.3, 128)), &mut l, &mut r);
            for (kind, _, seq, payload) in packets(&mut a) {
                b.push_packet(&down(7, 1 << 3, kind, seq, &payload));
            }
            b.process(None, &mut bl, &mut br);
            // 改設定前已經混好的那一幀（最多 960 取樣）還會播完
            if i >= 10 {
                e += bl.iter().map(|x| x * x).sum::<f32>();
            }
        }
        assert!(e < 1e-6, "{e}");
    }
}
