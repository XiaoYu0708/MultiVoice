# 接續工作清單

> 最後更新：2026-11-15。完成一項就把它從這裡移除或打勾，並同步更新 `PLAN.md` 的里程碑。

## 目前狀態
- M0（技術驗證）：本機部分完成；跨網路三項待第二台裝置
- M1（單頻道房主模式）：完成
- **M2（多頻道收聽／開麥）：完成**（commit `c3dec79`、`4323507`）
- M4（桌面 UI）：頻道卡片、**全域 PTT／廣播鍵／靜音鍵、提示音完成（待遊戲實測）**；系統匣、Profile、懸浮視窗未做
- M3（房主轉移）、M5、M6、M7：未開始

使用者指定的順序是：試用桌面版 → M2 多頻道 → 全域 PTT 熱鍵（完成，待實測）→ 下一步請問使用者（M4 剩下的系統匣等，或 M3／M5）。

---

## 1. 全域 PTT 熱鍵（M4）— 程式完成，**待使用者在遊戲中實測**

已完成：
- `crates/hotkeys`：LL hook（不攔截）、按鍵解析（`Hotkey` 的 `FromStr`/`Display`）、`held()`（多按其他修飾鍵也算）、
  錄製模式、`watch_presses`（切換型按鍵的按下計數，短按不漏）、`is_admin` / `relaunch_as_admin`。
  手動測試工具：`cargo run -p hotkeys --example probe --release`
- session：`TalkMode::Ptt` + `ChannelCfg::ptt_key`；`AudioOpts` 加 `broadcast_key`、`mute_key`、`cues`；
  遮罩邏輯在 `session::talk::TalkState`（純函式、有測試），PTT 不經過 VAD、放開後每槽位延長 160 ms；
  提示音 `talk::TonePlayer` 混進播放端（PTT 依槽位不同音高、廣播較高、靜音下降音；跟著輸出音量，拒聽時也播）
- desktop：說話下拉加「按鍵說話」+ 設定按鍵按鈕（選了還沒設鍵會直接進入錄製）；語音設定加全域廣播鍵／靜音鍵／提示音；
  `settings.toml` 的 `ptt_keys`（頻道名稱 → 鍵）加入頻道時自動套用（含自動加入的大廳）；非管理員時顯示提示與「以系統管理員身分重新啟動」
- 驗證：用 `SendInput` 模擬 F20（PTT）／F21（靜音鍵）+ PrintWindow 截圖，確認「送出中」與「已靜音」切換正確

待做：
- [ ] 使用者實測：遊戲在前景、遊戲以管理員執行、滑鼠側鍵、提示音聽感（音量 0.12、45 ms×2）
- [ ] 「語音設定」裡全域按鍵區塊還沒截圖確認排版（摺疊區塊預設關閉，截圖工具點不開）
- [ ] CLI 沒有 PTT 參數（需要的話加 `--ptt 鍵`）
- [ ] 偵測 UIPI：目前只是非管理員時一律顯示提示；可改成「遊戲在前景但 `event_count` 不動」時才提示

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
