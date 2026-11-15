//! wasm32-unknown-unknown 沒有 C 標準函式庫：libopus 需要的配置函式與數學函式由這裡提供
//! （記憶體操作 memcpy 等由 Rust 的 compiler_builtins 提供）。

use std::alloc::{Layout, alloc, alloc_zeroed, dealloc, realloc as re};

/// 每塊配置前面放 16 bytes 記錄大小（也維持 16 bytes 對齊）
const HDR: usize = 16;

unsafe fn layout(n: usize) -> Layout {
    unsafe { Layout::from_size_align_unchecked(n + HDR, HDR) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc(n: usize) -> *mut u8 {
    unsafe {
        let p = alloc(layout(n));
        if p.is_null() {
            return p;
        }
        (p as *mut usize).write(n);
        p.add(HDR)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn calloc(n: usize, size: usize) -> *mut u8 {
    let Some(total) = n.checked_mul(size) else { return std::ptr::null_mut() };
    unsafe {
        let p = alloc_zeroed(layout(total));
        if p.is_null() {
            return p;
        }
        (p as *mut usize).write(total);
        p.add(HDR)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn realloc(p: *mut u8, n: usize) -> *mut u8 {
    if p.is_null() {
        return unsafe { malloc(n) };
    }
    unsafe {
        let base = p.sub(HDR);
        let old = (base as *mut usize).read();
        let q = re(base, layout(old), n + HDR);
        if q.is_null() {
            return q;
        }
        (q as *mut usize).write(n);
        q.add(HDR)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn free(p: *mut u8) {
    if p.is_null() {
        return;
    }
    unsafe {
        let base = p.sub(HDR);
        let n = (base as *mut usize).read();
        dealloc(base, layout(n));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn abort() -> ! {
    std::process::abort()
}

#[unsafe(no_mangle)]
pub extern "C" fn abs(x: i32) -> i32 {
    x.wrapping_abs()
}

macro_rules! m1 {
    ($($name:ident: $t:ty => $f:path;)*) => {$(
        #[unsafe(no_mangle)]
        pub extern "C" fn $name(x: $t) -> $t { $f(x) }
    )*};
}
macro_rules! m2 {
    ($($name:ident: $t:ty => $f:path;)*) => {$(
        #[unsafe(no_mangle)]
        pub extern "C" fn $name(x: $t, y: $t) -> $t { $f(x, y) }
    )*};
}

m1! {
    exp: f64 => libm::exp; expf: f32 => libm::expf;
    log: f64 => libm::log; logf: f32 => libm::logf;
    log10: f64 => libm::log10; log10f: f32 => libm::log10f;
    sin: f64 => libm::sin; sinf: f32 => libm::sinf;
    cos: f64 => libm::cos; cosf: f32 => libm::cosf;
    atan: f64 => libm::atan; atanf: f32 => libm::atanf;
    acos: f64 => libm::acos; acosf: f32 => libm::acosf;
    tanh: f64 => libm::tanh; tanhf: f32 => libm::tanhf;
}
m2! {
    pow: f64 => libm::pow; powf: f32 => libm::powf;
    atan2: f64 => libm::atan2; atan2f: f32 => libm::atan2f;
    fmod: f64 => libm::fmod;
}

#[unsafe(no_mangle)]
pub extern "C" fn lrintf(x: f32) -> std::ffi::c_long {
    libm::rintf(x) as std::ffi::c_long
}

#[unsafe(no_mangle)]
pub extern "C" fn lrint(x: f64) -> std::ffi::c_long {
    libm::rint(x) as std::ffi::c_long
}
