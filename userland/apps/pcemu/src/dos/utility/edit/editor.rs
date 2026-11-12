//! The editing `EDIT` does on its document, as MS-DOS 5's does it. The
//! cursor stands at a line and a column, and may stand past a line's end,
//! where typing fills the line out with blanks. Text typed goes in, or over
//! what is there; Enter splits the line, the new one indented as the last;
//! Shift with a move selects; the window is kept over the cursor.
//!
//! A place is found from one already known — the cursor, or the window's
//! top — so the cost of an edit follows the distance it moves, never the
//! document's length.

use super::text::{in_word, line_ends, Full, Store, Text, CR, LF};
use crate::frames::record;
use alloc::vec::Vec;
use core::ops::Range;

/// The widest column the cursor reaches past a line's end.
pub const MOST_COLUMN: u16 = 255;
/// The tab stops when none are set.
pub const TAB: u8 = 8;

record! {
    /// A place in the document: its line's start and number, and its column.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Spot {
        start: u32,
        line: u32,
        column: u16,
    }
}

record! {
    /// The editor's state.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Editor {
        cursor: Spot,
        /// The column moves up and down aim at.
        goal: u16,
        /// The selection's other end, where Shift began it.
        anchor: Option<Spot>,
        /// The first line shown, and the first column.
        top: Spot,
        left: u16,
        /// The text window's rows and columns.
        rows: u16,
        columns: u16,
        tab: u8,
        insert: bool,
        modified: bool,
        /// The document's lines, kept as each change makes or takes them.
        lines: u32,
    }
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            cursor: Spot::default(),
            goal: 0,
            anchor: None,
            top: Spot::default(),
            left: 0,
            rows: 20,
            columns: 78,
            tab: TAB,
            insert: true,
            modified: false,
            lines: 1,
        }
    }
}

/// How the cursor moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Left,
    Right,
    Up,
    Down,
    /// To the line's indentation, then to its margin.
    Home,
    /// To column 1.
    LineStart,
    End,
    WordLeft,
    WordRight,
    PageUp,
    PageDown,
    /// A window's width left or right.
    PageLeft,
    PageRight,
    /// The document's first and last places.
    Top,
    Bottom,
    /// The window's first and last lines.
    WindowTop,
    WindowBottom,
    /// The next line's start.
    NextLine,
}

/// The display width of `bytes`, a line's, its tabs taken to stops `tab`
/// apart.
#[must_use]
pub fn width_of(bytes: &[u8], tab: u8) -> u16 {
    bytes
        .iter()
        .fold(0u16, |column, &byte| advance(column, byte, tab))
}

/// The column after `byte` drawn at `column`.
#[must_use]
pub fn advance(column: u16, byte: u8, tab: u8) -> u16 {
    if byte == b'\t' {
        let tab = u16::from(tab.max(1));
        (column / tab + 1).saturating_mul(tab)
    } else {
        column.saturating_add(1)
    }
}

/// What a line holds: its bytes, its CR or LF not among them.
fn line_bytes<S: Store>(text: &mut Text<'_, S>, start: u32) -> Vec<u8> {
    let end = text.line_end(start);
    text.read(start..end)
}

/// Where in `bytes` column `column` falls: the index of the byte drawn
/// there, or past the end, and the blanks to fill before it.
fn index_of(bytes: &[u8], column: u16, tab: u8) -> (usize, u16) {
    let mut at = 0u16;
    for (n, &byte) in bytes.iter().enumerate() {
        let next = advance(at, byte, tab);
        if column < next {
            return (n, 0);
        }
        at = next;
    }
    (bytes.len(), column.saturating_sub(at))
}

fn offset(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The blanks a line begins with.
fn indent_of(line: &[u8]) -> usize {
    line.iter()
        .take_while(|&&byte| byte == b' ' || byte == b'\t')
        .count()
}

/// The editing of `text` with `editor`'s state.
pub struct Edit<'e, 't, S: Store> {
    pub editor: &'e mut Editor,
    pub text: &'e mut Text<'t, S>,
}

impl<S: Store> Edit<'_, '_, S> {
    fn tab(&self) -> u8 {
        self.editor.tab
    }

    /// `bytes` put in at `at`, the lines they make counted.
    fn put(&mut self, at: u32, bytes: &[u8]) -> Result<(), Full> {
        self.text.insert(at, bytes)?;
        self.editor.lines = self.editor.lines.saturating_add(line_ends(bytes));
        self.editor.modified = true;
        Ok(())
    }

    /// Bytes `range` taken out, the lines they ended counted off.
    fn remove(&mut self, range: Range<u32>) {
        let gone = self.text.breaks_in(range.clone());
        self.text.delete(range);
        self.editor.lines = self.editor.lines.saturating_sub(gone).max(1);
        self.editor.modified = true;
    }

    /// The cursor's line.
    fn line(&mut self) -> Vec<u8> {
        line_bytes(self.text, self.editor.cursor.start)
    }

    /// Where `spot` is in the document's bytes, clamped to its line's end.
    pub fn offset_of(&mut self, spot: Spot) -> u32 {
        let bytes = line_bytes(self.text, spot.start);
        let (n, _) = index_of(&bytes, spot.column, self.tab());
        spot.start + offset(n)
    }

    /// Where the cursor is in the document's bytes.
    pub fn cursor_offset(&mut self) -> u32 {
        let cursor = self.editor.cursor;
        self.offset_of(cursor)
    }

    /// The spot at byte `at`, found from `near`, a spot whose line still
    /// starts where it says.
    pub fn spot_near(&mut self, at: u32, near: Spot) -> Spot {
        let start = self.text.line_start(at);
        let line = if start >= near.start {
            near.line
                .saturating_add(self.text.breaks_in(near.start..start))
        } else {
            near.line
                .saturating_sub(self.text.breaks_in(start..near.start))
        };
        let bytes = self.text.read(start..at);
        Spot {
            start,
            line,
            column: width_of(&bytes, self.tab()),
        }
    }

    /// The spot at byte `at`, found from the cursor.
    pub fn spot_at(&mut self, at: u32) -> Spot {
        let cursor = self.editor.cursor;
        self.spot_near(at, cursor)
    }

    /// The selection's bytes, the earlier end first; none without one.
    pub fn selection(&mut self) -> Option<Range<u32>> {
        let anchor = self.editor.anchor?;
        let (a, b) = (self.offset_of(anchor), self.offset_of(self.editor.cursor));
        (a != b).then(|| a.min(b)..a.max(b))
    }

    /// The selection's earlier end, the spot whose line no change to the
    /// selection can move.
    fn earlier(&mut self) -> Spot {
        let cursor = self.editor.cursor;
        let Some(anchor) = self.editor.anchor else {
            return cursor;
        };
        if self.offset_of(anchor) < self.offset_of(cursor) {
            anchor
        } else {
            cursor
        }
    }

    /// The cursor moved by `how`, the selection kept and extended with
    /// `select`, else given up.
    pub fn go(&mut self, how: Move, select: bool) {
        if select {
            self.editor.anchor.get_or_insert(self.editor.cursor);
        } else {
            self.editor.anchor = None;
        }
        let rows = u32::from(self.editor.rows.max(1));
        let columns = self.editor.columns.max(1);
        let cursor = self.editor.cursor;
        match how {
            Move::Left => self.set_column(cursor.column.saturating_sub(1)),
            Move::Right => {
                let reach = self.reach_of();
                self.set_column(cursor.column.saturating_add(1).min(reach));
            }
            Move::Home => {
                let line = self.line();
                let indent = width_of(&line[..indent_of(&line)], self.tab());
                self.set_column(if cursor.column == indent { 0 } else { indent });
            }
            Move::LineStart => self.set_column(0),
            Move::End => {
                let line = self.line();
                self.set_column(width_of(&line, self.tab()));
            }
            Move::Up => self.lines_up(1),
            Move::Down => self.lines_down(1),
            Move::PageUp => {
                self.lines_up(rows);
                self.scroll_up(rows);
            }
            Move::PageDown => {
                self.lines_down(rows);
                self.scroll_down(rows);
            }
            Move::PageLeft => {
                self.editor.left = self.editor.left.saturating_sub(columns);
                self.set_column(cursor.column.saturating_sub(columns));
            }
            Move::PageRight => {
                let reach = self.reach_of();
                self.editor.left = self.editor.left.saturating_add(columns).min(reach);
                self.set_column(cursor.column.saturating_add(columns).min(reach));
            }
            Move::Top => self.land_spot(Spot::default()),
            Move::Bottom => {
                let len = self.text.len();
                let last = self.spot_at(len);
                self.land_spot(last);
            }
            Move::WindowTop => {
                let up = cursor.line - self.editor.top.line.min(cursor.line);
                self.lines_up(up);
            }
            Move::WindowBottom => {
                let bottom = self.editor.top.line.saturating_add(rows - 1);
                self.lines_down(bottom.saturating_sub(cursor.line));
            }
            Move::NextLine => {
                if let Some(start) = self.text.next_line(cursor.start) {
                    self.land_spot(Spot {
                        start,
                        line: cursor.line + 1,
                        column: 0,
                    });
                }
            }
            Move::WordLeft => self.word_left(),
            Move::WordRight => self.word_right(),
        }
        self.show_cursor();
    }

    /// The widest column the cursor may stand at on its line: the line's
    /// end where it is longer than the cursor's reach past it.
    fn reach_of(&mut self) -> u16 {
        let line = self.line();
        width_of(&line, self.tab()).max(MOST_COLUMN)
    }

    fn set_column(&mut self, column: u16) {
        self.editor.cursor.column = column;
        self.editor.goal = column;
    }

    fn land_spot(&mut self, spot: Spot) {
        self.editor.cursor = spot;
        self.editor.goal = spot.column;
    }

    fn lines_up(&mut self, n: u32) {
        for _ in 0..n {
            let Some(start) = self.text.previous_line(self.editor.cursor.start) else {
                break;
            };
            self.editor.cursor.start = start;
            self.editor.cursor.line -= 1;
        }
        self.editor.cursor.column = self.editor.goal;
    }

    fn lines_down(&mut self, n: u32) {
        for _ in 0..n {
            let Some(start) = self.text.next_line(self.editor.cursor.start) else {
                break;
            };
            self.editor.cursor.start = start;
            self.editor.cursor.line += 1;
        }
        self.editor.cursor.column = self.editor.goal;
    }

    /// The window scrolled `n` lines up.
    pub fn scroll_up(&mut self, n: u32) {
        for _ in 0..n {
            let Some(start) = self.text.previous_line(self.editor.top.start) else {
                break;
            };
            self.editor.top = Spot {
                start,
                line: self.editor.top.line - 1,
                column: 0,
            };
        }
    }

    /// The window scrolled `n` lines down.
    pub fn scroll_down(&mut self, n: u32) {
        for _ in 0..n {
            let Some(start) = self.text.next_line(self.editor.top.start) else {
                break;
            };
            self.editor.top = Spot {
                start,
                line: self.editor.top.line + 1,
                column: 0,
            };
        }
    }

    /// The window scrolled a line, `down` or up, the cursor kept on its
    /// line while that is in view and moved into it where it is not.
    pub fn scroll_line(&mut self, down: bool) {
        self.editor.anchor = None;
        let rows = u32::from(self.editor.rows.max(1));
        if down {
            self.scroll_down(1);
            if self.editor.cursor.line < self.editor.top.line {
                self.lines_down(1);
            }
        } else {
            self.scroll_up(1);
            if self.editor.cursor.line >= self.editor.top.line + rows {
                self.lines_up(1);
            }
        }
    }

    /// The window moved the least that shows the cursor.
    pub fn show_cursor(&mut self) {
        let (cursor, rows) = (self.editor.cursor, u32::from(self.editor.rows.max(1)));
        if cursor.line < self.editor.top.line {
            self.editor.top = Spot {
                column: 0,
                ..cursor
            };
        } else if cursor.line >= self.editor.top.line + rows {
            let mut top = Spot {
                column: 0,
                ..cursor
            };
            for _ in 0..rows - 1 {
                match self.text.previous_line(top.start) {
                    Some(start) => {
                        top = Spot {
                            start,
                            line: top.line - 1,
                            column: 0,
                        }
                    }
                    None => break,
                }
            }
            self.editor.top = top;
        }
        let columns = self.editor.columns.max(1);
        if cursor.column < self.editor.left {
            self.editor.left = cursor.column;
        } else if cursor.column >= self.editor.left + columns {
            self.editor.left = cursor.column + 1 - columns;
        }
    }

    /// The cursor put on line `line` at column `column`, or as near as the
    /// document reaches, found by walking from the line it is on.
    pub fn go_to(&mut self, line: u32, column: u16) {
        self.editor.anchor = None;
        let cursor = self.editor.cursor;
        if line > cursor.line {
            self.lines_down(line - cursor.line);
        } else {
            self.lines_up(cursor.line - line);
        }
        self.set_column(column);
        self.show_cursor();
    }

    /// The previous word's start, across lines.
    fn word_left(&mut self) {
        let mut at = self.offset_of(self.editor.cursor);
        let word = |byte: Option<u8>| byte.is_some_and(in_word);
        while at > 0 && !word(self.text.byte(at - 1)) {
            at -= 1;
        }
        while at > 0 && word(self.text.byte(at - 1)) {
            at -= 1;
        }
        let spot = self.spot_at(at);
        self.land_spot(spot);
    }

    /// The next word's start, across lines.
    fn word_right(&mut self) {
        let len = self.text.len();
        let mut at = self.offset_of(self.editor.cursor);
        let word = |byte: Option<u8>| byte.is_some_and(in_word);
        while at < len && word(self.text.byte(at)) {
            at += 1;
        }
        while at < len && !word(self.text.byte(at)) {
            at += 1;
        }
        let spot = self.spot_at(at);
        self.land_spot(spot);
    }

    /// The word the cursor stands in selected, the cursor at its end;
    /// nothing where it stands in none.
    pub fn select_word(&mut self) {
        let at = self.offset_of(self.editor.cursor);
        let word = |byte: Option<u8>| byte.is_some_and(in_word);
        let (mut start, mut end) = (at, at);
        while start > 0 && word(self.text.byte(start - 1)) {
            start -= 1;
        }
        while word(self.text.byte(end)) {
            end += 1;
        }
        if start == end {
            return;
        }
        let first = self.spot_at(start);
        let last = self.spot_at(end);
        self.editor.anchor = Some(first);
        self.land_spot(last);
        self.show_cursor();
    }

    /// The cursor's line filled out with blanks to the cursor; where the
    /// cursor then stands in the bytes.
    fn reach(&mut self) -> Result<u32, Full> {
        let line = self.line();
        let (n, blanks) = index_of(&line, self.editor.cursor.column, self.tab());
        let at = self.editor.cursor.start + offset(n);
        if blanks > 0 {
            self.put(at, &alloc::vec![b' '; usize::from(blanks)])?;
            return Ok(at + u32::from(blanks));
        }
        Ok(at)
    }

    /// The selection taken out, the cursor at its start: whether there was
    /// one.
    pub fn clear(&mut self) -> bool {
        let Some(range) = self.selection() else {
            self.editor.anchor = None;
            return false;
        };
        let earlier = self.earlier();
        self.remove(range.clone());
        self.editor.anchor = None;
        let bytes = self.text.read(earlier.start..range.start);
        self.land_spot(Spot {
            column: width_of(&bytes, self.tab()),
            ..earlier
        });
        self.show_cursor();
        true
    }

    /// `bytes` typed at the cursor, in place of the selection, over the
    /// text there when overstriking.
    ///
    /// # Errors
    ///
    /// [`Full`] where the document has no room, the selection gone.
    pub fn type_in(&mut self, bytes: &[u8]) -> Result<(), Full> {
        let replaced = self.clear();
        let at = self.reach()?;
        if !self.editor.insert && !replaced {
            let end = self.text.line_end(at);
            let over = end.min(at + offset(bytes.len()));
            self.remove(at..over);
        }
        self.put(at, bytes)?;
        self.land(at + offset(bytes.len()));
        Ok(())
    }

    /// The cursor put at byte `at`, at or past its line's start, the window
    /// following it.
    fn land(&mut self, at: u32) {
        let spot = self.spot_at(at);
        self.land_spot(spot);
        self.show_cursor();
    }

    /// Enter: the line split at the cursor, the new line indented as the
    /// one it came from.
    ///
    /// # Errors
    ///
    /// [`Full`] where the document has no room.
    pub fn enter(&mut self) -> Result<(), Full> {
        self.clear();
        let line = self.line();
        let (n, _) = index_of(&line, self.editor.cursor.column, self.tab());
        let at = self.editor.cursor.start + offset(n.min(line.len()));
        let mut bytes = alloc::vec![CR, LF];
        bytes.extend_from_slice(&line[..indent_of(&line).min(n)]);
        self.put(at, &bytes)?;
        self.land(at + offset(bytes.len()));
        Ok(())
    }

    /// Ctrl+N: the line split at the cursor, which stays where it was.
    ///
    /// # Errors
    ///
    /// [`Full`] where the document has no room.
    pub fn split(&mut self) -> Result<(), Full> {
        self.clear();
        let line = self.line();
        let (n, _) = index_of(&line, self.editor.cursor.column, self.tab());
        let at = self.editor.cursor.start + offset(n);
        self.put(at, &[CR, LF])
    }

    /// Backspace: the character before the cursor taken out, or, at a
    /// line's start, the line joined to the one before.
    pub fn back(&mut self) {
        if self.clear() {
            return;
        }
        let cursor = self.editor.cursor;
        if cursor.column > 0 {
            let line = self.line();
            if cursor.column > width_of(&line, self.tab()) {
                self.go(Move::Left, false);
                return;
            }
            let (n, _) = index_of(&line, cursor.column - 1, self.tab());
            let at = cursor.start + offset(n);
            self.remove(at..at + 1);
            self.land(at);
            return;
        }
        if cursor.start == 0 {
            return;
        }
        let end = self.text.line_end(cursor.start - 1);
        self.remove(end..cursor.start);
        let start = self.text.line_start(end);
        let bytes = self.text.read(start..end);
        self.land_spot(Spot {
            start,
            line: cursor.line.saturating_sub(1),
            column: width_of(&bytes, self.tab()),
        });
        self.show_cursor();
    }

    /// Del: the selection, or the character at the cursor, taken out; at
    /// or past a line's end, the next line joined to it.
    ///
    /// # Errors
    ///
    /// [`Full`] where the line must be filled out to the cursor first and
    /// the document has no room.
    pub fn delete(&mut self) -> Result<(), Full> {
        if self.clear() {
            return Ok(());
        }
        let line = self.line();
        let (n, _) = index_of(&line, self.editor.cursor.column, self.tab());
        if n < line.len() {
            let at = self.editor.cursor.start + offset(n);
            self.remove(at..at + 1);
            return Ok(());
        }
        if self.text.next_line(self.editor.cursor.start).is_none() {
            return Ok(());
        }
        let at = self.reach()?;
        let end = self.text.line_end(at);
        let next = self.text.next_line(at).unwrap_or(end);
        self.remove(end..next);
        Ok(())
    }

    /// The cursor's line taken out, its bytes and its end given back.
    pub fn cut_line(&mut self) -> Vec<u8> {
        self.editor.anchor = None;
        let start = self.editor.cursor.start;
        let end = self
            .text
            .next_line(start)
            .unwrap_or_else(|| self.text.len());
        let cut = self.text.read(start..end);
        self.remove(start..end);
        self.show_cursor();
        cut
    }

    /// The cursor's line from the cursor to its end taken out and given
    /// back.
    pub fn cut_to_end(&mut self) -> Vec<u8> {
        self.editor.anchor = None;
        let at = self.offset_of(self.editor.cursor);
        let end = self.text.line_end(at);
        let cut = self.text.read(at..end);
        self.remove(at..end);
        cut
    }

    /// The selection's bytes, taken out with `cut`; none without one.
    pub fn copy(&mut self, cut: bool) -> Option<Vec<u8>> {
        let range = self.selection()?;
        let bytes = self.text.read(range);
        if cut {
            self.clear();
        }
        Some(bytes)
    }

    /// The lines the selection touches: its first line's start, and the
    /// line after its last, a selection ending at a line's start not
    /// reaching into that line.
    fn selected_lines(&mut self) -> Option<(Spot, u32)> {
        let anchor = self.editor.anchor?;
        let cursor = self.editor.cursor;
        let (first, last) = if (anchor.line, anchor.column) <= (cursor.line, cursor.column) {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        let lines = last.line - first.line + u32::from(last.column > 0 || last.line == first.line);
        Some((Spot { column: 0, ..first }, lines))
    }

    /// A selection end held as the bytes it stands at, so it can be put
    /// back once blanks are put in or taken out before it on its line.
    fn hold(&mut self, spot: Spot) -> Held {
        let bytes = line_bytes(self.text, spot.start);
        let (index, past) = index_of(&bytes, spot.column, self.tab());
        Held {
            line: spot.line,
            index,
            past,
        }
    }

    /// `held` put back at byte `index` of its line, whose start is found
    /// from `first`, a spot on an earlier line or its own that no change
    /// moved.
    fn put_back(&mut self, first: Spot, held: Held, index: usize) -> Spot {
        let mut start = first.start;
        for _ in first.line..held.line {
            match self.text.next_line(start) {
                Some(next) => start = next,
                None => break,
            }
        }
        let bytes = line_bytes(self.text, start);
        let index = index.min(bytes.len());
        let column = width_of(&bytes[..index], self.tab()).saturating_add(held.past);
        Spot {
            start,
            line: held.line,
            column,
        }
    }

    /// The cursor and anchor put back, each at the index `moved` gives its
    /// held place.
    fn put_ends_back(
        &mut self,
        first: Spot,
        ends: [Option<Held>; 2],
        moved: impl Fn(Held) -> usize,
    ) {
        if let Some(held) = ends[0] {
            self.editor.cursor = self.put_back(first, held, moved(held));
            self.editor.goal = self.editor.cursor.column;
        }
        let anchor = ends[1].map(|held| self.put_back(first, held, moved(held)));
        self.editor.anchor = anchor;
    }

    fn held_ends(&mut self) -> [Option<Held>; 2] {
        let cursor = self.editor.cursor;
        let anchor = self.editor.anchor;
        [
            Some(self.hold(cursor)),
            anchor.map(|anchor| self.hold(anchor)),
        ]
    }

    /// Tab with a selection: each line it touches indented by a tab, the
    /// selection kept over the same text.
    ///
    /// # Errors
    ///
    /// [`Full`] where the document has no room, the lines before it
    /// indented.
    pub fn indent(&mut self) -> Result<(), Full> {
        let Some((first, lines)) = self.selected_lines() else {
            return Ok(());
        };
        let ends = self.held_ends();
        let mut start = first.start;
        let mut indented = 0;
        let mut result = Ok(());
        while indented < lines {
            if let Err(full) = self.put(start, b"\t") {
                result = Err(full);
                break;
            }
            indented += 1;
            let Some(next) = self.text.next_line(start) else {
                break;
            };
            start = next;
        }
        let reached = first.line + indented;
        self.put_ends_back(first, ends, |held| {
            if held.line < reached && held.index > 0 {
                held.index + 1
            } else {
                held.index
            }
        });
        result
    }

    /// Shift+Tab: each line the selection touches, or the cursor's line,
    /// outdented by its leading tab or a tab stop's blanks.
    pub fn outdent(&mut self) {
        let (first, lines) = self.selected_lines().unwrap_or((
            Spot {
                column: 0,
                ..self.editor.cursor
            },
            1,
        ));
        let ends = self.held_ends();
        let tab = usize::from(self.tab().max(1));
        let mut gone_on = [(u32::MAX, 0usize); 2];
        let mut start = first.start;
        for n in 0..lines {
            let line = line_bytes(self.text, start);
            let gone = if line.first() == Some(&b'\t') {
                1
            } else {
                line.iter()
                    .take(tab)
                    .take_while(|&&byte| byte == b' ')
                    .count()
            };
            if gone > 0 {
                self.remove(start..start + offset(gone));
            }
            for (slot, held) in gone_on.iter_mut().zip(ends) {
                if held.is_some_and(|held| held.line == first.line + n) {
                    *slot = (first.line + n, gone);
                }
            }
            match self.text.next_line(start) {
                Some(next) => start = next,
                None => break,
            }
        }
        self.put_ends_back(first, ends, |held| {
            let gone = gone_on
                .iter()
                .find(|&&(line, _)| line == held.line)
                .map_or(0, |&(_, gone)| gone);
            held.index - held.index.min(gone)
        });
    }
}

/// A selection end as [`Edit::hold`] keeps it: its line, the index of the
/// byte it stands at, and the blanks it stands past the line's end.
#[derive(Clone, Copy, Debug)]
struct Held {
    line: u32,
    index: usize,
    past: u16,
}

#[cfg(test)]
#[path = "editor_tests.rs"]
mod tests;
