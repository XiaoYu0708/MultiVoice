// release 版不顯示主控台視窗
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod crash;
mod icon_shape;
mod instance;
mod presets;
mod settings;
mod style;
mod tray;
mod update;

const TITLE: &str = "MultiVoice";

fn main() {
    if cfg!(debug_assertions) {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "warn,iroh::net_report=error".into()),
            )
            .init();
    }
    crash::install_panic_hook();
    // 同一個設定檔已經在執行：叫回它的視窗，不開新的
    let args: Vec<String> = std::env::args().collect();
    let profile = args.iter().position(|a| a == "--profile").and_then(|i| args.get(i + 1)).cloned().unwrap_or("default".into());
    let Some(_instance) = instance::acquire(&profile, args.iter().any(|a| a == instance::RESTARTED)) else {
        return;
    };
    update::cleanup_old();

    // 先用 OpenGL（glow）；顯示卡驅動沒裝、虛擬機、遠端桌面等 OpenGL 不夠新的環境會開不了視窗，
    // 這時改用 wgpu（DirectX 12，沒有顯示卡時用 Windows 內建的軟體繪製 WARP），並記住下次直接用 wgpu。
    // 測試：`--renderer wgpu|glow` 指定；環境變數 MULTIVOICE_TEST_GL_FAIL=1 讓 OpenGL 故意失敗
    let forced = args.iter().position(|a| a == "--renderer").and_then(|i| args.get(i + 1)).map(String::as_str);
    let use_wgpu = match forced {
        Some("wgpu") => true,
        Some("glow") => false,
        _ => crash::wgpu_marker().is_some_and(|m| m.exists()),
    };
    if use_wgpu {
        if let Err(e) = run(&profile, eframe::Renderer::Wgpu) {
            crash::report(&format!("開不了程式視窗（wgpu）。請更新顯示卡驅動後再試。

{e}"));
        }
        return;
    }
    // OpenGL 失敗有兩種：回傳錯誤（這裡處理）或 panic（crash.rs 的 panic hook 處理）
    crash::TRYING_GL.store(true, std::sync::atomic::Ordering::Relaxed);
    let result = run(&profile, eframe::Renderer::Glow);
    let failed = crash::TRYING_GL.swap(false, std::sync::atomic::Ordering::Relaxed) && result.is_err();
    if let Err(e) = result {
        crash::log(&format!("OpenGL 無法開啟視窗：{e}"));
        if failed {
            drop(_instance);
            crash::relaunch_with_wgpu();
        }
    }
}

fn run(profile: &str, renderer: eframe::Renderer) -> eframe::Result {
    let mut options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_inner_size([420.0, 640.0])
            .with_min_inner_size([360.0, 480.0])
            .with_icon(style::icon()),
        renderer,
        ..Default::default()
    };
    if renderer == eframe::Renderer::Glow && std::env::var_os("MULTIVOICE_TEST_GL_FAIL").is_some() {
        options.multisampling = 255;
    }
    let profile = profile.to_string();
    eframe::run_native(
        TITLE,
        options,
        Box::new(move |cc| {
            // 走到這裡代表視窗與繪製都建好了，之後的 panic 跟 OpenGL 無關
            crash::TRYING_GL.store(false, std::sync::atomic::Ordering::Relaxed);
            style::setup(&cc.egui_ctx);
            let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("無法建立 tokio runtime");
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
