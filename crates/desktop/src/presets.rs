//! 頻道設定檔（Profile）：房間有哪些頻道，以及「我在各頻道的收聽／說話／混音設定」存成一組，一鍵切換。
//!
//! 頻道 ID 每次建房都不同，所以跟 PTT 鍵一樣用**頻道名稱**對應。
//! 套用時：設定檔裡標為加入的頻道照設定加入，其他頻道離開；
//! 設定檔裡有、房間沒有的頻道：我是房主就建立（開房時選設定檔也是這樣建好頻道），不是房主就略過。

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
    /// 我有沒有加入（false = 只記得房間有這個頻道；舊版設定檔只存加入的頻道，所以預設 true）
    pub joined: bool,
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
            joined: true,
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
        "ptt" => TalkMode::Ptt,
        // 「一直開麥」已從介面拿掉：舊設定檔的 open 也改成偵測說話
        _ => TalkMode::Voice,
    }
}

impl PresetChannel {
    fn from_cfg(channel: &str, c: &ChannelCfg) -> Self {
        Self {
            channel: channel.into(),
            joined: true,
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

/// 房間裡的頻道（名稱, 我的設定；None = 沒加入）→ 設定檔；重複的名稱只存第一個
pub fn capture<'a>(name: &str, room: impl IntoIterator<Item = (&'a str, Option<&'a ChannelCfg>)>) -> Preset {
    let mut channels: Vec<PresetChannel> = Vec::new();
    for (n, c) in room {
        if channels.iter().any(|x| x.channel == n) {
            continue;
        }
        channels.push(match c {
            Some(c) => PresetChannel::from_cfg(n, c),
            None => PresetChannel { channel: n.into(), joined: false, ..Default::default() },
        });
    }
    Preset { name: name.into(), channels }
}

impl Preset {
    /// 加入的頻道數
    pub fn joined_count(&self) -> usize {
        self.channels.iter().filter(|c| c.joined).count()
    }
}

/// 設定檔裡有、房間裡沒有的頻道名稱（依設定檔順序；房主套用時要建立的）
pub fn missing<'a>(preset: &Preset, room: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let room: Vec<&str> = room.into_iter().collect();
    preset.channels.iter().filter(|c| !room.contains(&c.channel.as_str())).map(|c| c.channel.clone()).collect()
}

/// 套用計畫：房間裡每個頻道（ID, 名稱）要變成什麼設定（None = 離開）
pub fn plan<'a>(preset: &Preset, room: impl IntoIterator<Item = (ChannelId, &'a str)>) -> Vec<(ChannelId, Option<ChannelCfg>)> {
    room.into_iter()
        .map(|(id, name)| {
            let cfg = preset.channels.iter().find(|c| c.channel == name).filter(|c| c.joined).map(PresetChannel::to_cfg);
            (id, cfg)
        })
        .collect()
}

/// 改名：新名稱不能是空的，也不能和別的設定檔重複
pub fn rename(presets: &mut [Preset], old: &str, new: &str) -> Result<(), &'static str> {
    let new = new.trim();
    if new.is_empty() {
        return Err("名稱不能是空的");
    }
    if new != old && presets.iter().any(|p| p.name == new) {
        return Err("已經有同名的設定檔");
    }
    let p = presets.iter_mut().find(|p| p.name == old).ok_or("找不到這個設定檔")?;
    p.name = new.to_string();
    Ok(())
}

/// v0.8.1～0.8.3 的「頻道模板」（只存頻道名稱，不含大廳）；讀舊設定時轉成設定檔
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoomTemplate {
    pub name: String,
    pub channels: Vec<String>,
}

impl RoomTemplate {
    /// 開房用模板的效果：只在大廳，其他頻道建好但不加入
    pub fn into_preset(self, lobby: &str) -> Preset {
        let mut channels = vec![PresetChannel { channel: lobby.into(), ..Default::default() }];
        for c in self.channels {
            if !channels.iter().any(|x| x.channel == c) {
                channels.push(PresetChannel { channel: c, joined: false, ..Default::default() });
            }
        }
        Preset { name: self.name, channels }
    }
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
        let p = capture("公會戰指揮", [("指揮部", Some(&a)), ("大廳", Some(&b)), ("二隊", None)]);
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
        assert!(!back.presets[0].channels[2].joined);
        assert_eq!(p.joined_count(), 2);
    }

    #[test]
    fn old_presets_without_joined_field_are_joined() {
        let p: Preset = toml::from_str("name = \"舊\"
[[channels]]
channel = \"小隊\"
talk = \"ptt\"
").unwrap();
        assert!(p.channels[0].joined);
    }

    #[test]
    fn plan_joins_leaves_and_reports_missing() {
        let p = capture(
            "副本",
            [("小隊", Some(&cfg(TalkMode::Voice, None))), ("不存在", Some(&cfg(TalkMode::Open, None))), ("旁觀", None), ("小隊", None)],
        );
        assert_eq!(p.channels.len(), 3, "重複的名稱只存第一個");
        let room = [(0, "大廳"), (3, "小隊"), (4, "旁觀")];
        let changes = plan(&p, room);
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0], (0, None), "設定檔沒有大廳 → 離開");
        assert_eq!(changes[1].0, 3);
        assert_eq!(changes[1].1.unwrap().talk, TalkMode::Voice);
        assert_eq!(changes[2], (4, None), "記得有這個頻道但沒加入 → 離開");
        assert_eq!(missing(&p, room.iter().map(|r| r.1)), ["不存在"]);
    }

    #[test]
    fn old_template_becomes_preset_in_lobby_only() {
        let t = RoomTemplate { name: "公會戰".into(), channels: vec!["指揮部".into(), "大廳".into(), "一隊".into()] };
        let p = t.into_preset("大廳");
        let names: Vec<&str> = p.channels.iter().map(|c| c.channel.as_str()).collect();
        assert_eq!(names, ["大廳", "指揮部", "一隊"]);
        assert_eq!(p.joined_count(), 1);
        assert_eq!(missing(&p, ["大廳"]), ["指揮部", "一隊"]);
    }

    #[test]
    fn rename_rejects_duplicates() {
        let mut ps = vec![capture("甲", [("一隊", None)]), capture("乙", [("二隊", None)])];
        assert!(rename(&mut ps, "甲", "乙").is_err());
        assert!(rename(&mut ps, "甲", "  ").is_err());
        assert!(rename(&mut ps, "甲", "甲").is_ok());
        assert!(rename(&mut ps, "甲", " 公會戰 ").is_ok());
        assert_eq!(ps[0].name, "公會戰");
        assert_eq!(ps[0].channels[0].channel, "一隊");
    }
}
