//! libopus 的最小 FFI（與桌面版 `audio::codec` 相同的設定：48 kHz、mono、20 ms、VoIP、FEC）。
//! wasm 由 build.rs 編譯的 libopus 提供符號；原生則由 opusic-sys 提供。

use std::ffi::c_int;

use crate::FRAME_SAMPLES;

#[repr(C)]
struct OpusEncoder {
    _p: [u8; 0],
}
#[repr(C)]
struct OpusDecoder {
    _p: [u8; 0],
}

const OPUS_APPLICATION_VOIP: c_int = 2048;
const OPUS_SET_BITRATE_REQUEST: c_int = 4002;
const OPUS_SET_INBAND_FEC_REQUEST: c_int = 4012;
const OPUS_SET_PACKET_LOSS_PERC_REQUEST: c_int = 4014;
const OPUS_SET_COMPLEXITY_REQUEST: c_int = 4010;

unsafe extern "C" {
    fn opus_encoder_create(fs: i32, channels: c_int, application: c_int, error: *mut c_int) -> *mut OpusEncoder;
    fn opus_encoder_ctl(st: *mut OpusEncoder, request: c_int, ...) -> c_int;
    fn opus_encode_float(st: *mut OpusEncoder, pcm: *const f32, frame_size: c_int, data: *mut u8, max: i32) -> i32;
    fn opus_encoder_destroy(st: *mut OpusEncoder);
    fn opus_decoder_create(fs: i32, channels: c_int, error: *mut c_int) -> *mut OpusDecoder;
    fn opus_decode_float(
        st: *mut OpusDecoder,
        data: *const u8,
        len: i32,
        pcm: *mut f32,
        frame_size: c_int,
        decode_fec: c_int,
    ) -> c_int;
    fn opus_decoder_destroy(st: *mut OpusDecoder);
}

// 原生建置：確保連結到 opusic-sys 的 libopus
#[cfg(not(target_family = "wasm"))]
use opusic_sys as _;

pub const MAX_PACKET: usize = 1276;

pub struct Encoder(*mut OpusEncoder);

impl Encoder {
    pub fn new(bitrate: i32) -> Option<Self> {
        let mut err = 0;
        let p = unsafe { opus_encoder_create(48_000, 1, OPUS_APPLICATION_VOIP, &mut err) };
        if p.is_null() || err != 0 {
            return None;
        }
        unsafe {
            opus_encoder_ctl(p, OPUS_SET_BITRATE_REQUEST, bitrate as c_int);
            opus_encoder_ctl(p, OPUS_SET_INBAND_FEC_REQUEST, 1 as c_int);
            opus_encoder_ctl(p, OPUS_SET_PACKET_LOSS_PERC_REQUEST, 10 as c_int);
            // 手機 CPU 較弱：複雜度 8（預設 10），音質差異很小
            opus_encoder_ctl(p, OPUS_SET_COMPLEXITY_REQUEST, 8 as c_int);
        }
        Some(Self(p))
    }

    /// 編碼一幀（960 取樣），回傳位元組數
    pub fn encode(&mut self, pcm: &[f32], out: &mut [u8]) -> Option<usize> {
        debug_assert_eq!(pcm.len(), FRAME_SAMPLES);
        let n = unsafe { opus_encode_float(self.0, pcm.as_ptr(), FRAME_SAMPLES as c_int, out.as_mut_ptr(), out.len() as i32) };
        (n > 0).then_some(n as usize)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { opus_encoder_destroy(self.0) }
    }
}

pub struct Decoder(*mut OpusDecoder);

impl Decoder {
    pub fn new() -> Option<Self> {
        let mut err = 0;
        let p = unsafe { opus_decoder_create(48_000, 1, &mut err) };
        (!p.is_null() && err == 0).then_some(Self(p))
    }

    fn run(&mut self, data: Option<&[u8]>, fec: bool, out: &mut [f32]) -> bool {
        let (ptr, len) = data.map_or((std::ptr::null(), 0), |d| (d.as_ptr(), d.len() as i32));
        let n = unsafe { opus_decode_float(self.0, ptr, len, out.as_mut_ptr(), FRAME_SAMPLES as c_int, fec as c_int) };
        n == FRAME_SAMPLES as c_int
    }

    pub fn decode(&mut self, packet: &[u8], out: &mut [f32]) -> bool {
        self.run(Some(packet), false, out)
    }

    /// 用下一包的 FEC 復原這一幀
    pub fn fec(&mut self, next: &[u8], out: &mut [f32]) -> bool {
        self.run(Some(next), true, out)
    }

    /// 封包遺失補償
    pub fn plc(&mut self, out: &mut [f32]) -> bool {
        self.run(None, false, out)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { opus_decoder_destroy(self.0) }
    }
}
