//! 提醒引擎：后台线程定期扫描日程/待办，到期的提醒通过 toast 弹窗通知。
//!
//! 语义：
//! - 提醒时点 = 开始时间 - 提前分钟数（全天日程按 09:00 计）；
//! - 分钟级重复：从首个提醒时点起每 N 分钟重提一次，直到结束时间
//!   （无结束时间按开始后 1 小时计）；
//! - 按天重复（每天/每周/每月/每年/农历每年）：每次出现单独提醒，开始时间取当天的
//!   同时刻；尊重“重复至”截止日与“仅此次”例外；
//! - 错过提醒时点 2 分钟内（睡眠恢复/重启）补提一次，过时不补；
//! - 已提醒的档期登记到 remind_done.json，重启不重提（7 天后自动清理）；
//! - toast 上点「稍后10分钟」登记到 snooze.json，到点由本线程补发。

use std::collections::HashMap;
use std::sync::mpsc::Sender;

use chrono::Timelike;

/// 扫描间隔
const SCAN_SECS: u64 = 20;
/// 补提醒宽限窗口
const GRACE_MS: i64 = 2 * 60 * 1000;

/// 发送一条提醒
pub fn spawn(tx: Sender<crate::toast::ToastMsg>) {
    std::thread::Builder::new()
        .name("reminder".into())
        .stack_size(128 * 1024)
        .spawn(move || {
            // 空转修剪低频兜底：每 15 轮（5 分钟）一次，高频修剪-换页互相打架。
            // 注意 scan 必须每轮无条件执行（提醒宽限窗仅 2 分钟，漏轮=丢提醒）。
            let mut scans: u32 = 0;
            loop {
                scans += 1;
                let fired = scan(&tx);
                if scans.is_multiple_of(15) && !fired {
                    crate::trim_working_set();
                }
                std::thread::sleep(std::time::Duration::from_secs(SCAN_SECS));
            }
        })
        .ok();
}

use crate::clock::now_ms;

/// 本地墙上时间 → epoch 毫秒。
/// 注意：NaiveDateTime 直接 timestamp_millis 会按 UTC 解释（晚间事件比 now 晚一个时区，
/// 永远到不了提醒时点），必须先按本地时区换算。
fn local_ms(dt: chrono::NaiveDateTime) -> i64 {
    use chrono::TimeZone;
    chrono::Local
        .from_local_datetime(&dt)
        .single()
        .map(|d| d.timestamp_millis())
        .unwrap_or_else(|| dt.and_utc().timestamp_millis())
}

fn done_path() -> std::path::PathBuf {
    crate::config::data_dir().join("remind_done.json")
}

// ---------------- 提醒历史（remind_log.json，侧栏"最近提醒"卡片读取） ----------------

/// 提醒历史条目
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RemindLogEntry {
    /// 触发时点（epoch 毫秒）
    pub t: i64,
    pub title: String,
    pub body: String,
}

fn log_path() -> std::path::PathBuf {
    crate::config::data_dir().join("remind_log.json")
}

/// 追加一条提醒历史（上限 200 条 / 7 天过期，原子写盘）。任意线程可调用。
pub fn log_remind(title: &str, body: &str) {
    let now = now_ms();
    let (list, healed) = crate::config::load_json_or_bak::<Vec<RemindLogEntry>>(&log_path());
    if healed {
        crate::log::warn("remind_log.json 损坏，已自动从备份恢复");
    }
    let mut list = list.unwrap_or_default();
    list.push(RemindLogEntry { t: now, title: title.to_string(), body: body.to_string() });
    list.retain(|e| now - e.t < 7 * 24 * 3600 * 1000);
    if list.len() > 200 {
        let drop = list.len() - 200;
        list.drain(..drop);
    }
    if let Ok(text) = serde_json::to_string(&list) {
        crate::config::backup_file(&log_path());
        let _ = crate::config::write_file_atomic(&log_path(), &text);
    }
}

/// 最近的提醒历史（按时间倒序，最多 n 条）：侧栏"最近提醒"卡片用
pub fn recent_reminds(n: usize) -> Vec<RemindLogEntry> {
    let (list, _) = crate::config::load_json_or_bak::<Vec<RemindLogEntry>>(&log_path());
    let mut list = list.unwrap_or_default();
    list.sort_by_key(|e| std::cmp::Reverse(e.t));
    list.truncate(n);
    list
}

/// 已提醒登记：档期 key → 提醒时点毫秒（加载时清理 7 天前的旧记录）
fn load_done(now: i64) -> HashMap<String, i64> {
    let (map, healed) = crate::config::load_json_or_bak::<HashMap<String, i64>>(&done_path());
    if healed {
        crate::toast::notify("提醒记录已恢复", "remind_done.json 损坏，已自动从备份恢复");
    }
    map.unwrap_or_default().into_iter().filter(|(_, ts)| now - *ts < 7 * 24 * 3600 * 1000).collect()
}

fn save_done(map: &HashMap<String, i64>) {
    if let Ok(text) = serde_json::to_string(map) {
        crate::config::backup_file(&done_path());
        let _ = crate::config::write_file_atomic(&done_path(), &text);
    }
}

/// 消费到点的“稍后提醒”（toast 点击登记），逐条补发。
/// 文件读-改-写与 toast 线程的 snooze_add 串行（同一把锁）；锁内不发通知，
/// 避免 WinRT Toast 发送耗时期间占锁阻塞用户点击“稍后”。
fn fire_snoozes(tx: &Sender<crate::toast::ToastMsg>, now: i64) -> bool {
    let due: Vec<crate::toast::SnoozeEntry> = {
        let _g = crate::toast::snooze_lock();
        let (list, healed) = crate::config::load_json_or_bak::<Vec<crate::toast::SnoozeEntry>>(&crate::config::data_dir().join("snooze.json"));
        if healed {
            crate::toast::notify("稍后提醒已恢复", "snooze.json 损坏，已自动从备份恢复");
        }
        let Some(list) = list else { return false };
        let mut keep = Vec::new();
        let mut due = Vec::new();
        for e in list {
            if e.t <= now {
                due.push(e);
            } else {
                keep.push(e);
            }
        }
        if !due.is_empty() {
            if let Ok(text) = serde_json::to_string(&keep) {
                // 与 toast::snooze_add 一致：备份轮转 + 原子写
                let path = crate::config::data_dir().join("snooze.json");
                let _ = crate::config::write_with_backup(&path, &text);
            }
        }
        due
    };
    let mut fired = false;
    for e in due {
        fired = true;
        log_remind(&e.title, &e.body);
        deliver(tx, &e.title, &e.body, &e.act);
    }
    fired
}

/// 发出一条提醒：系统通知中心可用则优先走系统 Toast，否则回退内置提醒卡片
/// （三个提醒出口共用：到点提醒 / 稍后提醒补发 / 错过汇总）
fn deliver(tx: &Sender<crate::toast::ToastMsg>, title: &str, body: &str, act: &crate::toast::Act) {
    let sys_ok = crate::config::use_system_toast_on()
        && crate::wnotify::show_reminder(title, body, act, crate::config::remind_sound_on());
    if !sys_ok {
        if crate::config::use_system_toast_on() {
            crate::log::warn("系统通知不可用（未开启或发送失败），已回退内置提醒卡片");
        }
        let _ = tx.send(crate::toast::ToastMsg { title: title.to_string(), body: body.to_string(), act: act.clone(), quiet: false });
    }
}

/// 待提醒条目（归一化后的日程/待办）
struct Cand {
    /// 去重 id（条目 id，旧数据回退为字段哈希）
    dedup: String,
    title: String,
    body: String,
    fire0: i64,
    /// 重复间隔毫秒（None=单次）
    step: Option<i64>,
    /// 重复提醒的截止时点
    boundary: i64,
    /// 待办一键完成动作
    act: crate::toast::Act,
}

/// 错过提醒进入当日汇总的时限（更久的不追）
const MISSED_MS: i64 = 24 * 3600 * 1000;
/// 错过汇总条目：条目去重 id → (标题, 正文)
type Missed = HashMap<String, (String, String)>;

/// 日程侧候选：带提醒的富条目（按天重复的展开到前后一天的 occurrence，覆盖“1天前”
/// 跨日提醒）。纯函数，数据由调用方传入（scan 传进程内共享态，测试传内存表）
fn agenda_cands(agenda: &crate::events::AgendaMap, today: chrono::NaiveDate) -> Vec<Cand> {
    use crate::events::AgendaEntry;
    let mut cands = Vec::new();
    for (key, items) in agenda.iter() {
        for (idx, e) in items.iter().enumerate() {
            let AgendaEntry::Rich(r) = e else { continue };
            let Some(remind_min) = r.remind else { continue };
            let Some(start) = crate::events::parse_start(&r.start) else { continue };
            let dedup = item_dedup("a", key, idx, &r.id, &r.name, &r.start, r.remind, r.repeat);
            match r.recur.as_deref() {
                None => {
                    let start_ms = local_ms(start);
                    let end_ms = crate::events::parse_start(&r.end)
                        .map(local_ms)
                        .filter(|&e| e > start_ms)
                        .unwrap_or(start_ms + 3600 * 1000);
                    cands.push(Cand {
                        dedup,
                        title: "日程提醒".into(),
                        body: body_text(&r.name, &r.start, r.all_day),
                        fire0: start_ms - remind_min * 60_000,
                        step: r.repeat.map(|m| m.max(1) * 60_000),
                        boundary: if r.repeat.is_some() { end_ms } else { start_ms },
                        act: crate::toast::Act::None,
                    });
                }
                Some(rc) => {
                    // 重复日程：生成窗口内各次出现，每次单独提醒（尊重截止日/仅此次）
                    let t = start.time();
                    let dur = crate::events::parse_start(&r.end)
                        .map(|e| e - start)
                        .filter(|d| *d > chrono::Duration::zero())
                        .unwrap_or(chrono::Duration::hours(1));
                    for off in [-1i64, 0, 1] {
                        let od = today + chrono::Duration::days(off);
                        if !crate::events::recur_occurs(r.recur_until.as_deref(), &r.skip_dates, rc, start.date(), od) {
                            continue;
                        }
                        let Some(ostart) = od.and_hms_opt(t.hour(), t.minute(), 0) else { continue };
                        let ostart_ms = local_ms(ostart);
                        let oend_ms = local_ms(ostart + dur);
                        cands.push(Cand {
                            dedup: dedup.clone(),
                            title: "日程提醒".into(),
                            body: body_text(&r.name, &r.start, r.all_day),
                            fire0: ostart_ms - remind_min * 60_000,
                            step: None,
                            boundary: oend_ms,
                            act: crate::toast::Act::None,
                        });
                    }
                }
            }
        }
    }
    cands
}

/// 待办侧候选：带提醒且未完成的（全天待办按当天 09:00 计，与全天日程一致；
/// 按天重复的同样展开）。纯函数，同 agenda_cands
fn todo_cands(todos: &[crate::sidebar::Todo], today: chrono::NaiveDate) -> Vec<Cand> {
    use crate::events::{fmt_dt_store, parse_start};
    let date_of = |s: Option<&str>| s.and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
    let mut cands = Vec::new();
    for (idx, td) in todos.iter().enumerate() {
        if td.done && td.recur.is_none() {
            continue;
        }
        let Some(remind_min) = td.remind else { continue };
        // 开始时间：带时间用存储值；全天待办取归属日 09:00
        let start = match td.start.as_deref().and_then(parse_start) {
            Some(s) if td.has_time => Some(s),
            _ => td.date.as_deref().and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()).and_then(|d| d.and_hms_opt(9, 0, 0)),
        };
        let Some(start) = start else { continue };
        let all_day = !td.has_time;
        // 去重键用实际生效的开始时间（全天待办无存储 start，取合成的 09:00）
        let start_store = td.start.clone().unwrap_or_else(|| fmt_dt_store(start));
        let dedup = item_dedup("t", &td.date.clone().unwrap_or_default(), idx, &td.id, &td.text, &start_store, td.remind, td.repeat);
        match td.recur.as_deref() {
            None => {
                let start_ms = local_ms(start);
                let end_ms = td
                    .end
                    .as_deref()
                    .and_then(parse_start)
                    .map(local_ms)
                    .filter(|&e| e > start_ms)
                    .unwrap_or(start_ms + 3600 * 1000);
                cands.push(Cand {
                    dedup: dedup.clone(),
                    title: "待办提醒".into(),
                    body: body_text(&td.text, &start_store, all_day),
                    fire0: start_ms - remind_min * 60_000,
                    step: td.repeat.map(|m| m.max(1) * 60_000),
                    boundary: if td.repeat.is_some() { end_ms } else { start_ms },
                    act: crate::toast::Act::TodoDone { id: td.id.clone(), date: td.date.clone().unwrap_or_default(), recur: false },
                });
            }
            Some(rc) => {
                let Some(anchor) = date_of(td.date.as_deref()) else { continue };
                let t = start.time();
                let dur = td
                    .end
                    .as_deref()
                    .and_then(parse_start)
                    .map(|e| e - start)
                    .filter(|d| *d > chrono::Duration::zero())
                    .unwrap_or(chrono::Duration::hours(1));
                for off in [-1i64, 0, 1] {
                    let od = today + chrono::Duration::days(off);
                    // 锚点日当天也是一次出现；尊重截止日/仅此次/当天已完成
                    if od != anchor && !crate::events::recur_until_ok(td.recur_until.as_deref(), od) {
                        continue;
                    }
                    if crate::events::recur_skipped(&td.skip_dates, od) {
                        continue;
                    }
                    if td.done_dates.iter().any(|k| {
                        chrono::NaiveDate::parse_from_str(k, "%Y-%m-%d").map(|d| d == od).unwrap_or(false)
                    }) {
                        continue;
                    }
                    if od != anchor && !crate::events::recur_hits(rc, anchor, od) {
                        continue;
                    }
                    let Some(ostart) = od.and_hms_opt(t.hour(), t.minute(), 0) else { continue };
                    let ostart_ms = local_ms(ostart);
                    let oend_ms = local_ms(ostart + dur);
                    cands.push(Cand {
                        dedup: dedup.clone(),
                        title: "待办提醒".into(),
                        body: body_text(&td.text, &start_store, all_day),
                        fire0: ostart_ms - remind_min * 60_000,
                        step: None,
                        boundary: oend_ms,
                        act: crate::toast::Act::TodoDone {
                            id: td.id.clone(),
                            date: crate::ics::key_of_date(od),
                            recur: true,
                        },
                    });
                }
            }
        }
    }
    cands
}

/// 当前应处的提醒档期：最后一个 ≤ now 的时点（重复提醒不超过截止时点）
fn current_slot(c: &Cand, now: i64) -> Option<i64> {
    match c.step {
        None => (c.fire0 <= now).then_some(c.fire0),
        Some(step) => {
            if c.fire0 <= now {
                let k = (now - c.fire0) / step;
                let s = c.fire0 + k * step;
                (s <= c.boundary).then_some(s)
            } else {
                None
            }
        }
    }
}

/// 纯计算：本轮应触发的提醒（连同其档期）与错过汇总条目。scan 据此执行副作用。
/// - 已提醒过的档期（done 登记）直接跳过，且先于宽限/错过判定：成功提醒过的档期在
///   宽限过后若走到错过分支，会被误报成“错过了提醒”；
/// - 档期过后 2 分钟内补提（睡眠恢复/重启），过时不补；24 小时内的错过进当日汇总
///   （避免逐条轰炸），更久的不追。
fn decide<'a>(
    cands: &'a [Cand],
    now: i64,
    done: &HashMap<String, i64>,
) -> (Vec<(&'a Cand, i64)>, Missed) {
    let mut fire = Vec::new();
    let mut missed: Missed = HashMap::new();
    let mut seen = std::collections::HashSet::new();
    for c in cands {
        let Some(slot) = current_slot(c, now) else { continue };
        let dk = format!("{}|{}", c.dedup, slot);
        if done.contains_key(&dk) || !seen.insert(dk) {
            continue;
        }
        let age = now - slot;
        if age > GRACE_MS || slot > c.boundary {
            if age > GRACE_MS && age <= MISSED_MS && slot <= c.boundary {
                missed.entry(c.dedup.clone()).or_insert((c.title.clone(), c.body.clone()));
            }
            continue;
        }
        fire.push((c, slot));
    }
    (fire, missed)
}

/// 错过汇总正文：最多列 3 条，其余以“等 N 条”带过
fn summary_body(missed: &Missed) -> String {
    let n = missed.len();
    let mut body = missed.values().take(3).map(|(_, b)| b.clone()).collect::<Vec<_>>().join("；");
    if n > 3 {
        body = format!("{} 等 {} 条", body, n);
    }
    body
}

fn scan(tx: &Sender<crate::toast::ToastMsg>) -> bool {
    let now = now_ms();
    let mut fired = fire_snoozes(tx, now);
    let today = chrono::Local::now().date_naive();

    // 日程读进程内共享态（main 启动时注册）：此前每轮从磁盘重读 agenda.json 全量解析，
    // 每 20 秒一次与“隐藏态低内存”目标相悖（解析器代码页反复换入），且可能读到
    // 比 UI 内存态旧的落盘文件；内存态在 edit_and_save 中先更新后落盘，永远更新。
    // 待办同理取 Arc 快照（不再每轮磁盘解析 todo.json）
    let mut cands = Vec::new();
    if let Some(shared) = crate::events::shared_agenda() {
        let agenda = shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        cands.extend(agenda_cands(&agenda, today));
    }
    cands.extend(todo_cands(&crate::sidebar::todos_snapshot(), today));

    let mut done = load_done(now);
    let (fire, missed) = decide(&cands, now, &done);
    let mut dirty = false;
    for (c, slot) in fire {
        done.insert(format!("{}|{}", c.dedup, slot), slot);
        dirty = true;
        fired = true;
        log_remind(&c.title, &c.body);
        deliver(tx, &c.title, &c.body, &c.act);
    }
    // 错过提醒汇总：一天只弹一次（登记在 remind_done，重启/重扫不重发）
    if !missed.is_empty() {
        let mk = format!("missed|{}", today.format("%Y-%m-%d"));
        if let std::collections::hash_map::Entry::Vacant(slot) = done.entry(mk) {
            slot.insert(now);
            dirty = true;
            fired = true;
            let body = summary_body(&missed);
            log_remind("错过提醒汇总", &body);
            deliver(tx, "错过了提醒", &body, &crate::toast::Act::None);
        }
    }
    if dirty {
        save_done(&done);
    }
    fired
}

/// 条目去重 id：优先条目自身 id，旧数据回退为字段哈希
fn item_dedup(kind: &str, key: &str, idx: usize, id: &str, text: &str, start: &str, remind: Option<i64>, repeat: Option<i64>) -> String {
    if !id.is_empty() {
        return format!("{}|{}", kind, id);
    }
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (kind, key, idx, text, start, remind, repeat).hash(&mut h);
    format!("{}|h{:x}", kind, h.finish())
}

/// 提醒正文：`14:30 项目评审` / `全天 团建`（遵循 12 小时制设置）；按天重复的每次出现共用
fn body_text(name: &str, start: &str, all_day: bool) -> String {
    let when = if all_day {
        "全天".to_string()
    } else {
        crate::events::fmt_time_str(start)
    };
    format!("{} {}", when, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AgendaEntry, AgendaMap, RichEvent};
    use chrono::TimeZone;

    const MIN: i64 = 60_000;
    /// 任取一个"现在"
    const NOW: i64 = 1_700_000_000_000;

    fn cand(fire0: i64, step: Option<i64>, boundary: i64) -> Cand {
        Cand {
            dedup: "a|x".into(),
            title: "日程提醒".into(),
            body: "全天 测试".into(),
            fire0,
            step,
            boundary,
            act: crate::toast::Act::None,
        }
    }

    fn run(cands: &[Cand], done: &HashMap<String, i64>) -> (usize, usize) {
        let (fire, missed) = decide(cands, NOW, done);
        (fire.len(), missed.len())
    }

    #[test]
    fn fires_only_inside_grace_window() {
        let empty = HashMap::new();
        // 未到点
        assert_eq!(run(&[cand(NOW + 1, None, NOW + MIN)], &empty), (0, 0));
        // 刚到点 / 宽限窗内最后一毫秒
        assert_eq!(run(&[cand(NOW, None, NOW + MIN)], &empty), (1, 0));
        assert_eq!(run(&[cand(NOW - GRACE_MS, None, NOW + MIN)], &empty), (1, 0));
        // 超出宽限 1ms：不补提，进当日汇总
        assert_eq!(run(&[cand(NOW - GRACE_MS - 1, None, NOW + MIN)], &empty), (0, 1));
    }

    #[test]
    fn missed_older_than_a_day_is_dropped() {
        let empty = HashMap::new();
        assert_eq!(run(&[cand(NOW - MISSED_MS, None, NOW)], &empty), (0, 1));
        assert_eq!(run(&[cand(NOW - MISSED_MS - 1, None, NOW)], &empty), (0, 0));
    }

    #[test]
    fn already_reminded_slot_is_skipped_and_not_reported_missed() {
        // 档期已在 done 登记（提醒成功过）：宽限过后也不得被误报成"错过了提醒"
        let mut done = HashMap::new();
        done.insert(format!("a|x|{}", NOW - 10 * MIN), 0);
        assert_eq!(run(&[cand(NOW - 10 * MIN, None, NOW + MIN)], &done), (0, 0));
        // 其它档期的登记不影响本档期
        done.insert(format!("a|x|{}", NOW - 30 * MIN), 0);
        assert_eq!(run(&[cand(NOW - MIN, None, NOW + MIN)], &done), (1, 0));
    }

    #[test]
    fn repeating_reminder_steps_until_boundary() {
        let empty = HashMap::new();
        // 每 5 分钟重复，首个档期 12 分钟前：当前档期 = 首档 + 2 步 = 2 分钟前
        let c = cand(NOW - 12 * MIN, Some(5 * MIN), NOW + 60 * MIN);
        assert_eq!(current_slot(&c, NOW), Some(NOW - 2 * MIN));
        assert_eq!(run(&[c], &empty), (1, 0));
        // 最新档期已越过截止时点：不再提醒
        let c = cand(NOW - 12 * MIN, Some(5 * MIN), NOW - 2 * MIN - 1);
        assert_eq!(current_slot(&c, NOW), None);
        assert_eq!(run(&[c], &empty), (0, 0));
        // 首档还没到
        let c = cand(NOW + MIN, Some(5 * MIN), NOW + 60 * MIN);
        assert_eq!(current_slot(&c, NOW), None);
    }

    #[test]
    fn duplicate_candidates_fire_once_and_missed_groups_by_item() {
        let empty = HashMap::new();
        assert_eq!(run(&[cand(NOW - MIN, None, NOW + MIN), cand(NOW - MIN, None, NOW + MIN)], &empty), (1, 0));
        // 同一条目的多个错过档期只汇总一次
        let mut b = cand(NOW - 10 * MIN, None, NOW + MIN);
        b.dedup = "a|y".into();
        assert_eq!(run(&[cand(NOW - 10 * MIN, None, NOW + MIN), b], &empty), (0, 2));
        let mut b = cand(NOW - 10 * MIN, None, NOW + MIN);
        b.dedup = "a|y".into();
        let mut c2 = cand(NOW - 20 * MIN, None, NOW + MIN);
        c2.dedup = "a|y".into();
        assert_eq!(run(&[b, c2], &empty), (0, 1));
    }

    #[test]
    fn summary_body_lists_three_then_counts() {
        let mk = |n: usize| (0..n).map(|i| (format!("k{i}"), (String::new(), format!("条目{i}")))).collect::<Missed>();
        assert_eq!(summary_body(&mk(0)), "");
        // HashMap 遍历顺序不定：3 条时不带计数，且恰好是这 3 条
        let three = summary_body(&mk(3));
        let mut parts: Vec<&str> = three.split('；').collect();
        parts.sort();
        assert_eq!(parts, ["条目0", "条目1", "条目2"]);
        // 超过 3 条：只列 3 条并注明总数
        let s = summary_body(&mk(5));
        assert!(s.ends_with(" 等 5 条"), "{s}");
        assert_eq!(s.trim_end_matches(" 等 5 条").split('；').count(), 3, "{s}");
    }

    /// 本地墙上时间 → epoch：与 chrono 的本地时区换算一致
    #[test]
    fn local_ms_matches_chronic_local_time() {
        let dt = chrono::NaiveDate::from_ymd_opt(2026, 11, 4).unwrap().and_hms_opt(9, 30, 0).unwrap();
        let want = chrono::Local.from_local_datetime(&dt).single().unwrap().timestamp_millis();
        assert_eq!(local_ms(dt), want);
    }

    fn rich(id: &str, name: &str, start: &str, remind: i64, recur: Option<&str>, until: Option<&str>) -> AgendaEntry {
        AgendaEntry::Rich(RichEvent {
            id: id.into(),
            name: name.into(),
            all_day: false,
            start: start.into(),
            end: start.into(),
            remind: Some(remind),
            repeat: None,
            recur: recur.map(str::to_string),
            recur_until: until.map(str::to_string),
            skip_dates: vec![],
            note: None,
            color: None,
        })
    }

    fn at(date: chrono::NaiveDate, h: u32, m: u32) -> i64 {
        local_ms(date.and_hms_opt(h, m, 0).unwrap())
    }

    /// 按天重复 + 提前 1 天的日程：窗口内各次出现各成候选（覆盖跨日提醒），提醒时点 =
    /// 出现日 09:00 − 24h；尊重截止日与"仅此次"
    #[test]
    fn recurring_agenda_expands_with_lead_time_and_until() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 4).unwrap(); // 周三
        let mut map = AgendaMap::new();
        let ev = |recur: &str, until: Option<&str>| rich("e1", "站会", "2026-10-28 09:00", 1440, Some(recur), until);

        // 每周（周三）重复，截止日含当天：窗口 [昨天, 明天] 里只有今天这一次
        map.insert("2026-10-28".into(), vec![ev("w", Some("2026-11-4"))]);
        let cands = agenda_cands(&map, today);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].fire0, at(today, 9, 0) - 1440 * MIN);
        // 每次出现单独提醒：不带分钟级重复，截止 = 出现开始后 1 小时
        assert!(cands.iter().all(|c| c.step.is_none() && c.boundary == c.fire0 + 1440 * MIN + 3600 * 1000));
        assert_eq!(cands[0].dedup, "a|e1");

        // 截止日改到昨天：今天不再出现
        map.insert("2026-10-28".into(), vec![ev("w", Some("2026-11-3"))]);
        assert!(agenda_cands(&map, today).is_empty());

        // 每天重复、无截止：昨天/今天/明天三次
        map.insert("2026-10-28".into(), vec![ev("d", None)]);
        let cands = agenda_cands(&map, today);
        assert_eq!(cands.len(), 3);
        for (i, off) in [-1i64, 0, 1].iter().enumerate() {
            assert_eq!(cands[i].fire0, at(today + chrono::Duration::days(*off), 9, 0) - 1440 * MIN);
        }

        // "仅此次"排除今天
        if let AgendaEntry::Rich(r) = &mut map.get_mut("2026-10-28").unwrap()[0] {
            r.skip_dates.push("2026-11-4".into());
        }
        let cands = agenda_cands(&map, today);
        assert_eq!(cands.len(), 2);
        assert!(cands.iter().all(|c| c.fire0 != at(today, 9, 0) - 1440 * MIN));
    }

    #[test]
    fn agenda_without_reminder_or_unparsed_start_is_skipped() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        let mut map = AgendaMap::new();
        map.insert("2026-11-4".into(), vec![AgendaEntry::Legacy("旧条目".into()), rich("n", "无提醒", "2026-11-4 09:00", 0, None, None)]);
        if let AgendaEntry::Rich(r) = &mut map.get_mut("2026-11-4").unwrap()[1] {
            r.remind = None;
        }
        map.insert("2026-11-5".into(), vec![rich("bad", "坏数据", "garbage", 0, None, None)]);
        assert!(agenda_cands(&map, today).is_empty());
    }

    fn todo(id: &str, date: &str) -> crate::sidebar::Todo {
        crate::sidebar::test_todo(id, id, date)
    }

    /// 每天重复、提前 1 小时的全天待办：按天展开为三个候选，提醒时点 = 当天 09:00 − 1h
    #[test]
    fn recurring_todo_expands_daily_and_honours_skip_and_done() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        let mut td = todo("t1", "2026-11-1");
        td.remind = Some(60);
        td.recur = Some("d".into());
        let cands = todo_cands(&[td.clone()], today);
        assert_eq!(cands.len(), 3);
        assert_eq!(cands[1].fire0, at(today, 9, 0) - 60 * MIN);
        assert_eq!(cands[1].boundary, at(today, 10, 0));
        assert!(matches!(&cands[1].act, crate::toast::Act::TodoDone { recur: true, date, .. } if date == "2026-11-4"));
        assert_eq!(cands[1].body, "全天 t1");

        // 仅这一天（明天）排除、今天已完成
        td.skip_dates.push("2026-11-5".into());
        td.done_dates.push("2026-11-4".into());
        let cands = todo_cands(&[td], today);
        assert_eq!(cands.len(), 1, "只剩昨天那次出现");
        assert_eq!(cands[0].fire0, at(today - chrono::Duration::days(1), 9, 0) - 60 * MIN);
    }

    #[test]
    fn plain_todos_completion_and_lead_time() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        let mut done_td = todo("d", "2026-11-4");
        done_td.remind = Some(15);
        done_td.done = true;
        assert!(todo_cands(&[done_td], today).is_empty(), "已完成的非重复待办不提醒");

        let mut timed = todo("t", "2026-11-4");
        timed.remind = Some(15);
        timed.has_time = true;
        timed.start = Some("2026-11-4 14:30".into());
        let cands = todo_cands(&[timed], today);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].fire0, at(today, 14, 30) - 15 * MIN);
        assert_eq!(cands[0].boundary, at(today, 14, 30));
        assert_eq!(cands[0].body, "14:30 t");
        assert!(cands[0].step.is_none());
    }

    #[test]
    fn item_dedup_prefers_stable_id() {
        assert_eq!(item_dedup("t", "k", 3, "id1", "x", "y", None, None), "t|id1");
        let h = item_dedup("t", "k", 3, "", "x", "y", None, None);
        assert!(h.starts_with("t|h") && h != item_dedup("t", "k", 4, "", "x", "y", None, None));
    }
}
