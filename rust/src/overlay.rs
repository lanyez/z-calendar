//! 任务栏时钟接管：透明可点击覆盖层（Win32 原生窗口，独立线程）
use std::sync::{Arc, Mutex};

use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::windef::{HWND, RECT};
use winapi::um::libloaderapi::GetModuleHandleW;
use winapi::um::winuser::*;

#[derive(Clone, Copy)]
pub struct ClockInfo {
    pub rect: RECT,
    pub mon: (i32, i32, i32, i32),
    pub work: (i32, i32, i32, i32),
}

pub type SharedClock = Arc<Mutex<Option<ClockInfo>>>;

#[derive(Clone, Copy)]
pub enum OverlayEvent {
    LeftClick,
    RightClick,
}

static CLOCK: std::sync::OnceLock<SharedClock> = std::sync::OnceLock::new();
static OVERLAY_HWND: Mutex<usize> = Mutex::new(0);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn spawn(clock: SharedClock) {
    std::thread::Builder::new()
        .name("overlay".into())
        .stack_size(512 * 1024)
        .spawn(move || unsafe { message_loop(clock) })
        .ok();
}

unsafe fn message_loop(clock: SharedClock) {
    let _ = CLOCK.set(clock);

    let hinstance = GetModuleHandleW(std::ptr::null());
    let cls = wide("CalendarFlyoutOverlay");
    let mut wc: WNDCLASSW = std::mem::zeroed();
    wc.lpfnWndProc = Some(wndproc);
    wc.hInstance = hinstance;
    wc.lpszClassName = cls.as_ptr();
    RegisterClassW(&wc);

    let title = wide("CalendarFlyoutOverlayWin");
    let hwnd = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
        cls.as_ptr(),
        title.as_ptr(),
        WS_POPUP,
        0,
        0,
        77,
        44,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        hinstance,
        std::ptr::null_mut(),
    );
    if hwnd.is_null() {
        return;
    }
    *OVERLAY_HWND.lock().unwrap() = hwnd as usize;

    // 全窗 alpha=1/255：肉眼不可见但可接收鼠标点击
    SetLayeredWindowAttributes(hwnd, 0, 1, LWA_ALPHA);
    SetTimer(hwnd, 1, 1000, None);
    update_overlay();

    let mut msg: MSG = std::mem::zeroed();
    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            update_overlay();
            0
        }
        WM_LBUTTONDOWN => {
            crate::flyout::overlay_click(0);
            0
        }
        WM_RBUTTONDOWN => {
            crate::flyout::overlay_click(2);
            0
        }
        WM_SETCURSOR => {
            // 显式设置箭头：类光标机制在某些环境下仍会显示忙碌
            if (lp as i32 & 0xFFFF) == 1 {
                // HTCLIENT
                SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_ARROW));
                1
            } else {
                DefWindowProcW(hwnd, msg, wp, lp)
            }
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn find_clock() -> Option<ClockInfo> {
    let tray = FindWindowExW(
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        wide("Shell_TrayWnd").as_ptr(),
        std::ptr::null(),
    );
    if tray.is_null() || IsWindowVisible(tray) == 0 {
        return None;
    }
    let tn = FindWindowExW(tray, std::ptr::null_mut(), wide("TrayNotifyWnd").as_ptr(), std::ptr::null());
    if tn.is_null() {
        return None;
    }
    let clk = FindWindowExW(tn, std::ptr::null_mut(), wide("TrayClockWClass").as_ptr(), std::ptr::null());
    if clk.is_null() || IsWindowVisible(clk) == 0 {
        return None;
    }
    let mut rect: RECT = std::mem::zeroed();
    if GetWindowRect(clk, &mut rect) == 0 {
        return None;
    }
    if rect.right - rect.left <= 0 || rect.bottom - rect.top <= 0 {
        return None;
    }
    let mon = MonitorFromWindow(clk, MONITOR_DEFAULTTONEAREST);
    if mon.is_null() {
        return None;
    }
    let mut mi: MONITORINFO = std::mem::zeroed();
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(mon, &mut mi) == 0 {
        return None;
    }
    Some(ClockInfo {
        rect,
        mon: (mi.rcMonitor.left, mi.rcMonitor.top, mi.rcMonitor.right, mi.rcMonitor.bottom),
        work: (mi.rcWork.left, mi.rcWork.top, mi.rcWork.right, mi.rcWork.bottom),
    })
}

unsafe fn is_foreground_fullscreen(mon: (i32, i32, i32, i32)) -> bool {
    let fg = GetForegroundWindow();
    if fg.is_null() {
        return false;
    }
    let mut r: RECT = std::mem::zeroed();
    if GetWindowRect(fg, &mut r) == 0 {
        return false;
    }
    let covers = r.left <= mon.0 && r.top <= mon.1 && r.right >= mon.2 && r.bottom >= mon.3;
    if !covers {
        return false;
    }
    (GetWindowLongW(fg, GWL_STYLE) as u32) & WS_CAPTION == 0
}

unsafe fn update_overlay() {
    let hwnd = *OVERLAY_HWND.lock().unwrap();
    if hwnd == 0 {
        return;
    }
    let hwnd = hwnd as HWND;
    let info = find_clock();
    let mut shown = false;
    if let Some(ref ci) = info {
        if !is_foreground_fullscreen(ci.mon) {
            let r = &ci.rect;
            let vis_h = ci.mon.3.min(r.bottom) - ci.mon.1.max(r.top);
            let vis_w = ci.mon.2.min(r.right) - ci.mon.0.max(r.left);
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            if vis_h >= 20.min(h) && vis_w >= 20 {
                shown = true;
            }
        }
    }

    let clock = CLOCK.get().unwrap();
    match (shown, info) {
        (true, Some(ci)) => {
            let (x, y, w, h) = (ci.rect.left - 2, ci.rect.top - 2, ci.rect.right - ci.rect.left + 4, ci.rect.bottom - ci.rect.top + 4);
            SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            *clock.lock().unwrap() = Some(ci);
        }
        _ => {
            ShowWindow(hwnd, SW_HIDE);
            *clock.lock().unwrap() = None;
        }
    }
}
