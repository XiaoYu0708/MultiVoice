//! 字型、配色、元件外觀（開關、主按鈕、卡片）、視窗圖示
//!
//! 配色與網頁版 `crates/web/www/style.css` 一致；元件手法參考 uiverse.io 常見的寫法
//! （漸層主按鈕、開關、卡片陰影），用 egui 畫出來。

use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, Stroke};

pub const GREEN: Color32 = Color32::from_rgb(67, 181, 129);
pub const RED: Color32 = Color32::from_rgb(220, 80, 70);
pub const AMBER: Color32 = Color32::from_rgb(230, 170, 60);
pub const BLUE: Color32 = Color32::from_rgb(66, 120, 220);
pub const VIOLET: Color32 = Color32::from_rgb(123, 92, 240);

/// 深色／淺色主題的底色
struct Palette {
    bg: Color32,
    panel: Color32,
    panel2: Color32,
    line: Color32,
    fg: Color32,
    /// 音量條等凹下去的底色
    sunken: Color32,
    shadow: Color32,
}

const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x16, 0x18, 0x1d),
    panel: Color32::from_rgb(0x1f, 0x22, 0x29),
    panel2: Color32::from_rgb(0x27, 0x2b, 0x34),
    line: Color32::from_rgb(0x34, 0x39, 0x46),
    fg: Color32::from_rgb(0xe9, 0xeb, 0xf0),
    sunken: Color32::from_rgb(0x12, 0x14, 0x19),
    shadow: Color32::from_black_alpha(70),
};

const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xf4, 0xf5, 0xf7),
    panel: Color32::from_rgb(0xff, 0xff, 0xff),
    panel2: Color32::from_rgb(0xee, 0xf0, 0xf4),
    line: Color32::from_rgb(0xd9, 0xdc, 0xe3),
    fg: Color32::from_rgb(0x1b, 0x1d, 0x22),
    sunken: Color32::from_rgb(0xe4, 0xe7, 0xec),
    shadow: Color32::from_black_alpha(18),
};

fn palette(dark: bool) -> &'static Palette {
    if dark { &DARK } else { &LIGHT }
}

/// 套用配色與圓角（深色、淺色各一份，跟著系統主題）
fn apply_palette(style: &mut egui::Style, p: &Palette) {
    let v = &mut style.visuals;
    v.panel_fill = p.bg;
    v.window_fill = p.panel;
    v.faint_bg_color = p.panel;
    v.extreme_bg_color = p.sunken;
    v.text_edit_bg_color = Some(p.panel2);
    v.window_stroke = Stroke::new(1.0, p.line);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    v.selection.bg_fill = BLUE.lerp_to_gamma(p.panel2, 0.35);
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = BLUE.lerp_to_gamma(Color32::WHITE, 0.25);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.panel;
    w.noninteractive.weak_bg_fill = p.panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.fg);
    w.noninteractive.corner_radius = CornerRadius::same(10);
    let hover_line = p.line.lerp_to_gamma(BLUE, 0.6);
    for (state, tint, stroke) in [
        (&mut w.inactive, 0.0, p.line),
        (&mut w.hovered, 0.14, hover_line),
        (&mut w.active, 0.26, BLUE),
        (&mut w.open, 0.14, hover_line),
    ] {
        let fill = p.panel2.lerp_to_gamma(BLUE, tint);
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
    let p = palette(ui.visuals().dark_mode);
    egui::Frame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0, p.line))
        .corner_radius(12)
        .inner_margin(12)
        .shadow(egui::Shadow { offset: [0, 2], blur: 10, spread: 0, color: p.shadow })
}

/// 頻道卡片：已加入時底色帶一點藍；有人在這個頻道說話時綠框加光暈
pub fn channel_card(ui: &egui::Ui, joined: bool, speaking: bool) -> egui::Frame {
    let p = palette(ui.visuals().dark_mode);
    let mut f = card(ui);
    if joined {
        f = f.fill(p.panel.lerp_to_gamma(BLUE, 0.08)).stroke(Stroke::new(1.0, p.line.lerp_to_gamma(BLUE, 0.55)));
    }
    if speaking {
        let glow = egui::Shadow { offset: [0, 0], blur: 14, spread: 0, color: GREEN.gamma_multiply(0.35) };
        f = f.stroke(Stroke::new(1.5, GREEN)).shadow(glow);
    }
    f
}

/// 主按鈕：藍→紫漸層、滑過變亮並加強光暈、按下時往下沉一點
pub fn primary_button(ui: &mut egui::Ui, enabled: bool, text: &str, size: egui::Vec2) -> egui::Response {
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, text));
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let hover = ui.ctx().animate_bool_responsive(resp.id, enabled && resp.hovered());
    let pressed = enabled && resp.is_pointer_button_down_on();
    let rect = if pressed { rect.translate(egui::vec2(0.0, 1.0)) } else { rect };
    let (mut left, mut right) = (BLUE, VIOLET);
    if !enabled {
        let bg = palette(ui.visuals().dark_mode).panel2;
        (left, right) = (left.lerp_to_gamma(bg, 0.6), right.lerp_to_gamma(bg, 0.6));
    }
    left = left.lerp_to_gamma(Color32::WHITE, 0.1 * hover);
    right = right.lerp_to_gamma(Color32::WHITE, 0.1 * hover);
    let painter = ui.painter();
    let r = 10.0_f32.min(rect.height() / 2.0);
    if enabled {
        let glow = egui::Shadow {
            offset: [0, 4],
            blur: (10.0 + 8.0 * hover) as u8,
            spread: 0,
            color: BLUE.gamma_multiply(0.35 + 0.2 * hover),
        };
        painter.add(glow.as_shape(rect, r));
    }
    // 圓角漸層：兩端各畫一個圓角色塊，中間用網格補上漸層
    painter.rect_filled(egui::Rect::from_min_max(rect.min, egui::pos2(rect.left() + 2.0 * r, rect.bottom())), r, left);
    painter.rect_filled(egui::Rect::from_min_max(egui::pos2(rect.right() - 2.0 * r, rect.top()), rect.max), r, right);
    let mut mesh = egui::Mesh::default();
    let (x0, x1) = (rect.left() + r, rect.right() - r);
    mesh.colored_vertex(egui::pos2(x0, rect.top()), left);
    mesh.colored_vertex(egui::pos2(x1, rect.top()), right);
    mesh.colored_vertex(egui::pos2(x1, rect.bottom()), right);
    mesh.colored_vertex(egui::pos2(x0, rect.bottom()), left);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
    let fg = if enabled { Color32::WHITE } else { Color32::WHITE.gamma_multiply(0.5) };
    painter.text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(17.0), fg);
    resp
}

/// 開關（取代勾選框）：點開關或文字都會切換
pub fn toggle(ui: &mut egui::Ui, on: &mut bool, text: impl Into<egui::WidgetText>) -> egui::Response {
    let (w, h, gap) = (36.0, 20.0, 8.0);
    let wrap = ui.available_width() - w - gap;
    let galley = text.into().into_galley(ui, Some(egui::TextWrapMode::Wrap), wrap, egui::TextStyle::Button);
    let size = egui::vec2(w + gap + galley.size().x, galley.size().y.max(h));
    let (rect, mut resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), *on, galley.text()));
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let p = palette(ui.visuals().dark_mode);
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let track = egui::Rect::from_min_size(egui::pos2(rect.left(), rect.center().y - h / 2.0), egui::vec2(w, h));
    let painter = ui.painter();
    if t > 0.0 {
        let glow = egui::Shadow { offset: [0, 0], blur: 10, spread: 0, color: BLUE.gamma_multiply(0.35 * t) };
        painter.add(glow.as_shape(track, h / 2.0));
    }
    let fill = p.panel2.lerp_to_gamma(BLUE, t);
    let stroke = if resp.hovered() { p.line.lerp_to_gamma(BLUE, 0.7) } else { p.line.lerp_to_gamma(BLUE, t) };
    painter.rect(track, h / 2.0, fill, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    let knob_r = h / 2.0 - 3.0;
    let x = egui::lerp((track.left() + h / 2.0)..=(track.right() - h / 2.0), t);
    let knob = Color32::from_rgb(0x9a, 0xa0, 0xad).lerp_to_gamma(Color32::WHITE, t);
    painter.circle_filled(egui::pos2(x, track.center().y + 0.5), knob_r + 0.5, Color32::from_black_alpha(60));
    painter.circle_filled(egui::pos2(x, track.center().y), knob_r, knob);
    let color = ui.visuals().text_color();
    painter.galley(egui::pos2(track.right() + gap, rect.center().y - galley.size().y / 2.0), galley, color);
    resp
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
        s.spacing.item_spacing.y = 6.0;
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
    });
    ctx.style_mut_of(egui::Theme::Dark, |s| apply_palette(s, &DARK));
    ctx.style_mut_of(egui::Theme::Light, |s| apply_palette(s, &LIGHT));
}

/// 視窗圖示：藍色圓角方塊上的白色麥克風（程式繪製，不需要圖檔）
pub fn icon() -> egui::IconData {
    egui::IconData { rgba: icon_rgba(64, [66, 120, 220], false), width: 64, height: 64 }
}

pub use crate::icon_shape::icon_rgba;

#[cfg(test)]
mod tests {
    /// 手動檢查用：`ICON_DUMP=路徑 cargo test -p desktop dump_icons -- --ignored` 輸出各尺寸、各狀態的 RGBA 原始檔
    #[test]
    #[ignore]
    fn dump_icons() {
        let dir = std::env::var("ICON_DUMP").expect("請設定 ICON_DUMP");
        for n in [16usize, 20, 24, 32] {
            for (i, bg) in [[128, 128, 128], [66, 120, 220], [67, 181, 129], [220, 80, 70]].into_iter().enumerate() {
                std::fs::write(format!("{dir}/icon_{n}_{i}.rgba"), super::icon_rgba(n, bg, i == 3)).unwrap();
            }
        }
    }
}
