//! GDI+ 平面 API 封装（最小子集）+ 绘制助手
use std::collections::HashMap;
use std::sync::Mutex;

pub type Gp = *mut winapi::ctypes::c_void;

pub const PIXEL_FORMAT_32BPP_PARGB: i32 = 0xE200B;
pub const UNIT_PIXEL: i32 = 2;
pub const SMOOTH_ANTI_ALIAS: i32 = 4;
pub const TEXT_AA_GRID_FIT: i32 = 4;
pub const FONT_STYLE_NORMAL: i32 = 0;
pub const FONT_STYLE_BOLD: i32 = 1;
pub const HALIGN_NEAR: i32 = 0;
pub const HALIGN_CENTER: i32 = 1;
pub const HALIGN_FAR: i32 = 2;

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

    /// 居中单行文本（快捷）
    pub fn text_c(&self, s: &str, cx: f32, cy: f32, px: f32, bold: bool, mdl2: bool, color: u32) {
        self.text(s, cx - 150.0, cy - 20.0, 300.0, 40.0, HALIGN_CENTER, HALIGN_CENTER, px, bold, mdl2, color);
    }

    pub fn measure(&self, s: &str, px: f32, bold: bool, mdl2: bool) -> (f32, f32) {
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
