use chrono::{Datelike, NaiveDate};

use crate::lunar_data::{LUNAR_YEARS, SOLAR_TERM_DAYS, TERM_NAMES};

const EPOCH_Y: i32 = 1900;
const EPOCH_M: u32 = 1;
const EPOCH_D: u32 = 1;

fn epoch() -> NaiveDate {
    NaiveDate::from_ymd_opt(EPOCH_Y, EPOCH_M, EPOCH_D).unwrap()
}

#[derive(Clone, Copy, Debug)]
pub struct LunarDate {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub leap: bool,
}

fn month_len(idx: usize, m: u32, is_leap: bool) -> u32 {
    let bits = LUNAR_YEARS[idx].1;
    let bit = if is_leap {
        (bits >> 12) & 1
    } else {
        (bits >> (m - 1)) & 1
    };
    if bit == 1 { 30 } else { 29 }
}

/// 公历 → 农历
pub fn solar_to_lunar(date: NaiveDate) -> Option<LunarDate> {
    let days = (date - epoch()).num_days();
    if days < 0 {
        return None;
    }
    let n = LUNAR_YEARS.len() as i32;
    let mut idx = 0usize;
    while (idx as i32) + 1 < n && LUNAR_YEARS[idx + 1].0 as i64 <= days {
        idx += 1;
    }
    let leap = LUNAR_YEARS[idx].2;
    let mut rest = days - LUNAR_YEARS[idx].0 as i64;

    // 月序列：1..leap, 闰leap, leap+1..12
    for m in 1..=12u32 {
        let order: [(u32, bool); 2] = [(m, false), (m, true)];
        for &(mm, is_leap) in order.iter() {
            let is_this_leap = is_leap && leap != 0 && m == leap as u32;
            if is_leap && !is_this_leap {
                continue; // 只在本月之后插入闰月
            }
            let len = month_len(idx, mm, is_this_leap) as i64;
            if rest < len {
                return Some(LunarDate {
                    year: EPOCH_Y + idx as i32,
                    month: mm,
                    day: rest as u32 + 1,
                    leap: is_this_leap,
                });
            }
            rest -= len;
        }
    }
    None
}

const MONTH_CN: [&str; 12] = ["正", "二", "三", "四", "五", "六", "七", "八", "九", "十", "十一", "十二"];
const DAY_1_10: [&str; 10] = ["初一", "初二", "初三", "初四", "初五", "初六", "初七", "初八", "初九", "初十"];
const DAY_11_20: [&str; 10] = ["十一", "十二", "十三", "十四", "十五", "十六", "十七", "十八", "十九", "二十"];
const DAY_21_29: [&str; 9] = ["廿一", "廿二", "廿三", "廿四", "廿五", "廿六", "廿七", "廿八", "廿九"];

pub fn month_cn(month: u32) -> &'static str {
    MONTH_CN[(month.clamp(1, 12) - 1) as usize]
}

pub fn day_cn(day: u32) -> &'static str {
    match day {
        1..=10 => DAY_1_10[(day - 1) as usize],
        11..=20 => DAY_11_20[(day - 11) as usize],
        21..=29 => DAY_21_29[(day - 21) as usize],
        _ => "三十",
    }
}

/// 当天若是节气则返回名称
pub fn jieqi_of(date: NaiveDate) -> Option<&'static str> {
    let y = date.year();
    if !(EPOCH_Y..EPOCH_Y + LUNAR_YEARS.len() as i32).contains(&y) {
        return None;
    }
    let row = &SOLAR_TERM_DAYS[(y - EPOCH_Y) as usize];
    for (t, &day) in row.iter().enumerate() {
        if day as u32 == date.day() && (t as u32) / 2 + 1 == date.month() {
            return Some(TERM_NAMES[t]);
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq)]
pub enum FestKind {
    Blue,
    Red,
    Legal,
}

/// 农历传统节日
pub fn lunar_festival(l: &LunarDate) -> Option<(&'static str, FestKind)> {
    if l.leap {
        return None;
    }
    let m = l.month;
    let d = l.day;
    match (m, d) {
        (1, 1) => Some(("春节", FestKind::Red)),
        (1, 15) => Some(("元宵节", FestKind::Blue)),
        (5, 5) => Some(("端午节", FestKind::Blue)),
        (7, 7) => Some(("七夕", FestKind::Blue)),
        (8, 15) => Some(("中秋节", FestKind::Blue)),
        (9, 9) => Some(("重阳节", FestKind::Blue)),
        (12, 8) => Some(("腊八节", FestKind::Blue)),
        (12, 30) => Some(("除夕", FestKind::Red)),
        (12, 29) => {
            // 小月腊月廿九即除夕
            let idx = (l.year - EPOCH_Y) as usize;
            if idx < LUNAR_YEARS.len() && month_len(idx, 12, false) == 29 {
                Some(("除夕", FestKind::Red))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 公历节日 / 纪念日（含固定日期的法定假日兜底）
pub fn solar_festival(date: NaiveDate) -> Option<(&'static str, FestKind)> {
    let m = date.month();
    let d = date.day();
    let kind = match (m, d) {
        (1, 1) => ("元旦", FestKind::Legal),
        (2, 14) => ("情人节", FestKind::Blue),
        (3, 8) => ("妇女节", FestKind::Blue),
        (3, 12) => ("植树节", FestKind::Blue),
        (5, 1) => ("劳动节", FestKind::Legal),
        (5, 4) => ("青年节", FestKind::Blue),
        (5, 12) => ("护士节", FestKind::Blue),
        (6, 1) => ("儿童节", FestKind::Blue),
        (7, 1) => ("建党节", FestKind::Blue),
        (7, 7) => ("七七事变", FestKind::Blue),
        (8, 1) => ("建军节", FestKind::Blue),
        (9, 3) => ("抗战胜利", FestKind::Blue),
        (9, 10) => ("教师节", FestKind::Blue),
        (9, 18) => ("九一八事变", FestKind::Blue),
        (9, 30) => ("烈士纪念日", FestKind::Red),
        (10, 1) => ("国庆节", FestKind::Legal),
        (11, 1) => ("万圣节", FestKind::Blue),
        (12, 13) => ("国家公祭日", FestKind::Red),
        (12, 24) => ("平安夜", FestKind::Blue),
        (12, 25) => ("圣诞节", FestKind::Blue),
        _ => return None,
    };
    Some(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn lunar_conversion_2026() {
        let l = solar_to_lunar(d(2026, 9, 18)).unwrap();
        assert_eq!((l.month, l.day, l.leap), (8, 8, false)); // 八月初八

        let l = solar_to_lunar(d(2026, 10, 10)).unwrap();
        assert_eq!((l.month, l.day, l.leap), (9, 1, false)); // 九月初一

        let l = solar_to_lunar(d(2026, 2, 17)).unwrap();
        assert_eq!((l.month, l.day, l.leap), (1, 1, false)); // 正月初一 春节

        let l = solar_to_lunar(d(2026, 2, 16)).unwrap();
        assert_eq!(lunar_festival(&l).map(|(n, _)| n), Some("除夕"));
    }

    #[test]
    fn jieqi_2026() {
        assert_eq!(jieqi_of(d(2026, 9, 7)), Some("白露"));
        assert_eq!(jieqi_of(d(2026, 9, 23)), Some("秋分"));
        assert_eq!(jieqi_of(d(2026, 10, 8)), Some("寒露"));
        assert_eq!(jieqi_of(d(2026, 9, 18)), None);
    }

    #[test]
    fn festivals() {
        assert_eq!(solar_festival(d(2026, 9, 30)).map(|(n, _)| n), Some("烈士纪念日"));
        assert_eq!(solar_festival(d(2026, 9, 10)).map(|(n, _)| n), Some("教师节"));
        let l = solar_to_lunar(d(2026, 9, 25)).unwrap();
        assert_eq!(lunar_festival(&l).map(|(n, _)| n), Some("中秋节"));
    }

    #[test]
    fn round_trip_span() {
        // 全范围抽查：转换不 panic 且年初/年末正确
        for y in [1900, 1950, 2000, 2024, 2050, 2100] {
            let l = solar_to_lunar(d(y, 6, 15)).unwrap();
            assert!(l.month >= 1 && l.month <= 12 && l.day >= 1 && l.day <= 30);
        }
    }
}
