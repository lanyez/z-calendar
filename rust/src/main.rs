// 让 exe 成为纯 GUI 程序：双击运行不再闪现控制台窗口
#![windows_subsystem = "windows"]

mod config;
mod flyout;
mod gdi;
mod ics;
mod lunar;
mod lunar_data;
mod overlay;
mod tray;
mod weather;

use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use winapi::um::winuser::{DispatchMessageW, GetMessageW, TranslateMessage};

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

fn main() {
    // 单实例：重复启动时通知已有实例弹出日历，然后退出
    let show_event: usize;
    unsafe {
        let name: Vec<u16> = "z-calendar-show-event\0".encode_utf16().collect();
        use winapi::um::synchapi::{CreateEventW, OpenEventW, SetEvent};
        use winapi::um::winnt::EVENT_MODIFY_STATE;
        let existing = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        if !existing.is_null() {
            SetEvent(existing);
            return; // 已有实例在运行：唤醒它弹出日历后退出
        }
        let ev = CreateEventW(std::ptr::null_mut(), 0, 0, name.as_ptr());
        show_event = ev as usize;
    }

    gdi::startup();

    let config = config::Config::load();
    let holidays = ics::load_cache();
    let weather = weather::load_cache();

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

    let agenda = Arc::new(Mutex::new(flyout::load_agenda()));
    let tray = Arc::new(Mutex::new(tray::create(&st.config.lock().unwrap())));
    // 托盘图标可见性按配置（默认显示）
    {
        let show_tray = st.config.lock().unwrap().show_tray;
        if let Some(t) = tray.lock().unwrap().as_ref() {
            t.set_visible(show_tray);
        }
    }

    flyout::create_window(st.clone(), agenda, tray.clone());
    flyout::create_settings_window(st.clone(), tray);
    overlay::spawn(clock);

    // 调试：启动即显示设置窗口
    if std::env::var("CAL_SETTINGS").map(|v| v == "1").unwrap_or(false) {
        flyout::show_settings();
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
                        flyout::request_show();
                    }
                }
            })
            .ok();
    }

    spawn_fetch(st.clone(), refresh_rx);
    spawn_weather(st, weather_rx);

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
            // 失败 1 分钟后重试，成功 30 分钟后刷新
            let wait = if ok { Duration::from_secs(30 * 60) } else { Duration::from_secs(60) };
            crate::trim_working_set();
            match rx.recv_timeout(wait) {
                Ok(()) => {}
                Err(_) => {}
            }
        })
        .ok();
}
