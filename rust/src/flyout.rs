//! 日历弹窗：Win32 分层窗口 + GDI+ 自绘
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, RwLock};

use chrono::{Datelike, Duration, Local, NaiveDate, Timelike};
use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::windef::{HWND, POINT, RECT, SIZE};
use winapi::um::wingdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, SelectObject};
use winapi::um::winuser::*;

use crate::config::{apply_autostart, Config};
use crate::gdi::{self, Cache, Painter};
use crate::ics::{DayInfo, DayType, HolidayMap};
use crate::lunar::{self, FestKind};
use crate::tray;
use crate::weather::Weather;

// ---------------- 配色（取自设计图） ----------------
const BG: u32 = gdi::argb(255, 0x20, 0x28, 0x38);
const BLUE: u32 = gdi::argb(255, 0x3E, 0x87, 0xFA);
const RED: u32 = gdi::argb(255, 0xE5, 0x4B, 0x4B);
const TXT: u32 = gdi::argb(255, 0xE0, 0xE4, 0xEB);
const DATE_COL: u32 = gdi::argb(255, 0xDD, 0xE2, 0xE9);
const SUB: u32 = gdi::argb(255, 0x9A, 0xA1, 0xA9);
const SUB_DIM: u32 = gdi::argb(255, 0x5C, 0x66, 0x73);
const LEGAL: u32 = gdi::argb(255, 0xE4, 0xE7, 0xEB);
const WEEK_HEAD: u32 = gdi::argb(255, 0xA6, 0xAD, 0xB6);
const WEEK_NUM: u32 = gdi::argb(255, 0x6E, 0x76, 0x81);
const ICON_COL: u32 = gdi::argb(255, 0x8A, 0x91, 0x9C);
const ROW_TXT: u32 = gdi::argb(255, 0xD7, 0xDD, 0xE4);
const TITLE_COL: u32 = gdi::argb(255, 0xDF, 0xE5, 0xEC);
const HOVER_BG: u32 = gdi::argb(20, 255, 255, 255);
const WHITE: u32 = gdi::argb(255, 255, 255, 255);
const PLUS_TOP: u32 = gdi::argb(255, 0x38, 0xA6, 0xFA);
const PLUS_BOT: u32 = gdi::argb(255, 0x2E, 0x8E, 0xF0);
const SUN: u32 = gdi::argb(255, 0xFF, 0xC8, 0x50);
const CLOUD: u32 = gdi::argb(255, 0xE8, 0xEC, 0xF2);
const RAIN: u32 = gdi::argb(255, 0x6F, 0xA8, 0xFF);
const BORDER: u32 = gdi::argb(22, 255, 255, 255);
const DIVIDER: u32 = gdi::argb(20, 255, 255, 255);
const POPUP_BG: u32 = gdi::argb(255, 0x26, 0x30, 0x42);

pub const WIN_W: f32 = 560.0;
pub const WIN_H: f32 = 736.0;

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
    AgendaAdd,
    InputBox,
    OpenSettings,
    MenuShow,
    MenuAuto,
    MenuRefresh,
    MenuQuit,
    /// 头部左侧天气热区（悬停弹出近一周天气面板）
    Weather,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Page {
    Calendar,
    Agenda,
}

struct Ui {
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
    agenda: Arc<Mutex<HashMap<String, Vec<String>>>>,
    shown: bool,
    page: Page,
    view_y: i32,
    view_m: u32,
    selected: NaiveDate,
    regions: Vec<(gdi::RectF, Action)>,
    hover: Option<Action>,
    menu_open: bool,
    draft: String,
    comp: String,
    caret_on: bool,
    tick: u32,
    last_second: u32,
    idle_timer: bool,
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
    Refresh,
    Confirm,
    Close,
}

struct SettingsUi {
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
    tab: usize,
    regions: Vec<(gdi::RectF, SAction)>,
    hover: Option<SAction>,
    week_menu_open: bool,
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

        let w = SETTINGS_W as i32;
        let h = SETTINGS_H as i32;
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
            tab: std::env::var("CAL_TAB").ok().and_then(|v| v.parse().ok()).unwrap_or(0),
            regions: Vec::new(),
            hover: None,
            week_menu_open: false,
            refreshing: false,
            ics_status: None,
            dumped: false,
            dump_path: std::env::var("CAL_DUMP2").unwrap_or_default(),
        });
        unsafe {
            let hdc = GetDC(std::ptr::null_mut());
            sui.mem_dc = CreateCompatibleDC(hdc) as usize;
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = sui.w as i32;
            bmi.bmiHeader.biHeight = -(sui.h as i32);
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;
            let mut bits: *mut winapi::ctypes::c_void = std::ptr::null_mut();
            let hbmp = CreateDIBSection(hdc, &bmi, 0, &mut bits, std::ptr::null_mut(), 0);
            SelectObject(sui.mem_dc as winapi::shared::windef::HDC, hbmp as winapi::shared::windef::HGDIOBJ);
            ReleaseDC(std::ptr::null_mut(), hdc);
            let mut bmp: gdi::Gp = std::ptr::null_mut();
            GdipCreateBitmapFromScan0(
                sui.w as i32,
                sui.h as i32,
                (sui.w * 4.0) as i32,
                gdi::PIXEL_FORMAT_32BPP_PARGB,
                bits as *mut u8,
                &mut bmp,
            );
            sui.bmp = bmp;
            sui.scan0 = bits as *mut u8;
            // 常驻 Graphics：redraw 复用，避免每次创建/销毁撑大堆
            GdipGetImageGraphicsContext(sui.bmp, &mut sui.g);
            GdipSetSmoothingMode(sui.g, gdi::SMOOTH_ANTI_ALIAS);
            GdipSetTextRenderingHint(sui.g, gdi::TEXT_AA_GRID_FIT);
        }

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
        let x = wa.left + ((wa.right - wa.left) - SETTINGS_W as i32) / 2;
        let y = wa.top + ((wa.bottom - wa.top) - SETTINGS_H as i32) / 2;
        (x, y)
    }
}

type c_void_ty2 = winapi::ctypes::c_void;

#[link(name = "user32")]
extern "system" {
    fn SystemParametersInfoW(action: u32, param: u32, data: *mut c_void_ty2, init: u32) -> i32;
}

/// 以指定页签打开设置窗口（日期侧边栏“卡片管理”入口）
pub fn show_settings_tab(tab: usize) {
    {
        let mut guard = SETTINGS_UI.lock().unwrap();
        if let Some(sui) = guard.as_mut() {
            sui.0.tab = tab;
            sui.0.week_menu_open = false;
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
        SetWindowPos(hwnd as HWND, HWND_TOPMOST, x, y, SETTINGS_W as i32, SETTINGS_H as i32, SWP_NOACTIVATE);
        ShowWindow(hwnd as HWND, SW_SHOWNA);
    }
    {
        let mut guard = SETTINGS_UI.lock().unwrap();
        if let Some(sui) = guard.as_mut() {
            sui.0.week_menu_open = false; // 重开时收起上次遗留的下拉
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

impl SettingsUi {
    fn hit_add(regions: &mut Vec<(gdi::RectF, SAction)>, x: f32, y: f32, w: f32, h: f32, a: SAction) {
        regions.push((gdi::RectF { x, y, w, h }, a));
    }

    fn hovered(&self, a: &SAction) -> bool {
        self.hover.map(|h| h == *a).unwrap_or(false)
    }

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
        let p = unsafe {
            Painter {
                g,
                cache: cache_ptr,
                sf: self.sf,
                w: self.w,
                h: self.h,
            }
        };
        let mut regions: Vec<(gdi::RectF, SAction)> = Vec::new();
        p.clear();
        self.paint(&p, &mut regions);
        self.regions = regions;
        self.ulw();

        if !self.dumped && !self.dump_path.is_empty() {
            self.dumped = true;
            save_bmp(self.scan0, self.w as i32, self.h as i32, &self.dump_path);
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
        p.fill_round(px, py, pw, ph, 12.0, POPUP_BG);
        p.stroke_round(px, py, pw, ph, 12.0, 1.0, BORDER);

        // 标题栏（原生拖动区域）
        p.text("设置", px + 16.0, py + 12.0, 100.0, 26.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 15.0, true, false, TITLE_COL);
        // 关闭
        let cx_btn = px + pw - 34.0;
        Self::hit_add(regions, cx_btn, py + 12.0, 24.0, 24.0, SAction::Close);
        let hov = self.hovered(&SAction::Close);
        p.text("✕", cx_btn, py + 12.0, 24.0, 24.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, if hov { RED } else { WEEK_NUM });

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
                p.fill_rect(px + 12.0, ty + 8.0, 3.0, 26.0, BLUE);
            } else if hov {
                p.fill_round(px + 12.0, ty, 120.0, 42.0, 8.0, gdi::argb(14, 255, 255, 255));
            }
            p.text(glyph, px + 22.0, ty, 22.0, 42.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 14.0, false, true, if sel { BLUE } else { ICON_COL });
            p.text(name, px + 50.0, ty, 80.0, 42.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, if sel { WHITE } else { ROW_TXT });
        }
        p.fill_rect(px + 136.0, py + 50.0, 1.0, ph - 60.0, DIVIDER);

        let cx = px + 148.0;
        let cw = px + pw - 16.0 - cx;

        let cfg = self.st.config.lock().unwrap().clone();
        let last_ics = cfg.last_ics_update;

        if self.tab == 0 {
            let rows: [(u8, &str, bool); 3] = [
                (0, "开机自启", cfg.autostart),
                (1, "自动更新假期数据（每天检查）", cfg.auto_update),
                (8, "显示系统托盘图标", cfg.show_tray),
            ];
            let mut y = py + 54.0;
            for (idx, name, on) in rows {
                Self::hit_add(regions, cx, y, cw, 40.0, SAction::Toggle(idx));
                p.text(name, cx + 2.0, y, cw - 60.0, 40.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, ROW_TXT);
                let sw_x = cx + cw - 46.0;
                p.fill_round(sw_x, y + 10.0, 36.0, 20.0, 10.0, if on { BLUE } else { gdi::argb(36, 255, 255, 255) });
                let kx = if on { sw_x + 26.0 } else { sw_x + 10.0 };
                p.fill_circle(kx, y + 20.0, 7.0, WHITE);
                y += 46.0;
            }
            y += 8.0;
            let btn_w = if self.refreshing { 190.0 } else { 160.0 };
            Self::hit_add(regions, cx, y, btn_w, 32.0, SAction::Refresh);
            let hov = self.hovered(&SAction::Refresh) && !self.refreshing;
            p.fill_round(cx, y, btn_w, 32.0, 8.0, gdi::argb(38, 62, 135, 250));
            p.stroke_round(cx, y, btn_w, 32.0, 8.0, 1.0, gdi::argb(110, 62, 135, 250));
            let label = if self.refreshing { "正在更新节假日数据…" } else { "立即更新节假日数据" };
            p.text(label, cx, y, btn_w, 32.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, BLUE);
            y += 44.0;
            let upd = if last_ics > 0 {
                chrono::DateTime::from_timestamp((last_ics / 1000) as i64, 0)
                    .map(|dt| dt.with_timezone(&chrono::Local).format("%Y/%m/%d %H:%M").to_string())
                    .unwrap_or_default()
            } else {
                "尚未更新".to_string()
            };
            p.text(&format!("上次更新：{}", upd), cx, y, cw, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 11.0, false, false, SUB_DIM);
            y += 22.0;
            // 更新结果提示（成功绿 / 失败红）
            if let Some(ok) = self.ics_status {
                let (msg, col) = if ok {
                    ("✓ 节假日数据更新成功", gdi::argb(255, 0x5B, 0xC2, 0x8E))
                } else {
                    ("✕ 更新失败，请检查网络后重试", RED)
                };
                p.text(msg, cx, y, cw, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.0, false, false, col);
                y += 20.0;
            }
            for line in [
                "节假日数据来源：chinese-days（cdn.jsdelivr.net），",
                "包含法定节假日与调休补班，自动获取最新年份。",
            ] {
                p.text(line, cx, y, cw, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 11.0, false, false, SUB_DIM);
                y += 20.0;
            }
        } else if self.tab == 1 {
            let rows: [(u8, &str, bool); 6] = [
                (2, "显示农历/节日信息", cfg.show_lunar),
                (3, "显示调休安排", cfg.show_adjust),
                (4, "显示非当前月日期", cfg.show_other_month),
                (5, "使用12小时制", cfg.hour12),
                (6, "显示周数", cfg.show_week_num),
                (7, "显示天气预报", cfg.show_weather),
            ];
            let mut y = py + 54.0;
            for (idx, name, on) in rows {
                Self::hit_add(regions, cx, y, cw, 40.0, SAction::Toggle(idx));
                p.text(name, cx + 2.0, y, cw - 60.0, 40.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, ROW_TXT);
                let sw_x = cx + cw - 46.0;
                p.fill_round(sw_x, y + 10.0, 36.0, 20.0, 10.0, if on { BLUE } else { gdi::argb(36, 255, 255, 255) });
                let kx = if on { sw_x + 26.0 } else { sw_x + 10.0 };
                p.fill_circle(kx, y + 20.0, 7.0, WHITE);
                y += 46.0;
            }
            Self::hit_add(regions, cx, y, cw, 40.0, SAction::WeekDropdown);
            p.text("一周开始", cx + 2.0, y, cw - 120.0, 40.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, ROW_TXT);
            let names = ["星期一", "星期二", "星期三", "星期四", "星期五", "星期六", "星期日"];
            let vn = names[(cfg.week_start as usize) % 7];
            let pill_w = 96.0;
            let pill_x = cx + cw - pill_w - 4.0;
            let pill_top = y + 5.0;
            let hov = self.hovered(&SAction::WeekDropdown);
            p.fill_round(pill_x, pill_top, pill_w, 30.0, 8.0, if hov { gdi::argb(50, 62, 135, 250) } else { gdi::argb(28, 62, 135, 250) });
            p.stroke_round(pill_x, pill_top, pill_w, 30.0, 8.0, 1.0, gdi::argb(110, 62, 135, 250));
            p.text(&format!("{} ▾", vn), pill_x, pill_top, pill_w, 30.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, BLUE);
            y += 52.0;
            p.text("更改设置后立即生效，无需保存。", cx, y, cw, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 11.0, false, false, SUB_DIM);
            // 下拉展开：选项列表（后绘制=命中优先），下方放不下则向上弹出
            if self.week_menu_open {
                let opt_h = 26.0;
                let list_h = opt_h * names.len() as f32 + 8.0;
                let list_top = if pill_top + 30.0 + list_h > py + ph - 50.0 {
                    pill_top - list_h
                } else {
                    pill_top + 32.0
                };
                p.fill_round(pill_x - 1.0, list_top, pill_w + 2.0, list_h, 8.0, POPUP_BG);
                p.stroke_round(pill_x - 1.0, list_top, pill_w + 2.0, list_h, 8.0, 1.0, BORDER);
                for (i, name) in names.iter().enumerate() {
                    let oy = list_top + 4.0 + opt_h * i as f32;
                    Self::hit_add(regions, pill_x + 1.0, oy, pill_w - 2.0, opt_h, SAction::WeekPick(i));
                    let h = self.hovered(&SAction::WeekPick(i));
                    if h {
                        p.fill_round(pill_x + 2.0, oy, pill_w - 4.0, opt_h - 1.0, 6.0, gdi::argb(50, 62, 135, 250));
                    }
                    let sel = (cfg.week_start as usize) % 7 == i;
                    p.text(name, pill_x + 1.0, oy, pill_w - 2.0, opt_h, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, if sel { BLUE } else { ROW_TXT });
                }
            }
        } else {
            // 侧栏管理：卡片开关
            p.fill_round(cx, py + 50.0, cw, 52.0, 8.0, POPUP_BG);
            p.text("侧栏卡片", cx + 14.0, py + 56.0, cw - 28.0, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, true, false, TITLE_COL);
            p.text("点击日历日期时在左侧展示，开关控制卡片显示", cx + 14.0, py + 76.0, cw - 28.0, 15.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 10.5, false, false, SUB_DIM);
            let rows: [(u8, &str, bool); 7] = [
                (10, "日期信息", cfg.sidebar_date),
                (11, "黄历信息", cfg.sidebar_almanac),
                (12, "最近事件", cfg.sidebar_events),
                (13, "今日日程", cfg.sidebar_agenda),
                (14, "历史上的今天", cfg.sidebar_history),
                (15, "时间格言", cfg.sidebar_motto),
                (16, "待办清单", cfg.sidebar_todo),
            ];
            let mut y = py + 114.0;
            for (idx, name, on) in rows {
                Self::hit_add(regions, cx, y, cw, 38.0, SAction::Toggle(idx));
                p.fill_round(cx, y, cw, 38.0, 8.0, gdi::argb(14, 255, 255, 255));
                p.text(name, cx + 14.0, y, cw - 70.0, 38.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, false, false, ROW_TXT);
                let sw_x = cx + cw - 50.0;
                p.fill_round(sw_x, y + 9.0, 36.0, 20.0, 10.0, if on { BLUE } else { gdi::argb(36, 255, 255, 255) });
                let kx = if on { sw_x + 26.0 } else { sw_x + 10.0 };
                p.fill_circle(kx, y + 19.0, 7.0, WHITE);
                y += 42.0;
            }
            p.text("开关即时生效；日期侧边栏底部也可进入本页。", cx + 2.0, y + 6.0, cw, 16.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 10.5, false, false, SUB_DIM);
        }

        // 确定按钮（右下角）
        let btn = gdi::RectF { x: px + pw - 106.0, y: py + ph - 44.0, w: 90.0, h: 30.0 };
        Self::hit_add(regions, btn.x, btn.y, btn.w, btn.h, SAction::Confirm);
        let hov = self.hovered(&SAction::Confirm);
        p.fill_round(btn.x, btn.y, btn.w, btn.h, 8.0, if hov { gdi::argb(255, 0x53, 0x99, 0xFB) } else { BLUE });
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
                self.redraw();
            }
            SAction::WeekDropdown => {
                self.week_menu_open = !self.week_menu_open;
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
            if cx >= 10.0 && cx <= (r.right - r.left) as f32 - 40.0 && cy >= 10.0 && cy <= 50.0 {
                2 // HTCAPTION
            } else {
                DefWindowProcW(hwnd, msg, wp, lp)
            }
        }
        WM_LBUTTONDOWN => {
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
                } else if let Some(a) = a {
                    match a {
                        SAction::Toggle(_) | SAction::WeekDropdown | SAction::Refresh | SAction::Tab(_) => {
                            ui.handle_action(&a);
                        }
                        SAction::Confirm | SAction::Close => {
                            hide_settings();
                        }
                        _ => {}
                    }
                }
            }
            0
        }
        WM_MOUSEMOVE => {
            let mut guard = SETTINGS_UI.lock().unwrap();
            if let Some(sui) = guard.as_mut() {
                let ui = &mut sui.0;
                let x = ((lp & 0xFFFF) as u16 as i16) as f32 / ui.sf;
                let y = (((lp as usize) >> 16) as u16 as i16) as f32 / ui.sf;
                let a = ui.action_at(x, y);
                if a != ui.hover {
                    ui.hover = a;
                    ui.redraw();
                    let clickable = matches!(a, Some(SAction::Tab(_)) | Some(SAction::Toggle(_)) | Some(SAction::WeekDropdown) | Some(SAction::WeekPick(_)) | Some(SAction::Refresh) | Some(SAction::Confirm) | Some(SAction::Close));
                    SetCursor(if clickable { LoadCursorW(std::ptr::null_mut(), IDC_HAND) } else { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) });
                }
            }
            0
        }
        WM_KEYDOWN => {
            if wp as i32 == 0x1B {
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
    /// 最近一次手动刷新时间（用于显示“更新中…”并防止连点）
    refreshing: Option<std::time::Instant>,
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

        let w = FC_W as i32;
        let h = FC_H as i32;
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
            sf: 1.0,
            w: w as f32,
            h: h as f32,
            mem_dc: 0,
            bmp: std::ptr::null_mut(),
            scan0: std::ptr::null_mut(),
            g: std::ptr::null_mut(),
            cache: Cache::new(),
            st,
            dumped: false,
            dump_path: std::env::var("CAL_DUMP3").unwrap_or_default(),
            link_rect: gdi::RectF { x: 0.0, y: 0.0, w: 0.0, h: 0.0 },
            link_hover: false,
            refreshing: None,
        });
        let hdc = GetDC(std::ptr::null_mut());
        fui.mem_dc = CreateCompatibleDC(hdc) as usize;
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = fui.w as i32;
        bmi.bmiHeader.biHeight = -(fui.h as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut winapi::ctypes::c_void = std::ptr::null_mut();
        let hbmp = CreateDIBSection(hdc, &bmi, 0, &mut bits, std::ptr::null_mut(), 0);
        SelectObject(fui.mem_dc as winapi::shared::windef::HDC, hbmp as winapi::shared::windef::HGDIOBJ);
        ReleaseDC(std::ptr::null_mut(), hdc);
        let mut bmp: gdi::Gp = std::ptr::null_mut();
        GdipCreateBitmapFromScan0(
            fui.w as i32,
            fui.h as i32,
            (fui.w * 4.0) as i32,
            gdi::PIXEL_FORMAT_32BPP_PARGB,
            bits as *mut u8,
            &mut bmp,
        );
        fui.bmp = bmp;
        fui.scan0 = bits as *mut u8;
        GdipGetImageGraphicsContext(fui.bmp, &mut fui.g);

        // 初始绘制（兼 CAL_DUMP3 首帧转储）
        fui.redraw();
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
        // 与日期侧栏互斥：打开天气时自动收起日期侧栏（先收起，锚点即回到日历左缘）
        crate::sidebar::sidebar_hide();
        let mut mr: RECT = std::mem::zeroed();
        GetWindowRect(main_hwnd as HWND, &mut mr);
        let mut wa: RECT = std::mem::zeroed();
        SystemParametersInfoW(0x0030 /*SPI_GETWORKAREA*/, 0, &mut wa as *mut RECT as *mut c_void_ty2, 0);
        // 主面板可见边缘在窗口内 10px 处：面板右缘贴其左缘（间隔 0），顶部与日历对齐；
        // 日期侧栏打开时锚定到侧栏左缘（向左串联）
        let anchor = match crate::sidebar::sidebar_left_x() {
            Some(sx) => sx,
            None => mr.left + 10,
        };
        let mut x = anchor - FC_W as i32;
        let mut y = mr.top + 10;
        if y + FC_H as i32 > wa.bottom - 4 {
            y = wa.bottom - 4 - FC_H as i32;
        }
        if y < wa.top + 4 {
            y = wa.top + 4;
        }
        if x < wa.left + 4 {
            x = mr.right - 10;
        }
        let xmax = wa.right - FC_W as i32 - 4;
        if x > xmax {
            x = xmax;
        }
        SetWindowPos(fh, HWND_TOPMOST, x, y, FC_W as i32, FC_H as i32, SWP_NOACTIVATE);
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

pub fn forecast_redraw() {
    let mut guard = FORECAST_UI.lock().unwrap();
    if let Some(f) = guard.as_mut() {
        f.0.redraw();
    }
}

impl ForecastUi {
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

        if !self.dumped && !self.dump_path.is_empty() {
            self.dumped = true;
            save_bmp(self.scan0, self.w as i32, self.h as i32, &self.dump_path);
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
        // 面板底（无边框、无阴影环，窗口即面板）
        p.fill_round(0.0, 0.0, FC_W, FC_H, 12.0, POPUP_BG);

        let w = self.st.weather.lock().unwrap().clone();
        let Some(w) = w else { return };
        if w.days.is_empty() {
            return;
        }
        let today = Local::now().date_naive();

        // 头部：定位城市 + 更新时间 + 手动刷新链接
        p.text("\u{E81D}", 16.0, 12.0, 18.0, 22.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.0, false, true, BLUE);
        p.text(&w.city, 36.0, 12.0, 160.0, 22.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, true, false, TITLE_COL);
        let refreshing = self
            .refreshing
            .map(|t| t.elapsed() < std::time::Duration::from_secs(5))
            .unwrap_or(false);
        let time_str = chrono::DateTime::from_timestamp((w.ts / 1000) as i64, 0)
            .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M").to_string())
            .unwrap_or_else(|| "--:--".into());
        let head = format!("更新时间：{}", time_str);
        let (tail, tail_col) = if refreshing {
            ("更新中…", if self.link_hover { WHITE } else { BLUE })
        } else {
            ("更新", if self.link_hover { WHITE } else { BLUE })
        };
        let hw = p.measure(&head, 10.0, false, false).0;
        let tw = p.measure(tail, 10.0, false, false).0;
        let gap = 6.0;
        let hx = FC_W - 16.0 - hw - gap - tw;
        p.text(&head, hx, 14.0, hw + 2.0, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 10.0, false, false, SUB_DIM);
        p.text(tail, hx + hw + gap, 14.0, tw + 2.0, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 10.0, false, false, tail_col);
        self.link_rect = gdi::RectF { x: hx + hw + gap, y: 14.0, w: tw, h: 18.0 };
        p.fill_rect(16.0, 42.0, FC_W - 32.0, 1.0, DIVIDER);

        // 每日列（不足 7 天时居中排布）
        let n = w.days.len().min(7);
        let col_w = (FC_W - 24.0) / 7.0;
        let x0 = 12.0 + ((FC_W - 24.0) - col_w * n as f32) / 2.0;
        for (i, d) in w.days.iter().take(n).enumerate() {
            let cx = x0 + col_w * i as f32 + col_w / 2.0;
            let weekday = ["星期日", "星期一", "星期二", "星期三", "星期四", "星期五", "星期六"]
                [d.date.weekday().num_days_from_sunday() as usize];
            p.text(weekday, cx - col_w / 2.0, 52.0, col_w, 18.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.5, true, false, if d.date == today { WHITE } else { DATE_COL });

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
            p.text(&dstr, tx, 72.0, dw + 2.0, 14.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 10.0, false, false, SUB);
            if !tag.is_empty() {
                p.text(tag, tx + dw + gap, 72.0, tw + 2.0, 14.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 10.0, false, false, BLUE);
            }

            draw_weather(p, cx, 110.0, d.code, 1.25);
            p.text(crate::weather::wmo_text(d.code), cx - col_w / 2.0, 138.0, col_w, 16.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 11.0, false, false, ROW_TXT);
            p.text(&format!("{} ~ {}°C", d.tmin, d.tmax), cx - col_w / 2.0, 158.0, col_w, 18.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, DATE_COL);

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
                    p.text("空气 --", cx - col_w / 2.0, 180.0, col_w, 14.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 10.0, false, false, SUB_DIM);
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
                let x = ((lp & 0xFFFF) as u16 as i16) as f32;
                let y = (((lp as usize) >> 16) as u16 as i16) as f32;
                let r = f.link_rect;
                let hit = x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
                if hit != f.link_hover {
                    f.link_hover = hit;
                    f.redraw();
                }
                if hit {
                    SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_HAND));
                    // 注册离开跟踪，移出链接后恢复箭头与颜色
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
                if f.link_hover {
                    f.link_hover = false;
                    f.redraw();
                }
            }
            0
        }
        WM_LBUTTONDOWN => {
            let mut guard = FORECAST_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                let x = ((lp & 0xFFFF) as u16 as i16) as f32;
                let y = (((lp as usize) >> 16) as u16 as i16) as f32;
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
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}


pub fn create_window(st: SharedState, agenda: Arc<Mutex<HashMap<String, Vec<String>>>>, tray: Arc<Mutex<Option<tray::Tray>>>) {
    unsafe {
        let cls = crate::wide("z-calendar-main");
        let hinstance = winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null());
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinstance;
        wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        wc.lpszClassName = cls.as_ptr();
        RegisterClassW(&wc);

        let w = WIN_W as i32;
        let h = WIN_H as i32;
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
            agenda,
            shown: false,
            page,
            view_y,
            view_m,
            selected: today,
            regions: Vec::new(),
            hover: None,
            menu_open: false,
            draft: String::new(),
            comp: String::new(),
            caret_on: true,
            tick: 0,
            last_second: 0,
            idle_timer: false,
            dumped: false,
            dump_path: std::env::var("CAL_DUMP").unwrap_or_default(),
        });
        create_dib(&mut ui);
        // 常驻 Graphics：redraw 复用，避免每次创建/销毁撑大堆
        unsafe {
            GdipGetImageGraphicsContext(ui.bmp, &mut ui.g);
            GdipSetSmoothingMode(ui.g, gdi::SMOOTH_ANTI_ALIAS);
            GdipSetTextRenderingHint(ui.g, gdi::TEXT_AA_GRID_FIT);
        }

        UI.lock().unwrap().replace(SendUi(ui));

        // 初始绘制（调试 dump 依赖 + 保证显示前内容就绪）
        {
            let mut guard = UI.lock().unwrap();
            if let Some(sui) = guard.as_mut() {
                sui.0.redraw();
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

/// 弹窗位置：时钟上方右侧对齐
fn position_for(clock: Option<&crate::overlay::ClockInfo>, sf: f32, win_w: i32, win_h: i32) -> (i32, i32) {
    let (mut x, mut y);
    match clock {
        Some(ci) => {
            let clk_right = ci.rect.right as f32 / sf;
            let clk_top = ci.rect.top as f32 / sf;
            let clk_bottom = ci.rect.bottom as f32 / sf;
            x = clk_right - win_w as f32 + 12.0;
            y = clk_top - win_h as f32 - 6.0;
            let wa = (
                ci.work.0 as f32 / sf,
                ci.work.1 as f32 / sf,
                ci.work.2 as f32 / sf,
                ci.work.3 as f32 / sf,
            );
            if y < wa.1 {
                y = (clk_bottom + 6.0).min(wa.3 - win_h as f32);
            }
            x = wa.0.max(x.min(wa.2 - win_w as f32 - 4.0));
            y = wa.1.max(y.min(wa.3 - win_h as f32));
        }
        None => {
            x = 32000.0;
            y = 32000.0;
        }
    }
    (x.round() as i32, y.round() as i32)
}

fn set_shown(ui: &mut Ui, show: bool) {
    if show {
        ui.shown = true;
        ui.menu_open = false;
    } else {
        ui.shown = false;
        ui.menu_open = false;
        SHOWN_FLAG.store(0, Ordering::Relaxed);
        crate::trim_working_set();
    }
}

/// 显示弹窗：窗口操作在 UI 锁之外执行（避免消息重入死锁）
fn perform_show(hwnd: HWND) {
    let (x, y) = {
        let guard = UI.lock().unwrap();
        let ui = &guard.as_ref().unwrap().0;
        let clock = ui.st.clock.lock().unwrap().clone();
        position_for(clock.as_ref(), ui.sf, ui.w as i32, ui.h as i32)
    };
    let (w, h) = (WIN_W as i32, WIN_H as i32);
    unsafe {
        SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
    let mut guard = UI.lock().unwrap();
    if let Some(sui) = guard.as_mut() {
        let ui = &mut sui.0;
        ui.shown = true;
        ui.menu_open = false;
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
                sui.0.menu_open = false;
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

// ================= 绘制 =================
impl Ui {
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
        let p = unsafe {
            Painter {
                g,
                cache: cache_ptr,
                sf: self.sf,
                w: self.w,
                h: self.h,
            }
        };
        let mut regions: Vec<(gdi::RectF, Action)> = Vec::new();
        p.clear();
        self.paint_frame(&p);
        match self.page {
            Page::Calendar => self.paint_calendar(&p, &mut regions),
            Page::Agenda => self.paint_agenda(&p, &mut regions),
        }
        if self.menu_open {
            self.paint_context_menu(&p, &mut regions);
        }
        self.regions = regions;
        self.ulw();

        if !self.dumped && !self.dump_path.is_empty() {
            self.dumped = true;
            save_bmp(self.scan0, self.w as i32, self.h as i32, &self.dump_path);
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

    fn paint_frame(&self, p: &Painter) {
        p.fill_round(10.0, 10.0, WIN_W - 20.0, WIN_H - 20.0, 12.0, BG);
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
        let pad = 18.0;
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
        p.text(&clock_str, left, 27.0, 380.0, 54.0, gdi::HALIGN_NEAR, gdi::HALIGN_NEAR, 40.0, true, false, TXT);
        let l = now.date_naive();
        let lunar = lunar::solar_to_lunar(l);
        let lunar_txt = lunar
            .map(|l| format!("{}月{}", lunar::month_cn(l.month), lunar::day_cn(l.day)))
            .unwrap_or_default();
        let date_str = format!("{}年{}月{}日", l.year(), l.month(), l.day());
        p.text(&date_str, left, 84.0, 200.0, 22.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 14.0, false, false, DATE_COL);
        let date_w = p.measure(&date_str, 14.0, false, false).0;
        p.text(&lunar_txt, left + date_w + 8.0, 84.0, 140.0, 22.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 14.0, false, false, SUB);

        // 天气（右上角，悬停弹出近一周天气面板）
        let wx = if cfg.show_weather { self.st.weather.lock().unwrap().clone() } else { None };
        if let Some(w) = &wx {
            let can_hover = !w.days.is_empty();
            if can_hover && self.hovered(&Action::Weather) {
                p.fill_round(right - 78.0, 36.0, 76.0, 58.0, 8.0, HOVER_BG);
            }
            draw_weather(p, right - 22.0, 50.0, w.code, 1.0);
            p.text(&format!("{}°C", w.temp), right - 44.0, 73.0, 44.0, 20.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 15.0, false, false, DATE_COL);
            if can_hover {
                Self::hit_add(regions, right - 78.0, 32.0, 76.0, 64.0, Action::Weather);
            }
        }

        // 分隔线
        p.fill_rect(left, 113.0, right - left, 1.0, DIVIDER);

        // 月份栏
        Self::hit_add(regions, left, 121.0, 200.0, 26.0, Action::Title);
        p.text(&format!("{}年{}月", self.view_y, self.view_m), left, 121.0, 200.0, 26.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 17.0, true, false, TITLE_COL);
        let bw = 26.0;
        let gear_x = right - bw;
        let next_x = gear_x - bw - 4.0;
        let prev_x = next_x - bw - 4.0;
        Self::hit_add(regions, prev_x, 121.0, bw, 26.0, Action::Prev);
        Self::hit_add(regions, next_x, 121.0, bw, 26.0, Action::Next);
        Self::hit_add(regions, gear_x, 121.0, bw, 26.0, Action::OpenSettings);
        for (bx, glyph, px, act) in [
            (prev_x, "‹", 20.0, Action::Prev),
            (next_x, "›", 20.0, Action::Next),
            (gear_x, "\u{E713}", 13.0, Action::OpenSettings),
        ] {
            let hov = self.hovered(&act);
            if hov {
                p.fill_round(bx, 121.0, bw, 26.0, 6.0, HOVER_BG);
            }
            p.text(glyph, bx, 121.0, bw, 26.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, px, false, glyph == "\u{E713}", if hov { WHITE } else { ICON_COL });
        }

        // 星期表头（周起始日可配置）
        let show_gutter = cfg.show_week_num;
        let gutter_w = if show_gutter { 26.0 } else { 0.0 };
        let col_w = (WIN_W - 20.0 - pad * 2.0 - gutter_w) / 7.0;
        let week_names = ["一", "二", "三", "四", "五", "六", "日"];
        let gy = 154.0;
        let ws = cfg.week_start as usize % 7;
        for c in 0..7usize {
            let wd = (ws + c) % 7;
            let cx = left + gutter_w + col_w * c as f32 + col_w / 2.0;
            p.text(week_names[wd], cx - 30.0, gy, 60.0, 20.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, WEEK_HEAD);
        }

        // 月网格
        let grid_y = 176.0;
        let row_h = ((WIN_H - 10.0 - 6.0 - 58.0 - grid_y) / 6.0).max(60.0);
        let first = NaiveDate::from_ymd_opt(self.view_y, self.view_m, 1).unwrap_or(today);
        let first_wd = first.weekday().num_days_from_monday() as i64;
        let offset = (first_wd - ws as i64 + 7) % 7;
        let start = first - Duration::days(offset);
        // 持锁借用代替 clone：重绘每秒发生，避免整表复制把堆撑大
        let holidays = self.st.holidays.read().unwrap();
        let agenda = self.agenda.lock().unwrap();

        for r in 0..6i64 {
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
                    WEEK_NUM,
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
                let has_agenda = agenda.get(&key).map(|v| !v.is_empty()).unwrap_or(false);
                Self::hit_add(regions, cx, row_top, col_w, row_h, Action::Cell(date));
                paint_day_cell(p, self, &rect, date, today, self.selected, in_month, hol, has_agenda, &cfg);
            }
        }

        // 底部工具栏
        self.paint_bottom_bar(p, regions);
    }

    fn paint_bottom_bar(&self, p: &Painter, regions: &mut Vec<(gdi::RectF, Action)>) {
        let bar_y = WIN_H - 10.0 - 58.0;
        let bar_h = 58.0;
        let w = WIN_W - 20.0;
        let slot = w / 5.0;
        p.fill_rect(10.0, bar_y, w, 1.0, DIVIDER);
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
                    p.fill_circle(cx, cy, 21.0, HOVER_BG);
                }
                p.fill_circle(cx, cy, 20.0, PLUS_BOT);
                p.fill_circle(cx, cy - 1.0, 18.5, PLUS_TOP);
                p.text("+", cx - 20.0, cy - 20.0, 40.0, 40.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 24.0, true, false, WHITE);
            } else {
                let col = if hovered { gdi::argb(255, 0xC7, 0xCD, 0xD4) } else { ICON_COL };
                if hovered {
                    p.fill_round(10.0 + slot * i as f32 + 6.0, bar_y + 6.0, slot - 12.0, bar_h - 10.0, 8.0, gdi::argb(14, 255, 255, 255));
                }
                p.text(glyph, cx - 25.0, cy - 20.0, 50.0, 22.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, *size, false, true, col);
                p.text(name, cx - 25.0, cy + 3.0, 50.0, 16.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 10.0, false, false, col);
            }
        }
    }

    fn paint_agenda(&self, p: &Painter, regions: &mut Vec<(gdi::RectF, Action)>) {
        let d = self.selected;
        let weekday = "日一二三四五六"
            .chars()
            .nth(d.weekday().num_days_from_sunday() as usize)
            .unwrap();
        Self::hit_add(regions, 20.0, 16.0, 30.0, 30.0, Action::Back);
        p.text("\u{E72B}", 20.0, 16.0, 30.0, 30.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 15.0, false, true, ICON_COL);
        p.text(&format!("{}月{}日 周{} · 日程", d.month(), d.day(), weekday), 58.0, 16.0, 320.0, 30.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 15.0, true, false, TITLE_COL);

        let key = crate::ics::key_of_date(d);
        let items = self.agenda.lock().unwrap().get(&key).cloned().unwrap_or_default();
        let n = items.len();
        let row_h = 38.0;
        let gap = 6.0;
        let max_visible = ((600.0f32 / (row_h + gap)).floor() as usize).max(1);
        let end_i = max_visible.min(n);
        for (i, text) in items[..end_i].iter().enumerate() {
            let y = 58.0 + (row_h + gap) * i as f32;
            let rx = 20.0;
            let rw = WIN_W - 20.0 - 40.0;
            p.fill_round(rx, y, rw, row_h, 8.0, gdi::argb(11, 255, 255, 255));
            p.text(text, rx + 12.0, y, rw - 50.0, row_h, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, ROW_TXT);
            let del_x = rx + rw - 32.0;
            Self::hit_add(regions, del_x, y, 26.0, row_h, Action::AgendaDel(i));
            let del_hov = self.hovered(&Action::AgendaDel(i));
            p.text("✕", del_x, y, 26.0, row_h, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, if del_hov { RED } else { WEEK_NUM });
        }
        if n == 0 {
            p.text("这一天还没有日程", 10.0, 130.0, WIN_W - 20.0, 20.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, SUB_DIM);
        } else if n > max_visible {
            p.text(&format!("共 {} 条，仅显示前 {} 条", n, max_visible), 20.0, 655.0, WIN_W - 40.0, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 10.0, false, false, SUB_DIM);
        }

        // 底部输入
        let input = gdi::RectF { x: 28.0, y: 686.0, w: WIN_W - 20.0 - 28.0 - 92.0, h: 30.0 };
        Self::hit_add(regions, input.x, input.y, input.w, input.h, Action::InputBox);
        p.fill_round(input.x, input.y, input.w, input.h, 8.0, gdi::argb(18, 255, 255, 255));
        p.stroke_round(input.x, input.y, input.w, input.h, 8.0, 1.0, gdi::argb(24, 255, 255, 255));
        let mut shown_text = self.draft.clone();
        if !self.comp.is_empty() {
            shown_text.push_str(&self.comp);
        }
        let draft_w = p.measure(&shown_text, 13.0, false, false).0;
        if shown_text.is_empty() {
            p.text("添加日程，如 14:00 项目评审", input.x + 10.0, input.y, input.w - 16.0, input.h, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, SUB_DIM);
            if self.caret_on {
                p.line(input.x + 10.0, input.y + 6.0, input.x + 10.0, input.y + input.h - 6.0, 1.0, ROW_TXT);
            }
        } else {
            p.text(&shown_text, input.x + 10.0, input.y, input.w - 16.0, input.h, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, ROW_TXT);
            if self.caret_on {
                p.line(input.x + 10.0 + draft_w + 1.0, input.y + 6.0, input.x + 10.0 + draft_w + 1.0, input.y + input.h - 6.0, 1.0, ROW_TXT);
            }
        }
        let btn = gdi::RectF { x: WIN_W - 20.0 - 84.0, y: 686.0, w: 84.0, h: 30.0 };
        Self::hit_add(regions, btn.x, btn.y, btn.w, btn.h, Action::AgendaAdd);
        let hov = self.hovered(&Action::AgendaAdd);
        p.fill_round(btn.x, btn.y, btn.w, btn.h, 8.0, if hov { gdi::argb(255, 0x53, 0x99, 0xFB) } else { BLUE });
        p.text("添加", btn.x, btn.y, btn.w, btn.h, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 13.0, false, false, WHITE);
    }


    fn paint_context_menu(&self, p: &Painter, regions: &mut Vec<(gdi::RectF, Action)>) {
        let mx = WIN_W - 206.0;
        let my = 16.0;
        let mw = 186.0;
        let row_h = 34.0;
        let autostart = self.st.config.lock().unwrap().autostart;
        let items: [(Action, String); 4] = [
            (Action::MenuShow, "显示日历".into()),
            (Action::MenuAuto, format!("开机自启{}", if autostart { " ✓" } else { "" })),
            (Action::MenuRefresh, "立即更新节假日数据".into()),
            (Action::MenuQuit, "退出".into()),
        ];
        Self::hit_add(regions, 10.0, 10.0, WIN_W - 20.0, WIN_H - 20.0, Action::None);
        p.fill_round(mx, my, mw, row_h * 4.0 + 8.0, 10.0, gdi::argb(255, 0x2A, 0x33, 0x45));
        p.stroke_round(mx, my, mw, row_h * 4.0 + 8.0, 10.0, 1.0, gdi::argb(26, 255, 255, 255));
        for (i, (act, name)) in items.iter().enumerate() {
            let y = my + 6.0 + row_h * i as f32;
            Self::hit_add(regions, mx + 6.0, y, mw - 12.0, row_h - 2.0, *act);
            let hov = self.hovered(act);
            if hov {
                p.fill_round(mx + 6.0, y, mw - 12.0, row_h - 2.0, 6.0, gdi::argb(16, 255, 255, 255));
            }
            let col = if *act == Action::MenuQuit { RED } else { ROW_TXT };
            p.text(name, mx + 18.0, y, mw - 30.0, row_h - 2.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, col);
        }
    }
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
    cfg: &Config,
) {
    let cx = rect.x + rect.w / 2.0;
    let num_cy = rect.y + rect.h * 0.24;
    let line1_cy = rect.y + rect.h * 0.52;
    let line2_cy = rect.y + rect.h * 0.78;
    let hovered = ui.hover.map(|h| matches!(h, Action::Cell(d) if d == date)).unwrap_or(false);
    if in_month && hovered {
        p.fill_round(rect.x + 2.0, rect.y + 2.0, rect.w - 4.0, rect.h - 4.0, 8.0, gdi::argb(13, 255, 255, 255));
    }

    let is_today = date == today;
    let is_selected = date == selected;
    let weekend = date.weekday().num_days_from_monday() >= 5;
    // 周末红色，其他白色
    let mut num_col = if weekend { RED } else { TXT };
    if !in_month {
        num_col = SUB_DIM;
    }
    let num = date.day().to_string();
    if is_today {
        p.fill_circle(cx, num_cy, 14.0, BLUE);
        p.text(&num, cx - 20.0, num_cy - 14.0, 40.0, 28.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 16.0, true, false, WHITE);
    } else {
        if is_selected {
            p.stroke_circle(cx, num_cy, 14.5, 1.5, BLUE);
        }
        p.text(&num, cx - 20.0, num_cy - 14.0, 40.0, 28.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 16.0, true, false, num_col);
    }

    // 次行：农历/节日/节气（可整体关闭）
    if cfg.show_lunar {
        let l = lunar::solar_to_lunar(date);
        let lf = l.as_ref().and_then(lunar::lunar_festival);
        let sfest = lunar::solar_festival(date);
        let term = lunar::jieqi_of(date);
        let legal = hol
            .filter(|h| h.ty == DayType::Xiu && (h.idx == 0 || h.len <= 4))
            .map(|h| h.name.as_str());

        let (sub, col) = if let Some((n, k)) = lf {
            (n.to_string(), fest_color(k))
        } else if let Some(n) = legal {
            (n.to_string(), LEGAL)
        } else if let Some((n, k)) = sfest {
            (n.to_string(), fest_color(k))
        } else if let Some(t) = term {
            (t.to_string(), BLUE)
        } else if let Some(l) = l {
            let txt = if l.day == 1 {
                format!("{}月", lunar::month_cn(l.month))
            } else {
                lunar::day_cn(l.day).to_string()
            };
            (txt, if in_month { SUB } else { SUB_DIM })
        } else {
            (String::new(), SUB)
        };
        if !sub.is_empty() {
            let chars: Vec<char> = sub.chars().collect();
            if chars.len() <= 5 {
                p.text(&sub, rect.x + 1.0, line1_cy - 9.0, rect.w - 2.0, 18.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, col);
            } else {
                // 超过 5 字换行：第一行 5 字，其余第二行
                let line1: String = chars[..5].iter().collect();
                let line2: String = chars[5..].iter().collect();
                p.text(&line1, rect.x + 1.0, line1_cy - 10.0, rect.w - 2.0, 16.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, col);
                p.text(&line2, rect.x + 1.0, line2_cy - 9.0, rect.w - 2.0, 16.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, col);
            }
        }
    }

    // 休/班角标（调休安排可关闭）
    if cfg.show_adjust {
        if let Some(h) = hol {
            let tag = if h.ty == DayType::Ban { "班" } else { "休" };
            let col = if h.ty == DayType::Ban { RED } else { BLUE };
            let bx = rect.x + rect.w - 12.0 - 6.5;
            let by = rect.y + 8.0 - 6.5;
            p.fill_round(bx, by, 13.0, 13.0, 3.0, col);
            p.text(tag, bx, by, 13.0, 13.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 9.0, false, false, WHITE);
        }
    }

    if has_agenda {
        p.fill_circle(cx, rect.y + rect.h - 5.0, 2.0, BLUE);
    }
}

fn fest_color(k: FestKind) -> u32 {
    match k {
        FestKind::Blue => BLUE,
        FestKind::Red => RED,
        FestKind::Legal => LEGAL,
    }
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
                p.line(cx + i as f32 * 7.0 * sc, cy + 6.0 * sc, cx + i as f32 * 7.0 * sc - 2.0 * sc, cy + 12.0 * sc, 2.0, RAIN);
            }
        }
        71..=77 | 85 | 86 => {
            draw_cloud(p, cx, cy - 5.0 * sc, 1.05 * sc);
            for i in -1..=1 {
                p.fill_circle(cx + i as f32 * 7.0 * sc, cy + 9.0 * sc, 1.8 * sc, CLOUD);
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
                SUN,
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
    p.fill_circle(cx, cy, 7.0 * s, SUN);
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::TAU / 8.0;
        let (dx, dy) = (a.sin(), -a.cos());
        p.line(cx + dx * (r / p.sf + 2.5), cy + dy * (r / p.sf + 2.5), cx + dx * (r / p.sf + 5.5), cy + dy * (r / p.sf + 5.5), 2.0 * s, SUN);
    }
}

fn draw_cloud(p: &Painter, cx: f32, cy: f32, s: f32) {
    p.fill_circle(cx - 6.0 * s, cy + 1.0 * s, 6.0 * s, CLOUD);
    p.fill_circle(cx + 1.0 * s, cy - 2.0 * s, 7.5 * s, CLOUD);
    p.fill_circle(cx + 7.0 * s, cy + 2.0 * s, 5.5 * s, CLOUD);
    p.fill_round(cx - 10.0 * s, cy + 2.0 * s, 21.0 * s, 5.0 * s, 3.5 * s, CLOUD);
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

pub fn load_agenda() -> HashMap<String, Vec<String>> {
    let path = crate::config::data_dir().join("agenda.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_agenda(map: &HashMap<String, Vec<String>>) {
    let path = crate::config::data_dir().join("agenda.json");
    if let Ok(text) = serde_json::to_string(map) {
        let _ = std::fs::write(path, text);
    }
}

// ================= 交互 =================
impl Ui {
    fn handle_action(&mut self, action: &Action) {
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
            }
            Action::Title | Action::BottomToday => {
                let t = Local::now().date_naive();
                self.view_y = t.year();
                self.view_m = t.month();
                self.selected = t;
                self.page = Page::Calendar;
                self.redraw();
            }
            Action::Cell(d) => {
                self.selected = *d;
                // 点击日期：打开/切换日期侧边栏；再次点击同一日期收起
                if crate::sidebar::sidebar_visible() && crate::sidebar::sidebar_date() == Some(*d) {
                    crate::sidebar::sidebar_hide();
                } else {
                    crate::sidebar::sidebar_show(*d);
                }
                self.redraw();
            }
            Action::BottomAgenda | Action::BottomPlus => {
                self.page = Page::Agenda;
                self.redraw();
            }
            Action::BottomExit | Action::MenuQuit => {
                unsafe {
                    DestroyWindow(self.hwnd as HWND);
                }
            }
            Action::Back => {
                self.page = Page::Calendar;
                self.redraw();
            }
            Action::AgendaDel(i) => {
                let key = crate::ics::key_of_date(self.selected);
                let mut a = self.agenda.lock().unwrap();
                if let Some(v) = a.get_mut(&key) {
                    v.remove(*i);
                    if v.is_empty() {
                        a.remove(&key);
                    }
                }
                drop(a);
                save_agenda(&self.agenda.lock().unwrap());
                crate::sidebar::sidebar_repaint();
                self.redraw();
            }
            Action::AgendaAdd => {
                self.add_agenda();
            }
            Action::OpenSettings | Action::BottomSettings => {
                // 窗口操作必须在 UI 锁之外执行（见 WM_LBUTTONDOWN）：
                // ShowWindow 激活设置窗口会同步触发本面板 WM_ACTIVATE 重入加锁 → 死锁
            }
            Action::None => {}
            Action::MenuShow => {
                self.menu_open = false;
                self.redraw();
            }
            Action::MenuRefresh => {
                let _ = self.st.refresh_tx.send(());
            }
            Action::MenuAuto => {
                let on = self.st.config.lock().unwrap().autostart;
                {
                    let mut cfg = self.st.config.lock().unwrap();
                    cfg.autostart = !on;
                    cfg.save();
                }
                apply_autostart(!on);
                if let Some(t) = self.tray.lock().unwrap().as_ref() {
                    t.autostart_item.set_checked(!on);
                }
                self.redraw();
            }
            Action::InputBox | Action::Weather => {}
        }
    }

    fn add_agenda(&mut self) {
        let text = self.draft.trim().to_string();
        if text.is_empty() {
            return;
        }
        let key = crate::ics::key_of_date(self.selected);
        self.agenda.lock().unwrap().entry(key).or_default().push(text);
        save_agenda(&self.agenda.lock().unwrap());
        crate::sidebar::sidebar_repaint();
        self.draft.clear();
        self.comp.clear();
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
}

#[link(name = "gdiplus")]
extern "system" {
    fn GdipCreateBitmapFromScan0(w: i32, h: i32, stride: i32, format: i32, scan0: *mut u8, bitmap: *mut gdi::Gp) -> i32;
    fn GdipGetImageGraphicsContext(image: gdi::Gp, graphics: *mut gdi::Gp) -> i32;
    fn GdipSetSmoothingMode(graphics: gdi::Gp, mode: i32) -> i32;
    fn GdipSetTextRenderingHint(graphics: gdi::Gp, mode: i32) -> i32;
    fn GdipDeleteGraphics(graphics: gdi::Gp) -> i32;
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            ValidateRect(hwnd, std::ptr::null_mut());
            0
        }
        WM_ERASEBKGND => 1,
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
                }
            }
            if let Some(a) = hit {
                let mut exit = false;
                let mut open_settings = false;
                {
                    let mut guard = UI.lock().unwrap();
                    if let Some(sui) = guard.as_mut() {
                        let ui = &mut sui.0;
                        if matches!(a, Action::BottomExit | Action::MenuQuit) {
                            exit = true;
                        } else if matches!(a, Action::OpenSettings | Action::BottomSettings) {
                            // 设置窗口为 NOACTIVATE：不抢焦点，面板保持打开
                            ui.page = Page::Calendar;
                            ui.menu_open = false;
                            ui.hover = None;
                            ui.redraw();
                            open_settings = true;
                        } else {
                            ui.handle_action(&a);
                        }
                    }
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
                            let clickable = matches!(a, Some(Action::Cell(_)) | Some(Action::Prev) | Some(Action::Next) | Some(Action::Title) | Some(Action::BottomAgenda) | Some(Action::BottomToday) | Some(Action::BottomPlus) | Some(Action::BottomSettings) | Some(Action::BottomExit) | Some(Action::AgendaDel(_)) | Some(Action::AgendaAdd) | Some(Action::OpenSettings) | Some(Action::Back) | Some(Action::MenuShow) | Some(Action::MenuAuto) | Some(Action::MenuRefresh) | Some(Action::MenuQuit));
                            SetCursor(if clickable { LoadCursorW(std::ptr::null_mut(), IDC_HAND) } else { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) });
                        }
                        fc_open = a == Some(Action::Weather);
                    }
                }
            }
            // 窗口操作在 UI 锁之外执行（防消息重入死锁）；面板常驻，失焦不关闭
            if fc_open {
                forecast_open(hwnd as usize);
            }
            0
        }
        WM_KEYDOWN => {
            let mut hide = false;
            {
                let mut guard = UI.lock().unwrap();
                if let Some(sui) = guard.as_mut() {
                    let ui = &mut sui.0;
                    match wp as i32 {
                        0x1B => {
                            if ui.menu_open {
                                ui.menu_open = false;
                                ui.redraw();
                            } else if ui.page != Page::Calendar {
                                ui.page = Page::Calendar;
                                ui.redraw();
                            } else {
                                ui.shown = false;
                                hide = true;
                            }
                        }
                        0x08 => {
                            if ui.page == Page::Agenda {
                                ui.draft.pop();
                                ui.redraw();
                            }
                        }
                        0x56 => {
                            if (GetKeyState(0x11) as u16) & 0x8000 != 0 && ui.page == Page::Agenda {
                                paste_clipboard(ui);
                            }
                        }
                        0x0D => {
                            if ui.page == Page::Agenda {
                                ui.add_agenda();
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
            }
            0
        }
        WM_CHAR => {
            let mut guard = UI.lock().unwrap();
            if let Some(sui) = guard.as_mut() {
                let ui = &mut sui.0;
                if ui.page == Page::Agenda && (wp as u32) >= 0x20 {
                    if let Some(ch) = char::from_u32(wp as u32) {
                        ui.draft.push(ch);
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
                            ui.draft.push_str(&s);
                            ui.comp.clear();
                        }
                    } else if lp as u32 & 0x0008 != 0 {
                        ui.comp = read_ime_string(hwnd, 0x0008).unwrap_or_default();
                    }
                    ui.redraw();
                }
            }
            0
        }
                WM_ACTIVATE => {
            let low = (wp & 0xFFFF) as u16;
            // 设置窗口打开期间面板不随失焦隐藏（二者共存）；
            // 光标位于软件自身弹窗（天气侧栏/设置等）上时同样保持显示：
            // 只有点击发生在软件相关窗口之外才关闭日历
            if low == 0 && !settings_visible() && !cursor_on_own_popup() {
                let hide = {
                    let mut guard = UI.lock().unwrap();
                    let mut hide = false;
                    if let Some(sui) = guard.as_mut() {
                        let ui = &mut sui.0;
                        if ui.shown {
                            ui.shown = false;
                            ui.menu_open = false;
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
            if wp == 2 {
                perform_show(hwnd);
                let mut guard = UI.lock().unwrap();
                if let Some(sui) = guard.as_mut() {
                    sui.0.menu_open = true;
                    sui.0.redraw();
                }
            } else if shown {
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
            ui.draft.push_str(&String::from_utf16_lossy(slice));
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
