//! 任务栏时钟点击接管：低级鼠标钩子（WH_MOUSE_LL）
//! 不建窗口、不遮挡、不修改原生时钟；点击落在时钟矩形内时吞掉该次点击
//! 并打开本日历（系统日历因此不会再弹出），其余鼠标事件原样放行。
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
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

static CLOCK: std::sync::OnceLock<SharedClock> = std::sync::OnceLock::new();

// 时钟矩形缓存（供钩子回调无锁读取）
static CLK_VALID: AtomicBool = AtomicBool::new(false);
static CLK_L: AtomicI32 = AtomicI32::new(0);
static CLK_T: AtomicI32 = AtomicI32::new(0);
static CLK_R: AtomicI32 = AtomicI32::new(0);
static CLK_B: AtomicI32 = AtomicI32::new(0);
// 全屏应用时放行点击（任务栏不可见）
static SUSPENDED: AtomicBool = AtomicBool::new(false);

const WH_MOUSE_LL: i32 = 14;

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

    let hinstance = GetModuleHandleW(std::ptr::null_mut());
    let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), hinstance, 0);
    // NULL 窗口定时器：WM_TIMER 直接投递到线程消息队列
    SetTimer(std::ptr::null_mut(), 1, 1000, None);
    update_overlay();

    let mut msg: MSG = std::mem::zeroed();
    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
        if msg.message == WM_TIMER && msg.hwnd.is_null() {
            update_overlay();
        }
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    if !hook.is_null() {
        UnhookWindowsHookEx(hook);
    }
}

/// 低级鼠标钩子：点击原生时钟 → 吞掉并打开本日历
unsafe extern "system" fn hook_proc(n_code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if n_code >= 0 {
        let msg = wp as u32;
        if matches!(msg, WM_LBUTTONDOWN | WM_RBUTTONDOWN) {
            // 右键菜单打开时：菜单内点击放行给菜单窗口；菜单外点击收起菜单并吞掉
            if let Some((ml, mt, mr, mb)) = crate::ctxmenu::rect() {
                let info = &*(lp as *const MSLLHOOKSTRUCT);
                let (x, y) = (info.pt.x, info.pt.y);
                if x >= ml && x < mr && y >= mt && y < mb {
                    return CallNextHookEx(std::ptr::null_mut(), n_code, wp, lp);
                }
                crate::ctxmenu::close();
                return 1;
            }
        }
        // 日期右键菜单打开时：菜单内左键放行给菜单窗口；菜单外左键收起菜单并吞掉
        // （右键不拦截，右键其它日期格仍由主面板的 WM_RBUTTONDOWN 切换菜单）
        if msg == WM_LBUTTONDOWN {
            if let Some((ml, mt, mr, mb)) = crate::ctxmenu::ctxmenu_date::date_menu_rect() {
                let info = &*(lp as *const MSLLHOOKSTRUCT);
                let (x, y) = (info.pt.x, info.pt.y);
                if x >= ml && x < mr && y >= mt && y < mb {
                    return CallNextHookEx(std::ptr::null_mut(), n_code, wp, lp);
                }
                crate::ctxmenu::ctxmenu_date::date_menu_close();
                return 1;
            }
        }
        if matches!(msg, WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP)
            && CLK_VALID.load(Ordering::Relaxed)
            && !SUSPENDED.load(Ordering::Relaxed)
        {
            let info = &*(lp as *const MSLLHOOKSTRUCT);
            let (x, y) = (info.pt.x, info.pt.y);
            let (l, t, r, b) = (
                CLK_L.load(Ordering::Relaxed),
                CLK_T.load(Ordering::Relaxed),
                CLK_R.load(Ordering::Relaxed),
                CLK_B.load(Ordering::Relaxed),
            );
            if x >= l && x <= r && y >= t && y <= b {
                match msg {
                    WM_LBUTTONDOWN => crate::flyout::overlay_click(0),
                    WM_RBUTTONDOWN => crate::ctxmenu::toggle(x, y),
                    _ => {}
                }
                // 吞掉该次点击（含配对的抬起），原生时钟与系统日历均不响应
                return 1;
            }
        }
    }
    CallNextHookEx(std::ptr::null_mut(), n_code, wp, lp)
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
    let clock = CLOCK.get().unwrap();
    let info = find_clock();
    let mut suspended = true;
    if let Some(ref ci) = info {
        if !is_foreground_fullscreen(ci.mon) {
            let r = &ci.rect;
            let vis_h = ci.mon.3.min(r.bottom) - ci.mon.1.max(r.top);
            let vis_w = ci.mon.2.min(r.right) - ci.mon.0.max(r.left);
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            if vis_h >= 20.min(h) && vis_w >= 20 {
                suspended = false;
            }
        }
    }

    SUSPENDED.store(suspended, Ordering::Relaxed);
    match (suspended, info) {
        (false, Some(ci)) => {
            let r = &ci.rect;
            CLK_L.store(r.left, Ordering::Relaxed);
            CLK_T.store(r.top, Ordering::Relaxed);
            CLK_R.store(r.right, Ordering::Relaxed);
            CLK_B.store(r.bottom, Ordering::Relaxed);
            CLK_VALID.store(true, Ordering::Relaxed);
            *clock.lock().unwrap() = Some(ci);
        }
        _ => {
            CLK_VALID.store(false, Ordering::Relaxed);
            *clock.lock().unwrap() = None;
        }
    }
}
