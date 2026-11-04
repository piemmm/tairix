//! Unit tests for how the screen's rows end and what the scrollback holds.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use core::fmt::Write as _;
use tairix_mix::SplitMix64;

use tairix_reclaim::PressureBand;
use tairix_vt::{EraseMode, CONTINUATION};

use super::{charge, content, Grid, RowEnd, MAX_DIMENSION};
use crate::parser::Parser;
use crate::tests::ROOM;

/// Pieces of what a careless or hostile program writes: text, wide glyphs and
/// line breaks among the sequences that move the cursor, erase, scroll, set
/// the scroll region, and swap screens.
const PIECES: [&str; 24] = [
    "a",
    "bc",
    " ",
    "世",
    "\r\n",
    "\n",
    "\r",
    "\x08",
    "\x1b[H",
    "\x1b[3;7H",
    "\x1b[5C",
    "\x1b[2A",
    "\x1b[K",
    "\x1b[1K",
    "\x1b[J",
    "\x1b[1J",
    "\x1b[2J",
    "\x1b[3J",
    "\x1b[2S",
    "\x1b[T",
    "\x1b[2;3r",
    "\x1b[r",
    "\x1b[?1049h",
    "\x1b[?1049l",
];

/// A `cols`×`rows` grid with `bytes` applied.
fn fed(cols: u16, rows: u16, bytes: &[u8]) -> Grid {
    let mut grid = Grid::new(cols, rows, ROOM).expect("valid size");
    Parser::new().feed(&mut grid, bytes);
    grid
}

/// Every line the scrollback holds: its text and whether it continues.
fn held(grid: &Grid) -> Vec<(String, bool)> {
    (0..grid.scrollback.len())
        .map(|index| {
            let line = grid.scrollback.lines[index];
            let text = (0..line.len)
                .map(|col| grid.scrollback.cell(index, col).ch)
                .collect();
            (text, line.wrapped)
        })
        .collect()
}

/// A held line's text and whether it continues.
fn kept(text: &str, wrapped: bool) -> (String, bool) {
    (String::from(text), wrapped)
}

/// The text of screen row `row`, without its trailing blanks.
fn row_text(grid: &Grid, row: u16) -> String {
    let text: String = (0..grid.cols)
        .filter_map(|col| grid.cell(col, row))
        .map(|cell| cell.ch)
        .collect();
    String::from(text.trim_end())
}

/// End the line being rejoined, without its continuation cells or its
/// trailing spaces.
fn close_line(open: &mut String, lines: &mut Vec<String>) {
    lines.push(
        open.trim_end()
            .chars()
            .filter(|&ch| ch != CONTINUATION)
            .collect(),
    );
    open.clear();
}

/// The text of every line the grid holds, scrollback first and up to the
/// cursor's, each rejoined from the rows it wraps across.
fn logical_lines(grid: &Grid) -> Vec<String> {
    let mut lines = Vec::new();
    let mut open = String::new();
    for index in 0..grid.scrollback.len() {
        let line = grid.scrollback.lines[index];
        open.extend((0..line.len).map(|col| grid.scrollback.cell(index, col).ch));
        if !line.wrapped {
            close_line(&mut open, &mut lines);
        }
    }
    for row in 0..=grid.cursor_row {
        let cells: Vec<_> = (0..grid.cols)
            .filter_map(|col| grid.cell(col, row))
            .collect();
        let end = grid.ends[usize::from(row)];
        open.extend(content(&cells, end, 0).iter().map(|cell| cell.ch));
        if end == RowEnd::Hard || row == grid.cursor_row {
            close_line(&mut open, &mut lines);
        }
    }
    lines
}

/// What a shell might write, drawn from `seed`: letters, spaces, wide glyphs
/// and line breaks.
fn shell_text(seed: u64, len: usize) -> String {
    let mut draws = SplitMix64::new(seed);
    let mut text = String::new();
    for _ in 0..len {
        match draws.next_u64() % 12 {
            0 => text.push_str("\r\n"),
            1 => text.push('世'),
            2 | 3 => text.push(' '),
            letter => text.push(char::from(b'a' + u8::try_from(letter).unwrap_or(0))),
        }
    }
    text
}

/// What a careless or hostile program might write, drawn from `seed`.
fn program_bytes(seed: u64, pieces: usize) -> Vec<u8> {
    let mut draws = SplitMix64::new(seed);
    let mut bytes = Vec::new();
    for _ in 0..pieces {
        let pick = usize::try_from(draws.next_u64()).unwrap_or(0) % PIECES.len();
        bytes.extend_from_slice(PIECES[pick].as_bytes());
    }
    bytes
}

/// Assert everything the grid holds is in step: the cursor on the grid, the
/// screen and its row ends the grid's size, the view within reach, and the
/// scrollback within its bound, its lines laid end to end over its cells, each
/// charged what its cells are and each one that continues filling its row.
fn assert_in_step(grid: &Grid, seed: u64) {
    let (cols, rows) = (grid.cols, grid.rows);
    assert!(
        grid.cursor_col < cols && grid.cursor_row < rows,
        "seed {seed}"
    );
    assert_eq!(
        grid.cells.len(),
        usize::from(cols) * usize::from(rows),
        "seed {seed}"
    );
    assert_eq!(grid.ends.len(), usize::from(rows), "seed {seed}");
    let scrollback = &grid.scrollback;
    assert!(scrollback.view <= grid.scrollback_len(), "seed {seed}");
    assert!(!scrollback.exceeds(scrollback.bound()), "seed {seed}");
    assert_eq!(
        scrollback.ended,
        scrollback.lines.iter().filter(|line| !line.wrapped).count(),
        "seed {seed}"
    );
    assert!(scrollback.cells.len() <= scrollback.charged, "seed {seed}");
    assert!(scrollback.lines.len() <= scrollback.charged, "seed {seed}");
    let mut at = scrollback.base;
    let mut charged = 0;
    for line in &scrollback.lines {
        assert_eq!(line.start, at, "seed {seed}");
        assert!(!line.wrapped || line.len + 1 >= cols, "seed {seed}");
        let cells: Vec<_> = scrollback.cells_of(*line).collect();
        assert_eq!(usize::from(line.charge), charge(&cells), "seed {seed}");
        charged += usize::from(line.charge);
        at = at.wrapping_add(usize::from(line.len));
    }
    assert_eq!(charged, scrollback.charged, "seed {seed}");
    assert_eq!(
        at.wrapping_sub(scrollback.base),
        scrollback.cells.len(),
        "seed {seed}"
    );
    for row in 0..rows {
        for col in 0..cols {
            assert!(grid.view_cell(col, row).is_some(), "seed {seed}");
        }
    }
}

#[test]
fn the_auto_wrap_marks_the_row_it_leaves() {
    assert_eq!(
        fed(4, 3, b"abcde").ends,
        [RowEnd::Wrapped, RowEnd::Hard, RowEnd::Hard]
    );
    // A wide glyph wrapping whole leaves its padding behind.
    assert_eq!(
        fed(3, 2, "ab世".as_bytes()).ends,
        [RowEnd::Padded, RowEnd::Hard]
    );
    // On the last row below the scroll region a line feed moves nothing, so
    // the glyph after a full row overwrites that row rather than continuing it.
    let grid = fed(4, 3, b"\x1b[1;2r\x1b[3;1Habcde");
    assert_eq!(grid.ends[2], RowEnd::Hard);
    assert_eq!(grid.cell(0, 2).map(|cell| cell.ch), Some('e'));
}

#[test]
fn a_rows_end_travels_with_it() {
    let grid = fed(4, 2, b"abcdefghi");
    // `abcd` left the screen still continuing, and `efgh` moved up with its
    // wrap.
    assert_eq!(held(&grid), [kept("abcd", true)]);
    assert_eq!(grid.ends, [RowEnd::Wrapped, RowEnd::Hard]);
    // A scroll down carries `abcd` and the `efgh` it continues into together,
    // and `efgh` no longer continues into the `i` the scroll pushed off.
    let mut grid = fed(4, 3, b"abcdefghi");
    grid.scroll_down(1);
    assert_eq!(grid.ends, [RowEnd::Hard, RowEnd::Wrapped, RowEnd::Hard]);
}

#[test]
fn an_erase_through_a_rows_end_ends_its_line_there() {
    let mut grid = fed(4, 3, b"abcdefghi");
    grid.move_to(1, 0);
    grid.erase_in_line(EraseMode::ToStart);
    assert_eq!(
        grid.ends[0],
        RowEnd::Wrapped,
        "short of the end, the line goes on"
    );
    grid.move_to(3, 0);
    grid.erase_in_line(EraseMode::ToStart);
    assert_eq!(grid.ends[0], RowEnd::Hard);
    grid.move_to(2, 1);
    grid.erase_in_line(EraseMode::ToEnd);
    assert_eq!(grid.ends[1], RowEnd::Hard);

    let mut grid = fed(4, 3, b"abcdefghi");
    grid.move_to(1, 1);
    grid.erase_in_display(EraseMode::ToStart);
    assert_eq!(grid.ends[..2], [RowEnd::Hard, RowEnd::Wrapped]);
    grid.erase_in_display(EraseMode::ToEnd);
    assert_eq!(grid.ends, [RowEnd::Hard; 3]);

    let mut grid = fed(4, 3, b"abcdefghi");
    grid.clear();
    assert_eq!(grid.ends, [RowEnd::Hard; 3]);
}

#[test]
fn only_a_region_at_the_screens_top_gives_its_lines_up() {
    assert!(held(&fed(4, 3, b"\x1b[2;3r\x1b[3;1Ha\r\nb\r\nc")).is_empty());
    assert_eq!(
        held(&fed(4, 3, b"\x1b[1;2r\x1b[2;1Ha\r\nb\r\nc")),
        [kept("", false), kept("a", false)]
    );
    // An explicit scroll gives its lines up too.
    assert_eq!(
        held(&fed(4, 3, b"a\r\nb\x1b[2S")),
        [kept("a", false), kept("b", false)]
    );
}

#[test]
fn a_held_line_keeps_exactly_what_belongs_to_it() {
    let erased = |cols, rows, bytes: &[u8]| {
        let mut grid = fed(cols, rows, bytes);
        grid.erase_in_display(EraseMode::All);
        held(&grid)
    };
    // A line that ends on its row loses its trailing blanks; one that wraps
    // keeps them, since they are part of it.
    assert_eq!(
        erased(6, 3, b"ab  \r\ncdef  gh"),
        [kept("ab", false), kept("cdef  ", true), kept("gh", false)]
    );
    // The blank a wide glyph left is not part of its line.
    assert_eq!(
        erased(6, 2, "abcde世".as_bytes()),
        [kept("abcde", true), kept("世\u{0}", false)]
    );
    // A blank that shows a colour is kept; one that shows nothing is not.
    assert_eq!(
        erased(6, 2, b"\x1b[44mab  \x1b[0m\r\n\x1b[31mcd  "),
        [kept("ab  ", false), kept("cd", false)]
    );
}

/// One endless line is held to a widest screen's worth of cells for each line
/// of the limit, its oldest rows giving way, and what is allocated is held to
/// that too; a limit of none keeps nothing.
#[test]
fn an_endless_line_is_held_to_a_widest_screens_cells_a_line() {
    let mut grid = Grid::new(100, 1, 2).expect("valid size");
    Parser::new().feed(&mut grid, &[b'x'; 3000]);
    let bound = 2 * usize::from(MAX_DIMENSION);
    let scrollback = &grid.scrollback;
    assert!(scrollback.charged <= bound);
    assert!(
        scrollback.charged > bound - 100,
        "the bound, not the line count, held it"
    );
    assert!(
        scrollback.cells.capacity() <= bound,
        "what is allocated is bounded too"
    );
    assert!(scrollback.lines.capacity() <= bound);

    let mut grid = Grid::new(100, 1, 0).expect("valid size");
    Parser::new().feed(&mut grid, b"one\r\ntwo\r\nthree");
    assert_eq!(grid.scrollback.len(), 0);
}

#[test]
fn a_reflow_keeps_every_line_whatever_the_sizes() {
    for seed in 0..64 {
        let mut grid = fed(12, 5, shell_text(seed, 200).as_bytes());
        let before = logical_lines(&grid);
        let mut draws = SplitMix64::new(!seed);
        for _ in 0..6 {
            let cols = 1 + u16::try_from(draws.next_u64() % 20).unwrap_or(0);
            let rows = 1 + u16::try_from(draws.next_u64() % 8).unwrap_or(0);
            let _ = grid.resize(cols, rows);
            assert_eq!(logical_lines(&grid), before, "seed {seed} at {cols}x{rows}");
        }
    }
}

#[test]
fn every_operation_keeps_the_grid_and_its_scrollback_in_step() {
    for seed in 0..48 {
        let mut draws = SplitMix64::new(seed);
        let mut grid = Grid::new(9, 4, 8).expect("valid size");
        let mut parser = Parser::new();
        for _ in 0..40 {
            parser.feed(&mut grid, &program_bytes(draws.next_u64(), 12));
            let choice = draws.next_u64();
            match choice % 4 {
                0 => {
                    let cols = 1 + u16::try_from(choice / 4 % 14).unwrap_or(0);
                    let rows = 1 + u16::try_from(choice / 64 % 7).unwrap_or(0);
                    let _ = grid.resize(cols, rows);
                }
                1 => {
                    grid.scroll_view(i32::try_from(choice / 4 % 41).unwrap_or(0) - 20);
                }
                2 => {
                    let band = usize::try_from(choice / 4 % 5).unwrap_or(0);
                    grid.shed_for(PressureBand::ALL[band]);
                }
                _ => {}
            }
            assert_in_step(&grid, seed);
        }
    }
}

/// The trace: a line that scrolled off still continuing, and the screen it
/// continued onto erased from its home and written afresh. What continued
/// into the erased row ends where it is, so a resize glues nothing onto it.
#[test]
fn erasing_a_rows_start_ends_the_line_that_continued_into_it() {
    let mut grid = fed(4, 2, b"abcdefghij\x1b[H\x1b[JXY");
    assert_eq!(held(&grid), [kept("abcd", false)]);
    assert!(grid.resize(8, 1));
    assert_eq!(row_text(&grid, 0), "XY");
    assert_eq!((grid.cursor_col, grid.cursor_row), (2, 0));
    assert_eq!(held(&grid), [kept("abcd", false)]);

    // An erase of the screen down to that row, of the row's start by line,
    // and a scroll that frees it all end the line before it too.
    for taken in ["\x1b[1J", "\x1b[K", "\x1b[1K", "\x1b[2K", "\x1b[T"] {
        let grid = fed(4, 2, format!("abcdefghij\x1b[H{taken}").as_bytes());
        assert_eq!(held(&grid), [kept("abcd", false)], "{taken:?}");
    }
    let grid = fed(4, 3, b"abcdefgh\x1b[2;1H\x1b[K");
    assert_eq!(grid.ends[0], RowEnd::Hard, "and the row above, on screen");
}

/// Narrowing to one column lays every line out a cell a row, which the limit
/// counts as the one line it was, so widening again finds every one.
#[test]
fn narrowing_to_one_column_and_back_costs_no_history() {
    let mut grid = Grid::new(20, 2, 40).expect("valid size");
    let text = (0..30).fold(String::new(), |mut text, n| {
        let _ = write!(text, "line {n:02} 世\r\n");
        text
    });
    Parser::new().feed(&mut grid, text.as_bytes());
    let before = logical_lines(&grid);
    assert_eq!(before.len(), 31);
    assert!(grid.resize(1, 2));
    assert!(grid.scrollback.len() > 29 * 9, "a cell a row");
    assert!(grid.resize(20, 2));
    assert_eq!(logical_lines(&grid), before);
}

/// A reflow trims as it lays out, so neither ring grows past the bound
/// however narrow the screen goes or however much of it the scrollback takes.
#[test]
fn a_reflow_stays_within_the_bound_as_it_goes() {
    let mut grid = Grid::new(10, 6, 12).expect("valid size");
    Parser::new().feed(&mut grid, shell_text(3, 600).as_bytes());
    let bound = grid.scrollback.bound();
    for (cols, rows) in [(1, 6), (7, 2), (10, 1), (1, 1), (3, 9), (10, 6)] {
        assert!(grid.resize(cols, rows), "{cols}x{rows}");
        let scrollback = &grid.scrollback;
        assert!(scrollback.held_lines() <= bound.lines, "{cols}x{rows}");
        assert!(scrollback.cells.capacity() <= bound.cells, "{cols}x{rows}");
        assert!(scrollback.lines.capacity() <= bound.cells, "{cols}x{rows}");
        assert_in_step(&grid, 0);
    }
}

/// A resize that keeps the width lays no held line out again: the screen alone
/// moves rows to or from the scrollback.
#[test]
fn a_height_only_resize_leaves_the_held_lines_where_they_are() {
    let mut grid = fed(10, 4, shell_text(9, 300).as_bytes());
    let base = grid.scrollback.base;
    assert!(grid.scrollback.len() > 4);
    assert!(grid.resize(10, 7));
    assert!(grid.resize(10, 2));
    assert_eq!(grid.scrollback.relays, 0);
    assert_eq!(grid.scrollback.base, base, "none was laid out again");
    assert!(grid.resize(9, 2));
    assert_eq!(grid.scrollback.relays, 1);
}
