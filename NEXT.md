# 接續工作清單

> 最後更新：2026-11-15。完成一項就把它從這裡移除或打勾，並同步更新 `PLAN.md` 的里程碑。

## 目前狀態
- M0（技術驗證）：本機部分完成；跨網路三項待第二台裝置
- M1（單頻道房主模式）：完成
- **M2（多頻道收聽／開麥）：完成**（commit `c3dec79`、`4323507`）
- M4（桌面 UI）：單頻道版 + 多頻道頻道卡片完成；**全域 PTT、系統匣、提示音未做**
- M3（房主轉移）、M5、M6、M7：未開始

使用者指定的順序是：試用桌面版 → M2 多頻道 → **全域 PTT 熱鍵（下一步）**。

---

## 1. 全域 PTT 熱鍵（下一步，M4）

目標：遊戲在前景時也能用。每個頻道可設「按住某鍵才說話」，另有全域廣播鍵、全域靜音鍵。支援鍵盤與滑鼠側鍵。

### 參考程式碼（使用者自己的專案，已驗證可用）
`%USERPROFILE%\Projects\遊戲\副本退出快捷腳本\src\`
- `engine.rs` 第 372–420 行：`keyboard_proc` / `mouse_proc`（`WH_KEYBOARD_LL`、`WH_MOUSE_LL`，含 `WM_XBUTTONDOWN` 側鍵）；第 560–585 行：hook 執行緒 + `GetMessageW` 迴圈
- `hotkey.rs`：按鍵名稱解析／格式化（`parse_key`、`key_name`、`format_combo`、`is_modifier`）
- `win.rs`：`is_admin`、`relaunch_as_admin`（`runas`）
- 依賴：`windows-sys 0.59`（features 見該專案 `Cargo.toml`）

### 進度
- [x] `crates/hotkeys`：按鍵解析（`Hotkey` 實作 `FromStr`/`Display`，例 "Ctrl+F9"、"Mouse4"、"LAlt"）、
  LL hook 執行緒（不攔截）、`Hotkey::held()`（多按其他修飾鍵也算）、錄製模式
  （`start_recording` / `take_recorded`，Esc 取消、單按修飾鍵放開也算）、`event_count`、`is_admin`、`relaunch_as_admin`
- [ ] 下一步從下面第 2 點（session）開始

### 建議設計
1. **新 crate `hotkeys`**（已完成，見上）（或放在 `desktop/src/hotkeys.rs`，但 CLI 也可能要用，建議獨立 crate，只在 Windows 編譯）：
   - 專用執行緒安裝 LL hook，維護「目前按住的鍵」集合 → 寫入 `Arc<AtomicU64>` 或回呼
   - **不攔截按鍵**（`CallNextHookEx` 照常傳下去），遊戲也要收得到
   - 忽略 `LLKHF_INJECTED`？先不要忽略（有些人用巨集）；記在設定裡可選
   - 錄製模式：UI 按「設定按鍵」後，下一個按下的鍵（含滑鼠側鍵、可帶 Ctrl/Alt/Shift）回傳給 UI；Esc 取消
2. **session**：
   - `TalkMode` 加 `Ptt(KeyId)`；`Shared` 加 `ptt_masks: Mutex<Vec<(KeyId, u64)>>` 或「每個鍵 → 槽位遮罩」
   - 擷取執行緒每幀：`mask = open_mask | (hang>0 ? vad_mask : 0) | Σ(按住的鍵對應的遮罩)`；PTT 不經過 VAD
   - 全域廣播鍵：按住時 `mask |= 所有 talk != Off 的頻道`（PLAN §2.3）；全域靜音鍵：切換 `muted`
   - PTT 放開後也要有短暫 hangover（約 100–200 ms），避免句尾被切
   - 注意：`talk = true` 的訂閱條件 = `talk != Off`，PTT 頻道也要 `talk = true`
3. **desktop**：
   - 頻道卡片的說話下拉加「按鍵說話」，旁邊一顆「設定按鍵」按鈕顯示目前按鍵
   - 語音設定加「全域廣播鍵」「全域靜音鍵」
   - 按鍵設定要保存：頻道 ID 每次建房不同 → 先以**頻道名稱**為鍵存在 `settings.toml`（`ptt_keys: { "指揮部": "Mouse4" }`），加入頻道時套用
   - 遊戲以系統管理員執行時，非管理員程式收不到它的按鍵（UIPI）→ 偵測不到時在 UI 提示，提供「以管理員身分重新啟動」（參考 QuickExit `win.rs`）
4. **開／關麥提示音**（同一段做比較順）：PTT 按下／放開、靜音切換時播短音；直接混進播放端的立體聲輸出即可（不用另開裝置），每頻道可不同音高
5. **測試**：
   - 單元測試：按鍵狀態 → 遮罩計算（純函式，放 session 或 hotkeys crate）
   - 手動：使用者在遊戲中實測（注意要測遊戲在前景、以及遊戲是管理員權限的情況）
   - 可用 `SendInput` 自動化測試 hook 是否收到（QuickExit `win.rs` 有 `send_key`）

### 同一個里程碑剩下的
- 系統匣（`tray-icon`）、最小化到系統匣常駐
- 多組設定檔（Profile，例如「公會戰指揮」一鍵切換訂閱組合）
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
- [ ] 目前**任何成員都能建立／改名／刪除頻道**，沒有權限檢查
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
