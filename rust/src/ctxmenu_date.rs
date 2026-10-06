
// ================= 日期右键菜单（新增日程 / 新增待办） =================
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::windef::{HWND, POINT, RECT, SIZE};
use winapi::um::wingdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, SelectObject};
use winapi::um::winuser::*;

use super::{CM_ROW, CM_W, POPUP_BG, ROW_TXT};
use crate::gdi::{self, Cache, Painter};

#[link(name = "gdiplus")]
extern "system" {
    fn GdipCreateBitmapFromScan0(w: i32, h: i32, stride: i32, format: i32, scan0: *mut u8, bitmap: *mut gdi::Gp) -> i32;
    fn GdipGetImageGraphicsContext(image: gdi::Gp, graphics: *mut gdi::Gp) -> i32;
    fn GdipSetSmoothingMode(graphics: gdi::Gp, mode: i32) -> i32;
    fn GdipSetTextRenderingHint(graphics: gdi::Gp, mode: i32) -> i32;
}
static DM_HWND: AtomicUsize = AtomicUsize::new(0);
static DM_UI: Mutex<Option<SendDm>> = Mutex::new(None);
static DM_RECT: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);
static DM_AT: Mutex<(i32, i32)> = Mutex::new((0, 0));
static DM_DATE: Mutex<Option<chrono::NaiveDate>> = Mutex::new(None);

struct SendDm(Box<DateMenuUi>);
unsafe impl Send for SendDm {}

#[derive(Clone, Copy, PartialEq, Debug)]
enum DmAction {
    AddAgenda,
    AddTodo,
}

struct DateMenuUi {
    hwnd: usize,
    /// 创建/最近重建位图时的 sf（redraw 检测失配后按 gdi::scale() 重建）
    sf: f32,
    mem_dc: usize,
    hbmp: usize,
    bmp: gdi::Gp,
    scan0: *mut u8,
    g: gdi::Gp,
    cache: Cache,
    regions: Vec<(gdi::RectF, DmAction)>,
    hover: Option<DmAction>,
}

pub fn date_menu_visible() -> bool {
    let h = DM_HWND.load(Ordering::Relaxed);
    h != 0 && unsafe { IsWindowVisible(h as HWND) != 0 }
}

pub fn date_menu_rect() -> Option<(i32, i32, i32, i32)> {
    if !date_menu_visible() {
        return None;
    }
    *DM_RECT.lock().unwrap()
}

pub fn date_menu_close() {
    let h = DM_HWND.load(Ordering::Relaxed);
    if h != 0 && unsafe { IsWindowVisible(h as HWND) != 0 } {
        unsafe {
            ShowWindow(h as HWND, SW_HIDE);
        }
        crate::trim_working_set();
    }
}

/// 在日期格右键位置打开菜单
pub fn date_menu_toggle(x: i32, y: i32, date: chrono::NaiveDate) {
    if date_menu_visible() {
        date_menu_close();
        return;
    }
    *DM_DATE.lock().unwrap() = Some(date);
    *DM_AT.lock().unwrap() = (x, y);
    unsafe {
        let mut guard = DM_UI.lock().unwrap();
        let Some(f) = guard.as_mut() else { return };
        let f = &mut f.0;
        let pt = POINT { x, y };
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let (cm_w, dm_h) = (gdi::phys(CM_W) as i32, gdi::phys(76.0) as i32);
        let mut wa = (0, 0, x + cm_w, y + dm_h);
        if !mon.is_null() {
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(mon, &mut mi) != 0 {
                wa = (mi.rcWork.left, mi.rcWork.top, mi.rcWork.right, mi.rcWork.bottom);
            }
        }
        let mut mx = x;
        let mut my = y;
        if mx + cm_w > wa.2 {
            mx = x - cm_w;
        }
        if my + dm_h > wa.3 {
            my = y - dm_h;
        }
        mx = mx.max(wa.0);
        my = my.max(wa.1);
        SetWindowPos(f.hwnd as HWND, HWND_TOPMOST, mx, my, cm_w, dm_h, SWP_NOACTIVATE);
        ShowWindow(f.hwnd as HWND, SW_SHOWNA);
        f.redraw();
        *DM_RECT.lock().unwrap() = Some((mx, my, mx + cm_w, my + dm_h));
    }
}

pub fn create_date_menu_window() {
    unsafe {
        let cls = crate::wide("z-calendar-datemenu");
        let hinstance = winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null_mut());
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(date_menu_wndproc);
        wc.hInstance = hinstance;
        wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        wc.lpszClassName = cls.as_ptr();
        RegisterClassW(&wc);

        let title = crate::wide("Z日历日期菜单");
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_NOACTIVATE,
            cls.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            32000,
            32000,
            gdi::phys(CM_W) as i32,
            gdi::phys(76.0) as i32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null_mut(),
        );
        if hwnd.is_null() {
            return;
        }
        DM_HWND.store(hwnd as usize, Ordering::Relaxed);

        let mut ui = Box::new(DateMenuUi {
            hwnd: hwnd as usize,
            sf: gdi::scale(),
            mem_dc: 0,
            hbmp: 0,
            bmp: std::ptr::null_mut(),
            scan0: std::ptr::null_mut(),
            g: std::ptr::null_mut(),
            cache: Cache::new(),
            regions: Vec::new(),
            hover: None,
        });
        DM_UI.lock().unwrap().replace(SendDm(ui));
    }
}

impl DateMenuUi {
    fn redraw(&mut self) {
        // 屏幕缩放变化：按新 sf 重建位图（常驻窗口，位图随 sf 失配惰性重建）
        if (self.sf - gdi::scale()).abs() > 0.001 || self.bmp.is_null() {
            unsafe {
                gdi::free_dib(&mut self.mem_dc, &mut self.hbmp, &mut self.bmp, &mut self.g, &mut self.scan0);
                let (mem_dc, hbmp, bmp, scan0) = gdi::alloc_dib(CM_W, 76.0);
                self.mem_dc = mem_dc;
                self.hbmp = hbmp;
                self.bmp = bmp;
                self.scan0 = scan0;
            }
            self.sf = gdi::scale();
        }
        if self.g.is_null() {
            unsafe { GdipGetImageGraphicsContext(self.bmp, &mut self.g); }
        }
        let cache_ptr: *const Cache = &self.cache;
        let p = Painter { g: self.g, cache: cache_ptr, sf: gdi::scale(), w: CM_W, h: 76.0, dc: self.mem_dc, scan0: self.scan0 };
        self.paint(&p);
        self.ulw();
    }

    fn ulw(&self) {
        unsafe {
            let mut r: RECT = std::mem::zeroed();
            GetWindowRect(self.hwnd as HWND, &mut r);
            let mut ppt = POINT { x: r.left, y: r.top };
            let mut size = SIZE { cx: gdi::phys(CM_W) as i32, cy: gdi::phys(76.0) as i32 };
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
        p.fill_round(0.0, 0.0, CM_W, 76.0, 10.0, POPUP_BG());
        p.stroke_round(0.0, 0.0, CM_W, 76.0, 10.0, 1.0, crate::theme::ov(26));
        let items: [(DmAction, &str); 2] = [(DmAction::AddAgenda, "新增日程"), (DmAction::AddTodo, "新增待办")];
        self.regions.clear();
        for (i, (act, name)) in items.iter().enumerate() {
            let y = 4.0 + CM_ROW * i as f32;
            self.regions.push((gdi::RectF { x: 6.0, y, w: CM_W - 12.0, h: CM_ROW - 2.0 }, *act));
            let hov = self.hover == Some(*act);
            if hov {
                p.fill_round(6.0, y, CM_W - 12.0, CM_ROW - 2.0, 6.0, crate::theme::ov(16));
            }
            p.text(name, 18.0, y, CM_W - 30.0, CM_ROW - 2.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, ROW_TXT());
        }
    }
}

unsafe extern "system" fn date_menu_wndproc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            ValidateRect(hwnd, std::ptr::null_mut());
            0
        }
        WM_ERASEBKGND => 1,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_MOUSEMOVE => {
            let mut guard = DM_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                let s = gdi::scale();
                let x = ((lp & 0xFFFF) as u16 as i16) as f32 / s;
                let y = (((lp as usize) >> 16) as u16 as i16) as f32 / s;
                let hit = f.regions.iter().rev().find(|(r, _)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h).map(|(_, a)| *a);
                if hit != f.hover {
                    f.hover = hit;
                    f.redraw();
                }
            }
            0
        }
        WM_LBUTTONDOWN => {
            let mut guard = DM_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                let s = gdi::scale();
                let x = ((lp & 0xFFFF) as u16 as i16) as f32 / s;
                let y = (((lp as usize) >> 16) as u16 as i16) as f32 / s;
                let hit = f.regions.iter().rev().find(|(r, _)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h).map(|(_, a)| *a);
                if let Some(act) = hit {
                    let date = DM_DATE.lock().unwrap().unwrap_or_else(|| chrono::Local::now().date_naive());
                    let (ax, ay) = *DM_AT.lock().unwrap();
                    drop(guard);
                    date_menu_close();
                    match act {
                        DmAction::AddAgenda => crate::inputbox::open(ax, ay, date, crate::inputbox::Kind::Agenda),
                        DmAction::AddTodo => crate::inputbox::open(ax, ay, date, crate::inputbox::Kind::Todo),
                    }
                } else {
                    // 点击菜单空白处（未命中条目）也收起菜单
                    drop(guard);
                    date_menu_close();
                }
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
