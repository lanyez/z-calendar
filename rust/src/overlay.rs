//! 任务栏时钟点击接管：低级鼠标钩子（WH_MOUSE_LL）
//! 不建窗口、不遮挡、不修改原生时钟；点击落在时钟矩形内时吞掉该次点击
//! 并打开本日历（系统日历因此不会再弹出），其余鼠标事件原样放行。
//! 放行条件按点击瞬间的当前前台窗口实时判定：仅当全屏应用盖住时钟所在
//! 显示器（任务栏不可见）时放行；桌面（Progman/WorkerW）不算全屏应用。
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::windef::{HWND, POINT, RECT};
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
// 时钟所在显示器矩形（钩子命中矩形时按当前前台实时复检全屏用）
static MON_L: AtomicI32 = AtomicI32::new(0);
static MON_T: AtomicI32 = AtomicI32::new(0);
static MON_R: AtomicI32 = AtomicI32::new(0);
static MON_B: AtomicI32 = AtomicI32::new(0);
// 全屏应用时放行点击（任务栏不可见）。仅作缓存快照（1 秒一轮），钩子拦截时实时判定，不依赖它
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

/// 任务栏时钟所在显示器的工作区（提醒卡片等跟随弹窗定位用）；尚未定位到时钟时 None
pub fn clock_work_area() -> Option<(i32, i32, i32, i32)> {
    let arc = CLOCK.get()?;
    let ci = arc.lock().unwrap().clone()?;
    Some(ci.work)
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
                // 按当前前台实时判定全屏：1 秒一轮的快照在桌面/全屏切换的过渡期是陈旧的，
                // 会把本该拦截的点击放行给系统日历（或反之吞掉全屏应用里的点击）
                let mon = (
                    MON_L.load(Ordering::Relaxed),
                    MON_T.load(Ordering::Relaxed),
                    MON_R.load(Ordering::Relaxed),
                    MON_B.load(Ordering::Relaxed),
                );
                if !is_foreground_fullscreen(mon) {
                    match msg {
                        WM_LBUTTONDOWN => crate::flyout::overlay_click(0),
                        WM_RBUTTONDOWN => crate::ctxmenu::request_toggle(x, y),
                        _ => {}
                    }
                    // 吞掉该次点击（含配对的抬起），原生时钟与系统日历均不响应
                    return 1;
                }
            }
        }
    }
    CallNextHookEx(std::ptr::null_mut(), n_code, wp, lp)
}

/// 时钟定位：先试 Win10 经典窗口链（含 ExplorerPatcher 在 Win11 恢复经典任务栏的
/// 情形）；找不到（Win11 原生任务栏时钟是 XAML 渲染，无 TrayClockWClass）则回退
/// 读取 uia_clock 定位线程的快照。
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
    if let Some(ci) = find_clock_classic(tray) {
        return Some(ci);
    }
    find_clock_uia()
}

/// Win10 经典链：Shell_TrayWnd → TrayNotifyWnd → TrayClockWClass
unsafe fn find_clock_classic(tray: HWND) -> Option<ClockInfo> {
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
    monitor_info_at(rect)
}

/// Win11：XAML 时钟无 HWND，矩形来自 UIA 后台线程（uia_clock）的定位快照
unsafe fn find_clock_uia() -> Option<ClockInfo> {
    let rect = crate::uia_clock::snapshot_rect()?;
    if rect.right - rect.left <= 0 || rect.bottom - rect.top <= 0 {
        return None;
    }
    monitor_info_at(rect)
}

/// 时钟矩形所在显示器及其工作区（矩形中心点定位显示器）
unsafe fn monitor_info_at(rect: RECT) -> Option<ClockInfo> {
    let pt = POINT { x: (rect.left + rect.right) / 2, y: (rect.top + rect.bottom) / 2 };
    let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
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

/// 桌面窗口：Progman/WorkerW 覆盖整个显示器且无标题栏，样式特征与全屏应用一致。
/// 点击桌面后桌面就是前台窗口，不排除会被 is_foreground_fullscreen 误判成全屏应用，
/// 导致点击时钟被放行、弹出系统自带日历。
unsafe fn is_desktop_window(h: HWND) -> bool {
    if h.is_null() {
        return false;
    }
    if h == GetShellWindow() {
        return true;
    }
    let mut buf = [0u16; 16];
    let n = GetClassNameW(h, buf.as_mut_ptr(), 16);
    if n <= 0 {
        return false;
    }
    let cls = String::from_utf16_lossy(&buf[..n as usize]);
    cls == "Progman" || cls == "WorkerW"
}

unsafe fn is_foreground_fullscreen(mon: (i32, i32, i32, i32)) -> bool {
    let fg = GetForegroundWindow();
    if fg.is_null() || is_desktop_window(fg) {
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
    match info {
        Some(ci) => {
            let r = &ci.rect;
            CLK_L.store(r.left, Ordering::Relaxed);
            CLK_T.store(r.top, Ordering::Relaxed);
            CLK_R.store(r.right, Ordering::Relaxed);
            CLK_B.store(r.bottom, Ordering::Relaxed);
            MON_L.store(ci.mon.0, Ordering::Relaxed);
            MON_T.store(ci.mon.1, Ordering::Relaxed);
            MON_R.store(ci.mon.2, Ordering::Relaxed);
            MON_B.store(ci.mon.3, Ordering::Relaxed);
            CLK_VALID.store(true, Ordering::Relaxed);
            // 全屏（任务栏不可见）时对 flyout 报 None：弹窗摆放会藏到屏外
            *clock.lock().unwrap() = if suspended { None } else { Some(ci) };
        }
        None => {
            CLK_VALID.store(false, Ordering::Relaxed);
            *clock.lock().unwrap() = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：桌面壳窗口（Progman）覆盖整个显示器且无标题栏，曾把它的前台状态
    /// 误判成"全屏应用"而放行时钟点击，导致点击桌面后再点时钟弹出系统日历
    #[test]
    fn desktop_shell_window_is_excluded_from_fullscreen() {
        unsafe {
            let shell = GetShellWindow();
            assert!(!shell.is_null(), "shell desktop window must exist");
            assert!(is_desktop_window(shell), "GetShellWindow must be recognized as desktop");

            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            let mon = MonitorFromWindow(shell, MONITOR_DEFAULTTONEAREST);
            assert!(GetMonitorInfoW(mon, &mut mi) != 0);
            let m = (mi.rcMonitor.left, mi.rcMonitor.top, mi.rcMonitor.right, mi.rcMonitor.bottom);
            // 即使桌面盖满整个显示器，也不得视为全屏应用
            let covers_screen = {
                let mut r: RECT = std::mem::zeroed();
                GetWindowRect(shell, &mut r) != 0
                    && r.left <= m.0 && r.top <= m.1 && r.right >= m.2 && r.bottom >= m.3
            };
            if covers_screen {
                // 桌面为前台时不判全屏（前提：前台确实是桌面）
                if GetForegroundWindow() == shell {
                    assert!(!is_foreground_fullscreen(m), "desktop must not count as fullscreen app");
                }
            }

            // 任务栏窗口不是桌面壳
            let tray = FindWindowW(wide("Shell_TrayWnd").as_ptr(), std::ptr::null());
            if !tray.is_null() {
                assert!(!is_desktop_window(tray), "taskbar is not the desktop shell window");
            }
        }
    }
}
