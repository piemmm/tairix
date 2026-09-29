//! One document as a window edits it: its bytes, history and selection, how
//! it is shown, and every command a key or a menu runs on it.
//!
//! Nothing here reads a file, draws a pixel, or lexes a byte. A command
//! changes the document and reports which lines it touched, so the view
//! repaints those and no more.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::{ControlFlow, Range};

use tairix_reclaim::PressureBand;
use tairix_syntax::{Diagnostic, Format};

use crate::detect::{indentation, line_ending, looks_binary, Indent, LineEnding};
use crate::document::{Document, OutOfMemory, Snapshot};
use crate::hex::{self, HexCaret, Nibble, Pane, BYTES_PER_ROW};
use crate::highlight::{Highlight, LineEdit};
use crate::history::{History, Kind, Restored};
use crate::selection::Selection;
use crate::text::{self, Glyph};

/// Most bytes one edit made on the loop copies: a bound on the work a single
/// keystroke, paste or line command may cost there.
pub const MAX_EDIT_BYTES: usize = 16 * 1024 * 1024;

/// Undo steps kept when memory pressure asks the history to give way.
pub const PRESSURE_UNDO_STEPS: usize = 64;

/// Longest indentation a new line copies from the one it breaks.
const MAX_AUTO_INDENT: usize = 256;

/// How the document is shown.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Mode {
    /// Characters in a grid, control bytes as tokens.
    #[default]
    Text,
    /// Bytes as hex digits and ASCII.
    Hex,
}

/// Where a motion takes the caret.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Motion {
    /// One stop back.
    Left,
    /// One stop on.
    Right,
    /// One row up.
    Up,
    /// One row down.
    Down,
    /// Back to the start of a word.
    WordLeft,
    /// On to the end of a word.
    WordRight,
    /// To the row's indentation, or its start when already there.
    LineStart,
    /// To the row's end.
    LineEnd,
    /// A page of rows up.
    PageUp,
    /// A page of rows down.
    PageDown,
    /// To the document's start.
    DocumentStart,
    /// To the document's end.
    DocumentEnd,
}

/// What a window can be asked to do to its document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Move the caret, extending the selection when `extend`.
    Move {
        /// Where to.
        motion: Motion,
        /// Whether the selection's anchor stays.
        extend: bool,
    },
    /// Type a character.
    Type(char),
    /// Break the line, keeping its indentation.
    Newline,
    /// Indent: the selected lines, or one level at the caret.
    Tab,
    /// Outdent the selected lines.
    Backtab,
    /// Delete the selection, or the stop before the caret.
    Backspace,
    /// Delete the selection, or the stop after the caret.
    Delete,
    /// Delete back to the start of a word.
    DeleteWordLeft,
    /// Delete on to the end of a word.
    DeleteWordRight,
    /// Select the whole document.
    SelectAll,
    /// Undo the newest step.
    Undo,
    /// Redo the newest undone step.
    Redo,
    /// Comment the selected lines out, or back in.
    ToggleComment,
    /// Indent the selected lines by one level.
    Indent,
    /// Outdent the selected lines by one level.
    Outdent,
    /// Switch between inserting and overwriting.
    ToggleOverwrite,
    /// Put the caret at the start of a 1-based line.
    GoToLine(usize),
    /// Put the caret on the next line a diagnostic names.
    NextProblem,
    /// Show the document as text or as hex.
    SetMode(Mode),
    /// Colour and check the document as a format.
    SetFormat(Format),
    /// Indent with this.
    SetIndent(Indent),
    /// Put tab stops this many columns apart.
    SetTabWidth(u8),
}

/// Why a command did nothing.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// The allocator refused the room; the document is as it was.
    OutOfMemory,
    /// The edit is past what one edit may copy.
    TooLarge,
    /// The format has no comment that runs to the end of a line.
    NoLineComment,
    /// The document changed since the work was asked for.
    Changed,
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::OutOfMemory => "there is not enough memory for that",
            Self::TooLarge => "that is too much text to change at once",
            Self::NoLineComment => "this format has no line comment",
            Self::Changed => "the document changed; try again",
        })
    }
}

/// Lines a change touched: `first` through `last`, or everything from
/// `first` on when lines moved.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Lines {
    /// The first line touched.
    pub first: usize,
    /// The last line touched, or `None` when every line after `first` moved.
    pub last: Option<usize>,
}

impl Lines {
    fn of(edit: LineEdit) -> Self {
        Self {
            first: edit.line,
            last: (edit.removed_lines == edit.inserted_lines)
                .then_some(edit.line + edit.inserted_lines),
        }
    }

    fn union(self, other: Self) -> Self {
        Self {
            first: self.first.min(other.first),
            last: self.last.zip(other.last).map(|(a, b)| a.max(b)),
        }
    }
}

/// What running a command changed.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Effect {
    /// The lines whose text changed.
    pub text: Option<Lines>,
    /// The selection or caret moved.
    pub selection: bool,
    /// How the document is shown changed: the whole view reflows.
    pub view: bool,
    /// What could not be done, and why.
    pub refused: Option<Refusal>,
}

impl Effect {
    const fn moved(selection: bool) -> Self {
        Self {
            text: None,
            selection,
            view: false,
            refused: None,
        }
    }

    const fn reshown() -> Self {
        Self {
            text: None,
            selection: true,
            view: true,
            refused: None,
        }
    }

    const fn refused(why: Refusal) -> Self {
        Self {
            text: None,
            selection: false,
            view: false,
            refused: Some(why),
        }
    }
}

/// One document and the state of the window editing it.
#[derive(Debug)]
pub struct Editor {
    document: Document,
    history: History,
    selection: Selection,
    pane: Pane,
    nibble: Nibble,
    mode: Mode,
    /// Typing replaces the character under the caret in the text view.
    text_overwrite: bool,
    /// Typing puts new bytes in, rather than overwriting, in the hex view.
    hex_insert: bool,
    /// The column vertical motion aims for, kept across consecutive moves.
    goal: Option<usize>,
    eol: LineEnding,
    indent: Indent,
    tab: u8,
    highlight: Highlight,
    /// Bumped by every change, so an answer about an older document is known.
    generation: u64,
    diagnostics: Vec<Diagnostic>,
    /// The generation the diagnostics describe.
    checked: Option<u64>,
    /// The document frozen at the current generation, handed out again
    /// rather than frozen afresh while nothing changes.
    frozen: Option<Arc<Snapshot>>,
    /// The memory-pressure band the history was last sized for.
    pressure: PressureBand,
}

impl Editor {
    /// Edit `document`, shown as its head suggests and coloured as
    /// `format`.
    #[must_use]
    pub fn new(document: Document, format: Format) -> Self {
        let mode = if looks_binary(&document) {
            Mode::Hex
        } else {
            Mode::Text
        };
        Self {
            eol: line_ending(&document),
            indent: indentation(&document),
            document,
            history: History::new(),
            selection: Selection::default(),
            pane: Pane::Hex,
            nibble: Nibble::High,
            mode,
            text_overwrite: false,
            hex_insert: false,
            goal: None,
            tab: 8,
            highlight: Highlight::new(format),
            generation: 0,
            diagnostics: Vec::new(),
            checked: None,
            frozen: None,
            pressure: PressureBand::Normal,
        }
    }

    /// The document.
    #[must_use]
    pub const fn document(&self) -> &Document {
        &self.document
    }

    /// The selection.
    #[must_use]
    pub const fn selection(&self) -> Selection {
        self.selection
    }

    /// Where the caret is in the hex view.
    #[must_use]
    pub const fn hex_caret(&self) -> HexCaret {
        HexCaret {
            offset: self.selection.head,
            pane: self.pane,
            nibble: self.nibble,
        }
    }

    /// How the document is shown.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// Whether typing overwrites in the current view.
    #[must_use]
    pub const fn overwrite(&self) -> bool {
        match self.mode {
            Mode::Text => self.text_overwrite,
            Mode::Hex => !self.hex_insert,
        }
    }

    /// The document's line-ending convention.
    #[must_use]
    pub const fn line_ending(&self) -> LineEnding {
        self.eol
    }

    /// What one level of indentation is.
    #[must_use]
    pub const fn indent(&self) -> Indent {
        self.indent
    }

    /// Columns between tab stops.
    #[must_use]
    pub const fn tab_width(&self) -> usize {
        self.tab as usize
    }

    /// The colouring.
    #[must_use]
    pub const fn highlight(&self) -> &Highlight {
        &self.highlight
    }

    /// The colouring, for the loop to feed.
    pub fn highlight_mut(&mut self) -> (&mut Highlight, &Document) {
        (&mut self.highlight, &self.document)
    }

    /// The format the document is coloured and checked as.
    #[must_use]
    pub const fn format(&self) -> Format {
        self.highlight.format()
    }

    /// What the system's own parser says about the document, and whether
    /// that is about its current text.
    #[must_use]
    pub fn diagnostics(&self) -> (&[Diagnostic], bool) {
        (&self.diagnostics, self.checked == Some(self.generation))
    }

    /// Which change the document is at.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether the document differs from its file.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.history.is_modified()
    }

    /// Whether there is a step to undo, and one to redo.
    #[must_use]
    pub fn can_undo_redo(&self) -> (bool, bool) {
        (self.history.can_undo(), self.history.can_redo())
    }

    /// Run `command`, with a page of `page_rows` rows for paging.
    pub fn run(&mut self, command: &Command, page_rows: usize) -> Effect {
        if !matches!(
            command,
            Command::Move {
                motion: Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown,
                ..
            }
        ) {
            self.goal = None;
        }
        match *command {
            Command::Move { motion, extend } => self.travel(motion, extend, page_rows.max(1)),
            Command::Type(ch) => self.type_char(ch),
            Command::Newline => self.newline(),
            Command::Tab => self.tab(),
            Command::Backtab | Command::Outdent => self.line_command(Line::Outdent),
            Command::Indent => self.line_command(Line::Indent),
            Command::ToggleComment => self.line_command(Line::Comment),
            Command::Backspace => self.delete(false, false),
            Command::Delete => self.delete(true, false),
            Command::DeleteWordLeft => self.delete(false, true),
            Command::DeleteWordRight => self.delete(true, true),
            Command::SelectAll => {
                let all = Selection {
                    anchor: 0,
                    head: self.document.len(),
                };
                self.select(all)
            }
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::ToggleOverwrite => {
                match self.mode {
                    Mode::Text => self.text_overwrite = !self.text_overwrite,
                    Mode::Hex => self.hex_insert = !self.hex_insert,
                }
                Effect::moved(true)
            }
            Command::GoToLine(line) => {
                let line = line.saturating_sub(1).min(self.document.line_count() - 1);
                self.select(Selection::caret(self.document.line_start(line)))
            }
            Command::NextProblem => self.next_problem(),
            Command::SetMode(mode) => self.set_mode(mode),
            Command::SetFormat(format) => {
                if format == self.format() {
                    return Effect::default();
                }
                self.highlight.set_format(format);
                self.diagnostics.clear();
                self.checked = None;
                Effect::reshown()
            }
            Command::SetIndent(indent) => {
                self.indent = match indent {
                    Indent::Spaces(width) => Indent::Spaces(width.clamp(1, MAX_INDENT_SPACES)),
                    Indent::Tab => Indent::Tab,
                };
                Effect::moved(false)
            }
            Command::SetTabWidth(width) => {
                let width = width.clamp(1, 16);
                if width == self.tab {
                    return Effect::default();
                }
                self.tab = width;
                Effect::reshown()
            }
        }
    }

    fn select(&mut self, selection: Selection) -> Effect {
        let selection = selection.clamped(self.document.len());
        let moved = selection != self.selection;
        self.selection = selection;
        self.history.close_run();
        Effect::moved(moved)
    }

    /// `selection` with both ends on caret stops: in the text view, never
    /// inside a character.
    fn on_stops(&self, selection: Selection) -> Selection {
        if self.mode != Mode::Text {
            return selection;
        }
        Selection {
            anchor: text::unit_start(&self.document, selection.anchor),
            head: text::unit_start(&self.document, selection.head),
        }
    }

    fn set_mode(&mut self, mode: Mode) -> Effect {
        if mode == self.mode {
            return Effect::default();
        }
        self.mode = mode;
        self.selection = self.on_stops(self.selection);
        self.pane = Pane::Hex;
        self.nibble = Nibble::High;
        Effect::reshown()
    }

    /// Replace `range` with `text` as one step of kind `kind`, leaving
    /// `after` selected.
    fn edit(&mut self, range: Range<usize>, text: &[u8], kind: Kind, after: Selection) -> Effect {
        if text.len() > MAX_EDIT_BYTES {
            return Effect::refused(Refusal::TooLarge);
        }
        let len = self.document.len();
        let range = range.start.min(len)..range.end.clamp(range.start.min(len), len);
        if range.is_empty() && text.is_empty() {
            return Effect::default();
        }
        let before = self.selection;
        let removal = LineEdit::removing(&self.document, range.clone());
        let Ok(change) = self.document.replace(range, text) else {
            return Effect::refused(Refusal::OutOfMemory);
        };
        let edit = removal.inserted(&self.document, text.len());
        self.highlight.edited(edit);
        self.history.record(change, before, after, kind);
        self.changed(after);
        Effect {
            text: Some(Lines::of(edit)),
            selection: true,
            view: false,
            refused: None,
        }
    }

    fn changed(&mut self, after: Selection) {
        self.generation += 1;
        self.frozen = None;
        self.selection = after.clamped(self.document.len());
        self.nibble = Nibble::High;
    }

    fn travel(&mut self, motion: Motion, extend: bool, page: usize) -> Effect {
        let before = self.selection;
        let collapse = !extend && !before.is_empty();
        let to = match self.mode {
            Mode::Text => self.text_target(motion, collapse, page),
            Mode::Hex => self.hex_target(motion, extend, collapse, page),
        };
        self.history.close_run();
        self.selection = before.moved(to, extend);
        Effect::moved(self.selection != before)
    }

    fn text_target(&mut self, motion: Motion, collapse: bool, page: usize) -> usize {
        let doc = &self.document;
        let caret = self.selection.head;
        let range = self.selection.range();
        match motion {
            Motion::Left if collapse => range.start,
            Motion::Right if collapse => range.end,
            Motion::Left => text::prev_stop(doc, caret),
            Motion::Right => text::next_stop(doc, caret),
            Motion::WordLeft => text::word_start(doc, caret),
            Motion::WordRight => text::word_end(doc, caret),
            Motion::Up => self.vertical(false, 1),
            Motion::Down => self.vertical(true, 1),
            Motion::PageUp => self.vertical(false, page),
            Motion::PageDown => self.vertical(true, page),
            Motion::LineStart => {
                let row = text::row_of(doc, caret);
                let bounds = text::row_bounds(doc, row);
                let indent = if row.part == 0 {
                    indentation_end(doc, bounds)
                } else {
                    bounds.start
                };
                if caret == indent {
                    bounds.start
                } else {
                    indent
                }
            }
            Motion::LineEnd => text::row_end(doc, text::row_bounds(doc, text::row_of(doc, caret))),
            Motion::DocumentStart => 0,
            Motion::DocumentEnd => doc.len(),
        }
    }

    /// Where `rows` rows up or down from the caret lands, at the goal
    /// column; the document's end when there are no more rows.
    fn vertical(&mut self, down: bool, rows: usize) -> usize {
        let doc = &self.document;
        let tab = self.tab_width();
        let caret = self.selection.head;
        let mut row = text::row_of(doc, caret);
        let goal = *self
            .goal
            .get_or_insert_with(|| text::column_of(doc, text::row_bounds(doc, row), caret, tab));
        let mut moved = 0;
        while moved < rows {
            let next = if down {
                text::next_row(doc, row)
            } else {
                text::prev_row(doc, row)
            };
            let Some(next) = next else { break };
            row = next;
            moved += 1;
        }
        if moved == 0 {
            return if down { doc.len() } else { 0 };
        }
        text::offset_at(doc, text::row_bounds(doc, row), goal * 2, tab)
    }

    fn hex_target(&mut self, motion: Motion, extend: bool, collapse: bool, page: usize) -> usize {
        let len = self.document.len();
        let caret = self.hex_caret();
        let range = self.selection.range();
        let row_start = caret.offset - caret.offset % BYTES_PER_ROW;
        let next = match motion {
            Motion::Left if collapse => at_byte(caret, range.start),
            Motion::Right if collapse => at_byte(caret, range.end),
            // A selection is of whole bytes, so extending steps by bytes.
            Motion::Left | Motion::WordLeft if extend => {
                at_byte(caret, caret.offset.saturating_sub(1))
            }
            Motion::Right | Motion::WordRight if extend => {
                at_byte(caret, (caret.offset + 1).min(len))
            }
            Motion::Left => hex::step_left(caret),
            Motion::Right => hex::step_right(caret, len),
            Motion::WordLeft => at_byte(caret, caret.offset.saturating_sub(1)),
            Motion::WordRight => at_byte(caret, (caret.offset + 1).min(len)),
            Motion::Up => at_byte(caret, caret.offset.saturating_sub(BYTES_PER_ROW)),
            Motion::Down => at_byte(caret, (caret.offset + BYTES_PER_ROW).min(len)),
            Motion::PageUp => at_byte(caret, caret.offset.saturating_sub(BYTES_PER_ROW * page)),
            Motion::PageDown => at_byte(
                caret,
                caret.offset.saturating_add(BYTES_PER_ROW * page).min(len),
            ),
            Motion::LineStart => at_byte(caret, row_start),
            Motion::LineEnd => at_byte(caret, (row_start + BYTES_PER_ROW - 1).min(len)),
            Motion::DocumentStart => at_byte(caret, 0),
            Motion::DocumentEnd => at_byte(caret, len),
        };
        self.pane = next.pane;
        self.nibble = if next.offset == len {
            Nibble::High
        } else {
            next.nibble
        };
        next.offset
    }

    /// Put the hex caret in `pane` on a pane-relative stop: switching panes.
    pub fn set_pane(&mut self, pane: Pane) -> Effect {
        if pane == self.pane {
            return Effect::default();
        }
        self.pane = pane;
        self.nibble = Nibble::High;
        Effect::moved(true)
    }

    /// Put the caret where a click landed, extending the selection when
    /// `extend`.
    pub fn click(&mut self, at: usize, extend: bool) -> Effect {
        self.goal = None;
        let before = self.selection;
        let at = if self.mode == Mode::Text {
            text::unit_start(&self.document, at)
        } else {
            at
        };
        self.history.close_run();
        self.selection = before.moved(at.min(self.document.len()), extend);
        self.nibble = Nibble::High;
        Effect::moved(self.selection != before)
    }

    /// Select a match a search found: exactly its bytes in the hex view, and
    /// in the text view every character it touches, so a match inside a
    /// character still leaves a selection the next search starts past.
    pub fn select_match(&mut self, range: Range<usize>) -> Effect {
        let len = self.document.len();
        let (start, end) = (range.start.min(len), range.end.min(len));
        let selection = if self.mode == Mode::Text && start < end {
            let last = text::unit_start(&self.document, end - 1);
            Selection {
                anchor: text::unit_start(&self.document, start),
                head: text::next_stop(&self.document, last),
            }
        } else {
            Selection {
                anchor: start,
                head: end,
            }
        };
        self.goal = None;
        self.nibble = Nibble::High;
        self.select(selection)
    }

    /// Put the hex caret where a click landed.
    pub fn click_hex(&mut self, caret: HexCaret, extend: bool) -> Effect {
        let mut effect = self.click(caret.offset, extend);
        effect.selection |= self.pane != caret.pane || self.nibble != caret.nibble;
        self.pane = caret.pane;
        self.nibble = caret.nibble;
        effect
    }

    fn type_char(&mut self, ch: char) -> Effect {
        match self.mode {
            Mode::Text if !ch.is_control() => {
                let mut buf = [0u8; 4];
                let bytes = ch.encode_utf8(&mut buf).as_bytes();
                let range = self.typing_range();
                let kind = if self.selection.is_empty() {
                    Kind::Typing
                } else {
                    Kind::Other
                };
                let after = Selection::caret(range.start + bytes.len());
                self.edit(range, bytes, kind, after)
            }
            Mode::Hex => self.type_hex(ch),
            Mode::Text => Effect::default(),
        }
    }

    /// What typing replaces: the selection, or in overwrite mode the stop
    /// under the caret unless it ends the line.
    fn typing_range(&self) -> Range<usize> {
        let caret = self.selection.head;
        if !self.selection.is_empty() {
            return self.selection.range();
        }
        if self.text_overwrite {
            if let Some((Some(_), len)) = text::unit_at(&self.document, caret) {
                return caret..caret + len;
            }
        }
        caret..caret
    }

    fn type_hex(&mut self, ch: char) -> Effect {
        let caret = self.hex_caret();
        if !self.selection.is_empty() {
            // Typing over a selection replaces it with what was typed.
            let range = self.selection.range();
            let caret = HexCaret {
                offset: range.start,
                nibble: Nibble::High,
                ..caret
            };
            let Some(edit) = hex_edit(caret, None, ch, true) else {
                return Effect::default();
            };
            return self.apply_hex(range, edit.byte, edit.caret, Kind::Other);
        }
        let current = self.document.byte(caret.offset);
        let Some(edit) = hex_edit(caret, current, ch, self.hex_insert) else {
            return Effect::default();
        };
        self.apply_hex(edit.range, edit.byte, edit.caret, Kind::Typing)
    }

    fn apply_hex(&mut self, range: Range<usize>, byte: u8, caret: HexCaret, kind: Kind) -> Effect {
        let effect = self.edit(range, &[byte], kind, Selection::caret(caret.offset));
        if effect.refused.is_none() {
            self.pane = caret.pane;
            self.nibble = caret.nibble;
        }
        effect
    }

    fn newline(&mut self) -> Effect {
        if self.mode == Mode::Hex {
            return Effect::default();
        }
        let range = self.selection.range();
        let doc = &self.document;
        let line = doc.line_of(range.start);
        let first_row = text::row_bounds(doc, text::Row { line, part: 0 });
        let indent_end = indentation_end(doc, first_row)
            .min(range.start)
            .min(first_row.start + MAX_AUTO_INDENT);
        let mut text = Vec::new();
        if text
            .try_reserve(self.eol.bytes().len() + indent_end.saturating_sub(first_row.start))
            .is_err()
        {
            return Effect::refused(Refusal::OutOfMemory);
        }
        text.extend_from_slice(self.eol.bytes());
        if indent_end > first_row.start
            && doc
                .copy_range(first_row.start..indent_end, &mut text)
                .is_err()
        {
            return Effect::refused(Refusal::OutOfMemory);
        }
        let after = Selection::caret(range.start + text.len());
        self.edit(range, &text, Kind::Other, after)
    }

    fn tab(&mut self) -> Effect {
        if self.mode == Mode::Hex {
            return Effect::default();
        }
        let range = self.selection.range();
        let doc = &self.document;
        if doc.line_of(range.start) != doc.line_of(range.end) {
            return self.line_command(Line::Indent);
        }
        let unit = match self.indent {
            Indent::Tab => unit(Indent::Tab),
            Indent::Spaces(width) => {
                let bounds = text::row_bounds(doc, text::row_of(doc, range.start));
                let column = text::column_of(doc, bounds, range.start, self.tab_width());
                let width = usize::from(width.clamp(1, MAX_INDENT_SPACES));
                &SPACES[..width - column % width]
            }
        };
        let after = Selection::caret(range.start + unit.len());
        self.edit(range, unit, Kind::Typing, after)
    }

    fn delete(&mut self, forward: bool, word: bool) -> Effect {
        let range = if self.selection.is_empty() {
            let caret = self.selection.head;
            match (self.mode, forward, word) {
                (Mode::Hex, true, _) => caret..(caret + 1).min(self.document.len()),
                (Mode::Hex, false, _) => caret.saturating_sub(1)..caret,
                (Mode::Text, true, false) => caret..text::next_stop(&self.document, caret),
                (Mode::Text, false, false) => text::prev_stop(&self.document, caret)..caret,
                (Mode::Text, true, true) => caret..text::word_end(&self.document, caret),
                (Mode::Text, false, true) => text::word_start(&self.document, caret)..caret,
            }
        } else {
            self.selection.range()
        };
        if range.is_empty() {
            return Effect::default();
        }
        self.nibble = Nibble::High;
        self.edit(
            range.clone(),
            b"",
            Kind::Other,
            Selection::caret(range.start),
        )
    }

    fn undo(&mut self) -> Effect {
        if let Some(changes) = self.history.next_undo() {
            if self.document.room_to_revert(changes).is_err() {
                return Effect::refused(Refusal::OutOfMemory);
            }
        }
        let Self {
            document,
            history,
            highlight,
            ..
        } = self;
        let mut lines: Option<Lines> = None;
        let restored = history.undo(|change| {
            let removal =
                LineEdit::removing(document, change.at..change.at + change.inserted_len());
            document.revert(change);
            let edit = removal.inserted(document, change.removed_len());
            highlight.edited(edit);
            lines = Some(lines.map_or(Lines::of(edit), |seen| seen.union(Lines::of(edit))));
        });
        self.restored(restored, lines)
    }

    fn redo(&mut self) -> Effect {
        if let Some(changes) = self.history.next_redo() {
            if self.document.room_to_reapply(changes).is_err() {
                return Effect::refused(Refusal::OutOfMemory);
            }
        }
        let Self {
            document,
            history,
            highlight,
            ..
        } = self;
        let mut lines: Option<Lines> = None;
        let restored = history.redo(|change| {
            let removal = LineEdit::removing(document, change.at..change.at + change.removed_len());
            document.reapply(change);
            let edit = removal.inserted(document, change.inserted_len());
            highlight.edited(edit);
            lines = Some(lines.map_or(Lines::of(edit), |seen| seen.union(Lines::of(edit))));
        });
        self.restored(restored, lines)
    }

    fn restored(
        &mut self,
        restored: Result<Option<Restored>, OutOfMemory>,
        lines: Option<Lines>,
    ) -> Effect {
        let Ok(restored) = restored else {
            return Effect::refused(Refusal::OutOfMemory);
        };
        let Some(Restored { selection, eol }) = restored else {
            return Effect::default();
        };
        if let Some(eol) = eol {
            self.eol = eol;
        }
        // A step recorded in the hex view may have left an end inside a
        // character, where the text view has no caret stop.
        self.changed(self.on_stops(selection));
        Effect {
            text: lines,
            selection: true,
            view: false,
            refused: None,
        }
    }

    fn next_problem(&mut self) -> Effect {
        let caret_line = self.document.line_of(self.selection.head);
        let mut lines: Vec<usize> = self
            .diagnostics
            .iter()
            .filter_map(|diagnostic| diagnostic.line)
            .map(|line| (line as usize).saturating_sub(1))
            .collect();
        lines.sort_unstable();
        let Some(&line) = lines
            .iter()
            .find(|&&line| line > caret_line)
            .or_else(|| lines.first())
        else {
            return Effect::default();
        };
        let line = line.min(self.document.line_count() - 1);
        self.select(Selection::caret(self.document.line_start(line)))
    }

    /// Replace the selection with `bytes`: a paste, or a replace of the
    /// match the selection holds.
    pub fn replace_selection(&mut self, bytes: &[u8]) -> Effect {
        let range = self.selection.range();
        let after = Selection::caret(range.start + bytes.len());
        self.edit(range, bytes, Kind::Other, after)
    }

    /// Replace every one of `matches` — ascending, apart, all found in the
    /// document as it is — with `bytes`, as one step.
    pub fn replace_all(&mut self, matches: &[Range<usize>], bytes: &[u8]) -> Effect {
        let (Some(first), Some(last)) = (matches.first(), matches.last()) else {
            return Effect::default();
        };
        if bytes.len().saturating_mul(matches.len()) > MAX_EDIT_BYTES {
            return Effect::refused(Refusal::TooLarge);
        }
        let before = self.selection;
        let removal = LineEdit::removing(&self.document, first.start..last.end);
        let Ok(change) = self.document.replace_each(matches, bytes) else {
            return Effect::refused(Refusal::OutOfMemory);
        };
        let edit = removal.inserted(&self.document, change.inserted_len());
        self.highlight.edited(edit);
        let after = Selection::caret(first.start + bytes.len());
        self.history.record(change, before, after, Kind::Other);
        self.changed(after);
        Effect {
            text: Some(Lines::of(edit)),
            selection: true,
            view: false,
            refused: None,
        }
    }

    /// The selected bytes, for the clipboard.
    ///
    /// # Errors
    ///
    /// [`Refusal::TooLarge`] past [`MAX_EDIT_BYTES`], or
    /// [`Refusal::OutOfMemory`].
    pub fn copy(&self) -> Result<Vec<u8>, Refusal> {
        let range = self.selection.range();
        if range.len() > MAX_EDIT_BYTES {
            return Err(Refusal::TooLarge);
        }
        let mut out = Vec::new();
        self.document
            .copy_range(range, &mut out)
            .map_err(|_| Refusal::OutOfMemory)?;
        Ok(out)
    }

    /// Take the selection out, answering its bytes for the clipboard.
    pub fn cut(&mut self) -> (Effect, Option<Vec<u8>>) {
        match self.copy() {
            Ok(bytes) if !bytes.is_empty() => (self.replace_selection(b""), Some(bytes)),
            Ok(_) => (Effect::default(), None),
            Err(why) => (Effect::refused(why), None),
        }
    }

    /// The document frozen as it is now, and its generation: the same
    /// snapshot again while nothing has changed, so a repeated search or
    /// save costs nothing to set out.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the piece list cannot be held.
    pub fn snapshot(&mut self) -> Result<(u64, Arc<Snapshot>), OutOfMemory> {
        if let Some(frozen) = &self.frozen {
            return Ok((self.generation, Arc::clone(frozen)));
        }
        let frozen = Arc::new(self.document.snapshot()?);
        self.frozen = Some(Arc::clone(&frozen));
        Ok((self.generation, frozen))
    }

    /// The document of `generation` reached its file.
    pub fn saved(&mut self, generation: u64) {
        if generation == self.generation {
            self.history.mark_saved();
        } else {
            self.history.forget_saved();
        }
    }

    /// Adopt the whole document converted to `to` as one step, when
    /// `generation` is still the document's; with no `chunks`, every line
    /// break already was `to`, and only the convention new ones take is set.
    pub fn converted(
        &mut self,
        generation: u64,
        chunks: Option<Vec<Vec<u8>>>,
        to: LineEnding,
    ) -> Effect {
        if generation != self.generation {
            return Effect::refused(Refusal::Changed);
        }
        let Some(chunks) = chunks else {
            self.eol = to;
            return Effect::default();
        };
        let before = self.selection;
        let len = self.document.len();
        let caret_line = self.document.line_of(before.head);
        let removal = LineEdit::removing(&self.document, 0..len);
        let Ok(change) = self.document.replace_owned(0..len, chunks) else {
            return Effect::refused(Refusal::OutOfMemory);
        };
        let edit = removal.inserted(&self.document, change.inserted_len());
        self.highlight.edited(edit);
        let after = Selection::caret(
            self.document
                .line_start(caret_line.min(self.document.line_count() - 1)),
        );
        self.history
            .record_conversion(change, before, after, [self.eol, to]);
        self.eol = to;
        self.changed(after);
        Effect {
            text: Some(Lines {
                first: 0,
                last: None,
            }),
            selection: true,
            view: false,
            refused: None,
        }
    }

    /// Take in what the parser of the document's store said about
    /// `generation`.
    pub fn checked(&mut self, generation: u64, diagnostics: Vec<Diagnostic>) {
        if generation == self.generation {
            self.diagnostics = diagnostics;
            self.checked = Some(generation);
        }
    }

    /// Whether the document is a store its parser has not yet seen as it is.
    #[must_use]
    pub fn wants_check(&self) -> bool {
        self.format().is_store() && self.checked != Some(self.generation)
    }

    /// Size the history for pressure `band`. Pressure that arrives or deepens
    /// gives up all but the newest undo steps, and the text only they still
    /// named; pressure that eases gives nothing back, since nothing trimmed
    /// can return.
    pub fn adopt_pressure(&mut self, band: PressureBand) {
        let deepened = band > self.pressure;
        self.pressure = band;
        if !deepened {
            return;
        }
        self.frozen = None;
        self.history.trim(PRESSURE_UNDO_STEPS);
        // A tally that cannot be held lets nothing go: the trim still stands.
        let _ = self.document.release_unnamed(self.history.changes());
    }

    fn line_command(&mut self, command: Line) -> Effect {
        if self.mode == Mode::Hex {
            return Effect::default();
        }
        let marker = match command {
            Line::Comment => match self.format().line_comment() {
                Some(marker) => marker.as_bytes(),
                None => return Effect::refused(Refusal::NoLineComment),
            },
            Line::Indent | Line::Outdent => &[],
        };
        let doc = &self.document;
        let selection = self.selection;
        let range = selection.range();
        let first = doc.line_of(range.start);
        let mut last = doc.line_of(range.end);
        if last > first && range.end == doc.line_start(last) {
            last -= 1;
        }
        let start = doc.line_start(first);
        let end = doc.line_bounds(last).end;
        if end - start > MAX_EDIT_BYTES {
            return Effect::refused(Refusal::TooLarge);
        }
        let mut old = Vec::new();
        if doc.copy_range(start..end, &mut old).is_err() {
            return Effect::refused(Refusal::OutOfMemory);
        }
        let indent = unit(self.indent);
        let outdent_spaces = match self.indent {
            Indent::Tab => self.tab_width(),
            Indent::Spaces(width) => usize::from(width.clamp(1, MAX_INDENT_SPACES)),
        };
        let caret = selection
            .head
            .checked_sub(start)
            .filter(|_| selection.is_empty());
        let Some((new, caret_at)) =
            transform_lines(&old, command, indent, outdent_spaces, marker, caret)
        else {
            return Effect::refused(Refusal::OutOfMemory);
        };
        if new == old {
            return Effect::default();
        }
        let after = if selection.is_empty() {
            Selection::caret(start + caret_at)
        } else if selection.head >= selection.anchor {
            Selection {
                anchor: start,
                head: start + new.len(),
            }
        } else {
            Selection {
                anchor: start + new.len(),
                head: start,
            }
        };
        self.edit(start..end, &new, Kind::Other, after)
    }
}

/// A line-wise command.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Line {
    Indent,
    Outdent,
    Comment,
}

/// Spaces an indentation unit is cut from.
const SPACES: &[u8; 16] = b"                ";

/// Widest indentation unit of spaces.
const MAX_INDENT_SPACES: u8 = 16;

/// The bytes one level of `indent` is.
fn unit(indent: Indent) -> &'static [u8] {
    match indent {
        Indent::Tab => b"\t",
        Indent::Spaces(width) => &SPACES[..usize::from(width.clamp(1, MAX_INDENT_SPACES))],
    }
}

/// The hex caret moved to byte `offset`, in the same pane.
const fn at_byte(caret: HexCaret, offset: usize) -> HexCaret {
    HexCaret {
        offset,
        pane: caret.pane,
        nibble: Nibble::High,
    }
}

/// The edit typing `ch` makes in the hex view, if it is one that pane takes.
fn hex_edit(caret: HexCaret, current: Option<u8>, ch: char, insert: bool) -> Option<hex::HexEdit> {
    match caret.pane {
        Pane::Hex => {
            hex::digit_value(ch).map(|digit| hex::type_digit(caret, current, digit, insert))
        }
        Pane::Ascii if ch.is_ascii() && !ch.is_ascii_control() => {
            Some(hex::type_byte(caret, current, ch as u8, insert))
        }
        Pane::Ascii => None,
    }
}

/// Where the leading white space of the row `bounds` ends.
fn indentation_end(document: &Document, bounds: crate::document::LineBounds) -> usize {
    let mut end = bounds.end;
    text::for_each_glyph(document, bounds.start..bounds.end, |offset, _, glyph| {
        if matches!(glyph, Glyph::Tab | Glyph::Char(' ')) {
            ControlFlow::Continue(())
        } else {
            end = offset;
            ControlFlow::Break(())
        }
    });
    end
}

/// The lines of `old` with `command` applied, and where a caret `caret`
/// bytes into them lands; `None` when the allocator refuses.
fn transform_lines(
    old: &[u8],
    command: Line,
    unit: &[u8],
    outdent_spaces: usize,
    marker: &[u8],
    caret: Option<usize>,
) -> Option<(Vec<u8>, usize)> {
    let content =
        |line: &[u8]| -> usize { line.strip_suffix(b"\r").map_or(line.len(), <[u8]>::len) };
    let lead = |line: &[u8]| {
        line.iter()
            .take_while(|&&b| b == b' ' || b == b'\t')
            .count()
    };
    let blank = |line: &[u8]| lead(line) == content(line);
    let lines = || old.split(|&b| b == b'\n');
    let uncomment = command == Line::Comment
        && lines()
            .filter(|line| !blank(line))
            .all(|line| line[lead(line)..].starts_with(marker));
    let column = lines()
        .filter(|line| !blank(line))
        .map(&lead)
        .min()
        .unwrap_or(0);
    let count = crate::document::count_newlines(old) + 1;
    let mut out = Vec::new();
    out.try_reserve(old.len() + count * (unit.len().max(marker.len() + 1)))
        .ok()?;
    let mut caret_at = caret.unwrap_or(0);
    let mut seen = 0usize;
    for (index, line) in lines().enumerate() {
        if index > 0 {
            out.push(b'\n');
            seen += 1;
        }
        let (at, removed, inserted, spaced): (usize, usize, &[u8], bool) = match command {
            Line::Indent if !blank(line) => (0, 0, unit, false),
            Line::Indent | Line::Comment if blank(line) => (0, 0, &[], false),
            Line::Outdent => {
                let take = if line.first() == Some(&b'\t') {
                    1
                } else {
                    line.iter()
                        .take(outdent_spaces)
                        .take_while(|&&b| b == b' ')
                        .count()
                };
                (0, take, &[], false)
            }
            Line::Comment if uncomment => {
                let at = lead(line);
                let space = line.get(at + marker.len()) == Some(&b' ');
                (at, marker.len() + usize::from(space), &[], false)
            }
            Line::Comment | Line::Indent => (column, 0, marker, true),
        };
        let line_start = out.len();
        out.extend_from_slice(&line[..at]);
        out.extend_from_slice(inserted);
        if spaced {
            out.push(b' ');
        }
        out.extend_from_slice(&line[at + removed..]);
        if let Some(within) = caret
            .and_then(|caret| caret.checked_sub(seen))
            .filter(|&within| within <= line.len())
        {
            let grown = inserted.len() + usize::from(spaced);
            caret_at = line_start
                + if within >= at + removed {
                    within - removed + grown
                } else if within > at {
                    at + grown
                } else {
                    within
                };
        }
        seen += line.len();
    }
    Some((out, caret_at))
}

/// Converting every line break of a snapshot to one convention, a step at a
/// time: first finding whether any break differs, so an already uniform
/// document is neither copied nor changed, then writing the converted text
/// out as chunks a document takes in whole. A lone CR is content and stays.
pub struct Conversion {
    to: LineEnding,
    /// How far into the snapshot the current stage has read.
    at: usize,
    stage: Stage,
}

enum Stage {
    /// Looking for a break not already in the convention; `previous` is the
    /// byte before `at`, the CR of a CRLF split across a stop.
    Checking { previous: u8 },
    /// Writing the converted text; a CR at a stop waits for the next byte to
    /// learn whether it begins a CRLF.
    Converting { out: Chunked, held_cr: bool },
}

/// What one step of a [`Conversion`] came to.
pub enum Converted {
    /// There is more to go through.
    Partial,
    /// The converted text; `None` when every break already was in the
    /// convention.
    Done(Result<Option<Vec<Vec<u8>>>, OutOfMemory>),
}

impl Conversion {
    /// A conversion to `to`, not yet begun.
    #[must_use]
    pub const fn new(to: LineEnding) -> Self {
        Self {
            to,
            at: 0,
            stage: Stage::Checking { previous: 0 },
        }
    }

    /// The convention being converted to.
    #[must_use]
    pub const fn to(&self) -> LineEnding {
        self.to
    }

    /// Go on through at most `budget` more bytes of `snapshot`.
    pub fn step(&mut self, snapshot: &Snapshot, budget: usize) -> Converted {
        let crlf = self.to == LineEnding::CrLf;
        let to = self.to.bytes();
        let mut left = budget.max(1);
        let mut differs = false;
        let Self { at, stage, .. } = self;
        snapshot.walk(*at, |slice| {
            let bytes = &slice[..slice.len().min(left)];
            left -= bytes.len();
            *at += bytes.len();
            match stage {
                Stage::Checking { previous } => {
                    let mut from = 0;
                    while let Some(found) = tairix_util::lanes::nth(&bytes[from..], b'\n', 1) {
                        let feed = from + found;
                        let before = feed.checked_sub(1).map_or(*previous, |at| bytes[at]);
                        if (before == b'\r') != crlf {
                            differs = true;
                            return ControlFlow::Break(());
                        }
                        from = feed + 1;
                    }
                    *previous = bytes.last().copied().unwrap_or(*previous);
                }
                Stage::Converting { out, held_cr } => {
                    convert_into(bytes, to, out, held_cr);
                    if out.refused {
                        return ControlFlow::Break(());
                    }
                }
            }
            if left == 0 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        if differs {
            self.at = 0;
            self.stage = Stage::Converting {
                out: Chunked::default(),
                held_cr: false,
            };
            return Converted::Partial;
        }
        let ended = self.at >= snapshot.len();
        match &mut self.stage {
            Stage::Checking { .. } if ended => Converted::Done(Ok(None)),
            Stage::Converting { out, .. } if out.refused => Converted::Done(Err(OutOfMemory)),
            Stage::Converting { out, held_cr } if ended => {
                if *held_cr {
                    out.put(b"\r");
                }
                Converted::Done(core::mem::take(out).finish().map(Some))
            }
            _ => Converted::Partial,
        }
    }
}

/// Put `bytes` onto `out` with every line break spelled `to`; a CR ending
/// `bytes` is left `held` for the next to settle.
fn convert_into(mut bytes: &[u8], to: &[u8], out: &mut Chunked, held: &mut bool) {
    while !bytes.is_empty() {
        if core::mem::take(held) {
            if bytes[0] == b'\n' {
                out.put(to);
                bytes = &bytes[1..];
                continue;
            }
            out.put(b"\r");
        }
        match bytes.iter().position(|&b| b == b'\r' || b == b'\n') {
            None => {
                out.put(bytes);
                return;
            }
            Some(at) => {
                out.put(&bytes[..at]);
                if bytes[at] == b'\n' {
                    out.put(to);
                } else {
                    *held = true;
                }
                bytes = &bytes[at + 1..];
            }
        }
    }
}

/// Bytes gathered into chunks of a fixed size, every allocation fallible.
#[derive(Default)]
struct Chunked {
    chunks: Vec<Vec<u8>>,
    current: Vec<u8>,
    refused: bool,
}

impl Chunked {
    /// Bytes per chunk.
    const CHUNK: usize = 1024 * 1024;

    fn put(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() && !self.refused {
            if self.current.capacity() == 0 && self.current.try_reserve_exact(Self::CHUNK).is_err()
            {
                self.refused = true;
                return;
            }
            let take = (self.current.capacity() - self.current.len()).min(bytes.len());
            self.current.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.current.len() == self.current.capacity() {
                self.seal();
            }
        }
    }

    fn seal(&mut self) {
        if self.chunks.try_reserve(1).is_err() {
            self.refused = true;
            return;
        }
        self.chunks.push(core::mem::take(&mut self.current));
    }

    fn finish(mut self) -> Result<Vec<Vec<u8>>, OutOfMemory> {
        if !self.current.is_empty() {
            self.seal();
        }
        if self.refused {
            Err(OutOfMemory)
        } else {
            Ok(self.chunks)
        }
    }
}

#[cfg(test)]
#[path = "editor_tests.rs"]
mod tests;
