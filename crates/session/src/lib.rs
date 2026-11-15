//! MultiVoice 工作階段（CLI 與桌面版共用）。
//!
//! 分兩段：
//! 1. [`host_room`] / [`join_room`]（async，可在任何 tokio 執行緒）：建立端點、建房或加入，回傳 [`Connected`]
//! 2. [`Session::start`]（同步）：啟動音訊。cpal 的串流在 Windows 上不是 `Send`，
//!    所以 [`Session`] 要留在建立它的執行緒（GUI 執行緒或 CLI 主執行緒）。
//!
//! UI 透過 [`Session::snapshot`] 取得狀態快照、用 `set_*`／頻道方法下指令。

pub mod identity;
pub mod pipeline;
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
pub use pipeline::{AudioOpts, ChannelCfg, JitterSummary, Shared, TalkMode};
pub use transport::RelaySetup as Relays;

/// 「誰在說話」的一行（置頂小視窗用）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeakLine {
    pub name: String,
    /// 經由哪些頻道（我自己：正在送出的頻道）
    pub channels: Vec<String>,
    pub me: bool,
}

/// 可以在其他執行緒讀取「誰在說話」（`Session` 本身不是 `Send`）
#[derive(Clone)]
pub struct SpeakFeed {
    client: Client,
    shared: Arc<Shared>,
    my_name: String,
}

impl SpeakFeed {
    /// 目前正在說話的人：我自己（送出中）排第一，其他人依序
    pub fn lines(&self) -> Vec<SpeakLine> {
        let channel_names: HashMap<ChannelId, String> =
            self.client.channels().into_iter().map(|c| (c.id, c.name)).collect();
        let names = |ids: &[ChannelId]| ids.iter().filter_map(|c| channel_names.get(c).cloned()).collect::<Vec<_>>();
        let mut out = Vec::new();
        let mine = self.shared.talking_to();
        if !mine.is_empty() {
            out.push(SpeakLine { name: self.my_name.clone(), channels: names(&mine), me: true });
        }
        let speaking = self.shared.speaking(Duration::from_millis(300));
        if !speaking.is_empty() {
            let members: HashMap<SessionIdx, String> =
                self.client.members().into_iter().map(|m| (m.session_idx, m.display_name)).collect();
            for (idx, via) in speaking {
                let name = members.get(&idx).cloned().unwrap_or_else(|| format!("#{idx}"));
                out.push(SpeakLine { name, channels: names(&via), me: false });
            }
        }
        out
    }
}

/// 已連上房間，但音訊還沒啟動
pub struct Connected {
    client: Client,
    events: UnboundedReceiver<ClientEvent>,
    endpoints: Vec<Endpoint>,
    host: Option<Host>,
    ticket: Ticket,
    name: String,
}

/// 建立房間：本機同時跑房主邏輯與自己的客戶端
pub async fn host_room(profile: &str, name: &str, relays: RelaySetup) -> Result<Connected> {
    let key = identity::load_or_create(profile)?;
    // 房主端點用一次性的金鑰；自己的成員身分用 profile 的永久金鑰
    let host_ep = transport::bind(transport::generate_secret(), relays.clone(), true).await?;
    let ticket = Ticket {
        room_id: RoomId(transport::random_bytes()),
        secret: RoomSecret(transport::random_bytes()),
        bootstrap: vec![transport::node_id(&host_ep.id())],
    };
    let host = Host::new(HostConfig { room_id: ticket.room_id, secret: ticket.secret, term: 1, max_members: 100 });
    tokio::spawn({
        let (host, ep) = (host.clone(), host_ep.clone());
        async move { host.run(ep).await }
    });
    if !matches!(relays, RelaySetup::Disabled) {
        // 等連上 relay，別人才找得到這個房間；逾時也繼續（可能只是 relay 暫時連不上）
        let _ = tokio::time::timeout(Duration::from_secs(10), host_ep.online()).await;
    }
    let ep = transport::bind(key, relays, false).await?;
    let (client, events) = Client::join_addr(&ep, transport::loopback_addr(&host_ep), &ticket, name).await?;
    Ok(Connected { client, events, endpoints: vec![ep, host_ep], host: Some(host), ticket, name: name.into() })
}

pub async fn join_room(ticket: &str, profile: &str, name: &str, relays: RelaySetup) -> Result<Connected> {
    let ticket: Ticket = ticket.parse().context("邀請票無效")?;
    let key = identity::load_or_create(profile)?;
    let ep = transport::bind(key, relays, false).await?;
    match Client::join(&ep, &ticket, name).await {
        Ok((client, events)) => Ok(Connected { client, events, endpoints: vec![ep], host: None, ticket, name: name.into() }),
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
}

impl RoomState {
    fn push_log(&mut self, s: String) {
        self.log.push_back(s);
        while self.log.len() > 50 {
            self.log.pop_front();
        }
    }
}

/// 進行中的工作階段（音訊已啟動）。不是 `Send`，留在建立它的執行緒。
pub struct Session {
    client: Client,
    shared: Arc<Shared>,
    audio: Option<pipeline::AudioHandle>,
    endpoints: Vec<Endpoint>,
    host: Option<Host>,
    ticket: Ticket,
    name: String,
    room: Arc<Mutex<RoomState>>,
    /// 目前套用中的音訊設定
    opts: AudioOpts,
    pub input_name: String,
    pub output_name: String,
}

impl Session {
    /// 啟動音訊、自動加入大廳，並在 `rt` 上開 task 處理控制事件
    pub fn start(c: Connected, audio: &AudioOpts, rt: &tokio::runtime::Handle) -> Result<Session> {
        // 全域按鍵（PTT／廣播／靜音）；失敗不影響其他功能
        let hotkey_err = hotkeys::start().err();
        let shared = Arc::new(Shared::new());
        let handle = match pipeline::start(c.client.clone(), shared.clone(), audio) {
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
        let room = Arc::new(Mutex::new(RoomState::default()));
        {
            let mut r = room.lock().unwrap();
            r.names = c.client.members().into_iter().map(|m| (m.session_idx, m.display_name)).collect();
            r.channel_names = c.client.channels().into_iter().map(|ch| (ch.id, ch.name)).collect();
            r.push_log(if c.host.is_some() { "已建立房間".into() } else { "已加入房間".into() });
            if let Some(e) = hotkey_err {
                r.push_log(format!("全域按鍵無法使用：{e}"));
            }
        }
        let mut events = c.events;
        let (room2, shared2, client2) = (room.clone(), shared.clone(), c.client.clone());
        rt.spawn(async move {
            while let Some(ev) = events.recv().await {
                let mut r = room2.lock().unwrap();
                match ev {
                    ClientEvent::MemberJoined(m) => {
                        r.push_log(format!("{} 加入了", m.display_name));
                        r.names.insert(m.session_idx, m.display_name);
                    }
                    ClientEvent::MemberLeft(i) => {
                        let name = r.names.get(&i).cloned().unwrap_or_else(|| format!("#{i}"));
                        r.push_log(format!("{name} 離開了"));
                        shared2.forget(i);
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
            input_name: handle.input_name.clone(),
            output_name: handle.output_name.clone(),
            client: c.client,
            shared,
            audio: Some(handle),
            endpoints: c.endpoints,
            host: c.host,
            ticket: c.ticket,
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

        let (mic_peak, mic_vad) = self.shared.mic_level();
        let tx_active = self.shared.tx_active.load(Ordering::Relaxed);
        let r = self.room.lock().unwrap();
        Snapshot {
            me: MemberView { idx: my_idx, name: self.name.clone(), speaking: tx_active },
            members,
            channels,
            talking_to: talking_to.iter().filter_map(|c| channel_names.get(c).cloned()).collect(),
            is_host: self.host.is_some(),
            ticket: self.ticket.to_string(),
            muted: self.shared.is_muted(),
            deafened: self.shared.is_deafened(),
            tx_active,
            mic_peak,
            mic_vad,
            path: self.client.path(),
            ping: r.ping,
            jitter: self.shared.jitter_summary(),
            underruns: self.shared.underruns(),
            log: r.log.iter().cloned().collect(),
            disconnected: r.disconnected.clone(),
        }
    }

    /// 「誰在說話」（給其他執行緒用，例如置頂小視窗）
    pub fn speak_feed(&self) -> SpeakFeed {
        SpeakFeed { client: self.client.clone(), shared: self.shared.clone(), my_name: self.name.clone() }
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

    /// 進房後調整音訊設定。降噪、音量立即生效；換了裝置則重啟音訊（連線不中斷）。
    /// 新裝置開不起來時退回原本的裝置，並回傳錯誤。（開麥預設只影響之後加入的頻道）
    pub fn apply_audio(&mut self, new: &AudioOpts) -> Result<()> {
        self.shared.apply(new);
        let devices_changed = new.input != self.opts.input || new.output != self.opts.output;
        if !devices_changed {
            self.opts = new.clone();
            return Ok(());
        }
        // 先停掉舊的（等執行緒結束），避免兩組擷取執行緒同時送出
        self.audio.take();
        match pipeline::start(self.client.clone(), self.shared.clone(), new) {
            Ok(h) => {
                self.input_name = h.input_name.clone();
                self.output_name = h.output_name.clone();
                self.audio = Some(h);
                self.opts = new.clone();
                self.room.lock().unwrap().push_log("已切換音訊裝置".into());
                Ok(())
            }
            Err(e) => {
                // 保留新的即時設定，只退回裝置
                let old = AudioOpts { input: self.opts.input.clone(), output: self.opts.output.clone(), ..new.clone() };
                if let Ok(h) = pipeline::start(self.client.clone(), self.shared.clone(), &old) {
                    self.audio = Some(h);
                    self.opts = old;
                }
                Err(e.context("無法開啟新的音訊裝置，已改回原本的裝置"))
            }
        }
    }

    /// 停止音訊（在目前執行緒），回傳一個可以丟到 tokio 上執行的離開流程
    pub fn leave(mut self) -> impl std::future::Future<Output = ()> + Send + 'static {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.audio.take();
        let client = self.client.clone();
        let endpoints = std::mem::take(&mut self.endpoints);
        let host = self.host.take();
        async move {
            client.leave().await;
            drop(host);
            for ep in endpoints {
                ep.close().await;
            }
        }
    }
}
