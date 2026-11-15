//! MultiVoice 網頁客戶端的音訊引擎（[`engine::Engine`]），編成 wasm 後在 AudioWorklet 裡跑。
//!
//! AudioWorklet 的環境沒有 TextDecoder 等 API，wasm-bindgen 的膠水程式碼用不了，
//! 所以這裡只匯出 C ABI 函式（`mv_*`），由 `worklet.js` 直接讀寫 wasm 記憶體：
//! - 每次 `process()`：麥克風取樣寫進 `mv_input()`，呼叫 `mv_process(n, 有沒有麥克風)`，
//!   從 `mv_out_l()` / `mv_out_r()` 讀出播放的聲音
//! - 收到語音封包：寫進 `mv_packet_buf(len)`，再呼叫 `mv_push_packet(len)`
//! - 要送出的封包：`mv_outbox()` / `mv_outbox_len()`，讀完 `mv_outbox_clear()`
//! - 狀態：`mv_status()`（i32 陣列，格式見該函式）

pub mod engine;
#[cfg(target_family = "wasm")]
mod libc;
pub mod opus;

use engine::Engine;
use mixer_core::route::ChannelMix;
use mixer_core::talk::SLOTS;

pub const FRAME_SAMPLES: usize = 960;
/// AudioWorklet 一次處理的取樣數（render quantum）；保留到 1024 以防未來瀏覽器加大
const MAX_QUANTUM: usize = 1024;

pub struct Handle {
    engine: Engine,
    input: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    packet: Vec<u8>,
    status: Vec<i32>,
}

fn h<'a>(p: *mut Handle) -> &'a mut Handle {
    unsafe { &mut *p }
}

fn mask(lo: u32, hi: u32) -> u64 {
    (hi as u64) << 32 | lo as u64
}

#[unsafe(no_mangle)]
pub extern "C" fn mv_new() -> *mut Handle {
    Box::into_raw(Box::new(Handle {
        engine: Engine::new(),
        input: vec![0.0; MAX_QUANTUM],
        out_l: vec![0.0; MAX_QUANTUM],
        out_r: vec![0.0; MAX_QUANTUM],
        packet: Vec::new(),
        status: Vec::new(),
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn mv_input(p: *mut Handle) -> *mut f32 {
    h(p).input.as_mut_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn mv_out_l(p: *mut Handle) -> *mut f32 {
    h(p).out_l.as_mut_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn mv_out_r(p: *mut Handle) -> *mut f32 {
    h(p).out_r.as_mut_ptr()
}

/// 處理 `n` 個取樣（≤ 1024）。`has_input` = 0 代表麥克風沒開
#[unsafe(no_mangle)]
pub extern "C" fn mv_process(p: *mut Handle, n: u32, has_input: u32) {
    let s = h(p);
    let n = (n as usize).min(MAX_QUANTUM);
    let input = (has_input != 0).then(|| &s.input[..n]);
    s.engine.process(input, &mut s.out_l[..n], &mut s.out_r[..n]);
}

#[unsafe(no_mangle)]
pub extern "C" fn mv_mic_stopped(p: *mut Handle) {
    h(p).engine.mic_stopped();
}

/// 取得放收到封包的緩衝區（至少 `len` bytes）
#[unsafe(no_mangle)]
pub extern "C" fn mv_packet_buf(p: *mut Handle, len: u32) -> *mut u8 {
    let s = h(p);
    s.packet.resize(len as usize, 0);
    s.packet.as_mut_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn mv_push_packet(p: *mut Handle, len: u32) {
    let s = h(p);
    let n = (len as usize).min(s.packet.len());
    let pkt = std::mem::take(&mut s.packet);
    s.engine.push_packet(&pkt[..n]);
    s.packet = pkt;
}

#[unsafe(no_mangle)]
pub extern "C" fn mv_outbox(p: *mut Handle) -> *const u8 {
    h(p).engine.outbox.as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn mv_outbox_len(p: *mut Handle) -> u32 {
    h(p).engine.outbox.len() as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn mv_outbox_clear(p: *mut Handle) {
    h(p).engine.outbox.clear();
}

/// 我的某個槽位對應的頻道混音設定；`active` = 0 代表槽位沒在用
#[unsafe(no_mangle)]
pub extern "C" fn mv_set_slot(p: *mut Handle, slot: u32, active: u32, volume: f32, pan: f32, priority: u32) {
    let s = h(p);
    if let Some(x) = s.engine.slots.get_mut(slot as usize) {
        *x = (active != 0).then_some(ChannelMix { volume, pan, priority: priority != 0 });
    }
}

/// 說話模式：一直開麥、偵測說話的頻道（我的槽位遮罩）
#[unsafe(no_mangle)]
pub extern "C" fn mv_set_talk(p: *mut Handle, open_lo: u32, open_hi: u32, vad_lo: u32, vad_hi: u32) {
    let s = h(p);
    s.engine.open_mask = mask(open_lo, open_hi);
    s.engine.vad_mask = mask(vad_lo, vad_hi);
}

/// 目前按住的按鍵說話按鈕對應的頻道
#[unsafe(no_mangle)]
pub extern "C" fn mv_set_ptt(p: *mut Handle, lo: u32, hi: u32) {
    h(p).engine.ptt_mask = mask(lo, hi);
}

#[unsafe(no_mangle)]
pub extern "C" fn mv_set_flags(p: *mut Handle, muted: u32, deafened: u32, cues: u32) {
    let e = &mut h(p).engine;
    e.muted = muted != 0;
    e.deafened = deafened != 0;
    e.cues = cues != 0;
}

#[unsafe(no_mangle)]
pub extern "C" fn mv_set_volume(p: *mut Handle, v: f32) {
    h(p).engine.volume = v.clamp(0.0, 4.0);
}

#[unsafe(no_mangle)]
pub extern "C" fn mv_set_gate(p: *mut Handle, db: f32) {
    h(p).engine.gate_db = db;
}

/// 狀態（i32 陣列，長度在 `[0]`）：
/// `[長度, 送出遮罩低 32, 高 32, 送出中, 偵測到說話, 音量 dB×10, 解碼, FEC, PLC, 遲到, 說話人數 n,
///   (成員, via 低 32, via 高 32) × n]`
#[unsafe(no_mangle)]
pub extern "C" fn mv_status(p: *mut Handle) -> *const i32 {
    let s = h(p);
    let e = &s.engine;
    let t = e.totals();
    let sp = e.speaking();
    s.status.clear();
    s.status.extend_from_slice(&[
        0,
        e.tx_mask as u32 as i32,
        (e.tx_mask >> 32) as u32 as i32,
        e.active() as i32,
        e.voiced as i32,
        (e.level_db * 10.0) as i32,
        t.decoded as i32,
        t.fec as i32,
        t.plc as i32,
        t.late as i32,
        sp.len() as i32,
    ]);
    for (idx, via) in sp {
        s.status.extend_from_slice(&[idx as i32, via as u32 as i32, (via >> 32) as u32 as i32]);
    }
    s.status[0] = s.status.len() as i32;
    s.status.as_ptr()
}

const _: () = assert!(SLOTS == 64);
