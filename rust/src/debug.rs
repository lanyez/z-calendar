//! 调试开关（环境变量）集中登记。此前 16 个 `CAL_*` 开关散落在 5 个模块里，无处可查。
//!
//! 发布包同样保留这些钩子（体积可忽略）：它们是对照真实构建排查渲染、定位、
//! 通知问题的手段，排查时用的正是发布配置的 exe。
//!
//! | 变量 | 作用 |
//! |------|------|
//! | `CAL_DUMP` / `CAL_DUMP2` / `CAL_DUMP3` / `CAL_DUMP4` | 把主面板 / 设置窗 / 天气面板 / 侧栏的首帧转储为 BMP（值 = 输出文件路径） |
//! | `CAL_DUMP_EXIT=1` | 首帧转储完成后退出（无人值守的截图回归） |
//! | `CAL_PAGE=agenda` 或 `monthpick` | 启动时所在页 |
//! | `CAL_YM=YYYY-M` | 启动时的年月 |
//! | `CAL_SIDEBAR=1` | 弹出日历时自动打开日期侧栏 |
//! | `CAL_SETTINGS=1` / `CAL_TAB=n` | 启动即显示设置窗口 / 指定页签 |
//! | `CAL_SF=1.25` | 强制覆盖 DPI 缩放（覆盖时不参与缩放轮询） |
//! | `CAL_TEXT_HINT=0..5` | 文本渲染提示（对比渲染效果） |
//! | `CAL_FAKE_WX=1` | 使用固定示例天气（离线验证界面） |
//! | `CAL_WNREADY=1` / `CAL_WNSHOW=1` | 通知中心就绪链路检查 / 发一条测试通知，结果写 wnotify.log |
//! | `CAL_UIA_LOG=1` | 每轮 UIA 扫描结果写 uia.log |
//! | `CAL_DATA_DIR=<dir>` | 改用别的数据目录（与已安装实例并行运行开发构建时隔离数据） |
//! | `CAL_INSTANCE=<suffix>` | 给单实例事件名加后缀，允许开发构建与已安装实例同时运行 |
//! | `CAL_SELFTEST=<dir>` | 界面自检：各类窗口各渲染一帧转储到 dir 后退出（需 `--features selftest`，见 selftest 模块） |

#[cfg(feature = "selftest")]
use std::path::Path;

use winapi::shared::windef::HWND;
use winapi::um::winuser::{PostMessageW, WM_CLOSE};

pub const DUMP_MAIN: &str = "CAL_DUMP";
pub const DUMP_SETTINGS: &str = "CAL_DUMP2";
pub const DUMP_FORECAST: &str = "CAL_DUMP3";
pub const DUMP_SIDEBAR: &str = "CAL_DUMP4";
pub const DUMP_EXIT: &str = "CAL_DUMP_EXIT";
pub const PAGE: &str = "CAL_PAGE";
pub const YM: &str = "CAL_YM";
pub const SIDEBAR: &str = "CAL_SIDEBAR";
pub const SETTINGS: &str = "CAL_SETTINGS";
pub const TAB: &str = "CAL_TAB";
pub const SF: &str = "CAL_SF";
pub const TEXT_HINT: &str = "CAL_TEXT_HINT";
pub const FAKE_WX: &str = "CAL_FAKE_WX";
pub const WN_READY: &str = "CAL_WNREADY";
pub const WN_SHOW: &str = "CAL_WNSHOW";
pub const UIA_LOG: &str = "CAL_UIA_LOG";
pub const DATA_DIR: &str = "CAL_DATA_DIR";
pub const INSTANCE: &str = "CAL_INSTANCE";
#[cfg(feature = "selftest")]
pub const SELFTEST: &str = "CAL_SELFTEST";
/// 自检用浅色主题（默认深色）
#[cfg(feature = "selftest")]
pub const SELFTEST_LIGHT: &str = "CAL_SELFTEST_LIGHT";

/// 布尔开关：已设置且值等于 "1"
pub fn flag(name: &str) -> bool {
    std::env::var(name).map(|v| v == "1").unwrap_or(false)
}

/// 取值；未设置为空串
pub fn value(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// 取值（可为非 UTF-8 路径）；未设置或为空时 None
pub fn value_os(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).filter(|v| !v.is_empty())
}

/// 是否已设置（不论取值）
pub fn is_set(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

/// 取值并解析；未设置或格式不对为 None
pub fn parse<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

/// `CAL_YM=YYYY-M` → (年, 月)
pub fn view_month() -> (Option<i32>, Option<u32>) {
    let v = std::env::var(YM).ok();
    let year = v.as_deref().and_then(|s| s.split('-').next()).and_then(|x| x.parse().ok());
    let month = v.as_deref().and_then(|s| s.split('-').nth(1)).and_then(|x| x.parse().ok());
    (year, month)
}

/// 首帧转储状态（每个窗口一份；对应的 `CAL_DUMP*` 未设置时不做任何事）
#[derive(Default)]
pub struct FrameDump {
    path: String,
    done: bool,
}

impl FrameDump {
    pub fn from_env(name: &str) -> Self {
        FrameDump { path: value(name), done: false }
    }

    pub fn enabled(&self) -> bool {
        !self.path.is_empty()
    }

    /// 首次调用时把窗口的后台位图写成 BMP；`CAL_DUMP_EXIT=1` 时随后向 close_hwnd
    /// 发 WM_CLOSE（通常是主窗口：其窗口过程据此退出消息循环）
    pub fn take(&mut self, close_hwnd: usize, pixels: *const u8, w: i32, h: i32) {
        if self.done || self.path.is_empty() {
            return;
        }
        self.done = true;
        save_bmp(pixels, w, h, &self.path);
        if flag(DUMP_EXIT) {
            unsafe {
                PostMessageW(close_hwnd as HWND, WM_CLOSE, 0, 0);
            }
        }
    }
}

/// 把一张后台位图写成 `<dir>/<name>.bmp`。w / h / sf 为窗口的逻辑尺寸与缩放
/// （位图可能比要存的区域大，如按最大高度分配的弹窗）
#[cfg(feature = "selftest")]
pub(crate) fn save_frame(dir: &Path, name: &str, surf: &crate::gdi::Dib, w: f32, h: f32, sf: f32) {
    if !surf.is_alloc() {
        eprintln!("selftest: {name}: 位图未分配");
        return;
    }
    let px = ((w * sf) as i32, (h * sf) as i32);
    save_bmp(surf.pixels(), px.0, px.1, dir.join(format!("{name}.bmp")).to_string_lossy().as_ref());
}

/// 32bpp BGRA（自上而下）位图 → 32 位 BMP 文件
fn save_bmp(scan0: *const u8, w: i32, h: i32, path: &str) {
    let stride = (w.max(0) as usize) * 4;
    let rows = h.max(0) as usize;
    let data_size = stride * rows;
    let file_size = 54 + data_size;
    let mut buf = Vec::with_capacity(file_size);
    buf.extend_from_slice(b"BM");
    buf.extend_from_slice(&(file_size as u32).to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&54u32.to_le_bytes());
    buf.extend_from_slice(&40u32.to_le_bytes());
    buf.extend_from_slice(&w.to_le_bytes());
    buf.extend_from_slice(&h.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&32u16.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&(data_size as u32).to_le_bytes());
    buf.extend_from_slice(&2835u32.to_le_bytes());
    buf.extend_from_slice(&2835u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    if scan0.is_null() {
        let _ = std::fs::write(path, buf);
        return;
    }
    unsafe {
        // BMP 自下而上存储
        for row in (0..rows).rev() {
            let src = std::slice::from_raw_parts(scan0.add(row * stride), stride);
            buf.extend_from_slice(src);
        }
    }
    let _ = std::fs::write(path, buf);
}
