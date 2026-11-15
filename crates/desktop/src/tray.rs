//! 系統匣圖示（Windows）。
//!
//! 不用 `tray-icon` crate：它要在 UI 執行緒更新圖示，但 egui 視窗藏起來後畫面迴圈就停了，
//! 遊戲中（視窗縮在系統匣）反而看不到說話狀態。這裡開一條獨立執行緒，自己跑訊息迴圈與計時器，
//! 直接讀 session 的 [`Shared`]（本來就跨執行緒）更新圖示：
//!
//! - 灰：沒進房間　藍：在房間　綠：送出中　紅：靜音／拒聽
//! - 左鍵：叫回主視窗　右鍵：選單（顯示、靜音、拒聽、結束）

use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use session::pipeline::Shared;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, MSGFLT_ALLOW,
    AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, HICON, IsIconic, MF_CHECKED, MF_SEPARATOR,
    MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SM_CXSMICON,
    SW_HIDE, SW_RESTORE, SW_SHOW, SendMessageW, SetForegroundWindow, SetTimer, ShowWindow, TPM_BOTTOMALIGN,
    TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_LBUTTONUP,
    WM_NULL, WM_RBUTTONUP, WM_TIMER, WNDCLASSW,
};

/// 圖示事件（Shell 回呼）
const WM_TRAY: u32 = WM_APP + 1;
/// UI 執行緒要求移除圖示並結束執行緒（同步）
const WM_TRAY_REMOVE: u32 = WM_APP + 2;
/// UI 執行緒要求顯示「仍在執行」通知
const WM_TRAY_BALLOON: u32 = WM_APP + 3;
/// 叫回主視窗（再執行一次 exe 時，新的那個傳過來，見 instance.rs）
pub const WM_SHOW_MAIN: u32 = WM_APP + 4;
/// 系統匣隱藏視窗的類別名稱
pub const CLASS: &str = "MultiVoiceTray";

/// 系統匣隱藏視窗的標題（每個設定檔不同，instance.rs 用它找到已經在執行的那一個）
pub fn window_name(profile: &str) -> String {
    format!("MultiVoiceTray:{profile}")
}

const ID_SHOW: usize = 1;
const ID_MUTE: usize = 2;
const ID_DEAFEN: usize = 3;
const ID_QUIT: usize = 9;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Look {
    Idle,
    InRoom,
    Talking,
    Muted,
}

/// UI 執行緒與系統匣執行緒共用
pub struct Tray {
    /// 目前的工作階段（沒進房間時 None）
    shared: Mutex<Option<Arc<Shared>>>,
    /// 使用者從系統匣選了「結束」：主視窗的關閉要求不要再縮到系統匣
    quit: AtomicBool,
    main: AtomicIsize,
    tray_hwnd: AtomicIsize,
}

static TRAY: OnceLock<Arc<Tray>> = OnceLock::new();

impl Tray {
    /// 建立系統匣圖示（整個程式一個）。`main` 是主視窗的 HWND
    pub fn start(main: isize, profile: &str) -> Option<Arc<Tray>> {
        let tray = TRAY
            .get_or_init(|| {
                Arc::new(Tray {
                    shared: Mutex::new(None),
                    quit: AtomicBool::new(false),
                    main: AtomicIsize::new(main),
                    tray_hwnd: AtomicIsize::new(0),
                })
            })
            .clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let name = window_name(profile);
        std::thread::Builder::new()
            .name("tray".into())
            .spawn(move || unsafe { run(tx, &name) })
            .ok()?;
        rx.recv().ok().flatten().map(|_| tray)
    }

    pub fn set_session(&self, shared: Option<Arc<Shared>>) {
        *self.shared.lock().unwrap() = shared;
    }

    pub fn quit_requested(&self) -> bool {
        self.quit.load(Ordering::Relaxed)
    }

    /// 把主視窗藏到系統匣；`notify` = 顯示「仍在執行」通知
    pub fn hide_main(&self, notify: bool) {
        unsafe {
            ShowWindow(self.main.load(Ordering::Relaxed) as HWND, SW_HIDE);
            if notify {
                PostMessageW(self.tray_hwnd.load(Ordering::Relaxed) as HWND, WM_TRAY_BALLOON, 0, 0);
            }
        }
    }

    /// 移除圖示（程式結束前呼叫；等系統匣執行緒處理完才返回）
    pub fn remove(&self) {
        let h = self.tray_hwnd.swap(0, Ordering::SeqCst);
        if h != 0 {
            unsafe { SendMessageW(h as HWND, WM_TRAY_REMOVE, 0, 0) };
        }
    }
}

fn tray() -> &'static Arc<Tray> {
    TRAY.get().expect("系統匣尚未建立")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// 把字串寫進固定長度的 UTF-16 欄位（截斷、補 0）
fn fill(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

/// RGBA → HICON（32 bpp，含 alpha）
unsafe fn make_icon(look: Look) -> HICON {
    let n = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64) as usize;
    let bg = match look {
        Look::Idle => [128, 128, 128],
        Look::InRoom => [66, 120, 220],
        Look::Talking => [67, 181, 129],
        Look::Muted => [220, 80, 70],
    };
    let mut px = crate::style::icon_rgba(n, bg, look == Look::Muted);
    // Windows 要 BGRA；AND 遮罩一個像素一個位元組也可以（32 bpp 時以 alpha 為準）
    let mut and_mask = Vec::with_capacity(n * n);
    for p in px.chunks_exact_mut(4) {
        p.swap(0, 2);
        and_mask.push(255 - p[3]);
    }
    unsafe { CreateIcon(null_mut(), n as i32, n as i32, 1, 32, and_mask.as_ptr(), px.as_ptr()) }
}

struct Icons {
    idle: HICON,
    in_room: HICON,
    talking: HICON,
    muted: HICON,
}

impl Icons {
    fn get(&self, l: Look) -> HICON {
        match l {
            Look::Idle => self.idle,
            Look::InRoom => self.in_room,
            Look::Talking => self.talking,
            Look::Muted => self.muted,
        }
    }
}

/// 系統匣執行緒的狀態（只在這條執行緒用）
struct Local {
    icons: Icons,
    look: Look,
    taskbar_created: u32,
}

thread_local! {
    static LOCAL: std::cell::RefCell<Option<Local>> = const { std::cell::RefCell::new(None) };
}

fn current_look() -> Look {
    match tray().shared.lock().unwrap().as_ref() {
        None => Look::Idle,
        Some(s) if s.is_muted() => Look::Muted,
        Some(s) if s.tx_active.load(Ordering::Relaxed) => Look::Talking,
        Some(_) => Look::InRoom,
    }
}

fn tip(look: Look) -> &'static str {
    match look {
        Look::Idle => "MultiVoice",
        Look::InRoom => "MultiVoice - 在房間中",
        Look::Talking => "MultiVoice - 送出中",
        Look::Muted => "MultiVoice - 已靜音",
    }
}

unsafe fn notify(hwnd: HWND, op: u32, look: Look, balloon: bool) {
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = LOCAL.with(|l| l.borrow().as_ref().map(|l| l.icons.get(look)).unwrap_or(null_mut()));
    fill(&mut nid.szTip, tip(look));
    if balloon {
        nid.uFlags |= NIF_INFO;
        nid.dwInfoFlags = NIIF_INFO;
        fill(&mut nid.szInfoTitle, "MultiVoice 仍在執行");
        fill(&mut nid.szInfo, "語音持續連線中。點系統匣圖示叫回視窗，右鍵可以結束。");
    }
    unsafe { Shell_NotifyIconW(op, &nid) };
}

fn show_main() {
    let h = tray().main.load(Ordering::Relaxed) as HWND;
    unsafe {
        ShowWindow(h, SW_SHOW);
        if IsIconic(h) != 0 {
            ShowWindow(h, SW_RESTORE);
        }
        SetForegroundWindow(h);
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let shared = tray().shared.lock().unwrap().clone();
    unsafe {
        let m = CreatePopupMenu();
        AppendMenuW(m, MF_STRING, ID_SHOW, wide("顯示 MultiVoice").as_ptr());
        if let Some(s) = &shared {
            let check = |on: bool| if on { MF_STRING | MF_CHECKED } else { MF_STRING };
            AppendMenuW(m, MF_SEPARATOR, 0, null_mut());
            AppendMenuW(m, check(s.is_muted()), ID_MUTE, wide("靜音").as_ptr());
            AppendMenuW(m, check(s.is_deafened()), ID_DEAFEN, wide("拒聽").as_ptr());
        }
        AppendMenuW(m, MF_SEPARATOR, 0, null_mut());
        AppendMenuW(m, MF_STRING, ID_QUIT, wide("結束").as_ptr());
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // 沒有先設前景，點選單外面時選單不會消失（Win32 的老規矩）
        SetForegroundWindow(hwnd);
        TrackPopupMenu(m, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, null_mut());
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(m);
    }
}

fn on_command(id: usize) {
    let shared = tray().shared.lock().unwrap().clone();
    match (id, shared) {
        (ID_SHOW, _) => show_main(),
        (ID_MUTE, Some(s)) => s.toggle_mute(),
        (ID_DEAFEN, Some(s)) => s.set_deafened(!s.is_deafened()),
        (ID_QUIT, _) => {
            tray().quit.store(true, Ordering::Relaxed);
            // 視窗藏著時 egui 不會跑畫面迴圈、處理不了關閉要求：先叫出來再關
            show_main();
            unsafe { PostMessageW(tray().main.load(Ordering::Relaxed) as HWND, WM_CLOSE, 0, 0) };
        }
        _ => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => match (lparam & 0xFFFF) as u32 {
            WM_LBUTTONUP => show_main(),
            WM_RBUTTONUP => unsafe { show_menu(hwnd) },
            _ => {}
        },
        WM_COMMAND => on_command(wparam & 0xFFFF),
        WM_TIMER => {
            let look = current_look();
            let changed = LOCAL.with(|l| {
                let mut l = l.borrow_mut();
                let l = l.as_mut().unwrap();
                std::mem::replace(&mut l.look, look) != look
            });
            if changed {
                unsafe { notify(hwnd, NIM_MODIFY, look, false) };
            }
        }
        WM_TRAY_BALLOON => unsafe { notify(hwnd, NIM_MODIFY, current_look(), true) },
        WM_SHOW_MAIN => show_main(),
        WM_TRAY_REMOVE => unsafe {
            notify(hwnd, NIM_DELETE, Look::Idle, false);
            DestroyWindow(hwnd);
        },
        WM_DESTROY => unsafe { PostQuitMessage(0) },
        m if LOCAL.with(|l| l.borrow().as_ref().is_some_and(|l| l.taskbar_created == m)) => {
            // 檔案總管重新啟動：圖示要重新加回去
            unsafe { notify(hwnd, NIM_ADD, current_look(), false) };
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    0
}

/// 系統匣執行緒：建立隱藏視窗與圖示，跑訊息迴圈
unsafe fn run(ready: std::sync::mpsc::Sender<Option<()>>, name: &str) {
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let class = wide(CLASS);
        let title = wide(name);
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst, lpszClassName: class.as_ptr(), ..std::mem::zeroed() };
        RegisterClassW(&wc);
        // 一般的隱藏頂層視窗（不用 message-only，才收得到 TaskbarCreated 廣播）
        let hwnd = CreateWindowExW(0, class.as_ptr(), title.as_ptr(), 0, 0, 0, 0, 0, null_mut(), null_mut(), hinst, std::ptr::null());
        if hwnd.is_null() {
            let _ = ready.send(None);
            return;
        }
        // 我以系統管理員執行、再點 exe 的是一般權限時，Windows 預設會擋掉它傳來的訊息
        ChangeWindowMessageFilterEx(hwnd, WM_SHOW_MAIN, MSGFLT_ALLOW, null_mut());
        let icons = Icons {
            idle: make_icon(Look::Idle),
            in_room: make_icon(Look::InRoom),
            talking: make_icon(Look::Talking),
            muted: make_icon(Look::Muted),
        };
        let taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        LOCAL.with(|l| *l.borrow_mut() = Some(Local { icons, look: Look::Idle, taskbar_created }));
        notify(hwnd, NIM_ADD, Look::Idle, false);
        tray().tray_hwnd.store(hwnd as isize, Ordering::SeqCst);
        SetTimer(hwnd, 1, 150, None);
        let _ = ready.send(Some(()));

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
