//! MultiVoice 工作階段（CLI 與桌面版共用）。
//!
//! 分兩段：
//! 1. [`host_room`] / [`join_room`]（async，可在任何 tokio 執行緒）：建立端點、建房或加入，回傳 [`Connected`]
//! 2. [`Session::start`]（同步）：啟動音訊。cpal 的串流在 Windows 上不是 `Send`，
//!    所以 [`Session`] 要留在建立它的執行緒（GUI 執行緒或 CLI 主執行緒）。
//!
//! UI 透過 [`Session::snapshot`] 取得狀態快照、用 `set_*` 下指令。

pub mod identity;
pub mod pipeline;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use client_core::{Client, ClientEvent};
use host_core::{Host, HostConfig};
use iroh::Endpoint;
use protocol::SessionIdx;
use protocol::ticket::{RoomId, RoomSecret, Ticket};
use tokio::sync::mpsc::UnboundedReceiver;
use transport::RelaySetup;

pub use pipeline::{AudioOpts, JitterSummary, Shared};
pub use transport::RelaySetup as Relays;

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

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub me: MemberView,
    pub members: Vec<MemberView>,
    pub is_host: bool,
    pub ticket: String,
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
    pub input_name: String,
    pub output_name: String,
}

impl Session {
    /// 啟動音訊，並在 `rt` 上開一個 task 處理控制事件
    pub fn start(c: Connected, audio: &AudioOpts, rt: &tokio::runtime::Handle) -> Result<Session> {
        let shared = Arc::new(Shared::new());
        let handle = match pipeline::start(c.client.clone(), shared.clone(), audio, rt) {
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
        let room = Arc::new(Mutex::new(RoomState::default()));
        {
            let mut r = room.lock().unwrap();
            r.names = c.client.members().into_iter().map(|m| (m.session_idx, m.display_name)).collect();
            r.push_log(if c.host.is_some() { "已建立房間".into() } else { "已加入房間".into() });
        }
        let mut events = c.events;
        let (room2, shared2) = (room.clone(), shared.clone());
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
                    ClientEvent::Rtt(ms) => r.ping = Some(ms),
                    ClientEvent::Disconnected(why) => {
                        r.push_log(format!("連線中斷：{why}"));
                        r.disconnected = Some(why);
                    }
                }
            }
        });
        Ok(Session {
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
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        let speaking = self.shared.speaking(Duration::from_millis(300));
        let members = self
            .client
            .members()
            .into_iter()
            .map(|m| MemberView { speaking: speaking.contains(&m.session_idx), idx: m.session_idx, name: m.display_name })
            .collect();
        let (mic_peak, mic_vad) = self.shared.mic_level();
        let tx_active = self.shared.tx_active.load(Ordering::Relaxed);
        let r = self.room.lock().unwrap();
        Snapshot {
            me: MemberView { idx: self.client.session_idx(), name: self.name.clone(), speaking: tx_active },
            members,
            is_host: self.host.is_some(),
            ticket: self.ticket.to_string(),
            muted: self.shared.muted.load(Ordering::Relaxed),
            deafened: self.shared.deafened.load(Ordering::Relaxed),
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

    pub fn set_muted(&self, v: bool) {
        self.shared.muted.store(v, Ordering::Relaxed);
    }

    pub fn set_deafened(&self, v: bool) {
        self.shared.deafened.store(v, Ordering::Relaxed);
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
