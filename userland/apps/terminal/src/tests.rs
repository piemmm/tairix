//! Headless unit tests for the terminal model and renderer.
//!
//! Everything is exercised without a kernel: the [`ShellSource`] seam is an
//! in-memory queue, so the grid, the control parser, the shell glue, and the
//! renderer are all testable in isolation.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_abi::Errno;
use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect};
use tairix_input::{Key, Modifiers, NamedKey};
use tairix_raster::Color;
use tairix_reclaim::PressureBand;
use tairix_theme::Palette;

use tairix_vt::{encode_all_into, BasicColor, Color as VtColor, Op, Sgr};

/// The face the renderer tests measure: the same monospace face the `Run`
/// binary derives from a desktop at the reference density, so a test's cell
/// arithmetic matches the terminal a user actually sees.
fn test_font() -> BitmapFont {
    BitmapFont::monospace(tairix_font::atlas::CELL_HEIGHT)
}

/// Encode a sequence of operations into a fresh `Vec` over the sink API.
fn encode_all(ops: &[Op]) -> alloc::vec::Vec<u8> {
    let mut out = alloc::vec::Vec::new();
    encode_all_into(ops, &mut out);
    out
}

use crate::grid::{Grid, MAX_DIMENSION};
use crate::parser::Parser;
use crate::render::Screen;
use crate::shell::ShellSource;
use crate::terminal::{Reshaped, Terminal};
use crate::Cell;

/// An in-memory shell channel: each `read` returns the next queued chunk (or
/// an empty slice), and every `write` is captured. A pending error makes the
/// next call fail.
#[derive(Default)]
struct QueueShell {
    output: VecDeque<Vec<u8>>,
    written: Vec<u8>,
    read_error: Option<Errno>,
    write_error: Option<Errno>,
}

impl QueueShell {
    fn with_output(chunks: &[&[u8]]) -> Self {
        Self {
            output: chunks.iter().map(|chunk| chunk.to_vec()).collect(),
            ..Self::default()
        }
    }
}

impl ShellSource for QueueShell {
    fn read(&mut self) -> Result<Vec<u8>, Errno> {
        if let Some(errno) = self.read_error.take() {
            return Err(errno);
        }
        Ok(self.output.pop_front().unwrap_or_default())
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), Errno> {
        if let Some(errno) = self.write_error.take() {
            return Err(errno);
        }
        self.written.extend_from_slice(bytes);
        Ok(())
    }
}

/// The glyph of one cell, or a space when off-grid.
fn glyph(grid: &Grid, col: u16, row: u16) -> char {
    grid.cell(col, row).map_or(' ', |cell| cell.ch)
}

/// The cell at `(col, row)`, expected to be on-grid.
fn cell_at(grid: &Grid, col: u16, row: u16) -> Cell {
    grid.cell(col, row).expect("cell on grid")
}

/// Fill `row` of `grid` with `text`, one glyph per column from column 0.
fn put_row(grid: &mut Grid, row: u16, text: &str) {
    grid.move_to(0, row);
    for ch in text.chars() {
        grid.write_char(ch);
    }
}

/// The text of one row as `read` reads it, with trailing blanks trimmed.
fn text_of(grid: &Grid, row: u16, read: fn(&Grid, u16, u16) -> Option<Cell>) -> String {
    let text: String = (0..grid.cols())
        .map(|col| read(grid, col, row).map_or(' ', |cell| cell.ch))
        .collect();
    String::from(text.trim_end())
}

/// The visible text of one grid row, with trailing blanks trimmed.
fn row_text(grid: &Grid, row: u16) -> String {
    text_of(grid, row, Grid::cell)
}

/// The text the view shows on one row, with trailing blanks trimmed.
fn view_text(grid: &Grid, row: u16) -> String {
    text_of(grid, row, Grid::view_cell)
}

/// Lines enough in a test's scrollback that no test meets its limit unless it
/// means to.
pub(crate) const ROOM: usize = 1 << 20;

/// Feed `bytes` through a fresh parser into a fresh `cols`×`rows` grid.
fn render_bytes(cols: u16, rows: u16, bytes: &[u8]) -> Grid {
    let mut grid = Grid::new(cols, rows, ROOM).expect("valid grid size");
    let mut parser = Parser::new();
    parser.feed(&mut grid, bytes);
    grid
}

/// A `cols`×`rows` terminal over an idle shell.
fn terminal(cols: u16, rows: u16) -> Terminal<QueueShell> {
    Terminal::new(cols, rows, QueueShell::default(), ROOM).expect("valid size")
}

#[test]
fn grid_new_rejects_degenerate_sizes() {
    assert!(Grid::new(0, 24, ROOM).is_none());
    assert!(Grid::new(80, 0, ROOM).is_none());
    assert!(Grid::new(MAX_DIMENSION + 1, 24, ROOM).is_none());
    assert!(Grid::new(80, MAX_DIMENSION + 1, ROOM).is_none());
    assert!(Grid::new(1, 1, ROOM).is_some());
}

#[test]
fn a_shrink_keeps_the_cursors_line_on_screen_and_a_grow_takes_back_what_it_gave_up() {
    let mut grid = render_bytes(6, 3, b"abc\r\ndef");

    // The row above the cursor's goes to the scrollback rather than being
    // dropped, and growing takes it back.
    assert!(grid.resize(4, 1));
    assert_eq!((grid.cols(), grid.rows()), (4, 1));
    assert_eq!(row_text(&grid, 0), "def");
    assert_eq!((grid.cursor_col(), grid.cursor_row()), (3, 0));
    assert_eq!(grid.scrollback_len(), 1);

    assert!(grid.resize(8, 4));
    assert_eq!((grid.cols(), grid.rows()), (8, 4));
    assert_eq!(row_text(&grid, 0), "abc");
    assert_eq!(row_text(&grid, 1), "def");
    assert_eq!(row_text(&grid, 3), "");
    assert_eq!((grid.cursor_col(), grid.cursor_row()), (3, 1));
    assert_eq!(grid.scrollback_len(), 0);

    // A degenerate, oversized, or no-op resize changes nothing (fail closed).
    assert!(!grid.resize(0, 4));
    assert!(!grid.resize(8, MAX_DIMENSION + 1));
    assert!(!grid.resize(8, 4));
    assert_eq!((grid.cols(), grid.rows()), (8, 4));
}

#[test]
fn a_bell_is_heard_once_however_often_it_rang_and_never_drawn() {
    let mut term = terminal(20, 3);
    assert!(!term.take_bell());
    term.feed(b"a\x07b\x07\x07");
    assert_eq!(row_text(term.grid(), 0), "ab", "the bell leaves no mark");
    assert!(term.take_bell());
    assert!(!term.take_bell(), "asked once, it is spent");
    term.feed(b"\x1b]0;title\x07");
    assert!(!term.take_bell(), "an OSC terminator is not a bell");
    term.feed(b"\x07");
    assert!(term.take_bell());
}

#[test]
fn terminal_resize_reshapes_the_grid() {
    let mut term = terminal(20, 3);
    term.feed(b"hi");
    term.resize(40, 10);
    assert_eq!(term.grid().cols(), 20, "nothing moves until given");
    assert_eq!(term.settle(), Reshaped::Took { cols: 40, rows: 10 });
    assert_eq!((term.grid().cols(), term.grid().rows()), (40, 10));
    // The content survives the reshape; the size in force, or none asked for,
    // is no change.
    assert_eq!(row_text(term.grid(), 0), "hi");
    term.resize(40, 10);
    assert_eq!(term.settle(), Reshaped::Unchanged);
    assert_eq!(term.settle(), Reshaped::Unchanged);
}

#[test]
fn a_wide_glyph_occupies_a_lead_and_a_continuation_cell() {
    // The same two-column layout the curses window writer and the framebuffer
    // console produce, so column arithmetic agrees across the stack.
    let grid = render_bytes(6, 2, "世a".as_bytes());
    assert_eq!(glyph(&grid, 0, 0), '世');
    assert_eq!(glyph(&grid, 1, 0), tairix_vt::CONTINUATION);
    assert_eq!(glyph(&grid, 2, 0), 'a');
    assert_eq!((grid.cursor_col(), grid.cursor_row()), (3, 0));
}

#[test]
fn a_wide_glyph_wraps_whole_when_one_column_remains() {
    let grid = render_bytes(3, 2, "ab世".as_bytes());
    assert_eq!(glyph(&grid, 0, 0), 'a');
    assert_eq!(glyph(&grid, 1, 0), 'b');
    // The leftover column was blanked rather than half-filled.
    assert_eq!(glyph(&grid, 2, 0), ' ');
    assert_eq!(glyph(&grid, 0, 1), '世');
    assert_eq!(glyph(&grid, 1, 1), tairix_vt::CONTINUATION);
}

#[test]
fn a_wide_glyph_on_a_one_column_grid_never_leaks_into_the_next_row() {
    // One column can never hold a two-column glyph: the wrap-whole rule
    // blanks the cell and wraps first, and the continuation is dropped
    // rather than aliasing the next row's first cell. Each glyph then fills
    // its row and owes a wrap the next one pays, so the three rows are the
    // blanked first cell, the lead, and the following narrow glyph.
    let grid = render_bytes(1, 3, "世x".as_bytes());
    assert_eq!(glyph(&grid, 0, 0), ' ');
    assert_eq!(glyph(&grid, 0, 1), '世');
    assert_eq!(glyph(&grid, 0, 2), 'x');
}

// --- The owed wrap at the right edge -------------------------------------

#[test]
fn filling_the_last_column_leaves_the_cursor_resting_on_it() {
    let grid = render_bytes(4, 2, b"abcd");
    assert_eq!(row_text(&grid, 0), "abcd");
    assert_eq!(row_text(&grid, 1), "");
    assert_eq!((grid.cursor_col(), grid.cursor_row()), (3, 0));
}

#[test]
fn the_glyph_after_a_full_row_pays_the_owed_wrap() {
    let grid = render_bytes(4, 2, b"abcde");
    assert_eq!(row_text(&grid, 0), "abcd");
    assert_eq!(row_text(&grid, 1), "e");
    assert_eq!((grid.cursor_col(), grid.cursor_row()), (1, 1));
}

#[test]
fn a_full_width_bottom_row_does_not_scroll_the_screen() {
    // The reported defect: a full-screen monitor paints a status bar across
    // the whole bottom row. Taking the wrap eagerly turned that bar's last
    // glyph into a line feed on the bottom margin, scrolling every row up
    // one — after which the tool's next partial repaint landed a row out.
    let grid = render_bytes(4, 3, b"top\x1b[3;1Habcd");
    assert_eq!(row_text(&grid, 0), "top");
    assert_eq!(row_text(&grid, 1), "");
    assert_eq!(row_text(&grid, 2), "abcd");
    assert_eq!((grid.cursor_col(), grid.cursor_row()), (3, 2));
}

#[test]
fn the_glyph_after_a_full_bottom_row_scrolls_exactly_one_line() {
    let grid = render_bytes(4, 3, b"top\x1b[3;1Habcde");
    assert_eq!(row_text(&grid, 0), "");
    assert_eq!(row_text(&grid, 1), "abcd");
    assert_eq!(row_text(&grid, 2), "e");
}

#[test]
fn an_absolute_move_cancels_the_owed_wrap() {
    let grid = render_bytes(4, 2, b"abcd\x1b[1;1HZ");
    assert_eq!(row_text(&grid, 0), "Zbcd");
    assert_eq!(row_text(&grid, 1), "");
}

#[test]
fn a_carriage_return_cancels_the_owed_wrap() {
    let grid = render_bytes(4, 2, b"abcd\rY");
    assert_eq!(row_text(&grid, 0), "Ybcd");
    assert_eq!(row_text(&grid, 1), "");
}

#[test]
fn a_rubout_at_the_right_edge_erases_the_glyph_that_filled_the_row() {
    // A rubout is backspace, space, backspace: with the wrap owed the cursor
    // already rests on the last column, so it erases the row's final glyph
    // rather than the one before it.
    let grid = render_bytes(4, 2, b"abcd\x08 \x08");
    assert_eq!(row_text(&grid, 0), "abc");
    assert_eq!(glyph(&grid, 3, 0), ' ');
    assert_eq!((grid.cursor_col(), grid.cursor_row()), (3, 0));
}

#[test]
fn erase_to_end_of_line_from_the_owed_wrap_clears_the_last_cell() {
    let grid = render_bytes(4, 2, b"abcd\x1b[K");
    assert_eq!(row_text(&grid, 0), "abc");
    assert_eq!(glyph(&grid, 3, 0), ' ');
}

#[test]
fn setting_the_scroll_region_homes_the_cursor_into_the_region() {
    // `DECSTBM` homes to the top-left of the region, not of the screen, so a
    // program that reserves a header row starts drawing below it.
    let grid = render_bytes(4, 4, b"\x1b[2;4r");
    assert_eq!((grid.cursor_col(), grid.cursor_row()), (0, 1));
}

/// The shared conformance script's view of this screen model.
struct ConformanceGrid {
    grid: Grid,
}

impl tairix_vt::conformance::ScreenModel for ConformanceGrid {
    fn cols(&self) -> u16 {
        self.grid.cols()
    }

    fn rows(&self) -> u16 {
        self.grid.rows()
    }

    fn apply(&mut self, op: &Op) {
        crate::parser::apply(&mut self.grid, op);
    }

    fn glyph(&self, col: u16, row: u16) -> char {
        glyph(&self.grid, col, row)
    }

    fn cursor(&self) -> (u16, u16) {
        (self.grid.cursor_col(), self.grid.cursor_row())
    }
}

#[test]
fn the_screen_model_passes_the_shared_conformance_script() {
    // The framebuffer console runs this same script, so the two screens a
    // program can be drawn on cannot disagree about where its output lands.
    let mut screen = ConformanceGrid {
        grid: Grid::new(
            tairix_vt::conformance::COLS,
            tairix_vt::conformance::ROWS,
            ROOM,
        )
        .expect("valid conformance size"),
    };
    if let Err(divergence) = tairix_vt::conformance::check(&mut screen) {
        panic!("screen conformance: {divergence:?}");
    }
}

#[test]
fn overwriting_either_half_of_a_wide_glyph_clears_the_other_half() {
    for col in [0, 1] {
        let mut grid = Grid::new(4, 1, ROOM).expect("valid size");
        grid.write_char('日');
        grid.move_to(col, 0);
        grid.write_char('x');

        assert_eq!(glyph(&grid, col, 0), 'x');
        assert_eq!(glyph(&grid, 1 - col, 0), ' ');
    }
}

#[test]
fn partial_erase_expands_to_clear_a_complete_wide_glyph() {
    let mut grid = Grid::new(4, 1, ROOM).expect("valid size");
    grid.write_char('日');
    grid.move_to(0, 0);
    grid.erase_in_line(tairix_vt::EraseMode::ToStart);
    assert_eq!((glyph(&grid, 0, 0), glyph(&grid, 1, 0)), (' ', ' '));

    grid.move_to(0, 0);
    grid.write_char('日');
    grid.move_to(1, 0);
    grid.erase_in_line(tairix_vt::EraseMode::ToEnd);
    assert_eq!((glyph(&grid, 0, 0), glyph(&grid, 1, 0)), (' ', ' '));
}

#[test]
fn writing_text_fills_cells_and_advances_cursor() {
    let grid = render_bytes(20, 3, b"hi");
    assert_eq!(row_text(&grid, 0), "hi");
    assert_eq!(grid.cursor_col(), 2);
    assert_eq!(grid.cursor_row(), 0);
}

#[test]
fn writing_past_the_right_edge_wraps_to_the_next_row() {
    let grid = render_bytes(3, 3, b"abcd");
    assert_eq!(row_text(&grid, 0), "abc");
    assert_eq!(row_text(&grid, 1), "d");
    assert_eq!(grid.cursor_row(), 1);
    assert_eq!(grid.cursor_col(), 1);
}

#[test]
fn line_feed_on_the_last_row_scrolls_the_screen_up() {
    // A tty emits CRLF between lines; line feed alone only moves down.
    let grid = render_bytes(4, 2, b"aa\r\nbb\r\ncc");
    // "aa" scrolled off the top; "bb" moved up and "cc" is on the last row.
    assert_eq!(row_text(&grid, 0), "bb");
    assert_eq!(row_text(&grid, 1), "cc");
    assert_eq!(grid.cursor_row(), 1);
}

#[test]
fn line_feed_alone_moves_down_without_returning_to_column_zero() {
    let grid = render_bytes(10, 3, b"ab\ncd");
    assert_eq!(row_text(&grid, 0), "ab");
    // No carriage return, so the second word starts under the cursor column.
    assert_eq!(glyph(&grid, 2, 1), 'c');
    assert_eq!(glyph(&grid, 3, 1), 'd');
    assert_eq!(grid.cursor_row(), 1);
}

#[test]
fn carriage_return_returns_to_column_zero_and_overwrites() {
    let grid = render_bytes(10, 2, b"abc\rX");
    assert_eq!(row_text(&grid, 0), "Xbc");
    assert_eq!(grid.cursor_col(), 1);
}

#[test]
fn backspace_moves_left_without_erasing() {
    let grid = render_bytes(10, 2, b"abc\x08");
    assert_eq!(row_text(&grid, 0), "abc");
    assert_eq!(grid.cursor_col(), 2);
}

#[test]
fn tab_advances_to_the_next_tab_stop() {
    let grid = render_bytes(20, 2, b"a\tb");
    assert_eq!(glyph(&grid, 8, 0), 'b');
    assert_eq!(glyph(&grid, 0, 0), 'a');
}

#[test]
fn csi_cursor_position_is_one_based() {
    // ESC[2;3H places the cursor on row 2, column 3 (1-based).
    let mut grid = render_bytes(10, 5, b"\x1b[2;3H");
    assert_eq!(grid.cursor_row(), 1);
    assert_eq!(grid.cursor_col(), 2);
    grid.write_char('Z');
    assert_eq!(glyph(&grid, 2, 1), 'Z');
}

#[test]
fn csi_home_with_no_params_is_top_left() {
    let grid = render_bytes(10, 5, b"abc\x1b[H");
    assert_eq!(grid.cursor_row(), 0);
    assert_eq!(grid.cursor_col(), 0);
}

#[test]
fn csi_relative_cursor_moves_default_to_one() {
    // Down two, right three, then up one, left one.
    let grid = render_bytes(10, 10, b"\x1b[2B\x1b[3C\x1b[A\x1b[D");
    assert_eq!(grid.cursor_row(), 1);
    assert_eq!(grid.cursor_col(), 2);
}

#[test]
fn csi_erase_in_line_clears_to_end() {
    let grid = render_bytes(10, 2, b"abcdef\x1b[3D\x1b[K");
    assert_eq!(row_text(&grid, 0), "abc");
}

#[test]
fn csi_erase_in_display_two_clears_everything() {
    let grid = render_bytes(6, 2, b"hello\nworld\x1b[2J");
    assert_eq!(row_text(&grid, 0), "");
    assert_eq!(row_text(&grid, 1), "");
}

#[test]
fn unrecognised_escape_and_high_bytes_are_dropped() {
    // A non-CSI escape (ESC c), a private mode we do not model (bracketed
    // paste, ESC[?2004l), and a stray high byte all leave the visible text
    // intact: the shared parser drops what it does not recognise.
    let grid = render_bytes(10, 2, b"a\x1bcb\x1b[?2004lc\xffd");
    assert_eq!(row_text(&grid, 0), "abcd");
}

#[test]
fn pump_applies_shell_output_to_the_screen() {
    let shell = QueueShell::with_output(&[b"hello"]);
    let mut term = Terminal::new(20, 3, shell, ROOM).expect("valid size");
    let applied = term.pump().expect("read succeeds");
    assert_eq!(applied, 5);
    assert_eq!(row_text(term.grid(), 0), "hello");
}

#[test]
fn pump_with_no_output_changes_nothing() {
    let shell = QueueShell::default();
    let mut term = Terminal::new(20, 3, shell, ROOM).expect("valid size");
    let applied = term.pump().expect("empty read succeeds");
    assert_eq!(applied, 0);
    assert_eq!(row_text(term.grid(), 0), "");
}

#[test]
fn pump_propagates_a_read_error_and_leaves_the_screen_unchanged() {
    let mut shell = QueueShell::with_output(&[b"after"]);
    shell.read_error = Some(Errno::NotFound);
    let mut term = Terminal::new(20, 3, shell, ROOM).expect("valid size");
    assert_eq!(term.pump(), Err(Errno::NotFound));
    assert_eq!(row_text(term.grid(), 0), "");
}

#[test]
fn send_forwards_input_to_the_shell() {
    let mut term = terminal(20, 3);
    term.send_str("ls\n").expect("write succeeds");
    term.send(b"\x03").expect("write succeeds");
    // The model never echoes input to the screen.
    assert_eq!(row_text(term.grid(), 0), "");
}

#[test]
fn shell_seam_captures_written_bytes_verbatim() {
    let mut shell = QueueShell::default();
    shell.write(b"ls\n").expect("write succeeds");
    shell.write(b"\x03").expect("write succeeds");
    assert_eq!(shell.written, b"ls\n\x03");
}

#[test]
fn send_propagates_a_write_error() {
    let shell = QueueShell {
        write_error: Some(Errno::PermissionDenied),
        ..QueueShell::default()
    };
    let mut term = Terminal::new(20, 3, shell, ROOM).expect("valid size");
    assert_eq!(term.send_str("x"), Err(Errno::PermissionDenied));
}

/// The colours a terminal on the system scheme paints with, fully opaque —
/// what a test that is not about the profile expects to see.
fn system_colors() -> crate::scheme::Painted {
    crate::scheme::Painted::resolve(
        crate::scheme::Scheme::System,
        &crate::profile::Profile::default().custom,
        u8::MAX,
    )
}

/// A `width` × `height` screen painted once from `grid` — the whole-window
/// first paint every renderer test that only cares about pixels wants.
fn painted_screen(
    grid: &Grid,
    painted: &crate::scheme::Painted,
    width: u32,
    height: u32,
) -> Screen {
    let mut screen = Screen::new(width, height).expect("surface");
    let _ = screen.paint(grid, painted, test_font());
    screen
}

/// The surface rectangle a `cols` × `rows` block of cells at `(col, row)`
/// occupies in the test face.
fn cell_rect(col: u32, row: u32, cols: u32, rows: u32) -> Rect {
    let font = test_font();
    let (cw, lh) = (font.cell_width(), font.line_height());
    Rect::new(
        i32::try_from(cw * col).expect("in range"),
        i32::try_from(lh * row).expect("in range"),
        cw * cols,
        lh * rows,
    )
}

/// A `cols` × `rows` terminal and a screen sized exactly to it.
fn grid_and_screen(cols: u16, rows: u16) -> (Terminal<QueueShell>, Screen) {
    let font = test_font();
    let terminal = terminal(cols, rows);
    let screen = Screen::new(
        font.cell_width() * u32::from(cols),
        font.line_height() * u32::from(rows),
    )
    .expect("surface");
    (terminal, screen)
}

#[test]
fn render_produces_a_surface_of_the_viewport_size() {
    let shell = QueueShell::with_output(&[b"hi"]);
    let mut term = Terminal::new(20, 3, shell, ROOM).expect("valid size");
    term.pump().expect("read succeeds");
    let screen = painted_screen(term.grid(), &system_colors(), 120, 40);
    assert_eq!(screen.surface().width(), 120);
    assert_eq!(screen.surface().height(), 40);
}

#[test]
fn hebrew_glyphs_occupy_distinct_single_cells() {
    let grid = render_bytes(5, 1, "אבםa".as_bytes());
    assert_eq!(row_text(&grid, 0), "אבםa");

    let overwritten = render_bytes(3, 1, "אב\u{1B}[1;2H ".as_bytes());
    assert_eq!(glyph(&overwritten, 0, 0), 'א');
    assert_eq!(glyph(&overwritten, 1, 0), ' ');
}

#[test]
fn render_keeps_hebrew_ink_inside_each_coloured_cell() {
    let mut term = terminal(4, 1);
    term.feed("\u{1B}[?25l\u{1B}[48;2;10;20;30mאבם".as_bytes());
    let screen = painted_screen(term.grid(), &system_colors(), 60, 28);
    let surface = screen.surface();
    let background = Color::rgb(10, 20, 30);
    for column in 0..3 {
        let first_x = column * 15;
        assert!(
            (first_x..first_x + 15).any(|x| {
                (0..28).any(|y| {
                    surface.get(x, y).map(tairix_raster::Pixel::unpremultiply) != Some(background)
                })
            }),
            "Hebrew cell {column} contains only background"
        );
    }
}

#[test]
fn render_keeps_wide_japanese_ink_over_a_coloured_background() {
    let mut term = terminal(3, 1);
    term.feed("\u{1B}[?25l\u{1B}[48;2;10;20;30m日".as_bytes());
    let screen = painted_screen(term.grid(), &system_colors(), 45, 28);
    let surface = screen.surface();
    let background = Color::rgb(10, 20, 30);
    assert!(
        (15..30).any(|x| {
            (0..28).any(|y| {
                surface.get(x, y).map(tairix_raster::Pixel::unpremultiply) != Some(background)
            })
        }),
        "the continuation cell contains only background"
    );
}

#[test]
fn render_highlights_the_cursor_cell() {
    let mut term = terminal(20, 3);
    term.feed(b"\x1b[H");
    let screen = painted_screen(term.grid(), &system_colors(), 120, 40);
    let surface = screen.surface();
    let accent: Color = Palette::DARK.accent.into();
    let surface_bg: Color = Palette::DARK.surface.into();
    // The top-left pixel sits under the home cursor, so it carries the accent
    // fill rather than the plain surface colour.
    let top_left = surface.get(0, 0).map(tairix_raster::Pixel::unpremultiply);
    assert_eq!(top_left, Some(accent));
    assert_ne!(accent, surface_bg);
}

#[test]
fn render_handles_a_zero_sized_viewport_without_panicking() {
    let term = terminal(20, 3);
    // A zero-width viewport is degenerate but allocatable: it paints nothing.
    let screen = painted_screen(term.grid(), &system_colors(), 0, 40);
    assert_eq!(screen.surface().width(), 0);
    assert!(screen.surface().pixels().is_empty());
}

#[test]
fn a_first_paint_damages_the_whole_window() {
    let (term, mut screen) = grid_and_screen(20, 3);
    let damage = screen.paint(term.grid(), &system_colors(), test_font());
    assert_eq!(damage, cell_rect(0, 0, 20, 3));
}

/// The reported defect: typing a character must cost the cell it wrote and
/// the two the cursor moved between — never the whole window.
#[test]
fn typing_one_character_damages_only_the_cells_it_touched() {
    let painted = system_colors();
    let (mut term, mut screen) = grid_and_screen(80, 24);
    let _ = screen.paint(term.grid(), &painted, test_font());

    term.feed(b"a");

    // Column 0 gained a glyph and the cursor block moved to column 1.
    assert_eq!(
        screen.paint(term.grid(), &painted, test_font()),
        cell_rect(0, 0, 2, 1)
    );
}

#[test]
fn a_repaint_of_an_unchanged_grid_damages_nothing() {
    let painted = system_colors();
    let (mut term, mut screen) = grid_and_screen(20, 3);
    term.feed(b"steady");
    let _ = screen.paint(term.grid(), &painted, test_font());

    assert!(screen.paint(term.grid(), &painted, test_font()).is_empty());
}

#[test]
fn a_hidden_cursor_reappearing_damages_only_its_own_cell() {
    let painted = system_colors();
    let (mut term, mut screen) = grid_and_screen(20, 3);
    term.feed(b"\x1b[2;3H\x1b[?25l");
    let _ = screen.paint(term.grid(), &painted, test_font());

    term.feed(b"\x1b[?25h");

    assert_eq!(
        screen.paint(term.grid(), &painted, test_font()),
        cell_rect(2, 1, 1, 1)
    );
}

/// Overwriting the *continuation* half of a wide glyph must repaint the lead
/// cell too, or the orphaned left half of the old glyph would survive.
#[test]
fn clobbering_a_continuation_cell_damages_its_lead_cell() {
    let painted = system_colors();
    let (mut term, mut screen) = grid_and_screen(10, 1);
    term.feed("\u{1B}[?25l日本".as_bytes());
    let _ = screen.paint(term.grid(), &painted, test_font());

    // Column 1 is 日's continuation; writing there leaves column 0 stale.
    term.feed(b"\x1b[1;2Hx");

    let damage = screen.paint(term.grid(), &painted, test_font());
    assert_eq!(damage.left(), 0, "the lead cell was not repainted");
}

/// The safety property behind repainting only part of the window: whatever
/// the change, the retained surface must end up byte-identical to a fresh
/// whole-window paint of the same grid.
#[test]
fn an_incremental_repaint_matches_a_whole_one_pixel_for_pixel() {
    const CASES: &[(&str, &str)] = &[
        ("hello", "!"),
        ("hello", "\u{1B}[1;1Hx"),
        ("ab", "\u{1B}[?25l"),
        ("\u{1B}[?25labc", "\u{1B}[?25h"),
        ("line one\r\nline two", "\r\n\r\n\r\nscrolled"),
        ("日本", "\u{1B}[1;2Hx"),
        ("日本", "\u{1B}[1;1H\u{1B}[K"),
        ("x", "\u{1B}[41mred"),
        ("\u{1B}[41mred", "\u{1B}[0m plain"),
        ("filled text here", "\u{1B}[2J"),
        ("abc", "\u{1B}[1;1H\u{1B}[1;33mA"),
    ];
    let painted = system_colors();
    let font = test_font();
    for (setup, change) in CASES {
        let (mut term, mut incremental) = grid_and_screen(12, 4);
        term.feed(setup.as_bytes());
        let _ = incremental.paint(term.grid(), &painted, font);
        term.feed(change.as_bytes());
        let _ = incremental.paint(term.grid(), &painted, font);

        let whole = painted_screen(
            term.grid(),
            &painted,
            incremental.surface().width(),
            incremental.surface().height(),
        );
        assert_eq!(
            incremental.surface().pixels(),
            whole.surface().pixels(),
            "incremental repaint diverged after {change:?}"
        );
    }
}

/// The other half of the safety argument: every pixel a repaint *changes*
/// must lie inside the rectangle it reported, or the session would leave a
/// stale patch on screen. Walked over a script that scrolls, recolours,
/// erases and clobbers wide glyphs, checking both directions each step.
#[test]
fn every_pixel_a_repaint_changes_lies_inside_the_rect_it_reported() {
    const SCRIPT: &[&str] = &[
        "hello world",
        "\r\n",
        "\u{1B}[41mred on black",
        "\u{1B}[0m\r\n日本語",
        "\u{1B}[1;1Hx",
        "\u{1B}[2;2H\u{1B}[K",
        "\u{1B}[?25l",
        "more text\r\n",
        "\u{1B}[?25h",
        "\r\n\r\n\r\nscrolled off",
        "\u{1B}[H\u{1B}[1;33mA",
        "\u{1B}[2J",
    ];
    let painted = system_colors();
    let font = test_font();
    let (mut term, mut screen) = grid_and_screen(12, 4);
    let _ = screen.paint(term.grid(), &painted, font);

    for step in SCRIPT {
        let before = screen.surface().clone();
        term.feed(step.as_bytes());
        let reported = screen.paint(term.grid(), &painted, font);

        for y in 0..screen.surface().height() {
            for x in 0..screen.surface().width() {
                if screen.surface().get(x, y) == before.get(x, y) {
                    continue;
                }
                assert!(
                    reported.contains(Point::new(
                        i32::try_from(x).expect("in range"),
                        i32::try_from(y).expect("in range")
                    )),
                    "{step:?} changed ({x}, {y}) outside the reported {reported:?}"
                );
            }
        }

        let whole = painted_screen(
            term.grid(),
            &painted,
            screen.surface().width(),
            screen.surface().height(),
        );
        assert_eq!(
            screen.surface().pixels(),
            whole.surface().pixels(),
            "the retained screen diverged after {step:?}"
        );
    }
}

#[test]
fn invalidating_forces_the_next_paint_to_cover_the_window() {
    let painted = system_colors();
    let (mut term, mut screen) = grid_and_screen(20, 3);
    term.feed(b"text");
    let _ = screen.paint(term.grid(), &painted, test_font());

    screen.invalidate();

    assert_eq!(
        screen.paint(term.grid(), &painted, test_font()),
        cell_rect(0, 0, 20, 3)
    );
}

#[test]
fn a_resize_repaints_the_whole_new_surface() {
    let painted = system_colors();
    let font = test_font();
    let (mut term, mut screen) = grid_and_screen(20, 3);
    term.feed(b"text");
    let _ = screen.paint(term.grid(), &painted, font);

    assert!(screen.resize(font.cell_width() * 10, font.line_height() * 2));
    term.resize(10, 2);
    assert_eq!(term.settle(), Reshaped::Took { cols: 10, rows: 2 });

    assert_eq!(
        screen.paint(term.grid(), &painted, font),
        cell_rect(0, 0, 10, 2)
    );
}

/// A grid reshaped under a screen of the same pixel size still repaints
/// whole: the cell snapshot no longer describes the new shape.
#[test]
fn a_reshaped_grid_repaints_the_whole_window() {
    let painted = system_colors();
    let (mut term, mut screen) = grid_and_screen(20, 3);
    let _ = screen.paint(term.grid(), &painted, test_font());

    term.resize(3, 20);
    assert_eq!(term.settle(), Reshaped::Took { cols: 3, rows: 20 });

    assert_eq!(
        screen.paint(term.grid(), &painted, test_font()),
        cell_rect(0, 0, 20, 3)
    );
}

#[test]
fn sgr_folds_colour_and_flags_into_the_written_cells() {
    // Bold + red foreground, then "Hi"; both glyphs carry the folded rendition.
    let grid = render_bytes(10, 2, b"\x1b[1;31mHi");
    let cell = cell_at(&grid, 0, 0);
    assert_eq!(cell.ch, 'H');
    assert!(cell.attrs.bold);
    assert_eq!(cell.attrs.foreground, VtColor::Basic(BasicColor::Red));
    assert_eq!(
        cell_at(&grid, 1, 0).attrs.foreground,
        VtColor::Basic(BasicColor::Red)
    );
}

#[test]
fn sgr_reset_returns_following_cells_to_plain() {
    let grid = render_bytes(10, 2, b"\x1b[1ma\x1b[0mb");
    assert!(cell_at(&grid, 0, 0).attrs.bold);
    assert_eq!(cell_at(&grid, 1, 0).attrs, tairix_vt::Attributes::PLAIN);
}

#[test]
fn sgr_256_index_and_truecolour_reach_the_cells() {
    // 256-colour foreground 200, then truecolour background 10;20;30.
    let grid = render_bytes(10, 2, b"\x1b[38;5;200mx\x1b[48;2;10;20;30my");
    assert_eq!(cell_at(&grid, 0, 0).attrs.foreground, VtColor::Indexed(200));
    let y = cell_at(&grid, 1, 0);
    assert_eq!(y.attrs.foreground, VtColor::Indexed(200));
    assert_eq!(y.attrs.background, VtColor::Rgb(10, 20, 30));
}

#[test]
fn scroll_region_confines_scrolling_to_its_rows() {
    // Width 4 leaves a trailing blank column so filling a row never wraps.
    let mut grid = Grid::new(4, 3, ROOM).expect("valid size");
    put_row(&mut grid, 0, "aaa");
    put_row(&mut grid, 1, "bbb");
    put_row(&mut grid, 2, "ccc");
    // Restrict scrolling to rows 2..=3 (1-based) and scroll that region up one.
    grid.set_scroll_region(2, 3);
    grid.scroll_up(1);
    assert_eq!(row_text(&grid, 0), "aaa"); // outside the region, untouched
    assert_eq!(row_text(&grid, 1), "ccc"); // pulled up within the region
    assert_eq!(row_text(&grid, 2), ""); // freed bottom line blanked
}

#[test]
fn line_feed_at_the_bottom_margin_scrolls_only_the_region() {
    let mut grid = Grid::new(4, 3, ROOM).expect("valid size");
    put_row(&mut grid, 0, "aaa");
    put_row(&mut grid, 1, "bbb");
    put_row(&mut grid, 2, "ccc");
    // Region rows 2..=3 (1-based); homing leaves the cursor at the top, so step
    // it down to the bottom margin and feed a line: the region scrolls up but
    // row 1 (outside it) is untouched.
    grid.set_scroll_region(2, 3);
    grid.move_to(0, 2);
    grid.line_feed();
    assert_eq!(row_text(&grid, 0), "aaa");
    assert_eq!(row_text(&grid, 1), "ccc");
    assert_eq!(row_text(&grid, 2), "");
    assert_eq!(grid.cursor_row(), 2); // stays on the bottom margin
}

#[test]
fn alternate_screen_saves_and_restores_the_main_screen() {
    let mut term = terminal(10, 2);
    term.feed(b"main");
    term.feed(b"\x1b[?1049h");
    assert!(term.grid().on_alternate_screen());
    // The alternate screen starts blank; what we draw there is independent.
    assert_eq!(row_text(term.grid(), 0), "");
    term.feed(b"alt");
    assert_eq!(row_text(term.grid(), 0), "alt");
    term.feed(b"\x1b[?1049l");
    assert!(!term.grid().on_alternate_screen());
    assert_eq!(row_text(term.grid(), 0), "main");
}

#[test]
fn cursor_visibility_follows_the_show_hide_sequences() {
    let mut term = terminal(10, 2);
    assert!(term.grid().cursor_visible());
    term.feed(b"\x1b[?25l");
    assert!(!term.grid().cursor_visible());
    term.feed(b"\x1b[?25h");
    assert!(term.grid().cursor_visible());
}

#[test]
fn hidden_cursor_is_not_painted() {
    let mut term = terminal(20, 3);
    term.feed(b"\x1b[H\x1b[?25l");
    let screen = painted_screen(term.grid(), &system_colors(), 120, 40);
    let surface = screen.surface();
    let page: Color = Palette::DARK.document.into();
    // With the cursor hidden the home cell shows the plain page, not accent.
    let top_left = surface.get(0, 0).map(tairix_raster::Pixel::unpremultiply);
    assert_eq!(top_left, Some(page));
}

#[test]
fn osc_sets_the_window_title() {
    let grid = render_bytes(10, 2, b"\x1b]0;TAIRiX\x07rest");
    assert_eq!(grid.title(), "TAIRiX");
    // The title sequence leaves the screen text alone.
    assert_eq!(row_text(&grid, 0), "rest");
}

#[test]
fn saved_cursor_round_trips_position_and_pen() {
    let grid = render_bytes(10, 3, b"\x1b[2;3H\x1b[1m\x1b7\x1b[H\x1b[0m\x1b8Z");
    // ESC 8 restored the saved (row 2, col 3) position and the bold pen, so the
    // glyph lands there and is bold.
    let cell = cell_at(&grid, 2, 1);
    assert_eq!(cell.ch, 'Z');
    assert!(cell.attrs.bold);
}

#[test]
fn emitter_output_is_parsed_identically_by_the_consumer() {
    // The "one vocabulary" guarantee: the emulator consumes exactly what
    // `lib/vt`'s emitter produces. Encode a representative operation stream and
    // feed the bytes straight into the consumer's grid.
    let ops = [
        Op::Sgr(Sgr::Foreground(VtColor::Rgb(0x30, 0x70, 0xf0))),
        Op::Sgr(Sgr::Bold),
        Op::Print('O'),
        Op::Print('k'),
        Op::CarriageReturn,
        Op::LineFeed,
        Op::Sgr(Sgr::Reset),
        Op::Print('p'),
    ];
    let grid = render_bytes(10, 3, &encode_all(&ops));

    let first = cell_at(&grid, 0, 0);
    assert_eq!(first.ch, 'O');
    assert!(first.attrs.bold);
    assert_eq!(first.attrs.foreground, VtColor::Rgb(0x30, 0x70, 0xf0));
    assert_eq!(cell_at(&grid, 1, 0).ch, 'k');

    let plain = cell_at(&grid, 0, 1);
    assert_eq!(plain.ch, 'p');
    assert_eq!(plain.attrs, tairix_vt::Attributes::PLAIN);
}

// --- The spawned shell's pty wiring (`spawned`) --------------------------

#[test]
fn shell_wires_route_input_output_and_diagnostics_onto_the_pty_slave() {
    use tairix_abi::{FdWire, SpawnAttach, SpawnSession, CONSOLE_INHERIT, SPAWN_UID_INHERIT};

    let attach = crate::spawned::shell_wires(7);
    // The child's stdin, stdout, *and* stderr are all the one pty slave (a
    // terminal shows output and diagnostics on the same tty, and the shell
    // sees one controlling terminal); stdinfo is closed.
    assert_eq!(attach.wires[0], FdWire::Handle(7));
    assert_eq!(attach.wires[1], FdWire::Handle(7));
    assert_eq!(attach.wires[2], FdWire::Handle(7));
    assert_eq!(attach.wires[3], FdWire::Closed);
    // Credential and console are inherited: the wires narrow, never widen.
    assert_eq!(attach.target_uid, SPAWN_UID_INHERIT);
    assert_eq!(attach.console, CONSOLE_INHERIT);
    assert_eq!(attach.flags, 0);
    // The shell anchors its own session, so its jobs end with the window.
    assert_eq!(attach.session, SpawnSession::New);
    // The block is canonical: it survives the same parse the kernel runs.
    assert_eq!(SpawnAttach::parse(&attach.to_le_bytes()), Ok(attach));
}

#[test]
fn shell_env_forwards_the_inherited_environment_and_sets_term() {
    use crate::spawned::shell_env;

    // The logged-in user's identity and locale reach the shell unchanged, so
    // its prompt shows the real user (never the "user@host" fallback), and
    // the terminal's own TERM is appended.
    let inherited: [&[u8]; 4] = [
        b"USER=root",
        b"HOME=/Users/root",
        b"LOGNAME=root",
        b"LANG=en-US",
    ];
    let env = shell_env("xterm-256color", inherited.iter().copied());
    assert_eq!(
        env,
        alloc::vec![
            b"USER=root".to_vec(),
            b"HOME=/Users/root".to_vec(),
            b"LOGNAME=root".to_vec(),
            b"LANG=en-US".to_vec(),
            b"TERM=xterm-256color".to_vec(),
        ]
    );
}

#[test]
fn shell_env_replaces_any_inherited_term_with_the_emulators_own() {
    use crate::spawned::shell_env;

    // A stale inherited TERM (from the text console the session descended
    // from) must never describe this graphical emulator: it is dropped and
    // the terminal's own value is the only TERM the shell sees.
    let inherited: [&[u8]; 3] = [b"TERM=vt100", b"USER=root", b"TERM=linux"];
    let env = shell_env("xterm-256color", inherited.iter().copied());
    assert_eq!(
        env,
        alloc::vec![b"USER=root".to_vec(), b"TERM=xterm-256color".to_vec()]
    );
    // Exactly one TERM entry, and it is the emulator's.
    assert_eq!(
        env.iter().filter(|e| e.starts_with(b"TERM=")).count(),
        1,
        "the shell must see exactly one TERM, naming this emulator",
    );
}

#[test]
fn shell_env_with_no_inherited_environment_still_sets_term() {
    use crate::spawned::shell_env;

    // A terminal spawned with an empty environment (no login ran) still hands
    // the shell a valid TERM rather than nothing.
    let env = shell_env("xterm-256color", core::iter::empty());
    assert_eq!(env, alloc::vec![b"TERM=xterm-256color".to_vec()]);
}

#[test]
fn pipe_source_read_drains_one_bounded_chunk() {
    let data: &[u8] = b"prompt$ ";
    let mut served = false;
    let mut source = crate::spawned::StreamShellSource::new(
        |out: &mut [u8]| {
            assert_eq!(out.len(), crate::spawned::READ_CHUNK);
            assert!(!served, "one wake drains exactly one chunk");
            served = true;
            out[..data.len()].copy_from_slice(data);
            Ok(data.len())
        },
        |_: &[u8]| -> Result<usize, Errno> { unreachable!("read never writes") },
    );
    assert_eq!(source.read(), Ok(data.to_vec()));
}

#[test]
fn pipe_source_read_surfaces_eof_and_errors_as_refusals() {
    // End-of-stream (the shell exited, pipe drained) is the seam's typed
    // "shell has exited" refusal, never a fabricated empty read.
    let mut source = crate::spawned::StreamShellSource::new(
        |_: &mut [u8]| Ok(0),
        |_: &[u8]| -> Result<usize, Errno> { unreachable!() },
    );
    assert_eq!(source.read(), Err(Errno::NotFound));
    // A failing primitive propagates untouched.
    let mut failing = crate::spawned::StreamShellSource::new(
        |_: &mut [u8]| Err(Errno::BadAddress),
        |_: &[u8]| -> Result<usize, Errno> { unreachable!() },
    );
    assert_eq!(failing.read(), Err(Errno::BadAddress));
}

#[test]
fn pipe_source_write_loops_over_short_writes_until_delivered() {
    let mut delivered: Vec<u8> = Vec::new();
    let mut calls = 0usize;
    {
        let mut source = crate::spawned::StreamShellSource::new(
            |_: &mut [u8]| -> Result<usize, Errno> { unreachable!("write never reads") },
            |bytes: &[u8]| {
                calls += 1;
                // Accept at most three bytes per call: a full pipe accepts
                // its free space, and the source must resume at the tail.
                let n = bytes.len().min(3);
                delivered.extend_from_slice(&bytes[..n]);
                Ok(n)
            },
        );
        assert_eq!(source.write(b"echo hi\n"), Ok(()));
    }
    assert_eq!(delivered, b"echo hi\n");
    assert_eq!(calls, 3);
}

#[test]
fn pipe_source_write_fails_closed_on_a_wedged_or_failing_channel() {
    // A zero-byte acceptance for a non-empty remainder can only mean a
    // broken channel: fail closed rather than spin.
    let mut wedged = crate::spawned::StreamShellSource::new(
        |_: &mut [u8]| -> Result<usize, Errno> { unreachable!() },
        |_: &[u8]| Ok(0),
    );
    assert_eq!(wedged.write(b"x"), Err(Errno::BrokenPipe));
    // A failing primitive propagates untouched.
    let mut failing = crate::spawned::StreamShellSource::new(
        |_: &mut [u8]| -> Result<usize, Errno> { unreachable!() },
        |_: &[u8]| Err(Errno::BrokenPipe),
    );
    assert_eq!(failing.write(b"x"), Err(Errno::BrokenPipe));
    // A zero-length write is complete before the primitive is consulted.
    let mut untouched = crate::spawned::StreamShellSource::new(
        |_: &mut [u8]| -> Result<usize, Errno> { unreachable!() },
        |_: &[u8]| -> Result<usize, Errno> { unreachable!("nothing to deliver") },
    );
    assert_eq!(untouched.write(b""), Ok(()));
}

#[test]
fn a_shell_reap_names_a_load_failure_and_tells_gone_from_running() {
    use tairix_abi::{Reap, LOAD_MALFORMED, LOAD_NOT_FOUND, LOAD_OOM, LOAD_UNVERIFIED};

    use crate::spawned::{shell_reap, ShellReap};

    for (status, reason) in [
        (LOAD_NOT_FOUND, "program not found or not readable"),
        (LOAD_UNVERIFIED, "signature or hash verification failed"),
        (LOAD_MALFORMED, "executable is malformed or incompatible"),
        (LOAD_OOM, "out of memory while loading"),
    ] {
        assert_eq!(
            shell_reap(Reap::Exited(status)),
            ShellReap::Gone(Some(reason))
        );
    }
    assert_eq!(shell_reap(Reap::Exited(0)), ShellReap::Gone(None));
    assert_eq!(shell_reap(Reap::Exited(1)), ShellReap::Gone(None));
    assert_eq!(shell_reap(Reap::Running), ShellReap::Running);
    assert_eq!(
        shell_reap(Reap::Gone),
        ShellReap::Gone(None),
        "a child the kernel no longer knows has nothing left to reap"
    );
}

#[test]
fn a_closed_windows_shell_is_reaped_on_its_own_exit_and_an_unwatched_one_on_any_wake() {
    use crate::spawned::{EndingShell, EndingShells, ShellReap};

    let watched = EndingShell {
        token: Some(40),
        pid: 7,
    };
    let unwatched = EndingShell {
        token: None,
        pid: 8,
    };
    let mut ending = EndingShells::new();
    ending.push(watched);
    ending.push(unwatched);
    assert!(ending.watches(40));
    assert!(!ending.watches(41));

    let mut gone = alloc::vec::Vec::new();
    // Another member's wake reaps only the unwatched shell, still running.
    ending.reap(
        Some(41),
        |_| ShellReap::Running,
        |shell, _| gone.push(shell),
    );
    assert!(gone.is_empty());
    // Now exited, it is reaped on that wake, and the watched one waits for its own.
    ending.reap(
        Some(41),
        |_| ShellReap::Gone(None),
        |shell, _| gone.push(shell),
    );
    assert_eq!(gone, [unwatched]);
    assert!(ending.watches(40));
    ending.reap(
        Some(40),
        |_| ShellReap::Gone(Some("program not found or not readable")),
        |shell, reason| {
            assert_eq!(reason, Some("program not found or not readable"));
            gone.push(shell);
        },
    );
    assert_eq!(gone, [unwatched, watched]);
    assert!(!ending.watches(40), "a reaped shell is let go");
}

#[test]
fn a_reflow_rejoins_what_it_wrapped_and_a_round_trip_restores_it() {
    let mut grid = render_bytes(10, 3, b"abcdefghijklmno");
    assert!(grid.resize(4, 3));
    // Fifteen glyphs at four columns are four rows, the first in the
    // scrollback.
    assert_eq!(grid.scrollback_len(), 1);
    assert_eq!(row_text(&grid, 0), "efgh");
    assert_eq!(row_text(&grid, 1), "ijkl");
    assert_eq!(row_text(&grid, 2), "mno");
    assert_eq!((grid.cursor_col(), grid.cursor_row()), (3, 2));

    assert!(grid.resize(10, 3));
    assert_eq!(grid.scrollback_len(), 0);
    assert_eq!(row_text(&grid, 0), "abcdefghij");
    assert_eq!(row_text(&grid, 1), "klmno");
    assert_eq!((grid.cursor_col(), grid.cursor_row()), (5, 1));
}

#[test]
fn a_wide_glyph_reflows_whole_without_the_padding_it_left() {
    let mut grid = render_bytes(5, 2, "abcd世".as_bytes());
    assert_eq!(glyph(&grid, 4, 0), ' ');
    assert_eq!(glyph(&grid, 0, 1), '世');

    assert!(grid.resize(10, 2));
    assert_eq!(
        glyph(&grid, 4, 0),
        '世',
        "the padding was never part of the line"
    );
    assert_eq!(glyph(&grid, 5, 0), tairix_vt::CONTINUATION);
    assert_eq!((grid.cursor_col(), grid.cursor_row()), (6, 0));

    assert!(grid.resize(5, 2));
    assert_eq!(row_text(&grid, 0), "abcd");
    assert_eq!(glyph(&grid, 0, 1), '世');
    assert_eq!(glyph(&grid, 1, 1), tairix_vt::CONTINUATION);
    assert_eq!((grid.cursor_col(), grid.cursor_row()), (2, 1));
}

#[test]
fn a_reflow_keeps_an_owed_wrap_and_a_cursor_past_its_lines_end() {
    let mut grid = render_bytes(4, 2, b"abcd");
    assert!(grid.resize(8, 2));
    assert_eq!((grid.cursor_col(), grid.cursor_row()), (4, 0));
    assert!(grid.resize(4, 2));
    assert_eq!((grid.cursor_col(), grid.cursor_row()), (3, 0));
    grid.write_char('e');
    assert_eq!(row_text(&grid, 1), "e", "the wrap was owed again, and paid");

    let mut grid = render_bytes(10, 3, b"ab\x1b[6G");
    assert!(grid.resize(4, 3));
    assert_eq!((grid.cursor_col(), grid.cursor_row()), (1, 1));
    assert!(grid.resize(10, 3));
    assert_eq!((grid.cursor_col(), grid.cursor_row()), (5, 0));
}

#[test]
fn the_alternate_screen_is_clipped_over_a_reflowed_main_screen() {
    let mut grid = render_bytes(10, 2, b"abcdefghijkl\x1b[?1049h0123456789");
    assert!(grid.resize(5, 2));
    assert_eq!(row_text(&grid, 0), "01234");

    let mut parser = Parser::new();
    parser.feed(&mut grid, b"\x1b[?1049l");
    assert_eq!(grid.scrollback_len(), 1);
    assert_eq!(row_text(&grid, 0), "fghij");
    assert_eq!(row_text(&grid, 1), "kl");
    assert_eq!((grid.cursor_col(), grid.cursor_row()), (2, 1));
}

#[test]
fn the_alternate_screen_gives_the_scrollback_nothing() {
    let mut grid = render_bytes(10, 2, b"one\r\ntwo\r\nthree");
    assert_eq!(grid.scrollback_len(), 1);
    let mut parser = Parser::new();
    parser.feed(&mut grid, b"\x1b[?1049ha\r\nb\r\nc\r\n\x1b[2J");
    assert_eq!(grid.scrollback_len(), 0, "the view reaches none from it");
    assert!(!grid.scroll_view(5));
    parser.feed(&mut grid, b"\x1b[?1049l");
    assert_eq!(grid.scrollback_len(), 1);
}

#[test]
fn erasing_the_display_keeps_its_rows_and_erasing_the_saved_lines_forgets_them() {
    let mut grid = render_bytes(10, 3, b"a\r\nb\x1b[2J");
    assert_eq!(row_text(&grid, 0), "");
    assert_eq!(
        grid.scrollback_len(),
        2,
        "the rows down to the last one written"
    );
    assert!(grid.scroll_view(2));
    assert_eq!(view_text(&grid, 0), "a");
    assert_eq!(view_text(&grid, 1), "b");
    assert_eq!(view_text(&grid, 2), "");

    let mut parser = Parser::new();
    parser.feed(&mut grid, b"\x1b[3J");
    assert_eq!(grid.scrollback_len(), 0);
    assert_eq!(grid.view_offset(), 0);
}

#[test]
fn the_view_stays_on_its_lines_while_output_arrives() {
    let mut term = terminal(10, 2);
    term.feed(b"1\r\n2\r\n3\r\n4\r\n5");
    assert!(term.scroll_view(2));
    let shown = [view_text(term.grid(), 0), view_text(term.grid(), 1)];
    assert_eq!(shown, ["2", "3"]);

    term.feed(b"\r\n6\r\n7");
    assert_eq!(term.grid().view_offset(), 4);
    assert_eq!(
        [view_text(term.grid(), 0), view_text(term.grid(), 1)],
        shown
    );

    assert!(term.view_to_bottom());
    assert_eq!(view_text(term.grid(), 1), "7");
}

#[test]
fn scrolling_the_view_clamps_to_the_scrollback() {
    let mut term = terminal(10, 2);
    assert!(!term.scroll_view(1), "there is nothing to scroll back over");
    term.feed(b"1\r\n2\r\n3\r\n4");
    assert!(term.scroll_view(100));
    assert_eq!(term.grid().view_offset(), 2);
    assert!(!term.scroll_view(1));
    assert!(term.scroll_view(-1));
    assert_eq!(term.grid().view_offset(), 1);
    assert!(term.scroll_view(i32::MIN));
    assert_eq!(term.grid().view_offset(), 0);
    assert!(!term.view_to_bottom());
}

#[test]
fn the_view_shows_the_cursor_only_where_its_row_is() {
    let mut term = terminal(10, 3);
    term.feed(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\x1b[H");
    assert_eq!(term.grid().view_cursor(), Some((0, 0)));
    assert!(term.scroll_view(2));
    assert_eq!(term.grid().view_cursor(), Some((0, 2)));
    assert!(term.scroll_view(1));
    assert_eq!(term.grid().view_cursor(), None);
}

#[test]
fn a_view_on_the_oldest_line_moves_on_when_that_line_gives_way() {
    let mut term = Terminal::new(10, 1, QueueShell::default(), 3).expect("valid size");
    term.feed(b"1\r\n2\r\n3\r\n4\r\n5\r\n6");
    assert_eq!(term.grid().scrollback_len(), 3);
    assert!(term.scroll_view(3));
    assert_eq!(view_text(term.grid(), 0), "3", "the oldest two gave way");

    term.feed(b"\r\n7");
    assert_eq!(term.grid().view_offset(), 3);
    assert_eq!(view_text(term.grid(), 0), "4");

    assert!(term.shed_for(PressureBand::Severe));
    assert_eq!(
        term.grid().scrollback_len(),
        0,
        "severe pressure takes it all"
    );
    assert_eq!(term.grid().view_offset(), 0);
    term.feed(b"\r\n8");
    assert_eq!(
        term.grid().scrollback_len(),
        0,
        "and keeps none while it lasts"
    );
}

/// The scrollback keeps the last lines of output up to the user's limit, 500
/// unless they set another: a line wrapped across rows counts once, and a
/// lowered limit lets the oldest go at once.
#[test]
fn the_scrollback_keeps_the_last_lines_up_to_the_users_limit() {
    let lines = usize::from(crate::profile::Profile::default().scrollback_lines);
    assert_eq!(lines, 500);
    let mut term = Terminal::new(10, 1, QueueShell::default(), lines).expect("valid size");
    let output = (0..600).fold(String::new(), |mut output, n| {
        let _ = write!(output, "{n}\r\n");
        output
    });
    term.feed(output.as_bytes());
    assert_eq!(term.grid().scrollback_len(), 500);
    assert!(term.scroll_view(500));
    assert_eq!(
        view_text(term.grid(), 0),
        "100",
        "the oldest hundred gave way"
    );

    let mut term = Terminal::new(4, 1, QueueShell::default(), 3).expect("valid size");
    term.feed(b"abcdefghij\r\nk\r\nl\r\nm");
    assert_eq!(
        term.grid().scrollback_len(),
        5,
        "a line wrapped over three rows counts once among three"
    );
    assert!(term.set_scrollback_limit(1));
    assert_eq!(term.grid().scrollback_len(), 1);
    assert!(term.scroll_view(1));
    assert_eq!(view_text(term.grid(), 0), "l");
    assert!(!term.set_scrollback_limit(1), "nothing more to let go");
}

#[test]
fn the_clear_command_hands_what_it_cleared_to_the_scrollback() {
    let mut term = terminal(10, 3);
    term.feed(b"1\r\n2\r\n3\r\n4");
    assert!(term.scroll_view(1));
    term.clear();
    assert_eq!(row_text(term.grid(), 0), "");
    assert_eq!((term.grid().cursor_col(), term.grid().cursor_row()), (0, 0));
    assert_eq!(
        term.grid().view_offset(),
        0,
        "the view returns to the live screen"
    );
    assert_eq!(term.grid().scrollback_len(), 4);
    term.clear_scrollback();
    assert_eq!(term.grid().scrollback_len(), 0);
}

#[test]
fn a_view_scrolled_back_paints_as_a_whole_paint_of_it_would() {
    let painted = system_colors();
    let font = test_font();
    let (mut term, mut screen) = grid_and_screen(12, 4);
    term.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven");
    let _ = screen.paint(term.grid(), &painted, font);
    for delta in [2, 1, -3, 10, i32::MIN] {
        term.scroll_view(delta);
        let _ = screen.paint(term.grid(), &painted, font);
        let whole = painted_screen(
            term.grid(),
            &painted,
            screen.surface().width(),
            screen.surface().height(),
        );
        assert_eq!(
            screen.surface().pixels(),
            whole.surface().pixels(),
            "the retained screen diverged after scrolling {delta}"
        );
    }
}

#[test]
fn a_view_scrolled_past_the_cursors_row_draws_no_cursor() {
    let painted = system_colors();
    let accent: Color = Palette::DARK.accent.into();
    let mut term = terminal(20, 3);
    term.feed(b"\r\n\r\n\r\n\r\n\r\n\x1b[H");
    let top_left = |term: &Terminal<QueueShell>| {
        painted_screen(term.grid(), &painted, 120, 40)
            .surface()
            .get(0, 0)
            .map(tairix_raster::Pixel::unpremultiply)
    };
    assert_eq!(top_left(&term), Some(accent));
    assert!(term.scroll_view(3));
    assert_ne!(top_left(&term), Some(accent));
}

/// A wheel detent moves the view three lines back, a turn the other way
/// brings it forward, and a turn short of a line is carried to the next.
#[test]
fn the_wheel_scrolls_the_view_three_lines_a_detent_and_carries_the_rest() {
    use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
    let mut term = terminal(10, 2);
    term.feed(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n8\r\n9");
    assert!(term.scroll_wheel(-SCROLL_UNITS_PER_DETENT));
    assert_eq!(term.grid().view_offset(), 3);
    assert!(term.scroll_wheel(SCROLL_UNITS_PER_DETENT));
    assert_eq!(term.grid().view_offset(), 0);
    // A sixth of a detent is half a line.
    let sixth = -SCROLL_UNITS_PER_DETENT / 6;
    assert!(!term.scroll_wheel(sixth), "short of a line moves nothing");
    assert!(term.scroll_wheel(sixth), "but is carried");
    assert_eq!(term.grid().view_offset(), 1);
}

/// Shift with Page Up, Page Down, Home and End moves the view; the same keys
/// unshifted, or with another modifier held, are the shell's.
#[test]
fn the_scrolling_chords_move_the_view_and_leave_every_other_key_to_the_shell() {
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let mut term = terminal(10, 4);
    term.feed(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n8\r\n9\r\n10");
    let named = Key::Named;
    assert_eq!(term.view_key(named(NamedKey::PageUp), shift), Some(true));
    assert_eq!(term.grid().view_offset(), 3, "a screen less a line");
    assert_eq!(term.view_key(named(NamedKey::Home), shift), Some(true));
    assert_eq!(term.grid().view_offset(), term.grid().scrollback_len());
    assert_eq!(term.view_key(named(NamedKey::PageDown), shift), Some(true));
    assert_eq!(term.view_key(named(NamedKey::End), shift), Some(true));
    assert_eq!(term.grid().view_offset(), 0);
    assert_eq!(term.view_key(named(NamedKey::End), shift), Some(false));
    for modifiers in [
        Modifiers::default(),
        Modifiers {
            ctrl: true,
            ..shift
        },
    ] {
        assert_eq!(term.view_key(named(NamedKey::PageUp), modifiers), None);
    }
    assert_eq!(term.view_key(Key::Char('a'), shift), None);
}

/// What the user keeps is all working set: no pressure short of severe takes
/// any, severe takes it all and none is kept while that lasts, and the band
/// lifting lets it grow again. A limit of none keeps none.
#[test]
fn a_windows_scrollback_gives_way_only_to_severe_pressure() {
    let mut term = Terminal::new(10, 1, QueueShell::default(), ROOM).expect("valid size");
    term.feed(b"1\r\n2\r\n3\r\n4\r\n5\r\n6");
    assert_eq!(term.grid().scrollback_len(), 5);
    for band in [
        PressureBand::Normal,
        PressureBand::Mild,
        PressureBand::Moderate,
    ] {
        assert!(!term.shed_for(band), "{band:?} takes none");
    }
    assert!(term.shed_for(PressureBand::Severe));
    assert_eq!(term.grid().scrollback_len(), 0);
    term.feed(b"\r\n7");
    assert_eq!(term.grid().scrollback_len(), 0);
    assert!(!term.shed_for(PressureBand::Normal));
    term.feed(b"\r\n8");
    assert_eq!(term.grid().scrollback_len(), 1, "and lifts with it");

    let mut term = Terminal::new(10, 1, QueueShell::default(), 0).expect("valid size");
    term.feed(b"1\r\n2");
    assert_eq!(term.grid().scrollback_len(), 0);
}

/// What `clear` writes for the terminal this emulator advertises leaves
/// nothing it erased to scroll back to, and with `-x` keeps the history, the
/// erased screen with it.
#[test]
fn what_clear_writes_leaves_nothing_to_scroll_back_to() {
    let caps = tairix_termcap::from_term(crate::TERM).capabilities();
    let clear = tairix_clear::clear_bytes(&caps, false).expect("this terminal can clear");
    let mut term = terminal(10, 3);
    term.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nsecret");
    assert_eq!(term.grid().scrollback_len(), 2);
    term.feed(&clear);
    assert_eq!(
        term.grid().scrollback_len(),
        0,
        "the erased screen cannot be scrolled back to"
    );
    assert_eq!(row_text(term.grid(), 0), "");
    assert_eq!((term.grid().cursor_col(), term.grid().cursor_row()), (0, 0));

    let kept = tairix_clear::clear_bytes(&caps, true).expect("this terminal can clear");
    let mut term = terminal(10, 3);
    term.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nsecret");
    term.feed(&kept);
    assert_eq!(term.grid().scrollback_len(), 5, "-x keeps it all");
}

/// The trace: the rows `ED 2` gave the scrollback and a prompt written at the
/// top. The erased screen counts in use to its full height, so a resize wider,
/// narrower or shorter keeps it where it is rather than bringing them back.
#[test]
fn a_resize_after_an_erase_brings_no_erased_row_back() {
    for (cols, rows) in [(11, 3), (9, 3), (10, 2)] {
        let mut grid = render_bytes(10, 3, b"1\r\n2\r\n3\r\n4\x1b[H\x1b[2J$ ");
        assert!(grid.resize(cols, rows));
        assert_eq!(row_text(&grid, 0), "$", "at {cols}x{rows}");
        assert_eq!(
            (grid.cursor_col(), grid.cursor_row()),
            (2, 0),
            "at {cols}x{rows}"
        );
        assert_eq!(grid.scrollback_len(), 4, "at {cols}x{rows}");
    }
}

/// The trace: a screen left blank by scrolling, the cursor homed. Its one row
/// in use ends the screen whichever way it is resized, the lines above it
/// coming back to fill it.
#[test]
fn a_blank_screen_ends_at_its_cursor_when_the_width_changes() {
    let mut bytes = b"x\r\n".to_vec();
    bytes.extend([b' '; 40]);
    bytes.extend_from_slice(b"\r\n\r\n\r\n\x1b[H");
    let mut grid = render_bytes(10, 3, &bytes);
    assert!(grid.resize(40, 3));
    assert_eq!(row_text(&grid, 0), "x");
    assert_eq!((grid.cursor_col(), grid.cursor_row()), (0, 2));
    assert_eq!(grid.scrollback_len(), 0);
}

/// A full-screen program on the alternate screen, which keeps no scrollback,
/// is sent the scrolling chords; the view takes them again once it is gone.
#[test]
fn a_full_screen_program_is_sent_the_scrolling_chords() {
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let named = Key::Named;
    let mut term = terminal(10, 4);
    term.feed(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\x1b[?1049h");
    for key in [
        NamedKey::PageUp,
        NamedKey::PageDown,
        NamedKey::Home,
        NamedKey::End,
    ] {
        assert_eq!(term.view_key(named(key), shift), None, "{key:?}");
    }
    term.feed(b"\x1b[?1049l");
    assert_eq!(term.view_key(named(NamedKey::PageUp), shift), Some(true));
}

/// A drag hands the screen many sizes before the window is painted: the paint
/// gives it the last once, laying the scrollback out again only for a new
/// width, and a size the grid refuses is not given, so the shell is never told
/// it.
#[test]
fn a_burst_of_resizes_reflows_once_and_only_for_a_new_width() {
    let mut term = terminal(20, 4);
    term.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix");
    for (cols, rows) in [(18, 4), (12, 9), (30, 2), (25, 6)] {
        term.resize(cols, rows);
    }
    assert_eq!(term.settle(), Reshaped::Took { cols: 25, rows: 6 });
    assert_eq!(term.grid().relays(), 1, "one burst, one reflow");
    for rows in [3, 8, 5] {
        term.resize(25, rows);
    }
    assert_eq!(term.settle(), Reshaped::Took { cols: 25, rows: 5 });
    assert_eq!(
        term.grid().relays(),
        1,
        "a burst that keeps the width lays nothing out"
    );
    term.resize(40, 5);
    term.resize(25, 5);
    assert_eq!(
        term.settle(),
        Reshaped::Unchanged,
        "a burst back to the size in force"
    );
    term.resize(MAX_DIMENSION + 1, 5);
    assert_eq!(term.settle(), Reshaped::Refused);
    assert_eq!((term.grid().cols(), term.grid().rows()), (25, 5));
    assert_eq!(term.grid().relays(), 1);
}
