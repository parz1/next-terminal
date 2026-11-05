// 将字节解析成 Alacritty 屏幕；窗口和命令行示例复用，不负责 PTY 或绘制。
use alacritty_terminal::Term;
use alacritty_terminal::event::{EventListener, VoidListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::Config;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::Processor;

pub const COLUMNS: u16 = 80;
pub const ROWS: u16 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    pub columns: u16,
    pub rows: u16,
}

impl Default for GridSize {
    fn default() -> Self {
        Self {
            columns: COLUMNS,
            rows: ROWS,
        }
    }
}

impl GridSize {
    pub fn from_pixels(width: f32, height: f32, cell_width: f32, cell_height: f32) -> Self {
        Self {
            columns: (width / cell_width.max(1.0))
                .floor()
                .clamp(2.0, u16::MAX as f32) as u16,
            rows: (height / cell_height.max(1.0))
                .floor()
                .clamp(1.0, u16::MAX as f32) as u16,
        }
    }
}

impl Dimensions for GridSize {
    fn columns(&self) -> usize {
        self.columns as usize
    }

    fn screen_lines(&self) -> usize {
        self.rows as usize
    }

    fn total_lines(&self) -> usize {
        self.rows as usize
    }
}

fn new_terminal() -> Term<VoidListener> {
    with_listener(VoidListener)
}

pub fn with_listener<T: EventListener>(listener: T) -> Term<T> {
    // 缩放重排可能将内容推入历史，保留有限缓冲以免再次放大时丢失。
    let config = Config {
        scrolling_history: 1000,
        ..Config::default()
    };
    Term::new(config, &GridSize::default(), listener)
}

pub fn parse(bytes: &[u8]) -> Term<VoidListener> {
    let mut terminal = new_terminal();
    let mut processor: Processor = Processor::default();
    processor.advance(&mut terminal, bytes);
    terminal
}

fn row_text(terminal: &Term<VoidListener>, row: i32) -> String {
    let mut text = String::new();
    for column in 0..terminal.columns() {
        let cell = &terminal.grid()[Line(row)][Column(column)];
        // 宽字符占两个格子，但诊断文本只输出一次字符，跳过占位格。
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        // Alacritty 可能在 Tab 起点的格子中保留 '\t'；跳过的列已在网格中占位。
        // 诊断时将该格显示为空格，不能再让输出端重新做一次 Tab 展开。
        text.push(if cell.c == '\t' { ' ' } else { cell.c });
        if let Some(extra) = cell.zerowidth() {
            text.extend(extra);
        }
    }
    text.trim_end_matches(' ').to_owned()
}

pub fn print_snapshot(terminal: &Term<VoidListener>) {
    println!(
        "--- Alacritty grid: {} columns x {} rows (trailing spaces omitted) ---",
        terminal.columns(),
        terminal.screen_lines()
    );
    for row in 0..terminal.screen_lines() {
        // 只是网格的文字诊断；颜色、样式等属性仍保存在各个 Cell 中。
        println!(
            "{row:02} |{}|",
            row_text(terminal, row as i32).escape_debug()
        );
    }
    let point = terminal.grid().cursor.point;
    println!(
        "Cursor (0-based): row={}, column={}",
        point.line.0, point.column.0
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::index::Point;
    use alacritty_terminal::vte::ansi::{Color, NamedColor};

    #[test]
    fn pixel_size_uses_only_whole_cells_and_clamps_minimum() {
        assert_eq!(
            GridSize::from_pixels(809.9, 439.9, 10.0, 22.0),
            GridSize {
                columns: 80,
                rows: 19
            }
        );
        assert_eq!(
            GridSize::from_pixels(810.0, 440.0, 10.0, 22.0),
            GridSize {
                columns: 81,
                rows: 20
            }
        );
        assert_eq!(
            GridSize::from_pixels(0.0, -20.0, 10.0, 22.0),
            GridSize {
                columns: 2,
                rows: 1
            }
        );
    }

    #[test]
    fn history_selection_stays_on_visible_text_when_new_output_arrives() {
        use alacritty_terminal::grid::Scroll;
        use alacritty_terminal::index::Side;
        use alacritty_terminal::selection::{Selection, SelectionType};
        use alacritty_terminal::term::viewport_to_point;

        let mut terminal = new_terminal();
        terminal.resize(GridSize {
            columns: 16,
            rows: 4,
        });
        let mut processor: Processor = Processor::default();
        for index in 1..=10 {
            processor.advance(&mut terminal, format!("H{index:02}\r\n").as_bytes());
        }
        terminal.scroll_display(Scroll::Delta(5));
        let offset = terminal.grid().display_offset();
        let start = viewport_to_point(offset, Point::new(0, Column(0)));
        let end = viewport_to_point(offset, Point::new(1, Column(2)));
        assert_eq!(row_text(&terminal, start.line.0), "H03");
        let mut selection = Selection::new(SelectionType::Simple, start, Side::Left);
        selection.update(end, Side::Right);
        terminal.selection = Some(selection);
        assert_eq!(terminal.selection_to_string().as_deref(), Some("H03\nH04"));

        processor.advance(&mut terminal, b"H11\r\n");
        let offset = terminal.grid().display_offset();
        assert_eq!(row_text(&terminal, -(offset as i32)), "H03");
        assert_eq!(terminal.selection_to_string().as_deref(), Some("H03\nH04"));

        terminal.scroll_display(Scroll::Delta(i32::MAX / 2));
        assert_eq!(terminal.grid().display_offset(), terminal.history_size());
        assert_eq!(
            row_text(&terminal, -(terminal.grid().display_offset() as i32)),
            "H01"
        );
        terminal.scroll_display(Scroll::Delta(-10000));
        assert_eq!(terminal.grid().display_offset(), 0);
        processor.advance(&mut terminal, b"ready>");
        assert_eq!(row_text(&terminal, 2), "H11");
        assert_eq!(row_text(&terminal, 3), "ready>");
    }

    #[test]
    fn resize_reflows_long_output_and_preserves_cursor_for_more_output() {
        let mut terminal = new_terminal();
        let mut processor: Processor = Processor::default();
        terminal.resize(GridSize {
            columns: 20,
            rows: 6,
        });
        processor.advance(&mut terminal, b"0123456789abcdefghijklmnopqrstu");
        terminal.resize(GridSize {
            columns: 10,
            rows: 6,
        });
        assert_eq!(row_text(&terminal, -2), "0123456789");
        assert_eq!(row_text(&terminal, -1), "abcdefghij");
        assert_eq!(row_text(&terminal, 0), "klmnopqrst");
        assert_eq!(row_text(&terminal, 1), "u");
        assert_eq!(terminal.grid().cursor.point, Point::new(Line(1), Column(1)));
        terminal.resize(GridSize {
            columns: 20,
            rows: 6,
        });
        processor.advance(&mut terminal, b"!");
        assert_eq!(row_text(&terminal, 0), "0123456789abcdefghij");
        assert_eq!(row_text(&terminal, 1), "klmnopqrstu!");
        assert_eq!(
            terminal.grid().cursor.point,
            Point::new(Line(1), Column(12))
        );
    }

    #[test]
    fn carriage_return_overwrites_without_erasing_the_rest() {
        let terminal = parse(b"hello\rOK");
        assert_eq!(row_text(&terminal, 0), "OKllo");
        assert_eq!(terminal.grid().cursor.point, Point::new(Line(0), Column(2)));
    }

    #[test]
    fn selection_preserves_unicode_and_hard_newlines_but_joins_soft_wraps() {
        use alacritty_terminal::index::Side;
        use alacritty_terminal::selection::{Selection, SelectionType};

        let mut terminal = new_terminal();
        terminal.resize(GridSize {
            columns: 8,
            rows: 4,
        });
        let mut processor: Processor = Processor::default();
        processor.advance(&mut terminal, "中e\u{301}12345678\r\nnext".as_bytes());
        let mut selection = Selection::new(
            SelectionType::Simple,
            Point::new(Line(0), Column(0)),
            Side::Left,
        );
        selection.update(Point::new(Line(2), Column(3)), Side::Right);
        terminal.selection = Some(selection);
        assert_eq!(
            terminal.selection_to_string().as_deref(),
            Some("中e\u{301}12345678\nnext")
        );
        // 反向拖选也应得到相同文本。
        let mut selection = Selection::new(
            SelectionType::Simple,
            Point::new(Line(2), Column(3)),
            Side::Right,
        );
        selection.update(Point::new(Line(0), Column(0)), Side::Left);
        terminal.selection = Some(selection);
        assert_eq!(
            terminal.selection_to_string().as_deref(),
            Some("中e\u{301}12345678\nnext")
        );
    }

    #[test]
    fn erase_line_and_cursor_movement_change_existing_cells() {
        let terminal = parse(b"loading 100%\rOK\x1b[K\r\nnext\x1b[2D!");
        assert_eq!(row_text(&terminal, 0), "OK");
        assert_eq!(row_text(&terminal, 1), "ne!t");
        assert_eq!(terminal.grid().cursor.point, Point::new(Line(1), Column(3)));
    }

    #[test]
    fn linefeed_moves_down_but_carriage_return_moves_to_column_zero() {
        let terminal = parse(b"AB\nC\rD");
        assert_eq!(row_text(&terminal, 0), "AB");
        assert_eq!(row_text(&terminal, 1), "D C");
        assert_eq!(terminal.grid().cursor.point, Point::new(Line(1), Column(1)));
    }

    #[test]
    fn tab_places_text_at_the_next_tab_stop() {
        let terminal = parse(b"A\tB");
        assert_eq!(row_text(&terminal, 0), "A       B");
        assert_eq!(terminal.grid().cursor.point, Point::new(Line(0), Column(9)));
    }

    #[test]
    fn parser_preserves_split_utf8_and_escape_sequences() {
        let bytes = "\x1b[31m中A\x1b[0me\u{301}".as_bytes();
        // 穷举每一个两块切分点，包括中文 UTF-8 和 ESC 控制序列的中间。
        for split in 0..=bytes.len() {
            let mut terminal = new_terminal();
            let mut processor: Processor = Processor::default();
            processor.advance(&mut terminal, &bytes[..split]);
            processor.advance(&mut terminal, &bytes[split..]);
            assert_eq!(row_text(&terminal, 0), "中Ae\u{301}", "split={split}");
            assert!(
                terminal.grid()[Line(0)][Column(1)]
                    .flags
                    .contains(Flags::WIDE_CHAR_SPACER)
            );
            assert_eq!(
                terminal.grid()[Line(0)][Column(2)].fg,
                Color::Named(NamedColor::Red)
            );
            assert_eq!(terminal.grid().cursor.point, Point::new(Line(0), Column(4)));
        }
    }
}
