use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DayType {
    Xiu, // 休
    Ban, // 班
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct DayInfo {
    pub name: String,
    pub ty: DayType,
    pub idx: usize, // 假期区间内第几天（0 起）
    pub len: usize, // 假期区间总天数
}

pub type HolidayMap = HashMap<String, DayInfo>;

pub fn key_of(y: i32, m: u32, d: u32) -> String {
    format!("{}-{}-{}", y, m, d)
}

pub fn key_of_date(date: NaiveDate) -> String {
    key_of(date.year(), date.month(), date.day())
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    holidays: HolidayMap,
}

pub fn load_cache() -> HolidayMap {
    let path = crate::config::data_dir().join("holidays.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<CacheFile>(&t).ok())
        .map(|c| c.holidays)
        .unwrap_or_default()
}

pub fn save_cache(map: &HolidayMap) {
    let path = crate::config::data_dir().join("holidays.json");
    if let Ok(text) = serde_json::to_string(&CacheFile { holidays: map.clone() }) {
        let _ = std::fs::write(path, text);
    }
}

fn parse_ics_date(s: &str) -> Option<NaiveDate> {
    let b = s.as_bytes();
    if b.len() < 8 {
        return None;
    }
    let y: i32 = s.get(0..4)?.parse().ok()?;
    let m: u32 = s.get(4..6)?.parse().ok()?;
    let d: u32 = s.get(6..8)?.parse().ok()?;
    NaiveDate::from_ymd_opt(y, m, d)
}

/// 解析 chinese-days 的 holidays.ics（DTEND 为-exclusive 结束日）
pub fn parse_ics(text: &str) -> HolidayMap {
    let mut map = HolidayMap::new();
    let unfolded: String = text.replace("\r\n", "\n").replace("\n ", "").replace("\n\t", "");
    let mut ev: Option<(Option<NaiveDate>, Option<NaiveDate>, String, String, String, String)> =
        None; // (start, end, summary, desc, cat, special)

    for raw in unfolded.lines() {
        let line = raw.trim();
        match line {
            "BEGIN:VEVENT" => ev = Some((None, None, String::new(), String::new(), String::new(), String::new())),
            "END:VEVENT" => {
                if let Some((start, end, summary, desc, _cat, special)) = ev.take() {
                    add_event(start, end, &summary, &desc, &special, &mut map);
                }
            }
            _ => {
                if let Some(e) = ev.as_mut() {
                    if let Some(i) = line.find(':') {
                        let key = line[..i].to_uppercase();
                        let val = line[i + 1..].trim();
                        match key.as_str() {
                            k if k.starts_with("DTSTART") => e.0 = parse_ics_date(val),
                            k if k.starts_with("DTEND") => e.1 = parse_ics_date(val),
                            "SUMMARY" => e.2 = val.to_string(),
                            "DESCRIPTION" => e.3 = val.to_string(),
                            "CATEGORIES" => e.4 = val.to_string(),
                            "X-APPLE-SPECIAL-DAY" => e.5 = val.to_string(),
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    map
}

fn add_event(
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    summary: &str,
    desc: &str,
    special: &str,
    map: &mut HolidayMap,
) {
    let start = match start {
        Some(d) => d,
        None => return,
    };
    let end = match end {
        Some(e) if e > start => e,
        _ => start.succ_opt().unwrap(),
    };

    let mut name = summary.trim().to_string();
    let mut ty: Option<DayType> = None;
    // SUMMARY 形如 "国庆节(休)" / "国庆节(班)"（按字符处理，兼容全角括号）
    {
        let chars: Vec<char> = name.chars().collect();
        if chars.len() >= 3 {
            let last = *chars.last().unwrap();
            if last == ')' || last == '）' {
                if let Some(open_pos) = chars[..chars.len() - 1]
                    .iter()
                    .rposition(|&c| c == '(' || c == '（')
                {
                    let inner: String = chars[open_pos + 1..chars.len() - 1].iter().collect();
                    let inner = inner.trim();
                    if inner == "休" || inner == "班" {
                        ty = Some(if inner == "班" { DayType::Ban } else { DayType::Xiu });
                        name = chars[..open_pos].iter().collect::<String>().trim().to_string();
                    }
                }
            }
        }
    }
    let ty = ty.unwrap_or_else(|| {
        if special == "ALTERNATE-WORKDAY" {
            DayType::Ban
        } else if special == "WORK-HOLIDAY" {
            DayType::Xiu
        } else if desc.contains('班') {
            DayType::Ban
        } else if desc.contains('休') {
            DayType::Xiu
        } else {
            DayType::Xiu
        }
    });
    if name.is_empty() {
        name = if ty == DayType::Ban { "补班".into() } else { "节假日".into() };
    }

    let len = (end - start).num_days() as usize;
    let mut cur = start;
    let mut idx = 0usize;
    while cur < end {
        map.insert(
            key_of_date(cur),
            DayInfo { name: name.clone(), ty, idx, len },
        );
        cur = cur.succ_opt().unwrap();
        idx += 1;
    }
}

pub fn fetch_map(url: &str) -> Option<HolidayMap> {
    let resp = ureq::get(url)
        .timeout(std::time::Duration::from_secs(20))
        .call()
        .ok()?;
    let text = resp.into_string().ok()?;
    if !text.contains("BEGIN:VCALENDAR") {
        return None;
    }
    let map = parse_ics(&text);
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}
