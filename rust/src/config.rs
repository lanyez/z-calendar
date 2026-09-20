use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_ICS_URL: &str = "https://cdn.jsdelivr.net/npm/chinese-days/dist/holidays.ics";

fn def_true() -> bool {
    true
}
fn def_week_start() -> u32 {
    0 // 星期一
}
fn def_ics() -> String {
    DEFAULT_ICS_URL.to_string()
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    // 软件设置
    #[serde(default)]
    pub autostart: bool,
    #[serde(default = "def_true")]
    pub auto_update: bool,
    #[serde(default = "def_true")]
    pub show_tray: bool,        // 显示系统托盘图标
    // 日历设置
    #[serde(default = "def_true")]
    pub show_weather: bool,
    #[serde(default = "def_true")]
    pub show_lunar: bool,       // 显示农历/节日信息
    #[serde(default = "def_true")]
    pub show_adjust: bool,      // 显示调休安排（休/班角标）
    #[serde(default = "def_true")]
    pub show_other_month: bool, // 显示非当前月日期
    #[serde(default)]
    pub hour12: bool,           // 使用12小时制
    #[serde(default = "def_true")]
    pub show_week_num: bool,    // 显示周数
    #[serde(default = "def_week_start")]
    pub week_start: u32,        // 一周开始：0=星期一 ... 6=星期日
    // 其他
    #[serde(default = "def_ics")]
    pub ics_url: String,
    #[serde(default)]
    pub last_ics_update: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            autostart: false,
            auto_update: true,
            show_tray: true,
            show_weather: true,
            show_lunar: true,
            show_adjust: true,
            show_other_month: true,
            hour12: false,
            show_week_num: true,
            week_start: 0,
            ics_url: DEFAULT_ICS_URL.to_string(),
            last_ics_update: 0,
        }
    }
}

pub fn data_dir() -> PathBuf {
    let dir = std::env::var("APPDATA")
        .map(|p| PathBuf::from(p).join("CalendarFlyout"))
        .unwrap_or_else(|_| PathBuf::from("."));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

impl Config {
    pub fn load() -> Self {
        let mut cfg = Config::default();
        if let Ok(text) = std::fs::read_to_string(config_path()) {
            if let Ok(v) = serde_json::from_str::<Config>(&text) {
                cfg = v;
            }
        }
        if cfg.ics_url.is_empty() {
            cfg.ics_url = DEFAULT_ICS_URL.to_string();
        }
        cfg
    }

    pub fn save(&self) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(config_path(), text);
        }
    }
}

/// 开机自启：写/删 HKCU\...\Run\Z日历
pub fn apply_autostart(enable: bool) {
    use winreg::enums::*;
    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    let run = r"Software\Microsoft\Windows\CurrentVersion\Run";
    let _ = (|| -> std::io::Result<()> {
        let key = hkcu.open_subkey_with_flags(run, KEY_SET_VALUE)?;
        if enable {
            let exe = std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            key.set_value("Z日历", &format!("\"{}\"", exe))?;
        } else {
            let _ = key.delete_value("Z日历");
            let _ = key.delete_value("CalendarFlyout"); // 清理旧名称的注册项
        }
        Ok(())
    })();
}
