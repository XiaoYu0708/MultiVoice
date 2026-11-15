//! 檢查新版本與一鍵更新（M8）。
//!
//! - 檢查：啟動時在背景查 GitHub 最新的 Release（公開 API，不需登入；未登入每小時 60 次）。
//!   查不到（沒網路、被限流、還沒有 Release）就安靜略過，不打擾使用者
//! - 更新：下載 Release 附的 `multivoice.exe` 到同資料夾的 `multivoice.exe.new`，核對大小與 GitHub 提供的 SHA-256，
//!   再把執行中的 exe 改名成 `.old`（Windows 不能覆寫執行中的 exe，但可以改名）、新檔改成原本的名字，
//!   最後由呼叫端重新啟動。任何一步失敗都還原，不會留下壞掉的執行檔。`.old` 在下次啟動時刪掉（[`cleanup_old`]）
//! - 測試用：環境變數 `MULTIVOICE_TEST_VERSION=0.0.1` 假裝目前是舊版；參數 `--auto-update` 找到新版就直接更新

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// GitHub 儲存庫（owner/repo）
pub const REPO: &str = "XiaoYu0708/MultiVoice";
/// 目前版本（workspace 的 version）
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Release 裡桌面版執行檔的名稱
const ASSET: &str = "multivoice.exe";

#[derive(Clone, Debug)]
pub struct Release {
    /// 不含開頭的 v，例如 "0.2.0"
    pub version: String,
    /// Release 頁面
    pub url: String,
    /// 可以自動更新的執行檔（舊的 Release 或檔案不見時沒有）
    pub asset: Option<Asset>,
}

#[derive(Clone, Debug)]
pub struct Asset {
    pub url: String,
    pub size: u64,
    /// 小寫十六進位；GitHub 較舊的 Release 沒有
    pub sha256: Option<String>,
}

#[derive(Deserialize)]
struct AssetJson {
    name: String,
    size: u64,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

#[derive(Deserialize)]
struct Latest {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<AssetJson>,
}

/// 目前版本（可用環境變數假裝成舊版來測試）
pub fn current() -> String {
    std::env::var("MULTIVOICE_TEST_VERSION").unwrap_or_else(|_| VERSION.to_string())
}

/// 背景查詢；結果（有新版才是 Some）從回傳的 channel 取
pub fn spawn_check() -> mpsc::Receiver<Option<Release>> {
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new().name("update-check".into()).spawn(move || {
        let r = fetch().ok().filter(|r| is_newer(&r.version, &current()));
        let _ = tx.send(r);
    });
    rx
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(timeout)).build().into()
}

fn fetch() -> Result<Release, Box<dyn std::error::Error>> {
    let latest: Latest = agent(Duration::from_secs(10))
        .get(&format!("https://api.github.com/repos/{REPO}/releases/latest"))
        // GitHub API 規定要有 User-Agent；用通用名稱，不帶任何使用者資訊
        .header("User-Agent", "MultiVoice-update-check")
        .header("Accept", "application/vnd.github+json")
        .call()?
        .body_mut()
        .read_json()?;
    if latest.draft || latest.prerelease {
        return Err("不是正式版".into());
    }
    let asset = latest.assets.into_iter().find(|a| a.name.eq_ignore_ascii_case(ASSET)).map(|a| Asset {
        url: a.browser_download_url,
        size: a.size,
        sha256: a.digest.and_then(|d| d.strip_prefix("sha256:").map(str::to_ascii_lowercase)),
    });
    Ok(Release { version: latest.tag_name.trim_start_matches('v').to_string(), url: latest.html_url, asset })
}

// ---------------------------------------------------------------- 一鍵更新

/// 進行中的更新（下載 → 驗證 → 換檔）
pub struct Install {
    /// 已下載的位元組
    pub done: Arc<AtomicU64>,
    pub total: u64,
    rx: mpsc::Receiver<Result<PathBuf, String>>,
}

impl Install {
    /// 0..1
    pub fn progress(&self) -> f32 {
        if self.total == 0 { 0.0 } else { self.done.load(Ordering::Relaxed) as f32 / self.total as f32 }
    }

    /// 完成時：Ok(新執行檔路徑，可以重新啟動了) 或 Err(原因)
    pub fn poll(&self) -> Option<Result<PathBuf, String>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err("更新程序異常結束".into())),
        }
    }
}

/// 在背景下載並換掉目前的執行檔
pub fn spawn_install(asset: Asset) -> Install {
    let done = Arc::new(AtomicU64::new(0));
    let (tx, rx) = mpsc::channel();
    let total = asset.size;
    let d = done.clone();
    let _ = std::thread::Builder::new().name("update-install".into()).spawn(move || {
        let r = std::env::current_exe()
            .map_err(|e| format!("找不到目前的執行檔：{e}"))
            .and_then(|exe| install(&asset, &exe, &d).map(|()| exe));
        let _ = tx.send(r);
    });
    Install { done, total, rx }
}

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut s = exe.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn install(asset: &Asset, exe: &Path, done: &AtomicU64) -> Result<(), String> {
    let new = sibling(exe, ".new");
    let r = download(asset, &new, done).and_then(|()| replace(exe, &new));
    if r.is_err() {
        let _ = std::fs::remove_file(&new);
    }
    r
}

fn download(asset: &Asset, to: &Path, done: &AtomicU64) -> Result<(), String> {
    let mut resp = agent(Duration::from_secs(300))
        .get(&asset.url)
        .header("User-Agent", "MultiVoice-update")
        .call()
        .map_err(|e| format!("下載失敗：{e}"))?;
    let mut body = resp.body_mut().as_reader();
    let mut file = std::fs::File::create(to).map_err(|e| format!("無法寫入程式所在的資料夾（{e}）"))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut n_total = 0u64;
    loop {
        let n = body.read(&mut buf).map_err(|e| format!("下載中斷：{e}"))?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| format!("寫入失敗：{e}"))?;
        n_total += n as u64;
        done.store(n_total, Ordering::Relaxed);
    }
    file.sync_all().map_err(|e| format!("寫入失敗：{e}"))?;
    if n_total != asset.size {
        return Err(format!("下載的檔案大小不對（{n_total}／{} bytes）", asset.size));
    }
    if let Some(want) = &asset.sha256 {
        let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if &got != want {
            return Err("下載的檔案驗證失敗（SHA-256 不符）".into());
        }
    }
    Ok(())
}

/// 執行中的 exe 改名成 `.old`，新檔換上；失敗就改回來
fn replace(exe: &Path, new: &Path) -> Result<(), String> {
    let old = sibling(exe, ".old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).map_err(|e| format!("無法替換執行檔（{e}）"))?;
    if let Err(e) = std::fs::rename(new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(format!("無法替換執行檔（{e}）"));
    }
    Ok(())
}

/// 啟動時清掉上次更新留下的 `.old`（舊程式可能還在結束中，多試幾次）
pub fn cleanup_old() {
    let Ok(exe) = std::env::current_exe() else { return };
    let old = sibling(&exe, ".old");
    if !old.exists() {
        return;
    }
    let _ = std::thread::Builder::new().name("update-cleanup".into()).spawn(move || {
        for _ in 0..20 {
            if std::fs::remove_file(&old).is_ok() || !old.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

/// 用新的執行檔重新啟動：保留 `--profile`；`rejoin` = 重新啟動後自動加入這張邀請票的房間
pub fn restart(exe: &Path, rejoin: Option<&str>) -> std::io::Result<()> {
    let mut args: Vec<String> = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--profile" => {
                args.push(a);
                args.extend(it.next());
            }
            // 自動加入／建房只在第一次啟動有意義
            "--auto-join" => {
                it.next();
            }
            "--auto-host" | "--auto-update" | crate::instance::RESTARTED => {}
            _ => args.push(a),
        }
    }
    // 舊程式（我）還在結束中：新的要等我結束，不是把我叫醒
    args.push(crate::instance::RESTARTED.into());
    if let Some(t) = rejoin {
        args.push("--auto-join".into());
        args.push(t.to_string());
    }
    std::process::Command::new(exe).args(args).spawn().map(|_| ())
}

/// 版本號比較（"0.10.0" > "0.9.3"）；看不懂的部分當 0
pub fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.trim_start_matches('v').split(['.', '-', '+']).take(3).map(|p| p.parse().unwrap_or(0)).collect()
    };
    let (a, b) = (parse(latest), parse(current));
    (0..3).map(|i| a.get(i).copied().unwrap_or(0)).cmp((0..3).map(|i| b.get(i).copied().unwrap_or(0))).is_gt()
}

/// 用預設瀏覽器開網址
pub fn open_url(url: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let w = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let (verb, target) = (w("open"), w(url));
        unsafe {
            ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), target.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("v0.10.0", "0.9.3"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(is_newer("0.1.1", "0.1"));
    }

    /// 換檔：成功時新內容就位、舊檔留在 .old；新檔不存在時還原
    #[test]
    fn replace_and_rollback() {
        let dir = std::env::temp_dir().join(format!("mv-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("multivoice.exe");
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(sibling(&exe, ".new"), b"new").unwrap();
        replace(&exe, &sibling(&exe, ".new")).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(sibling(&exe, ".old")).unwrap(), b"old");

        assert!(replace(&exe, &dir.join("missing")).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"new", "失敗時還原");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
