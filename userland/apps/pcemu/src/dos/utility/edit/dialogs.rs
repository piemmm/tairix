//! `EDIT`'s dialogs, laid out as MS-DOS 5's are: Open and Save As with
//! their directory and its lists, Find, Change and its question at each
//! match, Print, Display, the questions an unsaved document and an existing
//! file ask, a message, and About. Each is its controls in a framed box
//! with its buttons under a rule. Tab and Shift+Tab move among them, Alt
//! with a letter goes to the control it marks, Space sets a check box,
//! the arrows choose an option or an entry, Enter presses the focused
//! button or the first, and Esc cancels; the mouse reaches each directly.

use super::menus::key_letter;
use super::view::{glyph, Cells, Palette};
use crate::frames::record;
use alloc::vec::Vec;

/// The dialogs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    None,
    Open,
    SaveAs,
    Find,
    Change,
    Verify,
    Print,
    Display,
    Unsaved,
    Overwrite,
    Message,
    About,
}

impl Kind {
    const ALL: [Self; 12] = [
        Self::None,
        Self::Open,
        Self::SaveAs,
        Self::Find,
        Self::Change,
        Self::Verify,
        Self::Print,
        Self::Display,
        Self::Unsaved,
        Self::Overwrite,
        Self::Message,
        Self::About,
    ];

    /// The dialog `byte` numbers, none for a number no dialog has.
    #[must_use]
    pub fn of(byte: u8) -> Self {
        Self::ALL
            .get(usize::from(byte))
            .copied()
            .unwrap_or_default()
    }

    #[must_use]
    pub fn number(self) -> u8 {
        u8::try_from(Self::ALL.iter().position(|&kind| kind == self).unwrap_or(0)).unwrap_or(0)
    }
}

numbered!(Kind);

/// The buttons a dialog is ended by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Ok,
    Cancel,
    Help,
    Yes,
    No,
    Verify,
    ChangeAll,
    Change,
    Skip,
}

impl Button {
    const fn label(self) -> &'static [u8] {
        match self {
            Self::Ok => b"OK",
            Self::Cancel => b"Cancel",
            Self::Help => b"&Help",
            Self::Yes => b"&Yes",
            Self::No => b"&No",
            Self::Verify => b"Find and &Verify",
            Self::ChangeAll => b"Change &All",
            Self::Change => b"&Change",
            Self::Skip => b"&Skip",
        }
    }

    /// The columns it takes, its angle brackets among them.
    fn width(self) -> u8 {
        u8::try_from(shown_len(self.label()) + 4).unwrap_or(u8::MAX)
    }
}

/// The check boxes' bits.
pub mod check {
    pub const MATCH_CASE: u8 = 0x01;
    pub const WHOLE_WORD: u8 = 0x02;
    pub const SCROLL_BARS: u8 = 0x04;
}

/// What a list holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Files,
    Directories,
    Foreground,
    Background,
}

/// Which of a dialog's two lists `source` is.
#[must_use]
pub const fn list_index(source: Source) -> usize {
    match source {
        Source::Files | Source::Foreground => 0,
        Source::Directories | Source::Background => 1,
    }
}

/// A control, placed by its row from the box's top and its column from
/// the first inside the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Field {
        row: u8,
        column: u8,
        width: u8,
        label: &'static [u8],
        field: u8,
    },
    Check {
        row: u8,
        column: u8,
        label: &'static [u8],
        bit: u8,
    },
    Choice {
        row: u8,
        column: u8,
        label: &'static [u8],
        value: u8,
    },
    /// A list: its frame's top row, its entries' rows below, its width
    /// inside the frame; its label sits above it.
    List {
        row: u8,
        column: u8,
        rows: u8,
        width: u8,
        label: &'static [u8],
        source: Source,
    },
    /// Fixed text, centred where its column is none.
    Text {
        row: u8,
        column: Option<u8>,
        text: &'static [u8],
    },
    /// The directory the lists show.
    Path { row: u8, column: u8 },
    /// The note a message shows, centred.
    Note { row: u8 },
}

impl Control {
    fn letter(&self) -> Option<u8> {
        match self {
            Self::Field { label, .. }
            | Self::Check { label, .. }
            | Self::Choice { label, .. }
            | Self::List { label, .. } => key_letter(label),
            Self::Text { .. } | Self::Path { .. } | Self::Note { .. } => None,
        }
    }
}

/// The columns a label takes: `&` marks its key letter and is not shown.
fn shown_len(label: &[u8]) -> usize {
    label.len() - usize::from(label.contains(&b'&'))
}

fn columns_of(label: &[u8]) -> u8 {
    u8::try_from(shown_len(label)).unwrap_or(u8::MAX)
}

/// A dialog's layout: its title, the rows its controls take, its width
/// with its frame, its controls and its buttons, the first the default.
pub struct Layout {
    pub title: &'static [u8],
    pub rows: u8,
    pub width: u8,
    pub controls: &'static [Control],
    pub buttons: &'static [Button],
}

const NAME_FIELD: Control = Control::Field {
    row: 2,
    column: 2,
    width: 46,
    label: b"File &Name:",
    field: 0,
};
const OPEN_CONTROLS: [Control; 4] = [
    NAME_FIELD,
    Control::Path { row: 4, column: 2 },
    Control::List {
        row: 7,
        column: 2,
        rows: 8,
        width: 30,
        label: b"&Files",
        source: Source::Files,
    },
    Control::List {
        row: 7,
        column: 37,
        rows: 8,
        width: 18,
        label: b"&Dirs/Drives",
        source: Source::Directories,
    },
];
const SAVE_CONTROLS: [Control; 3] = [
    NAME_FIELD,
    Control::Path { row: 4, column: 2 },
    Control::List {
        row: 7,
        column: 2,
        rows: 8,
        width: 18,
        label: b"&Dirs/Drives",
        source: Source::Directories,
    },
];
const FIND_FIELD: Control = Control::Field {
    row: 2,
    column: 2,
    width: 40,
    label: b"&Find What:",
    field: 0,
};
const FIND_CONTROLS: [Control; 3] = [
    FIND_FIELD,
    Control::Check {
        row: 4,
        column: 2,
        label: b"Match &Upper/Lowercase",
        bit: check::MATCH_CASE,
    },
    Control::Check {
        row: 4,
        column: 34,
        label: b"&Whole Word",
        bit: check::WHOLE_WORD,
    },
];
const CHANGE_CONTROLS: [Control; 4] = [
    FIND_FIELD,
    Control::Field {
        row: 4,
        column: 2,
        width: 40,
        label: b"Change &To:",
        field: 1,
    },
    Control::Check {
        row: 6,
        column: 2,
        label: b"Match &Upper/Lowercase",
        bit: check::MATCH_CASE,
    },
    Control::Check {
        row: 6,
        column: 34,
        label: b"&Whole Word",
        bit: check::WHOLE_WORD,
    },
];
const PRINT_CONTROLS: [Control; 2] = [
    Control::Choice {
        row: 2,
        column: 4,
        label: b"&Selected Text Only",
        value: 0,
    },
    Control::Choice {
        row: 3,
        column: 4,
        label: b"&Complete Document",
        value: 1,
    },
];
const DISPLAY_CONTROLS: [Control; 5] = [
    Control::Text {
        row: 2,
        column: Some(2),
        text: b"Colors for normal text:",
    },
    Control::List {
        row: 5,
        column: 2,
        rows: 6,
        width: 18,
        label: b"&Foreground",
        source: Source::Foreground,
    },
    Control::List {
        row: 5,
        column: 24,
        rows: 6,
        width: 18,
        label: b"&Background",
        source: Source::Background,
    },
    Control::Check {
        row: 14,
        column: 2,
        label: b"&Scroll Bars",
        bit: check::SCROLL_BARS,
    },
    Control::Field {
        row: 14,
        column: 24,
        width: 3,
        label: b"&Tab Stops:",
        field: 0,
    },
];
const UNSAVED_CONTROLS: [Control; 1] = [Control::Text {
    row: 1,
    column: None,
    text: b"Loaded file is not saved. Save it now?",
}];
const OVERWRITE_CONTROLS: [Control; 1] = [Control::Text {
    row: 1,
    column: None,
    text: b"File already exists. Overwrite?",
}];
const VERIFY_CONTROLS: [Control; 1] = [Control::Text {
    row: 1,
    column: None,
    text: b"Change this occurrence?",
}];
const MESSAGE_CONTROLS: [Control; 1] = [Control::Note { row: 1 }];
const ABOUT_CONTROLS: [Control; 3] = [
    Control::Text {
        row: 1,
        column: None,
        text: b"TAIRiX DOS Editor",
    },
    Control::Text {
        row: 2,
        column: None,
        text: crate::dos::shell::text::VERSION,
    },
    Control::Text {
        row: 4,
        column: None,
        text: b"Free software under the GNU GPL, version 2 or later",
    },
];

/// What each dialog is.
#[must_use]
pub const fn layout(kind: Kind) -> Layout {
    const fn of(
        title: &'static [u8],
        rows: u8,
        width: u8,
        controls: &'static [Control],
        buttons: &'static [Button],
    ) -> Layout {
        Layout {
            title,
            rows,
            width,
            controls,
            buttons,
        }
    }
    const OK_CANCEL_HELP: &[Button] = &[Button::Ok, Button::Cancel, Button::Help];
    match kind {
        Kind::None => of(b"", 0, 0, &[], &[]),
        Kind::Open => of(b"Open", 16, 66, &OPEN_CONTROLS, OK_CANCEL_HELP),
        Kind::SaveAs => of(b"Save As", 16, 66, &SAVE_CONTROLS, OK_CANCEL_HELP),
        Kind::Find => of(b"Find", 5, 60, &FIND_CONTROLS, OK_CANCEL_HELP),
        Kind::Change => of(
            b"Change",
            7,
            68,
            &CHANGE_CONTROLS,
            &[
                Button::Verify,
                Button::ChangeAll,
                Button::Cancel,
                Button::Help,
            ],
        ),
        Kind::Verify => of(
            b"Change",
            2,
            50,
            &VERIFY_CONTROLS,
            &[Button::Change, Button::Skip, Button::Cancel],
        ),
        Kind::Print => of(b"Print", 4, 46, &PRINT_CONTROLS, OK_CANCEL_HELP),
        Kind::Display => of(b"Display", 15, 50, &DISPLAY_CONTROLS, OK_CANCEL_HELP),
        Kind::Unsaved => of(
            b"",
            2,
            50,
            &UNSAVED_CONTROLS,
            &[Button::Yes, Button::No, Button::Cancel, Button::Help],
        ),
        Kind::Overwrite => of(b"", 2, 46, &OVERWRITE_CONTROLS, &[Button::Yes, Button::No]),
        Kind::Message => of(b"", 2, 36, &MESSAGE_CONTROLS, &[Button::Ok]),
        Kind::About => of(b"About", 5, 60, &ABOUT_CONTROLS, &[Button::Ok]),
    }
}

/// The colours' names, as Display lists them; the background takes the
/// first eight.
pub const COLOURS: [&[u8]; 16] = [
    b"Black",
    b"Blue",
    b"Green",
    b"Cyan",
    b"Red",
    b"Magenta",
    b"Brown",
    b"White",
    b"Gray",
    b"Bright Blue",
    b"Bright Green",
    b"Bright Cyan",
    b"Bright Red",
    b"Pink",
    b"Yellow",
    b"Bright White",
];

/// The longest text a field holds: a drive, a directory DOS can reach and
/// a name in it.
pub const FIELD_LEN: usize = 80;

record! {
    /// A text field's contents, its cursor, the first character shown, and
    /// whether the next character typed replaces it all, as it does when the
    /// field has just been reached.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Field {
        bytes: [u8; FIELD_LEN],
        len: u8,
        cursor: u8,
        left: u8,
        fresh: bool,
    }
}

impl crate::frames::Field for [Field; 2] {
    const SIZE: usize = 2 * <Field as crate::frames::Field>::SIZE;
    fn take(bytes: &[u8], at: &mut usize) -> Self {
        let first = <Field as crate::frames::Field>::take(bytes, at);
        [first, <Field as crate::frames::Field>::take(bytes, at)]
    }
    fn put(self, bytes: &mut [u8], at: &mut usize) {
        for field in self {
            crate::frames::Field::put(field, bytes, at);
        }
    }
}

impl Default for Field {
    fn default() -> Self {
        Self {
            bytes: [0; FIELD_LEN],
            len: 0,
            cursor: 0,
            left: 0,
            fresh: false,
        }
    }
}

impl Field {
    #[must_use]
    pub fn text(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len).min(FIELD_LEN)]
    }

    /// The field given `text`, as much as it holds, the cursor after it.
    pub fn set(&mut self, text: &[u8]) {
        let len = text.len().min(FIELD_LEN);
        self.bytes = [0; FIELD_LEN];
        self.bytes[..len].copy_from_slice(&text[..len]);
        self.len = u8::try_from(len).unwrap_or(0);
        self.cursor = self.len;
        self.left = 0;
    }

    /// `key` given to the field.
    fn key(&mut self, key: DialogKey) {
        if core::mem::take(&mut self.fresh) && matches!(key, DialogKey::Char(_)) {
            self.set(b"");
        }
        let len = usize::from(self.len).min(FIELD_LEN);
        let at = usize::from(self.cursor).min(len);
        match key {
            DialogKey::Char(byte) if byte >= 0x20 && len < FIELD_LEN => {
                self.bytes.copy_within(at..len, at + 1);
                self.bytes[at] = byte;
                self.len += 1;
                self.cursor += 1;
            }
            DialogKey::Backspace if at > 0 => {
                self.bytes.copy_within(at..len, at - 1);
                self.len -= 1;
                self.cursor -= 1;
            }
            DialogKey::Delete if at < len => {
                self.bytes.copy_within(at + 1..len, at);
                self.len -= 1;
            }
            DialogKey::Left => self.cursor = self.cursor.saturating_sub(1),
            DialogKey::Right => self.cursor = (self.cursor + 1).min(self.len),
            DialogKey::Home => self.cursor = 0,
            DialogKey::End => self.cursor = self.len,
            _ => {}
        }
    }

    /// The first character shown moved the least that shows the cursor in
    /// `width` columns.
    fn follow(&mut self, width: u8) {
        let width = width.max(1);
        if self.cursor < self.left {
            self.left = self.cursor;
        } else if self.cursor >= self.left.saturating_add(width) {
            self.left = self.cursor + 1 - width;
        }
    }
}

/// The entries of a dialog's lists, as the editor holds them.
pub trait Lists {
    /// The entries `source` has.
    fn count(&mut self, source: Source) -> u16;
    /// Entry `n` of `source`, as shown.
    fn entry(&mut self, source: Source, n: u16) -> Vec<u8>;
}

record! {
    /// An open dialog's state.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Dialog {
        kind: Kind,
        /// The control or button focused: the controls first, then the
        /// buttons.
        focus: u8,
        fields: [Field; 2],
        /// A message's text.
        note: Field,
        checks: u8,
        choice: u8,
        /// Each list's first entry shown and its chosen one.
        tops: [u16; 2],
        picks: [u16; 2],
    }
}

/// What a key or a click does to a dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Kept,
    Pressed(Button),
    /// A list's chosen entry moved.
    Picked(Source),
    /// A list's chosen entry was taken, with Enter or a double click.
    Taken(Source),
}

/// The keys a dialog answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogKey {
    Char(u8),
    Tab,
    BackTab,
    Enter,
    Escape,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Backspace,
    Delete,
    Alt(u8),
}

/// Where a dialog lies on the screen: its top, left, bottom and right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub top: u8,
    pub left: u8,
    pub bottom: u8,
    pub right: u8,
}

impl Rect {
    /// The screen cell of row `row` and inside column `column`.
    const fn at(self, row: u8, column: u8) -> (u8, u8) {
        (self.top + row, self.left + 1 + column)
    }
}

/// What a dialog is drawn on and with: the screen, its colours, the
/// directory its lists show, and their entries.
pub struct Canvas<'a> {
    pub cells: &'a mut Cells,
    pub palette: &'a Palette,
    pub path: &'a [u8],
    pub lists: &'a mut dyn Lists,
}

impl Dialog {
    /// Dialog `kind`, opened, focus on its first control.
    #[must_use]
    pub fn open(kind: Kind) -> Self {
        let mut dialog = Self {
            kind,
            ..Self::default()
        };
        dialog.focus = dialog.first_focus();
        dialog.reached();
        dialog
    }

    /// A message dialog saying `note`.
    #[must_use]
    pub fn message(note: &[u8]) -> Self {
        let mut dialog = Self::open(Kind::Message);
        dialog.note.set(note);
        dialog
    }

    fn controls(&self) -> &'static [Control] {
        layout(self.kind).controls
    }

    fn buttons(&self) -> &'static [Button] {
        layout(self.kind).buttons
    }

    fn stops(&self) -> u8 {
        u8::try_from(self.controls().len() + self.buttons().len()).unwrap_or(0)
    }

    /// Whether `stop` can hold the focus: a button, or a control other than
    /// fixed text and the options not chosen.
    fn focusable(&self, stop: u8) -> bool {
        match self.controls().get(usize::from(stop)) {
            Some(Control::Text { .. } | Control::Path { .. } | Control::Note { .. }) => false,
            Some(Control::Choice { value, .. }) => *value == self.choice,
            _ => true,
        }
    }

    fn first_focus(&self) -> u8 {
        (0..self.stops())
            .find(|&stop| self.focusable(stop))
            .unwrap_or(0)
    }

    /// The control focused, if focus is on one rather than a button.
    #[must_use]
    pub fn control(&self) -> Option<Control> {
        self.controls().get(usize::from(self.focus)).copied()
    }

    /// The button focused, if focus is on one.
    #[must_use]
    pub fn button(&self) -> Option<Button> {
        let at = usize::from(self.focus).checked_sub(self.controls().len())?;
        self.buttons().get(at).copied()
    }

    /// The button Enter presses: the one focused, else the first.
    fn default_button(&self) -> Button {
        self.button()
            .or_else(|| self.buttons().first().copied())
            .unwrap_or(Button::Cancel)
    }

    /// The field focused has just been reached: its whole text is replaced
    /// by the next character typed.
    fn reached(&mut self) {
        if let Some(Control::Field { field, .. }) = self.control() {
            self.fields[usize::from(field.min(1))].fresh = true;
        }
    }

    fn step_focus(&mut self, forward: bool) {
        let stops = self.stops().max(1);
        for _ in 0..stops {
            self.focus = if forward {
                (self.focus + 1) % stops
            } else {
                (self.focus + stops - 1) % stops
            };
            if self.focusable(self.focus) {
                break;
            }
        }
        self.reached();
    }

    /// `key` given to the dialog, its lists' entries in `lists`.
    pub fn key(&mut self, key: DialogKey, lists: &mut dyn Lists) -> Outcome {
        match key {
            DialogKey::Tab => self.step_focus(true),
            DialogKey::BackTab => self.step_focus(false),
            DialogKey::Escape => return Outcome::Pressed(Button::Cancel),
            DialogKey::Alt(letter) => return self.alt(letter),
            DialogKey::Char(letter) if letter.is_ascii_alphabetic() && !self.takes_letters() => {
                return self.alt(letter)
            }
            DialogKey::Enter => {
                if let Some(Control::List { source, .. }) = self.control() {
                    return Outcome::Taken(source);
                }
                return Outcome::Pressed(self.default_button());
            }
            _ => return self.in_control(key, lists),
        }
        Outcome::Kept
    }

    /// Whether the control focused takes letters itself: a field types
    /// them, a list finds its entries by them. Elsewhere a letter is a key
    /// letter, as with Alt.
    fn takes_letters(&self) -> bool {
        matches!(
            self.control(),
            Some(Control::Field { .. } | Control::List { .. })
        )
    }

    fn alt(&mut self, letter: u8) -> Outcome {
        let letter = letter.to_ascii_uppercase();
        if let Some(at) = self
            .controls()
            .iter()
            .position(|control| control.letter() == Some(letter))
        {
            self.focus = u8::try_from(at).unwrap_or(0);
            match self.control() {
                Some(Control::Check { bit, .. }) => self.checks ^= bit,
                Some(Control::Choice { value, .. }) => self.choice = value,
                _ => self.reached(),
            }
            return Outcome::Kept;
        }
        self.buttons()
            .iter()
            .find(|button| key_letter(button.label()) == Some(letter))
            .map_or(Outcome::Kept, |&button| Outcome::Pressed(button))
    }

    fn in_control(&mut self, key: DialogKey, lists: &mut dyn Lists) -> Outcome {
        match self.control() {
            Some(Control::Field { field, width, .. }) => {
                let field = &mut self.fields[usize::from(field.min(1))];
                field.key(key);
                field.follow(width);
                Outcome::Kept
            }
            Some(Control::Check { bit, .. }) if key == DialogKey::Char(b' ') => {
                self.checks ^= bit;
                Outcome::Kept
            }
            Some(Control::Choice { .. }) => {
                let last = self.choices().saturating_sub(1);
                match key {
                    DialogKey::Up | DialogKey::Left => self.choose(self.choice.saturating_sub(1)),
                    DialogKey::Down | DialogKey::Right => self.choose((self.choice + 1).min(last)),
                    _ => {}
                }
                Outcome::Kept
            }
            Some(Control::List { rows, source, .. }) => self.list_key(key, rows, source, lists),
            _ => Outcome::Kept,
        }
    }

    /// Option `value` chosen, and focused.
    pub fn choose(&mut self, value: u8) {
        self.choice = value;
        let found = self.controls().iter().position(
            |control| matches!(control, Control::Choice { value: each, .. } if *each == value),
        );
        if let Some(at) = found {
            self.focus = u8::try_from(at).unwrap_or(0);
        }
    }

    fn choices(&self) -> u8 {
        u8::try_from(
            self.controls()
                .iter()
                .filter(|control| matches!(control, Control::Choice { .. }))
                .count(),
        )
        .unwrap_or(0)
    }

    fn list_key(
        &mut self,
        key: DialogKey,
        rows: u8,
        source: Source,
        lists: &mut dyn Lists,
    ) -> Outcome {
        let n = list_index(source);
        let len = lists.count(source);
        let pick = self.picks[n];
        let page = u16::from(rows.max(1));
        let moved = match key {
            DialogKey::Up | DialogKey::Left => pick.saturating_sub(1),
            DialogKey::Down | DialogKey::Right => pick.saturating_add(1),
            DialogKey::PageUp => pick.saturating_sub(page),
            DialogKey::PageDown => pick.saturating_add(page),
            DialogKey::Home => 0,
            DialogKey::End => len.saturating_sub(1),
            DialogKey::Char(letter) if letter.is_ascii_graphic() => {
                match next_starting(lists, source, pick, letter) {
                    Some(found) => found,
                    None => return Outcome::Kept,
                }
            }
            _ => return Outcome::Kept,
        };
        self.pick(n, moved.min(len.saturating_sub(1)), page);
        Outcome::Picked(source)
    }

    /// List `n`'s entry `pick` chosen, its window moved the least that
    /// shows it in `rows`.
    pub fn pick(&mut self, n: usize, pick: u16, rows: u16) {
        self.picks[n] = pick;
        if pick < self.tops[n] {
            self.tops[n] = pick;
        } else if pick >= self.tops[n] + rows {
            self.tops[n] = pick + 1 - rows;
        }
    }

    /// The box's width with its frame: a message's fits its note.
    fn width(&self) -> u8 {
        let width = layout(self.kind).width;
        if self.kind == Kind::Message {
            return width.max(self.note.len.saturating_add(8));
        }
        width
    }

    /// Where the box lies on a screen of `rows` and `columns`, centred.
    #[must_use]
    pub fn rect(&self, rows: u8, columns: u8) -> Rect {
        let height = layout(self.kind).rows + 4;
        let width = self.width().min(columns);
        let top = rows.saturating_sub(height) / 2;
        let left = columns.saturating_sub(width) / 2;
        Rect {
            top,
            left,
            bottom: top + height - 1,
            right: left + width - 1,
        }
    }

    /// The column inside the frame `text` starts at when centred.
    fn centred(&self, text: &[u8]) -> u8 {
        let inside = self.width().saturating_sub(2);
        inside.saturating_sub(u8::try_from(text.len()).unwrap_or(u8::MAX)) / 2
    }

    /// Where each button starts on the screen.
    fn button_columns(&self, rect: Rect) -> Vec<u8> {
        let buttons = self.buttons();
        let gaps = u8::try_from(buttons.len().saturating_sub(1) * 3).unwrap_or(0);
        let total = buttons.iter().map(|button| button.width()).sum::<u8>() + gaps;
        let mut column = rect.left + 1 + self.width().saturating_sub(2).saturating_sub(total) / 2;
        buttons
            .iter()
            .map(|button| {
                let at = column;
                column += button.width() + 3;
                at
            })
            .collect()
    }

    /// The dialog drawn on `canvas`.
    pub fn draw(&self, canvas: &mut Canvas<'_>) {
        let rect = self.rect(canvas.cells.rows, canvas.cells.columns);
        let (cells, palette) = (&mut *canvas.cells, canvas.palette);
        cells.frame(
            rect.top,
            rect.left,
            rect.bottom,
            rect.right,
            palette.dialog,
            layout(self.kind).title,
        );
        cells.shadow(rect.top, rect.left, rect.bottom, rect.right, palette.shadow);
        for (n, control) in self.controls().iter().enumerate() {
            self.draw_control(canvas, rect, *control, usize::from(self.focus) == n);
        }
        let (cells, palette) = (&mut *canvas.cells, canvas.palette);
        let rule = rect.bottom - 2;
        cells.put(rule, rect.left, glyph::TEE_LEFT, palette.dialog);
        for column in rect.left + 1..rect.right {
            cells.put(rule, column, glyph::ACROSS, palette.dialog);
        }
        cells.put(rule, rect.right, glyph::TEE_RIGHT, palette.dialog);
        let lit = self.default_button();
        for (&button, column) in self.buttons().iter().zip(self.button_columns(rect)) {
            let bracket = if button == lit {
                palette.hot
            } else {
                palette.dialog
            };
            let row = rect.bottom - 1;
            cells.put(row, column, b'<', bracket);
            let used = cells.label(row, column + 2, button.label(), palette.dialog, palette.hot);
            cells.put(row, column + 3 + used, b'>', bracket);
        }
    }

    fn draw_control(&self, canvas: &mut Canvas<'_>, rect: Rect, control: Control, focused: bool) {
        let (cells, palette) = (&mut *canvas.cells, canvas.palette);
        match control {
            Control::Text { row, column, text } => {
                let (row, column) = rect.at(row, column.unwrap_or_else(|| self.centred(text)));
                cells.write(row, column, text, palette.dialog);
            }
            Control::Note { row } => {
                let note = self.note.text();
                let (row, column) = rect.at(row, self.centred(note));
                cells.write(row, column, note, palette.dialog);
            }
            Control::Path { row, column } => {
                let (row, column) = rect.at(row, column);
                let room = usize::from(rect.right.saturating_sub(column + 1));
                let path = canvas.path;
                cells.write(
                    row,
                    column,
                    &path[path.len().saturating_sub(room)..],
                    palette.dialog,
                );
            }
            Control::Field {
                row,
                column,
                width,
                label,
                field,
            } => {
                let (row, column) = rect.at(row, column);
                let start =
                    column + cells.label(row, column, label, palette.dialog, palette.hot) + 1;
                self.draw_field(cells, palette, (row, start), width, field, focused);
            }
            Control::Check {
                row,
                column,
                label,
                bit,
            } => {
                let (row, column) = rect.at(row, column);
                cells.write(
                    row,
                    column,
                    if self.checks & bit != 0 {
                        b"[X] "
                    } else {
                        b"[ ] "
                    },
                    palette.dialog,
                );
                cells.label(row, column + 4, label, palette.dialog, palette.hot);
            }
            Control::Choice {
                row,
                column,
                label,
                value,
            } => {
                let (row, column) = rect.at(row, column);
                cells.write(
                    row,
                    column,
                    if self.choice == value {
                        b"(\x07) "
                    } else {
                        b"( ) "
                    },
                    palette.dialog,
                );
                cells.label(row, column + 4, label, palette.dialog, palette.hot);
            }
            Control::List {
                row,
                column,
                rows,
                width,
                label,
                source,
            } => {
                let (row, column) = rect.at(row, column);
                cells.label(row - 1, column + 1, label, palette.dialog, palette.hot);
                self.draw_list(canvas, (row, column), (rows, width), source, focused);
            }
        }
    }

    /// Draws text field `field` from `(row, start)`, its text `width` columns
    /// between its brackets.
    fn draw_field(
        &self,
        cells: &mut Cells,
        palette: &Palette,
        (row, start): (u8, u8),
        width: u8,
        field: u8,
        focused: bool,
    ) {
        let field = &self.fields[usize::from(field.min(1))];
        cells.put(row, start, b'[', palette.dialog);
        let attribute = if focused && field.fresh {
            palette.chosen
        } else {
            palette.field
        };
        cells.fill(row, start + 1, row, start + width, b' ', palette.field);
        let shown = field
            .text()
            .get(usize::from(field.left)..)
            .unwrap_or_default();
        cells.write(
            row,
            start + 1,
            &shown[..shown.len().min(usize::from(width))],
            attribute,
        );
        cells.put(row, start + width + 1, b']', palette.dialog);
    }

    /// Draws list `source` framed from `(row, column)`, `rows` entries of
    /// `width` columns inside the frame, its pick lit.
    fn draw_list(
        &self,
        canvas: &mut Canvas<'_>,
        (row, column): (u8, u8),
        (rows, width): (u8, u8),
        source: Source,
        focused: bool,
    ) {
        let palette = canvas.palette;
        canvas.cells.frame(
            row,
            column,
            row + rows + 1,
            column + width + 1,
            palette.dialog,
            b"",
        );
        let n = list_index(source);
        let count = canvas.lists.count(source);
        for (line, index) in (row + 1..row + 1 + rows).zip(self.tops[n]..count) {
            let entry = canvas.lists.entry(source, index);
            let attribute = match (index == self.picks[n], focused) {
                (true, true) => palette.chosen,
                (true, false) => palette.hot,
                (false, _) => palette.dialog,
            };
            canvas
                .cells
                .fill(line, column + 1, line, column + width, b' ', attribute);
            canvas.cells.write(
                line,
                column + 1,
                &entry[..entry.len().min(usize::from(width))],
                attribute,
            );
        }
    }

    /// Where the cursor stands in the dialog, for the program to place it.
    #[must_use]
    pub fn cursor(&self, rows: u8, columns: u8) -> Option<(u8, u8)> {
        let rect = self.rect(rows, columns);
        if let Some(button) = self.button() {
            let at = self.buttons().iter().position(|&each| each == button)?;
            return Some((rect.bottom - 1, self.button_columns(rect)[at] + 2));
        }
        match self.control()? {
            Control::Field {
                row,
                column,
                label,
                field,
                ..
            } => {
                let field = &self.fields[usize::from(field.min(1))];
                let (row, column) = rect.at(row, column);
                Some((
                    row,
                    column + columns_of(label) + 2 + field.cursor.saturating_sub(field.left),
                ))
            }
            Control::Check { row, column, .. } | Control::Choice { row, column, .. } => {
                Some(rect.at(row, column + 1))
            }
            Control::List {
                row,
                column,
                rows,
                source,
                ..
            } => {
                let n = list_index(source);
                let line = self.picks[n]
                    .saturating_sub(self.tops[n])
                    .min(u16::from(rows.saturating_sub(1)));
                let (row, column) = rect.at(row + 1, column + 1);
                Some((row + u8::try_from(line).unwrap_or(0), column))
            }
            Control::Text { .. } | Control::Path { .. } | Control::Note { .. } => None,
        }
    }

    /// The mouse's left button pressed at `at` on a screen of `screen`'s
    /// rows and columns, `double` a second press there in quick succession.
    pub fn click(
        &mut self,
        screen: (u8, u8),
        at: (u8, u8),
        double: bool,
        lists: &mut dyn Lists,
    ) -> Outcome {
        let rect = self.rect(screen.0, screen.1);
        let (row, column) = at;
        if row == rect.bottom - 1 {
            let columns = self.button_columns(rect);
            let found = self
                .buttons()
                .iter()
                .zip(columns)
                .find(|&(button, start)| (start..start + button.width()).contains(&column));
            return found.map_or(Outcome::Kept, |(&button, _)| Outcome::Pressed(button));
        }
        for (n, control) in self.controls().iter().enumerate() {
            if let Some(outcome) = self.hit(rect, *control, at, double, lists) {
                self.focus = u8::try_from(n).unwrap_or(0);
                return outcome;
            }
        }
        Outcome::Kept
    }

    /// What a press at `at` does to `control`, none where it misses it.
    fn hit(
        &mut self,
        rect: Rect,
        control: Control,
        at: (u8, u8),
        double: bool,
        lists: &mut dyn Lists,
    ) -> Option<Outcome> {
        let (row, column) = at;
        match control {
            Control::Field {
                row: line,
                column: start,
                width,
                label,
                field,
            } => {
                let (line, start) = rect.at(line, start);
                let first = start + columns_of(label) + 2;
                if row != line || !(start..first + width).contains(&column) {
                    return None;
                }
                let field = &mut self.fields[usize::from(field.min(1))];
                field.fresh = false;
                field.cursor = column
                    .saturating_sub(first)
                    .saturating_add(field.left)
                    .min(field.len);
                Some(Outcome::Kept)
            }
            Control::Check {
                row: line,
                column: start,
                label,
                bit,
            } => {
                let (line, start) = rect.at(line, start);
                let hit = row == line && (start..start + 4 + columns_of(label)).contains(&column);
                hit.then(|| {
                    self.checks ^= bit;
                    Outcome::Kept
                })
            }
            Control::Choice {
                row: line,
                column: start,
                label,
                value,
            } => {
                let (line, start) = rect.at(line, start);
                let hit = row == line && (start..start + 4 + columns_of(label)).contains(&column);
                hit.then(|| {
                    self.choice = value;
                    Outcome::Kept
                })
            }
            Control::List {
                row: line,
                column: start,
                rows,
                width,
                source,
                ..
            } => {
                let (top, start) = rect.at(line, start);
                let inside = (top..=top + rows + 1).contains(&row)
                    && (start..=start + width + 1).contains(&column);
                inside.then(|| self.list_click(top, rows, source, row, double, lists))
            }
            Control::Text { .. } | Control::Path { .. } | Control::Note { .. } => None,
        }
    }

    /// A press on a list whose frame's top is `top`: its frame's top and
    /// foot scroll it, an entry is chosen, or taken with `double`.
    fn list_click(
        &mut self,
        top: u8,
        rows: u8,
        source: Source,
        row: u8,
        double: bool,
        lists: &mut dyn Lists,
    ) -> Outcome {
        let n = list_index(source);
        let page = u16::from(rows);
        let count = lists.count(source);
        let pick = if row == top {
            self.tops[n].saturating_sub(1)
        } else if row == top + rows + 1 {
            (self.tops[n] + page).min(count.saturating_sub(1))
        } else {
            let pick = self.tops[n] + u16::from(row - top - 1);
            if pick >= count {
                return Outcome::Kept;
            }
            self.pick(n, pick, page);
            return if double {
                Outcome::Taken(source)
            } else {
                Outcome::Picked(source)
            };
        };
        self.pick(n, pick, page);
        Outcome::Picked(source)
    }
}

/// The next entry of `source` after `from` whose first letter is `letter`,
/// round to the first; a drive's `[-` passed over.
fn next_starting(lists: &mut dyn Lists, source: Source, from: u16, letter: u8) -> Option<u16> {
    let count = u32::from(lists.count(source));
    (1..=count)
        .map(|step| (u32::from(from) + step) % count.max(1))
        .filter_map(|n| u16::try_from(n).ok())
        .find(|&n| {
            let entry = lists.entry(source, n);
            let first = entry
                .iter()
                .copied()
                .find(|&byte| byte != b'[' && byte != b'-');
            first.is_some_and(|first| first.eq_ignore_ascii_case(&letter))
        })
}

#[cfg(test)]
#[path = "dialogs_tests.rs"]
mod tests;
