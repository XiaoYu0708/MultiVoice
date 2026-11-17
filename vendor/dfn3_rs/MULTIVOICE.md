# dfn3_rs（MultiVoice 修改版）

來源：crates.io `dfn3_rs` 0.1.2（<https://github.com/TanixLu/dfn3_rs>，MIT OR Apache-2.0，見 `NOTICE`）。

MultiVoice 的修改（2026-11-17）：

- `src/simd.rs`：加上 `wasm32` 的 SIMD128 kernel（`mod wasm`：GEMM、GRU、tanh），
  網頁版的強力降噪（`crates/web-dfn`）要用；原本在非 x86 平台只有純量版本，wasm 上慢約 8.7 倍。
  x86 與純量路徑沒有改。只在用 `-C target-feature=+simd128` 建置時啟用（`tools/build-web.sh` 會帶；沒帶就走純量）
- `Cargo.toml`：拿掉 `[profile.release]`（在 workspace 裡不會生效）

驗證：同樣的輸入，wasm SIMD 版與 wasm 純量版的輸出差異 SNR > 130 dB。
