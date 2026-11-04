//! The character-cell screen the terminal draws to, and the scrollback above
//! it.
//!
//! A [`Grid`] is a rectangle of [`Cell`]s plus a cursor and the current
//! rendition pen. It exposes the cursor-relative operations a terminal needs —
//! writing a glyph, the C0 control moves (backspace, tab, line feed, carriage
//! return), absolute and relative cursor positioning, the erase operations, the
//! scroll region and explicit scrolling, the alternate screen, cursor
//! visibility, and the saved-cursor state — and nothing about byte parsing:
//! turning a byte stream into these calls is the
//! [`Parser`](crate::parser::Parser)'s job, so the two concerns stay separate.
//!
//! Lines that leave the top of the main screen are kept in a scrollback a view
//! can be scrolled back over, held to a share of the machine's memory and to
//! the pressure on it, and each row records whether its line continues on the
//! next, so a resize reflows the main screen and its scrollback rather than
//! cutting lines at the new edge.
//!
//! The [`Cell`] and its [`Attributes`] are [`lib/vt`](tairix_vt)'s shared
//! representation, not a second copy: the emulator is a *consumer* of the one
//! ANSI/VT/xterm vocabulary, so a cell here is exactly the
//! cell a curses renderer emits.
//!
//! Every operation is total and saturating: an out-of-range coordinate clamps
//! into the grid and a full region scrolls rather than growing, so a hostile
//! or buggy byte stream can never index out of bounds or panic.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_reclaim::{shrink_target, CacheBudget, PressureBand, ReclaimClass};
use tairix_vt::{char_width, Attributes, Cell, Color, EraseMode, CONTINUATION};

/// The largest grid dimension, in cells, the terminal will allocate.
///
/// A fixed ceiling keeps `cols * rows` bounded so a caller cannot ask the
/// terminal to allocate an unreasonable buffer; a larger request fails closed
/// in [`Grid::new`].
pub const MAX_DIMENSION: u16 = 1024;

/// The tab stop interval, in cells.
const TAB_WIDTH: u16 = 8;

/// The reclaim class the scrollback gives way under: the one given up last,
/// though a working-set floor over the whole limit keeps it all short of
/// severe pressure whatever the class.
const SCROLLBACK_CLASS: ReclaimClass = ReclaimClass::ReliabilityAssist;

/// The most memory one line of the limit can take: the widest screen's row of
/// cells and the record of it.
fn line_bytes() -> usize {
    usize::from(MAX_DIMENSION) * core::mem::size_of::<Cell>() + core::mem::size_of::<Line>()
}

/// What a limit of `lines` costs at most, for the reclaim model to read
/// memory pressure against: all of it working set, since a line given up
/// cannot be had back.
fn limit_budget(lines: usize) -> CacheBudget {
    let bytes = lines.saturating_mul(line_bytes());
    CacheBudget::from_ceiling(bytes).with_working_set_floor(bytes)
}

/// How a row ends: what a resize rejoins the screen's lines by.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum RowEnd {
    /// The line ends on this row.
    Hard,
    /// The line continues on the next row.
    Wrapped,
    /// The line continues on the next row, and the last cell is the blank a
    /// wide glyph left when it wrapped whole.
    Padded,
}

/// A snapshot of the screen state saved when entering the alternate screen and
/// restored when leaving it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Screen {
    cursor_col: u16,
    cursor_row: u16,
    pen: Attributes,
    scroll_top: u16,
    scroll_bottom: u16,
    cells: Vec<Cell>,
    ends: Vec<RowEnd>,
    in_use: u16,
}

impl Screen {
    /// Take `laid` as this screen, its scroll region the whole of it.
    fn adopt(&mut self, laid: Laid) {
        self.cursor_col = laid.cursor.col;
        self.cursor_row = laid.cursor.row;
        self.scroll_top = 0;
        self.scroll_bottom = laid.rows.saturating_sub(1);
        self.cells = laid.cells;
        self.ends = laid.ends;
        self.in_use = laid.in_use;
    }
}

/// A saved cursor position and pen (`ESC 7` / `ESC 8`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct SavedCursor {
    col: u16,
    row: u16,
    pen: Attributes,
}

/// A character-cell screen with a cursor, a rendition pen, and the
/// scrollback above it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Grid {
    cols: u16,
    rows: u16,
    cursor_col: u16,
    cursor_row: u16,
    /// Whether the last glyph filled the row and the wrap is owed.
    ///
    /// The cursor still rests on the last column; the wrap is paid by the
    /// next glyph and cancelled by anything that moves or erases first. The
    /// column itself therefore stays inside the grid, so every reader — the
    /// erase operations, the renderer's cursor block — addresses a real cell.
    pending_wrap: bool,
    cells: Vec<Cell>,
    /// How each row of `cells` ends.
    ends: Vec<RowEnd>,
    /// The rows from the top a resize counts in use however blank: all of
    /// them once the whole display has been erased, so an erased screen keeps
    /// its place.
    in_use: u16,
    /// The rendition new glyphs are written with (the folded SGR state).
    pen: Attributes,
    /// Whether the cursor is shown (`CSI ? 25 h` / `l`).
    cursor_visible: bool,
    /// The 0-based top row of the scroll region (inclusive).
    scroll_top: u16,
    /// The 0-based bottom row of the scroll region (inclusive).
    scroll_bottom: u16,
    /// The saved cursor for `ESC 7` / `ESC 8`, if any.
    saved_cursor: Option<SavedCursor>,
    /// The main screen, saved while the alternate screen is active.
    alternate: Option<Screen>,
    /// The lines the main screen has given up, and the view back over them.
    scrollback: Scrollback,
    /// The window title most recently set by an OSC title sequence.
    title: String,
}

impl Grid {
    /// Create a `cols`×`rows` grid of blank cells with the cursor at the home
    /// position `(0, 0)`, the plain rendition pen, the cursor shown, the
    /// scroll region covering the whole screen, and a scrollback keeping the
    /// last `scrollback` lines with no memory pressure on it.
    ///
    /// Returns `None` for a zero dimension, a dimension above
    /// [`MAX_DIMENSION`], or a screen there is no memory for, so an unusable
    /// screen size fails closed rather than allocating something degenerate.
    #[must_use]
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Option<Self> {
        if !usable(cols, rows) {
            return None;
        }
        Some(Self {
            cols,
            rows,
            cursor_col: 0,
            cursor_row: 0,
            pending_wrap: false,
            cells: blank_cells(cols, rows)?,
            ends: vec![RowEnd::Hard; usize::from(rows)],
            in_use: 0,
            pen: Attributes::PLAIN,
            cursor_visible: true,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            saved_cursor: None,
            alternate: None,
            scrollback: Scrollback::new(scrollback),
            title: String::new(),
        })
    }

    /// Reshape the screen to `cols`×`rows` (a window resize).
    ///
    /// The main screen is reflowed with its scrollback: its lines are rejoined
    /// and split again at the new width, a wide glyph wrapping whole, and the
    /// cursor keeps its place in its line, an owed wrap included. The screen
    /// then ends at the last row in use — the cursor's, the last written, or
    /// the last of an erased screen, which counts whole — when there is
    /// enough to fill it, rows coming back from the scrollback as it grows
    /// and going to it as it shrinks, the oldest held lines giving way to the
    /// bound; otherwise its content stays at the top. The cursor always stays
    /// on screen. Only a new width lays the scrollback out again. An active
    /// alternate screen keeps its top-left overlap over the reflowed main
    /// screen. The saved cursor is clamped, the scroll region reset to the
    /// whole screen as a real tty does on `TIOCSWINSZ`, and the view returned
    /// to the live screen.
    ///
    /// Returns `false`, changing nothing, for a zero/oversized dimension, a
    /// no-op resize, or a size there is no memory for (fail closed): all the
    /// layout needs is had before anything moves.
    #[must_use]
    pub fn resize(&mut self, cols: u16, rows: u16) -> bool {
        if !usable(cols, rows) || (cols, rows) == (self.cols, self.rows) {
            return false;
        }
        let Some(laid) = self.relaid(cols, rows) else {
            return false;
        };
        self.adopt(laid);
        true
    }

    /// The screen laid out at `cols`×`rows`: the main screen reflowed with the
    /// scrollback, or the alternate screen clipped over a reflowed main
    /// screen. `None`, changing nothing, when there is no memory for it.
    fn relaid(&mut self, cols: u16, rows: u16) -> Option<Laid> {
        let shown = Rows {
            cells: &self.cells,
            ends: &self.ends,
            cols: self.cols,
            in_use: self.in_use,
        };
        let cursor = Cursor {
            col: self.cursor_col,
            row: self.cursor_row,
            pending: self.pending_wrap,
        };
        let Some(main) = self.alternate.as_mut() else {
            return self.scrollback.reflow(shown, cursor, cols, rows);
        };
        let mut alt = Laid::blank(cols, rows)?;
        alt.clip(shown, cursor);
        let beneath = Rows {
            cells: &main.cells,
            ends: &main.ends,
            cols: self.cols,
            in_use: main.in_use,
        };
        let at = Cursor {
            col: main.cursor_col,
            row: main.cursor_row,
            pending: false,
        };
        let reflowed = self.scrollback.reflow(beneath, at, cols, rows)?;
        main.adopt(reflowed);
        Some(alt)
    }

    /// Take `laid` as the live screen, its scroll region the whole of it and
    /// the saved cursor clamped into it.
    fn adopt(&mut self, laid: Laid) {
        self.cols = laid.cols;
        self.rows = laid.rows;
        self.cursor_col = laid.cursor.col;
        self.cursor_row = laid.cursor.row;
        self.pending_wrap = laid.cursor.pending;
        self.cells = laid.cells;
        self.ends = laid.ends;
        self.in_use = laid.in_use;
        self.reset_scroll_region();
        if let Some(saved) = self.saved_cursor.as_mut() {
            saved.col = saved.col.min(self.cols.saturating_sub(1));
            saved.row = saved.row.min(self.rows.saturating_sub(1));
        }
    }

    /// The number of columns.
    #[must_use]
    pub const fn cols(&self) -> u16 {
        self.cols
    }

    /// The number of rows.
    #[must_use]
    pub const fn rows(&self) -> u16 {
        self.rows
    }

    /// The cursor column, `0..cols`.
    #[must_use]
    pub const fn cursor_col(&self) -> u16 {
        self.cursor_col
    }

    /// The cursor row, `0..rows`.
    #[must_use]
    pub const fn cursor_row(&self) -> u16 {
        self.cursor_row
    }

    /// Whether the cursor is currently shown.
    #[must_use]
    pub const fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// The rendition new glyphs are currently written with.
    #[must_use]
    pub const fn pen(&self) -> Attributes {
        self.pen
    }

    /// Whether the alternate screen buffer is currently active.
    #[must_use]
    pub const fn on_alternate_screen(&self) -> bool {
        self.alternate.is_some()
    }

    /// The window title most recently set by an OSC title sequence (empty if
    /// none has been set).
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The live screen's cell at `(col, row)`, or `None` if the coordinate is
    /// off the grid.
    #[must_use]
    pub fn cell(&self, col: u16, row: u16) -> Option<Cell> {
        if col >= self.cols || row >= self.rows {
            return None;
        }
        self.cells.get(self.index(col, row)).copied()
    }

    /// The cell the view shows at `(col, row)` — the live screen's, or the
    /// scrollback's where the view is scrolled back over it — or `None` if
    /// the coordinate is off the grid.
    #[must_use]
    pub fn view_cell(&self, col: u16, row: u16) -> Option<Cell> {
        if col >= self.cols || row >= self.rows {
            return None;
        }
        let back = self.scrollback.view;
        let row = usize::from(row);
        if row < back {
            let index = self.scrollback.len() + row - back;
            return Some(self.scrollback.cell(index, col));
        }
        let shown = u16::try_from(row - back).ok()?;
        self.cells.get(self.index(col, shown)).copied()
    }

    /// Where the view shows the cursor, or `None` while it is scrolled back
    /// past the cursor's row.
    #[must_use]
    pub fn view_cursor(&self) -> Option<(u16, u16)> {
        let row = usize::from(self.cursor_row).saturating_add(self.scrollback.view);
        let row = u16::try_from(row).ok().filter(|&row| row < self.rows)?;
        Some((self.cursor_col, row))
    }

    /// How many lines the view is scrolled back from the live screen.
    #[cfg(test)]
    #[must_use]
    pub const fn view_offset(&self) -> usize {
        self.scrollback.view
    }

    /// How many times a resize has laid the scrollback out at a new width.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn relays(&self) -> usize {
        self.scrollback.relays
    }

    /// How many lines of scrollback the view can reach: none while the
    /// alternate screen, which keeps none, is shown.
    #[must_use]
    pub fn scrollback_len(&self) -> usize {
        if self.alternate.is_some() {
            0
        } else {
            self.scrollback.len()
        }
    }

    /// Scroll the view `delta` lines back into the scrollback, or toward the
    /// live screen for a negative `delta`, clamped to what it holds; returns
    /// whether the view moved.
    pub fn scroll_view(&mut self, delta: i32) -> bool {
        let reach = self.scrollback_len();
        let was = self.scrollback.view;
        let step = usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX);
        self.scrollback.view = if delta < 0 {
            was.saturating_sub(step)
        } else {
            was.saturating_add(step).min(reach)
        };
        self.scrollback.view != was
    }

    /// Return the view to the live screen; returns whether it moved.
    pub fn view_to_bottom(&mut self) -> bool {
        core::mem::take(&mut self.scrollback.view) != 0
    }

    /// Forget the scrollback (`CSI 3 J`), leaving the screen as it is.
    pub fn clear_scrollback(&mut self) {
        self.scrollback.clear();
    }

    /// Hold the scrollback to what memory pressure at `band` leaves of its
    /// limit, from now until the band moves, the oldest lines going first and
    /// their memory given back; answers whether any went.
    pub fn shed_for(&mut self, band: PressureBand) -> bool {
        self.scrollback.shed_for(band)
    }

    /// Keep the last `lines` lines of scrollback from now on, the oldest past
    /// it going at once; answers whether any went.
    pub fn set_scrollback_limit(&mut self, lines: usize) -> bool {
        self.scrollback.set_limit(lines)
    }

    /// Replace the rendition pen, so subsequently written glyphs carry `attrs`.
    pub fn set_attributes(&mut self, attrs: Attributes) {
        self.pen = attrs;
    }

    /// Set whether the cursor is shown.
    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.cursor_visible = visible;
    }

    /// Set the window title from a parsed OSC title.
    pub fn set_title(&mut self, title: &str) {
        self.title.clear();
        self.title.push_str(title);
    }

    /// Write `ch` at the cursor with the current pen and advance one column
    /// (two for a double-width glyph).
    ///
    /// Filling the last column does not wrap: the cursor rests on that column
    /// and the wrap is *owed*, paid by the next glyph and cancelled by
    /// anything that moves or erases first — the rule every terminal follows
    /// and the shared [`conformance`](tairix_vt::conformance) script pins.
    /// Wrapping eagerly would line-feed, and at the bottom margin scroll the
    /// whole screen, the moment a program painted a full-width row.
    ///
    /// A double-width glyph (see [`char_width`]) occupies two cells: the lead
    /// cell and a [`CONTINUATION`] cell to its right — the same layout the
    /// `lib/curses` window writer and the framebuffer console produce, so a
    /// TUI's column arithmetic and this grid agree. When only one column
    /// remains the wide glyph wraps whole, blanking the leftover column.
    pub fn write_char(&mut self, ch: char) {
        let width = char_width(ch);
        if self.pending_wrap {
            self.wrap(RowEnd::Wrapped);
        }
        let pen = self.pen;
        if width == 2 && self.cursor_col.saturating_add(1) >= self.cols {
            self.clear_wide_at(self.cursor_col, self.cursor_row);
            let index = self.index(self.cursor_col, self.cursor_row);
            if let Some(cell) = self.cells.get_mut(index) {
                *cell = Cell::styled(' ', pen);
            }
            self.wrap(RowEnd::Padded);
        }
        self.clear_wide_at(self.cursor_col, self.cursor_row);
        if width == 2 {
            self.clear_wide_at(self.cursor_col.saturating_add(1), self.cursor_row);
        }
        let index = self.index(self.cursor_col, self.cursor_row);
        if let Some(cell) = self.cells.get_mut(index) {
            *cell = Cell::styled(ch, pen);
        }
        // On a degenerate one-column grid there is no second cell; writing it
        // anyway would alias the next row (`index` is row-major).
        if width == 2 && self.cursor_col.saturating_add(1) < self.cols {
            let index = self.index(self.cursor_col.saturating_add(1), self.cursor_row);
            if let Some(cell) = self.cells.get_mut(index) {
                *cell = Cell::styled(CONTINUATION, pen);
            }
        }
        let next = self.cursor_col.saturating_add(width);
        if next < self.cols {
            self.cursor_col = next;
        } else {
            self.cursor_col = self.cols.saturating_sub(1);
            self.pending_wrap = true;
        }
    }

    /// Continue the cursor's line on the next row, as the auto-wrap does,
    /// recording how the row it leaves ends.
    fn wrap(&mut self, end: RowEnd) {
        let row = self.cursor_row;
        // On the last row below the scroll region a line feed moves nothing,
        // so the next glyph overwrites this row rather than continuing it.
        if row == self.scroll_bottom || row.saturating_add(1) < self.rows {
            self.set_end(row, end);
        }
        self.carriage_return();
        self.line_feed();
    }

    /// Record how `row` ends.
    fn set_end(&mut self, row: u16, end: RowEnd) {
        if let Some(slot) = self.ends.get_mut(usize::from(row)) {
            *slot = end;
        }
    }

    /// End the line that continued into `row`, whose start an erase or a
    /// scroll has taken: the row above's, or at the main screen's top the
    /// scrollback's newest.
    fn cut_before(&mut self, row: u16) {
        match row.checked_sub(1) {
            Some(above) => self.set_end(above, RowEnd::Hard),
            None if self.alternate.is_none() => self.scrollback.end_newest(),
            None => {}
        }
    }

    /// Whether the cursor is on the last column, so an erase through it
    /// reaches the row's end.
    fn at_last_column(&self) -> bool {
        self.cursor_col.saturating_add(1) >= self.cols
    }

    /// Place the cursor at `(col, row)`, clamped into the grid.
    ///
    /// Every explicit cursor movement goes through here, so none of them can
    /// forget that moving cancels an owed wrap or leave the column outside
    /// the grid.
    fn place(&mut self, col: u16, row: u16) {
        self.cursor_col = col.min(self.cols.saturating_sub(1));
        self.cursor_row = row.min(self.rows.saturating_sub(1));
        self.pending_wrap = false;
    }

    /// Move the cursor down one row, scrolling the region up when it is already
    /// on the bottom margin.
    pub fn line_feed(&mut self) {
        self.pending_wrap = false;
        if self.cursor_row == self.scroll_bottom {
            self.scroll_region_up(1);
        } else if self.cursor_row.saturating_add(1) < self.rows {
            self.cursor_row += 1;
        }
    }

    /// Move the cursor to the start of the current row.
    pub fn carriage_return(&mut self) {
        self.place(0, self.cursor_row);
    }

    /// Move the cursor one column left, stopping at the left edge.
    ///
    /// With a wrap owed the cursor already rests on the last column, so the
    /// backspace only cancels the wrap: a rubout (backspace, space,
    /// backspace) then erases the glyph that filled the row rather than the
    /// one before it.
    pub fn backspace(&mut self) {
        if self.pending_wrap {
            self.pending_wrap = false;
            return;
        }
        self.place(self.cursor_col.saturating_sub(1), self.cursor_row);
    }

    /// Advance the cursor to the next tab stop, clamped to the last column.
    pub fn tab(&mut self) {
        let next = (self.cursor_col / TAB_WIDTH)
            .saturating_add(1)
            .saturating_mul(TAB_WIDTH);
        self.place(next, self.cursor_row);
    }

    /// Move the cursor to `(col, row)`, clamping each coordinate into the
    /// grid.
    pub fn move_to(&mut self, col: u16, row: u16) {
        self.place(col, row);
    }

    /// Move the cursor to 0-based `col` on the current row, clamped.
    pub fn move_to_column(&mut self, col: u16) {
        self.place(col, self.cursor_row);
    }

    /// Move the cursor up `n` rows, stopping at the top.
    pub fn move_up(&mut self, n: u16) {
        self.place(self.cursor_col, self.cursor_row.saturating_sub(n));
    }

    /// Move the cursor down `n` rows, stopping at the bottom.
    pub fn move_down(&mut self, n: u16) {
        self.place(self.cursor_col, self.cursor_row.saturating_add(n));
    }

    /// Move the cursor left `n` columns, stopping at the left edge.
    pub fn move_left(&mut self, n: u16) {
        self.place(self.cursor_col.saturating_sub(n), self.cursor_row);
    }

    /// Move the cursor right `n` columns, stopping at the right edge.
    pub fn move_right(&mut self, n: u16) {
        self.place(self.cursor_col.saturating_add(n), self.cursor_row);
    }

    /// Move the cursor `n` rows down to the start of that row (`CNL`).
    pub fn next_line(&mut self, n: u16) {
        self.place(0, self.cursor_row.saturating_add(n));
    }

    /// Move the cursor `n` rows up to the start of that row (`CPL`).
    pub fn prev_line(&mut self, n: u16) {
        self.place(0, self.cursor_row.saturating_sub(n));
    }

    /// Erase part of the current row relative to the cursor.
    ///
    /// Follows ANSI `EL`: [`EraseMode::ToEnd`] clears from the cursor to the
    /// end of the row, [`EraseMode::ToStart`] from the start of the row to the
    /// cursor (inclusive), and [`EraseMode::All`] the whole row. The cursor
    /// does not move. An erase through the row's end ends its line there, and
    /// one through its start ends the line that continued into it.
    pub fn erase_in_line(&mut self, mode: EraseMode) {
        self.pending_wrap = false;
        let row_start = self.index(0, self.cursor_row);
        let row_end = row_start + usize::from(self.cols);
        let cursor = self.index(self.cursor_col, self.cursor_row);
        match mode {
            EraseMode::ToEnd => self.blank(cursor, row_end),
            EraseMode::ToStart => self.blank(row_start, cursor + 1),
            EraseMode::All => self.blank(row_start, row_end),
        }
        if mode != EraseMode::ToStart || self.at_last_column() {
            self.set_end(self.cursor_row, RowEnd::Hard);
        }
        if mode != EraseMode::ToEnd || self.cursor_col == 0 {
            self.cut_before(self.cursor_row);
        }
    }

    /// Erase part of the whole screen relative to the cursor.
    ///
    /// Follows ANSI `ED`: [`EraseMode::ToEnd`] clears from the cursor to the
    /// end of the screen, [`EraseMode::ToStart`] from the top of the screen to
    /// the cursor (inclusive), and [`EraseMode::All`] the whole screen — on
    /// the main screen once its rows, down to the last one written, have gone
    /// to the scrollback, so what was on screen can still be scrolled back to,
    /// and with every row then counted in use, so a resize keeps the erased
    /// screen where it is. An erase through a row's start ends the line that
    /// continued into it. The cursor does not move.
    pub fn erase_in_display(&mut self, mode: EraseMode) {
        self.pending_wrap = false;
        let cursor = self.index(self.cursor_col, self.cursor_row);
        let len = self.cells.len();
        let row = usize::from(self.cursor_row);
        let ended = match mode {
            EraseMode::ToEnd => {
                self.blank(cursor, len);
                row..self.ends.len()
            }
            EraseMode::ToStart => {
                self.blank(0, cursor + 1);
                0..row + usize::from(self.at_last_column())
            }
            EraseMode::All => {
                self.save_screen();
                self.blank(0, len);
                self.in_use = self.rows;
                0..self.ends.len()
            }
        };
        if let Some(ends) = self.ends.get_mut(ended) {
            ends.fill(RowEnd::Hard);
        }
        match mode {
            EraseMode::ToEnd if self.cursor_col > 0 => {}
            EraseMode::ToEnd => self.cut_before(self.cursor_row),
            EraseMode::ToStart | EraseMode::All => self.cut_before(0),
        }
    }

    /// Set the scroll region to the 1-based rows `top..=bottom`, clamped into
    /// the grid; a degenerate or inverted request falls back to the whole
    /// screen (fail closed).
    ///
    /// `DECSTBM` homes the cursor to the top-left *of the region*, not of the
    /// screen, so a program that reserves a header above its scrolling body
    /// starts drawing inside the body.
    pub fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let last = self.rows.saturating_sub(1);
        let top = top.saturating_sub(1).min(last);
        let bottom = bottom.saturating_sub(1).min(last);
        if top < bottom {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
        } else {
            self.reset_scroll_region();
        }
        self.place(0, self.scroll_top);
    }

    /// Reset the scroll region to the whole screen.
    pub fn reset_scroll_region(&mut self) {
        self.scroll_top = 0;
        self.scroll_bottom = self.rows.saturating_sub(1);
    }

    /// Scroll the scroll region up `n` lines (`SU`): content moves toward the
    /// top and the freed bottom lines are blanked, the lines a region at the
    /// screen's top scrolls off going to the scrollback. The cursor does not
    /// move.
    pub fn scroll_up(&mut self, n: u16) {
        self.scroll_region_up(n);
    }

    /// Scroll the scroll region down `n` lines (`SD`): content moves toward the
    /// bottom and the freed top lines are blanked, ending the lines that
    /// continued into the region's top and out of its bottom. The cursor does
    /// not move.
    pub fn scroll_down(&mut self, n: u16) {
        if n == 0 {
            return;
        }
        self.shift_region(n, false);
        self.cut_before(self.scroll_top);
        self.set_end(self.scroll_bottom, RowEnd::Hard);
    }

    /// Switch to the alternate screen buffer, saving the main screen and
    /// returning the view to the live screen. A second request while already
    /// on the alternate screen is a no-op.
    pub fn enter_alt_screen(&mut self) {
        if self.alternate.is_some() {
            return;
        }
        self.alternate = Some(self.snapshot());
        self.scrollback.view = 0;
        self.clear();
    }

    /// Switch back to the main screen buffer, restoring its saved contents. A
    /// request while not on the alternate screen is a no-op.
    pub fn leave_alt_screen(&mut self) {
        if let Some(main) = self.alternate.take() {
            self.restore(main);
        }
    }

    /// Save the cursor position and pen (`ESC 7`).
    pub fn save_cursor(&mut self) {
        self.saved_cursor = Some(SavedCursor {
            col: self.cursor_col,
            row: self.cursor_row,
            pen: self.pen,
        });
    }

    /// Restore the saved cursor position and pen (`ESC 8`). With nothing saved,
    /// the cursor homes and the pen resets — the conventional fallback.
    pub fn restore_cursor(&mut self) {
        if let Some(saved) = self.saved_cursor {
            self.place(saved.col, saved.row);
            self.pen = saved.pen;
        } else {
            self.place(0, 0);
            self.pen = Attributes::PLAIN;
        }
    }

    /// Blank every cell and move the cursor home. The pen is left unchanged.
    pub fn clear(&mut self) {
        self.cells.fill(Cell::BLANK);
        self.ends.fill(RowEnd::Hard);
        self.place(0, 0);
    }

    /// The flat-buffer index of `(col, row)`. Callers guarantee the
    /// coordinate is in range; an out-of-range index simply addresses a cell
    /// that `cells.get` will reject.
    fn index(&self, col: u16, row: u16) -> usize {
        usize::from(row) * usize::from(self.cols) + usize::from(col)
    }

    /// Clear the complete wide glyph, if any, intersecting `(col, row)`.
    fn clear_wide_at(&mut self, col: u16, row: u16) {
        if col >= self.cols || row >= self.rows {
            return;
        }
        let index = self.index(col, row);
        let Some(cell) = self.cells.get(index).copied() else {
            return;
        };
        let blank = Cell::styled(' ', self.pen);
        if cell.ch == CONTINUATION && col > 0 {
            if let Some(span) = self.cells.get_mut(index - 1..=index) {
                span.fill(blank);
            }
        } else if char_width(cell.ch) == 2 {
            let end = (index + 2).min(self.cells.len());
            if let Some(span) = self.cells.get_mut(index..end) {
                span.fill(blank);
            }
        }
    }

    /// Blank the half-open cell range `start..end`, expanding its boundaries
    /// to clear any wide glyph it intersects.
    fn blank(&mut self, start: usize, end: usize) {
        let len = self.cells.len();
        let mut start = start.min(len);
        let mut end = end.min(len);
        if start < end
            && self
                .cells
                .get(start)
                .is_some_and(|cell| cell.ch == CONTINUATION)
        {
            start = start.saturating_sub(1);
        }
        if start < end
            && self
                .cells
                .get(end - 1)
                .is_some_and(|cell| char_width(cell.ch) == 2)
        {
            end = end.saturating_add(1).min(len);
        }
        let blank = Cell::styled(' ', self.pen);
        if let Some(slice) = self.cells.get_mut(start..end) {
            slice.fill(blank);
        }
    }

    /// Scroll the scroll region up `n` lines, blanking the freed bottom lines;
    /// a region at the screen's top hands the lines it scrolls off to the
    /// scrollback, and one below it ends the line that continued into it.
    fn scroll_region_up(&mut self, n: u16) {
        if self.scroll_top == 0 {
            for row in 0..n.min(self.region_rows()) {
                self.save_row(row);
            }
        } else if n > 0 {
            self.cut_before(self.scroll_top);
        }
        self.shift_region(n, true);
    }

    /// The scroll region's height in rows.
    fn region_rows(&self) -> u16 {
        self.scroll_bottom.saturating_sub(self.scroll_top) + 1
    }

    /// Move the scroll region's rows `n` rows toward its top (`up`) or its
    /// bottom, blanking the rows that frees.
    fn shift_region(&mut self, n: u16, up: bool) {
        let lines = usize::from(n.min(self.region_rows()));
        let stride = usize::from(self.cols);
        let top = usize::from(self.scroll_top);
        let bottom = usize::from(self.scroll_bottom) + 1;
        let blank = Cell::styled(' ', self.pen);
        if let Some(region) = self.cells.get_mut(top * stride..bottom * stride) {
            shift_rows(region, stride, lines, up, blank);
        }
        if let Some(region) = self.ends.get_mut(top..bottom) {
            shift_rows(region, 1, lines, up, RowEnd::Hard);
        }
    }

    /// Hand `row` to the scrollback as its newest line, on the main screen:
    /// the alternate screen keeps none.
    fn save_row(&mut self, row: u16) {
        if self.alternate.is_some() {
            return;
        }
        let start = self.index(0, row);
        let end = self
            .ends
            .get(usize::from(row))
            .copied()
            .unwrap_or(RowEnd::Hard);
        if let Some(cells) = self.cells.get(start..start + usize::from(self.cols)) {
            self.scrollback
                .push(content(cells, end, 0), end != RowEnd::Hard);
        }
    }

    /// Hand the main screen's rows, down to the last one written, to the
    /// scrollback.
    fn save_screen(&mut self) {
        if self.alternate.is_some() {
            return;
        }
        let written = self.shown().written();
        for row in (0..self.rows).take(written) {
            self.save_row(row);
        }
    }

    /// The live screen's rows.
    fn shown(&self) -> Rows<'_> {
        Rows {
            cells: &self.cells,
            ends: &self.ends,
            cols: self.cols,
            in_use: self.in_use,
        }
    }

    /// Capture the current screen state for the alternate-screen swap.
    fn snapshot(&self) -> Screen {
        Screen {
            cursor_col: self.cursor_col,
            cursor_row: self.cursor_row,
            pen: self.pen,
            scroll_top: self.scroll_top,
            scroll_bottom: self.scroll_bottom,
            cells: self.cells.clone(),
            ends: self.ends.clone(),
            in_use: self.in_use,
        }
    }

    /// Restore a previously captured screen state.
    fn restore(&mut self, screen: Screen) {
        self.pen = screen.pen;
        self.scroll_top = screen.scroll_top;
        self.scroll_bottom = screen.scroll_bottom;
        self.cells = screen.cells;
        self.ends = screen.ends;
        self.in_use = screen.in_use;
        self.place(screen.cursor_col, screen.cursor_row);
    }
}

/// Whether `cols`×`rows` is a screen the grid will allocate.
const fn usable(cols: u16, rows: u16) -> bool {
    cols != 0 && rows != 0 && cols <= MAX_DIMENSION && rows <= MAX_DIMENSION
}

/// A blank `cols`×`rows` screen, or `None` when there is no memory for one.
fn blank_cells(cols: u16, rows: u16) -> Option<Vec<Cell>> {
    let count = usize::from(cols) * usize::from(rows);
    let mut cells = Vec::new();
    cells.try_reserve_exact(count).ok()?;
    cells.resize(count, Cell::BLANK);
    Some(cells)
}

/// Whether `cell` paints nothing but the default background, so a line may
/// end before it and lose nothing.
fn is_blank(cell: Cell) -> bool {
    cell.ch == ' ' && cell.attrs.background == Color::Default && !cell.attrs.reverse
}

/// The cells of a row that belong to its line: all of a row the line
/// continues from, less the blank a wide glyph left, and of a row the line
/// ends on, those up to its last non-blank cell — or the first `at_least`,
/// where the cursor rests beyond them.
fn content(row: &[Cell], end: RowEnd, at_least: usize) -> &[Cell] {
    let len = match end {
        RowEnd::Wrapped => row.len(),
        RowEnd::Padded => row.len() - usize::from(row.last().is_some_and(|cell| cell.ch == ' ')),
        RowEnd::Hard => row
            .iter()
            .rposition(|&cell| !is_blank(cell))
            .map_or(0, |last| last + 1)
            .max(at_least),
    };
    row.get(..len).unwrap_or(row)
}

/// What a line of `cells` is charged against the scrollback's bound: a cell
/// for each column it covers — a wide glyph's two whether or not its
/// continuation is held — and one for a line of none, so laying the line out
/// at another width never charges it more.
fn charge(cells: &[Cell]) -> usize {
    let mut covered = 0;
    let mut after_lead = false;
    for cell in cells {
        let width = char_width(cell.ch);
        let paired = after_lead && cell.ch == CONTINUATION;
        if !paired {
            covered += usize::from(width);
        }
        after_lead = width == 2;
    }
    covered.max(1)
}

/// How a row of `len` cells ends in a `cols`-wide screen: hard where its line
/// ends, padded where it continues short of the edge.
fn row_end(wrapped: bool, len: usize, cols: u16) -> RowEnd {
    match (wrapped, len < usize::from(cols)) {
        (false, _) => RowEnd::Hard,
        (true, true) => RowEnd::Padded,
        (true, false) => RowEnd::Wrapped,
    }
}

/// Move `region`'s rows, `stride` items each, `lines` rows toward its start
/// (`up`) or its end, filling the rows that frees with `fill`.
fn shift_rows<T: Copy>(region: &mut [T], stride: usize, lines: usize, up: bool, fill: T) {
    let shift = lines.saturating_mul(stride).min(region.len());
    let kept = region.len() - shift;
    let freed = if up {
        region.copy_within(shift.., 0);
        kept..region.len()
    } else {
        region.copy_within(..kept, shift);
        0..shift
    };
    if let Some(freed) = region.get_mut(freed) {
        freed.fill(fill);
    }
}

/// Make room in `deque` for `more` items, growing it geometrically but never
/// past `bound`, so a bound on what is held bounds what is allocated; `false`,
/// growing nothing, when what it must hold passes the bound or memory cannot
/// be had.
fn reserve_within<T>(deque: &mut VecDeque<T>, more: usize, bound: usize) -> bool {
    let need = deque.len().saturating_add(more);
    if need <= deque.capacity() {
        return true;
    }
    if need > bound {
        return false;
    }
    let grown = deque.capacity().saturating_mul(2).max(need).min(bound);
    deque.try_reserve_exact(grown - deque.len()).is_ok()
}

/// Make room in `deque` for `total` items in all; `None` when memory cannot
/// be had.
fn reserve_exactly<T>(deque: &mut VecDeque<T>, total: usize) -> Option<()> {
    deque
        .try_reserve_exact(total.saturating_sub(deque.len()))
        .ok()
}

/// A screen's rows: `cols` cells each, how each ends, and how many from the
/// top are in use however blank.
#[derive(Copy, Clone)]
struct Rows<'a> {
    cells: &'a [Cell],
    ends: &'a [RowEnd],
    cols: u16,
    in_use: u16,
}

impl<'a> Rows<'a> {
    /// How many rows there are.
    fn count(self) -> usize {
        self.ends.len()
    }

    /// The cells of row `row`.
    fn row(self, row: usize) -> &'a [Cell] {
        let stride = usize::from(self.cols);
        self.cells
            .get(row * stride..(row + 1) * stride)
            .unwrap_or(&[])
    }

    /// How row `row` ends.
    fn end(self, row: usize) -> RowEnd {
        self.ends.get(row).copied().unwrap_or(RowEnd::Hard)
    }

    /// How many rows from the top reach the last that holds a cell that is not
    /// blank.
    fn written(self) -> usize {
        (0..self.count())
            .rev()
            .find(|&row| self.row(row).iter().any(|&cell| !is_blank(cell)))
            .map_or(0, |row| row + 1)
    }

    /// How many rows from the top a resize lays out: through the cursor's, the
    /// last written, and those in use however blank; the rest hold nothing.
    fn used(self, cursor: Cursor) -> usize {
        self.written()
            .max(usize::from(cursor.row) + 1)
            .max(usize::from(self.in_use))
            .min(self.count())
    }

    /// Row `row` of the first `used` as a resize lays it: the cells of its line,
    /// the cursor's kept out to where it rests; how it ends, the last ending its
    /// line since nothing follows it; and the cursor's index in those cells,
    /// when it is on it.
    fn laid_row(
        self,
        row: usize,
        used: usize,
        cursor: Cursor,
    ) -> (&'a [Cell], RowEnd, Option<usize>) {
        let end = if row + 1 < used {
            self.end(row)
        } else {
            RowEnd::Hard
        };
        let target = (row == usize::from(cursor.row))
            .then_some(usize::from(cursor.col) + usize::from(cursor.pending));
        (
            content(self.row(row), end, target.unwrap_or(0)),
            end,
            target,
        )
    }
}

/// A cursor position, and whether a wrap is owed there.
#[derive(Copy, Clone)]
struct Cursor {
    col: u16,
    row: u16,
    pending: bool,
}

/// A screen laid out at a new size: its rows, how each ends, how many are in
/// use, and its cursor.
struct Laid {
    cols: u16,
    rows: u16,
    cells: Vec<Cell>,
    ends: Vec<RowEnd>,
    in_use: u16,
    cursor: Cursor,
}

impl Laid {
    /// A blank `cols`×`rows` screen with the cursor home, or `None` when there
    /// is no memory for one.
    fn blank(cols: u16, rows: u16) -> Option<Self> {
        Some(Self {
            cols,
            rows,
            cells: blank_cells(cols, rows)?,
            ends: vec![RowEnd::Hard; usize::from(rows)],
            in_use: 0,
            cursor: Cursor {
                col: 0,
                row: 0,
                pending: false,
            },
        })
    }

    /// Copy in `old`'s top-left overlap with the cursor clamped into it: rows
    /// kept where they were rather than reflowed.
    fn clip(&mut self, old: Rows<'_>, cursor: Cursor) {
        let stride = usize::from(self.cols).max(1);
        for (row, into) in self.cells.chunks_mut(stride).enumerate() {
            for (slot, &cell) in into.iter_mut().zip(old.row(row)) {
                *slot = cell;
            }
        }
        self.cursor = Cursor {
            col: cursor.col.min(self.cols.saturating_sub(1)),
            row: cursor.row.min(self.rows.saturating_sub(1)),
            pending: false,
        };
    }

    /// Its rows as laid so far.
    fn shown(&self) -> Rows<'_> {
        Rows {
            cells: &self.cells,
            ends: &self.ends,
            cols: self.cols,
            in_use: self.in_use,
        }
    }

    /// Make row `slot` hold `cells`, blank past them, ending as `end`.
    fn set_row(&mut self, slot: usize, cells: impl IntoIterator<Item = Cell>, end: RowEnd) {
        let stride = usize::from(self.cols);
        if let Some(into) = self.cells.get_mut(slot * stride..(slot + 1) * stride) {
            into.fill(Cell::BLANK);
            for (cell, from) in into.iter_mut().zip(cells) {
                *cell = from;
            }
        }
        if let Some(slot_end) = self.ends.get_mut(slot) {
            *slot_end = end;
        }
    }
}

/// The screen a reflow lays its rows into: the newest of them, as many as it
/// has rows, the oldest handed to the scrollback as more arrive — never the
/// cursor's, which stays on screen, so the rest are dropped instead.
struct Fill<'a> {
    laid: &'a mut Laid,
    /// The slot of the oldest row held.
    head: usize,
    /// How many slots hold a row.
    held: usize,
    /// The slot of the row the cursor rests on, once laid.
    cursor: Option<usize>,
    /// Whether rows are being dropped, the cursor's being the oldest held.
    full: bool,
}

impl<'a> Fill<'a> {
    /// An empty screen over `laid`.
    fn new(laid: &'a mut Laid) -> Self {
        Self {
            laid,
            head: 0,
            held: 0,
            cursor: None,
            full: false,
        }
    }

    /// Lay `row` as the newest row, `holds_cursor` when the cursor rests on
    /// it, handing the oldest to `ring` when every slot holds one.
    fn push(&mut self, ring: &mut Scrollback, row: &[Cell], wrapped: bool, holds_cursor: bool) {
        let slots = usize::from(self.laid.rows);
        if self.full || slots == 0 {
            return;
        }
        if self.held == slots {
            if self.cursor == Some(self.head) {
                self.full = true;
                return;
            }
            let shown = self.laid.shown();
            let end = shown.end(self.head);
            ring.push(content(shown.row(self.head), end, 0), end != RowEnd::Hard);
            self.head = (self.head + 1) % slots;
            self.held -= 1;
        }
        let slot = (self.head + self.held) % slots;
        let end = row_end(wrapped, row.len(), self.laid.cols);
        self.laid.set_row(slot, row.iter().copied(), end);
        if holds_cursor {
            self.cursor = Some(slot);
        }
        self.held += 1;
    }
}

/// The lines the main screen has given up, oldest first, and the view back
/// over them.
///
/// Every row's cells share one ring and its extent another, so holding a row
/// allocates nothing once the rings have grown. The user's limit counts lines
/// of output — a line ends where the output ended it, however many rows it
/// wraps across — and the cells they cover, a widest screen's worth for each
/// line, bound what one endless line may take. Neither count moves with the
/// width the rows are laid at, so a resize never costs history the limit
/// keeps. Memory pressure past moderate takes lines off the limit.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Scrollback {
    /// The held rows' cells, oldest first.
    cells: VecDeque<Cell>,
    /// The held rows, oldest first.
    lines: VecDeque<Line>,
    /// Where `cells[0]` was pushed.
    base: usize,
    /// What the held rows are charged, in all ([`charge`]).
    charged: usize,
    /// How many held rows end their line.
    ended: usize,
    /// The lines the user keeps.
    limit: usize,
    /// The memory pressure the bound is read at.
    band: PressureBand,
    /// How many lines the view is scrolled back from the live screen.
    view: usize,
    /// How many times the held lines have been laid out at a new width, so a
    /// test can prove a burst of resizes costs one. Test-only: the product
    /// carries no counter.
    #[cfg(test)]
    relays: usize,
}

/// One held line.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Line {
    /// Where its cells were pushed, counted modulo `usize`: only its distance
    /// from `base`, always below the ring's length, is ever taken.
    start: usize,
    /// How many cells it holds.
    len: u16,
    /// What it is charged ([`charge`]).
    charge: u16,
    /// Whether it continues on the next line.
    wrapped: bool,
}

impl Line {
    /// How this line ends as a row of a `cols`-wide screen.
    fn end(self, cols: u16) -> RowEnd {
        row_end(self.wrapped, usize::from(self.len), cols)
    }
}

/// What the scrollback's rows may come to: lines of output, and the cells
/// they cover.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Bound {
    lines: usize,
    cells: usize,
}

impl Scrollback {
    /// An empty scrollback keeping the last `limit` lines, with no memory
    /// pressure on it.
    fn new(limit: usize) -> Self {
        Self {
            cells: VecDeque::new(),
            lines: VecDeque::new(),
            base: 0,
            charged: 0,
            ended: 0,
            limit,
            band: PressureBand::Normal,
            view: 0,
            #[cfg(test)]
            relays: 0,
        }
    }

    /// How many lines are held.
    fn len(&self) -> usize {
        self.lines.len()
    }

    /// The most lines, and cells, the rows may hold at the band in force.
    fn bound(&self) -> Bound {
        let lines =
            shrink_target(self.band, SCROLLBACK_CLASS, limit_budget(self.limit)) / line_bytes();
        Bound {
            lines,
            cells: lines.saturating_mul(usize::from(MAX_DIMENSION)),
        }
    }

    /// Whether the newest row's line goes on into the next row pushed.
    fn continues(&self) -> bool {
        self.lines.back().is_some_and(|line| line.wrapped)
    }

    /// How many lines the rows hold, an unfinished last one among them.
    fn held_lines(&self) -> usize {
        self.ended.saturating_add(usize::from(self.continues()))
    }

    /// Whether the rows hold more than `bound` allows.
    fn exceeds(&self, bound: Bound) -> bool {
        self.held_lines() > bound.lines || self.charged > bound.cells
    }

    /// Cell `col` of held line `index`, blank past the line's end.
    fn cell(&self, index: usize, col: u16) -> Cell {
        self.lines
            .get(index)
            .filter(|line| col < line.len)
            .and_then(|line| {
                let at = line.start.wrapping_sub(self.base);
                self.cells.get(at.saturating_add(usize::from(col)))
            })
            .copied()
            .unwrap_or(Cell::BLANK)
    }

    /// The cells held line `line` holds.
    fn cells_of(&self, line: Line) -> impl Iterator<Item = Cell> + '_ {
        let from = line.start.wrapping_sub(self.base).min(self.cells.len());
        let to = from
            .saturating_add(usize::from(line.len))
            .min(self.cells.len());
        self.cells.range(from..to).copied()
    }

    /// Hold `cells` as the newest line, the oldest giving way to the bound; a
    /// line the bound or memory cannot hold is dropped, and the line before it
    /// ends where it is. A view scrolled back stays on the lines it shows.
    fn push(&mut self, cells: &[Cell], wrapped: bool) {
        let cost = charge(cells);
        let bound = self.bound();
        if bound.lines == 0 || cost > bound.cells {
            self.end_newest();
            return;
        }
        while !self.lines.is_empty()
            && (self.charged.saturating_add(cost) > bound.cells
                || self.held_lines() + usize::from(!self.continues()) > bound.lines)
        {
            self.pop_oldest();
        }
        if self.append(cells, cost, wrapped).is_none() {
            self.end_newest();
        } else if self.view > 0 {
            self.view = self.view.saturating_add(1);
        }
    }

    /// Hold `cells`, charged `cost`, as the newest line in the room the bound
    /// leaves; `None`, holding nothing, when the room cannot be had.
    fn append(&mut self, cells: &[Cell], cost: usize, wrapped: bool) -> Option<()> {
        let len = u16::try_from(cells.len()).ok()?;
        let charge = u16::try_from(cost).ok()?;
        let bound = self.bound().cells;
        let room = reserve_within(&mut self.cells, cells.len(), bound)
            && reserve_within(&mut self.lines, 1, bound);
        if !room {
            return None;
        }
        let start = self.base.wrapping_add(self.cells.len());
        self.cells.extend(cells);
        self.lines.push_back(Line {
            start,
            len,
            charge,
            wrapped,
        });
        self.charged = self.charged.saturating_add(cost);
        self.ended += usize::from(!wrapped);
        Some(())
    }

    /// Drop the oldest line, the view keeping within what is left.
    fn pop_oldest(&mut self) {
        if let Some(line) = self.lines.pop_front() {
            let len = usize::from(line.len).min(self.cells.len());
            self.cells.drain(..len);
            self.base = self.base.wrapping_add(len);
            self.charged = self.charged.saturating_sub(usize::from(line.charge));
            self.ended = self.ended.saturating_sub(usize::from(!line.wrapped));
            self.view = self.view.min(self.lines.len());
        }
    }

    /// End the newest line where it is, what it continued into being gone.
    fn end_newest(&mut self) {
        if let Some(line) = self.lines.back_mut().filter(|line| line.wrapped) {
            line.wrapped = false;
            self.ended += 1;
        }
    }

    /// Hold the lines to `band` from now on, the oldest it leaves no room for
    /// going at once and the memory they held given back; answers whether any
    /// went.
    fn shed_for(&mut self, band: PressureBand) -> bool {
        self.band = band;
        self.hold_to_bound()
    }

    /// Keep the last `limit` lines from now on; answers whether any went.
    fn set_limit(&mut self, limit: usize) -> bool {
        self.limit = limit;
        self.hold_to_bound()
    }

    /// Let the oldest lines go until the rows are within the bound, giving
    /// back the memory they held; answers whether any went.
    fn hold_to_bound(&mut self) -> bool {
        let (held, bound) = (self.lines.len(), self.bound());
        while !self.lines.is_empty() && self.exceeds(bound) {
            self.pop_oldest();
        }
        let shed = self.lines.len() < held;
        if shed {
            self.cells.shrink_to_fit();
            self.lines.shrink_to_fit();
        }
        shed
    }

    /// Hold nothing and give the memory back, the view on the live screen.
    fn clear(&mut self) {
        self.cells = VecDeque::new();
        self.lines = VecDeque::new();
        self.base = 0;
        self.charged = 0;
        self.ended = 0;
        self.view = 0;
    }

    /// Lay the held lines and `old` out afresh at `cols`×`rows`, handing back
    /// the screen and keeping the lines above it within the bound, the view on
    /// the live screen; the held lines are laid afresh only when the width
    /// changed, since they are laid at it otherwise. All the layout needs is
    /// had first: `None`, changing nothing, when memory cannot be.
    fn reflow(&mut self, old: Rows<'_>, cursor: Cursor, cols: u16, rows: u16) -> Option<Laid> {
        let relay = old.cols != cols;
        let mut source = Vec::new();
        if relay {
            source.try_reserve_exact(usize::from(old.cols)).ok()?;
        }
        let mut laid = Laid::blank(cols, rows)?;
        let mut wrap = Rewrap::new(cols)?;
        let used = old.used(cursor);
        self.make_room(old, cursor, used, cols)?;
        if relay {
            self.relay(&mut wrap, &mut source);
        }
        wrap.start_screen();
        let mut fill = Fill::new(&mut laid);
        for row in 0..used {
            let (cells, end, target) = old.laid_row(row, used, cursor);
            wrap.lay(cells, target, &mut |laid_row, _, wrapped, holds| {
                fill.push(self, laid_row, wrapped, holds);
            });
            if end == RowEnd::Hard {
                wrap.end_line(&mut |laid_row, _, wrapped, holds| {
                    fill.push(self, laid_row, wrapped, holds);
                });
            }
            if fill.full {
                break;
            }
        }
        let (head, kept, at) = (fill.head, fill.held, fill.cursor);
        let pulled = self.pull(&mut laid, head, kept);
        let slots = usize::from(rows).max(1);
        let row = at.map_or(0, |slot| (slot + slots - head) % slots + pulled);
        let placed = wrap.cursor;
        laid.cursor = Cursor {
            col: placed
                .and_then(|placed| u16::try_from(placed.col).ok())
                .unwrap_or(0),
            row: u16::try_from(row).unwrap_or(0),
            pending: placed.is_some_and(|placed| placed.pending),
        };
        laid.in_use = u16::try_from(kept + pulled).unwrap_or(rows);
        self.view = 0;
        Some(laid)
    }

    /// Make the room laying `old`'s first `used` rows out at `cols` takes —
    /// the held lines' own where the width changes, and what the screen may
    /// give up — so the layout allocates nothing and cannot fail part-way,
    /// never past the bound; `None`, holding what it held, when memory cannot
    /// be had.
    fn make_room(&mut self, old: Rows<'_>, cursor: Cursor, used: usize, cols: u16) -> Option<()> {
        let per_row = usize::from(cols.saturating_sub(1).max(1));
        let rows_for = |cost: usize| cost / per_row + 1;
        let held_rows = if old.cols == cols {
            self.lines.len()
        } else {
            self.lines.iter().fold(0usize, |rows, line| {
                rows.saturating_add(rows_for(usize::from(line.charge)))
            })
        };
        let (mut given, mut given_rows) = (0usize, 0usize);
        for row in 0..used {
            let cost = charge(old.laid_row(row, used, cursor).0);
            given = given.saturating_add(cost);
            given_rows = given_rows.saturating_add(rows_for(cost));
        }
        let bound = self.bound().cells;
        let cells = self.charged.saturating_add(given).min(bound);
        let lines = held_rows.saturating_add(given_rows).min(bound);
        reserve_exactly(&mut self.cells, cells)?;
        reserve_exactly(&mut self.lines, lines)
    }

    /// Lay every held line out afresh at `wrap`'s width, in place in the ring,
    /// through `source`; the newest line's tail, where it continues onto the
    /// screen, is left open in `wrap`.
    fn relay(&mut self, wrap: &mut Rewrap, source: &mut Vec<Cell>) {
        #[cfg(test)]
        {
            self.relays += 1;
        }
        for _ in 0..self.lines.len() {
            let Some(line) = self.lines.pop_front() else {
                break;
            };
            let len = usize::from(line.len).min(self.cells.len());
            source.clear();
            if source.try_reserve(len).is_ok() {
                source.extend(self.cells.drain(..len));
            } else {
                self.cells.drain(..len);
            }
            self.base = self.base.wrapping_add(len);
            self.charged = self.charged.saturating_sub(usize::from(line.charge));
            self.ended = self.ended.saturating_sub(usize::from(!line.wrapped));
            wrap.lay(source.as_slice(), None, &mut |row, cost, wrapped, _| {
                let _ = self.append(row, cost, wrapped);
            });
            if !line.wrapped {
                wrap.end_line(&mut |row, cost, wrapped, _| {
                    let _ = self.append(row, cost, wrapped);
                });
            }
        }
    }

    /// Put the screen's rows in order from slot `head` and, where fewer were
    /// laid (`kept`) than it has, fill its top from the newest held lines, the
    /// laid rows moving down beneath them; answers how many came back.
    fn pull(&mut self, laid: &mut Laid, head: usize, kept: usize) -> usize {
        let (cols, stride) = (laid.cols, usize::from(laid.cols));
        let mid = head.saturating_mul(stride).min(laid.cells.len());
        laid.cells.rotate_left(mid);
        let turn = head.min(laid.ends.len());
        laid.ends.rotate_left(turn);
        let pulled = usize::from(laid.rows)
            .saturating_sub(kept)
            .min(self.lines.len());
        if pulled == 0 {
            return 0;
        }
        if let Some(moved) = laid.cells.get_mut(..(kept + pulled) * stride) {
            moved.rotate_right(pulled * stride);
        }
        if let Some(moved) = laid.ends.get_mut(..kept + pulled) {
            moved.rotate_right(pulled);
        }
        let first = self.lines.len() - pulled;
        for (slot, &line) in self.lines.iter().skip(first).enumerate() {
            laid.set_row(slot, self.cells_of(line), line.end(cols));
        }
        let (gone, ended) =
            self.lines
                .iter()
                .skip(first)
                .fold((0usize, 0usize), |(gone, ended), line| {
                    (
                        gone + usize::from(line.charge),
                        ended + usize::from(!line.wrapped),
                    )
                });
        if let Some(line) = self.lines.get(first).copied() {
            self.cells.truncate(line.start.wrapping_sub(self.base));
        }
        self.lines.truncate(first);
        self.charged = self.charged.saturating_sub(gone);
        self.ended = self.ended.saturating_sub(ended);
        pulled
    }
}

/// Where the cursor lands in the screen being laid: its row, counted from the
/// screen's first, its column, and whether its wrap is owed.
#[derive(Copy, Clone)]
struct Placed {
    line: usize,
    col: usize,
    pending: bool,
}

/// Lays lines out in rows of one width, a row closing when the next glyph does
/// not fit, so a wide glyph wraps whole.
struct Rewrap {
    /// The width rows are laid at.
    cols: usize,
    /// The row being filled.
    row: Vec<Cell>,
    /// What the row being filled is charged so far ([`charge`]).
    charge: usize,
    /// How many rows have closed since the screen's began.
    closed: usize,
    /// Whether the cursor lands on the next glyph laid, or where its line
    /// ends.
    due: bool,
    /// Where the cursor landed.
    cursor: Option<Placed>,
}

impl Rewrap {
    /// A layout at `cols`, or `None` when there is no memory for its row.
    fn new(cols: u16) -> Option<Self> {
        let mut row = Vec::new();
        row.try_reserve_exact(usize::from(cols)).ok()?;
        Some(Self {
            cols: usize::from(cols),
            row,
            charge: 0,
            closed: 0,
            due: false,
            cursor: None,
        })
    }

    /// Count the rows that close from here on as the screen's.
    fn start_screen(&mut self) {
        self.closed = 0;
    }

    /// Lay `source` out after what the open row holds, handing `out` each row
    /// that fills; `target` is the cursor's index in `source`, when it is on
    /// this row.
    fn lay(
        &mut self,
        source: &[Cell],
        target: Option<usize>,
        out: &mut impl FnMut(&[Cell], usize, bool, bool),
    ) {
        let mut index = 0;
        while let Some(&lead) = source.get(index) {
            self.due |= target == Some(index);
            index += 1;
            // A continuation is laid with its lead, and one without a lead is
            // dropped.
            if lead.ch == CONTINUATION {
                continue;
            }
            let wide = char_width(lead.ch) == 2;
            let mut tail = None;
            if wide {
                let paired = source
                    .get(index)
                    .copied()
                    .filter(|cell| cell.ch == CONTINUATION);
                let on_tail = paired.is_some() && target == Some(index);
                index += usize::from(paired.is_some());
                if self.cols < 2 {
                    // One column holds no continuation: a cursor on it rests
                    // on the lead.
                    self.due |= on_tail;
                } else {
                    let cell = paired.unwrap_or(Cell::styled(CONTINUATION, lead.attrs));
                    tail = Some((cell, on_tail));
                }
            }
            let width = 1 + usize::from(tail.is_some());
            if self.row.len() + width > self.cols {
                self.close(true, out);
            }
            let col = self.row.len();
            if self.due {
                self.land(col);
            }
            self.row.push(lead);
            self.charge += 1 + usize::from(wide);
            if let Some((cell, on_tail)) = tail {
                if on_tail {
                    self.land(col + 1);
                }
                self.row.push(cell);
            }
        }
        self.due |= target.is_some_and(|at| at >= source.len());
    }

    /// Record that the cursor lands at `col` of the open row.
    fn land(&mut self, col: usize) {
        self.cursor = Some(Placed {
            line: self.closed,
            col,
            pending: false,
        });
        self.due = false;
    }

    /// End the line being laid, landing a cursor still due where it ends:
    /// after a full row, with the wrap owed.
    fn end_line(&mut self, out: &mut impl FnMut(&[Cell], usize, bool, bool)) {
        if self.due {
            let col = self.row.len();
            self.cursor = Some(Placed {
                line: self.closed,
                col: col.min(self.cols.saturating_sub(1)),
                pending: col >= self.cols,
            });
            self.due = false;
        }
        self.close(false, out);
    }

    /// Hand `out` the open row — its cells, what it is charged, whether its
    /// line continues, and whether the cursor rests on it — and start a fresh
    /// one.
    fn close(&mut self, wrapped: bool, out: &mut impl FnMut(&[Cell], usize, bool, bool)) {
        let holds_cursor = self.cursor.is_some_and(|placed| placed.line == self.closed);
        let cost = self.charge.max(1);
        out(self.row.as_slice(), cost, wrapped, holds_cursor);
        self.row.clear();
        self.charge = 0;
        self.closed += 1;
    }
}

#[cfg(test)]
#[path = "grid_tests.rs"]
mod tests;
