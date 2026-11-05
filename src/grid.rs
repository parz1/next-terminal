//! 终端网格的绘制计划与绘制。
//!
//! 分两步：`layout` 是不依赖窗口的纯函数，把 Alacritty 网格整理成合并后的背景条、
//! 文字段和光标；`paint` 只把这份计划交给 GPUI。这样合并规则可以单元测试，
//! 绘制调用数量也从“每格一次”降到“每段一次”。
use crate::theme::{self, Rgb, Theme};
use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Point as GridPoint};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{TermMode, viewport_to_point};
use alacritty_terminal::vte::ansi::{Color, CursorShape as TermCursorShape, NamedColor};
use gpui::{
    App, BorderStyle, Bounds, Pixels, StrikethroughStyle, TextRun, UnderlineStyle, Window, fill,
    font, outline, point, px, size,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Underline {
    None,
    Straight,
    Curly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextStyle {
    pub color: Rgb,
    pub bold: bool,
    pub italic: bool,
    pub underline: Underline,
    pub strikethrough: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundSpan {
    pub row: usize,
    pub column: usize,
    pub cells: usize,
    pub color: Rgb,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextSpan {
    pub row: usize,
    pub column: usize,
    pub text: String,
    pub style: TextStyle,
    pub advance: Advance,
}

/// 文字段里每个字形占几格。等宽段在绘制时强制按格宽排列，所以可以安全合并；
/// 组合字符（一个格子里有多个码位）无法逐字形对齐，单独定位。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Advance {
    Narrow,
    Wide,
    Free,
}

impl Advance {
    pub fn cells(self) -> Option<usize> {
        match self {
            Advance::Narrow => Some(1),
            Advance::Wide => Some(2),
            Advance::Free => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Hollow,
    Beam,
    Underline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorLayout {
    pub row: usize,
    pub column: usize,
    pub cells: usize,
    pub shape: CursorShape,
    pub color: Rgb,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GridFrame {
    pub background: Rgb,
    pub backgrounds: Vec<BackgroundSpan>,
    pub texts: Vec<TextSpan>,
    pub cursor: Option<CursorLayout>,
}

/// 光标显示请求：`None` 表示当前不该画光标（例如命令框正在编辑）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorFocus {
    Focused,
    Unfocused,
}

pub fn layout<T>(
    terminal: &Term<T>,
    theme: &Theme,
    cursor_focus: Option<CursorFocus>,
) -> GridFrame {
    let default_background = resolve(Color::Named(NamedColor::Background), terminal, theme);
    let selection = terminal
        .selection
        .as_ref()
        .and_then(|selection| selection.to_range(terminal));
    let display_offset = terminal.grid().display_offset();
    let cursor = cursor_layout(terminal, theme, cursor_focus);
    let block_cursor = cursor.filter(|cursor| cursor.shape == CursorShape::Block);

    let mut frame = GridFrame {
        background: default_background,
        cursor,
        ..GridFrame::default()
    };
    for row in 0..terminal.screen_lines() {
        let mut background: Option<BackgroundSpan> = None;
        let mut text: Option<TextSpan> = None;
        for column in 0..terminal.columns() {
            let grid_point = viewport_to_point(display_offset, GridPoint::new(row, Column(column)));
            let cell = &terminal.grid()[grid_point.line][grid_point.column];
            let (foreground, cell_background) = cell_colors(cell, terminal, theme);

            let selected = selection.is_some_and(|range| {
                range.contains(grid_point)
                    || (cell.flags.contains(Flags::WIDE_CHAR)
                        && range.contains(GridPoint::new(grid_point.line, Column(column + 1))))
            });
            let fill_color = if selected {
                Some(theme.colors.selection)
            } else {
                (cell_background != default_background).then_some(cell_background)
            };
            match (&mut background, fill_color) {
                (Some(span), Some(color)) if span.color == color => span.cells += 1,
                (_, color) => {
                    frame.backgrounds.extend(background.take());
                    background = color.map(|color| BackgroundSpan {
                        row,
                        column,
                        cells: 1,
                        color,
                    });
                }
            }

            // 宽字符右半边的占位格属于前一个字，不打断连续的中文段。
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let Some((glyphs, advance)) = cell_text(cell) else {
                // 空白和制表位打断文字段，下一段从网格列重新定位。
                frame.texts.extend(text.take());
                continue;
            };
            let under_cursor =
                block_cursor.is_some_and(|cursor| cursor.row == row && cursor.column == column);
            let style = TextStyle {
                color: if under_cursor {
                    theme.colors.cursor_text
                } else {
                    foreground
                },
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
                underline: if cell.flags.contains(Flags::UNDERCURL) {
                    Underline::Curly
                } else if cell.flags.intersects(Flags::ALL_UNDERLINES) {
                    Underline::Straight
                } else {
                    Underline::None
                },
                strikethrough: cell.flags.contains(Flags::STRIKEOUT),
            };
            match &mut text {
                Some(span)
                    if advance != Advance::Free
                        && span.advance == advance
                        && span.style == style
                        && advance.cells().is_some_and(|cells| {
                            span.column + span.text.chars().count() * cells == column
                        }) =>
                {
                    span.text.push_str(&glyphs);
                }
                _ => {
                    frame.texts.extend(text.take());
                    text = Some(TextSpan {
                        row,
                        column,
                        text: glyphs,
                        style,
                        advance,
                    });
                }
            }
        }
        frame.backgrounds.extend(background);
        frame.texts.extend(text);
    }
    frame
}

/// 返回需要绘制的文字及其字宽。空白格返回 `None`。
fn cell_text(cell: &Cell) -> Option<(String, Advance)> {
    if cell
        .flags
        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER | Flags::HIDDEN)
        || cell.c == '\t'
    {
        return None;
    }
    let zerowidth = cell.zerowidth();
    let decorated = cell
        .flags
        .intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT);
    if cell.c == ' ' && zerowidth.is_none() && !decorated {
        return None;
    }
    let mut text = cell.c.to_string();
    if let Some(extra) = zerowidth {
        text.extend(extra);
    }
    let advance = if zerowidth.is_some() {
        Advance::Free
    } else if cell.flags.contains(Flags::WIDE_CHAR) {
        Advance::Wide
    } else {
        Advance::Narrow
    };
    Some((text, advance))
}

fn cell_colors<T>(cell: &Cell, terminal: &Term<T>, theme: &Theme) -> (Rgb, Rgb) {
    let mut foreground = resolve(cell.fg, terminal, theme);
    let mut background = resolve(cell.bg, terminal, theme);
    if cell.flags.contains(Flags::DIM) && !cell.flags.contains(Flags::BOLD) {
        foreground = theme::mix(foreground, background, 0.66);
    }
    if cell.flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut foreground, &mut background);
    }
    (foreground, background)
}

fn cursor_layout<T>(
    terminal: &Term<T>,
    theme: &Theme,
    focus: Option<CursorFocus>,
) -> Option<CursorLayout> {
    let focus = focus?;
    if terminal.grid().display_offset() != 0 || !terminal.mode().contains(TermMode::SHOW_CURSOR) {
        return None;
    }
    let requested = terminal.cursor_style().shape;
    let shape = match (focus, requested) {
        (_, TermCursorShape::Hidden) => return None,
        (CursorFocus::Unfocused, _) | (_, TermCursorShape::HollowBlock) => CursorShape::Hollow,
        (CursorFocus::Focused, TermCursorShape::Block) => CursorShape::Block,
        (CursorFocus::Focused, TermCursorShape::Beam) => CursorShape::Beam,
        (CursorFocus::Focused, TermCursorShape::Underline) => CursorShape::Underline,
    };
    let point = terminal.grid().cursor.point;
    let line = point.line.0;
    if line < 0 || line as usize >= terminal.screen_lines() {
        return None;
    }
    let cell = &terminal.grid()[point.line][point.column];
    Some(CursorLayout {
        row: line as usize,
        column: point.column.0,
        cells: if cell.flags.contains(Flags::WIDE_CHAR) {
            2
        } else {
            1
        },
        shape,
        color: resolve(Color::Named(NamedColor::Cursor), terminal, theme),
    })
}

/// 程序通过 OSC 4/10/11/12 改写的颜色优先，其余来自主题。
pub fn resolve<T>(color: Color, terminal: &Term<T>, theme: &Theme) -> Rgb {
    let index = match color {
        Color::Spec(value) => {
            return (value.r as u32) << 16 | (value.g as u32) << 8 | value.b as u32;
        }
        Color::Named(value) => value as usize,
        Color::Indexed(value) => value as usize,
    };
    if let Some(value) = terminal.colors()[index] {
        return (value.r as u32) << 16 | (value.g as u32) << 8 | value.b as u32;
    }
    theme.indexed(index)
}

/// 主等宽字体的单格尺寸。ascent/descent 用来把所有文字段对齐到同一条基线。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellMetrics {
    pub width: Pixels,
    pub ascent: Pixels,
    pub descent: Pixels,
}

impl CellMetrics {
    pub fn measure(window: &Window, theme: &Theme) -> Self {
        let style = TextStyle {
            color: theme.colors.foreground,
            bold: false,
            italic: false,
            underline: Underline::None,
            strikethrough: false,
        };
        let line = window.text_system().shape_line(
            "M".into(),
            px(theme.metrics.font_size),
            &[text_run("M", &style, theme)],
            None,
        );
        Self {
            width: line.width,
            ascent: line.ascent,
            descent: line.descent,
        }
    }
}

pub struct GridMetrics {
    pub origin: gpui::Point<Pixels>,
    pub cell: CellMetrics,
    pub line_height: Pixels,
    pub font_size: Pixels,
}

pub fn paint(
    frame: &GridFrame,
    metrics: &GridMetrics,
    bounds: Bounds<Pixels>,
    theme: &Theme,
    window: &mut Window,
    cx: &mut App,
) {
    let cell_bounds = |row: usize, column: usize, cells: usize| {
        Bounds::new(
            metrics.origin
                + point(
                    metrics.cell.width * column as f32,
                    metrics.line_height * row as f32,
                ),
            size(metrics.cell.width * cells as f32, metrics.line_height),
        )
    };
    if frame.background != theme.colors.background {
        window.paint_quad(fill(bounds, theme::color(frame.background)));
    }
    for span in &frame.backgrounds {
        window.paint_quad(fill(
            cell_bounds(span.row, span.column, span.cells),
            theme::color(span.color),
        ));
    }
    // 实心光标在文字之下，格内文字已在布局阶段换成反色。
    if let Some(cursor) = frame
        .cursor
        .filter(|cursor| cursor.shape == CursorShape::Block)
    {
        window.paint_quad(fill(
            cell_bounds(cursor.row, cursor.column, cursor.cells),
            theme::color(cursor.color),
        ));
    }
    for span in &frame.texts {
        let run = text_run(&span.text, &span.style, theme);
        let force_width = span
            .advance
            .cells()
            .map(|cells| metrics.cell.width * cells as f32);
        let line = window.text_system().shape_line(
            span.text.clone().into(),
            metrics.font_size,
            &[run],
            force_width,
        );
        // GPUI 按每段自己的字体度量居中；中文回退字体的上下高度不同，
        // 不校正会让同一行的中英文基线错开。统一对齐到主等宽字体的基线。
        let baseline_shift =
            ((metrics.cell.ascent - metrics.cell.descent) - (line.ascent - line.descent)) / 2.;
        let origin = cell_bounds(span.row, span.column, 1).origin + point(px(0.), baseline_shift);
        if let Err(error) = line.paint(origin, metrics.line_height, window, cx) {
            eprintln!("字形绘制失败：{error}");
        }
    }
    if let Some(cursor) = frame.cursor {
        let cell = cell_bounds(cursor.row, cursor.column, cursor.cells);
        let color = theme::color(cursor.color);
        match cursor.shape {
            CursorShape::Block => {}
            CursorShape::Hollow => {
                window.paint_quad(outline(cell, color, BorderStyle::Solid));
            }
            CursorShape::Beam => {
                window.paint_quad(fill(
                    Bounds::new(cell.origin, size(px(2.), cell.size.height)),
                    color,
                ));
            }
            CursorShape::Underline => {
                window.paint_quad(fill(
                    Bounds::new(
                        point(cell.left(), cell.bottom() - px(2.)),
                        size(cell.size.width, px(2.)),
                    ),
                    color,
                ));
            }
        }
    }
}

pub fn text_run(text: &str, style: &TextStyle, theme: &Theme) -> TextRun {
    let mut face = font(theme.mono_family);
    if style.bold {
        face = face.bold();
    }
    if style.italic {
        face = face.italic();
    }
    TextRun {
        len: text.len(),
        font: face,
        color: theme::color(style.color),
        background_color: None,
        underline: match style.underline {
            Underline::None => None,
            Underline::Straight | Underline::Curly => Some(UnderlineStyle {
                thickness: px(1.),
                color: None,
                wavy: style.underline == Underline::Curly,
            }),
        },
        strikethrough: style.strikethrough.then_some(StrikethroughStyle {
            thickness: px(1.),
            color: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::vte::ansi::Processor;
    use next_terminal::screen::{GridSize, with_listener};

    fn term(columns: u16, rows: u16, bytes: &[u8]) -> Term<VoidListener> {
        let mut terminal = with_listener(VoidListener);
        terminal.resize(GridSize { columns, rows });
        let mut processor: Processor = Processor::default();
        processor.advance(&mut terminal, bytes);
        terminal
    }

    fn texts(frame: &GridFrame) -> Vec<(usize, usize, &str)> {
        frame
            .texts
            .iter()
            .map(|span| (span.row, span.column, span.text.as_str()))
            .collect()
    }

    #[test]
    fn words_merge_into_one_span_and_spaces_split_them() {
        let theme = Theme::graphite();
        let terminal = term(20, 2, b"hello world\r\n  ls");
        let frame = layout(&terminal, &theme, None);
        assert_eq!(
            texts(&frame),
            [(0, 0, "hello"), (0, 6, "world"), (1, 2, "ls")]
        );
        // 默认背景不产生任何矩形。
        assert!(frame.backgrounds.is_empty());
        assert!(
            frame
                .texts
                .iter()
                .all(|span| span.advance == Advance::Narrow)
        );
    }

    #[test]
    fn style_changes_split_spans_and_colors_resolve_from_theme() {
        let theme = Theme::graphite();
        let terminal = term(20, 1, b"ab\x1b[31mcd\x1b[1mef\x1b[0m");
        let frame = layout(&terminal, &theme, None);
        assert_eq!(texts(&frame), [(0, 0, "ab"), (0, 2, "cd"), (0, 4, "ef")]);
        assert_eq!(frame.texts[0].style.color, theme.colors.foreground);
        assert_eq!(frame.texts[1].style.color, theme.ansi[1]);
        assert!(!frame.texts[1].style.bold);
        assert!(frame.texts[2].style.bold);
    }

    #[test]
    fn wide_runs_merge_and_combining_cells_are_positioned_individually() {
        let theme = Theme::graphite();
        let terminal = term(20, 1, "ab中文cde\u{301}f".as_bytes());
        let frame = layout(&terminal, &theme, None);
        assert_eq!(
            texts(&frame),
            [
                (0, 0, "ab"),
                (0, 2, "中文"),
                (0, 6, "cd"),
                (0, 8, "e\u{301}"),
                (0, 9, "f")
            ]
        );
        let advances: Vec<_> = frame.texts.iter().map(|span| span.advance).collect();
        assert_eq!(
            advances,
            [
                Advance::Narrow,
                Advance::Wide,
                Advance::Narrow,
                Advance::Free,
                Advance::Narrow
            ]
        );
        // 宽字符后紧跟窄字符、或中间隔着空格时各自成段。
        let terminal = term(20, 1, "中a 文".as_bytes());
        let frame = layout(&terminal, &theme, None);
        assert_eq!(texts(&frame), [(0, 0, "中"), (0, 2, "a"), (0, 4, "文")]);
    }

    #[test]
    fn backgrounds_merge_per_row_and_inverse_swaps_colors() {
        let theme = Theme::graphite();
        let terminal = term(10, 2, b"\x1b[44m   \x1b[0m x\r\n\x1b[7mab\x1b[0m");
        let frame = layout(&terminal, &theme, None);
        assert_eq!(
            frame.backgrounds,
            [
                BackgroundSpan {
                    row: 0,
                    column: 0,
                    cells: 3,
                    color: theme.ansi[4]
                },
                BackgroundSpan {
                    row: 1,
                    column: 0,
                    cells: 2,
                    color: theme.colors.foreground
                }
            ]
        );
        assert_eq!(frame.texts[1].style.color, theme.colors.background);
    }

    #[test]
    fn underlined_spaces_are_drawn_and_decorations_are_kept() {
        let theme = Theme::graphite();
        let terminal = term(10, 1, b"\x1b[4ma b\x1b[0m \x1b[9mx\x1b[0m");
        let frame = layout(&terminal, &theme, None);
        assert_eq!(texts(&frame), [(0, 0, "a b"), (0, 4, "x")]);
        assert_eq!(frame.texts[0].style.underline, Underline::Straight);
        assert!(frame.texts[1].style.strikethrough);
    }

    #[test]
    fn block_cursor_inverts_only_its_own_cell() {
        let theme = Theme::graphite();
        let terminal = term(10, 1, b"abc\x1b[2D");
        let frame = layout(&terminal, &theme, Some(CursorFocus::Focused));
        let cursor = frame.cursor.unwrap();
        assert_eq!(
            (cursor.row, cursor.column, cursor.shape),
            (0, 1, CursorShape::Block)
        );
        assert_eq!(texts(&frame), [(0, 0, "a"), (0, 1, "b"), (0, 2, "c")]);
        assert_eq!(frame.texts[1].style.color, theme.colors.cursor_text);

        let unfocused = layout(&terminal, &theme, Some(CursorFocus::Unfocused));
        assert_eq!(unfocused.cursor.unwrap().shape, CursorShape::Hollow);
        assert_eq!(texts(&unfocused), [(0, 0, "abc")]);
        assert!(layout(&terminal, &theme, None).cursor.is_none());
    }

    #[test]
    fn program_cursor_shape_and_hidden_cursor_are_respected() {
        let theme = Theme::graphite();
        let beam = term(10, 1, b"\x1b[6 q");
        assert_eq!(
            layout(&beam, &theme, Some(CursorFocus::Focused))
                .cursor
                .unwrap()
                .shape,
            CursorShape::Beam
        );
        let hidden = term(10, 1, b"\x1b[?25l");
        assert!(
            layout(&hidden, &theme, Some(CursorFocus::Focused))
                .cursor
                .is_none()
        );
    }

    #[test]
    fn selection_paints_selected_cells_and_merges_across_text() {
        use alacritty_terminal::index::{Line, Side};
        use alacritty_terminal::selection::{Selection, SelectionType};
        let theme = Theme::graphite();
        let mut terminal = term(10, 1, b"hello");
        let mut selection = Selection::new(
            SelectionType::Simple,
            GridPoint::new(Line(0), Column(1)),
            Side::Left,
        );
        selection.update(GridPoint::new(Line(0), Column(3)), Side::Right);
        terminal.selection = Some(selection);
        let frame = layout(&terminal, &theme, None);
        assert_eq!(
            frame.backgrounds,
            [BackgroundSpan {
                row: 0,
                column: 1,
                cells: 3,
                color: theme.colors.selection
            }]
        );
    }

    #[test]
    fn program_palette_overrides_theme_colors() {
        let theme = Theme::graphite();
        let terminal = term(10, 1, b"\x1b]4;1;rgb:12/34/56\x07\x1b[31mx");
        let frame = layout(&terminal, &theme, None);
        assert_eq!(frame.texts[0].style.color, 0x123456);
    }
}
