//! 永久身分金鑰：`%APPDATA%\MultiVoice\identity-<profile>.key`（32 bytes）。
//! NodeId 就是帳號（PLAN.md §3.3），所以金鑰要保存下來。

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use iroh::SecretKey;

fn dir() -> Result<PathBuf> {
    let base = std::env::var_os("APPDATA").context("找不到 APPDATA")?;
    Ok(PathBuf::from(base).join("MultiVoice"))
}

pub fn load_or_create(profile: &str) -> Result<SecretKey> {
    if profile.is_empty() || !profile.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("profile 名稱只能用英數字、- 與 _");
    }
    let path = dir()?.join(format!("identity-{profile}.key"));
    if let Ok(bytes) = std::fs::read(&path) {
        let b: [u8; 32] = bytes.as_slice().try_into().with_context(|| format!("{} 已損毀", path.display()))?;
        return Ok(SecretKey::from_bytes(&b));
    }
    let key = transport::generate_secret();
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, key.to_bytes()).with_context(|| format!("無法寫入 {}", path.display()))?;
    Ok(key)
}
