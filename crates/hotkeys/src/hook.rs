//! Windows 低階鍵盤／滑鼠 hook（參考 QuickExit `engine.rs`）。只記錄狀態，一律 `CallNextHookEx`。

use std::os::windows::ffi::OsStrExt;
use std::ptr::null_mut;
use std::sync::OnceLock;
use std::sync::mpsc::channel;

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{IsUserAnAdmin, ShellExecuteW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, SW_SHOWNORMAL,
    SetWindowsHookExW, TranslateMessage, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_MBUTTONDOWN,
    WM_MBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

use crate::{Trigger, on_input};

static STARTED: OnceLock<Result<(), String>> = OnceLock::new();

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        match wparam as u32 {
            WM_KEYDOWN | WM_SYSKEYDOWN => on_input(Trigger::Key(kb.vkCode as u16), true),
            WM_KEYUP | WM_SYSKEYUP => on_input(Trigger::Key(kb.vkCode as u16), false),
            _ => {}
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let ms = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        let xbtn = || if (ms.mouseData >> 16) & 0xFFFF == 1 { Trigger::MouseX1 } else { Trigger::MouseX2 };
        match wparam as u32 {
            WM_MBUTTONDOWN => on_input(Trigger::MouseMiddle, true),
            WM_MBUTTONUP => on_input(Trigger::MouseMiddle, false),
            WM_XBUTTONDOWN => on_input(xbtn(), true),
            WM_XBUTTONUP => on_input(xbtn(), false),
            _ => {}
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

pub fn start() -> Result<(), String> {
    STARTED
        .get_or_init(|| {
            // LL hook 必須裝在有訊息迴圈的執行緒上；獨立一條，避免 UI 重繪拖慢輸入
            let (tx, rx) = channel::<Result<(), String>>();
            std::thread::Builder::new()
                .name("hotkeys-hook".into())
                .spawn(move || unsafe {
                    let hmod = GetModuleHandleW(std::ptr::null());
                    let kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), hmod, 0);
                    let ms = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), hmod, 0);
                    if kb.is_null() || ms.is_null() {
                        let _ = tx.send(Err("無法安裝鍵盤／滑鼠監聽".into()));
                        return;
                    }
                    let _ = tx.send(Ok(()));
                    let mut msg: MSG = std::mem::zeroed();
                    while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                })
                .map_err(|e| format!("無法建立監聽執行緒：{e}"))?;
            rx.recv().map_err(|_| "監聽執行緒異常結束".to_string())?
        })
        .clone()
}

pub fn is_admin() -> bool {
    unsafe { IsUserAnAdmin() != 0 }
}

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

pub fn relaunch_as_admin(extra: &[&str]) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !extra.contains(&a.as_str()))
        .chain(extra.iter().map(|s| s.to_string()))
        .map(|a| if a.contains([' ', '"']) { format!("\"{}\"", a.replace('"', "\\\"")) } else { a })
        .collect();
    let (exe, args, verb) = (wide(exe.as_os_str()), wide(args.join(" ").as_ref()), wide("runas".as_ref()));
    let r = unsafe {
        ShellExecuteW(null_mut(), verb.as_ptr(), exe.as_ptr(), args.as_ptr(), std::ptr::null(), SW_SHOWNORMAL)
    };
    r as isize > 32
}
