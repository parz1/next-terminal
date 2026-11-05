//! 被动观察 OSC 133 和本项目的 ZLE 通知；输出可由前台程序伪造，不作为输入授权。
use alacritty_terminal::vte::{Parser, Perform};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellMark {
    PromptStarted,
    CommandStarted,
    CommandFinished(Option<u8>),
    Editing { continuation: bool, empty: bool },
    EditorFinished,
    OtherEditor,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShellState {
    #[default]
    Unknown,
    PromptDrawing,
    Editing {
        continuation: bool,
        empty: bool,
    },
    EditorInactive,
    Running,
    Finished(Option<u8>),
    OtherEditor,
}

#[derive(Default)]
pub struct ShellObserver {
    parser: Parser,
    state: ShellState,
    cwd: Option<String>,
}

impl ShellObserver {
    pub fn advance(&mut self, bytes: &[u8], mut on_mark: impl FnMut(ShellMark)) {
        struct Marks<'a, F>(&'a mut ShellState, &'a mut Option<String>, F);
        impl<F: FnMut(ShellMark)> Perform for Marks<'_, F> {
            fn osc_dispatch(&mut self, params: &[&[u8]], _: bool) {
                // OSC 7 只更新显示用的当前目录；VTE 按分号切分参数，这里拼回原值。
                if let [b"7", rest @ ..] = params {
                    if let Some(path) = parse_osc7(&rest.join(&b';')) {
                        *self.1 = Some(path);
                    }
                    return;
                }
                let mark = match params {
                    [b"133", b"A"] => ShellMark::PromptStarted,
                    [b"133", b"C"] => ShellMark::CommandStarted,
                    [b"133", b"D"] => ShellMark::CommandFinished(None),
                    [b"133", b"D", code] => {
                        let Ok(code) = std::str::from_utf8(code).unwrap_or("").parse::<u8>() else {
                            return;
                        };
                        ShellMark::CommandFinished(Some(code))
                    }
                    [
                        b"777",
                        b"next-terminal",
                        b"zle",
                        b"1",
                        context @ (b"start" | b"cont"),
                        empty @ (b"0" | b"1"),
                    ] => ShellMark::Editing {
                        continuation: *context == b"cont",
                        empty: *empty == b"1",
                    },
                    [b"777", b"next-terminal", b"zle", b"1", b"inactive"] => {
                        ShellMark::EditorFinished
                    }
                    [b"777", b"next-terminal", b"zle", b"1", b"other"] => ShellMark::OtherEditor,
                    _ => return,
                };
                *self.0 = match mark {
                    ShellMark::PromptStarted => ShellState::PromptDrawing,
                    ShellMark::CommandStarted => ShellState::Running,
                    ShellMark::CommandFinished(code) => ShellState::Finished(code),
                    ShellMark::Editing {
                        continuation,
                        empty,
                    } => ShellState::Editing {
                        continuation,
                        empty,
                    },
                    ShellMark::EditorFinished => ShellState::EditorInactive,
                    ShellMark::OtherEditor => ShellState::OtherEditor,
                };
                (self.2)(mark);
            }
        }
        self.parser.advance(
            &mut Marks(&mut self.state, &mut self.cwd, &mut on_mark),
            bytes,
        );
    }

    /// shell 最近一次通过 OSC 7 报告的目录。前台程序也能输出 OSC 7，因此只用于展示。
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    pub fn state(&self) -> ShellState {
        self.state
    }

    /// 输入已入队但 shell 尚未回复，旧的空行观察不能代表此刻仍为空。
    pub fn invalidate(&mut self) {
        self.state = ShellState::Unknown;
    }

    pub fn label(&self) -> &'static str {
        match self.state {
            ShellState::Unknown => "等待 shell 状态",
            ShellState::PromptDrawing => "通知：提示符绘制中",
            ShellState::Editing {
                continuation: false,
                empty: true,
            } => "观察：主提示符 · 空行",
            ShellState::Editing {
                continuation: false,
                empty: false,
            } => "观察：主提示符 · 已有输入",
            ShellState::Editing {
                continuation: true,
                empty: true,
            } => "观察：续行 · 当前行为空",
            ShellState::Editing {
                continuation: true,
                empty: false,
            } => "观察：续行 · 已有输入",
            ShellState::EditorInactive => "观察：行编辑已结束",
            ShellState::Running => "通知：命令开始",
            ShellState::Finished(_) => "通知：命令结束",
            ShellState::OtherEditor => "观察：其他编辑上下文",
        }
    }
}

/// 解析 `file://host/path`，路径按字节百分号解码；不是合法 UTF-8 或缺少路径时忽略。
fn parse_osc7(uri: &[u8]) -> Option<String> {
    let rest = uri.strip_prefix(b"file://")?;
    let path = &rest[rest.iter().position(|&byte| byte == b'/')?..];
    let mut bytes = Vec::with_capacity(path.len());
    let mut index = 0;
    while index < path.len() {
        if path[index] == b'%' {
            let hex = std::str::from_utf8(path.get(index + 1..index + 3)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            bytes.push(path[index]);
            index += 1;
        }
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc7_reports_decoded_directory_without_touching_shell_state() {
        let mut observer = ShellObserver::default();
        let mut marks = Vec::new();
        observer.advance(
            b"\x1b]7;file://mac.local/Users/me/d%20ir/%E4%B8%AD;x%25y\x07",
            |mark| marks.push(mark),
        );
        assert_eq!(observer.cwd(), Some("/Users/me/d ir/中;x%y"));
        assert!(marks.is_empty());
        assert_eq!(observer.state(), ShellState::Unknown);
        observer.invalidate();
        assert_eq!(observer.cwd(), Some("/Users/me/d ir/中;x%y"));
        // 非法编码、非 UTF-8、其他协议和缺少路径都不覆盖已知目录。
        for bad in [
            &b"\x1b]7;file://host/bad%zz\x07"[..],
            b"\x1b]7;file://host/%FF\x07",
            b"\x1b]7;kitty-shell-cwd://host/tmp\x07",
            b"\x1b]7;file://host\x07",
        ] {
            observer.advance(bad, |_| {});
        }
        assert_eq!(observer.cwd(), Some("/Users/me/d ir/中;x%y"));
    }

    #[test]
    fn editing_state_is_distinct_from_prompt_drawing_and_is_invalidated_on_input() {
        let mut observer = ShellObserver::default();
        observer.advance(b"\x1b]133;A\x07", |_| {});
        assert_eq!(observer.state(), ShellState::PromptDrawing);
        for (wire, continuation, empty) in [
            ("start;1", false, true),
            ("start;0", false, false),
            ("cont;1", true, true),
            ("cont;0", true, false),
        ] {
            let bytes = format!("\x1b]777;next-terminal;zle;1;{wire}\x1b\\");
            // 协议头、参数和结束符都可能被 PTY 分块；逐字节喂入。
            for byte in bytes.bytes() {
                observer.advance(&[byte], |_| {});
            }
            assert_eq!(
                observer.state(),
                ShellState::Editing {
                    continuation,
                    empty
                }
            );
            observer.invalidate();
            assert_eq!(observer.state(), ShellState::Unknown);
        }
        observer.advance(b"\x1b]777;next-terminal;zle;1;inactive\x07", |_| {});
        assert_eq!(observer.state(), ShellState::EditorInactive);
        observer.advance(b"\x1b]133;C\x07", |_| {});
        assert_eq!(observer.state(), ShellState::Running);
        observer.advance(b"\x1b]133;D;1\x07", |_| {});
        assert_eq!(observer.state(), ShellState::Finished(Some(1)));
        observer.advance(b"\x1b]777;next-terminal;zle;1;other\x07", |_| {});
        assert_eq!(observer.state(), ShellState::OtherEditor);
    }

    #[test]
    fn invalid_or_future_editing_messages_do_not_claim_a_primary_prompt() {
        let mut observer = ShellObserver::default();
        let mut marks = Vec::new();
        observer.advance(b"\x1b]777;next-terminal;zle;2;start;1\x07\x1b]777;next-terminal;zle;1;start;2\x07\x1b]777;next-terminal;zle;1;start;1;extra\x07\x1b]777;someone-else;zle;1;start;1\x07", |mark| marks.push(mark));
        assert!(marks.is_empty());
        assert_eq!(observer.state(), ShellState::Unknown);
    }

    #[test]
    fn marks_survive_every_possible_chunk_boundary_and_keep_order() {
        let bytes = b"text\x1b]133;A\x07prompt\x1b]133;C\x1b\\output\x1b]133;D;1\x07";
        let expected = [
            ShellMark::PromptStarted,
            ShellMark::CommandStarted,
            ShellMark::CommandFinished(Some(1)),
        ];
        for split in 0..=bytes.len() {
            let mut observer = ShellObserver::default();
            let mut marks = Vec::new();
            observer.advance(&bytes[..split], |mark| marks.push(mark));
            observer.advance(&bytes[split..], |mark| marks.push(mark));
            assert_eq!(marks, expected);
        }
    }

    #[test]
    fn ordinary_output_and_unrelated_or_invalid_osc_do_not_create_marks() {
        let mut observer = ShellObserver::default();
        let mut marks = Vec::new();
        observer.advance(b"133;A hello\x1b]0;title\x07\x1b]133;B\x07\x1b]133;D;no\x07\x1b]133;D;256\x07\x1b]133;A;extra\x07", |mark| marks.push(mark));
        assert!(marks.is_empty());
        assert_eq!(observer.state(), ShellState::Unknown);
        observer.advance(b"\x1b]133;D\x1b\\", |mark| marks.push(mark));
        assert_eq!(marks, [ShellMark::CommandFinished(None)]);
    }
}
