//! 单窗口输入权限。这里不依赖 GPUI，也不从提示符文字或终端回显猜 shell 状态。
use crate::shell_control::{ControlEvent, ReadyRequest};
use crate::shell_observer::ShellState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Starting,
    Unknown,
    /// 未接入集成时，用户明确确认了空的主提示符。
    PromptConfirmed,
    PromptReady,
    /// 只知道命令已入队，不宣称程序已经开始或结束。
    CommandSubmitted,
    Closing,
    Exited,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMode {
    Direct,
    Composer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputTarget {
    Terminal,
    Editor,
    Blocked,
}

pub struct InputControl {
    state: SessionState,
    mode: InputMode,
    alternate_screen: bool,
    connected: bool,
    automatic: bool,
    observed: ShellState,
    pending: Option<ReadyRequest>,
    next_request: u64,
    grant: Option<u64>,
}

impl Default for InputControl {
    fn default() -> Self {
        Self {
            state: SessionState::Starting,
            mode: InputMode::Direct,
            alternate_screen: false,
            connected: false,
            automatic: true,
            observed: ShellState::Unknown,
            pending: None,
            next_request: 0,
            grant: None,
        }
    }
}

impl InputControl {
    pub fn state(&self) -> SessionState {
        self.state
    }
    pub fn mode(&self) -> InputMode {
        self.mode
    }

    pub fn started(&mut self) {
        if self.state == SessionState::Starting {
            self.state = SessionState::Unknown;
        }
    }

    pub fn is_live(&self) -> bool {
        matches!(
            self.state,
            SessionState::Unknown
                | SessionState::PromptConfirmed
                | SessionState::PromptReady
                | SessionState::CommandSubmitted
        )
    }

    pub fn can_resize(&self) -> bool {
        self.state == SessionState::Starting || self.is_live()
    }

    pub fn can_confirm_prompt(&self) -> bool {
        self.is_live() && !self.alternate_screen && (!self.connected || self.empty_primary())
    }

    fn empty_primary(&self) -> bool {
        self.observed
            == ShellState::Editing {
                continuation: false,
                empty: true,
            }
    }

    pub fn integrated(&self) -> bool {
        self.connected
    }
    pub fn automatic(&self) -> bool {
        self.automatic
    }
    pub fn submission_generation(&self) -> Option<u64> {
        self.grant
    }

    fn revoke(&mut self) {
        self.pending = None;
        self.grant = None;
        self.mode = InputMode::Direct;
        if matches!(
            self.state,
            SessionState::PromptConfirmed | SessionState::PromptReady
        ) {
            self.state = SessionState::Unknown;
        }
    }

    pub fn observe_generation(&mut self, generation: u64) {
        if self.grant.is_some_and(|grant| grant != generation)
            || self
                .pending
                .is_some_and(|pending| pending.generation != generation)
        {
            self.revoke();
            self.observed = ShellState::Unknown;
        }
    }

    pub fn next_check(&mut self, generation: u64) -> Option<ReadyRequest> {
        self.observe_generation(generation);
        if !self.is_live()
            || !self.connected
            || !self.automatic
            || self.alternate_screen
            || !self.empty_primary()
            || self.pending.is_some()
            || self.can_submit()
        {
            return None;
        }
        self.next_request += 1;
        let request = ReadyRequest {
            request: self.next_request,
            generation,
        };
        self.pending = Some(request);
        Some(request)
    }

    pub fn observe_control(&mut self, event: ControlEvent, generation: u64) {
        if !self.is_live() {
            return;
        }
        self.observe_generation(generation);
        match event {
            ControlEvent::Connected => {
                self.connected = true;
                self.observed = ShellState::Unknown;
            }
            ControlEvent::Disconnected => {
                self.connected = false;
                self.revoke();
                self.observed = ShellState::Unknown;
            }
            ControlEvent::State(state) if self.connected => {
                self.observed = state;
                if !self.empty_primary() {
                    self.revoke();
                }
            }
            ControlEvent::Checked {
                request,
                generation: replied_generation,
                ready,
            } => {
                let reply = ReadyRequest {
                    request,
                    generation: replied_generation,
                };
                if self.pending != Some(reply) {
                    return;
                }
                self.pending = None;
                if ready
                    && replied_generation == generation
                    && self.connected
                    && self.automatic
                    && !self.alternate_screen
                    && self.empty_primary()
                {
                    self.grant = Some(generation);
                    self.state = SessionState::PromptReady;
                    self.mode = InputMode::Composer;
                } else {
                    self.observed = ShellState::Unknown;
                }
            }
            _ => {}
        }
    }

    pub fn confirm_empty_prompt(&mut self) -> Result<(), &'static str> {
        if !self.is_live() {
            return Err("会话尚不可输入或已经结束");
        }
        if self.alternate_screen {
            return Err("备用屏幕正在使用，保持终端直接交互");
        }
        self.automatic = true;
        if self.connected {
            if !self.empty_primary() {
                return Err("shell 尚未处于空主提示符");
            }
            // 集成存在时，按钮也必须走独立通道检查，不能跳过握手授权。
            return Ok(());
        }
        self.state = SessionState::PromptConfirmed;
        self.mode = InputMode::Composer;
        Ok(())
    }

    pub fn use_terminal(&mut self) {
        self.automatic = false;
        self.revoke();
    }

    pub fn observe_screen(&mut self, alternate_screen: bool) {
        self.alternate_screen = alternate_screen;
        if alternate_screen {
            self.revoke();
            self.observed = ShellState::Unknown;
        }
        // 离开备用屏幕不意味着已回到主提示符。
    }

    pub fn can_submit(&self) -> bool {
        matches!(
            self.state,
            SessionState::PromptConfirmed | SessionState::PromptReady
        ) && self.mode == InputMode::Composer
            && !self.alternate_screen
    }

    pub fn submitted(&mut self) -> Result<(), &'static str> {
        if !self.can_submit() {
            return Err("尚未确认空主提示符，命令未提交");
        }
        self.state = SessionState::CommandSubmitted;
        self.mode = InputMode::Direct;
        self.grant = None;
        self.pending = None;
        self.observed = ShellState::Unknown;
        Ok(())
    }

    pub fn can_write_direct(&self) -> bool {
        self.is_live() && self.mode == InputMode::Direct
    }

    pub fn direct_input_sent(&mut self) {
        if self.can_write_direct() {
            self.revoke();
            self.observed = ShellState::Unknown;
            self.state = SessionState::Unknown;
        }
    }

    /// 焦点只选定键盘目标，不能授予提交权限或绕过会话生命周期。
    pub fn keyboard_target(&self, editor_focused: bool, terminal_focused: bool) -> InputTarget {
        if self.can_submit() && editor_focused {
            InputTarget::Editor
        } else if self.can_write_direct() && terminal_focused {
            InputTarget::Terminal
        } else {
            InputTarget::Blocked
        }
    }

    /// 选择终端输出后粘贴，仍应回到命令框；不能因焦点改变而写入 PTY。
    pub fn paste_target(&self) -> InputTarget {
        if self.can_submit() {
            InputTarget::Editor
        } else if self.can_write_direct() {
            InputTarget::Terminal
        } else {
            InputTarget::Blocked
        }
    }

    pub fn begin_close(&mut self) {
        self.revoke();
        if !matches!(self.state, SessionState::Exited | SessionState::Failed) {
            self.state = SessionState::Closing;
        }
        self.mode = InputMode::Direct;
    }

    pub fn exited(&mut self) {
        self.revoke();
        self.state = SessionState::Exited;
        self.mode = InputMode::Direct;
    }
    pub fn failed(&mut self) {
        self.revoke();
        self.state = SessionState::Failed;
        self.mode = InputMode::Direct;
    }

    pub fn label(&self) -> &'static str {
        match self.state {
            SessionState::Starting => "启动中",
            SessionState::Unknown if !self.automatic => "普通终端模式 · 自动恢复已暂停",
            SessionState::Unknown if self.pending.is_some() => "正在确认空主提示符",
            SessionState::Unknown if self.connected => "等待 shell 主提示符",
            SessionState::Unknown => "shell 状态未知 · 可手动确认",
            SessionState::PromptConfirmed => "空提示符已手动确认",
            SessionState::PromptReady => "空主提示符已确认 · 自动模式",
            SessionState::CommandSubmitted => "命令已提交 · 终端直接交互",
            SessionState::Closing => "关闭中",
            SessionState::Exited => "已退出",
            SessionState::Failed => "会话错误",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn integrated() -> InputControl {
        let mut input = live();
        input.observe_control(ControlEvent::Connected, 0);
        input.observe_control(
            ControlEvent::State(ShellState::Editing {
                continuation: false,
                empty: true,
            }),
            0,
        );
        input
    }

    fn acknowledge(input: &mut InputControl, request: ReadyRequest, current: u64, ready: bool) {
        input.observe_control(
            ControlEvent::Checked {
                request: request.request,
                generation: request.generation,
                ready,
            },
            current,
        );
    }

    #[test]
    fn independent_state_only_requests_a_check_and_matching_ack_authorizes_once() {
        let mut input = integrated();
        assert!(!input.can_submit());
        let request = input.next_check(0).unwrap();
        assert!(input.next_check(0).is_none());
        acknowledge(&mut input, request, 0, true);
        assert!(input.can_submit());
        assert_eq!(input.submission_generation(), Some(0));
        input.submitted().unwrap();
        acknowledge(&mut input, request, 0, true);
        assert!(!input.can_submit());
    }

    #[test]
    fn late_reply_and_generation_changes_cannot_restore_or_reuse_permission() {
        let mut input = integrated();
        let request = input.next_check(0).unwrap();
        input.direct_input_sent();
        acknowledge(&mut input, request, 1, true);
        assert!(!input.can_submit());
        input.observe_control(
            ControlEvent::State(ShellState::Editing {
                continuation: false,
                empty: true,
            }),
            1,
        );
        let next = input.next_check(1).unwrap();
        acknowledge(&mut input, request, 1, true);
        assert!(!input.can_submit());
        acknowledge(&mut input, next, 1, true);
        assert!(input.can_submit());
        input.observe_generation(2); // 包括 Alacritty 查询回复这样的后台写入。
        assert!(!input.can_submit());
    }

    #[test]
    fn manual_direct_mode_persists_across_prompts_until_explicitly_resumed() {
        let mut input = integrated();
        let request = input.next_check(0).unwrap();
        acknowledge(&mut input, request, 0, true);
        input.use_terminal();
        input.observe_control(
            ControlEvent::State(ShellState::Editing {
                continuation: false,
                empty: true,
            }),
            0,
        );
        assert!(input.next_check(0).is_none());
        assert!(input.can_write_direct());
        input.confirm_empty_prompt().unwrap();
        assert!(!input.can_submit());
        let request = input.next_check(0).unwrap();
        acknowledge(&mut input, request, 0, true);
        assert!(input.can_submit());
    }

    #[test]
    fn busy_continuation_other_editor_and_alternate_screen_do_not_grant_composer() {
        for state in [
            ShellState::Running,
            ShellState::EditorInactive,
            ShellState::PromptDrawing,
            ShellState::OtherEditor,
            ShellState::Editing {
                continuation: true,
                empty: true,
            },
            ShellState::Editing {
                continuation: false,
                empty: false,
            },
        ] {
            let mut input = integrated();
            let request = input.next_check(0).unwrap();
            input.observe_control(ControlEvent::State(state), 0);
            acknowledge(&mut input, request, 0, true);
            assert!(!input.can_submit());
            assert!(input.next_check(0).is_none());
        }
        let mut input = integrated();
        let request = input.next_check(0).unwrap();
        input.observe_screen(true);
        acknowledge(&mut input, request, 0, true);
        input.observe_screen(false);
        assert!(!input.can_submit());
        assert!(input.automatic()); // Vim 不是用户主动锁定普通终端模式。
    }

    #[test]
    fn rejected_disconnected_and_closed_checks_remain_direct() {
        let mut input = integrated();
        let request = input.next_check(0).unwrap();
        acknowledge(&mut input, request, 0, false); // 拒绝或超时。
        assert!(!input.can_submit());
        acknowledge(&mut input, request, 0, true);
        assert!(!input.can_submit());
        input.observe_control(ControlEvent::Disconnected, 0);
        assert!(!input.can_submit());
        assert!(input.can_write_direct());
        input.exited();
        input.observe_control(ControlEvent::Connected, 0);
        acknowledge(&mut input, request, 0, true);
        assert!(!input.can_submit());
        assert!(!input.can_write_direct());
        for state in [SessionState::Closing, SessionState::Failed] {
            let mut input = integrated();
            let request = input.next_check(0).unwrap();
            match state {
                SessionState::Closing => input.begin_close(),
                _ => input.failed(),
            }
            acknowledge(&mut input, request, 0, true);
            input.observe_control(ControlEvent::Connected, 0);
            assert_eq!(input.state(), state);
            assert!(!input.can_submit());
            assert!(!input.can_write_direct());
        }
    }

    fn live() -> InputControl {
        let mut input = InputControl::default();
        input.started();
        input
    }

    #[test]
    fn spawning_a_shell_does_not_grant_submission_permission() {
        let mut input = InputControl::default();
        assert!(!input.can_write_direct());
        assert!(input.can_resize());
        assert!(input.confirm_empty_prompt().is_err());
        input.started();
        assert_eq!(input.state(), SessionState::Unknown);
        assert!(input.can_write_direct());
        assert!(input.submitted().is_err());
        assert_eq!(input.keyboard_target(true, false), InputTarget::Blocked);
    }

    #[test]
    fn one_confirmation_authorizes_only_one_submission() {
        let mut input = live();
        input.confirm_empty_prompt().unwrap();
        assert!(input.can_submit());
        assert!(!input.can_write_direct());
        input.submitted().unwrap();
        assert_eq!(input.state(), SessionState::CommandSubmitted);
        assert_eq!(input.mode(), InputMode::Direct);
        assert!(input.submitted().is_err());
        assert_eq!(input.paste_target(), InputTarget::Terminal);
    }

    #[test]
    fn selecting_output_changes_focus_but_cannot_reroute_a_draft_to_pty() {
        let mut input = live();
        input.confirm_empty_prompt().unwrap();
        assert_eq!(input.keyboard_target(true, false), InputTarget::Editor);
        assert_eq!(input.keyboard_target(false, true), InputTarget::Blocked);
        assert_eq!(input.paste_target(), InputTarget::Editor);
        assert!(input.can_submit()); // 执行按钮不要求编辑器有焦点。
        input.use_terminal();
        assert!(!input.can_submit());
        assert_eq!(input.keyboard_target(false, true), InputTarget::Terminal);
        input.direct_input_sent();
        assert_eq!(input.state(), SessionState::Unknown);
    }

    #[test]
    fn alternate_screen_revokes_confirmation_without_restoring_it_on_exit() {
        let mut input = live();
        input.confirm_empty_prompt().unwrap();
        input.observe_screen(true);
        assert!(!input.can_submit());
        assert!(input.confirm_empty_prompt().is_err());
        assert!(input.can_write_direct());
        input.observe_screen(false);
        assert_eq!(input.state(), SessionState::Unknown);
        assert!(!input.can_submit());
        input.confirm_empty_prompt().unwrap();
        assert!(input.can_submit());
    }

    #[test]
    fn exit_failure_and_close_block_every_input_route_and_late_start_events() {
        for state in [
            SessionState::Closing,
            SessionState::Exited,
            SessionState::Failed,
        ] {
            let mut input = live();
            input.confirm_empty_prompt().unwrap();
            match state {
                SessionState::Closing => input.begin_close(),
                SessionState::Exited => input.exited(),
                SessionState::Failed => input.failed(),
                _ => unreachable!(),
            }
            input.started();
            input.observe_screen(false);
            assert_eq!(input.state(), state);
            assert_eq!(input.mode(), InputMode::Direct);
            assert_eq!(input.keyboard_target(false, true), InputTarget::Blocked);
            assert_eq!(input.paste_target(), InputTarget::Blocked);
            assert!(!input.can_resize());
            assert!(input.submitted().is_err());
            assert!(input.confirm_empty_prompt().is_err());
        }
    }
}
