//! 字型、配色、視窗圖示

use std::sync::Arc;

use eframe::egui::{self, Color32};

pub const GREEN: Color32 = Color32::from_rgb(67, 181, 129);
pub const RED: Color32 = Color32::from_rgb(220, 80, 70);
pub const AMBER: Color32 = Color32::from_rgb(230, 170, 60);
pub const BLUE: Color32 = Color32::from_rgb(66, 120, 220);

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
    ctx.global_style_mut(|s| {
        for font in s.text_styles.values_mut() {
            font.size += 1.0;
        }
        s.spacing.item_spacing.y = 6.0;
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
    });
}

/// 視窗圖示：藍色圓角方塊上的白色麥克風（程式繪製，不需要圖檔）
pub fn icon() -> egui::IconData {
    const N: usize = 64;
    let mut rgba = vec![0u8; N * N * 4];
    for y in 0..N {
        for x in 0..N {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // 圓角方塊背景
            let r = 14.0;
            let dx = ((fx - 32.0).abs() - (30.0 - r)).max(0.0);
            let dy = ((fy - 32.0).abs() - (30.0 - r)).max(0.0);
            if dx * dx + dy * dy > r * r {
                continue;
            }
            // 麥克風頭：圓角膠囊
            let cap = {
                let cy = fy.clamp(19.0, 29.0);
                let (ddx, ddy) = (fx - 32.0, fy - cy);
                ddx * ddx + ddy * ddy <= 7.5 * 7.5
            };
            // 支架：下半圓弧
            let arc = {
                let (ddx, ddy) = (fx - 32.0, fy - 29.0);
                let d = (ddx * ddx + ddy * ddy).sqrt();
                fy >= 29.0 && (11.0..=14.0).contains(&d)
            };
            let stem = (30.5..=33.5).contains(&fx) && (42.0..=49.0).contains(&fy);
            let base = (24.0..=40.0).contains(&fx) && (48.0..=51.0).contains(&fy);
            let c: [u8; 4] = if cap || arc || stem || base { [255, 255, 255, 255] } else { [66, 120, 220, 255] };
            rgba[(y * N + x) * 4..][..4].copy_from_slice(&c);
        }
    }
    egui::IconData { rgba, width: N as u32, height: N as u32 }
}
