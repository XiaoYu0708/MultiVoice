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

1. 到 [Releases](https://github.com/XiaoYu0708/MultiVoice/releases/latest) 下載：
   - `MultiVoice-Setup.exe`：安裝程式（裝在自己的使用者資料夾、不需要系統管理員權限；有開始功能表捷徑，可以從「設定 → 應用程式」解除安裝）
   - 或 `multivoice.exe`：免安裝，直接執行
   - 兩種都會在有新版時提示、一鍵更新
   - 執行檔沒有數位簽章，Windows 可能會跳出 SmartScreen 警告，按「其他資訊 → 仍要執行」
   - Windows Defender 也可能把它當成病毒擋下來，詳見下方〈[被 Windows Defender 擋下來？](#被-windows-defender-擋下來)〉
2. 一個人按「建立房間」，把「複製邀請票」得到的 `mv1…` 文字傳給朋友
3. 朋友貼上邀請票按「加入房間」

沒裝程式的人（例如用手機）：房間裡的人按「複製邀請連結」傳過去，打開就能用瀏覽器加入。
手機用網頁版也可以切到遊戲或鎖定螢幕，照樣說話和收聽。

有新版本時程式會在畫面上方提示。**房間裡的人版本要一致**（不同版本可能無法連線）。

如果遊戲以系統管理員身分執行，MultiVoice 也要以系統管理員身分執行，按鍵說話才收得到按鍵。

### 被 Windows Defender 擋下來？

Defender 可能把下載的 `multivoice.exe` 判定成木馬並移除，名稱通常類似 `Trojan:Win32/Bearfoos.A!ml`。
結尾的 `!ml` 代表這是**機器學習的推測**，不是比對到已知病毒。新發佈、沒有數位簽章、下載人數少的程式很常被這樣誤判；
MultiVoice 又有幾個表面上像惡意程式的行為，更容易被誤判：
- 全域按鍵監聽（遊戲中按鍵說話要用）
- 自己下載新版並替換執行檔（一鍵更新）
- 以系統管理員身分重新啟動自己
- 點對點網路連線

程式碼全部公開在這個儲存庫，可以自己檢查或從原始碼建置。要確認下載的檔案沒被竄改：

1. 在 [Releases](https://github.com/XiaoYu0708/MultiVoice/releases/latest) 頁面看 `multivoice.exe` 旁邊的 SHA-256
2. 在下載的檔案所在資料夾開 PowerShell 執行 `Get-FileHash multivoice.exe`，兩邊一致就是官方發佈的檔案

確認後如果要使用：開「Windows 安全性 → 病毒與威脅防護 → 保護歷程記錄」，點那筆記錄，選「動作 → 允許」或「還原」。
也歡迎到 [Microsoft 的誤判回報頁面](https://www.microsoft.com/en-us/wdsi/filesubmission) 提交檔案（選「不是惡意程式」），
回報的人越多，誤判越快被修正。不想處理的話，也可以先用網頁版。

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

設計文件與進度：[PLAN.md](PLAN.md)。
