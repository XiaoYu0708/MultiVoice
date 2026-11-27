//! 遊戲中浮動顯示（Windows）：在遊戲畫面角落顯示頻道裡的人、誰正在說話（類似 Discord 的遊戲內覆蓋）。
//!
//! 不注入遊戲（避免觸發反作弊），而是一個置頂、滑鼠穿透、不搶焦點、不出現在工作列的透明視窗：
//! - 遊戲要用「無邊框視窗」或「視窗」模式才蓋得上去；「獨佔全螢幕」時任何視窗都蓋不上
//! - 跟系統匣一樣是獨立執行緒 + Win32 視窗：主視窗縮到系統匣時 egui 不會更新，這裡仍要繼續顯示
//! - 每個像素各自的透明度（`UpdateLayeredWindow`）：圓角、頭像圓圈都有反鋸齒；文字用 GDI 畫成灰階遮罩再自己合成
//! - MultiVoice 主視窗在前景時隱藏（主視窗本身就看得到）；「只在全螢幕時」= 前景視窗蓋滿整個螢幕
//! - 改設定時預覽幾秒（主視窗在前景也顯示，沒進房間時用範例內容）
//!
//! 測試：環境變數 `MULTIVOICE_TEST_OVERLAY=1` 不管前景視窗、一直顯示

use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use session::{OverlayFeed, OverlayRow, OverlayView};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET, DIB_RGB_COLORS, DeleteDC, DeleteObject,
    FW_BOLD, FW_NORMAL, FW_SEMIBOLD, GdiFlush, GetDC, GetMonitorInfoW, GetTextExtentPoint32W, HDC, HFONT, HMONITOR,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, ReleaseDC, SelectObject, SetBkMode, SetTextColor,
    TRANSPARENT, TextOutW,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClassNameW, GetForegroundWindow, GetMessageW, GetWindowRect,
    HWND_TOPMOST, MSG, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SetTimer, SetWindowPos, ShowWindow, TranslateMessage, ULW_ALPHA, UpdateLayeredWindow, WM_TIMER, WNDCLASSW,
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

    fn right(self) -> bool {
        matches!(self, Corner::TopRight | Corner::BottomRight)
    }

    fn bottom(self) -> bool {
        matches!(self, Corner::BottomLeft | Corner::BottomRight)
    }
}

/// 浮動顯示的設定（UI 執行緒每一幀更新）
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayCfg {
    pub enabled: bool,
    pub corner: Corner,
    /// true = 一直顯示；false = 只在前景是全螢幕程式（遊戲）時
    pub always: bool,
    /// 只顯示正在說話的人
    pub only_speaking: bool,
    /// 不透明度 0.3..1
    pub opacity: f32,
    /// 大小倍率 0.75..1.5
    pub scale: f32,
}

impl Default for OverlayCfg {
    fn default() -> Self {
        Self { enabled: false, corner: Corner::TopLeft, always: false, only_speaking: false, opacity: 0.9, scale: 1.0 }
    }
}

/// UI 執行緒與浮動顯示執行緒共用
pub struct Overlay {
    feed: Mutex<Option<OverlayFeed>>,
    cfg: Mutex<OverlayCfg>,
    /// 預覽到什麼時候、我的名字（沒進房間時的範例內容用）
    preview: Mutex<Option<(Instant, String)>>,
    main: AtomicIsize,
    started: AtomicBool,
}

static OVERLAY: OnceLock<Arc<Overlay>> = OnceLock::new();

/// 改設定後預覽多久
const PREVIEW: Duration = Duration::from_secs(4);

impl Overlay {
    /// 建立浮動顯示（整個程式一個）。`main` 是主視窗的 HWND
    pub fn start(main: isize) -> Option<Arc<Overlay>> {
        let o = OVERLAY
            .get_or_init(|| {
                Arc::new(Overlay {
                    feed: Mutex::new(None),
                    cfg: Mutex::new(OverlayCfg::default()),
                    preview: Mutex::new(None),
                    main: AtomicIsize::new(main),
                    started: AtomicBool::new(false),
                })
            })
            .clone();
        if o.started.swap(true, Ordering::SeqCst) {
            return Some(o);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("overlay".into()).spawn(move || unsafe { run(tx) }).ok()?;
        rx.recv().ok().flatten().map(|_| o)
    }

    pub fn set_feed(&self, feed: Option<OverlayFeed>) {
        *self.feed.lock().unwrap() = feed;
    }

    pub fn set_cfg(&self, cfg: OverlayCfg) {
        *self.cfg.lock().unwrap() = cfg;
    }

    /// 預覽幾秒（改設定時呼叫）；`my_name` 用在沒進房間時的範例內容
    pub fn preview(&self, my_name: &str) {
        *self.preview.lock().unwrap() = Some((Instant::now() + PREVIEW, my_name.to_string()));
    }
}

fn overlay() -> &'static Arc<Overlay> {
    OVERLAY.get().expect("浮動顯示尚未建立")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

// ---- 顏色（和 style.rs 的深色主題一致） ----

type Rgb = [u8; 3];
const BG: Rgb = [0x0f, 0x0f, 0x23];
const NAME_ON: Rgb = [0xf1, 0xf5, 0xf9];
const NAME_OFF: Rgb = [0xa8, 0xb2, 0xc4];
const SUB: Rgb = [0x94, 0xa3, 0xb8];
const GREEN: Rgb = [34, 197, 94];
const RED: Rgb = [244, 63, 94];
const WHITE: Rgb = [255, 255, 255];
/// 頭像底色（依名字挑一個）
const AVATARS: [Rgb; 8] = [
    [124, 58, 237],
    [162, 28, 175],
    [2, 132, 199],
    [234, 88, 12],
    [13, 148, 136],
    [219, 39, 119],
    [79, 70, 229],
    [161, 98, 7],
];

fn avatar_color(name: &str) -> Rgb {
    // FNV-1a：同一個名字每次都是同一個顏色
    let h = name.bytes().fold(0x811c9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x01000193));
    AVATARS[h as usize % AVATARS.len()]
}

/// 頭像上的字：名字的第一個字（英文字母轉大寫）
fn initial(name: &str) -> String {
    name.trim().chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".into())
}

// ---- 版面（以 96 DPI、大小 100% 為準） ----

const ROW_H: f32 = 30.0;
const ROW_GAP: f32 = 4.0;
const AVATAR_R: f32 = 12.0;
const RING_R: f32 = 14.5;
const TEXT_X: f32 = ROW_H + 4.0;
const PAD_R: f32 = 12.0;
/// 名字和後面頻道／狀態文字之間
const SPAN_GAP: f32 = 8.0;
/// 離螢幕邊緣
const MARGIN: f32 = 16.0;
const MAX_NAME: usize = 20;

/// 一行裡的一段文字
struct Span {
    text: Vec<u16>,
    font: usize,
    color: Rgb,
}

/// 三種字型：名字、頻道與狀態、頭像上的字
const F_NAME: usize = 0;
const F_SUB: usize = 1;
const F_INITIAL: usize = 2;

fn spans(row: &OverlayRow, view: &OverlayView) -> Vec<Span> {
    let mut name: String = row.name.chars().take(MAX_NAME).collect();
    if row.name.chars().count() > MAX_NAME {
        name.push('…');
    }
    let mut v = vec![Span { text: wide(&name), font: F_NAME, color: if row.speaking { NAME_ON } else { NAME_OFF } }];
    if row.me && (view.muted || view.deafened) {
        v.push(Span { text: wide(if view.deafened { "拒聽" } else { "靜音" }), font: F_SUB, color: RED });
    }
    if row.speaking && !row.channels.is_empty() {
        v.push(Span { text: wide(&row.channels.join("、")), font: F_SUB, color: SUB });
    }
    v
}

/// 要畫的內容；跟上次一樣就不重畫
#[derive(Clone, PartialEq)]
struct Frame {
    view: OverlayView,
    cfg: OverlayCfg,
    area: (i32, i32, i32, i32),
    dpi: u32,
}

struct Local {
    last: Option<Frame>,
    visible: bool,
    last_top: Instant,
}

thread_local! {
    static LOCAL: std::cell::RefCell<Local> =
        std::cell::RefCell::new(Local { last: None, visible: false, last_top: Instant::now() });
}

fn test_mode() -> bool {
    static T: OnceLock<bool> = OnceLock::new();
    *T.get_or_init(|| std::env::var_os("MULTIVOICE_TEST_OVERLAY").is_some())
}

/// 前景視窗是不是蓋滿整個螢幕（無邊框全螢幕的遊戲、全螢幕播放影片）；桌面與工作列不算
unsafe fn is_fullscreen(fg: HWND, mon: HMONITOR) -> bool {
    if fg.is_null() {
        return false;
    }
    unsafe {
        let mut class = [0u16; 64];
        let n = GetClassNameW(fg, class.as_mut_ptr(), class.len() as i32).max(0) as usize;
        let class = String::from_utf16_lossy(&class[..n]);
        if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
            return false;
        }
        let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetWindowRect(fg, &mut r);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(mon, &mut mi);
        let m = mi.rcMonitor;
        r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
    }
}

/// 每 100 ms：決定要不要顯示、內容有變就重畫
unsafe fn tick(hwnd: HWND) {
    let o = overlay();
    let cfg = *o.cfg.lock().unwrap();
    let preview = {
        let mut p = o.preview.lock().unwrap();
        if p.as_ref().is_some_and(|(t, _)| Instant::now() >= *t) {
            *p = None;
        }
        p.as_ref().map(|(_, n)| n.clone())
    };
    let fg = unsafe { GetForegroundWindow() };
    let mon = unsafe { MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST) };
    let full = unsafe { is_fullscreen(fg, mon) };
    let main = o.main.load(Ordering::Relaxed) as HWND;
    let previewing = cfg.enabled && preview.is_some();
    let show = previewing || test_mode() || (cfg.enabled && fg != main && (cfg.always || full));

    let feed = o.feed.lock().unwrap().clone();
    let mut view = match &feed {
        Some(f) if show => f.view(),
        _ => OverlayView::default(),
    };
    if cfg.only_speaking {
        view.rows.retain(|r| r.speaking);
    }
    if previewing && view.rows.is_empty() {
        view = sample(preview.as_deref().unwrap_or("我"));
    }
    if !show || view.rows.is_empty() {
        LOCAL.with(|l| {
            let mut l = l.borrow_mut();
            if l.visible {
                unsafe { ShowWindow(hwnd, SW_HIDE) };
                l.visible = false;
            }
            l.last = None;
        });
        return;
    }

    let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
    mi.cbSize = size_of::<MONITORINFO>() as u32;
    unsafe { GetMonitorInfoW(mon, &mut mi) };
    // 全螢幕遊戲時工作列被蓋住，用整個螢幕；否則避開工作列
    let r = if full { mi.rcMonitor } else { mi.rcWork };
    let (mut dx, mut dy) = (96u32, 96u32);
    unsafe { GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
    let frame = Frame { view, cfg, area: (r.left, r.top, r.right, r.bottom), dpi: dx.max(96) };

    let (changed, was_visible, top_due) = LOCAL.with(|l| {
        let l = l.borrow();
        (l.last.as_ref() != Some(&frame), l.visible, l.last_top.elapsed() > Duration::from_secs(1))
    });
    if changed {
        unsafe { render(hwnd, &frame) };
    }
    unsafe {
        if !was_visible {
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        // 定期重新設為最上層（有些遊戲會把自己拉到最上層）
        if !was_visible || top_due {
            SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }
    LOCAL.with(|l| {
        let mut l = l.borrow_mut();
        l.visible = true;
        if !was_visible || top_due {
            l.last_top = Instant::now();
        }
        l.last = Some(frame);
    });
}

/// 預覽用的範例內容（沒進房間、或「只顯示說話的人」時沒人在說話）
fn sample(me: &str) -> OverlayView {
    OverlayView {
        rows: vec![
            OverlayRow { name: me.to_string(), me: true, speaking: true, channels: vec!["大廳".into()] },
            OverlayRow { name: "隊友".into(), me: false, speaking: false, channels: Vec::new() },
        ],
        muted: false,
        deafened: false,
    }
}

/// 32 bpp 由上而下的 DIB；`bits` 是 BGRA（預乘 alpha）
struct Dib {
    dc: HDC,
    bmp: *mut core::ffi::c_void,
    old: *mut core::ffi::c_void,
    bits: *mut u32,
    w: i32,
    h: i32,
}

impl Dib {
    unsafe fn new(screen: HDC, w: i32, h: i32) -> Option<Dib> {
        unsafe {
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            };
            let mut bits = null_mut();
            let bmp = CreateDIBSection(screen, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bmp.is_null() || bits.is_null() {
                return None;
            }
            let dc = CreateCompatibleDC(screen);
            let old = SelectObject(dc, bmp);
            // 新建的 DIB 內容是 0（全透明）
            Some(Dib { dc, bmp, old, bits: bits as *mut u32, w, h })
        }
    }

    fn px(&mut self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize) }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bmp);
            DeleteDC(self.dc);
        }
    }
}

/// 把一個顏色以覆蓋率 `a`（0..1）疊到預乘 alpha 的像素上
fn blend(p: &mut u32, c: Rgb, a: f32) {
    if a <= 0.0 {
        return;
    }
    let a = a.min(1.0);
    let [b0, g0, r0, a0] = p.to_le_bytes();
    let k = 1.0 - a;
    let mix = |src: u8, dst: u8| (src as f32 * a + dst as f32 * k).round().clamp(0.0, 255.0) as u8;
    *p = u32::from_le_bytes([mix(c[2], b0), mix(c[1], g0), mix(c[0], r0), (255.0 * a + a0 as f32 * k).round() as u8]);
}

/// 依覆蓋率函式（像素中心座標 → 0..1）填色，只掃 `x0..x1 × y0..y1`
fn fill(d: &mut Dib, (x0, y0, x1, y1): (f32, f32, f32, f32), c: Rgb, alpha: f32, cov: impl Fn(f32, f32) -> f32) {
    let (w, h) = (d.w, d.h);
    let px = d.px();
    for y in (y0.floor().max(0.0) as i32)..(y1.ceil().min(h as f32) as i32) {
        for x in (x0.floor().max(0.0) as i32)..(x1.ceil().min(w as f32) as i32) {
            let a = cov(x as f32 + 0.5, y as f32 + 0.5);
            blend(&mut px[(y * w + x) as usize], c, a * alpha);
        }
    }
}

fn circle(d: &mut Dib, cx: f32, cy: f32, r: f32, c: Rgb, alpha: f32) {
    fill(d, (cx - r - 1.0, cy - r - 1.0, cx + r + 1.0, cy + r + 1.0), c, alpha, |x, y| {
        (r - ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() + 0.5).clamp(0.0, 1.0)
    });
}

/// 膠囊形（左右兩端是半圓）
fn pill(d: &mut Dib, x0: f32, y0: f32, x1: f32, y1: f32, c: Rgb, alpha: f32) {
    let r = (y1 - y0) / 2.0;
    let cy = y0 + r;
    fill(d, (x0, y0, x1, y1), c, alpha, |x, y| {
        // 到中間那條線段（x0+r..x1-r）的距離
        let nx = x.clamp(x0 + r, x1 - r);
        (r - ((x - nx).powi(2) + (y - cy).powi(2)).sqrt() + 0.5).clamp(0.0, 1.0)
    });
}

/// 文字：在 `scratch` 用 GDI 畫白字黑底（灰階反鋸齒）當遮罩，再以顏色 `c` 疊到 `d`
unsafe fn text(d: &mut Dib, scratch: &mut Dib, font: HFONT, s: &[u16], x: i32, y: i32, size: SIZE, c: Rgb) {
    if s.is_empty() {
        return;
    }
    let (x0, y0) = (x.max(0), y.max(0));
    let (x1, y1) = ((x + size.cx + 2).min(d.w), (y + size.cy + 2).min(d.h));
    let w = scratch.w;
    {
        let px = scratch.px();
        for yy in y0..y1 {
            px[(yy * w + x0) as usize..(yy * w + x1) as usize].fill(0);
        }
    }
    unsafe {
        SelectObject(scratch.dc, font);
        SetTextColor(scratch.dc, 0x00ff_ffff);
        SetBkMode(scratch.dc, TRANSPARENT as i32);
        TextOutW(scratch.dc, x, y, s.as_ptr(), s.len() as i32);
        GdiFlush();
    }
    let mask: Vec<u8> = {
        let px = scratch.px();
        (y0..y1).flat_map(|yy| (x0..x1).map(move |xx| (yy, xx))).map(|(yy, xx)| (px[(yy * w + xx) as usize] >> 8) as u8).collect()
    };
    let dw = d.w;
    let dpx = d.px();
    let mut i = 0;
    for yy in y0..y1 {
        for xx in x0..x1 {
            blend(&mut dpx[(yy * dw + xx) as usize], c, mask[i] as f32 / 255.0);
            i += 1;
        }
    }
}

unsafe fn measure(dc: HDC, font: HFONT, s: &[u16]) -> SIZE {
    let mut sz = SIZE { cx: 0, cy: 0 };
    if !s.is_empty() {
        unsafe {
            SelectObject(dc, font);
            GetTextExtentPoint32W(dc, s.as_ptr(), s.len() as i32, &mut sz);
        }
    }
    sz
}

unsafe fn make_font(px: f32, weight: u32) -> HFONT {
    let face: Vec<u16> = "Microsoft JhengHei UI".encode_utf16().chain(Some(0)).collect();
    unsafe {
        CreateFontW(
            -(px.round() as i32),
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
            ANTIALIASED_QUALITY as u32,
            0,
            face.as_ptr(),
        )
    }
}

/// 畫好整個浮動顯示，並更新視窗的位置、大小、內容
unsafe fn render(hwnd: HWND, f: &Frame) {
    let s = f.dpi as f32 / 96.0 * f.cfg.scale.clamp(0.5, 2.0);
    unsafe {
        let screen = GetDC(null_mut());
        let fonts = [make_font(15.0 * s, FW_SEMIBOLD), make_font(13.0 * s, FW_NORMAL), make_font(13.0 * s, FW_BOLD)];
        // 先量每一行的寬度
        let rows: Vec<(Vec<Span>, Vec<SIZE>)> = f
            .view
            .rows
            .iter()
            .map(|r| {
                let sp = spans(r, &f.view);
                let sizes = sp.iter().map(|x| measure(screen, fonts[x.font], &x.text)).collect();
                (sp, sizes)
            })
            .collect();
        let widths: Vec<i32> = rows
            .iter()
            .map(|(_, sizes)| {
                let t: i32 = sizes.iter().map(|z| z.cx).sum::<i32>() + (SPAN_GAP * s) as i32 * (sizes.len() as i32 - 1);
                ((TEXT_X + PAD_R) * s) as i32 + t
            })
            .collect();
        let w = widths.iter().copied().max().unwrap_or(1).max(1);
        let n = rows.len() as f32;
        let h = ((ROW_H * n + ROW_GAP * (n - 1.0)) * s).ceil() as i32;

        if let (Some(mut d), Some(mut scratch)) = (Dib::new(screen, w, h), Dib::new(screen, w, h)) {
            let right = f.cfg.corner.right();
            for (i, ((sp, sizes), &rw)) in rows.iter().zip(&widths).enumerate() {
                let y0 = (ROW_H + ROW_GAP) * s * i as f32;
                let y1 = y0 + ROW_H * s;
                // 靠右的角落：每一行也靠右對齊
                let x0 = if right { (w - rw) as f32 } else { 0.0 };
                let row = &f.view.rows[i];
                pill(&mut d, x0, y0, x0 + rw as f32, y1, BG, if row.speaking { 0.88 } else { 0.68 });
                let (cx, cy) = (x0 + ROW_H * s / 2.0, y0 + ROW_H * s / 2.0);
                if row.speaking {
                    circle(&mut d, cx, cy, RING_R * s, GREEN, 1.0);
                    circle(&mut d, cx, cy, (AVATAR_R + 0.5) * s, BG, 1.0);
                }
                let av = if row.speaking { AVATAR_R - 0.5 } else { AVATAR_R };
                circle(&mut d, cx, cy, av * s, avatar_color(&row.name), if row.speaking { 1.0 } else { 0.8 });
                let ini = wide(&initial(&row.name));
                let isz = measure(screen, fonts[F_INITIAL], &ini);
                text(&mut d, &mut scratch, fonts[F_INITIAL], &ini, (cx - isz.cx as f32 / 2.0).round() as i32, (cy - isz.cy as f32 / 2.0).round() as i32, isz, WHITE);
                let mut x = (x0 + TEXT_X * s) as i32;
                for (span, sz) in sp.iter().zip(sizes) {
                    let ty = (cy - sz.cy as f32 / 2.0).round() as i32;
                    text(&mut d, &mut scratch, fonts[span.font], &span.text, x, ty, *sz, span.color);
                    x += sz.cx + (SPAN_GAP * s) as i32;
                }
            }

            let (l, t, r, b) = f.area;
            let m = (MARGIN * s) as i32;
            let x = if right { r - m - w } else { l + m };
            let y = if f.cfg.corner.bottom() { b - m - h } else { t + m };
            let blendf = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: (f.cfg.opacity.clamp(0.2, 1.0) * 255.0).round() as u8,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let pos = POINT { x, y };
            let size = SIZE { cx: w, cy: h };
            let src = POINT { x: 0, y: 0 };
            UpdateLayeredWindow(hwnd, screen, &pos, &size, d.dc, &src, 0, &blendf, ULW_ALPHA);
        }
        for f in fonts {
            DeleteObject(f);
        }
        ReleaseDC(null_mut(), screen);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => unsafe { tick(hwnd) },
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
        // 置頂、滑鼠穿透、不搶焦點、不出現在工作列與 Alt+Tab
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            class.as_ptr(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            null_mut(),
            null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            let _ = ready.send(None);
            return;
        }
        SetTimer(hwnd, 1, 100, None);
        let _ = ready.send(Some(()));
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
