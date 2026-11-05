//! 直接终端模式下的输入法组字状态。
//!
//! 终端没有可编辑的文档：组字文字只叠加显示在终端光标处，上屏后才作为普通输入写入 PTY。
//! 平台输入法接口以 UTF-16 偏移描述范围，这里统一换算为 Rust 字符串的 UTF-8 偏移。
use std::ops::Range;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preedit {
    text: String,
    /// 组字文字内的插入点或选区（UTF-8 偏移）。
    selection: Range<usize>,
}

impl Preedit {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// 输入法给出的选区以 UTF-16 计；缺省时插入点在组字文字末尾。
    pub fn set(&mut self, text: &str, selected_utf16: Option<Range<usize>>) {
        self.text = text.to_owned();
        self.selection = match selected_utf16 {
            Some(range) => {
                let start = utf8_offset(text, range.start);
                start..utf8_offset(text, range.end).max(start)
            }
            None => text.len()..text.len(),
        };
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.selection = 0..0;
    }

    /// 组字时的插入点（UTF-8 偏移），用于绘制光标。
    pub fn caret(&self) -> usize {
        self.selection.end
    }

    pub fn marked_range_utf16(&self) -> Option<Range<usize>> {
        (!self.text.is_empty()).then(|| 0..utf16_len(&self.text))
    }

    pub fn selected_range_utf16(&self) -> Range<usize> {
        utf16_offset(&self.text, self.selection.start)..utf16_offset(&self.text, self.selection.end)
    }

    /// 输入法查询的范围可能越界，按组字文字长度截断，并返回实际范围。
    pub fn text_for_range(&self, range_utf16: Range<usize>) -> (String, Range<usize>) {
        let start = utf8_offset(&self.text, range_utf16.start);
        let end = utf8_offset(&self.text, range_utf16.end).max(start);
        (
            self.text[start..end].to_owned(),
            utf16_offset(&self.text, start)..utf16_offset(&self.text, end),
        )
    }

    /// UTF-16 范围对应的 UTF-8 范围，用于计算候选框位置。
    pub fn utf8_range(&self, range_utf16: Range<usize>) -> Range<usize> {
        let start = utf8_offset(&self.text, range_utf16.start);
        start..utf8_offset(&self.text, range_utf16.end).max(start)
    }
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// UTF-16 偏移 → UTF-8 偏移；落在代理对中间或越界时取最近的合法字符边界。
fn utf8_offset(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (index, ch) in text.char_indices() {
        if units >= utf16 {
            return index;
        }
        units += ch.len_utf16();
    }
    text.len()
}

fn utf16_offset(text: &str, utf8: usize) -> usize {
    text.char_indices()
        .take_while(|(index, _)| *index < utf8)
        .map(|(_, ch)| ch.len_utf16())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_defaults_to_the_end_and_follows_utf16_selection() {
        let mut preedit = Preedit::default();
        assert!(preedit.marked_range_utf16().is_none());
        preedit.set("ni hao", None);
        assert_eq!(preedit.caret(), 6);
        assert_eq!(preedit.marked_range_utf16(), Some(0..6));
        assert_eq!(preedit.selected_range_utf16(), 6..6);

        // 中文每字 3 个 UTF-8 字节、1 个 UTF-16 单元。
        preedit.set("你好", Some(1..1));
        assert_eq!(preedit.caret(), 3);
        assert_eq!(preedit.selected_range_utf16(), 1..1);

        preedit.clear();
        assert!(preedit.is_empty());
        assert!(preedit.marked_range_utf16().is_none());
    }

    #[test]
    fn surrogate_pairs_and_out_of_range_queries_are_clamped() {
        let mut preedit = Preedit::default();
        // 😀 占 2 个 UTF-16 单元、4 个 UTF-8 字节。
        preedit.set("a😀b", None);
        assert_eq!(preedit.marked_range_utf16(), Some(0..4));
        assert_eq!(preedit.text_for_range(1..3), ("😀".to_owned(), 1..3));
        assert_eq!(preedit.text_for_range(3..99), ("b".to_owned(), 3..4));
        // 输入法可能给出反向范围，按空范围处理。
        let reversed = Range { start: 9, end: 2 };
        assert_eq!(preedit.text_for_range(reversed), (String::new(), 4..4));
        assert_eq!(preedit.utf8_range(0..1), 0..1);
        assert_eq!(preedit.utf8_range(1..3), 1..5);
        // 落在代理对中间时不会切出非法的 UTF-8 边界。
        preedit.set("😀", Some(1..1));
        assert!(preedit.text().is_char_boundary(preedit.caret()));
    }
}
