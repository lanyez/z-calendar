//! 右键菜单：右键任务栏时钟时在鼠标位置弹出的独立小窗口
//! （软件设置 / 开机自启 / 日期与时间 / 退出Z日历）。
//! 窗口创建、摆放、绘制与悬停处理见 menuwin；本模块只提供菜单内容与各项动作
use std::sync::{Arc, Mutex};

use winapi::shared::windef::HWND;
use winapi::um::winuser::DestroyWindow;

use crate::flyout::SharedState;
use crate::menuwin::{self, Menu, MenuRow};
use crate::tray;

/// 菜单动作需要的句柄（主线程创建窗口时登记）
struct Ctx {
    st: SharedState,
    tray: Arc<Mutex<Option<tray::Tray>>>,
}

// 托盘图标句柄不是 Send：仅主线程经 CTX 访问
unsafe impl Send for Ctx {}

static CTX: Mutex<Option<Ctx>> = Mutex::new(None);

/// 带勾选列（开机自启打勾）；点菜单空白处不收起
static MENU: Menu = Menu::new(rows, on_pick, false, true);

/// 行内容每次绘制现取（切换开机自启后原地刷新勾选状态）
fn rows() -> Vec<MenuRow> {
    let autostart = CTX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .map(|c| c.st.config.lock().unwrap_or_else(std::sync::PoisonError::into_inner).autostart)
        .unwrap_or(false);
    vec![
        MenuRow::new("软件设置", menuwin::row_text),
        MenuRow::new("开机自启", menuwin::row_text).checked(autostart),
        MenuRow::new("日期与时间", menuwin::row_text),
        MenuRow::new("退出Z日历", menuwin::danger),
    ]
}

/// 选中回调（主线程、不持菜单自身的锁）
fn on_pick(i: usize) {
    match i {
        0 => {
            MENU.close();
            crate::flyout::show_settings();
        }
        1 => {
            // 开机自启：写配置 + 注册表 Run；托盘菜单项同步打勾
            {
                let ctx = CTX.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(c) = ctx.as_ref() {
                    let on = {
                        let mut cfg = c.st.config.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        cfg.autostart = !cfg.autostart;
                        cfg.save();
                        cfg.autostart
                    };
                    crate::config::apply_autostart(on);
                    if let Some(t) = c.tray.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref() {
                        t.autostart_item.set_checked(on);
                    }
                }
            }
            // 菜单原地刷新勾选状态。必须在 CTX 锁释放之后：重绘会重新取行（再次锁 CTX）
            MENU.repaint();
        }
        2 => {
            MENU.close();
            crate::flyout::open_date_time_settings();
        }
        _ => {
            MENU.close();
            unsafe {
                DestroyWindow(crate::flyout::hwnd() as HWND);
            }
        }
    }
}

/// 菜单屏幕矩形（未显示时返回 None）；钩子线程据此判断点击是否落在菜单内
pub fn rect() -> Option<(i32, i32, i32, i32)> {
    MENU.rect()
}

/// 钩子线程用：在鼠标位置打开/收起（只 PostMessage，理由见 menuwin 模块说明）
pub fn request_toggle(x: i32, y: i32) {
    MENU.post_toggle(x, y);
}

/// 钩子线程用：收起菜单（只 PostMessage）
pub fn post_close() {
    MENU.post_close();
}

pub fn create_window(st: SharedState, tray: Arc<Mutex<Option<tray::Tray>>>) {
    *CTX.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Ctx { st, tray });
    MENU.create("Z日历菜单");
}

/// 界面自检（见 selftest 模块）：渲染一帧并转储（第 2 行悬停），窗口不显示
#[cfg(feature = "selftest")]
pub fn debug_render(dir: &std::path::Path) {
    MENU.debug_render(dir, "50-menu-clock", Some(1));
}
