// release 版不顯示主控台視窗
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod overlay;
mod presets;
mod settings;
mod style;
mod tray;

const TITLE: &str = "MultiVoice";

fn main() -> eframe::Result {
    if cfg!(debug_assertions) {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "warn,iroh::net_report=error".into()),
            )
            .init();
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("無法建立 tokio runtime");

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_inner_size([420.0, 640.0])
            .with_min_inner_size([360.0, 480.0])
            .with_icon(style::icon()),
        ..Default::default()
    };
    eframe::run_native(
        TITLE,
        options,
        Box::new(move |cc| {
            style::setup(&cc.egui_ctx);
            // 系統匣需要主視窗的 HWND（藏起來／叫回來都直接用 Win32）
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let hwnd = match cc.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Win32(h)) => Some(h.hwnd.get()),
                _ => None,
            };
            let tray = hwnd.and_then(tray::Tray::start);
            let overlay = hwnd.and_then(overlay::Overlay::start);
            Ok(Box::new(app::App::new(rt, tray, overlay)))
        }),
    )
}
