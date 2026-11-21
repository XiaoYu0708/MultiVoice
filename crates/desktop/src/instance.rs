//! 只開一個：同一個設定檔（`--profile`）已經在執行時，再執行 exe 不會開新的，而是把原本的視窗叫回前面
//! （包括縮在系統匣裡的）。
//!
//! - 用具名 mutex（`Local\MultiVoice-<設定檔>`）判斷有沒有在執行；持有到程式結束
//! - 叫回視窗：找原本那個程式的系統匣隱藏視窗（[`crate::tray::window_name`]），傳 [`crate::tray::WM_SHOW_MAIN`]
//! - 自己重新啟動自己（一鍵更新、以系統管理員重新啟動）時帶 `--restarted`：舊程式還在結束中（離開房間最多 2 秒），
//!   這時要等它結束，不是叫醒它

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};

/// 重新啟動自己時帶的參數
pub const RESTARTED: &str = "--restarted";

/// 持有中的 mutex（程式結束時由系統釋放）
pub struct Guard {
    #[cfg(windows)]
    _h: HANDLE,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// 取得「這個設定檔只有我在跑」的鎖。
/// 已經有人在跑：`wait` = false 就叫回它的視窗並回傳 None（呼叫端直接結束）；`wait` = true 就等它結束（最多 15 秒）
pub fn acquire(profile: &str, wait: bool) -> Option<Guard> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::CreateMutexW;
        let name = wide(&format!("Local\\MultiVoice-{profile}"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let h = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
            if h.is_null() {
                // 建不起來（權限等）：不擋使用者
                return Some(Guard { _h: h });
            }
            if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
                return Some(Guard { _h: h });
            }
            unsafe { CloseHandle(h) };
            if !wait {
                show_existing(profile);
                return None;
            }
            if std::time::Instant::now() > deadline {
                // 舊的一直沒結束：還是開起來，總比打不開好
                return Some(Guard { _h: std::ptr::null_mut() });
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (profile, wait);
        Some(Guard {})
    }
}

/// 這個設定檔已經有一個在跑（找得到它的系統匣視窗）就叫回它、回傳 true；不等待。
/// 一般權限時用（以管理員執行的那一個建的 mutex，一般權限打不開，只能找視窗）
pub fn show_running(profile: &str) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;
        let (class, title) = (wide(crate::tray::CLASS), wide(&crate::tray::window_name(profile)));
        if unsafe { FindWindowW(class.as_ptr(), title.as_ptr()) }.is_null() {
            return false;
        }
        show_existing(profile);
        true
    }
    #[cfg(not(windows))]
    {
        let _ = profile;
        false
    }
}

/// 叫回已經在執行的那一個（它可能剛啟動、系統匣還沒建好：找不到就再等一下）
#[cfg(windows)]
fn show_existing(profile: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, PostMessageW,
    };
    let class = wide(crate::tray::CLASS);
    let title = wide(&crate::tray::window_name(profile));
    for _ in 0..25 {
        let h = unsafe { FindWindowW(class.as_ptr(), title.as_ptr()) };
        if !h.is_null() {
            unsafe {
                // 我是使用者剛點開的（在前景）：讓對方可以把自己的視窗拉到最前面
                let mut pid = 0u32;
                GetWindowThreadProcessId(h, &mut pid);
                AllowSetForegroundWindow(pid);
                PostMessageW(h, crate::tray::WM_SHOW_MAIN, 0, 0);
            }
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}
