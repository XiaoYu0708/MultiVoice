//! 啟動失敗、程式崩潰時讓使用者看得到原因。
//!
//! release 版是視窗程式（沒有主控台），panic 或開不了視窗時原本什麼都不會顯示，
//! 使用者只會覺得「雙擊沒反應」。這裡把錯誤寫進 `%APPDATA%\MultiVoice\error.log`，並跳出對話框。

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// 正在用 OpenGL 開視窗：這時 panic（eframe 找不到合適的 OpenGL 設定會 panic，
/// 而且發生在視窗訊息回呼裡，攔不住、程式會直接結束）就改用 wgpu 重新啟動自己，不跳對話框
pub static TRYING_GL: AtomicBool = AtomicBool::new(false);

/// 記住這台電腦要用 wgpu（OpenGL 失敗過）
pub fn wgpu_marker() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("MultiVoice").join("use-wgpu"))
}

/// 記下改用 wgpu，並帶 `--renderer wgpu` 重新啟動自己（新程式會等這個結束才開始）
pub fn relaunch_with_wgpu() {
    if let Some(m) = wgpu_marker() {
        let _ = std::fs::create_dir_all(m.parent().unwrap());
        let _ = std::fs::write(m, "OpenGL 無法使用，改用 wgpu（刪掉這個檔案就會再試 OpenGL）
");
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let mut args = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--renderer" => {
                it.next();
            }
            crate::instance::RESTARTED => {}
            _ => args.push(a),
        }
    }
    args.extend(["--renderer".into(), "wgpu".into(), crate::instance::RESTARTED.into()]);
    match std::process::Command::new(exe).args(&args).spawn() {
        Ok(_) => log("OpenGL 無法使用，改用 wgpu 重新啟動"),
        Err(e) => report(&format!("OpenGL 無法使用，改用 wgpu 重新啟動也失敗：{e}")),
    }
}

pub fn log_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("MultiVoice").join("error.log"))
}

/// 附加一筆紀錄（含版本與時間），檔案太大就從頭寫
pub fn log(msg: &str) {
    let Some(path) = log_path() else { return };
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let append = std::fs::metadata(&path).map(|m| m.len() < 256 * 1024).unwrap_or(false);
    let file = std::fs::OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(&path);
    if let Ok(mut f) = file {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "[unix {secs}] MultiVoice {}：{msg}", env!("CARGO_PKG_VERSION"));
    }
}

/// 寫紀錄並跳出錯誤對話框（會等使用者按確定）
pub fn report(msg: &str) {
    log(msg);
    let where_ = log_path().map(|p| format!("\n\n這段訊息也記在：\n{}", p.display())).unwrap_or_default();
    message_box(&format!("MultiVoice 發生錯誤，無法繼續執行。\n\n{msg}{where_}"));
}

/// panic 時寫紀錄並跳出對話框（只在主執行緒；其他執行緒的 panic 只寫紀錄，避免一直跳視窗）
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "（未知）".into());
        let at = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("?");
        let text = format!("程式錯誤（執行緒 {name}，{at}）：{msg}");
        if name != "main" {
            log(&text);
        } else if TRYING_GL.swap(false, Ordering::Relaxed) {
            log(&text);
            relaunch_with_wgpu();
        } else {
            report(&text);
        }
        default(info);
    }));
}

#[cfg(windows)]
fn message_box(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MessageBoxW};
    let w = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (text, title) = (w(text), w("MultiVoice"));
    unsafe {
        MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR | MB_SETFOREGROUND);
    }
}

#[cfg(not(windows))]
fn message_box(text: &str) {
    eprintln!("{text}");
}
