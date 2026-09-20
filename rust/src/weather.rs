use serde::Deserialize;

#[derive(Clone, Debug)]
pub struct Weather {
    pub temp: i32,
    pub code: u32,
    pub city: String,
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
struct OpenMeteo {
    current_weather: OpenMeteoCurrent,
}

#[derive(Deserialize)]
struct OpenMeteoCurrent {
    temperature: f64,
    weathercode: u32,
}

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
    // 缓存 3 小时内有效
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    if now - ts > 3 * 3600 * 1000 {
        return None;
    }
    Some(Weather {
        temp: v.get("temp")?.as_i64()? as i32,
        code: v.get("code")?.as_u64()? as u32,
        city: v.get("city").and_then(|c| c.as_str()).unwrap_or("").into(),
    })
}

pub fn save_cache(w: &Weather) {
    let path = crate::config::data_dir().join("weather.json");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let obj = serde_json::json!({ "ts": now, "temp": w.temp, "code": w.code, "city": w.city });
    let _ = std::fs::write(path, obj.to_string());
}

pub fn fetch() -> Option<Weather> {
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
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}&current_weather=true",
        loc.latitude, loc.longitude
    );
    let text = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(8))
        .call()
        .ok()?
        .into_string()
        .ok()?;
    // 先精确结构，失败再用宽松结构兜底
    let cur: OpenMeteoCurrent = match serde_json::from_str::<OpenMeteo>(&text) {
        Ok(o) => o.current_weather,
        Err(_) => serde_json::from_str::<WmoCode>(&text).ok()?.into(),
    };
    Some(Weather {
        temp: cur.temperature.round() as i32,
        code: cur.weathercode,
        city: loc.city,
    })
}

impl From<WmoCode> for OpenMeteoCurrent {
    fn from(w: WmoCode) -> Self {
        OpenMeteoCurrent { temperature: w.temperature, weathercode: w.weathercode }
    }
}

#[cfg(test)]
mod tests {
    // 真实网络拉取验证（需联网）
    #[test]
    fn fetch_real_network() {
        let w = super::fetch();
        println!("weather = {:?}", w);
        assert!(w.is_some(), "fetch failed");
    }
}
