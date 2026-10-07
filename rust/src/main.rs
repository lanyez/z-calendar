// 让 exe 成为纯 GUI 程序：双击运行不再闪现控制台窗口
#![windows_subsystem = "windows"]

mod almanac;
mod config;
mod ctxmenu;
mod events;
mod flyout;
mod inputbox;
mod gdi;
mod history;
mod ics;
mod lunar;
mod lunar_data;
mod motto;
mod net;
mod overlay;
mod recur_menu;
mod reminder;
mod sidebar;
mod textedit;
mod theme;
mod toast;
mod tray;
mod wnotify;
mod uia_clock;
mod weather;

use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use winapi::um::libloaderapi::{GetModuleHandleW, GetProcAddress};
use winapi::um::winuser::{DispatchMessageW, GetMessageW, SetProcessDPIAware, TranslateMessage};

/// UTF-16 编码（供各模块拼 Win32 字符串）
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[link(name = "kernel32")]
extern "system" {
    fn SetProcessWorkingSetSize(hproc: winapi::um::winnt::HANDLE, min: usize, max: usize) -> i32;
    fn GetCurrentProcess() -> winapi::um::winnt::HANDLE;
}

unsafe fn set_min_ws() {
    SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
}

/// 修剪工作集（供 flyout 在隐藏时调用）
pub fn trim_working_set() {
    unsafe {
        set_min_ws();
        compact_heap();
    }
}

#[link(name = "kernel32")]
extern "system" {
    fn GetProcessHeap() -> winapi::um::winnt::HANDLE;
    fn HeapCompact(hheap: winapi::um::winnt::HANDLE, flags: u32) -> usize;
}

/// 压缩进程堆：合并空闲段并尝试反提交（长跑后堆段只增不减）
fn compact_heap() {
    unsafe {
        HeapCompact(GetProcessHeap(), 0);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 进程 DPI 感知：不声明时（DPI Unaware）DWM 会把整窗位图拉伸到当前显示缩放
/// 比例，文字和图形整体发虚（125%/150% 缩放的 Win11 上尤其明显）。
/// 用 Per-Monitor V2：所有坐标（窗口矩形 / 低级鼠标钩子 / UIA / 光标）恒为物理
/// 像素并实时反映当前缩放——System Aware 的坐标系固定在登录时的 DPI，用户改
/// 缩放后时钟矩形与钩子坐标错位，点击时钟会穿透给系统日历。缩放变化由
/// flyout 的定时器轮询（gdi::primary_scale）感知并全局重缩放。
/// 已知限制：多显示器不同缩放时按主屏 sf 绘制，副屏由 DWM 微调（逐窗口
/// 不同 sf 需要随 WM_DPICHANGED 重排，暂未做）。
unsafe fn set_dpi_aware() {
    let user32 = GetModuleHandleW(wide("user32.dll").as_ptr());
    if !user32.is_null() {
        let f = GetProcAddress(user32, b"SetProcessDpiAwarenessContext\0".as_ptr() as *const i8);
        if !f.is_null() {
            type SetCtxFn = unsafe extern "system" fn(winapi::shared::windef::HWND) -> i32;
            let f: SetCtxFn = std::mem::transmute(f);
            // DPI_AWARENESS_CONTEXT：PER_MONITOR_AWARE_V2 = -4，SYSTEM_AWARE = -2
            if f(-4isize as winapi::shared::windef::HWND) != 0 {
                return;
            }
            if f(-2isize as winapi::shared::windef::HWND) != 0 {
                return;
            }
        }
    }
    // 老系统回退
    SetProcessDPIAware();
}

fn main() {
    // 必须先于一切窗口/DPI 相关调用
    unsafe { set_dpi_aware() };

    // 通知中心按钮激活参数（zcal: 前缀）：已有实例时经 IPC 文件转发，否则本实例处理
    let toast_args: Vec<String> = std::env::args().skip(1).filter(|a| a.starts_with("zcal:")).collect();

    // 单实例：重复启动时通知已有实例弹出日历（或处理通知激活），然后退出
    let show_event: usize;
    unsafe {
        let name: Vec<u16> = "z-calendar-show-event\0".encode_utf16().collect();
        use winapi::um::synchapi::{CreateEventW, OpenEventW, SetEvent};
        use winapi::um::winnt::EVENT_MODIFY_STATE;
        let existing = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        if !existing.is_null() {
            if !toast_args.is_empty() {
                let _ = std::fs::write(wnotify::ipc_path(), toast_args.join("\n"));
            }
            SetEvent(existing);
            return; // 已有实例在运行：唤醒它后退出
        }
        let ev = CreateEventW(std::ptr::null_mut(), 0, 0, name.as_ptr());
        show_event = ev as usize;
    }

    gdi::startup();

    let config = config::Config::load();
    config::init_flags(&config);
    gdi::set_text_scale(config.ui_font_scale);
    theme::set_mode(config.theme);
    let holidays = ics::load_cache();
    // 历史上的今天：删除不是当天的缓存数据
    history::purge_stale();
    // 时间格言：删除旧版本遗留的缓存（现改为每次点击日期都重新获取）
    motto::purge_stale();
    let weather = if std::env::var("CAL_FAKE_WX").map(|v| v == "1").unwrap_or(false) {
        Some(weather::fake())
    } else {
        weather::load_cache()
    };

    let (refresh_tx, refresh_rx) = mpsc::channel::<()>();
    let (weather_tx, weather_rx) = mpsc::channel::<()>();
    let clock: overlay::SharedClock = Arc::new(Mutex::new(None));

    let st = flyout::SharedState {
        config: Arc::new(Mutex::new(config)),
        holidays: Arc::new(RwLock::new(holidays)),
        weather: Arc::new(Mutex::new(weather)),
        clock: clock.clone(),
        refresh_tx,
        weather_tx,
    };

    // 提醒弹窗线程先起（日程/待办加载若触发数据自愈，通知才能送达）
    let toast_tx = toast::spawn();

    let agenda = Arc::new(Mutex::new(flyout::load_agenda()));
    // 注册共享句柄：提醒卡片撤销恢复等无窗口上下文的模块读写同一份数据
    events::set_shared_agenda(agenda.clone());
    let tray = Arc::new(Mutex::new(tray::create(&st.config.lock().unwrap())));
    // 托盘图标可见性按配置（默认显示）
    {
        let show_tray = st.config.lock().unwrap().show_tray;
        if let Some(t) = tray.lock().unwrap().as_ref() {
            t.set_visible(show_tray);
        }
    }

    flyout::create_window(st.clone(), agenda.clone(), tray.clone());
    sidebar::create_window(st.clone(), agenda.clone());
    ctxmenu::create_window(st.clone(), tray.clone());
    ctxmenu::ctxmenu_date::create_date_menu_window();
    inputbox::create_window(agenda.clone());
    recur_menu::create_window(agenda.clone());
    flyout::create_settings_window(st.clone(), tray);
    flyout::create_forecast_window(st.clone());
    overlay::spawn(clock);
    // Win11 的任务栏时钟是 XAML 渲染（无 TrayClockWClass 窗口），由 UIA 定位线程
    // 提供时钟矩形；Win10 走 overlay 的经典窗口链，无需此线程
    if uia_clock::is_win11_or_newer() {
        uia_clock::spawn();
    }

    // 调试：启动即显示设置窗口
    if std::env::var("CAL_SETTINGS").map(|v| v == "1").unwrap_or(false) {
        flyout::show_settings();
    }
    // 调试：验证通知中心就绪链路（AUMID + 开始菜单快捷方式），结果写 wnotify.log
    if std::env::var("CAL_WNREADY").map(|v| v == "1").unwrap_or(false) {
        let ok = wnotify::ensure_ready();
        let lnk = wnotify::lnk_exists();
        let _ = std::fs::write(
            config::data_dir().join("wnotify.log"),
            format!("ensure_ready={} lnk_exists={} fail_step={} hr={}
", ok, lnk, crate::wnotify::last_fail(), crate::wnotify::last_hr()),
        );
    }
    // 调试：发一条真实系统通知验证端到端链路（屏幕出现一次测试 Toast）
    if std::env::var("CAL_WNSHOW").map(|v| v == "1").unwrap_or(false) {
        wnotify::ensure_ready();
        let act = toast::Act::TodoDone { id: "test".into(), date: "2026-10-7".into(), recur: false };
        let ok = wnotify::show_reminder("Z日历 · 通知中心测试", "系统 Toast 发送成功（可点按钮激活）", &act, true);
        let _ = std::fs::write(config::data_dir().join("wnotify.log"), format!("show_reminder={}
", ok));
    }

    // 修剪工作集：启动完成后内存降到最低（按需自动换回）
    {
        std::thread::Builder::new()
            .name("ws-trim".into())
            .spawn(|| {
                std::thread::sleep(Duration::from_secs(3));
                unsafe {
                    SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
                }
            })
            .ok();
    }

    // 第二实例唤醒线程
    {
        let ev = show_event;
        std::thread::Builder::new()
            .name("show-event".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                if ev == 0 {
                    return;
                }
                unsafe {
                    use winapi::um::synchapi::WaitForSingleObject;
                    use winapi::um::winbase::INFINITE;
                    loop {
                        WaitForSingleObject(ev as winapi::um::winnt::HANDLE, INFINITE);
                        // 通知中心按钮激活：优先处理 IPC 转发的参数，否则弹出日历
                        let p = wnotify::ipc_path();
                        if let Ok(args) = std::fs::read_to_string(&p) {
                            let _ = std::fs::remove_file(&p);
                            if !args.trim().is_empty() {
                                wnotify::dispatch(&args);
                                continue;
                            }
                        }
                        flyout::request_show();
                    }
                }
            })
            .ok();
    }

    spawn_fetch(st.clone(), refresh_rx);
    spawn_weather(st, weather_rx);

    // 提醒引擎：扫描日程/待办到期提醒，经 toast 弹窗通知
    reminder::spawn(toast_tx);

    // 本实例即通知激活的启动目标（无其他实例在跑）：处理按钮参数后常驻
    if !toast_args.is_empty() {
        wnotify::dispatch(&toast_args.join("\n"));
    }

    // 主线程消息循环
    unsafe {
        let mut msg: winapi::um::winuser::MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn do_fetch(st: &flyout::SharedState) -> bool {
    let url = st.config.lock().unwrap().ics_url.clone();
    match ics::fetch_map(&url) {
        Some(map) => {
            {
                let mut h = st.holidays.write().unwrap();
                *h = map.clone();
            }
            ics::save_cache(&st.holidays.read().unwrap());
            {
                let mut cfg = st.config.lock().unwrap();
                cfg.last_ics_update = now_ms();
                cfg.save();
            }
            true
        }
        None => false,
    }
}

fn spawn_fetch(st: flyout::SharedState, rx: mpsc::Receiver<()>) {
    std::thread::Builder::new()
        .name("ics-updater".into())
        .stack_size(256 * 1024)
        .spawn(move || loop {
            let should = {
                let cfg = st.config.lock().unwrap();
                cfg.auto_update && now_ms() - cfg.last_ics_update > 12 * 3600 * 1000
            };
            if should {
                let ok = do_fetch(&st);
                flyout::post_ics_result(ok);
            }
            match rx.recv_timeout(Duration::from_secs(30 * 60)) {
                Ok(()) => {
                    let ok = do_fetch(&st);
                    flyout::post_ics_result(ok);
                }
                Err(_) => {}
            }
            // 拉取产生的临时缓冲用完即修剪
            crate::trim_working_set();
        })
        .ok();
}

fn spawn_weather(st: flyout::SharedState, rx: mpsc::Receiver<()>) {
    std::thread::Builder::new()
        .name("weather".into())
        .stack_size(256 * 1024)
        .spawn(move || loop {
            let mut ok = true;
            let show = st.config.lock().unwrap().show_weather;
            if show {
                match weather::fetch() {
                    Some(w) => {
                        weather::save_cache(&w);
                        *st.weather.lock().unwrap() = Some(w);
                    }
                    None => ok = false,
                }
            } else {
                *st.weather.lock().unwrap() = None;
            }
            // 失败 1 分钟后重试，成功 1 小时后刷新（每小时自动更新）
            let wait = if ok { Duration::from_secs(60 * 60) } else { Duration::from_secs(60) };
            crate::trim_working_set();
            match rx.recv_timeout(wait) {
                Ok(()) => {}
                Err(_) => {}
            }
        })
        .ok();
}
