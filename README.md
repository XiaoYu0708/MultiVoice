# MultiVoice

給遊戲用的多頻道語音軟體（Windows）。可以**同時收聽、同時對多個頻道說話**，例如一直對「小隊」開麥、按住滑鼠側鍵才對「指揮部」說話。

- 不需要伺服器：建房的人的電腦就是房主，負責轉發聲音；房主離開或斷線時，房間會自動交給其他人
- 點對點連線（[iroh](https://github.com/n0-computer/iroh)），連不上時經 relay 轉送
- 每個頻道可以個別設定：收聽、說話方式（偵測說話／一直開麥／按鍵說話）、音量、左右定位、優先頻道
- 按鍵說話在遊戲中也有效，支援滑鼠側鍵；開關麥提示音
- 降噪（RNNoise）、Opus 編碼
- 系統匣常駐、頻道設定檔一鍵切換
- 網頁版：手機或沒裝程式的人，打開邀請連結就能用瀏覽器加入（不能當房主）

## 下載與使用

1. 到 [Releases](https://github.com/XiaoYu0708/MultiVoice/releases/latest) 下載 `multivoice.exe`，直接執行（不需安裝）
   - 執行檔沒有數位簽章，Windows 可能會跳出 SmartScreen 警告，按「其他資訊 → 仍要執行」
2. 一個人按「建立房間」，把「複製邀請票」得到的 `mv1…` 文字傳給朋友
3. 朋友貼上邀請票按「加入房間」

沒裝程式的人（例如用手機）：房間裡的人按「複製網頁版連結」傳過去，打開就能用瀏覽器加入。
手機用網頁版也可以切到遊戲或鎖定螢幕，照樣說話和收聽。

有新版本時程式會在畫面上方提示。**房間裡的人版本要一致**（不同版本可能無法連線）。

如果遊戲以系統管理員身分執行，MultiVoice 也要以系統管理員身分執行，按鍵說話才收得到按鍵。

## 從原始碼建置

需要 Rust（stable）與 CMake（編譯內附的 libopus）。

```bash
cargo build --release -p desktop   # 桌面版：target/release/multivoice.exe
cargo build --release -p cli       # 命令列版（除錯／自動測試用）
cargo test --workspace
```

網頁版另外需要 `rustup target add wasm32-unknown-unknown`、`wasm-bindgen-cli`（版本同 `Cargo.lock` 的 wasm-bindgen）與 clang（LLVM，用來把 ring 與 libopus 編成 WebAssembly）：

```bash
tools/build-web.sh           # 輸出到 dist/web/（純靜態檔案）
tools/build-web.sh --serve   # 建置後在 http://127.0.0.1:8080/ 測試
```

設計文件：[PLAN.md](PLAN.md)；目前進度：[NEXT.md](NEXT.md)。
