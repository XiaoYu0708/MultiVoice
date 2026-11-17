//! 強力降噪（DeepFilterNet3）的呼叫介面。
//!
//! - wasm：DeepFilterNet3 在另一個 wasm 模組（`crates/web-dfn` → `web_dfn.wasm`，開了才下載），
//!   這裡透過匯入函式 `mv.dfn(指標)` 請 `worklet.js` 把 480 個取樣拷過去處理再拷回來
//! - 原生（單元測試）：直接用 `dfn3_rs`

pub const HALF: usize = 480;

#[cfg(target_family = "wasm")]
#[link(wasm_import_module = "mv")]
unsafe extern "C" {
    /// 就地降噪 `ptr` 開始的 480 個取樣；回傳 0 代表模組還沒載入（沒處理）
    fn dfn(ptr: *mut f32) -> u32;
}

#[derive(Default)]
pub struct Strong {
    #[cfg(not(target_family = "wasm"))]
    d: Option<Box<dfn3_rs::Denoiser>>,
}

impl Strong {
    /// 就地降噪一段（10 ms）；沒處理就回傳 false（聲音不變）
    pub fn process(&mut self, half: &mut [f32; HALF]) -> bool {
        #[cfg(target_family = "wasm")]
        {
            unsafe { dfn(half.as_mut_ptr()) != 0 }
        }
        #[cfg(not(target_family = "wasm"))]
        {
            self.d.get_or_insert_with(|| Box::new(dfn3_rs::Denoiser::new())).process_frame(half);
            true
        }
    }
}

#[cfg(not(target_family = "wasm"))]
const _: () = assert!(dfn3_rs::FRAME_SIZE == HALF);
