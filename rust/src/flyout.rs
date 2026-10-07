//! 日历弹窗：Win32 分层窗口 + GDI+ 自绘
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, RwLock};

use chrono::{Datelike, Duration, Local, NaiveDate, Timelike};
use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::windef::{HWND, POINT, RECT, SIZE};
use winapi::um::wingdi::CreateRectRgn;
use winapi::um::winuser::*;

use crate::config::{apply_autostart, Config};
use crate::gdi::{self, Cache, Painter};
use crate::ics::{DayInfo, DayType, HolidayMap};
use crate::lunar::{self, FestKind};
use crate::tray;
use crate::weather::Weather;

// ---------------- 配色（深浅色取自 theme 色板） ----------------
fn BG() -> u32 { crate::theme::pal().bg }
fn BLUE() -> u32 { crate::theme::pal().blue }
/// 待办橙（月历格待办角标与日程页“待办”tag 同色）
fn TODO_ORANGE() -> u32 { crate::theme::pal().orange }
fn RED() -> u32 { crate::theme::pal().red }
fn TXT() -> u32 { crate::theme::pal().txt }
fn DATE_COL() -> u32 { crate::theme::pal().date }
fn SUB() -> u32 { crate::theme::pal().sub }
fn SUB_DIM() -> u32 { crate::theme::pal().dim }
fn LEGAL() -> u32 { crate::theme::pal().legal }
/// 节日/节假日名称统一使用的鲜艳蓝色
fn FEST_BLUE() -> u32 { crate::theme::pal().fest }
/// 日历格悬停圆圈的半透明高亮
fn CELL_HOVER() -> u32 { crate::theme::ov(28) }
fn WEEK_HEAD() -> u32 { crate::theme::pal().week_head }
fn WEEK_NUM() -> u32 { crate::theme::pal().week_num }
fn ICON_COL() -> u32 { crate::theme::pal().icon }
fn ROW_TXT() -> u32 { crate::theme::pal().row }
fn TITLE_COL() -> u32 { crate::theme::pal().title }
fn HOVER_BG() -> u32 { crate::theme::ov(20) }
fn ON_BG() -> u32 { crate::theme::pal().on_bg }
const WHITE: u32 = gdi::argb(255, 255, 255, 255);
fn PLUS_TOP() -> u32 { crate::theme::pal().plus_top }
fn PLUS_BOT() -> u32 { crate::theme::pal().plus_bot }
fn SUN() -> u32 { crate::theme::pal().sun }
fn CLOUD() -> u32 { crate::theme::pal().cloud }
fn RAIN() -> u32 { crate::theme::pal().rain }
fn BORDER() -> u32 { crate::theme::ov(22) }
fn DIVIDER() -> u32 { crate::theme::ov(20) }
fn POPUP_BG() -> u32 { crate::theme::pal().card }
/// 文本选区高亮（快捷输入/编辑框选中段）
fn SEL_BLUE() -> u32 { gdi::argb(70, 62, 135, 250) }

pub const WIN_W: f32 = 510.0;
pub const WIN_H: f32 = 640.0;

/// 可见面板在窗口内的内缩量：paint_frame 与各页布局都以窗口左上 (10,10) 为面板原点，
/// 窗口因此比可见面板大出一圈。贴屏幕右缘/任务栏摆放时这一圈被裁到屏幕外。
const PANEL_INSET: f32 = 10.0;

/// 面板圆角半径；贴住屏幕/任务栏的一侧不画圆角（否则边缘会留下月牙形空隙）
const PANEL_RADIUS: f32 = 12.0;

/// 可见面板下缘与任务栏上缘的间距：贴死会把任务栏顶边压住（圆角补平后更是连成一片），
/// 留一点缝把日历整体抬起来。嫌高/嫌低改这一个数即可。
const EDGE_GAP: f32 = 2.0;

/// 任务栏不在屏幕底部时，面板下缘与时钟上缘的间距
const CLOCK_GAP: f32 = 6.0;

const WM_APP_TOGGLE: UINT = 0x8000 + 1;
const WM_APP_SHOW: UINT = 0x8000 + 2;
const WM_APP_ICS_DONE: UINT = 0x8000 + 3;

static FLYOUT_HWND: AtomicUsize = AtomicUsize::new(0);
static SHOWN_FLAG: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone)]
pub struct SharedState {
    pub config: Arc<Mutex<Config>>,
    pub holidays: Arc<RwLock<HolidayMap>>,
    pub weather: Arc<Mutex<Option<Weather>>>,
    pub clock: crate::overlay::SharedClock,
    pub refresh_tx: Sender<()>,
    pub weather_tx: Sender<()>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    None,
    Prev,
    Next,
    Title,
    Cell(NaiveDate),
    BottomAgenda,
    BottomToday,
    BottomPlus,
    BottomSettings,
    BottomExit,
    Back,
    AgendaDel(usize),
    /// 点击日程条目 → 编辑弹窗
    AgendaEdit(usize),
    /// 点击待办条目 → 编辑弹窗（日程页合并列表）
    TodoEdit(usize),
    /// 删除待办条目（日程页合并列表）
    TodoDel(usize),
    /// 切换待办完成状态（勾选圈；日程页）
    TodoToggle(usize),
    /// 底部快捷输入的类型下拉（日程/待办）
    KindDropdown,
    KindPick(usize),
    AgendaAdd,
    InputBox,
    OpenSettings,
    /// 头部左侧天气热区（悬停弹出近一周天气面板）
    Weather,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Page {
    Calendar,
    Agenda,
}

/// 日程页扁平行：日期分组头 / 日程条目（agenda_resolved 下标）/ 待办条目（agenda_todos 下标）
#[derive(Clone, Copy, Debug)]
enum AgRow {
    Head(NaiveDate, bool),
    Agenda(usize),
    Todo(usize),
}

struct Ui {
    hwnd: usize,
    sf: f32,
    w: f32,
    h: f32,
    mem_dc: usize,
    hbmp: usize,
    bmp: gdi::Gp,
    scan0: *mut u8,
    g: gdi::Gp,
    cache: Cache,
    st: SharedState,
    tray: Arc<Mutex<Option<tray::Tray>>>,
    agenda: Arc<Mutex<crate::events::AgendaMap>>,
    shown: bool,
    page: Page,
    /// 面板贴住工作区右缘/下缘：该侧圆角改画直角
    flush_right: bool,
    flush_bottom: bool,
    view_y: i32,
    view_m: u32,
    selected: NaiveDate,
    regions: Vec<(gdi::RectF, Action)>,
    hover: Option<Action>,
    draft: String,
    comp: String,
    caret_on: bool,
    /// 快捷输入光标（char 下标）与选区锚点
    draft_caret: usize,
    draft_sel: Option<usize>,
    /// 快捷输入结果提示（解析失败/成功反馈），到期自动消失
    hint: String,
    hint_until: Option<std::time::Instant>,
    hint_warn: bool,
    /// 日历格悬停预览：当前悬停格 / 进入时点 / 已显示的预览格
    hover_cell: Option<NaiveDate>,
    hover_since: Option<std::time::Instant>,
    preview: Option<NaiveDate>,
    tick: u32,
    last_second: u32,
    idle_timer: bool,
    /// 日程页滚动偏移（超出可视行数时滚轮翻动）
    agenda_scroll: usize,
    /// 日程页当前解析出的行（含按天重复展开）：(原key, 原下标, 条目)
    agenda_resolved: Vec<(String, usize, crate::events::AgendaEntry)>,
    /// 日程页各分组的待办（与日程合并展示）：(全局待办下标, 待办, 分组日期)
    agenda_todos: Vec<(usize, crate::sidebar::Todo, NaiveDate)>,
    /// 与 agenda_resolved 对齐的分组日期（重复日程的编辑/删除定位需要出现日）
    agenda_resolved_dates: Vec<NaiveDate>,
    /// 底部快捷输入类型：0=日程 1=待办
    draft_kind: u8,
    /// 快捷输入类型下拉展开中
    kind_menu_open: bool,
    dumped: bool,
    dump_path: String,
}


// ================= 独立设置窗口（可全屏拖动） =================
static SETTINGS_HWND: AtomicUsize = AtomicUsize::new(0);
static SETTINGS_POS: Mutex<Option<(i32, i32)>> = Mutex::new(None);

#[derive(Clone, Copy, PartialEq, Debug)]
enum SAction {
    Tab(usize),
    Toggle(u8),
    WeekDropdown,
    WeekPick(usize),
    MottoDropdown,
    MottoPick(usize),
    /// 界面字号下拉（软件设置页）
    FontScaleDropdown,
    FontScalePick(usize),
    /// 主题下拉（跟随系统/深色/浅色）
    ThemeDropdown,
    ThemePick(usize),
    /// 侧栏管理卡片行（按下开始拖动，原地松开=切换开关）；参数为真实顺序槽位
    SbRow(usize),
    Refresh,
    Confirm,
    Close,
}

/// 侧栏卡片拖动状态
#[derive(Clone, Copy)]
struct SbDrag {
    from: usize,
    sy: f32,
    y: f32,
    moved: bool,
}

/// 侧栏管理卡片行顶边（py=10 固定；motto 行下多一个“格言类型”行）
fn sb_slot_tops(motto_slot: usize) -> [f32; 7] {
    let mut tops = [0.0f32; 7];
    for (i, t) in tops.iter_mut().enumerate() {
        *t = 124.0 + 42.0 * i as f32 + if i > motto_slot { 42.0 } else { 0.0 };
    }
    tops
}

/// 指针 y → 卡片槽位
fn sb_slot_at(tops: &[f32; 7], y: f32) -> usize {
    for (i, t) in tops.iter().enumerate() {
        if y >= *t && y < *t + 42.0 {
            return i;
        }
    }
    if y < tops[0] {
        0
    } else {
        6
    }
}

struct SettingsUi {
    hwnd: usize,
    sf: f32,
    w: f32,
    h: f32,
    mem_dc: usize,
    hbmp: usize,
    bmp: gdi::Gp,
    scan0: *mut u8,
    g: gdi::Gp,
    cache: Cache,
    st: SharedState,
    tray: Arc<Mutex<Option<tray::Tray>>>,
    tab: usize,
    regions: Vec<(gdi::RectF, SAction)>,
    hover: Option<SAction>,
    week_menu_open: bool,
    /// 界面字号下拉展开中
    font_menu_open: bool,
    /// 主题下拉展开中
    theme_menu_open: bool,
    motto_menu_open: bool,
    /// 侧栏管理拖动排序状态
    sb_drag: Option<SbDrag>,
    refreshing: bool,
    ics_status: Option<bool>,
    dumped: bool,
    dump_path: String,
}

static SETTINGS_UI: Mutex<Option<SendSettings>> = Mutex::new(None);

struct SendSettings(Box<SettingsUi>);
unsafe impl Send for SendSettings {}

pub fn create_settings_window(st: SharedState, tray: Arc<Mutex<Option<tray::Tray>>>) {
    unsafe {
        let cls = crate::wide("z-calendar-settings");
        let hinstance = winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null());
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(settings_wndproc);
        wc.hInstance = hinstance;
        wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        wc.lpszClassName = cls.as_ptr();
        RegisterClassW(&wc);

        let w = gdi::phys(SETTINGS_W) as i32;
        let h = gdi::phys(SETTINGS_H) as i32;
        let title = crate::wide("Z日历 · 设置");
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
        SETTINGS_HWND.store(hwnd as usize, Ordering::Relaxed);

        let mut sui = Box::new(SettingsUi {
            hwnd: hwnd as usize,
            sf: gdi::scale(),
            w: SETTINGS_W,
            h: SETTINGS_H,
            mem_dc: 0,
            hbmp: 0,
            bmp: std::ptr::null_mut(),
            scan0: std::ptr::null_mut(),
            g: std::ptr::null_mut(),
            cache: Cache::new(),
            st,
            tray,
            tab: std::env::var("CAL_TAB").ok().and_then(|v| v.parse().ok()).unwrap_or(0),
            regions: Vec::new(),
            hover: None,
            week_menu_open: false,
            font_menu_open: false,
            theme_menu_open: false,
            motto_menu_open: false,
            sb_drag: None,
            refreshing: false,
            ics_status: None,
            dumped: false,
            dump_path: std::env::var("CAL_DUMP2").unwrap_or_default(),
        });
        // 后台位图不在创建时分配：show_settings→redraw 惰性分配，隐藏即释放
        SETTINGS_UI.lock().unwrap().replace(SendSettings(sui));
    }
}

pub const SETTINGS_W: f32 = 490.0;
pub const SETTINGS_H: f32 = 600.0;

fn settings_position() -> (i32, i32) {
    if let Some((x, y)) = *SETTINGS_POS.lock().unwrap() {
        return (x, y);
    }
    // 首次：出现在工作区中央
    unsafe {
        let mut wa: RECT = std::mem::zeroed();
        SystemParametersInfoW(0x0030 /*SPI_GETWORKAREA*/, 0, &mut wa as *mut RECT as *mut c_void_ty2, 0);
        let x = wa.left + ((wa.right - wa.left) - gdi::phys(SETTINGS_W) as i32) / 2;
        let y = wa.top + ((wa.bottom - wa.top) - gdi::phys(SETTINGS_H) as i32) / 2;
        (x, y)
    }
}

type c_void_ty2 = winapi::ctypes::c_void;

#[link(name = "user32")]
extern "system" {
    fn SystemParametersInfoW(action: u32, param: u32, data: *mut c_void_ty2, init: u32) -> i32;
}

/// 打开系统自带“日期与时间”设置
pub fn open_date_time_settings() {
    unsafe {
        #[link(name = "shell32")]
        extern "system" {
            fn ShellExecuteW(hwnd: HWND, op: *const u16, file: *const u16, params: *const u16, dir: *const u16, show: i32) -> isize;
        }
        let op = crate::wide("open");
        let target = crate::wide("ms-settings:dateandtime");
        ShellExecuteW(std::ptr::null_mut(), op.as_ptr(), target.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut(), 1);
    }
}

/// 以指定页签打开设置窗口（日期侧边栏“卡片管理”入口）
pub fn show_settings_tab(tab: usize) {
    {
        let mut guard = SETTINGS_UI.lock().unwrap();
        if let Some(sui) = guard.as_mut() {
            sui.0.tab = tab;
            sui.0.week_menu_open = false;
            sui.0.font_menu_open = false;
            sui.0.theme_menu_open = false;
            sui.0.motto_menu_open = false;
        }
    }
    show_settings();
}

pub fn show_settings() {
    let hwnd = SETTINGS_HWND.load(Ordering::Relaxed);
    if hwnd == 0 {
        return;
    }
    let (x, y) = settings_position();
    unsafe {
        // WS_EX_NOACTIVATE + SW_SHOWNA：设置窗口从不抢焦点，
        // 日历面板保持激活与显示，二者共存
        SetWindowPos(hwnd as HWND, HWND_TOPMOST, x, y, gdi::phys(SETTINGS_W) as i32, gdi::phys(SETTINGS_H) as i32, SWP_NOACTIVATE);
        ShowWindow(hwnd as HWND, SW_SHOWNA);
    }
    {
        let mut guard = SETTINGS_UI.lock().unwrap();
        if let Some(sui) = guard.as_mut() {
            sui.0.week_menu_open = false; // 重开时收起上次遗留的下拉
            sui.0.font_menu_open = false;
            sui.0.theme_menu_open = false;
            sui.0.motto_menu_open = false;
            sui.0.redraw();
        }
    }
}

fn settings_visible() -> bool {
    let h = SETTINGS_HWND.load(Ordering::Relaxed);
    h != 0 && unsafe { IsWindowVisible(h as HWND) != 0 }
}

/// 节假日数据更新完成（后台线程调用）：通知设置窗口显示结果
pub fn post_ics_result(ok: bool) {
    let h = SETTINGS_HWND.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            PostMessageW(h as HWND, WM_APP_ICS_DONE, ok as usize, 0);
        }
    }
}

fn hide_settings() {
    let hwnd = SETTINGS_HWND.load(Ordering::Relaxed);
    if hwnd == 0 {
        return;
    }
    // 注意：本函数绝不能拿 SETTINGS_UI 锁——WM_LBUTTONDOWN 的确定/✕ 路径
    // 持有该锁调用进来，重入加锁会同线程死锁；位图释放由调用方在锁外先做
    unsafe {
        if IsWindowVisible(hwnd as HWND) != 0 {
            let mut r: RECT = std::mem::zeroed();
            GetWindowRect(hwnd as HWND, &mut r);
            *SETTINGS_POS.lock().unwrap() = Some((r.left, r.top));
            ShowWindow(hwnd as HWND, SW_HIDE);
        }
        // 关闭设置后把前台还给仍显示的面板：其"失焦自动隐藏"恢复正常节奏
        //（设置打开期间面板失焦不隐藏，可能已不在前台）
        let fh = FLYOUT_HWND.load(Ordering::Relaxed);
        if fh != 0 && IsWindowVisible(fh as HWND) != 0 {
            SetForegroundWindow(fh as HWND);
        }
        crate::trim_working_set();
    }
}

/// 释放设置窗口后台位图（隐藏时压缩内存；下次 show_settings 重绘时重建）。
/// 必须在 SETTINGS_UI 锁外调用。
fn free_settings_surface() {
    let mut guard = SETTINGS_UI.lock().unwrap();
    if let Some(sui) = guard.as_mut() {
        let ui = &mut sui.0;
        unsafe {
            gdi::free_dib(&mut ui.mem_dc, &mut ui.hbmp, &mut ui.bmp, &mut ui.g, &mut ui.scan0);
        }
    }
}

impl SettingsUi {
    fn hit_add(regions: &mut Vec<(gdi::RectF, SAction)>, x: f32, y: f32, w: f32, h: f32, a: SAction) {
        regions.push((gdi::RectF { x, y, w, h }, a));
    }

    fn hovered(&self, a: &SAction) -> bool {
        self.hover.map(|h| h == *a).unwrap_or(false)
    }

    fn redraw(&mut self) {
        if self.bmp.is_null() {
            // 隐藏时位图已释放压缩内存：显示前重建
            let (mem_dc, hbmp, bmp, scan0) = unsafe { gdi::alloc_dib(self.w, self.h) };
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
        let p = unsafe {
            Painter {
                g,
                cache: cache_ptr,
                sf: self.sf,
                w: self.w,
                h: self.h,
                dc: self.mem_dc,
                scan0: self.scan0,
            }
        };
        let mut regions: Vec<(gdi::RectF, SAction)> = Vec::new();
        p.clear();
        self.paint(&p, &mut regions);
        self.regions = regions;
        self.ulw();

        if !self.dumped && !self.dump_path.is_empty() {
            self.dumped = true;
            save_bmp(self.scan0, (self.w * self.sf) as i32, (self.h * self.sf) as i32, &self.dump_path);
            if std::env::var("CAL_DUMP_EXIT").map(|v| v == "1").unwrap_or(false) {
                unsafe {
                    PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0);
                }
            }
        }
    }

    fn ulw(&self) {
        unsafe {
            let mut r: RECT = std::mem::zeroed();
            GetWindowRect(self.hwnd as HWND, &mut r);
            let mut ppt = POINT { x: r.left, y: r.top };
            let mut size = SIZE { cx: (self.w * self.sf) as i32, cy: (self.h * self.sf) as i32 };
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

    fn paint(&self, p: &Painter, regions: &mut Vec<(gdi::RectF, SAction)>) {
        let m = 10.0;
        let pw = SETTINGS_W - 20.0;
        let ph = SETTINGS_H - 20.0;
        let px = m;
        let py = m;
        // 阴影 + 面板
        for i in 1..=8 {
            let a = (3 + i * 2) as u8;
            p.stroke_round(
                px - i as f32,
                py - i as f32,
                pw + i as f32 * 2.0,
                ph + i as f32 * 2.0,
                12.0 + i as f32,
                1.5,
                gdi::argb(a / 2, 0, 0, 0),
            );
        }
        p.fill_round(px, py, pw, ph, 12.0, POPUP_BG());
        p.stroke_round(px, py, pw, ph, 12.0, 1.0, BORDER());

        // 标题栏（原生拖动区域）
        p.text("设置", px + 16.0, py + 12.0, 100.0, 26.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 15.0, true, false, TITLE_COL());
        // 关闭
        let cx_btn = px + pw - 34.0;
        Self::hit_add(regions, cx_btn, py + 12.0, 24.0, 24.0, SAction::Close);
        let hov = self.hovered(&SAction::Close);
        p.text("✕", cx_btn, py + 12.0, 24.0, 24.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, if hov { RED() } else { WEEK_NUM() });

        // 左侧页签
        let tabs: [(usize, &str, &str); 3] = [
            (0, "\u{E713}", "软件设置"),
            (1, "\u{E787}", "日历设置"),
            (2, "\u{E81D}", "侧栏管理"),
        ];
        for (i, (tab_id, glyph, name)) in tabs.iter().enumerate() {
            let ty = py + 56.0 + 50.0 * i as f32;
            let sel = self.tab == *tab_id;
            let act = SAction::Tab(*tab_id);
            Self::hit_add(regions, px + 12.0, ty, 120.0, 42.0, act);
            let hov = self.hovered(&act);
            if sel {
                p.fill_round(px + 12.0, ty, 120.0, 42.0, 8.0, gdi::argb(40, 62, 135, 250));
                p.fill_rect(px + 12.0, ty + 8.0, 3.0, 26.0, BLUE());
            } else if hov {
                p.fill_round(px + 12.0, ty, 120.0, 42.0, 8.0, crate::theme::ov(14));
            }
            p.text(glyph, px + 22.0, ty, 22.0, 42.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 14.0, false, true, if sel { BLUE() } else { ICON_COL() });
            p.text(name, px + 50.0, ty, 80.0, 42.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, if sel { ON_BG() } else { ROW_TXT() });
        }
        p.fill_rect(px + 136.0, py + 50.0, 1.0, ph - 60.0, DIVIDER());

        let cx = px + 148.0;
        let cw = px + pw - 16.0 - cx;

        let cfg = self.st.config.lock().unwrap().clone();
        let last_ics = cfg.last_ics_update;

        if self.tab == 0 {
            let rows: [(u8, &str, bool); 4] = [
                (0, "开机自启", cfg.autostart),
                (1, "自动更新假期数据（每天检查）", cfg.auto_update),
                (8, "显示系统托盘图标", cfg.show_tray),
                (17, "提醒走系统通知中心（失败自动回退）", cfg.use_system_toast),
            ];
            let mut y = py + 54.0;
            for (idx, name, on) in rows {
                Self::hit_add(regions, cx, y, cw, 40.0, SAction::Toggle(idx));
                p.text(name, cx + 2.0, y, cw - 60.0, 40.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, ROW_TXT());
                let sw_x = cx + cw - 46.0;
                p.fill_round(sw_x, y + 10.0, 36.0, 20.0, 10.0, if on { BLUE() } else { crate::theme::ov(36) });
                let kx = if on { sw_x + 26.0 } else { sw_x + 10.0 };
                p.fill_circle(kx, y + 20.0, 7.0, WHITE);
                y += 46.0;
            }
            // 主题（跟随系统 / 深色 / 浅色）
            Self::hit_add(regions, cx, y, cw, 40.0, SAction::ThemeDropdown);
            p.text("主题", cx + 2.0, y, cw - 120.0, 40.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, ROW_TXT());
            let theme_names = ["跟随系统", "深色主题", "浅色主题"];
            let cur_t = (cfg.theme as usize).min(2);
            let tpill_w = 104.0;
            let tpill_x = cx + cw - tpill_w - 4.0;
            let tpill_top = y + 5.0;
            let thov = self.hovered(&SAction::ThemeDropdown);
            p.fill_round(tpill_x, tpill_top, tpill_w, 30.0, 8.0, if thov { gdi::argb(50, 62, 135, 250) } else { gdi::argb(28, 62, 135, 250) });
            p.stroke_round(tpill_x, tpill_top, tpill_w, 30.0, 8.0, 1.0, gdi::argb(110, 62, 135, 250));
            p.text(&format!("{} ▾", theme_names[cur_t]), tpill_x, tpill_top, tpill_w, 30.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, BLUE());
            let tpill = (tpill_x, tpill_top, tpill_w);
            y += 52.0;
            // 界面字号（100%/110%/125%，全局生效）
            Self::hit_add(regions, cx, y, cw, 40.0, SAction::FontScaleDropdown);
            p.text("界面字号", cx + 2.0, y, cw - 120.0, 40.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, ROW_TXT());
            let scale_vals = [1.0f32, 1.1, 1.25];
            let scale_names = ["100%", "110%", "125%"];
            let cur_i = scale_vals.iter().position(|v| (cfg.ui_font_scale - v).abs() < 0.01).unwrap_or(0);
            let fpill_w = 96.0;
            let fpill_x = cx + cw - fpill_w - 4.0;
            let fpill_top = y + 5.0;
            let fhov = self.hovered(&SAction::FontScaleDropdown);
            p.fill_round(fpill_x, fpill_top, fpill_w, 30.0, 8.0, if fhov { gdi::argb(50, 62, 135, 250) } else { gdi::argb(28, 62, 135, 250) });
            p.stroke_round(fpill_x, fpill_top, fpill_w, 30.0, 8.0, 1.0, gdi::argb(110, 62, 135, 250));
            p.text(&format!("{} ▾", scale_names[cur_i]), fpill_x, fpill_top, fpill_w, 30.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, BLUE());
            let fpill = (fpill_x, fpill_top, fpill_w);
            y += 52.0;
            // 下拉展开：选项列表（下方放不下则向上弹出）
            if self.font_menu_open {
                let (fpill_x, fpill_top, fpill_w) = fpill;
                let opt_h = 26.0;
                let list_h = opt_h * scale_names.len() as f32 + 8.0;
                let list_top = if fpill_top + 30.0 + list_h > py + ph - 50.0 {
                    fpill_top - list_h
                } else {
                    fpill_top + 32.0
                };
                p.fill_round(fpill_x - 1.0, list_top, fpill_w + 2.0, list_h, 8.0, POPUP_BG());
                p.stroke_round(fpill_x - 1.0, list_top, fpill_w + 2.0, list_h, 8.0, 1.0, BORDER());
                for (i, name) in scale_names.iter().enumerate() {
                    let oy = list_top + 4.0 + opt_h * i as f32;
                    Self::hit_add(regions, fpill_x + 1.0, oy, fpill_w - 2.0, opt_h, SAction::FontScalePick(i));
                    let h = self.hovered(&SAction::FontScalePick(i));
                    if h {
                        p.fill_round(fpill_x + 2.0, oy, fpill_w - 4.0, opt_h - 1.0, 6.0, gdi::argb(50, 62, 135, 250));
                    }
                    let sel = i == cur_i;
                    p.text(name, fpill_x + 1.0, oy, fpill_w - 2.0, opt_h, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, if sel { BLUE() } else { ROW_TXT() });
                }
            }
            // 主题下拉展开：最后绘制保证覆盖在界面字号行之上（命中也优先）
            if self.theme_menu_open {
                let (tpill_x, tpill_top, tpill_w) = tpill;
                let opt_h = 26.0;
                let list_h = opt_h * theme_names.len() as f32 + 8.0;
                let list_top = if tpill_top + 30.0 + list_h > py + ph - 50.0 {
                    tpill_top - list_h
                } else {
                    tpill_top + 32.0
                };
                p.fill_round(tpill_x - 1.0, list_top, tpill_w + 2.0, list_h, 8.0, POPUP_BG());
                p.stroke_round(tpill_x - 1.0, list_top, tpill_w + 2.0, list_h, 8.0, 1.0, BORDER());
                for (i, name) in theme_names.iter().enumerate() {
                    let oy = list_top + 4.0 + opt_h * i as f32;
                    Self::hit_add(regions, tpill_x + 1.0, oy, tpill_w - 2.0, opt_h, SAction::ThemePick(i));
                    let h = self.hovered(&SAction::ThemePick(i));
                    if h {
                        p.fill_round(tpill_x + 2.0, oy, tpill_w - 4.0, opt_h - 1.0, 6.0, gdi::argb(50, 62, 135, 250));
                    }
                    let sel = i == cur_t;
                    p.text(name, tpill_x + 1.0, oy, tpill_w - 2.0, opt_h, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, if sel { BLUE() } else { ROW_TXT() });
                }
            }
            y += 8.0;
            let btn_w = if self.refreshing { 190.0 } else { 160.0 };
            Self::hit_add(regions, cx, y, btn_w, 32.0, SAction::Refresh);
            let hov = self.hovered(&SAction::Refresh) && !self.refreshing;
            p.fill_round(cx, y, btn_w, 32.0, 8.0, gdi::argb(38, 62, 135, 250));
            p.stroke_round(cx, y, btn_w, 32.0, 8.0, 1.0, gdi::argb(110, 62, 135, 250));
            let label = if self.refreshing { "正在更新节假日数据…" } else { "立即更新节假日数据" };
            p.text(label, cx, y, btn_w, 32.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, BLUE());
            y += 44.0;
            let upd = if last_ics > 0 {
                chrono::DateTime::from_timestamp((last_ics / 1000) as i64, 0)
                    .map(|dt| dt.with_timezone(&chrono::Local).format("%Y/%m/%d %H:%M").to_string())
                    .unwrap_or_default()
            } else {
                "尚未更新".to_string()
            };
            p.text(&format!("上次更新：{}", upd), cx, y, cw, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 11.0, false, false, SUB_DIM());
            y += 22.0;
            // 更新结果提示（成功绿 / 失败红）
            if let Some(ok) = self.ics_status {
                let (msg, col) = if ok {
                    ("✓ 节假日数据更新成功", gdi::argb(255, 0x5B, 0xC2, 0x8E))
                } else {
                    ("✕ 更新失败，请检查网络后重试", RED())
                };
                p.text(msg, cx, y, cw, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.0, false, false, col);
                y += 20.0;
            }
            for line in [
                "节假日数据来源：chinese-days（cdn.jsdelivr.net），",
                "包含法定节假日与调休补班，自动获取最新年份。",
            ] {
                p.text(line, cx, y, cw, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 11.0, false, false, SUB_DIM());
                y += 20.0;
            }
        } else if self.tab == 1 {
            let rows: [(u8, &str, bool); 7] = [
                (2, "显示农历/节日信息", cfg.show_lunar),
                (3, "显示调休安排", cfg.show_adjust),
                (4, "显示非当前月日期", cfg.show_other_month),
                (5, "使用12小时制", cfg.hour12),
                (6, "显示周数", cfg.show_week_num),
                (7, "显示天气预报", cfg.show_weather),
                (9, "提醒提示音", crate::config::remind_sound_on()),
            ];
            let mut y = py + 54.0;
            for (idx, name, on) in rows {
                Self::hit_add(regions, cx, y, cw, 40.0, SAction::Toggle(idx));
                p.text(name, cx + 2.0, y, cw - 60.0, 40.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, ROW_TXT());
                let sw_x = cx + cw - 46.0;
                p.fill_round(sw_x, y + 10.0, 36.0, 20.0, 10.0, if on { BLUE() } else { crate::theme::ov(36) });
                let kx = if on { sw_x + 26.0 } else { sw_x + 10.0 };
                p.fill_circle(kx, y + 20.0, 7.0, WHITE);
                y += 46.0;
            }
            Self::hit_add(regions, cx, y, cw, 40.0, SAction::WeekDropdown);
            p.text("一周开始", cx + 2.0, y, cw - 120.0, 40.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, ROW_TXT());
            let names = ["星期一", "星期二", "星期三", "星期四", "星期五", "星期六", "星期日"];
            let vn = names[(cfg.week_start as usize) % 7];
            let pill_w = 96.0;
            let pill_x = cx + cw - pill_w - 4.0;
            let pill_top = y + 5.0;
            let hov = self.hovered(&SAction::WeekDropdown);
            p.fill_round(pill_x, pill_top, pill_w, 30.0, 8.0, if hov { gdi::argb(50, 62, 135, 250) } else { gdi::argb(28, 62, 135, 250) });
            p.stroke_round(pill_x, pill_top, pill_w, 30.0, 8.0, 1.0, gdi::argb(110, 62, 135, 250));
            p.text(&format!("{} ▾", vn), pill_x, pill_top, pill_w, 30.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, BLUE());
            y += 52.0;
            p.text("更改设置后立即生效，无需保存。", cx, y, cw, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 11.0, false, false, SUB_DIM());
            // 下拉展开：选项列表（后绘制=命中优先），下方放不下则向上弹出
            if self.week_menu_open {
                let opt_h = 26.0;
                let list_h = opt_h * names.len() as f32 + 8.0;
                let list_top = if pill_top + 30.0 + list_h > py + ph - 50.0 {
                    pill_top - list_h
                } else {
                    pill_top + 32.0
                };
                p.fill_round(pill_x - 1.0, list_top, pill_w + 2.0, list_h, 8.0, POPUP_BG());
                p.stroke_round(pill_x - 1.0, list_top, pill_w + 2.0, list_h, 8.0, 1.0, BORDER());
                for (i, name) in names.iter().enumerate() {
                    let oy = list_top + 4.0 + opt_h * i as f32;
                    Self::hit_add(regions, pill_x + 1.0, oy, pill_w - 2.0, opt_h, SAction::WeekPick(i));
                    let h = self.hovered(&SAction::WeekPick(i));
                    if h {
                        p.fill_round(pill_x + 2.0, oy, pill_w - 4.0, opt_h - 1.0, 6.0, gdi::argb(50, 62, 135, 250));
                    }
                    let sel = (cfg.week_start as usize) % 7 == i;
                    p.text(name, pill_x + 1.0, oy, pill_w - 2.0, opt_h, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, if sel { BLUE() } else { ROW_TXT() });
                }
            }
        } else {
            // 侧栏管理：卡片开关 + 拖动排序（顺序即侧栏卡片显示顺序）
            p.fill_round(cx, py + 50.0, cw, 52.0, 8.0, POPUP_BG());
            p.text("侧栏卡片", cx + 14.0, py + 56.0, cw - 28.0, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, true, false, TITLE_COL());
            p.text("开关控制显示；拖动卡片行调整侧栏顺序", cx + 14.0, py + 76.0, cw - 28.0, 15.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 10.5, false, false, SUB_DIM());
            let order = cfg.sidebar_card_order();
            // 拖动预览：把拖动中的卡片移到当前指针槽位
            let (drag_from, drag_y, drag_moved) = self.sb_drag.as_ref().map(|d| (d.from, d.y, d.moved)).unwrap_or((0usize, 0.0f32, false));
            let mut display: Vec<&'static str> = order.clone();
            if drag_moved {
                let tops0 = sb_slot_tops(display.iter().position(|c| *c == "motto").unwrap_or(6));
                let target = sb_slot_at(&tops0, drag_y);
                let item = display.remove(drag_from.min(display.len() - 1));
                display.insert(target.min(display.len()), item);
            }
            let drag_id = order.get(drag_from).copied();
            let tops = sb_slot_tops(display.iter().position(|c| *c == "motto").unwrap_or(6));
            let motto_names = ["名人名言", "文学", "互联网", "科普"];
            let motto_idx = crate::motto::TYPES
                .iter()
                .position(|(c, _)| c.to_string() == cfg.motto_type)
                .unwrap_or(0);
            let mut motto_pill = (0.0f32, 0.0f32);
            for (slot, id) in display.iter().enumerate() {
                let id: &str = id;
                let (_, name, tidx) = crate::config::SIDEBAR_CARDS.iter().find(|(c, _, _)| *c == id).copied().unwrap();
                let on = cfg.sidebar_enabled(id);
                let y = tops[slot];
                let dragging = drag_moved && Some(id) == drag_id;
                // 整行拖动热区（开关区域后绘制，命中优先）；参数为真实顺序槽位
                Self::hit_add(regions, cx, y, cw, 38.0, SAction::SbRow(order.iter().position(|c| *c == id).unwrap()));
                p.fill_round(cx, y, cw, 38.0, 8.0, gdi::argb(if dragging { 34 } else { 14 }, 255, 255, 255));
                if dragging {
                    p.stroke_round(cx + 0.5, y + 0.5, cw - 1.0, 37.0, 8.0, 1.5, BLUE());
                }
                // 拖动把手（6 点）
                for (dx, dy) in [(0.0f32, 0.0f32), (5.0, 0.0), (0.0, 4.0), (5.0, 4.0), (0.0, 8.0), (5.0, 8.0)] {
                    p.fill_circle(cx + 14.0 + dx, y + 15.0 + dy, 1.3, crate::theme::ov(130));
                }
                p.text(name, cx + 28.0, y, cw - 98.0, 38.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, false, false, ROW_TXT());
                let sw_x = cx + cw - 50.0;
                Self::hit_add(regions, sw_x - 4.0, y, 44.0, 38.0, SAction::Toggle(tidx));
                p.fill_round(sw_x, y + 9.0, 36.0, 20.0, 10.0, if on { BLUE() } else { crate::theme::ov(36) });
                let kx = if on { sw_x + 26.0 } else { sw_x + 10.0 };
                p.fill_circle(kx, y + 19.0, 7.0, WHITE);
                if id == "motto" {
                    // 时间格言：其下插入格言类型（一言 API 分类）
                    Self::hit_add(regions, cx, y + 42.0, cw, 38.0, SAction::MottoDropdown);
                    p.fill_round(cx, y + 42.0, cw, 38.0, 8.0, crate::theme::ov(14));
                    p.text("格言类型", cx + 14.0, y + 42.0, cw - 110.0, 38.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, false, false, ROW_TXT());
                    let pill_w = 96.0;
                    let pill_x = cx + cw - pill_w - 8.0;
                    let pill_top = y + 42.0 + 4.0;
                    let hov = self.hovered(&SAction::MottoDropdown);
                    p.fill_round(pill_x, pill_top, pill_w, 30.0, 8.0, if hov { gdi::argb(50, 62, 135, 250) } else { gdi::argb(28, 62, 135, 250) });
                    p.stroke_round(pill_x, pill_top, pill_w, 30.0, 8.0, 1.0, gdi::argb(110, 62, 135, 250));
                    p.text(&format!("{} ▾", motto_names[motto_idx]), pill_x, pill_top, pill_w, 30.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, BLUE());
                    motto_pill = (pill_x, pill_top);
                }
            }
            // 底部不再重复提示：页首“开关控制显示；拖动卡片行调整侧栏顺序”已说明；
            // 且“时间格言”拖到最后一格时格言类型行延伸到底部，此处放文本会与之重叠
            // 下拉展开：选项列表（下方放不下则向上弹出）
            if self.motto_menu_open {
                let (pill_x, pill_top) = motto_pill;
                let pill_w = 96.0;
                let opt_h = 26.0;
                let list_h = opt_h * motto_names.len() as f32 + 8.0;
                let list_top = if pill_top + 30.0 + list_h > py + ph - 50.0 {
                    pill_top - list_h
                } else {
                    pill_top + 32.0
                };
                p.fill_round(pill_x - 1.0, list_top, pill_w + 2.0, list_h, 8.0, POPUP_BG());
                p.stroke_round(pill_x - 1.0, list_top, pill_w + 2.0, list_h, 8.0, 1.0, BORDER());
                for (i, name) in motto_names.iter().enumerate() {
                    let oy = list_top + 4.0 + opt_h * i as f32;
                    Self::hit_add(regions, pill_x + 1.0, oy, pill_w - 2.0, opt_h, SAction::MottoPick(i));
                    let h = self.hovered(&SAction::MottoPick(i));
                    if h {
                        p.fill_round(pill_x + 2.0, oy, pill_w - 4.0, opt_h - 1.0, 6.0, gdi::argb(50, 62, 135, 250));
                    }
                    let sel = i == motto_idx;
                    p.text(name, pill_x + 1.0, oy, pill_w - 2.0, opt_h, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, if sel { BLUE() } else { ROW_TXT() });
                }
            }
        }

        // 确定按钮（右下角）
        let btn = gdi::RectF { x: px + pw - 106.0, y: py + ph - 44.0, w: 90.0, h: 30.0 };
        Self::hit_add(regions, btn.x, btn.y, btn.w, btn.h, SAction::Confirm);
        let hov = self.hovered(&SAction::Confirm);
        p.fill_round(btn.x, btn.y, btn.w, btn.h, 8.0, if hov { gdi::argb(255, 0x53, 0x99, 0xFB) } else { BLUE() });
        p.text("确定", btn.x, btn.y, btn.w, btn.h, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 13.0, false, false, WHITE);
    }

    fn handle_action(&mut self, action: &SAction) {
        match action {
            SAction::Tab(i) => {
                self.tab = *i;
                self.redraw();
            }
            SAction::Toggle(idx) => {
                let on = {
                    let mut cfg = self.st.config.lock().unwrap();
                    match idx {
                        0 => {
                            cfg.autostart = !cfg.autostart;
                            cfg.autostart
                        }
                        1 => {
                            cfg.auto_update = !cfg.auto_update;
                            cfg.auto_update
                        }
                        2 => {
                            cfg.show_lunar = !cfg.show_lunar;
                            cfg.show_lunar
                        }
                        3 => {
                            cfg.show_adjust = !cfg.show_adjust;
                            cfg.show_adjust
                        }
                        4 => {
                            cfg.show_other_month = !cfg.show_other_month;
                            cfg.show_other_month
                        }
                        5 => {
                            cfg.hour12 = !cfg.hour12;
                            crate::config::set_hour12(cfg.hour12);
                            cfg.hour12
                        }
                        6 => {
                            cfg.show_week_num = !cfg.show_week_num;
                            cfg.show_week_num
                        }
                        7 => {
                            cfg.show_weather = !cfg.show_weather;
                            cfg.show_weather
                        }
                        8 => {
                            cfg.show_tray = !cfg.show_tray;
                            cfg.show_tray
                        }
                        9 => {
                            // 提醒提示音（设置位走原子缓存，弹窗线程读取）
                            let v = !crate::config::remind_sound_on();
                            crate::config::set_remind_sound(v);
                            cfg.remind_sound = v;
                            v
                        }
                        17 => {
                            // 系统通知中心开关（原子缓存供提醒线程读取）
                            cfg.use_system_toast = !cfg.use_system_toast;
                            crate::config::set_use_system_toast(cfg.use_system_toast);
                            cfg.use_system_toast
                        }
                        10 => {
                            cfg.sidebar_date = !cfg.sidebar_date;
                            cfg.sidebar_date
                        }
                        11 => {
                            cfg.sidebar_almanac = !cfg.sidebar_almanac;
                            cfg.sidebar_almanac
                        }
                        12 => {
                            cfg.sidebar_events = !cfg.sidebar_events;
                            cfg.sidebar_events
                        }
                        13 => {
                            cfg.sidebar_agenda = !cfg.sidebar_agenda;
                            cfg.sidebar_agenda
                        }
                        14 => {
                            cfg.sidebar_history = !cfg.sidebar_history;
                            cfg.sidebar_history
                        }
                        15 => {
                            cfg.sidebar_motto = !cfg.sidebar_motto;
                            cfg.sidebar_motto
                        }
                        16 => {
                            cfg.sidebar_todo = !cfg.sidebar_todo;
                            cfg.sidebar_todo
                        }
                        _ => cfg.show_weather,
                    }
                };
                self.st.config.lock().unwrap().save();
                if (10..=16).contains(idx) {
                    crate::sidebar::sidebar_repaint();
                }
                if *idx == 0 {
                    apply_autostart(on);
                    if let Some(t) = self.tray.lock().unwrap().as_ref() {
                        t.autostart_item.set_checked(on);
                    }
                }
                if *idx == 7 {
                    let _ = self.st.weather_tx.send(());
                }
                if *idx == 8 {
                    if let Some(t) = self.tray.lock().unwrap().as_ref() {
                        t.set_visible(on);
                    }
                }
                if *idx == 17 && on {
                    // 开启时后台确保 AUMID 与开始菜单快捷方式就绪（不阻塞 UI）
                    std::thread::Builder::new()
                        .name("lnk".into())
                        .spawn(|| {
                            crate::wnotify::ensure_ready();
                        })
                        .ok();
                }
                self.redraw();
            }
            SAction::WeekDropdown => {
                self.week_menu_open = !self.week_menu_open;
                self.motto_menu_open = false;
                self.hover = None;
                self.redraw();
            }
            SAction::WeekPick(i) => {
                let mut cfg = self.st.config.lock().unwrap();
                cfg.week_start = *i as u32;
                cfg.save();
                drop(cfg);
                self.week_menu_open = false;
                self.hover = None;
                self.redraw();
            }
            SAction::FontScaleDropdown => {
                self.font_menu_open = !self.font_menu_open;
                self.week_menu_open = false;
                self.motto_menu_open = false;
                self.hover = None;
                self.redraw();
            }
            SAction::FontScalePick(_) => {
                // 字号切换需走 rescale_all（内部拿 SETTINGS_UI 锁），在 WM_LBUTTONDOWN
                // 锁外处理；此处仅兜底（正常路径到不了）
            }
            SAction::ThemeDropdown => {
                self.theme_menu_open = !self.theme_menu_open;
                self.font_menu_open = false;
                self.week_menu_open = false;
                self.motto_menu_open = false;
                self.hover = None;
                self.redraw();
            }
            SAction::ThemePick(i) => {
                let v = (*i).min(2) as u8;
                let mut cfg = self.st.config.lock().unwrap();
                cfg.theme = v;
                cfg.save();
                drop(cfg);
                crate::theme::set_mode(v);
                self.theme_menu_open = false;
                self.hover = None;
                self.redraw();
                // 其他窗口按新主题立即重绘
                flyout_repaint();
                crate::sidebar::sidebar_repaint();
            }
            SAction::MottoDropdown => {
                self.motto_menu_open = !self.motto_menu_open;
                self.week_menu_open = false;
                self.hover = None;
                self.redraw();
            }
            SAction::MottoPick(i) => {
                let letter = crate::motto::TYPES.get(*i).map(|(c, _)| c.to_string()).unwrap_or_else(|| "d".to_string());
                {
                    let mut cfg = self.st.config.lock().unwrap();
                    cfg.motto_type = letter;
                    cfg.save();
                }
                self.motto_menu_open = false;
                self.hover = None;
                // 分类变化后立即拉取新类型格言并刷新侧栏
                crate::sidebar::sidebar_repaint();
                self.redraw();
            }
            // 侧栏卡片行的按下/松开在 WM_LBUTTONDOWN/UP 处理
            SAction::SbRow(_) => {}
            SAction::Refresh => {
                if !self.refreshing {
                    self.refreshing = true;
                    self.ics_status = None;
                    self.redraw();
                    let _ = self.st.refresh_tx.send(());
                }
            }
            SAction::Confirm | SAction::Close => {
                hide_settings();
            }
        }
    }

    fn action_at(&self, x: f32, y: f32) -> Option<SAction> {
        for (r, a) in self.regions.iter().rev() {
            if x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h {
                return Some(*a);
            }
        }
        None
    }
}

unsafe extern "system" fn settings_wndproc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            ValidateRect(hwnd, std::ptr::null_mut());
            0
        }
        WM_ERASEBKGND => 1,
        // 点击设置窗口不改变激活状态：主面板保持激活与显示
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_NCHITTEST => {
            // 标题栏返回 HTCAPTION：可原生拖动到全屏任意位置
            let x = ((lp as usize) & 0xFFFF) as u16 as i16 as i32;
            let y = (((lp as usize) >> 16) as u16 as i16) as i32;
            let mut r: RECT = std::mem::zeroed();
            GetWindowRect(hwnd, &mut r);
            let cx = (x - r.left) as f32;
            let cy = (y - r.top) as f32;
            if cx >= gdi::phys(10.0) && cx <= (r.right - r.left) as f32 - gdi::phys(40.0) && cy >= gdi::phys(10.0) && cy <= gdi::phys(50.0) {
                2 // HTCAPTION
            } else {
                DefWindowProcW(hwnd, msg, wp, lp)
            }
        }
        WM_LBUTTONDOWN => {
            // 拖动开始前先在锁外处理鼠标捕获：SetCapture 可能同步派发 WM_CAPTURECHANGED
            // （上次拖动后捕获仍在本窗口时同样会触发），其处理需要 SETTINGS_UI 锁，
            // 绝不能在本锁持有期间调用（同线程重入 → 死锁）
            let sb_hit = {
                let guard = SETTINGS_UI.lock().unwrap();
                guard.as_ref().and_then(|s| {
                    let ui = &s.0;
                    let x = ((lp & 0xFFFF) as u16 as i16) as f32 / ui.sf;
                    let y = (((lp as usize) >> 16) as u16 as i16) as f32 / ui.sf;
                    match ui.action_at(x, y) {
                        Some(SAction::SbRow(slot)) => Some((slot, y)),
                        _ => None,
                    }
                })
            };
            if let Some((slot, y)) = sb_hit {
                unsafe {
                    if GetCapture() != hwnd {
                        SetCapture(hwnd);
                    }
                }
                let mut guard = SETTINGS_UI.lock().unwrap();
                if let Some(sui) = guard.as_mut() {
                    let ui = &mut sui.0;
                    ui.sb_drag = Some(SbDrag { from: slot, sy: y, y, moved: false });
                    ui.hover = None;
                    ui.redraw();
                }
                return 0;
            }
            let mut request_close = false;
            let mut font_pick: Option<usize> = None;
            {
                let mut guard = SETTINGS_UI.lock().unwrap();
                if let Some(sui) = guard.as_mut() {
                    let ui = &mut sui.0;
                    let x = ((lp & 0xFFFF) as u16 as i16) as f32 / ui.sf;
                    let y = (((lp as usize) >> 16) as u16 as i16) as f32 / ui.sf;
                    let a = ui.action_at(x, y);
                    if ui.week_menu_open {
                        // 下拉展开中：仅选项/控件本身响应，点其他位置只收起
                        match a {
                            Some(SAction::WeekPick(i)) => ui.handle_action(&SAction::WeekPick(i)),
                            _ => {
                                ui.week_menu_open = false;
                                ui.hover = None;
                                ui.redraw();
                            }
                        }
                    } else if ui.motto_menu_open {
                        match a {
                            Some(SAction::MottoPick(i)) => ui.handle_action(&SAction::MottoPick(i)),
                            _ => {
                                ui.motto_menu_open = false;
                                ui.hover = None;
                                ui.redraw();
                            }
                        }
                    } else if ui.font_menu_open {
                        // 下拉展开中：仅选项/控件本身响应，点其他位置只收起
                        match a {
                            Some(SAction::FontScalePick(i)) => ui.handle_action(&SAction::FontScalePick(i)),
                            _ => {
                                ui.font_menu_open = false;
                                ui.hover = None;
                                ui.redraw();
                            }
                        }
                    } else if ui.theme_menu_open {
                        // 下拉展开中：仅选项/控件本身响应，点其他位置只收起
                        match a {
                            Some(SAction::ThemePick(i)) => ui.handle_action(&SAction::ThemePick(i)),
                            _ => {
                                ui.theme_menu_open = false;
                                ui.hover = None;
                                ui.redraw();
                            }
                        }
                    } else if let Some(a) = a {
                        match a {
                            SAction::Toggle(_) | SAction::WeekDropdown | SAction::MottoDropdown | SAction::FontScaleDropdown | SAction::ThemeDropdown | SAction::Refresh | SAction::Tab(_) => {
                                ui.handle_action(&a);
                            }
                            // 字号切换要走 rescale_all（其内部需拿 SETTINGS_UI 锁），
                            // 延后到锁外执行；确认/关闭同理（hide_settings/free 不能持锁重入）
                            SAction::FontScalePick(i) => {
                                font_pick = Some(i);
                            }
                            SAction::Confirm | SAction::Close => {
                                request_close = true;
                            }
                            _ => {}
                        }
                    }
                }
            }
            if let Some(i) = font_pick {
                let vals = [1.0f32, 1.1, 1.25];
                let v = vals[i.min(vals.len() - 1)];
                {
                    let mut guard = SETTINGS_UI.lock().unwrap();
                    if let Some(sui) = guard.as_mut() {
                        let mut cfg = sui.0.st.config.lock().unwrap();
                        cfg.ui_font_scale = v;
                        cfg.save();
                    }
                }
                // 字号并入全局缩放链：切换等价一次 DPI 变化，全部窗口重摆重绘
                gdi::set_text_scale(v);
                rescale_all(gdi::scale());
            }
            if request_close {
                free_settings_surface();
                hide_settings();
            }
            0
        }
        WM_MOUSEMOVE => {
            let mut guard = SETTINGS_UI.lock().unwrap();
            if let Some(sui) = guard.as_mut() {
                let ui = &mut sui.0;
                let x = ((lp & 0xFFFF) as u16 as i16) as f32 / ui.sf;
                let y = (((lp as usize) >> 16) as u16 as i16) as f32 / ui.sf;
                if let Some(d) = ui.sb_drag.as_mut() {
                    // 拖动中：更新指针位置并实时预览
                    d.y = y;
                    if (y - d.sy).abs() > 3.0 && !d.moved {
                        d.moved = true;
                    }
                    ui.redraw();
                } else {
                    let a = ui.action_at(x, y);
                    if a != ui.hover {
                        ui.hover = a;
                        ui.redraw();
                        let clickable = matches!(a, Some(SAction::Tab(_)) | Some(SAction::Toggle(_)) | Some(SAction::SbRow(_)) | Some(SAction::WeekDropdown) | Some(SAction::WeekPick(_)) | Some(SAction::MottoDropdown) | Some(SAction::MottoPick(_)) | Some(SAction::FontScaleDropdown) | Some(SAction::FontScalePick(_)) | Some(SAction::ThemeDropdown) | Some(SAction::ThemePick(_)) | Some(SAction::Toggle(_)) | Some(SAction::Refresh) | Some(SAction::Confirm) | Some(SAction::Close));
                        SetCursor(if clickable { LoadCursorW(std::ptr::null_mut(), IDC_HAND) } else { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) });
                    }
                    // 注册离开跟踪：移出窗口后清悬停高亮（防残留）
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    TrackMouseEvent(&mut tme);
                }
            }
            0
        }
        WM_MOUSELEAVE => {
            let mut guard = SETTINGS_UI.lock().unwrap();
            if let Some(sui) = guard.as_mut() {
                let ui = &mut sui.0;
                if ui.hover.is_some() {
                    ui.hover = None;
                    ui.redraw();
                }
            }
            0
        }
        WM_LBUTTONUP => {
            // 拖动提交 / 原地点击切换（后者延后到锁外执行，handle_action 会操作托盘等）
            let mut click_toggle: Option<u8> = None;
            {
                let mut guard = SETTINGS_UI.lock().unwrap();
                if let Some(sui) = guard.as_mut() {
                    let ui = &mut sui.0;
                    if let Some(d) = ui.sb_drag.take() {
                        if d.moved {
                            // 拖动提交：按松开位置重排并保存
                            let mut order = ui.st.config.lock().unwrap().sidebar_card_order();
                            let motto_slot = order.iter().position(|c| *c == "motto").unwrap_or(6);
                            let tops = sb_slot_tops(motto_slot);
                            let item = order.remove(d.from.min(order.len() - 1));
                            let target = sb_slot_at(&tops, d.y).min(order.len());
                            order.insert(target, item);
                            {
                                let mut cfg = ui.st.config.lock().unwrap();
                                cfg.set_sidebar_order(&order);
                                cfg.save();
                            }
                            crate::sidebar::sidebar_repaint();
                            ui.redraw();
                        } else {
                            let order = ui.st.config.lock().unwrap().sidebar_card_order();
                            click_toggle = order
                                .get(d.from)
                                .and_then(|id| crate::config::SIDEBAR_CARDS.iter().find(|(c, _, _)| c == id).copied())
                                .map(|(_, _, t)| t);
                        }
                    }
                }
            }
            if let Some(tidx) = click_toggle {
                let mut guard = SETTINGS_UI.lock().unwrap();
                if let Some(sui) = guard.as_mut() {
                    sui.0.handle_action(&SAction::Toggle(tidx));
                }
            }
            // 拖动结束：显式释放鼠标捕获（系统不会在按键释放时自动释放，残留捕获
            // 会让下次 SetCapture 同步派发 WM_CAPTURECHANGED，并吞掉窗口外的点击）。
            // 此时 SETTINGS_UI 锁已释放，WM_CAPTURECHANGED 同步重入加锁是安全的。
            unsafe {
                if GetCapture() == hwnd {
                    ReleaseCapture();
                }
            }
            0
        }
        WM_CAPTURECHANGED => {
            let mut guard = SETTINGS_UI.lock().unwrap();
            if let Some(sui) = guard.as_mut() {
                if sui.0.sb_drag.take().is_some() {
                    sui.0.redraw();
                }
            }
            0
        }
        WM_KEYDOWN => {
            if wp as i32 == 0x1B {
                free_settings_surface();
                hide_settings();
            }
            0
        }
        WM_APP_ICS_DONE => {
            let mut guard = SETTINGS_UI.lock().unwrap();
            if let Some(sui) = guard.as_mut() {
                let ui = &mut sui.0;
                ui.refreshing = false;
                ui.ics_status = Some(wp != 0);
                ui.redraw();
            }
            0
        }
        WM_CLOSE => {
            hide_settings();
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn settings_hover_unused() {}

// ================= 近一周天气面板（紧贴日历左侧，顶部对齐，悬停天气热区时弹出） =================
pub const FC_W: f32 = 576.0;
pub const FC_H: f32 = 212.0;

static FORECAST_HWND: AtomicUsize = AtomicUsize::new(0);
static FORECAST_UI: Mutex<Option<SendFc>> = Mutex::new(None);

struct SendFc(Box<ForecastUi>);
unsafe impl Send for SendFc {}

struct ForecastUi {
    hwnd: usize,
    sf: f32,
    w: f32,
    h: f32,
    mem_dc: usize,
    hbmp: usize,
    bmp: gdi::Gp,
    scan0: *mut u8,
    g: gdi::Gp,
    cache: Cache,
    st: SharedState,
    dumped: bool,
    dump_path: String,
    /// “更新”链接热区（面板坐标系，paint 时更新）
    link_rect: gdi::RectF,
    link_hover: bool,
    /// 关闭按钮悬停
    close_hover: bool,
    /// 最近一次手动刷新时间（用于显示“更新中…”并防止连点）
    refreshing: Option<std::time::Instant>,
}

/// 天气面板关闭按钮热区（面板坐标系）
fn forecast_close_rect() -> gdi::RectF {
    gdi::RectF { x: FC_W - 28.0, y: 12.0, w: 20.0, h: 20.0 }
}

pub fn forecast_visible() -> bool {
    let h = FORECAST_HWND.load(Ordering::Relaxed);
    h != 0 && unsafe { IsWindowVisible(h as HWND) != 0 }
}

pub fn create_forecast_window(st: SharedState) {
    unsafe {
        let cls = crate::wide("z-calendar-forecast");
        let hinstance = winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null());
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(forecast_wndproc);
        wc.hInstance = hinstance;
        wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        wc.lpszClassName = cls.as_ptr();
        RegisterClassW(&wc);

        let w = gdi::phys(FC_W) as i32;
        let h = gdi::phys(FC_H) as i32;
        let title = crate::wide("Z日历天气");
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
        FORECAST_HWND.store(hwnd as usize, Ordering::Relaxed);

        let mut fui = Box::new(ForecastUi {
            hwnd: hwnd as usize,
            sf: gdi::scale(),
            w: FC_W,
            h: FC_H,
            mem_dc: 0,
            hbmp: 0,
            bmp: std::ptr::null_mut(),
            scan0: std::ptr::null_mut(),
            g: std::ptr::null_mut(),
            cache: Cache::new(),
            st,
            dumped: false,
            dump_path: std::env::var("CAL_DUMP3").unwrap_or_default(),
            link_rect: gdi::RectF { x: 0.0, y: 0.0, w: 0.0, h: 0.0 },
            link_hover: false,
            close_hover: false,
            refreshing: None,
        });
        let (f_mem_dc, f_hbmp, f_bmp, f_scan0) = gdi::alloc_dib(fui.w, fui.h);
        fui.mem_dc = f_mem_dc;
        fui.hbmp = f_hbmp;
        fui.bmp = f_bmp;
        fui.scan0 = f_scan0;
        GdipGetImageGraphicsContext(fui.bmp, &mut fui.g);

        // 初始绘制仅服务于 CAL_DUMP3 首帧转储（调试时绘制后立即释放；正常路径由
        // forecast_open→redraw 惰性分配）
        if !fui.dump_path.is_empty() {
            fui.redraw();
            unsafe {
                gdi::free_dib(&mut fui.mem_dc, &mut fui.hbmp, &mut fui.bmp, &mut fui.g, &mut fui.scan0);
            }
        }
        FORECAST_UI.lock().unwrap().replace(SendFc(fui));
    }
}

/// 悬停天气热区：在主面板左侧弹出面板（放不下时改到右侧）
pub fn forecast_open(main_hwnd: usize) {
    unsafe {
        let mut guard = FORECAST_UI.lock().unwrap();
        let Some(f) = guard.as_mut() else { return };
        let f = &mut f.0;
        let w = f.st.weather.lock().unwrap().clone();
        let Some(w) = w else { return };
        if w.days.is_empty() {
            return;
        }
        let fh = f.hwnd as HWND;
        if IsWindowVisible(fh) != 0 {
            return;
        }
        // 天气面板与日期侧栏同屏：侧栏开着时锚定其左缘（向左串联），二者不再互斥
        let mut mr: RECT = std::mem::zeroed();
        GetWindowRect(main_hwnd as HWND, &mut mr);
        let mut wa: RECT = std::mem::zeroed();
        SystemParametersInfoW(0x0030 /*SPI_GETWORKAREA*/, 0, &mut wa as *mut RECT as *mut c_void_ty2, 0);
        // 主面板可见边缘在窗口内 10px 处：面板右缘贴其左缘（间隔 0），顶部与日历对齐；
        // 日期侧栏打开时锚定到侧栏左缘（向左串联）。anchor/工作区均为物理像素，
        // 偏移与面板尺寸按 sf 放大
        let anchor = match crate::sidebar::sidebar_left_x() {
            Some(sx) => sx,
            None => mr.left + gdi::phys(10.0) as i32,
        };
        let mut x = anchor - gdi::phys(FC_W) as i32;
        let mut y = mr.top + gdi::phys(10.0) as i32;
        if y + gdi::phys(FC_H) as i32 > wa.bottom - gdi::phys(4.0) as i32 {
            y = wa.bottom - gdi::phys(4.0) as i32 - gdi::phys(FC_H) as i32;
        }
        if y < wa.top + gdi::phys(4.0) as i32 {
            y = wa.top + gdi::phys(4.0) as i32;
        }
        // 左侧放不下（屏幕过窄）：贴着工作区左缘，宁可压住日历也不越出屏幕
        // （窗口为贴屏幕右缘摆放，右缘已伸出屏幕外，不能再用窗口右缘做退路）
        let xmax = wa.right - gdi::phys(FC_W) as i32 - gdi::phys(4.0) as i32;
        x = x.max(wa.left + gdi::phys(4.0) as i32).min(xmax);
        SetWindowPos(fh, HWND_TOPMOST, x, y, gdi::phys(FC_W) as i32, gdi::phys(FC_H) as i32, SWP_NOACTIVATE);
        ShowWindow(fh, SW_SHOWNA);
        f.redraw();
    }
}

/// 天气面板已显示时按当前锚定（日期侧栏开/关）重新摆放
pub fn forecast_reposition() {
    if forecast_visible() {
        let mh = hwnd();
        if mh != 0 {
            let h = FORECAST_HWND.load(Ordering::Relaxed);
            unsafe {
                ShowWindow(h as HWND, SW_HIDE);
            }
            forecast_open(mh);
        }
    }
}

pub fn forecast_close() {
    let h = FORECAST_HWND.load(Ordering::Relaxed);
    if h != 0 && unsafe { IsWindowVisible(h as HWND) != 0 } {
        unsafe {
            ShowWindow(h as HWND, SW_HIDE);
        }
        // 释放后台位图压缩内存（下次 forecast_open 重绘时重建）
        let mut guard = FORECAST_UI.lock().unwrap();
        if let Some(f) = guard.as_mut() {
            let ui = &mut f.0;
            unsafe {
                gdi::free_dib(&mut ui.mem_dc, &mut ui.hbmp, &mut ui.bmp, &mut ui.g, &mut ui.scan0);
            }
        }
        crate::trim_working_set();
    }
}

/// 光标是否位于软件自身弹窗（天气侧栏/设置窗口）内
fn cursor_on_own_popup() -> bool {
    unsafe {
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        for h in [FORECAST_HWND.load(Ordering::Relaxed), SETTINGS_HWND.load(Ordering::Relaxed), crate::sidebar::sidebar_hwnd()] {
            if h != 0 && IsWindowVisible(h as HWND) != 0 {
                let mut r: RECT = std::mem::zeroed();
                GetWindowRect(h as HWND, &mut r);
                if pt.x >= r.left && pt.x <= r.right && pt.y >= r.top && pt.y <= r.bottom {
                    return true;
                }
            }
        }
        false
    }
}

/// 新增日程/待办弹窗打开期间面板不随失焦隐藏
pub fn foreground_is_own() -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_null() {
            return false;
        }
        let fgu = fg as usize;
        [
            hwnd(),
            SETTINGS_HWND.load(Ordering::Relaxed),
            FORECAST_HWND.load(Ordering::Relaxed),
            crate::sidebar::sidebar_hwnd(),
            crate::inputbox::hwnd(),
        ]
        .contains(&fgu)
    }
}

/// 重绘主面板（日程保存后刷新日程页）
pub fn flyout_repaint() {
    let mut guard = UI.lock().unwrap();
    if let Some(sui) = guard.as_mut() {
        if sui.0.shown {
            sui.0.redraw();
        }
    }
}

/// 设置窗口可见时重绘（主题切换 / 字号变化等全局外观事件）
pub fn settings_repaint_if_visible() {
    if settings_visible() {
        let mut guard = SETTINGS_UI.lock().unwrap();
        if let Some(sui) = guard.as_mut() {
            sui.0.redraw();
        }
    }
}

pub fn forecast_redraw() {
    let mut guard = FORECAST_UI.lock().unwrap();
    if let Some(f) = guard.as_mut() {
        f.0.redraw();
    }
}

impl ForecastUi {
    fn redraw(&mut self) {
        if self.bmp.is_null() {
            // 隐藏时位图已释放压缩内存：显示前重建
            let (mem_dc, hbmp, bmp, scan0) = unsafe { gdi::alloc_dib(self.w, self.h) };
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
        let p = Painter { g, cache: cache_ptr, sf: self.sf, w: self.w, h: self.h, dc: self.mem_dc, scan0: self.scan0 };
        self.paint(&p);
        self.ulw();

        if !self.dumped && !self.dump_path.is_empty() {
            self.dumped = true;
            save_bmp(self.scan0, (self.w * self.sf) as i32, (self.h * self.sf) as i32, &self.dump_path);
            if std::env::var("CAL_DUMP_EXIT").map(|v| v == "1").unwrap_or(false) {
                unsafe {
                    PostMessageW(hwnd() as HWND, WM_CLOSE, 0, 0);
                }
            }
        }
    }

    fn ulw(&self) {
        unsafe {
            let mut r: RECT = std::mem::zeroed();
            GetWindowRect(self.hwnd as HWND, &mut r);
            let mut ppt = POINT { x: r.left, y: r.top };
            let mut size = SIZE { cx: (self.w * self.sf) as i32, cy: (self.h * self.sf) as i32 };
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
        // 面板底（无边框、无阴影环，窗口即面板）
        p.fill_round(0.0, 0.0, FC_W, FC_H, 12.0, POPUP_BG());

        let w = self.st.weather.lock().unwrap().clone();
        let Some(w) = w else { return };
        if w.days.is_empty() {
            return;
        }
        let today = Local::now().date_naive();

        // 头部：定位城市 + 更新时间 + 手动刷新链接
        p.text("\u{E81D}", 16.0, 12.0, 18.0, 22.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.0, false, true, BLUE());
        p.text(&w.city, 36.0, 12.0, 160.0, 22.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, true, false, TITLE_COL());
        let refreshing = self
            .refreshing
            .map(|t| t.elapsed() < std::time::Duration::from_secs(5))
            .unwrap_or(false);
        let time_str = chrono::DateTime::from_timestamp((w.ts / 1000) as i64, 0)
            .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M").to_string())
            .unwrap_or_else(|| "--:--".into());
        let head = format!("更新时间：{}", time_str);
        let (tail, tail_col) = if refreshing {
            ("更新中…", if self.link_hover { ON_BG() } else { BLUE() })
        } else {
            ("更新", if self.link_hover { ON_BG() } else { BLUE() })
        };
        let hw = p.measure(&head, 10.0, false, false).0;
        let tw = p.measure(tail, 10.0, false, false).0;
        let gap = 6.0;
        // 右侧预留关闭按钮区域
        let hx = FC_W - 34.0 - hw - gap - tw;
        p.text(&head, hx, 14.0, hw + 2.0, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 10.0, false, false, SUB_DIM());
        p.text(tail, hx + hw + gap, 14.0, tw + 2.0, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 10.0, false, false, tail_col);
        self.link_rect = gdi::RectF { x: hx + hw + gap, y: 14.0, w: tw, h: 18.0 };
        // 关闭按钮 ✕
        let cr = forecast_close_rect();
        p.text("✕", cr.x, cr.y, cr.w, cr.h, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 11.0, false, false, if self.close_hover { RED() } else { SUB_DIM() });
        p.fill_rect(16.0, 42.0, FC_W - 32.0, 1.0, DIVIDER());

        // 每日列（不足 7 天时居中排布）
        let n = w.days.len().min(7);
        let col_w = (FC_W - 24.0) / 7.0;
        let x0 = 12.0 + ((FC_W - 24.0) - col_w * n as f32) / 2.0;
        for (i, d) in w.days.iter().take(n).enumerate() {
            let cx = x0 + col_w * i as f32 + col_w / 2.0;
            let weekday = ["星期日", "星期一", "星期二", "星期三", "星期四", "星期五", "星期六"]
                [d.date.weekday().num_days_from_sunday() as usize];
            p.text(weekday, cx - col_w / 2.0, 52.0, col_w, 18.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.5, true, false, if d.date == today { ON_BG() } else { DATE_COL() });

            // 日期 + 相对标签（今天/明天/后天）
            let dstr = d.date.format("%m-%d").to_string();
            let tag = match (d.date - today).num_days() {
                0 => "今天",
                1 => "明天",
                2 => "后天",
                _ => "",
            };
            let dw = p.measure(&dstr, 10.0, false, false).0;
            let tw = if tag.is_empty() { 0.0 } else { p.measure(tag, 10.0, false, false).0 };
            let gap = if tag.is_empty() { 0.0 } else { 5.0 };
            let tx = cx - (dw + gap + tw) / 2.0;
            p.text(&dstr, tx, 72.0, dw + 2.0, 14.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 10.0, false, false, SUB());
            if !tag.is_empty() {
                p.text(tag, tx + dw + gap, 72.0, tw + 2.0, 14.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 10.0, false, false, BLUE());
            }

            draw_weather(p, cx, 110.0, d.code, 1.25);
            p.text(crate::weather::wmo_text(d.code), cx - col_w / 2.0, 138.0, col_w, 16.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 11.0, false, false, ROW_TXT());
            p.text(&format!("{} ~ {}°C", d.tmin, d.tmax), cx - col_w / 2.0, 158.0, col_w, 18.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, DATE_COL());

            // 空气质量（短等级带“空气”前缀，与参考样式一致）
            match d.aqi {
                Some(a) => {
                    let (cat, (r, g2, b)) = crate::weather::aqi_level(a);
                    let label = if cat.chars().count() <= 1 {
                        format!("空气{} {}", cat, a)
                    } else {
                        format!("{} {}", cat, a)
                    };
                    p.text(&label, cx - col_w / 2.0, 180.0, col_w, 14.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 10.0, false, false, gdi::argb(255, r, g2, b));
                }
                None => {
                    p.text("空气 --", cx - col_w / 2.0, 180.0, col_w, 14.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 10.0, false, false, SUB_DIM());
                }
            }
        }
    }
}

unsafe extern "system" fn forecast_wndproc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            ValidateRect(hwnd, std::ptr::null_mut());
            0
        }
        WM_ERASEBKGND => 1,
        // 点击面板不改变激活状态：主面板不会因失焦隐藏，点击消息正常送达本面板
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_MOUSEMOVE => {
            let mut guard = FORECAST_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                let x = ((lp & 0xFFFF) as u16 as i16) as f32 / f.sf;
                let y = (((lp as usize) >> 16) as u16 as i16) as f32 / f.sf;
                let r = f.link_rect;
                let hit = x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
                if hit != f.link_hover {
                    f.link_hover = hit;
                    f.redraw();
                }
                let cr = forecast_close_rect();
                let chit = x >= cr.x && x < cr.x + cr.w && y >= cr.y && y < cr.y + cr.h;
                if chit != f.close_hover {
                    f.close_hover = chit;
                    f.redraw();
                }
                if hit || chit {
                    SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_HAND));
                    // 注册离开跟踪，移出链接/按钮后恢复箭头与颜色
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    TrackMouseEvent(&mut tme);
                }
            }
            0
        }
        WM_MOUSELEAVE => {
            let mut guard = FORECAST_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                if f.link_hover || f.close_hover {
                    f.link_hover = false;
                    f.close_hover = false;
                    f.redraw();
                }
            }
            0
        }
        WM_LBUTTONDOWN => {
            let mut want_close = false;
            {
                let mut guard = FORECAST_UI.lock().unwrap();
                if let Some(f) = guard.as_mut() {
                    let f = &mut f.0;
                    let x = ((lp & 0xFFFF) as u16 as i16) as f32 / f.sf;
                    let y = (((lp as usize) >> 16) as u16 as i16) as f32 / f.sf;
                    let cr = forecast_close_rect();
                    if x >= cr.x && x < cr.x + cr.w && y >= cr.y && y < cr.y + cr.h {
                        want_close = true;
                    } else {
                        let r = f.link_rect;
                        let refreshing = f
                            .refreshing
                            .map(|t| t.elapsed() < std::time::Duration::from_secs(5))
                            .unwrap_or(false);
                        if !refreshing && x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h {
                            // 点击“更新”：唤醒天气线程立即拉取（5 秒内防连点）
                            f.refreshing = Some(std::time::Instant::now());
                            let _ = f.st.weather_tx.send(());
                            f.redraw();
                        }
                    }
                }
            }
            // 关闭在锁外执行（forecast_close 会重新拿 FORECAST_UI 锁）
            if want_close {
                forecast_close();
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}


pub fn create_window(st: SharedState, agenda: Arc<Mutex<crate::events::AgendaMap>>, tray: Arc<Mutex<Option<tray::Tray>>>) {
    unsafe {
        let cls = crate::wide("z-calendar-main");
        let hinstance = winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null());
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinstance;
        wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        wc.lpszClassName = cls.as_ptr();
        RegisterClassW(&wc);

        let w = gdi::phys(WIN_W) as i32;
        let h = gdi::phys(WIN_H) as i32;
        let title = crate::wide("Z日历");
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED,
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
        FLYOUT_HWND.store(hwnd as usize, Ordering::Relaxed);
        // 命中区域只取可见面板：窗口为贴屏幕右缘与任务栏摆放，四周那一圈透明内缩会超出
        // 屏幕/压在任务栏上，不设区域就会吞掉任务栏上的点击
        set_flyout_region(hwnd);

        let page = match std::env::var("CAL_PAGE").as_deref() {
            Ok("agenda") => Page::Agenda,
            _ => Page::Calendar,
        };
        let settings_open = std::env::var("CAL_SETTINGS").map(|v| v == "1").unwrap_or(false);
        let settings_tab: usize = std::env::var("CAL_TAB")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let today = Local::now().date_naive();
        let view_y = std::env::var("CAL_YM")
            .ok()
            .and_then(|v| v.split('-').next().and_then(|x| x.parse().ok()))
            .unwrap_or(today.year());
        let view_m = std::env::var("CAL_YM")
            .ok()
            .and_then(|v| v.split('-').nth(1).and_then(|x| x.parse().ok()))
            .unwrap_or(today.month());

        let mut ui = Box::new(Ui {
            hwnd: hwnd as usize,
            sf: gdi::scale(),
            w: WIN_W,
            h: WIN_H,
            mem_dc: 0,
            hbmp: 0,
            bmp: std::ptr::null_mut(),
            scan0: std::ptr::null_mut(),
            g: std::ptr::null_mut(),
            cache: Cache::new(),
            st,
            tray,
            agenda,
            shown: false,
            page,
            flush_right: false,
            flush_bottom: false,
            view_y,
            view_m,
            selected: today,
            regions: Vec::new(),
            hover: None,
            draft: String::new(),
            comp: String::new(),
            caret_on: true,
            draft_caret: 0,
            draft_sel: None,
            hint: String::new(),
            hint_until: None,
            hint_warn: false,
            hover_cell: None,
            hover_since: None,
            preview: None,
            tick: 0,
            last_second: 0,
            idle_timer: false,
            agenda_scroll: 0,
            agenda_resolved: Vec::new(),
            agenda_todos: Vec::new(),
            agenda_resolved_dates: Vec::new(),
            draft_kind: 0,
            kind_menu_open: false,
            dumped: false,
            dump_path: std::env::var("CAL_DUMP").unwrap_or_default(),
        });
        // 后台位图不再启动时常驻：显示时 perform_show→redraw 惰性分配，隐藏即释放。
        // 初始绘制仅服务于 CAL_DUMP 首帧转储（转储完成立即释放）
        UI.lock().unwrap().replace(SendUi(ui));
        {
            let mut guard = UI.lock().unwrap();
            if let Some(sui) = guard.as_mut() {
                if !sui.0.dump_path.is_empty() {
                    sui.0.redraw();
                    free_surface(&mut sui.0);
                }
            }
        }

        SetTimer(hwnd, 1, 250, None);
    }
}

pub fn shown_flag() -> usize {
    SHOWN_FLAG.load(Ordering::Relaxed)
}

pub fn hwnd() -> usize {
    FLYOUT_HWND.load(Ordering::Relaxed)
}

pub fn overlay_click(button: usize) {
    let h = FLYOUT_HWND.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            PostMessageW(h as HWND, WM_APP_TOGGLE, button, 0);
        }
    }
}

pub fn request_show() {
    let h = FLYOUT_HWND.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            PostMessageW(h as HWND, WM_APP_SHOW, 0, 0);
        }
    }
}

static UI: Mutex<Option<SendUi>> = Mutex::new(None);

struct SendUi(Box<Ui>);
unsafe impl Send for SendUi {}

/// 弹窗摆放结果：窗口左上角坐标 + 面板是否贴住右缘/下缘（贴边侧画直角）
struct Placement {
    x: i32,
    y: i32,
    flush_right: bool,
    flush_bottom: bool,
}

/// 弹窗位置：任务栏停在屏幕底部时，可见面板右缘贴工作区右缘、下缘距任务栏上缘 EDGE_GAP
/// （抬起来，不压住任务栏）；其余任务栏位置（上/左/右）仍贴在时钟上方，上方放不下则改到时钟下方
fn position_for(clock: Option<&crate::overlay::ClockInfo>, sf: f32, win_w: i32, win_h: i32) -> Placement {
    let Some(ci) = clock else {
        return Placement { x: 32000, y: 32000, flush_right: false, flush_bottom: false };
    };
    let clk_right = ci.rect.right as f32 / sf;
    let clk_top = ci.rect.top as f32 / sf;
    let clk_bottom = ci.rect.bottom as f32 / sf;
    let mon_bottom = ci.mon.3 as f32 / sf;
    let wa = (
        ci.work.0 as f32 / sf,
        ci.work.1 as f32 / sf,
        ci.work.2 as f32 / sf,
        ci.work.3 as f32 / sf,
    );
    let (ww, wh) = (win_w as f32, win_h as f32);
    // 可见面板在窗口内四周各内缩 PANEL_INSET，故面板边缘 = 窗口原点 + PANEL_INSET。
    // 下面各式把"面板要贴的屏幕位置"换算成窗口原点：
    //   win = 目标面板右/下缘 - 窗口尺寸 + PANEL_INSET
    let x_flush = wa.2 - ww + PANEL_INSET;
    let y_flush = wa.3 - EDGE_GAP - wh + PANEL_INSET;
    let x_clock = clk_right - ww + PANEL_INSET;
    let y_clock = clk_top - CLOCK_GAP - wh + PANEL_INSET;
    // 任务栏停在屏幕底部 ⇒ 工作区只在下方被占：右缘贴工作区右缘、下缘贴任务栏
    // （原生时钟居中于任务栏、够不到屏幕下缘，故用工作区与显示器下缘之差判定，而非时钟自身）
    let docked = wa.3 < mon_bottom;
    let (mut x, mut y) = if docked { (x_flush, y_flush) } else { (x_clock, y_clock) };
    // 非贴边时上方放不下 → 改到时钟下方
    if !docked && y + PANEL_INSET < wa.1 {
        y = clk_bottom - PANEL_INSET;
    }
    // 收进工作区：面板左缘 ≥ 工作区左缘、右缘 ≤ 工作区右缘、下缘 ≤ 工作区下缘
    x = (wa.0 - PANEL_INSET).max(x.min(x_flush));
    y = (wa.1 - PANEL_INSET).max(y.min(y_flush));
    // 最终落在工作区右/下缘 ⇒ 该侧贴边，画直角消除圆角与屏幕边缘之间的月牙缝
    let (xi, yi) = (x.round() as i32, y.round() as i32);
    Placement {
        x: xi,
        y: yi,
        flush_right: (xi as f32 + ww - PANEL_INSET - wa.2).abs() < 0.5,
        flush_bottom: (yi as f32 + wh - PANEL_INSET - wa.3).abs() < 0.5,
    }
}

fn set_shown(ui: &mut Ui, show: bool) {
    if show {
        ui.shown = true;
    } else {
        ui.shown = false;
        SHOWN_FLAG.store(0, Ordering::Relaxed);
        crate::trim_working_set();
    }
}

/// 显示弹窗：窗口操作在 UI 锁之外执行（避免消息重入死锁）
fn perform_show(hwnd: HWND) {
    let p = {
        let guard = UI.lock().unwrap();
        let ui = &guard.as_ref().unwrap().0;
        let clock = ui.st.clock.lock().unwrap().clone();
        position_for(clock.as_ref(), ui.sf, ui.w as i32, ui.h as i32)
    };
    let (w, h) = (gdi::phys(WIN_W) as i32, gdi::phys(WIN_H) as i32);
    unsafe {
        SetWindowPos(hwnd, HWND_TOPMOST, gdi::phys(p.x as f32) as i32, gdi::phys(p.y as f32) as i32, w, h, SWP_NOACTIVATE);
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
    let mut guard = UI.lock().unwrap();
    if let Some(sui) = guard.as_mut() {
        let ui = &mut sui.0;
        ui.flush_right = p.flush_right;
        ui.flush_bottom = p.flush_bottom;
        ui.shown = true;
        SHOWN_FLAG.store(1, Ordering::Relaxed);
        ui.redraw();
        // 调试：CAL_SIDEBAR=1 弹出日历时自动打开日期侧边栏
        if std::env::var("CAL_SIDEBAR").map(|v| v == "1").unwrap_or(false) {
            crate::sidebar::sidebar_show(ui.selected);
        }
    }
}

fn perform_hide(hwnd: HWND) {
    let was_shown;
    {
        let mut guard = UI.lock().unwrap();
        match guard.as_mut() {
            Some(sui) => {
                was_shown = sui.0.shown;
                sui.0.shown = false;
                if was_shown {
                    free_surface(&mut sui.0);
                }
            }
            None => was_shown = false,
        }
    }
    if was_shown {
        unsafe {
            ShowWindow(hwnd, SW_HIDE);
        }
        forecast_close();
        crate::sidebar::sidebar_hide();
    }
    // 设置窗口保持打开，只由其"确定/✕"按钮关闭
    SHOWN_FLAG.store(0, Ordering::Relaxed);
    crate::trim_working_set();
}

fn toggle(ui: &mut Ui) {
    let show = !ui.shown;
    set_shown(ui, show);
}

/// 主面板窗口命中区域：可见面板四周内缩 PANEL_INSET（物理像素）。
/// 区域是固定像素裁剪，屏幕缩放变化后必须按新 sf 重建，否则窗口被旧区域
/// 裁剪、日历显示不全。
fn set_flyout_region(hwnd: HWND) {
    let inset = gdi::phys(PANEL_INSET) as i32;
    let w = gdi::phys(WIN_W) as i32;
    let h = gdi::phys(WIN_H) as i32;
    unsafe {
        let rgn = CreateRectRgn(inset, inset, w - inset, h - inset);
        SetWindowRgn(hwnd, rgn, 0);
    }
}

// ================= 屏幕缩放变化（PMv2 轮询） =================

/// 轮询主屏有效 DPI：与当前 sf 不同 → 全局按新 sf 重建/重摆。
/// 在主面板 WM_TIMER（锁外）调用。
fn poll_scale_change() {
    if gdi::scale_overridden() {
        return;
    }
    let now = gdi::primary_scale();
    // 与纯 DPI 缩放比较（全局 scale 含字号系数，不能作对比基准）
    if now > 0.0 && (now - gdi::dpi_scale()).abs() > 0.001 {
        gdi::set_scale(now);
        rescale_all(gdi::scale());
    }
}

/// 屏幕缩放变化：所有窗口按新 sf 重建/重摆（仅 UI 线程调用，内部自行加锁，
/// 调用方不得持有任何 UI 锁）
fn rescale_all(sf: f32) {
    // 主面板：命中区域与位图都按新 sf 重建；显示中 → 立即按新 sf 重摆重绘
    let shown;
    {
        let mut guard = UI.lock().unwrap();
        if let Some(sui) = guard.as_mut() {
            let ui = &mut sui.0;
            ui.sf = sf;
            free_surface(ui);
            shown = ui.shown;
        } else {
            shown = false;
        }
    }
    let h = FLYOUT_HWND.load(Ordering::Relaxed);
    if h != 0 {
        set_flyout_region(h as HWND);
    }
    if shown && h != 0 {
        perform_show(h as HWND);
    }
    // 设置窗口：可见 → 原地重设尺寸并重绘；不可见 → 仅更新 sf（位图已释放）
    let settings_vis = settings_visible();
    {
        let mut guard = SETTINGS_UI.lock().unwrap();
        if let Some(sui) = guard.as_mut() {
            let ui = &mut sui.0;
            ui.sf = sf;
            unsafe {
                gdi::free_dib(&mut ui.mem_dc, &mut ui.hbmp, &mut ui.bmp, &mut ui.g, &mut ui.scan0);
            }
            if settings_vis {
                unsafe {
                    SetWindowPos(
                        ui.hwnd as HWND,
                        std::ptr::null_mut(),
                        0,
                        0,
                        gdi::phys(SETTINGS_W) as i32,
                        gdi::phys(SETTINGS_H) as i32,
                        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                ui.redraw();
            }
        }
    }
    // 近一周天气面板：瞬态，收起（下次悬停自动按新 sf 重建重摆）
    forecast_close();
    // 日期侧栏 / 新建编辑弹窗 / 提醒卡片
    crate::sidebar::rescale(sf);
    crate::inputbox::rescale(sf);
    crate::toast::rescale();
    // 三个右键菜单：下次打开时按新 sf 自愈重建（redraw 检测 sf 失配），无需处理
}

// ================= 绘制 =================
impl Ui {
    fn redraw(&mut self) {
        if self.bmp.is_null() {
            // 隐藏时位图已释放压缩内存：显示前重建
            unsafe { create_dib(self); }
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
        let p = unsafe {
            Painter {
                g,
                cache: cache_ptr,
                sf: self.sf,
                w: self.w,
                h: self.h,
                dc: self.mem_dc,
                scan0: self.scan0,
            }
        };
        let mut regions: Vec<(gdi::RectF, Action)> = Vec::new();
        p.clear();
        self.paint_frame(&p);
        match self.page {
            Page::Calendar => self.paint_calendar(&p, &mut regions),
            Page::Agenda => self.paint_agenda(&p, &mut regions),
        }        self.regions = regions;
        self.ulw();

        if !self.dumped && !self.dump_path.is_empty() {
            self.dumped = true;
            save_bmp(self.scan0, (self.w * self.sf) as i32, (self.h * self.sf) as i32, &self.dump_path);
            if std::env::var("CAL_DUMP_EXIT").map(|v| v == "1").unwrap_or(false) {
                unsafe {
                    PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0);
                }
            }
        }
    }

    fn ulw(&self) {
        unsafe {
            let mut r: RECT = std::mem::zeroed();
            GetWindowRect(self.hwnd as HWND, &mut r);
            let mut ppt = POINT { x: r.left, y: r.top };
            let mut size = SIZE { cx: (self.w * self.sf) as i32, cy: (self.h * self.sf) as i32 };
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

    fn paint_frame(&self, p: &Painter) {
        let (x, y) = (PANEL_INSET, PANEL_INSET);
        let (w, h) = (WIN_W - PANEL_INSET * 2.0, WIN_H - PANEL_INSET * 2.0);
        p.fill_round(x, y, w, h, PANEL_RADIUS, BG());
        // 贴边的角补成直角：圆角会在屏幕/任务栏边缘留下一块透明月牙
        if self.flush_right {
            p.fill_rect(x + w - PANEL_RADIUS, y, PANEL_RADIUS, PANEL_RADIUS, BG());
            p.fill_rect(x + w - PANEL_RADIUS, y + h - PANEL_RADIUS, PANEL_RADIUS, PANEL_RADIUS, BG());
        }
        if self.flush_bottom {
            p.fill_rect(x, y + h - PANEL_RADIUS, PANEL_RADIUS, PANEL_RADIUS, BG());
            p.fill_rect(x + w - PANEL_RADIUS, y + h - PANEL_RADIUS, PANEL_RADIUS, PANEL_RADIUS, BG());
        }
    }

    fn hit_add(regions: &mut Vec<(gdi::RectF, Action)>, x: f32, y: f32, w: f32, h: f32, a: Action) {
        regions.push((gdi::RectF { x, y, w, h }, a));
    }

    fn hovered(&self, a: &Action) -> bool {
        self.hover.map(|h| h == *a).unwrap_or(false)
    }

    fn paint_calendar(&self, p: &Painter, regions: &mut Vec<(gdi::RectF, Action)>) {
        let today = Local::now().date_naive();
        let now = Local::now();
        let cfg = self.st.config.lock().unwrap().clone();
        let panel_x0 = 10.0;
        let panel_x1 = WIN_W - 10.0;
        let pad = 14.0;
        let left = panel_x0 + pad;
        let right = panel_x1 - pad;

        // 头部：时钟（12/24 小时制） + 日期 + 天气
        let clock_str = if cfg.hour12 {
            let (pm, h12) = now.hour12();
            format!(
                "{} {:02}:{:02}:{:02}",
                if pm { "下午" } else { "上午" },
                h12,
                now.minute(),
                now.second()
            )
        } else {
            format!("{:02}:{:02}:{:02}", now.hour(), now.minute(), now.second())
        };
        p.text(&clock_str, left, 22.0, 320.0, 52.0, gdi::HALIGN_NEAR, gdi::HALIGN_NEAR, 40.0, true, false, TXT());
        let l = now.date_naive();
        let lunar = lunar::solar_to_lunar(l);
        let lunar_txt = lunar
            .map(|l| format!("{}月{}", lunar::month_cn(l.month), lunar::day_cn(l.day)))
            .unwrap_or_default();
        let date_str = format!("{}年{}月{}日", l.year(), l.month(), l.day());
        p.text(&date_str, left, 78.0, 200.0, 20.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.5, false, false, DATE_COL());
        let date_w = p.measure(&date_str, 13.5, false, false).0;
        p.text(&lunar_txt, left + date_w + 8.0, 78.0, 140.0, 20.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.5, false, false, SUB());

        // 天气（右上角，悬停弹出近一周天气面板）
        let wx = if cfg.show_weather { self.st.weather.lock().unwrap().clone() } else { None };
        if let Some(w) = &wx {
            let can_hover = !w.days.is_empty();
            if can_hover && self.hovered(&Action::Weather) {
                p.fill_round(right - 78.0, 32.0, 76.0, 58.0, 8.0, HOVER_BG());
            }
            draw_weather(p, right - 22.0, 46.0, w.code, 1.0);
            p.text(&format!("{}°C", w.temp), right - 44.0, 70.0, 44.0, 20.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 15.0, false, false, DATE_COL());
            if can_hover {
                Self::hit_add(regions, right - 78.0, 28.0, 76.0, 62.0, Action::Weather);
            }
        }

        // 分隔线
        p.fill_rect(left, 102.0, right - left, 1.0, DIVIDER());

        // 月份栏
        Self::hit_add(regions, left, 108.0, 160.0, 24.0, Action::Title);
        p.text(&format!("{}年{}月", self.view_y, self.view_m), left, 108.0, 160.0, 24.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 16.0, true, false, TITLE_COL());
        let bw = 26.0;
        let gear_x = right - bw;
        let next_x = gear_x - bw - 4.0;
        let prev_x = next_x - bw - 4.0;
        Self::hit_add(regions, prev_x, 108.0, bw, 24.0, Action::Prev);
        Self::hit_add(regions, next_x, 108.0, bw, 24.0, Action::Next);
        Self::hit_add(regions, gear_x, 108.0, bw, 24.0, Action::OpenSettings);
        for (bx, glyph, px, act) in [
            (prev_x, "‹", 20.0, Action::Prev),
            (next_x, "›", 20.0, Action::Next),
            (gear_x, "\u{E713}", 13.0, Action::OpenSettings),
        ] {
            let hov = self.hovered(&act);
            if hov {
                p.fill_round(bx, 108.0, bw, 24.0, 6.0, HOVER_BG());
            }
            p.text(glyph, bx, 108.0, bw, 24.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, px, false, glyph == "\u{E713}", if hov { ON_BG() } else { ICON_COL() });
        }

        // 星期表头（周起始日可配置）
        let show_gutter = cfg.show_week_num;
        let gutter_w = if show_gutter { 26.0 } else { 0.0 };
        let col_w = (WIN_W - 20.0 - pad * 2.0 - gutter_w) / 7.0;
        let week_names = ["一", "二", "三", "四", "五", "六", "日"];
        let gy = 132.0;
        let ws = cfg.week_start as usize % 7;
        for c in 0..7usize {
            let wd = (ws + c) % 7;
            let cx = left + gutter_w + col_w * c as f32 + col_w / 2.0;
            p.text(week_names[wd], cx - 30.0, gy, 60.0, 16.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 11.5, false, false, WEEK_HEAD());
        }

        // 月网格：显示非当前月日期时固定 6 行（含与当前月相邻的上一周/下一周）；
        // 关闭时只保留包含当前月日期的行，行高变大补满日历（窗口高度不变）
        let grid_y = 150.0;
        let grid_h = WIN_H - 10.0 - 6.0 - 48.0 - grid_y;
        let first = NaiveDate::from_ymd_opt(self.view_y, self.view_m, 1).unwrap_or(today);
        let first_wd = first.weekday().num_days_from_monday() as i64;
        let offset = (first_wd - ws as i64 + 7) % 7;
        let start = first - Duration::days(offset);
        let dim = {
            let (ny, nm) = if self.view_m == 12 { (self.view_y + 1, 1) } else { (self.view_y, self.view_m + 1) };
            NaiveDate::from_ymd_opt(ny, nm, 1)
                .and_then(|d| d.pred_opt())
                .map(|d| d.day() as i64)
                .unwrap_or(30)
        };
        let rows_min = ((offset + dim + 6) / 7).max(4);
        let rows = if cfg.show_other_month { 6 } else { rows_min };
        let row_h = (grid_h / rows as f32).max(52.0);
        // 持锁借用代替 clone：重绘每秒发生，避免整表复制把堆撑大
        let holidays = self.st.holidays.read().unwrap();
        let agenda = self.agenda.lock().unwrap();

        // 含未完成待办的日期（橙点角标，按天重复的待办展开到可见范围）
        let todo_keys = crate::sidebar::todo_keys_between(start, start + Duration::days((rows * 7 - 1) as i64));
        // 按天重复的日程规则：逐格判断角标
        let recur_rules = crate::events::recur_rules(&agenda);
        // 跨天日程覆盖的日期（中段/尾段补角标；开始日由原生条目负责）
        let span_keys = crate::events::agenda_span_keys(&agenda, start, start + Duration::days((rows * 7 - 1) as i64));
        for r in 0..rows {
            let row_top = grid_y + row_h * r as f32;
            let row_start = start + Duration::days(r * 7);
            if show_gutter {
                // 周数右对齐：数字右缘距网格 15px
                p.text(
                    &format!("{}", row_start.iso_week().week()),
                    10.0,
                    row_top + 7.0,
                    left + gutter_w - 15.0 - 10.0,
                    14.0,
                    gdi::HALIGN_FAR,
                    gdi::HALIGN_CENTER,
                    9.0,
                    false,
                    false,
                    WEEK_NUM(),
                );
            }
            for c in 0..7i64 {
                let date = start + Duration::days(r * 7 + c);
                let cx = left + gutter_w + col_w * c as f32;
                let rect = gdi::RectF { x: cx, y: row_top, w: col_w, h: row_h };
                let in_month = date.month() == self.view_m && date.year() == self.view_y;
                if !in_month && !cfg.show_other_month {
                    continue; // 不显示非当前月日期
                }
                let key = crate::ics::key_of_date(date);
                let hol = holidays.get(&key);
                let has_agenda = agenda.get(&key).map(|v| !v.is_empty()).unwrap_or(false)
                    || recur_rules.iter().any(|rule| rule.hits(date))
                    || span_keys.contains(&key);
                let has_todo = todo_keys.contains(&key);
                Self::hit_add(regions, cx, row_top, col_w, row_h, Action::Cell(date));
                paint_day_cell(p, self, &rect, date, today, self.selected, in_month, hol, has_agenda, has_todo, &cfg);
            }
        }

        // 底部工具栏
        self.paint_bottom_bar(p, regions);

        // 悬停预览：停在同一日期格 400ms 后显示当天日程/待办摘要（月历页）
        if let Some(pv) = self.preview {
            if pv >= start {
                let day_off = (pv - start).num_days();
                if day_off >= 0 && (day_off as usize) < (rows * 7) as usize {
                    let r = day_off / 7;
                    let c = day_off % 7;
                    let cell = gdi::RectF {
                        x: left + gutter_w + col_w * c as f32,
                        y: grid_y + row_h * r as f32,
                        w: col_w,
                        h: row_h,
                    };
                    let items = crate::events::agenda_on(&agenda, pv);
                    let tds = crate::sidebar::todos_for(&crate::ics::key_of_date(pv));
                    paint_day_preview(p, &cell, pv, &items, &tds);
                }
            }
        }
    }

    fn paint_bottom_bar(&self, p: &Painter, regions: &mut Vec<(gdi::RectF, Action)>) {
        let bar_y = WIN_H - 10.0 - 48.0;
        let bar_h = 48.0;
        let w = WIN_W - 20.0;
        let slot = w / 5.0;
        p.fill_rect(10.0, bar_y, w, 1.0, DIVIDER());
        let items: [(&str, &str, f32, Action); 5] = [
            ("日程", "\u{E787}", 16.0, Action::BottomAgenda),
            ("今天", "\u{E823}", 16.0, Action::BottomToday),
            ("+", "", 0.0, Action::BottomPlus),
            ("设置", "\u{E713}", 15.0, Action::BottomSettings),
            ("退出", "\u{E7E8}", 16.0, Action::BottomExit),
        ];
        for (i, (name, glyph, size, act)) in items.iter().enumerate() {
            let cx = 10.0 + slot * i as f32 + slot / 2.0;
            let cy = bar_y + bar_h / 2.0;
            Self::hit_add(regions, 10.0 + slot * i as f32, bar_y, slot, bar_h, *act);
            let hovered = self.hovered(act);
            if *name == "+" {
                if hovered {
                    p.fill_circle(cx, cy, 21.0, HOVER_BG());
                }
                p.fill_circle(cx, cy, 20.0, PLUS_BOT());
                p.fill_circle(cx, cy - 1.0, 18.5, PLUS_TOP());
                p.text("+", cx - 20.0, cy - 20.0, 40.0, 40.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 24.0, true, false, WHITE);
            } else {
                let col = if hovered { gdi::argb(255, 0xC7, 0xCD, 0xD4) } else { ICON_COL() };
                if hovered {
                    p.fill_round(10.0 + slot * i as f32 + 6.0, bar_y + 6.0, slot - 12.0, bar_h - 10.0, 8.0, crate::theme::ov(14));
                }
                p.text(glyph, cx - 25.0, cy - 20.0, 50.0, 22.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, *size, false, true, col);
                p.text(name, cx - 25.0, cy + 3.0, 50.0, 16.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 10.0, false, false, col);
            }
        }
    }

    fn paint_agenda(&mut self, p: &Painter, regions: &mut Vec<(gdi::RectF, Action)>) {
        Self::hit_add(regions, 20.0, 16.0, 30.0, 30.0, Action::Back);
        p.text("\u{E72B}", 20.0, 16.0, 30.0, 30.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 15.0, false, true, ICON_COL());
        p.text("日程 / 待办", 58.0, 16.0, 320.0, 30.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 15.0, true, false, TITLE_COL());

        // 以日期分组（今天起 30 天、升序）：每天合并展示日程与待办，今天的分组置顶高亮
        let today = Local::now().date_naive();
        self.agenda_resolved.clear();
        self.agenda_resolved_dates.clear();
        self.agenda_todos.clear();
        let mut rows: Vec<AgRow> = Vec::new();
        for off in 0..30i64 {
            let d = today + Duration::days(off);
            let items = crate::events::agenda_on(&self.agenda.lock().unwrap(), d);
            let tds = crate::sidebar::todos_for(&crate::ics::key_of_date(d));
            if items.is_empty() && tds.is_empty() {
                continue;
            }
            rows.push(AgRow::Head(d, off == 0));
            let base = self.agenda_resolved.len();
            for i in 0..items.len() {
                rows.push(AgRow::Agenda(base + i));
            }
            self.agenda_resolved.extend(items.iter().cloned());
            for _ in 0..items.len() {
                self.agenda_resolved_dates.push(d);
            }
            let base_t = self.agenda_todos.len();
            for i in 0..tds.len() {
                rows.push(AgRow::Todo(base_t + i));
            }
            self.agenda_todos.extend(tds.into_iter().map(|(gi, t)| (gi, t, d)));
        }
        let row_h = 38.0;
        let gap = 6.0;
        let head_h = 26.0;
        let input_y = WIN_H - 66.0;
        let list_bottom = input_y - 28.0;
        // 滚动：off 为首个可见行下标（行高不一，逐行绘制到列表底部为止）
        let off = self.agenda_scroll.min(rows.len().saturating_sub(1));
        let mut y = 58.0;
        let mut clipped = off > 0;
        for (ri, row) in rows.iter().enumerate() {
            if ri < off {
                continue;
            }
            match *row {
                AgRow::Head(d, is_today) => {
                    let wd = "日一二三四五六".chars().nth(d.weekday().num_days_from_sunday() as usize).unwrap();
                    let label = format!("{}-{:02}-{:02} 周{}{}", d.year(), d.month(), d.day(), wd, if is_today { " · 今天" } else { "" });
                    p.text(&label, 22.0, y, 260.0, head_h, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.0, true, false, if is_today { BLUE() } else { SUB() });
                    y += head_h + 2.0;
                }
                AgRow::Agenda(i) => {
                    if y + row_h > list_bottom {
                        clipped = true;
                        break;
                    }
                    let text = crate::events::display(&self.agenda_resolved[i].2);
                    self.draw_item_row(p, regions, y, row_h, "日程", BLUE(), &text, Action::AgendaEdit(i), Action::AgendaDel(i), None);
                    y += row_h + gap;
                }
                AgRow::Todo(i) => {
                    if y + row_h > list_bottom {
                        clipped = true;
                        break;
                    }
                    let (gi, t, _) = self.agenda_todos[i].clone();
                    let tm = if t.has_time { crate::events::fmt_time_str(t.start.as_deref().unwrap_or("")) } else { String::new() };
                    let text = if tm.is_empty() { t.text.clone() } else { format!("{} {}", tm, t.text) };
                    self.draw_item_row(p, regions, y, row_h, "待办", TODO_ORANGE(), &text, Action::TodoEdit(i), Action::TodoDel(i), Some((t.done, i)));
                    let _ = gi;
                    y += row_h + gap;
                }
            }
        }
        if rows.is_empty() {
            p.text("近 30 天没有日程或待办", 10.0, 130.0, WIN_W - 20.0, 20.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, SUB_DIM());
        } else if clipped {
            p.text("滚轮查看更多", 20.0, input_y - 22.0, WIN_W - 40.0, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 10.0, false, false, SUB_DIM());
        }

        // 底部：类型下拉（日程/待办）+ 输入 + 添加
        let pill = gdi::RectF { x: 28.0, y: WIN_H - 66.0, w: 78.0, h: 30.0 };
        Self::hit_add(regions, pill.x, pill.y, pill.w, pill.h, Action::KindDropdown);
        let hov = self.hovered(&Action::KindDropdown);
        p.fill_round(pill.x, pill.y, pill.w, pill.h, 8.0, if hov { gdi::argb(50, 62, 135, 250) } else { gdi::argb(28, 62, 135, 250) });
        p.stroke_round(pill.x, pill.y, pill.w, pill.h, 8.0, 1.0, gdi::argb(110, 62, 135, 250));
        let (kind_name, kind_col) = if self.draft_kind == 0 { ("日程", BLUE()) } else { ("待办", TODO_ORANGE()) };
        p.text(&format!("{} ▾", kind_name), pill.x, pill.y, pill.w, pill.h, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, kind_col);
        let input = gdi::RectF { x: pill.x + pill.w + 8.0, y: WIN_H - 66.0, w: WIN_W - 20.0 - 92.0 - (pill.x + pill.w + 8.0), h: 30.0 };
        Self::hit_add(regions, input.x, input.y, input.w, input.h, Action::InputBox);
        p.fill_round(input.x, input.y, input.w, input.h, 8.0, crate::theme::ov(18));
        p.stroke_round(input.x, input.y, input.w, input.h, 8.0, 1.0, crate::theme::ov(24));
        // 快捷输入：组合串显示在光标处；选区高亮 + 光标按位置绘制
        let (head, comp, tail) = self.draft_split();
        let shown_text = format!("{}{}{}", head, comp, tail);
        let hint_active = !self.hint.is_empty()
            && self.draft.is_empty()
            && self.hint_until.map(|t| std::time::Instant::now() < t).unwrap_or(false);
        if hint_active {
            let col = if self.hint_warn { TODO_ORANGE() } else { BLUE() };
            p.text(&self.hint, input.x + 10.0, input.y, input.w - 16.0, input.h, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.0, false, false, col);
        } else if shown_text.is_empty() {
            let ph = if self.draft_kind == 0 { "添加日程，如 14:00 项目评审" } else { "添加待办，如 明天 交报告" };
            p.text(ph, input.x + 10.0, input.y, input.w - 16.0, input.h, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, SUB_DIM());
            if self.caret_on {
                p.line(input.x + 10.0, input.y + 6.0, input.x + 10.0, input.y + input.h - 6.0, 1.0, ROW_TXT());
            }
        } else {
            // 选区高亮（IME 组合中不显示选区）
            if self.comp.is_empty() {
                let (sa, sb) = crate::textedit::sel_range(&self.draft, self.draft_caret, self.draft_sel);
                if sa < sb {
                    let chars: Vec<char> = self.draft.chars().collect();
                    let pre_a: String = chars[..sa].iter().collect();
                    let pre_b: String = chars[..sb].iter().collect();
                    let x1 = input.x + 10.0 + p.measure(&pre_a, 13.0, false, false).0;
                    let x2 = input.x + 10.0 + p.measure(&pre_b, 13.0, false, false).0;
                    p.fill_round(x1, input.y + 5.0, (x2 - x1).max(3.0), input.h - 10.0, 3.0, SEL_BLUE());
                }
            }
            p.text(&shown_text, input.x + 10.0, input.y, input.w - 16.0, input.h, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, ROW_TXT());
            if self.caret_on {
                let caret_x = input.x + 10.0 + p.measure(&format!("{}{}", head, comp), 13.0, false, false).0 + 1.0;
                p.line(caret_x, input.y + 6.0, caret_x, input.y + input.h - 6.0, 1.0, ROW_TXT());
            }
        }
        let btn = gdi::RectF { x: WIN_W - 20.0 - 84.0, y: WIN_H - 66.0, w: 84.0, h: 30.0 };
        Self::hit_add(regions, btn.x, btn.y, btn.w, btn.h, Action::AgendaAdd);
        let hov = self.hovered(&Action::AgendaAdd);
        p.fill_round(btn.x, btn.y, btn.w, btn.h, 8.0, if hov { gdi::argb(255, 0x53, 0x99, 0xFB) } else { BLUE() });
        p.text("添加", btn.x, btn.y, btn.w, btn.h, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 13.0, false, false, WHITE);
        // 类型下拉展开（向上弹）
        if self.kind_menu_open {
            let opt_h = 30.0;
            let list_h = opt_h * 2.0 + 8.0;
            let list_top = pill.y - list_h - 6.0;
            p.fill_round(pill.x - 1.0, list_top, pill.w + 2.0, list_h, 8.0, POPUP_BG());
            p.stroke_round(pill.x - 1.0, list_top, pill.w + 2.0, list_h, 8.0, 1.0, BORDER());
            for (i, name) in ["日程", "待办"].iter().enumerate() {
                let oy = list_top + 4.0 + opt_h * i as f32;
                Self::hit_add(regions, pill.x + 1.0, oy, pill.w - 2.0, opt_h, Action::KindPick(i));
                let hov2 = self.hovered(&Action::KindPick(i));
                if hov2 {
                    p.fill_round(pill.x + 2.0, oy, pill.w - 4.0, opt_h - 1.0, 6.0, gdi::argb(50, 62, 135, 250));
                }
                let sel = (i == 0) == (self.draft_kind == 0);
                let col = if i == 0 { BLUE() } else { TODO_ORANGE() };
                p.text(name, pill.x + 1.0, oy, pill.w - 2.0, opt_h, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, if sel { col } else { ROW_TXT() });
            }
        }
    }

    /// 日程页条目行：行体热区 + 勾选圈（待办）+ tag 胶囊 + 文本 + ✕ 删除热区
    /// （勾选/删除区域后绘制，命中优先）
    fn draw_item_row(&self, p: &Painter, regions: &mut Vec<(gdi::RectF, Action)>, y: f32, row_h: f32, tag: &str, tag_col: u32, text: &str, edit_act: Action, del_act: Action, check: Option<(bool, usize)>) {
        let rx = 20.0;
        let rw = WIN_W - 20.0 - 40.0;
        let row_hov = self.hovered(&edit_act);
        p.fill_round(rx, y, rw, row_h, 8.0, gdi::argb(if row_hov { 24 } else { 11 }, 255, 255, 255));
        Self::hit_add(regions, rx, y, rw, row_h, edit_act);
        let mut cx_off = 8.0;
        if let Some((done, ti)) = check {
            // 勾选圈（点击切换完成状态）
            let ccx = rx + 18.0;
            p.stroke_circle(ccx, y + row_h / 2.0, 6.0, 1.2, if done { BLUE() } else { SUB() });
            if done {
                p.fill_circle(ccx, y + row_h / 2.0, 3.5, BLUE());
            }
            Self::hit_add(regions, rx + 8.0, y, 20.0, row_h, Action::TodoToggle(ti));
            cx_off = 30.0;
        }
        let tag_x = rx + cx_off;
        let tag_w = 30.0;
        let tag_y = y + (row_h - 16.0) / 2.0;
        p.fill_round(tag_x, tag_y, tag_w, 16.0, 4.0, gdi::argb(34, (tag_col >> 16) as u8, (tag_col >> 8) as u8, tag_col as u8));
        p.text(tag, tag_x, tag_y, tag_w, 16.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 9.5, false, false, tag_col);
        p.text(text, tag_x + tag_w + 8.0, y, rx + rw - 46.0 - (tag_x + tag_w + 8.0), row_h, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, ROW_TXT());
        let del_x = rx + rw - 32.0;
        Self::hit_add(regions, del_x, y, 26.0, row_h, del_act);
        let del_hov = self.hovered(&del_act);
        p.text("✕", del_x, y, 26.0, row_h, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, if del_hov { RED() } else { WEEK_NUM() });
    }

}

/// 圆圈几何：包住数字与次行农历/节日文本（宽度按次行扩展，最大不超格子）。
/// 今天/选中/悬停三种圆圈共用，保证形状一致。
fn circle_geo(p: &Painter, rect: &gdi::RectF, sub: &str) -> (f32, f32) {
    let num_cy = rect.y + rect.h * 0.30;
    let line1_cy = rect.y + rect.h * 0.64;
    let line2_cy = line1_cy + 14.0;
    let (mut top, mut bottom, mut w_half): (f32, f32, f32) = (num_cy - 18.0, num_cy + 18.0, 18.0);
    let chars: Vec<char> = sub.chars().collect();
    if !chars.is_empty() {
        bottom = line1_cy + 9.0;
        let l1: String = if chars.len() <= 5 {
            sub.to_string()
        } else {
            chars[..5].iter().collect()
        };
        if chars.len() > 5 {
            let l2: String = chars[5..].iter().collect();
            bottom = line2_cy + 9.0;
            w_half = w_half.max(p.measure(&l2, 11.0, false, false).0 / 2.0 + 9.0);
        }
        w_half = w_half.max(p.measure(&l1, 11.0, false, false).0 / 2.0 + 9.0);
    }
    let cy = (top + bottom) / 2.0;
    let r = ((bottom - top) / 2.0 + 4.0).max(w_half).min(rect.w / 2.0 - 1.0);
    (cy, r)
}

fn paint_day_cell(
    p: &Painter,
    ui: &Ui,
    rect: &gdi::RectF,
    date: NaiveDate,
    today: NaiveDate,
    selected: NaiveDate,
    in_month: bool,
    hol: Option<&DayInfo>,
    has_agenda: bool,
    has_todo: bool,
    cfg: &Config,
) {
    let cx = rect.x + rect.w / 2.0;
    let num_cy = rect.y + rect.h * 0.30;
    let line1_cy = rect.y + rect.h * 0.64;
    let line2_cy = rect.y + rect.h * 0.64 + 14.0;
    let hovered = ui.hover.map(|h| matches!(h, Action::Cell(d) if d == date)).unwrap_or(false);

    let is_today = date == today;
    let is_selected = date == selected;
    let weekend = date.weekday().num_days_from_monday() >= 5;
    // 周末红色；本月工作日纯白（提高对比度）
    let mut num_col = if weekend { RED() } else { ON_BG() };
    if !in_month {
        num_col = SUB_DIM();
    }
    let num = date.day().to_string();

    // 次行内容（先计算：今天的圆圈要把农历/节日一并圈住）
    let mut sub = String::new();
    let mut sub_col = SUB();
    if cfg.show_lunar {
        let l = lunar::solar_to_lunar(date);
        let lf = l.as_ref().and_then(lunar::lunar_festival);
        let sfest = lunar::solar_festival(date);
        let term = lunar::jieqi_of(date);
        let legal = hol
            .filter(|h| h.ty == DayType::Xiu)
            .map(|h| h.name.as_str());

        let (s, c) = if let Some((n, k)) = lf {
            (n.to_string(), fest_color(k))
        } else if let Some(n) = legal {
            (n.to_string(), FEST_BLUE())
        } else if let Some((n, k)) = sfest {
            (n.to_string(), fest_color(k))
        } else if let Some(t) = term {
            (t.to_string(), FEST_BLUE())
        } else if let Some(l) = l {
            let txt = if l.day == 1 {
                format!("{}月", lunar::month_cn(l.month))
            } else {
                lunar::day_cn(l.day).to_string()
            };
            (txt, if in_month { SUB() } else { SUB_DIM() })
        } else {
            (String::new(), SUB())
        };
        sub = s;
        sub_col = c;
    }

    // 悬停：半透明圆圈垫底（选中/今天的格子悬停时同样有效）；
    // 今天：大蓝圈包住数字与次行；选中：同形状的空心蓝环
    if hovered && in_month {
        let (cy, r) = circle_geo(p, rect, &sub);
        p.fill_circle(cx, cy, r, CELL_HOVER());
    }
    if is_today {
        let (cy, r) = circle_geo(p, rect, &sub);
        p.fill_circle(cx, cy, r, BLUE());
    } else if is_selected {
        let (cy, r) = circle_geo(p, rect, &sub);
        p.stroke_circle(cx, cy, r - 2.25, 4.5, BLUE());
    }
    p.text(&num, cx - 22.0, num_cy - 16.0, 44.0, 32.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 18.0, true, false, if is_today { WHITE } else { num_col });

    // 次行：农历/节日/节气
    if !sub.is_empty() {
        let chars: Vec<char> = sub.chars().collect();
        let max_w = rect.w - 4.0;
        // 今天圆圈为蓝色，次行用白色保证可读
        let col = if is_today { WHITE } else { sub_col };
        if chars.len() <= 5 {
            // 5 字内自适应缩小字号完整显示（如“烈士纪念日”）
            let px = fit_text_px(p, &sub, max_w, 11.0, 9.0);
            p.text(&sub, rect.x + 1.0, line1_cy - 8.0, rect.w - 2.0, 16.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, px, false, false, col);
        } else {
            // 超过 5 字换行：第一行 5 字，其余第二行
            let line1: String = chars[..5].iter().collect();
            let line2: String = chars[5..].iter().collect();
            let px1 = fit_text_px(p, &line1, max_w, 11.0, 9.0);
            p.text(&line1, rect.x + 1.0, line1_cy - 8.0, rect.w - 2.0, 15.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, px1, false, false, col);
            let px2 = fit_text_px(p, &line2, max_w, 11.0, 9.0);
            p.text(&line2, rect.x + 1.0, line2_cy - 8.0, rect.w - 2.0, 15.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, px2, false, false, col);
        }
    }

    // 休/班角标（调休安排可关闭）
    if cfg.show_adjust {
        if let Some(h) = hol {
            let tag = if h.ty == DayType::Ban { "班" } else { "休" };
            let col = if h.ty == DayType::Ban { RED() } else { BLUE() };
            let bx = rect.x + rect.w - 12.0 - 6.5;
            let by = rect.y + 8.0 - 6.5;
            p.fill_round(bx, by, 13.0, 13.0, 3.0, col);
            p.text(tag, bx, by, 13.0, 13.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 9.0, false, false, WHITE);
        }
    }

    if has_agenda {
        p.fill_circle(rect.x + 7.0, rect.y + 7.0, 2.0, BLUE());
    }
    // 待办角标：橙点（与日程蓝点并排）
    if has_todo {
        let x = if has_agenda { rect.x + 14.0 } else { rect.x + 7.0 };
        p.fill_circle(x, rect.y + 7.0, 2.0, TODO_ORANGE());
    }
}

/// 自适应字号：从 base 逐步缩小到 min，使文本宽度不超过 max_w（日期格 5 字节日名完整显示）
fn fit_text_px(p: &Painter, s: &str, max_w: f32, base: f32, min: f32) -> f32 {
    let mut px = base;
    while px > min && p.measure(s, px, false, false).0 > max_w {
        px -= 0.5;
    }
    px
}

/// 日期格悬停预览面板：当天日程/待办摘要（最多 5 行 + 共 N 条）
fn paint_day_preview(
    p: &Painter,
    cell: &gdi::RectF,
    date: NaiveDate,
    items: &[(String, usize, crate::events::AgendaEntry)],
    todos: &[(usize, crate::sidebar::Todo)],
) {
    const PW: f32 = 210.0;
    let head_h = 22.0;
    let row_h = 17.0;
    let max_rows = 5usize;
    let n_total = items.len() + todos.len();
    let shown = n_total.min(max_rows);
    let body_rows = shown.max(if n_total == 0 { 1 } else { 0 });
    let foot = if n_total > max_rows { 14.0 } else { 0.0 };
    let ph = 6.0 + head_h + 4.0 + body_rows as f32 * row_h + foot + 6.0;
    // 面板放在格子右侧优先，放不下放左侧；垂直贴格子顶部并收进面板区
    let mut x = cell.x + cell.w + 4.0;
    if x + PW > WIN_W - 14.0 {
        x = cell.x - PW - 4.0;
    }
    x = x.max(14.0);
    let mut y = cell.y;
    if y + ph > WIN_H - 66.0 {
        y = WIN_H - 66.0 - ph;
    }
    y = y.max(112.0);
    p.fill_round(x, y, PW, ph, 8.0, POPUP_BG());
    p.stroke_round(x, y, PW, ph, 8.0, 1.0, BORDER());
    let wd = "日一二三四五六"
        .chars()
        .nth(date.weekday().num_days_from_sunday() as usize)
        .unwrap();
    p.text(
        &format!("{}月{}日 周{}", date.month(), date.day(), wd),
        x + 10.0,
        y + 4.0,
        PW - 20.0,
        head_h,
        gdi::HALIGN_NEAR,
        gdi::HALIGN_CENTER,
        12.0,
        true,
        false,
        TITLE_COL(),
    );
    let mut ry = y + 6.0 + head_h;
    if n_total == 0 {
        p.text("当天没有日程或待办", x + 10.0, ry, PW - 20.0, row_h, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 11.0, false, false, SUB_DIM());
        return;
    }
    let mut drawn = 0usize;
    for (_, _, e) in items {
        if drawn >= max_rows {
            break;
        }
        p.fill_circle(x + 14.0, ry + 8.0, 2.0, BLUE());
        p.text(&crate::events::display(e), x + 22.0, ry, PW - 32.0, row_h, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 11.0, false, false, ROW_TXT());
        ry += row_h;
        drawn += 1;
    }
    for (_, t) in todos {
        if drawn >= max_rows {
            break;
        }
        p.fill_circle(x + 14.0, ry + 8.0, 2.0, TODO_ORANGE());
        let label = if t.done { format!("✓ {}", t.text) } else { t.text.clone() };
        p.text(&label, x + 22.0, ry, PW - 32.0, row_h, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 11.0, false, false, if t.done { SUB_DIM() } else { ROW_TXT() });
        ry += row_h;
        drawn += 1;
    }
    if foot > 0.0 {
        p.text(&format!("共 {} 条", n_total), x + 10.0, ry, PW - 20.0, foot, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 10.0, false, false, SUB_DIM());
    }
}

/// 节日名称统一鲜艳蓝色（不再区分红/蓝）
fn fest_color(_k: FestKind) -> u32 {
    FEST_BLUE()
}

fn draw_weather(p: &Painter, cx: f32, cy: f32, code: u32, sc: f32) {
    match code {
        0 => draw_sun(p, cx, cy, sc),
        1..=2 => {
            draw_sun(p, cx - 6.0 * sc, cy - 7.0 * sc, 0.65 * sc);
            draw_cloud(p, cx + 3.0 * sc, cy + 4.0 * sc, sc);
        }
        3 => draw_cloud(p, cx, cy, 1.2 * sc),
        45 | 48 => {
            draw_cloud(p, cx, cy - 4.0 * sc, sc);
            for i in 0..2 {
                p.line(cx - 9.0 * sc, cy + (4.0 + i as f32 * 5.0) * sc, cx + 9.0 * sc, cy + (4.0 + i as f32 * 5.0) * sc, 2.0, gdi::argb(180, 0xE8, 0xEC, 0xF2));
            }
        }
        51..=67 | 80..=82 => {
            draw_cloud(p, cx, cy - 5.0 * sc, 1.05 * sc);
            for i in -1..=1 {
                p.line(cx + i as f32 * 7.0 * sc, cy + 6.0 * sc, cx + i as f32 * 7.0 * sc - 2.0 * sc, cy + 12.0 * sc, 2.0, RAIN());
            }
        }
        71..=77 | 85 | 86 => {
            draw_cloud(p, cx, cy - 5.0 * sc, 1.05 * sc);
            for i in -1..=1 {
                p.fill_circle(cx + i as f32 * 7.0 * sc, cy + 9.0 * sc, 1.8 * sc, CLOUD());
            }
        }
        c if c >= 95 => {
            draw_cloud(p, cx, cy - 5.0 * sc, 1.05 * sc);
            p.fill_polygon(
                &[
                    (cx + 1.0 * sc, cy + 4.0 * sc),
                    (cx - 4.0 * sc, cy + 11.0 * sc),
                    (cx + 0.0 * sc, cy + 11.0 * sc),
                    (cx - 2.0 * sc, cy + 17.0 * sc),
                    (cx + 4.0 * sc, cy + 9.0 * sc),
                    (cx + 0.0 * sc, cy + 9.0 * sc),
                    (cx + 3.0 * sc, cy + 4.0 * sc),
                ],
                SUN(),
            );
        }
        _ => {
            draw_sun(p, cx - 6.0 * sc, cy - 7.0 * sc, 0.65 * sc);
            draw_cloud(p, cx + 3.0 * sc, cy + 4.0 * sc, sc);
        }
    }
}

fn draw_sun(p: &Painter, cx: f32, cy: f32, s: f32) {
    let r = 7.0 * p.sf * s;
    p.fill_circle(cx, cy, 7.0 * s, SUN());
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::TAU / 8.0;
        let (dx, dy) = (a.sin(), -a.cos());
        p.line(cx + dx * (r / p.sf + 2.5), cy + dy * (r / p.sf + 2.5), cx + dx * (r / p.sf + 5.5), cy + dy * (r / p.sf + 5.5), 2.0 * s, SUN());
    }
}

fn draw_cloud(p: &Painter, cx: f32, cy: f32, s: f32) {
    p.fill_circle(cx - 6.0 * s, cy + 1.0 * s, 6.0 * s, CLOUD());
    p.fill_circle(cx + 1.0 * s, cy - 2.0 * s, 7.5 * s, CLOUD());
    p.fill_circle(cx + 7.0 * s, cy + 2.0 * s, 5.5 * s, CLOUD());
    p.fill_round(cx - 10.0 * s, cy + 2.0 * s, 21.0 * s, 5.0 * s, 3.5 * s, CLOUD());
}

fn save_bmp(scan0: *const u8, w: i32, h: i32, path: &str) {
    let stride = (w as usize) * 4;
    let data_size = stride * h as usize;
    let file_size = 54 + data_size;
    let mut buf = Vec::with_capacity(file_size);
    buf.extend_from_slice(b"BM");
    buf.extend_from_slice(&(file_size as u32).to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&54u32.to_le_bytes());
    buf.extend_from_slice(&40u32.to_le_bytes());
    buf.extend_from_slice(&w.to_le_bytes());
    buf.extend_from_slice(&h.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&32u16.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&(data_size as u32).to_le_bytes());
    buf.extend_from_slice(&2835u32.to_le_bytes());
    buf.extend_from_slice(&2835u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    unsafe {
        for row in (0..h as usize).rev() {
            let src = std::slice::from_raw_parts(scan0.add(row * stride), stride);
            buf.extend_from_slice(src);
        }
    }
    let _ = std::fs::write(path, buf);
}

pub fn load_agenda() -> crate::events::AgendaMap {
    crate::events::load()
}

fn save_agenda(map: &crate::events::AgendaMap) {
    crate::events::save(map);
}

// ================= 交互 =================
impl Ui {
    /// 设置快捷输入结果提示（5 秒后自动消失）
    fn set_hint(&mut self, text: &str, warn: bool) {
        self.hint = text.to_string();
        self.hint_warn = warn;
        self.hint_until = Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
    }

    /// 输入新内容时清除提示
    fn clear_hint(&mut self) {
        self.hint.clear();
        self.hint_until = None;
    }

    /// IME 组合串显示在光标处（返回 组合前文本/组合串/组合后文本）
    fn draft_split(&self) -> (String, String, String) {
        let chars: Vec<char> = self.draft.chars().collect();
        let pos = self.draft_caret.min(chars.len());
        (
            chars[..pos].iter().collect(),
            self.comp.clone(),
            chars[pos..].iter().collect(),
        )
    }

    /// 光标落在宽度 x 附近的字符下标（近似：CJK 13px / ASCII 6.8px，13px 字号）
    fn draft_caret_at_x(&mut self, w: f32) {
        let chars: Vec<char> = self.draft.chars().collect();
        let mut acc = 0.0f32;
        let mut pos = 0usize;
        for c in &chars {
            let cw = if c.is_ascii() { 6.8 } else { 13.0 };
            if acc + cw / 2.0 > w {
                break;
            }
            acc += cw;
            pos += 1;
        }
        self.draft_caret = pos;
        self.draft_sel = None;
    }

    /// 当前页的日程解析行（编辑/删除/撤销定位）：(key, idx, 条目, 出现日期)
    fn resolved_row(&self, i: usize) -> Option<(String, usize, crate::events::AgendaEntry, NaiveDate)> {
        self.agenda_resolved
            .get(i)
            .map(|(k, x, e)| (k.clone(), *x, e.clone(), self.agenda_resolved_dates.get(i).copied().unwrap_or(self.selected)))
    }

    /// 当前页的待办行：(全局下标, 待办, 出现日期)
    fn todo_row(&self, i: usize) -> Option<(usize, crate::sidebar::Todo, NaiveDate)> {
        self.agenda_todos.get(i).cloned()
    }

    fn handle_action(&mut self, action: &Action) {
        // 视图/翻页一律清掉悬停预览
        let clear_preview = |ui: &mut Ui| {
            ui.preview = None;
            ui.hover_cell = None;
            ui.hover_since = None;
        };

        match action {

            Action::Prev => {
                self.view_m -= 1;
                if self.view_m < 1 {
                    self.view_m = 12;
                    self.view_y -= 1;
                }
                self.redraw();
            }
            Action::Next => {
                self.view_m += 1;
                if self.view_m > 12 {
                    self.view_m = 1;
                    self.view_y += 1;
                }
                self.redraw();
            },
            Action::Title | Action::BottomToday => {
                let t = Local::now().date_naive();
                self.view_y = t.year();
                self.view_m = t.month();
                self.selected = t;
                self.page = Page::Calendar;
                clear_preview(self);
                self.redraw();
            }
            Action::BottomAgenda => {
                self.page = Page::Agenda;
                self.agenda_scroll = 0;
                clear_preview(self);
                self.redraw();
            }
            Action::Cell(d) => {
                self.selected = *d;
                clear_preview(self);
                // 点击日期：打开/切换日期侧边栏；再次点击同一日期收起
                if crate::sidebar::sidebar_visible() && crate::sidebar::sidebar_date() == Some(*d) {
                    crate::sidebar::sidebar_hide();
                } else {
                    crate::sidebar::sidebar_show(*d);
                }
                self.redraw();
            }
            Action::BottomPlus => {
                // 与右键日期“新增日程”一致：打开新建弹窗；
                // 窗口操作须在 UI 锁之外执行（见 WM_LBUTTONDOWN），此处不处理
            }
            Action::BottomExit => {
                unsafe {
                    DestroyWindow(self.hwnd as HWND);
                }
            }
            Action::Back => {
                self.page = Page::Calendar;
                clear_preview(self);
                self.redraw();
            }
            Action::AgendaDel(i) => {
                // 用解析行的原 key/下标定位（重复日程行也指向主条目）
                if let Some((key, idx, entry, view_date)) = self.resolved_row(*i) {
                    let is_recur = matches!(&entry, crate::events::AgendaEntry::Rich(r) if r.recur.is_some());
                    if is_recur {
                        // 重复日程：弹“仅这一天/整个系列”选择菜单（菜单窗口不碰 UI 锁，锁内调用安全）
                        let mut pt = POINT { x: 0, y: 0 };
                        unsafe { GetCursorPos(&mut pt) };
                        crate::recur_menu::open(pt.x, pt.y, crate::recur_menu::RmTarget::Agenda { key, idx, date: view_date });
                    } else {
                        let body = crate::events::display(&entry);
                        let mut a = self.agenda.lock().unwrap();
                        let removed = crate::events::agenda_remove_at(&mut a, &key, idx);
                        drop(a);
                        if removed {
                            save_agenda(&self.agenda.lock().unwrap());
                            crate::toast::notify_undo(crate::toast::UndoData::Agenda { key, idx, entry }, &body);
                        }
                        crate::sidebar::sidebar_repaint();
                        self.redraw();
                    }
                }
            }
            Action::TodoDel(i) => {
                // 用缓存的待办下标定位；重复待办弹“仅这一天/整个系列”选择菜单（出现日取分组日期）
                if let Some((gi, td, d)) = self.todo_row(*i) {
                    if td.recur.is_some() {
                        let mut pt = POINT { x: 0, y: 0 };
                        unsafe { GetCursorPos(&mut pt) };
                        crate::recur_menu::open(pt.x, pt.y, crate::recur_menu::RmTarget::Todo { gi, date: d });
                    } else {
                        // UI 锁内不能走 remove_todo_at（其内部 flyout_repaint 会重入本锁死锁）
                        let body = if td.has_time {
                            format!("{} {}", crate::events::fmt_time_str(td.start.as_deref().unwrap_or("")), td.text)
                        } else {
                            td.text.clone()
                        };
                        crate::sidebar::remove_todo_at_quiet(gi);
                        crate::toast::notify_undo(crate::toast::UndoData::Todos { items: vec![(gi, td)] }, &body);
                        crate::sidebar::sidebar_repaint();
                        self.redraw();
                    }
                }
            }
            // 勾选切换在 WM_LBUTTONDOWN 锁外处理（toggle_todo_on 内部会重入 UI 锁）
            Action::TodoToggle(_) => {}
            Action::AgendaAdd => {
                self.add_agenda();
            }
            Action::KindDropdown => {
                self.kind_menu_open = !self.kind_menu_open;
                self.redraw();
            }
            Action::KindPick(i) => {
                self.draft_kind = *i as u8;
                self.kind_menu_open = false;
                self.redraw();
            }
            Action::OpenSettings | Action::BottomSettings => {
                // 窗口操作必须在 UI 锁之外执行（见 WM_LBUTTONDOWN）：
                // ShowWindow 激活设置窗口会同步触发本面板 WM_ACTIVATE 重入加锁 → 死锁
            }
            Action::None => {}
            // 行点击在 WM_LBUTTONDOWN 锁外打开编辑弹窗，此处不处理
            Action::AgendaEdit(_) => {}
            Action::TodoEdit(_) => {}
            // 软件设置 / 日期与时间 在 WM_LBUTTONDOWN 锁外处理
            Action::InputBox | Action::Weather => {}
        }
    }

    fn add_agenda(&mut self) {
        let text = self.draft.trim().to_string();
        if text.is_empty() {
            return;
        }
        // “14:30 项目评审”“明天9点开会”“周五 18:30 团建”这类输入 → 解析日期词/时间/区间；
        // 类型下拉选中“待办”时生成待办（未分类优先级），否则生成日程（无日期词落在今天）
        let base = Local::now().date_naive();
        if let Some((day, sm, em, name)) = crate::events::parse_quick_time(&text, base) {
            let day = day.unwrap_or(base);
            if self.draft_kind == 1 {
                // 快捷待办：带时间记 start，无时间为全天待办
                let td = crate::sidebar::Todo {
                    id: crate::events::gen_id(),
                    text: name.clone(),
                    done: false,
                    date: Some(crate::ics::key_of_date(day)),
                    priority: 0,
                    has_time: sm.is_some(),
                    start: sm.and_then(|sm| day.and_hms_opt(sm / 60, sm % 60, 0)).map(crate::events::fmt_dt_store),
                    end: None,
                    remind: None,
                    repeat: None,
                    recur: None,
                    recur_until: None,
                    skip_dates: Vec::new(),
                    done_dates: Vec::new(),
                };
                crate::sidebar::add_todo_quiet(td);
                crate::sidebar::sidebar_repaint();
                self.draft.clear();
                self.comp.clear();
                self.draft_caret = 0;
                self.draft_sel = None;
                self.set_hint(&format!("已添加待办：{}", name), false);
                self.redraw();
                return;
            }
            let entry = match sm {
                Some(sm) => {
                    let em = em.unwrap_or(sm + 60).min(24 * 60 - 1);
                    match (day.and_hms_opt(sm / 60, sm % 60, 0), day.and_hms_opt(em / 60, em % 60, 0)) {
                        (Some(s), Some(e)) => Some(crate::events::AgendaEntry::Rich(crate::events::RichEvent {
                            id: crate::events::gen_id(),
                            name: name.clone(),
                            all_day: false,
                            start: crate::events::fmt_dt_store(s),
                            end: crate::events::fmt_dt_store(e),
                            remind: None,
                            repeat: None,
                            recur: None,
                            recur_until: None,
                            skip_dates: Vec::new(),
                        })),
                        _ => None,
                    }
                }
                None => Some(crate::events::AgendaEntry::Rich(crate::events::RichEvent {
                    id: crate::events::gen_id(),
                    name: name.clone(),
                    all_day: true,
                    start: crate::events::fmt_d_store(day),
                    end: crate::events::fmt_d_store(day),
                    remind: None,
                    repeat: None,
                    recur: None,
                    recur_until: None,
                    skip_dates: Vec::new(),
                })),
            };
            if let Some(entry) = entry {
                let key = crate::ics::key_of_date(day);
                self.agenda.lock().unwrap().entry(key).or_default().push(entry);
                save_agenda(&self.agenda.lock().unwrap());
                crate::sidebar::sidebar_repaint();
                self.draft.clear();
                self.comp.clear();
                self.draft_caret = 0;
                self.draft_sel = None;
                self.set_hint(&format!("已添加日程：{}", name), false);
                self.redraw();
                return;
            }
        }
        // 兜底：待办类型下纯文本直接生成今天待办；日程类型 → 旧纯文本条目。
        // 解析不出日期/时间必须给显式反馈（此前是静默降级，用户不知道时间丢了）
        if self.draft_kind == 1 {
            let td = crate::sidebar::Todo {
                id: crate::events::gen_id(),
                text,
                done: false,
                date: Some(crate::ics::key_of_date(base)),
                priority: 0,
                has_time: false,
                start: None,
                end: None,
                remind: None,
                repeat: None,
                recur: None,
                recur_until: None,
                skip_dates: Vec::new(),
                done_dates: Vec::new(),
            };
            crate::sidebar::add_todo_quiet(td);
            crate::sidebar::sidebar_repaint();
            self.set_hint("未识别日期/时间，已添加到今天的待办", true);
        } else {
            let key = crate::ics::key_of_date(base);
            self.agenda.lock().unwrap().entry(key).or_default().push(crate::events::AgendaEntry::Legacy(text));
            save_agenda(&self.agenda.lock().unwrap());
            crate::sidebar::sidebar_repaint();
            self.set_hint("未识别日期/时间，已存为今日纯文本日程", true);
        }
        self.draft.clear();
        self.comp.clear();
        self.draft_caret = 0;
        self.draft_sel = None;
        self.redraw();
    }

    fn action_at(&self, x: f32, y: f32) -> Option<Action> {
        // 逆序：后绘制的（弹窗/菜单等顶层）优先命中
        for (r, a) in self.regions.iter().rev() {
            if x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h {
                return Some(*a);
            }
        }
        None
    }
}

// ================= 窗口过程 =================
unsafe fn create_dib(ui: &mut Ui) {
    let (mem_dc, hbmp, bmp, scan0) = gdi::alloc_dib(ui.w, ui.h);
    ui.mem_dc = mem_dc;
    ui.hbmp = hbmp;
    ui.bmp = bmp;
    ui.scan0 = scan0;
}

/// 释放后台位图（隐藏时调用压缩内存；下次 redraw 自动重建）
fn free_surface(ui: &mut Ui) {
    unsafe {
        gdi::free_dib(&mut ui.mem_dc, &mut ui.hbmp, &mut ui.bmp, &mut ui.g, &mut ui.scan0);
    }
}

#[link(name = "gdiplus")]
extern "system" {
    fn GdipCreateBitmapFromScan0(w: i32, h: i32, stride: i32, format: i32, scan0: *mut u8, bitmap: *mut gdi::Gp) -> i32;
    fn GdipGetImageGraphicsContext(image: gdi::Gp, graphics: *mut gdi::Gp) -> i32;
    fn GdipSetSmoothingMode(graphics: gdi::Gp, mode: i32) -> i32;
    fn GdipSetTextRenderingHint(graphics: gdi::Gp, mode: i32) -> i32;
    fn GdipDeleteGraphics(graphics: gdi::Gp) -> i32;
}

fn x_of(lp: LPARAM) -> i32 {
    ((lp as usize) & 0xFFFF) as u16 as i16 as i32
}

fn y_of(lp: LPARAM) -> i32 {
    (((lp as usize) >> 16) as u16 as i16) as i32
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            ValidateRect(hwnd, std::ptr::null_mut());
            0
        }
        WM_ERASEBKGND => 1,
        // 系统主题切换广播：跟随系统模式失效缓存并重绘可见窗口
        WM_SETTINGCHANGE => {
            if crate::theme::mode() == crate::theme::MODE_SYSTEM {
                crate::theme::invalidate_system();
                flyout_repaint();
                crate::sidebar::sidebar_repaint();
                settings_repaint_if_visible();
            }
            0
        }
        WM_TIMER => {
            let mut want_show = false;
            let mut want_hide = false;
            let mut quit = false;
            let mut idle_retune = false;
            let mut new_interval: u32 = 250;
            let mut fc_repaint = false;
            {
                let mut guard = UI.lock().unwrap();
                if let Some(sui) = guard.as_mut() {
                    let ui = &mut sui.0;
                    ui.tick += 1;
                    for id in tray::poll_menu_events() {
                        match id.as_str() {
                            "show" => want_show = true,
                            "autostart" => {
                                let on = ui.st.config.lock().unwrap().autostart;
                                {
                                    let mut cfg = ui.st.config.lock().unwrap();
                                    cfg.autostart = !on;
                                    cfg.save();
                                }
                                apply_autostart(!on);
                                if let Some(t) = ui.tray.lock().unwrap().as_ref() {
                                    t.autostart_item.set_checked(!on);
                                }
                            }
                            "refresh" => {
                                let _ = ui.st.refresh_tx.send(());
                            }
                            "quit" => quit = true,
                            _ => {}
                        }
                    }
                    if tray::poll_icon_clicks() > 0 {
                        if ui.shown {
                            want_hide = true;
                        } else {
                            want_show = true;
                        }
                    }
                    if ui.shown {
                        let sec = Local::now().second();
                        if sec != ui.last_second {
                            ui.last_second = sec;
                            if ui.page == Page::Calendar {
                                ui.redraw();
                            }
                        }
                        if ui.page == Page::Agenda && ui.tick % 2 == 0 {
                            ui.caret_on = !ui.caret_on;
                            // 快捷输入提示到期清除
                            if ui.hint_until.map(|t| std::time::Instant::now() >= t).unwrap_or(false) {
                                ui.hint.clear();
                                ui.hint_until = None;
                            }
                            ui.redraw();
                        }
                        // 近一周天气面板：随秒重绘（每小时更新的数据及时反映）
                        if forecast_visible() {
                            fc_repaint = true;
                        }
                    } else if !settings_visible() {
                        // 空闲防涨：回到空闲立即修剪，之后每 10 秒一次
                        if !ui.idle_timer {
                            ui.idle_timer = true;
                            idle_retune = true;
                            new_interval = 500;
                            crate::trim_working_set();
                        } else if ui.tick % 40 == 0 {
                            crate::trim_working_set();
                        }
                        // 空闲时降低定时器频率，减少代码页回填
                    } else if ui.idle_timer {
                        ui.idle_timer = false;
                        idle_retune = true;
                        new_interval = 250;
                    }
                }
            }
            if want_show {
                perform_show(hwnd);
            }
            if want_hide {
                perform_hide(hwnd);
            }
            if fc_repaint {
                forecast_redraw();
            }
            if quit {
                DestroyWindow(hwnd);
            }
            if idle_retune {
                SetTimer(hwnd, 1, new_interval, None);
            }
            // 屏幕缩放轮询（PMv2）：真实 DPI 与 sf 不同 → 全局重缩放（锁外、内部自行加锁）
            poll_scale_change();
            0
        }
        WM_RBUTTONDOWN => {
            // 右键日期格：打开“新增日程 / 新增待办”菜单
            let hit = {
                let mut guard = UI.lock().unwrap();
                if let Some(sui) = guard.as_mut() {
                    let ui = &mut sui.0;
                    if ui.shown {
                        let x = ((lp & 0xFFFF) as u16 as i16) as f32 / ui.sf;
                        let y = (((lp as usize) >> 16) as u16 as i16) as f32 / ui.sf;
                        ui.action_at(x, y)
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some(Action::Cell(d)) = hit {
                // 菜单需要屏幕坐标：客户区坐标 + 窗口原点
                let mut r: RECT = std::mem::zeroed();
                GetWindowRect(hwnd, &mut r);
                crate::ctxmenu::ctxmenu_date::date_menu_toggle(r.left + x_of(lp), r.top + y_of(lp), d);
            }
            0
        }
        WM_LBUTTONDOWN => {
            let mut hit: Option<Action> = None;
            {
                let mut guard = UI.lock().unwrap();
                if let Some(sui) = guard.as_mut() {
                    let ui = &mut sui.0;
                    let x = ((lp & 0xFFFF) as u16 as i16) as f32 / ui.sf;
                    let y = (((lp as usize) >> 16) as u16 as i16) as f32 / ui.sf;
                    hit = ui.action_at(x, y);
                    // 任意点击先收起悬停预览
                    ui.preview = None;
                    ui.hover_cell = None;
                    ui.hover_since = None;
                    // 快捷输入框点击：把光标放到点击位置附近
                    if hit == Some(Action::InputBox) {
                        ui.clear_hint();
                        ui.draft_caret_at_x(x - 114.0 - 10.0);
                        ui.redraw();
                    }
                    // 类型下拉展开中：仅选项/控件本身响应，点其他位置只收起
                    if ui.kind_menu_open {
                        match hit {
                            Some(Action::KindPick(i)) => ui.handle_action(&Action::KindPick(i)),
                            _ => {
                                ui.kind_menu_open = false;
                                ui.redraw();
                            }
                        }
                        hit = None;
                    }
                }
            }
            if let Some(a) = hit {
                let mut exit = false;
                let mut open_settings = false;
                let mut open_input: Option<NaiveDate> = None;
                let mut edit_agenda: Option<(String, usize, crate::events::AgendaEntry, NaiveDate)> = None;
                let mut edit_todo: Option<(usize, crate::sidebar::Todo, NaiveDate)> = None;
                let mut toggle_todo: Option<(usize, String)> = None;
                {
                    let mut guard = UI.lock().unwrap();
                    if let Some(sui) = guard.as_mut() {
                        let ui = &mut sui.0;
                        if matches!(a, Action::BottomExit) {
                            exit = true;
                        } else if matches!(a, Action::OpenSettings | Action::BottomSettings) {
                            // 设置窗口为 NOACTIVATE：不抢焦点，面板保持打开
                            ui.page = Page::Calendar;
                            ui.hover = None;
                            ui.redraw();
                            open_settings = true;
                        } else if matches!(a, Action::BottomPlus) {
                            // “+”打开新增日程弹窗（与右键日期一致），日期取当前选中
                            ui.hover = None;
                            open_input = Some(ui.selected);
                            ui.redraw();
                        } else if let Action::TodoToggle(i) = a {
                            // 勾选切换在锁外执行（toggle_todo_on 内部 flyout_repaint 会重入本锁）
                            if let Some((gi, _, d)) = ui.todo_row(i) {
                                toggle_todo = Some((gi, crate::ics::key_of_date(d)));
                            }
                        } else if let Action::AgendaEdit(i) = a {
                            // 日程条目点击 → 编辑弹窗（锁外打开；行数据与出现日取自本次绘制的解析缓存）
                            let target = ui.resolved_row(i);
                            if target.is_some() {
                                ui.hover = None;
                                ui.redraw();
                                edit_agenda = target;
                            }
                        } else if let Action::TodoEdit(i) = a {
                            // 待办条目点击 → 编辑弹窗（锁外打开；数据与出现日取自本次绘制的待办缓存）
                            let target = ui.todo_row(i);
                            if target.is_some() {
                                ui.hover = None;
                                ui.redraw();
                                edit_todo = target;
                            }
                        } else {
                            ui.handle_action(&a);
                        }
                    }
                }
                if let Some((gi, key)) = toggle_todo {
                    crate::sidebar::toggle_todo_on(gi, &key);
                    crate::sidebar::sidebar_repaint();
                }
                if exit {
                    DestroyWindow(hwnd);
                }
                if open_settings {
                    // 锁外执行窗口操作（防消息重入死锁）；打开设置侧窗时收起天气面板
                    forecast_close();
                    crate::sidebar::sidebar_hide();
                    show_settings();
                }
                if let Some(date) = open_input {
                    // 锁外执行窗口操作：inputbox::open 内 SetForegroundWindow 会同步
                    // 触发本面板 WM_ACTIVATE 重入加锁 → 死锁；锚点取面板所在屏幕
                    let mut r: RECT = std::mem::zeroed();
                    GetWindowRect(hwnd, &mut r);
                    crate::inputbox::open(r.left, r.top, date, crate::inputbox::Kind::Agenda);
                }
                if let Some((key, idx, entry, view_date)) = edit_agenda {
                    // 锁外打开编辑弹窗；条目来自点击时缓存的解析行
                    let mut r: RECT = std::mem::zeroed();
                    GetWindowRect(hwnd, &mut r);
                    match entry {
                        crate::events::AgendaEntry::Rich(ev) => {
                            crate::inputbox::open_agenda_edit(r.left, r.top, &key, idx, &ev, Some(view_date));
                        }
                        crate::events::AgendaEntry::Legacy(text) => {
                            // 旧纯文本条目：转为富条目编辑（保存后替换）
                            let ev = crate::events::RichEvent {
                                id: String::new(),
                                name: text,
                                all_day: false,
                                start: format!("{} 09:00", key),
                                end: format!("{} 10:00", key),
                                remind: None,
                                repeat: None,
                                recur: None,
                                recur_until: None,
                                skip_dates: Vec::new(),
                            };
                            crate::inputbox::open_agenda_edit(r.left, r.top, &key, idx, &ev, Some(view_date));
                        }
                    }
                }
                if let Some((gi, td, view_date)) = edit_todo {
                    // 锁外打开编辑弹窗；重复待办的编辑定位需要出现日
                    let mut r: RECT = std::mem::zeroed();
                    GetWindowRect(hwnd, &mut r);
                    let occ = if td.recur.is_some() { Some(view_date) } else { None };
                    crate::inputbox::open_todo_edit(r.left, r.top, gi, &td, occ);
                }
            }
            0
        }
        WM_MOUSEMOVE => {
            let mut fc_open = false;
            {
                let mut guard = UI.lock().unwrap();
                if let Some(sui) = guard.as_mut() {
                    let ui = &mut sui.0;
                    if ui.shown {
                        let x = ((lp & 0xFFFF) as u16 as i16) as f32 / ui.sf;
                        let y = (((lp as usize) >> 16) as u16 as i16) as f32 / ui.sf;
                        let a = ui.action_at(x, y);
                        if a != ui.hover {
                            ui.hover = a;
                            ui.redraw();
                            let clickable = matches!(a, Some(Action::Cell(_)) | Some(Action::Prev) | Some(Action::Next) | Some(Action::Title) | Some(Action::BottomAgenda) | Some(Action::BottomToday) | Some(Action::BottomPlus) | Some(Action::BottomSettings) | Some(Action::BottomExit) | Some(Action::AgendaDel(_)) | Some(Action::AgendaEdit(_)) | Some(Action::TodoDel(_)) | Some(Action::TodoEdit(_)) | Some(Action::TodoToggle(_)) | Some(Action::AgendaAdd) | Some(Action::KindDropdown) | Some(Action::KindPick(_)) | Some(Action::OpenSettings) | Some(Action::Back));
                            SetCursor(if clickable { LoadCursorW(std::ptr::null_mut(), IDC_HAND) } else { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) });
                        }
                        fc_open = a == Some(Action::Weather);
                        // 悬停预览：日历页停在同一日期格 400ms 后显示当天日程
                        match a {
                            Some(Action::Cell(d)) if ui.page == Page::Calendar => {
                                let now = std::time::Instant::now();
                                if ui.hover_cell != Some(d) {
                                    ui.hover_cell = Some(d);
                                    ui.hover_since = Some(now);
                                    if ui.preview.take().is_some() {
                                        ui.redraw();
                                    }
                                } else if ui.preview.is_none()
                                    && ui.hover_since.map(|t| now.duration_since(t) >= std::time::Duration::from_millis(400)).unwrap_or(false)
                                {
                                    ui.preview = Some(d);
                                    ui.redraw();
                                }
                            }
                            _ => {
                                if ui.preview.take().is_some() {
                                    ui.hover_cell = None;
                                    ui.hover_since = None;
                                    ui.redraw();
                                }
                            }
                        }
                        // 注册离开跟踪：鼠标移出后清悬停高亮/预览（防残留）
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        TrackMouseEvent(&mut tme);
                    }
                }
            }
            // 窗口操作在 UI 锁之外执行（防消息重入死锁）；面板常驻，失焦不关闭
            if fc_open {
                forecast_open(hwnd as usize);
            }
            0
        }
        WM_MOUSELEAVE => {
            let mut repaint = false;
            {
                let mut guard = UI.lock().unwrap();
                if let Some(sui) = guard.as_mut() {
                    let ui = &mut sui.0;
                    if ui.hover.is_some() || ui.preview.is_some() || ui.hover_cell.is_some() {
                        ui.hover = None;
                        ui.preview = None;
                        ui.hover_cell = None;
                        ui.hover_since = None;
                        repaint = ui.shown;
                    }
                }
            }
            if repaint {
                flyout_repaint();
            }
            0
        }
        WM_KEYDOWN => {
            let mut hide = false;
            {
                let mut guard = UI.lock().unwrap();
                if let Some(sui) = guard.as_mut() {
                    let ui = &mut sui.0;
                    let shift = (GetKeyState(0x10) as u16) & 0x8000 != 0;
                    let ctrl = (GetKeyState(0x11) as u16) & 0x8000 != 0;
                    match wp as i32 {
                        0x1B => {
                            if ui.page != Page::Calendar {
                                ui.page = Page::Calendar;
                                ui.redraw();
                            } else {
                                ui.shown = false;
                                free_surface(ui);
                                hide = true;
                            }
                        }
                        0x08 => {
                            if ui.page == Page::Agenda {
                                crate::textedit::backspace(&mut ui.draft, &mut ui.draft_caret, &mut ui.draft_sel);
                                ui.clear_hint();
                                ui.redraw();
                            }
                        }
                        0x2E => {
                            // Delete：删除光标后一个字符/选区（快捷输入）
                            if ui.page == Page::Agenda {
                                crate::textedit::delete_fwd(&mut ui.draft, &mut ui.draft_caret, &mut ui.draft_sel);
                                ui.clear_hint();
                                ui.redraw();
                            }
                        }
                        0x56 => {
                            if ctrl && ui.page == Page::Agenda {
                                paste_clipboard(ui);
                            }
                        }
                        0x41 => {
                            // Ctrl+A 全选（快捷输入）
                            if ctrl && ui.page == Page::Agenda {
                                crate::textedit::select_all(&ui.draft, &mut ui.draft_caret, &mut ui.draft_sel);
                                ui.redraw();
                            }
                        }
                        0x43 => {
                            // Ctrl+C 复制选中文本
                            if ctrl && ui.page == Page::Agenda {
                                if let Some(t) = crate::textedit::sel_text(&ui.draft, ui.draft_caret, ui.draft_sel) {
                                    unsafe {
                                        crate::textedit::set_clipboard(ui.hwnd, &t);
                                    }
                                }
                            }
                        }
                        0x58 => {
                            // Ctrl+X 剪切
                            if ctrl && ui.page == Page::Agenda {
                                if let Some(t) = crate::textedit::sel_text(&ui.draft, ui.draft_caret, ui.draft_sel) {
                                    unsafe {
                                        crate::textedit::set_clipboard(ui.hwnd, &t);
                                    }
                                    crate::textedit::delete_sel(&mut ui.draft, &mut ui.draft_caret, &mut ui.draft_sel);
                                    ui.clear_hint();
                                    ui.redraw();
                                }
                            }
                        }
                        0x25 | 0x27 | 0x26 | 0x28 => {
                            // ←→ ±1 天 / ↑↓ ±7 天：月历移动选中日期（跨月翻页）；
                            // 日程页为快捷输入的光标移动（↑↓ 折算行首/行尾）
                            match ui.page {
                                Page::Calendar => {
                                    let delta: i64 = match wp as i32 {
                                        0x25 => -1,
                                        0x27 => 1,
                                        0x26 => -7,
                                        _ => 7,
                                    };
                                    ui.selected += Duration::days(delta);
                                    if ui.selected.month() != ui.view_m || ui.selected.year() != ui.view_y {
                                        ui.view_m = ui.selected.month();
                                        ui.view_y = ui.selected.year();
                                    }
                                    if crate::sidebar::sidebar_visible() {
                                        crate::sidebar::sidebar_show(ui.selected);
                                    }
                                    ui.redraw();
                                }
                                Page::Agenda => {
                                    let ext = shift;
                                    match wp as i32 {
                                        0x25 => crate::textedit::move_caret(&ui.draft, &mut ui.draft_caret, &mut ui.draft_sel, -1, ext),
                                        0x27 => crate::textedit::move_caret(&ui.draft, &mut ui.draft_caret, &mut ui.draft_sel, 1, ext),
                                        0x26 => crate::textedit::home_end(&ui.draft, &mut ui.draft_caret, &mut ui.draft_sel, false, ext),
                                        _ => crate::textedit::home_end(&ui.draft, &mut ui.draft_caret, &mut ui.draft_sel, true, ext),
                                    }
                                    ui.redraw();
                                }
                            }
                        }
                        0x21 | 0x22 => {
                            // PgUp / PgDn 翻月
                            if ui.page == Page::Calendar {
                                if wp as i32 == 0x21 {
                                    ui.view_m -= 1;
                                    if ui.view_m < 1 {
                                        ui.view_m = 12;
                                        ui.view_y -= 1;
                                    }
                                } else {
                                    ui.view_m += 1;
                                    if ui.view_m > 12 {
                                        ui.view_m = 1;
                                        ui.view_y += 1;
                                    }
                                }
                                ui.redraw();
                            }
                        }
                        0x24 | 0x23 => {
                            // Home 回今天（Home）/ 行尾（End）；日程页为行首/行尾
                            if ui.page == Page::Agenda {
                                crate::textedit::home_end(&ui.draft, &mut ui.draft_caret, &mut ui.draft_sel, wp as i32 == 0x23, shift);
                                ui.redraw();
                            } else if wp as i32 == 0x24 {
                                let today = Local::now().date_naive();
                                ui.selected = today;
                                ui.view_m = today.month();
                                ui.view_y = today.year();
                                if crate::sidebar::sidebar_visible() {
                                    crate::sidebar::sidebar_show(today);
                                }
                                ui.redraw();
                            }
                        }
                        0x0D => {
                            if ui.page == Page::Agenda {
                                ui.add_agenda();
                            } else if ui.page == Page::Calendar {
                                // 回车：选中日期打开/切换侧栏（与点击日期行为一致）
                                let d = ui.selected;
                                if crate::sidebar::sidebar_visible() && crate::sidebar::sidebar_date() == Some(d) {
                                    crate::sidebar::sidebar_hide();
                                } else {
                                    crate::sidebar::sidebar_show(d);
                                }
                                ui.redraw();
                            }
                        }
                        _ => {}
                    }
                }
            }
            if hide {
                unsafe { ShowWindow(hwnd, SW_HIDE); }
                forecast_close();
                crate::sidebar::sidebar_hide();
                crate::trim_working_set();
            }
            0
        }
        WM_CHAR => {
            let mut guard = UI.lock().unwrap();
            if let Some(sui) = guard.as_mut() {
                let ui = &mut sui.0;
                if ui.page == Page::Agenda && (wp as u32) >= 0x20 {
                    if let Some(ch) = char::from_u32(wp as u32) {
                        crate::textedit::insert(&mut ui.draft, &mut ui.draft_caret, &mut ui.draft_sel, &ch.to_string());
                        ui.clear_hint();
                        ui.redraw();
                    }
                }
            }
            0
        }
        WM_IME_COMPOSITION => {
            let mut guard = UI.lock().unwrap();
            if let Some(sui) = guard.as_mut() {
                let ui = &mut sui.0;
                if ui.page == Page::Agenda {
                    if lp as u32 & 0x0800 != 0 {
                        if let Some(s) = read_ime_string(hwnd, 0x0800) {
                            crate::textedit::insert(&mut ui.draft, &mut ui.draft_caret, &mut ui.draft_sel, &s);
                            ui.comp.clear();
                            ui.clear_hint();
                        }
                    } else if lp as u32 & 0x0008 != 0 {
                        ui.comp = read_ime_string(hwnd, 0x0008).unwrap_or_default();
                    }
                    ui.redraw();
                }
            }
            0
        }
        WM_MOUSEWHEEL => {
            // 滚轮：月历翻月，日程页滚动列表
            let delta = ((wp as usize) >> 16) as u16 as i16;
            let mut guard = UI.lock().unwrap();
            if let Some(sui) = guard.as_mut() {
                let ui = &mut sui.0;
                if ui.shown && delta != 0 {
                    let down = delta < 0;
                    match ui.page {
                        Page::Agenda => {
                            if down {
                                ui.agenda_scroll = ui.agenda_scroll.saturating_add(1);
                            } else {
                                ui.agenda_scroll = ui.agenda_scroll.saturating_sub(1);
                            }
                            ui.redraw();
                        }
                        Page::Calendar => {
                            // 日历页滚轮翻月：向下 = 下个月，向上 = 上个月
                            ui.preview = None;
                            if down {
                                ui.view_m += 1;
                                if ui.view_m > 12 {
                                    ui.view_m = 1;
                                    ui.view_y += 1;
                                }
                            } else {
                                ui.view_m -= 1;
                                if ui.view_m < 1 {
                                    ui.view_m = 12;
                                    ui.view_y -= 1;
                                }
                            }
                            ui.redraw();
                        }
                    }
                }
            }
            0
        }
                WM_ACTIVATE => {
            let low = (wp & 0xFFFF) as u16;
            // 设置窗口打开期间面板不随失焦隐藏（二者共存）；
            // 光标位于软件自身弹窗（天气侧栏/设置等）上时同样保持显示：
            // 只有点击发生在软件相关窗口之外才关闭日历
            if low == 0 && !settings_visible() && !cursor_on_own_popup() && !crate::inputbox::visible() {
                let hide = {
                    let mut guard = UI.lock().unwrap();
                    let mut hide = false;
                    if let Some(sui) = guard.as_mut() {
                        let ui = &mut sui.0;
                        if ui.shown {
                            ui.shown = false;
                            free_surface(ui);
                            hide = true;
                        }
                    }
                    hide
                };
                if hide {
                    ShowWindow(hwnd, SW_HIDE);
                    forecast_close();
                    crate::sidebar::sidebar_hide();
                    // 设置窗口保持打开：只由其"确定/✕"按钮关闭
                    SHOWN_FLAG.store(0, Ordering::Relaxed);
                    crate::trim_working_set();
                }
            }
            0
        }
        WM_APP_TOGGLE => {
            let shown = {
                let guard = UI.lock().unwrap();
                guard.as_ref().map(|s| s.0.shown).unwrap_or(false)
            };
            if shown {
                perform_hide(hwnd);
            } else {
                perform_show(hwnd);
            }
            0
        }
        WM_APP_SHOW => {
            perform_show(hwnd);
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn read_ime_string(hwnd: HWND, mode: i32) -> Option<String> {
    let himc = ImmGetContext(hwnd);
    if himc.is_null() {
        return None;
    }
    let len = ImmGetCompositionStringW(himc, mode, std::ptr::null_mut(), 0);
    let mut out = None;
    if len >= 0 {
        let mut buf = vec![0u8; (len + 2) as usize];
        let got = ImmGetCompositionStringW(himc, mode, buf.as_mut_ptr() as *mut c_void_ty, len);
        if got >= 0 {
            let slice = std::slice::from_raw_parts::<u16>(buf.as_ptr() as *const u16, (len as usize) / 2);
            out = Some(String::from_utf16_lossy(slice));
        }
    }
    ImmReleaseContext(hwnd, himc);
    out
}

type c_void_ty = winapi::ctypes::c_void;

#[link(name = "imm32")]
extern "system" {
    fn ImmGetContext(hwnd: HWND) -> *mut c_void_ty;
    fn ImmReleaseContext(hwnd: HWND, himc: *mut c_void_ty) -> i32;
    fn ImmGetCompositionStringW(himc: *mut c_void_ty, index: i32, buf: *mut c_void_ty, len: i32) -> i32;
}

unsafe fn paste_clipboard(ui: &mut Ui) {
    if OpenClipboard(ui.hwnd as HWND) == 0 {
        return;
    }
    let h = GetClipboardData(13);
    if !h.is_null() {
        let ptr = GlobalLock(h as usize);
        if !ptr.is_null() {
            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(ptr, len);
            let text = String::from_utf16_lossy(slice);
            crate::textedit::insert(&mut ui.draft, &mut ui.draft_caret, &mut ui.draft_sel, &text);
            ui.clear_hint();
            GlobalUnlock(h as usize);
        }
    }
    CloseClipboard();
    ui.redraw();
}

#[link(name = "kernel32")]
extern "system" {
    fn GlobalLock(h: usize) -> *mut u16;
    fn GlobalUnlock(h: usize) -> i32;
}
