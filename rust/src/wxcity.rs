//! 天气城市选择弹框：热门城市直达 + nmc.cn autocomplete 搜索，
//! 选择后解析数字站号写入配置并加入天气面板 tab。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use winapi::shared::windef::{HWND, POINT, RECT};
use winapi::shared::minwindef::{LRESULT, UINT, WPARAM, LPARAM};
use winapi::um::winuser::*;
use winapi::um::wingdi::BLENDFUNCTION;

use crate::flyout::SharedState;
use crate::gdi;

const CP_W: f32 = 400.0;
const CP_H: f32 = 420.0;

const CP_MSG_HITS: UINT = WM_APP + 1; // 搜索结果（wp=gen，lp=Box<Vec<CityHit>>）
const CP_MSG_ADD: UINT = WM_APP + 2;  // 站号解析完成（lp=Box<Result<(站号, 名称), 名称>>）

const HOT_CITIES: [&str; 16] = [
    "北京", "上海", "广州", "深圳", "杭州", "成都", "重庆", "武汉",
    "西安", "南京", "天津", "苏州", "长沙", "厦门", "青岛", "哈尔滨",
];

static CP_HWND: AtomicUsize = AtomicUsize::new(0);
static CP_UI: Mutex<Option<SendCp>> = Mutex::new(None);

struct SendCp(Box<CityUi>);
unsafe impl Send for SendCp {}

struct CityUi {
    hwnd: usize,
    sf: f32,
    w: f32,
    h: f32,
    mem_dc: usize,
    hbmp: usize,
    bmp: gdi::Gp,
    scan0: *mut u8,
    g: gdi::Gp,
    cache: gdi::Cache,
    st: SharedState,
    text: String,
    caret: usize,
    sel: Option<usize>,
    undo: crate::textedit::Undo,
    /// IME 组合中的拼音串（跟随显示）
    comp: String,
    caret_on: bool,
    results: Vec<crate::weather::CityHit>,
    sel_row: usize,
    searching: bool,
    search_gen: u64,
    /// 正在解析站号（防重复点击）
    busy: bool,
    close_hover: bool,
    hot_hover: Option<usize>,
    row_hover: Option<usize>,
    /// 底部状态文字与是否为错误（红）
    status: String,
    status_err: bool,
    // paint 时更新的热区
    close_rect: gdi::RectF,
    hot_rects: Vec<gdi::RectF>,
    row_rects: Vec<gdi::RectF>,
}

pub fn hwnd() -> usize {
    CP_HWND.load(Ordering::Relaxed)
}

fn rect_hit(x: f32, y: f32, r: &gdi::RectF) -> bool {
    x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
}

// ---------------------------------------------------------------------------
// imm32：IME 组合串读取（与 inputbox 相同方式）
// ---------------------------------------------------------------------------
#[link(name = "imm32")]
extern "system" {
    fn ImmGetContext(hwnd: HWND) -> *mut winapi::ctypes::c_void;
    fn ImmReleaseContext(hwnd: HWND, himc: *mut winapi::ctypes::c_void) -> i32;
    fn ImmGetCompositionStringW(himc: *mut winapi::ctypes::c_void, index: i32, buf: *mut winapi::ctypes::c_void, len: i32) -> i32;
}

#[link(name = "user32")]
extern "system" {
    fn SetFocus(hwnd: HWND) -> HWND;
    fn SetForegroundWindow(hwnd: HWND) -> i32;
}

#[link(name = "gdiplus")]
extern "system" {
    fn GdipGetImageGraphicsContext(image: gdi::Gp, graphics: *mut gdi::Gp) -> i32;
    fn GdipSetSmoothingMode(graphics: gdi::Gp, mode: i32) -> i32;
    fn GdipSetTextRenderingHint(graphics: gdi::Gp, mode: i32) -> i32;
}

const GCS_COMPSTR: i32 = 0x0008;
const GCS_RESULTSTR: i32 = 0x0800;

unsafe fn read_ime(hwnd: HWND, mode: i32) -> Option<String> {
    let himc = ImmGetContext(hwnd);
    if himc.is_null() {
        return None;
    }
    let len = ImmGetCompositionStringW(himc, mode, std::ptr::null_mut(), 0);
    let mut out = None;
    if len >= 0 {
        let mut buf = vec![0u8; (len + 2) as usize];
        let got = ImmGetCompositionStringW(himc, mode, buf.as_mut_ptr() as *mut winapi::ctypes::c_void, len);
        if got >= 0 {
            let slice = std::slice::from_raw_parts::<u16>(buf.as_ptr() as *const u16, (len as usize) / 2);
            out = Some(String::from_utf16_lossy(slice));
        }
    }
    ImmReleaseContext(hwnd, himc);
    out
}

// ---------------------------------------------------------------------------
// 窗口创建 / 打开 / 关闭
// ---------------------------------------------------------------------------

unsafe extern "system" fn cp_wndproc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            ValidateRect(hwnd, std::ptr::null_mut());
            0
        }
        WM_ERASEBKGND => 1,
        WM_TIMER => {
            let mut guard = CP_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                match wp {
                    1 => {
                        f.caret_on = !f.caret_on;
                        f.redraw();
                    }
                    2 => {
                        KillTimer(hwnd, 2);
                        if f.text.trim().is_empty() {
                            f.results.clear();
                            f.sel_row = 0;
                            f.searching = false;
                            f.redraw();
                        } else {
                            f.searching = true;
                            f.search_gen += 1;
                            let gen = f.search_gen;
                            let q = f.text.clone();
                            f.redraw();
                            let h = hwnd as usize;
                            std::thread::Builder::new()
                                .name("wxsearch".into())
                                .stack_size(256 * 1024)
                                .spawn(move || {
                                    let hits = crate::weather::search_cities(&q);
                                    let boxd = Box::new(hits);
                                    unsafe {
                                        PostMessageW(h as HWND, CP_MSG_HITS, gen as WPARAM, Box::into_raw(boxd) as LPARAM);
                                    }
                                })
                                .ok();
                        }
                    }
                    _ => {}
                }
            }
            0
        }
        CP_MSG_HITS => {
            let mut guard = CP_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                let hits = unsafe { Box::from_raw(lp as *mut Vec<crate::weather::CityHit>) };
                if wp as u64 == f.search_gen {
                    f.results = hits.iter().take(8).cloned().collect();
                    f.sel_row = 0;
                    f.searching = false;
                    f.redraw();
                }
            }
            0
        }
        CP_MSG_ADD => {
            let mut guard = CP_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                let got = unsafe { Box::from_raw(lp as *mut Result<(String, String), String>) };
                f.busy = false;
                match *got {
                    Ok((id, name)) => {
                        match add_city(&f.st, &id, &name) {
                            Ok(()) => {
                                let st = f.st.clone();
                                drop(guard);
                                // 添加成功后侧窗切到新城市 tab（数据到达即生效）
                                crate::flyout::forecast_focus_city(&id);
                                let _ = st.weather_tx.send(());
                                close();
                                return 0;
                            }
                            Err(e) => {
                                f.status = e.to_string();
                                f.status_err = true;
                                f.redraw();
                            }
                        }
                    }
                    Err(who) => {
                        crate::log::warn(&format!("天气城市添加失败: {}", who));
                        f.status = format!("添加 {} 失败，请稍后重试", who);
                        f.status_err = true;
                        f.redraw();
                    }
                }
            }
            0
        }
        WM_CHAR => {
            let mut guard = CP_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                if (wp as u32) >= 0x20 {
                    if let Some(ch) = char::from_u32(wp as u32) {
                        f.undo.snapshot(&f.text, f.caret, f.sel, "ins");
                        crate::textedit::insert(&mut f.text, &mut f.caret, &mut f.sel, &ch.to_string());
                        f.comp.clear();
                        on_text_changed(f, hwnd);
                    }
                }
            }
            0
        }
        WM_IME_COMPOSITION => {
            let mut guard = CP_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                if lp as i32 & GCS_RESULTSTR != 0 {
                    if let Some(s) = unsafe { read_ime(hwnd, GCS_RESULTSTR) } {
                        f.undo.snapshot(&f.text, f.caret, f.sel, "ins");
                        crate::textedit::insert(&mut f.text, &mut f.caret, &mut f.sel, &s);
                        f.comp.clear();
                        on_text_changed(f, hwnd);
                    }
                } else if lp as i32 & GCS_COMPSTR != 0 {
                    f.comp = unsafe { read_ime(hwnd, GCS_COMPSTR) }.unwrap_or_default();
                    f.redraw();
                }
            }
            0
        }
        WM_KEYDOWN => {
            let mut guard = CP_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                let shift = unsafe { (GetKeyState(0x10) as u16) & 0x8000 != 0 };
                let ctrl = unsafe { (GetKeyState(0x11) as u16) & 0x8000 != 0 };
                match wp as i32 {
                    0x1B => {
                        drop(guard);
                        close();
                    }
                    0x0D => {
                        // 回车：选中当前搜索结果
                        if !f.results.is_empty() && !f.busy {
                            let hit = f.results[f.sel_row.min(f.results.len() - 1)].clone();
                            begin_resolve(f, &hit);
                        }
                    }
                    0x26 => {
                        if !f.results.is_empty() {
                            f.sel_row = f.sel_row.saturating_sub(1);
                            f.redraw();
                        }
                    }
                    0x28 => {
                        if !f.results.is_empty() {
                            f.sel_row = (f.sel_row + 1).min(f.results.len() - 1);
                            f.redraw();
                        }
                    }
                    0x08 => {
                        f.undo.snapshot(&f.text, f.caret, f.sel, "");
                        crate::textedit::backspace(&mut f.text, &mut f.caret, &mut f.sel);
                        on_text_changed(f, hwnd);
                    }
                    0x2E => {
                        f.undo.snapshot(&f.text, f.caret, f.sel, "");
                        crate::textedit::delete_fwd(&mut f.text, &mut f.caret, &mut f.sel);
                        on_text_changed(f, hwnd);
                    }
                    0x25 => {
                        crate::textedit::move_caret(&f.text, &mut f.caret, &mut f.sel, -1, shift);
                        f.redraw();
                    }
                    0x27 => {
                        crate::textedit::move_caret(&f.text, &mut f.caret, &mut f.sel, 1, shift);
                        f.redraw();
                    }
                    0x24 | 0x23 => {
                        crate::textedit::home_end(&f.text, &mut f.caret, &mut f.sel, wp as i32 == 0x23, shift);
                        f.redraw();
                    }
                    0x41 if ctrl => {
                        crate::textedit::select_all(&f.text, &mut f.caret, &mut f.sel);
                        f.redraw();
                    }
                    0x5A if ctrl => {
                        f.undo.undo(&mut f.text, &mut f.caret, &mut f.sel);
                        on_text_changed(f, hwnd);
                    }
                    0x59 if ctrl => {
                        f.undo.redo(&mut f.text, &mut f.caret, &mut f.sel);
                        on_text_changed(f, hwnd);
                    }
                    _ => {}
                }
            }
            0
        }
        WM_MOUSEMOVE => {
            let mut guard = CP_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                let x = ((lp & 0xFFFF) as u16 as i16) as f32 / f.sf;
                let y = (((lp as usize) >> 16) as u16 as i16) as f32 / f.sf;
                let ch = rect_hit(x, y, &f.close_rect);
                let hh = f.hot_rects.iter().position(|r| rect_hit(x, y, r));
                let rh = f.row_rects.iter().position(|r| rect_hit(x, y, r));
                if ch != f.close_hover || hh != f.hot_hover || rh != f.row_hover {
                    f.close_hover = ch;
                    f.hot_hover = hh;
                    f.row_hover = rh;
                    f.redraw();
                }
                if ch || hh.is_some() || rh.is_some() {
                    unsafe {
                        SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_HAND));
                    }
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    unsafe {
                        TrackMouseEvent(&mut tme);
                    }
                }
            }
            0
        }
        WM_MOUSELEAVE => {
            let mut guard = CP_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                let dirty = f.close_hover || f.hot_hover.is_some() || f.row_hover.is_some();
                f.close_hover = false;
                f.hot_hover = None;
                f.row_hover = None;
                if dirty {
                    f.redraw();
                }
            }
            0
        }
        WM_LBUTTONDOWN => {
            let mut guard = CP_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                let x = ((lp & 0xFFFF) as u16 as i16) as f32 / f.sf;
                let y = (((lp as usize) >> 16) as u16 as i16) as f32 / f.sf;
                if rect_hit(x, y, &f.close_rect) {
                    drop(guard);
                    close();
                } else if let Some(i) = f.row_rects.iter().position(|r| rect_hit(x, y, r)) {
                    if !f.busy {
                        let hit = f.results[i].clone();
                        begin_resolve(f, &hit);
                    }
                } else if let Some(i) = f.hot_rects.iter().position(|r| rect_hit(x, y, r)) {
                    if !f.busy {
                        begin_search_add(f, HOT_CITIES[i]);
                    }
                }
            }
            0
        }
        WM_ACTIVATE => {
            // 失焦且焦点不在软件自身窗口：关闭
            if (wp & 0xFFFF) as u16 == 0 && !crate::flyout::foreground_is_own() {
                close();
            }
            0
        }
        WM_DESTROY => {
            KillTimer(hwnd, 1);
            KillTimer(hwnd, 2);
            CP_HWND.store(0, Ordering::Relaxed);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// 文本变化：重置选中行并启动防抖搜索定时器
fn on_text_changed(f: &mut CityUi, hwnd: HWND) {
    f.sel_row = 0;
    f.redraw();
    unsafe {
        SetTimer(hwnd, 2, 350, None);
    }
}

/// 搜索结果 → 站号 → 加入（后台线程）
fn begin_resolve(f: &mut CityUi, hit: &crate::weather::CityHit) {
    f.busy = true;
    f.status = format!("正在添加 {}…", hit.name);
    f.status_err = false;
    f.redraw();
    let h = f.hwnd;
    let hit = hit.clone();
    std::thread::Builder::new()
        .name("wxresolve".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            // 展示名去掉括号注记（如“赣县（赣州）” → “赣县”，tab 显示更紧凑）
            let display = hit.name.split('（').next().unwrap_or(&hit.name).to_string();
            let got = crate::weather::resolve_hit(&hit)
                .map(|id| (id, display.clone()))
                .ok_or_else(|| display.clone());
            let boxd = Box::new(got);
            unsafe {
                PostMessageW(h as HWND, CP_MSG_ADD, 0, Box::into_raw(boxd) as LPARAM);
            }
        })
        .ok();
}

/// 热门城市：先搜索取第一条结果再解析站号（后台线程）
fn begin_search_add(f: &mut CityUi, name: &str) {
    f.busy = true;
    f.status = format!("正在添加 {}…", name);
    f.status_err = false;
    f.redraw();
    let h = f.hwnd;
    let name = name.to_string();
    std::thread::Builder::new()
        .name("wxresolve".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            // 搜索结果按名称校正后取站号（nmc 相关性不稳，首条未必是目标城市）
            let hits = crate::weather::search_cities(&name);
            let got = crate::weather::pick_best(&hits, &name)
                .and_then(|hit| crate::weather::resolve_hit(hit).map(|id| (id, name.clone())))
                .ok_or_else(|| name.clone());
            let boxd = Box::new(got);
            unsafe {
                PostMessageW(h as HWND, CP_MSG_ADD, 0, Box::into_raw(boxd) as LPARAM);
            }
        })
        .ok();
}

/// 加入自定义城市（最多 4 个，站号去重）
fn add_city(st: &SharedState, id: &str, name: &str) -> Result<(), &'static str> {
    let dup_loc = st
        .weather
        .lock()
        .unwrap()
        .first()
        .map(|w| w.station == id)
        .unwrap_or(false);
    let mut cfg = st.config.lock().unwrap();
    if cfg.wx_cities.len() >= 4 {
        return Err("最多添加 4 个自定义城市");
    }
    if dup_loc || cfg.wx_cities.iter().any(|c| c.id == id) {
        return Err("该城市已在列表中");
    }
    cfg.wx_cities.push(crate::config::WxCity { id: id.into(), name: name.into() });
    cfg.save();
    Ok(())
}

/// 打开弹框（复用已创建的窗口；清空上次输入）
pub fn open(st: SharedState) {
    unsafe {
        if CP_HWND.load(Ordering::Relaxed) == 0 {
            create(st.clone());
        }
        let h = CP_HWND.load(Ordering::Relaxed);
        if h == 0 {
            return;
        }
        {
            let mut guard = CP_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                f.st = st;
                f.text.clear();
                f.caret = 0;
                f.sel = None;
                f.comp.clear();
                f.results.clear();
                f.sel_row = 0;
                f.searching = false;
                f.busy = false;
                f.status.clear();
                f.status_err = false;
                f.close_hover = false;
                f.hot_hover = None;
                f.row_hover = None;
                f.caret_on = true; // 打开即显示光标（不等待首个闪烁周期）
            }
        }
        // 居中于鼠标所在显示器工作区（与新建弹窗一致的摆放策略）
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        let (wa_l, wa_t, wa_r, wa_b) = crate::gdi::work_area_of_point(pt.x, pt.y);
        let x = wa_l + (wa_r - wa_l - gdi::phys(CP_W) as i32) / 2;
        let y = wa_t + (wa_b - wa_t - gdi::phys(CP_H) as i32) / 2;
        SetWindowPos(h as HWND, HWND_TOPMOST, x, y, gdi::phys(CP_W) as i32, gdi::phys(CP_H) as i32, SWP_NOACTIVATE);
        ShowWindow(h as HWND, SW_SHOW);
        SetForegroundWindow(h as HWND);
        SetFocus(h as HWND);
        let mut guard = CP_UI.lock().unwrap();
        if let Some(f) = guard.as_mut() {
            f.0.redraw();
        }
        SetTimer(h as HWND, 1, 500, None); // 光标闪烁
    }
}

pub fn close() {
    let h = CP_HWND.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            ShowWindow(h as HWND, SW_HIDE);
            KillTimer(h as HWND, 1);
            KillTimer(h as HWND, 2);
        }
    }
}

pub fn create(st: SharedState) {
    unsafe {
        let cls = crate::wide("z-calendar-wxcity");
        let hinstance = winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null_mut());
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(cp_wndproc);
        wc.hInstance = hinstance;
        wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        wc.lpszClassName = cls.as_ptr();
        RegisterClassW(&wc);

        let title = crate::wide("Z日历添加城市");
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED,
            cls.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            32000,
            32000,
            gdi::phys(CP_W) as i32,
            gdi::phys(CP_H) as i32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null_mut(),
        );
        if hwnd.is_null() {
            return;
        }
        CP_HWND.store(hwnd as usize, Ordering::Relaxed);

        let mut ui = Box::new(CityUi {
            hwnd: hwnd as usize,
            sf: gdi::scale(),
            w: CP_W,
            h: CP_H,
            mem_dc: 0,
            hbmp: 0,
            bmp: std::ptr::null_mut(),
            scan0: std::ptr::null_mut(),
            g: std::ptr::null_mut(),
            cache: gdi::Cache::new(),
            st,
            text: String::new(),
            caret: 0,
            sel: None,
            undo: crate::textedit::Undo::new(64),
            comp: String::new(),
            caret_on: true,
            results: Vec::new(),
            sel_row: 0,
            searching: false,
            search_gen: 0,
            busy: false,
            close_hover: false,
            hot_hover: None,
            row_hover: None,
            status: String::new(),
            status_err: false,
            close_rect: gdi::RectF { x: 0.0, y: 0.0, w: 0.0, h: 0.0 },
            hot_rects: Vec::new(),
            row_rects: Vec::new(),
        });
        let (mem_dc, hbmp, bmp, scan0) = gdi::alloc_dib(ui.w, ui.h);
        ui.mem_dc = mem_dc;
        ui.hbmp = hbmp;
        ui.bmp = bmp;
        ui.scan0 = scan0;
        GdipGetImageGraphicsContext(ui.bmp, &mut ui.g);
        CP_UI.lock().unwrap().replace(SendCp(ui));
    }
}

// ---------------------------------------------------------------------------
// 绘制
// ---------------------------------------------------------------------------

impl CityUi {
    fn redraw(&mut self) {
        if self.bmp.is_null() {
            let (mem_dc, hbmp, bmp, scan0) = unsafe { gdi::alloc_dib(self.w, self.h) };
            self.mem_dc = mem_dc;
            self.hbmp = hbmp;
            self.bmp = bmp;
            self.scan0 = scan0;
        }
        if self.g.is_null() {
            unsafe {
                GdipGetImageGraphicsContext(self.bmp, &mut self.g);
            }
        }
        let cache_ptr: *const gdi::Cache = &self.cache;
        unsafe {
            GdipSetSmoothingMode(self.g, gdi::SMOOTH_ANTI_ALIAS);
            GdipSetTextRenderingHint(self.g, gdi::text_hint());
        }
        let p = gdi::Painter {
            g: self.g,
            cache: cache_ptr,
            sf: self.sf,
            w: self.w,
            h: self.h,
            dc: self.mem_dc,
            scan0: self.scan0,
        };
        self.paint(&p);
        self.ulw();
    }

    fn ulw(&self) {
        unsafe {
            let mut r: RECT = std::mem::zeroed();
            GetWindowRect(self.hwnd as HWND, &mut r);
            let mut ppt = POINT { x: r.left, y: r.top };
            let mut size = winapi::shared::windef::SIZE { cx: (self.w * self.sf) as i32, cy: (self.h * self.sf) as i32 };
            let mut src = POINT { x: 0, y: 0 };
            let mut blend = BLENDFUNCTION {
                BlendOp: 0,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: 1,
            };
            UpdateLayeredWindow(
                self.hwnd as HWND,
                std::ptr::null_mut(),
                &mut ppt,
                &mut size,
                self.mem_dc as winapi::shared::windef::HDC,
                &mut src,
                0,
                &mut blend,
                2,
            );
        }
    }

    fn paint(&mut self, p: &gdi::Painter) {
        use crate::gdi::{HALIGN_CENTER, HALIGN_NEAR};
        let pal = crate::theme::pal();

        p.fill_round(0.0, 0.0, CP_W, CP_H, 12.0, pal.popup);

        // 标题 + 关闭
        p.text("添加城市", 16.0, 10.0, 160.0, 22.0, HALIGN_NEAR, HALIGN_CENTER, 13.5, true, false, pal.title);
        let cr = gdi::RectF { x: CP_W - 30.0, y: 10.0, w: 20.0, h: 20.0 };
        p.text("✕", cr.x, cr.y, cr.w, cr.h, HALIGN_CENTER, HALIGN_CENTER, 11.0, false, false, if self.close_hover { pal.red } else { pal.dim });
        self.close_rect = cr;
        p.fill_rect(16.0, 38.0, CP_W - 32.0, 1.0, crate::theme::ov(20));

        // 输入框
        let inp = gdi::RectF { x: 16.0, y: 46.0, w: CP_W - 32.0, h: 30.0 };
        p.fill_round(inp.x, inp.y, inp.w, inp.h, 6.0, pal.field);
        p.stroke_round(inp.x, inp.y, inp.w, inp.h, 6.0, 1.0, crate::theme::ov(50));
        let tx_x = inp.x + 10.0;
        let tx_w = inp.w - 20.0;
        if self.text.is_empty() && self.comp.is_empty() {
            p.text("输入城市名搜索，如：广州", tx_x, inp.y, tx_w, inp.h, HALIGN_NEAR, HALIGN_CENTER, 12.0, false, false, pal.dim);
            // 空输入也画闪烁光标：提示输入框已获得焦点（与有文字时同一节奏）
            if self.caret_on {
                p.fill_rect(tx_x, inp.y + 8.0, 1.0, inp.h - 16.0, pal.sub);
            }
        } else {
            let shown: String = format!("{}{}", self.text, self.comp);
            let avail = tx_w;
            let full_w = p.measure(&shown, 12.5, false, false).0;
            let overflow = full_w > avail;
            let vis: String = if overflow {
                let mut s: String = shown.chars().rev().collect();
                while p.measure(&s, 12.5, false, false).0 > avail && s.len() > 4 {
                    s.pop();
                }
                s.chars().rev().collect()
            } else {
                shown.clone()
            };
            p.text(&vis, tx_x, inp.y, avail, inp.h, HALIGN_NEAR, HALIGN_CENTER, 12.5, false, false, pal.txt);
            // 光标（按可见文本宽度近似；caret 为 char 计数，须按字符取前缀，
            // 中文多字节下字节切片会 panic）
            if self.caret_on && !overflow {
                let pre: String = format!("{}{}", self.text.chars().take(self.caret).collect::<String>(), &self.comp);
                let cw = p.measure(&pre, 12.5, false, false).0;
                p.fill_rect(tx_x + cw, inp.y + 8.0, 1.0, inp.h - 16.0, pal.sub);
            }
        }

        self.hot_rects.clear();
        self.row_rects.clear();

        if self.text.trim().is_empty() {
            // 热门城市
            p.text("热门城市", 16.0, 84.0, 120.0, 18.0, HALIGN_NEAR, HALIGN_CENTER, 11.0, false, false, pal.sub);
            let cols = 4usize;
            let gap = 8.0;
            let cell_w = (CP_W - 32.0 - gap * (cols as f32 - 1.0)) / cols as f32;
            for (i, name) in HOT_CITIES.iter().enumerate() {
                let (cx, cy) = (16.0 + (i % cols) as f32 * (cell_w + gap), 106.0 + (i / cols) as f32 * 38.0);
                let rect = gdi::RectF { x: cx, y: cy, w: cell_w, h: 30.0 };
                let hov = self.hot_hover == Some(i);
                p.fill_round(rect.x, rect.y, rect.w, rect.h, 6.0, if hov { crate::theme::ov(40) } else { pal.card });
                p.text(name, rect.x, rect.y, rect.w, rect.h, HALIGN_CENTER, HALIGN_CENTER, 12.0, false, false, if hov { pal.txt } else { pal.date });
                self.hot_rects.push(rect);
            }
            p.text(
                "点击城市直接添加，或在上方输入框搜索；最多添加 4 个（含定位共 5 个）",
                16.0,
                272.0,
                CP_W - 32.0,
                18.0,
                HALIGN_NEAR,
                HALIGN_CENTER,
                10.0,
                false,
                false,
                pal.dim,
            );
        } else if self.searching {
            p.text("搜索中…", 16.0, 90.0, CP_W - 32.0, 20.0, HALIGN_CENTER, HALIGN_CENTER, 11.5, false, false, pal.dim);
        } else if self.results.is_empty() {
            p.text("无匹配城市", 16.0, 90.0, CP_W - 32.0, 20.0, HALIGN_CENTER, HALIGN_CENTER, 11.5, false, false, pal.dim);
        } else {
            for (i, hit) in self.results.iter().enumerate() {
                let rect = gdi::RectF { x: 16.0, y: 86.0 + i as f32 * 36.0, w: CP_W - 32.0, h: 34.0 };
                let hov = self.row_hover == Some(i) || self.sel_row == i;
                if hov {
                    p.fill_round(rect.x, rect.y, rect.w, rect.h, 6.0, crate::theme::ov(36));
                }
                p.text(&hit.name, rect.x + 10.0, rect.y, rect.w * 0.55, rect.h, HALIGN_NEAR, HALIGN_CENTER, 12.5, false, false, pal.txt);
                p.text(&hit.province, rect.x + rect.w * 0.55, rect.y, rect.w * 0.45 - 12.0, rect.h, gdi::HALIGN_FAR, HALIGN_CENTER, 10.5, false, false, pal.sub);
                self.row_rects.push(rect);
            }
        }

        // 底部状态
        if !self.status.is_empty() {
            p.text(
                &self.status,
                16.0,
                CP_H - 34.0,
                CP_W - 32.0,
                20.0,
                HALIGN_NEAR,
                HALIGN_CENTER,
                10.5,
                false,
                false,
                if self.status_err { pal.red } else { pal.blue },
            );
        }
    }
}
