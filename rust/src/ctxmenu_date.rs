//! 日期右键菜单（新增日程 / 新增待办）：右键月历日期格时弹出。
//! 窗口创建、摆放、绘制与悬停处理见 menuwin；本模块只提供菜单内容与动作
use std::sync::Mutex;

use chrono::NaiveDate;

use crate::menuwin::{self, Menu, MenuRow};

/// 点菜单空白处收起；无勾选列
static MENU: Menu = Menu::new(rows, on_pick, true, false);

/// 右键的那个日期与屏幕位置（点击菜单项时打开对应的编辑弹窗）
static TARGET: Mutex<Option<NaiveDate>> = Mutex::new(None);
static AT: Mutex<(i32, i32)> = Mutex::new((0, 0));

fn rows() -> Vec<MenuRow> {
    vec![MenuRow::new("新增日程", menuwin::row_text), MenuRow::new("新增待办", menuwin::row_text)]
}

/// 选中回调（主线程、不持菜单自身的锁）
fn on_pick(i: usize) {
    let date = TARGET.lock().unwrap_or_else(std::sync::PoisonError::into_inner).unwrap_or_else(|| chrono::Local::now().date_naive());
    let (ax, ay) = *AT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    MENU.close();
    match i {
        0 => crate::inputbox::open(ax, ay, date, crate::inputbox::Kind::Agenda),
        _ => crate::inputbox::open(ax, ay, date, crate::inputbox::Kind::Todo),
    }
}

/// 菜单屏幕矩形（未显示时返回 None）；钩子线程据此判断点击是否落在菜单内
pub fn date_menu_rect() -> Option<(i32, i32, i32, i32)> {
    MENU.rect()
}

/// 钩子线程用：收起菜单（只 PostMessage，理由见 menuwin 模块说明）
pub fn date_menu_post_close() {
    MENU.post_close();
}

/// 在日期格右键位置打开菜单（已显示则收起）
pub fn date_menu_toggle(x: i32, y: i32, date: NaiveDate) {
    if MENU.visible() {
        MENU.close();
        return;
    }
    *TARGET.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(date);
    *AT.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = (x, y);
    MENU.show(x, y);
}

pub fn create_date_menu_window() {
    MENU.create("Z日历日期菜单");
}

/// 界面自检（见 selftest 模块）：渲染一帧并转储（第 1 行悬停），窗口不显示
#[cfg(feature = "selftest")]
pub fn debug_render(dir: &std::path::Path) {
    MENU.debug_render(dir, "51-menu-date", Some(0));
}
