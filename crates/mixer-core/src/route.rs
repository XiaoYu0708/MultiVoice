//! 每頻道混音規則（PLAN.md §2.4）：
//! - 一位說話者可能同時經由多個頻道被聽到（只會收到一份聲音）：
//!   增益取這些頻道中**最大**的頻道音量，聲道定位跟著那個頻道
//! - 優先頻道有人說話時，其他頻道的聲音壓低 [`DUCK_GAIN`]（約 -12 dB）

/// 某個頻道在本機的混音設定
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelMix {
    /// 0.0 = 這個頻道靜音
    pub volume: f32,
    /// -1.0（左）..0.0（中）..1.0（右）
    pub pan: f32,
    pub priority: bool,
}

impl Default for ChannelMix {
    fn default() -> Self {
        Self { volume: 1.0, pan: 0.0, priority: false }
    }
}

/// -12 dB
pub const DUCK_GAIN: f32 = 0.25;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Route {
    pub gain: f32,
    pub pan: f32,
    /// 經由優先頻道聽到
    pub priority: bool,
}

/// 依「經由哪些頻道」決定這位說話者的增益與定位；全部靜音則回傳 None（不必解碼混音）
pub fn route<'a>(via: impl IntoIterator<Item = &'a ChannelMix>) -> Option<Route> {
    let mut best: Option<&ChannelMix> = None;
    let mut priority = false;
    for c in via {
        if c.volume <= 0.0 {
            continue;
        }
        priority |= c.priority;
        if best.is_none_or(|b| c.volume > b.volume) {
            best = Some(c);
        }
    }
    best.map(|b| Route { gain: b.volume, pan: b.pan, priority })
}

/// 平衡式定位：置中時左右都是 1（不會變小聲），往左推只降低右聲道，反之亦然
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let p = pan.clamp(-1.0, 1.0);
    ((1.0 - p).min(1.0), (1.0 + p).min(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn ch(volume: f32, pan: f32, priority: bool) -> ChannelMix {
        ChannelMix { volume, pan, priority }
    }

    #[test]
    fn loudest_channel_wins_and_carries_pan() {
        let a = ch(0.5, -1.0, false);
        let b = ch(0.9, 0.5, false);
        assert_eq!(route([&a, &b]), Some(Route { gain: 0.9, pan: 0.5, priority: false }));
    }

    #[test]
    fn muted_channels_are_ignored() {
        let muted = ch(0.0, 0.0, true);
        let quiet = ch(0.3, 0.0, false);
        assert_eq!(route([&muted]), None, "只經由靜音頻道 → 不播放");
        assert_eq!(route([&muted, &quiet]), Some(Route { gain: 0.3, pan: 0.0, priority: false }), "靜音的優先頻道不算");
    }

    #[test]
    fn priority_flag_from_any_audible_channel() {
        let p = ch(0.2, 0.0, true);
        let loud = ch(1.0, 1.0, false);
        let r = route([&p, &loud]).unwrap();
        assert!(r.priority);
        assert_eq!((r.gain, r.pan), (1.0, 1.0));
    }

    #[test]
    fn balance_pan() {
        assert_eq!(pan_gains(0.0), (1.0, 1.0));
        assert_eq!(pan_gains(-1.0), (1.0, 0.0));
        assert_eq!(pan_gains(0.5), (0.5, 1.0));
        assert_eq!(pan_gains(9.0), (0.0, 1.0));
    }
}
