//! 重复日程/待办的删除选择菜单：✕ 点击重复条目时在鼠标位置弹出
//! （仅删除这一天 = 追加例外日期 / 删除整个系列 = 移除主条目）
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::NaiveDate;
use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::windef::{HWND, POINT, RECT, SIZE};
use winapi::um::wingdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, SelectObject};
use winapi::um::winuser::*;

use crate::events::AgendaMap;
use crate::gdi::{self, Cache, Painter};

#[link(name = "gdiplus")]
extern "system" {
    fn GdipCreateBitmapFromScan0(w: i32, h: i32, stride: i32, format: i32, scan0: *mut u8, bitmap: *mut gdi::Gp) -> i32;
    fn GdipGetImageGraphicsContext(bmp: gdi::Gp, g: *mut gdi::Gp) -> i32;
    fn GdipSetSmoothingMode(g: gdi::Gp, mode: i32) -> i32;
    fn GdipSetTextRenderingHint(g: gdi::Gp, mode: i32) -> i32;
}

const RM_W: f32 = 186.0;
const RM_ROW: f32 = 34.0;
const RM_H: f32 = RM_ROW * 2.0 + 8.0;
const POPUP_BG: u32 = gdi::argb(255, 0x2A, 0x33, 0x45);
const ROW_TXT: u32 = gdi::argb(255, 0xD7, 0xDD, 0xE4);
const RED: u32 = gdi::argb(255, 0xE5, 0x4B, 0x4B);

static RM_HWND: AtomicUsize = AtomicUsize::new(0);
static RM_UI: Mutex<Option<SendRm>> = Mutex::new(None);
static RM_TARGET: Mutex<Option<RmTarget>> = Mutex::new(None);

struct SendRm(Box<RecurMenuUi>);
unsafe impl Send for SendRm {}

/// 待操作目标：日程（原 key/下标 + 出现日期）或待办（全局下标 + 出现日期）
#[derive(Clone, Debug)]
pub enum RmTarget {
    Agenda { key: String, idx: usize, date: NaiveDate },
    Todo { gi: usize, date: NaiveDate },
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum RmAction {
    ThisDay,
    Series,
}

struct RecurMenuUi {
    hwnd: usize,
    mem_dc: usize,
    bmp: gdi::Gp,
    g: gdi::Gp,
    cache: Cache,
    regions: Vec<(gdi::RectF, RmAction)>,
    hover: Option<RmAction>,
}

pub fn visible() -> bool {
    let h = RM_HWND.load(Ordering::Relaxed);
    h != 0 && unsafe { IsWindowVisible(h as HWND) != 0 }
}

pub fn close() {
    let h = RM_HWND.load(Ordering::Relaxed);
    if h != 0 && unsafe { IsWindowVisible(h as HWND) != 0 } {
        unsafe {
            ShowWindow(h as HWND, SW_HIDE);
        }
        crate::trim_working_set();
    }
}

/// 在鼠标位置打开删除选择菜单
pub fn open(x: i32, y: i32, target: RmTarget) {
    if visible() {
        close();
    }
    *RM_TARGET.lock().unwrap() = Some(target);
    unsafe {
        let mut guard = RM_UI.lock().unwrap();
        let Some(f) = guard.as_mut() else { return };
        let f = &mut f.0;
        let pt = POINT { x, y };
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut wa = (0, 0, x + RM_W as i32, y + RM_H as i32);
        if !mon.is_null() {
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(mon, &mut mi) != 0 {
                wa = (mi.rcWork.left, mi.rcWork.top, mi.rcWork.right, mi.rcWork.bottom);
            }
        }
        let mut mx = x;
        let mut my = y;
        if mx + RM_W as i32 > wa.2 {
            mx = x - RM_W as i32;
        }
        if my + RM_H as i32 > wa.3 {
            my = y - RM_H as i32;
        }
        mx = mx.max(wa.0);
        my = my.max(wa.1);
        SetWindowPos(f.hwnd as HWND, HWND_TOPMOST, mx, my, RM_W as i32, RM_H as i32, SWP_NOACTIVATE);
        ShowWindow(f.hwnd as HWND, SW_SHOWNA);
        f.redraw();
    }
}

pub fn create_window(agenda: Arc<Mutex<AgendaMap>>) {
    unsafe {
        *AGENDA_SHARED.lock().unwrap() = Some(agenda);
        let cls = crate::wide("z-calendar-recurmenu");
        let hinstance = winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null_mut());
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinstance;
        wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        wc.lpszClassName = cls.as_ptr();
        RegisterClassW(&wc);

        let title = crate::wide("Z日历重复菜单");
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_NOACTIVATE,
            cls.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            32000,
            32000,
            RM_W as i32,
            RM_H as i32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null_mut(),
        );
        if hwnd.is_null() {
            return;
        }
        RM_HWND.store(hwnd as usize, Ordering::Relaxed);

        let mut ui = Box::new(RecurMenuUi {
            hwnd: hwnd as usize,
            mem_dc: 0,
            bmp: std::ptr::null_mut(),
            g: std::ptr::null_mut(),
            cache: Cache::new(),
            regions: Vec::new(),
            hover: None,
        });
        let hdc = GetDC(std::ptr::null_mut());
        ui.mem_dc = CreateCompatibleDC(hdc) as usize;
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = RM_W as i32;
        bmi.bmiHeader.biHeight = -RM_H as i32;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut winapi::ctypes::c_void = std::ptr::null_mut();
        let hbmp = CreateDIBSection(hdc, &bmi, 0, &mut bits, std::ptr::null_mut(), 0);
        SelectObject(ui.mem_dc as winapi::shared::windef::HDC, hbmp as winapi::shared::windef::HGDIOBJ);
        ReleaseDC(std::ptr::null_mut(), hdc);
        let mut bmp: gdi::Gp = std::ptr::null_mut();
        GdipCreateBitmapFromScan0(RM_W as i32, RM_H as i32, RM_W as i32 * 4, gdi::PIXEL_FORMAT_32BPP_PARGB, bits as *mut u8, &mut bmp);
        GdipGetImageGraphicsContext(bmp, &mut ui.g);
        ui.bmp = bmp;
        RM_UI.lock().unwrap().replace(SendRm(ui));
    }
}

static AGENDA_SHARED: Mutex<Option<Arc<Mutex<AgendaMap>>>> = Mutex::new(None);

/// 应用后刷新相关界面
fn refresh() {
    crate::sidebar::sidebar_repaint();
    crate::flyout::flyout_repaint();
}

impl RecurMenuUi {
    fn redraw(&mut self) {
        if self.g.is_null() {
            unsafe { GdipGetImageGraphicsContext(self.bmp, &mut self.g); }
        }
        let cache_ptr: *const Cache = &self.cache;
        let p = Painter { g: self.g, cache: cache_ptr, sf: 1.0, w: RM_W, h: RM_H };
        unsafe {
            GdipSetSmoothingMode(self.g, gdi::SMOOTH_ANTI_ALIAS);
            GdipSetTextRenderingHint(self.g, gdi::TEXT_AA_GRID_FIT);
        }
        self.paint(&p);
        self.ulw();
    }

    fn ulw(&self) {
        unsafe {
            let mut r: RECT = std::mem::zeroed();
            GetWindowRect(self.hwnd as HWND, &mut r);
            let mut ppt = POINT { x: r.left, y: r.top };
            let mut size = SIZE { cx: RM_W as i32, cy: RM_H as i32 };
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
        p.fill_round(0.0, 0.0, RM_W, RM_H, 10.0, POPUP_BG);
        p.stroke_round(0.0, 0.0, RM_W, RM_H, 10.0, 1.0, gdi::argb(26, 255, 255, 255));
        let items: [(RmAction, &str, u32); 2] = [(RmAction::ThisDay, "仅删除这一天", ROW_TXT), (RmAction::Series, "删除整个系列", RED)];
        self.regions.clear();
        for (i, (act, name, col)) in items.iter().enumerate() {
            let y = 4.0 + RM_ROW * i as f32;
            self.regions.push((gdi::RectF { x: 6.0, y, w: RM_W - 12.0, h: RM_ROW - 2.0 }, *act));
            let hov = self.hover == Some(*act);
            if hov {
                p.fill_round(6.0, y, RM_W - 12.0, RM_ROW - 2.0, 6.0, gdi::argb(16, 255, 255, 255));
            }
            p.text(name, 18.0, y, RM_W - 30.0, RM_ROW - 2.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, *col);
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
            let mut guard = RM_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                let x = ((lp & 0xFFFF) as u16 as i16) as f32;
                let y = (((lp as usize) >> 16) as u16 as i16) as f32;
                let hit = f.regions.iter().rev().find(|(r, _)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h).map(|(_, a)| *a);
                if hit != f.hover {
                    f.hover = hit;
                    f.redraw();
                }
            }
            0
        }
        WM_LBUTTONDOWN => {
            let mut guard = RM_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                let x = ((lp & 0xFFFF) as u16 as i16) as f32;
                let y = (((lp as usize) >> 16) as u16 as i16) as f32;
                let hit = f.regions.iter().rev().find(|(r, _)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h).map(|(_, a)| *a);
                if let Some(act) = hit {
                    let target = RM_TARGET.lock().unwrap().take();
                    drop(guard);
                    close();
                    if let Some(t) = target {
                        match (act, t) {
                            (RmAction::ThisDay, RmTarget::Agenda { key, idx, date }) => {
                                let agenda = AGENDA_SHARED.lock().unwrap().clone();
                                if let Some(a) = agenda {
                                    let mut map = a.lock().unwrap();
                                    crate::events::agenda_skip_day(&mut map, &key, idx, date);
                                    crate::events::save(&map);
                                    drop(map);
                                    refresh();
                                }
                            }
                            (RmAction::Series, RmTarget::Agenda { key, idx, .. }) => {
                                let agenda = AGENDA_SHARED.lock().unwrap().clone();
                                if let Some(a) = agenda {
                                    let mut map = a.lock().unwrap();
                                    crate::events::agenda_remove_at(&mut map, &key, idx);
                                    crate::events::save(&map);
                                    drop(map);
                                    refresh();
                                }
                            }
                            (RmAction::ThisDay, RmTarget::Todo { gi, date }) => {
                                crate::sidebar::todo_skip_day(gi, date);
                                refresh();
                            }
                            (RmAction::Series, RmTarget::Todo { gi, .. }) => {
                                crate::sidebar::remove_todo_at(gi);
                                refresh();
                            }
                        }
                    }
                } else {
                    // 点击菜单空白处也收起
                    drop(guard);
                    close();
                }
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
