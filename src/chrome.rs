//! 界面外框的展示规则：状态色调与路径缩写。只读状态，不改变输入权限。
use crate::theme::{Colors, Rgb};
use next_terminal::input_control::{InputControl, SessionState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Ready,
    Busy,
    Idle,
    Error,
}

impl Tone {
    pub fn color(self, colors: &Colors) -> Rgb {
        match self {
            Tone::Ready => colors.success,
            Tone::Busy => colors.warning,
            Tone::Idle => colors.text_faint,
            Tone::Error => colors.danger,
        }
    }
}

pub fn tone(control: &InputControl) -> Tone {
    match control.state() {
        SessionState::PromptReady | SessionState::PromptConfirmed => Tone::Ready,
        SessionState::CommandSubmitted => Tone::Busy,
        SessionState::Failed => Tone::Error,
        SessionState::Starting
        | SessionState::Unknown
        | SessionState::Closing
        | SessionState::Exited => Tone::Idle,
    }
}

/// 家目录及其子目录显示为 `~`，与 shell 提示符习惯一致。
pub fn display_path(path: &str, home: Option<&str>) -> String {
    let Some(home) = home
        .map(|home| home.trim_end_matches('/'))
        .filter(|home| !home.is_empty())
    else {
        return path.to_owned();
    };
    if path == home {
        "~".to_owned()
    } else if let Some(rest) = path.strip_prefix(home).filter(|rest| rest.starts_with('/')) {
        format!("~{rest}")
    } else {
        path.to_owned()
    }
}

/// 从 shell 路径取出程序名，例如 `/bin/zsh` → `zsh`。
pub fn shell_name(shell: &str) -> &str {
    shell.rsplit('/').next().unwrap_or(shell)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_is_abbreviated_only_on_path_boundaries() {
        let home = Some("/Users/me");
        assert_eq!(display_path("/Users/me", home), "~");
        assert_eq!(display_path("/Users/me/work/a b", home), "~/work/a b");
        assert_eq!(display_path("/Users/meow", home), "/Users/meow");
        assert_eq!(display_path("/tmp", home), "/tmp");
        assert_eq!(display_path("/Users/me/x", Some("/Users/me/")), "~/x");
        assert_eq!(display_path("/x", Some("")), "/x");
        assert_eq!(display_path("/x", None), "/x");
    }

    #[test]
    fn tone_follows_session_state() {
        let mut control = InputControl::default();
        assert_eq!(tone(&control), Tone::Idle);
        control.started();
        control.confirm_empty_prompt().unwrap();
        assert_eq!(tone(&control), Tone::Ready);
        control.submitted().unwrap();
        assert_eq!(tone(&control), Tone::Busy);
        control.failed();
        assert_eq!(tone(&control), Tone::Error);
        assert_eq!(shell_name("/bin/zsh"), "zsh");
        assert_eq!(shell_name("zsh"), "zsh");
    }
}
