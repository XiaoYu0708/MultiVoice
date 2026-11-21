//! 常駐房間：在伺服器上一直開著的房間，只負責轉發，本身不說也不聽（沒有音訊、可以在沒有音效卡的 Linux／Docker 跑）。
//!
//!   multivoice-room-server --state /data/room.txt --channel 小隊A --channel 小隊B
//!
//! - 房間 ID、密鑰、轉發端點的金鑰存在 `--state` 檔，重開後**邀請票不變**（貼在別處的邀請連結一直有效）
//! - 管理者是一個沒人持有的身分：沒有人能改名／刪頻道、移出成員、更換邀請（避免邀請連結被換掉）；成員可以自己建頻道
//! - `--channel` 每次啟動都建好（大廳一定有）；執行中別人建的頻道重開後不保留
//! - 邀請票印在輸出，也寫到 `--state` 旁邊的 `ticket.txt`
//! - 成員端的桌面版仍可能在轉發者暫時斷線時接手轉發（一般的房主轉移），伺服器回來後會合併回來

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use host_core::{Host, HostConfig, Seed};
use protocol::control::ChannelInfo;
use protocol::ticket::{RoomId, RoomSecret, Ticket};

struct State {
    room_id: [u8; 32],
    secret: [u8; 32],
    host_key: [u8; 32],
    owner: [u8; 32],
}

fn hex(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Result<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        bail!("長度不對");
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

impl State {
    fn load_or_create(path: &Path) -> Result<Self> {
        if let Ok(text) = std::fs::read_to_string(path) {
            let get = |k: &str| -> Result<[u8; 32]> {
                let line = text.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).with_context(|| format!("{} 少了 {k}", path.display()))?;
                unhex(line).with_context(|| format!("{} 的 {k} 格式錯誤", path.display()))
            };
            return Ok(Self { room_id: get("room_id")?, secret: get("secret")?, host_key: get("host_key")?, owner: get("owner")? });
        }
        let owner_key = transport::generate_secret();
        let st = Self {
            room_id: transport::random_bytes(),
            secret: transport::random_bytes(),
            host_key: transport::random_bytes(),
            // 管理者：只留公開的 ID、丟掉私鑰，沒有人能以管理者身分登入
            owner: transport::node_id(&owner_key.public()),
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let text = format!(
            "# MultiVoice 常駐房間（multivoice-room-server）。刪掉這個檔案＝換一個新房間（舊的邀請連結失效）\nroom_id={}\nsecret={}\nhost_key={}\nowner={}\n",
            hex(&st.room_id),
            hex(&st.secret),
            hex(&st.host_key),
            hex(&st.owner)
        );
        std::fs::write(path, text).with_context(|| format!("無法寫入 {}", path.display()))?;
        println!("建立新房間：{}", path.display());
        Ok(st)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,iroh=warn".into()))
        .init();
    let mut state_path = PathBuf::from("room.txt");
    let mut channels: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--state" => state_path = args.next().context("--state 要接檔案路徑")?.into(),
            "--channel" => channels.push(args.next().context("--channel 要接頻道名稱")?),
            "-h" | "--help" => {
                println!("用法：multivoice-room-server [--state room.txt] [--channel 名稱]...");
                return Ok(());
            }
            other => bail!("看不懂的參數：{other}"),
        }
    }
    let st = State::load_or_create(&state_path)?;
    let ep = transport::bind(transport::secret_from_bytes(&st.host_key), transport::RelaySetup::Default, true).await?;
    let ticket = Ticket { room_id: RoomId(st.room_id), secret: RoomSecret(st.secret), bootstrap: vec![transport::node_id(&ep.id())] };
    let seed = Seed {
        channels: channels.iter().enumerate().map(|(i, name)| ChannelInfo { id: i as u32 + 1, name: name.clone() }).collect(),
        members: Vec::new(),
    };
    let host = Host::new(HostConfig {
        room_id: ticket.room_id,
        secret: ticket.secret,
        term: 1,
        max_members: 100,
        owner: Some(st.owner),
        local: None,
        seed: Some(seed),
    });
    tokio::spawn({
        let (host, ep) = (host.clone(), ep.clone());
        async move { host.run(ep).await }
    });
    // 連上 relay 後別人才找得到這個房間
    if tokio::time::timeout(std::time::Duration::from_secs(15), ep.online()).await.is_err() {
        eprintln!("15 秒內沒連上 relay，繼續等待（可能只是 relay 暫時連不上）");
    }
    let ticket = ticket.to_string();
    let ticket_file = state_path.with_file_name("ticket.txt");
    std::fs::write(&ticket_file, format!("{ticket}\nhttps://multivoice.xiaoyu0708.ip-ddns.com/#{ticket}\n")).ok();
    println!("房間已開啟，邀請票（也寫在 {}）：\n{ticket}", ticket_file.display());

    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    println!("關閉房間");
    host.stop();
    ep.close().await;
    Ok(())
}
