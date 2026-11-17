//! A small always-on-top message in the corner of the screen Live is on.
//! Used instead of Windows notifications, which Focus Assist hides while an app
//! (often Live) is full screen — exactly when "Saved v_8" matters.
use std::cell::RefCell;
use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Info,
    Ok,
    Error,
}

struct Toast {
    hwnd: HWND,
    title: String,
    body: String,
    kind: Kind,
}

thread_local! {
    static TOAST: RefCell<Option<Toast>> = const { RefCell::new(None) };
}

const TIMER_HIDE: usize = 7;

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

fn font(px: i32, weight: i32) -> HFONT {
    unsafe {
        CreateFontW(-px, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET.0 as u32, OUT_DEFAULT_PRECIS.0 as u32, CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32, (DEFAULT_PITCH.0 | FF_SWISS.0) as u32, w!("Segoe UI"))
    }
}

struct Layout {
    w: i32,
    h: i32,
    title: RECT,
    body: RECT,
    scale: f32,
}

fn layout(hwnd: HWND, title: &str, body: &str) -> Layout {
    unsafe {
        let dpi = GetDpiForWindow(hwnd).max(96);
        let s = dpi as f32 / 96.0;
        let px = |v: f32| (v * s).round() as i32;
        let (pad, bar, width) = (px(14.0), px(4.0), px(380.0));
        let hdc = GetDC(hwnd);
        let measure = |text: &str, f: HFONT| {
            let old = SelectObject(hdc, f);
            let mut r = RECT { left: 0, top: 0, right: width - 2 * pad - bar, bottom: 0 };
            let mut t: Vec<u16> = text.encode_utf16().collect();
            DrawTextW(hdc, &mut t, &mut r, DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX);
            SelectObject(hdc, old);
            r
        };
        let (ft, fb) = (font(px(15.0), 600), font(px(13.0), 400));
        let rt = measure(title, ft);
        let rb = if body.is_empty() { RECT::default() } else { measure(body, fb) };
        let _ = DeleteObject(ft);
        let _ = DeleteObject(fb);
        ReleaseDC(hwnd, hdc);
        let x = bar + pad;
        let title_r = RECT { left: x, top: pad, right: width - pad, bottom: pad + rt.bottom };
        let gap = if body.is_empty() { 0 } else { px(4.0) };
        let body_r = RECT { left: x, top: title_r.bottom + gap, right: width - pad, bottom: title_r.bottom + gap + rb.bottom };
        Layout { w: width, h: body_r.bottom + pad, title: title_r, body: body_r, scale: s }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            TOAST.with(|t| {
                if let Ok(t) = t.try_borrow() {
                    if let Some(t) = t.as_ref() {
                        let l = layout(hwnd, &t.title, &t.body);
                        let mut rc = RECT::default();
                        let _ = GetClientRect(hwnd, &mut rc);
                        let bg = CreateSolidBrush(rgb(0x2a, 0x2a, 0x2a));
                        FillRect(hdc, &rc, bg);
                        let _ = DeleteObject(bg);
                        let accent = match t.kind {
                            Kind::Info => rgb(0xff, 0x9b, 0x45),
                            Kind::Ok => rgb(0x8f, 0xd1, 0x8f),
                            Kind::Error => rgb(0xe0, 0x6c, 0x6c),
                        };
                        let bar = CreateSolidBrush(accent);
                        let bar_rect = RECT { right: (4.0 * l.scale).round() as i32, ..rc };
                        FillRect(hdc, &bar_rect, bar);
                        let _ = DeleteObject(bar);
                        let border = CreateSolidBrush(rgb(0x45, 0x45, 0x45));
                        FrameRect(hdc, &rc, border);
                        let _ = DeleteObject(border);
                        SetBkMode(hdc, TRANSPARENT);
                        let px = |v: f32| (v * l.scale).round() as i32;
                        let (ft, fb) = (font(px(15.0), 600), font(px(13.0), 400));
                        let old = SelectObject(hdc, ft);
                        SetTextColor(hdc, rgb(0xee, 0xee, 0xee));
                        let mut r = l.title;
                        let mut s: Vec<u16> = t.title.encode_utf16().collect();
                        DrawTextW(hdc, &mut s, &mut r, DT_WORDBREAK | DT_NOPREFIX);
                        if !t.body.is_empty() {
                            SelectObject(hdc, fb);
                            SetTextColor(hdc, rgb(0xa8, 0xa8, 0xa8));
                            let mut r = l.body;
                            let mut s: Vec<u16> = t.body.encode_utf16().collect();
                            DrawTextW(hdc, &mut s, &mut r, DT_WORDBREAK | DT_NOPREFIX);
                        }
                        SelectObject(hdc, old);
                        let _ = DeleteObject(ft);
                        let _ = DeleteObject(fb);
                    }
                }
            });
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_HIDE => {
            let _ = KillTimer(hwnd, TIMER_HIDE);
            let _ = ShowWindow(hwnd, SW_HIDE);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ShowWindow(hwnd, SW_HIDE);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

pub fn init(hinst: HINSTANCE) {
    unsafe {
        let class = w!("LiveVersionsToast");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            &HSTRING::from("Live Versions notification"),
            WS_POPUP,
            0,
            0,
            10,
            10,
            None,
            None,
            hinst,
            None,
        ) else {
            return;
        };
        TOAST.with(|t| *t.borrow_mut() = Some(Toast { hwnd, title: String::new(), body: String::new(), kind: Kind::Info }));
    }
}

/// Show (or replace) the message. `ms` = 0 keeps it until replaced or clicked.
pub fn show(title: &str, body: &str, kind: Kind, ms: u32) {
    let hwnd = TOAST.with(|t| {
        let mut g = t.borrow_mut();
        let t = g.as_mut()?;
        t.title = title.to_string();
        t.body = body.to_string();
        t.kind = kind;
        Some(t.hwnd)
    });
    let Some(hwnd) = hwnd else { return };
    unsafe {
        let l = layout(hwnd, title, body);
        // Corner of the monitor Live is on (else the one with the active window).
        let anchor = super::live::live_main_window().map(|(h, _)| h).unwrap_or_else(|| GetForegroundWindow());
        let mon = MonitorFromWindow(anchor, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let margin = (18.0 * l.scale) as i32;
        let x = mi.rcWork.right - l.w - margin;
        let y = mi.rcWork.bottom - l.h - margin;
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, l.w, l.h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        // Re-measure in case the window moved to a monitor with another DPI.
        let l2 = layout(hwnd, title, body);
        if l2.w != l.w || l2.h != l.h {
            let x = mi.rcWork.right - l2.w - margin;
            let y = mi.rcWork.bottom - l2.h - margin;
            let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, l2.w, l2.h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
        let _ = InvalidateRect(hwnd, None, true);
        let _ = UpdateWindow(hwnd);
        let _ = KillTimer(hwnd, TIMER_HIDE);
        if ms > 0 {
            SetTimer(hwnd, TIMER_HIDE, ms, None);
        }
    }
}
