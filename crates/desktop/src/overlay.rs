//! 置頂的「誰在說話」小視窗（Windows）。
//!
//! 跟系統匣一樣是獨立執行緒 + Win32 視窗：主視窗縮到系統匣時 egui 不會更新，這個視窗仍要繼續顯示。
//! - 半透明、置頂、滑鼠穿透（`WS_EX_TRANSPARENT`）、不搶焦點、不出現在工作列；不注入遊戲
//! - 有人說話才出現；MultiVoice 主視窗在前景時隱藏（主視窗本身就看得到）
//! - 遊戲用「獨佔全螢幕」時任何視窗都蓋不上去，要用「無邊框視窗」模式才看得到

use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use session::{SpeakFeed, SpeakLine};
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DeleteObject, Ellipse,
    EndPaint, FW_BOLD, FW_NORMAL, FillRect, GetTextExtentPoint32W, HDC, HFONT, InvalidateRect, PAINTSTRUCT,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT, TextOutW, GetDC, ReleaseDC, GetStockObject, NULL_PEN,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetForegroundWindow, GetMessageW, HWND_TOPMOST, LWA_ALPHA,
    MSG, RegisterClassW, SPI_GETWORKAREA, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetLayeredWindowAttributes,
    SetTimer, SetWindowPos, ShowWindow, SystemParametersInfoW, TranslateMessage, WM_PAINT, WM_TIMER, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

/// 放在螢幕哪個角落
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    pub const ALL: [Corner; 4] = [Corner::TopLeft, Corner::TopRight, Corner::BottomLeft, Corner::BottomRight];

    pub fn label(self) -> &'static str {
        match self {
            Corner::TopLeft => "左上",
            Corner::TopRight => "右上",
            Corner::BottomLeft => "左下",
            Corner::BottomRight => "右下",
        }
    }

    /// 存進設定檔的字串
    pub fn key(self) -> &'static str {
        match self {
            Corner::TopLeft => "top_left",
            Corner::TopRight => "top_right",
            Corner::BottomLeft => "bottom_left",
            Corner::BottomRight => "bottom_right",
        }
    }

    pub fn from_key(s: &str) -> Corner {
        Corner::ALL.into_iter().find(|c| c.key() == s).unwrap_or(Corner::TopLeft)
    }
}

pub struct Overlay {
    feed: Mutex<Option<SpeakFeed>>,
    enabled: AtomicBool,
    corner: AtomicU8,
    main: AtomicIsize,
}

static OVERLAY: OnceLock<Arc<Overlay>> = OnceLock::new();

impl Overlay {
    pub fn start(main: isize) -> Option<Arc<Overlay>> {
        let o = OVERLAY
            .get_or_init(|| {
                Arc::new(Overlay {
                    feed: Mutex::new(None),
                    enabled: AtomicBool::new(true),
                    corner: AtomicU8::new(0),
                    main: AtomicIsize::new(main),
                })
            })
            .clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("overlay".into()).spawn(move || unsafe { run(tx) }).ok()?;
        rx.recv().ok().flatten().map(|_| o)
    }

    pub fn set_feed(&self, feed: Option<SpeakFeed>) {
        *self.feed.lock().unwrap() = feed;
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    pub fn set_corner(&self, c: Corner) {
        let i = Corner::ALL.iter().position(|&x| x == c).unwrap_or(0);
        self.corner.store(i as u8, Ordering::Relaxed);
    }
}

fn overlay() -> &'static Arc<Overlay> {
    OVERLAY.get().expect("小視窗尚未建立")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

const BG: COLORREF = rgb(24, 24, 28);
const NAME: COLORREF = rgb(240, 240, 240);
const CHANNEL: COLORREF = rgb(160, 160, 170);
const DOT_OTHER: COLORREF = rgb(67, 181, 129);
const DOT_ME: COLORREF = rgb(66, 120, 220);

/// 視窗執行緒的狀態
struct Local {
    lines: Vec<SpeakLine>,
    visible: bool,
    corner: u8,
    /// (dpi, 名稱字型, 頻道字型)
    fonts: Option<(u32, HFONT, HFONT)>,
}

thread_local! {
    static LOCAL: std::cell::RefCell<Local> = const {
        std::cell::RefCell::new(Local { lines: Vec::new(), visible: false, corner: 0, fonts: None })
    };
}

/// 依 DPI 建字型（DPI 變了才重建）
unsafe fn fonts(hwnd: HWND) -> (f32, HFONT, HFONT) {
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let scale = dpi as f32 / 96.0;
    LOCAL.with(|l| {
        let mut l = l.borrow_mut();
        if let Some((d, a, b)) = l.fonts
            && d == dpi
        {
            return (scale, a, b);
        }
        if let Some((_, a, b)) = l.fonts.take() {
            unsafe {
                DeleteObject(a as _);
                DeleteObject(b as _);
            }
        }
        let face: Vec<u16> = "Microsoft JhengHei UI".encode_utf16().chain(Some(0)).collect();
        let mk = |px: f32, weight: u32| unsafe {
            CreateFontW(
                -(px * scale).round() as i32,
                0,
                0,
                0,
                weight as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                0,
                0,
                CLEARTYPE_QUALITY as u32,
                0,
                face.as_ptr(),
            )
        };
        let (a, b) = (mk(17.0, FW_BOLD), mk(15.0, FW_NORMAL));
        l.fonts = Some((dpi, a, b));
        (scale, a, b)
    })
}

/// 每行的版面：(名稱文字, 頻道文字)
fn texts(line: &SpeakLine) -> (Vec<u16>, Vec<u16>) {
    let name = if line.me { format!("{}（我）", line.name) } else { line.name.clone() };
    let ch = if line.channels.is_empty() { String::new() } else { format!("  {}", line.channels.join("、")) };
    (wide(&name), wide(&ch))
}

unsafe fn text_width(dc: HDC, font: HFONT, s: &[u16]) -> i32 {
    if s.is_empty() {
        return 0;
    }
    let mut sz = SIZE { cx: 0, cy: 0 };
    unsafe {
        SelectObject(dc, font as _);
        GetTextExtentPoint32W(dc, s.as_ptr(), s.len() as i32, &mut sz);
    }
    sz.cx
}

/// 版面常數（以 96 DPI 為準）
const PAD: f32 = 10.0;
const LINE_H: f32 = 26.0;
const DOT: f32 = 10.0;
const GAP: f32 = 8.0;

/// 重新計算大小與位置並顯示；沒有人說話就隱藏
unsafe fn relayout(hwnd: HWND) {
    let o = overlay();
    let main = o.main.load(Ordering::Relaxed) as HWND;
    let feed = o.feed.lock().unwrap().clone();
    let show = o.enabled.load(Ordering::Relaxed) && unsafe { GetForegroundWindow() } != main;
    let lines = match (&feed, show) {
        (Some(f), true) => f.lines(),
        _ => Vec::new(),
    };
    let corner = o.corner.load(Ordering::Relaxed);
    let changed = LOCAL.with(|l| {
        let mut l = l.borrow_mut();
        let c = l.lines != lines || l.visible != !lines.is_empty() || l.corner != corner;
        l.lines = lines.clone();
        l.corner = corner;
        c
    });
    if !changed {
        return;
    }
    if lines.is_empty() {
        unsafe { ShowWindow(hwnd, SW_HIDE) };
        LOCAL.with(|l| l.borrow_mut().visible = false);
        return;
    }
    unsafe {
        let (scale, f_name, f_ch) = fonts(hwnd);
        let dc = GetDC(hwnd);
        let mut w = 0;
        for line in &lines {
            let (n, c) = texts(line);
            w = w.max(text_width(dc, f_name, &n) + text_width(dc, f_ch, &c));
        }
        ReleaseDC(hwnd, dc);
        let width = w + ((PAD * 2.0 + DOT + GAP) * scale) as i32;
        let height = ((PAD * 2.0 + LINE_H * lines.len() as f32) * scale) as i32;

        let mut work = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut RECT as _, 0);
        let m = (16.0 * scale) as i32;
        let (x, y) = match Corner::ALL[o.corner.load(Ordering::Relaxed) as usize % 4] {
            Corner::TopLeft => (work.left + m, work.top + m),
            Corner::TopRight => (work.right - m - width, work.top + m),
            Corner::BottomLeft => (work.left + m, work.bottom - m - height),
            Corner::BottomRight => (work.right - m - width, work.bottom - m - height),
        };
        // 每次都重新設為最上層（有些遊戲會把自己拉到最上層）
        SetWindowPos(hwnd, HWND_TOPMOST, x, y, width, height, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        InvalidateRect(hwnd, std::ptr::null(), 1);
    }
    LOCAL.with(|l| l.borrow_mut().visible = true);
}

unsafe fn paint(hwnd: HWND) {
    unsafe {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let dc = BeginPaint(hwnd, &mut ps);
        let (scale, f_name, f_ch) = fonts(hwnd);
        let bg = CreateSolidBrush(BG);
        FillRect(dc, &ps.rcPaint, bg);
        DeleteObject(bg as _);
        SetBkMode(dc, TRANSPARENT as i32);
        let lines = LOCAL.with(|l| l.borrow().lines.clone());
        let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
        for (i, line) in lines.iter().enumerate() {
            let top = (PAD + LINE_H * i as f32) * scale;
            let cy = top + LINE_H * scale / 2.0;
            // 說話指示點：別人綠色、自己藍色
            let dot = CreateSolidBrush(if line.me { DOT_ME } else { DOT_OTHER });
            let old = SelectObject(dc, dot as _);
            let (dx, r) = (PAD * scale, DOT * scale / 2.0);
            Ellipse(dc, dx as i32, (cy - r) as i32, (dx + 2.0 * r) as i32, (cy + r) as i32);
            SelectObject(dc, old);
            DeleteObject(dot as _);

            let (n, c) = texts(line);
            let x = ((PAD + DOT + GAP) * scale) as i32;
            SelectObject(dc, f_name as _);
            SetTextColor(dc, NAME);
            let mut sz = SIZE { cx: 0, cy: 0 };
            GetTextExtentPoint32W(dc, n.as_ptr(), n.len() as i32, &mut sz);
            TextOutW(dc, x, (cy - sz.cy as f32 / 2.0) as i32, n.as_ptr(), n.len() as i32);
            if !c.is_empty() {
                SelectObject(dc, f_ch as _);
                SetTextColor(dc, CHANNEL);
                let mut sz2 = SIZE { cx: 0, cy: 0 };
                GetTextExtentPoint32W(dc, c.as_ptr(), c.len() as i32, &mut sz2);
                TextOutW(dc, x + sz.cx, (cy - sz2.cy as f32 / 2.0) as i32, c.as_ptr(), c.len() as i32);
            }
        }
        SelectObject(dc, old_pen);
        EndPaint(hwnd, &ps);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => unsafe { relayout(hwnd) },
        WM_PAINT => unsafe { paint(hwnd) },
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    0
}

unsafe fn run(ready: std::sync::mpsc::Sender<Option<()>>) {
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let class: Vec<u16> = "MultiVoiceOverlay".encode_utf16().chain(Some(0)).collect();
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst, lpszClassName: class.as_ptr(), ..std::mem::zeroed() };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            class.as_ptr(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            null_mut(),
            null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            let _ = ready.send(None);
            return;
        }
        // 整個視窗 93% 不透明（太透明時後面的畫面會干擾閱讀）
        SetLayeredWindowAttributes(hwnd, 0, 237, LWA_ALPHA);
        SetTimer(hwnd, 1, 100, None);
        let _ = ready.send(Some(()));
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
