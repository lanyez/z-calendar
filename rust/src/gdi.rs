//! GDI+ 平面 API 封装（最小子集）+ 绘制助手
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

pub type Gp = *mut winapi::ctypes::c_void;

pub const PIXEL_FORMAT_32BPP_PARGB: i32 = 0xE200B;
pub const UNIT_PIXEL: i32 = 2;
pub const SMOOTH_ANTI_ALIAS: i32 = 4;
// TextRenderingHint（GDI+ 枚举原值）：1=无抗锯齿 3=灰度抗锯齿+网格对齐
// 4=纯灰度抗锯齿（无网格对齐，文字发虚） 5=ClearType 子像素（最锐利）
// 注意：从内存位图创建的 Graphics 上 GDI+ 一律回退为灰度抗锯齿（含 ClearType），
// 锐利与否取决于进程是否 DPI 感知（未感知会被 DWM 整窗拉伸发虚）
pub const TEXT_HINT_AA_GRID_FIT: i32 = 3;
pub const TEXT_HINT_CLEAR_TYPE: i32 = 5;
pub const FONT_STYLE_NORMAL: i32 = 0;
pub const FONT_STYLE_BOLD: i32 = 1;
pub const HALIGN_NEAR: i32 = 0;
pub const HALIGN_CENTER: i32 = 1;
pub const HALIGN_FAR: i32 = 2;

static SCALE: AtomicU32 = AtomicU32::new(0); // ×1000；0 = 未初始化
static OVERRIDE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// 全局缩放系数（逻辑 px → 物理 px）。进程为 Per-Monitor V2 感知；屏幕缩放
/// 变化时由 flyout 的轮询经 set_scale 更新，所有窗口随后重建/重摆。
/// 调试钩子 CAL_SF=1.25/1.5/2 可强制覆盖（覆盖时不参与缩放轮询）。
pub fn scale() -> f32 {
    let v = SCALE.load(Ordering::Relaxed);
    if v != 0 {
        return v as f32 / 1000.0;
    }
    let s = unsafe { detect_scale() };
    set_scale(s);
    s
}

/// 调试覆盖 CAL_SF=… 生效时为 true：缩放轮询停用，避免真实 DPI 覆盖调试值
pub fn scale_overridden() -> bool {
    *OVERRIDE.get_or_init(|| std::env::var("CAL_SF").is_ok())
}

pub fn set_scale(s: f32) {
    SCALE.store((s * 1000.0).round().max(1.0) as u32, Ordering::Relaxed);
}

/// 主屏当前有效缩放（真实值，shcore!GetDpiForMonitor 实时反映屏幕设置）。
/// 注意 System-Aware 进程的 GetDeviceCaps(LOGPIXELSX) 固定在登录时的 DPI，
/// 改缩放后拿不到新值，故探测优先走 GetDpiForMonitor。
pub fn primary_scale() -> f32 {
    if scale_overridden() {
        return scale();
    }
    unsafe { detect_scale() }
}

unsafe fn detect_scale() -> f32 {
    if let Ok(v) = std::env::var("CAL_SF") {
        if let Ok(f) = v.parse::<f32>() {
            if f > 0.0 {
                return f;
            }
        }
    }
    #[link(name = "shcore")]
    extern "system" {
        fn GetDpiForMonitor(
            hmonitor: winapi::shared::windef::HMONITOR,
            dpi_type: u32,
            dpi_x: *mut u32,
            dpi_y: *mut u32,
        ) -> i32;
    }
    use winapi::shared::windef::POINT;
    use winapi::um::wingdi::{GetDeviceCaps, LOGPIXELSX};
    use winapi::um::winuser::{MonitorFromPoint, MONITOR_DEFAULTTOPRIMARY};
    let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
    if !mon.is_null() {
        let (mut dx, mut dy) = (0u32, 0u32);
        if GetDpiForMonitor(mon, 0 /*MDT_EFFECTIVE_DPI*/, &mut dx, &mut dy) == 0 && dx > 0 {
            return dx as f32 / 96.0;
        }
    }
    let hdc = GetDC(std::ptr::null_mut());
    let dpi = if hdc.is_null() { 96 } else { GetDeviceCaps(hdc, LOGPIXELSX) };
    if !hdc.is_null() {
        ReleaseDC(std::ptr::null_mut(), hdc);
    }
    if dpi > 0 { dpi as f32 / 96.0 } else { 1.0 }
}

/// 逻辑坐标 → 物理像素
pub fn phys(v: f32) -> f32 {
    v * scale()
}

// ---------- 后台位图生命周期（隐藏时释放以压缩提交内存，显示时重建） ----------

/// 为分层窗口分配后台位图（物理尺寸 = 逻辑 × sf）：
/// 内存 DC + 32bpp DIBSection + 绑定 scan0 的 GDI+ Bitmap。
/// 返回 (mem_dc, hbmp, bmp, scan0)；Graphics 由调用方 GdipGetImageGraphicsContext 获取。
pub unsafe fn alloc_dib(logical_w: f32, logical_h: f32) -> (usize, usize, Gp, *mut u8) {
    use winapi::shared::windef::HDC;
    use winapi::um::wingdi::{CreateCompatibleDC, CreateDIBSection, SelectObject, BI_RGB, BITMAPINFO, BITMAPINFOHEADER};
    let w_px = phys(logical_w) as i32;
    let h_px = phys(logical_h) as i32;
    let hdc = GetDC(std::ptr::null_mut());
    let mem_dc = CreateCompatibleDC(hdc) as usize;
    let mut bmi: BITMAPINFO = std::mem::zeroed();
    bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = w_px;
    bmi.bmiHeader.biHeight = -h_px;
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB;
    let mut bits: *mut winapi::ctypes::c_void = std::ptr::null_mut();
    let hbmp = CreateDIBSection(hdc, &bmi, 0, &mut bits, std::ptr::null_mut(), 0);
    SelectObject(mem_dc as HDC, hbmp as *mut winapi::ctypes::c_void);
    ReleaseDC(std::ptr::null_mut(), hdc);
    let mut bmp: Gp = std::ptr::null_mut();
    GdipCreateBitmapFromScan0(w_px, h_px, w_px * 4, PIXEL_FORMAT_32BPP_PARGB, bits as *mut u8, &mut bmp);
    (mem_dc, hbmp as usize, bmp, bits as *mut u8)
}

/// 释放后台位图。幂等；显示后 redraw 前需重新 alloc_dib 并重新获取 Graphics。
/// hbmp 必须记录：DIBSection 句柄不会随 DC 删除，漏删会在每次显示/隐藏循环中泄漏。
pub unsafe fn free_dib(mem_dc: &mut usize, hbmp: &mut usize, bmp: &mut Gp, g: &mut Gp, scan0: &mut *mut u8) {
    if !g.is_null() {
        GdipDeleteGraphics(*g);
        *g = std::ptr::null_mut();
    }
    if !bmp.is_null() {
        GdipDisposeImage(*bmp);
        *bmp = std::ptr::null_mut();
    }
    if *mem_dc != 0 {
        DeleteDC(*mem_dc as winapi::shared::windef::HDC);
        *mem_dc = 0;
    }
    if *hbmp != 0 {
        DeleteObject(*hbmp as *mut winapi::ctypes::c_void);
        *hbmp = 0;
    }
    *scan0 = std::ptr::null_mut();
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateCompatibleDC(hdc: winapi::shared::windef::HDC) -> winapi::shared::windef::HDC;
    fn DeleteDC(hdc: winapi::shared::windef::HDC) -> i32;
    fn CreateDIBSection(
        hdc: winapi::shared::windef::HDC,
        bmi: *const winapi::um::wingdi::BITMAPINFO,
        usage: u32,
        bits: *mut *mut winapi::ctypes::c_void,
        section: winapi::shared::ntdef::HANDLE,
        offset: u32,
    ) -> winapi::shared::windef::HBITMAP;
    fn SelectObject(hdc: winapi::shared::windef::HDC, obj: *mut winapi::ctypes::c_void) -> *mut winapi::ctypes::c_void;
    fn DeleteObject(obj: *mut winapi::ctypes::c_void) -> i32;
}

#[link(name = "user32")]
extern "system" {
    fn GetDC(hwnd: winapi::shared::windef::HWND) -> winapi::shared::windef::HDC;
    fn ReleaseDC(hwnd: winapi::shared::windef::HWND, hdc: winapi::shared::windef::HDC) -> i32;
}

/// 文本渲染模式，默认 ClearType。调试钩子 CAL_TEXT_HINT=0~5 可覆盖（对比渲染效果用）。
pub fn text_hint() -> i32 {
    static HINT: std::sync::OnceLock<i32> = std::sync::OnceLock::new();
    *HINT.get_or_init(|| {
        std::env::var("CAL_TEXT_HINT")
            .ok()
            .and_then(|v| v.parse::<i32>().ok())
            .filter(|&v| (0..=5).contains(&v))
            .unwrap_or(TEXT_HINT_CLEAR_TYPE)
    })
}

// ---------- 界面字号（设置里 100%/110%/125%）与 GDI ClearType 文本路径 ----------

static TEXT_SCALE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1000); // ×1000

/// 界面字号系数（只作用于正文文本，图标字体不受影响）
pub fn text_scale() -> f32 {
    TEXT_SCALE.load(std::sync::atomic::Ordering::Relaxed) as f32 / 1000.0
}

pub fn set_text_scale(v: f32) {
    let v = v.clamp(0.5, 3.0);
    TEXT_SCALE.store((v * 1000.0).round() as u32, std::sync::atomic::Ordering::Relaxed);
}

/// GDI HFONT 缓存（px 已含缩放与字号系数；进程内按需创建，数量有限不释放）
static GFONTS: std::sync::LazyLock<Mutex<HashMap<i64, usize>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

unsafe fn gdi_font(px: i32, bold: bool) -> usize {
    use winapi::um::wingdi::{
        CreateFontW, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, DEFAULT_CHARSET, FW_BOLD, FW_NORMAL, OUT_DEFAULT_PRECIS,
    };
    let key = ((px as i64) << 1) | bold as i64;
    let mut map = GFONTS.lock().unwrap();
    if let Some(f) = map.get(&key) {
        return *f;
    }
    let face = wide("Microsoft YaHei UI");
    let hfont = CreateFontW(
        -px,
        0,
        0,
        0,
        if bold { FW_BOLD as i32 } else { FW_NORMAL as i32 },
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        0,
        face.as_ptr(),
    );
    let hf = hfont as usize;
    map.insert(key, hf);
    hf
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PointF {
    pub x: f32,
    pub y: f32,
}

#[repr(C)]
struct GdiplusStartupInput {
    version: u32,
    debug_event_callback: Gp,
    suppress_background_thread: i32,
    suppress_external_codecs: i32,
}

#[link(name = "gdiplus")]
extern "system" {
    fn GdiplusStartup(token: *mut usize, input: *const GdiplusStartupInput, output: Gp) -> i32;
    fn GdipCreateFromHDC(hdc: Gp, graphics: *mut Gp) -> i32;
    fn GdipDeleteGraphics(graphics: Gp) -> i32;
    fn GdipGraphicsClear(graphics: Gp, color: u32) -> i32;
    fn GdipSetSmoothingMode(graphics: Gp, mode: i32) -> i32;
    fn GdipSetTextRenderingHint(graphics: Gp, mode: i32) -> i32;
    fn GdipCreateSolidFill(color: u32, brush: *mut Gp) -> i32;
    fn GdipDeleteBrush(brush: Gp) -> i32;
    fn GdipCreatePen1(color: u32, width: f32, unit: i32, pen: *mut Gp) -> i32;
    fn GdipDeletePen(pen: Gp) -> i32;
    fn GdipCreatePath(fill_mode: i32, path: *mut Gp) -> i32;
    fn GdipDeletePath(path: Gp) -> i32;
    fn GdipAddPathArc(path: Gp, x: f32, y: f32, w: f32, h: f32, start: f32, sweep: f32) -> i32;
    fn GdipAddPathLine(path: Gp, x1: f32, y1: f32, x2: f32, y2: f32) -> i32;
    fn GdipCloseFigure(path: Gp) -> i32;
    fn GdipDrawPath(graphics: Gp, pen: Gp, path: Gp) -> i32;
    fn GdipFillPath(graphics: Gp, brush: Gp, path: Gp) -> i32;
    fn GdipFillRectangle(graphics: Gp, brush: Gp, x: f32, y: f32, w: f32, h: f32) -> i32;
    fn GdipDrawLine(graphics: Gp, pen: Gp, x1: f32, y1: f32, x2: f32, y2: f32) -> i32;
    fn GdipFillEllipse(graphics: Gp, brush: Gp, x: f32, y: f32, w: f32, h: f32) -> i32;
    fn GdipDrawEllipse(graphics: Gp, pen: Gp, x: f32, y: f32, w: f32, h: f32) -> i32;
    fn GdipFillPolygon(graphics: Gp, brush: Gp, points: *const PointF, count: i32, fill_mode: i32) -> i32;
    fn GdipCreateFontFamilyFromName(name: *const u16, placeholder: Gp, family: *mut Gp) -> i32;
    fn GdipCreateFont(family: Gp, em_size: f32, style: i32, unit: i32, font: *mut Gp) -> i32;
    fn GdipDeleteFont(font: Gp) -> i32;
    fn GdipCreateStringFormat(attrs: i32, lang: u16, format: *mut Gp) -> i32;
    fn GdipDeleteStringFormat(format: Gp) -> i32;
    fn GdipSetStringFormatAlign(format: Gp, align: i32) -> i32;
    fn GdipSetStringFormatFlags(format: Gp, flags: i32) -> i32;
    fn GdipSetStringFormatTrimming(format: Gp, trimming: i32) -> i32;
    fn GdipSetStringFormatLineAlign(format: Gp, align: i32) -> i32;
    fn GdipDrawString(graphics: Gp, text: *const u16, len: i32, font: Gp, layout: *const RectF, format: Gp, brush: Gp) -> i32;
    fn GdipMeasureString(graphics: Gp, text: *const u16, len: i32, font: Gp, layout: *const RectF, format: Gp, bounding: *mut RectF, codepoints: *mut i32, lines: *mut i32) -> i32;
    fn GdipCreateBitmapFromScan0(w: i32, h: i32, stride: i32, format: i32, scan0: *mut u8, bitmap: *mut Gp) -> i32;
    fn GdipGetImageGraphicsContext(image: Gp, graphics: *mut Gp) -> i32;
    fn GdipDisposeImage(image: Gp) -> i32;
}

pub fn startup() {
    unsafe {
        let input = GdiplusStartupInput {
            version: 1,
            debug_event_callback: std::ptr::null_mut(),
            suppress_background_thread: 0,
            suppress_external_codecs: 0,
        };
        let mut token: usize = 0;
        GdiplusStartup(&mut token, &input, std::ptr::null_mut());
    }
}

pub const fn argb(a: u8, r: u8, g: u8, b: u8) -> u32 {
    ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

thread_local! {
    // 文本绘制的高频小分配复用缓冲（绘制都在主线程）
    static WIDE_SCRATCH: std::cell::RefCell<Vec<u16>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn wide_into(buf: &mut Vec<u16>, s: &str) {
    buf.clear();
    buf.extend(s.encode_utf16());
    buf.push(0);
}

/// GDI+ 资源缓存（画刷/画笔/字体），绘制时按需创建
pub struct Cache {
    brushes: Mutex<HashMap<u32, Gp>>,
    pens: Mutex<HashMap<(u32, u32), Gp>>,
    fonts: Mutex<HashMap<u64, Gp>>,
    family_yahei: Gp,
    family_mdl2: Gp,
    format: Gp,
}

unsafe impl Send for Cache {}
unsafe impl Sync for Cache {}

impl Cache {
    pub fn new() -> Cache {
        unsafe {
            let n1 = wide("Microsoft YaHei UI");
            let mut family_yahei: Gp = std::ptr::null_mut();
            if GdipCreateFontFamilyFromName(n1.as_ptr(), std::ptr::null_mut(), &mut family_yahei) != 0 {
                let n2 = wide("Microsoft YaHei");
                GdipCreateFontFamilyFromName(n2.as_ptr(), std::ptr::null_mut(), &mut family_yahei);
            }
            let n3 = wide("Segoe MDL2 Assets");
            let mut family_mdl2: Gp = std::ptr::null_mut();
            GdipCreateFontFamilyFromName(n3.as_ptr(), std::ptr::null_mut(), &mut family_mdl2);
            let mut format: Gp = std::ptr::null_mut();
            GdipCreateStringFormat(0, 0, &mut format);
            GdipSetStringFormatFlags(format, 0x1000); // NoWrap
            GdipSetStringFormatTrimming(format, 3);   // EllipsisCharacter
            Cache {
                brushes: Mutex::new(HashMap::new()),
                pens: Mutex::new(HashMap::new()),
                fonts: Mutex::new(HashMap::new()),
                family_yahei,
                family_mdl2,
                format,
            }
        }
    }

    unsafe fn brush(&self, color: u32) -> Gp {
        let mut map = self.brushes.lock().unwrap();
        if let Some(b) = map.get(&color) {
            return *b;
        }
        let mut b: Gp = std::ptr::null_mut();
        GdipCreateSolidFill(color, &mut b);
        map.insert(color, b);
        b
    }

    unsafe fn pen(&self, color: u32, width: f32) -> Gp {
        let key = (color, (width * 4.0) as u32);
        let mut map = self.pens.lock().unwrap();
        if let Some(p) = map.get(&key) {
            return *p;
        }
        let mut p: Gp = std::ptr::null_mut();
        GdipCreatePen1(color, width, UNIT_PIXEL, &mut p);
        map.insert(key, p);
        p
    }

    unsafe fn font(&self, px: f32, bold: bool, mdl2: bool) -> Gp {
        let key = ((px * 8.0) as u64) << 8 | (bold as u64) << 4 | (mdl2 as u64);
        let mut map = self.fonts.lock().unwrap();
        if let Some(f) = map.get(&key) {
            return *f;
        }
        let family = if mdl2 { self.family_mdl2 } else { self.family_yahei };
        let mut f: Gp = std::ptr::null_mut();
        GdipCreateFont(
            family,
            px,
            if bold { FONT_STYLE_BOLD } else { FONT_STYLE_NORMAL },
            UNIT_PIXEL,
            &mut f,
        );
        map.insert(key, f);
        f
    }
}

/// 单个分层窗口的绘制器
pub struct Painter {
    pub g: Gp,
    pub cache: *const Cache,
    pub sf: f32,
    pub w: f32,
    pub h: f32,
    /// 后台 DIBSection 的内存 DC 与像素指针（GDI ClearType 文本路径用；0/null 走 GDI+）
    pub dc: usize,
    pub scan0: *mut u8,
}

impl Painter {
    pub fn s(&self, v: f32) -> f32 {
        v * self.sf
    }

    pub fn fill_round(&self, x: f32, y: f32, w: f32, h: f32, radius: f32, color: u32) {
        unsafe {
            let (x, y, w, h, rad) = (self.s(x), self.s(y), self.s(w), self.s(h), self.s(radius));
            let mut path: Gp = std::ptr::null_mut();
            if GdipCreatePath(0, &mut path) != 0 {
                return;
            }
            let d = rad * 2.0;
            GdipAddPathArc(path, x, y, d, d, 180.0, 90.0);
            GdipAddPathArc(path, x + w - d, y, d, d, 270.0, 90.0);
            GdipAddPathArc(path, x + w - d, y + h - d, d, d, 0.0, 90.0);
            GdipAddPathArc(path, x, y + h - d, d, d, 90.0, 90.0);
            GdipAddPathLine(path, x, y + h - rad, x, y + rad);
            GdipFillPath(self.g, (*self.cache).brush(color), path);
            GdipDeletePath(path);
        }
    }

    pub fn stroke_round(&self, x: f32, y: f32, w: f32, h: f32, radius: f32, width: f32, color: u32) {
        unsafe {
            let (x, y, w, h, rad, width) = (self.s(x), self.s(y), self.s(w), self.s(h), self.s(radius), self.s(width));
            let mut path: Gp = std::ptr::null_mut();
            if GdipCreatePath(0, &mut path) != 0 {
                return;
            }
            let d = rad * 2.0;
            GdipAddPathArc(path, x, y, d, d, 180.0, 90.0);
            GdipAddPathArc(path, x + w - d, y, d, d, 270.0, 90.0);
            GdipAddPathArc(path, x + w - d, y + h - d, d, d, 0.0, 90.0);
            GdipAddPathArc(path, x, y + h - d, d, d, 90.0, 90.0);
            GdipAddPathLine(path, x, y + h - rad, x, y + rad);
            GdipDrawPath(self.g, (*self.cache).pen(color, width), path);
            GdipDeletePath(path);
        }
    }

    pub fn fill_rect(&self, x: f32, y: f32, w: f32, h: f32, color: u32) {
        unsafe {
            let (x, y, w, h) = (self.s(x), self.s(y), self.s(w), self.s(h));
            GdipFillRectangle(self.g, (*self.cache).brush(color), x, y, w, h);
        }
    }

    pub fn line(&self, x1: f32, y1: f32, x2: f32, y2: f32, width: f32, color: u32) {
        unsafe {
            GdipDrawLine(
                self.g,
                (*self.cache).pen(color, self.s(width)),
                self.s(x1),
                self.s(y1),
                self.s(x2),
                self.s(y2),
            );
        }
    }

    pub fn fill_circle(&self, cx: f32, cy: f32, r: f32, color: u32) {
        unsafe {
            let (cx, cy, r) = (self.s(cx), self.s(cy), self.s(r));
            GdipFillEllipse(self.g, (*self.cache).brush(color), cx - r, cy - r, r * 2.0, r * 2.0);
        }
    }

    pub fn stroke_circle(&self, cx: f32, cy: f32, r: f32, width: f32, color: u32) {
        unsafe {
            let (cx, cy, r, width) = (self.s(cx), self.s(cy), self.s(r), self.s(width));
            GdipDrawEllipse(self.g, (*self.cache).pen(color, width), cx - r, cy - r, r * 2.0, r * 2.0);
        }
    }

    pub fn fill_polygon(&self, pts: &[(f32, f32)], color: u32) {
        unsafe {
            let pts: Vec<PointF> = pts
                .iter()
                .map(|(x, y)| PointF { x: self.s(*x), y: self.s(*y) })
                .collect();
            GdipFillPolygon(self.g, (*self.cache).brush(color), pts.as_ptr(), pts.len() as i32, 0);
        }
    }

    pub fn text(&self, s: &str, x: f32, y: f32, w: f32, h: f32, halign: i32, valign: i32, px: f32, bold: bool, mdl2: bool, color: u32) {
        // 界面字号系数只作用于正文文本（mdl2 图标字体保持原大）
        let px = px * text_scale();
        // 内存位图上 GDI+ 的 ClearType 被静默回退为灰度；正文改走 GDI DrawTextW 保留子像素渲染。
        // CAL_TEXT_HINT 覆盖时走 GDI+（保留调试 A/B 通道），图标字体也保持 GDI+ 灰度。
        if !mdl2 && self.dc != 0 && !self.scan0.is_null() && text_hint() == TEXT_HINT_CLEAR_TYPE
            && self.text_gdi(s, x, y, w, h, halign, valign, px, bold, color)
        {
            return;
        }
        unsafe {
            let rect = RectF { x: self.s(x), y: self.s(y), w: self.s(w), h: self.s(h) };
            GdipSetStringFormatAlign((*self.cache).format, halign);
            GdipSetStringFormatLineAlign((*self.cache).format, valign);
            WIDE_SCRATCH.with(|b| {
                let mut buf = b.borrow_mut();
                wide_into(&mut buf, s);
                GdipDrawString(
                    self.g,
                    buf.as_ptr(),
                    (buf.len() - 1) as i32,
                    (*self.cache).font(self.s(px), bold, mdl2),
                    &rect,
                    (*self.cache).format,
                    (*self.cache).brush(color),
                );
            });
        }
    }

    /// GDI ClearType 文本路径。成功绘制返回 true。
    /// GDI 写 32bpp DIB 会把字形像素的 alpha 清零：绘制前快照、绘制后按差分把
    /// 被触碰的像素 alpha 置回 255，未触碰像素（含半透明底）原样保留。
    fn text_gdi(&self, s: &str, x: f32, y: f32, w: f32, h: f32, halign: i32, valign: i32, px: f32, bold: bool, color: u32) -> bool {
        use winapi::um::wingdi::{GdiFlush, SelectObject, SetBkMode, SetTextColor, TRANSPARENT};
        use winapi::um::winuser::{DrawTextW, DT_CENTER, DT_END_ELLIPSIS, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER};
        let bw = self.s(self.w) as i32;
        let bh = self.s(self.h) as i32;
        let (rx, ry, rw, rh) = (
            self.s(x).round() as i32,
            self.s(y).round() as i32,
            (self.s(w).ceil() as i32).max(1),
            (self.s(h).ceil() as i32).max(1),
        );
        // 与位图求交（裁剪语义与 GDI+ 一致）
        let (cx0, cy0) = (rx.max(0), ry.max(0));
        let (cx1, cy1) = ((rx + rw).min(bw), (ry + rh).min(bh));
        if cx1 <= cx0 || cy1 <= cy0 {
            return true;
        }
        let px_i = self.s(px).round() as i32;
        if px_i <= 0 {
            return false;
        }
        let stride = bw as usize * 4;
        let rows = (cy1 - cy0) as usize;
        let cols = (cx1 - cx0) as usize;
        let rect_bytes = rows * cols * 4;
        let mut snap = vec![0u8; rect_bytes];
        unsafe {
            let hdc = self.dc as winapi::shared::windef::HDC;
            let hfont = gdi_font(px_i, bold);
            if hfont == 0 {
                return false;
            }
            // 快照
            let base = self.scan0.add(cy0 as usize * stride + cx0 as usize * 4);
            std::ptr::copy_nonoverlapping(base, snap.as_mut_ptr(), rect_bytes);
            let prev = SelectObject(hdc, hfont as *mut winapi::ctypes::c_void);
            SetBkMode(hdc, TRANSPARENT as i32);
            // COLORREF = 0x00BBGGRR，与 GDI+ 的 0xAARRGGBB 字节序相反
            let cr = ((color & 0xFF) << 16) | (color & 0xFF00) | ((color >> 16) & 0xFF);
            SetTextColor(hdc, cr as u32);
            let mut rc = winapi::shared::windef::RECT { left: rx, top: ry, right: rx + rw, bottom: ry + rh };
            let mut fmt = DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS;
            fmt |= match halign {
                HALIGN_CENTER => DT_CENTER,
                HALIGN_FAR => DT_RIGHT,
                _ => 0,
            };
            if valign == HALIGN_CENTER {
                fmt |= DT_VCENTER;
            }
            let mut buf: Vec<u16> = s.encode_utf16().collect();
            buf.push(0);
            DrawTextW(hdc, buf.as_mut_ptr(), (buf.len() - 1) as i32, &mut rc, fmt);
            GdiFlush();
            SelectObject(hdc, prev);
            // 差分修复：字形触碰的像素 alpha 置回 255；未触碰像素原样保留
            for row in 0..rows {
                let dst = self.scan0.add((cy0 as usize + row) * stride + cx0 as usize * 4) as *mut u32;
                let src = snap.as_ptr().add(row * cols * 4) as *const u32;
                for col in 0..cols {
                    if *dst.add(col) != *src.add(col) {
                        *dst.add(col) |= 0xFF00_0000;
                    }
                }
            }
        }
        true
    }

    /// 居中单行文本（快捷）
    pub fn text_c(&self, s: &str, cx: f32, cy: f32, px: f32, bold: bool, mdl2: bool, color: u32) {
        self.text(s, cx - 150.0, cy - 20.0, 300.0, 40.0, HALIGN_CENTER, HALIGN_CENTER, px, bold, mdl2, color);
    }

    pub fn measure(&self, s: &str, px: f32, bold: bool, mdl2: bool) -> (f32, f32) {
        let px = px * text_scale();
        unsafe {
            let layout = RectF { x: 0.0, y: 0.0, w: 10000.0, h: 200.0 };
            let mut out = RectF { x: 0.0, y: 0.0, w: 0.0, h: 0.0 };
            WIDE_SCRATCH.with(|b| {
                let mut buf = b.borrow_mut();
                wide_into(&mut buf, s);
                GdipMeasureString(
                    self.g,
                    buf.as_ptr(),
                    (buf.len() - 1) as i32,
                    (*self.cache).font(self.s(px), bold, mdl2),
                    &layout,
                    (*self.cache).format,
                    &mut out,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
            });
            (out.w / self.sf, out.h / self.sf)
        }
    }

    pub fn clear(&self) {
        unsafe { GdipGraphicsClear(self.g, 0); }
    }
}
