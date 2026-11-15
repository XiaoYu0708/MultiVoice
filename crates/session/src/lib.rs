//! MultiVoice 工作階段（CLI 與桌面版共用）。
//!
//! 分兩段：
//! 1. [`host_room`] / [`join_room`]（async，可在任何 tokio 執行緒）：建立端點、建房或加入，回傳 [`Connected`]
//! 2. [`Session::start`]（同步）：啟動音訊。cpal 的串流在 Windows 上不是 `Send`，
//!    所以 [`Session`] 要留在建立它的執行緒（GUI 執行緒或 CLI 主執行緒）。
//!
//! UI 透過 [`Session::snapshot`] 取得狀態快照、用 `set_*`／頻道方法下指令。

pub mod identity;
pub mod mictest;
pub mod pipeline;
pub mod takeover;
pub mod talk;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use client_core::{Client, ClientEvent};
use host_core::{Host, HostConfig};
use iroh::Endpoint;
use protocol::ticket::{RoomId, RoomSecret, Ticket};
use protocol::{ChannelId, DEFAULT_CHANNEL, SessionIdx};
use tokio::sync::mpsc::UnboundedReceiver;
use transport::RelaySetup;

pub use hotkeys::{self, Hotkey};
pub use mixer_core::route::ChannelMix;
pub use audio::frontend::FrontendCfg;
pub use pipeline::{AudioOpts, ChannelCfg, JitterSummary, Shared, TalkMode};
pub use transport::RelaySetup as Relays;
pub use transport::WEB_CLIENT_URL;

/// 已連上房間，但音訊還沒啟動
pub struct Connected {
    client: Client,
    events: UnboundedReceiver<ClientEvent>,
    endpoints: Vec<Endpoint>,
    host: Option<Host>,
    /// 備用房主端點（M3；加入別人的房間時才有）
    standby: Option<Endpoint>,
    /// 備用端點要等連上 relay 才宣告
    wait_online: bool,
    name: String,
}

/// 建立房間：本機同時跑房主邏輯與自己的客戶端
pub async fn host_room(profile: &str, name: &str, relays: RelaySetup, can_host: bool) -> Result<Connected> {
    let key = identity::load_or_create(profile)?;
    // 只有房主自己（這個 profile 的身分）能改名／刪除頻道
    let owner = transport::node_id(&key.public());
    // 房主端點用一次性的金鑰；自己的成員身分用 profile 的永久金鑰
    let host_ep = transport::bind(transport::generate_secret(), relays.clone(), true).await?;
    let ticket = Ticket {
        room_id: RoomId(transport::random_bytes()),
        secret: RoomSecret(transport::random_bytes()),
        bootstrap: vec![transport::node_id(&host_ep.id())],
    };
    let host = Host::new(HostConfig { room_id: ticket.room_id, secret: ticket.secret, term: 1, max_members: 100, owner: Some(owner), seed: None });
    tokio::spawn({
        let (host, ep) = (host.clone(), host_ep.clone());
        async move { host.run(ep).await }
    });
    if !matches!(relays, RelaySetup::Disabled) {
        // 等連上 relay，別人才找得到這個房間；逾時也繼續（可能只是 relay 暫時連不上）
        let _ = tokio::time::timeout(Duration::from_secs(10), host_ep.online()).await;
    }
    let ep = transport::bind(key, relays.clone(), false).await?;
    let (client, events) = Client::join_addr(&ep, transport::loopback_addr(&host_ep), &ticket, name).await?;
    // 房主也建備用端點：讓出房主後，之後還能再當候補
    let wait_online = !matches!(relays, RelaySetup::Disabled);
    let standby = if can_host { transport::bind(transport::generate_secret(), relays, true).await.ok() } else { None };
    let mut endpoints = vec![ep, host_ep];
    endpoints.extend(standby.clone());
    Ok(Connected { client, events, endpoints, host: Some(host), standby, wait_online, name: name.into() })
}

/// `can_host = false`：不當房主候補（不建備用端點；房主離開時不會由我接手）
pub async fn join_room(ticket: &str, profile: &str, name: &str, relays: RelaySetup, can_host: bool) -> Result<Connected> {
    let ticket: Ticket = ticket.parse().context("邀請票無效")?;
    let key = identity::load_or_create(profile)?;
    let wait_online = !matches!(relays, RelaySetup::Disabled);
    let ep = transport::bind(key, relays.clone(), false).await?;
    match Client::join(&ep, &ticket, name).await {
        Ok((client, events)) => {
            // 備用房主端點（房主離開或失聯時可能由我接手）；建不起來也不影響加入
            let standby =
                if can_host { transport::bind(transport::generate_secret(), relays, true).await.ok() } else { None };
            let mut endpoints = vec![ep];
            endpoints.extend(standby.clone());
            Ok(Connected { client, events, endpoints, host: None, standby, wait_online, name: name.into() })
        }
        Err(e) => {
            ep.close().await;
            Err(e)
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemberView {
    pub idx: SessionIdx,
    pub name: String,
    pub speaking: bool,
}

/// 頻道裡的一位成員
#[derive(Clone, Debug)]
pub struct ChannelMemberView {
    pub idx: SessionIdx,
    pub name: String,
    pub is_me: bool,
    pub listen: bool,
    pub talk: bool,
    /// 正在這個頻道說話
    pub speaking: bool,
}

#[derive(Clone, Debug)]
pub struct ChannelView {
    pub id: ChannelId,
    pub name: String,
    /// 我在這個頻道的設定；None = 沒加入
    pub mine: Option<ChannelCfg>,
    pub members: Vec<ChannelMemberView>,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub me: MemberView,
    pub members: Vec<MemberView>,
    pub channels: Vec<ChannelView>,
    /// 目前正在送出聲音的頻道名稱
    pub talking_to: Vec<String>,
    pub is_host: bool,
    /// 正在找新房主（房主離開或失聯）
    pub reconnecting: bool,
    pub ticket: String,
    /// 實際上沒有送出聲音（自己靜音，或拒聽中）
    pub muted: bool,
    pub deafened: bool,
    pub tx_active: bool,
    pub mic_peak: f32,
    pub mic_vad: f32,
    /// (是否經 relay, QUIC RTT ms)
    pub path: Option<(bool, f64)>,
    /// 控制串流 ping（ms）
    pub ping: Option<f64>,
    pub jitter: JitterSummary,
    /// 最近約 5 秒收到的語音裡，需要補償（FEC／PLC）的比例；沒人說話時 None
    pub loss: Option<f32>,
    /// 我是房主時的負載
    pub host_load: Option<HostLoad>,
    /// 有候補可以接手（「讓出房主」能不能按）
    pub can_hand_over: bool,
    pub underruns: u64,
    /// 最近的事件（新的在後）
    pub log: Vec<String>,
    /// 連線已中斷時的原因
    pub disconnected: Option<String>,
}

#[derive(Default)]
struct RoomState {
    /// 成員名字（含已離開的），事件紀錄顯示用
    names: HashMap<SessionIdx, String>,
    channel_names: HashMap<ChannelId, String>,
    ping: Option<f64>,
    log: VecDeque<String>,
    disconnected: Option<String>,
    /// 換房主期間大家陸續重連，這段時間不記「某某加入／離開了」
    quiet_until: Option<std::time::Instant>,
}

impl RoomState {
    fn quiet(&self) -> bool {
        self.quiet_until.is_some_and(|t| std::time::Instant::now() < t)
    }

    fn hush(&mut self) {
        self.quiet_until = Some(std::time::Instant::now() + Duration::from_secs(8));
    }

    fn push_log(&mut self, s: String) {
        self.log.push_back(s);
        while self.log.len() > 50 {
            self.log.pop_front();
        }
    }
}

/// 房主負載
#[derive(Clone, Copy, Debug)]
pub struct HostLoad {
    /// 轉發給幾個人（不含自己）
    pub listeners: usize,
    /// 轉發的上傳頻寬（kbps）
    pub up_kbps: f64,
}

/// 每秒取樣一次累計值，算最近一段時間的速率
#[derive(Default)]
struct Meter {
    /// (時間, 正常解碼, 補償, 房主送出位元組)
    samples: VecDeque<(std::time::Instant, u64, u64, u64)>,
}

impl Meter {
    const WINDOW: Duration = Duration::from_secs(5);

    fn sample(&mut self, decoded: u64, concealed: u64, bytes_out: u64) {
        let now = std::time::Instant::now();
        if self.samples.back().is_some_and(|s| now.duration_since(s.0) < Duration::from_secs(1)) {
            return;
        }
        self.samples.push_back((now, decoded, concealed, bytes_out));
        while self.samples.len() > 2 && now.duration_since(self.samples[0].0) > Self::WINDOW {
            self.samples.pop_front();
        }
    }

    /// (遺失率, 上傳 kbps)
    fn rates(&self) -> (Option<f32>, f64) {
        let (Some(a), Some(b)) = (self.samples.front(), self.samples.back()) else { return (None, 0.0) };
        // 有人離開時 jitter buffer 會被移除，累計值可能變小：用 saturating
        let good = b.1.saturating_sub(a.1);
        let bad = b.2.saturating_sub(a.2);
        let total = good + bad;
        // 少於 1 秒的語音（50 幀）不算，數字會亂跳
        let loss = (total >= 50).then(|| bad as f32 / total as f32);
        let secs = b.0.duration_since(a.0).as_secs_f64();
        let kbps = if secs > 0.0 { b.3.saturating_sub(a.3) as f64 * 8.0 / 1000.0 / secs } else { 0.0 };
        (loss, kbps)
    }
}

/// 進行中的工作階段（音訊已啟動）。不是 `Send`，留在建立它的執行緒。
pub struct Session {
    client: Client,
    shared: Arc<Shared>,
    /// 音訊裝置（專屬執行緒，處理熱插拔）
    audio: Option<pipeline::AudioRunner>,
    endpoints: Vec<Endpoint>,
    /// 換房主的流程，也記著「我現在是不是房主」
    takeover: Arc<takeover::Takeover>,
    /// 遺失率、房主上傳量
    meter: Mutex<Meter>,
    /// UI 執行緒不在 tokio 裡：需要開 task 時先 enter
    rt: tokio::runtime::Handle,
    name: String,
    room: Arc<Mutex<RoomState>>,
    /// 目前套用中的音訊設定
    opts: AudioOpts,
}

impl Session {
    /// 啟動音訊、自動加入大廳，並在 `rt` 上開 task 處理控制事件
    pub fn start(c: Connected, audio: &AudioOpts, rt: &tokio::runtime::Handle) -> Result<Session> {
        // 全域按鍵（PTT／廣播／靜音）；失敗不影響其他功能
        let hotkey_err = hotkeys::start().err();
        let shared = Arc::new(Shared::new());
        // 在音訊啟動前設定，擷取執行緒一開始就是靜音狀態（不會響「關麥」提示音）
        if audio.start_muted {
            shared.set_muted(true);
        }
        let room = Arc::new(Mutex::new(RoomState::default()));
        let log: takeover::Log = {
            let room = room.clone();
            Arc::new(move |s| room.lock().unwrap().push_log(s))
        };
        let handle = match pipeline::AudioRunner::start(c.client.clone(), shared.clone(), audio, log.clone()) {
            Ok(h) => h,
            Err(e) => {
                // 音訊裝置打不開：離開房間、關閉端點，不留下半套連線
                let (client, endpoints) = (c.client, c.endpoints);
                rt.spawn(async move {
                    client.leave().await;
                    for ep in endpoints {
                        ep.close().await;
                    }
                });
                return Err(e);
            }
        };
        pipeline::spawn_receive(c.client.clone(), shared.clone(), rt);
        {
            let mut r = room.lock().unwrap();
            r.names = c.client.members().into_iter().map(|m| (m.session_idx, m.display_name)).collect();
            r.channel_names = c.client.channels().into_iter().map(|ch| (ch.id, ch.name)).collect();
            r.push_log(if c.host.is_some() { "已建立房間".into() } else { "已加入房間".into() });
            if let Some(e) = hotkey_err {
                r.push_log(format!("全域按鍵無法使用：{e}"));
            }
        }
        let takeover = takeover::Takeover::new(c.client.clone(), c.standby, c.host, log);
        {
            let _guard = rt.enter();
            takeover.start_standby(c.wait_online);
            takeover.start_merge_loop();
        }
        let mut events = c.events;
        let (room2, shared2, client2, takeover2) = (room.clone(), shared.clone(), c.client.clone(), takeover.clone());
        rt.spawn(async move {
            while let Some(ev) = events.recv().await {
                let mut r = room2.lock().unwrap();
                match ev {
                    ClientEvent::MemberJoined(m) => {
                        if !r.quiet() {
                            r.push_log(format!("{} 加入了", m.display_name));
                        }
                        r.names.insert(m.session_idx, m.display_name);
                    }
                    ClientEvent::MemberLeft(i) => {
                        let name = r.names.get(&i).cloned().unwrap_or_else(|| format!("#{i}"));
                        if !r.quiet() {
                            r.push_log(format!("{name} 離開了"));
                        }
                        shared2.forget(i);
                    }
                    ClientEvent::MigrateTo { session_idx, host, term } => {
                        r.hush();
                        tokio::spawn(takeover2.clone().on_migrate(session_idx, host, term));
                    }
                    ClientEvent::MoveTo { host } => {
                        r.hush();
                        tokio::spawn(takeover2.clone().on_move(host));
                    }
                    ClientEvent::HostLost(_) => {
                        r.hush();
                        tokio::spawn(takeover2.clone().on_host_lost());
                    }
                    ClientEvent::Reconnected { .. } => {
                        r.hush();
                        r.channel_names = client2.channels().into_iter().map(|ch| (ch.id, ch.name)).collect();
                        for m in client2.members() {
                            r.names.insert(m.session_idx, m.display_name);
                        }
                        // 換房主時頻道可能有變：重算槽位與說話遮罩
                        shared2.rebuild(&client2.my_slots());
                    }
                    ClientEvent::ChannelAdded(ch) => {
                        r.push_log(format!("新增頻道「{}」", ch.name));
                        r.channel_names.insert(ch.id, ch.name);
                    }
                    ClientEvent::ChannelRenamed(id, name) => {
                        let old = r.channel_names.insert(id, name.clone()).unwrap_or_default();
                        r.push_log(format!("頻道「{old}」改名為「{name}」"));
                    }
                    ClientEvent::ChannelRemoved(id) => {
                        let name = r.channel_names.remove(&id).unwrap_or_default();
                        r.push_log(format!("頻道「{name}」已刪除"));
                        shared2.set_cfg(id, None);
                        shared2.rebuild(&client2.my_slots());
                    }
                    ClientEvent::HostError(_, msg) => r.push_log(format!("房主拒絕：{msg}")),
                    ClientEvent::SubscriptionChanged(_) => {}
                    ClientEvent::Rtt(ms) => r.ping = Some(ms),
                    ClientEvent::Disconnected(why) => {
                        r.push_log(format!("連線中斷：{why}"));
                        r.disconnected = Some(why);
                    }
                }
            }
        });
        let s = Session {
            client: c.client,
            shared,
            audio: Some(handle),
            endpoints: c.endpoints,
            takeover,
            meter: Mutex::new(Meter::default()),
            rt: rt.clone(),
            name: c.name,
            room,
            opts: audio.clone(),
        };
        // 自動加入大廳
        s.set_channel(DEFAULT_CHANNEL, Some(s.default_channel_cfg()))?;
        Ok(s)
    }

    /// 加入頻道時的預設設定：收聽 + 依「開麥預設」偵測說話或常開
    pub fn default_channel_cfg(&self) -> ChannelCfg {
        ChannelCfg {
            listen: true,
            talk: if self.opts.open_mic { TalkMode::Open } else { TalkMode::Voice },
            mix: ChannelMix::default(),
            ptt_key: None,
        }
    }

    /// 加入、更新（Some）或離開（None）頻道。不收聽也不說話等同離開。
    pub fn set_channel(&self, channel: ChannelId, cfg: Option<ChannelCfg>) -> Result<()> {
        let cfg = cfg.filter(|c| c.listen || c.talk != TalkMode::Off);
        match cfg {
            Some(c) => {
                self.client.subscribe(channel, c.listen, c.talk != TalkMode::Off)?;
                self.shared.set_cfg(channel, Some(c));
            }
            None => {
                self.client.unsubscribe(channel);
                self.shared.set_cfg(channel, None);
            }
        }
        self.shared.rebuild(&self.client.my_slots());
        Ok(())
    }

    pub fn create_channel(&self, name: &str) {
        self.client.create_channel(name);
    }

    pub fn rename_channel(&self, channel: ChannelId, name: &str) {
        self.client.rename_channel(channel, name);
    }

    pub fn delete_channel(&self, channel: ChannelId) {
        self.client.delete_channel(channel);
    }

    pub fn snapshot(&self) -> Snapshot {
        let speaking = self.shared.speaking(Duration::from_millis(300));
        let talking_to = self.shared.talking_to();
        let my_idx = self.client.session_idx();
        let names: HashMap<SessionIdx, String> =
            self.client.members().into_iter().map(|m| (m.session_idx, m.display_name)).collect();
        let members = names
            .iter()
            .map(|(&idx, name)| MemberView { idx, name: name.clone(), speaking: speaking.iter().any(|(i, _)| *i == idx) })
            .collect::<Vec<_>>();
        let mut members = members;
        members.sort_by_key(|m| m.idx);

        let cfgs = self.shared.channel_cfgs();
        let subs = self.client.subscriptions();
        let channels = self
            .client
            .channels()
            .into_iter()
            .map(|ch| {
                let mut ms: Vec<ChannelMemberView> = subs
                    .iter()
                    .filter(|s| s.channel == ch.id)
                    .map(|s| {
                        let is_me = s.session_idx == my_idx;
                        let speaking = if is_me {
                            talking_to.contains(&ch.id)
                        } else {
                            speaking.iter().any(|(i, via)| *i == s.session_idx && via.contains(&ch.id))
                        };
                        ChannelMemberView {
                            idx: s.session_idx,
                            name: if is_me { self.name.clone() } else { names.get(&s.session_idx).cloned().unwrap_or_default() },
                            is_me,
                            listen: s.listen,
                            talk: s.talk,
                            speaking,
                        }
                    })
                    .collect();
                ms.sort_by_key(|m| (!m.is_me, m.idx));
                ChannelView { mine: cfgs.get(&ch.id).copied(), id: ch.id, name: ch.name, members: ms }
            })
            .collect();
        let channel_names: HashMap<ChannelId, String> =
            self.client.channels().into_iter().map(|c| (c.id, c.name)).collect();

        let jitter = self.shared.jitter_summary();
        let host = self.takeover.host();
        let is_host = host.is_some();
        let (loss, up_kbps) = {
            let mut m = self.meter.lock().unwrap();
            let bytes_out = host.as_ref().map(|h| h.stats().bytes_out).unwrap_or(0);
            m.sample(jitter.decoded, jitter.fec + jitter.plc, bytes_out);
            m.rates()
        };
        let host_load = host.map(|h| HostLoad { listeners: h.member_count().saturating_sub(1), up_kbps });
        let (mic_peak, mic_vad) = self.shared.mic_level();
        let tx_active = self.shared.tx_active.load(Ordering::Relaxed);
        let r = self.room.lock().unwrap();
        Snapshot {
            me: MemberView { idx: my_idx, name: self.name.clone(), speaking: tx_active },
            members,
            channels,
            talking_to: talking_to.iter().filter_map(|c| channel_names.get(c).cloned()).collect(),
            is_host: self.takeover.host().is_some(),
            reconnecting: self.takeover.recovering(),
            ticket: self.ticket().to_string(),
            muted: self.shared.is_muted(),
            deafened: self.shared.is_deafened(),
            tx_active,
            mic_peak,
            mic_vad,
            path: self.client.path(),
            ping: r.ping,
            loss,
            host_load,
            can_hand_over: is_host && !self.client.successors().is_empty(),
            jitter,
            underruns: self.shared.underruns(),
            log: r.log.iter().cloned().collect(),
            disconnected: r.disconnected.clone(),
        }
    }

    /// 讓出房主：交給第一候補，自己留在房間當一般成員。沒有候補時回傳 false
    pub fn hand_over(&self) -> bool {
        let _guard = self.rt.enter();
        self.takeover.hand_over()
    }

    /// 目前的邀請票：現任房主在最前面，後面是候補的備用端點（房主換人後舊票也比較可能還能用）
    pub fn ticket(&self) -> Ticket {
        let t = self.client.ticket();
        let mut bootstrap = vec![self.client.host_id()];
        for s in self.client.successors() {
            if !bootstrap.contains(&s.host.id) {
                bootstrap.push(s.host.id);
            }
        }
        for n in &t.bootstrap {
            if !bootstrap.contains(n) {
                bootstrap.push(*n);
            }
        }
        bootstrap.truncate(6);
        Ticket { room_id: t.room_id, secret: t.secret, bootstrap }
    }

    /// 跨執行緒共用的即時狀態（系統匣執行緒讀取說話／靜音狀態、切換靜音用）
    pub fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }

    /// 見 [`Shared::set_muted`]：拒聽中取消靜音會一起取消拒聽
    pub fn set_muted(&self, v: bool) {
        self.shared.set_muted(v);
    }

    /// 拒聽同時關麥；取消後靜音回到原本的狀態
    pub fn set_deafened(&self, v: bool) {
        self.shared.set_deafened(v);
    }

    /// 目前使用的麥克風名稱
    pub fn input_name(&self) -> String {
        self.audio.as_ref().map(|a| a.names().0).unwrap_or_default()
    }

    /// 目前使用的耳機／喇叭名稱
    pub fn output_name(&self) -> String {
        self.audio.as_ref().map(|a| a.names().1).unwrap_or_default()
    }

    /// 測試用：假裝音訊裝置出錯（驗證熱插拔會自動重開）
    pub fn simulate_audio_failure(&self) {
        if let Some(a) = &self.audio {
            a.simulate_failure();
        }
    }

    /// 進房後調整音訊設定。降噪、音量立即生效；換了裝置則重啟音訊（連線不中斷）。
    /// 新裝置開不起來時退回原本的裝置，並回傳錯誤。（開麥預設只影響之後加入的頻道）
    pub fn apply_audio(&mut self, new: &AudioOpts) -> Result<()> {
        self.shared.apply(new);
        let devices_changed = new.input != self.opts.input || new.output != self.opts.output;
        if !devices_changed {
            self.opts = new.clone();
            return Ok(());
        }
        let Some(a) = &self.audio else { return Ok(()) };
        match a.apply(new) {
            Ok(()) => {
                self.opts = new.clone();
                self.room.lock().unwrap().push_log("已切換音訊裝置".into());
                Ok(())
            }
            Err(e) => {
                // 保留新的即時設定，只退回裝置
                self.opts = AudioOpts { input: self.opts.input.clone(), output: self.opts.output.clone(), ..new.clone() };
                Err(e)
            }
        }
    }

    /// 停止音訊（在目前執行緒），回傳一個可以丟到 tokio 上執行的離開流程
    pub fn leave(mut self) -> impl std::future::Future<Output = ()> + Send + 'static {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.audio.take();
        let client = self.client.clone();
        let endpoints = std::mem::take(&mut self.endpoints);
        let host = self.takeover.host();
        // 離開後不再參與換房主（不會在背景又接手當房主）
        self.takeover.shutdown();
        async move {
            client.leave().await;
            // 我是房主：先交給第一候補，等大家改連過去再關（PLAN §5.2）。
            // 成員改連後還會跟我保留連線一下（先建後拆），所以等到沒人或最多 1.5 秒
            if let Some(h) = host
                && h.migrate().is_some()
            {
                let start = std::time::Instant::now();
                tokio::time::sleep(Duration::from_millis(300)).await;
                while h.member_count() > 0 && start.elapsed() < client_core::DRAIN {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            for ep in endpoints {
                ep.close().await;
            }
        }
    }
}
