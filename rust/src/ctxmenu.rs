//! 右键菜单：右键任务栏时钟时在鼠标位置弹出的独立小窗口
//! （软件设置 / 开机自启 / 日期与时间 / 退出Z日历）
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::windef::{HWND, POINT, RECT, SIZE};
use winapi::um::wingdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, SelectObject};
use winapi::um::winuser::*;

use crate::flyout::SharedState;
use crate::gdi::{self, Cache, Painter};
use crate::tray;

use winapi::um::libloaderapi::GetModuleHandleW;

#[link(name = "gdiplus")]
extern "system" {
    fn GdipCreateBitmapFromScan0(w: i32, h: i32, stride: i32, format: i32, scan0: *mut u8, bitmap: *mut gdi::Gp) -> i32;
    fn GdipGetImageGraphicsContext(image: gdi::Gp, graphics: *mut gdi::Gp) -> i32;
    fn GdipSetSmoothingMode(graphics: gdi::Gp, mode: i32) -> i32;
    fn GdipSetTextRenderingHint(graphics: gdi::Gp, mode: i32) -> i32;
}

// 配色（与主面板一致）
const BLUE: u32 = gdi::argb(255, 0x3E, 0x87, 0xFA);
const RED: u32 = gdi::argb(255, 0xE5, 0x4B, 0x4B);
const ROW_TXT: u32 = gdi::argb(255, 0xD7, 0xDD, 0xE4);
const POPUP_BG: u32 = gdi::argb(255, 0x2A, 0x33, 0x45);

const CM_W: f32 = 186.0;
const CM_ROW: f32 = 34.0;
const CM_H: f32 = CM_ROW * 4.0 + 8.0;

static CM_HWND: AtomicUsize = AtomicUsize::new(0);
static CM_UI: Mutex<Option<SendCm>> = Mutex::new(None);
static CM_RECT: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);

struct SendCm(Box<ContextMenuUi>);
unsafe impl Send for SendCm {}

#[derive(Clone, Copy, PartialEq, Debug)]
enum CmAction {
    Settings,
    AutoStart,
    TimeDate,
    Quit,
}

struct ContextMenuUi {
    hwnd: usize,
    sf: f32,
    w: f32,
    h: f32,
    mem_dc: usize,
    bmp: gdi::Gp,
    scan0: *mut u8,
    g: gdi::Gp,
    cache: Cache,
    st: SharedState,
    tray: Arc<Mutex<Option<tray::Tray>>>,
    regions: Vec<(gdi::RectF, CmAction)>,
    hover: Option<CmAction>,
}

pub mod ctxmenu_date {
    include!("ctxmenu_date.rs");
}

pub fn visible() -> bool {
    let h = CM_HWND.load(Ordering::Relaxed);
    h != 0 && unsafe { IsWindowVisible(h as HWND) != 0 }
}

/// 菜单屏幕矩形（未显示时返回 None）
pub fn rect() -> Option<(i32, i32, i32, i32)> {
    if !visible() {
        return None;
    }
    *CM_RECT.lock().unwrap()
}

pub fn close() {
    let h = CM_HWND.load(Ordering::Relaxed);
    if h != 0 && unsafe { IsWindowVisible(h as HWND) != 0 } {
        unsafe {
            ShowWindow(h as HWND, SW_HIDE);
        }
        crate::trim_working_set();
    }
}

/// 在鼠标位置打开/收起菜单
pub fn toggle(x: i32, y: i32) {
    if visible() {
        close();
        return;
    }
    unsafe {
        let mut guard = CM_UI.lock().unwrap();
        let Some(f) = guard.as_mut() else { return };
        let f = &mut f.0;
        let pt = POINT { x, y };
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut wa = (0, 0, x + CM_W as i32, y + CM_H as i32);
        if !mon.is_null() {
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(mon, &mut mi) != 0 {
                wa = (mi.rcWork.left, mi.rcWork.top, mi.rcWork.right, mi.rcWork.bottom);
            }
        }
        let mut mx = x;
        let mut my = y;
        if mx + CM_W as i32 > wa.2 {
            mx = x - CM_W as i32;
        }
        if my + CM_H as i32 > wa.3 {
            my = y - CM_H as i32;
        }
        mx = mx.max(wa.0);
        my = my.max(wa.1);
        SetWindowPos(f.hwnd as HWND, HWND_TOPMOST, mx, my, CM_W as i32, CM_H as i32, SWP_NOACTIVATE);
        ShowWindow(f.hwnd as HWND, SW_SHOWNA);
        f.redraw();
        *CM_RECT.lock().unwrap() = Some((mx, my, mx + CM_W as i32, my + CM_H as i32));
    }
}

pub fn create_window(st: SharedState, tray: Arc<Mutex<Option<tray::Tray>>>) {
    unsafe {
        let cls = crate::wide("z-calendar-ctxmenu");
        let hinstance = unsafe { GetModuleHandleW(std::ptr::null_mut()) };
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinstance;
        wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        wc.lpszClassName = cls.as_ptr();
        RegisterClassW(&wc);

        let w = CM_W as i32;
        let h = CM_H as i32;
        let title = crate::wide("Z日历菜单");
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_NOACTIVATE,
            cls.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            32000,
            32000,
            w,
            h,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null_mut(),
        );
        if hwnd.is_null() {
            return;
        }
        CM_HWND.store(hwnd as usize, Ordering::Relaxed);

        let mut cui = Box::new(ContextMenuUi {
            hwnd: hwnd as usize,
            sf: 1.0,
            w: w as f32,
            h: h as f32,
            mem_dc: 0,
            bmp: std::ptr::null_mut(),
            scan0: std::ptr::null_mut(),
            g: std::ptr::null_mut(),
            cache: Cache::new(),
            st,
            tray,
            regions: Vec::new(),
            hover: None,
        });
        let hdc = GetDC(std::ptr::null_mut());
        cui.mem_dc = CreateCompatibleDC(hdc) as usize;
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = cui.w as i32;
        bmi.bmiHeader.biHeight = -(cui.h as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut winapi::ctypes::c_void = std::ptr::null_mut();
        let hbmp = CreateDIBSection(hdc, &bmi, 0, &mut bits, std::ptr::null_mut(), 0);
        SelectObject(cui.mem_dc as winapi::shared::windef::HDC, hbmp as winapi::shared::windef::HGDIOBJ);
        ReleaseDC(std::ptr::null_mut(), hdc);
        let mut bmp: gdi::Gp = std::ptr::null_mut();
        GdipCreateBitmapFromScan0(
            cui.w as i32,
            cui.h as i32,
            (cui.w * 4.0) as i32,
            gdi::PIXEL_FORMAT_32BPP_PARGB,
            bits as *mut u8,
            &mut bmp,
        );
        cui.bmp = bmp;
        cui.scan0 = bits as *mut u8;
        GdipGetImageGraphicsContext(cui.bmp, &mut cui.g);
        CM_UI.lock().unwrap().replace(SendCm(cui));
    }
}

impl ContextMenuUi {
    fn redraw(&mut self) {
        if self.g.is_null() {
            unsafe { GdipGetImageGraphicsContext(self.bmp, &mut self.g); }
        }
        let cache_ptr: *const Cache = &self.cache;
        let g = self.g;
        unsafe {
            GdipSetSmoothingMode(g, gdi::SMOOTH_ANTI_ALIAS);
            GdipSetTextRenderingHint(g, gdi::TEXT_AA_GRID_FIT);
        }
        let p = Painter { g, cache: cache_ptr, sf: self.sf, w: self.w, h: self.h };
        self.paint(&p);
        self.ulw();
    }

    fn ulw(&self) {
        unsafe {
            let mut r: RECT = std::mem::zeroed();
            GetWindowRect(self.hwnd as HWND, &mut r);
            let mut ppt = POINT { x: r.left, y: r.top };
            let mut size = SIZE { cx: self.w as i32, cy: self.h as i32 };
            let mut src = POINT { x: 0, y: 0 };
            let mut blend = winapi::um::wingdi::BLENDFUNCTION {
                BlendOp: 0,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: 1,
            };
            UpdateLayeredWindow(
                self.hwnd as HWND,
                std::ptr::null_mut(),
                &mut ppt,
                &mut size,
                self.mem_dc as winapi::shared::windef::HDC,
                &mut src,
                0,
                &mut blend,
                2,
            );
        }
    }

    fn paint(&mut self, p: &Painter) {
        p.clear();
        p.fill_round(0.0, 0.0, CM_W, CM_H, 10.0, POPUP_BG);
        p.stroke_round(0.0, 0.0, CM_W, CM_H, 10.0, 1.0, gdi::argb(26, 255, 255, 255));
        let autostart = self.st.config.lock().unwrap().autostart;
        let items: [(CmAction, &str, bool); 4] = [
            (CmAction::Settings, "软件设置", false),
            (CmAction::AutoStart, "开机自启", autostart),
            (CmAction::TimeDate, "日期与时间", false),
            (CmAction::Quit, "退出Z日历", false),
        ];
        self.regions.clear();
        for (i, (act, name, checked)) in items.iter().enumerate() {
            let y = 4.0 + CM_ROW * i as f32;
            self.regions.push((gdi::RectF { x: 6.0, y, w: CM_W - 12.0, h: CM_ROW - 2.0 }, *act));
            let hov = self.hover == Some(*act);
            if hov {
                p.fill_round(6.0, y, CM_W - 12.0, CM_ROW - 2.0, 6.0, gdi::argb(16, 255, 255, 255));
            }
            let col = if *act == CmAction::Quit { RED } else { ROW_TXT };
            // 开机自启开启时在左侧打勾
            p.text(if *checked { "✓" } else { "" }, 10.0, y, 16.0, CM_ROW - 2.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, BLUE);
            p.text(name, 30.0, y, CM_W - 42.0, CM_ROW - 2.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, col);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            ValidateRect(hwnd, std::ptr::null_mut());
            0
        }
        WM_ERASEBKGND => 1,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_MOUSEMOVE => {
            let mut guard = CM_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                let x = ((lp & 0xFFFF) as u16 as i16) as f32;
                let y = (((lp as usize) >> 16) as u16 as i16) as f32;
                let hit = f.regions.iter().rev().find(|(r, _)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h).map(|(_, a)| *a);
                if hit != f.hover {
                    f.hover = hit;
                    f.redraw();
                }
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut tme);
            }
            0
        }
        WM_MOUSELEAVE => {
            let mut guard = CM_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                if f.hover.is_some() {
                    f.hover = None;
                    f.redraw();
                }
            }
            0
        }
        WM_LBUTTONDOWN => {
            let mut guard = CM_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                let x = ((lp & 0xFFFF) as u16 as i16) as f32;
                let y = (((lp as usize) >> 16) as u16 as i16) as f32;
                let hit = f.regions.iter().rev().find(|(r, _)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h).map(|(_, a)| *a);
                match hit {
                    Some(CmAction::Settings) => {
                        close();
                        crate::flyout::show_settings();
                    }
                    Some(CmAction::AutoStart) => {
                        let on = f.st.config.lock().unwrap().autostart;
                        {
                            let mut cfg = f.st.config.lock().unwrap();
                            cfg.autostart = !on;
                            cfg.save();
                        }
                        crate::config::apply_autostart(!on);
                        if let Some(t) = f.tray.lock().unwrap().as_ref() {
                            t.autostart_item.set_checked(!on);
                        }
                        f.redraw();
                    }
                    Some(CmAction::TimeDate) => {
                        close();
                        crate::flyout::open_date_time_settings();
                    }
                    Some(CmAction::Quit) => {
                        close();
                        DestroyWindow(crate::flyout::hwnd() as HWND);
                    }
                    _ => {}
                }
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
