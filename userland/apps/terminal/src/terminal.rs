//! The terminal model: the screen [`Grid`] glued to a [`ShellSource`].
//!
//! [`Terminal`] owns the screen [`Grid`], the [`Parser`] that drives it, and
//! the [`ShellSource`] channel to the hosted shell. It is the analogue of the
//! file browser's `Browser`: the navigation/parsing logic lives here and the
//! outside world is reached only through the injected seam, so the whole model
//! is testable without a kernel.
//!
//! Two operations cross the seam:
//!
//! * [`Terminal::pump`] reads whatever the shell has produced and feeds it to
//!   the grid, returning how many bytes were applied.
//! * [`Terminal::send`] / [`Terminal::send_str`] forward the user's input to
//!   the shell.
//!
//! The terminal never echoes input to the screen itself: echo (and all line
//! editing and job control) is the shell's responsibility, exactly as on a
//! real tty. A failing seam call surfaces the boundary [`Errno`] and leaves
//! the screen unchanged.
//!
//! The scrollback never crosses the seam: the view back over it is moved here
//! and read through [`Terminal::grid`]. A resize is asked for and given apart
//! ([`Terminal::resize`], [`Terminal::settle`]), so a burst of window sizes
//! costs the screen one.

use tairix_abi::Errno;
use tairix_input::wheel::wheel_steps;
use tairix_input::{Key, Modifiers, NamedKey};
use tairix_reclaim::PressureBand;
use tairix_vt::EraseMode;

use crate::grid::Grid;
use crate::parser::Parser;
use crate::shell::ShellSource;

/// A character-cell terminal hosting a shell over a [`ShellSource`] seam.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Terminal<S: ShellSource> {
    grid: Grid,
    parser: Parser,
    shell: S,
    /// What the wheel has turned short of a line.
    wheel_carry: i64,
    /// The size last asked for and not yet given.
    wanted: Option<(u16, u16)>,
}

/// Lines the wheel moves the view a detent.
pub const WHEEL_LINES: u64 = 3;

/// What [`Terminal::settle`] did with the size last asked for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Reshaped {
    /// None was asked for, or the one asked for is the size in force.
    Unchanged,
    /// The screen took this size, which the shell is now to be told.
    Took {
        /// The columns it took.
        cols: u16,
        /// The rows it took.
        rows: u16,
    },
    /// The size could not be had — a dimension the grid refuses, or memory it
    /// cannot get — and the screen keeps the one it has, as the shell does.
    Refused,
}

impl<S: ShellSource> Terminal<S> {
    /// Create a `cols`×`rows` terminal over `shell`, with a blank screen, the
    /// cursor at the home position, and a scrollback keeping the last
    /// `scrollback` lines.
    ///
    /// Returns `None` for a screen size [`Grid::new`] rejects, so an unusable
    /// geometry fails closed rather than allocating something degenerate.
    #[must_use]
    pub fn new(cols: u16, rows: u16, shell: S, scrollback: usize) -> Option<Self> {
        Some(Self {
            grid: Grid::new(cols, rows, scrollback)?,
            parser: Parser::new(),
            shell,
            wheel_carry: 0,
            wanted: None,
        })
    }

    /// The screen grid, for rendering and inspection.
    #[must_use]
    pub const fn grid(&self) -> &Grid {
        &self.grid
    }

    /// Ask for the screen to be `cols`×`rows` (a window resize). Nothing is
    /// reshaped until [`settle`](Self::settle), so a burst of asks costs one
    /// reshape, at the last size asked.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.wanted = Some((cols, rows));
    }

    /// Give the screen the size last asked for, reflowing the main screen with
    /// its scrollback ([`Grid::resize`]), and answer what that owes the shell.
    ///
    /// The shell learns a size only through the pty window size the caller
    /// sets for [`Reshaped::Took`] (`pty_set_size`), so its prompt and any
    /// full-screen program re-lay-out for the size the screen really has, and
    /// never for one it refused.
    #[must_use]
    pub fn settle(&mut self) -> Reshaped {
        let Some((cols, rows)) = self.wanted.take() else {
            return Reshaped::Unchanged;
        };
        if (cols, rows) == (self.grid.cols(), self.grid.rows()) {
            Reshaped::Unchanged
        } else if self.grid.resize(cols, rows) {
            Reshaped::Took { cols, rows }
        } else {
            Reshaped::Refused
        }
    }

    /// Read the bytes the shell has produced and apply them to the screen,
    /// returning how many bytes were applied.
    ///
    /// A read of no new bytes applies nothing and returns `0`; it is not an
    /// error.
    ///
    /// # Errors
    ///
    /// Propagates the [`Errno`] from the underlying [`ShellSource::read`],
    /// leaving the screen unchanged.
    pub fn pump(&mut self) -> Result<usize, Errno> {
        let bytes = self.shell.read()?;
        self.parser.feed(&mut self.grid, &bytes);
        Ok(bytes.len())
    }

    /// Forward raw input `bytes` to the shell.
    ///
    /// # Errors
    ///
    /// Propagates the [`Errno`] from the underlying [`ShellSource::write`].
    pub fn send(&mut self, bytes: &[u8]) -> Result<(), Errno> {
        self.shell.write(bytes)
    }

    /// Forward `text` to the shell as UTF-8 bytes.
    ///
    /// # Errors
    ///
    /// Propagates the [`Errno`] from the underlying [`ShellSource::write`].
    pub fn send_str(&mut self, text: &str) -> Result<(), Errno> {
        self.shell.write(text.as_bytes())
    }

    /// Feed raw `bytes` directly into the screen model without touching the
    /// shell.
    ///
    /// This is the in-process half of [`pump`](Self::pump): the binary uses
    /// `pump`, while a caller that already holds the bytes (or a test) can
    /// apply them directly.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.feed(&mut self.grid, bytes);
    }

    /// Whether the shell rang the bell since this was last asked: one sound
    /// however many times it rang.
    pub fn take_bell(&mut self) -> bool {
        self.parser.take_bell()
    }

    /// Erase the screen and return the cursor home, leaving the shell
    /// untouched.
    ///
    /// The *Clear screen* command: it erases what the emulator is showing as a
    /// `clear` would — on the main screen, the rows it showed go to the
    /// scrollback — and returns the view to the live screen, without writing
    /// anything to the shell, so a half-typed command line is not disturbed
    /// and no program sees input it did not receive from the user.
    pub fn clear(&mut self) {
        self.grid.move_to(0, 0);
        self.grid.erase_in_display(EraseMode::All);
        self.grid.view_to_bottom();
    }

    /// Forget the scrollback, leaving the screen and the shell untouched.
    pub fn clear_scrollback(&mut self) {
        self.grid.clear_scrollback();
    }

    /// Scroll the view `delta` lines back into the scrollback, or toward the
    /// live screen for a negative `delta`, clamped to what it holds; returns
    /// whether the view moved.
    #[cfg(test)]
    pub fn scroll_view(&mut self, delta: i32) -> bool {
        self.grid.scroll_view(delta)
    }

    /// Return the view to the live screen; returns whether it moved.
    pub fn view_to_bottom(&mut self) -> bool {
        self.grid.view_to_bottom()
    }

    /// Move the view by the wheel's `dy` scroll units, [`WHEEL_LINES`] a
    /// detent, what a turn leaves short of a line carried: a turn forward
    /// toward the live screen, back into the scrollback otherwise. Answers
    /// whether the view moved.
    pub fn scroll_wheel(&mut self, dy: i32) -> bool {
        let lines = wheel_steps(dy, WHEEL_LINES, &mut self.wheel_carry);
        let delta = i32::try_from(lines.saturating_neg()).unwrap_or(if lines > 0 {
            i32::MIN
        } else {
            i32::MAX
        });
        lines != 0 && self.grid.scroll_view(delta)
    }

    /// Move the view for a scrolling chord — Shift with Page Up or Page Down
    /// by a screen less a line, with Home to the oldest line, with End back to
    /// the live screen — answering whether it moved, or `None` for any other
    /// key, which is the shell's. On the alternate screen, which keeps no
    /// scrollback, every key is the program's.
    pub fn view_key(&mut self, key: Key, modifiers: Modifiers) -> Option<bool> {
        let chord = modifiers.shift && !(modifiers.ctrl || modifiers.alt || modifiers.meta);
        if !chord || self.grid.on_alternate_screen() {
            return None;
        }
        let page = i32::from(self.grid.rows().saturating_sub(1).max(1));
        Some(match key {
            Key::Named(NamedKey::PageUp) => self.grid.scroll_view(page),
            Key::Named(NamedKey::PageDown) => self.grid.scroll_view(-page),
            Key::Named(NamedKey::Home) => self.grid.scroll_view(i32::MAX),
            Key::Named(NamedKey::End) => self.grid.view_to_bottom(),
            _ => return None,
        })
    }

    /// Hold the scrollback to what memory pressure at `band` leaves it, from
    /// now until the band moves again ([`Grid::shed_for`]); answers whether
    /// any of it went.
    pub fn shed_for(&mut self, band: PressureBand) -> bool {
        self.grid.shed_for(band)
    }

    /// Keep the last `lines` lines of scrollback from now on
    /// ([`Grid::set_scrollback_limit`]); answers whether any went.
    pub fn set_scrollback_limit(&mut self, lines: usize) -> bool {
        self.grid.set_scrollback_limit(lines)
    }
}
