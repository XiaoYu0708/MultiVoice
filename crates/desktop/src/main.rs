// release 版不顯示主控台視窗
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod instance;
mod presets;
mod settings;
mod style;
mod tray;
mod update;

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
    // 同一個設定檔已經在執行：叫回它的視窗，不開新的
    let args: Vec<String> = std::env::args().collect();
    let profile = args.iter().position(|a| a == "--profile").and_then(|i| args.get(i + 1)).cloned().unwrap_or("default".into());
    let Some(_instance) = instance::acquire(&profile, args.iter().any(|a| a == instance::RESTARTED)) else {
        return Ok(());
    };
    update::cleanup_old();
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
            let tray = hwnd.and_then(|h| tray::Tray::start(h, &profile));
            Ok(Box::new(app::App::new(rt, tray)))
        }),
    )
}
