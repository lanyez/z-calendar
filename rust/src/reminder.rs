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
        .spawn(move || loop {
            scan(&tx);
            std::thread::sleep(std::time::Duration::from_secs(SCAN_SECS));
        })
        .ok();
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

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
        let _ = std::fs::write(done_path(), text);
    }
}

/// 消费到点的“稍后提醒”（toast 点击登记），逐条补发
fn fire_snoozes(tx: &Sender<crate::toast::ToastMsg>, now: i64) {
    let (list, healed) = crate::config::load_json_or_bak::<Vec<crate::toast::SnoozeEntry>>(&std::path::PathBuf::from(
        crate::config::data_dir().join("snooze.json"),
    ));
    if healed {
        crate::toast::notify("稍后提醒已恢复", "snooze.json 损坏，已自动从备份恢复");
    }
    let Some(list) = list else { return };
    let mut keep = Vec::new();
    let mut changed = false;
    for e in list {
        if e.t <= now {
            // 系统通知中心可用则优先走系统 Toast（失败自动回退内置卡片）
            let sys_ok = crate::config::use_system_toast_on()
                && crate::wnotify::show_reminder(&e.title, &e.body, &e.act, crate::config::remind_sound_on());
            if !sys_ok {
                if crate::config::use_system_toast_on() {
                    crate::log::warn("系统通知中心发送失败，已回退内置提醒卡片");
                }
                let _ = tx.send(crate::toast::ToastMsg { title: e.title.clone(), body: e.body.clone(), act: e.act.clone(), quiet: false });
            }
            changed = true;
        } else {
            keep.push(e);
        }
    }
    if changed {
        let path = crate::config::data_dir().join("snooze.json");
        if let Ok(text) = serde_json::to_string(&keep) {
            let _ = std::fs::write(path, text);
        }
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

fn scan(tx: &Sender<crate::toast::ToastMsg>) {
    let now = now_ms();
    fire_snoozes(tx, now);
    let mut cands: Vec<Cand> = Vec::new();
    let today = chrono::Local::now().date_naive();

    // 日程：带提醒的富条目（重复的展开到前后一天的 occurrence，覆盖“1天前”跨日提醒）
    let (agenda, healed) = crate::config::load_json_or_bak::<crate::events::AgendaMap>(&std::path::PathBuf::from(
        crate::config::data_dir().join("agenda.json"),
    ));
    if healed {
        crate::toast::notify("日程数据已恢复", "agenda.json 损坏，已自动从备份恢复");
    }
    let agenda = agenda.unwrap_or_default();
    for (key, items) in &agenda {
        for (idx, e) in items.iter().enumerate() {
            let crate::events::AgendaEntry::Rich(r) = e else { continue };
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

    // 待办：带时间且未完成的（重复的同样展开；无时间不提醒）
    let (todos, healed) = crate::config::load_json_or_bak::<Vec<crate::sidebar::Todo>>(&std::path::PathBuf::from(
        crate::config::data_dir().join("todo.json"),
    ));
    if healed {
        crate::toast::notify("待办数据已恢复", "todo.json 损坏，已自动从备份恢复");
    }
    let todos = todos.unwrap_or_default();
    for (idx, td) in todos.iter().enumerate() {
        if (td.done && td.recur.is_none()) || !td.has_time {
            continue;
        }
        let Some(remind_min) = td.remind else { continue };
        let Some(ref s) = td.start else { continue };
        let Some(start) = crate::events::parse_start(s) else { continue };
        let dedup = item_dedup("t", &td.date.clone().unwrap_or_default(), idx, &td.id, &td.text, s, td.remind, td.repeat);
        match td.recur.as_deref() {
            None => {
                let start_ms = local_ms(start);
                let end_ms = td
                    .end
                    .as_deref()
                    .and_then(crate::events::parse_start)
                    .map(local_ms)
                    .filter(|&e| e > start_ms)
                    .unwrap_or(start_ms + 3600 * 1000);
                cands.push(Cand {
                    dedup: dedup.clone(),
                    title: "待办提醒".into(),
                    body: body_text(&td.text, s, false),
                    fire0: start_ms - remind_min * 60_000,
                    step: td.repeat.map(|m| m.max(1) * 60_000),
                    boundary: if td.repeat.is_some() { end_ms } else { start_ms },
                    act: crate::toast::Act::TodoDone { id: td.id.clone(), date: td.date.clone().unwrap_or_default(), recur: false },
                });
            }
            Some(rc) => {
                let anchor = td
                    .date
                    .as_deref()
                    .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
                let Some(anchor) = anchor else { continue };
                let t = start.time();
                let dur = td
                    .end
                    .as_deref()
                    .and_then(crate::events::parse_start)
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
                        body: body_text(&td.text, s, false),
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

    let mut done = load_done(now);
    let mut dirty = false;
    for c in &cands {
        // 当前应处的提醒档期：最后一个 ≤ now 的时点
        let slot = match c.step {
            None => {
                if c.fire0 <= now {
                    Some(c.fire0)
                } else {
                    None
                }
            }
            Some(step) => {
                if c.fire0 <= now {
                    let k = (now - c.fire0) / step;
                    let s = c.fire0 + k * step;
                    (s <= c.boundary).then_some(s)
                } else {
                    None
                }
            }
        };
        let Some(slot) = slot else { continue };
        // 错过太久的档期不再补提（重启/睡眠恢复场景）
        if now - slot > GRACE_MS || slot > c.boundary {
            continue;
        }
        let dk = format!("{}|{}", c.dedup, slot);
        if done.contains_key(&dk) {
            continue;
        }
        done.insert(dk, slot);
        dirty = true;
        // 系统通知中心可用则优先走系统 Toast（失败自动回退内置卡片）
        let sys_ok = crate::config::use_system_toast_on()
            && crate::wnotify::show_reminder(&c.title, &c.body, &c.act, crate::config::remind_sound_on());
        if !sys_ok {
            if crate::config::use_system_toast_on() {
                crate::log::warn("系统通知中心发送失败（稍后提醒），已回退内置提醒卡片");
            }
            let _ = tx.send(crate::toast::ToastMsg { title: c.title.clone(), body: c.body.clone(), act: c.act.clone(), quiet: false });
        }
    }
    if dirty {
        save_done(&done);
    }
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
