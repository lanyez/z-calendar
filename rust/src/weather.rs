use chrono::NaiveDate;
use chrono::Timelike;
use serde::Deserialize;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};

/// CMA 接口要求浏览器 UA（默认 ureq UA 会被 403）
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

/// 单日预报（今日起近 7 天）
#[derive(Clone, Debug)]
pub struct DayWeather {
    pub date: NaiveDate,
    /// 图标代码（绘制层代码空间，抓取时由 CMA 代码转换）
    pub code: u32,
    /// 白天天气现象文字（接口原文，如“小雨”）
    pub text: String,
    pub tmin: i32,
    pub tmax: i32,
    /// 白天风向 / 风力（接口原文，如“南风”/“3~4级”）
    pub wind_dir: String,
    pub wind_scale: String,
}

/// 气象预警（view 接口 alarm 数组元素）
#[derive(Clone, Debug)]
pub struct Alarm {
    pub signaltype: String,
    pub signallevel: String,
    pub title: String,
    pub effective: String,
}

#[derive(Clone, Debug)]
pub struct Weather {
    pub temp: i32,
    pub code: u32,
    pub city: String,
    pub days: Vec<DayWeather>,
    /// 最近一次成功更新的时间戳（毫秒）
    pub ts: u64,
    /// 已解析的气象站号，下次刷新城市未变时直接复用
    pub station: String,
    // 实况明细（view 接口 now 块）
    pub humidity: Option<u32>,
    pub pressure: Option<u32>,
    pub feelst: Option<i32>,
    pub wind_dir: String,
    pub wind_scale: String,
    /// 节气（当日为节气日时接口返回名称，否则为空）
    pub jieqi: String,
    /// 接口数据发布时间原文（如 "2026/10/15 14:40"）
    pub last_update: String,
    pub alarms: Vec<Alarm>,
}

// ---------------------------------------------------------------------------
// 定位：ip-api.com 主定位（大陆准确、直接中文省市名），失败回退 ipwho.is +
// BigDataCloud 反向地理编码。站号解析只需省市名，不再需要坐标。
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct IpApi {
    status: String,
    #[serde(default)]
    city: String,
    #[serde(default, rename = "regionName")]
    region: String,
}

#[derive(Deserialize)]
struct IpWho {
    success: bool,
    #[serde(default)]
    latitude: f64,
    #[serde(default)]
    longitude: f64,
    #[serde(default)]
    city: String,
}

#[derive(Deserialize)]
struct BdcReverse {
    #[serde(default)]
    city: String,
    #[serde(default, rename = "principalSubdivision")]
    province: String,
}

/// DNS 解析：IPv4 地址排在 IPv6 之前。电信等运营商的 IPv6 出口在 IP 定位库中
/// 常被错标城市（如广东电信 IPv6 被标到海南儋州），IPv4 数据准确得多，
/// 故天气相关请求一律优先走 IPv4 出口定位（无 IPv4 时仍可回退 IPv6）。
/// 天气相关请求共用 Agent（IPv4 优先解析，实现在 net.rs）
fn agent() -> &'static ureq::Agent {
    crate::net::agent()
}

/// 统一 GET：带上限地读回原始字节，按 UTF-8 解析，避开响应头 charset 陷阱
fn http_get(url: &str) -> Option<Vec<u8>> {
    let mut body = agent()
        .get(url)
        .set("User-Agent", UA)
        .timeout(std::time::Duration::from_secs(8))
        .call()
        .ok()?
        .into_reader()
        .take(1 << 20);
    let mut buf = Vec::new();
    body.read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// 带一次重试的 GET（nmc 偶发 404/限流抖动时补一发）
fn http_get_retry(url: &str) -> Option<Vec<u8>> {
    http_get(url).or_else(|| {
        std::thread::sleep(std::time::Duration::from_millis(400));
        http_get(url)
    })
}

fn fetch_ip_api() -> Option<(String, String)> {
    // 境外明文 HTTP 服务从国内访问偶发超时，重试一次以减少落入备用定位
    for _ in 0..2 {
        let Some(buf) = http_get("http://ip-api.com/json/?lang=zh-CN&fields=status,message,city,regionName") else {
            continue;
        };
        let Ok(v) = serde_json::from_slice::<IpApi>(&buf) else {
            continue;
        };
        if v.status == "success" && !v.city.is_empty() {
            return Some((v.city.trim_end_matches('市').to_string(), v.region.trim_end_matches('省').to_string()));
        }
    }
    None
}

/// 坐标 →（简体中文城市名, 简体中文省名）；BigDataCloud 免费反向地理编码
fn fetch_city_cn(lat: f64, lon: f64) -> Option<(String, String)> {
    let url = format!(
        "https://api.bigdatacloud.net/data/reverse-geocode-client?latitude={}&longitude={}&localityLanguage=zh-Hans",
        lat, lon
    );
    let buf = http_get(&url)?;
    let v: BdcReverse = serde_json::from_slice(&buf).ok()?;
    if v.city.is_empty() {
        return None;
    }
    Some((v.city.trim_end_matches('市').to_string(), v.province.trim_end_matches('省').to_string()))
}

struct Located {
    city: String,
    province: String,
}

fn locate() -> Option<Located> {
    if let Some((city, province)) = fetch_ip_api() {
        return Some(Located { city, province });
    }
    let buf = http_get("https://ipwho.is/")?;
    let loc: IpWho = serde_json::from_slice(&buf).ok()?;
    if !loc.success {
        return None;
    }
    let (city, province) = fetch_city_cn(loc.latitude, loc.longitude).unwrap_or((loc.city, String::new()));
    Some(Located { city: city.trim_end_matches('市').to_string(), province })
}

// ---------------------------------------------------------------------------
// 站号解析：weather.cma.cn 字典接口（数据为 "代码,名称|..." 纯文本）。
// 先按省取列表（2 次请求），未命中再退全国站号表（34 次请求，本地缓存 30 天）。
// ---------------------------------------------------------------------------

/// 解析 "k1,v1|k2,v2|..." 字典格式
fn parse_dict(text: &str) -> Vec<(String, String)> {
    text.split('|')
        .filter_map(|p| {
            let (k, v) = p.split_once(',')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// 城市 → 站号：先精确（容忍“市”后缀），再互为包含的模糊匹配
fn name_match(list: &[(String, String)], city: &str) -> Option<String> {
    let city = city.trim_end_matches('市');
    if let Some((id, _)) = list.iter().find(|(_, n)| n.trim_end_matches('市') == city) {
        return Some(id.clone());
    }
    if city.is_empty() {
        return None;
    }
    list.iter()
        .find(|(_, n)| {
            let n = n.trim_end_matches('市');
            n.contains(city) || city.contains(n)
        })
        .map(|(id, _)| id.clone())
}

/// 全国站号表拉取失败的时间戳：失败后 10 分钟内不再重试全表，
/// 避免接口异常时刷新线程每分钟都打满 34 个请求
static TABLE_FAIL_TS: AtomicU64 = AtomicU64::new(0);

fn station_table_path() -> std::path::PathBuf {
    crate::config::data_dir().join("stations.json")
}

fn load_station_table() -> Option<Vec<(String, String)>> {
    let text = std::fs::read_to_string(station_table_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let ts = v.get("ts")?.as_i64()?;
    if now_ms() as i64 - ts > 30 * 24 * 3600 * 1000 {
        return None;
    }
    serde_json::from_value(v.get("st")?.clone()).ok()
}

/// 逐省拉取聚合全国站号表并写盘缓存
fn fetch_station_table() -> Option<Vec<(String, String)>> {
    if now_ms() - TABLE_FAIL_TS.load(Ordering::Relaxed) < 10 * 60 * 1000 {
        return None;
    }
    let result = (|| {
        let buf = http_get("https://weather.cma.cn/api/dict/province")?;
        let mut table = Vec::new();
        for (pcode, _) in parse_dict(&String::from_utf8_lossy(&buf)) {
            let Some(cbuf) = http_get(&format!("https://weather.cma.cn/api/dict/province/{}", pcode)) else {
                continue; // 单省失败不拖垮全表
            };
            table.extend(parse_dict(&String::from_utf8_lossy(&cbuf)));
        }
        if table.is_empty() {
            return None;
        }
        let obj = serde_json::json!({ "ts": now_ms(), "st": table });
        let _ = std::fs::write(station_table_path(), obj.to_string());
        Some(table)
    })();
    if result.is_none() {
        TABLE_FAIL_TS.store(now_ms(), Ordering::Relaxed);
    }
    result
}

/// 城市/省名 → 气象站号。省名做包含匹配以兼容“广西壮族自治区”类全称。
fn resolve_station(city: &str, province: &str) -> Option<String> {
    if !province.is_empty() {
        if let Some(buf) = http_get("https://weather.cma.cn/api/dict/province") {
            let provs = parse_dict(&String::from_utf8_lossy(&buf));
            if let Some((pcode, _)) = provs.iter().find(|(_, n)| n.contains(province) || province.contains(n.as_str())) {
                if let Some(cbuf) = http_get(&format!("https://weather.cma.cn/api/dict/province/{}", pcode)) {
                    if let Some(id) = name_match(&parse_dict(&String::from_utf8_lossy(&cbuf)), city) {
                        return Some(id);
                    }
                }
            }
        }
    }
    // 省未命中/省内未命中：全国表精确匹配（不做模糊，避免“朝阳”类同名歧义）
    let table = load_station_table().or_else(fetch_station_table)?;
    let city = city.trim_end_matches('市');
    table.iter().find(|(_, n)| n.trim_end_matches('市') == city).map(|(id, _)| id.clone())
}

// ---------------------------------------------------------------------------
// weather.cma.cn 天气数据
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CmaResp {
    #[serde(default)]
    code: i32,
    #[serde(default)]
    data: Option<CmaData>,
}

/// daily 用宽容的 Value 手工提取：weather.cma.cn 为扁平形态
/// （dayText/dayCode/high/low），同源的 www.nmc.cn 为嵌套形态
/// （day.weather.{info,img,temperature}），两种都兼容
#[derive(Deserialize)]
struct CmaData {
    #[serde(default)]
    location: Option<CmaLocation>,
    #[serde(default)]
    daily: Vec<serde_json::Value>,
    #[serde(default)]
    now: Option<CmaNow>,
    #[serde(default, rename = "jieQi")]
    jieqi: String,
    #[serde(default, rename = "lastUpdate")]
    last_update: String,
    #[serde(default)]
    alarm: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct CmaLocation {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct CmaNow {
    #[serde(default)]
    temperature: f64,
    #[serde(default)]
    humidity: Option<f64>,
    #[serde(default)]
    pressure: Option<f64>,
    #[serde(default)]
    feelst: Option<f64>,
    #[serde(default)]
    wind_direction: String,
    #[serde(default)]
    wind_scale: String,
}

/// alarm 数组元素 → Alarm（字段缺失时以空串兜底）
fn parse_alarm(v: &serde_json::Value) -> Alarm {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Alarm {
        signaltype: s("signaltype"),
        signallevel: s("signallevel"),
        title: s("title"),
        effective: s("effective"),
    }
}

/// 数值字段宽容提取：JSON 数字或数字字符串均可
fn flex(v: Option<&serde_json::Value>) -> Option<f64> {
    let v = v?;
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
}

/// CMA 天气代码 → 绘制层图标代码（沿用原 WMO 代码空间，draw_weather 无需改动）
fn cma_icon(code: u32) -> u32 {
    match code {
        0 => 0,                        // 晴
        1 => 1,                        // 多云
        2 => 3,                        // 阴
        3 | 7 | 8 | 21 | 6 | 19 => 61, // 阵雨/小中雨/雨夹雪/冻雨 → 雨图标
        9 | 22 => 63,                  // 大雨/中到大雨
        10 | 11 | 12 | 23 | 24 | 25 => 65, // 暴雨及以上
        4 | 5 => 95,                   // 雷阵雨（伴冰雹）
        13 | 14 | 26 => 71,            // 阵雪/小雪/小到中雪
        15 | 27 => 73,                 // 中雪/中到大雪
        16 | 17 | 28 => 75,            // 大雪/暴雪/大到暴雪
        18 => 45,                      // 雾
        20 | 29 | 30 | 31 | 53 => 3,   // 沙尘/霾 → 阴图标
        _ => 1,                        // 未知 → 多云
    }
}

fn fetch_view(station: &str) -> Option<CmaData> {
    let buf = http_get(&format!("https://weather.cma.cn/api/weather/view?stationid={}", station))?;
    let r: CmaResp = serde_json::from_slice(&buf).ok()?;
    if r.code != 0 {
        return None;
    }
    r.data
}

/// 单个预报日提取结果：白天代码/文字/最高温、夜间代码/最低温、白天风向/风力，
/// 兼容扁平/嵌套两种形态；夜间缺省时以白天数据兜底
struct DayParts {
    code: u32,
    text: String,
    tmax: i32,
    night_code: u32,
    tmin: i32,
    wind_dir: String,
    wind_scale: String,
}

fn day_parts(d: &serde_json::Value) -> Option<DayParts> {
    let str_of = |v: Option<&serde_json::Value>, k: &str| {
        v.and_then(|o| o.get(k)).and_then(|x| x.as_str()).unwrap_or("").to_string()
    };
    if let Some(text) = d.get("dayText").and_then(|v| v.as_str()) {
        let code = flex(d.get("dayCode")).unwrap_or(1.0) as u32;
        let night_code = flex(d.get("nightCode")).unwrap_or(code as f64) as u32;
        let tmax = flex(d.get("high")).unwrap_or(0.0).round() as i32;
        let tmin = flex(d.get("low")).map(|v| v.round() as i32).unwrap_or(tmax);
        return Some(DayParts {
            code,
            text: text.to_string(),
            tmax,
            night_code,
            tmin,
            wind_dir: str_of(Some(d), "dayWindDirection"),
            wind_scale: str_of(Some(d), "dayWindScale"),
        });
    }
    let day = d.get("day")?;
    let day_wx = day.get("weather")?;
    let night = d.get("night").and_then(|n| n.get("weather"));
    let code = flex(day_wx.get("img")).unwrap_or(1.0) as u32;
    let text = day_wx.get("info").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let tmax = flex(day_wx.get("temperature")).unwrap_or(0.0).round() as i32;
    let (night_code, tmin) = match night {
        Some(n) => (
            flex(n.get("img")).unwrap_or(code as f64) as u32,
            flex(n.get("temperature")).map(|v| v.round() as i32).unwrap_or(tmax),
        ),
        None => (code, tmax),
    };
    // 嵌套形态的风在 day.wind.{direct,power} 下
    let wind = day.get("wind");
    Some(DayParts {
        code,
        text,
        tmax,
        night_code,
        tmin,
        wind_dir: str_of(wind, "direct"),
        wind_scale: str_of(wind, "power"),
    })
}

fn build_days(data: &CmaData) -> Vec<DayWeather> {
    data.daily
        .iter()
        .filter_map(|d| {
            let date = NaiveDate::parse_from_str(d.get("date")?.as_str()?, "%Y/%m/%d").ok()?;
            let p = day_parts(d)?;
            Some(DayWeather {
                date,
                code: cma_icon(p.code),
                text: p.text,
                tmin: p.tmin,
                tmax: p.tmax,
                wind_dir: p.wind_dir,
                wind_scale: p.wind_scale,
            })
        })
        .collect()
}

/// 实况无天气现象字段：按本地时间取今日白天（06–18 时）/夜间预报代码
fn current_icon(data: &CmaData) -> u32 {
    let h = chrono::Local::now().hour();
    let Some(first) = data.daily.first() else { return 1 };
    let Some(p) = day_parts(first) else { return 1 };
    let code = if (18..24).contains(&h) || h < 6 { p.night_code } else { p.code };
    cma_icon(code)
}

/// 预报列宽内的紧凑风力文本：“无持续风向”只显示风力，超长风向同理
pub fn wind_short(dir: &str, scale: &str) -> String {
    let d = dir.trim();
    let s = scale.trim();
    if d.is_empty() || d == "无持续风向" || d.chars().count() > 3 {
        return s.to_string();
    }
    if s.is_empty() {
        return d.to_string();
    }
    format!("{} {}", d, s)
}

/// 预警级别 → 颜色（蓝/黄/橙/红）
pub fn alarm_color(level: &str) -> (u8, u8, u8) {
    match level {
        "蓝色" => (0x4A, 0x9E, 0xF5),
        "黄色" => (0xE0, 0xB4, 0x22),
        "橙色" => (0xF0, 0x8C, 0x1E),
        "红色" => (0xE0, 0x4A, 0x4A),
        _ => (0xE0, 0xB4, 0x22),
    }
}

// ---------------------------------------------------------------------------
// 缓存（weather.json；station 记录站号与定位城市，供下次刷新复用）
// ---------------------------------------------------------------------------

/// 单城市缓存 → Weather；旧格式（无天气文字/站号）返回 None 触发重拉
fn load_one(v: &serde_json::Value) -> Option<Weather> {
    let raw_days = v.get("days")?.as_array()?;
    let mut days = Vec::new();
    for d in raw_days {
        let date = NaiveDate::parse_from_str(d.get("d")?.as_str()?, "%Y-%m-%d").ok()?;
        days.push(DayWeather {
            date,
            code: d.get("code")?.as_u64()? as u32,
            text: d.get("t")?.as_str()?.into(),
            tmin: d.get("min")?.as_i64()? as i32,
            tmax: d.get("max")?.as_i64()? as i32,
            wind_dir: d.get("wd").and_then(|x| x.as_str()).unwrap_or("").into(),
            wind_scale: d.get("ws").and_then(|x| x.as_str()).unwrap_or("").into(),
        });
    }
    if raw_days.is_empty() || days.len() != raw_days.len() {
        return None;
    }
    let num = |k: &str| v.get(k).and_then(|x| x.as_u64()).map(|x| x as u32);
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let alarms = v
        .get("alarms")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|a| Alarm {
                    signaltype: a.get("t").and_then(|x| x.as_str()).unwrap_or("").into(),
                    signallevel: a.get("l").and_then(|x| x.as_str()).unwrap_or("").into(),
                    title: a.get("title").and_then(|x| x.as_str()).unwrap_or("").into(),
                    effective: a.get("eff").and_then(|x| x.as_str()).unwrap_or("").into(),
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Weather {
        temp: v.get("temp")?.as_i64()? as i32,
        code: v.get("code")?.as_u64()? as u32,
        city: s("city"),
        days,
        ts: v.get("ts")?.as_i64()? as u64,
        station: v.get("station")?.as_str()?.into(),
        humidity: num("hum"),
        pressure: num("prs"),
        feelst: v.get("feel").and_then(|x| x.as_i64()).map(|x| x as i32),
        wind_dir: s("wdir"),
        wind_scale: s("wscale"),
        jieqi: s("jieqi"),
        last_update: s("lupdate"),
        alarms,
    })
}

fn save_one(w: &Weather) -> serde_json::Value {
    let days: Vec<serde_json::Value> = w
        .days
        .iter()
        .map(|d| {
            serde_json::json!({
                "d": d.date.format("%Y-%m-%d").to_string(),
                "code": d.code,
                "t": d.text,
                "min": d.tmin,
                "max": d.tmax,
                "wd": d.wind_dir,
                "ws": d.wind_scale,
            })
        })
        .collect();
    let alarms: Vec<serde_json::Value> = w
        .alarms
        .iter()
        .map(|a| {
            serde_json::json!({
                "t": a.signaltype, "l": a.signallevel,
                "title": a.title, "eff": a.effective,
            })
        })
        .collect();
    serde_json::json!({
        "ts": w.ts, "temp": w.temp, "code": w.code, "city": w.city,
        "station": w.station, "days": days,
        "hum": w.humidity, "prs": w.pressure, "feel": w.feelst,
        "wdir": w.wind_dir, "wscale": w.wind_scale,
        "jieqi": w.jieqi, "lupdate": w.last_update, "alarms": alarms,
    })
}

/// 多城市缓存：{ts, located:{...}|null, custom:[...]}，3 小时内有效
pub fn load_cache() -> (Option<Weather>, Vec<Weather>) {
    let path = crate::config::data_dir().join("weather.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return (None, Vec::new());
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return (None, Vec::new());
    };
    let Some(ts) = v.get("ts").and_then(|t| t.as_i64()) else {
        return (None, Vec::new());
    };
    let now = now_ms() as i64;
    if now - ts > 3 * 3600 * 1000 {
        return (None, Vec::new());
    }
    let located = v.get("located").filter(|x| !x.is_null()).and_then(load_one);
    let custom = v
        .get("custom")
        .and_then(|x| x.as_array())
        .map(|arr| arr.iter().filter_map(load_one).collect())
        .unwrap_or_default();
    (located, custom)
}

pub fn save_cache(located: Option<&Weather>, custom: &[Weather]) {
    let path = crate::config::data_dir().join("weather.json");
    let obj = serde_json::json!({
        "ts": now_ms(),
        "located": located.map(save_one),
        "custom": custom.iter().map(save_one).collect::<Vec<_>>(),
    });
    let _ = std::fs::write(path, obj.to_string());
}

/// 上次成功缓存中的（站号, 定位城市）：城市未变时跳过字典解析
fn prev_station() -> Option<(String, String)> {
    let text = std::fs::read_to_string(crate::config::data_dir().join("weather.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let l = v.get("located")?;
    Some((l.get("station")?.as_str()?.into(), l.get("city")?.as_str()?.into()))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 城市搜索与站号解析（nmc.cn essearch autocomplete → cma 省字典）：
// autocomplete 返回 "混淆码|城市名|省名|/AGD/xxx.html|经度|纬度"，
// 混淆码不能用于 cma view 接口，取 URL 中的省代码 + 城市名到省字典精确解析
// ---------------------------------------------------------------------------

/// 城市搜索结果
#[derive(Clone, Debug)]
pub struct CityHit {
    pub name: String,
    pub province: String,
    /// 城市页路径（如 /ASH/shanghai.html）
    pub url: String,
}

/// 搜索城市（nmc.cn autocomplete，最多 10 条）
pub fn search_cities(q: &str) -> Vec<CityHit> {
    let q = q.trim();
    if q.is_empty() {
        return Vec::new();
    }
    // 中文按 UTF-8 字节做百分号编码
    let mut enc = String::new();
    for b in q.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            enc.push(*b as char);
        } else {
            enc.push_str(&format!("%{:02X}", b));
        }
    }
    // autocomplete 走 HTTPS（应用内 ureq/rustls 可正常校验该站证书，实测 200）
    let url = format!("https://www.nmc.cn/essearch/api/autocomplete?q={}&limit=10&_={}", enc, now_ms());
    let Some(buf) = http_get_retry(&url) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&buf) else {
        return Vec::new();
    };
    v.get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let s = e.as_str()?;
                    let parts: Vec<&str> = s.split('|').collect();
                    if parts.len() < 4 {
                        return None;
                    }
                    Some(CityHit {
                        name: parts[1].to_string(),
                        province: parts[2].to_string(),
                        url: parts[3].to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 从搜索结果中挑最匹配的一条：名称精确（容忍“市”后缀）优先，其次互含，
/// 最后回退首条。nmc 搜索相关性不稳（如“西安”首条返回“安溪”），
/// 热门城市点击必须按名称校正，不能直接取第一条。
pub fn pick_best<'a>(hits: &'a [CityHit], query: &str) -> Option<&'a CityHit> {
    let q = query.trim().trim_end_matches('市');
    if q.is_empty() {
        return hits.first();
    }
    hits.iter()
        .find(|h| h.name.trim_end_matches('市') == q)
        .or_else(|| {
            hits.iter().find(|h| {
                let n = h.name.trim_end_matches('市');
                !n.is_empty() && (n.contains(q) || q.contains(n))
            })
        })
        .or_else(|| hits.first())
}

/// 搜索结果 → 数字站号：优先取城市页内嵌的 `var stationid = "..."`（对所有
/// 城市精确，含“上海→徐家汇代表站”这类站名与城市名不一致的情况），
/// 失败回退按页面路径省代码取省站表做名称匹配
pub fn resolve_hit(hit: &CityHit) -> Option<String> {
    if !hit.url.is_empty() {
        // autocomplete 给的路径省略了 /publish/forecast 前缀（其站内 JS 会自行拼接）
        let page = format!("https://www.nmc.cn/publish/forecast{}", hit.url);
        if let Some(buf) = http_get_retry(&page) {
            let text = String::from_utf8_lossy(&buf);
            if let Some(id) = text.split("var stationid").nth(1).and_then(|s| s.split('"').nth(1)) {
                if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
                    return Some(id.to_string());
                }
            }
        }
    }
    // 回退：省站表名称匹配（括号注记取简名，如“赣县（赣州）”）
    let pcode = hit_pcode(&hit.url)?;
    let buf = http_get(&format!("https://weather.cma.cn/api/dict/province/{}", pcode))?;
    let list = parse_dict(&String::from_utf8_lossy(&buf));
    let name = hit.name.split('（').next().unwrap_or(&hit.name);
    name_match(&list, name.trim())
}

/// 城市页路径 "/ASH/shanghai.html" → 省代码 "ASH"（首段为空，取第一段非空片段）
fn hit_pcode(url: &str) -> Option<&str> {
    url.split('/').find(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphabetic() || c == '-'))
}

/// view 数据 → Weather（city_fallback 为定位城市名，接口无 location 时使用）
fn build_weather(data: &CmaData, city_fallback: &str, station: &str) -> Option<Weather> {
    let days = build_days(data);
    if days.is_empty() {
        return None;
    }
    let city = data
        .location
        .as_ref()
        .filter(|l| !l.name.is_empty())
        .map(|l| l.name.clone())
        .unwrap_or_else(|| city_fallback.to_string());
    let now = data.now.clone().unwrap_or_default();
    Some(Weather {
        temp: now.temperature.round() as i32,
        code: current_icon(data),
        city,
        days,
        ts: now_ms(),
        station: station.to_string(),
        humidity: now.humidity.map(|v| v.round() as u32),
        pressure: now.pressure.map(|v| v.round() as u32),
        feelst: now.feelst.map(|v| v.round() as i32),
        wind_dir: now.wind_direction.clone(),
        wind_scale: now.wind_scale.clone(),
        jieqi: data.jieqi.trim().to_string(),
        last_update: data.last_update.clone(),
        alarms: data.alarm.iter().map(parse_alarm).collect(),
    })
}

pub fn fetch() -> Option<Weather> {
    let loc = locate()?;
    // 站号复用：城市未变直接用上次的站号；接口拒绝（站号失效）时再重新解析
    let mut station = prev_station().filter(|(_, c)| *c == loc.city).map(|(s, _)| s);
    let data = match station.as_deref().and_then(fetch_view) {
        Some(d) => d,
        None => {
            let s = resolve_station(&loc.city, &loc.province)?;
            let d = fetch_view(&s)?;
            station = Some(s);
            d
        }
    };
    let s = station?;
    build_weather(&data, &loc.city, &s)
}

/// 按站号直接取一个城市的天气（自定义城市；display 非空时覆盖
/// 接口站点名，如 58367 的“徐家汇”显示为用户选择的“上海”）
pub fn fetch_station(station: &str, display: &str) -> Option<Weather> {
    let mut w = build_weather(&fetch_view(station)?, "", station)?;
    if !display.trim().is_empty() {
        w.city = display.trim().to_string();
    }
    Some(w)
}

/// 一次刷新全部城市：定位城市 + 自定义（custom 为 (站号, 名称)，最多 4 个）；
/// 单个城市失败时沿用 prev 里的旧数据（含定位城市）
pub fn fetch_all(custom: &[(String, String)], prev: &[Weather]) -> (Option<Weather>, Vec<Weather>) {
    let located = fetch().or_else(|| prev.first().cloned());
    let mut customs = Vec::new();
    for (id, name) in custom.iter().take(4) {
        if let Some(w) =
            fetch_station(id, name).or_else(|| prev.iter().find(|p| &p.station == id).cloned())
        {
            customs.push(w);
        }
    }
    (located, customs)
}

/// 调试：CAL_FAKE_WX=1 时使用固定示例数据（离线验证界面）
pub fn fake() -> Weather {
    let today = chrono::Local::now().date_naive();
    let rows: [(u32, &str, i32, i32, &str, &str); 7] = [
        (1, "多云", 25, 33, "南风", "微风"),
        (95, "雷阵雨", 24, 31, "南风", "3~4级"),
        (95, "雷阵雨", 23, 30, "无持续风向", "微风"),
        (3, "阴", 22, 28, "北风", "3~4级"),
        (61, "小雨", 21, 26, "东北风", "微风"),
        (0, "晴", 20, 29, "西北风", "微风"),
        (2, "局部多云", 19, 27, "无持续风向", "微风"),
    ];
    Weather {
        temp: 26,
        code: 1,
        city: "广州".into(),
        ts: now_ms(),
        station: "59287".into(),
        humidity: Some(73),
        pressure: Some(1008),
        feelst: Some(29),
        wind_dir: "西南风".into(),
        wind_scale: "2级".into(),
        jieqi: "寒露".into(),
        last_update: chrono::Local::now().format("%Y/%m/%d %H:%M").to_string(),
        alarms: vec![Alarm {
            signaltype: "雷电".into(),
            signallevel: "黄色".into(),
            title: "广州市气象台发布雷电黄色预警信号[III级/较大]".into(),
            effective: chrono::Local::now().format("%Y/%m/%d %H:%M").to_string(),
        }],
        days: rows
            .iter()
            .enumerate()
            .map(|(i, (code, text, tmin, tmax, wd, ws))| DayWeather {
                date: today + chrono::Duration::days(i as i64),
                code: *code,
                text: (*text).into(),
                tmin: *tmin,
                tmax: *tmax,
                wind_dir: (*wd).into(),
                wind_scale: (*ws).into(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn resolver_v4_first() {
        let addrs = crate::net::resolve_v4_first("localhost:1234").expect("resolve localhost");
        assert!(!addrs.is_empty());
        if let Some(i) = addrs.iter().position(|a| a.is_ipv6()) {
            assert!(addrs[i..].iter().all(|a| a.is_ipv6()), "IPv4 应排在 IPv6 之前");
        }
    }

    #[test]
    fn parse_dict_and_match() {
        let list = super::parse_dict("54398,顺义|54511,北京|54594,大兴");
        assert_eq!(list.len(), 3);
        assert_eq!(list[1].0, "54511");
        assert_eq!(super::name_match(&list, "北京市").as_deref(), Some("54511"));
        assert_eq!(super::name_match(&list, "顺义").as_deref(), Some("54398"));
        assert_eq!(super::name_match(&list, "不存在"), None);
    }

    #[test]
    fn cma_icon_mapping() {
        assert_eq!(super::cma_icon(0), 0); // 晴
        assert_eq!(super::cma_icon(2), 3); // 阴
        assert_eq!(super::cma_icon(7), 61); // 小雨
        assert_eq!(super::cma_icon(9), 63); // 大雨
        assert_eq!(super::cma_icon(10), 65); // 暴雨
        assert_eq!(super::cma_icon(4), 95); // 雷阵雨
        assert_eq!(super::cma_icon(14), 71); // 小雪
        assert_eq!(super::cma_icon(16), 75); // 大雪
        assert_eq!(super::cma_icon(18), 45); // 雾
        assert_eq!(super::cma_icon(53), 3); // 霾
    }

    #[test]
    fn parse_view_flat() {
        // 扁平形态：weather.cma.cn/api/weather/view 实测结构（广州 59287 / 庄河 54584）
        let sample = r#"{"msg":"success","code":0,"data":{"location":{"id":"59287","name":"广州","path":"中国, 广东, 广州","longitude":113.33,"latitude":23.17,"timezone":8},
            "daily":[
            {"date":"2026/10/15","high":32.0,"dayText":"晴","dayCode":0,"dayWindDirection":"无持续风向","dayWindScale":"微风","low":23.0,"nightText":"晴","nightCode":0,"nightWindDirection":"无持续风向","nightWindScale":"微风"},
            {"date":"2026/10/16","high":31.0,"dayText":"多云","dayCode":1,"dayWindDirection":"北风","dayWindScale":"3~4级","low":22.0,"nightText":"小雨","nightCode":7,"nightWindDirection":"北风","nightWindScale":"3~4级"}],
            "now":{"precipitation":0.0,"temperature":22.7,"pressure":1011.0,"humidity":47.0,"windDirection":"西南风","windDirectionDegree":186.0,"windSpeed":0.8,"windScale":"微风","feelst":23.7},
            "jieQi":"寒露",
            "alarm":[{"id":"x","title":"广州市气象台发布雷电黄色预警信号[III级/较大]","signaltype":"雷电","signallevel":"黄色","effective":"2026/10/15 13:37","eventType":"11B14","severity":"YELLOW","type":"p0012003"}],
            "lastUpdate":"2026/10/15 11:50"}}"#;
        let r: super::CmaResp = serde_json::from_str(sample).unwrap();
        assert_eq!(r.code, 0);
        let d = r.data.unwrap();
        assert_eq!(d.location.as_ref().unwrap().name, "广州");
        let days = super::build_days(&d);
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].date.to_string(), "2026-10-15");
        assert_eq!(days[0].text, "晴");
        assert_eq!(days[0].code, 0); // 晴图标
        assert_eq!(days[0].tmax, 32);
        assert_eq!(days[0].tmin, 23);
        assert_eq!(days[0].wind_dir, "无持续风向");
        assert_eq!(days[0].wind_scale, "微风");
        assert_eq!(days[1].text, "多云");
        assert_eq!(days[1].code, 1); // 多云图标
        assert_eq!(days[1].tmin, 22);
        let now = d.now.unwrap();
        assert_eq!(now.temperature, 22.7);
        assert_eq!(now.humidity, Some(47.0));
        assert_eq!(now.pressure, Some(1011.0));
        assert_eq!(now.feelst, Some(23.7));
        assert_eq!(now.wind_direction, "西南风");
        assert_eq!(now.wind_scale, "微风");
        assert_eq!(d.jieqi, "寒露");
        assert_eq!(d.last_update, "2026/10/15 11:50");
        let a = super::parse_alarm(&d.alarm[0]);
        assert_eq!(a.signaltype, "雷电");
        assert_eq!(a.signallevel, "黄色");
        assert!(a.title.contains("雷电黄色预警"));
    }

    #[test]
    fn parse_view_nested() {
        // 嵌套形态：同源 www.nmc.cn/rest/weather 结构，一并兼容
        let sample = r#"{"msg":"success","code":0,"data":{"real":{"station":{"code":"Wqsps","city":"北京"},"weather":{"temperature":21.8,"info":"晴","img":"0"}},"predict":{"detail":[
            {"date":"2026-10-15","day":{"weather":{"info":"晴","img":"0","temperature":"25"},"wind":{}},"night":{"weather":{"info":"多云","img":"1","temperature":"13"},"wind":{}},"precipitation":0.0}]}}}"#;
        // 该形态的 daily 位于 predict.detail，本实现只消费扁平形态的 daily 字段
        let r: super::CmaResp = serde_json::from_str(sample).unwrap();
        assert_eq!(r.code, 0);
        assert!(r.data.unwrap().daily.is_empty());
    }

    #[test]
    fn day_parts_nmc_shape() {
        // nmc.cn 嵌套预报日（date 形如 2026/10/15 时）也能提取
        let v: serde_json::Value = serde_json::json!({
            "date": "2026/10/15",
            "day": {"weather": {"info": "晴", "img": "0", "temperature": "25"}, "wind": {"direct": "南风", "power": "微风"}},
            "night": {"weather": {"info": "多云", "img": "1", "temperature": "13"}}
        });
        let p = super::day_parts(&v).unwrap();
        assert_eq!((p.code, p.text.as_str(), p.tmax, p.night_code, p.tmin), (0, "晴", 25, 1, 13));
        assert_eq!((p.wind_dir.as_str(), p.wind_scale.as_str()), ("南风", "微风"));
    }

    #[test]
    fn wind_short_and_alarm_color() {
        assert_eq!(super::wind_short("南风", "微风"), "南风 微风");
        assert_eq!(super::wind_short("无持续风向", "微风"), "微风");
        assert_eq!(super::wind_short("西北风", "3~4级"), "西北风 3~4级");
        assert_eq!(super::wind_short("", "微风"), "微风");
        assert_eq!(super::alarm_color("蓝色"), (0x4A, 0x9E, 0xF5));
        assert_eq!(super::alarm_color("红色"), (0xE0, 0x4A, 0x4A));
    }

    // 真实网络拉取验证（需联网）
    #[test]
    fn fetch_real_network() {
        let w = super::fetch();
        println!("weather = {:?}", w);
        assert!(w.is_some(), "fetch failed");
        let w = w.unwrap();
        assert!(!w.days.is_empty(), "no daily forecast");
        assert!(!w.station.is_empty(), "no station id");
    }

    // 诊断：热门城市全链路（搜索 → 名称校正 → 站号解析；需联网）
    #[test]
    fn diag_hot_cities() {
        // (城市, 期望站号)；西安专门验证“首条为安溪”时的名称校正
        let expect = [
            ("北京", "54511"),
            ("上海", "58367"),
            ("广州", "59287"),
            ("深圳", "59493"),
            ("杭州", "58457"),
        ];
        for (city, want) in expect {
            let hits = super::search_cities(city);
            let best = super::pick_best(&hits, city);
            println!("{}({} hits) → 最佳={:?}", city, hits.len(), best.map(|h| h.name.as_str()));
            assert_eq!(best.map(|h| h.name.as_str()), Some(city), "{} 名称校正失败", city);
            let id = best.and_then(super::resolve_hit);
            println!("  站号={:?}", id);
            assert_eq!(id.as_deref(), Some(want), "{} 站号不符", city);
        }
        let hits = super::search_cities("西安");
        let best = super::pick_best(&hits, "西安");
        println!("西安({} hits) → 最佳={:?}", hits.len(), best.map(|h| h.name.as_str()));
        assert_eq!(best.map(|h| h.name.as_str()), Some("西安"), "西安名称校正失败");
        assert!(best.and_then(super::resolve_hit).is_some(), "西安站号解析失败");
    }

    #[test]
    fn pick_best_by_name() {
        let mk = |name: &str, url: &str| super::CityHit { name: name.into(), province: String::new(), url: url.into() };
        // 模拟“西安”首条为“安溪”的相关性错乱
        let hits = vec![mk("安溪", "/AFJ/anxi.html"), mk("息县", "/AHA/xixian.html"), mk("西安", "/ASN/xian.html")];
        assert_eq!(super::pick_best(&hits, "西安").map(|h| h.name.as_str()), Some("西安"));
        assert_eq!(super::pick_best(&hits, "西安市").map(|h| h.name.as_str()), Some("西安"));
        assert_eq!(super::pick_best(&hits, "安溪").map(|h| h.name.as_str()), Some("安溪"));
        // 无任何匹配时回退首条
        assert_eq!(super::pick_best(&hits, "不存在城").map(|h| h.name.as_str()), Some("安溪"));
        assert_eq!(super::pick_best(&[], "北京").map(|h| h.name.as_str()), None);
    }

    #[test]
    fn hit_pcode_extraction() {
        assert_eq!(super::hit_pcode("/ASH/shanghai.html"), Some("ASH"));
        assert_eq!(super::hit_pcode("/AGD/guangzhou.html"), Some("AGD"));
        assert_eq!(super::hit_pcode(""), None);
    }

    // 真实网络：城市搜索 → 站号解析（需联网）
    #[test]
    fn search_real_network() {
        let hits = super::search_cities("广州");
        assert!(!hits.is_empty(), "no hits");
        assert_eq!(hits[0].name, "广州");
        assert_eq!(hits[0].url, "/AGD/guangzhou.html");
        let id = super::resolve_hit(&hits[0]);
        assert_eq!(id.as_deref(), Some("59287"));
        // 上海：站名（徐家汇）与城市名不一致，靠城市页 stationid 解析
        let sh = super::search_cities("上海");
        assert_eq!(sh[0].url, "/ASH/shanghai.html");
        assert_eq!(super::resolve_hit(&sh[0]).as_deref(), Some("58367"));
        let w = super::fetch_station("59287", "");
        assert!(w.is_some());
        assert_eq!(w.unwrap().city, "广州");
        // 自定义城市：展示名覆盖站点名（58367=徐家汇 → 显示“上海”）
        let w = super::fetch_station("58367", "上海").unwrap();
        assert_eq!(w.city, "上海");
    }
}
