//! 通用文本编辑内核：光标/选区一律按 char 计数，供主面板快捷输入与新建/编辑弹窗
//! 共用。调用方持有 (String, usize 光标, Option<usize> 选区锚点) 三元组，在此之上
//! 完成插入、删除、移动、剪贴板等操作。

/// 归一化选区范围：(较小 char 下标, 较大 char 下标)；无选区时为 (caret, caret)
pub fn sel_range(text: &str, caret: usize, sel: Option<usize>) -> (usize, usize) {
    let n = text.chars().count();
    let anchor = sel.unwrap_or(caret).min(n);
    let caret = caret.min(n);
    (anchor.min(caret), anchor.max(caret))
}

/// 在光标处插入文本（有选区时先替换选区），光标移到插入内容之后
pub fn insert(text: &mut String, caret: &mut usize, sel: &mut Option<usize>, s: &str) {
    let chars: Vec<char> = text.chars().collect();
    let (a, b) = sel_range(text, *caret, *sel);
    let mut out: String = chars[..a].iter().collect();
    out.push_str(s);
    out.extend(chars[b..].iter());
    *text = out;
    *caret = a + s.chars().count();
    *sel = None;
}

/// 删除当前选区。返回是否删了东西
pub fn delete_sel(text: &mut String, caret: &mut usize, sel: &mut Option<usize>) -> bool {
    let (a, b) = sel_range(text, *caret, *sel);
    if a == b {
        return false;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out: String = chars[..a].iter().collect();
    out.extend(chars[b..].iter());
    *text = out;
    *caret = a;
    *sel = None;
    true
}

/// Backspace：删选区，无选区删光标前一个字符
pub fn backspace(text: &mut String, caret: &mut usize, sel: &mut Option<usize>) {
    if delete_sel(text, caret, sel) {
        return;
    }
    let chars: Vec<char> = text.chars().collect();
    if *caret > 0 && *caret <= chars.len() {
        let mut out: String = chars[..*caret - 1].iter().collect();
        out.extend(chars[*caret..].iter());
        *text = out;
        *caret -= 1;
    }
}

/// Delete 键：删选区，无选区删光标后一个字符
pub fn delete_fwd(text: &mut String, caret: &mut usize, sel: &mut Option<usize>) {
    if delete_sel(text, caret, sel) {
        return;
    }
    let chars: Vec<char> = text.chars().collect();
    if *caret < chars.len() {
        let mut out: String = chars[..*caret].iter().collect();
        out.extend(chars[*caret + 1..].iter());
        *text = out;
    }
}

/// 左右移动光标（delta 为字符数，可负）；extend=true 时扩展选区（Shift 按住）
pub fn move_caret(text: &str, caret: &mut usize, sel: &mut Option<usize>, delta: i32, extend: bool) {
    let n = text.chars().count();
    let from = (*caret).min(n);
    if extend {
        if sel.is_none() {
            *sel = Some(from);
        }
    } else {
        *sel = None;
    }
    *caret = (from as i32 + delta).clamp(0, n as i32) as usize;
}

/// 按词跳转（Ctrl+←/→）：字母数字连串为一个词，其余逐字符
pub fn move_word(text: &str, caret: &mut usize, sel: &mut Option<usize>, forward: bool, extend: bool) {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut pos = (*caret).min(n);
    if extend {
        if sel.is_none() {
            *sel = Some(pos);
        }
    } else {
        *sel = None;
    }
    let word = |c: char| c.is_alphanumeric();
    if forward {
        while pos < n && !word(chars[pos]) {
            pos += 1;
        }
        while pos < n && word(chars[pos]) {
            pos += 1;
        }
    } else {
        while pos > 0 && !word(chars[pos - 1]) {
            pos -= 1;
        }
        while pos > 0 && word(chars[pos - 1]) {
            pos -= 1;
        }
    }
    *caret = pos;
}

/// Home/End（单行）；extend=true 时扩展选区
pub fn home_end(text: &str, caret: &mut usize, sel: &mut Option<usize>, end: bool, extend: bool) {
    let n = text.chars().count();
    let from = (*caret).min(n);
    if extend {
        if sel.is_none() {
            *sel = Some(from);
        }
    } else {
        *sel = None;
    }
    *caret = if end { n } else { 0 };
}

/// 全选
pub fn select_all(text: &str, caret: &mut usize, sel: &mut Option<usize>) {
    *sel = Some(0);
    *caret = text.chars().count();
}

/// 当前选中的文本（复制/剪切用）；无选区返回 None
pub fn sel_text(text: &str, caret: usize, sel: Option<usize>) -> Option<String> {
    let (a, b) = sel_range(text, caret, sel);
    if a == b {
        return None;
    }
    Some(text.chars().skip(a).take(b - a).collect())
}

/// 光标前最近一个 '\n' 之后的行首 char 下标（多行 Home 用）
pub fn line_start(text: &str, caret: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let pos = caret.min(chars.len());
    chars[..pos].iter().rposition(|c| *c == '\n').map(|i| i + 1).unwrap_or(0)
}

/// 光标后最近一个 '\n' 之前的行尾 char 下标（多行 End 用）
pub fn line_end(text: &str, caret: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let pos = caret.min(chars.len());
    chars[pos..].iter().position(|c| *c == '\n').map(|i| pos + i).unwrap_or(chars.len())
}

/// 多行文本上下移动一行（保持列位置）；Up/Down
pub fn move_line(text: &str, caret: &mut usize, sel: &mut Option<usize>, down: bool, extend: bool) {
    let chars: Vec<char> = text.chars().collect();
    let pos = (*caret).min(chars.len());
    if extend {
        if sel.is_none() {
            *sel = Some(pos);
        }
    } else {
        *sel = None;
    }
    let ls = chars[..pos].iter().rposition(|c| *c == '\n').map(|i| i + 1).unwrap_or(0);
    let le = chars[pos..].iter().position(|c| *c == '\n').map(|i| pos + i).unwrap_or(chars.len());
    let col = pos - ls;
    let (ts, te) = if down {
        if le >= chars.len() {
            return; // 已是最后一行
        }
        let nls = le + 1;
        let nle = chars[nls..].iter().position(|c| *c == '\n').map(|i| nls + i).unwrap_or(chars.len());
        (nls, nle)
    } else {
        if ls == 0 {
            return; // 已是第一行
        }
        let pls = chars[..ls - 1].iter().rposition(|c| *c == '\n').map(|i| i + 1).unwrap_or(0);
        (pls, ls - 1)
    };
    *caret = ts + col.min(te - ts);
}

/// 点击落点 → 近似 char 下标（CJK 13px / ASCII 6.8px，13px 字号；精度足够光标定位）
pub fn caret_at_x_approx(text: &str, w: f32) -> usize {
    let mut acc = 0.0f32;
    let mut pos = 0usize;
    for c in text.chars() {
        let cw = if c.is_ascii() { 6.8 } else { 13.0 };
        if acc + cw / 2.0 > w {
            break;
        }
        acc += cw;
        pos += 1;
    }
    pos
}

/// 写剪贴板（CF_UNICODETEXT）。成功后内存归系统所有；失败释放并返回 false
pub unsafe fn set_clipboard(hwnd: usize, s: &str) -> bool {
    use winapi::shared::ntdef::HANDLE;
    use winapi::shared::windef::HWND;
    use winapi::um::winbase::{GlobalAlloc, GlobalFree, GMEM_MOVEABLE};
    use winapi::um::winuser::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData, CF_UNICODETEXT};
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalLock(h: usize) -> *mut winapi::ctypes::c_void;
        fn GlobalUnlock(h: usize) -> i32;
    }
    if s.is_empty() {
        return false;
    }
    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if OpenClipboard(hwnd as HWND) == 0 {
            return false;
        }
        EmptyClipboard();
        let h = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) as usize;
        let ok = if h != 0 {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
                GlobalUnlock(h);
                // 成功后所有权移交系统，不再 GlobalFree
                !SetClipboardData(CF_UNICODETEXT, h as HANDLE).is_null() || {
                    GlobalFree(h as HANDLE);
                    false
                }
            } else {
                GlobalFree(h as HANDLE);
                false
            }
        } else {
            false
        };
        CloseClipboard();
        ok
    }
}
