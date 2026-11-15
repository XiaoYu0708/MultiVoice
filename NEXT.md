# 接續工作清單

> 最後更新：2026-11-15。完成一項就把它從這裡移除或打勾，並同步更新 `PLAN.md` 的里程碑。

## 目前狀態
- M0（技術驗證）：本機部分完成；跨網路三項待第二台裝置
- M1（單頻道房主模式）：完成
- **M2（多頻道收聽／開麥）：完成**（commit `c3dec79`、`4323507`）
- M4（桌面 UI）：頻道卡片、按鍵說話（PTT）、提示音（待遊戲實測）、系統匣、**頻道設定檔完成**；懸浮視窗未做
- M3（房主轉移）、M5、M6、M7：未開始

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
- 驗證：單元測試（TOML 來回、套用計畫）；實際點「套用」確認會離開沒列在設定檔的頻道並提示缺少的頻道
- 測試時出現過一次「點套用卻顯示已儲存」，乾淨重跑無法重現（當時使用者也在操作電腦，可能是點擊落點問題）；請使用者實際使用時留意
- 之後可考慮：設定檔切換快捷鍵（使用者目前只要按鍵說話，先不做）

### 同一個里程碑剩下的
- 多組設定檔（已完成，見上）（Profile，例如「公會戰指揮」一鍵切換訂閱組合）
- 置頂透明「誰在說話」小視窗（不注入遊戲）

---

## 2. M0 剩下的（需要第二台在不同網路的裝置）
- [ ] 跨網路互測：打洞成功率、真實 RTT、relay→直連切換時間（工具：`netprobe listen` / `netprobe connect <id>`，或直接用 `multivoice.exe`）
- [ ] 瀏覽器（Chrome Android / iOS Safari）用 iroh WASM 連自架 relay
- [ ] Docker Desktop 內 UDP 的直連效果（決定 `headless-host` 放 Docker 還是 Windows 原生）

## 3. M3：房主轉移（PLAN.md §5）
目前完全沒做。重點：狀態複製（`StateDelta`）、候補名單、計畫性轉移（先建後拆）、非預期轉移（失聯偵測 + 多數確認 + 任期號）、衝突合併、`sim` 混沌測試。
已鋪好的路：協定有 `term`、`Redirect`；`client-core::Client::join` 會跟隨 Redirect；同一 NodeId 重連沿用 `session_idx`。
注意：M2 加了頻道與訂閱，狀態複製要包含頻道列表；各成員的訂閱由成員自己在換房主後重送（`Resume`）。

## 4. M5：音質與穩定性
- [ ] **重取樣**：目前非 48 kHz 的裝置會直接報錯（`audio::device::check_rate`）。用 rubato 5 `Async::new_poly`
- [ ] 斷線自動重連並恢復訂閱；裝置熱插拔
- [ ] AGC、麥克風測試介面（顯示原始／降噪後音量、底噪頻段分析，見 `audio/examples/mic_probe.rs`）
- [ ] 評估 libopus 1.5（Deep PLC / DRED）、IAudioClient3 低延遲、DeepFilterNet
- [ ] VAD 門檻可調

## 5. M6：身分、權限、房間管理
- [ ] 桌面版只有房主看得到頻道的「改名」「刪除」按鈕（刪除要按兩次），但**協定與房主端沒有權限檢查**，
  其他客戶端（例如 CLI）仍能改名／刪除；要做的話 `HostConfig` 加房主自己的 NodeId，`host-core` 驗證後回 `Forbidden`
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
- 協定 v2 與舊版不相容，給朋友的執行檔要一起更新
