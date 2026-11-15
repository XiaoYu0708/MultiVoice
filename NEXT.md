# 接續工作清單

> 最後更新：2026-11-15。完成一項就把它從這裡移除或打勾，並同步更新 `PLAN.md` 的里程碑。

## 目前狀態
- M0（技術驗證）：本機部分完成；跨網路三項待第二台裝置
- M1（單頻道房主模式）：完成
- **M2（多頻道收聽／開麥）：完成**（commit `e7899e7`、`656d9b7`）
- **M4（桌面 UI）：完成**（頻道卡片、按鍵說話 PTT、提示音、系統匣、頻道設定檔）；PTT 待遊戲實測
- **M3（房主轉移）：完成**（計畫性＋非預期轉移＋雙房主合併；剩先建後拆、候補排序、混沌測試）
- M5：重取樣完成；M6：房主權限完成；M7：未開始

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
- [ ] 瀏覽器（Chrome Android / iOS Safari）用 iroh WASM 連自架 relay
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
- [ ] 先建後拆（目前是斷開後重連，約 0.4 秒空白）
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
- [ ] AGC、麥克風測試介面（顯示原始／降噪後音量、底噪頻段分析，見 `audio/examples/mic_probe.rs`）
- [ ] 評估 libopus 1.5（Deep PLC / DRED）、IAudioClient3 低延遲、DeepFilterNet
- [ ] VAD 門檻可調

## 5. M6：身分、權限、房間管理
- [x] 房主權限（2026-11-15）：`HostConfig::owner`（房主自己的成員 NodeId）；只有房主能改名／刪除頻道，其他人回 `Forbidden`；
  建立頻道所有人都可以。整合測試 `only_owner_can_rename_or_delete_channels`。M3 房主轉移時要決定 owner 是「建房者」還是「目前房主」
- [ ] 頻道改名後，其他成員以舊名稱存的 PTT 鍵不會跟著改（房主自己改名時有搬過去）
- [ ] `talk_without_listen` 權限（PLAN §2.2）目前沒有限制
- [ ] 角色、踢人、封鎖、重新產生邀請票、指定房主
- [ ] 自架 relay 改用 `access.shared_token`，客戶端 `RelayConfig::with_auth_token`
- [ ] 房間沉睡／喚醒；**頻道設定（音量、pan、說話模式）的保存**也等這個一起做

## 6. 之後
- M7 網頁客戶端、M8 安裝程式／自動更新、手機原生 App 評估

## 已知小問題
- CLI 只能用 `--channel` 在啟動時建頻道，執行中沒有頻道指令
- 房主拒絕操作（`HostError`）只記在「最近事件」，UI 沒有明顯提示
- 沒加入的頻道看不到誰在說話（收不到那個頻道的聲音，本來就不知道），是預期行為
- 協定 v4 與舊版不相容，給朋友的執行檔要一起更新（下次發佈要在說明裡提醒）
