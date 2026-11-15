//! 主畫面：大廳（建立／加入房間、音訊設定）與房間（成員、說話指示、靜音／拒聽）。

use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use session::{Connected, Relays, Session, Snapshot};

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
                self.session = Some(s);
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
        ui.label("開麥方式");
        ui.radio_value(&mut self.settings.open_mic, false, "偵測到說話才送出（建議）");
        ui.radio_value(&mut self.settings.open_mic, true, "一直開麥");

        ui.add_space(6.0);
        ui.checkbox(&mut self.settings.denoise, "降噪（消除風扇、鍵盤等背景聲）");
        if self.settings.denoise {
            ui.add(egui::Slider::new(&mut self.settings.speech_mix, 0.0..=0.6).text("小聲講話保留"));
            ui.weak("調高：小聲的字更清楚，但講話時會多一點背景聲");
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
        // 自己
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
        });

        ui.add_space(6.0);
        ui.label(RichText::new(format!("成員（{}）", snap.members.len() + 1)).strong());
        egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(ui.available_height() - 40.0).show(ui, |ui| {
            if snap.members.is_empty() {
                ui.weak("目前只有你。複製邀請票給朋友吧。");
            }
            for m in &snap.members {
                ui.horizontal(|ui| {
                    dot(ui, if m.speaking { GREEN } else { Color32::DARK_GRAY }, m.speaking);
                    ui.label(&m.name);
                    ui.weak(format!("#{}", m.idx));
                });
            }
        });

        if let Some(why) = &snap.disconnected {
            ui.add_space(6.0);
            ui.colored_label(RED, format!("連線中斷：{why}"));
        }
        egui::CollapsingHeader::new("最近事件").default_open(false).show(ui, |ui| {
            for line in snap.log.iter().rev().take(10) {
                ui.weak(line);
            }
        });
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

        let snap = self.session.as_ref().map(|s| s.snapshot());
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
            match &snap {
                Some(snap) => self.room(ui, snap),
                None => self.lobby(ui),
            }
        });

        if self.session.is_some() || self.pending.is_some() {
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
