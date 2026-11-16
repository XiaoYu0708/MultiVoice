//! MultiVoice 的 iroh 連線設定與控制訊息讀寫（host-core / client-core 共用）。

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use iroh::endpoint::{RecvStream, SendStream, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey, TransportAddr};
use protocol::NodeId;
use protocol::control::{HostAddr, decode_body, encode_frame, frame_len};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use protocol::ALPN;

/// 自架 relay（PLAN.md §7.3、§9）
pub const DEFAULT_RELAY: &str = "https://relay.xiaoyu0708.ip-ddns.com/";

/// 自架 relay 的存取權杖（relay 設定 `access.shared_token`）：**編譯時**由環境變數 `MULTIVOICE_RELAY_TOKEN` 帶入，
/// 不寫在原始碼裡（GitHub Actions 用 secret `RELAY_TOKEN`；本機放在不納入 git 的 `.cargo/config.toml` 的 `[env]`）。
/// 沒有權杖的版本連不上自架 relay，會改用 n0 的公共 relay。
/// 注意：權杖會編進公開的執行檔與網頁版，有心人拿得到；它只是擋掉隨便拿來用的其他 iroh 程式，不是密碼
pub const RELAY_TOKEN: Option<&str> = option_env!("MULTIVOICE_RELAY_TOKEN");

fn own_relay_config(url: RelayUrl) -> RelayConfig {
    let cfg = RelayConfig::from(url);
    match RELAY_TOKEN.filter(|t| !t.is_empty()) {
        Some(t) => cfg.with_auth_token(t),
        None => cfg,
    }
}

/// 網頁版客戶端（PLAN.md §8）。邀請連結 = 這個網址 + `#` + 邀請票（票不會傳到網站）
pub const WEB_CLIENT_URL: &str = "https://voice.xiaoyu0708.ip-ddns.com/";

#[derive(Clone, Debug)]
pub enum RelaySetup {
    /// 自架 relay 優先，n0 公共 relay 作為備援
    Default,
    /// 只用這些 relay
    Custom(Vec<RelayUrl>),
    /// 不用 relay、不用位址查詢，只走直連（測試用）
    Disabled,
}

/// 建立 iroh 端點。`accept = true` 時接受 MultiVoice 的連線（房主）。
pub async fn bind(secret: SecretKey, relays: RelaySetup, accept: bool) -> Result<Endpoint> {
    let mut b = match relays {
        RelaySetup::Disabled => Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled),
        RelaySetup::Default => {
            let map = iroh::defaults::prod::default_relay_map();
            let own: RelayUrl = DEFAULT_RELAY.parse().expect("DEFAULT_RELAY 是合法 URL");
            map.insert(own.clone(), Arc::new(own_relay_config(own)));
            Endpoint::builder(presets::N0).relay_mode(RelayMode::Custom(map))
        }
        RelaySetup::Custom(urls) => {
            // 指定的 relay（自己架的、測試用的）也帶權杖
            Endpoint::builder(presets::N0).relay_mode(RelayMode::Custom(RelayMap::from_iter(urls.into_iter().map(own_relay_config))))
        }
    };
    b = b.secret_key(secret);
    if accept {
        b = b.alpns(vec![ALPN.to_vec()]);
    }
    b.bind().await.context("無法建立 iroh 端點")
}

/// 測試用：Disabled 模式下沒有位址查詢，要直接給本機位址
#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
pub fn loopback_addr(ep: &Endpoint) -> EndpointAddr {
    let addrs = ep.bound_sockets().into_iter().map(|a| {
        let ip = if a.is_ipv4() { std::net::Ipv4Addr::LOCALHOST.into() } else { std::net::Ipv6Addr::LOCALHOST.into() };
        TransportAddr::Ip(SocketAddr::new(ip, a.port()))
    });
    EndpointAddr::from_parts(ep.id(), addrs)
}

/// 瀏覽器沒有 UDP socket：沒有本機位址
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
pub fn loopback_addr(ep: &Endpoint) -> EndpointAddr {
    EndpointAddr::from(ep.id())
}

/// 端點目前的位址（直連位址、relay、本機 loopback）→ 可以傳給其他成員的 [`HostAddr`]。
/// 加上 loopback 是為了關閉 relay 的本機測試；遠端撥到 loopback 只會失敗（iroh 會驗證 NodeId，不會連錯人）。
pub fn host_addr(ep: &Endpoint) -> HostAddr {
    let a = ep.addr();
    let mut addrs: Vec<SocketAddr> = a.ip_addrs().copied().collect();
    for t in loopback_addr(ep).addrs {
        if let TransportAddr::Ip(s) = t
            && !addrs.contains(&s)
        {
            addrs.push(s);
        }
    }
    HostAddr { id: node_id(&ep.id()), addrs, relay: a.relay_urls().next().map(|u| u.to_string()) }
}

/// `EndpointAddr` → [`HostAddr`]（可以放進協定訊息）
pub fn host_addr_of(a: &EndpointAddr) -> HostAddr {
    HostAddr {
        id: node_id(&a.id),
        addrs: a.ip_addrs().copied().collect(),
        relay: a.relay_urls().next().map(|u| u.to_string()),
    }
}

/// [`HostAddr`] → 可以撥號的 `EndpointAddr`
pub fn endpoint_addr(h: &HostAddr) -> Result<EndpointAddr> {
    let relay = h.relay.as_deref().and_then(|u| u.parse::<RelayUrl>().ok()).map(TransportAddr::Relay);
    let ips = h.addrs.iter().copied().map(TransportAddr::Ip);
    Ok(EndpointAddr::from_parts(endpoint_id(&h.id)?, ips.chain(relay)))
}

pub fn node_id(id: &EndpointId) -> NodeId {
    *id.as_bytes()
}

pub fn endpoint_id(n: &NodeId) -> Result<EndpointId> {
    EndpointId::from_bytes(n).context("無效的 NodeId")
}

/// 寫入一則控制訊息（長度前綴 + postcard）
pub async fn write_msg<T: Serialize>(send: &mut SendStream, msg: &T) -> Result<()> {
    send.write_all(&encode_frame(msg)).await.context("寫入控制訊息失敗")
}

/// 讀取一則控制訊息
pub async fn read_msg<T: DeserializeOwned>(recv: &mut RecvStream) -> Result<T> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await.context("讀取控制訊息失敗")?;
    let n = frame_len(len)?;
    let mut body = vec![0u8; n];
    recv.read_exact(&mut body).await.context("讀取控制訊息失敗")?;
    Ok(decode_body(&body)?)
}

/// 作業系統亂數（房間 ID、房間密鑰、加入挑戰的 nonce、身分金鑰）
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("作業系統亂數產生器無法使用");
    b
}

/// 隨機產生一把身分金鑰
pub fn generate_secret() -> SecretKey {
    SecretKey::from_bytes(&random_bytes())
}
