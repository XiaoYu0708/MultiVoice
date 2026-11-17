//! 網頁版的強力降噪：DeepFilterNet3（`dfn3_rs`，與桌面版的「強力降噪」相同），編成**獨立的** wasm 模組
//! `web_dfn.wasm`（含權重約 5.5 MB），使用者開了強力降噪才下載，在 AudioWorklet 裡由 `worklet.js` 載入。
//!
//! 和音訊引擎（`web_audio.wasm`）分開的原因：
//! - 權重很大，沒開的人不用下載
//! - 這個模組用 SIMD128 建置（比純量快約 8.7 倍）；不支援 SIMD 的舊瀏覽器只是開不了強力降噪，
//!   音訊引擎本身不受影響
//!
//! 介面（C ABI，同 web-audio，不用 wasm-bindgen）：`dfn_new()` 建立；把 480 個取樣（10 ms、48 kHz）
//! 寫進 `dfn_buf()`，呼叫 `dfn_process()` 後就地變成降噪後的聲音（延遲 30 ms）。

pub use dfn3_rs::FRAME_SIZE;

pub struct Handle {
    d: dfn3_rs::Denoiser,
    buf: [f32; FRAME_SIZE],
}

fn h<'a>(p: *mut Handle) -> &'a mut Handle {
    unsafe { &mut *p }
}

/// 建立降噪器（會配置記憶體；之後處理時不再配置）
#[unsafe(no_mangle)]
pub extern "C" fn dfn_new() -> *mut Handle {
    Box::into_raw(Box::new(Handle { d: dfn3_rs::Denoiser::new(), buf: [0.0; FRAME_SIZE] }))
}

/// 放一段（`FRAME_SIZE` 個取樣）聲音的緩衝區
#[unsafe(no_mangle)]
pub extern "C" fn dfn_buf(p: *mut Handle) -> *mut f32 {
    h(p).buf.as_mut_ptr()
}

/// 就地降噪 `dfn_buf()` 裡的一段，回傳估計的訊噪比（dB）
#[unsafe(no_mangle)]
pub extern "C" fn dfn_process(p: *mut Handle) -> f32 {
    let s = h(p);
    // 輸入必須是有限值（dfn3_rs 的要求）；壞掉的取樣當成 0
    for v in s.buf.iter_mut() {
        if !v.is_finite() {
            *v = 0.0;
        }
    }
    s.d.process_frame(&mut s.buf)
}

/// 換成不相關的串流（例如重新開麥）時清掉狀態
#[unsafe(no_mangle)]
pub extern "C" fn dfn_reset(p: *mut Handle) {
    h(p).d.reset();
}
