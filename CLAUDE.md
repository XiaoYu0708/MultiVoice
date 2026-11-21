# MultiVoice — 給 Claude 的專案說明

Rust 遊戲語音軟體：同時收聽／同時對多個頻道說話，房主模式（玩家客戶端兼轉發），只做語音。
完整設計在 `PLAN.md`（里程碑勾選即進度）。

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
- `.cargo/config.toml` 內容（若遺失要重建；`%USERPROFILE%` 要換成實際的使用者資料夾，cargo 不會展開環境變數）：
  ```toml
  [build]
  target-dir = "%USERPROFILE%/.cargo/target-multivoice"
  [env]
  CMAKE_POLICY_VERSION_MINIMUM = "3.5"   # audiopus_sys 內附 libopus 的 CMakeLists 太舊，cmake 4.x 會拒絕
  [target.x86_64-pc-windows-gnu]
  rustflags = ["-C", "link-arg=-Wl,--exclude-all-symbols"]   # iroh 的 cdylib 在 debug 版超過 PE 65535 匯出上限
  [env.MULTIVOICE_RELAY_TOKEN]
  value = "<自架 relay 權杖，與 deploy/.env、GitHub secret RELAY_TOKEN 相同>"
  ```
- **自架 relay 權杖**（`transport::RELAY_TOKEN`）：編譯時從環境變數 `MULTIVOICE_RELAY_TOKEN` 帶入，**不能寫進 git**（公開儲存庫）；
  存在三個地方：本機 `.cargo/config.toml`、`deploy/.env`（docker compose 傳給 relay 的 `IROH_RELAY_ACCESS_TOKEN`）、GitHub secret `RELAY_TOKEN`。
  沒帶權杖的版本連不上自架 relay（`access.shared_token` 生效後），只能用 n0 的 relay。權杖會編進公開的 exe／網頁版，只是門檻、不是密碼
- **網頁版**：`tools/build-web.sh`（輸出 `dist/web/`；`--serve` 開 http://127.0.0.1:8080/、`--deploy` 複製到 `deploy/web/`）。需要 `wasm32-unknown-unknown` target、`wasm-bindgen-cli` 0.2.130（要和 Cargo.lock 一致）、LLVM（`C:\Program Files\LLVM\bin`，腳本會自動加進 PATH）
- release 版開 LTO，**完整編譯要好幾分鐘**：Bash 指令的 timeout 要給足（例如 900000 ms），不要把 `timeout N cargo …` 包得太短（會被中斷，exit 143）。
- 正在執行的 `multivoice.exe` 會鎖住檔案，重編前要先關掉（`taskkill //F //IM multivoice.exe`）。
- 寫含中文的 PowerShell 腳本時，PowerShell 5.1 會把無 BOM 的 UTF-8 當 ANSI 讀 → 腳本內容盡量用英文。

## 架構（crates/）
| crate | 用途 |
|---|---|
| `protocol` | 協定 v5（v3 房主轉移、v4 雙房主合併、v5 房主跟著開房的人＋踢人＋更換邀請）：邀請票、加入驗證、控制訊息、語音封包標頭（上行 13 bytes 含 `talk_mask`、下行 15 bytes 含 `via_mask`） |
| `audio` | cpal 裝置（輸出為**立體聲**交錯、熱插拔偵測）、重取樣（`resample`）、Opus（libopus 1.6.1）、前端處理 `frontend`（RNNoise／DeepFilterNet3 降噪、VAD、AGC）、`list_devices`；`audio::denoise` 轉出 mixer-core 的 |
| `mixer-core` | jitter buffer、混音、每頻道路由（音量取最大、pan、優先 ducking）、說話遮罩與提示音（`talk`）、RNNoise 降噪＋人聲混回（`denoise`）——桌面與網頁共用 |
| `transport` | iroh 端點（relay 清單：自架 + n0 備援）、控制訊息讀寫、亂數 |
| `host-core` | 房主：加入驗證、成員與頻道、依 `talk_mask` 扇出 + 去重 + 補送 end-of-stream |
| `client-core` | 成員端：加入（含 Redirect）、房間狀態、槽位分配、收送語音、換房主時換連線（`reconnect`）、失聯偵測；計時用 `n0_future`（可編 WASM） |
| `hotkeys` | 全域按鍵：LL hook（不攔截）、按鍵解析、錄製模式（只在 Windows 有作用） |
| `session` | 建房／加入（async）+ 房主轉移流程（`takeover.rs`）+ 音訊管線（含按鍵說話 PTT、提示音）（同步、留在 UI 執行緒，cpal 串流不是 Send）+ 狀態快照 |
| `web` | 網頁版（瀏覽器主執行緒，wasm-bindgen）：iroh WASM + `client-core`，前端檔案在 `crates/web/www/` |
| `web-audio` | 網頁版音訊引擎（AudioWorklet 裡跑，C ABI 不用 wasm-bindgen）：libopus（clang 編成 wasm）、RNNoise 降噪（開著時關掉瀏覽器自己的降噪）、VAD、jitter buffer、混音；強力降噪透過匯入函式 `mv.dfn` 呼叫 `web-dfn` |
| `web-dfn` | 網頁版強力降噪（DeepFilterNet3，含權重約 5.5 MB）：獨立的 `web_dfn.wasm`，用 SIMD128 建置，開了才下載，由 `worklet.js` 載入 |
| `desktop` | egui 0.37 桌面版（`app.rs`、`settings.rs`、`style.rs`、`tray.rs` 系統匣、`presets.rs` 頻道設定檔（房間的頻道 + 我在各頻道的設定；開房時可選一個自動建好頻道）、`update.rs` 檢查新版與一鍵更新、`instance.rs` 同設定檔只開一個、`crash.rs` 錯誤對話框與 OpenGL→wgpu 備援） |
| `cli` | 命令列版：`host` / `join <票>`，`--open-mic --volume 0 --seconds N --channel 名稱` 方便自動測試 |
| `netprobe` | M0 網路探測工具 |

`vendor/dfn3_rs`：DeepFilterNet3 推論（crates.io 0.1.2 + 自己加的 wasm SIMD128 kernel，見 `vendor/dfn3_rs/MULTIVOICE.md`），
根目錄 `Cargo.toml` 的 `[patch.crates-io]` 讓桌面版與網頁版都用這份。

伺服器端基礎設施在 `deploy/`（Docker：Caddy + 自架 iroh-relay，`relay.xiaoyu0708.ip-ddns.com`）。

## 版本控制與發佈
- GitHub：`XiaoYu0708/MultiVoice`（**公開**儲存庫，分支 `master`）。文件不要寫入使用者的個人環境細節（使用者路徑、內網 IP、其他服務名稱）
- 版本號在根目錄 `Cargo.toml` 的 `[workspace.package] version`（所有 crate 共用）
- 發佈：`tools/release.sh 0.2.0 "更新說明"`（改版本 → 本機測試 → commit + push → 建 Release 草稿 → 推 tag `v0.2.0`
  → GitHub Actions `.github/workflows/release.yml` 用 **MSVC**（crt-static）建置、附上兩個 exe、公開草稿；腳本會等建置完成，最後重新編譯並部署網頁版，讓網頁版版本號一致）。
  發佈的 exe 不用本機 GNU 版：GNU 編的 exe 曾被 Defender 機器學習誤判（`Bearfoos.A!ml`、`Wacatac.B!ml`），MSVC 版實測沒被擋。
  手動執行該 workflow（`gh workflow run release.yml`）只建置不發佈，exe 在 Artifacts `multivoice-windows`
  **發佈前要先問使用者**（會公開執行檔）
- **每次更新版本，桌面版、網頁版、Android 版都要一起更新**（版本號一致、功能與操作一致）：
  - 桌面版：`tools/release.sh` 發佈 exe
  - 網頁版：`tools/release.sh` 最後會重新編譯並部署
  - Android 版：另一個專案 `C:\Users\XiaoYu\Projects\App開發\MultiVoice-Android`（原生 Kotlin，透過 JNI 用這裡的 Rust crates，建置方式見該專案的 `CLAUDE.md`；
    GitHub `XiaoYu0708/MultiVoice-Android` 是**私人**儲存庫，裡面有簽章金鑰，不能改成公開）。
    `versionName` 改成同一個版本號、`versionCode` 加 1，重新編譯簽好的 AAB／APK，
    **APK 要一起附到這個儲存庫的 GitHub Release**（和 exe 放在同一個 Release；「下載哪個檔案」說明也要列出 APK）
  - 改了協定或共用 crates（`client-core`、`mixer-core`、`transport`、`protocol`）時，三個版本都要重新編譯並測試互通
- **每次發佈的說明都要附上「下載哪個檔案」**：`MultiVoice-Setup.exe`（安裝版）、`multivoice.exe`（免安裝版，一鍵更新也是下載它）、`multivoice-cli.exe`（命令列版，一般人不用）各自的用途。
  `tools/release.sh` 會自動把這段加在更新說明後面；不用腳本、手動發佈時也要加
- 桌面版啟動時查 GitHub 最新 Release（`desktop/src/update.rs`），比目前版本新就在畫面上方提示；
  測試提示列：環境變數 `MULTIVOICE_TEST_VERSION=0.0.1`
- 協定版本（`protocol::PROTOCOL_VERSION`）不相容時要提高，並在發佈說明提醒大家一起更新

## 使用者的產品決定
- 快捷鍵**只做按鍵說話（每頻道 PTT）**，不做全域廣播鍵、全域靜音鍵
- 各頻道的說話方式、頻道音量放在頻道卡片裡；房間畫面另有「音訊設定」卡片（輸出音量、降噪、門檻、提示音…），**桌面與網頁操作一致**：寬視窗在右邊側欄、預設展開，窄視窗／手機在頻道下面、預設收起（桌面版房間裡不放麥克風測試）
- 不做置頂「誰在說話」小視窗（做過後移除）
- **M6 只做一部分**：不做身分組、封鎖名單、房間沉睡。已做的：
  - 「房主」＝管理者，是**開房的人**（不是負責轉發的那台；轉發者叫「轉發」，會自動換人，UI 的「讓出轉發」只換轉發者）。
    開房的人離開後沒人能管，同一台、同一個設定檔回來就恢復
  - 房主才能改名／刪除頻道、移出成員、更換邀請、把房主交給別人（成員右鍵選單）；**只能交給桌面版成員**（網頁版不能當轉發者，所以也不能當房主）
  - **移出成員一定同時更換邀請**（新密鑰只發給其他人，不需要封鎖名單）；複製邀請連結大家都可以
  - 頻道改名後設定跟著走、relay 權杖保留
- 網頁版：加入頻道就是收聽（不要「收聽」勾選）；不要浮動視窗與「背景只收聽」（改用 WebRTC 通話讓手機背景可說可聽）
- 桌面版與網頁版進房時麥克風預設靜音
- **桌面版一律以系統管理員執行**（`main.rs` 開頭：不是管理員就跳 UAC 重新啟動自己，UAC 按「否」就不開），讓以管理員執行的遊戲在前景時也能按鍵說話。
  自動化測試從一般權限啟動，要設環境變數 `MULTIVOICE_TEST_NO_ADMIN=1`（`tools/shot.ps1` 已設），否則每次都會跳 UAC

## 待辦
- **relay 權杖第二階段**：大家更新到 v0.8.0 以後，`deploy/iroh-relay.toml` 改成 `access.shared_token = ["…"]`（權杖由 `IROH_RELAY_ACCESS_TOKEN` 覆蓋）並重啟 iroh-relay 容器。**重啟前先問使用者**，之後測一次連線
- 使用者實測還沒回報的：遊戲中按鍵說話（含遊戲以系統管理員執行）、USB 耳機熱插拔、AGC／說話門檻聽感、Android 網頁版背景

## 測試與驗證方式
- `cargo test --workspace`（目前 85 個；房主轉移整合測試在 `crates/session/tests/takeover.rs`；整合測試在 `crates/client-core/tests/room.rs`，關閉 relay、只走本機直連）
- session 層：`cargo run -p session --example multichannel --release`、`--example switch_device`
- 重取樣：這台電腦的裝置都是 48 kHz，測非 48 kHz 路徑用環境變數 `MULTIVOICE_TEST_RATES=輸入,輸出`（例如 `44100,44100`；
  輸出任何取樣率都可以（Windows 轉換），輸入要裝置原生支援）；`cargo run -p audio --example rates` 列出各裝置預設取樣率
- 沒有麥克風（只聽）：桌面版／CLI 用環境變數 `MULTIVOICE_TEST_NO_MIC=1`；網頁版用 `tools/headless.mjs … --no-mic`
- 房主操作測試：CLI 房主加 `--kick-after N`（N 秒後移出其他所有人，用來看被移出的畫面）；桌面版環境變數 `MULTIVOICE_TEST_CONFIRM=kick|transfer|rotate` 進房後直接打開確認對話框（截圖用，不用自動點擊）
- 端對端煙霧測試：背景跑 `multivoice-cli host --open-mic --volume 0 --seconds N`，從輸出抓 `mv1…` 邀請票（輸出有 emoji，grep 會當成二進位檔，要用 `grep -ao`），再跑 `multivoice-cli join <票> --profile 別的名稱 …`
- **UI 驗證**：`tools/shot.ps1` 會啟動桌面版、用 PrintWindow 截圖（不怕被其他視窗蓋住），再用 Read 看圖：
  ```bash
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(cygpath -w tools/shot.ps1)" \
    -Exe "%USERPROFILE%\\.cargo\\target-multivoice\\release\\multivoice.exe" \
    -Out "<暫存區>\\shot.png" -WaitMs 6000 -Extra "--auto-join <票> --deafen"
  ```
  加 `-Maximize` 會先把視窗最大化再截圖（檢查全螢幕排版：寬時房間分「頻道｜側欄」兩欄，網頁版 1024px 以上也是）。
  桌面版測試參數：`--profile 名稱`、`--auto-host`、`--auto-join <票>`、`--deafen`、`--auto-update`（搭配 `MULTIVOICE_TEST_VERSION=0.0.1`，找到新版就直接更新）、
  `--restarted`（自己重新啟動自己時帶：等舊程式結束，不是叫醒它）。
  繪製：預設 OpenGL（glow），失敗時改用 wgpu（DX12）重新啟動並在 `%APPDATA%\MultiVoice\use-wgpu` 記住；`--renderer wgpu|glow` 指定、環境變數 `MULTIVOICE_TEST_GL_FAIL=1` 模擬 OpenGL 失敗。
  啟動失敗／panic 會跳對話框並寫 `%APPDATA%\MultiVoice\error.log`（`crash.rs`）。**同一個設定檔只能開一個**，測試時多開要用不同的 `--profile`
- 一鍵更新實測：把 exe 複製到暫存資料夾再跑（會被換成 GitHub 上的最新版），不要在 target 目錄裡直接測
  （`--deafen` 拒聽**也會關麥**；要驗證「送出中」時改在測試設定檔放 `output_volume = 0.0`，不要用 `--deafen`）。
  **桌面版與網頁版進房時麥克風預設靜音**（使用者要求，`AudioOpts::start_muted`），要驗證「送出中」得先按取消靜音；CLI 不受影響
- **UI 自動點擊前一定要確認 MultiVoice 是前景視窗**，否則會點到使用者的其他視窗；使用者正在用電腦時（例如剪貼簿內容變了）就停止自動點擊
- 測試會在 `%APPDATA%\MultiVoice\` 產生 `identity-<profile>.key`、`settings-<profile>.toml`，**測完要刪掉測試用的**（保留 `identity-default.key`、`identity-test2.key`、`settings.toml`）。
- **網頁版驗證**：`node tools/headless.mjs <網址> <秒> [--mobile] --eval "js" --eval sleep:N --shot 圖.png`（無頭 Chrome、假麥克風、自動允許權限；不需要 Chrome 擴充功能）。搭配背景的 CLI 房主與 `tools/build-web.sh --serve`
- 網頁版的部署（`--deploy`，會公開到 multivoice.<網域>）**要先問使用者**
- egui 字型是微軟正黑體，**沒有 emoji**（會顯示成方框），UI 不要用 emoji。

## 使用者的網路環境
- 固定公開 IP，路由器已轉發 TCP 80/443、UDP 7842 → 這台電腦
- 網域 `xiaoyu0708.ip-ddns.com`（ClouDNS 免費網域，DNS 在 Cloudflare；`multivoice.`（網頁版）／`relay.` 是灰雲 A 紀錄；根網域是使用者的 GitHub Pages，不要動）
- 同一台電腦的 Docker 還跑著使用者的其他服務，重啟 Docker Desktop 前要先問
