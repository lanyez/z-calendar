//! 右下角提醒弹窗：NOACTIVATE 分层窗口，不抢焦点，10 秒自动消失。
//! 卡片底部带操作按钮：待办提醒可「完成」；所有提醒可「稍后10分钟」（snooze.json 登记，
//! 提醒线程到点补发）。点击卡片其他区域打开日历。
//! 独立线程拥有窗口；提醒引擎通过 channel 投递 ToastMsg。

use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Mutex, OnceLock};

use crate::gdi::{Cache, Painter};
use crate::gdi;
use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
use winapi::shared::windef::{HWND, POINT, RECT, SIZE};
use winapi::um::wingdi::{BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, CreateCompatibleDC, CreateDIBSection, SelectObject};
use winapi::um::winuser::*;

const TOAST_W: f32 = 300.0;
const ITEM_H: f32 = 82.0;
const GAP: f32 = 10.0;
const MAX_ITEMS: usize = 3;
const SHOW_MS: i64 = 10_000;
const MARGIN: i32 = 12;
/// 稍后提醒的间隔（毫秒）
const SNOOZE_MS: i64 = 10 * 60 * 1000;

fn BG() -> u32 { crate::theme::pal().toast_bg }
fn BORDER() -> u32 { crate::theme::ov(40) }
fn BLUE() -> u32 { crate::theme::pal().blue }
const WHITE: u32 = gdi::argb(255, 255, 255, 255);
fn SUB() -> u32 { crate::theme::ov(170) }
fn ON_BG() -> u32 { crate::theme::pal().on_bg }

/// 卡片附带动作：待办提醒可一键完成（按 id 定位；重复待办按日子记录完成）；
/// 删除操作可撤销（快照数据随卡片携带）
#[derive(Clone, serde::Serialize, serde::Deserialize, Default)]
#[serde(tag = "k", content = "v")]
pub enum Act {
    #[default]
    None,
    TodoDone {
        id: String,
        date: String,
        recur: bool,
    },
    /// 撤销删除：携带被删条目的快照，点「撤销」原位恢复
    Undo {
        data: UndoData,
    },
}

/// 删除快照：日程（原 key/下标 + 条目）或待办（原全局下标 + 条目，可批量）
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "k", content = "v")]
pub enum UndoData {
    Agenda {
        key: String,
        idx: usize,
        entry: crate::events::AgendaEntry,
    },
    Todos {
        items: Vec<(usize, crate::sidebar::Todo)>,
    },
}

/// 提醒投递消息（quiet=true 不播提示音，如撤销卡片）
#[derive(Clone, Default)]
pub struct ToastMsg {
    pub title: String,
    pub body: String,
    pub act: Act,
    pub quiet: bool,
}

impl ToastMsg {
    pub fn plain(title: &str, body: &str) -> Self {
        ToastMsg { title: title.into(), body: body.into(), act: Act::None, quiet: false }
    }

    /// 删除撤销卡片（无提示音）
    pub fn undo(data: UndoData, body: &str) -> Self {
        ToastMsg { title: "已删除".into(), body: body.into(), act: Act::Undo { data }, quiet: true }
    }
}

/// 稍后提醒登记（snooze.json）：到点由提醒线程补发
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SnoozeEntry {
    /// 触发时点（epoch 毫秒）
    pub t: i64,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub act: Act,
}

/// 稍后提醒间隔（通知中心激活补提用）
pub const SNOOZE_MS_PUB: i64 = SNOOZE_MS;

pub fn now_ms_pub() -> i64 {
    now_ms()
}

/// 登记一条稍后提醒（通知中心「稍后10分钟」按钮与内置卡片共用）
pub fn snooze_add_entry(entry: SnoozeEntry) {
    snooze_add(entry);
}

fn snooze_path() -> std::path::PathBuf {
    crate::config::data_dir().join("snooze.json")
}

fn snooze_add(entry: SnoozeEntry) {
    let mut list: Vec<SnoozeEntry> = std::fs::read_to_string(snooze_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    list.push(entry);
    if let Ok(text) = serde_json::to_string(&list) {
        let _ = std::fs::write(snooze_path(), text);
    }
}

struct Item {
    title: String,
    body: String,
    act: Act,
    quiet: bool,
    born: i64,
}

impl Item {
    /// 待办完成按钮（Act::TodoDone 才有）
    fn has_done(&self) -> bool {
        matches!(self.act, Act::TodoDone { .. })
    }

    /// 撤销删除按钮（Act::Undo 才有）
    fn has_undo(&self) -> bool {
        matches!(self.act, Act::Undo { .. })
    }
}

// 按钮几何（与 paint_item 保持一致）
const BTN_H: f32 = 20.0;
const BTN_Y: f32 = 56.0;
const SNOOZE_W: f32 = 78.0;
const DONE_W: f32 = 48.0;
const UNDO_W: f32 = 60.0;

fn snooze_rect(item_y: f32) -> (f32, f32, f32, f32) {
    (TOAST_W - 14.0 - SNOOZE_W, item_y + BTN_Y, SNOOZE_W, BTN_H)
}
fn done_rect(item_y: f32) -> (f32, f32, f32, f32) {
    (TOAST_W - 14.0 - SNOOZE_W - 8.0 - DONE_W, item_y + BTN_Y, DONE_W, BTN_H)
}
fn undo_rect(item_y: f32) -> (f32, f32, f32, f32) {
    (TOAST_W - 14.0 - UNDO_W, item_y + BTN_Y, UNDO_W, BTN_H)
}

struct ToastUi {
    hwnd: usize,
    mem_dc: usize,
    /// 后台 DIBSection 像素指针（GDI ClearType 文本路径用）
    scan0: *mut u8,
    bmp: gdi::Gp,
    g: gdi::Gp,
    cache: Cache,
    items: Vec<Item>,
    /// 悬停按钮：(条目下标, 1=稍后 2=完成)
    hover_btn: Option<(usize, u8)>,
}

struct SendToast(Box<ToastUi>);
unsafe impl Send for SendToast {}

/// 仅在 toast 线程访问（事件循环与窗口过程同线程）
static TOAST_UI: Mutex<Option<SendToast>> = Mutex::new(None);

/// 全局投递口（spawn 时登记）；数据自愈等任意线程可发通知
static TOAST_TX: OnceLock<Sender<ToastMsg>> = OnceLock::new();

/// 发一条无动作的提示（数据恢复等系统通知）
pub fn notify(title: &str, body: &str) {
    if let Some(tx) = TOAST_TX.get() {
        let _ = tx.send(ToastMsg::plain(title, body));
    }
}

/// 发一条删除撤销卡片（10 秒内可点「撤销」恢复）
pub fn notify_undo(data: UndoData, body: &str) {
    if let Some(tx) = TOAST_TX.get() {
        let _ = tx.send(ToastMsg::undo(data, body));
    }
}

/// 撤销删除：按快照把日程/待办原位插回并保存、刷新界面
fn undo_restore(data: &UndoData) {
    match data {
        UndoData::Agenda { key, idx, entry } => {
            if let Some(agenda) = crate::events::shared_agenda() {
                let mut map = agenda.lock().unwrap();
                let v = map.entry(key.clone()).or_default();
                let pos = (*idx).min(v.len());
                v.insert(pos, entry.clone());
                crate::events::save(&map);
            }
        }
        UndoData::Todos { items } => {
            crate::sidebar::restore_todos(items);
        }
    }
    crate::sidebar::sidebar_repaint();
    crate::flyout::flyout_repaint();
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 启动弹窗线程，返回提醒投递口
pub fn spawn() -> Sender<ToastMsg> {
    let (tx, rx) = std::sync::mpsc::channel::<ToastMsg>();
    let _ = TOAST_TX.set(tx.clone());
    let _ = std::thread::Builder::new()
        .name("toast".into())
        .stack_size(256 * 1024)
        .spawn(move || unsafe {
            run_loop(rx);
        });
    tx
}

/// 屏幕缩放变化：可见时按新 sf 重摆重绘（toast 的绘制/尺寸均实时读 gdi::scale()）
pub fn rescale() {
    let mut guard = TOAST_UI.lock().unwrap();
    if let Some(f) = guard.as_mut() {
        let f = &mut f.0;
        if !f.items.is_empty() {
            unsafe {
                position(f.hwnd as HWND, f.items.len());
            }
            repaint(f);
        }
    }
}

unsafe fn run_loop(rx: Receiver<ToastMsg>) {
    let hinstance = winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null_mut());
    let cls = crate::wide("z-calendar-toast");
    let mut wc: WNDCLASSW = std::mem::zeroed();
    wc.lpfnWndProc = Some(wndproc);
    wc.hInstance = hinstance;
    wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
    wc.lpszClassName = cls.as_ptr();
    RegisterClassW(&wc);

    let max_h = gdi::phys(ITEM_H * MAX_ITEMS as f32 + GAP * (MAX_ITEMS - 1) as f32) as i32;
    let hwnd = CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_NOACTIVATE,
        cls.as_ptr(),
        crate::wide("Z日历提醒").as_ptr(),
        WS_POPUP,
        32000,
        32000,
        gdi::phys(TOAST_W) as i32,
        max_h,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        hinstance,
        std::ptr::null_mut(),
    );
    if hwnd.is_null() {
        return;
    }

    let hdc = GetDC(std::ptr::null_mut());
    let mem_dc = CreateCompatibleDC(hdc) as usize;
    let mut bmi: BITMAPINFO = std::mem::zeroed();
    bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = gdi::phys(TOAST_W) as i32;
    bmi.bmiHeader.biHeight = -max_h;
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB;
    let mut bits: *mut winapi::ctypes::c_void = std::ptr::null_mut();
    let hbmp = CreateDIBSection(hdc, &bmi, 0, &mut bits, std::ptr::null_mut(), 0);
    SelectObject(mem_dc as winapi::shared::windef::HDC, hbmp as winapi::shared::windef::HGDIOBJ);
    ReleaseDC(std::ptr::null_mut(), hdc);
    let mut bmp: gdi::Gp = std::ptr::null_mut();
    GdipCreateBitmapFromScan0(gdi::phys(TOAST_W) as i32, max_h, gdi::phys(TOAST_W) as i32 * 4, gdi::PIXEL_FORMAT_32BPP_PARGB, bits as *mut u8, &mut bmp);
    let mut g: gdi::Gp = std::ptr::null_mut();
    GdipGetImageGraphicsContext(bmp, &mut g);

    *TOAST_UI.lock().unwrap() = Some(SendToast(Box::new(ToastUi {
        hwnd: hwnd as usize,
        mem_dc,
        scan0: bits as *mut u8,
        bmp,
        g,
        cache: Cache::new(),
        items: Vec::new(),
        hover_btn: None,
    })));

    let mut shown = false;
    let mut msg: MSG = std::mem::zeroed();
    loop {
        // 收取提醒引擎投递 + 过期清理，有变化才重摆/重绘
        let mut dirty = false;
        let mut pushed_loud = 0usize;
        {
            let mut guard = TOAST_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                let f = &mut f.0;
                while let Ok(m) = rx.try_recv() {
                    if !m.quiet {
                        pushed_loud += 1;
                    }
                    f.items.push(Item { title: m.title, body: m.body, act: m.act, quiet: m.quiet, born: now_ms() });
                    if f.items.len() > MAX_ITEMS {
                        f.items.remove(0);
                    }
                    dirty = true;
                }
                let n0 = f.items.len();
                let now = now_ms();
                f.items.retain(|it| now - it.born < SHOW_MS);
                if f.items.len() != n0 {
                    dirty = true;
                }
                if f.items.is_empty() {
                    if shown {
                        ShowWindow(hwnd as HWND, SW_HIDE);
                        shown = false;
                        crate::trim_working_set();
                    }
                } else {
                    if pushed_loud > 0 && crate::config::remind_sound_on() {
                        play_alert();
                    }
                    position(hwnd as HWND, f.items.len());
                    ShowWindow(hwnd as HWND, SW_SHOWNOACTIVATE);
                    shown = true;
                    repaint(f);
                }
            }
        }
        // 等消息或超时（250ms 粒度驱动过期清理）
        MsgWaitForMultipleObjectsEx(0, std::ptr::null(), 250, QS_ALLINPUT, 0);
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// 移除一条卡片（按钮点击后）：空了收起窗口，否则重摆重绘
unsafe fn remove_item(idx: usize) {
    let empty;
    {
        let mut guard = TOAST_UI.lock().unwrap();
        let Some(f) = guard.as_mut() else { return };
        let f = &mut f.0;
        if idx >= f.items.len() {
            return;
        }
        f.items.remove(idx);
        f.hover_btn = None;
        empty = f.items.is_empty();
        if !empty {
            position(f.hwnd as HWND, f.items.len());
            repaint(f);
        }
    }
    if empty {
        ShowWindow(hwnd_of(), SW_HIDE);
        crate::trim_working_set();
    }
}

unsafe fn hwnd_of() -> HWND {
    TOAST_UI
        .lock()
        .unwrap()
        .as_ref()
        .map(|f| f.0.hwnd as HWND)
        .unwrap_or(std::ptr::null_mut())
}

/// 播放提醒提示音（系统“感叹号”音；异步不阻塞弹窗线程）
fn play_alert() {
    use winapi::um::playsoundapi::{PlaySoundW, SND_ALIAS, SND_ASYNC};
    let alias = crate::wide("SystemExclamation");
    unsafe {
        PlaySoundW(alias.as_ptr(), std::ptr::null_mut(), SND_ALIAS | SND_ASYNC);
    }
}

/// 摆放到主屏工作区右下角（工作区为物理像素，尺寸/边距按 sf 换算）
unsafe fn position(hwnd: HWND, n: usize) {
    let mut wa: RECT = std::mem::zeroed();
    SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut RECT as *mut winapi::ctypes::c_void, 0);
    let h = gdi::phys(ITEM_H * n as f32 + GAP * n.saturating_sub(1) as f32) as i32;
    let x = wa.right - gdi::phys(TOAST_W) as i32 - gdi::phys(MARGIN as f32) as i32;
    let y = wa.bottom - h - gdi::phys(MARGIN as f32) as i32;
    SetWindowPos(hwnd, HWND_TOPMOST, x, y, gdi::phys(TOAST_W) as i32, h, SWP_NOACTIVATE);
}

fn repaint(f: &mut ToastUi) {
    unsafe {
        if f.g.is_null() {
            GdipGetImageGraphicsContext(f.bmp, &mut f.g);
        }
        let cache_ptr: *const Cache = &f.cache;
        GdipSetSmoothingMode(f.g, gdi::SMOOTH_ANTI_ALIAS);
        GdipSetTextRenderingHint(f.g, gdi::text_hint());
        let p = Painter { g: f.g, cache: cache_ptr, sf: gdi::scale(), w: TOAST_W, h: ITEM_H * MAX_ITEMS as f32 + GAP * (MAX_ITEMS - 1) as f32, dc: f.mem_dc, scan0: f.scan0 };
        p.clear();
        let n = f.items.len();
        for (i, it) in f.items.iter().enumerate() {
            paint_item(&p, (ITEM_H + GAP) * i as f32, it, f.hover_btn.filter(|(hi, _)| *hi == i).map(|(_, b)| b));
        }
        let mut r: RECT = std::mem::zeroed();
        GetWindowRect(f.hwnd as HWND, &mut r);
        let h = gdi::phys(ITEM_H * n as f32 + GAP * n.saturating_sub(1) as f32) as i32;
        let mut ppt = POINT { x: r.left, y: r.top };
        let mut size = SIZE { cx: gdi::phys(TOAST_W) as i32, cy: h };
        let mut src = POINT { x: 0, y: 0 };
        let mut blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: 1 };
        UpdateLayeredWindow(
            f.hwnd as HWND,
            std::ptr::null_mut(),
            &mut ppt,
            &mut size,
            f.mem_dc as winapi::shared::windef::HDC,
            &mut src,
            0,
            &mut blend,
            2,
        );
    }
}

fn paint_pill(p: &Painter, rect: (f32, f32, f32, f32), label: &str, hov: bool) {
    let (x, y, w, h) = rect;
    p.fill_round(x, y, w, h, h / 2.0, if hov { crate::theme::ov(46) } else { crate::theme::ov(20) });
    p.stroke_round(x, y, w, h, h / 2.0, 1.0, BORDER());
    p.text(label, x, y, w, h, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 10.5, false, false, if hov { ON_BG() } else { SUB() });
}

fn paint_item(p: &Painter, y: f32, it: &Item, hover_btn: Option<u8>) {
    p.fill_round(0.0, y, TOAST_W, ITEM_H, 10.0, BG());
    p.stroke_round(0.5, y + 0.5, TOAST_W - 1.0, ITEM_H - 1.0, 10.0, 1.0, BORDER());
    // 铃铛图标
    p.fill_circle(28.0, y + ITEM_H / 2.0, 14.0, gdi::argb(60, 62, 135, 250));
    p.text("\u{E7E7}", 14.0, y + ITEM_H / 2.0 - 14.0, 28.0, 28.0, gdi::HALIGN_CENTER, gdi::HALIGN_CENTER, 13.0, false, true, BLUE());
    // 标题 + 正文（最多两行，逐行绘制；第二行放不下以省略号收尾）
    p.text(&it.title, 52.0, y + 8.0, TOAST_W - 66.0, 16.0, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 12.5, true, false, ON_BG());
    let max_w = TOAST_W - 66.0 - 8.0;
    let lines = wrap_two(p, &it.body, max_w);
    let lh = 15.0;
    let y0 = y + 25.0 + (36.0 - lh * lines.len() as f32) / 2.0;
    for (i, line) in lines.iter().enumerate() {
        p.text(line, 52.0, y0 + i as f32 * lh, TOAST_W - 66.0, lh, gdi::HALIGN_NEAR, gdi::HALIGN_CENTER, 11.0, false, false, SUB());
    }
    // 操作按钮：撤销卡片 = 撤销；待办 = 完成 + 稍后；其他 = 稍后
    if it.has_undo() {
        paint_pill(p, undo_rect(y), "撤销", hover_btn == Some(3));
    } else if it.has_done() {
        paint_pill(p, done_rect(y), "完成", hover_btn == Some(2));
        paint_pill(p, snooze_rect(y), "稍后10分钟", hover_btn == Some(1));
    } else {
        paint_pill(p, snooze_rect(y), "稍后10分钟", hover_btn == Some(1));
    }
}

/// 正文折行：最多两行，第二行放不下时回退并以省略号收尾（逐行返回，绘制时逐行画）
fn wrap_two(p: &Painter, body: &str, max_w: f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in body.chars() {
        cur.push(ch);
        if p.measure(&cur, 11.0, false, false).0 > max_w {
            cur.pop();
            if lines.len() == 1 {
                // 第二行溢出：截到省略号放得下为止
                while !cur.is_empty() && p.measure(&format!("{}…", cur), 11.0, false, false).0 > max_w {
                    cur.pop();
                }
                cur.push('…');
                lines.push(cur);
                return lines;
            }
            if cur.is_empty() {
                cur.push(ch); // 单字符超宽（不可能出现）：原样保留避免死循环
            }
            lines.push(std::mem::take(&mut cur));
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            ValidateRect(hwnd, std::ptr::null_mut());
            0
        }
        WM_ERASEBKGND => 1,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_MOUSEMOVE => {
            let s = gdi::scale();
            let (x, y) = (x_of(lp) as f32 / s, y_of(lp) as f32 / s);
            let mut hover_btn = None;
            {
                let guard = TOAST_UI.lock().unwrap();
                if let Some(f) = guard.as_ref() {
                    let f = &f.0;
                    for (i, it) in f.items.iter().enumerate() {
                        let iy = (ITEM_H + GAP) * i as f32;
                        if y >= iy && y < iy + ITEM_H {
                            let (sx, sy, sw, sh) = snooze_rect(iy);
                            if x >= sx && x < sx + sw && y >= sy && y < sy + sh {
                                hover_btn = Some((i, 1));
                            } else if it.has_undo() {
                                let (ux, uy, uw, uh) = undo_rect(iy);
                                if x >= ux && x < ux + uw && y >= uy && y < uy + uh {
                                    hover_btn = Some((i, 3));
                                }
                            } else if it.has_done() {
                                let (dx, dy, dw, dh) = done_rect(iy);
                                if x >= dx && x < dx + dw && y >= dy && y < dy + dh {
                                    hover_btn = Some((i, 2));
                                }
                            }
                            break;
                        }
                    }
                }
            }
            {
                let mut guard = TOAST_UI.lock().unwrap();
                if let Some(f) = guard.as_mut() {
                    if f.0.hover_btn != hover_btn {
                        f.0.hover_btn = hover_btn;
                        repaint(&mut f.0);
                    }
                }
            }
            SetCursor(LoadCursorW(std::ptr::null_mut(), if hover_btn.is_some() { IDC_HAND } else { IDC_ARROW }));
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut tme);
            0
        }
        WM_MOUSELEAVE => {
            let mut guard = TOAST_UI.lock().unwrap();
            if let Some(f) = guard.as_mut() {
                if f.0.hover_btn.is_some() {
                    f.0.hover_btn = None;
                    repaint(&mut f.0);
                }
            }
            0
        }
        WM_LBUTTONDOWN => {
            let s = gdi::scale();
            let (x, y) = (x_of(lp) as f32 / s, y_of(lp) as f32 / s);
            // 命中判定：先按钮，后卡片本体
            let mut hit: Option<(usize, u8)> = None; // 1=稍后 2=完成 3=撤销 0=本体
            {
                let guard = TOAST_UI.lock().unwrap();
                if let Some(f) = guard.as_ref() {
                    let f = &f.0;
                    for (i, it) in f.items.iter().enumerate() {
                        let iy = (ITEM_H + GAP) * i as f32;
                        if y >= iy && y < iy + ITEM_H {
                            let (sx, sy, sw, sh) = snooze_rect(iy);
                            if x >= sx && x < sx + sw && y >= sy && y < sy + sh {
                                hit = Some((i, 1));
                            } else if it.has_undo() {
                                let (ux, uy, uw, uh) = undo_rect(iy);
                                if x >= ux && x < ux + uw && y >= uy && y < uy + uh {
                                    hit = Some((i, 3));
                                }
                            } else if it.has_done() {
                                let (dx, dy, dw, dh) = done_rect(iy);
                                if x >= dx && x < dx + dw && y >= dy && y < dy + dh {
                                    hit = Some((i, 2));
                                }
                            } else {
                                hit = Some((i, 0));
                            }
                            break;
                        }
                    }
                }
            }
            if let Some((i, btn)) = hit {
                let item = TOAST_UI.lock().unwrap().as_ref().map(|f| {
                    let it = &f.0.items[i];
                    (it.title.clone(), it.body.clone(), it.act.clone(), it.quiet)
                });
                if let Some((title, body, act, quiet)) = item {
                    match btn {
                        1 => {
                            // 稍后10分钟：登记后由提醒线程到点补发
                            snooze_add(SnoozeEntry { t: now_ms() + SNOOZE_MS, title, body, act });
                            unsafe { remove_item(i) };
                        }
                        2 => {
                            if let Act::TodoDone { id, date, .. } = act {
                                crate::sidebar::complete_todo_by_id(&id, &date);
                            }
                            unsafe { remove_item(i) };
                        }
                        3 => {
                            // 撤销删除：按快照原位恢复
                            if let Act::Undo { data } = act {
                                undo_restore(&data);
                            }
                            unsafe { remove_item(i) };
                        }
                        _ => {
                            // 点击卡片本体：关闭提醒并打开日历（撤销卡片只关闭）
                            unsafe { remove_item(i) };
                            if !quiet {
                                crate::flyout::request_show();
                            }
                        }
                    }
                }
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn x_of(lp: LPARAM) -> i32 {
    ((lp as usize) & 0xFFFF) as u16 as i16 as i32
}
fn y_of(lp: LPARAM) -> i32 {
    (((lp as usize) >> 16) as u16 as i16) as i32
}

#[link(name = "gdiplus")]
extern "system" {
    fn GdipCreateBitmapFromScan0(w: i32, h: i32, stride: i32, format: i32, scan0: *mut u8, bitmap: *mut gdi::Gp) -> i32;
    fn GdipGetImageGraphicsContext(image: gdi::Gp, graphics: *mut gdi::Gp) -> i32;
    fn GdipSetSmoothingMode(graphics: gdi::Gp, mode: i32) -> i32;
    fn GdipSetTextRenderingHint(graphics: gdi::Gp, mode: i32) -> i32;
}
