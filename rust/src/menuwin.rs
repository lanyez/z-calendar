//! 通用弹出菜单小窗：NOACTIVATE 分层窗口，宽 186、行高 34、圆角 10。
//! 任务栏时钟右键菜单（ctxmenu）、日期右键菜单（ctxmenu_date）、重复条目删除范围
//! 菜单（recur_menu）共用——此前三处各抄了一份窗口创建、摆放、绘制与悬停处理。
//!
//! 线程规则：菜单窗口属主线程。低级鼠标钩子所在线程只能 PostMessage
//! （post_toggle / post_close）——跨线程 ShowWindow 是同步调用，会拖住钩子线程
//! （全系统鼠标输入随之停顿），反复超时后钩子被系统摘除
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::windef::{HWND, POINT};
use winapi::um::libloaderapi::GetModuleHandleW;
use winapi::um::winuser::*;

use crate::gdi::{self, Dib};

/// 菜单宽度（逻辑 px）
pub const W: f32 = 186.0;
/// 行高（逻辑 px）
pub const ROW: f32 = 34.0;
/// 上下留白
const PAD: f32 = 4.0;
const RADIUS: f32 = 10.0;

/// 主线程消息：在 (x, y) 打开/收起（lp = x | y << 16）；收起
const MSG_TOGGLE: UINT = 0x8000 + 0x40;
const MSG_CLOSE: UINT = 0x8000 + 0x41;

/// 行文字颜色：随主题现算（深浅色切换后重绘即生效）
type ColorFn = fn() -> u32;

/// 正文色
pub fn row_text() -> u32 {
    crate::theme::pal().row
}

/// 危险操作色（删除 / 退出）
pub fn danger() -> u32 {
    crate::theme::pal().red
}

/// 一行菜单项
#[derive(Clone)]
pub struct MenuRow {
    pub label: String,
    pub color: ColorFn,
    /// 左侧勾选列是否打勾（仅带勾选列的菜单显示）
    pub checked: bool,
}

impl MenuRow {
    pub fn new(label: &str, color: ColorFn) -> Self {
        MenuRow { label: label.to_string(), color, checked: false }
    }

    pub fn checked(mut self, on: bool) -> Self {
        self.checked = on;
        self
    }
}

struct MenuUi {
    /// 创建/最近重建位图时的 sf 与高度（与当前值不符则重建）
    sf: f32,
    h: f32,
    surf: Dib,
    cache: gdi::Cache,
    /// 行热区 → 行下标（paint 时更新）
    regions: Vec<(gdi::RectF, usize)>,
    hover: Option<usize>,
}

/// 一个菜单实例。用法：`static MENU: Menu = Menu::new(rows, on_pick, ...);`
pub struct Menu {
    hwnd: AtomicUsize,
    /// 显示中的屏幕矩形（钩子线程据此判断点击是否落在菜单内）
    rect: Mutex<Option<(i32, i32, i32, i32)>>,
    /// 仅主线程访问
    ui: Mutex<Option<MenuUi>>,
    /// 每次绘制现取行（菜单内的操作——如切换开机自启——之后原地刷新）
    rows: fn() -> Vec<MenuRow>,
    /// 选中回调（主线程、不持菜单自身的锁）
    on_pick: fn(usize),
    /// 点击菜单空白处是否收起
    close_on_blank: bool,
    /// 是否绘制左侧勾选列（有则行文字右移让位）
    check_col: bool,
}

static CLASS_DONE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
/// 已创建的菜单实例（窗口过程按句柄找回所属菜单）
static MENUS: Mutex<Vec<&'static Menu>> = Mutex::new(Vec::new());

impl Menu {
    pub const fn new(rows: fn() -> Vec<MenuRow>, on_pick: fn(usize), close_on_blank: bool, check_col: bool) -> Menu {
        Menu {
            hwnd: AtomicUsize::new(0),
            rect: Mutex::new(None),
            ui: Mutex::new(None),
            rows,
            on_pick,
            close_on_blank,
            check_col,
        }
    }

    /// 注册窗口类并创建（隐藏的）菜单窗口。仅主线程调用
    pub fn create(&'static self, title: &str) {
        unsafe {
            CLASS_DONE.get_or_init(|| {
                let class_name = crate::wide("z-calendar-menu");
                let mut wc: WNDCLASSW = std::mem::zeroed();
                wc.lpfnWndProc = Some(wndproc);
                wc.hInstance = GetModuleHandleW(std::ptr::null_mut());
                wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
                wc.lpszClassName = class_name.as_ptr();
                RegisterClassW(&wc);
            });
            let cls = crate::wide("z-calendar-menu");
            let title = crate::wide(title);
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_NOACTIVATE,
                cls.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                32000,
                32000,
                gdi::phys(W) as i32,
                gdi::phys(PAD * 2.0 + ROW * 2.0) as i32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null_mut()),
                std::ptr::null_mut(),
            );
            if hwnd.is_null() {
                return;
            }
            self.hwnd.store(hwnd as usize, Ordering::Relaxed);
            // 绘制状态：位图在首次 redraw 时按当时的缩放惰性分配
            *self.ui.lock().unwrap_or_else(PoisonError::into_inner) = Some(MenuUi {
                sf: 0.0,
                h: 0.0,
                surf: Dib::empty(),
                cache: gdi::Cache::new(),
                regions: Vec::new(),
                hover: None,
            });
            MENUS.lock().unwrap_or_else(PoisonError::into_inner).push(self);
        }
    }

    pub fn visible(&self) -> bool {
        let h = self.hwnd.load(Ordering::Relaxed) as HWND;
        !h.is_null() && unsafe { IsWindowVisible(h) != 0 }
    }

    /// 显示中的屏幕矩形（未显示时 None）。任意线程可调（钩子回调用）
    pub fn rect(&self) -> Option<(i32, i32, i32, i32)> {
        if !self.visible() {
            return None;
        }
        *self.rect.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 在屏幕坐标 (x, y) 处打开；超出工作区时向左/上翻转。仅主线程调用
    pub fn show(&self, x: i32, y: i32) {
        let h = self.hwnd.load(Ordering::Relaxed) as HWND;
        if h.is_null() {
            return;
        }
        let hgt = PAD * 2.0 + ROW * (self.rows)().len().max(1) as f32;
        let (mw, mh) = (gdi::phys(W) as i32, gdi::phys(hgt) as i32);
        let mut wa = (0, 0, x + mw, y + mh);
        let mon = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
        if !mon.is_null() {
            let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if unsafe { GetMonitorInfoW(mon, &mut mi) } != 0 {
                wa = (mi.rcWork.left, mi.rcWork.top, mi.rcWork.right, mi.rcWork.bottom);
            }
        }
        let mut mx = x;
        let mut my = y;
        if mx + mw > wa.2 {
            mx = x - mw;
        }
        if my + mh > wa.3 {
            my = y - mh;
        }
        mx = mx.max(wa.0);
        my = my.max(wa.1);
        unsafe {
            SetWindowPos(h, HWND_TOPMOST, mx, my, mw, mh, SWP_NOACTIVATE);
        }
        *self.rect.lock().unwrap_or_else(PoisonError::into_inner) = Some((mx, my, mx + mw, my + mh));
        unsafe {
            ShowWindow(h, SW_SHOWNA);
        }
        self.redraw(h);
    }

    /// 收起并释放后台位图。仅主线程调用；钩子线程用 post_close
    pub fn close(&self) {
        let h = self.hwnd.load(Ordering::Relaxed) as HWND;
        if h.is_null() || unsafe { IsWindowVisible(h) == 0 } {
            return;
        }
        unsafe {
            ShowWindow(h, SW_HIDE);
        }
        *self.rect.lock().unwrap_or_else(PoisonError::into_inner) = None;
        if let Some(ui) = self.ui.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
            ui.surf.release();
            ui.hover = None;
        }
        crate::trim_working_set();
    }

    /// 重绘（行内容或悬停状态变化后）。仅主线程调用
    pub fn repaint(&self) {
        let h = self.hwnd.load(Ordering::Relaxed) as HWND;
        if !h.is_null() {
            self.redraw(h);
        }
    }

    /// 钩子线程用：请求在 (x, y) 处打开/收起
    pub fn post_toggle(&self, x: i32, y: i32) {
        let h = self.hwnd.load(Ordering::Relaxed) as HWND;
        if h.is_null() {
            return;
        }
        let lp = ((((y as i16 as u16) as usize) << 16) | (x as i16 as u16) as usize) as LPARAM;
        unsafe {
            PostMessageW(h, MSG_TOGGLE, 0, lp);
        }
    }

    /// 钩子线程用：请求收起
    pub fn post_close(&self) {
        let h = self.hwnd.load(Ordering::Relaxed) as HWND;
        if !h.is_null() {
            unsafe {
                PostMessageW(h, MSG_CLOSE, 0, 0);
            }
        }
    }

    fn redraw(&self, hwnd: HWND) {
        let rows = (self.rows)();
        let h = PAD * 2.0 + ROW * rows.len().max(1) as f32;
        let sf = gdi::scale();
        let mut guard = self.ui.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(ui) = guard.as_mut() else { return };
        // 屏幕缩放变化或高度变化：按新尺寸重建位图（菜单窗口常驻，位图惰性重建）
        if (ui.sf - sf).abs() > 0.001 || ui.h != h || !ui.surf.is_alloc() {
            ui.sf = sf;
            ui.h = h;
            ui.surf = Dib::alloc(W, h);
        }
        let p = ui.surf.painter(&ui.cache, ui.sf, W, h);
        ui.regions.clear();
        p.clear();
        p.fill_round(0.0, 0.0, W, h, RADIUS, crate::theme::pal().popup);
        p.stroke_round(0.0, 0.0, W, h, RADIUS, 1.0, crate::theme::ov(26));
        for (i, row) in rows.iter().enumerate() {
            let y = PAD + ROW * i as f32;
            ui.regions.push((gdi::RectF { x: 6.0, y, w: W - 12.0, h: ROW - 2.0 }, i));
            if ui.hover == Some(i) {
                p.fill_round(6.0, y, W - 12.0, ROW - 2.0, 6.0, crate::theme::ov(16));
            }
            let (tx, tw) = if self.check_col {
                p.text(
                    if row.checked { "✓" } else { "" },
                    10.0,
                    y,
                    16.0,
                    ROW - 2.0,
                    gdi::HALIGN_CENTER,
                    gdi::HALIGN_CENTER,
                    12.0,
                    false,
                    false,
                    crate::theme::pal().blue,
                );
                (30.0, W - 42.0)
            } else {
                (18.0, W - 30.0)
            };
            p.text(&row.label, tx, y, tw, ROW - 2.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, (row.color)());
        }
        ui.surf.present(hwnd as usize, ui.sf, W, h);
    }

    /// 鼠标位置 → 命中的行下标
    fn hit(&self, px: i32, py: i32) -> Option<usize> {
        let guard = self.ui.lock().unwrap_or_else(PoisonError::into_inner);
        let ui = guard.as_ref()?;
        hit_in(&ui.regions, px as f32 / ui.sf, py as f32 / ui.sf)
    }

    fn set_hover(&self, hover: Option<usize>) {
        let changed = {
            let mut guard = self.ui.lock().unwrap_or_else(PoisonError::into_inner);
            match guard.as_mut() {
                Some(ui) if ui.hover != hover => {
                    ui.hover = hover;
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.repaint();
        }
    }
}

fn hit_in(regions: &[(gdi::RectF, usize)], x: f32, y: f32) -> Option<usize> {
    // 逆序：后绘制的行优先（行热区不重叠，结果唯一）
    regions
        .iter()
        .rev()
        .find(|(r, _)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
        .map(|(_, i)| *i)
}

fn x_of(lp: LPARAM) -> i32 {
    ((lp as usize) & 0xFFFF) as u16 as i16 as i32
}

fn y_of(lp: LPARAM) -> i32 {
    (((lp as usize) >> 16) as u16 as i16) as i32
}

/// 按窗口句柄找回所属菜单（引用是 'static，返回后不再持注册表锁）
fn menu_for(hwnd: HWND) -> Option<&'static Menu> {
    let menus = MENUS.lock().unwrap_or_else(PoisonError::into_inner);
    menus.iter().copied().find(|m| m.hwnd.load(Ordering::Relaxed) as HWND == hwnd)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: UINT, _wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(menu) = menu_for(hwnd) else {
        return unsafe { DefWindowProcW(hwnd, msg, _wp, lp) };
    };
    unsafe {
        match msg {
            WM_PAINT => {
                ValidateRect(hwnd, std::ptr::null_mut());
                0
            }
            WM_ERASEBKGND => 1,
            // 点击菜单不改变激活状态（主面板不因失焦隐藏）
            WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
            MSG_TOGGLE => {
                if menu.visible() {
                    menu.close();
                } else {
                    menu.show(x_of(lp), y_of(lp));
                }
                0
            }
            MSG_CLOSE => {
                menu.close();
                0
            }
            WM_MOUSEMOVE => {
                menu.set_hover(menu.hit(x_of(lp), y_of(lp)));
                // 注册离开跟踪：移出菜单后清除悬停高亮
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut tme);
                0
            }
            WM_MOUSELEAVE => {
                menu.set_hover(None);
                0
            }
            WM_LBUTTONDOWN => match menu.hit(x_of(lp), y_of(lp)) {
                Some(i) => {
                    (menu.on_pick)(i);
                    0
                }
                None => {
                    if menu.close_on_blank {
                        menu.close();
                    }
                    0
                }
            },
            _ => DefWindowProcW(hwnd, msg, _wp, lp),
        }
    }
}

#[cfg(feature = "selftest")]
impl Menu {
    /// 界面自检（见 selftest 模块）：渲染一帧（不显示）并转储；hover 指定悬停行
    pub fn debug_render(&self, dir: &std::path::Path, name: &str, hover: Option<usize>) {
        let h = self.hwnd.load(Ordering::Relaxed) as HWND;
        if h.is_null() {
            return;
        }
        if let Some(ui) = self.ui.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut() {
            ui.hover = hover;
        }
        self.redraw(h);
        let guard = self.ui.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(ui) = guard.as_ref() {
            crate::debug::save_frame(dir, name, &ui.surf, W, ui.h, ui.sf);
        }
    }
}
