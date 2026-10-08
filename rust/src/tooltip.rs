//! 截断文本的悬停提示：单例 NOACTIVATE 分层小窗，跟随鼠标附近显示完整内容。
//! 由 flyout/侧栏在鼠标停于"已截断的行"500ms 后调用 show()，移动/点击/离开时 hide()。
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use winapi::shared::windef::{HWND, POINT};
use winapi::um::winuser::*;

static TIP_HWND: AtomicUsize = AtomicUsize::new(0);
static VISIBLE: AtomicBool = AtomicBool::new(false);
static CLASS_DONE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
static SURF: Mutex<Option<TipSurf>> = Mutex::new(None);
static CACHE: std::sync::OnceLock<crate::gdi::Cache> = std::sync::OnceLock::new();

struct TipSurf {
    sf: f32,
    mem_dc: usize,
    hbmp: usize,
    bmp: usize,
    scan0: usize,
    g: usize,
}

const PAD_X: f32 = 10.0;
const PAD_Y: f32 = 8.0;
const LINE_H: f32 = 18.0;
const MAX_W: f32 = 320.0;
const MAX_LINES: usize = 8;

unsafe extern "system" fn tip_proc(hwnd: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// 文本宽度粗估（中文全宽、ASCII 半宽），仅用于截断判定，不要求精确
pub fn est_width(s: &str, px: f32) -> f32 {
    s.chars().map(|c| if c as u32 > 0x2E7F { px } else { px * 0.55 }).sum()
}

pub fn visible() -> bool {
    VISIBLE.load(Ordering::Relaxed)
}

pub fn hide() {
    if !VISIBLE.swap(false, Ordering::Relaxed) {
        return;
    }
    let h = TIP_HWND.load(Ordering::Relaxed) as HWND;
    if !h.is_null() {
        unsafe {
            ShowWindow(h, SW_HIDE);
        }
    }
}

fn ensure_window() -> usize {
    unsafe {
        CLASS_DONE.get_or_init(|| {
            let class_name: Vec<u16> = "zcal_tooltip\0".encode_utf16().collect();
            let wc = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(tip_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null_mut()),
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null_mut(),
                lpszClassName: class_name.as_ptr(),
            };
            RegisterClassW(&wc);
        });
        let mut h = TIP_HWND.load(Ordering::Relaxed) as HWND;
        if h.is_null() {
            let class_name: Vec<u16> = "zcal_tooltip\0".encode_utf16().collect();
            let title: Vec<u16> = "zcal\0".encode_utf16().collect();
            h = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                0,
                0,
                10,
                10,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                winapi::um::libloaderapi::GetModuleHandleW(std::ptr::null_mut()),
                std::ptr::null_mut(),
            );
            TIP_HWND.store(h as usize, Ordering::Relaxed);
        }
        h as usize
    }
}

/// 在屏幕坐标 (sx, sy) 附近浮出多行文本提示（自动折行，超长截断加省略号）
pub fn show(text: &str, sx: i32, sy: i32) {
    if text.trim().is_empty() {
        return;
    }
    let hwnd = ensure_window();
    if hwnd == 0 {
        return;
    }
    let sf = crate::gdi::scale();
    let pal = crate::theme::pal();

    // 贪心按字符折行（中文无分词边界），超行截断加省略号
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0.0f32;
    let mut truncated = false;
    'outer: for ch in text.chars() {
        let cw = if ch as u32 > 0x2E7F { 12.0 } else { 12.0 * 0.55 };
        if cur_w + cw > MAX_W - PAD_X * 2.0 && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0.0;
            if lines.len() == MAX_LINES {
                truncated = true;
                break 'outer;
            }
        }
        cur.push(ch);
        cur_w += cw;
    }
    if !cur.is_empty() {
        if lines.len() == MAX_LINES {
            truncated = true;
        } else {
            lines.push(cur);
        }
    }
    if truncated {
        if let Some(last) = lines.last_mut() {
            let mut cut = last.clone();
            while est_width(&format!("{}…", cut), 12.0) > MAX_W - PAD_X * 2.0 && !cut.is_empty() {
                cut.pop();
            }
            *last = format!("{}…", cut);
        }
    }

    // 分配/复用绘制表面（上限尺寸一次分配，sf 变化重建）
    {
        let mut guard = SURF.lock().unwrap();
        let stale = match guard.as_ref() {
            Some(t) => t.sf != sf,
            None => true,
        };
        if stale {
            *guard = None;
            let max_h = PAD_Y * 2.0 + LINE_H * MAX_LINES as f32 + 8.0;
            let (mem_dc, hbmp, bmp, scan0) = unsafe { crate::gdi::alloc_dib(MAX_W, max_h) };
            let g = unsafe { crate::gdi::graphics_from_image(bmp) };
            *guard = Some(TipSurf {
                sf,
                mem_dc,
                hbmp,
                bmp: bmp as usize,
                scan0: scan0 as usize,
                g: g as usize,
            });
        }
    }
    let guard = SURF.lock().unwrap();
    let tip = match guard.as_ref() {
        Some(t) => t,
        None => return,
    };
    let cache = CACHE.get_or_init(crate::gdi::Cache::new);
    let p = unsafe {
        crate::gdi::Painter {
            g: tip.g as crate::gdi::Gp,
            cache,
            sf: tip.sf,
            w: MAX_W,
            h: PAD_Y * 2.0 + LINE_H * MAX_LINES as f32 + 8.0,
            dc: tip.mem_dc,
            scan0: tip.scan0 as *mut u8,
        }
    };
    p.clear();

    let mut text_w: f32 = 0.0;
    for line in &lines {
        text_w = text_w.max(p.measure(line, 12.0, false, false).0);
    }
    let w = (text_w + PAD_X * 2.0 + 2.0).min(MAX_W);
    let h = PAD_Y * 2.0 + LINE_H * lines.len() as f32;
    p.fill_round(0.5, 0.5, w - 1.0, h - 1.0, 8.0, pal.popup);
    for (i, line) in lines.iter().enumerate() {
        let y = PAD_Y + LINE_H * i as f32;
        p.text(line, PAD_X, y, MAX_W - PAD_X * 2.0, LINE_H, crate::gdi::HALIGN_NEAR, crate::gdi::HALIGN_CENTER, 12.0, false, false, pal.row);
    }
    p.stroke_round(0.5, 0.5, w - 1.0, h - 1.0, 8.0, 1.0, crate::theme::ov(70));

    // ULW 到窗口并显示（不抢焦点）；位置按鼠标所在显示器工作区夹回
    let pw = (w * sf) as i32;
    let ph = (h * sf) as i32;
    unsafe {
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        let pt = POINT { x: sx, y: sy };
        GetMonitorInfoW(MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST), &mut mi);
        let mut x = sx + 16;
        let mut y = sy + 22;
        if x + pw > mi.rcWork.right {
            x = mi.rcWork.right - pw;
        }
        if y + ph > mi.rcWork.bottom {
            y = sy - ph - 12;
        }
        if y < mi.rcWork.top {
            y = mi.rcWork.top;
        }
        let mut ppt = POINT { x, y };
        let mut size = winapi::shared::windef::SIZE { cx: pw, cy: ph };
        let mut src = POINT { x: 0, y: 0 };
        let mut blend = winapi::um::wingdi::BLENDFUNCTION {
            BlendOp: 0,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: 1,
        };
        UpdateLayeredWindow(
            hwnd as HWND,
            std::ptr::null_mut(),
            &mut ppt,
            &mut size,
            tip.mem_dc as winapi::shared::windef::HDC,
            &mut src,
            0,
            &mut blend,
            2,
        );
        ShowWindow(hwnd as HWND, SW_SHOWNOACTIVATE);
    }
    VISIBLE.store(true, Ordering::Relaxed);
}
