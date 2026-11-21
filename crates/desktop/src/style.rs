//! 字型、配色、元件外觀（開關、主按鈕、控制按鈕、卡片、音量條、徽章、圖示）、視窗圖示
//!
//! 配色與網頁版 `crates/web/www/style.css` 一致：電競霓虹紫（ui-ux-pro-max 的「Gaming」配色）——
//! 深藍黑底、紫色主色、玫瑰紅表示關閉／危險、霓虹綠表示正在說話。
//! 圖示用 egui 線條畫（系統中文字型沒有 emoji）。

use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Stroke, pos2, vec2};

/// 主色（按鈕、開關、已加入的頻道）
pub const VIOLET: Color32 = Color32::from_rgb(124, 58, 237);
/// 主色漸層的另一端
pub const FUCHSIA: Color32 = Color32::from_rgb(162, 28, 175);
/// 深色底上的紫色文字
pub const VIOLET_HI: Color32 = Color32::from_rgb(167, 139, 250);
/// 關閉／危險（靜音、拒聽、刪除、錯誤）
pub const RED: Color32 = Color32::from_rgb(244, 63, 94);
/// 正在說話、送出中
pub const GREEN: Color32 = Color32::from_rgb(34, 197, 94);
/// 提醒（房主、門檻線、新版本）
pub const AMBER: Color32 = Color32::from_rgb(245, 158, 11);

/// 深色／淺色主題的底色
struct Palette {
    bg: Color32,
    panel: Color32,
    panel2: Color32,
    line: Color32,
    /// 已加入的頻道外框
    line_strong: Color32,
    fg: Color32,
    muted: Color32,
    /// 音量條等凹下去的底色
    sunken: Color32,
    shadow: Color32,
    /// 紫色文字（淺色主題要深一點才看得清楚）
    accent_text: Color32,
}

const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x0f, 0x0f, 0x23),
    panel: Color32::from_rgb(0x1e, 0x1c, 0x35),
    panel2: Color32::from_rgb(0x27, 0x27, 0x3b),
    line: Color32::from_rgb(0x34, 0x30, 0x5a),
    line_strong: Color32::from_rgb(0x6d, 0x3f, 0xd8),
    fg: Color32::from_rgb(0xe2, 0xe8, 0xf0),
    muted: Color32::from_rgb(0x94, 0xa3, 0xb8),
    sunken: Color32::from_rgb(0x0b, 0x0b, 0x1a),
    shadow: Color32::from_black_alpha(90),
    accent_text: VIOLET_HI,
};

const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xf5, 0xf3, 0xff),
    panel: Color32::from_rgb(0xff, 0xff, 0xff),
    panel2: Color32::from_rgb(0xf1, 0xee, 0xfb),
    line: Color32::from_rgb(0xdd, 0xd6, 0xfe),
    line_strong: Color32::from_rgb(0x8b, 0x5c, 0xf6),
    fg: Color32::from_rgb(0x1e, 0x1b, 0x4b),
    muted: Color32::from_rgb(0x5b, 0x56, 0x75),
    sunken: Color32::from_rgb(0xe9, 0xe5, 0xf7),
    shadow: Color32::from_rgba_premultiplied(10, 3, 20, 20),
    accent_text: Color32::from_rgb(0x6d, 0x28, 0xd9),
};

fn palette(dark: bool) -> &'static Palette {
    if dark { &DARK } else { &LIGHT }
}

fn pal(ui: &egui::Ui) -> &'static Palette {
    palette(ui.visuals().dark_mode)
}

/// 次要文字的顏色（跟著主題）
pub fn muted(ui: &egui::Ui) -> Color32 {
    pal(ui).muted
}

/// 紫色文字（跟著主題）
pub fn accent_text(ui: &egui::Ui) -> Color32 {
    pal(ui).accent_text
}

/// 不重要的圓點、分隔線等的顏色
pub fn faint(ui: &egui::Ui) -> Color32 {
    pal(ui).line
}

/// 套用配色與圓角（深色、淺色各一份，跟著系統主題）
fn apply_palette(style: &mut egui::Style, p: &Palette) {
    let v = &mut style.visuals;
    v.panel_fill = p.bg;
    v.window_fill = p.panel;
    v.faint_bg_color = p.panel;
    v.extreme_bg_color = p.sunken;
    v.text_edit_bg_color = Some(p.panel2);
    v.weak_text_color = Some(p.muted);
    v.warn_fg_color = AMBER;
    v.error_fg_color = RED;
    v.window_stroke = Stroke::new(1.0, p.line);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.selection.bg_fill = VIOLET.lerp_to_gamma(p.panel2, 0.25);
    v.selection.stroke = Stroke::new(1.5, p.accent_text);
    v.hyperlink_color = p.accent_text;
    v.slider_trailing_fill = true;
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.panel;
    w.noninteractive.weak_bg_fill = p.panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.fg);
    w.noninteractive.corner_radius = CornerRadius::same(10);
    let hover_line = p.line.lerp_to_gamma(VIOLET, 0.65);
    for (state, tint, stroke) in [
        (&mut w.inactive, 0.0, p.line),
        (&mut w.hovered, 0.16, hover_line),
        (&mut w.active, 0.3, VIOLET),
        (&mut w.open, 0.16, hover_line),
    ] {
        let fill = p.panel2.lerp_to_gamma(VIOLET, tint);
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::new(1.0, stroke);
        state.fg_stroke = Stroke::new(1.0, p.fg);
        state.corner_radius = CornerRadius::same(8);
        state.expansion = 0.0;
    }
}

/// 卡片：圓角、細框、淡淡的陰影
pub fn card(ui: &egui::Ui) -> egui::Frame {
    let p = pal(ui);
    egui::Frame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0, p.line))
        .corner_radius(14)
        .inner_margin(egui::Margin::symmetric(14, 12))
        .shadow(egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: p.shadow })
}

/// 大廳（表單）最寬多少：全螢幕時輸入框和按鈕不要拉滿整個畫面
pub const LOBBY_MAX_W: f32 = 560.0;
/// 房間畫面最寬多少（兩欄時：頻道 + 側欄）
pub const ROOM_MAX_W: f32 = 1240.0;
/// 底部控制列的按鈕最寬多少（靜音、拒聽各約 300）
pub const DOCK_MAX_W: f32 = 720.0;
/// 房間畫面的側欄寬度（房間資訊、我、最近事件）
pub const SIDE_W: f32 = 400.0;
/// 可用寬度到這麼寬就把房間分成兩欄（頻道至少約 480 寬）
pub const TWO_COLUMN_MIN_W: f32 = 900.0;

/// 內容最寬 `max_w` 並水平置中（視窗放大或全螢幕時不要拉滿整個畫面）
pub fn centered<R>(ui: &mut egui::Ui, max_w: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let avail = ui.available_rect_before_wrap();
    let w = avail.width().min(max_w);
    let rect = egui::Rect::from_min_size(egui::pos2(avail.center().x - w / 2.0, avail.top()), egui::vec2(w, avail.height()));
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
        ui.set_width(w);
        add(ui)
    })
    .inner
}

/// 房間畫面底部的控制列（靜音、拒聽、離開）：比背景亮一階、上方一條細線
pub fn dock_frame(ui: &egui::Ui) -> egui::Frame {
    let p = pal(ui);
    egui::Frame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0, p.line))
        .inner_margin(egui::Margin { left: 12, right: 12, top: 10, bottom: 8 })
}

/// 頻道卡片：已加入時底色帶一點紫、紫框；有人在這個頻道說話時綠框加光暈
pub fn channel_card(ui: &egui::Ui, joined: bool, speaking: bool) -> egui::Frame {
    let p = pal(ui);
    let mut f = card(ui);
    if joined {
        f = f.fill(p.panel.lerp_to_gamma(VIOLET, 0.1)).stroke(Stroke::new(1.0, p.line_strong));
    }
    if speaking {
        let glow = egui::Shadow { offset: [0, 0], blur: 18, spread: 0, color: GREEN.gamma_multiply(0.35) };
        f = f.stroke(Stroke::new(1.5, GREEN)).shadow(glow);
    }
    f
}

/// 圓角漸層色塊（左 → 右）：兩端各畫一個圓角色塊，中間用網格補上漸層
fn gradient_rect(painter: &egui::Painter, rect: Rect, r: f32, left: Color32, right: Color32) {
    let r = r.min(rect.height() / 2.0).min(rect.width() / 2.0);
    painter.rect_filled(Rect::from_min_max(rect.min, pos2(rect.left() + 2.0 * r, rect.bottom())), r, left);
    painter.rect_filled(Rect::from_min_max(pos2(rect.right() - 2.0 * r, rect.top()), rect.max), r, right);
    let mut mesh = egui::Mesh::default();
    let (x0, x1) = (rect.left() + r, rect.right() - r);
    mesh.colored_vertex(pos2(x0, rect.top()), left);
    mesh.colored_vertex(pos2(x1, rect.top()), right);
    mesh.colored_vertex(pos2(x1, rect.bottom()), right);
    mesh.colored_vertex(pos2(x0, rect.bottom()), left);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

/// 主按鈕：紫→洋紅漸層、滑過變亮並加強光暈、按下時往下沉一點
pub fn primary_button(ui: &mut egui::Ui, enabled: bool, text: &str, size: egui::Vec2) -> egui::Response {
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, text));
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let hover = ui.ctx().animate_bool_responsive(resp.id, enabled && resp.hovered());
    let pressed = enabled && resp.is_pointer_button_down_on();
    let rect = if pressed { rect.translate(vec2(0.0, 1.0)) } else { rect };
    let (mut left, mut right) = (VIOLET, FUCHSIA);
    if !enabled {
        let bg = pal(ui).panel2;
        (left, right) = (left.lerp_to_gamma(bg, 0.8), right.lerp_to_gamma(bg, 0.8));
    }
    left = left.lerp_to_gamma(Color32::WHITE, 0.1 * hover);
    right = right.lerp_to_gamma(Color32::WHITE, 0.1 * hover);
    let painter = ui.painter();
    let r = 12.0_f32.min(rect.height() / 2.0);
    if enabled {
        let glow = egui::Shadow {
            offset: [0, 4],
            blur: (12.0 + 10.0 * hover) as u8,
            spread: 0,
            color: VIOLET.gamma_multiply(0.4 + 0.2 * hover),
        };
        painter.add(glow.as_shape(rect, r));
    }
    gradient_rect(painter, rect, r, left, right);
    let fg = if enabled { Color32::WHITE } else { Color32::WHITE.gamma_multiply(0.5) };
    painter.text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(17.0), fg);
    if resp.has_focus() {
        painter.rect_stroke(rect.expand(2.0), r + 2.0, Stroke::new(2.0, VIOLET_HI), egui::StrokeKind::Outside);
    }
    resp
}

/// 開關（取代勾選框）：點開關或文字都會切換
pub fn toggle(ui: &mut egui::Ui, on: &mut bool, text: impl Into<egui::WidgetText>) -> egui::Response {
    let (w, h, gap) = (38.0, 22.0, 9.0);
    let wrap = ui.available_width() - w - gap;
    let galley = text.into().into_galley(ui, Some(egui::TextWrapMode::Wrap), wrap, egui::TextStyle::Button);
    let size = vec2(w + gap + galley.size().x, galley.size().y.max(h));
    let (rect, mut resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), *on, galley.text()));
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let p = pal(ui);
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let track = Rect::from_min_size(pos2(rect.left(), rect.center().y - h / 2.0), vec2(w, h));
    let painter = ui.painter();
    if t > 0.0 {
        let glow = egui::Shadow { offset: [0, 0], blur: 12, spread: 0, color: VIOLET.gamma_multiply(0.45 * t) };
        painter.add(glow.as_shape(track, h / 2.0));
    }
    let off = p.sunken;
    gradient_rect(painter, track, h / 2.0, off.lerp_to_gamma(VIOLET, t), off.lerp_to_gamma(FUCHSIA, t));
    let stroke = if resp.hovered() || resp.has_focus() {
        p.line.lerp_to_gamma(VIOLET_HI, 0.8)
    } else {
        p.line.gamma_multiply(1.0 - t)
    };
    painter.rect_stroke(track, h / 2.0, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    let knob_r = h / 2.0 - 3.0;
    let x = egui::lerp((track.left() + h / 2.0)..=(track.right() - h / 2.0), t);
    let knob = p.muted.lerp_to_gamma(Color32::WHITE, t);
    painter.circle_filled(pos2(x, track.center().y + 0.6), knob_r + 0.5, Color32::from_black_alpha(70));
    painter.circle_filled(pos2(x, track.center().y), knob_r, knob);
    let color = ui.visuals().text_color();
    painter.galley(pos2(track.right() + gap, rect.center().y - galley.size().y / 2.0), galley, color);
    resp
}

/// 線條圖示（Lucide 風格）
#[derive(Clone, Copy, PartialEq)]
pub enum Icon {
    Mic,
    Headphones,
    Leave,
    Hash,
    /// 房主（Lucide crown）
    Crown,
}

/// 在 `rect`（正方形）裡畫圖示；`slash`：加一條斜線（關閉）
pub fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32, slash: bool, bg: Color32) {
    let s = rect.width();
    let w = (s * 0.09).max(1.5);
    let st = Stroke::new(w, color);
    let at = |x: f32, y: f32| pos2(rect.left() + x * s, rect.top() + y * s);
    // 半圓弧：角度從 a0 到 a1（弧度，0 = 右、π/2 = 下）
    let arc = |center: Pos2, r: f32, a0: f32, a1: f32| -> Vec<Pos2> {
        (0..=16).map(|i| a0 + (a1 - a0) * i as f32 / 16.0).map(|a| center + r * vec2(a.cos(), a.sin())).collect()
    };
    match icon {
        Icon::Mic => {
            let cap = Rect::from_min_max(at(0.375, 0.08), at(0.625, 0.58));
            painter.rect_stroke(cap, s * 0.125, st, egui::StrokeKind::Middle);
            painter.add(egui::Shape::line(arc(at(0.5, 0.46), s * 0.27, 0.0, std::f32::consts::PI), st));
            painter.line_segment([at(0.5, 0.73), at(0.5, 0.9)], st);
        }
        Icon::Headphones => {
            let top = at(0.5, 0.52);
            painter.add(egui::Shape::line(arc(top, s * 0.36, std::f32::consts::PI, 2.0 * std::f32::consts::PI), st));
            painter.line_segment([at(0.14, 0.52), at(0.14, 0.72)], st);
            painter.line_segment([at(0.86, 0.52), at(0.86, 0.72)], st);
            for (x0, x1) in [(0.12, 0.33), (0.67, 0.88)] {
                painter.rect_filled(Rect::from_min_max(at(x0, 0.56), at(x1, 0.9)), s * 0.07, color);
            }
        }
        Icon::Leave => {
            painter.add(egui::Shape::line(vec![at(0.42, 0.12), at(0.16, 0.12), at(0.16, 0.88), at(0.42, 0.88)], st));
            painter.line_segment([at(0.4, 0.5), at(0.88, 0.5)], st);
            painter.add(egui::Shape::line(vec![at(0.68, 0.3), at(0.88, 0.5), at(0.68, 0.7)], st));
        }
        Icon::Crown => {
            // Lucide crown（24 格座標）：五角的皇冠 + 底線
            let g = |x: f32, y: f32| at(x / 24.0, y / 24.0);
            let crown = vec![g(2.5, 6.0), g(5.3, 16.5), g(18.7, 16.5), g(21.5, 6.0), g(16.2, 10.0), g(12.0, 3.5), g(7.8, 10.0)];
            painter.add(egui::Shape::closed_line(crown, st));
            painter.line_segment([g(5.0, 20.5), g(19.0, 20.5)], st);
        }
        Icon::Hash => {
            for (a, b) in [
                ((0.15, 0.38), (0.85, 0.38)),
                ((0.15, 0.62), (0.85, 0.62)),
                ((0.42, 0.14), (0.34, 0.86)),
                ((0.66, 0.14), (0.58, 0.86)),
            ] {
                painter.line_segment([at(a.0, a.1), at(b.0, b.1)], st);
            }
        }
    }
    if slash {
        let (a, b) = (at(0.1, 0.1), at(0.9, 0.9));
        painter.line_segment([a, b], Stroke::new(w * 2.6, bg));
        painter.line_segment([a, b], st);
    }
}

/// 底部控制列按鈕的樣子
#[derive(Clone, Copy, PartialEq)]
pub enum Ctl {
    /// 一般（例如：麥克風開著）
    Normal,
    /// 已關閉：玫瑰紅底、圖示加斜線（例如：已靜音）
    Off,
    /// 危險動作：玫瑰紅外框與文字（離開）
    Danger,
}

/// 底部控制列的大按鈕：圖示 + 文字，狀態不只靠顏色表示（關閉時圖示有斜線、文字也不同）
pub fn control_button(ui: &mut egui::Ui, icon: Icon, text: &str, kind: Ctl, size: egui::Vec2) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), text));
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let p = pal(ui);
    let hover = ui.ctx().animate_bool_responsive(resp.id, resp.hovered());
    let rect = if resp.is_pointer_button_down_on() { rect.shrink(1.0) } else { rect };
    let (fill, stroke, fg) = match kind {
        Ctl::Normal => (p.panel2.lerp_to_gamma(VIOLET, 0.12 * hover), p.line.lerp_to_gamma(VIOLET, 0.7 * hover), p.fg),
        Ctl::Off => (p.panel2.lerp_to_gamma(RED, 0.22 + 0.08 * hover), RED, RED.lerp_to_gamma(Color32::WHITE, 0.2)),
        Ctl::Danger => (p.panel2.lerp_to_gamma(RED, 0.12 * hover), p.line.lerp_to_gamma(RED, 0.45 + 0.5 * hover), RED),
    };
    let r = 12.0;
    let painter = ui.painter();
    if kind == Ctl::Off {
        let glow = egui::Shadow { offset: [0, 0], blur: 14, spread: 0, color: RED.gamma_multiply(0.25) };
        painter.add(glow.as_shape(rect, r));
    }
    painter.rect(rect, r, fill, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    if resp.has_focus() {
        painter.rect_stroke(rect.expand(2.0), r + 2.0, Stroke::new(2.0, VIOLET_HI), egui::StrokeKind::Outside);
    }
    let font = egui::FontId::proportional(15.0);
    let galley = painter.layout_no_wrap(text.to_owned(), font, fg);
    let icon_s = 18.0;
    let gap = 8.0;
    let total = icon_s + gap + galley.size().x;
    let x0 = rect.center().x - total / 2.0;
    let icon_rect = Rect::from_center_size(pos2(x0 + icon_s / 2.0, rect.center().y), vec2(icon_s, icon_s));
    paint_icon(painter, icon_rect, icon, fg, kind == Ctl::Off, fill);
    painter.galley(pos2(x0 + icon_s + gap, rect.center().y - galley.size().y / 2.0), galley, fg);
    resp
}

/// 頻道卡片的「加入／離開」按鈕：還沒加入時是紫色的，加入後變成低調的外框按鈕
pub fn join_button(ui: &mut egui::Ui, joined: bool) -> egui::Response {
    let p = pal(ui);
    let text = if joined { "離開" } else { "加入" };
    let b = if joined {
        egui::Button::new(egui::RichText::new(text).color(p.muted)).fill(Color32::TRANSPARENT)
    } else {
        egui::Button::new(egui::RichText::new(text).strong().color(Color32::WHITE))
            .fill(VIOLET)
            .stroke(Stroke::new(1.0, VIOLET.lerp_to_gamma(Color32::WHITE, 0.2)))
    };
    ui.add(b.min_size(vec2(60.0, 28.0)).corner_radius(8))
}

/// 小徽章（人數、狀態）：圓角膠囊、淡紫底
pub fn badge(ui: &mut egui::Ui, text: impl Into<String>, color: Color32) -> egui::Response {
    let font = egui::FontId::proportional(12.5);
    let galley = ui.painter().layout_no_wrap(text.into(), font, color);
    let size = galley.size() + vec2(16.0, 6.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect(rect, rect.height() / 2.0, color.gamma_multiply(0.14), Stroke::new(1.0, color.gamma_multiply(0.4)), egui::StrokeKind::Inside);
        painter.galley(rect.center() - galley.size() / 2.0, galley, color);
    }
    resp
}

/// 房主圖示（皇冠，琥珀色）：只有圖示，滑鼠移上去說明是房主（圖示本身沒有文字，要有說明）
pub fn owner_icon(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        paint_icon(ui.painter(), rect, Icon::Crown, AMBER, false, Color32::TRANSPARENT);
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, "房主"));
    resp.on_hover_text("房主：可以改名、刪除頻道、移出成員、更換邀請")
}

/// 狀態列的房主標記：皇冠 + 名字（`None` = 房主不在房間裡，皇冠變灰）
pub fn owner_chip(ui: &mut egui::Ui, name: Option<&str>) -> egui::Response {
    let color = if name.is_some() { AMBER } else { faint(ui) };
    let font = egui::FontId::proportional(12.5);
    let galley = ui.painter().layout_no_wrap(name.unwrap_or("").to_string(), font, color);
    let icon = 14.0;
    let gap = if name.is_some() { 4.0 } else { 0.0 };
    let size = vec2(8.0 + icon + gap + galley.size().x + 8.0, galley.size().y.max(icon) + 6.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect(rect, rect.height() / 2.0, color.gamma_multiply(0.14), Stroke::new(1.0, color.gamma_multiply(0.4)), egui::StrokeKind::Inside);
        let ir = Rect::from_min_size(pos2(rect.left() + 8.0, rect.center().y - icon / 2.0), vec2(icon, icon));
        paint_icon(painter, ir, Icon::Crown, color, false, Color32::TRANSPARENT);
        painter.galley(pos2(ir.right() + gap, rect.center().y - galley.size().y / 2.0), galley, color);
    }
    let label = match name {
        Some(n) => format!("房主：{n}"),
        None => "房主不在房間裡".to_string(),
    };
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, label.clone()));
    resp.on_hover_text(match name {
        Some(_) => format!("{label}（可以改名、刪除頻道、移出成員、更換邀請）"),
        None => "房主不在房間裡：改名、刪除頻道、移出成員要等他回來".to_string(),
    })
}

/// 區段標題（例如「頻道」）
pub fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(15.5).strong().color(ui.visuals().strong_text_color()));
}

/// 頻道圖示方塊：未加入灰底、加入紫色漸層、正在說話綠色
pub fn channel_icon(ui: &mut egui::Ui, joined: bool, speaking: bool) {
    let p = pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(28.0, 28.0), egui::Sense::hover());
    let painter = ui.painter();
    let fg = if joined || speaking { Color32::WHITE } else { p.muted };
    if speaking {
        painter.rect_filled(rect, 8.0, GREEN);
    } else if joined {
        gradient_rect(painter, rect, 8.0, VIOLET, FUCHSIA);
    } else {
        painter.rect_filled(rect, 8.0, p.panel2);
    }
    paint_icon(painter, rect.shrink(7.0), Icon::Hash, fg, false, Color32::TRANSPARENT);
}

/// 音量條：凹槽 + 漸層（說話時綠色、靜音時灰色）；`marks`：(dB 對應的比例, 顏色) 畫成直線
pub fn meter(ui: &mut egui::Ui, frac: f32, voiced: bool, dimmed: bool, height: f32, marks: &[(f32, Color32)]) {
    let p = pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), egui::Sense::hover());
    let painter = ui.painter();
    let r = height / 2.0;
    painter.rect(rect, r, p.sunken, Stroke::new(1.0, p.line), egui::StrokeKind::Inside);
    let frac = frac.clamp(0.0, 1.0);
    if frac > 0.0 {
        let mut fill = rect;
        fill.set_right(rect.left() + (rect.width() * frac).max(height));
        if dimmed {
            painter.rect_filled(fill, r, p.muted.gamma_multiply(0.5));
        } else if voiced {
            painter.add(egui::Shadow { offset: [0, 0], blur: 8, spread: 0, color: GREEN.gamma_multiply(0.4) }.as_shape(fill, r));
            gradient_rect(painter, fill, r, GREEN.gamma_multiply(0.7), GREEN);
        } else {
            gradient_rect(painter, fill, r, VIOLET, FUCHSIA);
        }
    }
    for &(f, color) in marks {
        let x = rect.left() + rect.width() * f.clamp(0.0, 1.0);
        painter.line_segment([pos2(x, rect.top() - 2.0), pos2(x, rect.bottom() + 2.0)], Stroke::new(2.0, color));
    }
}

/// 標題列的品牌：漸層圓角方塊（四條聲波）+ 名稱
pub fn brand(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(28.0, 28.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.add(egui::Shadow { offset: [0, 2], blur: 12, spread: 0, color: VIOLET.gamma_multiply(0.5) }.as_shape(rect, 8.0));
    gradient_rect(painter, rect, 8.0, VIOLET, FUCHSIA);
    let s = rect.width() / 512.0;
    let st = Stroke::new((34.0 * s).max(2.0), Color32::WHITE);
    for (x, y0, y1) in [(146.0, 196.0, 316.0), (216.0, 146.0, 366.0), (286.0, 186.0, 326.0), (356.0, 216.0, 296.0)] {
        painter.line_segment([rect.min + vec2(x, y0) * s, rect.min + vec2(x, y1) * s], st);
    }
    ui.add_space(2.0);
    ui.label(egui::RichText::new("MultiVoice").size(20.0).strong().color(ui.visuals().strong_text_color()));
}

/// 載入系統中文字型（egui 內建字型沒有中文），字體稍微放大
pub fn setup(ctx: &egui::Context) {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let mut fonts = egui::FontDefinitions::default();
    for file in ["msjh.ttc", "msyh.ttc", "mingliu.ttc", "simsun.ttc"] {
        if let Ok(bytes) = std::fs::read(format!("{windir}\\Fonts\\{file}")) {
            fonts.font_data.insert("cjk".into(), Arc::new(egui::FontData::from_owned(bytes)));
            fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "cjk".into());
            fonts.families.entry(egui::FontFamily::Monospace).or_default().push("cjk".into());
            break;
        }
    }
    ctx.set_fonts(fonts);
    ctx.all_styles_mut(|s| {
        for font in s.text_styles.values_mut() {
            font.size += 1.0;
        }
        s.spacing.item_spacing = vec2(8.0, 7.0);
        s.spacing.button_padding = vec2(11.0, 5.0);
        s.spacing.interact_size.y = 24.0;
        s.spacing.slider_rail_height = 6.0;
    });
    ctx.style_mut_of(egui::Theme::Dark, |s| apply_palette(s, &DARK));
    ctx.style_mut_of(egui::Theme::Light, |s| apply_palette(s, &LIGHT));
}

/// 視窗圖示：跟 exe、網頁版同一個圖案（程式繪製，不需要圖檔）
pub fn icon() -> egui::IconData {
    egui::IconData { rgba: crate::icon_shape::app_icon_rgba(128), width: 128, height: 128 }
}

pub use crate::icon_shape::icon_rgba;

#[cfg(test)]
mod tests {
    /// 手動檢查用：`ICON_DUMP=路徑 cargo test -p desktop dump_icons -- --ignored` 輸出各尺寸、各狀態的 RGBA 原始檔
    #[test]
    #[ignore]
    fn dump_icons() {
        let dir = std::env::var("ICON_DUMP").expect("請設定 ICON_DUMP");
        for n in [16usize, 32, 48, 256] {
            std::fs::write(format!("{dir}/app_{n}.rgba"), crate::icon_shape::app_icon_rgba(n)).unwrap();
        }
        for n in [16usize, 20, 24, 32] {
            for (i, bg) in [[128, 128, 128], [124, 58, 237], [34, 197, 94], [244, 63, 94]].into_iter().enumerate() {
                std::fs::write(format!("{dir}/icon_{n}_{i}.rgba"), super::icon_rgba(n, bg, i == 3)).unwrap();
            }
        }
    }
}
