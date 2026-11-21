//! Teletext (`plans/RISCOS-EMULATOR.md` §8): mode 7 as RISC OS 5 has it,
//! a page of characters drawn in software into an ordinary sixteen-colour
//! screen — each cell 16 by 20 pixels from `lib/teletext`'s glyphs, half-dot
//! for pixel — twice over, the second frame without its flashing characters,
//! the frames shown in turn as the colours flash. `VDU 23,18` sets the
//! transparency, animation, reveal and enhancement controls.

use alloc::vec::Vec;

use tairix_riscos_kernel::os::{Fail, Os};
use tairix_teletext::{is_double_height, Attributes, GLYPHS, HALF_DOTS, HALF_LINES};
use tairix_vdu::mode::Flags;

use crate::changed;
use crate::state::Vdu;
use crate::text::{Shift, Window};

/// A cell's pixels across.
pub(crate) const CELL_WIDTH: u32 = 16;
/// A cell's pixels down: one for each of its glyph's half-dot lines.
#[expect(clippy::cast_possible_truncation, reason = "twenty lines")]
pub(crate) const CELL_HEIGHT: u32 = HALF_LINES as u32;
/// The page's size most a mode may give it.
pub(crate) const MOST_COLUMNS: usize = 255;
pub(crate) const MOST_ROWS: usize = 255;

/// The page and its controls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Teletext {
    /// Each row's characters, a space where none was put.
    cells: Vec<Vec<u8>>,
    /// `VDU 23,18`'s settings: transparency, animation, reveal and the
    /// enhancements.
    controls: [u8; 4],
    /// Whether each row was last drawn as a double-height pair's lower half.
    drawn_lower: Vec<bool>,
}

impl Teletext {
    pub(crate) const fn new() -> Self {
        Self {
            cells: Vec::new(),
            controls: [0; 4],
            drawn_lower: Vec::new(),
        }
    }

    /// The character at `column`, `row`; nought off the page.
    pub(crate) fn char_at(&self, column: u32, row: u32) -> u8 {
        self.cells
            .get(row as usize)
            .and_then(|r| r.get(column as usize))
            .copied()
            .unwrap_or(0)
    }

    /// Whether concealed characters show.
    const fn revealed(&self) -> bool {
        self.controls[2] != 0
    }
}

impl Vdu {
    /// The page sized to the mode, every cell a space.
    fn teletext_page(&mut self, os: &mut Os<'_>) -> Result<(), Fail> {
        let (columns, rows) = (
            self.screen.format.columns() as usize,
            self.screen.format.rows() as usize,
        );
        let (columns, rows) = (columns.min(MOST_COLUMNS), rows.min(MOST_ROWS));
        if self.teletext.cells.len() == rows
            && self
                .teletext
                .cells
                .first()
                .is_some_and(|r| r.len() == columns)
        {
            return Ok(());
        }
        let mut cells = Vec::new();
        if cells.try_reserve_exact(rows).is_err() {
            return Err(os.no_memory());
        }
        for _ in 0..rows {
            let mut row = Vec::new();
            if row.try_reserve_exact(columns).is_err() {
                return Err(os.no_memory());
            }
            row.resize(columns, b' ');
            cells.push(row);
        }
        self.teletext.cells = cells;
        self.teletext.drawn_lower.clear();
        if self.teletext.drawn_lower.try_reserve_exact(rows).is_err() {
            return Err(os.no_memory());
        }
        self.teletext.drawn_lower.resize(rows, false);
        Ok(())
    }

    /// `c` put in the cell at `column`, `row`, and the rows it changes
    /// drawn again.
    pub(crate) fn teletext_put(
        &mut self,
        os: &mut Os<'_>,
        column: u32,
        row: u32,
        c: u8,
    ) -> Result<(), Fail> {
        self.teletext_page(os)?;
        if let Some(cell) = self
            .teletext
            .cells
            .get_mut(row as usize)
            .and_then(|r| r.get_mut(column as usize))
        {
            *cell = c;
        }
        self.teletext_draw(os, row, false)
    }

    /// The cells from `(left, top)` to `(right, bottom)` spaces.
    pub(crate) fn teletext_clear(
        &mut self,
        os: &mut Os<'_>,
        left: u32,
        top: u32,
        right: u32,
        bottom: u32,
    ) -> Result<(), Fail> {
        self.teletext_page(os)?;
        let rows = self
            .teletext
            .cells
            .iter_mut()
            .take((bottom as usize).saturating_add(1))
            .skip(top as usize);
        for cells in rows {
            for cell in cells
                .iter_mut()
                .take((right as usize).saturating_add(1))
                .skip(left as usize)
            {
                *cell = b' ';
            }
        }
        self.teletext_draw(os, top, true)
    }

    /// The window's cells moved `count` in `shift`, the cells left spaces;
    /// the part of the window past the page holds only spaces.
    pub(crate) fn teletext_shift(
        &mut self,
        os: &mut Os<'_>,
        window: Window,
        shift: Shift,
        count: u32,
    ) -> Result<(), Fail> {
        self.teletext_page(os)?;
        let page = &mut self.teletext.cells;
        let columns = page.first().map_or(0, Vec::len);
        let (left, top) = (window.left as usize, window.top as usize);
        let right = (window.right as usize).min(columns.saturating_sub(1));
        let bottom = (window.bottom as usize).min(page.len().saturating_sub(1));
        let count = count as usize;
        if columns > 0 && count > 0 && left <= right && top <= bottom {
            match shift {
                Shift::Up => {
                    for y in top..=bottom {
                        if y.saturating_add(count) <= bottom {
                            let (to, from) = page.split_at_mut(y + count);
                            to[y][left..=right].copy_from_slice(&from[0][left..=right]);
                        } else {
                            page[y][left..=right].fill(b' ');
                        }
                    }
                }
                Shift::Down => {
                    for y in (top..=bottom).rev() {
                        if y >= top.saturating_add(count) {
                            let (from, to) = page.split_at_mut(y);
                            to[0][left..=right].copy_from_slice(&from[y - count][left..=right]);
                        } else {
                            page[y][left..=right].fill(b' ');
                        }
                    }
                }
                Shift::Left | Shift::Right => {
                    for row in &mut page[top..=bottom] {
                        let cells = &mut row[left..=right];
                        let moved = count.min(cells.len());
                        let kept = cells.len() - moved;
                        if shift == Shift::Left {
                            cells.copy_within(moved.., 0);
                            cells[kept..].fill(b' ');
                        } else {
                            cells.copy_within(..kept, moved);
                            cells[..moved].fill(b' ');
                        }
                    }
                }
            }
        }
        self.teletext_draw(os, window.top, true)
    }

    /// `VDU 23,18,action,value`.
    pub(crate) fn teletext_control(
        &mut self,
        os: &mut Os<'_>,
        action: u8,
        value: u8,
    ) -> Result<(), Fail> {
        if let Some(slot) = self.teletext.controls.get_mut(usize::from(action)) {
            *slot = value;
        }
        if self.screen.format.has(Flags::TELETEXT) {
            self.teletext_page(os)?;
            self.teletext_draw(os, 0, true)?;
        }
        Ok(())
    }

    /// Draws row `first` into both frames, then each row after it whose
    /// half of a double-height pair has changed — or every row after it
    /// where `all`.
    fn teletext_draw(&mut self, os: &mut Os<'_>, first: u32, all: bool) -> Result<(), Fail> {
        let format = self.screen.format;
        let canvas = self.screen.canvas();
        let frame = format.line_length() * format.height();
        let cell = format.cell();
        let mut lower = false;
        let width = format.line_length() as usize;
        let (mut line, mut shown) = (Vec::new(), Vec::new());
        if line.try_reserve_exact(width).is_err() || shown.try_reserve_exact(MOST_COLUMNS).is_err()
        {
            return Err(os.no_memory());
        }
        line.resize(width, 0);
        for (row, cells) in (0u32..).zip(&self.teletext.cells) {
            let double_seen = cells.iter().any(|&byte| is_double_height(byte));
            let changed = self
                .teletext
                .drawn_lower
                .get(row as usize)
                .is_some_and(|&was| was != lower);
            if row >= first && (row == first || all || changed) {
                if let Some(was) = self.teletext.drawn_lower.get_mut(row as usize) {
                    *was = lower;
                }
                let mut attributes = Attributes::LINE_START;
                shown.clear();
                shown.extend(cells.iter().map(|&byte| attributes.character(byte)));
                let lines_shown = format
                    .height()
                    .saturating_sub(row * cell.space_y)
                    .min(cell.space_y);
                for pixel_line in 0..lines_shown {
                    for (frame_index, flash_on) in [(0, true), (1, false)] {
                        fill_line(
                            &mut line,
                            &shown,
                            pixel_line,
                            lower,
                            flash_on,
                            self.teletext.revealed(),
                        );
                        let at = canvas.address(row * cell.space_y + pixel_line, 0)
                            + frame_index * frame;
                        os.write_bytes(at, &line)?;
                    }
                }
            }
            lower = tairix_teletext::lower_after(lower, double_seen);
        }
        changed::cover_screen(os, &format)
    }
}

/// One pixel line of a row in four-bit pixels: each cell's sixteen pixels,
/// pixel `p` the half-dot under its middle.
fn fill_line(
    line: &mut [u8],
    shown: &[tairix_teletext::Shown],
    pixel_line: u32,
    lower: bool,
    flash_on: bool,
    revealed: bool,
) {
    line.fill(0);
    for (column, s) in shown.iter().enumerate() {
        let a = s.attributes;
        let visible = (!a.conceal || revealed) && (!a.flash || flash_on) && a.shows_in(lower);
        let half_line = if a.double {
            (tairix_teletext::double_first_line(lower) + pixel_line as usize / 2)
                .min(HALF_LINES - 1)
        } else {
            (pixel_line as usize).min(HALF_LINES - 1)
        };
        let bits = match s.cell.filter(|_| visible) {
            Some(index) => GLYPHS[index][half_line],
            None => 0,
        };
        for p in 0..CELL_WIDTH {
            let half_dot = (2 * p + 1) * HALF_DOTS / (2 * CELL_WIDTH);
            let lit = bits >> (HALF_DOTS - 1 - half_dot) & 1 != 0;
            let colour = if lit { a.foreground } else { a.background };
            let x = column * CELL_WIDTH as usize + p as usize;
            if let Some(byte) = line.get_mut(x / 2) {
                *byte |= colour << (4 * (x % 2));
            }
        }
    }
}
