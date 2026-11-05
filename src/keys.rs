//! 键盘、粘贴和滚轮到终端字节／行数的转换。纯函数，不持有窗口或会话状态。
use gpui::Keystroke;

/// 把一次按键编码为写入 PTY 的字节；无法表示的组合返回 `None`。
pub fn key_bytes(key: &Keystroke, application_cursor: bool) -> Option<Vec<u8>> {
    if key.modifiers.control {
        let byte = match key.key.as_str() {
            "space" | "@" => 0,
            "[" => 27,
            "\\" => 28,
            "]" => 29,
            "^" => 30,
            "_" => 31,
            name if name.len() == 1 && name.as_bytes()[0].is_ascii_alphabetic() => {
                name.as_bytes()[0].to_ascii_uppercase() - b'@'
            }
            _ => return None,
        };
        return Some(vec![byte]);
    }
    let cursor = |normal: &'static [u8], application: &'static [u8]| {
        if application_cursor {
            application
        } else {
            normal
        }
    };
    let special: &[u8] = match key.key.as_str() {
        "enter" => b"\r",
        "backspace" => b"\x7f",
        "tab" => b"\t",
        "escape" => b"\x1b",
        "space" => b" ",
        "delete" => b"\x1b[3~",
        "up" => cursor(b"\x1b[A", b"\x1bOA"),
        "down" => cursor(b"\x1b[B", b"\x1bOB"),
        "right" => cursor(b"\x1b[C", b"\x1bOC"),
        "left" => cursor(b"\x1b[D", b"\x1bOD"),
        "home" => b"\x1b[H",
        "end" => b"\x1b[F",
        _ => return key.key_char.as_ref().map(|text| text.as_bytes().to_vec()),
    };
    Some(special.to_vec())
}

pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    if bracketed {
        // ESC 不能从粘贴内容中提前结束 bracketed paste。
        format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', "")).into_bytes()
    } else {
        text.replace('\n', "\r").into_bytes()
    }
}

/// 触控板的小位移累计到完整行；反向滚动立即丢弃旧方向的余量。
pub fn scroll_lines(delta: f32, remainder: &mut f32) -> i32 {
    if delta != 0.0 && delta.signum() != remainder.signum() {
        *remainder = 0.0;
    }
    *remainder += delta;
    let lines = remainder.trunc() as i32;
    *remainder -= lines as f32;
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keystroke(source: &str) -> Keystroke {
        Keystroke::parse(source).unwrap()
    }

    #[test]
    fn wheel_lines_and_small_trackpad_deltas_scroll_in_both_directions() {
        use gpui::{ScrollDelta, point, px};
        const LINE_HEIGHT: f32 = 22.0;
        let mut remainder = 0.0;
        for (event, expected) in [
            (ScrollDelta::Lines(point(0.0, 3.0)), 3),
            (ScrollDelta::Pixels(point(px(0.0), px(11.0))), 0),
            (ScrollDelta::Pixels(point(px(0.0), px(11.0))), 1),
            (ScrollDelta::Pixels(point(px(0.0), px(17.0))), 0),
            (ScrollDelta::Pixels(point(px(0.0), px(-11.0))), 0),
            (ScrollDelta::Pixels(point(px(0.0), px(-11.0))), -1),
            (ScrollDelta::Lines(point(0.0, -3.0)), -3),
        ] {
            let delta = f32::from(event.pixel_delta(px(LINE_HEIGHT)).y) / LINE_HEIGHT;
            assert_eq!(scroll_lines(delta, &mut remainder), expected);
        }
    }

    #[test]
    fn paste_normalizes_line_endings_for_the_active_mode() {
        assert_eq!(paste_bytes("a\r\nb\rc\nd", false), b"a\rb\rc\rd");
        assert_eq!(
            paste_bytes("中\r\nb\rc\nd", true),
            "\x1b[200~中\nb\nc\nd\x1b[201~".as_bytes()
        );
    }

    #[test]
    fn bracketed_paste_cannot_be_closed_by_clipboard_escape_sequences() {
        assert_eq!(
            paste_bytes("a\x1b[201~b", true),
            b"\x1b[200~a[201~b\x1b[201~"
        );
    }

    #[test]
    fn control_and_cursor_keys_follow_terminal_modes() {
        assert_eq!(key_bytes(&keystroke("ctrl-c"), false), Some(vec![3]));
        assert_eq!(key_bytes(&keystroke("ctrl-["), false), Some(vec![27]));
        assert_eq!(key_bytes(&keystroke("ctrl-f1"), false), None);
        assert_eq!(key_bytes(&keystroke("up"), false), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes(&keystroke("up"), true), Some(b"\x1bOA".to_vec()));
        assert_eq!(key_bytes(&keystroke("backspace"), false), Some(vec![0x7f]));
        assert_eq!(
            key_bytes(&keystroke("delete"), false),
            Some(b"\x1b[3~".to_vec())
        );
    }
}
