//! `EDIT`'s help: its topics, written for this editor, and the window that
//! shows one over the text, scrolled by the arrows and the page keys and
//! closed with Esc. A dialog's Help button opens the topic about it, and
//! the dialog is waiting when the help closes.

use super::view::{Cells, Palette};
use crate::frames::record;

/// The topics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Topic {
    #[default]
    Started,
    Keys,
    Open,
    SaveAs,
    Find,
    Change,
    Print,
    Display,
    Unsaved,
}

impl Topic {
    const ALL: [Self; 9] = [
        Self::Started,
        Self::Keys,
        Self::Open,
        Self::SaveAs,
        Self::Find,
        Self::Change,
        Self::Print,
        Self::Display,
        Self::Unsaved,
    ];

    /// The topic `byte` numbers, Getting Started for one no topic has.
    #[must_use]
    pub fn of(byte: u8) -> Self {
        Self::ALL
            .get(usize::from(byte))
            .copied()
            .unwrap_or_default()
    }

    #[must_use]
    pub fn number(self) -> u8 {
        u8::try_from(
            Self::ALL
                .iter()
                .position(|&topic| topic == self)
                .unwrap_or(0),
        )
        .unwrap_or(0)
    }

    /// The topic's title, as the help window's frame shows it.
    #[must_use]
    pub const fn title(self) -> &'static [u8] {
        match self {
            Self::Started => b"HELP: Getting Started",
            Self::Keys => b"HELP: Keyboard",
            Self::Open => b"HELP: Open",
            Self::SaveAs => b"HELP: Save As",
            Self::Find => b"HELP: Find",
            Self::Change => b"HELP: Change",
            Self::Print => b"HELP: Print",
            Self::Display => b"HELP: Display",
            Self::Unsaved => b"HELP: Saving Changes",
        }
    }

    /// The topic's lines.
    #[must_use]
    pub const fn lines(self) -> &'static [&'static [u8]] {
        match self {
            Self::Started => STARTED,
            Self::Keys => KEYS,
            Self::Open => OPEN,
            Self::SaveAs => SAVE_AS,
            Self::Find => FIND,
            Self::Change => CHANGE,
            Self::Print => PRINT,
            Self::Display => DISPLAY,
            Self::Unsaved => UNSAVED,
        }
    }
}

numbered!(Topic);

const STARTED: &[&[u8]] = &[
    b"Welcome to the TAIRiX DOS Editor.",
    b"",
    b"Type to put text in at the cursor. To use a menu, press and release",
    b"Alt, or press F10, then the menu's highlighted letter; or hold Alt and",
    b"press the letter; or click the menu's name. In a menu, press an entry's",
    b"highlighted letter, or move to it and press Enter. Esc closes a menu or",
    b"a dialog without doing anything.",
    b"",
    b"Hold Shift while moving the cursor to select text, or drag across it",
    b"with the mouse. Shift+Del cuts the selection to the clipboard, Ctrl+Ins",
    b"copies it there, and Shift+Ins pastes the clipboard at the cursor.",
    b"",
    b"To start the editor with a file, name it on the command line:",
    b"",
    b"    EDIT [[drive:][path]filename] [/B] [/G] [/H] [/NOHI]",
    b"",
    b"    /B     Use a monochrome monitor with a colour adapter.",
    b"    /G     Write to a CGA display as fast as it allows.",
    b"    /H     Show as many lines as the display can.",
    b"    /NOHI  Use a monitor without bright colours.",
    b"",
    b"A file named that does not exist is created when it is first saved.",
    b"",
    b"For every key the editor answers, choose Keyboard from the Help menu.",
    b"Press Esc to leave Help.",
];

const KEYS: &[&[u8]] = &[
    b"Moving the cursor             Editing",
    b"  Left, Right    Ctrl+S, D      Insert or type over    Ins, Ctrl+V",
    b"  Up, Down       Ctrl+E, X      Delete a character     Del, Ctrl+G",
    b"  Word left      Ctrl+Left, A   Delete the one before  Backspace, Ctrl+H",
    b"  Word right     Ctrl+Right, F  Delete the line        Ctrl+Y",
    b"  Indentation    Home           Delete to line's end   Ctrl+Q,Y",
    b"  Line's start   Ctrl+Q,S       Split the line here    Ctrl+N",
    b"  Line's end     End, Ctrl+Q,D  Next line's start      Ctrl+Enter, Ctrl+J",
    b"  Window's top   Ctrl+Q,E       Indent the selection   Tab",
    b"  Window's foot  Ctrl+Q,X       Outdent it             Shift+Tab",
    b"  Start of file  Ctrl+Home, Ctrl+Q,R",
    b"  End of file    Ctrl+End, Ctrl+Q,C",
    b"",
    b"Scrolling                     The clipboard",
    b"  A line up      Ctrl+W         Cut the selection      Shift+Del",
    b"  A line down    Ctrl+Z         Copy it                Ctrl+Ins",
    b"  A page up      PgUp, Ctrl+R   Paste                  Shift+Ins",
    b"  A page down    PgDn, Ctrl+C   Delete the selection   Del",
    b"  A window left  Ctrl+PgUp",
    b"  A window right Ctrl+PgDn",
    b"",
    b"Selecting: hold Shift with any of the moves above.",
    b"",
    b"Searching: Find is Ctrl+Q,F, Change is Ctrl+Q,A, and F3 or Ctrl+L",
    b"repeats the last Find.",
    b"",
    b"Bookmarks: Ctrl+K and a digit 0 to 3 sets one where the cursor stands;",
    b"Ctrl+Q and the digit goes back to it.",
    b"",
    b"A control character: Ctrl+P, then the key, puts the character in.",
    b"",
    b"Menus: Alt or F10, then the letter. Help: F1. Leave Help: Esc.",
];

const OPEN: &[&[u8]] = &[
    b"Open reads a file into the editor in place of the document.",
    b"",
    b"File Name   Type a file's name, with its drive and path if it is not",
    b"            in the directory shown, and press Enter. A name with * or",
    b"            ? in it lists the files that match it instead.",
    b"Files       The files the directory holds that match the name. Move",
    b"            to one to take its name; press Enter or double-click it",
    b"            to open it.",
    b"Dirs/Drives Choose a directory, .. for the one holding this, or a",
    b"            drive, and press Enter or double-click to go there.",
];

const SAVE_AS: &[&[u8]] = &[
    b"Save As writes the document to a file of the name you give.",
    b"",
    b"File Name   Type the name to save under, with its drive and path if",
    b"            it is not to be in the directory shown. A file already",
    b"            there of that name is replaced only once you agree.",
    b"Dirs/Drives Choose a directory or drive to save in, and press Enter",
    b"            or double-click to go there.",
    b"",
    b"The file is written in full before the old one is replaced, so a",
    b"full disk never leaves you with neither.",
];

const FIND: &[&[u8]] = &[
    b"Find looks for text from the cursor to the end of the document, and",
    b"then from its start, and selects what it finds.",
    b"",
    b"Find What              The text to look for.",
    b"Match Upper/Lowercase  Find only text in the same capitals.",
    b"Whole Word             Find it only where it is not part of a word.",
    b"",
    b"F3 finds it again.",
];

const CHANGE: &[&[u8]] = &[
    b"Change finds text and puts other text in its place.",
    b"",
    b"Find What              The text to look for.",
    b"Change To              What to put in its place.",
    b"Match Upper/Lowercase  Change only text in the same capitals.",
    b"Whole Word             Change it only where it is not part of a word.",
    b"",
    b"Find and Verify shows each place it is found and asks whether to",
    b"change it there. Change All changes every place without asking.",
];

const PRINT: &[&[u8]] = &[
    b"Print sends the document to the printer.",
    b"",
    b"Selected Text Only  Print only what is selected.",
    b"Complete Document   Print all of it.",
];

const DISPLAY: &[&[u8]] = &[
    b"Display sets how the editor looks. It remembers what you choose.",
    b"",
    b"Foreground   The colour of the text.",
    b"Background   The colour behind it.",
    b"Scroll Bars  Whether the window shows its scroll bars.",
    b"Tab Stops    How many columns apart the tab stops are, 1 to 99.",
];

const UNSAVED: &[&[u8]] = &[
    b"The document has changes that have not been saved.",
    b"",
    b"Yes     Save them first, then carry on.",
    b"No      Carry on, and lose them.",
    b"Cancel  Go back to the document.",
];

record! {
    /// The help window's state: its topic and the first line shown.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Help {
        topic: Topic,
        top: u16,
    }
}

/// The keys the help window answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelpKey {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

/// The rows the window shows text in on a screen of `rows`.
#[must_use]
pub const fn shown(rows: u8) -> u8 {
    rows.saturating_sub(4)
}

impl Help {
    /// Topic `topic`, from its first line.
    #[must_use]
    pub const fn of(topic: Topic) -> Self {
        Self { topic, top: 0 }
    }

    fn last_top(self, rows: u8) -> u16 {
        let lines = u16::try_from(self.topic.lines().len()).unwrap_or(u16::MAX);
        lines.saturating_sub(u16::from(shown(rows)))
    }

    /// `key` given to the window on a screen of `rows`.
    pub fn key(&mut self, key: HelpKey, rows: u8) {
        let page = u16::from(shown(rows).max(1));
        let top = match key {
            HelpKey::Up => self.top.saturating_sub(1),
            HelpKey::Down => self.top.saturating_add(1),
            HelpKey::PageUp => self.top.saturating_sub(page),
            HelpKey::PageDown => self.top.saturating_add(page),
            HelpKey::Home => 0,
            HelpKey::End => u16::MAX,
        };
        self.top = top.min(self.last_top(rows));
    }

    /// The window drawn over the text window.
    pub fn draw(self, cells: &mut Cells, palette: &Palette) {
        let (rows, columns) = (cells.rows, cells.columns);
        cells.frame(
            1,
            0,
            rows - 2,
            columns - 1,
            palette.dialog,
            self.topic.title(),
        );
        let lines = self.topic.lines().iter().skip(usize::from(self.top));
        for (row, line) in (2..2 + shown(rows)).zip(lines) {
            cells.write(row, 2, line, palette.dialog);
        }
    }
}

/// The status line's hint while help is shown.
pub const HINT: &[u8] = b"<Esc=Cancel>  <PgUp=Back>  <PgDn=More>";

#[cfg(test)]
#[path = "help_tests.rs"]
mod tests;
