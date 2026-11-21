//! Text (`plans/RISCOS-EMULATOR.md` §8): the text window and cursor, moved
//! in the directions `VDU 23,16` sets; characters at the text cursor or, in
//! VDU 5, at the graphics cursor; scrolling, by moving the screen where the
//! window is the whole screen and by copying where it is not; `VDU 23,7`
//! and `23,8`; and copy-cursor editing.
//!
//! The cursor is kept along the window's own axes from its home corner, so
//! `x` runs the way characters advance and `y` the way lines do, whichever
//! way round `VDU 23,16` has turned them; `x` one past the last column is a
//! newline held over, which the next character takes.

use tairix_riscos_kernel::os::{Fail, Os};

use crate::changed::Rect;
use crate::state::Vdu;

/// `VDU 23,16`'s bits.
pub(crate) mod flags {
    /// A newline held over at the end of a line rather than taken at once.
    pub(crate) const HOLD_NEWLINE: u8 = 1;
    /// Characters advance leftward.
    pub(crate) const X_REVERSED: u8 = 1 << 1;
    /// Lines advance upward.
    pub(crate) const Y_REVERSED: u8 = 1 << 2;
    /// Characters advance down the screen, lines across it.
    pub(crate) const SWAPPED: u8 = 1 << 3;
    /// A line past the last wraps to the first rather than scrolling.
    pub(crate) const WRAP: u8 = 1 << 4;
    /// The cursor stays where a character is printed.
    pub(crate) const NO_ADVANCE: u8 = 1 << 5;
    /// VDU 5 text goes on past the graphics window's edges.
    pub(crate) const NO_EDGES: u8 = 1 << 6;
}

/// A window of character cells, inclusive at every edge, rows counted from
/// the top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Window {
    pub(crate) left: u32,
    pub(crate) bottom: u32,
    pub(crate) right: u32,
    pub(crate) top: u32,
}

/// The text cursor's shape and flash: `VDU 23,0,10`, `23,0,11` and `23,1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CursorShape {
    /// The first and last of the cell's lines it covers.
    pub(crate) start: u8,
    pub(crate) end: u8,
    /// 0 steady, 1 off, 2 flashing fast, 3 flashing slowly.
    pub(crate) appearance: u8,
    /// `VDU 23,1`: shown at all.
    pub(crate) shown: bool,
}

/// A direction the window's contents move in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shift {
    Up,
    Down,
    Left,
    Right,
}

/// The text the drivers keep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Text {
    pub(crate) window: Window,
    /// The cursor along the window's axes from its home corner.
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) flags: u8,
    /// VDU 5: text at the graphics cursor.
    pub(crate) vdu5: bool,
    /// A text window has been set since the default windows: scrolling
    /// copies.
    pub(crate) windowed: bool,
    /// VDU 14: paged mode.
    pub(crate) paged: bool,
    pub(crate) cursor: CursorShape,
    /// Copy-cursor editing's cursor, along the window's axes.
    pub(crate) editing: Option<(u32, u32)>,
}

impl Text {
    /// A mode's text: the whole screen, the cursor home, shown.
    pub(crate) fn for_screen(columns: u32, rows: u32, cell_lines: u32, flags: u8) -> Self {
        Self {
            window: Window {
                left: 0,
                bottom: rows - 1,
                right: columns - 1,
                top: 0,
            },
            x: 0,
            y: 0,
            flags,
            vdu5: false,
            windowed: false,
            paged: false,
            cursor: CursorShape {
                start: 0,
                end: (cell_lines - 1).min(31).to_le_bytes()[0],
                appearance: 2,
                shown: true,
            },
            editing: None,
        }
    }

    const fn swapped(&self) -> bool {
        self.flags & flags::SWAPPED != 0
    }

    /// The window's extent along `x` and along `y`.
    pub(crate) const fn extent(&self) -> (u32, u32) {
        let columns = self.window.right - self.window.left + 1;
        let rows = self.window.bottom - self.window.top + 1;
        if self.swapped() {
            (rows, columns)
        } else {
            (columns, rows)
        }
    }

    /// The cell at `(x, y)` along the window's axes, as a column and a row
    /// of the screen, held to the window's last column and row.
    pub(crate) const fn cell(&self, x: u32, y: u32) -> (u32, u32) {
        let (width, height) = self.extent();
        let x = if x < width { x } else { width - 1 };
        let y = if y < height { y } else { height - 1 };
        let (across, down) = if self.swapped() { (y, x) } else { (x, y) };
        let (across_reversed, down_reversed) = if self.swapped() {
            (
                self.flags & flags::Y_REVERSED != 0,
                self.flags & flags::X_REVERSED != 0,
            )
        } else {
            (
                self.flags & flags::X_REVERSED != 0,
                self.flags & flags::Y_REVERSED != 0,
            )
        };
        let column = if across_reversed {
            self.window.right - across
        } else {
            self.window.left + across
        };
        let row = if down_reversed {
            self.window.bottom - down
        } else {
            self.window.top + down
        };
        (column, row)
    }

    /// The screen cell `(column, row)` along the window's axes, if it is in
    /// the window.
    pub(crate) const fn along(&self, column: u32, row: u32) -> Option<(u32, u32)> {
        let w = self.window;
        if column < w.left || column > w.right || row < w.top || row > w.bottom {
            return None;
        }
        let swapped = self.swapped();
        let (across_reversed, down_reversed) = if swapped {
            (
                self.flags & flags::Y_REVERSED != 0,
                self.flags & flags::X_REVERSED != 0,
            )
        } else {
            (
                self.flags & flags::X_REVERSED != 0,
                self.flags & flags::Y_REVERSED != 0,
            )
        };
        let across = if across_reversed {
            w.right - column
        } else {
            column - w.left
        };
        let down = if down_reversed {
            w.bottom - row
        } else {
            row - w.top
        };
        Some(if swapped {
            (down, across)
        } else {
            (across, down)
        })
    }

    /// The way the contents move when lines advance past the window's last.
    const fn shift_for_newline(&self) -> Shift {
        match (self.swapped(), self.flags & flags::Y_REVERSED != 0) {
            (false, false) => Shift::Up,
            (false, true) => Shift::Down,
            (true, false) => Shift::Left,
            (true, true) => Shift::Right,
        }
    }

    /// The cursor's screen cell, a held newline at the last column.
    pub(crate) const fn cursor_cell(&self) -> (u32, u32) {
        let (width, _) = self.extent();
        let x = if self.x >= width { width - 1 } else { self.x };
        self.cell(x, self.y)
    }

    /// `VDU 23,16`: the flags, the cursor kept where it is on the screen.
    pub(crate) fn set_flags(&mut self, flags: u8) {
        let (column, row) = self.cursor_cell();
        let editing = self.editing.map(|(x, y)| self.cell(x, y));
        self.flags = flags;
        if let Some((x, y)) = self.along(column, row) {
            self.x = x;
            self.y = y;
        }
        self.editing = editing.and_then(|(column, row)| self.along(column, row));
    }
}

/// What a step of text output needs before it can be taken: the keys looked
/// at for a scroll, which the drivers cannot do while they hold their state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Needs {
    Nothing,
    KeysBeforeScroll,
}

impl Vdu {
    /// The pixels a text cell takes across and down, its glyph's lines and
    /// the cell's.
    fn cell_pixels(&self) -> (u32, u32, u32) {
        let cell = self.screen.format.cell();
        (cell.size_x, cell.size_y, cell.space_y)
    }

    /// Whether moving down a line now would scroll, and the keys have not
    /// been looked at for it.
    pub(crate) fn needs_before_newline(&self) -> Needs {
        let (_, height) = self.text.extent();
        let scrolls = self.text.y + 1 >= height && self.text.flags & flags::WRAP == 0;
        if scrolls && !self.stream.scroll_cleared && !self.text.vdu5 {
            Needs::KeysBeforeScroll
        } else {
            Needs::Nothing
        }
    }

    /// Whether printing a character now would scroll first.
    pub(crate) fn needs_before_print(&self) -> Needs {
        let (width, _) = self.text.extent();
        if self.text.x >= width && !self.text.vdu5 {
            self.needs_before_newline()
        } else {
            Needs::Nothing
        }
    }

    /// The paged mode's limit on lines between halts: about three quarters
    /// of the window.
    pub(crate) fn page_lines(&self) -> u32 {
        let (_, height) = self.text.extent();
        (height * 3 / 4).max(1)
    }

    /// VDU 10: the cursor down a line, scrolling or wrapping past the last.
    pub(crate) fn line_down(&mut self, os: &mut Os<'_>) -> Result<(), Fail> {
        if self.text.vdu5 {
            self.vdu5_line(1);
            return Ok(());
        }
        let (_, height) = self.text.extent();
        let lines = os.byte_var(tairix_riscos_kernel::byte::var::PAGE_LINES);
        os.set_byte_var(
            tairix_riscos_kernel::byte::var::PAGE_LINES,
            lines.wrapping_add(1),
        );
        if self.text.y + 1 < height {
            self.text.y += 1;
        } else if self.text.flags & flags::WRAP != 0 {
            self.text.y = 0;
        } else {
            self.stream.scroll_cleared = false;
            let shift = self.text.shift_for_newline();
            self.scroll_window(os, shift, false)?;
        }
        Ok(())
    }

    /// VDU 11: the cursor up a line, scrolling or wrapping before the first.
    pub(crate) fn line_up(&mut self, os: &mut Os<'_>) -> Result<(), Fail> {
        if self.text.vdu5 {
            self.vdu5_line(-1);
            return Ok(());
        }
        let (_, height) = self.text.extent();
        if self.text.y > 0 {
            self.text.y -= 1;
        } else if self.text.flags & flags::WRAP != 0 {
            self.text.y = height - 1;
        } else {
            let shift = match self.text.shift_for_newline() {
                Shift::Up => Shift::Down,
                Shift::Down => Shift::Up,
                Shift::Left => Shift::Right,
                Shift::Right => Shift::Left,
            };
            self.scroll_window(os, shift, false)?;
        }
        Ok(())
    }

    /// VDU 8: the cursor back a character, to the end of the line before
    /// from the first column.
    pub(crate) fn back(&mut self, os: &mut Os<'_>) -> Result<(), Fail> {
        if self.text.vdu5 {
            self.vdu5_step(-1);
            return Ok(());
        }
        let (width, _) = self.text.extent();
        if self.text.x >= width {
            self.text.x = width - 1;
        } else if self.text.x > 0 {
            self.text.x -= 1;
        } else {
            self.text.x = width - 1;
            self.line_up(os)?;
        }
        Ok(())
    }

    /// VDU 9: the cursor on a character, to the next line's start past the
    /// last column.
    pub(crate) fn forward(&mut self, os: &mut Os<'_>) -> Result<(), Fail> {
        if self.text.vdu5 {
            self.vdu5_step(1);
            return Ok(());
        }
        let (width, _) = self.text.extent();
        if self.text.x + 1 >= width {
            self.text.x = 0;
            self.line_down(os)?;
        } else {
            self.text.x += 1;
        }
        Ok(())
    }

    /// VDU 13: to the line's start.
    pub(crate) fn carriage_return(&mut self) {
        self.text.editing = None;
        if self.text.vdu5 {
            self.vdu5_return();
        } else {
            self.text.x = 0;
        }
    }

    /// VDU 30: home.
    pub(crate) fn home(&mut self) {
        if self.text.vdu5 {
            self.vdu5_home();
        } else {
            self.text.x = 0;
            self.text.y = 0;
        }
    }

    /// VDU 31: the cursor to `(x, y)` from home, if in the window; just past
    /// the last column holds a newline over where newlines are held.
    pub(crate) fn tab(&mut self, x: u8, y: u8) {
        let (x, y) = (u32::from(x), u32::from(y));
        if self.text.vdu5 {
            self.vdu5_tab(x, y);
            return;
        }
        let (width, height) = self.text.extent();
        let held = self.text.flags & flags::HOLD_NEWLINE != 0 && x == width;
        if (x < width || held) && y < height {
            self.text.x = x;
            self.text.y = y;
        }
    }

    /// Prints the character `c` at the text cursor or, in VDU 5, the
    /// graphics cursor, then moves on.
    pub(crate) fn print(&mut self, os: &mut Os<'_>, c: u8) -> Result<(), Fail> {
        if self.text.vdu5 {
            return self.vdu5_print(os, c);
        }
        let (width, _) = self.text.extent();
        if self.text.x >= width {
            self.text.x = 0;
            self.line_down(os)?;
        }
        let (column, row) = self.text.cursor_cell();
        self.draw_char(os, column, row, c)?;
        if self.text.flags & flags::NO_ADVANCE == 0 {
            self.text.x += 1;
            if self.text.x >= width && self.text.flags & flags::HOLD_NEWLINE == 0 {
                self.text.x = 0;
                self.line_down(os)?;
            }
        }
        Ok(())
    }

    /// VDU 127: back a character, then that character blanked.
    pub(crate) fn delete(&mut self, os: &mut Os<'_>) -> Result<(), Fail> {
        if self.text.flags & flags::NO_ADVANCE == 0 {
            self.back(os)?;
        }
        if self.text.vdu5 {
            return self.vdu5_blank(os);
        }
        let (column, row) = self.text.cursor_cell();
        self.clear_cells(os, column, row, column, row)
    }

    /// Draws character `c` in the cell at `column`, `row` in the text
    /// colours.
    pub(crate) fn draw_char(
        &mut self,
        os: &mut Os<'_>,
        column: u32,
        row: u32,
        c: u8,
    ) -> Result<(), Fail> {
        if self.screen.format.has(crate::mode::flag::TELETEXT) {
            return self.teletext_put(os, column, row, c);
        }
        let format = self.screen.format;
        let (fore, back) = self.colours.text_words(&format);
        let glyph = self.font.glyph(c).copied().unwrap_or([0; 8]);
        let (size_x, size_y, space_y) = self.cell_pixels();
        let canvas = self.screen.canvas();
        let log2_bpc = format.log2_bpc();
        let bytes_across = (size_x << log2_bpc) / 8;
        let mut line = [0u8; 64];
        let line = &mut line[..bytes_across as usize];
        let doubled = size_y > 8;
        for pixel_row in 0..space_y {
            let bits = if pixel_row < size_y {
                glyph[(if doubled { pixel_row / 2 } else { pixel_row }) as usize]
            } else {
                0
            };
            let gap = pixel_row >= size_y;
            glyph_line(line, bits, fore, back, log2_bpc, gap_word(self, gap, back));
            canvas.put(os, row * space_y + pixel_row, column * bytes_across, line)?;
        }
        self.changed_cells(os, column, row, column, row)
    }

    /// Blanks the cells from `(left, top)` to `(right, bottom)` in the text
    /// background.
    pub(crate) fn clear_cells(
        &mut self,
        os: &mut Os<'_>,
        left: u32,
        top: u32,
        right: u32,
        bottom: u32,
    ) -> Result<(), Fail> {
        if self.screen.format.has(crate::mode::flag::TELETEXT) {
            return self.teletext_clear(os, left, top, right, bottom);
        }
        let format = self.screen.format;
        let (_, back) = self.colours.text_words(&format);
        let (size_x, size_y, space_y) = self.cell_pixels();
        let bytes_across = (size_x << format.log2_bpc()) / 8;
        let canvas = self.screen.canvas();
        let (byte, length) = (left * bytes_across, (right - left + 1) * bytes_across);
        match gap_word(self, true, back) {
            Some(gap) if gap != back => {
                for row in top..=bottom {
                    let first = row * space_y;
                    canvas.fill_bytes(os, first..first + size_y, byte, length, back)?;
                    canvas.fill_bytes(os, first + size_y..first + space_y, byte, length, gap)?;
                }
            }
            _ => canvas.fill_bytes(
                os,
                top * space_y..(bottom + 1) * space_y,
                byte,
                length,
                back,
            )?,
        }
        self.changed_cells(os, left, top, right, bottom)
    }

    /// VDU 12 in VDU 4: the text window blanked, the cursor home.
    pub(crate) fn clear_text_window(&mut self, os: &mut Os<'_>) -> Result<(), Fail> {
        let w = self.text.window;
        self.clear_cells(os, w.left, w.top, w.right, w.bottom)?;
        self.text.x = 0;
        self.text.y = 0;
        Ok(())
    }

    /// Notes the cells from `(left, top)` to `(right, bottom)` changed.
    fn changed_cells(
        &mut self,
        os: &mut Os<'_>,
        left: u32,
        top: u32,
        right: u32,
        bottom: u32,
    ) -> Result<(), Fail> {
        let (size_x, _, space_y) = self.cell_pixels();
        let height = self.screen.format.height();
        let rect = Rect {
            left: (left * size_x).cast_signed(),
            right: ((right + 1) * size_x - 1).cast_signed(),
            top: (height - 1 - top * space_y).cast_signed(),
            bottom: (height - (bottom + 1) * space_y).cast_signed(),
        };
        crate::changed::cover(os, rect)
    }

    /// Moves the text window's contents one line in `shift`, or the whole
    /// screen's where `screen` asks; the line left behind blanked.
    pub(crate) fn scroll_window(
        &mut self,
        os: &mut Os<'_>,
        shift: Shift,
        screen: bool,
    ) -> Result<(), Fail> {
        let format = self.screen.format;
        let whole = Window {
            left: 0,
            top: 0,
            right: format.columns() - 1,
            bottom: format.rows() - 1,
        };
        let window = if screen { whole } else { self.text.window };
        let full = window == whole && !self.text.windowed;
        if full && matches!(shift, Shift::Up | Shift::Down) && self.screen.hardware_scrolls() {
            let (_, _, space_y) = self.cell_pixels();
            self.screen.hardware_scroll(shift == Shift::Up, space_y);
            let blank = if shift == Shift::Up {
                window.bottom
            } else {
                window.top
            };
            self.clear_cells(os, window.left, blank, window.right, blank)?;
            return crate::changed::cover_screen(os, &format);
        }
        self.shift_cells(os, window, shift, 1)
    }

    /// Moves the contents of `window` by `count` cells in `shift`, the cells
    /// left behind blanked.
    pub(crate) fn shift_cells(
        &mut self,
        os: &mut Os<'_>,
        window: Window,
        shift: Shift,
        count: u32,
    ) -> Result<(), Fail> {
        if self.screen.format.has(crate::mode::flag::TELETEXT) {
            return self.teletext_shift(os, window, shift, count);
        }
        let format = self.screen.format;
        let (size_x, _, space_y) = self.cell_pixels();
        let bytes_across = (size_x << format.log2_bpc()) / 8;
        let canvas = self.screen.canvas();
        let columns = window.right - window.left + 1;
        let rows = window.bottom - window.top + 1;
        match shift {
            Shift::Up | Shift::Down => {
                let moved = rows.saturating_sub(count);
                let (from, to) = if shift == Shift::Up {
                    (window.top + count, window.top)
                } else {
                    (window.top, window.top + count)
                };
                if moved > 0 {
                    canvas.copy_lines(
                        os,
                        (from * space_y, window.left * bytes_across),
                        (to * space_y, window.left * bytes_across),
                        moved * space_y,
                        columns * bytes_across,
                    )?;
                }
                let (first, last) = if shift == Shift::Up {
                    (window.top + moved, window.bottom)
                } else {
                    (window.top, window.top + count.min(rows) - 1)
                };
                self.clear_cells(os, window.left, first, window.right, last)?;
            }
            Shift::Left | Shift::Right => {
                let moved = columns.saturating_sub(count);
                let (from, to) = if shift == Shift::Left {
                    (window.left + count, window.left)
                } else {
                    (window.left, window.left + count)
                };
                if moved > 0 {
                    canvas.copy_lines(
                        os,
                        (window.top * space_y, from * bytes_across),
                        (window.top * space_y, to * bytes_across),
                        rows * space_y,
                        moved * bytes_across,
                    )?;
                }
                let (first, last) = if shift == Shift::Left {
                    (window.left + moved, window.right)
                } else {
                    (window.left, window.left + count.min(columns) - 1)
                };
                self.clear_cells(os, first, window.top, last, window.bottom)?;
            }
        }
        self.changed_cells(os, window.left, window.top, window.right, window.bottom)
    }

    /// `VDU 23,7,extent,direction,movement`: the window's or the screen's
    /// contents moved a cell, or across a byte where `movement` is 1, in a
    /// screen direction or one along the cursor's axes.
    pub(crate) fn scroll_23_7(
        &mut self,
        os: &mut Os<'_>,
        extent: u8,
        direction: u8,
        movement: u8,
    ) -> Result<(), Fail> {
        let shift = match direction & 7 {
            0 => Shift::Right,
            1 => Shift::Left,
            2 => Shift::Down,
            3 => Shift::Up,
            along => {
                let newline = self.text.shift_for_newline();
                let advance = match (
                    self.text.swapped(),
                    self.text.flags & flags::X_REVERSED != 0,
                ) {
                    (false, false) => Shift::Left,
                    (false, true) => Shift::Right,
                    (true, false) => Shift::Up,
                    (true, true) => Shift::Down,
                };
                let opposite = |s: Shift| match s {
                    Shift::Up => Shift::Down,
                    Shift::Down => Shift::Up,
                    Shift::Left => Shift::Right,
                    Shift::Right => Shift::Left,
                };
                match along {
                    4 => opposite(advance),
                    5 => advance,
                    6 => opposite(newline),
                    _ => newline,
                }
            }
        };
        let format = self.screen.format;
        let whole = Window {
            left: 0,
            top: 0,
            right: format.columns() - 1,
            bottom: format.rows() - 1,
        };
        let window = if extent == 0 { self.text.window } else { whole };
        if movement == 0 || matches!(shift, Shift::Up | Shift::Down) {
            return self.shift_cells(os, window, shift, 1);
        }
        self.shift_bytes(os, window, shift == Shift::Left)
    }

    /// Moves the contents of `window` a byte left or right, the byte left
    /// behind blanked.
    fn shift_bytes(
        &mut self,
        os: &mut Os<'_>,
        window: Window,
        left_ward: bool,
    ) -> Result<(), Fail> {
        let format = self.screen.format;
        let (size_x, _, space_y) = self.cell_pixels();
        let bytes_across = (size_x << format.log2_bpc()) / 8;
        let canvas = self.screen.canvas();
        let (_, back) = self.colours.text_words(&format);
        let top = window.top * space_y;
        let lines = (window.bottom - window.top + 1) * space_y;
        let left = window.left * bytes_across;
        let across = (window.right - window.left + 1) * bytes_across;
        if left_ward {
            canvas.copy_lines(os, (top, left + 1), (top, left), lines, across - 1)?;
            canvas.fill_bytes(os, top..top + lines, left + across - 1, 1, back)?;
        } else {
            canvas.copy_lines(os, (top, left), (top, left + 1), lines, across - 1)?;
            canvas.fill_bytes(os, top..top + lines, left, 1, back)?;
        }
        self.changed_cells(os, window.left, window.top, window.right, window.bottom)
    }

    /// `VDU 23,8,start,end,x1,y1,x2,y2`: blanks the cells from a place by
    /// `start` moved by `(x1, y1)`, up to the one by `end` moved by `(x2,
    /// y2)`, in the order characters are printed; the cursor stays.
    pub(crate) fn clear_block(
        &mut self,
        os: &mut Os<'_>,
        start: u8,
        end: u8,
        from: (i8, i8),
        to: (i8, i8),
    ) -> Result<(), Fail> {
        let (width, height) = self.text.extent();
        let place = |base: u8, (dx, dy): (i8, i8)| -> Option<i64> {
            let x = match base & 3 {
                0 => 0,
                1 => i64::from(self.text.x.min(width - 1)),
                2 => i64::from(width),
                _ => return None,
            };
            let y = match base & 0b1100 {
                0b0000 => 0,
                0b0100 => i64::from(self.text.y),
                0b1000 => i64::from(height) - 1,
                _ => return None,
            };
            Some((y + i64::from(dy)) * i64::from(width) + x + i64::from(dx))
        };
        let (Some(first), Some(last)) = (place(start, from), place(end, to)) else {
            return Ok(());
        };
        let cells = width.saturating_mul(height);
        let clamp = |at: i64| u32::try_from(at.clamp(0, i64::from(cells))).unwrap_or(cells);
        let (first, last) = (clamp(first), clamp(last));
        if first >= last {
            return Ok(());
        }
        let (x0, y0) = (first % width, first / width);
        let (x1, y1) = ((last - 1) % width, (last - 1) / width);
        let stretches = if y0 == y1 {
            [Some((x0, y0, x1, y1)), None, None]
        } else {
            [
                Some((x0, y0, width - 1, y0)),
                (y1 > y0 + 1).then_some((0, y0 + 1, width - 1, y1 - 1)),
                Some((0, y1, x1, y1)),
            ]
        };
        for (xa, ya, xb, yb) in stretches.into_iter().flatten() {
            let ((ca, ra), (cb, rb)) = (self.text.cell(xa, ya), self.text.cell(xb, yb));
            self.clear_cells(os, ca.min(cb), ra.min(rb), ca.max(cb), ra.max(rb))?;
        }
        Ok(())
    }

    /// The character in the cell at `column`, `row`: its pixels against the
    /// text background, matched to the soft character set; nought where none
    /// matches.
    pub(crate) fn read_char(&self, os: &mut Os<'_>, column: u32, row: u32) -> Result<u8, Fail> {
        if self.screen.format.has(crate::mode::flag::TELETEXT) {
            return Ok(self.teletext.char_at(column, row));
        }
        let format = self.screen.format;
        if column >= format.columns() || row >= format.rows() {
            return Ok(0);
        }
        let back = crate::colour::Colours::text_pixel(&format, &self.colours.text_back);
        let unit_mask = if format.log2_bpp() >= 5 {
            u32::MAX
        } else {
            (1 << (1 << format.log2_bpp())) - 1
        };
        let (size_x, size_y, space_y) = self.cell_pixels();
        let canvas = self.screen.canvas();
        let step = if size_y > 8 { 2 } else { 1 };
        let mut glyph = [0u8; 8];
        let mut pixels = alloc::vec::Vec::new();
        for (slot, i) in glyph.iter_mut().zip(0u32..) {
            let line = row * space_y + i * step;
            pixels.clear();
            canvas.pixels(
                os,
                line,
                column * size_x,
                column * size_x + size_x - 1,
                &mut pixels,
            )?;
            *slot = pixels.iter().take(8).fold(0, |bits, &p| {
                (bits << 1) | u8::from(p & unit_mask != back & unit_mask)
            });
        }
        Ok(self.font.find(glyph).unwrap_or(0))
    }

    /// A key that moves the copy cursor or copies: 0 Copy, then left, right,
    /// down and up. Copy gives the character under the copy cursor and moves
    /// it on; the arrows start editing at the text cursor.
    pub(crate) fn edit_key(&mut self, os: &mut Os<'_>, key: u8) -> Result<Option<u8>, Fail> {
        let (width, height) = self.text.extent();
        let at = self
            .text
            .editing
            .unwrap_or((self.text.x.min(width - 1), self.text.y));
        if key == 0 {
            let Some((x, y)) = self.text.editing else {
                return Ok(None);
            };
            let (column, row) = self.text.cell(x, y);
            let c = self.read_char(os, column, row)?;
            let next = if x + 1 < width {
                (x + 1, y)
            } else {
                (0, (y + 1).min(height - 1))
            };
            self.text.editing = Some(next);
            return Ok((c != 0).then_some(c));
        }
        let (x, y) = at;
        self.text.editing = Some(match key {
            1 => (if x == 0 { width - 1 } else { x - 1 }, y),
            2 => (if x + 1 >= width { 0 } else { x + 1 }, y),
            3 => (x, if y + 1 >= height { 0 } else { y + 1 }),
            _ => (x, if y == 0 { height - 1 } else { y - 1 }),
        });
        Ok(None)
    }
}

/// The bytes of one line of a glyph: each of its eight pixels, the leftmost
/// from the top bit, in the foreground or background word's pixel; a gap's
/// line wholly in `gap`.
fn glyph_line(out: &mut [u8], bits: u8, fore: u32, back: u32, log2_bpc: u32, gap: Option<u32>) {
    let unit = 1u32 << log2_bpc;
    let mask = if unit >= 32 {
        u32::MAX
    } else {
        (1 << unit) - 1
    };
    out.fill(0);
    for p in 0..8u32 {
        let lit = bits & (0x80 >> p) != 0;
        let word = match gap {
            Some(gap) => gap,
            None if lit => fore,
            None => back,
        };
        let value = word & mask;
        let bit = p << log2_bpc;
        let mut written = 0;
        while written < unit {
            let at = (bit + written) / 8;
            let shift = (bit + written) % 8;
            let take = (8 - shift).min(unit - written);
            let part = (value >> written) & ((1 << take) - 1);
            out[at as usize] |= (part << shift).to_le_bytes()[0];
            written += take;
        }
    }
}

/// The word a gap line is drawn in: the background, or in a BBC gap mode the
/// background's own colour moved up two.
fn gap_word(vdu: &Vdu, gap: bool, back: u32) -> Option<u32> {
    if !gap {
        return None;
    }
    let format = &vdu.screen.format;
    if format.has(crate::mode::flag::BBC_GAP) {
        let pixel = crate::colour::Colours::text_pixel(format, &vdu.colours.text_back) | 2;
        return Some(crate::colour::replicate(format, pixel));
    }
    Some(back)
}

#[cfg(test)]
mod tests;
