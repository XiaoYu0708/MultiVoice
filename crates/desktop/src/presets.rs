//! 頻道設定檔（Profile）：把「我在各頻道的收聽／說話／混音設定」存成一組，一鍵切換。
//!
//! 頻道 ID 每次建房都不同，所以跟 PTT 鍵一樣用**頻道名稱**對應。
//! 套用時：設定檔裡有的頻道照設定加入；沒有的頻道離開；設定檔裡有、房間沒有的頻道略過。
//!
//! 頻道模板（[`RoomTemplate`]）：房間有哪些頻道（只存名稱），開房時選一個就自動建好。

use protocol::ChannelId;
use serde::{Deserialize, Serialize};
use session::{ChannelCfg, ChannelMix, TalkMode};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub channels: Vec<PresetChannel>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PresetChannel {
    /// 頻道名稱
    pub channel: String,
    pub listen: bool,
    /// "off" / "voice" / "open" / "ptt"
    pub talk: String,
    /// 按鍵說話用的鍵（例如 "Mouse4"）
    pub ptt_key: Option<String>,
    pub volume: f32,
    pub pan: f32,
    pub priority: bool,
}

impl Default for PresetChannel {
    fn default() -> Self {
        Self {
            channel: String::new(),
            listen: true,
            talk: "voice".into(),
            ptt_key: None,
            volume: 1.0,
            pan: 0.0,
            priority: false,
        }
    }
}

fn talk_str(t: TalkMode) -> &'static str {
    match t {
        TalkMode::Off => "off",
        TalkMode::Voice => "voice",
        TalkMode::Open => "open",
        TalkMode::Ptt => "ptt",
    }
}

fn parse_talk(s: &str) -> TalkMode {
    match s {
        "off" => TalkMode::Off,
        "open" => TalkMode::Open,
        "ptt" => TalkMode::Ptt,
        _ => TalkMode::Voice,
    }
}

impl PresetChannel {
    fn from_cfg(channel: &str, c: &ChannelCfg) -> Self {
        Self {
            channel: channel.into(),
            listen: c.listen,
            talk: talk_str(c.talk).into(),
            ptt_key: c.ptt_key.map(|k| k.to_string()),
            volume: c.mix.volume,
            pan: c.mix.pan,
            priority: c.mix.priority,
        }
    }

    fn to_cfg(&self) -> ChannelCfg {
        ChannelCfg {
            listen: self.listen,
            talk: parse_talk(&self.talk),
            mix: ChannelMix { volume: self.volume, pan: self.pan, priority: self.priority },
            ptt_key: self.ptt_key.as_deref().and_then(|k| k.parse().ok()),
        }
    }
}

/// 目前已加入的頻道（名稱, 我的設定）→ 設定檔
pub fn capture<'a>(name: &str, joined: impl IntoIterator<Item = (&'a str, &'a ChannelCfg)>) -> Preset {
    Preset { name: name.into(), channels: joined.into_iter().map(|(n, c)| PresetChannel::from_cfg(n, c)).collect() }
}

/// 套用計畫：房間裡每個頻道（ID, 名稱）要變成什麼設定（None = 離開）。
/// 第二個回傳值是設定檔裡有、但房間裡找不到的頻道名稱
pub fn plan<'a>(
    preset: &Preset,
    room: impl IntoIterator<Item = (ChannelId, &'a str)>,
) -> (Vec<(ChannelId, Option<ChannelCfg>)>, Vec<String>) {
    let room: Vec<(ChannelId, &str)> = room.into_iter().collect();
    let changes = room
        .iter()
        .map(|&(id, name)| (id, preset.channels.iter().find(|c| c.channel == name).map(PresetChannel::to_cfg)))
        .collect();
    let missing = preset
        .channels
        .iter()
        .filter(|c| !room.iter().any(|(_, n)| *n == c.channel))
        .map(|c| c.channel.clone())
        .collect();
    (changes, missing)
}

/// 頻道模板：開房時自動建立的頻道（不含大廳，大廳建房時就有）
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoomTemplate {
    pub name: String,
    pub channels: Vec<String>,
}

/// 目前房間的頻道名稱（依順序）→ 模板；大廳與重複的名稱不存
pub fn capture_template<'a>(name: &str, lobby: &str, room: impl IntoIterator<Item = &'a str>) -> RoomTemplate {
    let mut channels: Vec<String> = Vec::new();
    for n in room {
        if n != lobby && !channels.iter().any(|c| c == n) {
            channels.push(n.to_string());
        }
    }
    RoomTemplate { name: name.into(), channels }
}

/// 套用模板：房間裡還沒有的頻道名稱（依模板順序）
pub fn template_missing<'a>(t: &RoomTemplate, room: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let room: Vec<&str> = room.into_iter().collect();
    t.channels.iter().filter(|c| !room.contains(&c.as_str())).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(talk: TalkMode, key: Option<&str>) -> ChannelCfg {
        ChannelCfg {
            listen: true,
            talk,
            mix: ChannelMix { volume: 0.5, pan: -1.0, priority: true },
            ptt_key: key.map(|k| k.parse().unwrap()),
        }
    }

    #[test]
    fn roundtrip_through_toml() {
        let a = cfg(TalkMode::Ptt, Some("Ctrl+Mouse4"));
        let b = cfg(TalkMode::Off, None);
        let p = capture("公會戰指揮", [("指揮部", &a), ("大廳", &b)]);
        // 跟設定檔一起存成 TOML 再讀回來
        #[derive(Serialize, Deserialize)]
        struct W {
            presets: Vec<Preset>,
        }
        let s = toml::to_string_pretty(&W { presets: vec![p.clone()] }).unwrap();
        let back: W = toml::from_str(&s).unwrap();
        assert_eq!(back.presets[0], p);
        assert_eq!(back.presets[0].channels[0].to_cfg(), a);
        assert_eq!(back.presets[0].channels[1].to_cfg(), b);
    }

    #[test]
    fn plan_joins_leaves_and_reports_missing() {
        let p = capture("副本", [("小隊", &cfg(TalkMode::Voice, None)), ("不存在", &cfg(TalkMode::Open, None))]);
        let (changes, missing) = plan(&p, [(0, "大廳"), (3, "小隊")]);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0], (0, None), "設定檔沒有大廳 → 離開");
        assert_eq!(changes[1].0, 3);
        assert_eq!(changes[1].1.unwrap().talk, TalkMode::Voice);
        assert_eq!(missing, vec!["不存在".to_string()]);
    }

    #[test]
    fn template_skips_lobby_and_existing() {
        let t = capture_template("公會戰", "大廳", ["大廳", "指揮部", "一隊", "指揮部", "二隊"]);
        assert_eq!(t.channels, ["指揮部", "一隊", "二隊"]);
        assert_eq!(template_missing(&t, ["大廳", "一隊"]), ["指揮部", "二隊"]);
        assert!(template_missing(&t, ["大廳", "指揮部", "一隊", "二隊"]).is_empty());
    }
}
