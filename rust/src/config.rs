use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_ICS_URL: &str = "https://cdn.jsdelivr.net/npm/chinese-days/dist/holidays.ics";

fn def_true() -> bool {
    true
}
fn def_week_start() -> u32 {
    0 // 星期一
}
fn def_font_scale() -> f32 {
    1.0
}
fn def_theme() -> u8 {
    1 // 默认深色主题
}
fn def_ics() -> String {
    DEFAULT_ICS_URL.to_string()
}
fn def_motto_type() -> String {
    "d".to_string() // 名人名言
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
    /// 界面字号系数（1.0/1.1/1.25）
    #[serde(default = "def_font_scale")]
    pub ui_font_scale: f32,
    /// 主题：0=跟随系统 1=深色（默认） 2=浅色
    #[serde(default = "def_theme")]
    pub theme: u8,
    // 其他
    #[serde(default = "def_ics")]
    pub ics_url: String,
    #[serde(default)]
    pub last_ics_update: u64,
    /// 提醒弹窗伴随提示音
    #[serde(default = "def_true")]
    pub remind_sound: bool,
    // 侧栏卡片（点击日期弹出的侧边栏）
    #[serde(default = "def_true")]
    pub sidebar_date: bool,      // 日期信息
    #[serde(default = "def_true")]
    pub sidebar_almanac: bool,   // 黄历信息
    #[serde(default = "def_true")]
    pub sidebar_events: bool,    // 最近事件
    #[serde(default = "def_true")]
    pub sidebar_agenda: bool,    // 今日日程
    #[serde(default)]
    pub sidebar_history: bool,   // 历史上的今天
    #[serde(default)]
    pub sidebar_motto: bool,     // 时间格言
    /// 时间格言分类：d=名人名言 a=文学 i=互联网 k=科普
    #[serde(default = "def_motto_type")]
    pub motto_type: String,
    #[serde(default = "def_true")]
    pub sidebar_todo: bool,      // 待办清单
    /// 侧栏卡片顺序（card id 列表；缺失的按默认顺序补齐）
    #[serde(default)]
    pub sidebar_order: Vec<String>,
}

/// 侧栏卡片定义：(card id, 名称, 设置开关 idx)
pub const SIDEBAR_CARDS: [(&str, &str, u8); 7] = [
    ("date", "日期信息", 10),
    ("almanac", "黄历信息", 11),
    ("events", "最近事件", 12),
    ("agenda", "今日日程", 13),
    ("history", "历史上的今天", 14),
    ("motto", "时间格言", 15),
    ("todo", "待办清单", 16),
];

impl Config {
    /// 侧栏卡片开关状态
    pub fn sidebar_enabled(&self, id: &str) -> bool {
        match id {
            "date" => self.sidebar_date,
            "almanac" => self.sidebar_almanac,
            "events" => self.sidebar_events,
            "agenda" => self.sidebar_agenda,
            "history" => self.sidebar_history,
            "motto" => self.sidebar_motto,
            "todo" => self.sidebar_todo,
            _ => false,
        }
    }

    /// 完整卡片顺序：sidebar_order 优先，缺失的按默认顺序补齐，未知 id 忽略
    pub fn sidebar_card_order(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for id in &self.sidebar_order {
            if let Some((cid, _, _)) = SIDEBAR_CARDS.iter().find(|(c, _, _)| id == c) {
                if !out.contains(cid) {
                    out.push(cid);
                }
            }
        }
        for (cid, _, _) in SIDEBAR_CARDS.iter() {
            if !out.contains(cid) {
                out.push(cid);
            }
        }
        out
    }

    /// 拖动排序后写回（传入重排后的完整顺序）
    pub fn set_sidebar_order(&mut self, order: &[&str]) {
        self.sidebar_order = order.iter().map(|s| s.to_string()).collect();
    }
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
            remind_sound: true,
            sidebar_date: true,
            sidebar_almanac: true,
            sidebar_events: true,
            sidebar_agenda: true,
            sidebar_history: false,
            sidebar_motto: false,
            motto_type: "d".to_string(),
            sidebar_todo: true,
            sidebar_order: Vec::new(),
            ui_font_scale: 1.0,
            theme: 1,
        }
    }
}

pub fn data_dir() -> PathBuf {
    let dir = std::env::var("APPDATA")
        .map(|p| PathBuf::from(p).join("z-calendar"))
        .unwrap_or_else(|_| PathBuf::from("."));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 写入前把现有文件复制为 .bak（数据文件损坏时可手工恢复）
pub fn backup_file(path: &PathBuf) {
    let bak = path.with_extension("bak");
    let _ = std::fs::copy(path, bak);
}

/// 读 JSON，主文件损坏时自动从 .bak 恢复。
/// 返回 (数据, 是否发生了恢复)。损坏文件留档为 .corrupt（即使没有可用备份，
/// 也先把坏文件改名，避免下次保存把坏内容复制进 .bak）。
pub fn load_json_or_bak<T: serde::de::DeserializeOwned>(path: &PathBuf) -> (Option<T>, bool) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return (None, false), // 文件不存在=首次使用
    };
    if let Ok(v) = serde_json::from_str::<T>(&text) {
        return (Some(v), false);
    }
    let _ = std::fs::rename(path, path.with_extension("corrupt"));
    let bak = path.with_extension("bak");
    if let Ok(bt) = std::fs::read_to_string(&bak) {
        if let Ok(v) = serde_json::from_str::<T>(&bt) {
            let _ = std::fs::copy(&bak, path);
            return (Some(v), true);
        }
    }
    (None, false)
}

// ---- 高频读取的设置位（绘制线程每次重绘都会用到，避免反复加锁/读盘） ----

static HOUR12: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static REMIND_SOUND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn hour12_on() -> bool {
    HOUR12.load(std::sync::atomic::Ordering::Relaxed)
}
pub fn set_hour12(v: bool) {
    HOUR12.store(v, std::sync::atomic::Ordering::Relaxed);
}
pub fn remind_sound_on() -> bool {
    REMIND_SOUND.load(std::sync::atomic::Ordering::Relaxed)
}
pub fn set_remind_sound(v: bool) {
    REMIND_SOUND.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// 启动时从配置同步原子缓存
pub fn init_flags(cfg: &Config) {
    set_hour12(cfg.hour12);
    set_remind_sound(cfg.remind_sound);
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
        if !matches!(cfg.motto_type.as_str(), "d" | "a" | "i" | "k") {
            cfg.motto_type = "d".to_string();
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
        }
        Ok(())
    })();
}
