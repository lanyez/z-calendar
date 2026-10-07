//! 日期侧边栏：点击日历日期时在日历左侧弹出，按设置显示卡片
//! （日期信息 / 黄历信息 / 最近事件 / 今日日程 / 历史上的今天 / 时间格言 / 待办清单）
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{Datelike, Duration, Local, NaiveDate, Timelike};
use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::windef::{HWND, POINT, RECT, SIZE};
use winapi::um::winuser::*;

use crate::almanac;
use crate::events::AgendaMap;
use crate::flyout::SharedState;
use crate::gdi::{self, Cache, Painter};
use crate::ics::{key_of_date, DayType};

pub const SB_W: f32 = 400.0;

/// 日程卡片一屏可见行数（更多靠滚轮）
const AGENDA_VISIBLE: usize = 5;
/// 待办卡片一屏可见行数（更多靠滚轮）
const TODO_VISIBLE: usize = 5;

/// 历史上的今天后台拉取完成：请求侧栏重绘（线程安全）
const WM_APP_HISTORY: UINT = 0x8000 + 1;

static SIDEBAR_HWND: AtomicUsize = AtomicUsize::new(0);
static SIDEBAR_UI: Mutex<Option<SendSb>> = Mutex::new(None);

struct SendSb(Box<SidebarUi>);
unsafe impl Send for SendSb {}

#[derive(Clone, Copy, PartialEq, Debug)]
enum SbAction {
    CardManage,
    /// 全部使用全局列表下标（todos_for/todos() 的 enumerate 序号）
    TodoToggle(usize),
    TodoEdit(usize),
    TodoDelete(usize),
    /// 逾期待办顺延到今天（全局下标）
    TodoPostpone(usize),
    /// 逾期未完成待办一键全部顺延到今天
    TodoPostponeAll,
    /// 清除所有已完成待办
    TodoClearDone,
    /// 日程行：agenda_rows 的下标（含按天重复展开的行），行体点击/✎ 都打开编辑
    AgendaEdit(usize),
    AgendaDelete(usize),
}

struct SidebarUi {
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
    agenda: Arc<Mutex<AgendaMap>>,
    date: Option<NaiveDate>,
    regions: Vec<(gdi::RectF, SbAction)>,
    hover: Option<SbAction>,
    dumped: bool,
    dump_path: String,
    // ---- 卡片滚动（滚轮作用于光标所在卡片） ----
    /// 日程卡片：滚动偏移 / 条目总数 / 列表区 y 范围
    agenda_scroll: usize,
    agenda_count: usize,
    agenda_band: (f32, f32),
    /// 待办卡片：滚动偏移 / 当日条目总数 / 列表区 y 范围
    todo_scroll: usize,
    todo_count: usize,
    todo_band: (f32, f32),
    /// 本次绘制解析出的日程行（含按天重复展开）：(原key, 原下标, 条目)
    agenda_rows: Vec<(String, usize, crate::events::AgendaEntry)>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Todo {
    /// 稳定 id（提醒去重用；旧条目为空）
    #[serde(default)]
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub done: bool,
    /// 归属日期（Y-M-D，与日程 key 同格式）
    #[serde(default)]
    pub date: Option<String>,
    /// 优先级：0=收集箱 1=重要且紧急 2=重要但不紧急 3=紧急但不重要 4=不重要不紧急
    #[serde(default)]
    pub priority: u8,
    /// 是否带有时间（无时间待办只显示内容）
    #[serde(default)]
    pub has_time: bool,
    /// "%Y-%m-%d %H:%M"
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    /// 提前提醒分钟数（None=不提醒，0=准时）
    #[serde(default)]
    pub remind: Option<i64>,
    /// 重复间隔分钟（None=单次）
    #[serde(default)]
    pub repeat: Option<i64>,
    /// 按天重复：d=每天 w=每周（同星期几） m=每月（同几号） y=每年 l=农历每年；None=不按天重复
    #[serde(default)]
    pub recur: Option<String>,
    /// 重复截止日期（"%Y-%m-%d"，含当天）；None=无限重复
    #[serde(default)]
    pub recur_until: Option<String>,
    /// “仅此次”删除/修改产生的例外日期
    #[serde(default)]
    pub skip_dates: Vec<String>,
    /// 按天重复待办的完成记录（已完成日子的 Y-M-D key；主条目 done 恒为 false）
    #[serde(default)]
    pub done_dates: Vec<String>,
}

static TODOS: Mutex<Option<Vec<Todo>>> = Mutex::new(None);

/// 缓存未加载时先读盘，避免写操作覆盖已有数据（损坏时自动从 .bak 恢复）
fn cached_list() -> Vec<Todo> {
    let mut g = TODOS.lock().unwrap();
    if g.is_none() {
        let path = crate::config::data_dir().join("todo.json");
        let (list, healed) = crate::config::load_json_or_bak::<Vec<Todo>>(&path);
        if healed {
            crate::toast::notify("待办数据已恢复", "todo.json 损坏，已自动从备份恢复");
        }
        *g = Some(list.unwrap_or_default());
    }
    g.clone().unwrap()
}

fn todos() -> Vec<Todo> {
    cached_list()
}

fn save_todos(list: &[Todo]) {
    let path = crate::config::data_dir().join("todo.json");
    if let Ok(text) = serde_json::to_string_pretty(list) {
        crate::config::backup_file(&path);
        let _ = std::fs::write(path, text);
    }
}

/// 新增待办（日期右键“新增待办”弹窗），并请求日历重绘。
/// 注意：不能在持有 flyout UI 锁的上下文调用（flyout_repaint 会重入 UI 锁死锁）——
/// 日程页底部快捷添加请用 add_todo_quiet 后自行补 sidebar_repaint/redraw。
pub fn add_todo_full(todo: Todo) {
    add_todo_quiet(todo);
    crate::flyout::flyout_repaint();
}

/// 新增待办（全局列表），不触发任何窗口重绘（调用方自行刷新）
pub fn add_todo_quiet(todo: Todo) {
    if todo.text.trim().is_empty() {
        return;
    }
    let mut todo = todo;
    if todo.id.is_empty() {
        todo.id = crate::events::gen_id();
    }
    let cached = cached_list();
    let mut g = TODOS.lock().unwrap();
    let list = g.get_or_insert_with(|| cached);
    list.push(todo);
    save_todos(list);
}

/// 全局下标取待办（编辑弹窗预填用）
pub fn todo_at(gi: usize) -> Option<Todo> {
    todos().into_iter().nth(gi)
}

/// 某日实际生效的待办（下标对应全局列表中的顺序）。
/// 重复待办在命中日子展开一条（尊重截止日与“仅此次”例外），done 反映当天的完成记录；
/// 列表按 时间→优先级 排序。
pub fn todos_for(key: &str) -> Vec<(usize, Todo)> {
    let date = chrono::NaiveDate::parse_from_str(key, "%Y-%m-%d").ok();
    let mut out: Vec<(usize, Todo)> = todos()
        .into_iter()
        .enumerate()
        .filter_map(|(i, mut t)| {
            if t.date.as_deref() == Some(key) {
                if let Some(rc) = &t.recur {
                    // 锚点日：同样尊重截止日/“仅此次”例外
                    let Some(anchor) = t
                        .date
                        .as_deref()
                        .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
                    else {
                        return None;
                    };
                    let Some(d) = date else { return None };
                    if !crate::events::recur_occurs(t.recur_until.as_deref(), &t.skip_dates, rc, anchor, d) {
                        return None;
                    }
                    // 锚点日本身也按当天完成记录显示
                    t.done = t.done_dates.iter().any(|k| k == key);
                }
                return Some((i, t));
            }
            if let (Some(rc), Some(anchor), Some(d)) = (&t.recur, t.date.as_deref().and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()), date) {
                if crate::events::recur_occurs(t.recur_until.as_deref(), &t.skip_dates, rc, anchor, d) {
                    t.done = t.done_dates.iter().any(|k| k == key);
                    return Some((i, t));
                }
            }
            None
        })
        .collect();
    out.sort_by(|a, b| todo_sort_key(&a.1).cmp(&todo_sort_key(&b.1)));
    out
}

/// 待办排序键：未完成在前 → 带时间的按时间升序在前 → 无时间的（随时做）在后 → 优先级（收集箱垫底）
/// （sort_by 为稳定排序，同键保持录入顺序）
fn todo_sort_key(t: &Todo) -> (u8, u8, u32, u8) {
    let done = t.done as u8;
    let untimed = (!t.has_time) as u8;
    let mins = t
        .start
        .as_deref()
        .filter(|_| t.has_time)
        .and_then(crate::events::parse_start)
        .map(|d| d.hour() as u32 * 60 + d.minute() as u32)
        .unwrap_or(0);
    let prio = match t.priority {
        0 => 5, // 收集箱（未分类）垫底
        p => p,
    };
    (done, untimed, mins, prio)
}

/// 是否存在某日的未完成待办（日历格角标）
pub fn has_todo(key: &str) -> bool {
    todos().iter().any(|t| t.date.as_deref() == Some(key) && !t.done)
}

/// 含未完成待办的日期集合（日历格角标），范围 [a, b]；
/// 重复待办按规则展开（尊重截止日/“仅此次”例外，当天已完成的除外）。
pub fn todo_keys_between(a: NaiveDate, b: NaiveDate) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    for t in todos() {
        if t.done && t.recur.is_none() {
            continue;
        }
        let anchor = t
            .date
            .as_deref()
            .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
        if let (Some(anchor), Some(rc)) = (anchor, &t.recur) {
            // 重复：展开到范围内（含锚点日，尊重截止日/例外）
            let from = anchor.max(a);
            let mut d = from;
            while d <= b {
                if crate::events::recur_occurs(t.recur_until.as_deref(), &t.skip_dates, rc, anchor, d) {
                    let k = crate::ics::key_of_date(d);
                    if !t.done_dates.iter().any(|x| *x == k) {
                        set.insert(k);
                    }
                }
                d += chrono::Duration::days(1);
            }
        } else if let Some(d) = anchor {
            if !t.done && d >= a && d <= b {
                set.insert(crate::ics::key_of_date(d));
            }
        }
    }
    set
}

/// 逾期未完成的待办（归属日期早于今天），按日期升序。
/// 注意：date 存储为不补零的 "Y-M-D"（key_of 格式），不能直接字符串比较，须解析成日期。
/// 按天重复的待办不会逾期（永远落在当天之后的日子上）。
pub fn overdue_todos() -> Vec<(usize, Todo)> {
    let today = chrono::Local::now().date_naive();
    let mut out: Vec<(usize, Todo)> = todos()
        .into_iter()
        .enumerate()
        .filter(|(_, t)| {
            t.recur.is_none()
                && !t.done
                && t.date
                    .as_deref()
                    .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                    .map(|d| d < today)
                    .unwrap_or(false)
        })
        .collect();
    out.sort_by(|a, b| {
        let ka = a
            .1
            .date
            .as_deref()
            .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
        let kb = b
            .1
            .date
            .as_deref()
            .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
        ka.cmp(&kb).then_with(|| todo_sort_key(&a.1).cmp(&todo_sort_key(&b.1)))
    });
    out
}

/// 切换待办完成状态（全局下标）。
/// 按天重复的待办按“日子”记录完成（done_dates），普通待办直接翻 done 位。
pub fn toggle_todo_on(gi: usize, date_key: &str) {
    let cached = cached_list();
    let mut g = TODOS.lock().unwrap();
    let list = g.get_or_insert_with(|| cached);
    if let Some(td) = list.get_mut(gi) {
        if td.recur.is_some() {
            if let Some(pos) = td.done_dates.iter().position(|k| k == date_key) {
                td.done_dates.remove(pos);
            } else {
                td.done_dates.push(date_key.to_string());
            }
        } else {
            td.done = !td.done;
        }
    }
    save_todos(list);
    drop(g);
    crate::flyout::flyout_repaint();
}

/// 删除待办（全局下标），并请求日历重绘。
/// 注意：不能在持有 flyout UI 锁的上下文调用（flyout_repaint 会重入 UI 锁死锁）——
/// 日程页合并列表的删除请用 remove_todo_at_quiet 后自行补 sidebar_repaint/redraw。
pub fn remove_todo_at(gi: usize) {
    remove_todo_at_quiet(gi);
    crate::flyout::flyout_repaint();
}

/// 删除待办（全局下标），不触发任何窗口重绘（调用方自行刷新）
pub fn remove_todo_at_quiet(gi: usize) {
    let cached = cached_list();
    let mut g = TODOS.lock().unwrap();
    let list = g.get_or_insert_with(|| cached);
    if gi < list.len() {
        list.remove(gi);
        save_todos(list);
    }
}

/// 编辑替换待办（全局下标；done/id 沿用原条目）
pub fn update_todo_at(gi: usize, mut td: Todo) {
    let cached = cached_list();
    let mut g = TODOS.lock().unwrap();
    let list = g.get_or_insert_with(|| cached);
    if let Some(old) = list.get(gi) {
        td.done = old.done;
        if td.id.is_empty() {
            td.id = old.id.clone();
        }
        list[gi] = td;
        save_todos(list);
    }
    drop(g);
    crate::flyout::flyout_repaint();
}

/// 逾期待办顺延到今天（全局下标；按天重复的待办没有逾期概念，忽略）
pub fn postpone_todo_to_today(gi: usize) {
    let cached = cached_list();
    let mut g = TODOS.lock().unwrap();
    let list = g.get_or_insert_with(|| cached);
    if let Some(td) = list.get_mut(gi) {
        if td.recur.is_none() {
            td.date = Some(crate::ics::key_of_date(chrono::Local::now().date_naive()));
            save_todos(list);
        }
    }
    drop(g);
    crate::flyout::flyout_repaint();
}

/// 一键清除所有已完成待办（按天重复的完成记录不受影响）。
/// 返回被清除的 (原全局下标, 条目) 列表（供撤销恢复）
pub fn clear_done_todos() -> Vec<(usize, Todo)> {
    let cached = cached_list();
    let mut g = TODOS.lock().unwrap();
    let list = g.get_or_insert_with(|| cached);
    let mut removed: Vec<(usize, Todo)> = Vec::new();
    let mut kept: Vec<Todo> = Vec::new();
    for (i, t) in list.drain(..).enumerate() {
        if t.done && t.recur.is_none() {
            removed.push((i, t));
        } else {
            kept.push(t);
        }
    }
    *list = kept;
    let changed = !removed.is_empty();
    if changed {
        save_todos(list);
    }
    drop(g);
    if changed {
        crate::flyout::flyout_repaint();
    }
    removed
}

/// 撤销删除：把待办按原全局下标插回（按下标从大到小依次插入，避免位移）。
/// 不触发窗口重绘（调用方自行刷新）
pub fn restore_todos(items: &[(usize, Todo)]) {
    if items.is_empty() {
        return;
    }
    let cached = cached_list();
    let mut g = TODOS.lock().unwrap();
    let list = g.get_or_insert_with(|| cached);
    let mut sorted: Vec<&(usize, Todo)> = items.iter().collect();
    sorted.sort_by(|a, b| b.0.cmp(&a.0));
    for (gi, td) in sorted {
        let mut td = td.clone();
        if td.id.is_empty() {
            td.id = crate::events::gen_id();
        }
        let pos = (*gi).min(list.len());
        list.insert(pos, td);
    }
    save_todos(list);
}

/// 逾期待办一键全部顺延到今天（全局下标列表），返回顺延条数。
/// 不触发窗口重绘（调用方自行刷新）
pub fn postpone_overdue_all(gis: &[usize]) -> usize {
    if gis.is_empty() {
        return 0;
    }
    let today = crate::ics::key_of_date(chrono::Local::now().date_naive());
    let cached = cached_list();
    let mut g = TODOS.lock().unwrap();
    let list = g.get_or_insert_with(|| cached);
    let mut n = 0;
    for &gi in gis {
        if let Some(td) = list.get_mut(gi) {
            if td.recur.is_none() && td.date.as_deref() != Some(today.as_str()) {
                td.date = Some(today.clone());
                n += 1;
            }
        }
    }
    if n > 0 {
        save_todos(list);
    }
    n
}

/// 是否存在已完成的待办（“清除已完成”按钮显隐）
pub fn has_done_todos() -> bool {
    todos().iter().any(|t| t.done && t.recur.is_none())
}

/// 提醒卡片「完成」按钮：按 id 定位待办标记完成（重复待办按日子记录完成）
pub fn complete_todo_by_id(id: &str, date_key: &str) {
    if id.is_empty() {
        return;
    }
    let cached = cached_list();
    let mut g = TODOS.lock().unwrap();
    let list = g.get_or_insert_with(|| cached);
    let mut changed = false;
    for td in list.iter_mut() {
        if td.id == id {
            if td.recur.is_some() {
                if !td.done_dates.iter().any(|k| k == date_key) {
                    td.done_dates.push(date_key.to_string());
                    changed = true;
                }
            } else if !td.done {
                td.done = true;
                changed = true;
            }
            break;
        }
    }
    if changed {
        save_todos(list);
    }
    drop(g);
    if changed {
        crate::flyout::flyout_repaint();
    }
}

/// 「仅删除这一天」：给重复待办追加例外日期（该天不再出现）
pub fn todo_skip_day(gi: usize, date: chrono::NaiveDate) -> bool {
    let cached = cached_list();
    let mut g = TODOS.lock().unwrap();
    let list = g.get_or_insert_with(|| cached);
    let mut changed = false;
    if let Some(td) = list.get_mut(gi) {
        if td.recur.is_some() && !crate::events::recur_skipped(&td.skip_dates, date) {
            td.skip_dates.push(crate::ics::key_of_date(date));
            changed = true;
        }
    }
    if changed {
        save_todos(list);
    }
    drop(g);
    if changed {
        crate::flyout::flyout_repaint();
    }
    changed
}

/// 「仅修改这一天」（编辑保存）：给重复待办追加例外日期并可顺带更新截止日
pub fn todo_patch_recur(gi: usize, until: Option<String>, skip: Option<chrono::NaiveDate>) {
    let cached = cached_list();
    let mut g = TODOS.lock().unwrap();
    let list = g.get_or_insert_with(|| cached);
    let mut changed = false;
    if let Some(td) = list.get_mut(gi) {
        if td.recur.is_some() {
            if td.recur_until != until {
                td.recur_until = until;
                changed = true;
            }
            if let Some(d) = skip {
                if !crate::events::recur_skipped(&td.skip_dates, d) {
                    td.skip_dates.push(crate::ics::key_of_date(d));
                    changed = true;
                }
            }
        }
    }
    if changed {
        save_todos(list);
    }
    drop(g);
    if changed {
        crate::flyout::flyout_repaint();
    }
}

/// 历史上的今天：内置精选事件（月, 日, 年, 事件）
const HISTORY_EVENTS: &[(u32, u32, i32, &str)] = &[
    (1, 1, 1912, "中华民国成立，孙中山就任临时大总统"),
    (1, 1, 1979, "中美两国正式建立外交关系"),
    (1, 11, 1851, "洪秀全金田起义，太平天国运动开始"),
    (1, 27, 1756, "奥地利作曲家莫扎特诞辰"),
    (1, 28, 1932, "日军进攻上海，一·二八事变爆发"),
    (2, 7, 1923, "京汉铁路工人大罢工（二七大罢工）"),
    (2, 12, 1912, "清帝溥仪退位，清朝灭亡"),
    (2, 19, 1473, "波兰天文学家哥白尼诞辰"),
    (2, 21, 1848, "马克思、恩格斯《共产党宣言》发表"),
    (3, 5, 1898, "周恩来诞辰"),
    (3, 10, 1876, "贝尔成功进行第一次电话通话"),
    (3, 12, 1925, "孙中山逝世"),
    (3, 14, 1879, "爱因斯坦诞辰"),
    (3, 14, 1883, "马克思逝世"),
    (3, 22, 1895, "卢米埃尔兄弟在巴黎首次放映电影"),
    (4, 5, 1975, "蒋介石在台北病逝"),
    (4, 12, 1927, "蒋介石发动四一二反革命政变"),
    (4, 15, 1912, "泰坦尼克号沉没"),
    (4, 18, 1906, "美国旧金山大地震"),
    (4, 21, 1900, "传说中的罗马建城日"),
    (4, 22, 1870, "列宁诞辰"),
    (4, 23, 1564, "英国剧作家莎士比亚诞辰"),
    (4, 24, 1970, "中国第一颗人造卫星东方红一号发射成功"),
    (5, 1, 1886, "芝加哥工人大罢工，国际劳动节由来"),
    (5, 3, 1928, "日军制造济南惨案（五三惨案）"),
    (5, 4, 1919, "五四运动爆发"),
    (5, 5, 1818, "马克思诞辰"),
    (5, 12, 2008, "四川汶川发生8.0级大地震"),
    (5, 14, 1948, "以色列宣布建国"),
    (5, 30, 1925, "五卅惨案发生"),
    (6, 3, 1839, "林则徐虎门销烟"),
    (6, 5, 1967, "第三次中东战争爆发"),
    (6, 6, 1944, "盟军在诺曼底登陆，开辟第二战场"),
    (6, 11, 1898, "光绪帝颁布《定国是诏》，戊戌变法开始"),
    (6, 15, 1215, "英国国王约翰签署《大宪章》"),
    (6, 17, 1900, "八国联军攻占大沽炮台"),
    (6, 22, 1941, "德国突袭苏联，苏德战争爆发"),
    (6, 26, 1945, "《联合国宪章》签署"),
    (6, 28, 1914, "萨拉热窝事件，第一次世界大战导火索"),
    (7, 1, 1921, "中国共产党成立"),
    (7, 1, 1997, "中国政府对香港恢复行使主权"),
    (7, 7, 1937, "卢沟桥事变，全民族抗战爆发"),
    (7, 11, 1405, "郑和率船队首次下西洋"),
    (7, 14, 1789, "巴黎人民攻占巴士底狱"),
    (7, 16, 1945, "人类历史上第一颗原子弹试爆成功"),
    (7, 25, 1894, "丰岛海战爆发，甲午战争开始"),
    (7, 27, 1953, "朝鲜停战协定在板门店签署"),
    (7, 28, 1976, "河北唐山发生7.8级大地震"),
    (8, 1, 1927, "南昌起义，人民军队诞生"),
    (8, 6, 1945, "美国在广岛投下原子弹"),
    (8, 8, 2008, "第29届夏季奥运会在北京开幕"),
    (8, 13, 1937, "八一三事变，淞沪会战爆发"),
    (8, 15, 1945, "日本宣布无条件投降"),
    (8, 22, 1904, "邓小平诞辰"),
    (8, 26, 1789, "法国制宪会议通过《人权宣言》"),
    (9, 2, 1945, "日本签署无条件投降书"),
    (9, 3, 1945, "中国人民抗日战争胜利纪念日"),
    (9, 7, 1901, "清政府签订《辛丑条约》"),
    (9, 9, 1976, "毛泽东逝世"),
    (9, 10, 1985, "中国第一个教师节"),
    (9, 18, 1931, "九一八事变爆发"),
    (9, 21, 1898, "慈禧发动戊戌政变，变法失败"),
    (9, 25, 1937, "八路军取得平型关大捷"),
    (9, 27, 1825, "世界第一条铁路在英国通车"),
    (10, 1, 1949, "中华人民共和国中央人民政府成立"),
    (10, 10, 1911, "武昌起义爆发，辛亥革命开始"),
    (10, 16, 1964, "中国第一颗原子弹爆炸成功"),
    (10, 24, 1945, "联合国正式成立"),
    (10, 25, 1971, "中华人民共和国恢复在联合国的一切合法权利"),
    (11, 7, 1917, "俄国十月革命胜利"),
    (11, 12, 1866, "孙中山诞辰"),
    (11, 24, 1859, "达尔文《物种起源》出版"),
    (12, 9, 1935, "一二·九运动爆发"),
    (12, 12, 1936, "张学良、杨虎城发动西安事变"),
    (12, 13, 1937, "南京大屠杀，30多万同胞遇难"),
    (12, 14, 1799, "美国首任总统华盛顿逝世"),
    (12, 25, 1642, "英国物理学家牛顿诞辰"),
    (12, 26, 1893, "毛泽东诞辰"),
];

fn history_of(m: u32, d: u32) -> Vec<(i32, &'static str)> {
    HISTORY_EVENTS
        .iter()
        .filter(|(hm, hd, _, _)| *hm == m && *hd == d)
        .map(|(_, _, y, t)| (*y, *t))
        .collect()
}

/// 时间格言兜底（一言 API 拉取中/失败时按日期轮换显示）
const MOTTOS: &[(&str, &str)] = &[
    ("一寸光阴一寸金，寸金难买寸光阴。", "《增广贤文》"),
    ("逝者如斯夫，不舍昼夜。", "《论语》"),
    ("人生天地之间，若白驹之过隙，忽然而已。", "《庄子》"),
    ("少壮不努力，老大徒伤悲。", "《长歌行》"),
    ("盛年不重来，一日难再晨。", "陶渊明"),
    ("莫等闲，白了少年头，空悲切。", "岳飞《满江红》"),
    ("黑发不知勤学早，白首方悔读书迟。", "颜真卿"),
    ("明日复明日，明日何其多。", "钱福《明日歌》"),
    ("光阴似箭，日月如梭。", "《增广贤文》"),
    ("一年之计在于春，一日之计在于晨。", "《增广贤文》"),
    ("时间就是性命。无端的空耗别人的时间，其实是无异于谋财害命的。", "鲁迅"),
    ("合理安排时间，就等于节约时间。", "培根"),
    ("完成工作的方法是爱惜每一分钟。", "达尔文"),
    ("时间是人类发展的空间。", "马克思"),
    ("天才就是这样，终身努力便成天才。", "门捷列夫"),
    ("任何节约归根到底是时间的节约。", "列宁"),
];

pub fn sidebar_hwnd() -> usize {
    SIDEBAR_HWND.load(Ordering::Relaxed)
}

pub fn sidebar_visible() -> bool {
    let h = SIDEBAR_HWND.load(Ordering::Relaxed);
    h != 0 && unsafe { IsWindowVisible(h as HWND) != 0 }
}

pub fn sidebar_date() -> Option<NaiveDate> {
    let mut g = SIDEBAR_UI.lock().unwrap();
    if let Some(f) = g.as_mut() {
        f.0.date
    } else {
        None
    }
}

/// 主日历窗口可见边缘（窗口内 10px）的左缘 x，供天气面板重新锚定
pub fn sidebar_left_x() -> Option<i32> {
    let h = SIDEBAR_HWND.load(Ordering::Relaxed);
    if h != 0 && unsafe { IsWindowVisible(h as HWND) != 0 } {
        unsafe {
            let mut r: RECT = std::mem::zeroed();
            GetWindowRect(h as HWND, &mut r);
            return Some(r.left);
        }
    }
    None
}

pub fn sidebar_hide() {
    let h = SIDEBAR_HWND.load(Ordering::Relaxed);
    if h != 0 && unsafe { IsWindowVisible(h as HWND) != 0 } {
        unsafe {
            ShowWindow(h as HWND, SW_HIDE);
        }
        // 释放后台位图压缩内存（下次 sidebar_show 重绘时重建）
        let mut guard = SIDEBAR_UI.lock().unwrap();
        if let Some(f) = guard.as_mut() {
            let ui = &mut f.0;
            unsafe {
                gdi::free_dib(&mut ui.mem_dc, &mut ui.hbmp, &mut ui.bmp, &mut ui.g, &mut ui.scan0);
            }
        }
        crate::trim_working_set();
    }
}

/// 屏幕缩放变化：更新 sf、释放位图；可见时按主面板重新锚定（等价重新打开）
pub fn rescale(sf: f32) {
    let date;
    {
        let mut guard = SIDEBAR_UI.lock().unwrap();
        let Some(f) = guard.as_mut() else { return };
        let ui = &mut f.0;
        ui.sf = sf;
        date = ui.date;
        unsafe {
            gdi::free_dib(&mut ui.mem_dc, &mut ui.hbmp, &mut ui.bmp, &mut ui.g, &mut ui.scan0);
        }
    }
    if sidebar_visible() {
        sidebar_hide();
        if let Some(d) = date {
            sidebar_show(d); // 重新定位/定高/重绘（惰性重建位图）
        }
    }
}

pub fn sidebar_repaint() {
    let h = SIDEBAR_HWND.load(Ordering::Relaxed);
    if h != 0 && unsafe { IsWindowVisible(h as HWND) != 0 } {
        let mut guard = SIDEBAR_UI.lock().unwrap();
        if let Some(f) = guard.as_mut() {
            f.0.redraw();
        }
    }
}

/// 供后台线程投递重绘请求（绘制始终在侧栏窗口线程执行）
pub fn post_repaint() {
    let h = SIDEBAR_HWND.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            PostMessageW(h as HWND, WM_APP_HISTORY, 0, 0);
        }
    }
}

pub fn sidebar_show(date: NaiveDate) {
    unsafe {
        // 时间格言：互联网分类每次打开都要换一条
        crate::motto::on_sidebar_show();
        // 天气面板与侧栏同屏（不再互斥）：侧栏打开时天气面板自动左移让位
        let mut guard = SIDEBAR_UI.lock().unwrap();
        let Some(f) = guard.as_mut() else { return };
        let f = &mut f.0;
        f.date = Some(date);
        // 位置：紧贴主日历可见左缘、顶部对齐
        let mh = crate::flyout::hwnd();
        if mh == 0 {
            return;
        }
        let mut mr: RECT = std::mem::zeroed();
        GetWindowRect(mh as HWND, &mut mr);
        // 主窗口矩形为物理像素，偏移与侧栏宽度按 sf 换算
        let x = mr.left + gdi::phys(10.0) as i32 - gdi::phys(SB_W) as i32;
        let y = mr.top + gdi::phys(10.0) as i32;
        // 高度与日历可见高度一致（换回逻辑坐标存入 f.h）
        f.h = (((mr.bottom - mr.top) as f32 / f.sf) - 20.0).max(300.0);
        SetWindowPos(f.hwnd as HWND, HWND_TOPMOST, x, y, gdi::phys(SB_W) as i32, gdi::phys(f.h) as i32, SWP_NOACTIVATE);
        ShowWindow(f.hwnd as HWND, SW_SHOWNA);
        f.redraw();
    }
}

pub fn create_window(st: SharedState, agenda: Arc<Mutex<AgendaMap>>) {
    unsafe {
        let cls = crate::wide("z-calendar-sidebar");
        let hinstance = winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null());
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(sidebar_wndproc);
        wc.hInstance = hinstance;
        wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        wc.lpszClassName = cls.as_ptr();
        RegisterClassW(&wc);

        let w = gdi::phys(SB_W) as i32;
        let h = gdi::phys(716.0) as i32;
        let title = crate::wide("Z日历详情");
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
        SIDEBAR_HWND.store(hwnd as usize, Ordering::Relaxed);

        let mut sui = Box::new(SidebarUi {
            hwnd: hwnd as usize,
            sf: gdi::scale(),
            w: SB_W,
            h: 716.0,
            mem_dc: 0,
            hbmp: 0,
            bmp: std::ptr::null_mut(),
            scan0: std::ptr::null_mut(),
            g: std::ptr::null_mut(),
            cache: Cache::new(),
            st,
            agenda,
            date: None,
            regions: Vec::new(),
            hover: None,
            dumped: false,
            dump_path: std::env::var("CAL_DUMP4").unwrap_or_default(),
            agenda_scroll: 0,
            agenda_count: 0,
            agenda_band: (0.0, 0.0),
            todo_scroll: 0,
            todo_count: 0,
            todo_band: (0.0, 0.0),
            agenda_rows: Vec::new(),
        });
        // 后台位图不在创建时分配：sidebar_show→redraw 惰性分配，隐藏即释放
        SIDEBAR_UI.lock().unwrap().replace(SendSb(sui));
    }
}

impl SidebarUi {
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
                    PostMessageW(crate::flyout::hwnd() as HWND, WM_CLOSE, 0, 0);
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

    fn enabled_cards(&self) -> Vec<&'static str> {
        // 按设置中的卡片顺序（侧栏管理可拖动调整），过滤出开启的卡片
        let cfg = self.st.config.lock().unwrap();
        cfg.sidebar_card_order()
            .into_iter()
            .filter(|id| cfg.sidebar_enabled(id))
            .collect()
    }

    fn almanac_height(&self, p: &Painter, date: NaiveDate) -> f32 {
        let zhi = almanac::jianzhi(date);
        let (yi, ji) = almanac::jianzhi_yiji(zhi);
        let max_w = SB_W - 32.0 - 28.0 - 30.0 - 32.0;
        let l1 = wrap_terms(p, yi, max_w).len() as f32;
        let l2 = wrap_terms(p, ji, max_w).len() as f32;
        8.0 + (l1 * 16.0).max(22.0) + 5.0 + (l2 * 16.0).max(22.0) + 8.0
    }

    fn agenda_height(&self, date: NaiveDate) -> f32 {
        let n = crate::events::agenda_on(&self.agenda.lock().unwrap(), date).len();
        if n == 0 {
            44.0
        } else {
            8.0 + (1 + n.min(AGENDA_VISIBLE)) as f32 * 20.0
                + if n > AGENDA_VISIBLE { 16.0 } else { 0.0 }
                + 8.0
        }
    }

    fn motto_height(&self, p: &Painter, date: NaiveDate) -> f32 {
        let (quote, _) = self.motto_content(date);
        let lines = wrap_chars(p, &quote, SB_W - 32.0 - 28.0).len() as f32;
        8.0 + lines * 18.0 + 5.0 + 16.0 + 8.0
    }

    /// 时间格言：一言 API 结果，拉取中/失败回退到内置格言
    fn motto_content(&self, date: NaiveDate) -> (String, String) {
        let ty = self.st.config.lock().unwrap().motto_type.clone();
        if let Some(m) = crate::motto::current(&ty) {
            return (m.text, m.from);
        }
        let (q, a) = motto_of(date);
        (q.to_string(), a.to_string())
    }

    fn paint(&mut self, p: &Painter) {
        p.clear();
        let date = match self.date {
            Some(d) => d,
            None => return,
        };
        let today = Local::now().date_naive();
        let cards = self.enabled_cards();
        self.regions.clear();
        self.agenda_rows.clear();

        // 页面底
        p.fill_round(0.0, 0.0, SB_W, self.h, 12.0, BG_PAGE());

        let mut y = 8.0;
        let cx = 16.0;
        let cw = SB_W - 32.0;
        for c in &cards {
            match *c {
                "date" => {
                    self.paint_date_card(p, cx, y, cw, date, today);
                    y += 84.0 + 8.0;
                }
                "almanac" => {
                    let h = self.almanac_height(p, date);
                    self.paint_almanac_card(p, cx, y, cw, h, date);
                    y += h + 8.0;
                }
                "events" => {
                    p.fill_round(cx, y, cw, 44.0, 10.0, POPUP_BG());
                    let text = self.next_event_text(date, today);
                    p.text(&text, cx, y, cw, 44.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, SUB());
                    y += 44.0 + 8.0;
                }
                "agenda" => {
                    let h = self.agenda_height(date);
                    self.paint_agenda_card(p, cx, y, cw, h, date);
                    y += h + 8.0;
                }
                "history" => {
                    // 任意日期：月缓存命中直接显示；未命中触发后台拉取，
                    // 期间/失败时用内置精选事件兜底
                    let cached = crate::history::load_for(date);
                    if cached.is_none() {
                        crate::history::ensure_fetched(date);
                    }
                    let mut items: Vec<(i32, String)> = match cached {
                        Some(v) =>
                            // 缓存数据已按重要性排好序，直接使用
                            v.into_iter().map(|it| (it.year, it.title)).collect(),
                        None => {
                            let mut v: Vec<(i32, String)> = history_of(date.month(), date.day())
                                .into_iter()
                                .map(|(y, t)| (y, t.to_string()))
                                .collect();
                            v.sort_by(|a, b| b.0.cmp(&a.0));
                            v
                        }
                    };
                    if items.is_empty() {
                        p.fill_round(cx, y, cw, 44.0, 10.0, POPUP_BG());
                        p.text("暂无记录", cx, y, cw, 44.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, SUB());
                        y += 44.0 + 8.0;
                    } else {
                        let n = items.len().min(3);
                        let h = 8.0 + 18.0 + n as f32 * 18.0 + 8.0;
                        p.fill_round(cx, y, cw, h, 10.0, POPUP_BG());
                        p.text("历史上的今天", cx + 14.0, y + 8.0, cw - 28.0, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, true, false, TITLE_COL());
                        for (i, (y2, t)) in items.iter().take(3).enumerate() {
                            let ytxt = if *y2 < 0 { format!("公元前{}年", -y2) } else { format!("{}年", y2) };
                            let line = format!("{}：{}", ytxt, t);
                            p.text(&line, cx + 14.0, y + 28.0 + i as f32 * 18.0, cw - 28.0, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 11.5, false, false, ROW_TXT());
                        }
                        y += h + 8.0;
                    }
                }
                "motto" => {
                    let h = self.motto_height(p, date);
                    self.paint_motto_card(p, cx, y, cw, h, date);
                    y += h + 8.0;
                }
                "todo" => {
                    let key = crate::ics::key_of_date(date);
                    let is_today = date == today;
                    let todos = todos_for(&key);
                    // 逾期未完成（仅在查看今天时列出，可顺延到今天）
                    let overdue = if is_today { overdue_todos() } else { Vec::new() };
                    let over_rows = overdue.len().min(3);
                    let n = todos.len();
                    self.todo_count = n;
                    let more = n > TODO_VISIBLE;
                    let past = date < today; // 查看过去的日期：未完成待办标红
                    let list_h = if todos.is_empty() { 18.0 } else { TODO_VISIBLE as f32 * 20.0 };
                    let over_h = if over_rows > 0 { over_rows as f32 * 20.0 + 4.0 } else { 0.0 };
                    let more_h = if more { 16.0 } else { 0.0 };
                    let h = 8.0 + 18.0 + 4.0 + over_h + list_h + more_h + 8.0;
                    p.fill_round(cx, y, cw, h, 10.0, POPUP_BG());
                    p.text("待办清单", cx + 14.0, y + 8.0, cw - 28.0, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, true, false, TITLE_COL());
                    // 逾期一键全部顺延（存在逾期时显示，位于“清除已完成”左侧）
                    if !overdue.is_empty() {
                        let bhov = self.hover == Some(SbAction::TodoPostponeAll);
                        p.text("全部顺延", cx + cw - 168.0, y + 9.0, 70.0, 18.0, gdi::HALIGN_FAR, gdi::HALIGN_CENTER, 10.5, false, false, if bhov { BLUE() } else { RED() });
                        self.regions.push((gdi::RectF { x: cx + cw - 174.0, y: y + 6.0, w: 80.0, h: 22.0 }, SbAction::TodoPostponeAll));
                    }
                    // 清除已完成（存在已完成条目时显示）
                    if has_done_todos() {
                        let bhov = self.hover == Some(SbAction::TodoClearDone);
                        p.text("清除已完成", cx + cw - 84.0, y + 9.0, 70.0, 18.0, gdi::HALIGN_FAR, gdi::HALIGN_CENTER, 10.5, false, false, if bhov { BLUE() } else { SUB_DIM() });
                        self.regions.push((gdi::RectF { x: cx + cw - 90.0, y: y + 6.0, w: 80.0, h: 22.0 }, SbAction::TodoClearDone));
                    }
                    let mut ry = y + 28.0;
                    if todos.is_empty() && over_rows == 0 {
                        p.text("暂无待办", cx, ry, cw, 18.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, SUB());
                        ry += 18.0;
                    }
                    // 逾期区（红字，带原日期前缀；“→”顺延到今天）
                    for (gi, td) in overdue.iter().take(3) {
                        let (oy, od) = td
                            .date
                            .as_deref()
                            .map(|d| (d.get(5..7).unwrap_or("").trim_start_matches('0'), d.get(8..10).unwrap_or("").trim_start_matches('0')))
                            .unwrap_or(("", ""));
                        let label = format!("{}-{} {}", oy, od, td.text);
                        self.paint_todo_row(p, cx, cw, ry, *gi, td, &label, true, true);
                        ry += 20.0;
                    }
                    if over_rows > 0 {
                        ry += 4.0;
                    }
                    // 当日待办（可滚动）
                    self.todo_band = (ry, ry + list_h);
                    let off = self.todo_scroll.min(n.saturating_sub(TODO_VISIBLE));
                    for (vi, (gi, td)) in todos.iter().enumerate().skip(off).take(TODO_VISIBLE) {
                        let label = if td.recur.is_some() { format!("{} ↻", td.text) } else { td.text.clone() };
                        self.paint_todo_row(p, cx, cw, ry, *gi, td, &label, past, false);
                        ry += 20.0;
                    }
                    if more {
                        p.text(&format!("共 {} 条 · 滚轮查看更多", n), cx + 14.0, ry, cw - 28.0, 16.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 10.5, false, false, SUB_DIM());
                    }
                    y += h + 8.0;
                }
                _ => {}
            }
        }

        // 卡片管理
        p.text("卡片管理", cx, y, cw, 24.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 11.5, false, false, BLUE());
        self.regions.push((gdi::RectF { x: cx, y, w: cw, h: 24.0 }, SbAction::CardManage));
    }

    /// 待办单行：勾选圈 + 优先级色点 + 文本（可带前缀）+（逾期的加“→”顺延）+ 编辑/删除
    fn paint_todo_row(&mut self, p: &Painter, cx: f32, cw: f32, ry: f32, gi: usize, td: &Todo, label: &str, red: bool, overdue: bool) {
        let col = if td.done { SUB_DIM() } else if red { RED() } else { ROW_TXT() };
        p.stroke_circle(cx + 21.0, ry + 10.0, 5.0, 1.2, if td.done { BLUE() } else { SUB() });
        if td.done {
            p.fill_circle(cx + 21.0, ry + 10.0, 3.0, BLUE());
        }
        let mut tx = cx + 36.0;
        if let Some(pc) = priority_color(td.priority) {
            p.fill_circle(cx + 35.0, ry + 9.0, 3.0, pc);
            tx = cx + 44.0;
        }
        p.text(label, tx, ry, cx + cw - 58.0 - tx, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.0, false, false, col);
        if td.done {
            let w = p.measure(label, 12.0, false, false).0;
            p.line(tx, ry + 9.0, tx + w, ry + 9.0, 1.0, SUB_DIM());
        }
        if overdue {
            let ph = self.hover == Some(SbAction::TodoPostpone(gi));
            p.text("→", cx + cw - 76.0, ry, 20.0, 18.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 10.5, false, false, if ph { BLUE() } else { RED() });
            self.regions.push((gdi::RectF { x: cx + cw - 78.0, y: ry, w: 22.0, h: 20.0 }, SbAction::TodoPostpone(gi)));
        }
        let ed_hover = self.hover == Some(SbAction::TodoEdit(gi));
        p.text("✎", cx + cw - 52.0, ry, 18.0, 18.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 10.0, false, false, if ed_hover { BLUE() } else { SUB_DIM() });
        let del_hover = self.hover == Some(SbAction::TodoDelete(gi));
        p.text("✕", cx + cw - 30.0, ry, 18.0, 18.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 10.5, false, false, if del_hover { RED() } else { SUB_DIM() });
        self.regions.push((gdi::RectF { x: cx + 12.0, y: ry, w: 20.0, h: 20.0 }, SbAction::TodoToggle(gi)));
        self.regions.push((gdi::RectF { x: cx + cw - 54.0, y: ry, w: 20.0, h: 20.0 }, SbAction::TodoEdit(gi)));
        self.regions.push((gdi::RectF { x: cx + cw - 32.0, y: ry, w: 20.0, h: 20.0 }, SbAction::TodoDelete(gi)));
    }

    fn paint_date_card(&self, p: &Painter, cx: f32, y: f32, cw: f32, date: NaiveDate, today: NaiveDate) {
        p.fill_round(cx, y, cw, 84.0, 10.0, POPUP_BG());
        let ix = cx + 14.0;
        let iy = y + 13.0;
        p.fill_round(ix, iy, 48.0, 48.0, 7.0, WHITE);
        p.stroke_round(ix, iy, 48.0, 48.0, 7.0, 1.0, crate::theme::ov(34));
        p.fill_round(ix, iy, 48.0, 13.0, 6.0, RED());
        p.fill_round(ix, iy + 9.0, 48.0, 39.0, 6.0, WHITE);
        p.fill_circle(ix + 13.0, iy + 1.0, 2.2, RED());
        p.fill_circle(ix + 35.0, iy + 1.0, 2.2, RED());
        p.text(&format!("{}", date.day()), ix, iy + 11.0, 48.0, 36.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 23.0, true, false, BLUE());

        let tx = ix + 58.0;
        let tw = cx + cw - 14.0 - tx - 48.0;
        let wd = ["日", "一", "二", "三", "四", "五", "六"][date.weekday().num_days_from_sunday() as usize];
        p.text(&format!("{}年{}月{}日 星期{}", date.year(), date.month(), date.day(), wd), tx, y + 12.0, tw + 40.0, 20.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 15.0, true, false, TITLE_COL());

        // 相对今天徽标
        let rel = (date - today).num_days();
        let tag = match rel {
            0 => "今天".to_string(),
            1 => "明天".to_string(),
            2 => "后天".to_string(),
            n if n < 0 => format!("{}天前", -n),
            n => format!("{}天后", n),
        };
        let bw = p.measure(&tag, 10.0, false, false).0 + 14.0;
        let bx = cx + cw - 14.0 - bw;
        p.fill_round(bx, y + 11.0, bw, 18.0, 9.0, BLUE());
        p.text(&tag, bx, y + 11.0, bw, 18.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 10.0, false, false, WHITE);

        let l = crate::lunar::solar_to_lunar(date);
        p.text(&almanac::day_week_line(date), tx, y + 35.0, cw - 28.0 - 62.0, 16.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.0, false, false, SUB());
        if let Some(l) = l {
            p.text(&almanac::lunar_ganzhi_line(&l, date), tx, y + 52.0, cw - 28.0 - 62.0, 16.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.0, false, false, SUB());
        }
    }

    fn paint_almanac_card(&self, p: &Painter, cx: f32, y: f32, cw: f32, h: f32, date: NaiveDate) {
        p.fill_round(cx, y, cw, h, 10.0, POPUP_BG());
        let zhi = almanac::jianzhi(date);
        let (yi, ji) = almanac::jianzhi_yiji(zhi);
        let max_w = cw - 28.0 - 32.0 - 8.0 - 36.0;
        let rows = [(yi, "宜", gdi::argb(36, 91, 194, 142), gdi::argb(255, 91, 194, 142)), (ji, "忌", gdi::argb(36, 229, 75, 75), gdi::argb(255, 229, 75, 75))];
        let mut ry = y + 12.0;
        for (terms, label, bg, fg) in rows {
            let lines = wrap_terms(p, terms, max_w);
            let row_h = (lines.len() as f32 * 16.0).max(22.0);
            p.fill_round(cx + 14.0, ry + (row_h - 22.0) / 2.0, 22.0, 22.0, 5.0, bg);
            p.text(label, cx + 14.0, ry + (row_h - 22.0) / 2.0, 22.0, 22.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.5, true, false, fg);
            for (i, line) in lines.iter().enumerate() {
                p.text(line, cx + 14.0 + 30.0, ry + (row_h - lines.len() as f32 * 16.0) / 2.0 + i as f32 * 16.0, max_w, 16.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.0, false, false, ROW_TXT());
            }
            ry += row_h + 5.0;
        }
        // 右侧竖排徽标：十二值日
        let name = format!("{}日", almanac::JIANZHI_NAMES[zhi]);
        let bx = cx + cw - 14.0 - 26.0;
        let by = y + 10.0;
        let bh = h - 20.0;
        p.fill_round(bx, by, 26.0, bh, 6.0, gdi::argb(30, 62, 135, 250));
        p.stroke_round(bx, by, 26.0, bh, 6.0, 1.0, gdi::argb(110, 62, 135, 250));
        let chars: Vec<char> = name.chars().collect();
        let total = chars.len() as f32 * 17.0;
        let start = by + (bh - total) / 2.0;
        for (i, ch) in chars.iter().enumerate() {
            p.text(&ch.to_string(), bx, start + i as f32 * 17.0, 26.0, 17.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, BLUE());
        }
    }

    fn paint_agenda_card(&mut self, p: &Painter, cx: f32, y: f32, cw: f32, h: f32, date: NaiveDate) {
        p.fill_round(cx, y, cw, h, 10.0, POPUP_BG());
        // 解析当日生效日程（含按天重复展开），缓存原 key/下标供编辑/删除定位
        let rows = crate::events::agenda_on(&self.agenda.lock().unwrap(), date);
        self.agenda_rows = rows.clone();
        let n = rows.len();
        self.agenda_count = n;
        let wd = ["日", "一", "二", "三", "四", "五", "六"][date.weekday().num_days_from_sunday() as usize];
        if n == 0 {
            p.text(&format!("{}年{}月{}日 星期{} 还没有日程", date.year(), date.month(), date.day(), wd), cx, y, cw, h, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 12.0, false, false, SUB());
            return;
        }
        p.text(&format!("{}年{}月{}日 星期{} 日程", date.year(), date.month(), date.day(), wd), cx + 14.0, y + 8.0, cw - 28.0, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 13.0, false, false, TITLE_COL());
        let list_top = y + 28.0;
        self.agenda_band = (list_top, list_top + AGENDA_VISIBLE as f32 * 20.0);
        let off = self.agenda_scroll.min(n.saturating_sub(AGENDA_VISIBLE));
        for (vi, (_, _, item)) in rows.iter().enumerate().skip(off).take(AGENDA_VISIBLE) {
            let ry = list_top + (vi - off) as f32 * 20.0;
            let text = crate::events::display(item);
            let ed_hov = self.hover == Some(SbAction::AgendaEdit(vi));
            p.text(&text, cx + 14.0, ry, cw - 48.0, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, false, false, if ed_hov { BLUE() } else { ROW_TXT() });
            // 行体点击 → 编辑弹窗
            self.regions.push((gdi::RectF { x: cx + 12.0, y: ry, w: cw - 48.0, h: 18.0 }, SbAction::AgendaEdit(vi)));
            let del_hover = self.hover == Some(SbAction::AgendaDelete(vi));
            p.text("✕", cx + cw - 30.0, ry, 18.0, 18.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 10.5, false, false, if del_hover { RED() } else { SUB_DIM() });
            self.regions.push((gdi::RectF { x: cx + cw - 32.0, y: ry, w: 20.0, h: 20.0 }, SbAction::AgendaDelete(vi)));
        }
        if n > AGENDA_VISIBLE {
            p.text(&format!("共 {} 条 · 滚轮查看更多", n), cx + 14.0, list_top + AGENDA_VISIBLE as f32 * 20.0, cw - 28.0, 16.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 10.5, false, false, SUB_DIM());
        }
    }

    fn paint_motto_card(&self, p: &Painter, cx: f32, y: f32, cw: f32, h: f32, date: NaiveDate) {
        p.fill_round(cx, y, cw, h, 10.0, POPUP_BG());
        let (quote, author) = self.motto_content(date);
        let lines = wrap_chars(p, &quote, cw - 28.0);
        for (i, line) in lines.iter().enumerate() {
            p.text(line, cx + 14.0, y + 10.0 + i as f32 * 18.0, cw - 28.0, 18.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.0, false, false, ROW_TXT());
        }
        p.text(&format!("—— {}", author), cx + 14.0, y + h - 22.0, cw - 28.0, 16.0, gdi::HALIGN_FAR, gdi::HALIGN_CENTER, 11.0, false, false, SUB());
    }

    fn next_event_text(&self, date: NaiveDate, today: NaiveDate) -> String {
        let holidays = self.st.holidays.read().unwrap();
        let start = if date > today { date } else { today };
        for i in 0..=370i64 {
            let d = start + Duration::days(i);
            if let Some(h) = holidays.get(&key_of_date(d)) {
                if h.ty == DayType::Xiu && h.idx == 0 {
                    return if i == 0 {
                        format!("今天是{}", h.name)
                    } else {
                        format!("{} · 还有 {} 天", h.name, i)
                    };
                }
            }
        }
        "暂无事件".to_string()
    }
}

fn motto_of(date: NaiveDate) -> (&'static str, &'static str) {
    MOTTOS[(date.ordinal() as usize) % MOTTOS.len()]
}

fn wrap_terms(p: &Painter, terms: &[&str], max_w: f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut w = 0.0f32;
    for t in terms {
        let tw = p.measure(t, 12.0, false, false).0;
        let add = if cur.is_empty() { tw } else { tw + 10.0 };
        if !cur.is_empty() && w + add > max_w {
            lines.push(std::mem::take(&mut cur));
            w = tw;
            cur.push_str(t);
        } else {
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(t);
            w += add;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

fn wrap_chars(p: &Painter, s: &str, max_w: f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut w = 0.0f32;
    for ch in s.chars() {
        let cw = p.measure(&ch.to_string(), 12.0, false, false).0;
        if w + cw > max_w && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            w = 0.0;
        }
        cur.push(ch);
        w += cw;
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

unsafe extern "system" fn sidebar_wndproc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            ValidateRect(hwnd, std::ptr::null_mut());
            0
        }
        // 历史上的今天后台拉取完成：重绘
        WM_APP_HISTORY => {
            let mut guard = SIDEBAR_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                f.0.redraw();
            }
            0
        }
        WM_ERASEBKGND => 1,
        // 点击侧栏不改变激活状态：主日历不会因失焦隐藏，点击消息正常送达
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_MOUSEMOVE => {
            let mut guard = SIDEBAR_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                let x = ((lp & 0xFFFF) as u16 as i16) as f32 / f.sf;
                let y = (((lp as usize) >> 16) as u16 as i16) as f32 / f.sf;
                let hit = f.regions.iter().rev().find(|(r, _)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h).map(|(_, a)| *a);
                let clickable = hit.is_some();
                if hit != f.hover {
                    f.hover = hit;
                    f.redraw();
                }
                if clickable {
                    SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_HAND));
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
            let mut guard = SIDEBAR_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                if f.hover.is_some() {
                    f.hover = None;
                    f.redraw();
                }
            }
            0
        }
        WM_LBUTTONDOWN => {
            let mut guard = SIDEBAR_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                let x = ((lp & 0xFFFF) as u16 as i16) as f32 / f.sf;
                let y = (((lp as usize) >> 16) as u16 as i16) as f32 / f.sf;
                let hit = f.regions.iter().rev().find(|(r, _)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h).map(|(_, a)| *a);
                match hit {
                    Some(SbAction::CardManage) => {
                        drop(guard);
                        crate::flyout::show_settings_tab(2);
                    }
                    Some(SbAction::TodoToggle(i)) => {
                        // 按天重复的待办按“当前查看的日子”记录完成
                        let key = f.date.map(crate::ics::key_of_date).unwrap_or_default();
                        crate::sidebar::toggle_todo_on(i, &key);
                        f.redraw();
                    }
                    Some(SbAction::TodoDelete(i)) => {
                        // 重复待办：✕ 弹“仅这一天/整个系列”选择；普通待办直接删（可撤销）
                        let td = crate::sidebar::todo_at(i);
                        if td.as_ref().map(|t| t.recur.is_some()).unwrap_or(false) {
                            let date = f.date.unwrap_or_else(|| chrono::Local::now().date_naive());
                            drop(guard);
                            let mut pt = POINT { x: 0, y: 0 };
                            unsafe { GetCursorPos(&mut pt) };
                            crate::recur_menu::open(pt.x, pt.y, crate::recur_menu::RmTarget::Todo { gi: i, date });
                        } else if let Some(td) = td {
                            let body = td.text.clone();
                            crate::sidebar::remove_todo_at(i);
                            crate::toast::notify_undo(crate::toast::UndoData::Todos { items: vec![(i, td)] }, &body);
                            f.redraw();
                        }
                    }
                    Some(SbAction::TodoPostpone(i)) => {
                        crate::sidebar::postpone_todo_to_today(i);
                        f.redraw();
                    }
                    Some(SbAction::TodoPostponeAll) => {
                        // 逾期未完成一键全部顺延到今天
                        let gis: Vec<usize> = overdue_todos().into_iter().map(|(gi, _)| gi).collect();
                        let n = postpone_overdue_all(&gis);
                        if n > 0 {
                            crate::toast::notify("待办已顺延", &format!("已把 {} 条逾期待办顺延到今天", n));
                        }
                        f.redraw();
                    }
                    Some(SbAction::TodoClearDone) => {
                        // 清除已完成（可撤销）
                        let removed = clear_done_todos();
                        if !removed.is_empty() {
                            let body = format!("{} 条已完成待办", removed.len());
                            crate::toast::notify_undo(crate::toast::UndoData::Todos { items: removed }, &body);
                        }
                        f.redraw();
                    }
                    Some(SbAction::TodoEdit(i)) => {
                        // 取出待办后在锁外打开编辑弹窗（inputbox 会激活窗口）
                        let td = crate::sidebar::todo_at(i);
                        let occ = if td.as_ref().map(|t| t.recur.is_some()).unwrap_or(false) {
                            f.date
                        } else {
                            None
                        };
                        let hwnd = f.hwnd;
                        drop(guard);
                        if let Some(td) = td {
                            let mut r: RECT = std::mem::zeroed();
                            unsafe { GetWindowRect(hwnd as HWND, &mut r) };
                            crate::inputbox::open_todo_edit(r.left, r.top, i, &td, occ);
                        }
                    }
                    Some(SbAction::AgendaEdit(i)) => {
                        // 取出解析行（含重复展开的行）后在锁外打开编辑弹窗
                        let target = f.agenda_rows.get(i).map(|(k, idx, e)| (k.clone(), *idx, e.clone()));
                        let occ = f.date;
                        let hwnd = f.hwnd;
                        drop(guard);
                        if let Some((key, idx, entry)) = target {
                            let ev = match entry {
                                crate::events::AgendaEntry::Rich(r) => r,
                                crate::events::AgendaEntry::Legacy(text) => {
                                    // 旧纯文本条目：转为富条目编辑（保存后替换）
                                    crate::events::RichEvent {
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
                                    }
                                }
                            };
                            let mut r: RECT = std::mem::zeroed();
                            unsafe { GetWindowRect(hwnd as HWND, &mut r) };
                            crate::inputbox::open_agenda_edit(r.left, r.top, &key, idx, &ev, occ);
                        }
                    }
                    Some(SbAction::AgendaDelete(i)) => {
                        // 用解析行的原 key/下标定位（重复日程行也指向主条目）
                        let target = f.agenda_rows.get(i).map(|(k, idx, e)| (k.clone(), *idx, e.clone()));
                        let is_recur = matches!(&target, Some((_, _, crate::events::AgendaEntry::Rich(r))) if r.recur.is_some());
                        if is_recur {
                            // 重复日程：✕ 弹“仅这一天/整个系列”选择
                            let (key, idx) = match target {
                                Some((k, idx, _)) => (k, idx),
                                None => unreachable!(),
                            };
                            let date = f.date.unwrap_or_else(|| chrono::Local::now().date_naive());
                            drop(guard);
                            let mut pt = POINT { x: 0, y: 0 };
                            unsafe { GetCursorPos(&mut pt) };
                            crate::recur_menu::open(pt.x, pt.y, crate::recur_menu::RmTarget::Agenda { key, idx, date });
                        } else if let Some((key, idx, entry)) = target {
                            let body = crate::events::display(&entry);
                            let mut map = f.agenda.lock().unwrap();
                            let removed = crate::events::agenda_remove_at(&mut map, &key, idx);
                            drop(map);
                            if removed {
                                let m = f.agenda.lock().unwrap();
                                crate::events::save(&m);
                                drop(m);
                                // 撤销卡片（快照携带被删条目）
                                crate::toast::notify_undo(crate::toast::UndoData::Agenda { key, idx, entry }, &body);
                                f.redraw();
                            }
                        }
                    }
                    _ => {}
                }
            }
            0
        }
        WM_MOUSEWHEEL => {
            // 滚轮作用于光标所在的卡片列表（日程/待办），翻动溢出内容
            let delta = ((wp as i32) >> 16) as i16 as f32;
            let mut guard = SIDEBAR_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                let mut pt = POINT { x: 0, y: 0 };
                unsafe {
                    GetCursorPos(&mut pt);
                    ScreenToClient(hwnd as HWND, &mut pt);
                }
                let cy = pt.y as f32 / f.sf;
                let (target, count) = if cy >= f.agenda_band.0 && cy < f.agenda_band.1 && f.agenda_count > AGENDA_VISIBLE {
                    (0, f.agenda_count)
                } else if cy >= f.todo_band.0 && cy < f.todo_band.1 && f.todo_count > TODO_VISIBLE {
                    (1, f.todo_count)
                } else {
                    (-1, 0)
                };
                if target >= 0 {
                    let visible = if target == 0 { AGENDA_VISIBLE } else { TODO_VISIBLE };
                    let max_off = count.saturating_sub(visible);
                    let step = ((delta / 120.0) * 2.0).round() as i32; // 每格滚 2 行
                    let cur = if target == 0 { f.agenda_scroll } else { f.todo_scroll };
                    let next = (cur as i32 - step).clamp(0, max_off as i32) as usize;
                    if next != cur {
                        if target == 0 {
                            f.agenda_scroll = next;
                        } else {
                            f.todo_scroll = next;
                        }
                        f.redraw();
                    }
                }
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// 需要的附加颜色/常量（取自 theme 色板）
fn BG_PAGE() -> u32 { crate::theme::pal().bg }
fn POPUP_BG() -> u32 { crate::theme::pal().card }
fn TITLE_COL() -> u32 { crate::theme::pal().title }
fn ROW_TXT() -> u32 { crate::theme::pal().row }
fn SUB() -> u32 { crate::theme::pal().sub }
fn SUB_DIM() -> u32 { crate::theme::pal().dim }
fn BLUE() -> u32 { crate::theme::pal().blue }
fn RED() -> u32 { crate::theme::pal().red }
const WHITE: u32 = gdi::argb(255, 255, 255, 255);

/// 优先级色点颜色（与新建待办弹窗一致）
fn priority_color(p: u8) -> Option<u32> {
    match p {
        1 => Some(gdi::argb(255, 0xE5, 0x48, 0x4D)),
        2 => Some(gdi::argb(255, 0xE8, 0x96, 0x3C)),
        3 => Some(gdi::argb(255, 0x3E, 0x87, 0xFA)),
        4 => Some(gdi::argb(255, 0x8A, 0x93, 0xA0)),
        _ => None,
    }
}

#[link(name = "gdiplus")]
extern "system" {
    fn GdipCreateBitmapFromScan0(w: i32, h: i32, stride: i32, format: i32, scan0: *mut u8, bitmap: *mut gdi::Gp) -> i32;
    fn GdipGetImageGraphicsContext(image: gdi::Gp, graphics: *mut gdi::Gp) -> i32;
    fn GdipSetSmoothingMode(graphics: gdi::Gp, mode: i32) -> i32;
    fn GdipSetTextRenderingHint(graphics: gdi::Gp, mode: i32) -> i32;
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
