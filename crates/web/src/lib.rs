//! MultiVoice 網頁客戶端：瀏覽器主執行緒這一半（PLAN.md §8）。
//!
//! - 連線：iroh（WASM）＋ [`client_core::Client`]，與桌面版共用同一份成員端邏輯。瀏覽器不能收發 UDP，一律經 relay
//! - 音訊不在這裡：AudioWorklet 裡的 `web-audio`（Opus、jitter buffer、混音）把要送出的封包交給 JS，
//!   JS 再呼叫 [`Room::send_packets`]；收到的封包由 [`Room::recv_voice`] 交給 JS 轉送進 AudioWorklet
//! - 換房主：網頁成員不能當房主（不會成為候補），只會跟著改連新房主（同桌面版 `session::takeover` 的成員部分）
//!
//! JS 介面都用 JSON 字串傳狀態，避免為每個型別寫綁定。

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Context, Result};
use client_core::{Client, ClientEvent};
use iroh::EndpointAddr;
use n0_future::time::{Duration, Instant};
use protocol::control::HostAddr;
use protocol::ticket::Ticket;
use protocol::voice::VoiceUp;
use serde_json::json;
use tokio::sync::mpsc;
use transport::RelaySetup;
use wasm_bindgen::prelude::*;

/// 每次嘗試連新房主的時間上限
const TRY_TIMEOUT: Duration = Duration::from_millis(2500);
/// 計畫性轉移：在這段時間內一直重試連新房主
const MIGRATE_TIMEOUT: Duration = Duration::from_secs(12);
/// 意外斷線：找這麼久都找不到新房主就放棄
const GIVE_UP_AFTER: Duration = Duration::from_secs(20);

fn js_err(e: anyhow::Error) -> JsError {
    JsError::new(&format!("{e:#}"))
}

#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

/// 協定版本（顯示用，也讓網頁確認 wasm 版本對得上）
#[wasm_bindgen]
pub fn version() -> String {
    format!("{} (協定 v{})", env!("CARGO_PKG_VERSION"), protocol::PROTOCOL_VERSION)
}

/// 檢查邀請票格式；正確回傳空字串，否則回傳錯誤說明
#[wasm_bindgen]
pub fn check_ticket(ticket: &str) -> String {
    match ticket.trim().parse::<Ticket>() {
        Ok(_) => String::new(),
        Err(e) => e.to_string(),
    }
}

struct Inner {
    client: Client,
    events: RefCell<Option<mpsc::UnboundedReceiver<ClientEvent>>>,
    /// 換房主進行中（避免同時跑兩個找房主流程）
    recovering: RefCell<bool>,
    rtt: RefCell<Option<f64>>,
    name: String,
}

/// 已加入的房間
#[wasm_bindgen]
pub struct Room {
    inner: Rc<Inner>,
}

/// 加入房間（回傳 Promise<Room>）
#[wasm_bindgen]
pub async fn join(ticket: String, name: String) -> Result<Room, JsError> {
    let ticket: Ticket = ticket.trim().parse().map_err(|e| JsError::new(&format!("邀請票格式錯誤：{e}")))?;
    let ep = transport::bind(transport::generate_secret(), RelaySetup::Default, false).await.map_err(js_err)?;
    let (client, events) = Client::join(&ep, &ticket, &name).await.map_err(js_err)?;
    Ok(Room {
        inner: Rc::new(Inner {
            client,
            events: RefCell::new(Some(events)),
            recovering: RefCell::new(false),
            rtt: RefCell::new(None),
            name,
        }),
    })
}

#[wasm_bindgen]
impl Room {
    /// 等下一個房間事件（Promise<string | undefined>，JSON：`{type, text?}`）。
    /// 回傳 undefined 代表連線已結束。換房主的流程在這裡自動處理
    pub fn next_event(&self) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let Some(mut rx) = inner.events.borrow_mut().take() else { return Ok(JsValue::UNDEFINED) };
            let ev = rx.recv().await;
            *inner.events.borrow_mut() = Some(rx);
            let Some(ev) = ev else { return Ok(JsValue::UNDEFINED) };
            Ok(JsValue::from_str(&handle_event(&inner, ev).to_string()))
        })
    }

    /// 等下一個語音封包（Promise<Uint8Array | undefined>，下行格式，交給 AudioWorklet 解析）
    pub fn recv_voice(&self) -> js_sys::Promise {
        let client = self.inner.client.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            match client.recv_voice().await {
                Ok(b) => Ok(js_sys::Uint8Array::from(&b[..]).into()),
                Err(_) => Ok(JsValue::UNDEFINED),
            }
        })
    }

    /// 送出 AudioWorklet 編好的封包：重複的 `[u16 長度（小端）][上行封包]`
    pub fn send_packets(&self, buf: &[u8]) {
        let mut b = buf;
        while b.len() >= 2 {
            let n = u16::from_le_bytes([b[0], b[1]]) as usize;
            let Some(pkt) = b.get(2..2 + n) else { break };
            if let Some(v) = VoiceUp::parse(pkt) {
                self.inner.client.send_voice(v.kind, v.talk_mask, v.seq, v.payload);
            }
            b = &b[2 + n..];
        }
    }

    /// 房間狀態（JSON）
    pub fn state(&self) -> String {
        let c = &self.inner.client;
        let me = c.session_idx();
        let mut members: Vec<serde_json::Value> =
            c.members().iter().map(|m| json!({ "idx": m.session_idx, "name": m.display_name })).collect();
        members.push(json!({ "idx": me, "name": self.inner.name, "me": true }));
        let subs = c.subscriptions();
        let slots = c.my_slots();
        let channels: Vec<serde_json::Value> = c
            .channels()
            .iter()
            .map(|ch| {
                let who: Vec<serde_json::Value> = subs
                    .iter()
                    .filter(|s| s.channel == ch.id)
                    .map(|s| json!({ "idx": s.session_idx, "listen": s.listen, "talk": s.talk }))
                    .collect();
                json!({ "id": ch.id, "name": ch.name, "members": who, "slot": slots.get(&ch.id) })
            })
            .collect();
        let path = c.path().map(|(relay, rtt)| json!({ "relay": relay, "rtt": rtt }));
        json!({
            "me": me,
            "members": members,
            "channels": channels,
            "rtt": *self.inner.rtt.borrow(),
            "path": path,
            "recovering": *self.inner.recovering.borrow(),
        })
        .to_string()
    }

    /// 加入或更新頻道訂閱，回傳我在這個頻道的槽位
    pub fn subscribe(&self, channel: u32, listen: bool, talk: bool) -> Result<u8, JsError> {
        self.inner.client.subscribe(channel, listen, talk).map_err(js_err)
    }

    pub fn unsubscribe(&self, channel: u32) {
        self.inner.client.unsubscribe(channel);
    }

    pub fn create_channel(&self, name: &str) {
        self.inner.client.create_channel(name);
    }

    /// 某位成員的名稱（找不到回傳空字串）
    pub fn member_name(&self, idx: u16) -> String {
        if idx == self.inner.client.session_idx() {
            return self.inner.name.clone();
        }
        self.inner.client.member_name(idx).unwrap_or_default()
    }

    /// 邀請票（給其他人加入用）
    pub fn ticket(&self) -> String {
        self.inner.client.ticket().to_string()
    }

    /// 離開房間（Promise）
    pub fn leave(&self) -> js_sys::Promise {
        let client = self.inner.client.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            client.leave().await;
            client.endpoint().close().await;
            Ok(JsValue::UNDEFINED)
        })
    }
}

/// 處理一個事件；需要換房主時在背景啟動流程。回傳給 JS 的 JSON
fn handle_event(inner: &Rc<Inner>, ev: ClientEvent) -> serde_json::Value {
    match ev {
        ClientEvent::Rtt(ms) => {
            *inner.rtt.borrow_mut() = Some(ms);
            json!({ "type": "rtt" })
        }
        ClientEvent::HostError(code, msg) => json!({ "type": "error", "text": format!("房主拒絕（{code:?}）：{msg}") }),
        ClientEvent::MigrateTo { host, .. } | ClientEvent::MoveTo { host } => {
            spawn_recover(inner, Some(host));
            json!({ "type": "log", "text": "房主換人了，正在改連新房主…" })
        }
        ClientEvent::HostLost(reason) => {
            spawn_recover(inner, None);
            json!({ "type": "log", "text": format!("與房主斷線（{reason}），正在找新房主…") })
        }
        ClientEvent::Reconnected { .. } => json!({ "type": "log", "text": "已連上房主" }),
        ClientEvent::Disconnected(reason) => json!({ "type": "disconnected", "text": reason }),
        // client-core 已經結束連線，不會重連
        ClientEvent::Kicked => json!({ "type": "disconnected", "text": "你被房主移出房間" }),
        // 網址列的邀請票也要換成新的（重新整理頁面才回得來）
        ClientEvent::InviteChanged => json!({ "type": "invite", "text": "邀請連結已更換，舊的連結不能再用" }),
        ClientEvent::OwnerChanged(_) => json!({ "type": "room" }),
        ClientEvent::MemberLeft(_)
        | ClientEvent::MemberJoined(_)
        | ClientEvent::ChannelAdded(_)
        | ClientEvent::ChannelRemoved(_)
        | ClientEvent::ChannelRenamed(..)
        | ClientEvent::SubscriptionChanged(_) => json!({ "type": "room" }),
    }
}

fn spawn_recover(inner: &Rc<Inner>, target: Option<HostAddr>) {
    if inner.recovering.replace(true) {
        return;
    }
    let inner = inner.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let ok = match target {
            Some(h) => {
                let r = match transport::endpoint_addr(&h) {
                    Ok(addr) => reconnect_until(&inner.client, addr, MIGRATE_TIMEOUT).await,
                    Err(e) => Err(e),
                };
                // 計畫性轉移失敗：當成意外斷線，改找候補
                r.is_ok() || find_new_host(&inner.client).await
            }
            None => find_new_host(&inner.client).await,
        };
        *inner.recovering.borrow_mut() = false;
        if !ok {
            inner.client.give_up("與房主斷線，也找不到新房主");
        }
    });
}

async fn try_addr(client: &Client, addr: EndpointAddr) -> bool {
    n0_future::time::timeout(TRY_TIMEOUT, client.reconnect(addr)).await.is_ok_and(|r| r.is_ok())
}

async fn reconnect_until(client: &Client, addr: EndpointAddr, within: Duration) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() < within {
        if try_addr(client, addr.clone()).await {
            return Ok(());
        }
        n0_future::time::sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow::anyhow!("新房主沒有回應")).context("改連新房主")
}

/// 意外斷線：原房主 → 候補名單依序試，直到有人接手
async fn find_new_host(client: &Client) -> bool {
    let started = Instant::now();
    let old = client.host_addr();
    let successors = client.successors();
    loop {
        if try_addr(client, old.clone()).await {
            return true;
        }
        for s in &successors {
            if let Ok(addr) = transport::endpoint_addr(&s.host)
                && try_addr(client, addr).await
            {
                return true;
            }
        }
        if started.elapsed() > GIVE_UP_AFTER {
            return false;
        }
        n0_future::time::sleep(Duration::from_millis(500)).await;
    }
}
