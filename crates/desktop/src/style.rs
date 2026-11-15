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
    egui::IconData { rgba: icon_rgba(64, [66, 120, 220], false), width: 64, height: 64 }
}

/// 圓角方塊（底色 `bg`）上的白色麥克風，`n`×`n` 的 RGBA。系統匣用不同底色表示狀態；
/// `slash` = 加一條斜線（靜音，色弱也分得出來）
pub fn icon_rgba(n: usize, bg: [u8; 3], slash: bool) -> Vec<u8> {
    let mut rgba = vec![0u8; n * n * 4];
    // 以 64×64 的座標設計，依 n 縮放；每個像素取 4×4 子取樣做反鋸齒
    let k = 64.0 / n as f32;
    for y in 0..n {
        for x in 0..n {
            let (mut inside, mut white) = (0u32, 0u32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let fx = (x as f32 + (sx as f32 + 0.5) / 4.0) * k;
                    let fy = (y as f32 + (sy as f32 + 0.5) / 4.0) * k;
                    match sample(fx, fy, slash) {
                        0 => {}
                        1 => inside += 1,
                        _ => {
                            inside += 1;
                            white += 1;
                        }
                    }
                }
            }
            if inside == 0 {
                continue;
            }
            let t = white as f32 / inside as f32;
            let px = &mut rgba[(y * n + x) * 4..][..4];
            for c in 0..3 {
                px[c] = (bg[c] as f32 * (1.0 - t) + 255.0 * t).round() as u8;
            }
            px[3] = (inside * 255 / 16) as u8;
        }
    }
    rgba
}

/// 64×64 座標上的一點：0 = 外面、1 = 底色、2 = 麥克風（白）
fn sample(fx: f32, fy: f32, slash: bool) -> u8 {
    // 圓角方塊背景
    let r = 14.0;
    let dx = ((fx - 32.0).abs() - (30.0 - r)).max(0.0);
    let dy = ((fy - 32.0).abs() - (30.0 - r)).max(0.0);
    if dx * dx + dy * dy > r * r {
        return 0;
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
    if slash {
        // 左上到右下的斜線，兩側留一圈底色把麥克風切開
        let d = (fx - fy).abs() / std::f32::consts::SQRT_2;
        let on = (12.0..=52.0).contains(&fx);
        if on && d <= 2.6 {
            return 2;
        }
        if on && d <= 5.2 {
            return 1;
        }
    }
    let stem = (30.5..=33.5).contains(&fx) && (42.0..=49.0).contains(&fy);
    let base = (24.0..=40.0).contains(&fx) && (48.0..=51.0).contains(&fy);
    if cap || arc || stem || base { 2 } else { 1 }
}

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
