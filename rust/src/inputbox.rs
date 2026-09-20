//! 日期右键“新增日程 / 新增待办”输入框：可激活小窗口，支持中文 IME 与剪贴板粘贴
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::NaiveDate;
use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::windef::{HWND, POINT, RECT, SIZE};
use winapi::um::wingdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, SelectObject};
use winapi::um::winuser::*;

use crate::gdi::{self, Cache, Painter};

const IB_W: f32 = 360.0;
const IB_H: f32 = 54.0;

static IB_HWND: AtomicUsize = AtomicUsize::new(0);
static IB_UI: Mutex<Option<SendIb>> = Mutex::new(None);
static IB_AGENDA: Mutex<Option<Arc<Mutex<HashMap<String, Vec<String>>>>>> = Mutex::new(None);

struct SendIb(Box<InputUi>);
unsafe impl Send for SendIb {}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Agenda,
    Todo,
}

struct InputUi {
    hwnd: usize,
    sf: f32,
    w: f32,
    h: f32,
    mem_dc: usize,
    bmp: gdi::Gp,
    scan0: *mut u8,
    g: gdi::Gp,
    cache: Cache,
    kind: Kind,
    date: NaiveDate,
    draft: String,
    comp: String,
    caret_on: bool,
}

#[link(name = "gdiplus")]
extern "system" {
    fn GdipCreateBitmapFromScan0(w: i32, h: i32, stride: i32, format: i32, scan0: *mut u8, bitmap: *mut gdi::Gp) -> i32;
    fn GdipGetImageGraphicsContext(image: gdi::Gp, graphics: *mut gdi::Gp) -> i32;
    fn GdipSetSmoothingMode(graphics: gdi::Gp, mode: i32) -> i32;
    fn GdipSetTextRenderingHint(graphics: gdi::Gp, mode: i32) -> i32;
}

#[link(name = "imm32")]
extern "system" {
    fn ImmGetContext(hwnd: HWND) -> *mut core::ffi::c_void;
    fn ImmReleaseContext(hwnd: HWND, himc: *mut core::ffi::c_void) -> i32;
    fn ImmGetCompositionStringW(himc: *mut core::ffi::c_void, index: i32, buf: *mut core::ffi::c_void, len: i32) -> i32;
}

#[link(name = "user32")]
extern "system" {
    fn SetFocus(hwnd: HWND) -> HWND;
}

const GCS_COMPSTR: i32 = 0x0008;
const GCS_RESULTSTR: i32 = 0x0800;

pub fn create_window(agenda: Arc<Mutex<HashMap<String, Vec<String>>>>) {
    unsafe {
        *IB_AGENDA.lock().unwrap() = Some(agenda);
        let cls = crate::wide("z-calendar-input");
        let hinstance = winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null_mut());
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinstance;
        wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        wc.lpszClassName = cls.as_ptr();
        RegisterClassW(&wc);

        let title = crate::wide("Z日历输入");
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED,
            cls.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            32000,
            32000,
            IB_W as i32,
            IB_H as i32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null_mut(),
        );
        if hwnd.is_null() {
            return;
        }
        IB_HWND.store(hwnd as usize, Ordering::Relaxed);

        let mut ui = Box::new(InputUi {
            hwnd: hwnd as usize,
            sf: 1.0,
            w: IB_W,
            h: IB_H,
            mem_dc: 0,
            bmp: std::ptr::null_mut(),
            scan0: std::ptr::null_mut(),
            g: std::ptr::null_mut(),
            cache: Cache::new(),
            kind: Kind::Agenda,
            date: chrono::Local::now().date_naive(),
            draft: String::new(),
            comp: String::new(),
            caret_on: true,
        });
        let hdc = GetDC(std::ptr::null_mut());
        ui.mem_dc = CreateCompatibleDC(hdc) as usize;
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = ui.w as i32;
        bmi.bmiHeader.biHeight = -(ui.h as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut winapi::ctypes::c_void = std::ptr::null_mut();
        let hbmp = CreateDIBSection(hdc, &bmi, 0, &mut bits, std::ptr::null_mut(), 0);
        SelectObject(ui.mem_dc as winapi::shared::windef::HDC, hbmp as winapi::shared::windef::HGDIOBJ);
        ReleaseDC(std::ptr::null_mut(), hdc);
        let mut bmp: gdi::Gp = std::ptr::null_mut();
        GdipCreateBitmapFromScan0(
            ui.w as i32,
            ui.h as i32,
            (ui.w * 4.0) as i32,
            gdi::PIXEL_FORMAT_32BPP_PARGB,
            bits as *mut u8,
            &mut bmp,
        );
        ui.bmp = bmp;
        ui.scan0 = bits as *mut u8;
        GdipGetImageGraphicsContext(ui.bmp, &mut ui.g);
        IB_UI.lock().unwrap().replace(SendIb(ui));
    }
}

/// 在指定位置附近打开输入框
pub fn open(at_x: i32, at_y: i32, date: NaiveDate, kind: Kind) {
    unsafe {
        let h = IB_HWND.load(Ordering::Relaxed);
        if h == 0 {
            return;
        }
        let h = h as HWND;
        {
            let mut guard = IB_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                f.kind = kind;
                f.date = date;
                f.draft.clear();
                f.comp.clear();
                f.caret_on = true;
            }
        }
        // 工作区钳制（鼠标所在显示器）
        let mon = MonitorFromPoint(POINT { x: at_x, y: at_y }, MONITOR_DEFAULTTONEAREST);
        let (wa_l, wa_t, wa_r, wa_b) = if !mon.is_null() {
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(mon, &mut mi) != 0 {
                (mi.rcWork.left, mi.rcWork.top, mi.rcWork.right, mi.rcWork.bottom)
            } else {
                (0, 0, at_x + IB_W as i32, at_y + IB_H as i32)
            }
        } else {
            (0, 0, at_x + IB_W as i32, at_y + IB_H as i32)
        };
        let mut x = at_x;
        let mut y = at_y;
        if x + IB_W as i32 > wa_r {
            x = at_x - IB_W as i32;
        }
        if y + IB_H as i32 > wa_b {
            y = at_y - IB_H as i32;
        }
        x = x.max(wa_l);
        y = y.max(wa_t);
        SetWindowPos(h, HWND_TOPMOST, x, y, IB_W as i32, IB_H as i32, SWP_NOACTIVATE);
        ShowWindow(h, SW_SHOW);
        SetForegroundWindow(h);
        SetFocus(h);
        redraw();
        SetTimer(h, 1, 500, None); // 光标闪烁
    }
}

fn redraw() {
    let mut guard = IB_UI.lock().unwrap();
    if let Some(f) = guard.as_mut() {
        f.0.redraw();
    }
}

impl InputUi {
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

    fn paint(&self, p: &Painter) {
        p.clear();
        p.fill_round(0.0, 0.0, IB_W, IB_H, 10.0, gdi::argb(255, 0x2A, 0x33, 0x45));
        p.stroke_round(0.5, 0.5, IB_W - 1.0, IB_H - 1.0, 10.0, 1.0, gdi::argb(140, 62, 135, 250));
        let title = match self.kind {
            Kind::Agenda => "新增日程",
            Kind::Todo => "新增待办",
        };
        p.text(title, 14.0, 0.0, 70.0, IB_H, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.0, true, false, gdi::argb(255, 0x3E, 0x87, 0xFA));
        let tx = 88.0;
        let tw = IB_W - tx - 16.0;
        let mut shown = self.draft.clone();
        shown.push_str(&self.comp);
        if shown.is_empty() {
            p.text("回车添加，Esc 取消", tx, 0.0, tw, IB_H, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.0, false, false, gdi::argb(255, 0x5C, 0x66, 0x73));
        } else {
            p.text(&shown, tx, 0.0, tw, IB_H, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, false, false, gdi::argb(255, 0xDD, 0xE2, 0xE9));
        }
        if self.caret_on {
            let w = p.measure(&shown, 12.5, false, false).0;
            p.line(tx + w + 2.0, 16.0, tx + w + 2.0, IB_H - 16.0, 1.2, gdi::argb(255, 0xDD, 0xE2, 0xE9));
        }
    }
}

fn confirm() {
    let mut guard = IB_UI.lock().unwrap();
    if let Some(f) = guard.as_mut() {
        let f = &mut f.0;
        let text = f.draft.trim().to_string();
        if text.is_empty() {
            return;
        }
        let key = crate::ics::key_of_date(f.date);
        match f.kind {
            Kind::Agenda => {
                if let Some(agenda) = IB_AGENDA.lock().unwrap().as_ref() {
                    let mut map = agenda.lock().unwrap();
                    map.entry(key).or_default().push(text.clone());
                    if let Ok(json) = serde_json::to_string(&*map) {
                        let _ = std::fs::write(crate::config::data_dir().join("agenda.json"), json);
                    }
                }
            }
            Kind::Todo => {
                crate::sidebar::add_todo(key, text);
            }
        }
        f.draft.clear();
        f.comp.clear();
    }
    drop(guard);
    crate::sidebar::sidebar_repaint();
    unsafe {
        let h = IB_HWND.load(Ordering::Relaxed);
        if h != 0 {
            ShowWindow(h as HWND, SW_HIDE);
            KillTimer(h as HWND, 1);
        }
    }
    crate::trim_working_set();
}

fn cancel() {
    unsafe {
        let h = IB_HWND.load(Ordering::Relaxed);
        if h != 0 {
            ShowWindow(h as HWND, SW_HIDE);
            KillTimer(h as HWND, 1);
        }
    }
    crate::trim_working_set();
}

unsafe fn read_ime(hwnd: HWND, mode: i32) -> Option<String> {
    let himc = ImmGetContext(hwnd);
    if himc.is_null() {
        return None;
    }
    let len = ImmGetCompositionStringW(himc, mode, std::ptr::null_mut(), 0);
    let mut out = None;
    if len >= 0 {
        let mut buf = vec![0u8; (len + 2) as usize];
        let got = ImmGetCompositionStringW(himc, mode, buf.as_mut_ptr() as *mut core::ffi::c_void, len);
        if got >= 0 {
            let slice = std::slice::from_raw_parts::<u16>(buf.as_ptr() as *const u16, (len as usize) / 2);
            out = Some(String::from_utf16_lossy(slice));
        }
    }
    ImmReleaseContext(hwnd, himc);
    out
}

unsafe fn paste_clipboard(ui: &mut InputUi) {
    if OpenClipboard(ui.hwnd as HWND) == 0 {
        return;
    }
    let h = GetClipboardData(13); // CF_UNICODETEXT
    if !h.is_null() {
        let ptr = GlobalLock(h as usize);
        if !ptr.is_null() {
            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(ptr, len);
            ui.draft.push_str(&String::from_utf16_lossy(slice));
            GlobalUnlock(h as usize);
        }
    }
    CloseClipboard();
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            ValidateRect(hwnd, std::ptr::null_mut());
            0
        }
        WM_ERASEBKGND => 1,
        WM_TIMER => {
            let mut guard = IB_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                f.caret_on = !f.caret_on;
                f.redraw();
            }
            0
        }
        WM_CHAR => {
            let mut guard = IB_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                if (wp as u32) >= 0x20 {
                    if let Some(ch) = char::from_u32(wp as u32) {
                        f.draft.push(ch);
                        f.redraw();
                    }
                }
            }
            0
        }
        WM_KEYDOWN => {
            let mut guard = IB_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                match wp as i32 {
                    0x0D => {
                        // 回车确认
                        drop(guard);
                        confirm();
                        return 0;
                    }
                    0x1B => {
                        drop(guard);
                        cancel();
                        return 0;
                    }
                    0x08 => {
                        f.draft.pop();
                        f.redraw();
                    }
                    0x56 => {
                        // Ctrl+V 粘贴
                        if (GetKeyState(0x11) as u16) & 0x8000 != 0 {
                            paste_clipboard(f);
                            f.redraw();
                        }
                    }
                    _ => {}
                }
            }
            0
        }
        WM_IME_COMPOSITION => {
            let mut guard = IB_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                if lp as i32 & GCS_RESULTSTR != 0 {
                    if let Some(s) = read_ime(hwnd, GCS_RESULTSTR) {
                        f.draft.push_str(&s);
                        f.comp.clear();
                    }
                } else if lp as i32 & GCS_COMPSTR != 0 {
                    f.comp = read_ime(hwnd, GCS_COMPSTR).unwrap_or_default();
                }
                f.redraw();
            }
            0
        }
        WM_ACTIVATE => {
            // 失焦自动取消
            if (wp & 0xFFFF) as u16 == 0 {
                cancel();
            }
            0
        }
        WM_LBUTTONDOWN => {
            // 点击输入框保持前台
            SetForegroundWindow(hwnd);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[link(name = "user32")]
extern "system" {
    fn OpenClipboard(hwnd: HWND) -> i32;
    fn CloseClipboard() -> i32;
    fn GetClipboardData(fmt: u32) -> *mut core::ffi::c_void;
    fn GlobalLock(h: usize) -> *mut u16;
    fn GlobalUnlock(h: usize) -> i32;
    fn SetForegroundWindow(hwnd: HWND) -> i32;
    fn KillTimer(hwnd: HWND, id: usize) -> i32;
}
