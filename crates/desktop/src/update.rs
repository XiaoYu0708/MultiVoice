//! 檢查新版本：啟動時在背景查 GitHub 最新的 Release，比目前版本新就在畫面上提示。
//!
//! 用 GitHub 公開 API（不需登入；未登入每小時 60 次，啟動時查一次綽綽有餘）。
//! 查不到（沒網路、被限流、還沒有 Release）就安靜略過，不打擾使用者。
//! 測試用：環境變數 `MULTIVOICE_TEST_VERSION=0.0.1` 假裝目前是舊版。

use std::sync::mpsc;
use std::time::Duration;

use serde::Deserialize;

/// GitHub 儲存庫（owner/repo）
pub const REPO: &str = "XiaoYu0708/MultiVoice";
/// 目前版本（workspace 的 version）
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug)]
pub struct Release {
    /// 不含開頭的 v，例如 "0.2.0"
    pub version: String,
    /// Release 頁面
    pub url: String,
}

#[derive(Deserialize)]
struct Latest {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
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

fn fetch() -> Result<Release, Box<dyn std::error::Error>> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build().into();
    let latest: Latest = agent
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
    Ok(Release { version: latest.tag_name.trim_start_matches('v').to_string(), url: latest.html_url })
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
}
