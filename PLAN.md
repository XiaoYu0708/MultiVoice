# MultiVoice — 多頻道同時收聽／同時開麥的遊戲語音軟體

> 代號 `MultiVoice`，正式名稱之後再定。
> 只做語音，**不做**文字訊息、檔案傳送、視訊、螢幕分享。
> 架構：**房主模式（Host-as-Server）+ 房主轉移**，不需要自架獨立伺服器。
> 規劃日期：2026-11-15

---

## 1. 產品定位

一般語音軟體（Discord、TeamSpeak、Mumble）的模型是「一次只能待在一個語音頻道」，而且都需要中央伺服器。
本專案的核心差異：

1. **同時收聽任意多個頻道**：例如同時聽「小隊頻道」「指揮頻道」「後勤頻道」。
2. **同時在任意多個頻道開麥**：例如一直對「小隊」開放麥克風，按住 `Mouse4` 才對「指揮」說話，按住 `Mouse5` 同時對全部頻道廣播。
3. **每個頻道各自控制**：音量、靜音、左右聲道定位、說話模式（關／常開(VAD)／按鍵說話 PTT）。
4. **不需要伺服器**：任何一位桌面版玩家按「建立房間」就成為房主，房主的客戶端負責轉發語音；房主離開或斷線時**自動轉移給下一位**，其他人不需要重新加入。

目標使用情境：公會戰／GVG 指揮、多小隊協同、MMO 團隊副本、朋友開黑。規模目標：單一房間 **50～100 人**（受房主上傳頻寬限制，見 §4.4）。

### 非目標（第一版不做）
- 文字聊天、私訊、表情、檔案
- 視訊、螢幕分享
- 端對端加密（架構上可做，見 §10）
- 手機原生 App（先做網頁版，見 §8；Rust 核心保留共用）

---

## 2. 核心概念與語意

### 2.1 名詞

| 名詞 | 意思 |
|---|---|
| Room（房間） | 一組頻道 + 成員 + 權限，由 `RoomId` 識別，**不綁定任何一台機器** |
| Host（房主） | 目前負責轉發語音與維護房間狀態的那個客戶端，可轉移 |
| Channel（頻道） | 房間內的語音頻道，可巢狀（樹狀），有各自權限 |
| Member（成員） | 一個使用者，以其 iroh 公鑰 `NodeId` 作為永久身分 |
| Subscription（訂閱） | 成員「進入」某頻道的狀態，包含是否收聽、說話模式 |
| Talk Target（說話目標） | 這一刻麥克風聲音要送往的頻道集合 |
| Term（任期） | 每換一次房主 +1，用來判斷誰是合法房主 |

### 2.2 每個頻道的使用者狀態

使用者對每個已加入的頻道有兩個獨立設定：

- `listen: bool`：是否收聽這個頻道
- `talk_mode: Off | Open(VAD) | PushToTalk(key)`：在這個頻道的說話方式

另外允許「只說不聽」（例如指揮對某小隊下令但不想聽該小隊的雜音），由頻道權限 `talk_without_listen` 控制。

**隱私規則**：只要有收聽某頻道，就一定會出現在該頻道的成員列表（不允許隱形竊聽）。只說不聽的人也會顯示，圖示不同。

### 2.3 每一個音訊幀（20 ms）的說話目標計算

```
active_targets =
    { ch | ch.talk_mode == Open  且 VAD 判定有聲音 }
  ∪ { ch | ch.talk_mode == PTT(k) 且 k 正被按住 }
  ∪ { 全域廣播鍵按住時：所有 talk_mode != Off 的頻道 }
  − 全域靜音時：全部清空
```

- `active_targets` 為空時不送封包；剛變空時送一個 end-of-stream 標記。
- **麥克風只編碼一次**：同一份 Opus 封包附帶目標集合送到房主，由房主扇出（fan-out）。不會因為對 5 個頻道說話就耗掉 5 倍的上傳頻寬與 CPU。

### 2.4 接收端去重與音量規則

- A 同時對頻道 1、2 說話，B 同時收聽 1、2 時，**B 只會收到一份** A 的聲音（房主去重）。封包會附帶「經由哪些頻道」。
- B 端該串流的實際增益：
  `gain = user_volume(A) × max( channel_volume(c) for c in via_channels if c 未被 B 靜音 )`
  如果全部 via 頻道都被靜音，就不播放。
- 聲道定位（pan）：取 via 頻道中音量最大那個頻道的 pan 設定。
- 優先頻道（ducking）：可把某頻道設為「優先」，該頻道有人說話時，其他頻道自動降低音量（例如 -12 dB）。

---

## 3. 系統架構

### 3.1 整體拓撲

```
                     ┌──────────── 房主（某位桌面版玩家）────────────┐
                     │  一般客戶端功能（自己也在說話/收聽）           │
                     │  + host-core：房間狀態、扇出、去重、權限        │
                     └───────▲──────────▲──────────▲─────────────────┘
          iroh QUIC（直連優先， │          │          │  打洞失敗才走 relay）
                     ┌───────┴──┐ ┌─────┴────┐ ┌───┴───────────┐
                     │ 桌面成員  │ │ 桌面成員  │ │ 網頁成員(手機) │
                     │ (候補房主)│ │          │ │ 一律經 relay   │
                     └──────────┘ └──────────┘ └───────────────┘
   每位成員都持有一份「房間狀態副本」，所以房主消失時候補者能立即接手。
```

- **星狀拓撲，不是網狀**：所有語音都經房主轉發，每位說話者上傳固定 1 路，與收聽人數無關。
- **房主 = 普通客戶端 + `host-core`**：伺服器邏輯是一個函式庫，跑在房主的客戶端行程裡。
- 想要「永遠在線的房主」時，同一份 `host-core` 可以包成無介面的 `headless-host`，放在這台電腦的 Docker 裡當常駐成員。它是可選的，不是必要元件（見 §7.3）。

### 3.2 客戶端內部

```
┌─────────────────────────── Client (Rust, Windows) ────────────────────────────┐
│  Mic ─► cpal capture ─► resample 48k ─► 降噪/AGC ─► VAD ─► Opus encode (一次)  │
│  全域熱鍵 hook (PTT) ─────────► Talk Target 計算 ──┘                           │
│                                                   ▼                           │
│                              iroh datagram → 目前房主                          │
│  Speaker ◄─ cpal output ◄─ Mixer ◄─ 每個說話者 jitter buffer ◄─ Opus decode     │
│                             ▲  (per-channel 音量/pan/ducking/靜音)             │
│  egui GUI / 系統匣 ◄──── 控制狀態（iroh reliable stream）                       │
│                                                                               │
│  [房主時才啟用] host-core：接受連線、扇出、權限、狀態複製、選定候補房主           │
└───────────────────────────────────────────────────────────────────────────────┘
```

### 3.3 關鍵設計決定

| 決定 | 選擇 | 理由 |
|---|---|---|
| 拓撲 | **房主星狀轉發（SFU），不解碼不混音** | 說話者上傳固定；房主 CPU 成本極低（只轉送封包），不影響房主自己玩遊戲 |
| 混音位置 | 各客戶端 | 每頻道音量／pan／ducking 本來就是個人化設定 |
| 傳輸 | **iroh 1.x**（QUIC）：控制走 reliable stream、語音走 unreliable datagram | 用公鑰撥號、自動打洞、失敗自動走 relay，不用轉埠、不用固定 IP 或 DDNS、不用申請憑證；內建 TLS 1.3；支援 multipath（Wi-Fi ↔ 行動網路切換不斷線）；可編譯成 WASM 給網頁版用 |
| 身分 | 每位使用者一把本機產生的 iroh 金鑰，`NodeId` 就是帳號 | 不需要註冊、不需要帳號伺服器；封鎖、權限都綁 `NodeId` |
| 編碼 | Opus 48 kHz mono、20 ms 幀、預設 32 kbps（可調 16–96）、開 in-band FEC | 遊戲語音業界標準 |
| 序列化 | `serde` + `postcard`（控制訊息）；語音封包手寫二進位標頭 | 所有角色共用同一個 `protocol` crate |
| 房主轉移 | 固定順序的候補名單 + 多數確認 + 任期號 | 正常情況不需要選舉，轉移快；異常情況用任期號避免雙房主（見 §5） |

---

## 4. 網路協定（草案）

### 4.1 連線與加入

**邀請票（Room Ticket）**：一段 base32 字串或連結，內容：
```
room_id        32 bytes，建房時隨機產生，永久不變
room_secret    32 bytes，用來證明「我有被邀請」（可重新產生以作廢舊邀請）
bootstrap      最近已知的數個成員 NodeId（含當下房主與候補房主）
```

**加入流程**：
1. 依序撥號 `bootstrap` 中的 NodeId（iroh 會自動解析位址與打洞）。
2. 對方如果是房主就繼續握手；如果不是，回覆 `Redirect { host: NodeId, term }`，**任何成員都能做重新導向**。
3. 送出 `Join { room_id, proof = HMAC(room_secret, host_challenge), display_name, client_kind }`。
4. 房主回覆 `Welcome`，內容包含完整房間狀態快照。

### 4.2 控制訊息（iroh 雙向 stream，長度前綴 + postcard）

Member → Host：
- `Join { ... }` / `Resume { session_idx, subscriptions, talk_targets }`：換房主後重新登記自己的狀態
- `Subscribe { channel_id, listen, talk }` / `Unsubscribe { channel_id }` / `SetListen { channel_id, listen }`
- ~~`RegisterTalkTarget`~~（M2 實作時取消，見 §4.3）。改為 `Subscribe { channel, slot, listen, talk }`：**槽位由成員自己挑**，之後上行 `talk_mask`、下行 `via_mask` 都用槽位
- `SetSelfState { muted, deafened }`
- `AdminAction { ... }`：建立/刪除頻道、改權限、踢人、封鎖、指定房主
- `Ping { t }` / `HostLostVote { term }`（見 §5.3）

Host → Member：
- `Welcome { session_idx, term, room_state, successors }`
- `StateDelta { term, version, delta }`：房間狀態增量（頻道、權限、名冊、訂閱），**所有成員都套用以維持副本**
- `ChannelSlot { slot: u8, channel_id }`：為該連線分配頻道槽位（用於 via 位元遮罩）
- `Successors { term, list: [NodeId; ≤3] }`：候補房主名單
- `MigrateTo { new_host, new_term }`：計畫性轉移通知
- `Heartbeat { term, version }`（每 500 ms）
- `Redirect { host, term }`、`Error { code, message }`、`Pong { t }`

### 4.3 語音 datagram

Member → Host（6 byte 標頭 + Opus payload）：
```
u8  kind        = 0x01 voice | 0x02 end-of-stream
u64 talk_mask   要對哪些頻道說話（說話者自己的槽位）；M2 起取代 target_id，標頭變 13 bytes
u32 seq         每幀 +1；換房主、換裝置後都不重置，接收端 jitter buffer 不受影響
[...] opus payload（20 ms @ 32 kbps ≈ 80 bytes）
```

Host → Member：
```
u8  kind
u16 sender_idx  房間內的成員索引（記錄在名冊中，換房主後不變）
u32 seq
u64 via_mask    此連線的頻道槽位遮罩（單連線最多訂閱 64 個頻道）
[...] opus payload（原封不動轉送）
```

### 4.4 房主扇出演算法與頻寬

```
on voice datagram from S with target_id t:
    channels = S.targets[t] ∩ S 有說話權限的頻道
    for ch in channels:
        for R in ch.listeners:
            if R == S or R.deafened or R 封鎖 S: continue
            out[R].via_mask |= R.slot_of(ch)
    for (R, mask) in out: R.send_datagram(header(S, seq, mask) + payload)
```
- 成員或 target 變動時預先算好「target → 收聽者 → mask」快取，熱路徑只做查表。
- 單一收聽者同時接收的說話者上限預設 16 人，超過時依「優先頻道 > 最近開始說話」挑選。

| 角色 | 頻寬 |
|---|---|
| 一般成員上傳 | 固定約 45 kbps（含標頭），與開麥頻道數無關 |
| 一般成員下載 | 同時說話人數 × 約 45 kbps |
| **房主上傳** | Σ(每位說話者 × 他的收聽人數) × 約 45 kbps。例：5 人同時說話、每人 50 位收聽者，約 **11 Mbps** |
| 房主 CPU | 只轉送不解碼，數百路封包/秒，幾乎可忽略 |

客戶端啟動時會量測上傳頻寬與 NAT 類型，用來決定能不能當房主，以及候補順序（§5.1）。

---

## 5. 房主轉移

### 5.1 誰能當房主（候補資格）

必須**全部**符合：
- 桌面版客戶端（網頁版與日後的手機版不能當房主：瀏覽器只能走 relay，背景會被暫停）
- 身分組權限 `can_host`（房間擁有者可設定，預設「成員」以上）
- 使用者沒有關閉「允許我成為房主」

候補排序分數（房主每 10 秒重新計算一次，有變動才廣播 `Successors`）：
1. 是否為直連（不經 relay）：權重最高
2. 量測到的上傳頻寬
3. 在房間內的連續在線時間
4. `headless-host`（常駐節點）永遠排第一

### 5.2 計畫性轉移（房主主動離開或手動指定）：先建後拆，聲音中斷 < 300 ms

```
1. 舊房主 → 全體：  MigrateTo { new_host = successors[0] 或指定對象, new_term = term + 1 }
2. 新房主：         以本機的狀態副本啟動 host-core，開始接受連線
3. 全體成員：       先撥號連上新房主並送 Resume（此時仍經舊房主收發語音）
4. 成員 Resume 成功後，語音改送新房主；舊房主在 2 秒內繼續轉發殘留封包後關閉
5. 舊房主降級為一般成員（若沒有離開房間）
```

### 5.3 非預期轉移（房主當機、斷網、關機）：目標 3～5 秒內恢復

**偵測**：連續 2.5 秒沒收到房主的 `Heartbeat`，且 QUIC 連線也判定斷開，就視為「房主失聯」。

**接手規則**（候補名單在事前已廣播，所以不需要臨時選舉）：
```
成員端：
  - 房主失聯 → 撥號 successors[0]，送 HostLostVote{term}
  - 1.5 秒內 successors[0] 沒回應 → 改試 successors[1]，依此類推
候補端（successors[i]）：
  - 自己也偵測到房主失聯，且等待 i × 1.5 秒仍沒有更前面的候補接手
  - 收到「舊名冊中除舊房主外，多數成員」的 HostLostVote → 以 term+1 升任房主
  - 10 秒內湊不到多數，但也聯絡不上任何人 → 仍升任（避免整間卡死，衝突由 §5.4 解決）
```
多數確認是為了防止「只有候補自己斷網、其實房主還活著」時誤升任。

### 5.4 衝突與恢復

- **任期號決勝**：任何節點收到 `term` 比自己認知更高的訊息，就承認對方為房主。
- **雙房主（網路分裂後重新接觸）**：比較 `term`，高者勝；`term` 相同時，比較連線成員數，再比較 `NodeId`。輸的一方送 `Redirect` 讓自己的成員轉過去，然後降級。
- **舊房主回來**：發現有更高 `term`，就以一般成員身分 `Resume` 加入。
- **狀態遺失**：轉移瞬間最後幾筆 `StateDelta` 若有成員沒收到，以新房主的副本為準（只可能遺失轉移前約 1 秒內的管理操作，管理員重做即可）。
- **每位成員自己的訂閱狀態由成員本人負責**：轉移後透過 `Resume` 重新登記，所以新房主的副本不需要包含訂閱的最新狀態。

### 5.5 房間的生命週期

- 最後一個人離開時，房間「沉睡」。每位成員本機都存有最後的房間設定（頻道樹、身分組、封鎖名單）。
- 任何 `can_host` 成員之後用同一張邀請票進來，如果找不到人，可以選擇「喚醒房間」，以本機副本當房主。多份副本版本不同時，以 `(term, version)` 最大者為準，其他人加入時會自動同步。
- 擁有者可以隨時重新產生 `room_secret`，讓舊邀請票失效。

### 5.6 信任模型（第一版）

- 房主能看到所有封包的中繼資料，也能拒絕轉發。所以**只有受信任的身分組才給 `can_host`**。
- 進階版本（M8 之後）：管理操作改由管理員的金鑰簽章，房主只能轉送、不能偽造；搭配端對端加密，房主就看不到語音內容。

---

## 6. 客戶端技術選型（Rust）

| 領域 | Crate | 備註 |
|---|---|---|
| P2P 傳輸 | `iroh` 1.x（至少 1.1，含安全修正） | datagram 與 stream；預設使用 n0 公共 relay（見 §9） |
| 音訊 I/O | `cpal`（WASAPI） | 低延遲 shared mode；裝置熱插拔要自己監聽並重建 stream |
| 重取樣 | `rubato` | 裝置非 48 kHz 時使用 |
| Opus | `audiopus` / `opus`（libopus 靜態編譯） | 需要 cmake，本機已有 WinLibs 的 cmake |
| 降噪 + VAD | `nnnoiseless`（RNNoise 純 Rust 移植） | 順便提供 VAD 機率值。戴耳機的玩家通常不需要 AEC，先不做 |
| 執行緒間傳遞 | `rtrb` / `ringbuf`（lock-free SPSC） | 音訊 callback 內**禁止**配置記憶體、上鎖、寫 log |
| 非同步 | `tokio` | |
| GUI | `eframe` / `egui` | 與 `副本退出快捷腳本` 一致，可沿用經驗 |
| 系統匣 | Win32 `Shell_NotifyIconW`（`windows-sys`） | 原訂 `tray-icon`，改自己做：需要在 egui 視窗隱藏時繼續更新圖示 |
| 全域熱鍵 / PTT | `windows-sys` `WH_KEYBOARD_LL` + `WH_MOUSE_LL` | 與 QuickExit 相同作法，支援滑鼠側鍵；PTT 預設**不攔截**按鍵 |
| 設定與房間副本 | `serde` + `toml`（設定）、`postcard` 檔（房間副本） | `%APPDATA%\MultiVoice\` |
| 日誌 | `tracing` + `tracing-appender` | |

### 6.1 執行緒模型
```
[音訊輸入 callback] ──ring──► [編碼執行緒] ──channel──► [tokio：iroh 連線]
[tokio：iroh 連線] ──per-sender queue──► [解碼+混音執行緒] ──ring──► [音訊輸出 callback]
[熱鍵 hook 執行緒] ──atomic 位元──► [編碼執行緒讀取 PTT 狀態]
[tokio：host-core]（房主時才啟動，與一般客戶端邏輯同行程、不同 task）
[GUI 執行緒] ◄──狀態快照 (ArcSwap)── 其他執行緒
```
房主自己的聲音也走同一套路徑，透過行程內 loopback 送進 host-core，所以房主與一般成員的程式路徑一致。

### 6.2 Jitter buffer
- 每個說話者一個（一人只有一條串流，不是每頻道一個）。
- 自適應目標延遲 20–100 ms。走 relay 的串流起始值設高一些。
- 遺失處理：下一包有 FEC 就用 FEC 復原，否則用 Opus PLC；連續遺失超過 100 ms 視為停止說話。
- 換房主期間：`seq` 連續、`sender_idx` 不變，jitter buffer 不用重建，只會出現一段 PLC 補償的空白。

### 6.3 延遲預算
| 階段 | 直連 | 經 relay |
|---|---|---|
| 擷取 + 幀長 + 編解碼 | ~32 ms | ~32 ms |
| 說話者 → 房主 → 收聽者（兩段網路） | 20–60 ms | 40–120 ms |
| Jitter buffer | 20–60 ms | 40–100 ms |
| 輸出緩衝 | ~10 ms | ~10 ms |
| **口到耳合計** | **約 80–160 ms** | **約 120–260 ms** |

房主模式比中央伺服器多一個變數：**房主的地理位置與網路品質**。這也是候補排序要看直連與頻寬的原因。

### 6.4 遊戲相關注意事項
- 遊戲如果以系統管理員身分執行，非管理員程式的 LL hook 會收不到按鍵（UIPI）。比照 QuickExit 提供「以管理員身分重新啟動」。
- 不注入遊戲、不做 in-game overlay（避免觸發反作弊）。說話提示改用系統匣圖示、可選的透明置頂小視窗、提示音。
- 低 CPU：閒置時 < 1%，當房主且 10 人同時說話時 < 3%。
- 房主的遊戲網路會被語音上傳佔用一部分頻寬。UI 要顯示房主負載，並提供「把房主讓給別人」按鈕。
- 開麥／關麥提示音（每頻道可選不同音），不用看畫面也知道現在對哪裡說話。

---

## 7. 專案結構（Cargo workspace）

### 7.1 目錄

```
遊戲/MultiVoice/
├─ PLAN.md                 ← 本文件
├─ README.md
├─ Cargo.toml              ← [workspace]
├─ .cargo/config.toml      ← target-dir 指到純英文路徑（見 §11）
├─ crates/
│  ├─ protocol/            ← 控制訊息、語音封包、邀請票、房間狀態型別與 delta（全角色共用、可編 WASM）
│  ├─ audio/               ← 擷取/播放/重取樣/降噪/VAD/Opus/jitter buffer/混音器（原生）
│  ├─ mixer-core/          ← 混音、jitter buffer 的純邏輯部分（無 I/O，原生與 WASM 共用）
│  ├─ host-core/           ← 房主邏輯：扇出、去重、權限、狀態複製、候補排序（無 UI、無音訊）
│  ├─ client-core/         ← 連線、加入/重新導向、訂閱、talk target、房主轉移的成員端狀態機
│  ├─ transport/           ← iroh 端點設定（relay 清單）、控制訊息讀寫
│  ├─ session/             ← 建房／加入、音訊管線（擷取→降噪→VAD→編碼、jitter→解碼→混音）、狀態快照
│  ├─ desktop/             ← egui 桌面程式（bin：multivoice.exe），透過 session 內嵌 host-core
│  ├─ cli/                 ← 命令列客戶端（bin：multivoice-cli.exe），除錯與自動測試用
│  ├─ web/                 ← 網頁客戶端（wasm-bindgen + AudioWorklet），見 §8
│  ├─ headless-host/       ← 無介面常駐房主（可選，Docker）
│  └─ sim/                 ← 模擬器：在一個行程內起 N 個虛擬成員，測試扇出語意與房主轉移
├─ deploy/
│  ├─ Dockerfile           ← headless-host 用（多階段建置，Linux 容器內編譯，不受中文路徑影響）
│  └─ docker-compose.yml
└─ build.ps1               ← 打包 desktop release，沿用 QuickExit 的腳本模式
```

### 7.2 分層原則
- `host-core`、`client-core`、`mixer-core`、`protocol` **不碰作業系統 API**，可以在 `sim` 裡用假網路（可注入延遲、遺失、斷線）跑自動化測試。房主轉移的正確性主要靠這一層的測試。
- `desktop` 與 `web` 只是最外層。日後做手機原生 App 時，重用 `client-core` + `audio`（Android 用 `cpal` 的 AAudio、iOS 用 CoreAudio），只換介面。

### 7.3 這台電腦的角色（固定公開 IP + Docker）

這台電腦有**固定公開 IP**，所以除了當玩家，還可以提供三個「基礎設施」服務。它們全都是**可選的**：這台電腦關機時，房間會轉移給玩家，照常運作；只是網頁成員改走 n0 公共 relay。

```
            網際網路（固定公開 IP）
                  │  TCP 80/443、UDP <relay QAD 埠>、UDP <host 埠>
┌─────────────────┴──── 這台電腦 (Docker Desktop) ───────────────────┐
│  caddy        自動申請 Let's Encrypt 憑證、HTTPS 終結                 │
│   ├─ https://voice.<網域>/   → 網頁客戶端靜態檔                       │
│   └─ https://relay.<網域>/   → iroh-relay（WebSocket 反向代理）       │
│  iroh-relay   自架 relay：網頁成員、打洞失敗的成員都走這裡，無限流     │
│  headless-host 常駐房主：can_host、候補第一，平常就由它轉發             │
└───────────────────────────────────────────────────────────────────┘
```

| 服務 | 用途 | 對外埠 |
|---|---|---|
| `caddy` | HTTPS 憑證（自動申請與續期）、託管網頁版、反向代理 relay | TCP 80（ACME 驗證、轉址）、TCP 443 |
| `iroh-relay` | 自架 relay；瀏覽器透過 **WSS** 連線，所以一定要有有效的 HTTPS 憑證 | 經 Caddy 走 443；另有一個 UDP 埠做 QUIC 位址探測（QAD），埠號以 M0 時 iroh 文件為準 |
| `headless-host` | 常駐房主。有公開 IP，大家都能直連，延遲最穩 | 一個固定 UDP 埠（例如 `11204/udp`） |

- 已確認本機 TCP 80/443 目前沒有被佔用，與現有容器不衝突。
- **網域：`xiaoyu0708.ip-ddns.com`**（ClouDNS 免費網域，**NS 已委派給 Cloudflare**，所以 DNS 紀錄在 Cloudflare 後台管理）。
  - 根網域目前指向 GitHub Pages（既有網站，**不要動**）。本專案只新增兩個子網域：
    ```
    voice.xiaoyu0708.ip-ddns.com   A   <固定 IP>   Proxy: DNS only（灰雲）
    relay.xiaoyu0708.ip-ddns.com   A   <固定 IP>   Proxy: DNS only（灰雲）
    ```
  - **一定要設成灰雲（DNS only）**：`relay` 除了 WSS，還有 UDP 位址探測（QAD），而 Cloudflare 代理不轉送任意 UDP；客戶端也需要拿到真實 IP。`voice` 也用灰雲，讓 Caddy 直接處理憑證，比較單純。
  - `ip-ddns.com` 在 Public Suffix List 的 ClouDNS 區段中（2026-11-15 查證），所以 Let's Encrypt 會把 `xiaoyu0708.ip-ddns.com` 當成獨立網域計算配額，不會和其他 ClouDNS 免費使用者共用簽發額度。
  - **憑證申請方式**：
    - 預設：官方 Caddy 映像，用 HTTP-01（TCP 80）或 TLS-ALPN-01（TCP 443），兩種都會自動嘗試。
    - 進階選項：因為 DNS 在 Cloudflare，可以改用 **DNS-01**（Caddy 加裝 `caddy-dns/cloudflare` 外掛，用只有 `Zone:DNS:Edit` 權限的 Cloudflare API Token）。好處是不用開 TCP 80，也能申請萬用字元憑證；缺點是要自建 Caddy 映像，並妥善保管 Token（放在 `.env`，不納入版本控制）。
  - 備援網頁託管可以直接用根網域現有的 GitHub Pages（例如 `xiaoyu0708.ip-ddns.com/multivoice/`）。
- **Docker Desktop 的 UDP 待驗證**：容器經過 WSL2 的 NAT，對外的來源埠可能被改寫，影響 `headless-host` 的直連與打洞。M0 要實測。若效果不好，`headless-host` 改成 Windows 原生程式（開機自動啟動），`caddy` 與 `iroh-relay` 留在 Docker（relay 走 TCP/WSS 為主，不受影響）。
- 路由器需要把上表的對外埠轉發到這台電腦，並在 Windows 防火牆開放。

---

## 8. 網頁客戶端（給手機玩家）

### 8.1 技術
| 部分 | 做法 |
|---|---|
| 傳輸 | iroh 編譯成 WASM（`default-features = false`）。**瀏覽器無法收發 UDP，所以一律經 relay 連到房主** |
| 共用邏輯 | `protocol`、`client-core`、`mixer-core` 編成 WASM |
| 麥克風 | `getUserMedia({ echoCancellation, noiseSuppression, autoGainControl })`：手機常開喇叭，瀏覽器內建的 AEC 正好補上原生版沒做的部分 |
| 編解碼 | 支援 WebCodecs `AudioEncoder/AudioDecoder`（Opus）時優先使用，否則改用 libopus 編成的 WASM |
| 混音 | AudioWorklet 內跑 `mixer-core`（per-channel 音量／pan／ducking 與桌面版一致） |
| PTT | 畫面上每個頻道一顆大按鈕 + 全域廣播鈕；預設用 VAD 常開 |
| 託管 | 純靜態檔案，由這台電腦的 Caddy 以 HTTPS 提供（`https://voice.<網域>/`，見 §7.3）；備案為 GitHub Pages / Cloudflare Pages。getUserMedia、WebCodecs、AudioWorklet 都**必須在 HTTPS 下**才能用 |
| 邀請連結 | `https://<網頁>/#<room ticket>`：票放在 `#` 後面，不會傳到託管網站 |
| 安裝 | PWA，可加到主畫面 |

### 8.2 必須先講清楚的限制
- **iOS：網頁切到背景（切去玩遊戲、鎖屏）時麥克風會被切斷。** Android 在背景也不穩定。所以網頁版適合「用另一台裝置在旁邊聽／說」，或在電腦瀏覽器上使用，**不適合在同一支手機上邊玩邊講**。手機要邊玩邊講，最後還是要做原生 App（列為 M8 之後）。
- 網頁成員一律走 relay，延遲比桌面版高約 40–100 ms。
- 網頁成員不能當房主。

---

## 9. Relay 策略

iroh 的 relay 只在打洞失敗時，以及網頁成員連線時使用。流量是端對端加密的 QUIC，relay 看不到內容。

| 優先序 | 使用的 relay | 說明 |
|---|---|---|
| 1（主要） | **自架 `iroh-relay`**：`https://relay.<網域>/`，跑在這台電腦（§7.3） | 固定公開 IP + HTTPS，沒有限流；網頁成員全靠它 |
| 2（備援） | n0 免費公共 relay | 這台電腦關機時使用。有流量限制，數值不公開，官方定位為開發測試用 |

- iroh 支援設定多個 relay（`RelayMode::Custom` 的 relay map），客戶端會挑延遲低且可用的那個。內建預設值為「自架 + n0 公共」。
- 客戶端設定提供「自訂 relay URL」。房主可以在房間設定中指定 relay，並透過 `Welcome` 下發給成員。
- 自架 relay 至少要用 iroh 1.1（修正了 relay 端的安全問題）。
- 建議在 M0 實測：一路語音（約 45 kbps）經 n0 公共 relay 長時間傳輸，會不會被限流（決定備援能撐多久）。

---

## 10. 開發里程碑

### M0：技術驗證（1 週）
- [x] `audio`：麥克風 → Opus → 解碼 → 喇叭 本機迴路，量測延遲（`examples/loopback.rs`）
  - 2026-11-15 實測（Seiren V3 Mini → Barracuda X，WASAPI shared，輸出緩衝 20 ms）：**端到端約 65 ms**（拍手起點從麥克風擷取到耳機播放，多次量測一致），20 秒內 0 欠載、0 漂移修正
  - 未包含：無線耳機的 2.4 GHz 傳輸延遲（Windows 不知道這段）
  - 可再壓低的地方：輸出緩衝目標 20 → 10 ms；WASAPI 輸出裝置延遲約 32 ms，可評估 IAudioClient3 低延遲模式或獨佔模式（M5）
- [x] 確認 libopus 能在 `x86_64-pc-windows-gnu` 工具鏈下靜態編譯（2026-11-15：32 kbps 平均封包 82.7 bytes，release 版編+解碼每幀 0.32 ms）
- [ ] iroh datagram：兩台不同網路的電腦互傳，分別量測直連與經 relay 的 RTT 和遺失率；測試公共 relay 長時間傳一路語音會不會被限流
  - 工具：`crates/netprobe`（`listen` / `connect <EndpointId>` / `local`，可加 `--relay-only`），每 20 ms 送 100 bytes 模擬語音
  - 2026-11-15 本機互測（iroh 1.4.1）：
    - 直連：RTT 中位數 0.3 ms，0% 掉包，連線建立 1 ms，啟動後 1 秒內選定直連
    - 強制經 n0 公共 relay（自動選到 `aps1` 新加坡）：RTT 最小 150 ms、**中位數 238 ms、p95 410 ms**、抖動 28 ms、0% 掉包（20 秒 1000 包）。本機互測走了 4 趟台灣↔新加坡，實際「成員→relay→房主」約為一半
    - 沒有出現限流警告；中位數比最小值高約 90 ms 且起伏大，推測是 relay 路徑（TCP/WebSocket）上的排隊
    - **結論：公共 relay 對台灣玩家太遠，自架 relay（§7.3）是必要的，不只是備案**
  - [ ] 尚未完成：兩台不同網路的電腦互測（驗證打洞成功率與真實 RTT）
- [x] 這台電腦：Caddy + 自架 iroh-relay（2026-11-15，`deploy/`）
  - Caddy 自動申請 `voice.` / `relay.` 的 Let's Encrypt 憑證成功；iroh-relay 以 `Reloading` 模式直接讀 Caddy 的憑證檔，Caddy 以 HTTPS 反向代理到 relay 的 8443；QAD 走 UDP 7842 直接對外
  - 路由器轉發 TCP 80/443、UDP 7842 → 這台電腦；Windows 防火牆已有 Docker Desktop Backend 放行規則（公用網路），不需另外設定
  - 經公開 IP 的 HTTPS 驗證成功（路由器支援 NAT loopback）
  - `netprobe local --relay-only --relay https://relay.xiaoyu0708.ip-ddns.com/`：RTT 中位數 **3.4 ms**、p95 5.2 ms、抖動 0.8 ms、0% 掉包；上線 0.4 秒、連線建立 6 ms（公共 relay 為 238 ms / 410 ms / 28 ms）。封包未離開本地網路，外部玩家的真實延遲待第二台裝置實測
  - iroh 的 `RelayConfig` 支援 `auth_token`，可配合 relay 的 `access.shared_token` 限制只有 MultiVoice 客戶端能使用（M6）
- [ ] iroh WASM 在 Chrome Android / iOS Safari 能否連上（只驗證連線）
- [ ] 這台電腦：Cloudflare 新增 `voice.` / `relay.` 灰雲 A 紀錄 → Docker 起 `caddy` + `iroh-relay`，確認 HTTPS 憑證自動申請成功、瀏覽器能經 WSS 連上 relay
- [ ] Docker Desktop 內 UDP 的直連效果（決定 `headless-host` 放 Docker 還是 Windows 原生）

### M1：單頻道房主模式（1–2 週）
- [x] `protocol`：邀請票（`mv1` + base32）、加入挑戰／證明（blake3 keyed，綁定房間 ID、nonce 與成員 NodeId）、控制訊息、語音封包標頭
- [x] `host-core` 最小版本、`client-core` 加入流程（邀請票、Redirect 跟隨）；同一 NodeId 重連沿用 `session_idx`
- [x] `mixer-core`：自適應 jitter buffer（RFC 3550 式抖動估計、FEC/PLC、end-of-stream、過深跳過）+ 混音／柔性限幅
- [x] `client-core/tests/room.rs`：同一行程內房主 + 成員的整合測試（關閉 relay、只走本機直連）
- [x] CLI（`crates/cli`，執行檔 `multivoice`）：`host` / `join <邀請票>`，VAD 閘門（人聲機率 + 降噪後音量 + 300 ms hangover + 1 幀前補）
  - 2026-11-15 本機兩個實例互通：加入 259 ms、直連、jitter buffer 穩定在 40 ms、0 FEC/PLC/欠載
- [x] 從第一天就帶 `term`、`sender_idx` 欄位（為房主轉移鋪路）
- [ ] 跨網路實測（需要第二台裝置）

### M2：核心功能：多頻道收聽／多頻道開麥（2 週）⭐（2026-11-15 完成）
- [x] 訂閱多頻道、每頻道 `listen` / `talk_mode`（關／偵測說話／常開；PTT 見 M4）
- [x] ~~Talk Target 註冊~~ → **上行直接帶 `talk_mask`**（協定 v2）+ 扇出 + 去重 + `via_mask`
  - 設計變更理由：預先註冊的 target_id 走控制串流，語音封包可能比登記先到而被丟掉（剛按 PTT 的第一個字）；PTT 多鍵組合也要事先登記。改帶 u64 遮罩，每包多 7 bytes（約 2.8 kbps）
  - 槽位由成員自己挑並在 `Subscribe` 告知房主，不必等回覆就能說話
- [x] 房主補送 end-of-stream：說話者換了頻道組合或停止說話時，對「不再聽得到」的人補一次，避免殘留 PLC 雜音
- [x] 混音器：每頻道音量、pan（平衡式，置中不變小聲）、優先頻道 ducking（-12 dB）；輸出改為立體聲
- [x] 頻道建立／改名／刪除（大廳不能刪），所有人看得到誰在哪個頻道、收聽／說話狀態
- [x] 自動化測試（`client-core/tests/room.rs`）：「說給 {1,2} 時，同時在 1、2 的人只收到一份」、talk 權限、補送 end-of-stream、頻道生命週期；`session/examples/multichannel.rs` 驗證 UI 用的狀態快照
- [x] 桌面版頻道介面：頻道卡片（加入／離開、收聽、說話模式、進階：音量／定位／優先）、頻道內成員與「在這個頻道說話」指示燈、目前送出的頻道、新增頻道、右鍵改名／刪除
- [ ] 頻道設定的保存（目前離開房間後不保留；房間 ID 每次建房都不同，要等 M5 房間沉睡／喚醒再設計）

### M3：房主轉移（2 週）⭐
- [x] 狀態複製：改用「新房主以自己客戶端看到的頻道與成員啟動」（`host_core::Seed`），不另外廣播 `StateDelta`；存檔留待房間沉睡（M6）
- [x] 候補排序與 `Successors` 廣播（第一版只看進房先後）
- [x] 計畫性轉移（`MigrateTo`）；目前是斷開後重連，本機實測 0.4 秒，先建後拆還沒做
- [x] 非預期轉移：失聯偵測（1 秒 ping、3.5 秒沒回應）、依序撥候補（第 k 位等 k×3 秒）、任期號
- [x] 衝突處理：雙房主合併（人數 → 任期 → NodeId），舊房主回歸時讓位（協定 v4 的 `HostProbe`／`MoveTo`）
- ~~`HostLostVote` 多數確認~~（改由合併處理分裂；見 NEXT.md）
- [ ] `sim` 混沌測試：隨機殺掉房主、切斷部分成員網路、轉移途中再殺新房主，驗證最終只剩一位房主且所有人都連上

### M4：桌面客戶端 UI（2 週）
- [x] **提前完成單頻道版（2026-11-15，使用者要求先有 UI）**：`crates/desktop`（執行檔 `multivoice.exe`，eframe/egui 0.37）
  - 大廳：名稱、建立房間、貼上邀請票加入、音訊設定（裝置、開麥方式、降噪、小聲講話保留）
  - 房間：複製邀請票、自己的麥克風音量表與送出狀態、成員列表與說話指示燈、靜音／拒聽／離開、連線狀態（直連/relay、緩衝、FEC/PLC、欠載）
  - 設定存在 `%APPDATA%\MultiVoice\settings.toml`（非 default 的身分設定檔各自一份）
  - 測試用參數：`--profile`、`--auto-host`、`--auto-join <邀請票>`、`--deafen`
  - 共用邏輯抽到 `crates/session`（建房／加入 async、音訊同步啟動；cpal 串流不是 `Send`，Session 留在 UI 執行緒）
  - 不用 emoji：微軟正黑體沒有 emoji 字形，會顯示成方框
- [x] egui 主視窗：頻道卡片（加入／離開、收聽、說話模式、音量／定位／優先、頻道內說話指示）；房主可改名／刪除頻道
- [x] 全域 PTT 熱鍵（每頻道一鍵），支援滑鼠側鍵（`crates/hotkeys`；待遊戲實測）
  - 使用者決定只做按鍵說話：**全域廣播鍵、全域靜音鍵不做**（§2 的廣播鍵設計保留作參考）
- [x] 系統匣、開關麥提示音、裝置選擇、設定存檔
  - 系統匣不用 `tray-icon`：它要在 UI 執行緒更新，egui 視窗藏起來後畫面迴圈會停，遊戲中反而看不到狀態。
    改成獨立執行緒 + Win32 `Shell_NotifyIconW`（`desktop/src/tray.rs`），圖示顏色：灰＝未進房、藍＝在房間、綠＝送出中、紅＋斜線＝靜音／拒聽
  - 拒聽同時關麥（取消拒聽恢復原本的靜音）
- [x] 多組「設定檔」（Profile），例如「公會戰指揮」「一般副本」，一鍵切換訂閱組合（以頻道名稱對應）

### M5：音質與穩定性（1–2 週）
- [x] 重取樣：非 48 kHz 裝置（例如 44.1 kHz 耳機）用 rubato 5 的 `Async::new_poly` 轉換（`audio::resample`，在擷取／播放執行緒做）
- [x] nnnoiseless 降噪（2026-11-15 提前完成，`audio::denoise`）：實際房間噪音峰值 -23 dBFS → 降噪後 -57 dBFS（約 35 dB），多 10 ms 延遲，單幀處理（含編解碼）< 1 ms
  - 合成噪音測試：風扇類偏低頻噪音可壓低 35～50 dB；但**寬頻噪音在 -25 dBFS 以上只能壓低 1～3 dB**
  - 訊噪比低時（實測本機底噪 -40 dBFS RMS，61% 落在人聲頻段）RNNoise 會連小聲講話一起壓掉。對策：**人聲時混回原音**（`speech_mix`，預設 0.3；以 RNNoise 延遲 480 取樣對齊；用上一段與這一段的人聲機率判斷，等於 10 ms 前瞻；收回 150 ms）。使用者試聽後確認可接受
  - 調低增益**不會**改善訊噪比；UI 的麥克風測試要引導「靠近麥克風、指向正確」，並顯示底噪頻段分析（`examples/mic_probe.rs`）
  - 開著降噪時拍手會被壓掉，量延遲要用 `--no-denoise`
  - 若之後仍不夠：評估 DeepFilterNet（低訊噪比表現較好，延遲與 CPU 較高）
- [x] VAD：**不能只看 RNNoise 的人聲機率**（風扇類噪音會被判成 0.9），要搭配降噪後的能量與持續時間（hangover）一起判斷
- [x] AGC、麥克風測試介面（音量條、門檻與環境噪音標記、聽自己的聲音）、VAD 門檻可調
- [x] 裝置熱插拔（`AudioRunner`）、斷線自動重連並恢復訂閱（M3 流程）
- [x] 網路統計（延遲、遺失率、直連/relay、房主負載）＋「讓出房主」
- [x] 評估：libopus 升級到 1.6.1（已做）；IAudioClient3 不做；DeepFilterNet3 做成可選的強力降噪（見 NEXT.md）

### M6：身分、權限、房間管理（1–2 週）—— 使用者決定不做（只保留已完成的房主權限）
- [ ] 身分組（Owner / Admin / Commander / Member / Guest）與頻道權限繼承
- [ ] `can_host`、踢人、封鎖（綁 NodeId）、重新產生邀請、指定房主
- [ ] 房間沉睡與喚醒

### M7：網頁客戶端（2–3 週）
- [ ] WASM 建置、AudioWorklet 混音、WebCodecs/WASM Opus
- [ ] 手機版介面（大顆 PTT 鈕）、PWA、邀請連結
- [ ] 實機測試：iOS Safari、Android Chrome

### M8：打磨與延伸（持續）
- [ ] 安裝程式 / 自動更新
- [ ] `headless-host` 正式部署（Docker 或 Windows 服務，依 M0 結果）
- ~~置頂透明「誰在說話」小視窗~~（使用者決定不做）
- [ ] 管理操作簽章、端對端加密
- [ ] 手機原生 App（評估）

### UI 草圖（M4）
```
┌ MultiVoice ─────────────────────────────── 🎤 全域靜音 [F8]  🔇 拒聽 ┐
│ 房間：公會語音   房主：會長（直連，上傳 3.2/50 Mbps）  [邀請] [讓出房主] │
│ 我的延遲 28ms  遺失 0%  直連                        候補：小明 → 阿華   │
├─────────────────────────────────────────────────────────────────────┤
│ 頻道              收聽  說話模式        PTT 鍵     音量    Pan   優先 │
│ ▾ 指揮部          [✔]  [PTT ▼]        Mouse4    ███▌    C     [★]  │
│     ● 會長 👑 (說話中)                                               │
│ ▾ 第一小隊        [✔]  [常開 ▼]       —         ███     L     [ ]  │
│     ● 我  ○ 小明  ○ 阿華 📱                                          │
│ ▸ 第二小隊        [✔]  [關 ▼]         —         ██      R     [ ]  │
│ ▸ 後勤            [ ]  [PTT ▼]        Mouse5    ██      C     [ ]  │
├─────────────────────────────────────────────────────────────────────┤
│ 全域廣播鍵：Ctrl+Mouse4（對所有說話模式非「關」的頻道說話）            │
│ 目前送出：▶ 第一小隊                                   麥克風 ▁▃▅▂▁ │
└─────────────────────────────────────────────────────────────────────┘
   👑 = 目前房主    📱 = 網頁成員
```
房主轉移時，頂端顯示「正在轉移房主給 小明…」，完成後顯示「小明 已成為房主」，並播放提示音。

---

## 11. 風險與未決問題

| 項目 | 說明 / 對策 |
|---|---|
| 房主上傳頻寬不足 | 候補排序看頻寬；UI 顯示房主負載；超載時自動建議轉移；單一收聽者串流上限 |
| 房主網路差，全房間受影響 | 房主連續高遺失時，自動計畫性轉移給更好的候補（可關閉） |
| 誤判房主失聯導致雙房主 | 多數確認 + 任期號 + 合併規則（§5.3–5.4），並以 `sim` 混沌測試驗證 |
| 公共 relay 限流 | 主力改用這台電腦自架的 relay，公共 relay 只當備援 |
| 這台電腦成為單點 | relay、網頁託管、常駐房主都在同一台；它關機時房間仍會轉移給玩家，網頁版改走公共 relay，網頁本身可在 GitHub Pages 放一份備援 |
| 家中對外暴露服務 | 只開必要的埠；Caddy、iroh-relay 都跑在容器內；relay 只轉送加密流量；定期更新映像 |
| 打洞失敗率 | 行動網路 CGNAT、對稱 NAT 會走 relay，延遲較高；候補排序避開這類成員 |
| 多頻道同時說話太吵 | 優先頻道 ducking、單人同時串流上限、每頻道 pan 分左右耳 |
| 誤開麥到錯頻道 | 每頻道不同提示音、UI 底部「目前送出」列、預設 PTT、常開需手動開啟 |
| 惡意房主 | 第一版只讓受信任身分組 `can_host`；之後加上管理操作簽章與 E2EE |
| iOS 網頁背景限制 | 先講明網頁版的定位；長期做原生 App |
| libopus 在 GNU 工具鏈下編譯 | ✅ 2026-11-15 驗證通過（debug 與 release/LTO 都可以），需要 `CMAKE_POLICY_VERSION_MINIMUM=3.5`（見 §12） |
| 內附 libopus 版本舊 | `audiopus_sys` 0.2.2 內附 **libopus 1.3.1**。1.5 版的 Deep PLC / DRED 在高掉包時音質好很多。M5 評估改用系統 libopus 1.5（pkg-config）或自行 vendoring |
| 反作弊誤判 | 不注入、不讀遊戲記憶體，只用標準 LL hook |
| 頻道數上限 | `via_mask` 為 u64，單連線最多訂閱 64 個頻道 |
| 名稱 | `MultiVoice` 為暫定代號 |

---

## 12. 開發環境備註（本機）

- 工具鏈：`stable-x86_64-pc-windows-gnu`（cargo 1.99.1）；網頁版另外需要 `rustup target add wasm32-unknown-unknown` 與 `wasm-bindgen-cli`
- C 工具：WinLibs 提供 gcc 16.2、cmake 4.4、ninja（編譯 libopus 會用到）
  - cmake 4.x 不接受 `cmake_minimum_required < 3.5`，而 `audiopus_sys` 內附的 libopus 正是如此，所以 `.cargo/config.toml` 的 `[env]` 設定了 `CMAKE_POLICY_VERSION_MINIMUM = "3.5"`。在其他電腦建置時也需要這個環境變數
  - iroh 宣告 `crate-type = ["lib", "cdylib"]`，Cargo 會連帶把它連結成 DLL；GNU ld 在 windows-gnu 上自動匯出所有符號，debug 版超過 PE 的 65535 上限（`export ordinal too large`）。`.cargo/config.toml` 加了 `[target.x86_64-pc-windows-gnu] rustflags = ["-C", "link-arg=-Wl,--exclude-all-symbols"]`
- 網路：**固定公開 IP**；網域 `xiaoyu0708.ip-ddns.com`（DNS 在 Cloudflare），新增 `voice.` / `relay.` 兩筆灰雲 A 紀錄，由 Caddy 自動申請 HTTPS 憑證（§7.3）
- Docker Desktop 29.x（WSL2）：跑 `caddy`、`iroh-relay`、`headless-host`（可選，§7.3）；放在 `deploy/docker-compose.yml`
- **中文路徑問題**：專案位於 `遊戲\` 底下，rustup 內建的 mingw `ld` 無法處理非 ASCII 路徑。比照 `副本退出快捷腳本`，建立 `.cargo/config.toml`：
  ```toml
  # GNU 工具鏈的 ld 無法處理中文路徑，所以把編譯輸出放到純英文路徑
  [build]
  target-dir = "%USERPROFILE%/.cargo/target-multivoice"
  ```
  此檔含本機路徑，不納入版本控制。
- release 打包時沿用 QuickExit `build.ps1` 的 `--remap-path-prefix`，避免 exe 內嵌使用者名稱與本機路徑。
- 測試房主轉移需要多台機器或多個網路環境：除了 `sim`，可以用 Docker 起數個 `headless-host` / CLI 成員，並用 `tc netem` 模擬延遲與斷線。
