//! 重复日程/待办的删除选择菜单：✕ 点击重复条目时在鼠标位置弹出
//! （仅删除这一天 = 追加例外日期 / 删除整个系列 = 移除主条目）。
//! 窗口创建、摆放、绘制与悬停处理见 menuwin；本模块只提供菜单内容与动作
use std::sync::Mutex;

use chrono::NaiveDate;

use crate::menuwin::{self, Menu, MenuRow};

/// 点菜单空白处收起；无勾选列
static MENU: Menu = Menu::new(rows, on_pick, true, false);

fn rows() -> Vec<MenuRow> {
    vec![MenuRow::new("仅删除这一天", menuwin::row_text), MenuRow::new("删除整个系列", menuwin::danger)]
}

/// 待操作目标：日程（原 key/下标 + 出现日期）或待办（全局下标 + 出现日期）
#[derive(Clone, Debug)]
pub enum RmTarget {
    Agenda { key: String, idx: usize, date: NaiveDate },
    Todo { gi: usize, date: NaiveDate },
}

static TARGET: Mutex<Option<RmTarget>> = Mutex::new(None);

/// 应用后刷新相关界面（同为主线程窗口，直接重绘）
fn refresh() {
    crate::sidebar::sidebar_repaint();
    crate::flyout::flyout_repaint();
}

/// 选中回调（主线程、不持菜单自身的锁）
fn on_pick(i: usize) {
    let target = TARGET.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    MENU.close();
    let (this_day, series) = (i == 0, i == 1);
    match target {
        Some(RmTarget::Agenda { key, idx, date }) if this_day => {
            if let Some(a) = crate::events::shared_agenda() {
                crate::events::edit_and_save(&a, |map| {
                    crate::events::agenda_skip_day(map, &key, idx, date);
                });
                refresh();
            }
        }
        Some(RmTarget::Agenda { key, idx, .. }) if series => {
            if let Some(a) = crate::events::shared_agenda() {
                let removed = crate::events::edit_and_save(&a, |map| {
                    let entry = map.get(&key).and_then(|v| v.get(idx)).cloned();
                    let removed = crate::events::agenda_remove_at(map, &key, idx);
                    (entry, removed)
                });
                if removed.1 {
                    if let Some(entry) = removed.0 {
                        let body = crate::events::display(&entry);
                        crate::toast::notify_undo(crate::toast::UndoData::Agenda { key, idx, entry }, &body);
                    }
                }
                refresh();
            }
        }
        Some(RmTarget::Todo { gi, date }) if this_day => {
            crate::sidebar::todo_skip_day(gi, date);
            refresh();
        }
        Some(RmTarget::Todo { gi, .. }) if series => {
            if let Some(td) = crate::sidebar::todo_at(gi) {
                let body = td.text.clone();
                crate::sidebar::remove_todo_at(gi);
                crate::toast::notify_undo(crate::toast::UndoData::Todos { items: vec![(gi, td)] }, &body);
            }
            refresh();
        }
        _ => {}
    }
}

/// 在鼠标位置打开删除选择菜单（已显示则收起）
pub fn open(x: i32, y: i32, target: RmTarget) {
    if MENU.visible() {
        MENU.close();
    }
    *TARGET.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(target);
    MENU.show(x, y);
}

pub fn create_window() {
    MENU.create("Z日历重复菜单");
}

/// 界面自检（见 selftest 模块）：渲染一帧并转储（第 2 行悬停），窗口不显示
#[cfg(feature = "selftest")]
pub fn debug_render(dir: &std::path::Path) {
    MENU.debug_render(dir, "52-menu-recur", Some(1));
}
