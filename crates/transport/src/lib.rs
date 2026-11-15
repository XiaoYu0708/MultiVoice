//! MultiVoice 的 iroh 連線設定與控制訊息讀寫（host-core / client-core 共用）。

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use iroh::endpoint::{RecvStream, SendStream, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey, TransportAddr};
use protocol::NodeId;
use protocol::control::{decode_body, encode_frame, frame_len};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use protocol::ALPN;

/// 自架 relay（PLAN.md §7.3、§9）
pub const DEFAULT_RELAY: &str = "https://relay.xiaoyu0708.ip-ddns.com/";

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
            map.insert(own.clone(), Arc::new(RelayConfig::from(own)));
            Endpoint::builder(presets::N0).relay_mode(RelayMode::Custom(map))
        }
        RelaySetup::Custom(urls) => {
            Endpoint::builder(presets::N0).relay_mode(RelayMode::Custom(RelayMap::from_iter(urls)))
        }
    };
    b = b.secret_key(secret);
    if accept {
        b = b.alpns(vec![ALPN.to_vec()]);
    }
    b.bind().await.context("無法建立 iroh 端點")
}

/// 測試用：Disabled 模式下沒有位址查詢，要直接給本機位址
pub fn loopback_addr(ep: &Endpoint) -> EndpointAddr {
    let addrs = ep.bound_sockets().into_iter().map(|a| {
        let ip = if a.is_ipv4() { std::net::Ipv4Addr::LOCALHOST.into() } else { std::net::Ipv6Addr::LOCALHOST.into() };
        TransportAddr::Ip(SocketAddr::new(ip, a.port()))
    });
    EndpointAddr::from_parts(ep.id(), addrs)
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
