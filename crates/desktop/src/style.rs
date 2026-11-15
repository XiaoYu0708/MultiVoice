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
