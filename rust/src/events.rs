//! 日程/待办共享数据类型与存取
//! agenda.json 兼容旧的纯文本条目：新条目存为对象，旧条目仍是字符串（serde untagged）
use std::collections::HashMap;

use chrono::{Datelike, NaiveDate, Timelike};

/// 富日程条目（日期右键“新增日程”创建）
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RichEvent {
    /// 稳定 id（提醒去重、编辑定位用；旧条目为空）
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub all_day: bool,
    /// "%Y-%m-%d %H:%M"；全天为 "%Y-%m-%d"
    pub start: String,
    pub end: String,
    /// 提前提醒分钟数（None=不提醒，0=准时）
    #[serde(default)]
    pub remind: Option<i64>,
    /// 重复间隔分钟（None=单次）
    #[serde(default)]
    pub repeat: Option<i64>,
    /// 按天重复：d=每天 w=每周（同星期几） m=每月（同几号） y=每年（同月同日） l=农历每年；None=不按天重复
    #[serde(default)]
    pub recur: Option<String>,
    /// 重复截止日期（"%Y-%m-%d"，含当天）；None=无限重复
    #[serde(default)]
    pub recur_until: Option<String>,
    /// “仅此次”删除/修改产生的例外日期（与归属日期同格式，不补零）
    #[serde(default)]
    pub skip_dates: Vec<String>,
}

static ID_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 生成条目 id（毫秒时间戳 + 进程内序号）
pub fn gen_id() -> String {
    let n = ID_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{:x}-{:x}", ts, n)
}

/// 解析存储时间："2026-10-05 14:30"；"2026-10-05"（全天）按 09:00 计
pub fn parse_start(s: &str) -> Option<chrono::NaiveDateTime> {
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M") {
        return Some(dt);
    }
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(9, 0, 0))
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum AgendaEntry {
    Rich(RichEvent),
    /// 旧版/主面板底部输入的纯文本日程
    Legacy(String),
}

pub type AgendaMap = HashMap<String, Vec<AgendaEntry>>;

pub fn load() -> AgendaMap {
    let path = crate::config::data_dir().join("agenda.json");
    match crate::config::load_json_or_bak::<AgendaMap>(&path) {
        (Some(m), true) => {
            crate::toast::notify("日程数据已恢复", "agenda.json 损坏，已自动从备份恢复");
            m
        }
        (Some(m), false) => m,
        (None, _) => Default::default(),
    }
}

pub fn save(map: &AgendaMap) {
    let path = crate::config::data_dir().join("agenda.json");
    if let Ok(text) = serde_json::to_string(map) {
        crate::config::backup_file(&path);
        let _ = std::fs::write(path, text);
    }
}

/// 列表展示文本（旧条目原样，富条目加时间/全天前缀；按天重复的加 ↻ 标记）
pub fn display(e: &AgendaEntry) -> String {
    match e {
        AgendaEntry::Legacy(s) => s.clone(),
        AgendaEntry::Rich(r) => {
            let recur_mark = if r.recur.is_some() { " ↻" } else { "" };
            if r.all_day {
                format!("全天 {}{}", r.name, recur_mark)
            } else if r.start.contains(' ') {
                format!("{} {}{}", fmt_time_str(&r.start), r.name, recur_mark)
            } else {
                format!("{}{}", r.name, recur_mark)
            }
        }
    }
}

// ---------------- 按天重复（每天/每周/每月/每年/农历每年） ----------------

/// 下拉里的按天重复选项（跟在分钟级重复之后）
pub const RECUR_VALUES: [&str; 5] = ["d", "w", "m", "y", "l"];
/// 分钟级重复选项个数（重复下拉前 N 项）
pub const REPEAT_MENU_MIN: usize = 5;
/// 重复下拉总项数 = 分钟级 5 + 按天 5
pub const REPEAT_MENU_LEN: usize = REPEAT_MENU_MIN + RECUR_VALUES.len();

pub fn recur_label(rc: &str) -> String {
    match rc {
        "d" => "每天".into(),
        "w" => "每周".into(),
        "m" => "每月".into(),
        "y" => "每年".into(),
        "l" => "农历每年".into(),
        _ => "单次".into(),
    }
}

/// 按天重复是否命中：date 晚于锚点日时判断（锚点日当天由原生条目直接展示）。
/// l=农历每年：按农历月/日比对（忽略闰月标记，闰月里的日子按普通月处理）。
pub fn recur_hits(kind: &str, anchor: NaiveDate, date: NaiveDate) -> bool {
    if date <= anchor {
        return false;
    }
    match kind {
        "d" => true,
        "w" => anchor.weekday() == date.weekday(),
        "m" => anchor.day() == date.day(),
        "y" => anchor.month() == date.month() && anchor.day() == date.day(),
        "l" => match (crate::lunar::solar_to_lunar(anchor), crate::lunar::solar_to_lunar(date)) {
            (Some(a), Some(d)) => a.month == d.month && a.day == d.day,
            _ => false,
        },
        _ => false,
    }
}

/// 重复截止判定：date 是否仍在截止日内（无截止 = 一直有效）
pub fn recur_until_ok(until: Option<&str>, date: NaiveDate) -> bool {
    match until.and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()) {
        Some(u) => date <= u,
        None => true,
    }
}

/// “仅此次”例外判定：date 是否被排除过
pub fn recur_skipped(skips: &[String], date: NaiveDate) -> bool {
    skips
        .iter()
        .filter_map(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .any(|d| d == date)
}

/// 重复条目在 date（含锚点日当天）是否有一次生效的出现：未截止、未被“仅此次”排除、且命中重复规则
pub fn recur_occurs(until: Option<&str>, skips: &[String], kind: &str, anchor: NaiveDate, date: NaiveDate) -> bool {
    if !recur_until_ok(until, date) || recur_skipped(skips, date) {
        return false;
    }
    date == anchor || recur_hits(kind, anchor, date)
}

/// 预解析的按天重复日程规则（月历格子角标用，避免逐格扫全表）
pub struct RecurRule {
    pub anchor: NaiveDate,
    pub kind: String,
    pub until: Option<String>,
    pub skip: Vec<String>,
}

impl RecurRule {
    pub fn hits(&self, date: NaiveDate) -> bool {
        recur_occurs(self.until.as_deref(), &self.skip, &self.kind, self.anchor, date)
    }
}

pub fn recur_rules(map: &AgendaMap) -> Vec<RecurRule> {
    let mut out = Vec::new();
    for v in map.values() {
        for e in v {
            if let AgendaEntry::Rich(r) = e {
                if let Some(rc) = &r.recur {
                    if let Some(sd) = parse_start(&r.start) {
                        out.push(RecurRule { anchor: sd.date(), kind: rc.clone(), until: r.recur_until.clone(), skip: r.skip_dates.clone() });
                    }
                }
            }
        }
    }
    out
}

/// 某日实际生效的日程（原生条目 + 重复展开，尊重截止日与“仅此次”例外），
/// 返回 (原key, 原下标, 条目)，按开始时间排序（无法解析时间的旧条目排最后）。
pub fn agenda_on(map: &AgendaMap, date: NaiveDate) -> Vec<(String, usize, AgendaEntry)> {
    let key = crate::ics::key_of_date(date);
    let mut out: Vec<(String, usize, AgendaEntry)> = Vec::new();
    if let Some(v) = map.get(&key) {
        for (i, e) in v.iter().enumerate() {
            // 重复条目的锚点日也可能被截止/“仅此次”排除
            if let AgendaEntry::Rich(r) = e {
                if let Some(rc) = &r.recur {
                    if let Some(sd) = parse_start(&r.start) {
                        if !recur_occurs(r.recur_until.as_deref(), &r.skip_dates, rc, sd.date(), date) {
                            continue;
                        }
                    }
                }
            }
            out.push((key.clone(), i, e.clone()));
        }
    }
    for (k, v) in map.iter() {
        if *k == key {
            continue;
        }
        for (i, e) in v.iter().enumerate() {
            let AgendaEntry::Rich(r) = e else { continue };
            let Some(rc) = &r.recur else { continue };
            let Some(sd) = parse_start(&r.start).map(|d| d.date()) else { continue };
            if recur_occurs(r.recur_until.as_deref(), &r.skip_dates, rc, sd, date) {
                out.push((k.clone(), i, e.clone()));
            }
        }
    }    out.sort_by(|a, b| {
        let ta = match &a.2 {
            AgendaEntry::Rich(r) => parse_start(&r.start).map(|d| d.time()),
            AgendaEntry::Legacy(_) => None,
        };
        let tb = match &b.2 {
            AgendaEntry::Rich(r) => parse_start(&r.start).map(|d| d.time()),
            AgendaEntry::Legacy(_) => None,
        };
        match (ta, tb) {
            (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.0.cmp(&b.0)).then_with(|| a.1.cmp(&b.1)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });
    out
}

// ---------------- 提醒 / 重复选项 ----------------

pub const REMIND_VALUES: [Option<i64>; 7] = [None, Some(0), Some(5), Some(15), Some(30), Some(60), Some(1440)];
pub const REPEAT_VALUES: [Option<i64>; 5] = [None, Some(5), Some(15), Some(30), Some(60)];

pub fn remind_label(v: Option<i64>) -> String {
    match v {
        None => "不提醒".into(),
        Some(0) => "准时".into(),
        Some(5) => "5分钟前".into(),
        Some(15) => "15分钟前".into(),
        Some(30) => "30分钟前".into(),
        Some(60) => "1小时前".into(),
        Some(1440) => "1天前".into(),
        Some(m) => format!("提前{}分钟", m),
    }
}

pub fn repeat_label(v: Option<i64>) -> String {
    match v {
        None => "单次".into(),
        Some(m) if m % 60 == 0 => format!("每{}小时", m / 60),
        Some(m) => format!("每{}分钟", m),
    }
}

// ---------------- 日期时间格式化 ----------------

/// 时:分 文本，遵循 12 小时制设置（“上午8:30”/“下午2:05”）
pub fn fmt_hm(h: u32, m: u32) -> String {
    if crate::config::hour12_on() {
        let ampm = if h < 12 { "上午" } else { "下午" };
        let h12 = match h % 12 {
            0 => 12,
            x => x,
        };
        format!("{}{}:{:02}", ampm, h12, m)
    } else {
        format!("{:02}:{:02}", h, m)
    }
}

/// 存储时间 "2026-10-05 14:30" → 时:分 文本（遵循 12 小时制设置；兼容不补零/纯日期）
pub fn fmt_time_str(s: &str) -> String {
    match parse_start(s) {
        Some(dt) => fmt_hm(dt.hour(), dt.minute()),
        None => String::new(),
    }
}

/// "2026年9月21日 周一"
pub fn fmt_date_cn(d: NaiveDate) -> String {
    let wd = ["日", "一", "二", "三", "四", "五", "六"][d.weekday().num_days_from_sunday() as usize];
    format!("{}年{}月{}日 周{}", d.year(), d.month(), d.day(), wd)
}

/// "2026年9月21日 周一 08:30"（遵循 12 小时制设置）
pub fn fmt_dt_cn(dt: chrono::NaiveDateTime) -> String {
    format!("{} {}", fmt_date_cn(dt.date()), fmt_hm(dt.hour(), dt.minute()))
}

/// 存储格式 "%Y-%m-%d %H:%M"
pub fn fmt_dt_store(dt: chrono::NaiveDateTime) -> String {
    format!("{:04}-{:02}-{:02} {:02}:{:02}", dt.year(), dt.month(), dt.day(), dt.hour(), dt.minute())
}

/// 存储格式 "%Y-%m-%d"
pub fn fmt_d_store(d: NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

// ---------------- 条目定位操作（编辑/删除/仅此次） ----------------

/// 删除 (key, idx) 处的条目（空列表顺带移除 key）。返回是否删除。
pub fn agenda_remove_at(map: &mut AgendaMap, key: &str, idx: usize) -> bool {
    let mut removed = false;
    if let Some(v) = map.get_mut(key) {
        if idx < v.len() {
            v.remove(idx);
            removed = true;
        }
        if v.is_empty() {
            map.remove(key);
        }
    }
    removed
}

/// 「仅删除/修改这一天」：给重复日程条目追加例外日期。返回是否追加。
pub fn agenda_skip_day(map: &mut AgendaMap, key: &str, idx: usize, date: NaiveDate) -> bool {
    let Some(v) = map.get_mut(key) else { return false };
    let Some(AgendaEntry::Rich(r)) = v.get_mut(idx) else { return false };
    if r.recur.is_none() {
        return false;
    }
    if !recur_skipped(&r.skip_dates, date) {
        r.skip_dates.push(crate::ics::key_of_date(date));
    }
    true
}

// ---------------- 快捷输入时间解析 ----------------
/// 解析文本开头的时间点，返回 (消耗字节数, 分钟数 0..1439)
/// 支持 "14:30" / "9:05" / "9点" / "9点半" / "14点30分"。
/// 裸数字（无冒号/无“点”）后面必须跟空格、区间符或结尾，避免把“3件事”当成 3:00。
fn parse_time_prefix(s: &str) -> Option<(usize, u32)> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let mut h = 0u32;
    let mut digits = 0;
    while i < b.len() && b[i].is_ascii_digit() && digits < 2 {
        h = h * 10 + (b[i] - b'0') as u32;
        i += 1;
        digits += 1;
    }
    if digits == 0 || h > 23 {
        return None;
    }
    let mut m = 0u32;
    let mut explicit = false;
    if i < b.len() && (b[i] == b':') {
        explicit = true;
        i += 1;
        let mut d2 = 0;
        while i < b.len() && b[i].is_ascii_digit() && d2 < 2 {
            m = m * 10 + (b[i] - b'0') as u32;
            i += 1;
            d2 += 1;
        }
        if d2 == 0 {
            return None;
        }
    } else if s[i..].starts_with('点') {
        explicit = true;
        i += '点'.len_utf8();
        if s[i..].starts_with("半") {
            i += '半'.len_utf8();
            m = 30;
        } else {
            let mut d2 = 0;
            let mut mv = 0u32;
            while i < b.len() && b[i].is_ascii_digit() && d2 < 2 {
                mv = mv * 10 + (b[i] - b'0') as u32;
                i += 1;
                d2 += 1;
            }
            if d2 > 0 {
                if s[i..].starts_with('分') {
                    i += '分'.len_utf8();
                }
                m = mv;
            }
        }
    }
    if m > 59 {
        return None;
    }
    // 裸数字后面必须跟空格/区间符/结尾（“3件事”不是时间）
    if !explicit && i < b.len() {
        let rest = &s[i..];
        let sep = rest.starts_with(' ') || rest.starts_with('-') || rest.starts_with('~') || rest.starts_with('—');
        if !sep {
            return None;
        }
    }
    Some((i, h * 60 + m))
}

/// 快捷输入解析：开头的时间（可带 "-" / "~" / "—" 区间）+ 后续文本。
/// 返回 (开始分钟, 结束分钟, 日程名)；无时间或名字为空返回 None（走纯文本旧条目）。
pub fn parse_quick_time(text: &str) -> Option<(u32, Option<u32>, String)> {
    let t = text.trim();
    let (i1, start) = parse_time_prefix(t)?;
    let rest = t[i1..].trim_start();
    // 区间结束时间
    let mut end = None;
    let after_sep = rest
        .strip_prefix('-')
        .or_else(|| rest.strip_prefix('~'))
        .or_else(|| rest.strip_prefix('—'));
    if let Some(after) = after_sep {
        let after = after.trim_start();
        if let Some((i2, em)) = parse_time_prefix(after) {
            let name = after[i2..].trim();
            if name.is_empty() {
                return None;
            }
            return Some((start, Some(em.max(start)), name.to_string()));
        }
    }
    if rest.is_empty() {
        return None;
    }
    Some((start, end, rest.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recur_hits_yearly_and_weekly() {
        let a = NaiveDate::from_ymd_opt(2026, 3, 5).unwrap();
        assert!(recur_hits("y", a, NaiveDate::from_ymd_opt(2027, 3, 5).unwrap()));
        assert!(!recur_hits("y", a, NaiveDate::from_ymd_opt(2027, 3, 6).unwrap()));
        assert!(recur_hits("w", a, NaiveDate::from_ymd_opt(2026, 3, 12).unwrap()));
        assert!(!recur_hits("w", a, NaiveDate::from_ymd_opt(2026, 3, 13).unwrap()));
        // 锚点日当天不算“展开命中”（由原生条目负责）
        assert!(!recur_hits("y", a, a));
    }

    #[test]
    fn recur_hits_lunar_yearly() {
        // 任取锚点日，向前找下一个农历月/日相同的日子，应命中；再往后一年也应命中
        let a = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
        let la = crate::lunar::solar_to_lunar(a).unwrap();
        let mut d = a;
        let mut found = 0;
        for _ in 0..800 {
            d = d.succ_opt().unwrap();
            if let Some(ld) = crate::lunar::solar_to_lunar(d) {
                if ld.month == la.month && ld.day == la.day {
                    assert!(recur_hits("l", a, d));
                    found += 1;
                    if found == 2 {
                        break;
                    }
                }
            }
        }
        assert_eq!(found, 2, "两年内应各命中一次农历重复");
    }

    #[test]
    fn recur_occurs_respects_until_and_skip() {
        let a = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 10, 2).unwrap();
        let d3 = NaiveDate::from_ymd_opt(2026, 10, 3).unwrap();
        // 每天 + 截止 10-2：2 号命中、3 号超限；截止日早于锚点日时整组失效（含锚点日）
        assert!(recur_occurs(Some("2026-10-2"), &[], "d", a, d2));
        assert!(!recur_occurs(Some("2026-10-2"), &[], "d", a, d3));
        assert!(recur_occurs(Some("2026-10-1"), &[], "d", a, a));
        assert!(!recur_occurs(Some("2026-9-1"), &[], "d", a, a));
        // 仅此次排除 2 号：2 号消失，3 号照常
        let skips = vec!["2026-10-2".to_string()];
        assert!(!recur_occurs(None, &skips, "d", a, d2));
        assert!(recur_occurs(None, &skips, "d", a, d3));
    }

    #[test]
    fn agenda_on_until_and_skip() {
        let a = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let mut map = AgendaMap::new();
        let key = crate::ics::key_of_date(a);
        map.insert(
            key.clone(),
            vec![AgendaEntry::Rich(RichEvent {
                id: "x".into(),
                name: "晨会".into(),
                all_day: false,
                start: "2026-10-1 09:00".into(),
                end: "2026-10-1 10:00".into(),
                remind: None,
                repeat: None,
                recur: Some("d".into()),
                recur_until: Some("2026-10-2".into()),
                skip_dates: vec!["2026-10-3".into()],
            })],
        );
        assert_eq!(agenda_on(&map, a).len(), 1); // 锚点日
        assert_eq!(agenda_on(&map, NaiveDate::from_ymd_opt(2026, 10, 2).unwrap()).len(), 1); // 截止日内
        assert_eq!(agenda_on(&map, NaiveDate::from_ymd_opt(2026, 10, 3).unwrap()).len(), 0); // 仅此次
        assert_eq!(agenda_on(&map, NaiveDate::from_ymd_opt(2026, 10, 4).unwrap()).len(), 0); // 超截止
    }

    #[test]
    fn parse_quick_time_cases() {
        assert_eq!(parse_quick_time("14:30 项目评审"), Some((870, None, "项目评审".into())));
        assert_eq!(parse_quick_time("9:00~10:00 晨会"), Some((540, Some(600), "晨会".into())));
        assert_eq!(parse_quick_time("9点 开会"), Some((540, None, "开会".into())));
        assert_eq!(parse_quick_time("9点半-11点半 午休"), Some((570, Some(690), "午休".into())));
        assert_eq!(parse_quick_time("14点30分 复盘"), Some((870, None, "复盘".into())));
        // 冒号/点形式可直接接文字；裸数字必须跟空格或区间符
        assert_eq!(parse_quick_time("9点开会"), Some((540, None, "开会".into())));
        assert_eq!(parse_quick_time("14:30开会"), Some((870, None, "开会".into())));
        // 无时间 / 纯数字 / 只有时间 → 旧文本条目
        assert_eq!(parse_quick_time("买菜"), None);
        assert_eq!(parse_quick_time("3件事要办"), None);
        assert_eq!(parse_quick_time("2026年计划"), None);
        assert_eq!(parse_quick_time("14:00"), None);
        assert_eq!(parse_quick_time(""), None);
    }
}

