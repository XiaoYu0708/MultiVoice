# 接續工作清單

> 最後更新：2026-11-16。完成一項就把它從這裡移除或打勾，並同步更新 `PLAN.md` 的里程碑。

## 目前狀態
- M0（技術驗證）：本機部分完成；跨網路三項待第二台裝置
- M1（單頻道房主模式）：完成
- **M2（多頻道收聽／開麥）：完成**（commit `e7899e7`、`656d9b7`）
- **M4（桌面 UI）：完成**（頻道卡片、按鍵說話 PTT、提示音、系統匣、頻道設定檔）；PTT 待遊戲實測
- **M3（房主轉移）：完成**（計畫性＋非預期轉移＋雙房主合併；剩先建後拆、候補排序、混沌測試）
- M5：重取樣完成；M6：房主權限完成
- **M7（網頁客戶端）：完成並已部署**（2026-11-16；v0.5.0 桌面版有「複製網頁版連結」）；手機跨網路實測正常

**GitHub**：`XiaoYu0708/MultiVoice`（公開）；發佈用 `tools/release.sh`，客戶端啟動時會提示新版本（見 CLAUDE.md「版本控制與發佈」）。

使用者指定的順序是：試用桌面版 → M2 多頻道 → 全域 PTT 熱鍵（完成，待實測）→ 下一步請問使用者（M4 剩下的系統匣等，或 M3／M5）。

---

## 1. 全域 PTT 熱鍵（M4）— 程式完成，**待使用者在遊戲中實測**

已完成：
- `crates/hotkeys`：LL hook（不攔截）、按鍵解析（`Hotkey` 的 `FromStr`/`Display`）、`held()`（多按其他修飾鍵也算）、
  錄製模式、`feed`（UI 補送按鍵）、`is_admin` / `relaunch_as_admin`。
  手動測試工具：`cargo run -p hotkeys --example probe --release`
- session：`TalkMode::Ptt` + `ChannelCfg::ptt_key`；`AudioOpts` 加 `cues`；
  遮罩邏輯在 `session::talk::TalkState`（純函式、有測試），PTT 不經過 VAD、放開後每槽位延長 160 ms；
  提示音 `talk::TonePlayer` 混進播放端（PTT 依槽位不同音高、靜音下降音；跟著輸出音量，拒聽時也播）
- desktop：說話下拉加「按鍵說話」+ 設定按鍵按鈕（選了還沒設鍵會直接進入錄製）；大廳的音訊設定有提示音開關；
  `settings.toml` 的 `ptt_keys`（頻道名稱 → 鍵）加入頻道時自動套用（含自動加入的大廳）；非管理員時顯示提示與「以系統管理員身分重新啟動」
- 驗證：用 `SendInput` 模擬 F20（PTT）+ PrintWindow 截圖，確認「送出中」切換正確
- **使用者決定（2026-11-15）**：快捷鍵**只做按鍵說話**，全域廣播鍵、全域靜音鍵不做（已移除程式碼）；
  房間裡不放「語音設定」區塊，設定都在各頻道卡片裡（裝置、降噪、預設說話方式在大廳的「音訊設定」）
- **已知限制與對策**：本程式自己的視窗在前景時，全域鍵盤 hook 收不到大部分按鍵（實測字母、Space、Alt 收不到；
  F19、滑鼠側鍵收得到；在 hook 執行緒停用 IME 沒用，推測是中文輸入法先攔走）。
  對策：desktop `feed_local_keys` 把 egui 的按鍵（優先用實體鍵）與修飾鍵變化用 `hotkeys::feed` 補送；
  在文字框打字時不補送；失去焦點時放開所有補送的鍵。錄製按鈕會交出鍵盤焦點（避免錄到 Space／Enter 時又觸發按鈕）

待做：
- [ ] 使用者實測：遊戲在前景、遊戲以管理員執行、滑鼠側鍵、提示音聽感（音量 0.12、45 ms×2）
- [ ] CLI 沒有 PTT 參數（需要的話加 `--ptt 鍵`）
- [ ] 偵測 UIPI：目前只是非管理員時一律顯示提示；可改成「遊戲在前景但 `event_count` 不動」時才提示

### 系統匣（完成）
- `desktop/src/tray.rs`：獨立執行緒 + 隱藏視窗 + `Shell_NotifyIconW`，每 150 ms 讀 `Session::shared()` 更新圖示
  （灰＝未進房、藍＝在房間、綠＝送出中、紅＋斜線＝靜音／拒聽）；左鍵叫回視窗，右鍵選單：顯示／靜音／拒聽／結束；
  檔案總管重啟（TaskbarCreated）會重新加回圖示
- 在房間中按 X → 藏到系統匣（第一次跳通知）；設定 `close_to_tray` 可關閉；不在房間按 X 直接結束；
  系統匣選「結束」→ 先叫出視窗再送 WM_CLOSE（視窗藏著時 egui 不跑畫面迴圈，處理不了關閉）
- 驗證：`WM_CLOSE` → 隱藏、程序仍在；模擬點圖示 → 視窗回來且重繪正常；模擬「結束」→ 程序結束
- 注意：Windows 11 新圖示預設收在「隱藏的圖示」裡，要使用者自己拖到工作列上
- 圖示檢查：`ICON_DUMP=<資料夾> cargo test -p desktop dump_icons -- --ignored` 輸出各尺寸 RGBA
- **UI 自動化注意**：模擬點擊前一定要確認 MultiVoice 是前景視窗（否則會點到使用者的其他視窗）；
  切前景用 `AttachThreadInput` + `SetForegroundWindow`；最小化／還原後游標要先移開再移回，egui 才認得點擊

### 頻道設定檔（完成）
- `desktop/src/presets.rs`：把已加入頻道的設定（收聽、說話模式、PTT 鍵、音量、定位、優先）以**頻道名稱**存成一組；
  套用＝設定檔裡有的頻道照設定加入、其他頻道離開、房間沒有的頻道列出提示；`ptt_keys` 跟著同步
- 房間「頻道」標題下一列：下拉選單、套用、另存（輸入名稱，同名覆蓋）、刪除；`last_preset` 記住上次用的
- 設定存在 `settings.toml` 的 `[[presets]]`
- 驗證：單元測試（TOML 來回、套用計畫）；實際點擊（CLI 建「指揮部」）：套用 → 離開大廳、加入指揮部（按鍵說話 Mouse4、優先），
  按住模擬的 Mouse4 → 「送出中：指揮部」；另存 → 名稱預填、Enter 儲存、內容正確
- 測試時出現過一次「點套用卻顯示已儲存」，之後照同樣順序重跑兩次都正常（當時使用者也在操作電腦，判斷是輸入混入）
- 之後可考慮：設定檔切換快捷鍵（使用者目前只要按鍵說話，先不做）

### 不做的項目
- 置頂「誰在說話」小視窗：做好後使用者決定不要（2026-11-15，已 revert commit `ac12c1c`），之後不要再加

---

## 2. M0 剩下的（需要第二台在不同網路的裝置）
- [ ] 跨網路互測：打洞成功率、真實 RTT、relay→直連切換時間（工具：`netprobe listen` / `netprobe connect <id>`，或直接用 `multivoice.exe`）
- [x] 瀏覽器用 iroh WASM 經自架 relay 連房主（2026-11-16，M7 網頁版；使用者用手機在不同網路實測通話正常）
- [ ] Docker Desktop 內 UDP 的直連效果（決定 `headless-host` 放 Docker 還是 Windows 原生）

## 3. M3：房主轉移（PLAN.md §5）— 第一版完成（2026-11-15）
做法（協定 v3：`ClientMsg::Standby`、`HostMsg::Successors`、`HostMsg::MigrateTo`）：
- 每位加入者（`join_room`）多建一個**備用房主端點**，上線後 `Standby` 告訴房主；平常只做轉址（拿含它的邀請票來的人 → `Redirect` 到現任房主）
- 房主依 `session_idx`（越早進房越前面）排候補名單，變動就廣播 `Successors`（房主自己不在內）
- **不做 StateDelta**：新房主用自己客戶端看到的頻道與成員（`host_core::Seed`）啟動，成員重連後 `session_idx` 不變；
  各成員的訂閱由 `Client::reconnect` 自動重送
- `Client` 內部可換連線（`reconnect`）：音訊執行緒的 `send_voice`／`recv_voice` 不用重啟，主視窗縮在系統匣也能轉移
- 計畫性：`Session::leave` 時房主 `Host::migrate()` → 廣播 `MigrateTo` → 等 0.7 秒再關。實測（本機）0.4 秒完成
- 非預期：客戶端每 1 秒 ping，**3.5 秒沒收到房主任何控制訊息就當失聯**（iroh 自己要 30 秒以上才放棄）→
  `session::takeover`：每輪先試原房主，再依序試候補，第 k 位候補在失聯 k×3 秒後自己接手，20 秒放棄
- 邀請票改成動態：現任房主 + 候補的備用端點（`Session::ticket`）
- 測試：`session/tests/takeover.rs`（計畫性、房主當機、房主＋第一候補同時當機）；真實執行檔（3 個 CLI）計畫性與 `taskkill /F` 都通過，
  桌面版在原房主離開後顯示「你是房主」

還沒做：
- [x] **雙房主合併**（協定 v4：`HostProbe`、`HostInfo`、`MoveTo`）：當房主的人每 3 秒探測前任房主與見過的所有候補備用端點
  （`Client::seen_standbys` 累積記錄）；同房間有另一個房主時，人少的讓位（`Host::yield_to` 廣播 `MoveTo` → `Host::stop`，
  備用端點回去做轉址）。比較順序：人數 → 任期 → 端點 ID（小的贏）。涵蓋候補自己斷網而自立、舊房主回來、兩個候補同時接手。
  測試：`split_brain_merges_back_into_bigger_room`、`old_host_with_fewer_members_yields`（約 3.7 秒合併）
- 不做 `HostLostVote` 多數確認：斷網的人湊不到票、PLAN 也規定 10 秒後仍升任，最後還是靠合併收尾
- [x] **先建後拆**（2026-11-16）：`Client::reconnect` 換到新房主後，舊連線再保留 `client_core::DRAIN`（1.5 秒），語音兩邊都送、兩邊都收（jitter buffer 依序號去重），舊連線上的控制訊息不再採用；房主離開時（`Session::leave`）等到沒人或 DRAIN 才關；改連重試間隔 300 → 100 ms。測試 `planned_migration_audio_gap`／`…_slow_member`：成員晚 400 ms 改連時，另一人聽到的空白從 357 ms（掉 16 包）降到 23 ms（0 遺失）
- [ ] 候補排序只看進房先後，沒看直連／頻寬；`can_host`、「允許我成為房主」開關
- [ ] `sim` 混沌測試
- [ ] 原始房主（建房者）在 `host_room` 沒有備用端點：轉移後他若沒離開也不會再當候補（目前房主離開就是整個程式離開，沒影響）

## 4. M5：音質與穩定性
- [x] **重取樣**（2026-11-15）：`audio::resample::Resampler`（rubato 5 `Async::new_poly` Septic，10 ms 一塊，同取樣率直接傳過）；
  環形緩衝裡是裝置取樣率，擷取執行緒轉成 48 kHz、播放執行緒轉成裝置取樣率（回呼內不做）。
  測試：單元測試（44.1↔48、96→48 的長度／頻率／振幅）；`MULTIVOICE_TEST_RATES=44100,44100` 強制開裝置的端對端測試
  （輸入 44.1k＋輸出 44.1k、輸出 22.05k、輸出 96k 都沒有欠載、沒有 PLC）。`cargo run -p audio --example rates` 列出各裝置取樣率
- [x] 斷線自動重連並恢復訂閱：由 M3 的流程涵蓋（失聯後每 0.5 秒重試原房主／候補，最多 20 秒；訂閱自動重送）
- [x] **裝置熱插拔**（2026-11-15）：`pipeline::AudioRunner` 在專屬執行緒持有 cpal 串流（不在 UI 執行緒，縮到系統匣也有效），
  每秒檢查：串流出錯（`StreamStats::failed`）就重開；指定的裝置不見 → 先用系統預設、插回來自動切回；
  設為「系統預設」時 Windows 換了預設裝置就跟著換；都開不起來時每 2 秒重試。訊息記在「最近事件」。
  驗證：`cargo run -p session --example switch_device --release`（含 `simulate_audio_failure` 自動重開）。
  **待使用者實測**：實際拔插 USB 耳機
- [x] **AGC、麥克風測試、VAD 門檻可調**（2026-11-15）：`audio::frontend::Frontend`（降噪 → VAD → AGC → 防爆音，通話與測試共用）。
  VAD 看降噪後、增益前的音量 ≥ 門檻（`gate_dbfs`，預設 -50，可調 -70..-20）且人聲機率 ≥ 0.6；
  AGC 只在說話時調整，拉向 -20 dBFS（+15／-6 dB，壓 0.2 秒、放大 2 秒）；底噪估計 → 「依環境噪音設定門檻」（底噪 + 12 dB）。
  麥克風測試 `session::mictest::MicTest`（不進房；可聽自己的聲音），UI 在大廳「音訊設定」；設定區收起來就停止。
  命令列：`cargo run -p session --example mic_test --release -- 秒數`（`MIC=名稱片段`）。
  **待使用者實測**：實際講話時的門檻與 AGC 聽感（測試時使用者的麥克風一開始是靜音的）
- [x] **網路統計**（2026-11-16）：底部狀態列顯示最近 5 秒的遺失率（需要 FEC／PLC 補償的幀比例）；房主在房間資訊看到
  「房主負載：轉發給 N 人｜上傳約 X kbps」與「讓出房主」按鈕（`Session::hand_over` → `Takeover::hand_over`：計畫性轉移但自己留下；
  `host_room` 也建備用端點，讓出後還能再當候補）。測試 `host_hands_over_and_stays_as_member`；桌面版實測按下後 CLI 成為新房主
- [x] **評估**（2026-11-16，量測程式不在 repo，結果如下）：
  - **libopus**：`opus` 0.3（libopus 1.3.1）→ **0.4（libopus 1.6.1，已升級）**，API 相容、位元流相容（實測 v0.3.0 與新版互通）。
    Deep PLC／OSCE／DRED：`opusic-sys` 有 `osce`、`dred` 功能；Deep PLC 要解碼器複雜度 ≥ 5，但 `opus` 0.4 的 Decoder 沒開放
    `set_complexity`，要自己用 `opusic_sys` FFI 包解碼器；DRED 的 ctl 在 `opus` 0.4 也還沒包。**待做**（效果只在掉包時聽得出來）
  - **IAudioClient3**：cpal 0.18 用 IAudioClient、系統預設週期（通常 10 ms）；改 IAudioClient3 每個方向約省 5～7 ms，
    相對整體 80～160 ms 效益小，又要改 cpal 的 WASAPI 後端 → **不做**
  - **DeepFilterNet3**（`dfn3_rs`，純 Rust、處理時不配置記憶體、AVX2）：合成寬頻噪音壓低 40 dB（RNNoise 9 dB）；
    TTS 中文語音 + -36 dBFS 噪音：句子之間 -64 dBFS（RNNoise -47）、語音音量 -0.9 dB、但語音段波形 SNR 11.1 dB（RNNoise 15.7，
    表示會改變人聲波形）；CPU 約 1% 個核心；延遲 30 ms（RNNoise 10 ms）；執行檔 +5.5 MB。
    → **做成可選的「強力降噪」**（大廳音訊設定，預設關閉；說話判斷仍用 RNNoise 的人聲機率），聽感由使用者在麥克風測試比較

## 5. M6：身分、權限、房間管理 —— **使用者決定不做**（2026-11-15）；以下保留紀錄，除非使用者改變主意
- [x] 房主權限（2026-11-15）：`HostConfig::owner`（房主自己的成員 NodeId）；只有房主能改名／刪除頻道，其他人回 `Forbidden`；
  建立頻道所有人都可以。整合測試 `only_owner_can_rename_or_delete_channels`。M3 房主轉移時要決定 owner 是「建房者」還是「目前房主」
- [ ] 頻道改名後，其他成員以舊名稱存的 PTT 鍵不會跟著改（房主自己改名時有搬過去）
- [ ] `talk_without_listen` 權限（PLAN §2.2）目前沒有限制
- [ ] 角色、踢人、封鎖、重新產生邀請票、指定房主
- [ ] 自架 relay 改用 `access.shared_token`，客戶端 `RelayConfig::with_auth_token`
- [ ] 房間沉睡／喚醒；**頻道設定（音量、pan、說話模式）的保存**也等這個一起做

## 6. M7：網頁客戶端 — 第一版完成（2026-11-16）

架構（PLAN.md §8）：
- `crates/web`（wasm-bindgen，瀏覽器主執行緒）：iroh（WASM，一律經 relay）＋ `client-core`（與桌面版共用；計時與背景工作改用 `n0_future`，原生仍走 tokio）。網頁成員不當候補，換房主時只跟著改連（`find_new_host`）
- `crates/web-audio`（C ABI，**不用** wasm-bindgen：AudioWorklet 沒有 TextDecoder）：libopus 用 clang 從原始碼編成 wasm（build.rs 讀 opusic-sys 附的原始碼，`libc/` 是最小標頭，malloc 與數學函式在 `src/libc.rs`）、RNNoise 人聲機率＋音量門檻、`mixer_core::talk`（與桌面版同一份說話遮罩與提示音）、jitter buffer（含 FEC／PLC）、每頻道混音
- `crates/web/www/`：`index.html`、`app.js`（UI、頻道卡片、按住說話）、`worklet.js`、PWA（manifest、`sw.js`、圖示）
- 降噪／回音消除／自動增益交給瀏覽器 getUserMedia；AudioContext 固定 48 kHz（瀏覽器不支援就報錯）
- 建置：`tools/build-web.sh`（`--serve` 本機測試、`--deploy` 複製到 `deploy/web/`）
- 測試：`node tools/headless.mjs <網址> <秒> --eval … --shot …`（無頭 Chrome、假麥克風）；Node 也能直接載入 `web_audio.wasm` 跑來回測試
- 桌面版：「複製網頁版連結」（`WEB_CLIENT_URL#票`）；加入時可直接貼網頁版連結（`protocol::ticket::clean_input`）

已驗證（2026-11-16，無頭 Chrome + CLI 房主，本機經自架 relay）：加入 0.9 秒、雙向語音、遺失 0%、多頻道＋按住說話、建立頻道、房主離開後自動改連接手者

待做：
- [x] 已部署（2026-11-16）：`tools/build-web.sh --deploy` 後 `MSYS_NO_PATHCONV=1 docker compose exec -T caddy caddy reload --config /etc/caddy/Caddyfile`（在 deploy/ 執行；Git Bash 要關路徑轉換）。正式網址端對端測試通過。之後更新網頁版前**先問使用者**
- [x] 使用者實測（2026-11-16）：手機（網頁版）與電腦（桌面版）在**不同網路**下可以正常通話
- [x] 使用者回報（2026-11-16）：iPhone 鎖定或切到其他 App 時，麥克風指示燈仍亮但雙向都沒聲音——iOS Safari 在背景會暫停網頁音訊，網頁無法繞過（PLAN §8.2）。已改善：回到前景時自動恢復 AudioContext、麥克風被暫停／停掉就自動重開（不行才顯示按鈕）、iPhone 在房間畫面顯示說明、試用 `navigator.audioSession.type = 'play-and-record'`。要背景通話只能做原生 App
- [ ] **iPhone 背景只收聽（實驗，待使用者實測）**：使用者可以接受背景時只聽不說。切到背景時放掉麥克風、`audioSession.type = 'playback'`，回到前景自動開回麥克風（`play-and-record`）。「音訊設定」可選方式一（Web Audio 直接播）或方式二（`MediaStreamDestination` → `<audio>`，走媒體播放）；網址加 `?ios` 可在其他瀏覽器測 iPhone 流程
- [x] 使用者實測（2026-11-16）：iPhone 背景**收聽正常**（方式一）
- [ ] **浮動視窗（子母畫面）實驗，待實測能不能說話**：房間畫面「浮動視窗」＝ canvas.captureStream → `<video autopictureinpicture>`，顯示麥克風狀態與誰在說話；浮動視窗開著時切到背景**不放掉麥克風**，看 iOS 會不會繼續收音
- [ ] 細項還沒回報：Android 背景（不同廠牌省電機制）、回到前景後是否自動恢復、喇叭回音、耗電
- iPhone 原生 App：使用者沒有 Mac；要做只能用雲端 macOS（GitHub Actions）＋ Apple 開發者帳號（每年 99 美元）＋ TestFlight，暫不做
- [ ] Firefox：MediaStreamSource 接到 48 kHz 的 AudioContext 是否正常（裝置不是 48 kHz 時）
- [ ] 網頁版的頻道左右定位、優先頻道（引擎已支援，UI 還沒放）
- [ ] GitHub Pages 備援託管

## 7. M8：打磨 — 進行中
- [x] **一鍵更新**（2026-11-16，`desktop/src/update.rs`）：提示列「立即更新」→ 下載 Release 的 `multivoice.exe` 到 `.new`、
  核對大小與 GitHub 的 SHA-256 → 執行中的 exe 改名 `.old`、新檔換上（失敗就還原）→ 帶原參數加 `--restarted` 重新啟動；
  在房間裡更新會帶 `--auto-join <目前的邀請票>` 自動回到房間；`.old` 下次啟動時刪掉。
  實測：假裝 0.0.1 → 下載 v0.5.0、雜湊相符、重新啟動；在 CLI 房主的房間裡更新 → 房主看到離開又加入。
  **v0.5.0 以前的版本沒有這個功能**，要手動下載一次 v0.6.0 之後才會自動更新
- [x] **只開一個**（`desktop/src/instance.rs`）：每個設定檔一個具名 mutex；再執行 exe 時找原本的系統匣隱藏視窗
  （標題 `MultiVoiceTray:<設定檔>`）送 `WM_SHOW_MAIN` 叫回視窗後結束（`AllowSetForegroundWindow`；
  `ChangeWindowMessageFilterEx` 讓一般權限也能叫回系統管理員身分執行的那個）；`--restarted` 時改成等舊的結束（最多 15 秒）
- [x] 網頁版拿掉頻道的「收聽」勾選（使用者要求：加入就是收聽，不想聽就離開頻道）
- [ ] 安裝程式（開始功能表捷徑、解除安裝）：目前單一 exe 免安裝，需要再做
- [ ] 手機原生 App 評估

## 已知小問題
- CLI 只能用 `--channel` 在啟動時建頻道，執行中沒有頻道指令
- 房主拒絕操作（`HostError`）只記在「最近事件」，UI 沒有明顯提示
- 沒加入的頻道看不到誰在說話（收不到那個頻道的聲音，本來就不知道），是預期行為
- 協定 v4 與舊版不相容，給朋友的執行檔要一起更新（下次發佈要在說明裡提醒）
