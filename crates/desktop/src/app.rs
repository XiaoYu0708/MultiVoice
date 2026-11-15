//! 主畫面：大廳（建立／加入房間、音訊設定）與房間（成員、說話指示、靜音／拒聽）。

use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use protocol::DEFAULT_CHANNEL;
use session::{ChannelCfg, Connected, Relays, Session, Snapshot, TalkMode, hotkeys};

use crate::settings::Settings;
use crate::style::{AMBER, BLUE, GREEN, RED};

pub struct App {
    rt: tokio::runtime::Runtime,
    settings: Settings,
    saved: Settings,
    /// 身分設定檔（同一台電腦開兩個實例測試時用 `--profile 名稱`）
    profile: String,
    ticket_input: String,
    pending: Option<(mpsc::Receiver<anyhow::Result<Connected>>, &'static str)>,
    session: Option<Session>,
    error: Option<String>,
    inputs: Vec<String>,
    outputs: Vec<String>,
    copied_at: Option<Instant>,
    /// 測試用：啟動後自動建房／加入
    auto: Option<Auto>,
    start_deafened: bool,
    /// 目前套用到工作階段的音訊設定（用來偵測使用者在房間裡改了設定）
    applied: Option<session::AudioOpts>,
    new_channel: String,
    /// 正在改名的頻道與輸入中的名稱
    renaming: Option<(u32, String)>,
    /// 正在錄製哪個按鍵
    recording: Option<KeyTarget>,
    /// 全域按鍵監聽啟動失敗的原因
    hotkey_err: Option<String>,
    /// 由本視窗補送給 hotkeys、目前按住的鍵（視窗失去焦點時要放開）
    local_down: Vec<hotkeys::Trigger>,
    /// 按了一次「刪除」、等待確認的頻道與時間
    confirm_delete: Option<(u32, Instant)>,
}

/// 要設定的按鍵
#[derive(Clone, Debug, PartialEq)]
enum KeyTarget {
    /// 頻道的 PTT 鍵（頻道 ID、名稱）
    Channel(u32, String),
    Broadcast,
    Mute,
}

enum Auto {
    Host,
    Join(String),
}

impl App {
    /// 命令列參數（測試用）：`--profile 名稱`、`--auto-host`、`--auto-join 邀請票`、`--deafen`
    pub fn new(rt: tokio::runtime::Runtime) -> Self {
        let mut args = std::env::args().skip(1);
        let mut profile = "default".to_string();
        let mut auto = None;
        let mut start_deafened = false;
        while let Some(a) = args.next() {
            match a.as_str() {
                "--profile" => profile = args.next().unwrap_or(profile),
                "--auto-host" => auto = Some(Auto::Host),
                "--auto-join" => auto = args.next().map(Auto::Join),
                "--deafen" => start_deafened = true,
                _ => {}
            }
        }
        let settings = Settings::load(&profile);
        Self {
            auto,
            start_deafened,
            rt,
            ticket_input: settings.last_ticket.clone(),
            saved: settings.clone(),
            settings,
            profile,
            pending: None,
            session: None,
            error: None,
            inputs: audio::device::list_devices(true),
            outputs: audio::device::list_devices(false),
            copied_at: None,
            applied: None,
            new_channel: String::new(),
            renaming: None,
            recording: None,
            // 大廳就要能錄製按鍵，所以一開始就啟動監聽
            hotkey_err: hotkeys::start().err(),
            local_down: Vec::new(),
            confirm_delete: None,
        }
    }

    /// 本視窗在前景時，全域 hook 收不到大部分鍵盤事件（見 `hotkeys::feed`），
    /// 把 egui 收到的按鍵與修飾鍵變化補送過去：錄製按鍵、PTT、靜音鍵在本視窗裡也能用
    fn feed_local_keys(&mut self, ctx: &egui::Context) {
        // 在文字框打字時不算按鍵（否則輸入頻道名稱時打到 PTT 鍵的字母會開麥）
        let typing = ctx.egui_wants_keyboard_input() && self.recording.is_none();
        let (focused, keys, mods) = ctx.input(|i| {
            let keys: Vec<(hotkeys::Trigger, bool)> = i
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key, physical_key, pressed, repeat: false, .. } => {
                        egui_trigger(physical_key.unwrap_or(*key)).map(|t| (t, *pressed))
                    }
                    _ => None,
                })
                .collect();
            (i.focused, keys, i.modifiers)
        });
        let mut want: Vec<(hotkeys::Trigger, bool)> = Vec::new();
        if focused && !typing {
            for (vk, on) in [(hotkeys::VK_CONTROL, mods.ctrl), (hotkeys::VK_MENU, mods.alt), (hotkeys::VK_SHIFT, mods.shift)] {
                let t = hotkeys::Trigger::Key(vk);
                if on != self.local_down.contains(&t) {
                    want.push((t, on));
                }
            }
            want.extend(keys);
        } else {
            // 失去焦點：之前補送的按下全部放開，避免卡住
            want.extend(self.local_down.iter().map(|&t| (t, false)));
        }
        for (t, down) in want {
            hotkeys::feed(t, down);
            self.local_down.retain(|&x| x != t);
            if down {
                self.local_down.push(t);
            }
        }
    }

    fn start_recording(&mut self, t: KeyTarget) {
        hotkeys::start_recording();
        self.recording = Some(t);
    }

    /// 錄製完成：存進設定，頻道的 PTT 鍵立即套用
    fn poll_recording(&mut self, snap: Option<&Snapshot>) {
        if self.recording.is_none() {
            return;
        }
        let Some(r) = hotkeys::take_recorded() else {
            if !hotkeys::is_recording() {
                self.recording = None;
            }
            return;
        };
        let t = self.recording.take().unwrap();
        let hotkeys::Recorded::Key(k) = r else { return };
        match t {
            KeyTarget::Broadcast => self.settings.broadcast_key = Some(k.to_string()),
            KeyTarget::Mute => self.settings.mute_key = Some(k.to_string()),
            KeyTarget::Channel(id, name) => {
                self.settings.ptt_keys.insert(name, k.to_string());
                let mine = snap.and_then(|s| s.channels.iter().find(|c| c.id == id)).and_then(|c| c.mine);
                if let (Some(s), Some(cfg)) = (&self.session, mine) {
                    let _ = s.set_channel(id, Some(ChannelCfg { talk: TalkMode::Ptt, ptt_key: Some(k), ..cfg }));
                }
            }
        }
    }



    fn start_host(&mut self) {
        let (tx, rx) = mpsc::channel();
        let (profile, name) = (self.profile.clone(), self.settings.name.trim().to_string());
        self.rt.spawn(async move {
            let _ = tx.send(session::host_room(&profile, &name, Relays::Default).await);
        });
        self.pending = Some((rx, "建立房間中…"));
        self.error = None;
    }

    fn start_join(&mut self) {
        let ticket = self.ticket_input.split_whitespace().collect::<String>();
        self.settings.last_ticket = ticket.clone();
        let (tx, rx) = mpsc::channel();
        let (profile, name) = (self.profile.clone(), self.settings.name.trim().to_string());
        self.rt.spawn(async move {
            let _ = tx.send(session::join_room(&ticket, &profile, &name, Relays::Default).await);
        });
        self.pending = Some((rx, "加入房間中…"));
        self.error = None;
    }

    /// 背景連線完成後，在 UI 執行緒啟動音訊（cpal 串流不能跨執行緒）
    fn poll_pending(&mut self) {
        let Some((rx, _)) = &self.pending else { return };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(anyhow::anyhow!("連線工作意外中止")),
        };
        self.pending = None;
        match result.and_then(|c| Session::start(c, &self.settings.audio_opts(), self.rt.handle())) {
            Ok(s) => {
                if self.start_deafened {
                    s.set_deafened(true);
                }
                // 自動加入的大廳也套用存好的 PTT 鍵
                if let Some(lobby) = s.snapshot().channels.iter().find(|c| c.id == DEFAULT_CHANNEL)
                    && self.settings.ptt_keys.contains_key(&lobby.name)
                {
                    let _ = s.set_channel(DEFAULT_CHANNEL, Some(join_cfg(&self.settings, &s, &lobby.name)));
                }
                self.session = Some(s);
                self.applied = Some(self.settings.audio_opts());
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    fn leave(&mut self) {
        if let Some(s) = self.session.take() {
            self.rt.spawn(s.leave());
        }
    }

    fn lobby(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.label("你的名稱");
        ui.add(egui::TextEdit::singleline(&mut self.settings.name).hint_text("其他人看到的名字").desired_width(f32::INFINITY));
        let name_ok = !self.settings.name.trim().is_empty();

        ui.add_space(12.0);
        let busy = self.pending.is_some();
        let full_w = ui.available_width();
        let big = |t: &str| egui::Button::new(RichText::new(t).size(17.0)).min_size(egui::vec2(full_w, 38.0));
        if ui.add_enabled(name_ok && !busy, big("建立房間（我當房主）")).clicked() {
            self.start_host();
        }

        ui.add_space(14.0);
        ui.weak("或加入別人的房間");
        ui.add(
            egui::TextEdit::multiline(&mut self.ticket_input)
                .hint_text("貼上 mv1 開頭的邀請票（Ctrl+V）")
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        let ticket_ok = self.ticket_input.trim().starts_with("mv1");
        if ui.add_enabled(name_ok && ticket_ok && !busy, big("加入房間")).clicked() {
            self.start_join();
        }

        if let Some((_, msg)) = &self.pending {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(*msg);
            });
        }
        if let Some(e) = &self.error {
            ui.add_space(8.0);
            ui.colored_label(RED, e);
        }

        ui.add_space(12.0);
        egui::CollapsingHeader::new("音訊設定").default_open(false).show(ui, |ui| self.audio_settings(ui));
        if self.profile != "default" {
            ui.weak(format!("身分設定檔：{}", self.profile));
        }
    }

    fn audio_settings(&mut self, ui: &mut egui::Ui) {
        let device_combo = |ui: &mut egui::Ui, label: &str, list: &[String], value: &mut Option<String>| {
            ui.label(label);
            egui::ComboBox::from_id_salt(label)
                .width(ui.available_width())
                .selected_text(value.clone().unwrap_or_else(|| "系統預設".into()))
                .show_ui(ui, |ui| {
                    ui.selectable_value(value, None, "系統預設");
                    for d in list {
                        ui.selectable_value(value, Some(d.clone()), d);
                    }
                });
        };
        device_combo(ui, "麥克風", &self.inputs, &mut self.settings.input_device);
        device_combo(ui, "耳機／喇叭", &self.outputs, &mut self.settings.output_device);
        if ui.small_button("重新整理裝置清單").clicked() {
            self.inputs = audio::device::list_devices(true);
            self.outputs = audio::device::list_devices(false);
        }

        ui.add_space(6.0);
        ui.label("加入頻道時預設的說話方式");
        ui.radio_value(&mut self.settings.open_mic, false, "偵測到說話才送出（建議）");
        ui.radio_value(&mut self.settings.open_mic, true, "一直開麥");
        ui.weak("每個頻道可以在頻道列表裡個別調整");

        ui.add_space(6.0);
        ui.checkbox(&mut self.settings.denoise, "降噪（消除風扇、鍵盤等背景聲）");
        if self.settings.denoise {
            ui.add(egui::Slider::new(&mut self.settings.speech_mix, 0.0..=0.6).text("小聲講話保留"));
            ui.weak("調高：小聲的字更清楚，但講話時會多一點背景聲");
        }

        ui.add_space(8.0);
        ui.label(RichText::new("全域按鍵（遊戲在前景時也有效）").strong());
        for (target, label) in [(KeyTarget::Broadcast, "廣播鍵（按住對所有頻道說話）"), (KeyTarget::Mute, "靜音鍵（按一下切換）")] {
            ui.horizontal(|ui| {
                ui.label(label);
                let cur = match target {
                    KeyTarget::Broadcast => self.settings.broadcast_key.clone(),
                    _ => self.settings.mute_key.clone(),
                };
                if key_button(ui, &self.recording, &target, cur.as_deref()) {
                    self.start_recording(target.clone());
                }
                if cur.is_some() && ui.small_button("清除").clicked() {
                    match target {
                        KeyTarget::Broadcast => self.settings.broadcast_key = None,
                        _ => self.settings.mute_key = None,
                    }
                }
            });
        }
        ui.checkbox(&mut self.settings.cues, "開／關麥提示音");
        if let Some(e) = &self.hotkey_err {
            ui.colored_label(RED, format!("全域按鍵無法使用：{e}"));
        } else if !hotkeys::is_admin() {
            ui.weak("如果遊戲以系統管理員身分執行，這裡也要以管理員身分執行，否則遊戲在前景時收不到按鍵");
            if ui.small_button("以系統管理員身分重新啟動").clicked() && hotkeys::relaunch_as_admin() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn room(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        // 房間資訊與邀請票
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                // 不用 emoji：系統中文字型沒有 emoji，會顯示成方框
                if snap.is_host {
                    ui.label(RichText::new("你是房主").strong().color(AMBER));
                } else {
                    ui.label(RichText::new("已加入房間").strong());
                }
                ui.weak(format!("{} 人", snap.members.len() + 1));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let copied = self.copied_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2));
                    if ui.button(if copied { "已複製" } else { "複製邀請票" }).clicked() {
                        ui.ctx().copy_text(snap.ticket.clone());
                        self.copied_at = Some(Instant::now());
                    }
                });
            });
            ui.weak("把邀請票傳給朋友，他們貼上就能加入");
        });

        ui.add_space(6.0);
        // 自己：麥克風音量與目前送出的頻道
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let color = if snap.muted {
                    RED
                } else if snap.tx_active {
                    GREEN
                } else {
                    Color32::GRAY
                };
                dot(ui, color, snap.tx_active && !snap.muted);
                ui.label(RichText::new(format!("{}（我）", snap.me.name)).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if snap.muted {
                        ui.colored_label(RED, "已靜音");
                    } else if snap.tx_active {
                        ui.colored_label(GREEN, "送出中");
                    }
                });
            });
            let db = 20.0 * snap.mic_peak.max(1e-6).log10();
            let frac = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
            let bar_color = if snap.muted { Color32::GRAY } else if db > -3.0 { AMBER } else { BLUE };
            ui.add(egui::ProgressBar::new(frac).desired_height(6.0).fill(bar_color));
            if snap.talking_to.is_empty() {
                ui.weak("目前送出：—");
            } else {
                ui.colored_label(GREEN, format!("目前送出：{}", snap.talking_to.join("、")));
            }
        });

        ui.add_space(6.0);
        ui.label(RichText::new("頻道").strong());
        for ch in &snap.channels {
            self.channel_card(ui, ch, snap.is_host);
        }
        // 新增頻道
        ui.horizontal(|ui| {
            let w = ui.available_width() - 70.0;
            let r = ui.add(egui::TextEdit::singleline(&mut self.new_channel).hint_text("新頻道名稱").desired_width(w));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let ok = !self.new_channel.trim().is_empty();
            if (ui.add_enabled(ok, egui::Button::new("新增")).clicked() || (enter && ok))
                && let Some(s) = &self.session
            {
                s.create_channel(self.new_channel.trim());
                self.new_channel.clear();
            }
        });

        if let Some(why) = &snap.disconnected {
            ui.add_space(6.0);
            ui.colored_label(RED, format!("連線中斷：{why}"));
        }

        ui.add_space(6.0);
        egui::CollapsingHeader::new("語音設定").default_open(false).show(ui, |ui| {
            ui.add(egui::Slider::new(&mut self.settings.output_volume, 0.0..=2.0).text("輸出音量"));
            ui.add_space(4.0);
            self.audio_settings(ui);
        });
        // 設定有變就套用：降噪、音量立即生效；換裝置會重啟音訊（不中斷連線）
        let opts = self.settings.audio_opts();
        if self.applied.as_ref() != Some(&opts) {
            if let Some(s) = self.session.as_mut() {
                self.error = s.apply_audio(&opts).err().map(|e| format!("{e:#}"));
                // 失敗時 session 已退回原本的裝置；把畫面上的選擇也改回去，避免每一幀重試
                if self.error.is_some() {
                    self.settings.input_device = self.applied.as_ref().and_then(|o| o.input.clone());
                    self.settings.output_device = self.applied.as_ref().and_then(|o| o.output.clone());
                }
            }
            self.applied = Some(self.settings.audio_opts());
        }
        if let Some(e) = &self.error {
            ui.colored_label(RED, e);
        }

        egui::CollapsingHeader::new("最近事件").default_open(false).show(ui, |ui| {
            for line in snap.log.iter().rev().take(10) {
                ui.weak(line);
            }
        });
    }

    /// 一個頻道：名稱（右鍵改名／刪除）、加入／離開、我的說話與收聽設定、頻道成員
    /// `is_host`：房主才顯示改名／刪除
    fn channel_card(&mut self, ui: &mut egui::Ui, ch: &session::ChannelView, is_host: bool) {
        let Some(s) = &self.session else { return };
        let joined = ch.mine.is_some();
        // 閉包裡借著 session，不能呼叫 &mut self 的方法：先記下，結束後再開始錄製
        let mut record: Option<KeyTarget> = None;
        let fill = if joined { ui.visuals().faint_bg_color } else { ui.visuals().panel_fill };
        egui::Frame::group(ui.style()).fill(fill).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if self.renaming.as_ref().is_some_and(|(id, _)| *id == ch.id) {
                    let (_, text) = self.renaming.as_mut().unwrap();
                    let r = ui.add(egui::TextEdit::singleline(text).desired_width(150.0));
                    r.request_focus();
                    let ok = !text.trim().is_empty();
                    if ok && (ui.small_button("確定").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                        let new = text.trim().to_string();
                        // 這個頻道存的 PTT 鍵跟著新名稱走
                        if let Some(k) = self.settings.ptt_keys.remove(&ch.name) {
                            self.settings.ptt_keys.insert(new.clone(), k);
                        }
                        s.rename_channel(ch.id, &new);
                        self.renaming = None;
                    } else if ui.small_button("取消").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.renaming = None;
                    }
                } else {
                    ui.label(RichText::new(&ch.name).strong());
                    ui.weak(format!("{} 人", ch.members.len()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // 房主管理：刪除要按兩次（3 秒內）
                    if is_host && self.renaming.is_none() {
                        if ch.id != DEFAULT_CHANNEL {
                            let confirming = self
                                .confirm_delete
                                .is_some_and(|(id, t)| id == ch.id && t.elapsed() < Duration::from_secs(3));
                            if confirming {
                                if ui.add(egui::Button::new(RichText::new("確定刪除").color(RED)).small()).clicked() {
                                    s.delete_channel(ch.id);
                                    self.confirm_delete = None;
                                }
                            } else if ui.small_button("刪除").on_hover_text("刪除這個頻道（所有人都會離開）").clicked() {
                                self.confirm_delete = Some((ch.id, Instant::now()));
                            }
                        }
                        if ui.small_button("改名").clicked() {
                            self.renaming = Some((ch.id, ch.name.clone()));
                        }
                        ui.add_space(6.0);
                    }
                    if joined {
                        if ui.small_button("離開").clicked() {
                            let _ = s.set_channel(ch.id, None);
                        }
                    } else if ui.small_button("加入").clicked() {
                        let _ = s.set_channel(ch.id, Some(join_cfg(&self.settings, s, &ch.name)));
                    }
                });
            });

            if let Some(mut cfg) = ch.mine {
                let before = cfg;
                ui.horizontal(|ui| {
                    ui.checkbox(&mut cfg.listen, "收聽");
                    ui.label("說話");
                    egui::ComboBox::from_id_salt(("talk", ch.id))
                        .width(110.0)
                        .selected_text(talk_label(cfg.talk))
                        .show_ui(ui, |ui| {
                            for m in [TalkMode::Off, TalkMode::Voice, TalkMode::Open, TalkMode::Ptt] {
                                ui.selectable_value(&mut cfg.talk, m, talk_label(m));
                            }
                        });
                    if cfg.talk == TalkMode::Ptt {
                        let cur = self.settings.ptt_keys.get(&ch.name).cloned();
                        let t = KeyTarget::Channel(ch.id, ch.name.clone());
                        if key_button(ui, &self.recording, &t, cur.as_deref()) {
                            record = Some(t);
                        }
                    }
                    if cfg.mix.priority {
                        ui.colored_label(AMBER, "優先");
                    }
                });
                egui::CollapsingHeader::new("進階").id_salt(("adv", ch.id)).default_open(false).show(ui, |ui| {
                    ui.add(egui::Slider::new(&mut cfg.mix.volume, 0.0..=2.0).text("音量"));
                    ui.add(egui::Slider::new(&mut cfg.mix.pan, -1.0..=1.0).text("左右定位").custom_formatter(|v, _| {
                        if v < -0.05 {
                            format!("左 {:.0}%", -v * 100.0)
                        } else if v > 0.05 {
                            format!("右 {:.0}%", v * 100.0)
                        } else {
                            "中".into()
                        }
                    }));
                    ui.checkbox(&mut cfg.mix.priority, "優先頻道（有人說話時壓低其他頻道）");
                });
                if cfg.talk != before.talk {
                    if cfg.talk == TalkMode::Ptt {
                        // 改成按鍵說話：套用存好的鍵；還沒設定就直接開始錄製
                        cfg.ptt_key = self.settings.ptt_key(&ch.name);
                        if cfg.ptt_key.is_none() {
                            record = Some(KeyTarget::Channel(ch.id, ch.name.clone()));
                        }
                    } else if before.talk == TalkMode::Ptt {
                        // 不再用按鍵說話：下次加入時也不套用
                        self.settings.ptt_keys.remove(&ch.name);
                    }
                }
                if cfg.talk == TalkMode::Ptt && cfg.ptt_key.is_none() {
                    ui.colored_label(AMBER, "還沒設定按鍵，這個頻道不會送出聲音");
                }
                if cfg != before {
                    // 不收聽也不說話等同離開，set_channel 會處理
                    let _ = s.set_channel(ch.id, Some(cfg));
                }
            }

            for m in &ch.members {
                ui.horizontal(|ui| {
                    dot(ui, if m.speaking { GREEN } else { Color32::DARK_GRAY }, m.speaking);
                    if m.is_me {
                        ui.label(RichText::new(format!("{}（我）", m.name)).strong());
                    } else {
                        ui.label(&m.name);
                    }
                    if m.listen && !m.talk {
                        ui.weak("只聽");
                    } else if m.talk && !m.listen {
                        ui.weak("只說");
                    }
                });
            }
        });
        if let Some(t) = record {
            self.start_recording(t);
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 16.0) / 3.0;
            let toggle = |on: bool, on_text: &str, off_text: &str| {
                let b = egui::Button::new(RichText::new(if on { on_text } else { off_text }).size(15.0))
                    .min_size(egui::vec2(w, 34.0));
                if on { b.fill(RED.gamma_multiply(0.6)) } else { b }
            };
            if ui.add(toggle(snap.muted, "已靜音", "靜音")).clicked() {
                if let Some(s) = &self.session {
                    s.set_muted(!snap.muted);
                }
            }
            if ui.add(toggle(snap.deafened, "已拒聽", "拒聽")).clicked() {
                if let Some(s) = &self.session {
                    s.set_deafened(!snap.deafened);
                }
            }
            if ui.add(egui::Button::new(RichText::new("離開").size(15.0)).min_size(egui::vec2(w, 34.0))).clicked() {
                self.leave();
            }
        });
        let path = match snap.path {
            Some((true, r)) => format!("經 relay {r:.0} ms"),
            Some((false, r)) => format!("直連 {r:.0} ms"),
            None => "連線中…".into(),
        };
        let j = &snap.jitter;
        ui.weak(format!(
            "{path}｜緩衝 {}｜補償 FEC {} / PLC {}｜欠載 {}",
            j.target, j.fec, j.plc, snap.underruns
        ));
        ui.add_space(2.0);
    }
}

/// 狀態圓點；`glow` 時外圈加光暈
fn dot(ui: &mut egui::Ui, color: Color32, glow: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    if glow {
        ui.painter().circle_filled(rect.center(), 7.0, color.gamma_multiply(0.35));
    }
    ui.painter().circle_filled(rect.center(), 4.5, color);
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        match self.auto.take() {
            Some(Auto::Host) => self.start_host(),
            Some(Auto::Join(t)) => {
                self.ticket_input = t;
                self.start_join();
            }
            None => {}
        }
        self.poll_pending();
        if self.settings != self.saved {
            self.settings.save(&self.profile);
            self.saved = self.settings.clone();
        }

        self.feed_local_keys(ui.ctx());
        let snap = self.session.as_ref().map(|s| s.snapshot());
        self.poll_recording(snap.as_ref());
        if let Some(snap) = &snap {
            egui::Panel::bottom("controls").show(ui, |ui| self.controls(ui, snap));
        }
        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("MultiVoice");
                if let Some(s) = &self.session {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.weak(format!("麥克風：{}", short(&s.input_name)));
                    });
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match &snap {
                Some(snap) => self.room(ui, snap),
                None => self.lobby(ui),
            });
        });

        if self.recording.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(30));
        } else if self.session.is_some() || self.pending.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(80));
        }
    }
}

/// 裝置名稱很長（例如「Microphone (Razer Seiren V3 Mini) [Microphone]」），只取括號內的型號
fn short(name: &str) -> &str {
    name.split_once('(').and_then(|(_, r)| r.split_once(')')).map(|(m, _)| m).unwrap_or(name)
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(s) = self.session.take() {
            let fut = s.leave();
            let _ = self.rt.block_on(async { tokio::time::timeout(Duration::from_secs(2), fut).await });
        }
    }
}

/// egui 的按鍵 → hotkeys 的按鍵（用名稱對應；Shift 才打得出的符號沒有對應）
fn egui_trigger(key: egui::Key) -> Option<hotkeys::Trigger> {
    let name = match key {
        egui::Key::OpenBracket => "[",
        egui::Key::CloseBracket => "]",
        k => k.name(),
    };
    hotkeys::parse_key(name)
}

/// 加入某頻道時的設定：有存 PTT 鍵就用按鍵說話，否則用預設
fn join_cfg(settings: &Settings, s: &Session, name: &str) -> ChannelCfg {
    let mut cfg = s.default_channel_cfg();
    if let Some(k) = settings.ptt_key(name) {
        cfg.talk = TalkMode::Ptt;
        cfg.ptt_key = Some(k);
    }
    cfg
}

/// 設定按鍵的按鈕：顯示目前的鍵；錄製中顯示提示。回傳 true = 要開始錄製
fn key_button(ui: &mut egui::Ui, recording: &Option<KeyTarget>, target: &KeyTarget, current: Option<&str>) -> bool {
    let active = recording.as_ref() == Some(target);
    let text = if active {
        RichText::new("請按鍵…（Esc 取消）").color(AMBER)
    } else {
        match current {
            Some(k) => RichText::new(k).strong(),
            None => RichText::new("設定按鍵"),
        }
    };
    let r = ui
        .add(egui::Button::new(text).small())
        .on_hover_text("按下要用的鍵（可搭配 Ctrl／Alt／Shift，也可以用滑鼠中鍵或側鍵）");
    // 不留鍵盤焦點：否則錄到 Space／Enter 的同時，egui 會把它當成再按一次這個按鈕
    r.surrender_focus();
    r.clicked() && !active
}

fn talk_label(m: TalkMode) -> &'static str {
    match m {
        TalkMode::Off => "不說話",
        TalkMode::Voice => "偵測說話",
        TalkMode::Open => "一直開麥",
        TalkMode::Ptt => "按鍵說話",
    }
}
