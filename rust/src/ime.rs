//! IME 组合串读取（imm32）：主面板快捷输入、新建/编辑弹窗、城市搜索框共用。
//! 此前三处各抄了一份 imm32 的 extern 声明与读取函数。
//! winapi 0.3 的绑定拆在 imm / immdev 两个模块里且句柄类型互不相通，这里保留一份手写声明
use winapi::shared::windef::HWND;

#[link(name = "imm32")]
extern "system" {
    fn ImmGetContext(hwnd: HWND) -> *mut winapi::ctypes::c_void;
    fn ImmReleaseContext(hwnd: HWND, himc: *mut winapi::ctypes::c_void) -> i32;
    fn ImmGetCompositionStringW(himc: *mut winapi::ctypes::c_void, index: i32, buf: *mut winapi::ctypes::c_void, len: i32) -> i32;
}

/// 组合中串（GCS_COMPSTR：拼音等预编辑文本，内联显示在光标处）
pub const COMP: i32 = 0x0008;
/// 结果串（GCS_RESULTSTR：上屏的最终文本）
pub const RESULT: i32 = 0x0800;

/// 读窗口当前输入上下文的组合串 / 结果串（which 取 COMP / RESULT）；
/// 无输入上下文或读取失败返回 None
pub unsafe fn composition_string(hwnd: HWND, which: i32) -> Option<String> {
    unsafe {
        let himc = ImmGetContext(hwnd);
        if himc.is_null() {
            return None;
        }
        // 返回值是字节数（不含结尾 NUL）；多留 2 字节容纳 NUL
        let len = ImmGetCompositionStringW(himc, which, std::ptr::null_mut(), 0);
        let mut out = None;
        if len >= 0 {
            let mut buf = vec![0u8; (len + 2) as usize];
            let got = ImmGetCompositionStringW(himc, which, buf.as_mut_ptr() as *mut winapi::ctypes::c_void, len);
            if got >= 0 {
                let units = std::slice::from_raw_parts::<u16>(buf.as_ptr() as *const u16, (len as usize) / 2);
                out = Some(String::from_utf16_lossy(units));
            }
        }
        ImmReleaseContext(hwnd, himc);
        out
    }
}
