//! 日程/待办共享数据类型与存取
//! agenda.json 兼容旧的纯文本条目：新条目存为对象，旧条目仍是字符串（serde untagged）
use std::collections::HashMap;

use chrono::{Datelike, NaiveDate, Timelike};

/// 富日程条目（日期右键“新增日程”创建）
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RichEvent {
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
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save(map: &AgendaMap) {
    let path = crate::config::data_dir().join("agenda.json");
    if let Ok(text) = serde_json::to_string(map) {
        let _ = std::fs::write(path, text);
    }
}

/// 列表展示文本（旧条目原样，富条目加时间/全天前缀）
pub fn display(e: &AgendaEntry) -> String {
    match e {
        AgendaEntry::Legacy(s) => s.clone(),
        AgendaEntry::Rich(r) => {
            if r.all_day {
                format!("全天 {}", r.name)
            } else if r.start.len() >= 16 && r.start.as_bytes()[10] == b' ' {
                format!("{} {}", &r.start[11..16], r.name)
            } else {
                r.name.clone()
            }
        }
    }
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

/// "2026年9月21日 周一"
pub fn fmt_date_cn(d: NaiveDate) -> String {
    let wd = ["日", "一", "二", "三", "四", "五", "六"][d.weekday().num_days_from_sunday() as usize];
    format!("{}年{}月{}日 周{}", d.year(), d.month(), d.day(), wd)
}

/// "2026年9月21日 周一 08:30"
pub fn fmt_dt_cn(dt: chrono::NaiveDateTime) -> String {
    format!("{} {:02}:{:02}", fmt_date_cn(dt.date()), dt.hour(), dt.minute())
}

/// 存储格式 "%Y-%m-%d %H:%M"
pub fn fmt_dt_store(dt: chrono::NaiveDateTime) -> String {
    format!("{:04}-{:02}-{:02} {:02}:{:02}", dt.year(), dt.month(), dt.day(), dt.hour(), dt.minute())
}

/// 存储格式 "%Y-%m-%d"
pub fn fmt_d_store(d: NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}
