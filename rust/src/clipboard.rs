//! 剪贴板（CF_UNICODETEXT）：写入选中文本、读出全文。
//! 此前写入在 textedit、读取在 inputbox 与 flyout 各抄一份，各自重复声明 winapi 已有的函数
use winapi::shared::windef::HWND;
use winapi::um::winbase::{GlobalAlloc, GlobalFree, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use winapi::um::winuser::{CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData, CF_UNICODETEXT};

/// 写剪贴板。成功后内存归系统所有；失败（空串/剪贴板被占用/分配失败）释放并返回 false
pub unsafe fn set_text(hwnd: usize, s: &str) -> bool {
    unsafe {
        if s.is_empty() {
            return false;
        }
        let wide = crate::wide(s);
        if OpenClipboard(hwnd as HWND) == 0 {
            return false;
        }
        EmptyClipboard();
        let h = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
        let ok = if h.is_null() {
            false
        } else {
            let p = GlobalLock(h) as *mut u16;
            if p.is_null() {
                GlobalFree(h);
                false
            } else {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
                GlobalUnlock(h);
                // 成功后所有权移交系统，不再 GlobalFree
                !SetClipboardData(CF_UNICODETEXT, h).is_null() || {
                    GlobalFree(h);
                    false
                }
            }
        };
        CloseClipboard();
        ok
    }
}

/// 读剪贴板全文；没有文本（或被占用）返回 None。
/// 以 GlobalSize 为上界扫描 NUL 终止符：异常数据（无终止符）时不越界读
pub unsafe fn read_text(hwnd: usize) -> Option<String> {
    unsafe {
        if OpenClipboard(hwnd as HWND) == 0 {
            return None;
        }
        let mut out = None;
        let h = GetClipboardData(CF_UNICODETEXT);
        if !h.is_null() {
            let ptr = GlobalLock(h) as *const u16;
            if !ptr.is_null() {
                let cap = GlobalSize(h) / 2;
                let mut len = 0usize;
                while len < cap && *ptr.add(len) != 0 {
                    len += 1;
                }
                out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len)));
                GlobalUnlock(h);
            }
        }
        CloseClipboard();
        out
    }
}
