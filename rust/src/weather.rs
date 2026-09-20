use chrono::NaiveDate;
use serde::Deserialize;
use std::io::Read;

/// 单日预报（今日起近 7 天）
#[derive(Clone, Debug)]
pub struct DayWeather {
    pub date: NaiveDate,
    pub code: u32,
    pub tmin: i32,
    pub tmax: i32,
    /// US AQI（按天取逐小时最大值），获取失败为 None
    pub aqi: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Weather {
    pub temp: i32,
    pub code: u32,
    pub city: String,
    pub days: Vec<DayWeather>,
    /// 最近一次成功更新的时间戳（毫秒）
    pub ts: u64,
}

#[derive(Deserialize)]
struct IpApi {
    status: String,
    #[serde(default)]
    city: String,
    #[serde(default)]
    lat: f64,
    #[serde(default)]
    lon: f64,
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
}

/// 主定位：ip-api.com（大陆城市定位准确，直接返回简体中文城市名 + 坐标）
fn fetch_ip_api() -> Option<(f64, f64, String)> {
    // 该服务对不同客户端可能返回错误的 charset 头（GBK），故按原始字节强制 UTF-8 解析
    let mut body = ureq::get("http://ip-api.com/json/?lang=zh-CN&fields=status,message,city,lat,lon")
        .timeout(std::time::Duration::from_secs(8))
        .call()
        .ok()?
        .into_reader()
        .take(1 << 20); // 上限 1MB，防异常响应撑爆内存
    let mut buf = Vec::new();
    body.read_to_end(&mut buf).ok()?;
    let v: IpApi = serde_json::from_slice(&buf).ok()?;
    if v.status != "success" || v.city.is_empty() {
        return None;
    }
    Some((v.lat, v.lon, v.city.trim_end_matches('市').to_string()))
}

/// 坐标 → 简体中文城市名（BigDataCloud 免费反向地理编码；失败返回 None）
fn fetch_city_cn(lat: f64, lon: f64) -> Option<String> {
    let url = format!(
        "https://api.bigdatacloud.net/data/reverse-geocode-client?latitude={}&longitude={}&localityLanguage=zh-Hans",
        lat, lon
    );
    let text = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(8))
        .call()
        .ok()?
        .into_string()
        .ok()?;
    let v: BdcReverse = serde_json::from_str(&text).ok()?;
    if v.city.is_empty() {
        return None;
    }
    Some(v.city.trim_end_matches('市').to_string())
}

#[derive(Deserialize)]
struct OpenMeteoCurrent {
    temperature: f64,
    weathercode: u32,
}

#[derive(Deserialize)]
struct OpenMeteoForecast {
    current_weather: OpenMeteoCurrent,
    #[serde(default)]
    daily: Option<OpenMeteoDaily>,
}

#[derive(Deserialize)]
struct OpenMeteoDaily {
    #[serde(default)]
    time: Vec<String>,
    #[serde(default)]
    weather_code: Vec<u32>,
    #[serde(default)]
    temperature_2m_max: Vec<f64>,
    #[serde(default)]
    temperature_2m_min: Vec<f64>,
}

#[derive(Deserialize)]
struct AirQuality {
    #[serde(default)]
    hourly: Option<AqHourly>,
}

#[derive(Deserialize)]
struct AqHourly {
    #[serde(default)]
    time: Vec<String>,
    #[serde(default)]
    us_aqi: Vec<Option<f64>>,
}

/// 宽松结构兜底：仅取当前天气
#[derive(Deserialize)]
struct WmoCode {
    #[serde(default)]
    temperature: f64,
    #[serde(default)]
    weathercode: u32,
}

pub fn load_cache() -> Option<Weather> {
    let path = crate::config::data_dir().join("weather.json");
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let ts = v.get("ts")?.as_i64()?;
    // 缓存 3 小时内有效（后台线程每小时自动刷新）
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    if now - ts > 3 * 3600 * 1000 {
        return None;
    }
    let days = v
        .get("days")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|d| {
                    let date = NaiveDate::parse_from_str(d.get("d")?.as_str()?, "%Y-%m-%d").ok()?;
                    Some(DayWeather {
                        date,
                        code: d.get("code")?.as_u64()? as u32,
                        tmin: d.get("min")?.as_i64()? as i32,
                        tmax: d.get("max")?.as_i64()? as i32,
                        aqi: d.get("aqi").and_then(|a| a.as_u64()).map(|a| a as u32),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Weather {
        temp: v.get("temp")?.as_i64()? as i32,
        code: v.get("code")?.as_u64()? as u32,
        city: v.get("city").and_then(|c| c.as_str()).unwrap_or("").into(),
        days,
        ts: ts as u64,
    })
}

pub fn save_cache(w: &Weather) {
    let path = crate::config::data_dir().join("weather.json");
    let days: Vec<serde_json::Value> = w
        .days
        .iter()
        .map(|d| {
            serde_json::json!({
                "d": d.date.format("%Y-%m-%d").to_string(),
                "code": d.code,
                "min": d.tmin,
                "max": d.tmax,
                "aqi": d.aqi,
            })
        })
        .collect();
    let obj = serde_json::json!({ "ts": w.ts, "temp": w.temp, "code": w.code, "city": w.city, "days": days });
    let _ = std::fs::write(path, obj.to_string());
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn fetch() -> Option<Weather> {
    // 主定位 ip-api.com（大陆准确、中文城市名），失败回退 ipwho.is + 反向地理编码
    let (lat, lon, city) = match fetch_ip_api() {
        Some(v) => v,
        None => {
            let loc: IpWho = serde_json::from_str(
                &ureq::get("https://ipwho.is/")
                    .timeout(std::time::Duration::from_secs(8))
                    .call()
                    .ok()?
                    .into_string()
                    .ok()?,
            )
            .ok()?;
            if !loc.success {
                return None;
            }
            let city = fetch_city_cn(loc.latitude, loc.longitude).unwrap_or(loc.city);
            (loc.latitude, loc.longitude, city)
        }
    };
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}&current_weather=true&daily=weather_code,temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=7",
        lat, lon
    );
    let text = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(8))
        .call()
        .ok()?
        .into_string()
        .ok()?;
    // 先精确结构（含每日预报），失败再用宽松结构兜底（仅当前天气）
    let (cur, mut days) = match serde_json::from_str::<OpenMeteoForecast>(&text) {
        Ok(f) => {
            let days = build_days(&f);
            (f.current_weather, days)
        }
        Err(_) => {
            let w: WmoCode = serde_json::from_str(&text).ok()?;
            (
                OpenMeteoCurrent { temperature: w.temperature, weathercode: w.weathercode },
                Vec::new(),
            )
        }
    };
    if !days.is_empty() {
        let aq_url = format!(
            "https://air-quality-api.open-meteo.com/v1/air-quality?latitude={}&longitude={}&hourly=us_aqi&timezone=auto&forecast_days=7",
            lat, lon
        );
        if let Ok(resp) = ureq::get(&aq_url).timeout(std::time::Duration::from_secs(8)).call() {
            if let Ok(t) = resp.into_string() {
                apply_aqi(&mut days, &t);
            }
        }
    }
    Some(Weather {
        temp: cur.temperature.round() as i32,
        code: cur.weathercode,
        city,
        days,
        ts: now_ms(),
    })
}

fn build_days(f: &OpenMeteoForecast) -> Vec<DayWeather> {
    let Some(d) = &f.daily else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, t) in d.time.iter().enumerate() {
        let Ok(date) = NaiveDate::parse_from_str(t, "%Y-%m-%d") else {
            continue;
        };
        out.push(DayWeather {
            date,
            code: d.weather_code.get(i).copied().unwrap_or(0),
            tmin: d.temperature_2m_min.get(i).copied().unwrap_or(0.0).round() as i32,
            tmax: d.temperature_2m_max.get(i).copied().unwrap_or(0.0).round() as i32,
            aqi: None,
        });
    }
    out
}

/// 逐小时 US AQI 按天取最大值，填入预报（获取失败时保持 None）
fn apply_aqi(days: &mut [DayWeather], text: &str) {
    let Ok(aq) = serde_json::from_str::<AirQuality>(text) else {
        return;
    };
    let Some(h) = aq.hourly else {
        return;
    };
    let mut max: std::collections::HashMap<NaiveDate, u32> = std::collections::HashMap::new();
    for (t, v) in h.time.iter().zip(h.us_aqi.iter()) {
        if t.len() < 10 {
            continue;
        }
        let Ok(date) = NaiveDate::parse_from_str(&t[..10], "%Y-%m-%d") else {
            continue;
        };
        if let Some(v) = v {
            let e = max.entry(date).or_insert(0);
            *e = (*e).max(v.round().max(0.0) as u32);
        }
    }
    for d in days.iter_mut() {
        d.aqi = max.get(&d.date).copied();
    }
}

/// WMO 天气代码 → 中文描述
pub fn wmo_text(code: u32) -> &'static str {
    match code {
        0 => "晴",
        1 => "大部晴朗",
        2 => "局部多云",
        3 => "阴",
        45 | 48 => "雾",
        51 => "小毛毛雨",
        53 => "毛毛雨",
        55 => "大毛毛雨",
        56 | 57 => "冻毛毛雨",
        61 => "小雨",
        63 => "中雨",
        65 => "大雨",
        66 | 67 => "冻雨",
        71 => "小雪",
        73 => "中雪",
        75 => "大雪",
        77 => "雪粒",
        80 => "小阵雨",
        81 => "阵雨",
        82 => "强阵雨",
        85 => "小阵雪",
        86 => "阵雪",
        95 => "雷阵雨",
        96 | 99 => "雷雨伴冰雹",
        _ => "未知",
    }
}

/// US AQI →（等级名, 颜色），等级划分对齐国内空气质量标准
pub fn aqi_level(aqi: u32) -> (&'static str, (u8, u8, u8)) {
    match aqi {
        0..=50 => ("优", (0x5B, 0xC2, 0x8E)),
        51..=100 => ("良", (0xD4, 0xC0, 0x4A)),
        101..=150 => ("轻度污染", (0xF0, 0x9A, 0x3E)),
        151..=200 => ("中度污染", (0xEE, 0x6A, 0x4A)),
        201..=300 => ("重度污染", (0xB4, 0x6B, 0xD6)),
        _ => ("严重污染", (0xD8, 0x4A, 0x5A)),
    }
}

/// 调试：CAL_FAKE_WX=1 时使用固定示例数据（离线验证界面）
pub fn fake() -> Weather {
    let today = chrono::Local::now().date_naive();
    let codes = [1u32, 95, 95, 3, 61, 0, 2];
    let aqis = [44u32, 115, 101, 104, 88, 63, 150];
    Weather {
        temp: 26,
        code: 1,
        city: "广州".into(),
        ts: now_ms(),
        days: (0..7)
            .map(|i| DayWeather {
                date: today + chrono::Duration::days(i as i64),
                code: codes[i],
                tmin: 25 - (i % 3) as i32,
                tmax: 35 - (i * 2 % 7) as i32,
                aqi: Some(aqis[i]),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parse_forecast_and_aqi() {
        let fc = r#"{"current_weather":{"temperature":26.4,"weathercode":95},"daily":{"time":["2026-09-20","2026-09-21"],"weather_code":[95,3],"temperature_2m_max":[35.2,33.0],"temperature_2m_min":[25.1,24.4]}}"#;
        let f: super::OpenMeteoForecast = serde_json::from_str(fc).unwrap();
        let mut days = super::build_days(&f);
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].code, 95);
        assert_eq!(days[0].tmax, 35);
        assert_eq!(days[0].tmin, 25);
        assert_eq!(days[1].code, 3);
        assert_eq!(days[1].aqi, None);

        let aq = r#"{"hourly":{"time":["2026-09-20T10:00","2026-09-20T23:00","2026-09-21T00:00"],"us_aqi":[42.4,null,120.6]}}"#;
        super::apply_aqi(&mut days, aq);
        assert_eq!(days[0].aqi, Some(42));
        assert_eq!(days[1].aqi, Some(121));

        assert_eq!(super::wmo_text(95), "雷阵雨");
        assert_eq!(super::wmo_text(0), "晴");
        assert_eq!(super::aqi_level(44).0, "优");
        assert_eq!(super::aqi_level(115).0, "轻度污染");
        assert_eq!(super::aqi_level(188).0, "中度污染");
    }

    // 真实网络拉取验证（需联网）
    #[test]
    fn fetch_real_network() {
        let w = super::fetch();
        println!("weather = {:?}", w);
        assert!(w.is_some(), "fetch failed");
        let w = w.unwrap();
        assert!(!w.days.is_empty(), "no daily forecast");
    }
}
