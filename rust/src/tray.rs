//! 托盘图标与菜单
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::config::Config;

pub struct Tray {
    _inner: TrayIcon,
    pub autostart_item: CheckMenuItem,
}

impl Tray {
    pub fn set_visible(&self, visible: bool) {
        let _ = self._inner.set_visible(visible);
    }
}

pub fn create(config: &Config) -> Option<Tray> {
    let menu = Menu::new();
    let show = MenuItem::with_id("show", "显示日历", true, None);
    let sep1 = PredefinedMenuItem::separator();
    let autostart_item = CheckMenuItem::with_id("autostart", "开机自启", true, config.autostart, None);
    let refresh = MenuItem::with_id("refresh", "立即更新节假日数据", true, None);
    let sep2 = PredefinedMenuItem::separator();
    let quit = MenuItem::with_id("quit", "退出", true, None);
    let _ = menu.append(&show);
    let _ = menu.append(&sep1);
    let _ = menu.append(&autostart_item.clone());
    let _ = menu.append(&refresh);
    let _ = menu.append(&sep2);
    let _ = menu.append(&quit);

    let tray = TrayIconBuilder::new().with_id("cf-tray")
        .with_icon(icon_rgba())
        .with_tooltip("Z日历 · 点击任务栏时钟查看节假日")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .ok()?;
    Some(Tray { _inner: tray, autostart_item })
}

/// 托盘图标：构建时由 icon.png 生成的 32×32 RGBA（与 exe 图标同一来源，见 build.rs）
fn icon_rgba() -> tray_icon::Icon {
    const RGBA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tray_rgba.bin"));
    tray_icon::Icon::from_rgba(RGBA.to_vec(), 32, 32).expect("icon")
}

/// 轮询托盘菜单事件（返回菜单 id）
pub fn poll_menu_events() -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(ev) = MenuEvent::receiver().try_recv() {
        out.push(ev.id.0.clone());
    }
    out
}

/// 轮询托盘图标左键点击
pub fn poll_icon_clicks() -> usize {
    let mut count = 0;
    while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
        if let TrayIconEvent::Click { button: MouseButton::Left, .. } = ev {
            count += 1;
        }
    }
    count
}
