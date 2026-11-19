//! 主畫面：大廳（建立／加入房間、音訊設定）與房間（成員、說話指示、靜音／拒聽）。

use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use protocol::DEFAULT_CHANNEL;
use session::{ChannelCfg, Connected, Relays, Session, Snapshot, TalkMode, hotkeys};

use crate::presets;
use crate::settings::Settings;
use crate::tray::Tray;
use crate::update;
use crate::style::{self, AMBER, Ctl, GREEN, Icon, RED};

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
    /// 複製邀請連結的時間（按鈕暫時顯示「已複製」）
    copied_at: Option<Instant>,
    /// 測試用：啟動後自動建房／加入
    auto: Option<Auto>,
    start_deafened: bool,
    /// 目前套用到工作階段的音訊設定（用來偵測使用者在房間裡改了設定）
    applied: Option<session::AudioOpts>,
    new_channel: String,
    /// 正在改名的頻道與輸入中的名稱
    renaming: Option<(u32, String)>,
    /// 上次看到的頻道名稱（頻道 ID → 名稱），用來發現改名、搬移以名稱存的設定
    channel_names: std::collections::BTreeMap<u32, String>,
    /// 正在錄製哪個按鍵
    recording: Option<KeyTarget>,
    /// 全域按鍵監聽啟動失敗的原因
    hotkey_err: Option<String>,
    /// 由本視窗補送給 hotkeys、目前按住的鍵（視窗失去焦點時要放開）
    local_down: Vec<hotkeys::Trigger>,
    /// 按了一次「刪除」、等待確認的頻道與時間
    confirm_delete: Option<(u32, Instant)>,
    /// 按了一次「更換邀請」、等待確認的時間
    confirm_rotate: Option<Instant>,
    /// 系統匣圖示（建立失敗時 None，關閉視窗就直接結束）
    tray: Option<std::sync::Arc<Tray>>,
    /// 這次執行是否已經顯示過「仍在執行」通知
    tray_notified: bool,
    /// 設定檔下拉選單目前選的名稱
    preset_sel: Option<String>,
    /// 正在輸入「另存設定檔」的名稱
    preset_new: Option<String>,
    /// 設定檔操作結果（顯示一段時間）
    preset_msg: Option<(String, Instant)>,
    /// 大廳：正在改名的設定檔（輸入中的新名稱）
    preset_renaming: Option<String>,
    /// 按了一次「刪除」的設定檔與時間（3 秒內再按一次才刪）
    preset_confirm_delete: Option<(String, Instant)>,
    /// 設定檔改名失敗等訊息
    preset_err: Option<String>,
    /// 開房連線完成後要套用的設定檔
    host_apply: Option<String>,
    /// 套用中的設定檔：房主要先建立缺少的頻道，等頻道出現再套用
    pending_apply: Option<PendingApply>,
    /// 背景檢查新版本的結果
    update_rx: Option<mpsc::Receiver<Option<update::Release>>>,
    /// 有新版本（使用者按「稍後」就清掉）
    update: Option<update::Release>,
    /// 一鍵更新進行中
    install: Option<update::Install>,
    install_err: Option<String>,
    /// 測試用：找到新版就直接更新
    auto_update: bool,
    /// 更新後重新啟動：關閉視窗時不要縮到系統匣
    exiting: bool,
    /// 麥克風測試（開著時記下用的是哪組裝置，換了就重開）
    mic_test: Option<(session::mictest::MicTest, Option<String>, Option<String>)>,
    /// 麥克風測試時播放自己的聲音
    mic_monitor: bool,
    mic_test_err: Option<String>,
    /// 這一幀有沒有畫到麥克風測試（設定區收起來、進了房間就停止測試）
    mic_test_shown: bool,
}

/// 正在設定 PTT 鍵的頻道（ID、名稱）
#[derive(Clone, Debug, PartialEq)]
struct KeyTarget(u32, String);

enum Auto {
    Host,
    Join(String),
}

impl App {
    /// 命令列參數（測試用）：`--profile 名稱`、`--auto-host`、`--auto-join 邀請票`、`--deafen`、`--auto-update`
    pub fn new(rt: tokio::runtime::Runtime, tray: Option<std::sync::Arc<Tray>>) -> Self {
        let mut args = std::env::args().skip(1);
        let mut profile = "default".to_string();
        let mut auto = None;
        let mut start_deafened = false;
        let mut auto_update = false;
        while let Some(a) = args.next() {
            match a.as_str() {
                "--profile" => profile = args.next().unwrap_or(profile),
                "--auto-host" => auto = Some(Auto::Host),
                "--auto-join" => auto = args.next().map(Auto::Join),
                "--deafen" => start_deafened = true,
                "--auto-update" => auto_update = true,
                _ => {}
            }
        }
        let settings = Settings::load(&profile);
        let last_preset = settings.last_preset.clone();
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
            channel_names: Default::default(),
            recording: None,
            // 大廳就要能錄製按鍵，所以一開始就啟動監聽
            hotkey_err: hotkeys::start().err(),
            local_down: Vec::new(),
            confirm_delete: None,
            confirm_rotate: None,
            tray,
            tray_notified: false,
            preset_sel: last_preset,
            preset_new: None,
            preset_msg: None,
            preset_renaming: None,
            preset_confirm_delete: None,
            preset_err: None,
            host_apply: None,
            pending_apply: None,
            update_rx: Some(update::spawn_check()),
            update: None,
            install: None,
            install_err: None,
            auto_update,
            exiting: false,
            mic_test: None,
            mic_monitor: false,
            mic_test_err: None,
            mic_test_shown: false,
        }
    }

    /// 本視窗在前景時，全域 hook 收不到大部分鍵盤事件（見 `hotkeys::feed`），
    /// 把 egui 收到的按鍵與修飾鍵變化補送過去：錄製按鍵、PTT 在本視窗裡也能用
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

    /// 在房間中按 X：縮到系統匣（從系統匣選「結束」時才真的關閉）
    fn handle_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        let Some(t) = &self.tray else { return };
        if self.session.is_some() && self.settings.close_to_tray && !t.quit_requested() && !self.exiting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            t.hide_main(!self.tray_notified);
            self.tray_notified = true;
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
        let KeyTarget(id, name) = self.recording.take().unwrap();
        let hotkeys::Recorded::Key(k) = r else { return };
        self.settings.ptt_keys.insert(name, k.to_string());
        let mine = snap.and_then(|s| s.channels.iter().find(|c| c.id == id)).and_then(|c| c.mine);
        if let (Some(s), Some(cfg)) = (&self.session, mine) {
            let _ = s.set_channel(id, Some(ChannelCfg { talk: TalkMode::Ptt, ptt_key: Some(k), ..cfg }));
        }
    }



    fn start_host(&mut self) {
        let (tx, rx) = mpsc::channel();
        let (profile, name) = (self.profile.clone(), self.settings.name.trim().to_string());
        let can_host = self.settings.allow_host;
        self.host_apply = self.host_preset().map(|p| p.name.clone());
        self.rt.spawn(async move {
            let _ = tx.send(session::host_room(&profile, &name, Relays::Default, can_host).await);
        });
        self.pending = Some((rx, "建立房間中…"));
        self.error = None;
    }

    fn start_join(&mut self) {
        let ticket = protocol::ticket::clean_input(&self.ticket_input);
        self.settings.last_ticket = ticket.clone();
        self.host_apply = None;
        let (tx, rx) = mpsc::channel();
        let (profile, name) = (self.profile.clone(), self.settings.name.trim().to_string());
        let can_host = self.settings.allow_host;
        self.rt.spawn(async move {
            let _ = tx.send(session::join_room(&ticket, &profile, &name, Relays::Default, can_host).await);
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
        self.mic_test = None;
        let host_apply = self.host_apply.take();
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
                // 開房時選了設定檔：建好頻道後套用（drive_pending_apply）
                self.pending_apply = host_apply.map(PendingApply::new);
                if let Some(t) = &self.tray {
                    t.set_session(Some(s.shared()));
                }
                self.channel_names.clear();
                self.session = Some(s);
                self.applied = Some(self.settings.audio_opts());
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    fn leave(&mut self) {
        if let Some(t) = &self.tray {
            t.set_session(None);
        }
        if let Some(s) = self.session.take() {
            self.rt.spawn(s.leave());
        }
    }

    /// 開房時要套用的設定檔（找不到就是不用）
    fn host_preset(&self) -> Option<&presets::Preset> {
        let n = self.settings.host_preset.as_ref()?;
        self.settings.presets.iter().find(|p| &p.name == n)
    }

    /// 大廳：開房時套用哪個設定檔，也在這裡改名、刪除（有存過設定檔才顯示）
    fn host_preset_picker(&mut self, ui: &mut egui::Ui) {
        if self.settings.presets.is_empty() {
            return;
        }
        if self.host_preset().is_none() {
            self.settings.host_preset = None;
        }
        const NONE: &str = "不使用（只有大廳）";
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("開房設定檔");
            let sel = self.settings.host_preset.clone();
            // 改名中：選單換成輸入框
            if let (Some(old), Some(text)) = (&sel, &mut self.preset_renaming) {
                let r = ui.add(egui::TextEdit::singleline(text).desired_width(170.0));
                r.request_focus();
                let mut done = None;
                if ui.small_button("確定").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    done = Some(true);
                } else if ui.small_button("取消").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = Some(false);
                }
                match done {
                    Some(true) => match presets::rename(&mut self.settings.presets, old, text) {
                        Ok(()) => {
                            let new = text.trim().to_string();
                            for n in [&mut self.settings.last_preset, &mut self.preset_sel] {
                                if n.as_ref() == Some(old) {
                                    *n = Some(new.clone());
                                }
                            }
                            self.settings.host_preset = Some(new);
                            self.preset_renaming = None;
                            self.preset_err = None;
                        }
                        Err(e) => self.preset_err = Some(e.into()),
                    },
                    Some(false) => {
                        self.preset_renaming = None;
                        self.preset_err = None;
                    }
                    None => {}
                }
                return;
            }
            let names: Vec<String> = self.settings.presets.iter().map(|p| p.name.clone()).collect();
            let text = sel.clone().unwrap_or_else(|| NONE.into());
            egui::ComboBox::from_id_salt("host_preset").width(170.0).selected_text(text).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.settings.host_preset, None, NONE);
                for n in &names {
                    ui.selectable_value(&mut self.settings.host_preset, Some(n.clone()), n);
                }
            });
            let Some(n) = sel else { return };
            if ui.small_button("改名").on_hover_text("幫這個設定檔改名").clicked() {
                self.preset_renaming = Some(n.clone());
                self.preset_confirm_delete = None;
            }
            if self.delete_preset_button(ui, &n) {
                self.settings.host_preset = None;
            }
        });
        if let Some(e) = &self.preset_err {
            ui.colored_label(RED, e);
        }
        if let Some(p) = self.host_preset() {
            let all: Vec<&str> = p.channels.iter().map(|c| c.channel.as_str()).collect();
            let joined: Vec<&str> = p.channels.iter().filter(|c| c.joined).map(|c| c.channel.as_str()).collect();
            ui.weak(format!("開房時建立：{}", all.join("、")));
            ui.weak(format!("我加入：{}", if joined.is_empty() { "—".to_string() } else { joined.join("、") }));
        }
    }

    /// 刪除設定檔的按鈕：要按兩次（3 秒內）。刪掉了回傳 true
    fn delete_preset_button(&mut self, ui: &mut egui::Ui, name: &str) -> bool {
        let confirming =
            self.preset_confirm_delete.as_ref().is_some_and(|(m, t)| m == name && t.elapsed() < Duration::from_secs(3));
        if confirming {
            if ui.add(egui::Button::new(RichText::new("確定刪除").color(RED)).small()).clicked() {
                self.settings.presets.retain(|p| p.name != name);
                self.preset_confirm_delete = None;
                self.preset_msg = Some((format!("已刪除設定檔「{name}」"), Instant::now()));
                return true;
            }
        } else if ui.small_button("刪除").on_hover_text("刪除這個設定檔").clicked() {
            self.preset_confirm_delete = Some((name.to_string(), Instant::now()));
        }
        false
    }

    fn lobby(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        let busy = self.pending.is_some();
        style::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("你的名稱").strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.settings.name)
                    .hint_text("其他人看到的名字")
                    .margin(egui::vec2(8.0, 6.0))
                    .desired_width(f32::INFINITY),
            );
        });
        let name_ok = !self.settings.name.trim().is_empty();

        ui.add_space(10.0);
        style::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("開新房間").strong());
            ui.weak("你當房主，把邀請連結傳給朋友");
            ui.add_space(4.0);
            let big = egui::vec2(ui.available_width(), 44.0);
            if style::primary_button(ui, name_ok && !busy, "建立房間", big).clicked() {
                self.start_host();
            }
            self.host_preset_picker(ui);
        });

        ui.add_space(10.0);
        style::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("加入別人的房間").strong());
            ui.add(
                egui::TextEdit::multiline(&mut self.ticket_input)
                    .hint_text("貼上邀請連結（Ctrl+V）")
                    .margin(egui::vec2(8.0, 6.0))
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(4.0);
            let ticket_ok = protocol::ticket::clean_input(&self.ticket_input).starts_with("mv1");
            let big = egui::vec2(ui.available_width(), 44.0);
            if style::primary_button(ui, name_ok && ticket_ok && !busy, "加入房間", big).clicked() {
                self.start_join();
            }
        });

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

        ui.add_space(10.0);
        style::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::CollapsingHeader::new(RichText::new("音訊設定").strong())
                .default_open(false)
                .show(ui, |ui| self.audio_settings(ui));
        });
        ui.add_space(6.0);
        if self.profile != "default" {
            ui.weak(format!("身分設定檔：{}", self.profile));
        }
        ui.weak(format!("版本 {}", update::current()));
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
        style::toggle(ui, &mut self.settings.denoise, "降噪（消除風扇、鍵盤等背景聲）");
        if self.settings.denoise {
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.settings.strong_denoise, false, "標準");
                ui.radio_value(&mut self.settings.strong_denoise, true, "強力（DeepFilterNet）")
                    .on_hover_text("背景很吵（冷氣、風扇、鍵盤）時更乾淨，但多約 20 ms 延遲。可以在下面的麥克風測試開「聽自己的聲音」比較");
            });
            if !self.settings.strong_denoise {
                ui.add(egui::Slider::new(&mut self.settings.speech_mix, 0.0..=0.6).text("小聲講話保留"));
                ui.weak("調高：小聲的字更清楚，但講話時會多一點背景聲");
            }
        }

        ui.add_space(6.0);
        ui.add(
            egui::Slider::new(&mut self.settings.gate_dbfs, -70.0..=-20.0)
                .step_by(1.0)
                .text("說話偵測門檻")
                .custom_formatter(|v, _| format!("{v:.0} dB")),
        );
        ui.weak("「偵測說話」模式下，聲音比門檻大才送出。背景吵時調高、講話常被切掉時調低");
        style::toggle(ui, &mut self.settings.agc, "自動音量（講話太小聲時自動放大、太大聲時壓低）");

        ui.add_space(6.0);
        self.mic_test_ui(ui);

        ui.add_space(8.0);
        ui.label(RichText::new("按鍵說話").strong());
        ui.weak("進房後在頻道的「說話」選「按鍵說話」設定按鍵；遊戲在前景時也有效");
        style::toggle(ui, &mut self.settings.cues, "開／關麥提示音");
        if self.tray.is_some() {
            style::toggle(ui, &mut self.settings.close_to_tray, "在房間中關閉視窗時縮到系統匣（不離開房間）");
        }
        style::toggle(ui, &mut self.settings.allow_host, "房主離開或斷線時，可以由我接手當房主")
            .on_hover_text("網路很慢或電腦效能不好時可以關掉；下次進房才生效");
        if let Some(e) = &self.hotkey_err {
            ui.colored_label(RED, format!("按鍵說話無法使用：{e}"));
        } else if !hotkeys::is_admin() {
            ui.weak("如果遊戲以系統管理員身分執行，這裡也要以管理員身分執行，否則遊戲在前景時收不到按鍵");
            if ui.small_button("以系統管理員身分重新啟動").clicked() && hotkeys::relaunch_as_admin(&[crate::instance::RESTARTED]) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn room(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        // 我聽各成員的音量（依名稱存，套到目前的成員編號）
        if let Some(s) = &self.session {
            let gains = snap
                .members
                .iter()
                .filter_map(|m| self.settings.member_volumes.get(&m.name).map(|&g| (m.idx, g)))
                .collect();
            s.shared().set_user_gains(gains);
        }
        // 房間資訊與邀請票
        style::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                // 不用 emoji：系統中文字型沒有 emoji，會顯示成方框
                if snap.reconnecting {
                    ui.spinner();
                    ui.label(RichText::new("正在重新連線…").strong().color(AMBER));
                } else {
                    dot(ui, GREEN, false);
                    ui.label(RichText::new("已連線").strong());
                    if snap.is_owner {
                        style::badge(ui, "房主", AMBER);
                    }
                }
                style::badge(ui, format!("{} 人", snap.members.len() + 1), style::accent_text(ui));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // 邀請連結 = 網頁版網址 + `#` + 邀請票：桌面版貼上也能加入，所以只給連結
                    let copied = self.copied_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2));
                    if ui.button(if copied { "已複製" } else { "複製邀請連結" }).clicked() {
                        ui.ctx().copy_text(format!("{}#{}", session::WEB_CLIENT_URL, snap.ticket));
                        self.copied_at = Some(Instant::now());
                    }
                    // 房主：更換邀請（要按兩次，3 秒內）
                    if snap.is_owner
                        && let Some(s) = &self.session
                    {
                        let confirming = self.confirm_rotate.is_some_and(|t| t.elapsed() < Duration::from_secs(3));
                        if confirming {
                            if ui.add(egui::Button::new(RichText::new("確定更換").color(RED))).clicked() {
                                s.rotate_invite();
                                self.confirm_rotate = None;
                            }
                        } else if ui
                            .button("更換邀請")
                            .on_hover_text("舊的邀請連結全部失效（已經在房間裡的人不受影響）")
                            .clicked()
                        {
                            self.confirm_rotate = Some(Instant::now());
                        }
                    }
                });
            });
            ui.weak("把邀請連結傳給朋友：有裝程式的貼上就能加入，沒裝的直接打開用瀏覽器加入");
            if !snap.is_owner {
                match &snap.owner_name {
                    Some(n) => ui.weak(format!("房主：{n}")),
                    None => ui.weak("房主不在房間裡（改名、刪除頻道要等他回來）"),
                };
            }
            // 轉發負載（PLAN §6.4）：語音由我轉發給大家，上傳被吃掉太多時可以讓給別人（房主不變）
            if let Some(load) = snap.host_load {
                ui.horizontal(|ui| {
                    ui.weak(format!("由你轉發：給 {} 人｜上傳約 {:.0} kbps", load.listeners, load.up_kbps));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let r = ui
                            .add_enabled(snap.can_hand_over, egui::Button::new("讓出轉發").small())
                            .on_hover_text("改由其他人轉發語音，你會留在房間裡，房主還是原本的人（遊戲網路吃緊時用）")
                            .on_disabled_hover_text("房間裡還沒有可以接手的人（網頁版不能轉發）");
                        if r.clicked()
                            && let Some(s) = &self.session
                        {
                            s.hand_over();
                        }
                    });
                });
            }
        });

        ui.add_space(6.0);
        // 自己：麥克風音量與目前送出的頻道
        style::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let color = if !snap.has_mic {
                    style::faint(ui)
                } else if snap.muted {
                    RED
                } else if snap.tx_active {
                    GREEN
                } else {
                    style::faint(ui)
                };
                dot(ui, color, snap.tx_active && !snap.muted);
                ui.label(RichText::new(format!("{}（我）", snap.me.name)).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !snap.has_mic {
                        ui.colored_label(AMBER, "沒有麥克風，只能聽");
                    } else if snap.deafened {
                        ui.colored_label(RED, "已拒聽（麥克風也關閉）");
                    } else if snap.muted {
                        ui.colored_label(RED, "已靜音");
                    } else if snap.tx_active {
                        ui.colored_label(GREEN, "送出中");
                    }
                });
            });
            let db = 20.0 * snap.mic_peak.max(1e-6).log10();
            // 麥克風關閉（靜音／拒聽）或沒有麥克風時音量條不動
            let mic_off = snap.muted || !snap.has_mic;
            let frac = if mic_off { 0.0 } else { ((db + 60.0) / 60.0).clamp(0.0, 1.0) };
            style::meter(ui, frac, snap.tx_active && !mic_off, mic_off, 8.0, &[]);
            if !snap.has_mic {
                ui.weak("插上麥克風後會自動開啟，不用重新進房");
            } else if snap.talking_to.is_empty() {
                ui.weak("目前送出：—");
            } else {
                ui.colored_label(GREEN, format!("目前送出：{}", snap.talking_to.join("、")));
            }
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            style::section_title(ui, "頻道");
            style::badge(ui, format!("{} 個", snap.channels.len()), style::accent_text(ui));
        });
        self.drive_pending_apply(snap);
        self.presets_bar(ui, snap);
        for ch in &snap.channels {
            self.channel_card(ui, ch, snap.is_owner);
            ui.add_space(2.0);
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

        // 設定有變就套用（例如系統匣或其他地方改了設定）：降噪、音量立即生效；換裝置會重啟音訊（不中斷連線）
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
    /// 麥克風測試：音量條（含門檻與環境噪音標記）、是否判定為說話、聽自己的聲音
    fn mic_test_ui(&mut self, ui: &mut egui::Ui) {
        self.mic_test_shown = true;
        ui.label(RichText::new("麥克風測試").strong());
        let (input, output) = (self.settings.input_device.clone(), self.settings.output_device.clone());
        // 換了裝置：重開
        if self.mic_test.as_ref().is_some_and(|(_, i, o)| *i != input || *o != output) {
            self.mic_test = None;
            self.start_mic_test();
        }
        ui.horizontal(|ui| {
            if self.mic_test.is_some() {
                if ui.button("停止測試").clicked() {
                    self.mic_test = None;
                }
            } else if ui.button("開始測試").clicked() {
                self.start_mic_test();
            }
            style::toggle(ui, &mut self.mic_monitor, "聽自己的聲音").on_hover_text("戴耳機時再開，用喇叭會產生回授");
        });
        if let Some(e) = &self.mic_test_err {
            ui.colored_label(RED, e);
        }
        let Some((t, ..)) = &self.mic_test else {
            ui.weak("對著麥克風正常講話，看看會不會被判定為說話；也可以聽聽降噪後的聲音");
            return;
        };
        t.set(self.settings.frontend_cfg(), self.mic_monitor);
        let r = t.reading();

        // 音量條：-70..0 dB
        let frac = |db: f32| ((db + 70.0) / 70.0).clamp(0.0, 1.0);
        let marks = [(frac(r.noise_db), style::muted(ui)), (frac(self.settings.gate_dbfs), AMBER)];
        style::meter(ui, frac(r.level_db), r.voiced, false, 14.0, &marks);

        ui.horizontal(|ui| {
            if r.voiced {
                ui.colored_label(GREEN, "偵測到說話");
            } else {
                ui.weak("安靜");
            }
            ui.weak(format!("｜音量 {:.0} dB｜環境噪音 {:.0} dB", r.level_db.max(-99.0), r.noise_db.max(-99.0)));
            if self.settings.agc {
                ui.weak(format!("｜自動音量 {:+.0} dB", r.gain_db));
            }
        });
        ui.horizontal(|ui| {
            ui.weak("黃線：門檻　灰線：環境噪音");
            let suggested = audio::frontend::suggest_gate(r.noise_db);
            if ui
                .small_button("依環境噪音設定門檻")
                .on_hover_text(format!("設成比環境噪音高 12 dB：{suggested:.0} dB（先安靜幾秒再按）"))
                .clicked()
            {
                self.settings.gate_dbfs = suggested.round();
            }
        });
        if r.peak > 0.98 {
            ui.colored_label(AMBER, "音量爆掉了：麥克風太近或系統的麥克風音量太大");
        }
    }

    fn start_mic_test(&mut self) {
        let (input, output) = (self.settings.input_device.clone(), self.settings.output_device.clone());
        match session::mictest::MicTest::start(input.clone(), output.clone(), self.settings.frontend_cfg(), self.mic_monitor) {
            Ok(t) => {
                self.mic_test = Some((t, input, output));
                self.mic_test_err = None;
            }
            Err(e) => self.mic_test_err = Some(format!("無法開啟麥克風：{e:#}")),
        }
    }

    /// 有新版本時的提示列：一鍵更新（下載 → 換檔 → 重新啟動，在房間裡的話重新啟動後自動加入）
    fn update_banner(&mut self, ui: &mut egui::Ui, snap: Option<&Snapshot>) {
        let Some(r) = self.update.clone() else { return };
        if self.auto_update && self.install.is_none() && self.install_err.is_none() {
            self.auto_update = false;
            if let Some(a) = r.asset.clone() {
                self.install = Some(update::spawn_install(a));
            }
        }
        // 下載完成：重新啟動
        if let Some(res) = self.install.as_ref().and_then(|i| i.poll()) {
            self.install = None;
            let rejoin = snap.map(|s| s.ticket.clone());
            match res.and_then(|exe| update::restart(&exe, rejoin.as_deref()).map_err(|e| format!("無法啟動新版本：{e}"))) {
                Ok(()) => {
                    self.exiting = true;
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Err(e) => self.install_err = Some(e),
            }
        }
        style::card(ui).fill(AMBER.gamma_multiply(0.25)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("有新版本 v{}", r.version)).strong().color(AMBER));
                ui.weak(format!("（目前 v{}）", update::current()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(i) = &self.install {
                        ui.add(egui::ProgressBar::new(i.progress()).desired_width(90.0).show_percentage());
                        ui.weak("下載中");
                        return;
                    }
                    if ui.small_button("稍後").clicked() {
                        self.update = None;
                    }
                    if ui.small_button("更新說明").clicked() {
                        update::open_url(&r.url);
                    }
                    if let Some(a) = &r.asset {
                        let hint = if snap.is_some() {
                            "下載後重新啟動，會短暫離開房間，重新啟動後自動加入"
                        } else {
                            "下載後自動重新啟動"
                        };
                        if ui.button("立即更新").on_hover_text(hint).clicked() {
                            self.install_err = None;
                            self.install = Some(update::spawn_install(a.clone()));
                        }
                    }
                });
            });
            if let Some(e) = &self.install_err {
                ui.colored_label(RED, format!("更新失敗：{e}"));
                ui.weak("可以按「更新說明」到下載頁面手動下載");
            } else {
                ui.weak("房間裡的人版本不同時可能無法連線，建議大家一起更新");
            }
        });
        ui.add_space(4.0);
    }

    /// 設定檔列：選擇、套用、另存目前組合、刪除
    fn presets_bar(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.horizontal(|ui| {
            ui.label("設定檔");
            let names: Vec<String> = self.settings.presets.iter().map(|p| p.name.clone()).collect();
            if self.preset_sel.as_ref().is_some_and(|n| !names.contains(n)) {
                self.preset_sel = None;
            }
            let text = match (&self.preset_sel, names.is_empty()) {
                (Some(n), _) => n.clone(),
                (None, true) => "（還沒有）".into(),
                (None, false) => "選擇…".into(),
            };
            egui::ComboBox::from_id_salt("preset").width(130.0).selected_text(text).show_ui(ui, |ui| {
                for n in &names {
                    ui.selectable_value(&mut self.preset_sel, Some(n.clone()), n);
                }
            });
            let sel = self.preset_sel.clone();
            if ui.add_enabled(sel.is_some(), egui::Button::new("套用").small()).clicked()
                && let Some(n) = &sel
            {
                self.pending_apply = Some(PendingApply::new(n.clone()));
            }
            if ui
                .small_button("另存")
                .on_hover_text("把房間的頻道和我在各頻道的設定存成設定檔；開房時選它就會自動建好這些頻道")
                .clicked()
            {
                self.preset_new = Some(sel.clone().unwrap_or_default());
            }
            if let Some(n) = &sel
                && self.delete_preset_button(ui, n)
            {
                self.preset_sel = None;
            }
        });
        if let Some(name) = &mut self.preset_new {
            let mut done = None;
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(name).hint_text("設定檔名稱，例如：公會戰指揮").desired_width(200.0));
                r.request_focus();
                let ok = !name.trim().is_empty();
                if ok && (ui.small_button("儲存").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    done = Some(true);
                } else if ui.small_button("取消").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = Some(false);
                }
            });
            match done {
                Some(true) => {
                    let name = self.preset_new.take().unwrap().trim().to_string();
                    let p = presets::capture(&name, snap.channels.iter().map(|c| (c.name.as_str(), c.mine.as_ref())));
                    let (n, joined) = (p.channels.len(), p.joined_count());
                    match self.settings.presets.iter_mut().find(|q| q.name == name) {
                        Some(q) => *q = p,
                        None => self.settings.presets.push(p),
                    }
                    self.preset_msg = Some((format!("已儲存設定檔「{name}」（{n} 個頻道，加入 {joined} 個）"), Instant::now()));
                    self.settings.last_preset = Some(name.clone());
                    self.preset_sel = Some(name);
                }
                Some(false) => self.preset_new = None,
                None => {}
            }
        }
        if let Some((msg, t)) = &self.preset_msg {
            if t.elapsed() < Duration::from_secs(5) {
                ui.weak(msg);
            } else {
                self.preset_msg = None;
            }
        }
    }

    /// 套用中的設定檔：我是房主就先建立缺少的頻道，等頻道都出現（最多 5 秒）再套用
    fn drive_pending_apply(&mut self, snap: &Snapshot) {
        let Some(pa) = &mut self.pending_apply else { return };
        let Some(p) = self.settings.presets.iter().find(|p| p.name == pa.name) else {
            self.pending_apply = None;
            return;
        };
        let missing = presets::missing(p, snap.channels.iter().map(|c| c.name.as_str()));
        if !pa.created && snap.is_owner && !missing.is_empty() {
            if let Some(s) = &self.session {
                for n in &missing {
                    s.create_channel(n);
                }
            }
            pa.created = true;
            pa.since = Instant::now();
            return;
        }
        if pa.created && !missing.is_empty() && pa.since.elapsed() < Duration::from_secs(5) {
            return;
        }
        let pa = self.pending_apply.take().unwrap();
        self.apply_preset(snap, &pa.name, pa.created);
    }

    /// 套用設定檔：設定檔裡加入的頻道照設定加入，其他頻道離開。`created`：剛幫忙建了頻道
    fn apply_preset(&mut self, snap: &Snapshot, name: &str, created: bool) {
        let Some(p) = self.settings.presets.iter().find(|p| p.name == name).cloned() else { return };
        let Some(s) = &self.session else { return };
        let mut failed = 0;
        for (id, cfg) in presets::plan(&p, snap.channels.iter().map(|c| (c.id, c.name.as_str()))) {
            if s.set_channel(id, cfg).is_err() {
                failed += 1;
            }
        }
        // 「加入頻道時自動用按鍵說話」的記錄也跟著設定檔走（只動設定檔裡加入的頻道）
        for c in p.channels.iter().filter(|c| c.joined) {
            match (&c.talk[..], &c.ptt_key) {
                ("ptt", Some(k)) => {
                    self.settings.ptt_keys.insert(c.channel.clone(), k.clone());
                }
                _ => {
                    self.settings.ptt_keys.remove(&c.channel);
                }
            }
        }
        self.settings.last_preset = Some(name.to_string());
        self.preset_sel = Some(name.to_string());
        let mut msg = if created { format!("已建立頻道並套用「{name}」") } else { format!("已套用「{name}」") };
        // 不是房主時建不了頻道：說一下哪些要加入的頻道房間裡沒有
        let missing: Vec<String> = presets::missing(&p, snap.channels.iter().map(|c| c.name.as_str()))
            .into_iter()
            .filter(|n| p.channels.iter().any(|c| &c.channel == n && c.joined))
            .collect();
        if !missing.is_empty() {
            msg += &format!("（房間裡沒有：{}）", missing.join("、"));
        }
        if failed > 0 {
            msg += &format!("（{failed} 個頻道設定失敗）");
        }
        self.preset_msg = Some((msg, Instant::now()));
    }

    /// `is_owner`：房主才顯示改名／刪除、移出成員、交出房主
    fn channel_card(&mut self, ui: &mut egui::Ui, ch: &session::ChannelView, is_owner: bool) {
        let Some(s) = &self.session else { return };
        let joined = ch.mine.is_some();
        // 閉包裡借著 session，不能呼叫 &mut self 的方法：先記下，結束後再開始錄製
        let mut record: Option<KeyTarget> = None;
        // 右鍵調整某位成員的音量：(名稱, 新倍率；None = 重設)
        let mut vol_change: Option<(String, Option<f32>)> = None;
        let volumes = self.settings.member_volumes.clone();
        let speaking = ch.members.iter().any(|m| m.speaking);
        style::channel_card(ui, joined, speaking).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if self.renaming.as_ref().is_some_and(|(id, _)| *id == ch.id) {
                    let (_, text) = self.renaming.as_mut().unwrap();
                    let r = ui.add(egui::TextEdit::singleline(text).desired_width(150.0));
                    r.request_focus();
                    let ok = !text.trim().is_empty();
                    if ok && (ui.small_button("確定").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                        // 存的 PTT 鍵與設定檔等房主確認改名後再跟著改（follow_renames）
                        s.rename_channel(ch.id, text.trim());
                        self.renaming = None;
                    } else if ui.small_button("取消").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.renaming = None;
                    }
                } else {
                    style::channel_icon(ui, joined, speaking);
                    ui.label(RichText::new(&ch.name).strong().size(16.0));
                    if !ch.members.is_empty() {
                        style::badge(ui, format!("{} 人", ch.members.len()), style::accent_text(ui));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // 房主管理：刪除要按兩次（3 秒內）
                    if is_owner && self.renaming.is_none() {
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
                    if style::join_button(ui, joined).clicked() {
                        let cfg = if joined { None } else { Some(join_cfg(&self.settings, s, &ch.name)) };
                        let _ = s.set_channel(ch.id, cfg);
                    }
                });
            });

            if let Some(mut cfg) = ch.mine {
                let before = cfg;
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    style::toggle(ui, &mut cfg.listen, "收聽");
                    ui.label("說話");
                    egui::ComboBox::from_id_salt(("talk", ch.id))
                        .width(110.0)
                        .selected_text(talk_label(cfg.talk))
                        .show_ui(ui, |ui| {
                            for m in [TalkMode::Off, TalkMode::Voice, TalkMode::Ptt] {
                                ui.selectable_value(&mut cfg.talk, m, talk_label(m));
                            }
                        });
                    if cfg.talk == TalkMode::Ptt {
                        let cur = self.settings.ptt_keys.get(&ch.name).cloned();
                        let t = KeyTarget(ch.id, ch.name.clone());
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
                    style::toggle(ui, &mut cfg.mix.priority, "優先頻道（有人說話時壓低其他頻道）");
                });
                if cfg.talk != before.talk {
                    if cfg.talk == TalkMode::Ptt {
                        // 改成按鍵說話：套用存好的鍵；還沒設定就直接開始錄製
                        cfg.ptt_key = self.settings.ptt_key(&ch.name);
                        if cfg.ptt_key.is_none() {
                            record = Some(KeyTarget(ch.id, ch.name.clone()));
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

            // 成員清單預設折疊（人多時卡片才不會很長）；折疊時標題列出誰在說話
            if !ch.members.is_empty() {
                let speaking: Vec<&str> = ch.members.iter().filter(|m| m.speaking).map(|m| m.name.as_str()).collect();
                let title = if speaking.is_empty() {
                    RichText::new("成員名單")
                } else {
                    RichText::new(format!("說話中：{}", speaking.join("、"))).color(GREEN)
                };
                egui::CollapsingHeader::new(title).id_salt(("members", ch.id)).default_open(false).show(ui, |ui| {
                    for m in &ch.members {
                        ui.horizontal(|ui| {
                            dot(ui, if m.speaking { GREEN } else { style::faint(ui) }, m.speaking);
                            if m.is_me {
                                ui.label(RichText::new(format!("{}（我）", m.name)).strong());
                            } else {
                                let gain = volumes.get(&m.name).copied().unwrap_or(1.0);
                                let r = ui
                                    .add(egui::Label::new(&m.name).sense(egui::Sense::click()))
                                    .on_hover_text(if is_owner { "右鍵：調整音量、移出房間、交出房主" } else { "右鍵：調整聽這個人的音量" });
                                if gain != 1.0 {
                                    ui.colored_label(if gain == 0.0 { RED } else { AMBER }, pct(gain));
                                }
                                r.context_menu(|ui| {
                                    ui.label(RichText::new(format!("{} 的音量", m.name)).strong());
                                    let mut pos = gain_to_pos(gain);
                                    ui.horizontal(|ui| {
                                        if ui.add(egui::Slider::new(&mut pos, 0.0..=1.0).show_value(false)).changed() {
                                            vol_change = Some((m.name.clone(), Some(pos_to_gain(pos))));
                                        }
                                        ui.label(pct(pos_to_gain(pos)));
                                    });
                                    ui.horizontal(|ui| {
                                        if ui.button("靜音").clicked() {
                                            vol_change = Some((m.name.clone(), Some(0.0)));
                                        }
                                        if ui.button("重設為 100%").clicked() {
                                            vol_change = Some((m.name.clone(), None));
                                            ui.close();
                                        }
                                    });
                                    ui.weak("只影響你聽到的音量");
                                    if is_owner {
                                        ui.separator();
                                        let r = ui
                                            .add_enabled(m.can_own, egui::Button::new("把房主交給他"))
                                            .on_hover_text("他之後可以改名、刪除頻道、移出成員；你會變成一般成員")
                                            .on_disabled_hover_text("只能交給用桌面版的成員（網頁版不能當房主）");
                                        if r.clicked() {
                                            s.transfer_owner(m.idx);
                                            ui.close();
                                        }
                                        // 選單點一下就會關：用子選單當確認
                                        ui.menu_button(RichText::new("移出房間").color(RED), |ui| {
                                            ui.weak("邀請連結會一起更換，他拿舊連結回不來；");
                                            ui.weak("房間裡的其他人會自動拿到新連結");
                                            if ui.button(RichText::new(format!("確定移出 {}", m.name)).color(RED)).clicked() {
                                                s.kick(m.idx);
                                            }
                                        });
                                    }
                                });
                            }
                            if m.listen && !m.talk {
                                ui.weak("只聽");
                            } else if m.talk && !m.listen {
                                ui.weak("只說");
                            }
                        });
                    }
                });
            }
        });
        if let Some(t) = record {
            self.start_recording(t);
        }
        if let Some((name, v)) = vol_change {
            match v {
                Some(g) if (g - 1.0).abs() > 0.005 => {
                    self.settings.member_volumes.insert(name, g);
                }
                _ => {
                    self.settings.member_volumes.remove(&name);
                }
            }
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.horizontal(|ui| {
            // 靜音、拒聽各佔一半，離開窄一點
            let gap = ui.spacing().item_spacing.x;
            let leave_w = 84.0;
            let w = ((ui.available_width() - leave_w - 2.0 * gap) / 2.0).max(80.0);
            let h = 40.0;
            let kind = |on: bool| if on { Ctl::Off } else { Ctl::Normal };
            let mute_text = if snap.muted { "已靜音" } else { "麥克風開" };
            let mute = style::control_button(ui, Icon::Mic, mute_text, kind(snap.muted), egui::vec2(w, h));
            let mute = if snap.deafened {
                mute.on_hover_text("拒聽中麥克風也會關閉；按這裡會同時取消拒聽")
            } else {
                mute.on_hover_text(if snap.muted { "按一下打開麥克風" } else { "按一下靜音" })
            };
            if mute.clicked() {
                if let Some(s) = &self.session {
                    s.set_muted(!snap.muted);
                }
            }
            let deaf_text = if snap.deafened { "已拒聽" } else { "收聽中" };
            let deaf = style::control_button(ui, Icon::Headphones, deaf_text, kind(snap.deafened), egui::vec2(w, h))
                .on_hover_text(if snap.deafened { "按一下恢復收聽" } else { "按一下拒聽（不聽也不說）" });
            if deaf.clicked() {
                if let Some(s) = &self.session {
                    s.set_deafened(!snap.deafened);
                }
            }
            if style::control_button(ui, Icon::Leave, "離開", Ctl::Danger, egui::vec2(leave_w, h))
                .on_hover_text("離開房間")
                .clicked()
            {
                self.leave();
            }
        });
        let path = match snap.path {
            Some((true, r)) => format!("經 relay {r:.0} ms"),
            Some((false, r)) => format!("直連 {r:.0} ms"),
            None => "連線中…".into(),
        };
        let j = &snap.jitter;
        let loss = match snap.loss {
            Some(l) => format!("{:.1}%", l * 100.0),
            None => "—".into(),
        };
        ui.add_space(2.0);
        ui.label(RichText::new(format!("{path}｜緩衝 {}｜遺失 {loss}｜欠載 {}", j.target, snap.underruns)).small().weak())
            .on_hover_text("遺失：最近 5 秒收到的語音中，需要補償（FEC／PLC）的比例。持續超過 5% 會聽到斷斷續續");
    }
}

/// 套用中的設定檔（見 [`App::drive_pending_apply`]）
struct PendingApply {
    name: String,
    /// 已經建立缺少的頻道
    created: bool,
    since: Instant,
}

impl PendingApply {
    fn new(name: String) -> Self {
        Self { name, created: false, since: Instant::now() }
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

        self.handle_close(ui.ctx());
        self.feed_local_keys(ui.ctx());
        let snap = self.session.as_ref().map(|s| s.snapshot());
        if let Some(snap) = &snap {
            self.settings.follow_renames(&mut self.channel_names, snap.channels.iter().map(|c| (c.id, c.name.as_str())));
        }
        self.poll_recording(snap.as_ref());
        if let Some(snap) = &snap {
            egui::Panel::bottom("controls")
                .frame(style::dock_frame(ui))
                .show(ui, |ui| self.controls(ui, snap));
        }
        if let Some(rx) = &self.update_rx
            && let Ok(r) = rx.try_recv()
        {
            self.update = r;
            self.update_rx = None;
        }
        self.mic_test_shown = false;
        egui::CentralPanel::default().show(ui, |ui| {
            self.update_banner(ui, snap.as_ref());
            ui.horizontal(|ui| {
                style::brand(ui);
                if let Some(s) = &self.session {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let name = s.input_name();
                        if name.is_empty() {
                            ui.colored_label(AMBER, "沒有麥克風（只能聽）");
                        } else {
                            ui.weak(format!("麥克風：{}", short(&name)));
                        }
                    });
                }
            });
            ui.add_space(4.0);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match &snap {
                Some(snap) => self.room(ui, snap),
                None => self.lobby(ui),
            });
        });

        if !self.mic_test_shown {
            self.mic_test = None;
        }
        if self.recording.is_some() || self.mic_test.is_some() || self.install.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(30));
        } else if self.session.is_some() || self.pending.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(80));
        } else if self.update_rx.is_some() {
            // 背景還在查新版本：結果出來時要重畫
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
    }
}

/// 裝置名稱很長（例如「Microphone (Razer Seiren V3 Mini) [Microphone]」），只取括號內的型號
fn short(name: &str) -> &str {
    name.split_once('(').and_then(|(_, r)| r.split_once(')')).map(|(m, _)| m).unwrap_or(name)
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(t) = &self.tray {
            t.set_session(None);
            t.remove();
        }
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

/// 成員音量滑桿：前半段 0～100%、後半段 100～1000%（直接線性的話 0～100% 只佔一小段，很難調）
fn pos_to_gain(pos: f32) -> f32 {
    let pos = pos.clamp(0.0, 1.0);
    if pos <= 0.5 { pos * 2.0 } else { 1.0 + (pos - 0.5) * 18.0 }
}

fn gain_to_pos(gain: f32) -> f32 {
    if gain <= 1.0 { gain.max(0.0) / 2.0 } else { (0.5 + (gain - 1.0) / 18.0).min(1.0) }
}

/// 倍率 → 「150%」（取到 5% 的整數倍）
fn pct(gain: f32) -> String {
    format!("{:.0}%", (gain * 20.0).round() * 5.0)
}

#[cfg(test)]
mod volume_tests {
    use super::*;

    #[test]
    fn slider_mapping() {
        assert_eq!(pos_to_gain(0.0), 0.0);
        assert_eq!(pos_to_gain(0.5), 1.0);
        assert_eq!(pos_to_gain(1.0), 10.0);
        for g in [0.0, 0.3, 1.0, 2.5, 10.0] {
            assert!((pos_to_gain(gain_to_pos(g)) - g).abs() < 1e-4, "{g}");
        }
        assert_eq!(pct(1.0), "100%");
        assert_eq!(pct(10.0), "1000%");
    }
}
