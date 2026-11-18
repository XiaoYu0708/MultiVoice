//! 使用者設定：`%APPDATA%\MultiVoice\settings.toml`，修改後自動儲存。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub name: String,
    /// None = 系統預設裝置
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub denoise: bool,
    /// 降噪時人聲混回原音的比例
    pub speech_mix: f32,
    /// 「偵測說話」的音量門檻（dBFS）
    pub gate_dbfs: f32,
    /// 自動音量（AGC）
    pub agc: bool,
    /// 強力降噪（DeepFilterNet3）
    pub strong_denoise: bool,
    /// 輸出音量（1.0 = 原音量）
    pub output_volume: f32,
    /// 最近一次使用的邀請票（方便重新加入）
    pub last_ticket: String,
    /// 按鍵說話：頻道名稱 → 按鍵（頻道 ID 每次建房都不同，所以用名稱）。
    /// 有設定的頻道，加入時自動用「按鍵說話」
    pub ptt_keys: BTreeMap<String, String>,
    /// 我聽各成員的音量（成員名稱 → 倍率 0..10）；右鍵成員名字調整。用名稱是因為網頁版成員每次的身分都不同
    pub member_volumes: BTreeMap<String, f32>,
    /// 開／關麥提示音
    pub cues: bool,
    /// 在房間中關閉視窗時縮到系統匣（不離開房間）
    pub close_to_tray: bool,
    /// 房主離開或斷線時可以由我接手當房主
    pub allow_host: bool,
    /// 頻道設定檔（房間的頻道 + 我在各頻道的設定，一鍵切換；開房時可選一個自動建好頻道）
    pub presets: Vec<crate::presets::Preset>,
    /// 最近套用／儲存的設定檔（下次開啟時預先選好）
    pub last_preset: Option<String>,
    /// 開房時套用的設定檔（None = 不用，只有大廳）
    pub host_preset: Option<String>,
    /// 舊版（v0.8.1～0.8.3）的頻道模板：讀進來後轉成設定檔，不再寫回
    #[serde(skip_serializing)]
    room_templates: Vec<crate::presets::RoomTemplate>,
    #[serde(skip_serializing)]
    last_template: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            name: std::env::var("USERNAME").unwrap_or_else(|_| "玩家".into()),
            input_device: None,
            output_device: None,
            denoise: true,
            speech_mix: 0.3,
            gate_dbfs: -50.0,
            agc: true,
            strong_denoise: false,
            output_volume: 1.0,
            last_ticket: String::new(),
            ptt_keys: BTreeMap::new(),
            member_volumes: BTreeMap::new(),
            cues: true,
            close_to_tray: true,
            allow_host: true,
            presets: Vec::new(),
            last_preset: None,
            host_preset: None,
            room_templates: Vec::new(),
            last_template: None,
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
        let mut s: Self = path(profile)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();
        s.migrate_templates();
        s
    }

    /// 舊版的頻道模板轉成設定檔（同名就在後面加「（模板）」）
    fn migrate_templates(&mut self) {
        for t in std::mem::take(&mut self.room_templates) {
            let was_host_choice = self.last_template.as_ref() == Some(&t.name);
            let mut p = t.into_preset(protocol::DEFAULT_CHANNEL_NAME);
            if self.presets.iter().any(|q| q.name == p.name) {
                p.name += "（模板）";
            }
            if self.presets.iter().any(|q| q.name == p.name) {
                continue;
            }
            if was_host_choice && self.host_preset.is_none() {
                self.host_preset = Some(p.name.clone());
            }
            self.presets.push(p);
        }
        self.last_template = None;
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
            denoise: self.denoise,
            mix: self.speech_mix,
            gate_dbfs: self.gate_dbfs,
            agc: self.agc,
            strong_denoise: self.strong_denoise,
            volume: self.output_volume,
            cues: self.cues,
            // 進房時麥克風預設關閉（使用者要求，2026-11-16）
            start_muted: true,
            ..Default::default()
        }
    }

    /// 麥克風前端處理設定（麥克風測試用）
    pub fn frontend_cfg(&self) -> session::FrontendCfg {
        session::FrontendCfg {
            denoise: self.denoise,
            speech_mix: self.speech_mix,
            gate_dbfs: self.gate_dbfs,
            agc: self.agc,
            strong: self.strong_denoise,
        }
    }

    /// 某頻道設定的 PTT 鍵
    pub fn ptt_key(&self, channel: &str) -> Option<session::Hotkey> {
        self.ptt_keys.get(channel).and_then(|k| k.parse().ok())
    }

    /// 頻道改名：以名稱存的 PTT 鍵與頻道設定檔跟著改（新名稱已經有設定就不覆蓋）
    pub fn rename_channel(&mut self, old: &str, new: &str) {
        if old == new {
            return;
        }
        if !self.ptt_keys.contains_key(new)
            && let Some(k) = self.ptt_keys.remove(old)
        {
            self.ptt_keys.insert(new.to_string(), k);
        }
        for p in &mut self.presets {
            if p.channels.iter().any(|c| c.channel == new) {
                continue;
            }
            for c in p.channels.iter_mut().filter(|c| c.channel == old) {
                c.channel = new.to_string();
            }
        }
    }

    /// 比對上次看到的頻道名稱（`known`，以頻道 ID 對應），有改名就 [`Self::rename_channel`]，再更新 `known`。
    /// 不管是誰改的名（房主改名後別人也會收到），存的設定都跟著走
    pub fn follow_renames<'a>(&mut self, known: &mut BTreeMap<u32, String>, current: impl IntoIterator<Item = (u32, &'a str)>) {
        let current: BTreeMap<u32, String> = current.into_iter().map(|(id, n)| (id, n.to_string())).collect();
        for (id, name) in &current {
            if let Some(old) = known.get(id) {
                self.rename_channel(old, name);
            }
        }
        *known = current;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::{Preset, PresetChannel};

    fn preset(name: &str, chans: &[&str]) -> Preset {
        Preset {
            name: name.into(),
            channels: chans.iter().map(|c| PresetChannel { channel: c.to_string(), ..Default::default() }).collect(),
        }
    }

    #[test]
    fn old_templates_become_presets() {
        let mut s: Settings = toml::from_str(
            "last_template = \"公會戰\"
             [[presets]]
name = \"副本\"
[[presets.channels]]
channel = \"小隊\"
             [[room_templates]]
name = \"公會戰\"
channels = [\"指揮部\", \"一隊\"]
             [[room_templates]]
name = \"副本\"
channels = [\"二隊\"]
",
        )
        .unwrap();
        s.migrate_templates();
        let names: Vec<&str> = s.presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["副本", "公會戰", "副本（模板）"]);
        assert_eq!(s.host_preset.as_deref(), Some("公會戰"));
        assert_eq!(s.presets[1].joined_count(), 1, "只在大廳");
        // 不再寫回舊欄位
        let out = toml::to_string_pretty(&s).unwrap();
        assert!(!out.contains("room_templates") && !out.contains("last_template"));
    }

    #[test]
    fn renamed_channel_keeps_ptt_key_and_presets() {
        let mut s = Settings::default();
        s.ptt_keys.insert("小隊".into(), "Mouse4".into());
        s.ptt_keys.insert("指揮".into(), "F1".into());
        s.presets = vec![preset("攻擊", &["小隊", "大廳"]), preset("衝突", &["小隊", "一隊"])];
        let mut known = BTreeMap::new();

        // 第一次只是記下來
        s.follow_renames(&mut known, [(1, "大廳"), (2, "小隊"), (3, "指揮")]);
        assert_eq!(s.ptt_keys.get("小隊").map(String::as_str), Some("Mouse4"));

        // 頻道 2 改名為「一隊」；頻道 3 改名為已經有鍵的名稱就不覆蓋
        s.ptt_keys.insert("二隊".into(), "F2".into());
        s.follow_renames(&mut known, [(1, "大廳"), (2, "一隊"), (3, "二隊")]);
        assert_eq!(s.ptt_keys.get("一隊").map(String::as_str), Some("Mouse4"));
        assert!(!s.ptt_keys.contains_key("小隊"));
        assert_eq!(s.ptt_keys.get("二隊").map(String::as_str), Some("F2"));
        assert_eq!(s.ptt_keys.get("指揮").map(String::as_str), Some("F1"));
        assert_eq!(s.presets[0].channels[0].channel, "一隊");
        assert_eq!(s.presets[0].channels[1].channel, "大廳");
        // 這個設定檔已經有「一隊」，不要變成兩個同名頻道
        assert_eq!(s.presets[1].channels[0].channel, "小隊");
        assert_eq!(known.get(&2).map(String::as_str), Some("一隊"));

        // 刪掉的頻道從 known 移除，之後同 ID 出現不會被當成改名
        s.follow_renames(&mut known, [(1, "大廳")]);
        s.follow_renames(&mut known, [(1, "大廳"), (2, "新頻道")]);
        assert_eq!(s.ptt_keys.get("一隊").map(String::as_str), Some("Mouse4"));
    }
}
