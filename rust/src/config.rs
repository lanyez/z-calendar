use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
fn def_font_family() -> String {
    "Microsoft YaHei UI".to_string()
}

/// 天气面板自定义城市（站号 + 展示名，最多 4 个；首格为定位城市）
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct WxCity {
    pub id: String,
    pub name: String,
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
    /// 天气面板自定义城市（定位城市之外，最多 4 个）
    #[serde(default)]
    pub wx_cities: Vec<WxCity>,
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
    /// 界面字体族（GDI/GDI+ 正文统一字体）
    #[serde(default = "def_font_family")]
    pub ui_font_family: String,
    /// 弹窗淡入淡出动画
    #[serde(default = "def_true")]
    pub fade_anim: bool,
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
    /// 提醒走系统通知中心（Win10/11 Toast；失败自动回退内置卡片）
    #[serde(default = "def_true")]
    pub use_system_toast: bool,
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
    /// 最近提醒（提醒历史，默认关）
    #[serde(default)]
    pub sidebar_reminds: bool,
    /// 侧栏卡片顺序（card id 列表；缺失的按默认顺序补齐）
    #[serde(default)]
    pub sidebar_order: Vec<String>,
}

/// 侧栏卡片定义：(card id, 名称, 对应的设置开关)
pub const SIDEBAR_CARDS: [(&str, &str, Toggle); 8] = [
    ("date", "日期信息", Toggle::SidebarDate),
    ("almanac", "黄历信息", Toggle::SidebarAlmanac),
    ("events", "最近事件", Toggle::SidebarEvents),
    ("agenda", "今日日程", Toggle::SidebarAgenda),
    ("history", "历史上的今天", Toggle::SidebarHistory),
    ("motto", "时间格言", Toggle::SidebarMotto),
    ("todo", "待办清单", Toggle::SidebarTodo),
    ("reminds", "最近提醒", Toggle::SidebarReminds),
];

/// 设置窗口里的一个开关项。此前用 0–19 的魔法下标在绘制与处理两处各维护一份，
/// 中途插入开关就会错位（17/18 曾被系统通知/动画开关占用，只能把"最近提醒"排到 19）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Toggle {
    Autostart,
    AutoUpdate,
    ShowLunar,
    ShowAdjust,
    ShowOtherMonth,
    Hour12,
    ShowWeekNum,
    ShowWeather,
    ShowTray,
    RemindSound,
    SidebarDate,
    SidebarAlmanac,
    SidebarEvents,
    SidebarAgenda,
    SidebarHistory,
    SidebarMotto,
    SidebarTodo,
    SidebarReminds,
    UseSystemToast,
    FadeAnim,
}

impl Toggle {
    /// 是否为侧栏卡片的显示开关（切换后侧栏需重绘）
    pub fn is_sidebar_card(self) -> bool {
        matches!(
            self,
            Toggle::SidebarDate
                | Toggle::SidebarAlmanac
                | Toggle::SidebarEvents
                | Toggle::SidebarAgenda
                | Toggle::SidebarHistory
                | Toggle::SidebarMotto
                | Toggle::SidebarTodo
                | Toggle::SidebarReminds
        )
    }

    /// 是否为影响月历画面的开关（农历/调休/非本月/12 小时制/周数/天气；切换后主面板需重绘）
    pub fn is_calendar_pref(self) -> bool {
        matches!(
            self,
            Toggle::ShowLunar
                | Toggle::ShowAdjust
                | Toggle::ShowOtherMonth
                | Toggle::Hour12
                | Toggle::ShowWeekNum
                | Toggle::ShowWeather
        )
    }
}

impl Config {
    /// 翻转一个开关并同步原子缓存（提醒音 / 通知方式 / 12 小时制 / 动画被其它线程高频
    /// 读取，走原子量）。返回翻转后的新状态
    pub fn toggle(&mut self, t: Toggle) -> bool {
        let field = match t {
            Toggle::Autostart => &mut self.autostart,
            Toggle::AutoUpdate => &mut self.auto_update,
            Toggle::ShowLunar => &mut self.show_lunar,
            Toggle::ShowAdjust => &mut self.show_adjust,
            Toggle::ShowOtherMonth => &mut self.show_other_month,
            Toggle::Hour12 => &mut self.hour12,
            Toggle::ShowWeekNum => &mut self.show_week_num,
            Toggle::ShowWeather => &mut self.show_weather,
            Toggle::ShowTray => &mut self.show_tray,
            Toggle::RemindSound => &mut self.remind_sound,
            Toggle::SidebarDate => &mut self.sidebar_date,
            Toggle::SidebarAlmanac => &mut self.sidebar_almanac,
            Toggle::SidebarEvents => &mut self.sidebar_events,
            Toggle::SidebarAgenda => &mut self.sidebar_agenda,
            Toggle::SidebarHistory => &mut self.sidebar_history,
            Toggle::SidebarMotto => &mut self.sidebar_motto,
            Toggle::SidebarTodo => &mut self.sidebar_todo,
            Toggle::SidebarReminds => &mut self.sidebar_reminds,
            Toggle::UseSystemToast => &mut self.use_system_toast,
            Toggle::FadeAnim => &mut self.fade_anim,
        };
        *field = !*field;
        let on = *field;
        init_flags(self);
        on
    }

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
            "reminds" => self.sidebar_reminds,
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
            wx_cities: Vec::new(),
            show_lunar: true,
            show_adjust: true,
            show_other_month: true,
            hour12: false,
            show_week_num: true,
            week_start: 0,
            ics_url: DEFAULT_ICS_URL.to_string(),
            last_ics_update: 0,
            remind_sound: true,
            use_system_toast: true,
            sidebar_date: true,
            sidebar_almanac: true,
            sidebar_events: true,
            sidebar_agenda: true,
            sidebar_history: false,
            sidebar_motto: false,
            motto_type: "d".to_string(),
            sidebar_todo: true,
            sidebar_reminds: false,
            sidebar_order: Vec::new(),
            ui_font_scale: 1.0,
            ui_font_family: "Microsoft YaHei UI".to_string(),
            fade_anim: true,
            theme: 1,
        }
    }
}

pub fn data_dir() -> PathBuf {
    // 开发调试：CAL_DATA_DIR 把数据目录指到别处——与已安装实例并行运行开发构建时
    // 隔离数据（配合 CAL_INSTANCE，见 debug 模块）
    if let Some(d) = crate::debug::value_os(crate::debug::DATA_DIR) {
        let dir = PathBuf::from(d);
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    let dir = std::env::var("APPDATA")
        .map(|p| PathBuf::from(p).join("z-calendar"))
        .unwrap_or_else(|_| PathBuf::from("."));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 进程内写盘串行锁。临时文件名固定为 <主文件名>.tmp，同一路径的并发写
/// （天气线程与主线程都会写 weather.json）在此排队，两个写者不会互相截断对方的临时文件
static IO_LOCK: Mutex<()> = Mutex::new(());

/// 写入前把现有文件滚动备份为 .bak / .bak1（保留两代；
/// 连续两次坏写不会同时毁掉两份备份，数据文件损坏时可手工恢复）
pub fn backup_file(path: &Path) {
    let bak = path.with_extension("bak");
    let bak1 = path.with_extension("bak1");
    if bak.exists() {
        let _ = std::fs::rename(&bak, &bak1);
    }
    if path.exists() {
        let _ = std::fs::copy(path, bak);
    }
}

/// 原子写文本：先写同目录临时文件再 rename 替换（Windows 的 rename 带
/// MOVEFILE_REPLACE_EXISTING 语义），写盘中途崩溃或并发读都不会看到半截文件。
/// 失败不致命（主文件保持上一次完整内容），由调用方忽略/记日志。
pub fn write_file_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let _g = IO_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    write_locked(path, text)
}

/// 备份 + 原子写，作为一次串行操作执行：两代备份的轮转不会被并发写者插队
pub fn write_with_backup(path: &Path, text: &str) -> std::io::Result<()> {
    let _g = IO_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    backup_file(path);
    write_locked(path, text)
}

/// 实际写入（调用方已持有 IO_LOCK）
fn write_locked(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// 读 JSON，主文件损坏时自动从 .bak（再退 .bak1）恢复。
/// 返回 (数据, 是否发生了恢复)。损坏文件留档为 .corrupt（即使没有可用备份，
/// 也先把坏文件改名，避免下次保存把坏内容复制进 .bak）。
pub fn load_json_or_bak<T: serde::de::DeserializeOwned>(path: &Path) -> (Option<T>, bool) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return (None, false), // 文件不存在=首次使用
    };
    if let Ok(v) = serde_json::from_str::<T>(&text) {
        return (Some(v), false);
    }
    let _ = std::fs::rename(path, path.with_extension("corrupt"));
    let bak = path.with_extension("bak");
    let bak1 = path.with_extension("bak1");
    for cand in [&bak, &bak1] {
        if let Ok(bt) = std::fs::read_to_string(cand) {
            if let Ok(v) = serde_json::from_str::<T>(&bt) {
                let _ = std::fs::copy(cand, path);
                return (Some(v), true);
            }
        }
    }
    (None, false)
}

// ---- 高频读取的设置位（绘制线程每次重绘都会用到，避免反复加锁/读盘） ----

static HOUR12: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static REMIND_SOUND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static SYS_TOAST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static FADE_ANIM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

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
/// 提醒是否走系统通知中心（提醒线程高频读取走原子缓存）
pub fn use_system_toast_on() -> bool {
    SYS_TOAST.load(std::sync::atomic::Ordering::Relaxed)
}
pub fn set_use_system_toast(v: bool) {
    SYS_TOAST.store(v, std::sync::atomic::Ordering::Relaxed);
}
/// 弹窗显隐是否带淡入淡出动画
pub fn fade_anim_on() -> bool {
    FADE_ANIM.load(std::sync::atomic::Ordering::Relaxed)
}
pub fn set_fade_anim(v: bool) {
    FADE_ANIM.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// 启动时从配置同步原子缓存
pub fn init_flags(cfg: &Config) {
    set_hour12(cfg.hour12);
    set_remind_sound(cfg.remind_sound);
    set_use_system_toast(cfg.use_system_toast);
    set_fade_anim(cfg.fade_anim);
}

fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

/// 本次启动时 config.json 是否由备份恢复过
static RECOVERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn recovered_from_backup() -> bool {
    RECOVERED.load(std::sync::atomic::Ordering::Relaxed)
}

impl Config {
    /// 启动时读取 config.json。主文件损坏时与其他数据文件一样自动从备份恢复并提示
    /// （此前是裸读取：损坏即静默回退默认值，下一次保存就把坏文件覆盖，设置全部丢失）
    pub fn load() -> Self {
        let (cfg, healed) = Self::load_from(&config_path());
        if healed {
            crate::log::warn("config.json 损坏，已自动从备份恢复");
            // 本函数先于提醒线程启动，通知发不出去：记下标志，由 main 在提醒线程起来后补发
            RECOVERED.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        cfg
    }

    /// 从指定路径读取并做合法性修正；返回 (配置, 是否从备份恢复)
    pub fn load_from(path: &Path) -> (Self, bool) {
        let (mut cfg, healed) = match load_json_or_bak::<Config>(path) {
            (Some(v), h) => (v, h),
            // 文件不存在（首次使用）或连备份也救不回来：回默认值
            (None, _) => (Config::default(), false),
        };
        if cfg.ics_url.is_empty() {
            cfg.ics_url = DEFAULT_ICS_URL.to_string();
        }
        if !matches!(cfg.motto_type.as_str(), "d" | "a" | "i" | "k") {
            cfg.motto_type = "d".to_string();
        }
        (cfg, healed)
    }

    /// 保存到 config.json（备份轮转 + 原子写）。返回是否写成功
    pub fn save(&self) -> bool {
        self.save_to(&config_path())
    }

    pub fn save_to(&self, path: &Path) -> bool {
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                CONFIG_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let ok = write_with_backup(path, &text).is_ok();
                if !ok {
                    crate::log::warn("config.json 写入失败（本次更改未保存）");
                }
                ok
            }
            Err(_) => false,
        }
    }
}

/// 配置数据代次：每次 save +1。设置窗口绘制的配置快照缓存据此失效
/// （此前设置窗内每次悬停移动都整体 clone 31 字段 Config）。
static CONFIG_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub fn config_gen() -> u64 {
    CONFIG_GEN.load(std::sync::atomic::Ordering::Relaxed)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试独立的临时目录（系统临时目录下；不触碰真实的 %APPDATA%\z-calendar）
    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zcal-test-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn json(cfg: &Config) -> String {
        serde_json::to_string(cfg).unwrap()
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = tmp("roundtrip").join("config.json");
        let mut cfg = Config { theme: 2, ..Config::default() };
        cfg.week_start = 6;
        cfg.ui_font_scale = 1.25;
        cfg.wx_cities.push(WxCity { id: "54511".into(), name: "北京".into() });
        assert!(cfg.save_to(&path));
        let (loaded, healed) = Config::load_from(&path);
        assert!(!healed);
        assert_eq!(json(&loaded), json(&cfg));
        // 原子写不留临时文件
        assert!(!path.with_extension("tmp").exists());
    }

    /// 回归：config.json 此前是裸读取——损坏即静默回退默认值，下一次保存把坏文件覆盖，
    /// 用户设置全部丢失。现在与其他数据文件一样从 .bak 恢复
    #[test]
    fn corrupt_file_is_restored_from_backup() {
        let path = tmp("heal").join("config.json");
        let v1 = Config { theme: 2, ..Config::default() };
        assert!(v1.save_to(&path));
        let v2 = Config { theme: 1, ..Config::default() };
        assert!(v2.save_to(&path)); // .bak ← 第 1 代，主文件 = 第 2 代
        assert_eq!(Config::load_from(&path).0.theme, 1);

        std::fs::write(&path, "{ not json").unwrap();
        let (cfg, healed) = Config::load_from(&path);
        assert!(healed);
        assert_eq!(cfg.theme, 2, "应恢复到上一代（.bak）");
        assert_eq!(Config::load_from(&path).0.theme, 2, "主文件已被恢复内容覆盖");
        assert!(path.with_extension("corrupt").exists(), "坏文件留档供手工找回");
    }

    #[test]
    fn falls_back_to_second_generation_backup() {
        let path = tmp("bak1").join("config.json");
        // 连续保存三代：主文件 = 第 3 代，.bak = 第 2 代，.bak1 = 第 1 代
        for theme in [1u8, 2, 0] {
            let c = Config { theme, ..Config::default() };
            assert!(c.save_to(&path));
        }
        let (bak, bak1) = (path.with_extension("bak"), path.with_extension("bak1"));
        assert_eq!(Config::load_from(&bak).0.theme, 2);
        assert_eq!(Config::load_from(&bak1).0.theme, 1);
        assert_eq!(Config::load_from(&path).0.theme, 0);

        // 主文件与 .bak 都损坏：退到 .bak1
        std::fs::write(&path, "x").unwrap();
        std::fs::write(&bak, "y").unwrap();
        let (cfg, healed) = Config::load_from(&path);
        assert!(healed);
        assert_eq!(cfg.theme, 1);
    }

    #[test]
    fn no_usable_backup_yields_defaults_and_keeps_corrupt_file() {
        let path = tmp("default").join("config.json");
        std::fs::write(&path, "garbage").unwrap();
        let (cfg, healed) = Config::load_from(&path);
        assert!(!healed);
        assert_eq!(json(&cfg), json(&Config::default()));
        // 坏文件改名留档（否则下一次保存会把坏内容滚动进 .bak）
        assert!(!path.exists());
        assert!(path.with_extension("corrupt").exists());
        assert!(!Config::load_from(&path).1, "文件不存在 = 首次使用");
    }

    #[test]
    fn missing_or_invalid_fields_are_filled() {
        let path = tmp("fill").join("config.json");
        std::fs::write(&path, r#"{"theme":2,"motto_type":"zzz","ics_url":""}"#).unwrap();
        let (cfg, _) = Config::load_from(&path);
        assert_eq!(cfg.theme, 2);
        assert_eq!(cfg.motto_type, "d", "非法取值回退");
        assert_eq!(cfg.ics_url, DEFAULT_ICS_URL, "空值回退");
        assert!(cfg.show_weather && cfg.sidebar_order.is_empty(), "缺省字段取默认值");
    }

    #[test]
    fn atomic_write_replaces_content_and_leaves_no_temp() {
        let dir = tmp("atomic");
        let path = dir.join("data.json");
        std::fs::write(&path, "old").unwrap();
        write_file_atomic(&path, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        assert!(!path.with_extension("tmp").exists());
        // 目标尚不存在：直接创建
        let p2 = dir.join("other.json");
        write_file_atomic(&p2, "x").unwrap();
        assert_eq!(std::fs::read_to_string(&p2).unwrap(), "x");
    }

    /// 备份 + 原子写是一次串行操作：每次保存滚动一代
    #[test]
    fn write_with_backup_rotates_generations() {
        let path = tmp("rotate").join("data.json");
        for i in 0..3 {
            write_with_backup(&path, &format!("v{i}")).unwrap();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v2");
        assert_eq!(std::fs::read_to_string(path.with_extension("bak")).unwrap(), "v1");
        assert_eq!(std::fs::read_to_string(path.with_extension("bak1")).unwrap(), "v0");
    }

    #[test]
    fn sidebar_card_order_appends_missing_cards() {
        let cfg = Config { sidebar_order: vec!["motto".into(), "nope".into(), "todo".into()], ..Config::default() };
        let order = cfg.sidebar_card_order();
        assert_eq!(&order[..3], &["motto", "todo", "date"]);
        assert_eq!(order.len(), SIDEBAR_CARDS.len(), "缺失的卡片按默认顺序补齐、未知 id 忽略、不重复");
    }

    /// 设置窗口的开关：每个 Toggle 都接到一个配置字段（新增变体而漏接线时此测试失败），
    /// 翻转后返回新状态；走原子缓存的几项同步刷新
    #[test]
    fn every_toggle_flips_a_field_and_syncs_atoms() {
        let mut cfg = Config::default();
        for t in [
            Toggle::Autostart,
            Toggle::AutoUpdate,
            Toggle::ShowLunar,
            Toggle::ShowAdjust,
            Toggle::ShowOtherMonth,
            Toggle::ShowWeekNum,
            Toggle::ShowWeather,
            Toggle::ShowTray,
            Toggle::SidebarDate,
            Toggle::SidebarAlmanac,
            Toggle::SidebarEvents,
            Toggle::SidebarAgenda,
            Toggle::SidebarHistory,
            Toggle::SidebarMotto,
            Toggle::SidebarTodo,
            Toggle::SidebarReminds,
        ] {
            // 默认开的开关翻一次变关、默认关的变开——只断言"状态翻转 + 再翻还原"
            let before = json(&cfg);
            let after = cfg.toggle(t);
            assert_ne!(json(&cfg), before, "{t:?}: 必须改动至少一个配置字段");
            assert_eq!(cfg.toggle(t), !after, "{t:?}: 返回值是翻转后的新状态");
            assert_eq!(json(&cfg), before, "{t:?}: 翻两次回到原样");
        }
        // 走原子缓存的开关（hour12 另有测试在读全局状态，这里不动它；同一条 init_flags 路径）
        assert!(!cfg.toggle(Toggle::RemindSound) && !remind_sound_on());
        assert!(!cfg.toggle(Toggle::UseSystemToast) && !use_system_toast_on());
        assert!(!cfg.toggle(Toggle::FadeAnim) && !fade_anim_on());
        init_flags(&Config::default());
        assert!(remind_sound_on() && use_system_toast_on() && fade_anim_on(), "恢复默认，不影响其它测试");
        // 侧栏卡片开关与卡片定义一一对应
        let cards: Vec<Toggle> = SIDEBAR_CARDS.iter().map(|(_, _, t)| *t).collect();
        assert!(cards.iter().all(|t| t.is_sidebar_card()));
        assert_eq!(cards.iter().filter(|t| t.is_sidebar_card()).count(), SIDEBAR_CARDS.len());
        assert!(!Toggle::Autostart.is_sidebar_card() && !Toggle::Autostart.is_calendar_pref());
    }
}
