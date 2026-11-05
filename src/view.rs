//! 单窗口终端视图：组合标题栏、终端网格、命令框和状态栏，并把输入事件交给会话。
//! 绘制细节在 `grid`，按键编码在 `keys`，颜色尺寸在 `theme`，展示规则在 `chrome`。
use crate::chrome::{self, display_path, shell_name};
use crate::editor::CommandEditor;
use crate::grid::{self, CellMetrics, CursorFocus, GridMetrics};
use crate::ime::Preedit;
use crate::keys::{is_text_input, key_bytes, paste_bytes, scroll_lines};
use crate::theme::{Metrics, Theme, color, tint};
use alacritty_terminal::Term;
use alacritty_terminal::event::{Event as TerminalEvent, EventListener};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{TermMode, viewport_to_point};
use alacritty_terminal::vte::ansi::Processor;
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, Div, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, Hsla, KeyDownEvent, MouseButton, MouseDownEvent,
    Pixels, Render, SharedString, UTF16Selection, Window, canvas, div, point, prelude::*, px,
};
use next_terminal::input_control::{InputControl, InputMode, InputTarget};
use next_terminal::pty::{Event, Input, Session, Shutdown};
use next_terminal::screen::GridSize;
use next_terminal::shell_observer::ShellObserver;
use std::ops::Range;
use std::time::Duration;

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
    /// 启动失败、退出和输入错误等需要用户注意的消息；正常运行时为空。
    notice: Option<SharedString>,
    shell: SharedString,
    grid_size: GridSize,
    cell: CellMetrics,
    grid_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
    scroll_remainder: f32,
    editor: Entity<CommandEditor>,
    control: InputControl,
    launch_directory: String,
    home: Option<String>,
    window_title: String,
    theme: Theme,
    /// 直接终端模式下输入法正在组字的文字；上屏前不写入 PTY。
    preedit: Preedit,
}

impl TerminalWindow {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let theme = Theme::global(cx).clone();
        let focus = cx.focus_handle();
        let editor = cx.new(CommandEditor::new);
        cx.observe(&editor, |_, _, cx| cx.notify()).detach();
        // 主题是全局令牌；以后设置页替换主题时，视图重新测量格宽并重排。
        cx.observe_global_in::<Theme>(window, |this, window, cx| {
            this.theme = Theme::global(cx).clone();
            this.sync_size(window, cx);
        })
        .detach();
        focus.focus(window);
        let cell = CellMetrics::measure(window, &theme);
        let grid_size = available_grid_size(window, cell.width, &theme.metrics, false);
        let mut view = Self {
            session: None,
            terminal: None,
            processor: Processor::default(),
            shell_observer: ShellObserver::default(),
            focus,
            notice: None,
            shell: SharedString::default(),
            grid_size,
            cell,
            grid_bounds: None,
            selecting: false,
            scroll_remainder: 0.0,
            editor,
            control: InputControl::default(),
            launch_directory: next_terminal::pty::launch_directory().display().to_string(),
            home: std::env::var("HOME").ok(),
            window_title: String::new(),
            theme,
            preedit: Preedit::default(),
        };
        view.update_window_title(window);
        match Session::start_with_size(grid_size) {
            Err(error) => {
                view.control.failed();
                view.notice = Some(format!("启动失败：{error:#}").into());
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
                                    this.update_window_title(window);
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
        cx.observe_window_activation(window, |_, _, cx| cx.notify())
            .detach();
        view
    }

    fn is_editing(&self) -> bool {
        self.control.mode() == InputMode::Composer
    }

    /// 当前目录优先使用 shell 的 OSC 7 报告，尚未报告时显示启动目录。
    fn directory(&self) -> String {
        display_path(
            self.shell_observer
                .cwd()
                .unwrap_or(self.launch_directory.as_str()),
            self.home.as_deref(),
        )
    }

    fn update_window_title(&mut self, window: &mut Window) {
        let title = self.directory();
        if title != self.window_title {
            window.set_window_title(&title);
            self.window_title = title;
        }
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
        // 输入目标改变时丢弃未上屏的组字，避免它在另一种模式下被写入。
        self.preedit.clear();
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
        let cell = CellMetrics::measure(window, &self.theme);
        let size = available_grid_size(window, cell.width, &self.theme.metrics, self.is_editing());
        if size != self.grid_size {
            if let Some(session) = &self.session {
                if self.control.can_resize()
                    && let Err(error) = session.input.resize(size)
                {
                    self.notice = Some(format!("更新终端尺寸失败：{error}").into());
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
        self.cell = cell;
        cx.notify();
    }

    fn set_editing(&mut self, editing: bool, window: &mut Window, cx: &mut Context<Self>) {
        let previous = self.control.mode();
        if editing {
            if self.control.confirm_empty_prompt().is_err() {
                // 拒绝原因已由状态栏说明，不污染隐藏的草稿错误。
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
        let direct_size = available_grid_size(
            window,
            CellMetrics::measure(window, &self.theme).width,
            &self.theme.metrics,
            false,
        );
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
            self.notice = Some(format!("提交失败：{error}").into());
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
            Event::Started { shell, .. } => {
                self.control.started();
                self.shell = shell_name(&shell).to_owned().into();
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
                self.preedit.clear();
                self.shell_observer.invalidate();
                self.control.exited();
                self.notice = Some(format!("shell 已退出：{status}").into());
                self.session.take();
            }
            Event::Failed(error) => {
                self.preedit.clear();
                self.shell_observer.invalidate();
                self.control.failed();
                self.notice = Some(format!("错误：{error}").into());
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
        // 可见字符交给平台输入法：拼音等先组字，上屏后经 replace_text_in_range 写入 PTY。
        // 长按重复的按键仍在这里直接写入，避免 macOS 弹出重音字符菜单而不能连续输入。
        if is_text_input(key) && !event.is_held {
            return;
        }
        let bytes = key_bytes(
            key,
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
                self.notice = Some(format!("输入失败：{error}").into());
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
        let x = f32::from(offset.x) / f32::from(self.cell.width);
        let row = (f32::from(offset.y) / self.theme.metrics.line_height)
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
        let line_height = self.theme.metrics.line_height;
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
        let delta = f32::from(event.delta.pixel_delta(px(line_height)).y) / line_height;
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

    fn grid_metrics(&self, origin: gpui::Point<Pixels>, cell: CellMetrics) -> GridMetrics {
        GridMetrics {
            origin,
            cell,
            line_height: px(self.theme.metrics.line_height),
            font_size: px(self.theme.metrics.font_size),
        }
    }

    /// 终端光标所在格子的左上角（窗口坐标）；光标不在可见区域时返回 None。
    fn cursor_origin(&self) -> Option<gpui::Point<Pixels>> {
        let bounds = self.grid_bounds?;
        let terminal = self.terminal.as_ref()?;
        let cursor = terminal.grid().cursor.point;
        let row = cursor.line.0 + terminal.grid().display_offset() as i32;
        if row < 0 || row as usize >= self.grid_size.rows as usize {
            return None;
        }
        Some(
            bounds.origin
                + point(
                    self.cell.width * cursor.column.0 as f32,
                    px(self.theme.metrics.line_height * row as f32),
                ),
        )
    }

    fn history_offset(&self) -> usize {
        self.terminal
            .as_ref()
            .map_or(0, |terminal| terminal.grid().display_offset())
    }
}

/// 直接终端模式的平台输入法接口。终端没有可编辑文档，范围只针对组字文字；
/// 上屏文本与普通按键一样经 `write_input` 写入，仍受输入权限约束。
impl EntityInputHandler for TerminalWindow {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let (text, actual) = self.preedit.text_for_range(range_utf16);
        actual_range.replace(actual);
        Some(text)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.preedit.selected_range_utf16(),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.preedit.marked_range_utf16()
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.preedit.clear();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.preedit.clear();
        if !text.is_empty() {
            self.write_input(text.as_bytes(), cx);
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range_utf16: Option<Range<usize>>,
        text: &str,
        selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.control.can_write_direct() {
            self.preedit.set(text, selected_range_utf16);
        } else {
            self.preedit.clear();
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let origin = self.cursor_origin()?;
        let metrics = self.grid_metrics(origin, self.cell);
        let line_height = metrics.line_height;
        let line = grid::shape_preedit(self.preedit.text(), &metrics, &self.theme, window);
        let range = self.preedit.utf8_range(range_utf16);
        Some(Bounds::from_corners(
            origin + point(line.x_for_index(range.start), px(0.)),
            origin + point(line.x_for_index(range.end), line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
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

// ---------------------------------------------------------------------------
// 界面组合

impl TerminalWindow {
    /// 透明标题栏：macOS 左侧留出红绿灯位置，标题居中显示当前目录。
    fn render_title_bar(&self) -> impl IntoElement {
        let theme = &self.theme;
        let metrics = theme.metrics;
        let colors = theme.colors;
        div()
            .h(px(metrics.title_height))
            .w_full()
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .px(px(metrics.title_inset_left))
            .gap(px(6.))
            .text_size(px(metrics.ui_text))
            .on_mouse_down(MouseButton::Left, |event: &MouseDownEvent, window, _| {
                if event.click_count == 2 {
                    window.titlebar_double_click();
                }
            })
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(color(colors.text_muted))
                    .child(self.directory()),
            )
            .when(!self.shell.is_empty(), |bar| {
                bar.child(
                    div()
                        .flex_shrink_0()
                        .text_color(color(colors.text_faint))
                        .child(format!("· {}", self.shell)),
                )
            })
    }

    fn render_terminal(&self, cx: &mut Context<Self>) -> AnyElement {
        let metrics = self.theme.metrics;
        let area = div()
            .flex_1()
            .min_h_0()
            .w_full()
            .px(px(metrics.grid_padding_x))
            .py(px(metrics.grid_padding_y))
            .overflow_hidden();
        if self.terminal.is_none() {
            return area
                .flex()
                .items_center()
                .justify_center()
                .text_color(color(self.theme.colors.text_muted))
                .child(self.notice.clone().unwrap_or_else(|| "正在启动…".into()))
                .into_any_element();
        }
        let cell = self.cell;
        let this = cx.weak_entity();
        area.child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.grid_bounds = Some(bounds);
                        let Some(terminal) = &this.terminal else {
                            return;
                        };
                        let direct = this.control.can_write_direct();
                        // 直接交互时终端自己接收输入法：组字显示在光标处，上屏后写入 PTY。
                        if direct {
                            window.handle_input(
                                &this.focus,
                                ElementInputHandler::new(bounds, cx.entity()),
                                cx,
                            );
                        }
                        // 命令框编辑或输入法组字时隐藏终端光标，避免同时出现两个插入点。
                        let cursor = (direct && this.preedit.is_empty()).then(|| {
                            if window.is_window_active() && this.focus.is_focused(window) {
                                CursorFocus::Focused
                            } else {
                                CursorFocus::Unfocused
                            }
                        });
                        let frame = grid::layout(terminal, &this.theme, cursor);
                        let metrics = this.grid_metrics(bounds.origin, cell);
                        grid::paint(&frame, &metrics, bounds, &this.theme, window, cx);
                        if direct
                            && !this.preedit.is_empty()
                            && let Some(origin) = this.cursor_origin()
                        {
                            grid::paint_preedit(
                                this.preedit.text(),
                                this.preedit.caret(),
                                origin,
                                &metrics,
                                &this.theme,
                                window,
                                cx,
                            );
                        }
                    });
                },
            )
            .w(cell.width * self.grid_size.columns as f32)
            .h(px(metrics.line_height * self.grid_size.rows as f32)),
        )
        .into_any_element()
    }

    /// 命令框是一张浮起的卡片：上行是目录与快捷键提示，下行是输入与执行按钮。
    fn render_composer(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let metrics = theme.metrics;
        let colors = theme.colors;
        let focused = self.editor.focus_handle(cx).is_focused(window);
        let error = self.editor.read(cx).error.clone();
        let hints = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .flex_shrink_0()
            .child(hint(theme, "↵", "执行"))
            .child(hint(theme, "esc", "交给终端"))
            .child(hint(theme, "⇧⌘E", "切换"));
        div()
            .flex_shrink_0()
            .h(px(metrics.composer_height))
            .mx(px(metrics.composer_margin))
            .mb(px(metrics.composer_margin))
            .px(px(14.))
            .py(px(10.))
            .flex()
            .flex_col()
            .justify_between()
            .rounded(px(metrics.radius))
            .bg(color(colors.surface))
            .border_1()
            .border_color(if focused {
                Hsla::from(tint(colors.accent, 0.5))
            } else {
                color(colors.border_strong)
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .text_size(px(metrics.ui_small))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(color(colors.text_muted))
                            .child(self.directory()),
                    )
                    .child(match error {
                        Some(error) => div()
                            .flex_shrink_0()
                            .text_color(color(colors.danger))
                            .child(error),
                        None => hints,
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_family(theme.mono_family)
                            .text_size(px(metrics.font_size + 2.))
                            .text_color(color(colors.accent))
                            .child("❯"),
                    )
                    .child(div().flex_1().min_w_0().child(self.editor.clone()))
                    .child(
                        div()
                            .id("submit")
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .h(px(26.))
                            .px(px(10.))
                            .rounded(px(6.))
                            .bg(tint(colors.accent, 0.14))
                            .hover(|style| style.bg(tint(colors.accent, 0.24)))
                            .text_size(px(metrics.ui_small))
                            .text_color(color(colors.accent))
                            .cursor_pointer()
                            .child("执行")
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
    }

    /// 状态栏：左侧是会话状态，右侧是历史位置、模式切换、网格尺寸和 shell。
    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let metrics = theme.metrics;
        let colors = theme.colors;
        let tone = chrome::tone(&self.control);
        let history = self.history_offset();
        let action =
            (!self.is_editing() && self.control.is_live() && self.control.can_confirm_prompt())
                .then(|| {
                    if self.control.integrated() {
                        "恢复命令框"
                    } else {
                        "启用命令框"
                    }
                });
        div()
            .h(px(metrics.status_height))
            .w_full()
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .px(px(metrics.grid_padding_x))
            .border_t_1()
            .border_color(color(colors.border))
            .text_size(px(metrics.ui_small))
            .text_color(color(colors.text_muted))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_shrink_0()
                            .size(px(7.))
                            .rounded_full()
                            .bg(color(tone.color(&colors))),
                    )
                    .child(div().flex_shrink_0().child(self.control.label()))
                    .when_some(self.notice.clone(), |row, notice| {
                        row.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(color(if tone == chrome::Tone::Error {
                                    colors.danger
                                } else {
                                    colors.text_faint
                                }))
                                .child(notice),
                        )
                    }),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .when(history > 0, |row| {
                        row.child(
                            div()
                                .text_color(color(colors.accent))
                                .child(format!("↑ {history} 行")),
                        )
                    })
                    .when_some(action, |row, label| {
                        row.child(
                            div()
                                .id("composer-toggle")
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .h(px(18.))
                                .px(px(6.))
                                .rounded(px(5.))
                                .text_color(color(colors.text))
                                .hover(|style| style.bg(color(colors.surface_raised)))
                                .cursor_pointer()
                                .child(label)
                                .child(keycap(theme, "⇧⌘E"))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, window, cx| {
                                        this.set_editing(true, window, cx);
                                        cx.stop_propagation();
                                    }),
                                ),
                        )
                    })
                    .child(div().text_color(color(colors.text_faint)).child(format!(
                        "{}×{}",
                        self.grid_size.columns, self.grid_size.rows
                    ))),
            )
    }
}

fn keycap(theme: &Theme, label: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .h(px(16.))
        .px(px(5.))
        .rounded(px(4.))
        .border_1()
        .border_color(color(theme.colors.border_strong))
        .bg(color(theme.colors.surface_raised))
        .text_size(px(10.5))
        .text_color(color(theme.colors.text_muted))
        .child(label)
}

fn hint(theme: &Theme, key: &'static str, label: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(5.))
        .text_color(color(theme.colors.text_faint))
        .child(keycap(theme, key))
        .child(label)
}

impl Render for TerminalWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.theme.colors;
        div()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(color(colors.background))
            .text_color(color(colors.text))
            .font_family(self.theme.ui_family)
            .text_size(px(self.theme.metrics.ui_text))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
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
            .child(self.render_title_bar())
            .child(self.render_terminal(cx))
            .when(self.is_editing(), |root| {
                root.child(self.render_composer(window, cx))
            })
            .child(self.render_status_bar(cx))
    }
}

/// 网格可用区域 = 窗口 − 标题栏 − 状态栏 − 网格留白 − （编辑时的）命令框卡片。
/// 布局与这里的计算共用同一份 Metrics，保证 PTY 行列与实际绘制一致。
fn available_grid_size(
    window: &Window,
    cell_width: Pixels,
    metrics: &Metrics,
    editing: bool,
) -> GridSize {
    let viewport = window.viewport_size();
    let composer = if editing {
        metrics.composer_height + metrics.composer_margin
    } else {
        0.0
    };
    GridSize::from_pixels(
        f32::from(viewport.width) - metrics.grid_padding_x * 2.0,
        f32::from(viewport.height)
            - metrics.title_height
            - metrics.status_height
            - metrics.grid_padding_y * 2.0
            - composer,
        f32::from(cell_width),
        metrics.line_height,
    )
}
