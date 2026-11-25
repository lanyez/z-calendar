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

/// 假期数据代次：共享表被替换（启动加载 / 定时拉取成功）后 +1。
/// 月历派生缓存（flyout::MonthCache，次行文本含法定节假日名）据此失效。
static HOLIDAY_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub fn holiday_gen() -> u64 {
    HOLIDAY_GEN.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn bump_holiday_gen() {
    HOLIDAY_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

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
        // 缓存文件：原子写（不保留备份，损坏时下次拉取即自愈）
        let _ = crate::config::write_file_atomic(&path, &text);
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
        } else {
            // 无类型标记（含 DESCRIPTION 带"休"）：默认按休
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
    let r = fetch_map_once(url).or_else(|| {
        // 主源失败且 URL 是默认 jsdelivr 域名时，换官方镜像域名重试（自定义 URL 不动）
        let (host, path) = split_host_path(url)?;
        if host != "cdn.jsdelivr.net" {
            return None;
        }
        ["fastly.jsdelivr.net", "gcore.jsdelivr.net"]
            .iter()
            .find_map(|m| fetch_map_once(&format!("https://{}{}", m, path)))
    });
    if r.is_none() {
        crate::log::warn("节假日数据拉取失败（主源+镜像均失败）");
    }
    r
}

fn split_host_path(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://")?;
    let i = rest.find('/')?;
    Some((&rest[..i], &rest[i..]))
}

fn fetch_map_once(url: &str) -> Option<HolidayMap> {
    let resp = crate::net::agent()
        .get(url)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// chinese-days holidays.ics 的形态：日期值、CRLF、折行的 SUMMARY
    const SAMPLE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nDTSTART;VALUE=DATE:20261001\r\nDTEND;VALUE=DATE:20261008\r\nSUMMARY:国庆节(休)\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nDTSTART;VALUE=DATE:20261010\r\nDTEND;VALUE=DATE:20261011\r\nSUMMARY:国庆节(班)\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nDTSTART:20260926T000000\r\nDTEND:20260927T000000\r\nSUMMARY:\r\n  中秋节(休)\r\nDESCRIPTION:休\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nDTSTART;VALUE=DATE:20261011\r\nSUMMARY:缺结束日(休)\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    fn d(y: i32, m: u32, dd: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, dd).unwrap()
    }

    #[test]
    fn parses_multi_day_holiday_workday_and_folded_lines() {
        let map = parse_ics(SAMPLE);
        let first = map.get(&key_of_date(d(2026, 10, 1))).expect("国庆首日");
        assert_eq!(first.name, "国庆节");
        assert!(first.ty == DayType::Xiu);
        assert_eq!((first.idx, first.len), (0, 7));
        let last = map.get(&key_of_date(d(2026, 10, 7))).unwrap();
        assert_eq!((last.idx, last.len), (6, 7));
        // DTEND 为排他结束日：10-8 不在假期内
        assert!(!map.contains_key(&key_of_date(d(2026, 10, 8))));

        let ban = map.get(&key_of_date(d(2026, 10, 10))).unwrap();
        assert_eq!(ban.name, "国庆节");
        assert!(ban.ty == DayType::Ban);
        assert_eq!((ban.idx, ban.len), (0, 1));

        // 折行的 SUMMARY（续行以空格开头）被拼回；日期时间形态的 DTSTART 取前 8 位
        let mid = map.get(&key_of_date(d(2026, 9, 26))).unwrap();
        assert_eq!(mid.name, "中秋节");
        assert!(mid.ty == DayType::Xiu);
        assert_eq!((mid.idx, mid.len), (0, 1));

        // 缺 DTEND：按单日处理
        let one = map.get(&key_of_date(d(2026, 10, 11))).unwrap();
        assert_eq!((one.idx, one.len), (0, 1));
        assert_eq!(map.len(), 7 + 1 + 1 + 1);
    }

    #[test]
    fn type_falls_back_to_description_then_defaults_to_xiu() {
        let mk = |summary: &str, desc: &str| {
            format!("BEGIN:VEVENT\nDTSTART;VALUE=DATE:20261010\nSUMMARY:{summary}\nDESCRIPTION:{desc}\nEND:VEVENT\n")
        };
        let ty = |text: &str| parse_ics(text).get(&key_of_date(d(2026, 10, 10))).unwrap().ty;
        // 括号后缀决定类型，全角括号同样识别，名称去掉后缀
        let m = parse_ics(&mk("春节（休）", ""));
        assert_eq!(m.get(&key_of_date(d(2026, 10, 10))).unwrap().name, "春节");
        assert!(ty(&mk("春节（休）", "")) == DayType::Xiu);
        assert!(ty(&mk("调休(班)", "")) == DayType::Ban);
        // 无后缀：看 DESCRIPTION
        assert!(ty(&mk("调休", "班")) == DayType::Ban);
        assert!(ty(&mk("假日", "休")) == DayType::Xiu);
        // 都没有：默认休
        assert!(ty(&mk("假日", "")) == DayType::Xiu);
        // 既无类型也无名称：补默认名
        let m = parse_ics("BEGIN:VEVENT\nDTSTART;VALUE=DATE:20261010\nEND:VEVENT\n");
        assert_eq!(m.get(&key_of_date(d(2026, 10, 10))).unwrap().name, "节假日");
    }

    #[test]
    fn empty_or_foreign_input_yields_empty_map() {
        assert!(parse_ics("").is_empty());
        assert!(parse_ics("<html>not an ics</html>").is_empty());
        // 缺 DTSTART 的事件丢弃
        assert!(parse_ics("BEGIN:VEVENT\nSUMMARY:无开始(休)\nEND:VEVENT\n").is_empty());
        // 非法日期
        assert!(parse_ics("BEGIN:VEVENT\nDTSTART;VALUE=DATE:20261399\nSUMMARY:x(休)\nEND:VEVENT\n").is_empty());
    }

    #[test]
    fn mirrors_are_tried_only_for_default_cdn() {
        let (host, path) = split_host_path("https://cdn.jsdelivr.net/npm/chinese-days/dist/holidays.ics").unwrap();
        assert_eq!((host, path), ("cdn.jsdelivr.net", "/npm/chinese-days/dist/holidays.ics"));
        assert!(split_host_path("http://cdn.jsdelivr.net/x").is_none(), "非 https 前缀");
        assert!(split_host_path("https://example.com").is_none(), "无路径");
    }
}
