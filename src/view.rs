use crate::editor::CommandEditor;
use alacritty_terminal::Term;
use alacritty_terminal::event::{Event as TerminalEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{TermMode, cell::Flags, viewport_to_point};
use alacritty_terminal::vte::ansi::{Color, Processor};
use gpui::{
    App, BorderStyle, Bounds, ClipboardItem, Context, Entity, FocusHandle, Focusable, Hsla,
    KeyDownEvent, MouseButton, Pixels, Render, SharedString, TextRun, Window, canvas, div, fill,
    font, outline, point, prelude::*, px, rgb, size,
};
use next_terminal::input_control::{InputControl, InputMode, InputTarget};
use next_terminal::pty::{Event, Input, Session, Shutdown};
use next_terminal::screen::GridSize;
use next_terminal::shell_observer::ShellObserver;
use std::time::Duration;

const FONT_SIZE: f32 = 16.0;
const LINE_HEIGHT: f32 = 22.0;
const PADDING: f32 = 16.0;
const GAP: f32 = 8.0;
const STATUS_HEIGHT: f32 = 20.0;
const COMPOSER_HEIGHT: f32 = 112.0;
const DIRECT_HEIGHT: f32 = 32.0;
const BACKGROUND: u32 = 0x181818;
const FOREGROUND: u32 = 0xd4d4d4;

// 光标位置查询等控制序列的回复必须回到同一个 PTY。
struct Replies(Input);
impl EventListener for Replies {
    fn send_event(&self, event: TerminalEvent) {
        if let TerminalEvent::PtyWrite(text) = event {
            let _ = self.0.write(text.as_bytes());
        }
    }
}

pub struct TerminalWindow {
    session: Option<Session>,
    terminal: Option<Term<Replies>>,
    processor: Processor,
    shell_observer: ShellObserver,
    focus: FocusHandle,
    status: SharedString,
    grid_size: GridSize,
    cell_width: Pixels,
    grid_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
    scroll_remainder: f32,
    editor: Entity<CommandEditor>,
    control: InputControl,
    launch_directory: SharedString,
}

impl TerminalWindow {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let editor = cx.new(CommandEditor::new);
        cx.observe(&editor, |_, _, cx| cx.notify()).detach();
        focus.focus(window);
        let cell_width = measure_cell_width(window);
        let grid_size = available_grid_size(window, cell_width, false);
        let mut view = Self {
            session: None,
            terminal: None,
            processor: Processor::default(),
            shell_observer: ShellObserver::default(),
            focus,
            status: "正在启动默认 shell…".into(),
            grid_size,
            cell_width,
            grid_bounds: None,
            selecting: false,
            scroll_remainder: 0.0,
            editor,
            control: InputControl::default(),
            launch_directory: std::env::current_dir()
                .map(|path| path.display().to_string())
                .unwrap_or_default()
                .into(),
        };
        match Session::start_with_size(grid_size) {
            Err(error) => {
                view.control.failed();
                view.status = format!("启动失败：{error:#}").into();
            }
            Ok((session, events)) => {
                view.terminal = Some(next_terminal::screen::with_listener(Replies(
                    session.input.clone(),
                )));
                view.terminal.as_mut().unwrap().resize(grid_size);
                let this = cx.weak_entity();
                window.on_window_should_close(cx, move |window, cx| {
                    let _ = this.update(cx, |this, cx| this.request_close(window, cx));
                    false
                });
                let shutdown = session.shutdown.clone();
                cx.on_app_quit(move |this, _| {
                    this.control.begin_close();
                    let shutdown = shutdown.clone();
                    shutdown.request();
                    async move {
                        shutdown.wait().await;
                    }
                })
                .detach();
                view.session = Some(session);
                let executor = cx.background_executor().clone();
                let window_handle = window.window_handle();
                cx.spawn(async move |this, cx| {
                    while let Ok(first) = events.recv().await {
                        let mut batch = vec![first];
                        // 有界通道 + 每帧合并，既限制积压，又避免逐字节触发重绘。
                        while batch.len() < 64 {
                            match events.try_recv() {
                                Ok(event) => batch.push(event),
                                Err(_) => break,
                            }
                        }
                        if window_handle
                            .update(cx, |_, window, cx| {
                                this.update(cx, |this, cx| {
                                    let previous = this.control.mode();
                                    for event in batch {
                                        this.handle_event(event);
                                    }
                                    this.refresh_control();
                                    this.apply_mode_change(previous, window, cx);
                                    cx.notify();
                                })
                            })
                            .is_err()
                        {
                            break;
                        }
                        executor.timer(Duration::from_millis(16)).await;
                    }
                })
                .detach();
            }
        }
        cx.observe_window_bounds(window, |this, window, cx| {
            this.sync_size(window, cx);
        })
        .detach();
        view
    }

    fn is_editing(&self) -> bool {
        self.control.mode() == InputMode::Composer
    }

    fn refresh_control(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        let generation = session.input.generation();
        if let Some(request) = self.control.next_check(generation) {
            match session.input.check_ready(request) {
                Ok(true) => {}
                Ok(false) | Err(_) => self.control.observe_control(
                    next_terminal::shell_control::ControlEvent::Checked {
                        request: request.request,
                        generation: request.generation,
                        ready: false,
                    },
                    session.input.generation(),
                ),
            }
        }
    }

    fn apply_mode_change(
        &mut self,
        previous: InputMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.control.mode() == previous {
            return;
        }
        if self.is_editing() {
            self.editor.focus_handle(cx).focus(window);
        } else {
            self.focus.focus(window);
        }
        self.sync_size(window, cx);
    }

    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shell_observer.invalidate();
        let previous = self.control.mode();
        self.control.begin_close();
        self.apply_mode_change(previous, window, cx);
        if let Some(session) = &self.session {
            close_after_shell(session.shutdown.clone(), window, cx);
        } else {
            window.remove_window();
        }
    }

    fn sync_size(&mut self, window: &Window, cx: &mut Context<Self>) {
        let cell_width = measure_cell_width(window);
        let size = available_grid_size(window, cell_width, self.is_editing());
        if size != self.grid_size {
            if let Some(session) = &self.session {
                if self.control.can_resize()
                    && let Err(error) = session.input.resize(size)
                {
                    self.status = format!("更新终端尺寸失败：{error}").into();
                    cx.notify();
                    return;
                }
            }
            if let Some(terminal) = &mut self.terminal {
                terminal.selection = None;
                terminal.resize(size);
            }
            self.selecting = false;
            self.scroll_remainder = 0.0;
            self.grid_size = size;
        }
        self.cell_width = cell_width;
        cx.notify();
    }

    fn set_editing(&mut self, editing: bool, window: &mut Window, cx: &mut Context<Self>) {
        let previous = self.control.mode();
        if editing {
            if self.control.confirm_empty_prompt().is_err() {
                // 拒绝原因已由状态栏与直接交互栏说明，不污染隐藏的草稿错误。
                return;
            }
        } else {
            self.control.use_terminal();
        }
        self.refresh_control();
        self.apply_mode_change(previous, window, cx);
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previous = self.control.mode();
        if let Some(session) = &self.session {
            self.control.observe_generation(session.input.generation());
        }
        self.apply_mode_change(previous, window, cx);
        if !self.control.can_submit() {
            return;
        }
        let Some(command) = self.editor.read(cx).command() else {
            return;
        };
        if command.trim().is_empty() {
            return;
        }
        let Some(session) = &self.session else { return };
        let bracketed = self
            .terminal
            .as_ref()
            .is_some_and(|term| term.mode().contains(TermMode::BRACKETED_PASTE));
        let mut bytes = paste_bytes(&command, bracketed);
        bytes.push(b'\r');
        let direct_size = available_grid_size(window, measure_cell_width(window), false);
        let written =
            session
                .input
                .submit(self.control.submission_generation(), direct_size, &bytes);
        if matches!(written, Ok(false)) {
            let previous = self.control.mode();
            self.control.observe_generation(session.input.generation());
            self.apply_mode_change(previous, window, cx);
            cx.notify();
            return;
        }
        if let Err(error) = written {
            let previous = self.control.mode();
            self.control.failed();
            self.status = format!("提交失败：{error}").into();
            self.apply_mode_change(previous, window, cx);
            cx.notify();
            return;
        }
        self.shell_observer.invalidate();
        self.editor.update(cx, |editor, cx| editor.clear(cx));
        if let Some(terminal) = &mut self.terminal {
            terminal.scroll_display(Scroll::Bottom);
        }
        let previous = self.control.mode();
        // 只有成功入队才消费本次确认；同一确认不能重复提交。
        self.control
            .submitted()
            .expect("submission was checked before writing");
        self.apply_mode_change(previous, window, cx);
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Started { shell, pid } => {
                self.control.started();
                self.status = format!("{shell} · PID {}", pid.unwrap_or(0)).into()
            }
            Event::Output(bytes) => {
                self.shell_observer.advance(&bytes, |_| {});
                if let Some(terminal) = &mut self.terminal {
                    self.processor.advance(terminal, &bytes);
                    self.control
                        .observe_screen(terminal.mode().contains(TermMode::ALT_SCREEN));
                }
            }
            Event::ShellControl(event) => {
                if let Some(session) = &self.session {
                    self.control
                        .observe_control(event, session.input.generation());
                }
            }
            Event::Exited(status) => {
                self.shell_observer.invalidate();
                self.control.exited();
                self.status = format!("shell 已退出：{status}").into();
                self.session.take();
            }
            Event::Failed(error) => {
                self.shell_observer.invalidate();
                self.control.failed();
                self.status = format!("错误：{error}").into();
            }
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let previous = self.control.mode();
        if let Some(session) = &self.session {
            self.control.observe_generation(session.input.generation());
        }
        self.apply_mode_change(previous, window, cx);
        let key = &event.keystroke;
        if key.modifiers.platform && key.modifiers.shift && key.key == "e" {
            self.set_editing(!self.is_editing(), window, cx);
            cx.stop_propagation();
            return;
        }
        let target = self.control.keyboard_target(
            self.editor.focus_handle(cx).is_focused(window),
            self.focus.is_focused(window),
        );
        let editor_focused = target == InputTarget::Editor;
        if editor_focused && !(key.modifiers.platform && (key.key == "w" || key.key == "q")) {
            if key.key == "enter" && self.editor.read(cx).command().is_some() {
                self.submit(window, cx);
                cx.stop_propagation();
            } else if key.key == "escape" && self.editor.read(cx).command().is_some() {
                self.set_editing(false, window, cx);
                cx.stop_propagation();
            }
            return;
        }
        if key.modifiers.platform {
            if key.key == "c" {
                if let Some(text) = self.terminal.as_ref().and_then(Term::selection_to_string) {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                cx.stop_propagation();
                return;
            }
            if key.key == "v" {
                if self.control.paste_target() == InputTarget::Editor {
                    self.editor.focus_handle(cx).focus(window);
                    self.editor
                        .update(cx, |editor, cx| editor.paste_clipboard(window, cx));
                    cx.stop_propagation();
                    return;
                }
                if self.control.paste_target() == InputTarget::Terminal
                    && let Some(text) = cx.read_from_clipboard().and_then(|item| item.text())
                {
                    let bracketed = self
                        .terminal
                        .as_ref()
                        .is_some_and(|term| term.mode().contains(TermMode::BRACKETED_PASTE));
                    self.write_input(&paste_bytes(&text, bracketed), cx);
                }
                cx.stop_propagation();
                return;
            }
            if key.key == "w" || key.key == "q" {
                self.request_close(window, cx);
                cx.stop_propagation();
            }
            return;
        }
        if target != InputTarget::Terminal {
            cx.stop_propagation();
            return;
        }
        let bytes = key_bytes(
            event,
            self.terminal
                .as_ref()
                .is_some_and(|term| term.mode().contains(TermMode::APP_CURSOR)),
        );
        if let Some(bytes) = bytes {
            self.write_input(&bytes, cx);
            cx.stop_propagation();
        }
    }

    fn write_input(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        if !self.control.can_write_direct() {
            return;
        }
        if let Some(session) = &self.session {
            if let Err(error) = session.input.write(bytes) {
                self.shell_observer.invalidate();
                self.control.failed();
                self.status = format!("输入失败：{error}").into();
                cx.notify();
                return;
            }
            self.control.direct_input_sent();
            self.shell_observer.invalidate();
            if let Some(terminal) = &mut self.terminal {
                terminal.selection = None;
                terminal.scroll_display(Scroll::Bottom);
            }
            self.selecting = false;
            self.scroll_remainder = 0.0;
            cx.notify();
        }
    }

    fn selection_position(&self, position: gpui::Point<Pixels>) -> Option<(GridPoint, Side)> {
        let bounds = self.grid_bounds?;
        let offset = position - bounds.origin;
        let x = f32::from(offset.x) / f32::from(self.cell_width);
        let row = (f32::from(offset.y) / LINE_HEIGHT)
            .floor()
            .clamp(0.0, self.grid_size.rows as f32 - 1.0) as i32;
        let column = x.floor().clamp(0.0, self.grid_size.columns as f32 - 1.0) as usize;
        let side = if x - (column as f32) < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        let terminal = self.terminal.as_ref()?;
        Some((
            viewport_to_point(
                terminal.grid().display_offset(),
                GridPoint::new(row as usize, Column(column)),
            ),
            side,
        ))
    }

    fn on_scroll_wheel(
        &mut self,
        event: &gpui::ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self
            .grid_bounds
            .is_some_and(|bounds| bounds.contains(&event.position))
        {
            return;
        }
        let Some(terminal) = &mut self.terminal else {
            return;
        };
        // 全屏程序的备用屏幕没有历史；鼠标协议与滚轮转方向键留待后续。
        if terminal.mode().contains(TermMode::ALT_SCREEN) {
            self.scroll_remainder = 0.0;
            return;
        }
        if matches!(event.touch_phase, gpui::TouchPhase::Started) {
            self.scroll_remainder = 0.0;
        }
        let delta = f32::from(event.delta.pixel_delta(px(LINE_HEIGHT)).y) / LINE_HEIGHT;
        let lines = scroll_lines(delta, &mut self.scroll_remainder);
        if lines != 0 {
            let previous = terminal.grid().display_offset();
            terminal.scroll_display(Scroll::Delta(lines));
            if terminal.grid().display_offset() == previous {
                self.scroll_remainder = 0.0;
            }
            self.selecting = false;
            cx.notify();
        }
        cx.stop_propagation();
    }

    fn update_selection(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        if let Some((point, side)) = self.selection_position(position) {
            if let Some(selection) = self
                .terminal
                .as_mut()
                .and_then(|term| term.selection.as_mut())
            {
                selection.update(point, side);
                cx.notify();
            }
        }
    }
}

fn scroll_lines(delta: f32, remainder: &mut f32) -> i32 {
    // 触控板的小位移累计到完整行；反向滚动立即丢弃旧方向的余量。
    if delta != 0.0 && delta.signum() != remainder.signum() {
        *remainder = 0.0;
    }
    *remainder += delta;
    let lines = remainder.trunc() as i32;
    *remainder -= lines as f32;
    lines
}

fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    if bracketed {
        // ESC 不能从粘贴内容中提前结束 bracketed paste。
        format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', "")).into_bytes()
    } else {
        text.replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::{LINE_HEIGHT, paste_bytes, scroll_lines};

    #[test]
    fn wheel_lines_and_small_trackpad_deltas_scroll_in_both_directions() {
        use gpui::{ScrollDelta, point, px};
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
}

fn close_after_shell(shutdown: Shutdown, window: &mut Window, cx: &mut App) {
    shutdown.request();
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        shutdown.wait().await;
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    })
    .detach();
}

fn key_bytes(event: &KeyDownEvent, application_cursor: bool) -> Option<Vec<u8>> {
    let key = &event.keystroke;
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
    let special: &[u8] = match key.key.as_str() {
        "enter" => b"\r",
        "backspace" => b"\x7f",
        "tab" => b"\t",
        "escape" => b"\x1b",
        "space" => b" ",
        "delete" => b"\x1b[3~",
        "up" => {
            if application_cursor {
                b"\x1bOA"
            } else {
                b"\x1b[A"
            }
        }
        "down" => {
            if application_cursor {
                b"\x1bOB"
            } else {
                b"\x1b[B"
            }
        }
        "right" => {
            if application_cursor {
                b"\x1bOC"
            } else {
                b"\x1b[C"
            }
        }
        "left" => {
            if application_cursor {
                b"\x1bOD"
            } else {
                b"\x1b[D"
            }
        }
        "home" => b"\x1b[H",
        "end" => b"\x1b[F",
        _ => return key.key_char.as_ref().map(|text| text.as_bytes().to_vec()),
    };
    Some(special.to_vec())
}

impl Render for TerminalWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(GAP))
            .p(px(PADDING))
            .overflow_hidden()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(FOREGROUND))
            .font_family("Menlo")
            .text_size(px(13.0))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    this.selecting = false;
                    if this
                        .grid_bounds
                        .is_some_and(|bounds| bounds.contains(&event.position))
                    {
                        this.focus.focus(window);
                        if let Some((point, side)) = this.selection_position(event.position) {
                            if let Some(terminal) = &mut this.terminal {
                                terminal.selection =
                                    Some(Selection::new(SelectionType::Simple, point, side));
                                this.selecting = true;
                            }
                        }
                    } else if let Some(terminal) = &mut this.terminal {
                        terminal.selection = None;
                    }
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if this.selecting && event.dragging() {
                    this.update_selection(event.position, cx);
                } else if !event.dragging() {
                    this.selecting = false;
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseUpEvent, _, cx| {
                    if this.selecting {
                        this.update_selection(event.position, cx);
                        this.selecting = false;
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseUpEvent, _, cx| {
                    if this.selecting {
                        this.update_selection(event.position, cx);
                        this.selecting = false;
                    }
                }),
            )
            .child(
                div()
                    .h(px(STATUS_HEIGHT))
                    .w_full()
                    .flex_shrink_0()
                    .line_height(px(STATUS_HEIGHT))
                    .overflow_hidden()
                    .child(format!(
                        "{} × {} · {} · {} · {}{}",
                        self.grid_size.columns,
                        self.grid_size.rows,
                        self.status,
                        self.control.label(),
                        self.shell_observer.label(),
                        self.terminal
                            .as_ref()
                            .map(|terminal| {
                                let offset = terminal.grid().display_offset();
                                if offset == 0 {
                                    String::new()
                                } else {
                                    format!(" · 历史 ↑{offset} 行")
                                }
                            })
                            .unwrap_or_default()
                    )),
            );
        if self.terminal.is_none() {
            return root;
        }
        let cell_width = self.cell_width;
        let this = cx.weak_entity();
        let root = root.child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.grid_bounds = Some(bounds);
                        if let Some(terminal) = &this.terminal {
                            paint_grid(
                                terminal,
                                bounds.origin,
                                cell_width,
                                this.control.can_write_direct(),
                                window,
                                cx,
                            );
                        }
                    });
                },
            )
            .w(cell_width * self.grid_size.columns as f32)
            .h(px(LINE_HEIGHT * self.grid_size.rows as f32))
            .flex_shrink_0(),
        );
        let footer = if self.is_editing() {
            div()
                .w_full()
                .h(px(COMPOSER_HEIGHT))
                .flex_shrink_0()
                .bg(rgb(0x222321))
                .border_t_1()
                .border_color(rgb(0x49463f))
                .border_l_2()
                .border_color(rgb(0xe6b75d))
                .px(px(14.))
                .py(px(7.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(0x96938b))
                        .overflow_hidden()
                        .child(format!("启动目录 · {}", self.launch_directory)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(
                            div()
                                .text_size(px(24.))
                                .text_color(rgb(0xe6b75d))
                                .child("❯"),
                        )
                        .child(div().flex_1().min_w_0().child(self.editor.clone()))
                        .child(
                            div()
                                .px(px(10.))
                                .py(px(4.))
                                .border_1()
                                .border_color(rgb(0x555148))
                                .cursor_pointer()
                                .child("↵")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, window, cx| {
                                        this.submit(window, cx);
                                        cx.stop_propagation();
                                    }),
                                ),
                        ),
                )
                .child(div().text_size(px(10.)).text_color(rgb(0x96938b)).child(
                    self.editor.read(cx).error.clone().unwrap_or_else(|| {
                        "命令编辑 · Enter 执行 · Esc 交给终端 · Cmd+Shift+E 切换".into()
                    }),
                ))
        } else {
            div()
                .w_full()
                .h(px(DIRECT_HEIGHT))
                .flex_shrink_0()
                .border_t_1()
                .border_color(rgb(0x49463f))
                .text_size(px(11.))
                .text_color(rgb(0xaaa69c))
                .flex()
                .items_center()
                .justify_between()
                .child("终端直接交互")
                .child(
                    div()
                        .cursor_pointer()
                        .child(if self.control.is_live() {
                            if self.control.can_confirm_prompt() {
                                if self.control.integrated() {
                                    "恢复自动命令框  ⇧⌘E"
                                } else {
                                    "确认已回到空主提示符，点击编辑命令  ⇧⌘E"
                                }
                            } else {
                                "保持终端输入 · 等待空主提示符"
                            }
                        } else {
                            "会话尚不可输入或已经结束"
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                this.set_editing(true, window, cx);
                                cx.stop_propagation();
                            }),
                        ),
                )
        };
        root.child(div().flex_1()).child(footer)
    }
}

fn measure_cell_width(window: &Window) -> Pixels {
    window
        .text_system()
        .shape_line(
            "M".into(),
            px(FONT_SIZE),
            &[text_run("M", rgb(FOREGROUND).into(), Flags::empty())],
            None,
        )
        .width
}

fn available_grid_size(window: &Window, cell_width: Pixels, editing: bool) -> GridSize {
    let viewport = window.viewport_size();
    GridSize::from_pixels(
        f32::from(viewport.width) - PADDING * 2.0,
        f32::from(viewport.height)
            - PADDING * 2.0
            - STATUS_HEIGHT
            - GAP * 3.0
            - if editing {
                COMPOSER_HEIGHT
            } else {
                DIRECT_HEIGHT
            },
        f32::from(cell_width),
        LINE_HEIGHT,
    )
}

fn text_run(text: &str, color: Hsla, flags: Flags) -> TextRun {
    let mut face = font("Menlo");
    if flags.contains(Flags::BOLD) {
        face = face.bold();
    }
    if flags.contains(Flags::ITALIC) {
        face = face.italic();
    }
    TextRun {
        len: text.len(),
        font: face,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

fn paint_grid(
    terminal: &Term<Replies>,
    origin: gpui::Point<Pixels>,
    cell_width: Pixels,
    show_cursor: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let selection = terminal
        .selection
        .as_ref()
        .and_then(|selection| selection.to_range(terminal));
    let display_offset = terminal.grid().display_offset();
    // 先画背景，再画字形，避免后一个占位格的背景覆盖中文的右半边。
    for row in 0..terminal.screen_lines() {
        for column in 0..terminal.columns() {
            let grid_point = viewport_to_point(display_offset, GridPoint::new(row, Column(column)));
            let cell = &terminal.grid()[grid_point.line][grid_point.column];
            let background = if cell.flags.contains(Flags::INVERSE) {
                cell.fg
            } else {
                cell.bg
            };
            let bounds = Bounds::new(
                origin + point(cell_width * column as f32, px(LINE_HEIGHT * row as f32)),
                size(cell_width, px(LINE_HEIGHT)),
            );
            let selected = selection.is_some_and(|range| {
                let point = grid_point;
                range.contains(point)
                    || (cell.flags.contains(Flags::WIDE_CHAR)
                        && range.contains(GridPoint::new(point.line, Column(column + 1))))
            });
            window.paint_quad(fill(
                bounds,
                if selected {
                    rgb(0x365880).into()
                } else {
                    resolve_color(background, terminal)
                },
            ));
        }
    }
    for row in 0..terminal.screen_lines() {
        for column in 0..terminal.columns() {
            let grid_point = viewport_to_point(display_offset, GridPoint::new(row, Column(column)));
            let cell = &terminal.grid()[grid_point.line][grid_point.column];
            if cell.flags.intersects(
                Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER | Flags::HIDDEN,
            ) || cell.c == '\t'
            {
                continue;
            }
            let mut text = cell.c.to_string();
            if let Some(extra) = cell.zerowidth() {
                text.extend(extra);
            }
            if text == " " {
                continue;
            }
            let foreground = if cell.flags.contains(Flags::INVERSE) {
                cell.bg
            } else {
                cell.fg
            };
            let run = text_run(&text, resolve_color(foreground, terminal), cell.flags);
            let line = window
                .text_system()
                .shape_line(text.into(), px(FONT_SIZE), &[run], None);
            let position = origin + point(cell_width * column as f32, px(LINE_HEIGHT * row as f32));
            // 不用字符串宽度定位下一格；每个字形始终回到网格给出的列位置。
            if let Err(error) = line.paint(position, px(LINE_HEIGHT), window, cx) {
                eprintln!("字形绘制失败：{error}");
            }
        }
    }
    if show_cursor && display_offset == 0 && terminal.mode().contains(TermMode::SHOW_CURSOR) {
        let cursor = terminal.grid().cursor.point;
        let cell = &terminal.grid()[cursor.line][cursor.column];
        let width = if cell.flags.contains(Flags::WIDE_CHAR) {
            cell_width * 2.0
        } else {
            cell_width
        };
        let bounds = Bounds::new(
            origin
                + point(
                    cell_width * cursor.column.0 as f32,
                    px(LINE_HEIGHT * cursor.line.0 as f32),
                ),
            size(width, px(LINE_HEIGHT)),
        );
        // 描边不遮住格内文字；暂不闪烁。
        window.paint_quad(outline(bounds, rgb(0xf5c969), BorderStyle::Solid));
    }
}

fn resolve_color(color: Color, terminal: &Term<Replies>) -> Hsla {
    let index = match color {
        Color::Spec(value) => {
            return rgb((value.r as u32) << 16 | (value.g as u32) << 8 | value.b as u32).into();
        }
        Color::Named(value) => value as usize,
        Color::Indexed(value) => value as usize,
    };
    if let Some(value) = terminal.colors()[index] {
        return rgb((value.r as u32) << 16 | (value.g as u32) << 8 | value.b as u32).into();
    }
    rgb(palette(index)).into()
}

fn palette(index: usize) -> u32 {
    const ANSI: [u32; 16] = [
        0x181818, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666,
        0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
    ];
    match index {
        0..=15 => ANSI[index],
        16..=231 => {
            let value = index - 16;
            let level = |x| if x == 0 { 0 } else { 55 + 40 * x as u32 };
            level(value / 36) << 16 | level((value / 6) % 6) << 8 | level(value % 6)
        }
        232..=255 => {
            let value = 8 + 10 * (index - 232) as u32;
            value << 16 | value << 8 | value
        }
        257 => BACKGROUND,
        258 => 0xf5c969,
        259..=266 => (ANSI[index - 259] & 0xfefefe) >> 1,
        268 => 0x888888,
        _ => FOREGROUND,
    }
}
