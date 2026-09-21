//! 日期右键“新增日程 / 新增待办”弹窗：完整表单
//! 待办：内容 / 优先级下拉（默认不选）/ 时间开关 / 开始·结束 / 提醒 / 重复
//! 日程：名称 / 全天 / 开始·结束 / 提醒 / 重复
//! 开始/结束为两步选择：先选日期（月历），再选时间（时/分列表）；全天日程只选日期
//! 分层窗口 + GDI+ 自绘，支持中文 IME 与剪贴板粘贴；打开时主日历保持显示
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};
use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::windef::{HWND, POINT, RECT, SIZE};
use winapi::um::wingdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, SelectObject};
use winapi::um::winuser::*;

use crate::events::{self, AgendaEntry, AgendaMap, RichEvent};
use crate::gdi::{self, Cache, Painter};

const DL_W: f32 = 400.0;
const H_MAX: f32 = 512.0; // 位图按最大高度分配（待办开时间时最高）

// 布局
const TITLE_H: f32 = 44.0;
const GAP: f32 = 8.0;
const SAVE_H: f32 = 34.0;
const ROW_H: f32 = 38.0;
const TOP_Y: f32 = 46.0; // 首卡片 y
const LIST_ROW: f32 = 30.0; // 下拉项行高
const PICK_ROW: f32 = 24.0; // 时/分列表行高
const PICK_VISIBLE: i32 = 8; // 时/分列表可见行数

// 配色（与主面板/侧栏一致）
const BLUE: u32 = gdi::argb(255, 0x3E, 0x87, 0xFA);
const BLUE_HOV: u32 = gdi::argb(255, 0x53, 0x99, 0xFB);
const RED: u32 = gdi::argb(255, 0xE5, 0x48, 0x4D);
const ORANGE: u32 = gdi::argb(255, 0xE8, 0x96, 0x3C);
const SLATE: u32 = gdi::argb(255, 0x8A, 0x93, 0xA0);
const BG_PAGE: u32 = gdi::argb(255, 0x20, 0x28, 0x38);
const CARD_BG: u32 = gdi::argb(255, 0x26, 0x30, 0x42);
const FIELD_BG: u32 = gdi::argb(255, 0x2A, 0x33, 0x45);
const TITLE_COL: u32 = gdi::argb(255, 0xDF, 0xE5, 0xEC);
const ROW_TXT: u32 = gdi::argb(255, 0xD7, 0xDD, 0xE4);
const SUB: u32 = gdi::argb(255, 0x9A, 0xA1, 0xA9);
const SUB_DIM: u32 = gdi::argb(255, 0x5C, 0x66, 0x73);
const WHITE: u32 = gdi::argb(255, 255, 255, 255);
const HOVER_BG: u32 = gdi::argb(14, 255, 255, 255);
const SEL_BG: u32 = gdi::argb(36, 62, 135, 250);
const BORDER_SUB: u32 = gdi::argb(24, 255, 255, 255);
const DROP_BG: u32 = gdi::argb(255, 0x24, 0x2E, 0x40);

// 优先级：0=不选（默认），1..4 对应四象限
const PRIORITIES: [(u8, &str, &str, u32); 4] = [
    (1, "Ⅰ", "重要且紧急", RED),
    (2, "Ⅱ", "重要但不紧急", ORANGE),
    (3, "Ⅲ", "紧急但不重要", BLUE),
    (4, "Ⅳ", "不重要不紧急", SLATE),
];

fn priority_label(p: u8) -> &'static str {
    match p {
        1 => "重要且紧急",
        2 => "重要但不紧急",
        3 => "紧急但不重要",
        4 => "不重要不紧急",
        _ => "不选",
    }
}

fn priority_badge(p: u8) -> Option<(&'static str, u32)> {
    match p {
        1 => Some(("Ⅰ", RED)),
        2 => Some(("Ⅱ", ORANGE)),
        3 => Some(("Ⅲ", BLUE)),
        4 => Some(("Ⅳ", SLATE)),
        _ => None,
    }
}

static IB_HWND: AtomicUsize = AtomicUsize::new(0);
static IB_UI: Mutex<Option<SendIb>> = Mutex::new(None);
static IB_AGENDA: Mutex<Option<Arc<Mutex<AgendaMap>>>> = Mutex::new(None);

struct SendIb(Box<DialogUi>);
unsafe impl Send for SendIb {}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Agenda,
    Todo,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum DlAction {
    Drag,
    Close,
    Save,
    ToggleTime,
    ToggleAllDay,
    PickRow(usize),  // 0=开始 1=结束 → 日期选择
    DropRow(usize),  // 0=提醒 1=重复 2=优先级 → 下拉
    PickMonthPrev,
    PickMonthNext,
    PickDay(usize),
    PickHour(u32),
    PickMinute(u32),
    PickDone, // 完成（关闭面板）
    PickNext, // 日期 → 时间
    PickPrev, // 时间 → 日期
    ListItem(usize),
    Noop,
}

enum Drop {
    /// 日期选择（月历）；row：编辑开始/结束；y/m：浏览月份
    Date { row: usize, y: i32, m: u32, anchor: f32 },
    /// 时间选择（时/分列表，日期选好后“下一步”进入）
    Time { row: usize, h_off: i32, m_off: i32, anchor: f32 },
    /// 下拉列表：0=提醒 1=重复 2=优先级
    List { list: usize, anchor: f32 },
}

impl Drop {
    fn anchor(&self) -> f32 {
        match self {
            Drop::Date { anchor, .. } | Drop::Time { anchor, .. } | Drop::List { anchor, .. } => *anchor,
        }
    }
}

struct DialogUi {
    hwnd: usize,
    sf: f32,
    h: f32,
    mem_dc: usize,
    bmp: gdi::Gp,
    scan0: *mut u8,
    g: gdi::Gp,
    cache: Cache,
    kind: Kind,
    date: NaiveDate,
    todo_text: String,
    agenda_name: String,
    priority: u8,
    time_on: bool,
    all_day: bool,
    a_start: NaiveDateTime,
    a_end: NaiveDateTime,
    remind: Option<i64>,
    repeat: Option<i64>,
    comp: String,
    caret_on: bool,
    drop: Option<Drop>,
    regions: Vec<(gdi::RectF, DlAction)>,
    hover: Option<DlAction>,
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
    fn OpenClipboard(hwnd: HWND) -> i32;
    fn CloseClipboard() -> i32;
    fn GetClipboardData(fmt: u32) -> *mut core::ffi::c_void;
    fn GlobalLock(h: usize) -> *mut u16;
    fn GlobalUnlock(h: usize) -> i32;
    fn SetForegroundWindow(hwnd: HWND) -> i32;
    fn KillTimer(hwnd: HWND, id: usize) -> i32;
    fn SendMessageW(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT;
    fn ScreenToClient(hwnd: HWND, pt: *mut POINT) -> i32;
}

const GCS_COMPSTR: i32 = 0x0008;
const GCS_RESULTSTR: i32 = 0x0800;

pub fn hwnd() -> usize {
    IB_HWND.load(Ordering::Relaxed)
}

pub fn visible() -> bool {
    let h = IB_HWND.load(Ordering::Relaxed);
    h != 0 && unsafe { IsWindowVisible(h as HWND) != 0 }
}

/// 弹窗总高度（与 paint 的布局累加保持一致）
fn dialog_h(kind: Kind, time_on: bool) -> f32 {
    let mut y = TOP_Y;
    match kind {
        Kind::Todo => {
            y += 130.0 + GAP; // 内容
            y += 52.0 + GAP; // 优先级
            y += if time_on { 206.0 } else { 50.0 } + GAP; // 时间卡片
        }
        Kind::Agenda => {
            y += 56.0 + GAP; // 名称
            y += 54.0 + GAP; // 全天
            y += 164.0 + GAP; // 时间卡片
        }
    }
    y + SAVE_H + 8.0
}

pub fn create_window(agenda: Arc<Mutex<AgendaMap>>) {
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

        let title = crate::wide("Z日历新建");
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED,
            cls.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            32000,
            32000,
            DL_W as i32,
            H_MAX as i32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null_mut(),
        );
        if hwnd.is_null() {
            return;
        }
        IB_HWND.store(hwnd as usize, Ordering::Relaxed);

        let now = chrono::Local::now().naive_local();
        let mut ui = Box::new(DialogUi {
            hwnd: hwnd as usize,
            sf: 1.0,
            h: 386.0,
            mem_dc: 0,
            bmp: std::ptr::null_mut(),
            scan0: std::ptr::null_mut(),
            g: std::ptr::null_mut(),
            cache: Cache::new(),
            kind: Kind::Agenda,
            date: chrono::Local::now().date_naive(),
            todo_text: String::new(),
            agenda_name: String::new(),
            priority: 0,
            time_on: false,
            all_day: false,
            a_start: date_at(now.date(), 9, 0),
            a_end: date_at(now.date(), 10, 0),
            remind: None,
            repeat: None,
            comp: String::new(),
            caret_on: true,
            drop: None,
            regions: Vec::new(),
            hover: None,
        });
        let hdc = GetDC(std::ptr::null_mut());
        ui.mem_dc = CreateCompatibleDC(hdc) as usize;
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = DL_W as i32;
        bmi.bmiHeader.biHeight = -H_MAX as i32;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut winapi::ctypes::c_void = std::ptr::null_mut();
        let hbmp = CreateDIBSection(hdc, &bmi, 0, &mut bits, std::ptr::null_mut(), 0);
        SelectObject(ui.mem_dc as winapi::shared::windef::HDC, hbmp as winapi::shared::windef::HGDIOBJ);
        ReleaseDC(std::ptr::null_mut(), hdc);
        let mut bmp: gdi::Gp = std::ptr::null_mut();
        GdipCreateBitmapFromScan0(DL_W as i32, H_MAX as i32, DL_W as i32 * 4, gdi::PIXEL_FORMAT_32BPP_PARGB, bits as *mut u8, &mut bmp);
        GdipGetImageGraphicsContext(bmp, &mut ui.g);
        ui.bmp = bmp;
        ui.scan0 = bits as *mut u8;
        IB_UI.lock().unwrap().replace(SendIb(ui));
    }
}

fn date_at(d: NaiveDate, h: u32, m: u32) -> NaiveDateTime {
    d.and_hms_opt(h, m, 0).unwrap_or_else(|| d.and_hms_opt(0, 0, 0).unwrap())
}

/// 在指定位置附近打开弹窗
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
                f.todo_text.clear();
                f.agenda_name.clear();
                f.comp.clear();
                f.priority = 0;
                f.time_on = false;
                f.all_day = false;
                f.a_start = date_at(date, 9, 0);
                f.a_end = date_at(date, 10, 0);
                f.remind = None;
                f.repeat = None;
                f.caret_on = true;
                f.drop = None;
                f.hover = None;
                f.h = dialog_h(kind, false);
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
                (0, 0, at_x + DL_W as i32, at_y + 400)
            }
        } else {
            (0, 0, at_x + DL_W as i32, at_y + 400)
        };
        let hh;
        {
            let guard = IB_UI.lock().unwrap();
            hh = guard.as_ref().map(|s| s.0.h).unwrap_or(386.0);
        }
        // 屏幕正中间（鼠标所在显示器的工作区）
        let x = wa_l + (wa_r - wa_l - DL_W as i32) / 2;
        let y = wa_t + (wa_b - wa_t - hh as i32) / 2;
        SetWindowPos(h, HWND_TOPMOST, x, y, DL_W as i32, hh as i32, SWP_NOACTIVATE);
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

impl DialogUi {
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
        let p = Painter { g, cache: cache_ptr, sf: self.sf, w: DL_W, h: self.h };
        self.paint(&p);
        self.ulw();
    }

    fn ulw(&self) {
        unsafe {
            let mut r: RECT = std::mem::zeroed();
            GetWindowRect(self.hwnd as HWND, &mut r);
            let mut ppt = POINT { x: r.left, y: r.top };
            let mut size = SIZE { cx: DL_W as i32, cy: self.h as i32 };
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

    fn hit_add(regions: &mut Vec<(gdi::RectF, DlAction)>, x: f32, y: f32, w: f32, h: f32, a: DlAction) {
        regions.push((gdi::RectF { x, y, w, h }, a));
    }

    fn text_mut(&mut self) -> &mut String {
        match self.kind {
            Kind::Todo => &mut self.todo_text,
            Kind::Agenda => &mut self.agenda_name,
        }
    }

    fn row_dt(&self, row: usize) -> NaiveDateTime {
        if row == 0 {
            self.a_start
        } else {
            self.a_end
        }
    }

    fn set_row_dt(&mut self, row: usize, dt: NaiveDateTime) {
        if row == 0 {
            self.a_start = dt;
            if self.a_end < dt {
                self.a_end = dt;
            }
        } else {
            self.a_end = dt;
            if self.a_end < self.a_start {
                self.a_start = self.a_end;
            }
        }
    }

    /// 下拉/选择面板矩形（x, y, w, h），锚定并钳制在窗口内
    fn drop_rect(&self) -> (f32, f32, f32, f32) {
        let Some(d) = &self.drop else { return (0.0, 0.0, 0.0, 0.0) };
        let anchor = d.anchor();
        let px = 88.0;
        let pw = 300.0;
        let py = |ph: f32| (anchor + 30.0).min(self.h - ph - 6.0).max(48.0);
        match d {
            Drop::Date { .. } => (px, py(264.0), pw, 264.0),
            Drop::Time { .. } => (px, py(274.0), pw, 274.0),
            Drop::List { list, .. } => {
                let n = match list {
                    0 => events::REMIND_VALUES.len(),
                    1 => events::REPEAT_VALUES.len(),
                    _ => 1 + PRIORITIES.len(),
                };
                let ph = n as f32 * LIST_ROW + 8.0;
                (194.0, py(ph), 190.0, ph)
            }
        }
    }

    fn paint(&mut self, p: &Painter) {
        p.clear();
        p.fill_round(0.5, 0.5, DL_W - 1.0, self.h - 1.0, 12.0, BG_PAGE);
        p.stroke_round(0.5, 0.5, DL_W - 1.0, self.h - 1.0, 12.0, 1.0, gdi::argb(120, 62, 135, 250));
        self.regions.clear();

        // 标题栏
        let title = match self.kind {
            Kind::Agenda => "新增日程",
            Kind::Todo => "新增待办",
        };
        p.text(title, 16.0, 0.0, 160.0, TITLE_H, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 15.0, true, false, TITLE_COL);
        let close_hov = self.hover == Some(DlAction::Close);
        p.text("✕", DL_W - 38.0, 7.0, 30.0, 30.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, if close_hov { WHITE } else { SUB });
        Self::hit_add(&mut self.regions, DL_W - 40.0, 6.0, 32.0, 32.0, DlAction::Close);
        Self::hit_add(&mut self.regions, 0.0, 0.0, DL_W - 46.0, TITLE_H, DlAction::Drag);

        let mut y = TOP_Y;
        match self.kind {
            Kind::Todo => {
                self.paint_text_card(p, y, 130.0, "输入待办内容…");
                y += 130.0 + GAP;
                self.paint_priority_row(p, y);
                y += 52.0 + GAP;
                self.paint_time_card(p, y, true);
                y += if self.time_on { 206.0 } else { 50.0 } + GAP;
            }
            Kind::Agenda => {
                self.paint_text_card(p, y, 56.0, "输入日程名称");
                y += 56.0 + GAP;
                // 全天（开关点击区与开关图形对齐）
                p.fill_round(14.0, y, 372.0, 54.0, 8.0, CARD_BG);
                p.text("全天", 20.0, y, 80.0, 54.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, false, false, ROW_TXT);
                self.paint_switch(p, DL_W - 52.0, y + 18.0, self.all_day);
                Self::hit_add(&mut self.regions, 328.0, y + 8.0, 64.0, 38.0, DlAction::ToggleAllDay);
                y += 54.0 + GAP;
                self.paint_time_card(p, y, false);
                y += 164.0 + GAP;
            }
        }

        // 保存
        let bw = 110.0;
        let bx = (DL_W - bw) / 2.0;
        let save_hov = self.hover == Some(DlAction::Save);
        p.fill_round(bx, y, bw, SAVE_H, 8.0, if save_hov { BLUE_HOV } else { BLUE });
        p.text("保存", bx, y, bw, SAVE_H, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 13.5, false, false, WHITE);
        Self::hit_add(&mut self.regions, bx, y, bw, SAVE_H, DlAction::Save);

        // 下拉/选择面板（最后绘制 → 命中优先）
        let is_date = matches!(self.drop, Some(Drop::Date { .. }));
        let is_time = matches!(self.drop, Some(Drop::Time { .. }));
        let is_list = matches!(self.drop, Some(Drop::List { .. }));
        if is_date {
            self.paint_date_panel(p);
        } else if is_time {
            self.paint_time_panel(p);
        } else if is_list {
            self.paint_dropdown(p);
        }
    }

    /// 文本输入卡片（待办内容多行 / 日程名称单行）
    fn paint_text_card(&mut self, p: &Painter, y: f32, ch: f32, placeholder: &str) {
        p.fill_round(14.0, y, 372.0, ch, 8.0, FIELD_BG);
        p.stroke_round(14.0, y, 372.0, ch, 8.0, 1.0, if self.caret_on { gdi::argb(140, 62, 135, 250) } else { BORDER_SUB });
        Self::hit_add(&mut self.regions, 14.0, y, 372.0, ch, DlAction::Noop);
        let mut shown = self.text_mut().clone();
        shown.push_str(&self.comp);
        let tx = 26.0;
        let tw = 348.0;
        if shown.is_empty() {
            p.text(placeholder, tx, y + 8.0, tw, ch - 16.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, false, false, SUB_DIM);
            if self.caret_on {
                p.line(tx + 1.0, y + 14.0, tx + 1.0, y + ch - 14.0, 1.2, ROW_TXT);
            }
            return;
        }
        let lines = wrap_lines(p, &shown, tw);
        let lh = 19.0;
        let max_lines = ((ch - 16.0) / lh).floor().max(1.0) as usize;
        let skip = lines.len().saturating_sub(max_lines);
        for (i, line) in lines.iter().skip(skip).enumerate() {
            p.text(line, tx, y + 8.0 + i as f32 * lh, tw, lh, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, false, false, ROW_TXT);
        }
        if self.caret_on {
            let last = &lines[lines.len() - 1];
            let w = p.measure(last, 12.5, false, false).0;
            let cy = y + 8.0 + (lines.len() - 1 - skip) as f32 * lh;
            p.line(tx + w + 2.0, cy + 3.0, tx + w + 2.0, cy + lh - 3.0, 1.2, ROW_TXT);
        }
    }

    /// 优先级行（下拉选择，默认不选）
    fn paint_priority_row(&mut self, p: &Painter, y: f32) {
        p.fill_round(14.0, y, 372.0, 52.0, 8.0, CARD_BG);
        let action = DlAction::DropRow(2);
        let hov = self.hover == Some(action);
        if hov {
            p.fill_round(16.0, y + 7.0, 368.0, 38.0, 6.0, HOVER_BG);
        }
        p.text("优先级", 20.0, y, 80.0, 52.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, false, false, ROW_TXT);
        let (vx, vw) = (70.0, DL_W - 118.0);
        if let Some((numeral, color)) = priority_badge(self.priority) {
            // 徽标 + 文本右对齐为一组
            let label = priority_label(self.priority);
            let tw = p.measure(label, 12.5, false, false).0;
            let total = 16.0 + 6.0 + tw;
            let sx = vx + vw - total;
            self.paint_badge(p, sx, y + 18.0, 16.0, numeral, color);
            p.text(label, sx + 22.0, y + 7.0, tw + 8.0, 38.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, false, false, ROW_TXT);
        } else {
            p.text("不选", vx, y + 7.0, vw, 38.0, gdi::HALIGN_FAR, gdi::HALIGN_CENTER, 12.5, false, false, SUB);
        }
        p.text("\u{E70D}", DL_W - 44.0, y + 7.0, 24.0, 38.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 9.0, false, true, SUB);
        Self::hit_add(&mut self.regions, 16.0, y + 7.0, 368.0, 38.0, action);
    }

    fn paint_badge(&self, p: &Painter, x: f32, y: f32, size: f32, numeral: &str, color: u32) {
        p.fill_round(x, y, size, size, 4.0, color);
        p.text(numeral, x, y, size, size, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, size * 0.55, true, false, WHITE);
    }

    fn paint_switch(&self, p: &Painter, x: f32, y: f32, on: bool) {
        p.fill_round(x, y, 36.0, 18.0, 9.0, if on { BLUE } else { gdi::argb(255, 0x3A, 0x44, 0x5A) });
        let kx = if on { x + 36.0 - 9.0 - 2.0 } else { x + 2.0 + 7.0 };
        p.fill_circle(kx, y + 9.0, 7.0, WHITE);
    }

    /// 时间卡片：待办含“时间”开关行（关时隐藏后续行），日程直接是四行
    fn paint_time_card(&mut self, p: &Painter, y: f32, with_toggle: bool) {
        let card_h = if with_toggle {
            if self.time_on { 206.0 } else { 50.0 }
        } else {
            164.0
        };
        p.fill_round(14.0, y, 372.0, card_h, 8.0, CARD_BG);
        let mut ry = y + 6.0;
        if with_toggle {
            p.text("时间", 20.0, ry, 80.0, ROW_H, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, false, false, ROW_TXT);
            self.paint_switch(p, DL_W - 52.0, ry + 10.0, self.time_on);
            // 点击区与开关图形对齐（348..384）
            Self::hit_add(&mut self.regions, 328.0, ry, 64.0, ROW_H, DlAction::ToggleTime);
            if !self.time_on {
                return;
            }
            ry = y + 46.0;
        } else {
            ry = y + 6.0;
        }
        // 开始 / 结束 / 提醒 / 重复
        for (i, label) in ["开始", "结束", "提醒", "重复"].iter().enumerate() {
            if i > 0 {
                p.line(20.0, ry, DL_W - 20.0, ry, 1.0, gdi::argb(10, 255, 255, 255));
            }
            let action = if i < 2 { DlAction::PickRow(i) } else { DlAction::DropRow(i - 2) };
            let hov = self.hover == Some(action);
            if hov {
                p.fill_round(16.0, ry, 368.0, ROW_H, 6.0, HOVER_BG);
            }
            p.text(*label, 20.0, ry, 60.0, ROW_H, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, false, false, ROW_TXT);
            let vx = 70.0;
            let vw = DL_W - 118.0;
            match i {
                0 => {
                    let s = if self.kind == Kind::Agenda && self.all_day { events::fmt_date_cn(self.a_start.date()) } else { events::fmt_dt_cn(self.a_start) };
                    p.text(&s, vx, ry, vw, ROW_H, gdi::HALIGN_FAR, gdi::HALIGN_CENTER, 12.5, false, false, ROW_TXT);
                }
                1 => {
                    let s = if self.kind == Kind::Agenda && self.all_day { events::fmt_date_cn(self.a_end.date()) } else { events::fmt_dt_cn(self.a_end) };
                    p.text(&s, vx, ry, vw, ROW_H, gdi::HALIGN_FAR, gdi::HALIGN_CENTER, 12.5, false, false, ROW_TXT);
                }
                2 => {
                    p.text(&events::remind_label(self.remind), vx, ry, vw, ROW_H, gdi::HALIGN_FAR, gdi::HALIGN_CENTER, 12.5, false, false, ROW_TXT);
                    p.text("\u{E70D}", DL_W - 44.0, ry, 24.0, ROW_H, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 9.0, false, true, SUB);
                }
                _ => {
                    p.text(&events::repeat_label(self.repeat), vx, ry, vw, ROW_H, gdi::HALIGN_FAR, gdi::HALIGN_CENTER, 12.5, false, false, ROW_TXT);
                    p.text("\u{E70D}", DL_W - 44.0, ry, 24.0, ROW_H, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 9.0, false, true, SUB);
                }
            }
            Self::hit_add(&mut self.regions, 16.0, ry, 368.0, ROW_H, action);
            ry += ROW_H;
        }
    }

    /// 日期选择面板（月历 + 底部“下一步/完成”）
    fn paint_date_panel(&mut self, p: &Painter) {
        let Some(Drop::Date { row, y: by, m: bm, .. }) = &self.drop else { return };
        let (row, by, bm) = (*row, *by, *bm);
        let (px, py, pw, ph) = self.drop_rect();
        let timed = self.kind == Kind::Todo || !self.all_day;
        p.fill_round(px, py, pw, ph, 10.0, DROP_BG);
        p.stroke_round(px, py, pw, ph, 10.0, 1.0, gdi::argb(90, 62, 135, 250));

        // 月份切换
        let prev_hov = self.hover == Some(DlAction::PickMonthPrev);
        let next_hov = self.hover == Some(DlAction::PickMonthNext);
        p.fill_round(px + 8.0, py + 8.0, 28.0, 26.0, 6.0, if prev_hov { HOVER_BG } else { gdi::argb(0, 0, 0, 0) });
        p.text("\u{E76B}", px + 8.0, py + 8.0, 28.0, 26.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 10.0, false, true, if prev_hov { WHITE } else { SUB });
        p.fill_round(px + pw - 36.0, py + 8.0, 28.0, 26.0, 6.0, if next_hov { HOVER_BG } else { gdi::argb(0, 0, 0, 0) });
        p.text("\u{E76C}", px + pw - 36.0, py + 8.0, 28.0, 26.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 10.0, false, true, if next_hov { WHITE } else { SUB });
        p.text(&format!("{}年{}月", by, bm), px + 40.0, py + 8.0, pw - 80.0, 26.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 13.0, true, false, TITLE_COL);
        Self::hit_add(&mut self.regions, px + 8.0, py + 8.0, 28.0, 26.0, DlAction::PickMonthPrev);
        Self::hit_add(&mut self.regions, px + pw - 36.0, py + 8.0, 28.0, 26.0, DlAction::PickMonthNext);

        // 星期表头 + 日格
        let cw = pw / 7.0;
        for (i, w) in ["日", "一", "二", "三", "四", "五", "六"].iter().enumerate() {
            p.text(*w, px + i as f32 * cw, py + 40.0, cw, 18.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 10.5, false, false, SUB);
        }
        let first_wd = match NaiveDate::from_ymd_opt(by, bm, 1) {
            Some(d) => d.weekday().num_days_from_sunday() as i32,
            None => 0,
        };
        let dim = days_in_month(by, bm);
        let sel_date = self.row_dt(row).date();
        let today = chrono::Local::now().date_naive();
        for idx in 0..42usize {
            let day = idx as i32 - first_wd + 1;
            if day < 1 || day > dim {
                continue;
            }
            let r = idx / 7;
            let c = idx % 7;
            let cx = px + c as f32 * cw;
            let cy = py + 60.0 + r as f32 * 26.0;
            let d0 = NaiveDate::from_ymd_opt(by, bm, day as u32).unwrap();
            let is_sel = d0 == sel_date;
            let is_today = d0 == today;
            if is_sel {
                p.fill_circle(cx + cw / 2.0, cy + 13.0, 11.0, BLUE);
            } else if is_today {
                p.stroke_circle(cx + cw / 2.0, cy + 13.0, 11.0, 1.0, BLUE);
            }
            p.text(&format!("{}", day), cx, cy, cw, 26.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 11.5, false, false, if is_sel { WHITE } else if is_today { BLUE } else { ROW_TXT });
            Self::hit_add(&mut self.regions, cx, cy, cw, 26.0, DlAction::PickDay(idx));
        }

        // 底部：当前日期 + 下一步（带时间）/ 完成（全天）
        p.text(&events::fmt_date_cn(self.row_dt(row).date()), px + 14.0, py + 222.0, 170.0, 36.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.0, false, false, SUB);
        let (label, action) = if timed { ("下一步", DlAction::PickNext) } else { ("完成", DlAction::PickDone) };
        self.paint_panel_btn(p, px + pw - 92.0, py + 226.0, 84.0, 28.0, label, action);
    }

    /// 时间选择面板（时/分列表 + 上一步/完成）
    fn paint_time_panel(&mut self, p: &Painter) {
        let Some(Drop::Time { row, h_off, m_off, .. }) = &self.drop else { return };
        let (row, h_off, m_off) = (*row, *h_off, *m_off);
        let (px, py, pw, _ph) = self.drop_rect();
        p.fill_round(px, py, pw, 274.0, 10.0, DROP_BG);
        p.stroke_round(px, py, pw, 274.0, 10.0, 1.0, gdi::argb(90, 62, 135, 250));

        // 当前时间
        let dt = self.row_dt(row);
        p.text(&format!("{:02}:{:02}", dt.hour(), dt.minute()), px, py + 6.0, pw, 22.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 13.0, true, false, BLUE);

        // 时 / 分列表（可见 8 行，滚轮滚动）
        let ly = py + 34.0;
        let lhh = PICK_VISIBLE as f32 * PICK_ROW;
        for (li, prefix) in ["时", "分"].iter().enumerate() {
            let lx = if li == 0 { px + 18.0 } else { px + pw - 140.0 };
            let lw = 122.0;
            p.fill_round(lx, ly, lw, lhh, 6.0, gdi::argb(10, 255, 255, 255));
            let (maxv, cur) = if li == 0 { (23, dt.hour() as i32) } else { (59, dt.minute() as i32) };
            let off = if li == 0 { h_off } else { m_off };
            for k in 0..PICK_VISIBLE {
                let v = off + k;
                if v < 0 || v > maxv {
                    continue;
                }
                let iy = ly + k as f32 * PICK_ROW;
                let action = if li == 0 { DlAction::PickHour(v as u32) } else { DlAction::PickMinute(v as u32) };
                let sel = v == cur;
                let hov = self.hover == Some(action);
                if hov && !sel {
                    p.fill_round(lx + 4.0, iy, lw - 8.0, PICK_ROW, 4.0, HOVER_BG);
                }
                p.text(&format!("{:02}{}", v, prefix), lx, iy, lw, PICK_ROW, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 11.5, false, false, if sel { BLUE } else { ROW_TXT });
                Self::hit_add(&mut self.regions, lx, iy, lw, PICK_ROW, action);
            }
        }

        // 底部：上一步 / 完成
        self.paint_panel_btn(p, px + 44.0, py + 236.0, 84.0, 28.0, "上一步", DlAction::PickPrev);
        self.paint_panel_btn(p, px + pw - 128.0, py + 236.0, 84.0, 28.0, "完成", DlAction::PickDone);
    }

    fn paint_panel_btn(&mut self, p: &Painter, x: f32, y: f32, w: f32, h: f32, label: &str, action: DlAction) {
        let hov = self.hover == Some(action);
        p.fill_round(x, y, w, h, 8.0, if hov { gdi::argb(34, 255, 255, 255) } else { gdi::argb(16, 255, 255, 255) });
        p.stroke_round(x, y, w, h, 8.0, 1.0, BORDER_SUB);
        p.text(label, x, y, w, h, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, ROW_TXT);
        Self::hit_add(&mut self.regions, x, y, w, h, action);
    }

    /// 提醒 / 重复 / 优先级 下拉
    fn paint_dropdown(&mut self, p: &Painter) {
        let Some(Drop::List { list, .. }) = &self.drop else { return };
        let list = *list;
        let (px, py, pw, ph) = self.drop_rect();
        p.fill_round(px, py, pw, ph, 10.0, DROP_BG);
        p.stroke_round(px, py, pw, ph, 10.0, 1.0, gdi::argb(90, 62, 135, 250));
        let n = match list {
            0 => events::REMIND_VALUES.len(),
            1 => events::REPEAT_VALUES.len(),
            _ => 1 + PRIORITIES.len(),
        };
        for i in 0..n {
            let iy = py + 4.0 + i as f32 * LIST_ROW;
            let action = DlAction::ListItem(i);
            let sel = match list {
                0 => events::REMIND_VALUES.get(i) == Some(&self.remind),
                1 => events::REPEAT_VALUES.get(i) == Some(&self.repeat),
                _ => {
                    let v = if i == 0 { 0 } else { PRIORITIES[i - 1].0 };
                    v == self.priority
                }
            };
            let hov = self.hover == Some(action);
            if hov || sel {
                p.fill_round(px + 4.0, iy, pw - 8.0, LIST_ROW - 2.0, 6.0, if sel { SEL_BG } else { HOVER_BG });
            }
            if list == 2 {
                // 优先级：首项“不选”，其后带罗马数字徽标
                if i == 0 {
                    p.text("不选", px + 16.0, iy, pw - 28.0, LIST_ROW - 2.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, false, false, if sel { BLUE } else { ROW_TXT });
                } else if let Some((_, numeral, label, color)) = PRIORITIES.get(i - 1) {
                    self.paint_badge(p, px + 12.0, iy + 6.0, 16.0, numeral, *color);
                    p.text(*label, px + 34.0, iy, pw - 44.0, LIST_ROW - 2.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, false, false, if sel { BLUE } else { ROW_TXT });
                }
            } else {
                let v = if list == 0 { events::REMIND_VALUES.get(i).copied().flatten() } else { events::REPEAT_VALUES.get(i).copied().flatten() };
                let label = if list == 0 { events::remind_label(v) } else { events::repeat_label(v) };
                p.text(&label, px + 16.0, iy, pw - 28.0, LIST_ROW - 2.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, false, false, if sel { BLUE } else { ROW_TXT });
            }
            Self::hit_add(&mut self.regions, px + 4.0, iy, pw - 8.0, LIST_ROW - 2.0, action);
        }
    }
}

fn days_in_month(y: i32, m: u32) -> i32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    NaiveDate::from_ymd_opt(ny, nm, 1)
        .and_then(|d| d.pred_opt())
        .map(|d| d.day() as i32)
        .unwrap_or(30)
}

fn wrap_lines(p: &Painter, s: &str, max_w: f32) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for para in s.split('\n') {
        if para.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        for ch in para.chars() {
            let cw = p.measure(&ch.to_string(), 12.5, false, false).0;
            if !cur.is_empty() && p.measure(&cur, 12.5, false, false).0 + cw > max_w {
                out.push(std::mem::take(&mut cur));
            }
            cur.push(ch);
        }
        out.push(cur);
    }
    out
}

fn confirm() {
    let mut guard = IB_UI.lock().unwrap();
    let Some(f) = guard.as_mut() else { return };
    let f = &mut f.0;
    match f.kind {
        Kind::Todo => {
            let text = f.todo_text.trim().to_string();
            if text.is_empty() {
                return;
            }
            let key = crate::ics::key_of_date(f.date);
            let timed = f.time_on;
            let todo = crate::sidebar::Todo {
                text,
                done: false,
                date: Some(key),
                priority: f.priority,
                has_time: timed,
                start: if timed { Some(events::fmt_dt_store(f.a_start)) } else { None },
                end: if timed { Some(events::fmt_dt_store(f.a_end)) } else { None },
                remind: if timed { f.remind } else { None },
                repeat: if timed { f.repeat } else { None },
            };
            drop(guard);
            crate::sidebar::add_todo_full(todo);
        }
        Kind::Agenda => {
            let name = f.agenda_name.trim().to_string();
            if name.is_empty() {
                return;
            }
            let key = crate::ics::key_of_date(f.date);
            let entry = AgendaEntry::Rich(RichEvent {
                name,
                all_day: f.all_day,
                start: if f.all_day { events::fmt_d_store(f.a_start.date()) } else { events::fmt_dt_store(f.a_start) },
                end: if f.all_day { events::fmt_d_store(f.a_end.date()) } else { events::fmt_dt_store(f.a_end) },
                remind: f.remind,
                repeat: f.repeat,
            });
            if let Some(agenda) = IB_AGENDA.lock().unwrap().as_ref() {
                let mut map = agenda.lock().unwrap();
                map.entry(key).or_default().push(entry);
                events::save(&map);
            }
            drop(guard);
        }
    }
    unsafe {
        let h = IB_HWND.load(Ordering::Relaxed);
        if h != 0 {
            ShowWindow(h as HWND, SW_HIDE);
            KillTimer(h as HWND, 1);
        }
    }
    crate::sidebar::sidebar_repaint();
    crate::flyout::flyout_repaint();
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

unsafe fn paste_clipboard(text: &mut String) {
    let h = IB_HWND.load(Ordering::Relaxed);
    if OpenClipboard(h as HWND) == 0 {
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
            text.push_str(&String::from_utf16_lossy(slice));
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
                        f.text_mut().push(ch);
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
                        // 回车：日程确认；待办为多行文本，Ctrl+Enter 确认
                        let ctrl = (GetKeyState(0x11) as u16) & 0x8000 != 0;
                        if f.kind == Kind::Agenda || ctrl {
                            drop(guard);
                            confirm();
                            return 0;
                        }
                        f.text_mut().push('\n');
                        f.redraw();
                    }
                    0x1B => {
                        // Esc：先收起下拉，再取消弹窗
                        if f.drop.is_some() {
                            f.drop = None;
                            f.redraw();
                        } else {
                            drop(guard);
                            cancel();
                            return 0;
                        }
                    }
                    0x08 => {
                        f.text_mut().pop();
                        f.redraw();
                    }
                    0x56 => {
                        // Ctrl+V 粘贴
                        if (GetKeyState(0x11) as u16) & 0x8000 != 0 {
                            let t = f.text_mut();
                            paste_clipboard(t);
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
                        f.text_mut().push_str(&s);
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
            // 失焦：焦点仍在软件自身窗口（主日历/侧栏等）时保持打开，切到其它程序才取消
            if (wp & 0xFFFF) as u16 == 0 && !crate::flyout::foreground_is_own() {
                cancel();
            }
            0
        }
        WM_LBUTTONDOWN => {
            let x = ((lp & 0xFFFF) as u16 as i16) as f32;
            let y = (((lp as usize) >> 16) as u16 as i16) as f32;
            SetForegroundWindow(hwnd);
            let mut guard = IB_UI.lock().unwrap();
            let Some(sui) = guard.as_mut() else { return 0 };
            let f = &mut sui.0;
            // 面板打开时点击面板外：先收起（吞掉本次点击）
            if f.drop.is_some() {
                let (dx, dy, dw, dh) = f.drop_rect();
                if x < dx || x >= dx + dw || y < dy || y >= dy + dh {
                    f.drop = None;
                    f.redraw();
                    return 0;
                }
            }
            let hit = f.regions.iter().rev().find(|(r, _)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h).map(|(_, a)| *a);
            match hit {
                Some(DlAction::Drag) => {
                    drop(guard);
                    SendMessageW(hwnd, WM_NCLBUTTONDOWN, HTCAPTION as WPARAM, 0);
                }
                Some(DlAction::Close) => {
                    drop(guard);
                    cancel();
                }
                Some(DlAction::Save) => {
                    drop(guard);
                    confirm();
                }
                Some(DlAction::ToggleTime) => {
                    f.time_on = !f.time_on;
                    f.drop = None;
                    f.h = dialog_h(f.kind, f.time_on);
                    let hh = f.h;
                    drop(guard);
                    unsafe {
                        SetWindowPos(hwnd, std::ptr::null_mut(), 0, 0, DL_W as i32, hh as i32, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
                    }
                    redraw();
                }
                Some(DlAction::ToggleAllDay) => {
                    f.all_day = !f.all_day;
                    if f.drop.is_some() {
                        f.drop = None; // 全天切换后选择面板形态变化，直接收起
                    }
                    f.redraw();
                }
                Some(DlAction::PickRow(row)) => {
                    let dt = f.row_dt(row);
                    let anchor = y;
                    f.drop = Some(Drop::Date { row, y: dt.year(), m: dt.month(), anchor });
                    f.redraw();
                }
                Some(DlAction::DropRow(list)) => {
                    f.drop = Some(Drop::List { list, anchor: y });
                    f.redraw();
                }
                Some(DlAction::PickMonthPrev) => {
                    if let Some(Drop::Date { y: by, m: bm, .. }) = &mut f.drop {
                        let (ny, nm) = if *bm == 1 { (*by - 1, 12) } else { (*by, *bm - 1) };
                        *by = ny;
                        *bm = nm;
                    }
                    f.redraw();
                }
                Some(DlAction::PickMonthNext) => {
                    if let Some(Drop::Date { y: by, m: bm, .. }) = &mut f.drop {
                        let (ny, nm) = if *bm == 12 { (*by + 1, 1) } else { (*by, *bm + 1) };
                        *by = ny;
                        *bm = nm;
                    }
                    f.redraw();
                }
                Some(DlAction::PickDay(idx)) => {
                    let (row, by, bm) = match &f.drop {
                        Some(Drop::Date { row, y, m, .. }) => (*row, *y, *m),
                        _ => (0, 2026, 1),
                    };
                    let first_wd = NaiveDate::from_ymd_opt(by, bm, 1).map(|d| d.weekday().num_days_from_sunday() as i32).unwrap_or(0);
                    let day = idx as i32 - first_wd + 1;
                    if day >= 1 && day <= days_in_month(by, bm) {
                        if let Some(d0) = NaiveDate::from_ymd_opt(by, bm, day as u32) {
                            let src = f.row_dt(row);
                            f.set_row_dt(row, date_at(d0, src.hour(), src.minute()));
                        }
                    }
                    f.redraw();
                }
                Some(DlAction::PickNext) => {
                    // 日期选好 → 进入时间选择（全天日程无此按钮）
                    if let Some(Drop::Date { row, anchor, .. }) = f.drop.take() {
                        let dt = f.row_dt(row);
                        let h_off = (dt.hour() as i32 - 3).clamp(0, 23 - PICK_VISIBLE + 1);
                        let m_off = (dt.minute() as i32 - 3).clamp(0, 59 - PICK_VISIBLE + 1);
                        f.drop = Some(Drop::Time { row, h_off, m_off, anchor });
                    }
                    f.redraw();
                }
                Some(DlAction::PickPrev) => {
                    if let Some(Drop::Time { row, anchor, .. }) = f.drop.take() {
                        let dt = f.row_dt(row);
                        f.drop = Some(Drop::Date { row, y: dt.year(), m: dt.month(), anchor });
                    }
                    f.redraw();
                }
                Some(DlAction::PickHour(v)) => {
                    let row = match &f.drop {
                        Some(Drop::Time { row, .. }) => *row,
                        _ => 0,
                    };
                    let src = f.row_dt(row);
                    f.set_row_dt(row, date_at(src.date(), v.min(23), src.minute()));
                    f.redraw();
                }
                Some(DlAction::PickMinute(v)) => {
                    let row = match &f.drop {
                        Some(Drop::Time { row, .. }) => *row,
                        _ => 0,
                    };
                    let src = f.row_dt(row);
                    f.set_row_dt(row, date_at(src.date(), src.hour(), v.min(59)));
                    f.redraw();
                }
                Some(DlAction::PickDone) => {
                    f.drop = None;
                    f.redraw();
                }
                Some(DlAction::ListItem(i)) => {
                    let list = match &f.drop {
                        Some(Drop::List { list, .. }) => *list,
                        _ => 0,
                    };
                    match list {
                        0 => {
                            if let Some(v) = events::REMIND_VALUES.get(i) {
                                f.remind = *v;
                            }
                        }
                        1 => {
                            if let Some(v) = events::REPEAT_VALUES.get(i) {
                                f.repeat = *v;
                            }
                        }
                        _ => {
                            if i == 0 {
                                f.priority = 0;
                            } else if let Some((v, _, _, _)) = PRIORITIES.get(i - 1) {
                                f.priority = *v;
                            }
                        }
                    }
                    f.drop = None;
                    f.redraw();
                }
                _ => {}
            }
            0
        }
        WM_MOUSEMOVE => {
            let mut guard = IB_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                let x = ((lp & 0xFFFF) as u16 as i16) as f32;
                let y = (((lp as usize) >> 16) as u16 as i16) as f32;
                let hit = f.regions.iter().rev().find(|(r, _)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h).map(|(_, a)| *a);
                if hit != f.hover {
                    f.hover = hit;
                    f.redraw();
                }
                let clickable = matches!(hit, Some(a) if !matches!(a, DlAction::Noop));
                SetCursor(LoadCursorW(std::ptr::null_mut(), if clickable { IDC_HAND } else { IDC_ARROW }));
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
            let mut guard = IB_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                if f.hover.is_some() {
                    f.hover = None;
                    f.redraw();
                }
            }
            0
        }
        WM_MOUSEWHEEL => {
            // 滚动时/分列表
            let delta = ((wp >> 16) as i16) as i32;
            let mut pt = POINT { x: (lp & 0xFFFF) as u16 as i16 as i32, y: (((lp as usize) >> 16) as u16 as i16) as i32 };
            unsafe { ScreenToClient(hwnd, &mut pt) };
            let mut guard = IB_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                if matches!(f.drop, Some(Drop::Time { .. })) {
                    let (px, py, pw, _) = f.drop_rect();
                    let ly = py + 34.0;
                    let lbot = ly + PICK_VISIBLE as f32 * PICK_ROW;
                    let in_hour = (pt.x as f32) >= px + 18.0 && (pt.x as f32) < px + 18.0 + 122.0 && (pt.y as f32) >= ly && (pt.y as f32) < lbot;
                    let in_min = (pt.x as f32) >= px + pw - 140.0 && (pt.x as f32) < px + pw - 140.0 + 122.0 && (pt.y as f32) >= ly && (pt.y as f32) < lbot;
                    if let Some(Drop::Time { h_off, m_off, .. }) = &mut f.drop {
                        if in_hour {
                            *h_off = (*h_off - delta / 120).clamp(0, 24 - PICK_VISIBLE);
                            f.redraw();
                        } else if in_min {
                            *m_off = (*m_off - delta / 120).clamp(0, 60 - PICK_VISIBLE);
                            f.redraw();
                        }
                    }
                }
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
