//! `EDIT`: MS-DOS 5's full-screen text editor, from its manual. What the
//! editor does is pure (`ui` and the modules under it) over a document in a
//! DOS memory block as large as memory allows; this program gives it the
//! screen, the keys, the mouse and the files.
//!
//! The screen is the active page's video memory, written whole after each
//! key from cells the editor composes, the screen it found kept and put
//! back. Keys come from INT 16h with the BIOS's shift flags read beside
//! them, so Shift with a keypad move selects on the 83-key keyboard. An
//! INT 09h filter in the program's memory tells Alt pressed and let go
//! alone from Alt with a key, and gives Ctrl+Ins, which that keyboard's
//! BIOS has no code for; the mouse driver's event handler puts a key of the
//! editor's own in the BIOS's buffer, for the editor to ask the driver what
//! happened. BREAK is set off while it runs, INT 23h is ignored, and a
//! critical error fails the call, the editor saying what failed.

/// Packs each of the enums named as the byte its `number` gives, read back
/// through its `of`.
macro_rules! numbered {
    ($($ty:ty),* $(,)?) => {$(
        impl crate::frames::Field for $ty {
            const SIZE: usize = 1;
            fn take(bytes: &[u8], at: &mut usize) -> Self {
                Self::of(<u8 as crate::frames::Field>::take(bytes, at))
            }
            fn put(self, bytes: &mut [u8], at: &mut usize) {
                crate::frames::Field::put(self.number(), bytes, at);
            }
        }
    )*};
}

mod block;
pub(crate) mod commands;
pub(crate) mod dialogs;
pub(crate) mod editor;
mod files;
#[cfg(test)]
mod fixture;
pub(crate) mod help;
mod hooks;
pub(crate) mod keys;
pub(crate) mod menus;
mod run;
mod screen;
pub(crate) mod settings;
pub(crate) mod text;
pub(crate) mod ui;
pub(crate) mod view;

pub use files::{
    EditList, EditLoad, EditOpen, EditPrint, EditSave, EditSaveAs, EditSettings, EditVisit,
};
pub use hooks::{EditHook, EditPointer, EditUnhook};
pub use run::EditLoop;
pub use screen::{EditRestore, EditScreen};

use super::{refuse, Refusal};
use crate::dos::message;
use crate::dos::program::{say_error, Flow, Frame, Id, Int, Program, Routine};
use crate::frames::{locals, record, take, Field, Outcome};
use alloc::vec::Vec;
use block::Block;
use text::{Gap, Text};
use ui::{Screen, Settings, Ui};

/// The handlers `EDIT` installs.
pub mod handler {
    /// INT 23h, ignored, so a document is never lost to Ctrl+Break.
    pub const BREAK: u16 = 0;
    /// INT 24h, failing the call, for the editor to say what failed.
    pub const CRITICAL: u16 = 1;
}

record! {
    /// Where the document lies: its block, the bytes the block holds, and
    /// the text's layout in them.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Layout {
        segment: u16,
        store: u32,
        start: u32,
        end: u32,
        capacity: u32,
    }
}

record! {
    /// The screen as `EDIT` found it, to put back, and where it draws.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Video {
        mode: u8,
        page: u8,
        rows: u8,
        cursor: u16,
        shape: u16,
        /// A mode of its own was set, and the font that shows more rows.
        set: bool,
        font: bool,
        /// The screen's cells are kept at [`at::SCREEN`].
        saved: bool,
        /// Where the page it draws lies.
        segment: u16,
        offset: u16,
        /// The cursor's shape as it last set it.
        shown: u16,
    }
}

record! {
    /// What `EDIT` has hooked: whether it led INT 09h to its filter and a
    /// mouse driver answers, and BREAK as it found it.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Hooks {
        keyboard: bool,
        mouse: bool,
        broken: u8,
    }
}

record! {
    /// What the command line asked, beside a file's name.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Asked {
        mono: bool,
        most: bool,
        plain: bool,
    }
}

/// Where `EDIT` keeps what it holds, in its own memory.
pub mod at {
    use super::{Asked, Hooks, Layout, Video};
    use crate::dos::utility::edit::ui::Ui;
    use crate::frames::Field;

    /// `n` bytes as an offset: every record here is far smaller than a
    /// segment, asserted below.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "each record is asserted to fit a segment"
    )]
    const fn bytes(n: usize) -> u16 {
        assert!(n < 0x1000);
        n as u16
    }

    pub const UI: u16 = crate::dos::program::at::STATE;
    pub const LAYOUT: u16 = UI + bytes(<Ui as Field>::SIZE);
    pub const VIDEO: u16 = LAYOUT + bytes(<Layout as Field>::SIZE);
    pub const HOOKS: u16 = VIDEO + bytes(<Video as Field>::SIZE);
    pub const ASKED: u16 = HOOKS + bytes(<Hooks as Field>::SIZE);
    /// Set by the mouse handler when its key is in the BIOS's buffer,
    /// cleared as the editor reads it, so moves never fill the buffer.
    pub const PENDING: u16 = ASKED + bytes(<Asked as Field>::SIZE);
    /// INT 09h's handler before the filter, the far pointer it chains
    /// through.
    pub const OLD_09: u16 = PENDING + 1;
    /// The INT 09h filter's code, and the mouse handler's.
    pub const FILTER: u16 = OLD_09 + 4;
    pub const FILTER_LEN: u16 = 0x40;
    pub const MOUSE: u16 = FILTER + FILTER_LEN;
    pub const MOUSE_LEN: u16 = 0x30;
    /// The DTA its directory searches report in.
    pub const DTA: u16 = MOUSE + MOUSE_LEN;
    pub const DTA_LEN: u16 = 0x30;
    /// A name given to DOS, and the temporary file's a save writes.
    pub const NAME: u16 = DTA + DTA_LEN;
    pub const NAME_LEN: u16 = 0x80;
    pub const TEMP: u16 = NAME + NAME_LEN;
    /// The settings' text, a directory's name, an FCB a drive is parsed
    /// into.
    pub const BUFFER: u16 = TEMP + NAME_LEN;
    pub const BUFFER_LEN: u16 = 0x100;
    /// The cells of the screen it found, as many as fifty rows of eighty.
    pub const SCREEN: u16 = BUFFER + BUFFER_LEN;
    pub const SCREEN_LEN: u16 = 80 * 50 * 2;
    /// The editor's state and the document's layout as they were before
    /// the mouse was asked what it did, to tell whether it changed them.
    pub const BEFORE: u16 = SCREEN + SCREEN_LEN;
    pub const HELD_LEN: u16 = LAYOUT + bytes(<Layout as Field>::SIZE) - UI;
    pub const LEN: u16 = BEFORE + HELD_LEN - UI;

    const _: () = assert!(crate::dos::program::at::STATE as u32 + LEN as u32 <= 0x1_0000);
}

/// A record kept at `offset` of the program's memory.
fn load<T: Field>(p: &mut Program<'_, '_>, offset: u16) -> T {
    let mut bytes = alloc::vec![0u8; T::SIZE];
    p.g.read_bytes(p.own, offset, &mut bytes);
    T::take(&bytes, &mut 0)
}

fn keep<T: Field>(p: &mut Program<'_, '_>, offset: u16, value: T) {
    let mut bytes = alloc::vec![0u8; T::SIZE];
    value.put(&mut bytes, &mut 0);
    p.g.write_bytes(p.own, offset, &bytes);
}

/// The editor's state as the program's memory holds it, for a step that
/// only reads it.
fn ui_of(p: &mut Program<'_, '_>) -> Ui {
    load(p, at::UI)
}

/// `f` run on the document alone as the program's memory lays it out, its
/// layout kept there after.
fn with_text<T>(p: &mut Program<'_, '_>, f: impl FnOnce(&mut Text<'_, Block<'_, '_>>) -> T) -> T {
    let layout: Layout = load(p, at::LAYOUT);
    let mut block = Block {
        g: &mut *p.g,
        segment: layout.segment,
        len: layout.store,
    };
    let mut text = Text::new(
        &mut block,
        Gap {
            start: layout.start,
            end: layout.end,
        },
        layout.capacity,
    );
    let out = f(&mut text);
    let (gap, capacity) = (text.gap(), text.capacity());
    keep(
        p,
        at::LAYOUT,
        Layout {
            start: gap.start,
            end: gap.end,
            capacity,
            ..layout
        },
    );
    out
}

/// `f` run on the editor and its document as the program's memory holds
/// them, both kept there after.
fn with_ui<T>(
    p: &mut Program<'_, '_>,
    f: impl FnOnce(&mut Ui, &mut Text<'_, Block<'_, '_>>) -> T,
) -> T {
    let mut ui: Ui = load(p, at::UI);
    let layout: Layout = load(p, at::LAYOUT);
    let mut block = Block {
        g: &mut *p.g,
        segment: layout.segment,
        len: layout.store,
    };
    let mut text = Text::new(
        &mut block,
        Gap {
            start: layout.start,
            end: layout.end,
        },
        layout.capacity,
    );
    let out = f(&mut ui, &mut text);
    let (gap, capacity) = (text.gap(), text.capacity());
    keep(p, at::UI, ui);
    keep(
        p,
        at::LAYOUT,
        Layout {
            start: gap.start,
            end: gap.end,
            capacity,
            ..layout
        },
    );
    out
}

/// `name` given to DOS: written at `offset` as ASCIIZ, as much as the room
/// there holds.
fn name_at(p: &mut Program<'_, '_>, offset: u16, name: &[u8]) {
    let len = name.len().min(usize::from(at::NAME_LEN) - 1);
    let mut bytes = name[..len].to_vec();
    bytes.push(0);
    p.g.write_bytes(p.own, offset, &bytes);
}

/// The ASCIIZ name at `offset`.
fn name_from(p: &mut Program<'_, '_>, offset: u16) -> Vec<u8> {
    let mut bytes = alloc::vec![0u8; usize::from(at::NAME_LEN)];
    p.g.read_bytes(p.own, offset, &mut bytes);
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    bytes.truncate(end);
    bytes
}

/// What the command line asks: the switches, and the file to edit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Request<'a> {
    pub asked: Asked,
    pub file: Option<&'a [u8]>,
}

/// The command line `tail` read.
///
/// # Errors
///
/// The common parser's refusal of a switch it does not know or a second
/// file.
pub fn request(tail: &[u8]) -> Result<Request<'_>, Refusal<'_>> {
    let mut request = Request::default();
    for word in tail
        .split(|&byte| matches!(byte, b' ' | b'\t'))
        .filter(|word| !word.is_empty())
    {
        let mut parts = word.split(|&byte| byte == b'/');
        if let Some(file) = parts.next().filter(|file| !file.is_empty()) {
            if request.file.replace(file).is_some() {
                return Err(Refusal::TooMany(file));
            }
        }
        for switch in parts {
            match switch.to_ascii_uppercase().as_slice() {
                b"B" => request.asked.mono = true,
                b"G" => {}
                b"H" => request.asked.most = true,
                b"NOHI" => request.asked.plain = true,
                _ => {
                    let at = switch.as_ptr() as usize - word.as_ptr() as usize;
                    return Err(Refusal::Switch(&word[at - 1..at + switch.len()]));
                }
            }
        }
    }
    Ok(request)
}

/// The least memory the document is given, in paragraphs.
const LEAST: u16 = 0x40;

/// The code a run ends with when it could not start.
const NOT_STARTED: u32 = 1;

locals! {
    /// `EDIT`'s main routine: the command line read, memory taken, the
    /// screen and the hooks set, the settings and any file named read, the
    /// editor run, and all put back as it was found.
    pub struct Edit {}
}

mod phase {
    pub const SAID: u8 = 1;
    pub const SHRUNK: u8 = 2;
    pub const SIZED: u8 = 3;
    pub const ALLOCATED: u8 = 4;
    pub const SCREEN_SET: u8 = 5;
    pub const HOOKED: u8 = 6;
    pub const SETTINGS_READ: u8 = 7;
    pub const NAMED: u8 = 8;
    pub const LOADED: u8 = 9;
    pub const RUN: u8 = 10;
    pub const UNHOOKED: u8 = 11;
    pub const RESTORED: u8 = 12;
}

impl Edit {
    /// The memory DOS gave past the program given back, then the largest
    /// block asked for.
    fn start(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let tail = p.tail();
        if let Err(refusal) = request(&tail) {
            *phase = phase::SAID;
            return Flow::Call(refuse(p, b"", refusal));
        }
        p.set8(at::PENDING, 0);
        *phase = phase::SHRUNK;
        Flow::Call(p.shrink())
    }

    /// The largest block taken for the document.
    fn allocate(p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let largest = p.result(crate::dos::program::Returned::Bx);
        if child.is_ok() || largest < LEAST {
            *phase = phase::SAID;
            return Flow::Call(say_error(p, message::MEMORY));
        }
        *phase = phase::ALLOCATED;
        Flow::Call(Frame::of(&Int {
            bx: largest,
            ..Int::dos(p.own, 0x4800)
        }))
    }

    /// The block taken, the switches kept, the screen set.
    fn allocated(p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Ok(segment) = child else {
            *phase = phase::SAID;
            return Flow::Call(say_error(p, message::MEMORY));
        };
        let paragraphs = p.result(crate::dos::program::Returned::Bx);
        let store = u32::from(paragraphs) * 16;
        let segment = u16::try_from(segment).unwrap_or(0);
        keep(
            p,
            at::LAYOUT,
            Layout {
                segment,
                store,
                start: 0,
                end: store,
                capacity: store,
            },
        );
        let tail = p.tail();
        let asked = request(&tail)
            .map(|request| request.asked)
            .unwrap_or_default();
        keep(p, at::ASKED, asked);
        *phase = phase::SCREEN_SET;
        Flow::Call(Frame::of(&EditScreen::default()))
    }

    /// The screen set: the editor made for it, the hooks set.
    fn screen_set(p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let screen = Screen::unpacked(take!(child));
        keep(p, at::UI, Ui::new(screen, Settings::default()));
        *phase = phase::HOOKED;
        Flow::Call(Frame::of(&EditHook::default()))
    }

    /// The settings read: a file named on the command line read in, or
    /// named for a new document where it is not there.
    fn named(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let tail = p.tail();
        let Some(file) = request(&tail).ok().and_then(|request| request.file) else {
            *phase = phase::RUN;
            return Flow::Call(Frame::of(&EditLoop::default()));
        };
        let file = file.to_vec();
        name_at(p, at::NAME, &file);
        *phase = phase::NAMED;
        Flow::Call(Frame::of(&EditLoad::default()))
    }

    /// A file named not found: the new document is named for it, the name
    /// made whole by DOS.
    fn loaded(p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let done = ui::Done::of(child.unwrap_or(0));
        if done == ui::Done::Failed(crate::dos::fs::error::FILE_NOT_FOUND.to_le_bytes()[0]) {
            *phase = phase::LOADED;
            return Flow::Call(files::truename(p));
        }
        with_ui(p, |ui, text| ui.done(done, text));
        *phase = phase::RUN;
        Flow::Call(Frame::of(&EditLoop::default()))
    }
}

impl Routine for Edit {
    const ID: Id = Id::Edit;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            phase::SAID => {
                take!(child);
                Flow::Return(Ok(NOT_STARTED))
            }
            phase::SHRUNK => {
                take!(child);
                *phase = phase::SIZED;
                Flow::Call(Frame::of(&Int {
                    bx: 0xFFFF,
                    ..Int::dos(p.own, 0x4800)
                }))
            }
            phase::SIZED => Self::allocate(p, phase, child),
            phase::ALLOCATED => Self::allocated(p, phase, child),
            phase::SCREEN_SET => Self::screen_set(p, phase, child),
            phase::HOOKED => {
                take!(child);
                *phase = phase::SETTINGS_READ;
                Flow::Call(Frame::of(&EditSettings::default()))
            }
            phase::SETTINGS_READ => {
                take!(child);
                Self::named(p, phase)
            }
            phase::NAMED => Self::loaded(p, phase, child),
            phase::LOADED => {
                let name = if child.is_ok() {
                    name_from(p, at::TEMP)
                } else {
                    name_from(p, at::NAME)
                };
                with_ui(p, |ui, _| ui.name.set(&name));
                *phase = phase::RUN;
                Flow::Call(Frame::of(&EditLoop::default()))
            }
            phase::RUN => {
                *phase = phase::UNHOOKED;
                Flow::Call(Frame::of(&EditUnhook::default()))
            }
            phase::UNHOOKED => {
                *phase = phase::RESTORED;
                Flow::Call(Frame::of(&EditRestore::default()))
            }
            phase::RESTORED => Flow::Return(Ok(0)),
            _ => Self::start(p, phase),
        }
    }
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod tests;
