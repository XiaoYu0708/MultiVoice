//! 使用者設定：`%APPDATA%\MultiVoice\settings.toml`，修改後自動儲存。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub name: String,
    /// None = 系統預設裝置
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    /// true = 一直開麥；false = 偵測到說話才送出
    pub open_mic: bool,
    pub denoise: bool,
    /// 降噪時人聲混回原音的比例
    pub speech_mix: f32,
    /// 輸出音量（1.0 = 原音量）
    pub output_volume: f32,
    /// 最近一次使用的邀請票（方便重新加入）
    pub last_ticket: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            name: std::env::var("USERNAME").unwrap_or_else(|_| "玩家".into()),
            input_device: None,
            output_device: None,
            open_mic: false,
            denoise: true,
            speech_mix: 0.3,
            output_volume: 1.0,
            last_ticket: String::new(),
        }
    }
}

/// 每個身分設定檔各自一份設定：default → settings.toml，其他 → settings-<profile>.toml
fn path(profile: &str) -> Option<PathBuf> {
    let file = if profile == "default" { "settings.toml".to_string() } else { format!("settings-{profile}.toml") };
    std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("MultiVoice").join(file))
}

impl Settings {
    pub fn load(profile: &str) -> Self {
        path(profile)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, profile: &str) {
        let Some(p) = path(profile) else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = toml::to_string_pretty(self) {
            let _ = std::fs::write(p, s);
        }
    }

    pub fn audio_opts(&self) -> session::AudioOpts {
        session::AudioOpts {
            input: self.input_device.clone(),
            output: self.output_device.clone(),
            open_mic: self.open_mic,
            denoise: self.denoise,
            mix: self.speech_mix,
            volume: self.output_volume,
            ..Default::default()
        }
    }
}
