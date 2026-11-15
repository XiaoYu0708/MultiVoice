//! 房主轉移的流程（M3，PLAN.md §5）。不含音訊，可以單獨測試（`tests/takeover.rs`）。
//!
//! - **備用端點**：成員進房時預先建一個可接受連線的 iroh 端點，上線後用 `Standby` 告訴房主。
//!   平常這個端點只做轉址：有人拿含它的邀請票來連，就 `Redirect` 到現任房主
//! - **計畫性轉移**（[`Takeover::on_migrate`]）：被指定的人以本機看到的房間狀態啟動房主（`Seed`），
//!   自己的客戶端改連過去；其他人改連新房主
//! - **非預期轉移**（[`Takeover::on_host_lost`]）：每 0.5 秒一輪，先試原房主（可能只是網路閃斷），
//!   再依序試候補；排第 k 位的候補在失聯 k × 3 秒後、前面的都連不上時自己接手。20 秒都不成就放棄
//!
//! 還沒做（PLAN §5.3–5.4）：接手前的多數確認、雙房主時以任期決勝與合併。
//! 所以「只有候補自己斷網、其實房主還在」時，他會自己開一個只有他的房間。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use client_core::Client;
use host_core::{Host, HostConfig, Seed};
use iroh::{Endpoint, EndpointAddr};
use protocol::control::{ClientMsg, HostAddr, HostMsg};
use protocol::SessionIdx;
use tokio::sync::Notify;
use tracing::{debug, info};

/// 一次連線嘗試的時限
const TRY_TIMEOUT: Duration = Duration::from_millis(1500);
/// 第 k 位候補在失聯 k × 這麼久之後才自己接手
const RANK_DELAY: Duration = Duration::from_secs(3);
const GIVE_UP_AFTER: Duration = Duration::from_secs(20);
/// 計畫性轉移時，等新房主上線的時限
const MIGRATE_TIMEOUT: Duration = Duration::from_secs(12);

pub type Log = Arc<dyn Fn(String) + Send + Sync>;

pub struct Takeover {
    client: Client,
    /// 我的備用房主端點（原始房主沒有）
    standby: Option<Endpoint>,
    /// 我正在當房主時的 host（原始房主一開始就有）
    host: Arc<Mutex<Option<Host>>>,
    /// 停止轉址（升任房主時，端點改由 host 接收連線）
    stop_redirector: Arc<Notify>,
    /// 正在找新房主
    busy: AtomicBool,
    log: Log,
}

impl Takeover {
    pub fn new(client: Client, standby: Option<Endpoint>, host: Option<Host>, log: Log) -> Arc<Self> {
        Arc::new(Self {
            client,
            standby,
            host: Arc::new(Mutex::new(host)),
            stop_redirector: Arc::new(Notify::new()),
            busy: AtomicBool::new(false),
            log,
        })
    }

    /// 我目前是不是房主
    pub fn host(&self) -> Option<Host> {
        self.host.lock().unwrap().clone()
    }

    /// 正在找新房主
    pub fn recovering(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    pub fn standby(&self) -> Option<&Endpoint> {
        self.standby.as_ref()
    }

    /// 備用端點上線後告訴房主，並開始轉址服務
    pub fn start_standby(self: &Arc<Self>, wait_online: bool) {
        let Some(ep) = self.standby.clone() else { return };
        let me = self.clone();
        tokio::spawn(async move {
            if wait_online {
                // 等連上 relay，位址裡才有 relay URL；逾時也照樣宣告（至少有直連位址）
                let _ = tokio::time::timeout(Duration::from_secs(10), ep.online()).await;
            }
            me.client.announce_standby(transport::host_addr(&ep));
            me.redirect_loop(ep).await;
        });
    }

    /// 平常：拿含備用端點的邀請票來的人，轉址到現任房主
    async fn redirect_loop(self: Arc<Self>, ep: Endpoint) {
        loop {
            let incoming = tokio::select! {
                i = ep.accept() => match i { Some(i) => i, None => return },
                _ = self.stop_redirector.notified() => return,
            };
            let me = self.clone();
            tokio::spawn(async move {
                let Ok(conn) = incoming.await else { return };
                let Ok(Ok((mut send, mut recv))) = tokio::time::timeout(Duration::from_secs(5), conn.accept_bi()).await
                else {
                    return;
                };
                if !matches!(transport::read_msg::<ClientMsg>(&mut recv).await, Ok(ClientMsg::Hello { .. })) {
                    return;
                }
                // 正在找新房主時不知道誰是房主：直接關掉，讓對方快點試下一個
                if !me.busy.load(Ordering::SeqCst) {
                    let msg = HostMsg::Redirect { host: me.client.host_id(), term: me.client.term() };
                    let _ = transport::write_msg(&mut send, &msg).await;
                    let _ = send.finish();
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                conn.close(0u32.into(), b"not host");
            });
        }
    }

    /// 以任期 `term` 升任房主：用我看到的房間狀態啟動 host-core，自己的客戶端改連過去
    async fn promote(&self, term: u64) -> Result<()> {
        let ep = self.standby.clone().ok_or_else(|| anyhow::anyhow!("沒有備用端點"))?;
        let me = self.client.node_id();
        let mut members: Vec<_> = self.client.members().into_iter().map(|m| (m.node_id, m.session_idx)).collect();
        members.push((me, self.client.session_idx()));
        let t = self.client.ticket();
        let host = Host::new(HostConfig {
            room_id: t.room_id,
            secret: t.secret,
            term,
            max_members: 100,
            owner: Some(me),
            seed: Some(Seed { channels: self.client.channels(), members }),
        });
        // notify_one 會保留一次通知：轉址 task 就算剛好不在等，下一輪也會馬上收到而退出
        self.stop_redirector.notify_one();
        tokio::time::sleep(Duration::from_millis(50)).await;
        tokio::spawn({
            let (host, ep) = (host.clone(), ep.clone());
            async move { host.run(ep).await }
        });
        *self.host.lock().unwrap() = Some(host);
        info!("升任房主（任期 {term}）");
        (self.log)(format!("你成為新房主（任期 {term}）"));
        let addr = transport::loopback_addr(&ep);
        for _ in 0..5 {
            if tokio::time::timeout(TRY_TIMEOUT, self.client.reconnect(addr.clone())).await.is_ok_and(|r| r.is_ok()) {
                return Ok(());
            }
        }
        anyhow::bail!("連不上自己的房主端點")
    }

    /// 計畫性轉移：房主要離開，指定 `session_idx` 接手
    pub async fn on_migrate(self: Arc<Self>, session_idx: SessionIdx, host: HostAddr, term: u64) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let result = if session_idx == self.client.session_idx() && self.standby.is_some() {
            self.promote(term).await
        } else {
            (self.log)("房主正在轉移，連線到新房主…".into());
            self.reconnect_until(transport::endpoint_addr(&host), MIGRATE_TIMEOUT).await
        };
        self.busy.store(false, Ordering::SeqCst);
        if let Err(e) = result {
            debug!("計畫性轉移失敗：{e:#}");
            // 新房主沒出現：當成房主失聯處理（候補名單裡的其他人會接手）
            self.clone().on_host_lost().await;
        }
    }

    async fn reconnect_until(&self, addr: Result<EndpointAddr>, within: Duration) -> Result<()> {
        let addr = addr?;
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if tokio::time::timeout(TRY_TIMEOUT, self.client.reconnect(addr.clone())).await.is_ok_and(|r| r.is_ok()) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        anyhow::bail!("新房主沒有回應")
    }

    /// 與房主失聯：找新房主，輪到自己就接手
    pub async fn on_host_lost(self: Arc<Self>) {
        {
            if self.busy.swap(true, Ordering::SeqCst) {
                return;
            }
            (self.log)("與房主斷線，正在找新房主…".into());
            let started = Instant::now();
            let old = self.client.host_addr();
            let term = self.client.term();
            let me = self.client.session_idx();
            let successors = self.client.successors();
            let my_rank = successors.iter().position(|s| s.session_idx == me).filter(|_| self.standby.is_some());
            let try_addr = |addr: EndpointAddr| {
                let c = self.client.clone();
                async move { tokio::time::timeout(TRY_TIMEOUT, c.reconnect(addr)).await.is_ok_and(|r| r.is_ok()) }
            };
            let done = 'outer: loop {
                // 原房主（可能只是網路閃斷）
                if try_addr(old.clone()).await {
                    (self.log)("已重新連上房主".into());
                    break true;
                }
                for (k, s) in successors.iter().enumerate() {
                    if Some(k) == my_rank {
                        // 前面的候補都連不上，而且等夠久了：自己接手
                        if started.elapsed() >= RANK_DELAY * k as u32 {
                            match self.promote(term + 1).await {
                                Ok(()) => break 'outer true,
                                Err(e) => debug!("接手失敗：{e:#}"),
                            }
                        }
                        break;
                    }
                    if let Ok(addr) = transport::endpoint_addr(&s.host)
                        && try_addr(addr).await
                    {
                        (self.log)("已連上新房主".into());
                        break 'outer true;
                    }
                }
                if started.elapsed() > GIVE_UP_AFTER {
                    break false;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            };
            self.busy.store(false, Ordering::SeqCst);
            if !done {
                self.client.give_up("與房主斷線，也找不到可以接手的人");
            }
        }
    }
}
