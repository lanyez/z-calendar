//! 主题设置：深色 / 浅色 / 跟随系统。
//! 跟随系统读注册表 HKCU\...\Themes\Personalize\AppsUseLightTheme，
//! 收到 WM_SETTINGCHANGE 后失效缓存，下次取色重读。

use std::sync::atomic::{AtomicU8, Ordering};

pub const MODE_SYSTEM: u8 = 0;
pub const MODE_DARK: u8 = 1;
pub const MODE_LIGHT: u8 = 2;

static MODE: AtomicU8 = AtomicU8::new(MODE_DARK); // 默认深色，main 启动后按配置设置
static SYS_LIGHT: AtomicU8 = AtomicU8::new(0);
static SYS_FRESH: AtomicU8 = AtomicU8::new(0);

pub fn set_mode(m: u8) {
    MODE.store(m.min(MODE_LIGHT), Ordering::Relaxed);
}

pub fn mode() -> u8 {
    MODE.load(Ordering::Relaxed)
}

/// 系统主题可能变了（WM_SETTINGCHANGE）：失效缓存，下次 is_light 重读注册表
pub fn invalidate_system() {
    SYS_FRESH.store(0, Ordering::Relaxed);
}

pub fn is_light() -> bool {
    match MODE.load(Ordering::Relaxed) {
        MODE_LIGHT => true,
        MODE_DARK => false,
        _ => {
            if SYS_FRESH.load(Ordering::Relaxed) == 0 {
                SYS_LIGHT.store(read_apps_use_light() as u8, Ordering::Relaxed);
                SYS_FRESH.store(1, Ordering::Relaxed);
            }
            SYS_LIGHT.load(Ordering::Relaxed) == 1
        }
    }
}

fn read_apps_use_light() -> bool {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    let hk = winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize", KEY_READ);
    let hk = match hk {
        Ok(k) => k,
        Err(_) => return false,
    };
    hk.get_value::<u32, _>("AppsUseLightTheme").map(|v| v == 1).unwrap_or(false)
}

/// 半透明覆盖层（悬停/边框/分隔线等）：深色主题用白色、浅色主题翻转为深色，
/// alpha 保持不变——所有 `argb(N, 255,255,255)` 用法统一走这里
pub fn ov(a: u8) -> u32 {
    if is_light() {
        crate::gdi::argb(a, 0x1B, 0x20, 0x28)
    } else {
        crate::gdi::argb(a, 255, 255, 255)
    }
}

pub struct Pal {
    // 背景：窗口 / 卡片·弹出 / 右键菜单 / 下拉列表 / 输入框
    pub bg: u32,
    pub card: u32,
    pub popup: u32,
    pub drop: u32,
    pub field: u32,
    // 文字层级
    pub txt: u32,
    pub date: u32,
    pub title: u32,
    pub row: u32,
    pub sub: u32,
    pub dim: u32,
    pub legal: u32,
    pub week_head: u32,
    pub week_num: u32,
    pub icon: u32,
    pub slate: u32,
    // 强调色
    pub blue: u32,
    pub blue_hov: u32,
    pub plus_top: u32,
    pub plus_bot: u32,
    pub fest: u32,
    pub red: u32,
    pub orange: u32,
    pub orange2: u32,
    pub sel_bg: u32,
    /// 深背景上的高对比文字（深色主题=白，浅色主题=深）
    pub on_bg: u32,
    // 天气图标
    pub sun: u32,
    pub cloud: u32,
    pub rain: u32,
    // 提醒卡片底色（自带透明度）
    pub toast_bg: u32,
}

use crate::gdi::{argb, argb_a};

/// 全局强调色（0xRRGGBB）：深浅主题共用一个蓝，改这里即全应用生效
pub const ACCENT: u32 = 0x3E87FA;

pub static DARK: Pal = Pal {
    bg: argb(255, 0x20, 0x28, 0x38),
    card: argb(255, 0x26, 0x30, 0x42),
    popup: argb(255, 0x2A, 0x33, 0x45),
    drop: argb(255, 0x24, 0x2E, 0x40),
    field: argb(255, 0x2A, 0x33, 0x45),
    txt: argb(255, 0xE0, 0xE4, 0xEB),
    date: argb(255, 0xDD, 0xE2, 0xE9),
    title: argb(255, 0xDF, 0xE5, 0xEC),
    row: argb(255, 0xD7, 0xDD, 0xE4),
    sub: argb(255, 0x9A, 0xA1, 0xA9),
    // 弱化文字：原 #5C6673 对深底仅约 2.5:1，提亮到 3.6:1 以上
    dim: argb(255, 0x77, 0x82, 0x90),
    legal: argb(255, 0xE4, 0xE7, 0xEB),
    week_head: argb(255, 0xA6, 0xAD, 0xB6),
    week_num: argb(255, 0x82, 0x8C, 0x9A),
    icon: argb(255, 0x8A, 0x91, 0x9C),
    slate: argb(255, 0x8A, 0x93, 0xA0),
    blue: argb_a(255, ACCENT),
    blue_hov: argb(255, 0x53, 0x99, 0xFB),
    plus_top: argb(255, 0x38, 0xA6, 0xFA),
    plus_bot: argb(255, 0x2E, 0x8E, 0xF0),
    fest: argb(255, 0x4D, 0xA3, 0xFF),
    red: argb(255, 0xE5, 0x4B, 0x4B),
    orange: argb(255, 0xF0, 0xA0, 0x3E),
    orange2: argb(255, 0xE8, 0x96, 0x3C),
    sel_bg: argb_a(36, ACCENT),
    on_bg: argb(255, 255, 255, 255),
    sun: argb(255, 0xFF, 0xC8, 0x50),
    cloud: argb(255, 0xE8, 0xEC, 0xF2),
    rain: argb(255, 0x6F, 0xA8, 0xFF),
    toast_bg: argb(246, 0x20, 0x28, 0x38),
};

pub static LIGHT: Pal = Pal {
    bg: argb(255, 0xF2, 0xF4, 0xF8),
    card: argb(255, 255, 255, 255),
    popup: argb(255, 255, 255, 255),
    drop: argb(255, 255, 255, 255),
    field: argb(255, 0xF3, 0xF5, 0xF9),
    txt: argb(255, 0x1B, 0x20, 0x28),
    date: argb(255, 0x1B, 0x20, 0x2A),
    title: argb(255, 0x19, 0x1E, 0x26),
    row: argb(255, 0x22, 0x28, 0x31),
    sub: argb(255, 0x5B, 0x64, 0x70),
    // 弱化文字：原 #9AA3AE 对浅底仅约 2.6:1，加深到 3.6:1 以上
    dim: argb(255, 0x76, 0x80, 0x8E),
    legal: argb(255, 0x2F, 0x35, 0x3D),
    week_head: argb(255, 0x56, 0x5E, 0x68),
    week_num: argb(255, 0x76, 0x80, 0x8E),
    icon: argb(255, 0x7A, 0x82, 0x8C),
    slate: argb(255, 0x6B, 0x72, 0x80),
    blue: argb_a(255, ACCENT),
    blue_hov: argb(255, 0x2B, 0x6F, 0xE0),
    plus_top: argb(255, 0x38, 0xA6, 0xFA),
    plus_bot: argb(255, 0x2E, 0x8E, 0xF0),
    // 节日蓝/待办橙在 9~11px 小字场景处于对比临界，各加深一档（白底 ≈4.8:1）
    fest: argb(255, 0x1F, 0x66, 0xD0),
    red: argb(255, 0xD1, 0x34, 0x38),
    orange: argb(255, 0xA6, 0x5F, 0x0E),
    orange2: argb(255, 0x9C, 0x5A, 0x0C),
    sel_bg: argb_a(60, ACCENT),
    on_bg: argb(255, 0x1B, 0x20, 0x28),
    sun: argb(255, 0xF0, 0xA6, 0x26),
    cloud: argb(255, 0x98, 0xA2, 0xAC),
    rain: argb(255, 0x4C, 0x8D, 0xF0),
    toast_bg: argb(246, 255, 255, 255),
};

pub fn pal() -> &'static Pal {
    if is_light() {
        &LIGHT
    } else {
        &DARK
    }
}
