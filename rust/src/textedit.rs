//! 通用文本编辑内核：光标/选区一律按 char 计数，供主面板快捷输入与新建/编辑弹窗
//! 共用。调用方持有 (String, usize 光标, Option<usize> 选区锚点) 三元组，在此之上
//! 完成插入、删除、移动、剪贴板、撤销/重做等操作。

/// 撤销/重做栈：变更前调用 snapshot 压入 (text, caret, sel) 快照；
/// undo/redo 在新旧栈间搬运并恢复。连续字符输入 500ms 内合并为一步
/// （整段打字一次撤销），光标移动/删除/粘贴各自独立成步。
pub struct Undo {
    cap: usize,
    past: Vec<(String, usize, Option<usize>)>,
    future: Vec<(String, usize, Option<usize>)>,
    /// 上一次快照是否为可合并的字符插入（"ins"）
    last_ins: bool,
    last_at: Option<std::time::Instant>,
}

impl Undo {
    pub fn new(cap: usize) -> Self {
        Undo { cap: cap.max(1), past: Vec::new(), future: Vec::new(), last_ins: false, last_at: None }
    }

    /// 变更前调用。kind = "ins"（字符插入，可合并）或其它（独立一步）。
    pub fn snapshot(&mut self, text: &str, caret: usize, sel: Option<usize>, kind: &str) {
        let now = std::time::Instant::now();
        let ins = kind == "ins";
        let coalesce = ins
            && self.last_ins
            && self.last_at.map(|t| now.duration_since(t).as_millis() < 500).unwrap_or(false);
        self.last_ins = ins;
        self.last_at = Some(now);
        if coalesce {
            return;
        }
        self.past.push((text.to_string(), caret, sel));
        if self.past.len() > self.cap {
            self.past.remove(0);
        }
        self.future.clear();
    }

    /// 撤销：恢复到最近一次快照。返回是否发生了恢复。
    pub fn undo(&mut self, text: &mut String, caret: &mut usize, sel: &mut Option<usize>) -> bool {
        let Some((t, c, s)) = self.past.pop() else { return false };
        self.future.push((std::mem::take(text), *caret, *sel));
        *text = t;
        *caret = c;
        *sel = s;
        self.last_ins = false;
        true
    }

    /// 重做：恢复被撤销的状态。返回是否发生了恢复。
    pub fn redo(&mut self, text: &mut String, caret: &mut usize, sel: &mut Option<usize>) -> bool {
        let Some((t, c, s)) = self.future.pop() else { return false };
        self.past.push((std::mem::take(text), *caret, *sel));
        *text = t;
        *caret = c;
        *sel = s;
        self.last_ins = false;
        true
    }
}

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



#[cfg(test)]
mod tests {
    use super::*;

    /// (文本, 光标, 选区锚点)
    type Edit = (String, usize, Option<usize>);

    fn ed(s: &str, caret: usize, sel: Option<usize>) -> Edit {
        (s.to_string(), caret, sel)
    }

    #[test]
    fn insert_replaces_selection_and_moves_caret() {
        let mut e = ed("甲乙丙", 3, Some(0));
        insert(&mut e.0, &mut e.1, &mut e.2, "新");
        assert_eq!(e, ed("新", 1, None));
        let mut e = ed("甲丙", 1, None);
        insert(&mut e.0, &mut e.1, &mut e.2, "乙");
        assert_eq!(e, ed("甲乙丙", 2, None), "光标落在插入内容之后");
        // 选区方向无关：光标在前、锚点在后同样整段替换
        let mut e = ed("甲乙丙", 1, Some(3));
        insert(&mut e.0, &mut e.1, &mut e.2, "X");
        assert_eq!(e, ed("甲X", 2, None));
    }

    #[test]
    fn deletions_handle_selection_and_bounds() {
        let mut e = ed("甲乙丙", 2, None);
        backspace(&mut e.0, &mut e.1, &mut e.2);
        assert_eq!(e, ed("甲丙", 1, None));
        delete_fwd(&mut e.0, &mut e.1, &mut e.2);
        assert_eq!(e, ed("甲", 1, None));
        // 有选区时 Backspace / Delete 都只删选区
        let mut e = ed("甲乙丙", 2, Some(1));
        backspace(&mut e.0, &mut e.1, &mut e.2);
        assert_eq!(e, ed("甲丙", 1, None));
        let mut e = ed("甲乙丙", 1, Some(2));
        delete_fwd(&mut e.0, &mut e.1, &mut e.2);
        assert_eq!(e, ed("甲丙", 1, None));
        // 边界：行首退格、行尾删除、越界下标都不 panic、不改文本
        let mut e = ed("甲", 0, None);
        backspace(&mut e.0, &mut e.1, &mut e.2);
        assert_eq!(e, ed("甲", 0, None));
        let mut e = ed("甲", 1, None);
        delete_fwd(&mut e.0, &mut e.1, &mut e.2);
        assert_eq!(e, ed("甲", 1, None));
        let mut e = ed("", 0, None);
        backspace(&mut e.0, &mut e.1, &mut e.2);
        assert_eq!(e, ed("", 0, None));
        // 下标按字符计：emoji（UTF-16 代理对）算一个字符
        let mut e = ed("a😀b", 2, None);
        backspace(&mut e.0, &mut e.1, &mut e.2);
        assert_eq!(e, ed("ab", 1, None));
    }

    #[test]
    fn selection_range_and_text() {
        assert_eq!(sel_range("abc", 2, None), (2, 2));
        assert_eq!(sel_range("abc", 3, Some(1)), (1, 3), "锚点在前");
        assert_eq!(sel_range("abc", 1, Some(3)), (1, 3), "光标在前");
        assert_eq!(sel_range("abc", 99, Some(99)), (3, 3), "越界下标夹紧到文本长度");
        assert_eq!(sel_text("abc", 3, Some(1)).as_deref(), Some("bc"));
        assert_eq!(sel_text("abc", 2, None), None);
        assert_eq!(sel_text("中😀文", 2, Some(0)).as_deref(), Some("中😀"));
    }

    #[test]
    fn caret_movement_clamps_and_shift_extends() {
        let mut e = ed("abcd", 1, None);
        move_caret(&e.0, &mut e.1, &mut e.2, 2, false);
        assert_eq!(e, ed("abcd", 3, None));
        move_caret(&e.0, &mut e.1, &mut e.2, -10, true);
        assert_eq!(e, ed("abcd", 0, Some(3)), "越界夹紧，Shift 以原位置为锚点");
        move_caret(&e.0, &mut e.1, &mut e.2, 1, false);
        assert_eq!(e, ed("abcd", 1, None), "不带 Shift 清选区");
        move_caret(&e.0, &mut e.1, &mut e.2, 100, false);
        assert_eq!(e.1, 4);
        home_end(&e.0, &mut e.1, &mut e.2, false, false);
        assert_eq!(e.1, 0);
        home_end(&e.0, &mut e.1, &mut e.2, true, false);
        assert_eq!(e.1, 4);
        select_all(&e.0, &mut e.1, &mut e.2);
        assert_eq!(e, ed("abcd", 4, Some(0)));
    }

    #[test]
    fn word_movement_skips_separators_then_consumes_a_run() {
        // Ctrl+→：先跳过非字母数字（", " 一并跳过），再吃掉一段字母数字
        let mut e = ed("foo, bar", 0, None);
        for pos in [3, 8] {
            move_word(&e.0, &mut e.1, &mut e.2, true, false);
            assert_eq!(e.1, pos);
        }
        // Ctrl+←：镜像（停在下一个词的开头）
        for pos in [5, 0] {
            move_word(&e.0, &mut e.1, &mut e.2, false, false);
            assert_eq!(e.1, pos);
        }
        // Shift 扩展选区
        let mut e = ed("foo bar", 0, None);
        move_word(&e.0, &mut e.1, &mut e.2, true, true);
        assert_eq!(e, ed("foo bar", 3, Some(0)));
    }

    #[test]
    fn line_helpers_and_vertical_movement_keep_column() {
        let text = "ab\ncde\nf";
        assert_eq!((line_start(text, 0), line_end(text, 0)), (0, 2));
        assert_eq!((line_start(text, 5), line_end(text, 5)), (3, 6));
        assert_eq!((line_start(text, 8), line_end(text, 8)), (7, 8), "最后一行行尾 = 文本末尾");
        let mut e = ed(text, 1, None);
        move_line(&e.0, &mut e.1, &mut e.2, true, false);
        assert_eq!(e.1, 4, "第 1 列 → 下一行第 1 列");
        move_line(&e.0, &mut e.1, &mut e.2, true, false);
        assert_eq!(e.1, 8, "短行夹到行尾");
        move_line(&e.0, &mut e.1, &mut e.2, true, false);
        assert_eq!(e.1, 8, "已是最后一行不动");
        move_line(&e.0, &mut e.1, &mut e.2, false, false);
        assert_eq!(e.1, 4);
        move_line(&e.0, &mut e.1, &mut e.2, false, true);
        assert_eq!(e, ed(text, 1, Some(4)), "Shift 扩展选区");
    }

    #[test]
    fn undo_redo_coalesce_and_capacity() {
        let mut u = Undo::new(4);
        let mut e = ed("", 0, None);
        // 连续字符输入合并为一步；非输入类操作各自独立
        u.snapshot(&e.0, e.1, e.2, "ins");
        insert(&mut e.0, &mut e.1, &mut e.2, "a");
        u.snapshot(&e.0, e.1, e.2, "ins");
        insert(&mut e.0, &mut e.1, &mut e.2, "b");
        u.snapshot(&e.0, e.1, e.2, "");
        insert(&mut e.0, &mut e.1, &mut e.2, "c");
        assert_eq!(e.0, "abc");
        assert!(u.undo(&mut e.0, &mut e.1, &mut e.2));
        assert_eq!(e.0, "ab", "撤销独立的一步");
        assert!(u.undo(&mut e.0, &mut e.1, &mut e.2));
        assert_eq!(e.0, "", "合并的连续输入一次撤销到位");
        assert!(!u.undo(&mut e.0, &mut e.1, &mut e.2), "栈空");
        assert!(u.redo(&mut e.0, &mut e.1, &mut e.2));
        assert!(u.redo(&mut e.0, &mut e.1, &mut e.2));
        assert_eq!(e.0, "abc");
        assert!(!u.redo(&mut e.0, &mut e.1, &mut e.2));
        // 撤销后再编辑：重做栈作废
        assert!(u.undo(&mut e.0, &mut e.1, &mut e.2));
        u.snapshot(&e.0, e.1, e.2, "ins");
        insert(&mut e.0, &mut e.1, &mut e.2, "d");
        assert_eq!(e.0, "abd");
        assert!(!u.redo(&mut e.0, &mut e.1, &mut e.2));
        // 光标与选区随快照恢复
        let mut e = ed("xyz", 3, None);
        u.snapshot(&e.0, e.1, e.2, "");
        select_all(&e.0, &mut e.1, &mut e.2);
        assert!(u.undo(&mut e.0, &mut e.1, &mut e.2));
        assert_eq!(e, ed("xyz", 3, None));
        // 容量：只保留最近 cap 个快照
        let mut u = Undo::new(2);
        let mut e = ed("", 0, None);
        for ch in "abcd".chars() {
            u.snapshot(&e.0, e.1, e.2, "");
            insert(&mut e.0, &mut e.1, &mut e.2, &ch.to_string());
        }
        assert_eq!(e.0, "abcd");
        for want in ["abc", "ab"] {
            assert!(u.undo(&mut e.0, &mut e.1, &mut e.2));
            assert_eq!(e.0, want);
        }
        assert!(!u.undo(&mut e.0, &mut e.1, &mut e.2), "更早的快照已被淘汰");
    }

    #[test]
    fn caret_at_x_is_approximate_by_char_class() {
        // 点击落点 → 字符下标：CJK 13px / ASCII 6.8px 的粗估
        assert_eq!(caret_at_x_approx("中a中", 0.0), 0);
        assert_eq!(caret_at_x_approx("中a中", 5.0), 0, "未过第一个字符的 midpoint");
        assert_eq!(caret_at_x_approx("中a中", 7.0), 1, "过了第一个字符的 midpoint");
        assert_eq!(caret_at_x_approx("中a中", 17.0), 2);
        assert_eq!(caret_at_x_approx("中a中", 100.0), 3, "越过行尾落在末尾");
        assert_eq!(caret_at_x_approx("", 50.0), 0);
    }
}
