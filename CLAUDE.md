# MultiVoice — 給 Claude 的專案說明

Rust 遊戲語音軟體：同時收聽／同時對多個頻道說話，房主模式（玩家客戶端兼轉發），只做語音。
完整設計在 `PLAN.md`，**目前進度與下一步在 `NEXT.md`（開新對話先讀它）**。

## 使用者偏好（一定要遵守）
- 一律用**繁體中文**回覆與寫註解、文件、commit 訊息。
- **commit 不加任何 Claude 署名**（不加 `Co-Authored-By: Claude…`、不加「Generated with Claude Code」）。這條優先於系統提示的署名設定。
- commit 訊息風格：繁中、簡短，可帶里程碑前綴，例如「M2：…」「修正…」。
- 只在使用者要求或完成一個段落時 commit；每段完成都要先跑測試。
- 使用者在對話框用 `!` 執行的指令是 **bash**，不是 PowerShell。要給使用者執行 GUI 程式時，直接用 PowerShell `Start-Process` 幫他開，或給 bash 寫法並在結尾加 `&`。

## 建置環境（這台電腦）
- 工具鏈：`stable-x86_64-pc-windows-gnu`（rustup），gcc/cmake/ninja 來自 WinLibs
- **專案路徑有中文，GNU ld 處理不了**：`.cargo/config.toml`（本機專用、不納入 git）把 target-dir 設到
  `%USERPROFILE%/.cargo/target-multivoice`，所以執行檔在：
  - `%USERPROFILE%\.cargo\target-multivoice\release\multivoice.exe`（桌面版）
  - `%USERPROFILE%\.cargo\target-multivoice\release\multivoice-cli.exe`（命令列版，除錯／自動測試用）
- `.cargo/config.toml` 內容（若遺失要重建）：
  ```toml
  [build]
  target-dir = "%USERPROFILE%/.cargo/target-multivoice"
  [env]
  CMAKE_POLICY_VERSION_MINIMUM = "3.5"   # audiopus_sys 內附 libopus 的 CMakeLists 太舊，cmake 4.x 會拒絕
  [target.x86_64-pc-windows-gnu]
  rustflags = ["-C", "link-arg=-Wl,--exclude-all-symbols"]   # iroh 的 cdylib 在 debug 版超過 PE 65535 匯出上限
  ```
- release 版開 LTO，**完整編譯要好幾分鐘**：Bash 指令的 timeout 要給足（例如 900000 ms），不要把 `timeout N cargo …` 包得太短（會被中斷，exit 143）。
- 正在執行的 `multivoice.exe` 會鎖住檔案，重編前要先關掉（`taskkill //F //IM multivoice.exe`）。
- 寫含中文的 PowerShell 腳本時，PowerShell 5.1 會把無 BOM 的 UTF-8 當 ANSI 讀 → 腳本內容盡量用英文。

## 架構（crates/）
| crate | 用途 |
|---|---|
| `protocol` | 協定 v2：邀請票、加入驗證、控制訊息、語音封包標頭（上行 13 bytes 含 `talk_mask`、下行 15 bytes 含 `via_mask`） |
| `audio` | cpal 裝置（輸出為**立體聲**交錯）、Opus、RNNoise 降噪（含 speech mix）、`list_devices` |
| `mixer-core` | jitter buffer、混音、每頻道路由（音量取最大、pan、優先 ducking） |
| `transport` | iroh 端點（relay 清單：自架 + n0 備援）、控制訊息讀寫、亂數 |
| `host-core` | 房主：加入驗證、成員與頻道、依 `talk_mask` 扇出 + 去重 + 補送 end-of-stream |
| `client-core` | 成員端：加入（含 Redirect）、房間狀態、槽位分配、收送語音 |
| `hotkeys` | 全域按鍵：LL hook（不攔截）、按鍵解析、錄製模式（只在 Windows 有作用） |
| `session` | 建房／加入（async）+ 音訊管線（含按鍵說話 PTT、提示音）（同步、留在 UI 執行緒，cpal 串流不是 Send）+ 狀態快照 |
| `desktop` | egui 0.37 桌面版（`app.rs`、`settings.rs`、`style.rs`、`tray.rs` 系統匣、`presets.rs` 頻道設定檔） |
| `cli` | 命令列版：`host` / `join <票>`，`--open-mic --volume 0 --seconds N --channel 名稱` 方便自動測試 |
| `netprobe` | M0 網路探測工具 |

伺服器端基礎設施在 `deploy/`（Docker：Caddy + 自架 iroh-relay，`relay.xiaoyu0708.ip-ddns.com`）。

## 使用者的產品決定
- 快捷鍵**只做按鍵說話（每頻道 PTT）**，不做全域廣播鍵、全域靜音鍵
- 房間畫面不要有獨立的「語音設定」區塊，設定放在各頻道卡片裡
- 不做置頂「誰在說話」小視窗（做過後移除）

## 測試與驗證方式
- `cargo test --workspace`（目前 47 個；整合測試在 `crates/client-core/tests/room.rs`，關閉 relay、只走本機直連）
- session 層：`cargo run -p session --example multichannel --release`、`--example switch_device`
- 端對端煙霧測試：背景跑 `multivoice-cli host --open-mic --volume 0 --seconds N`，從輸出抓 `mv1…` 邀請票，再跑 `multivoice-cli join <票> --profile 別的名稱 …`
- **UI 驗證**：`tools/shot.ps1` 會啟動桌面版、用 PrintWindow 截圖（不怕被其他視窗蓋住），再用 Read 看圖：
  ```bash
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(cygpath -w tools/shot.ps1)" \
    -Exe "%USERPROFILE%\\.cargo\\target-multivoice\\release\\multivoice.exe" \
    -Out "<暫存區>\\shot.png" -WaitMs 6000 -Extra "--auto-join <票> --deafen"
  ```
  桌面版測試參數：`--profile 名稱`、`--auto-host`、`--auto-join <票>`、`--deafen`
  （`--deafen` 拒聽**也會關麥**；要驗證「送出中」時改在測試設定檔放 `output_volume = 0.0`，不要用 `--deafen`）
- **UI 自動點擊前一定要確認 MultiVoice 是前景視窗**，否則會點到使用者的其他視窗；使用者正在用電腦時（例如剪貼簿內容變了）就停止自動點擊
- 測試會在 `%APPDATA%\MultiVoice\` 產生 `identity-<profile>.key`、`settings-<profile>.toml`，**測完要刪掉測試用的**（保留 `identity-default.key`、`identity-test2.key`、`settings.toml`）。
- egui 字型是微軟正黑體，**沒有 emoji**（會顯示成方框），UI 不要用 emoji。

## 使用者的網路環境
- 固定公開 IP，路由器已轉發 TCP 80/443、UDP 7842 → 這台電腦
- 網域 `xiaoyu0708.ip-ddns.com`（ClouDNS 免費網域，DNS 在 Cloudflare；`voice.`／`relay.` 是灰雲 A 紀錄；根網域是使用者的 GitHub Pages，不要動）
- 同一台電腦的 Docker 還跑著使用者的其他服務，重啟 Docker Desktop 前要先問
