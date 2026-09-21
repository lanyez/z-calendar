//! 历史上的今天：开源数据集（PrintNow/TodayInHistory，源自维基百科，3万余条中文事件）
//! asilu 接口只返回服务器当天且忽略日期参数，无法满足“任意日期”查询，故改用本数据集。
//!
//! 缓存策略（数据集按月拆分为 data_dir/history/{1..12}.json）：
//! - 点击日期时若当月缓存已存在 → 直接读缓存，不请求网络
//! - 首次点击某月日期 → 后台拉取一次全量数据集，按月拆分写入缓存并刷新侧栏
//!   （一次拉取即生成全部 12 个月缓存，之后永远走本地）
//! - 拉取失败 10 分钟内不重复请求；期间/失败时侧栏用内置精选事件兜底
//! - 显示：按“历史意义”评分挑选（历史事件 > 出生/逝世，重大历史节点加权），最多 3 条
use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// 数据集镜像（CDN 优先，GitHub 原始地址兜底）
const SOURCES: &[&str] = &[
    "https://cdn.jsdelivr.net/gh/PrintNow/TodayInHistory@master/history_in_today.json",
    "https://raw.githubusercontent.com/PrintNow/TodayInHistory/master/history_in_today.json",
];
/// 拉取失败后的最小重试间隔
const RETRY_MS: u64 = 10 * 60 * 1000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryItem {
    pub year: i32,
    pub title: String,
}

/// 月缓存文件：该月每天的事件（已按重要性从高到低排序）
#[derive(Serialize, Deserialize)]
struct MonthFile {
    entries: Vec<StoredEntry>,
}

#[derive(Serialize, Deserialize)]
struct StoredEntry {
    y: i32,
    d: u32,
    t: String,
    /// 数据集类型：1=事件 2=出生 3=逝世（旧版缓存无此字段默认 0）
    #[serde(default)]
    k: u8,
}

#[derive(Deserialize)]
struct RawEntry {
    year: String,
    month: String,
    day: String,
    data: String,
    #[serde(rename = "type")]
    ty: u8,
}

/// 重要性评分：越大越优先展示
/// （历史事件 > 出生/逝世；涉及中国及重大历史节点加权；过长文本轻微降权便于单行展示）
fn significance(kind: u8, title: &str) -> i32 {
    let mut s = match kind {
        1 => 10, // 历史事件
        _ => 0,  // 出生/逝世/未知
    };
    const KW: &[(&str, i32)] = &[
        ("新中国", 9), ("建国", 8), ("中华人民共和", 9), ("辛亥", 7), ("五四", 7),
        ("中国", 6), ("中华", 6), ("革命", 6), ("原子弹", 6), ("登月", 6), ("大屠杀", 6),
        ("成立", 5), ("战争", 5), ("爆发", 5), ("起义", 5), ("条约", 5), ("投降", 5),
        ("事变", 5), ("开国", 5), ("联合国", 5), ("奥运", 5), ("地震", 5), ("发射", 4),
        ("独立", 4), ("统一", 4), ("解放", 4), ("第一", 4), ("首次", 4), ("卫星", 4),
        ("日本", 3), ("清朝", 3), ("民国", 4), ("发现", 3), ("诺贝尔", 3), ("逝世", 1),
    ];
    for (kw, w) in KW {
        if title.contains(kw) {
            s += *w;
        }
    }
    if title.chars().count() > 45 {
        s -= 2;
    }
    s
}

fn cache_dir() -> std::path::PathBuf {
    crate::config::data_dir().join("history")
}

fn month_path(m: u32) -> std::path::PathBuf {
    cache_dir().join(format!("{}.json", m))
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 启动清理：删除旧版 asilu 单日缓存格式（history.json）
pub fn purge_stale() {
    let _ = std::fs::remove_file(crate::config::data_dir().join("history.json"));
}

/// 进程内月缓存（月 -> 日 -> 事件列表），避免每次重绘都解析磁盘文件
static MEMO: Mutex<Option<(u32, HashMap<u32, Vec<HistoryItem>>)>> = Mutex::new(None);

/// 指定日期的事件（年份从近到远排序）；当月无缓存返回 None
pub fn load_for(date: NaiveDate) -> Option<Vec<HistoryItem>> {
    let mut g = MEMO.lock().unwrap();
    let need_load = match g.as_ref() {
        Some((m, _)) => *m != date.month(),
        None => true,
    };
    if need_load {
        let text = std::fs::read_to_string(month_path(date.month())).ok()?;
        let mf: MonthFile = serde_json::from_str(&text).ok()?;
        let mut map: HashMap<u32, Vec<(i32, HistoryItem)>> = HashMap::new();
        for e in mf.entries {
            let score = significance(e.k, &e.t);
            map.entry(e.d).or_default().push((score, HistoryItem { year: e.y, title: e.t }));
        }
        // 重要性从高到低；同分按年份从近到远
        let mut map2: HashMap<u32, Vec<HistoryItem>> = HashMap::new();
        for (d, mut v) in map {
            v.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.year.cmp(&a.1.year)));
            map2.insert(d, v.into_iter().map(|(_, it)| it).collect());
        }
        *g = Some((date.month(), map2));
    }
    g.as_ref().and_then(|(_, m)| m.get(&date.day())).cloned()
}

/// (月份, 时间戳, 是否成功)：同月已请求成功不再请求；失败 10 分钟内不重复请求
static ATTEMPT: Mutex<Option<(u32, u64, bool)>> = Mutex::new(None);

/// 当月无缓存时后台拉取数据集并拆分缓存（有缓存不请求；重复调用安全）
pub fn ensure_fetched(date: NaiveDate) {
    if load_for(date).is_some() {
        return;
    }
    let now = now_ms();
    {
        let mut g = ATTEMPT.lock().unwrap();
        let skip = match g.as_ref() {
            Some((m, ts, ok)) => *m == date.month() && (*ok || now - *ts < RETRY_MS),
            None => false,
        };
        if skip {
            return;
        }
        *g = Some((date.month(), now, false));
    }
    std::thread::Builder::new()
        .name("history".into())
        // TLS 握手 + 大 JSON 解析栈要给足（128KB 会溢出并 abort 整个进程）
        .stack_size(512 * 1024)
        .spawn(move || {
            let ok = fetch_and_split().is_some();
            *ATTEMPT.lock().unwrap() = Some((date.month(), now_ms(), ok));
            if ok {
                crate::sidebar::post_repaint();
            }
            crate::trim_working_set();
        })
        .ok();
}

/// 拉取全量数据集并按月写入缓存（阻塞，仅后台线程调用）
fn fetch_and_split() -> Option<()> {
    let json = fetch_dataset()?;
    let raw: Vec<RawEntry> = serde_json::from_str(&json).ok()?;
    let mut months: Vec<Vec<StoredEntry>> = (0..12).map(|_| Vec::new()).collect();
    for e in raw {
        // 数据集字段均为字符串（如 "month":"9"、公元前为负数年份）
        let (Ok(y), Ok(m), Ok(d)) = (
            e.year.trim().parse::<i32>(),
            e.month.trim().parse::<u32>(),
            e.day.trim().parse::<u32>(),
        ) else {
            continue;
        };
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            continue;
        }
        let t = e.data.trim();
        if t.is_empty() {
            continue;
        }
        months[(m - 1) as usize].push(StoredEntry { y, d, t: t.to_string(), k: e.ty });
    }
    let _ = std::fs::create_dir_all(cache_dir());
    let mut wrote_any = false;
    for (i, v) in months.iter_mut().enumerate() {
        if v.is_empty() {
            continue;
        }
        let mf = MonthFile { entries: std::mem::take(v) };
        if let Ok(json) = serde_json::to_string(&mf) {
            if std::fs::write(month_path(i as u32 + 1), json).is_ok() {
                wrote_any = true;
            }
        }
    }
    if !wrote_any {
        return None;
    }
    // 内存缓存失效，下次 load_for 重新读盘
    *MEMO.lock().unwrap() = None;
    Some(())
}

/// 依次尝试镜像源下载全量数据集（阻塞，仅后台线程调用）
fn fetch_dataset() -> Option<String> {
    for url in SOURCES {
        let Ok(resp) = ureq::get(*url)
            .timeout(std::time::Duration::from_secs(30))
            .call()
        else {
            continue; // 该镜像失败，换下一个
        };
        let mut body = resp.into_reader().take(32 << 20); // 上限 32MB，防异常响应撑爆内存
        let mut buf = Vec::new();
        if body.read_to_end(&mut buf).is_err() || buf.is_empty() {
            continue;
        }
        let s = match String::from_utf8(buf) {
            Ok(s) => s,
            Err(_) => continue,
        };
        // 简单有效性检查：必须是数据集结构（数组且含 data 字段名）
        if s.starts_with('[') && s.contains("\"data\"") {
            return Some(s);
        }
    }
    None
}
