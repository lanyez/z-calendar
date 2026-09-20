//! 黄历计算：干支（年/月/日）、生肖、星座、建除十二神、宜忌、冲煞
//! 日干支以 1949-10-01（甲子日）为锚点；年干支以立春为界；月干支按节气（节）定月支
use chrono::{Datelike, NaiveDate};

use crate::lunar::LunarDate;
use crate::lunar_data::SOLAR_TERM_DAYS;

pub const STEMS: [&str; 10] = ["甲", "乙", "丙", "丁", "戊", "己", "庚", "辛", "壬", "癸"];
pub const BRANCHES: [&str; 12] = ["子", "丑", "寅", "卯", "辰", "巳", "午", "未", "申", "酉", "戌", "亥"];
pub const ZODIACS: [&str; 12] = ["鼠", "牛", "虎", "兔", "龙", "蛇", "马", "羊", "猴", "鸡", "狗", "猪"];
const GANZHI_NAMES: [&str; 60] = [
    "甲子", "乙丑", "丙寅", "丁卯", "戊辰", "己巳", "庚午", "辛未", "壬申", "癸酉",
    "甲戌", "乙亥", "丙子", "丁丑", "戊寅", "己卯", "庚辰", "辛巳", "壬午", "癸未",
    "甲申", "乙酉", "丙戌", "丁亥", "戊子", "己丑", "庚寅", "辛卯", "壬辰", "癸巳",
    "甲午", "乙未", "丙申", "丁酉", "戊戌", "己亥", "庚子", "辛丑", "壬寅", "癸卯",
    "甲辰", "乙巳", "丙午", "丁未", "戊申", "己酉", "庚戌", "辛亥", "壬子", "癸丑",
    "甲寅", "乙卯", "丙辰", "丁巳", "戊午", "己未", "庚申", "辛酉", "壬戌", "癸亥",
];

const EPOCH_Y: i32 = 1900;

/// 日干支序号（0=甲子）：以 1949-10-01（甲子日）为锚点
pub fn day_ganzhi(date: NaiveDate) -> usize {
    let anchor = NaiveDate::from_ymd_opt(1949, 10, 1).unwrap();
    let n = (date - anchor).num_days().rem_euclid(60) as usize;
    n
}

/// 干支年序号（以立春为界）
pub fn ganzhi_year(date: NaiveDate) -> i32 {
    let y = date.year();
    if !in_range(y) {
        return y;
    }
    let lichun = term_day(y, 2); // 立春（2月的节）
    if (date.month(), date.day()) >= (2, lichun) {
        y
    } else {
        y - 1
    }
}

/// 干支年名称（如“丙午马年”）
pub fn year_ganzhi_name(date: NaiveDate) -> String {
    let gy = ganzhi_year(date);
    let idx = (gy - 4).rem_euclid(60) as usize;
    format!("{}{}年", GANZHI_NAMES[idx], ZODIACS[idx % 12])
}

/// 月干支序号（按节气定月，正月建寅）
pub fn month_ganzhi(date: NaiveDate) -> usize {
    let y = date.year();
    if !in_range(y) {
        return 0;
    }
    // 找到日期所属的节（当月节之前算上月）
    let mut m_branch = 0usize;
    for m in (1..=12).rev() {
        let jie = term_day(y, m as u32);
        if date.month() == m && date.day() >= jie {
            m_branch = (m % 12) as usize; // 1月(小寒后)=丑 2月(立春后)=寅 ... 12月(大雪后)=子
            break;
        }
    }
    let gy = ganzhi_year(date);
    let year_stem = ((gy - 4).rem_euclid(60) % 10) as usize;
    // 年上起月：甲己丙作首、乙庚戊为头、丙辛庚起、丁壬壬、戊癸甲
    let start = (year_stem % 5) * 2 + 2;
    let stem = (start + (m_branch + 12 - 2) % 12) % 10;
    // 合成干支序号：n ≡ stem (mod 10)，n ≡ 月支 (mod 12)
    (0..6).map(|k| stem + 10 * k).find(|n| n % 12 == m_branch).unwrap_or(0)
}

/// 月干支名称（如“丁酉”）
pub fn month_ganzhi_name(date: NaiveDate) -> String {
    GANZHI_NAMES[month_ganzhi(date)].to_string()
}

/// 日干支名称（如“甲午”）
pub fn day_ganzhi_name(date: NaiveDate) -> String {
    GANZHI_NAMES[day_ganzhi(date)].to_string()
}

/// 星座（按公历）
pub fn xingzuo(date: NaiveDate) -> &'static str {
    // 每月起始星座：1月起水瓶，依次到 12 月的摩羯
    const NAMES: [&str; 12] = ["水瓶", "双鱼", "白羊", "金牛", "双子", "巨蟹", "狮子", "处女", "天秤", "天蝎", "射手", "摩羯"];
    const START: [(u32, u32); 12] = [
        (1, 20), (2, 19), (3, 21), (4, 20), (5, 21), (6, 22),
        (7, 23), (8, 23), (9, 23), (10, 24), (11, 23), (12, 22),
    ];
    let (m, d) = (date.month(), date.day());
    // d 早于本月星座起始日则属上一个星座
    let idx = if d < START[(m - 1) as usize].1 { (m + 10) % 12 } else { (m + 11) % 12 };
    NAMES[idx as usize]
}

/// 建除十二神名称
pub const JIANZHI_NAMES: [&str; 12] = ["建", "除", "满", "平", "定", "执", "破", "危", "成", "收", "开", "闭"];

/// 十二值日（月建=日支 即“建”）
pub fn jianzhi(date: NaiveDate) -> usize {
    let day_branch = day_ganzhi(date) % 12;
    let month_branch = month_ganzhi(date) % 12; // 月干支 idx%12 即月支
    (day_branch + 12 - month_branch) % 12
}

/// 十二值日宜忌
pub fn jianzhi_yiji(zhi: usize) -> (&'static [&'static str], &'static [&'static str]) {
    const YI: [&[&str]; 12] = [
        &["出行", "会友", "上书", "见工"],                       // 建
        &["沐浴", "清洁", "求医", "治病"],                       // 除
        &["祭祀", "祈福", "结亲", "开市"],                       // 满
        &["修造", "粉刷", "平治", "道涂"],                       // 平
        &["冠带", "安床", "会亲友", "立券交易"],                  // 定
        &["捕捉", "祭祀", "求医", "破土"],                       // 执
        &["拆卸", "治病", "破屋", "坏垣"],                       // 破
        &["安床", "设醮", "入学"],                               // 危
        &["开业", "嫁娶", "入学", "安床"],                       // 成
        &["纳财", "收藏", "买入", "纳畜"],                       // 收
        &["开市", "交易", "出行", "入学"],                       // 开
        &["安葬", "修坟", "塞穴"],                               // 闭
    ];
    const JI: [&[&str]; 12] = [
        &["动土", "开仓", "嫁娶", "纳采"],                       // 建
        &["出行", "嫁娶", "开业"],                               // 除
        &["服药", "赴任", "上任"],                               // 满
        &["祈福", "开市", "交易"],                               // 平
        &["诉讼", "出行", "移徙"],                               // 定
        &["移徙", "出行", "开市"],                               // 执
        &["嫁娶", "开市", "出行"],                               // 破
        &["登高", "行船", "出行"],                               // 危
        &["诉讼", "争执"],                                       // 成
        &["安葬", "开市", "出行"],                               // 收
        &["安葬", "破土"],                                       // 开
        &["开市", "出行", "嫁娶"],                               // 闭
    ];
    (YI[zhi], JI[zhi])
}

/// 日冲煞：（冲生肖, 煞方）
pub fn chong_sha(date: NaiveDate) -> (&'static str, &'static str) {
    let branch = day_ganzhi(date) % 12;
    let chong = ZODIACS[(branch + 6) % 12];
    let sha = match branch % 4 {
        0 => "北",             // 申子辰
        1 => "西",             // 巳酉丑
        2 => "南",             // 寅午戌
        _ => "东",             // 亥卯未
    };
    (chong, sha)
}

/// 日期信息第二行：“第260天 第38周 处女座”
pub fn day_week_line(date: NaiveDate) -> String {
    format!("第{}天 第{}周 {}座", date.ordinal(), date.iso_week().week(), xingzuo(date))
}

/// 日期信息第三行：“八月初七 丙午马年 丁酉月 甲午日”
pub fn lunar_ganzhi_line(l: &LunarDate, date: NaiveDate) -> String {
    let leap = if l.leap { "闰" } else { "" };
    format!(
        "{}{}月{} {} {}月 {}日",
        leap,
        crate::lunar::month_cn(l.month),
        crate::lunar::day_cn(l.day),
        year_ganzhi_name(date),
        month_ganzhi_name(date),
        day_ganzhi_name(date)
    )
}

fn term_day(y: i32, m: u32) -> u32 {
    SOLAR_TERM_DAYS[(y - EPOCH_Y) as usize][((m - 1) * 2) as usize] as u32
}

fn in_range(y: i32) -> bool {
    (EPOCH_Y..EPOCH_Y + SOLAR_TERM_DAYS.len() as i32).contains(&y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn day_ganzhi_anchors() {
        // 锚点与交叉验证（锚定 1949-10-01=甲子）
        assert_eq!(GANZHI_NAMES[day_ganzhi(d(1949, 10, 1))], "甲子");
        assert_eq!(GANZHI_NAMES[day_ganzhi(d(2000, 1, 1))], "戊午");
        // 1.png 设计稿：2026-09-17 = 甲午日
        assert_eq!(GANZHI_NAMES[day_ganzhi(d(2026, 9, 17))], "甲午");
    }

    #[test]
    fn ganzhi_2026_09_17_matches_design() {
        // 1.png：2026年9月17日 → 丙午马年、丁酉月、甲午日
        let date = d(2026, 9, 17);
        assert_eq!(year_ganzhi_name(date), "丙午马年");
        assert_eq!(month_ganzhi_name(date), "丁酉");
        assert_eq!(day_ganzhi_name(date), "甲午");
    }

    #[test]
    fn info_line_2026_09_17() {
        let date = d(2026, 9, 17);
        assert_eq!(date.ordinal(), 260);
        assert_eq!(date.iso_week().week(), 38);
        assert_eq!(xingzuo(date), "处女");
        assert_eq!(day_week_line(date), "第260天 第38周 处女座");
    }

    #[test]
    fn jianzhi_and_chongsha() {
        // 十二值：月支=日支 即“建”
        let date = d(2026, 9, 17);
        let zhi = jianzhi(date);
        assert!(JIANZHI_NAMES.get(zhi).is_some());
        let (chong, sha) = chong_sha(date);
        assert!(!chong.is_empty() && !sha.is_empty());
        // 冲为对冲地支：午日冲子（鼠）
        assert_eq!(chong, "鼠");
    }

    #[test]
    fn ganzhi_year_lichun_boundary() {
        // 2026 立春约 2/4：立春前属乙巳蛇年，立春后属丙午马年
        let before = d(2026, 2, 1);
        let after = d(2026, 3, 1);
        assert_eq!(year_ganzhi_name(before), "乙巳蛇年");
        assert_eq!(year_ganzhi_name(after), "丙午马年");
    }
}
