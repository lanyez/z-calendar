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
use winapi::um::winuser::*;

use crate::events::{self, AgendaEntry, AgendaMap, RichEvent};
use crate::gdi::{self, Cache, Painter};

const DL_W: f32 = 400.0;
const H_MAX: f32 = 620.0; // 位图按最大高度分配（待办开时间 + 修改范围/重复至两行附加卡时最高）

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
    ToggleUntil,         // 重复截止日开关
    ScopeSeries(bool),   // 修改范围：true=整个系列 false=仅这一天
    PickRow(usize),  // 0=开始 1=结束 2=重复至 → 日期选择
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
    hbmp: usize,
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
    /// 按天重复：d=每天 w=每周 m=每月 y=每年 l=农历每年
    recur: Option<String>,
    /// 重复截止日开关 + 日期（recur 生效时可编辑）
    until_on: bool,
    until_dt: NaiveDateTime,
    /// 修改范围（编辑重复条目时显示）：true=整个系列 false=仅这一天
    scope_series: bool,
    /// 出现日期（编辑重复条目时 Some，用于“仅这一天”定位；非重复为 None）
    scope_date: Option<NaiveDate>,
    comp: String,
    caret_on: bool,
    drop: Option<Drop>,
    regions: Vec<(gdi::RectF, DlAction)>,
    hover: Option<DlAction>,
    /// 编辑模式：Some((日期key, 下标, 原id, 原仅此次例外))——保存时原位替换（日期改动则移动）
    edit_agenda: Option<(String, usize, String, Vec<String>)>,
    /// 编辑模式：Some((全局下标, 原完成状态, 原id, 原按天完成记录, 原仅此次例外))
    edit_todo: Option<(usize, bool, String, Vec<String>, Vec<String>)>,
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

/// 弹窗总高度（与 paint 的布局累加保持一致）；recur_rows = 重复附加卡（修改范围/重复至）行数
fn dialog_h(kind: Kind, time_on: bool, recur_rows: usize) -> f32 {
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
    if recur_rows > 0 {
        y += 12.0 + ROW_H * recur_rows as f32 + GAP; // 重复附加卡
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
            gdi::phys(DL_W) as i32,
            gdi::phys(H_MAX) as i32,
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
            sf: gdi::scale(),
            h: 386.0,
            mem_dc: 0,
            hbmp: 0,
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
            recur: None,
            until_on: false,
            until_dt: date_at(now.date(), 0, 0),
            scope_series: true,
            scope_date: None,
            comp: String::new(),
            caret_on: true,
            drop: None,
            regions: Vec::new(),
            hover: None,
            edit_agenda: None,
            edit_todo: None,
        });
        // 后台位图不在创建时分配：open_with→redraw 惰性分配，关闭即释放
        IB_UI.lock().unwrap().replace(SendIb(ui));
    }
}

fn date_at(d: NaiveDate, h: u32, m: u32) -> NaiveDateTime {
    d.and_hms_opt(h, m, 0).unwrap_or_else(|| d.and_hms_opt(0, 0, 0).unwrap())
}

/// 在指定位置附近打开弹窗（新增）
pub fn open(at_x: i32, at_y: i32, date: NaiveDate, kind: Kind) {
    open_with(at_x, at_y, date, kind, Prefill::New);
}

/// 编辑已有日程（key + 下标定位；日期改动则移动到新日期）。
/// occ：重复条目的出现日期（侧栏/日程页当前查看的日子），用于“仅这一天”修改；
/// 传入时表单日期预定位到该次出现（保留时刻）。
pub fn open_agenda_edit(at_x: i32, at_y: i32, key: &str, idx: usize, ev: &RichEvent, occ: Option<NaiveDate>) {
    let Some(dt) = events::parse_start(&ev.start) else { return };
    let end = events::parse_start(&ev.end).unwrap_or(dt + chrono::Duration::hours(1));
    // 重复条目：表单预定位到本次出现的日期（保留时刻）
    let view_date = if ev.recur.is_some() { occ.unwrap_or(dt.date()) } else { dt.date() };
    let shifted = |d: NaiveDateTime| {
        if view_date != dt.date() {
            date_at(view_date, d.hour(), d.minute())
        } else {
            d
        }
    };
    let (s, e) = if ev.all_day {
        (date_at(view_date, 0, 0), date_at(end.date().max(view_date), 0, 0))
    } else {
        (shifted(dt), shifted(end))
    };
    open_with(
        at_x,
        at_y,
        view_date,
        Kind::Agenda,
        Prefill::Agenda {
            key: key.to_string(),
            idx,
            id: ev.id.clone(),
            name: ev.name.clone(),
            all_day: ev.all_day,
            start: s,
            end: e,
            remind: ev.remind,
            repeat: ev.repeat,
            recur: ev.recur.clone(),
            until: ev.recur_until.clone(),
            skips: ev.skip_dates.clone(),
            occ: if ev.recur.is_some() { Some(view_date) } else { None },
        },
    );
}

/// 编辑已有待办（全局下标定位；完成状态保持不变）。occ 语义同 open_agenda_edit。
pub fn open_todo_edit(at_x: i32, at_y: i32, gi: usize, td: &crate::sidebar::Todo, occ: Option<NaiveDate>) {
    let anchor = td
        .date
        .as_deref()
        .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
    let date = if td.recur.is_some() {
        occ.or(anchor).unwrap_or_else(|| chrono::Local::now().date_naive())
    } else {
        anchor.unwrap_or_else(|| chrono::Local::now().date_naive())
    };
    let moved = td.recur.is_some() && anchor.map(|a| a != date).unwrap_or(false);
    let st0 = td.start.as_deref().and_then(events::parse_start).unwrap_or_else(|| date_at(date, 9, 0));
    let en0 = td.end.as_deref().and_then(events::parse_start).unwrap_or_else(|| date_at(date, 10, 0));
    let (st, en) = if moved {
        (date_at(date, st0.hour(), st0.minute()), date_at(date, en0.hour(), en0.minute()))
    } else {
        (st0, en0)
    };
    open_with(
        at_x,
        at_y,
        date,
        Kind::Todo,
        Prefill::Todo {
            gi,
            done: td.done,
            id: td.id.clone(),
            text: td.text.clone(),
            priority: td.priority,
            time_on: td.has_time,
            start: st,
            end: en,
            remind: td.remind,
            repeat: td.repeat,
            recur: td.recur.clone(),
            until: td.recur_until.clone(),
            skips: td.skip_dates.clone(),
            done_dates: td.done_dates.clone(),
            occ: if td.recur.is_some() { Some(date) } else { None },
        },
    );
}

enum Prefill {
    New,
    Agenda { key: String, idx: usize, id: String, name: String, all_day: bool, start: NaiveDateTime, end: NaiveDateTime, remind: Option<i64>, repeat: Option<i64>, recur: Option<String>, until: Option<String>, skips: Vec<String>, occ: Option<NaiveDate> },
    Todo { gi: usize, done: bool, id: String, text: String, priority: u8, time_on: bool, start: NaiveDateTime, end: NaiveDateTime, remind: Option<i64>, repeat: Option<i64>, recur: Option<String>, until: Option<String>, skips: Vec<String>, done_dates: Vec<String>, occ: Option<NaiveDate> },
}

fn open_with(at_x: i32, at_y: i32, date: NaiveDate, kind: Kind, pre: Prefill) {
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
                f.recur = None;
                f.until_on = false;
                f.until_dt = date_at(date, 0, 0);
                f.scope_series = true;
                f.scope_date = None;
                f.caret_on = true;
                f.drop = None;
                f.hover = None;
                f.edit_agenda = None;
                f.edit_todo = None;
                match pre {
                    Prefill::New => {}
                    Prefill::Agenda { key, idx, id, name, all_day, start, end, remind, repeat, recur, until, skips, occ } => {
                        f.edit_agenda = Some((key, idx, id, skips));
                        f.agenda_name = name;
                        f.all_day = all_day;
                        f.a_start = start;
                        f.a_end = end;
                        f.remind = remind;
                        f.repeat = repeat;
                        f.recur = recur;
                        f.until_on = until.is_some();
                        f.until_dt = until
                            .and_then(|s| chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
                            .and_then(|d| d.and_hms_opt(0, 0, 0))
                            .unwrap_or_else(|| date_at(start.date() + chrono::Duration::days(30), 0, 0));
                        f.scope_date = occ;
                    }
                    Prefill::Todo { gi, done, id, text, priority, time_on, start, end, remind, repeat, recur, until, skips, done_dates, occ } => {
                        f.edit_todo = Some((gi, done, id, done_dates, skips));
                        f.todo_text = text;
                        f.priority = priority;
                        f.time_on = time_on;
                        f.a_start = start;
                        f.a_end = end;
                        f.remind = remind;
                        f.repeat = repeat;
                        f.recur = recur;
                        f.until_on = until.is_some();
                        f.until_dt = until
                            .and_then(|s| chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
                            .and_then(|d| d.and_hms_opt(0, 0, 0))
                            .unwrap_or_else(|| date_at(start.date() + chrono::Duration::days(30), 0, 0));
                        f.scope_date = occ;
                    }
                }
                f.h = dialog_h(f.kind, f.time_on, f.recur_extra_rows());
            }
        }
        // 工作区钳制（鼠标所在显示器，物理像素）
        let mon = MonitorFromPoint(POINT { x: at_x, y: at_y }, MONITOR_DEFAULTTONEAREST);
        let (wa_l, wa_t, wa_r, wa_b) = if !mon.is_null() {
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(mon, &mut mi) != 0 {
                (mi.rcWork.left, mi.rcWork.top, mi.rcWork.right, mi.rcWork.bottom)
            } else {
                (0, 0, at_x + gdi::phys(DL_W) as i32, at_y + gdi::phys(400.0) as i32)
            }
        } else {
            (0, 0, at_x + gdi::phys(DL_W) as i32, at_y + gdi::phys(400.0) as i32)
        };
        let hh;
        {
            let guard = IB_UI.lock().unwrap();
            hh = guard.as_ref().map(|s| s.0.h).unwrap_or(386.0);
        }
        // 屏幕正中间（鼠标所在显示器的工作区）
        let x = wa_l + (wa_r - wa_l - gdi::phys(DL_W) as i32) / 2;
        let y = wa_t + (wa_b - wa_t - gdi::phys(hh) as i32) / 2;
        SetWindowPos(h, HWND_TOPMOST, x, y, gdi::phys(DL_W) as i32, gdi::phys(hh) as i32, SWP_NOACTIVATE);
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

/// 弹窗高度变化后重设尺寸并保持垂直居中（钳制在所在显示器工作区内）。
/// old_h/new_h 由调用方给出：redraw 的 ULW 已把窗口改成新高度，不能再从窗口实测。
unsafe fn resize_keep_center(hwnd: HWND, old_h: f32, new_h: f32) {
    let mut r: RECT = std::mem::zeroed();
    GetWindowRect(hwnd, &mut r);
    let mut ny = r.top - (gdi::phys((new_h - old_h) / 2.0)) as i32;
    let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    if !mon.is_null() {
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(mon, &mut mi) != 0 {
            if ny + gdi::phys(new_h) as i32 > mi.rcWork.bottom {
                ny = mi.rcWork.bottom - gdi::phys(new_h) as i32;
            }
            if ny < mi.rcWork.top {
                ny = mi.rcWork.top;
            }
        }
    }
    SetWindowPos(hwnd, std::ptr::null_mut(), r.left, ny, gdi::phys(DL_W) as i32, gdi::phys(new_h) as i32, SWP_NOZORDER | SWP_NOACTIVATE);
}

impl DialogUi {
    fn redraw(&mut self) {
        if self.bmp.is_null() {
            // 隐藏时位图已释放压缩内存：显示前重建。
            // 必须按最大高度分配：弹窗高度随表单内容动态增长（ULW 直接提交新高度）
            let (mem_dc, hbmp, bmp, scan0) = unsafe { gdi::alloc_dib(DL_W, H_MAX) };
            self.mem_dc = mem_dc;
            self.hbmp = hbmp;
            self.bmp = bmp;
            self.scan0 = scan0;
        }
        if self.g.is_null() {
            unsafe { GdipGetImageGraphicsContext(self.bmp, &mut self.g); }
        }
        let cache_ptr: *const Cache = &self.cache;
        let g = self.g;
        unsafe {
            GdipSetSmoothingMode(g, gdi::SMOOTH_ANTI_ALIAS);
            GdipSetTextRenderingHint(g, gdi::text_hint());
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
            let mut size = SIZE { cx: gdi::phys(DL_W) as i32, cy: gdi::phys(self.h) as i32 };
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

    /// 重复附加卡行数：修改范围（编辑重复条目时）+ 重复至（选了按天重复时）
    fn recur_extra_rows(&self) -> usize {
        if self.recur.is_none() {
            return 0;
        }
        1 + self.scope_date.is_some() as usize
    }

    fn row_dt(&self, row: usize) -> NaiveDateTime {
        match row {
            0 => self.a_start,
            1 => self.a_end,
            _ => self.until_dt,
        }
    }

    fn set_row_dt(&mut self, row: usize, dt: NaiveDateTime) {
        match row {
            0 => {
                self.a_start = dt;
                if self.a_end < dt {
                    self.a_end = dt;
                }
            }
            1 => {
                self.a_end = dt;
                if self.a_end < self.a_start {
                    self.a_start = self.a_end;
                }
            }
            _ => self.until_dt = dt,
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
                    1 => events::REPEAT_MENU_LEN,
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
        let editing = self.edit_agenda.is_some() || self.edit_todo.is_some();
        let title = match self.kind {
            Kind::Agenda => {
                if editing {
                    "编辑日程"
                } else {
                    "新增日程"
                }
            }
            Kind::Todo => {
                if editing {
                    "编辑待办"
                } else {
                    "新增待办"
                }
            }
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
        // 重复附加卡：修改范围（编辑重复条目）/ 重复至（选了按天重复）
        let rows = self.recur_extra_rows();
        if rows > 0 {
            y = self.paint_recur_extra(p, y);
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
                    p.text(&self.repeat_pill_label(), vx, ry, vw, ROW_H, gdi::HALIGN_FAR, gdi::HALIGN_CENTER, 12.5, false, false, ROW_TXT);
                    p.text("\u{E70D}", DL_W - 44.0, ry, 24.0, ROW_H, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 9.0, false, true, SUB);
                }
            }
            Self::hit_add(&mut self.regions, 16.0, ry, 368.0, ROW_H, action);
            ry += ROW_H;
        }
    }

    /// 重复附加卡：修改范围（整个系列 / 仅这一天）+ 重复至（截止日开关 + 日期）
    fn paint_recur_extra(&mut self, p: &Painter, y: f32) -> f32 {
        let rows = self.recur_extra_rows();
        let ch = 12.0 + ROW_H * rows as f32;
        p.fill_round(14.0, y, 372.0, ch, 8.0, CARD_BG);
        let mut ry = y + 6.0;
        if self.scope_date.is_some() {
            p.text("修改范围", 20.0, ry, 80.0, ROW_H, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, false, false, ROW_TXT);
            let pills = [("整个系列", true), ("仅这一天", false)];
            let mut px = DL_W - 20.0 - 76.0 * 2.0 - 8.0;
            for (label, on) in pills {
                let sel = self.scope_series == on;
                let action = DlAction::ScopeSeries(on);
                let hov = self.hover == Some(action);
                if sel || hov {
                    p.fill_round(px, ry + 8.0, 76.0, ROW_H - 16.0, 6.0, if sel { SEL_BG } else { HOVER_BG });
                } else {
                    p.stroke_round(px, ry + 8.0, 76.0, ROW_H - 16.0, 6.0, 1.0, BORDER_SUB);
                }
                p.text(label, px, ry, 76.0, ROW_H, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, if sel { WHITE } else { ROW_TXT });
                Self::hit_add(&mut self.regions, px, ry, 76.0, ROW_H, action);
                px += 84.0;
            }
            p.line(20.0, ry + ROW_H, DL_W - 20.0, ry + ROW_H, 1.0, gdi::argb(10, 255, 255, 255));
            ry += ROW_H;
        }
        p.text("重复至", 20.0, ry, 80.0, ROW_H, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, false, false, ROW_TXT);
        self.paint_switch(p, DL_W - 52.0, ry + 10.0, self.until_on);
        Self::hit_add(&mut self.regions, DL_W - 60.0, ry, 56.0, ROW_H, DlAction::ToggleUntil);
        let pick = DlAction::PickRow(2);
        let hov = self.hover == Some(pick);
        if hov {
            p.fill_round(16.0, ry, 320.0, ROW_H, 6.0, HOVER_BG);
        }
        let label = if self.until_on {
            events::fmt_date_cn(self.until_dt.date())
        } else {
            "无限重复".to_string()
        };
        p.text(&label, 70.0, ry, 230.0, ROW_H, gdi::HALIGN_FAR, gdi::HALIGN_CENTER, 12.5, false, false, if self.until_on { ROW_TXT } else { SUB });
        if self.until_on {
            Self::hit_add(&mut self.regions, 70.0, ry, 230.0, ROW_H, pick);
        }
        y + ch + GAP
    }

    /// 日期选择面板（月历 + 底部“下一步/完成”）
    fn paint_date_panel(&mut self, p: &Painter) {
        let Some(Drop::Date { row, y: by, m: bm, .. }) = &self.drop else { return };
        let (row, by, bm) = (*row, *by, *bm);
        let (px, py, pw, ph) = self.drop_rect();
        // 重复至只选日期（无时间步）
        let timed = row != 2 && (self.kind == Kind::Todo || !self.all_day);
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

    /// 重复行的展示文本（按天重复优先于分钟级重复）
    fn repeat_pill_label(&self) -> String {
        if let Some(rc) = &self.recur {
            events::recur_label(rc)
        } else {
            events::repeat_label(self.repeat)
        }
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
            1 => events::REPEAT_MENU_LEN,
            _ => 1 + PRIORITIES.len(),
        };
        for i in 0..n {
            let iy = py + 4.0 + i as f32 * LIST_ROW;
            let action = DlAction::ListItem(i);
            let sel = match list {
                0 => events::REMIND_VALUES.get(i) == Some(&self.remind),
                1 => {
                    if i < events::REPEAT_MENU_MIN {
                        self.recur.is_none() && events::REPEAT_VALUES.get(i) == Some(&self.repeat)
                    } else {
                        events::RECUR_VALUES.get(i - events::REPEAT_MENU_MIN).copied() == self.recur.as_deref()
                    }
                }
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
            } else if list == 1 && i >= events::REPEAT_MENU_MIN {
                // 按天重复项（跟在分钟级重复之后）
                let label = events::recur_label(events::RECUR_VALUES[i - events::REPEAT_MENU_MIN]);
                p.text(&label, px + 16.0, iy, pw - 28.0, LIST_ROW - 2.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, false, false, if sel { BLUE } else { ROW_TXT });
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
    // 重复截止日（仅按天重复时存储）
    let until_store = if f.recur.is_some() && f.until_on {
        Some(events::fmt_d_store(f.until_dt.date()))
    } else {
        None
    };
    match f.kind {
        Kind::Todo => {
            let text = f.todo_text.trim().to_string();
            if text.is_empty() {
                return;
            }
            let key = crate::ics::key_of_date(f.date);
            let timed = f.time_on;
            let edit = f.edit_todo.take();
            let scope_only = f.recur.is_some() && !f.scope_series && edit.is_some();
            let scope_date = f.scope_date.unwrap_or(f.date);
            let todo = crate::sidebar::Todo {
                id: edit.as_ref().map(|(_, _, id, _, _)| id.clone()).unwrap_or_default(),
                text,
                done: edit.as_ref().map(|(_, done, _, _, _)| *done).unwrap_or(false),
                date: Some(key),
                priority: f.priority,
                has_time: timed,
                start: if timed { Some(events::fmt_dt_store(f.a_start)) } else { None },
                end: if timed { Some(events::fmt_dt_store(f.a_end)) } else { None },
                remind: if timed { f.remind } else { None },
                repeat: if timed { f.repeat } else { None },
                recur: f.recur.clone(),
                recur_until: until_store.clone(),
                skip_dates: edit.as_ref().map(|(_, _, _, _, sk)| sk.clone()).unwrap_or_default(),
                done_dates: edit.as_ref().map(|(_, _, _, dd, _)| dd.clone()).unwrap_or_default(),
            };
            drop(guard);
            if scope_only {
                // 仅这一天：系列条目追加例外（可顺带更新截止日），编辑结果另存为独立待办
                if let Some((gi, ..)) = edit {
                    crate::sidebar::todo_patch_recur(gi, until_store, Some(scope_date));
                }
                let mut single = todo;
                single.recur = None;
                single.recur_until = None;
                single.skip_dates = Vec::new();
                single.done = false;
                single.done_dates = Vec::new();
                single.id = String::new();
                crate::sidebar::add_todo_full(single);
            } else if let Some((gi, ..)) = edit {
                crate::sidebar::update_todo_at(gi, todo);
            } else {
                crate::sidebar::add_todo_full(todo);
            }
        }
        Kind::Agenda => {
            let name = f.agenda_name.trim().to_string();
            if name.is_empty() {
                return;
            }
            let key = crate::ics::key_of_date(f.date);
            let edit = f.edit_agenda.take();
            let scope_only = f.recur.is_some() && !f.scope_series && edit.is_some();
            let mut entry = AgendaEntry::Rich(RichEvent {
                id: edit.as_ref().map(|(_, _, id, _)| id.clone()).unwrap_or_else(events::gen_id),
                name,
                all_day: f.all_day,
                start: if f.all_day { events::fmt_d_store(f.a_start.date()) } else { events::fmt_dt_store(f.a_start) },
                end: if f.all_day { events::fmt_d_store(f.a_end.date()) } else { events::fmt_dt_store(f.a_end) },
                remind: f.remind,
                repeat: f.repeat,
                recur: f.recur.clone(),
                recur_until: until_store.clone(),
                skip_dates: edit.as_ref().map(|(_, _, _, sk)| sk.clone()).unwrap_or_default(),
            });
            if let Some(agenda) = IB_AGENDA.lock().unwrap().as_ref() {
                let mut map = agenda.lock().unwrap();
                match edit {
                    Some((old_key, idx, ..)) => {
                        if scope_only {
                            // 仅这一天：系列条目追加例外（可顺带更新截止日），编辑结果另存为独立日程
                            let scope = f.scope_date.unwrap_or(f.date);
                            if let Some(v) = map.get_mut(&old_key) {
                                if let Some(AgendaEntry::Rich(sr)) = v.get_mut(idx) {
                                    if !events::recur_skipped(&sr.skip_dates, scope) {
                                        sr.skip_dates.push(crate::ics::key_of_date(scope));
                                    }
                                    sr.recur_until = until_store;
                                }
                            }
                            if let AgendaEntry::Rich(sr) = &mut entry {
                                sr.recur = None;
                                sr.recur_until = None;
                                sr.skip_dates = Vec::new();
                                sr.id = events::gen_id();
                            }
                            map.entry(key).or_default().push(entry);
                        } else {
                            // 编辑保存：同日期原位替换；改了日期则移动到新日期
                            if old_key == key {
                                if let Some(v) = map.get_mut(&key) {
                                    if idx < v.len() {
                                        v[idx] = entry;
                                    }
                                }
                            } else {
                                if let Some(v) = map.get_mut(&old_key) {
                                    if idx < v.len() {
                                        v.remove(idx);
                                    }
                                    if v.is_empty() {
                                        map.remove(&old_key);
                                    }
                                }
                                map.entry(key).or_default().push(entry);
                            }
                        }
                        events::save(&map);
                    }
                    None => {
                        map.entry(key).or_default().push(entry);
                        events::save(&map);
                    }
                }
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
    free_surface();
    crate::sidebar::sidebar_repaint();
    crate::flyout::flyout_repaint();
    crate::trim_working_set();
}

/// 释放弹窗后台位图压缩内存（下次 redraw 重建）
fn free_surface() {
    let mut guard = IB_UI.lock().unwrap();
    if let Some(f) = guard.as_mut() {
        let ui = &mut f.0;
        unsafe {
            gdi::free_dib(&mut ui.mem_dc, &mut ui.hbmp, &mut ui.bmp, &mut ui.g, &mut ui.scan0);
        }
    }
}

fn cancel() {
    unsafe {
        let h = IB_HWND.load(Ordering::Relaxed);
        if h != 0 {
            ShowWindow(h as HWND, SW_HIDE);
            KillTimer(h as HWND, 1);
        }
    }
    free_surface();
    crate::trim_working_set();
}

/// 屏幕缩放变化：更新 sf、释放位图；可见时原地重设尺寸并重绘（保留已输入内容）
pub fn rescale(sf: f32) {
    let vis = visible();
    let mut guard = IB_UI.lock().unwrap();
    if let Some(f) = guard.as_mut() {
        let ui = &mut f.0;
        ui.sf = sf;
        unsafe {
            gdi::free_dib(&mut ui.mem_dc, &mut ui.hbmp, &mut ui.bmp, &mut ui.g, &mut ui.scan0);
        }
        if vis {
            unsafe {
                SetWindowPos(
                    ui.hwnd as HWND,
                    std::ptr::null_mut(),
                    0,
                    0,
                    gdi::phys(DL_W) as i32,
                    gdi::phys(ui.h) as i32,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            ui.redraw();
        }
    }
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
            let s = gdi::scale();
            let x = ((lp & 0xFFFF) as u16 as i16) as f32 / s;
            let y = (((lp as usize) >> 16) as u16 as i16) as f32 / s;
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
                    let old_h = f.h;
                    f.h = dialog_h(f.kind, f.time_on, f.recur_extra_rows());
                    let hh = f.h;
                    drop(guard);
                    unsafe {
                        resize_keep_center(hwnd, old_h, hh);
                    }
                    redraw();
                }
                Some(DlAction::ToggleUntil) => {
                    f.until_on = !f.until_on;
                    if f.until_on && f.until_dt.date() < f.a_start.date() {
                        // 打开时默认给出一个截止日：开始日 + 30 天
                        f.until_dt = date_at(f.a_start.date() + chrono::Duration::days(30), 0, 0);
                    }
                    f.redraw();
                }
                Some(DlAction::ScopeSeries(on)) => {
                    f.scope_series = on;
                    f.redraw();
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
                    if i < events::REPEAT_MENU_MIN {
                        // 分钟级重复（与按天重复互斥；重复附加卡随之消失）
                        if let Some(v) = events::REPEAT_VALUES.get(i) {
                            f.repeat = *v;
                            f.recur = None;
                        }
                    } else if let Some(rc) = events::RECUR_VALUES.get(i - events::REPEAT_MENU_MIN) {
                        // 按天重复（每天/每周/每月/每年/农历每年），重复附加卡随之出现
                        f.repeat = None;
                        f.recur = Some(rc.to_string());
                        if f.until_dt.date() < f.a_start.date() {
                            f.until_dt = date_at(f.a_start.date() + chrono::Duration::days(30), 0, 0);
                        }
                    }
                    // 重复附加卡出现/消失：重算弹窗高度（保持垂直居中）
                    let old_h = f.h;
                    let nh = dialog_h(f.kind, f.time_on, f.recur_extra_rows());
                    if (nh - old_h).abs() > 0.5 {
                        f.h = nh;
                        f.drop = None;
                        f.redraw();
                        drop(guard);
                        unsafe {
                            resize_keep_center(hwnd, old_h, nh);
                        }
                        redraw();
                        return 0;
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
                let x = ((lp & 0xFFFF) as u16 as i16) as f32 / f.sf;
                let y = (((lp as usize) >> 16) as u16 as i16) as f32 / f.sf;
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
                    let (cx, cy) = (pt.x as f32 / f.sf, pt.y as f32 / f.sf);
                    let in_hour = cx >= px + 18.0 && cx < px + 18.0 + 122.0 && cy >= ly && cy < lbot;
                    let in_min = cx >= px + pw - 140.0 && cx < px + pw - 140.0 + 122.0 && cy >= ly && cy < lbot;
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
