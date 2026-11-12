//! `EDIT` whole, and pure: each key and each mouse report taken to the
//! text, the menu bar, an open menu, a dialog or the help, and the command
//! it gives carried out on the document. What only DOS can do — reading a
//! directory, a file, writing one, printing — is asked of the program as an
//! [`Action`], and its outcome comes back as a [`Done`]. The screen and the
//! cursor are drawn from this state alone, so the editor is the same
//! whether its state was just made or read back from the program's memory.

use super::dialogs::{Canvas, Dialog, DialogKey, Field, Kind, Lists, Source, COLOURS};
use super::editor::{Edit, Editor, Move, Spot};
use super::help::{Help, HelpKey, Topic};
use super::keys::{own, Press};
use super::menus::{self, Able, Command, MenuKey};
use super::text::{Store, Text, CR, LF};
use super::view::{self, menu_columns, Bar, Cells, Look, Palette, BAR};
use crate::dos::char::scan;
use crate::frames::record;
use alloc::vec::Vec;

/// Where the keys go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Focus {
    #[default]
    Text,
    /// The menu bar, a menu chosen but not open.
    Bar,
    Menu,
    Dialog,
    Help,
}

/// Alt: up, down with no other key since, or down after one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Alt {
    #[default]
    Up,
    Alone,
    Used,
}

/// A two-key command begun: Ctrl+Q's, Ctrl+K's, or Ctrl+P's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Prefix {
    #[default]
    None,
    Quick,
    Block,
    Quote,
}

/// What waits on the question an unsaved document asks, or on the save it
/// asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Then {
    #[default]
    Nothing,
    New,
    Open,
    Exit,
}

/// The palettes the switches choose.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Shade {
    #[default]
    Colour,
    Mono,
    Plain,
}

/// Numbers each of the enums named by its place in its list of variants.
macro_rules! listed {
    ($($ty:ident [$($variant:ident),*]),* $(,)?) => {$(
        impl $ty {
            const ALL: &'static [Self] = &[$(Self::$variant),*];

            /// The variant `byte` numbers, the first for a number none has.
            #[must_use]
            pub fn of(byte: u8) -> Self {
                Self::ALL.get(usize::from(byte)).copied().unwrap_or_default()
            }

            #[must_use]
            pub fn number(self) -> u8 {
                u8::try_from(Self::ALL.iter().position(|&each| each == self).unwrap_or(0)).unwrap_or(0)
            }
        }
    )*};
}

listed!(
    Focus [Text, Bar, Menu, Dialog, Help],
    Alt [Up, Alone, Used],
    Prefix [None, Quick, Block, Quote],
    Then [Nothing, New, Open, Exit],
    Shade [Colour, Mono, Plain],
);
numbered!(Focus, Alt, Prefix, Then, Shade);

record! {
    /// What Display sets, and the editor keeps between runs.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Settings {
        /// The text's attribute: its background in the high nibble.
        colour: u8,
        bars: bool,
        tab: u8,
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            colour: 0x17,
            bars: true,
            tab: super::editor::TAB,
        }
    }
}

record! {
    /// The last search: what was looked for, what to change it to, and its
    /// check boxes.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Search {
        find: Field,
        change: Field,
        flags: u8,
    }
}

record! {
    /// A change being verified: where it stops once it has wrapped to the
    /// document's start, whether it has, whether it has found any, and how
    /// many it changed.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Changing {
        limit: u32,
        wrapped: bool,
        found: bool,
        changed: u16,
    }
}

record! {
    /// A bookmark's line and column.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Place {
        line: u32,
        column: u16,
    }
}

record! {
    /// The mouse as the editor last saw it: its button held, a selection
    /// being dragged, and the tick and cell of the last press, to tell a
    /// double click.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Mouse {
        held: bool,
        dragging: bool,
        pressed: u16,
        row: u8,
        column: u8,
    }
}

record! {
    /// The screen: its rows and columns, and the palette it is drawn in.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Screen {
        rows: u8,
        columns: u8,
        shade: Shade,
    }
}

record! {
    /// The marks: four bookmarks, any unset.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Marks {
        first: Option<Place>,
        second: Option<Place>,
        third: Option<Place>,
        fourth: Option<Place>,
    }
}

impl Marks {
    fn get(&self, n: u8) -> Option<Place> {
        [self.first, self.second, self.third, self.fourth]
            .get(usize::from(n))
            .copied()
            .flatten()
    }

    fn set(&mut self, n: u8, place: Place) {
        let slot = match n {
            0 => &mut self.first,
            1 => &mut self.second,
            2 => &mut self.third,
            _ => &mut self.fourth,
        };
        *slot = Some(place);
    }
}

record! {
    /// The editor's state.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Ui {
        editor: Editor,
        focus: Focus,
        /// The menu bar entry chosen, and the entry of its menu.
        menu: u8,
        item: u8,
        alt: Alt,
        prefix: Prefix,
        then: Then,
        dialog: Dialog,
        help: Help,
        /// The help shown is a dialog's, the dialog waiting beneath it.
        over: bool,
        settings: Settings,
        search: Search,
        changing: Changing,
        marks: Marks,
        mouse: Mouse,
        /// The document's file, none while it is untitled.
        name: Field,
        /// The file a save writes.
        target: Field,
        /// The directory a dialog's lists show, and the files Open lists.
        path: Field,
        pattern: Field,
        /// The clipboard's bytes, at the store's end.
        clip: u32,
        /// The entries of a dialog's listing below it: files, then
        /// directories and drives.
        files: u16,
        others: u16,
        screen: Screen,
    }
}

/// What the editor asks of the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing until the next key.
    Wait,
    /// The dialog's directory listed: its path, then the entries.
    List,
    /// What the dialog's name names opened: a file read in, or a
    /// directory, drive or pattern listed.
    Open,
    /// What it names saved to: a directory, drive or pattern listed, else
    /// the file saved unless it is another already there.
    SaveAs,
    /// The document written to the target, any file there replaced.
    Save,
    Print {
        selection: bool,
    },
    /// The display settings kept for the next run.
    Settings,
    Exit,
}

/// What came of an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Done {
    Listed,
    /// The file named is the document now.
    Loaded,
    /// The name Save As was given is a file already there, now the target.
    Exists,
    Saved,
    Printed,
    Stored,
    /// It failed with the DOS error, or one of [`failure`]'s.
    Failed(u8),
}

/// The failures that are not DOS's own errors.
pub mod failure {
    /// The disk took fewer bytes than it was given.
    pub const DISK_FULL: u8 = 0xF0;
    /// The file is larger than the editor's memory.
    pub const TOO_LARGE: u8 = 0xF1;
    /// The document, or the clipboard, has no room.
    pub const MEMORY: u8 = 0xF2;
    /// The file was written under the target's name, but its old file
    /// could not be replaced: the document is in the file the target names.
    pub const KEPT: u8 = 0xF3;
    /// A name to save under holds a wildcard.
    pub const BAD_NAME: u8 = 0xF4;
    /// A device took fewer bytes than it was given.
    pub const DEVICE: u8 = 0xF5;
}

impl Action {
    const PRINT: u32 = 6;
    const PRINT_SELECTION: u32 = 7;

    /// The action as a routine's outcome gives it.
    #[must_use]
    pub const fn number(self) -> u32 {
        match self {
            Self::Wait => 0,
            Self::List => 2,
            Self::Open => 3,
            Self::SaveAs => 4,
            Self::Save => 5,
            Self::Print { selection: false } => Self::PRINT,
            Self::Print { selection: true } => Self::PRINT_SELECTION,
            Self::Settings => 8,
            Self::Exit => 9,
        }
    }

    /// The action `number` gives, waiting for one no action has.
    #[must_use]
    pub const fn of(number: u32) -> Self {
        match number {
            2 => Self::List,
            3 => Self::Open,
            4 => Self::SaveAs,
            5 => Self::Save,
            Self::PRINT => Self::Print { selection: false },
            Self::PRINT_SELECTION => Self::Print { selection: true },
            8 => Self::Settings,
            9 => Self::Exit,
            _ => Self::Wait,
        }
    }
}

impl Done {
    const FAILED: u32 = 0x100;

    /// What came of an action as a routine's outcome gives it.
    #[must_use]
    pub fn number(self) -> u32 {
        match self {
            Self::Listed => 0,
            Self::Loaded => 1,
            Self::Exists => 2,
            Self::Saved => 3,
            Self::Printed => 4,
            Self::Stored => 5,
            Self::Failed(code) => Self::FAILED | u32::from(code),
        }
    }

    /// What `number` gives; a number no outcome has is a device's fault.
    #[must_use]
    pub const fn of(number: u32) -> Self {
        match number {
            0 => Self::Listed,
            1 => Self::Loaded,
            2 => Self::Exists,
            3 => Self::Saved,
            4 => Self::Printed,
            5 => Self::Stored,
            _ if number & !0xFF == Self::FAILED => Self::Failed(number.to_le_bytes()[0]),
            _ => Self::Failed(failure::DEVICE),
        }
    }
}

impl Screen {
    /// The screen as a routine's outcome gives it: its rows, columns and
    /// shade a byte each.
    #[must_use]
    pub fn packed(self) -> u32 {
        u32::from_le_bytes([self.rows, self.columns, self.shade.number(), 0])
    }

    #[must_use]
    pub fn unpacked(number: u32) -> Self {
        let [rows, columns, shade, _] = number.to_le_bytes();
        Self {
            rows,
            columns,
            shade: Shade::of(shade),
        }
    }
}

/// What the mouse did since it was last asked: the left button's presses
/// and releases, where the last of each was, whether it is held now and
/// where the pointer is, the shift flags, and the BIOS's tick count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pointer {
    pub presses: u16,
    pub pressed_at: (u8, u8),
    pub releases: u16,
    pub held: bool,
    pub at: (u8, u8),
    pub flags: u8,
    pub now: u16,
}

/// What the editor is given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// A code INT 16h read, and the shift flags beside it.
    Key {
        code: u16,
        flags: u8,
    },
    Pointer(Pointer),
}

/// The ticks within which a second press makes a double click.
const DOUBLE_TICKS: u16 = 9;

/// What the status line says.
mod hint {
    pub const TEXT: &[u8] = b"TAIRiX DOS Editor  <F1=Help> Press ALT to activate menus";
    pub const HELP_KEY: &[u8] = b"<F1=Help>";
    pub const BAR: &[u8] = b"F1=Help  Enter=Display Menu  Esc=Cancel  Arrow=Next Item";
    pub const DIALOG: &[u8] =
        b"F1=Help  Enter=Execute  Esc=Cancel  Tab=Next Field  Arrow=Next Item";
    pub const ESCAPE: &[u8] = b"<Esc=Cancel>";
}

/// A dialog listing's entry in the store: its kind, then its name.
pub mod entry {
    pub const LEN: usize = 13;
    pub const FILE: u8 = 0;
    pub const DIRECTORY: u8 = 1;
    pub const DRIVE: u8 = 2;
}

/// The files Open lists at first.
const PATTERN: &[u8] = b"*.TXT";

impl Ui {
    /// The editor at its start on `screen`, with `settings`.
    #[must_use]
    pub fn new(screen: Screen, settings: Settings) -> Self {
        let (rows, columns) = view::window(screen.rows, screen.columns);
        let editor = Editor {
            rows: u16::from(rows),
            columns: u16::from(columns),
            tab: settings.tab,
            ..Editor::default()
        };
        let mut pattern = Field::default();
        pattern.set(PATTERN);
        Self {
            editor,
            focus: Focus::Text,
            menu: 0,
            item: 0,
            alt: Alt::Up,
            prefix: Prefix::None,
            then: Then::Nothing,
            dialog: Dialog::default(),
            help: Help::default(),
            over: false,
            settings,
            search: Search::default(),
            changing: Changing::default(),
            marks: Marks::default(),
            mouse: Mouse::default(),
            name: Field::default(),
            target: Field::default(),
            path: Field::default(),
            pattern,
            clip: 0,
            files: 0,
            others: 0,
            screen,
        }
    }

    /// The colours it is drawn in.
    #[must_use]
    pub fn palette(&self) -> Palette {
        let palette = match self.screen.shade {
            Shade::Colour => Palette::COLOUR,
            Shade::Mono => Palette::MONO,
            Shade::Plain => Palette::PLAIN,
        };
        palette.with_text(self.settings.colour)
    }

    /// `input` taken: what the program is to do next.
    pub fn input<S: Store>(&mut self, input: Input, text: &mut Text<'_, S>) -> Action {
        match input {
            Input::Key { code, flags } => self.key(Press::of(code, flags), text),
            Input::Pointer(pointer) => self.pointer(pointer, text),
        }
    }

    fn key<S: Store>(&mut self, press: Press, text: &mut Text<'_, S>) -> Action {
        if press.is(own::ALT_DOWN) {
            self.alt = Alt::Alone;
            return Action::Wait;
        }
        if press.is(own::ALT_UP) {
            let alone = self.alt == Alt::Alone;
            self.alt = Alt::Up;
            return if alone {
                self.alt_tapped()
            } else {
                Action::Wait
            };
        }
        if self.alt == Alt::Alone {
            self.alt = Alt::Used;
        }
        match self.focus {
            Focus::Text => self.text_key(press, text),
            Focus::Bar => self.bar_key(press, text),
            Focus::Menu => self.menu_key(press, text),
            Focus::Dialog => self.dialog_key(press, text),
            Focus::Help => self.help_key(press),
        }
    }

    /// Alt pressed and let go alone: the menu bar taken or given up.
    fn alt_tapped(&mut self) -> Action {
        match self.focus {
            Focus::Text => {
                self.focus = Focus::Bar;
                self.menu = 0;
            }
            Focus::Bar | Focus::Menu => self.focus = Focus::Text,
            Focus::Dialog | Focus::Help => {}
        }
        Action::Wait
    }

    /// The editing of the document with the editor's state.
    pub fn edit<'e, 't, S: Store>(&'e mut self, text: &'e mut Text<'t, S>) -> Edit<'e, 't, S> {
        Edit {
            editor: &mut self.editor,
            text,
        }
    }

    fn go<S: Store>(&mut self, how: Move, select: bool, text: &mut Text<'_, S>) -> Action {
        self.edit(text).go(how, select);
        Action::Wait
    }

    fn text_key<S: Store>(&mut self, press: Press, text: &mut Text<'_, S>) -> Action {
        let prefix = core::mem::take(&mut self.prefix);
        if prefix != Prefix::None {
            return self.prefixed(prefix, press, text);
        }
        if press.alt {
            return match press.letter().and_then(menus::menu_for) {
                Some(menu) => self.drop_menu(menu, text),
                None => Action::Wait,
            };
        }
        if press.is(own::COPY) {
            return self.command(Command::Copy, text);
        }
        match press.char {
            None => self.text_special(press, text),
            Some(byte) => self.text_char(press, byte, text),
        }
    }

    /// A key with no character in the text.
    fn text_special<S: Store>(&mut self, press: Press, text: &mut Text<'_, S>) -> Action {
        let select = press.shift;
        let how = match press.scan {
            scan::F1 => return self.show_help(Topic::Started, false),
            scan::F3 => return self.command(Command::Repeat, text),
            scan::F10 => return self.alt_tapped(),
            scan::INSERT if press.shift => return self.command(Command::Paste, text),
            scan::INSERT => {
                self.editor.insert = !self.editor.insert;
                return Action::Wait;
            }
            scan::DELETE if press.shift => return self.command(Command::Cut, text),
            scan::DELETE => {
                let done = self.edit(text).delete();
                return self.edited(done, text);
            }
            scan::TAB => {
                self.edit(text).outdent();
                return Action::Wait;
            }
            scan::LEFT => Move::Left,
            scan::RIGHT => Move::Right,
            scan::UP => Move::Up,
            scan::DOWN => Move::Down,
            scan::HOME => Move::Home,
            scan::END => Move::End,
            scan::PAGE_UP => Move::PageUp,
            scan::PAGE_DOWN => Move::PageDown,
            scan::CTRL_LEFT => Move::WordLeft,
            scan::CTRL_RIGHT => Move::WordRight,
            scan::CTRL_HOME => Move::Top,
            scan::CTRL_END => Move::Bottom,
            scan::CTRL_PAGE_UP => Move::PageLeft,
            scan::CTRL_PAGE_DOWN => Move::PageRight,
            _ => return Action::Wait,
        };
        self.go(how, select, text)
    }

    /// A key with a character in the text.
    fn text_char<S: Store>(&mut self, press: Press, byte: u8, text: &mut Text<'_, S>) -> Action {
        match (press.scan, byte) {
            (scan::ENTER, CR) => {
                let done = self.edit(text).enter();
                self.edited(done, text)
            }
            (scan::ENTER, LF) => self.go(Move::NextLine, press.shift, text),
            (scan::BACKSPACE, _) => {
                self.edit(text).back();
                Action::Wait
            }
            (scan::TAB, _) => {
                let mut edit = self.edit(text);
                let done = if edit.selection().is_some() {
                    edit.indent()
                } else {
                    edit.type_in(b"\t")
                };
                self.edited(done, text)
            }
            (scan::ESC, _) => Action::Wait,
            (0, _) | (_, 0x20..) => self.type_byte(byte, text),
            (_, 1..=26) => match press.letter() {
                Some(letter) => self.wordstar(letter, press.shift, text),
                None => Action::Wait,
            },
            _ => Action::Wait,
        }
    }

    /// What an edit that may find the document full came to: the message
    /// that it is, else nothing more.
    pub(super) fn edited<S: Store>(
        &mut self,
        done: Result<(), super::text::Full>,
        text: &mut Text<'_, S>,
    ) -> Action {
        if done.is_err() {
            return self.failed(failure::MEMORY, text);
        }
        Action::Wait
    }

    /// `byte` typed at the cursor.
    fn type_byte<S: Store>(&mut self, byte: u8, text: &mut Text<'_, S>) -> Action {
        let done = self.edit(text).type_in(&[byte]);
        self.edited(done, text)
    }

    /// Ctrl and a letter: WordStar's keys.
    fn wordstar<S: Store>(&mut self, letter: u8, select: bool, text: &mut Text<'_, S>) -> Action {
        let how = match letter {
            b'S' => Move::Left,
            b'D' => Move::Right,
            b'E' => Move::Up,
            b'X' => Move::Down,
            b'A' => Move::WordLeft,
            b'F' => Move::WordRight,
            b'R' => Move::PageUp,
            b'C' => Move::PageDown,
            b'J' => Move::NextLine,
            b'W' | b'Z' => {
                self.edit(text).scroll_line(letter == b'Z');
                return Action::Wait;
            }
            b'G' => {
                let done = self.edit(text).delete();
                return self.edited(done, text);
            }
            b'H' => {
                self.edit(text).back();
                return Action::Wait;
            }
            b'V' => {
                self.editor.insert = !self.editor.insert;
                return Action::Wait;
            }
            b'Y' => {
                let line = self.edit(text).cut_line();
                return self.keep(&line, text);
            }
            b'N' => {
                let done = self.edit(text).split();
                return self.edited(done, text);
            }
            b'L' => return self.command(Command::Repeat, text),
            b'Q' | b'K' | b'P' => {
                self.prefix = match letter {
                    b'Q' => Prefix::Quick,
                    b'K' => Prefix::Block,
                    _ => Prefix::Quote,
                };
                return Action::Wait;
            }
            _ => return Action::Wait,
        };
        self.go(how, select, text)
    }

    /// The key after Ctrl+Q, Ctrl+K or Ctrl+P.
    fn prefixed<S: Store>(
        &mut self,
        prefix: Prefix,
        press: Press,
        text: &mut Text<'_, S>,
    ) -> Action {
        if prefix == Prefix::Quote {
            return match press.char {
                Some(byte) => self.type_byte(byte, text),
                None => Action::Wait,
            };
        }
        let Some(letter) = press.letter() else {
            return Action::Wait;
        };
        let mark = letter.checked_sub(b'0').filter(|&n| n < 4);
        if prefix == Prefix::Block {
            if let Some(n) = mark {
                let cursor = self.editor.cursor;
                self.marks.set(
                    n,
                    Place {
                        line: cursor.line,
                        column: cursor.column,
                    },
                );
            }
            return Action::Wait;
        }
        let how = match letter {
            b'S' => Move::LineStart,
            b'D' => Move::End,
            b'E' => Move::WindowTop,
            b'X' => Move::WindowBottom,
            b'R' => Move::Top,
            b'C' => Move::Bottom,
            b'Y' => {
                let cut = self.edit(text).cut_to_end();
                return self.keep(&cut, text);
            }
            b'F' => return self.command(Command::Find, text),
            b'A' => return self.command(Command::Change, text),
            _ => {
                if let Some(place) = mark.and_then(|n| self.marks.get(n)) {
                    self.edit(text).go_to(place.line, place.column);
                }
                return Action::Wait;
            }
        };
        self.go(how, press.shift, text)
    }

    /// The menu bar's keys, no menu open.
    fn bar_key<S: Store>(&mut self, press: Press, text: &mut Text<'_, S>) -> Action {
        let count = u8::try_from(BAR.len()).unwrap_or(1);
        match (press.char, press.scan) {
            (None, scan::LEFT) => self.menu = (self.menu + count - 1) % count,
            (None, scan::RIGHT) => self.menu = (self.menu + 1) % count,
            (None, scan::DOWN | scan::UP) | (Some(CR), _) => {
                return self.drop_menu(self.menu, text)
            }
            (None, scan::F1) => return self.show_help(Topic::Started, false),
            (Some(0x1B), _) => self.focus = Focus::Text,
            _ => {
                if let Some(menu) = press.letter().and_then(menus::menu_for) {
                    return self.drop_menu(menu, text);
                }
            }
        }
        Action::Wait
    }

    /// Menu `menu` opened at its first entry that can be chosen.
    fn drop_menu<S: Store>(&mut self, menu: u8, text: &mut Text<'_, S>) -> Action {
        self.focus = Focus::Menu;
        self.menu = menu;
        self.item = menus::first(menu, self.able(text));
        Action::Wait
    }

    /// What the commands may be chosen for now.
    fn able<S: Store>(&self, text: &mut Text<'_, S>) -> Able {
        let mut editor = self.editor;
        let selection = Edit {
            editor: &mut editor,
            text,
        }
        .selection()
        .is_some();
        Able {
            selection,
            clipboard: self.clip > 0,
            searched: self.search.find.len > 0,
        }
    }

    fn menu_key<S: Store>(&mut self, press: Press, text: &mut Text<'_, S>) -> Action {
        let able = self.able(text);
        let key = match (press.char, press.scan) {
            (None, scan::UP) => MenuKey::Up,
            (None, scan::DOWN) => MenuKey::Down,
            (None, scan::LEFT) => MenuKey::Left,
            (None, scan::RIGHT) => MenuKey::Right,
            (None, scan::F1) => return self.show_help(Topic::Started, false),
            (Some(CR), _) => MenuKey::Enter,
            (Some(0x1B), _) => MenuKey::Escape,
            _ => match press.letter() {
                Some(letter) => MenuKey::Letter(letter),
                None => return Action::Wait,
            },
        };
        match menus::key(self.menu, self.item, key, able) {
            menus::Outcome::At(menu, item) => {
                self.menu = menu;
                self.item = item;
                Action::Wait
            }
            menus::Outcome::Chosen(command) => {
                self.focus = Focus::Text;
                self.command(command, text)
            }
            menus::Outcome::Closed => {
                self.focus = Focus::Text;
                Action::Wait
            }
            menus::Outcome::Ignored => Action::Wait,
        }
    }

    fn dialog_key<S: Store>(&mut self, press: Press, text: &mut Text<'_, S>) -> Action {
        let key = match (press.char, press.scan) {
            (None, scan::F1) => return self.dialog_help(),
            (None, scan::TAB) => DialogKey::BackTab,
            (None, scan::LEFT) => DialogKey::Left,
            (None, scan::RIGHT) => DialogKey::Right,
            (None, scan::UP) => DialogKey::Up,
            (None, scan::DOWN) => DialogKey::Down,
            (None, scan::HOME) => DialogKey::Home,
            (None, scan::END) => DialogKey::End,
            (None, scan::PAGE_UP) => DialogKey::PageUp,
            (None, scan::PAGE_DOWN) => DialogKey::PageDown,
            (None, scan::DELETE) => DialogKey::Delete,
            (None, _) if press.alt => match press.letter() {
                Some(letter) => DialogKey::Alt(letter),
                None => return Action::Wait,
            },
            (Some(_), scan::TAB) => DialogKey::Tab,
            (Some(_), scan::BACKSPACE) => DialogKey::Backspace,
            (Some(CR), _) => DialogKey::Enter,
            (Some(0x1B), _) => DialogKey::Escape,
            (Some(byte), _) => DialogKey::Char(byte),
            (None, _) => return Action::Wait,
        };
        let outcome = self.dialog.key(key, &mut self.entries(text));
        self.outcome(outcome, text)
    }

    fn help_key(&mut self, press: Press) -> Action {
        let key = match (press.char, press.scan) {
            (Some(0x1B), _) => return self.close_help(),
            (None, scan::UP) => HelpKey::Up,
            (None, scan::DOWN) => HelpKey::Down,
            (None, scan::PAGE_UP) => HelpKey::PageUp,
            (None, scan::PAGE_DOWN) => HelpKey::PageDown,
            (None, scan::HOME | scan::CTRL_HOME) => HelpKey::Home,
            (None, scan::END | scan::CTRL_END) => HelpKey::End,
            _ => return Action::Wait,
        };
        self.help.key(key, self.screen.rows);
        Action::Wait
    }

    /// Help on `topic`, over the dialog where `over`.
    pub(super) fn show_help(&mut self, topic: Topic, over: bool) -> Action {
        self.help = Help::of(topic);
        self.over = over;
        self.focus = Focus::Help;
        Action::Wait
    }

    fn close_help(&mut self) -> Action {
        self.focus = if self.over {
            Focus::Dialog
        } else {
            Focus::Text
        };
        self.over = false;
        Action::Wait
    }

    /// The topic about the dialog open.
    pub(super) fn dialog_help(&mut self) -> Action {
        let topic = match self.dialog.kind {
            Kind::Open => Topic::Open,
            Kind::SaveAs | Kind::Overwrite => Topic::SaveAs,
            Kind::Find => Topic::Find,
            Kind::Change | Kind::Verify => Topic::Change,
            Kind::Print => Topic::Print,
            Kind::Display => Topic::Display,
            Kind::Unsaved => Topic::Unsaved,
            Kind::None | Kind::Message | Kind::About => Topic::Started,
        };
        self.show_help(topic, true)
    }

    /// The entries the dialog's lists show.
    pub(super) fn entries<'a, 't, S: Store>(
        &self,
        text: &'a mut Text<'t, S>,
    ) -> Entries<'a, 't, S> {
        Entries {
            files: self.files,
            others: self.others,
            clip: self.clip,
            text,
        }
    }

    /// The document's title: its file's name, or `Untitled`.
    #[must_use]
    pub fn title(&self) -> Vec<u8> {
        let name = self.name.text();
        if name.is_empty() {
            return b"Untitled".to_vec();
        }
        let from = name
            .iter()
            .rposition(|&byte| byte == b'\\' || byte == b':')
            .map_or(0, |at| at + 1);
        name[from..].to_vec()
    }

    fn hint(&self) -> &'static [u8] {
        match self.focus {
            Focus::Text => hint::TEXT,
            Focus::Bar => hint::BAR,
            Focus::Menu => menus::hint(self.menu, self.item),
            Focus::Dialog => hint::DIALOG,
            Focus::Help => super::help::HINT,
        }
    }

    /// The screen drawn.
    pub fn draw<S: Store>(&self, cells: &mut Cells, text: &mut Text<'_, S>) {
        let palette = self.palette();
        let bar = match self.focus {
            Focus::Bar | Focus::Menu => Bar::Chosen(self.menu),
            Focus::Text if self.alt != Alt::Up => Bar::Keys,
            _ => Bar::Plain,
        };
        view::menu_bar(cells, &palette, bar);
        let title = self.title();
        let look = Look {
            palette: &palette,
            title: &title,
            scroll_bars: self.settings.bars,
        };
        view::draw(cells, look, &self.editor, text, self.hint());
        match self.focus {
            Focus::Menu => {
                let able = self.able(text);
                menus::draw(cells, &palette, self.menu, self.item, able);
            }
            Focus::Dialog => {
                let mut entries = self.entries(text);
                self.dialog.draw(&mut Canvas {
                    cells,
                    palette: &palette,
                    path: self.path.text(),
                    lists: &mut entries,
                });
            }
            Focus::Help => self.help.draw(cells, &palette),
            Focus::Text | Focus::Bar => {}
        }
    }

    /// Where the hardware cursor stands, none where it is hidden.
    #[must_use]
    pub fn cursor(&self) -> Option<(u8, u8)> {
        match self.focus {
            Focus::Text => {
                let editor = &self.editor;
                let row = editor
                    .cursor
                    .line
                    .checked_sub(editor.top.line)
                    .filter(|&row| row < u32::from(editor.rows))?;
                let column = editor
                    .cursor
                    .column
                    .checked_sub(editor.left)
                    .filter(|&column| column < editor.columns)?;
                Some((2 + u8::try_from(row).ok()?, 1 + u8::try_from(column).ok()?))
            }
            Focus::Dialog => self.dialog.cursor(self.screen.rows, self.screen.columns),
            Focus::Bar | Focus::Menu | Focus::Help => None,
        }
    }

    /// Whether the cursor is the block an overtyping editor shows.
    #[must_use]
    pub fn overtype(&self) -> bool {
        self.focus == Focus::Text && !self.editor.insert
    }

    fn pointer<S: Store>(&mut self, pointer: Pointer, text: &mut Text<'_, S>) -> Action {
        let mut action = Action::Wait;
        if pointer.presses > 0 {
            let (row, column) = pointer.pressed_at;
            let quick = pointer.now.wrapping_sub(self.mouse.pressed) <= DOUBLE_TICKS;
            let double = pointer.presses > 1
                || (quick && (self.mouse.row, self.mouse.column) == (row, column));
            let pressed = if double {
                pointer.now.wrapping_sub(DOUBLE_TICKS + 1)
            } else {
                pointer.now
            };
            self.mouse = Mouse {
                held: true,
                dragging: false,
                pressed,
                row,
                column,
            };
            action = self.press(pointer.pressed_at, double, pointer.flags, text);
        }
        if pointer.held && self.mouse.dragging {
            self.drag(pointer.at, text);
        }
        if !pointer.held {
            if self.mouse.dragging && self.editor.anchor == Some(self.editor.cursor) {
                self.editor.anchor = None;
            }
            self.mouse.held = false;
            self.mouse.dragging = false;
        }
        action
    }

    /// The left button pressed at `at`.
    fn press<S: Store>(
        &mut self,
        at: (u8, u8),
        double: bool,
        flags: u8,
        text: &mut Text<'_, S>,
    ) -> Action {
        let (rows, columns) = (self.screen.rows, self.screen.columns);
        match self.focus {
            Focus::Dialog => {
                let outcome =
                    self.dialog
                        .click((rows, columns), at, double, &mut self.entries(text));
                self.outcome(outcome, text)
            }
            Focus::Help => {
                if at.0 == rows - 1
                    && (1..=u8::try_from(hint::ESCAPE.len()).unwrap_or(0)).contains(&at.1)
                {
                    return self.close_help();
                }
                let key = if at.0 < rows / 2 {
                    HelpKey::PageUp
                } else {
                    HelpKey::PageDown
                };
                self.help.key(key, rows);
                Action::Wait
            }
            Focus::Menu if at.0 > 0 => self.menu_click(at, text),
            Focus::Text | Focus::Bar | Focus::Menu => {
                if at.0 == 0 {
                    return self.bar_click(at.1, text);
                }
                self.focus = Focus::Text;
                self.text_click(
                    at,
                    double,
                    flags & (crate::bios::shift::LEFT_SHIFT | crate::bios::shift::RIGHT_SHIFT) != 0,
                    text,
                )
            }
        }
    }

    /// A press on the menu bar: its entry's menu opened, or closed when it
    /// is the one open.
    fn bar_click<S: Store>(&mut self, column: u8, text: &mut Text<'_, S>) -> Action {
        let starts = menu_columns(self.screen.columns);
        let found = BAR.iter().zip(starts).position(|(&(label, _), start)| {
            let width = u8::try_from(label.len() - 1).unwrap_or(0);
            (start.saturating_sub(1)..=start + width).contains(&column)
        });
        let Some(menu) = found.and_then(|menu| u8::try_from(menu).ok()) else {
            return Action::Wait;
        };
        if self.focus == Focus::Menu && self.menu == menu {
            self.focus = Focus::Text;
            return Action::Wait;
        }
        self.drop_menu(menu, text)
    }

    /// A press below the bar with a menu open: its entry chosen, or the
    /// menu closed where the press missed it.
    fn menu_click<S: Store>(&mut self, (row, column): (u8, u8), text: &mut Text<'_, S>) -> Action {
        let (top, left, bottom, right) = menus::rect(self.menu, self.screen.columns);
        if !(top + 1..bottom).contains(&row) || !(left..=right).contains(&column) {
            self.focus = Focus::Text;
            return Action::Wait;
        }
        let item = row - top - 1;
        let chosen = menus::MENUS[usize::from(self.menu.min(4))]
            .get(usize::from(item))
            .and_then(|each| each.command);
        match chosen.filter(|&command| self.able(text).allows(command)) {
            Some(command) => {
                self.focus = Focus::Text;
                self.command(command, text)
            }
            None => Action::Wait,
        }
    }

    /// A press below the bar with the text focused: the cursor placed in
    /// the text, a scroll bar moved, or the status line's help taken.
    fn text_click<S: Store>(
        &mut self,
        (row, column): (u8, u8),
        double: bool,
        shift: bool,
        text: &mut Text<'_, S>,
    ) -> Action {
        let (rows, columns) = (self.screen.rows, self.screen.columns);
        let last_text = rows.saturating_sub(3);
        if row == rows - 1 {
            let at = hint::TEXT
                .windows(hint::HELP_KEY.len())
                .position(|window| window == hint::HELP_KEY)
                .unwrap_or(0);
            let start = 1 + u8::try_from(at).unwrap_or(0);
            if (start..start + u8::try_from(hint::HELP_KEY.len()).unwrap_or(0)).contains(&column) {
                return self.show_help(Topic::Started, false);
            }
            return Action::Wait;
        }
        if self.settings.bars && column == columns - 1 && (2..=last_text).contains(&row) {
            return self.vertical_bar(row, text);
        }
        if self.settings.bars && row == rows - 2 && (1..=columns - 2).contains(&column) {
            return self.horizontal_bar(column, text);
        }
        if !(2..=last_text).contains(&row) || !(1..=columns - 2).contains(&column) {
            return Action::Wait;
        }
        if !shift {
            self.editor.anchor = None;
        }
        let anchor = self.editor.anchor.unwrap_or(self.editor.cursor);
        self.place((row, column), text);
        if shift {
            self.editor.anchor = Some(anchor);
        } else if double {
            self.edit(text).select_word();
            return Action::Wait;
        } else {
            self.editor.anchor = Some(self.editor.cursor);
        }
        self.mouse.dragging = true;
        Action::Wait
    }

    /// The cursor put at the window's cell at `at`, on the last line where
    /// the document ends above it.
    fn place<S: Store>(&mut self, (row, column): (u8, u8), text: &mut Text<'_, S>) {
        let mut spot = self.editor.top;
        for _ in 2..row {
            match text.next_line(spot.start) {
                Some(start) => {
                    spot = Spot {
                        start,
                        line: spot.line + 1,
                        column: 0,
                    }
                }
                None => break,
            }
        }
        spot.column = self
            .editor
            .left
            .saturating_add(u16::from(column.saturating_sub(1)));
        self.editor.cursor = spot;
        self.editor.goal = spot.column;
    }

    /// The pointer dragged to `at`, the selection following it, the window
    /// scrolled a line toward it where it is past the window's edge.
    fn drag<S: Store>(&mut self, (row, column): (u8, u8), text: &mut Text<'_, S>) {
        let last_text = self.screen.rows.saturating_sub(3);
        let column = column.clamp(1, self.screen.columns - 2);
        let row = if row < 2 {
            self.edit(text).scroll_up(1);
            2
        } else if row > last_text {
            self.edit(text).scroll_down(1);
            last_text
        } else {
            row
        };
        let anchor = self.editor.anchor;
        self.place((row, column), text);
        self.editor.anchor = anchor;
    }

    /// A press on the right-hand scroll bar: a line at its arrows, a page
    /// either side of its thumb.
    fn vertical_bar<S: Store>(&mut self, row: u8, text: &mut Text<'_, S>) -> Action {
        let (length, _) = view::window(self.screen.rows, self.screen.columns);
        let thumb = 2 + view::thumb(self.editor.cursor.line, self.editor.lines, length);
        let last = 1 + length;
        match row {
            2 => self.edit(text).scroll_line(false),
            _ if row == last => self.edit(text).scroll_line(true),
            _ if row < thumb => self.edit(text).go(Move::PageUp, false),
            _ if row > thumb => self.edit(text).go(Move::PageDown, false),
            _ => {}
        }
        Action::Wait
    }

    /// A press on the foot's scroll bar: a column at its arrows, a window's
    /// width either side of its thumb.
    fn horizontal_bar<S: Store>(&mut self, column: u8, text: &mut Text<'_, S>) -> Action {
        let columns = self.screen.columns;
        let thumb = 1 + view::thumb(
            u32::from(self.editor.cursor.column),
            u32::from(super::editor::MOST_COLUMN) + 1,
            columns - 2,
        );
        let how = match column {
            1 => Move::Left,
            _ if column == columns - 2 => Move::Right,
            _ if column < thumb => Move::PageLeft,
            _ if column > thumb => Move::PageRight,
            _ => return Action::Wait,
        };
        self.go(how, false, text)
    }
}

/// A dialog's lists over the editor's listing and the colours.
pub struct Entries<'a, 't, S: Store> {
    files: u16,
    others: u16,
    clip: u32,
    text: &'a mut Text<'t, S>,
}

impl<S: Store> Entries<'_, '_, S> {
    /// Listing entry `n` as the store holds it.
    fn raw(&mut self, n: u16) -> [u8; entry::LEN] {
        let mut out = [0u8; entry::LEN];
        self.text.read_end(entry_back(self.clip, n), &mut out);
        out
    }
}

/// How far from the store's end listing entry `n` begins, the clipboard
/// `clip` bytes there above the listing.
#[must_use]
pub fn entry_back(clip: u32, n: u16) -> u32 {
    let stride = u32::try_from(entry::LEN).unwrap_or(u32::MAX);
    clip.saturating_add((u32::from(n) + 1).saturating_mul(stride))
}

/// A listing entry as a list shows it.
#[must_use]
pub fn shown(raw: &[u8]) -> Vec<u8> {
    let name = &raw[1..];
    let name = &name[..name
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(name.len())];
    if raw[0] == entry::DRIVE {
        return [b"[-", &name[..1.min(name.len())], b"-]"].concat();
    }
    name.to_vec()
}

impl<S: Store> Lists for Entries<'_, '_, S> {
    fn count(&mut self, source: Source) -> u16 {
        match source {
            Source::Files => self.files,
            Source::Directories => self.others,
            Source::Foreground => 16,
            Source::Background => 8,
        }
    }

    fn entry(&mut self, source: Source, n: u16) -> Vec<u8> {
        match source {
            Source::Files => shown(&self.raw(n)),
            Source::Directories => shown(&self.raw(self.files.saturating_add(n))),
            Source::Foreground | Source::Background => COLOURS
                .get(usize::from(n))
                .map_or_else(Vec::new, |name| name.to_vec()),
        }
    }
}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;
