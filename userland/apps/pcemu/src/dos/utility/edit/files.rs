//! The work only DOS can do for `EDIT`: a directory listed for a dialog,
//! a directory or drive gone to, a file read in, the document saved,
//! printed, and its settings read and kept. Each routine's outcome is the
//! [`Done`] the editor is given.
//!
//! A save never puts the file it replaces at risk: the document is written
//! whole to a new file in the same directory, and only once that is closed
//! is the old one deleted and the new one given its name and the old one's
//! hidden and system attributes. A disk too full for both leaves the old
//! file as it was, the new one deleted.

use super::commands::put_entry;
use super::commands::{is_pattern, split_name};
use super::settings;
use super::ui::{entry, failure, Done};
use super::{at, load, name_at, name_from, ui_of, with_text, with_ui, Layout};
use crate::dos::dirops::finding;
use crate::dos::fs::error;
use crate::dos::memory::{environment_end, psp};
use crate::dos::program::{self, Flow, Frame, Id, Int, Program, Returned, Routine};
use crate::dos::utility::place;
use crate::frames::{locals, take, Fault, Outcome};
use alloc::vec::Vec;
use tairix_fat::dir::attr;

/// The most bytes read or written at one call.
const RUN: u32 = 0x8000;
/// The handle DOS opens on the printer.
const PRINTER: u16 = 4;
/// A form feed, which ends what is printed.
const FORM_FEED: u8 = 0x0C;
/// The attributes a replaced file passes to the file replacing it.
const KEPT_ATTRIBUTES: u8 = attr::HIDDEN | attr::SYSTEM;

/// The outcome reporting `done`.
fn reporting(done: Done) -> Flow {
    Flow::Return(Ok(done.number()))
}

/// The error `fault` stands for: a critical error the handler failed is
/// the device's error it kept, and a fault no call gives is access denied.
fn code_of(p: &mut Program<'_, '_>, fault: Fault) -> u8 {
    match fault {
        Fault::Error(error::FAIL_ON_24) => p.get8(program::at::CRITICAL),
        Fault::Error(code) => code.to_le_bytes()[0],
        _ => error::ACCESS_DENIED.to_le_bytes()[0],
    }
}

/// The outcome reporting the failure `fault` came to.
fn failing(p: &mut Program<'_, '_>, fault: Fault) -> Flow {
    let code = code_of(p, fault);
    reporting(Done::Failed(code))
}

/// The call making the name at [`at::NAME`] whole, DOS's own spelling of
/// it, at [`at::TEMP`].
pub fn truename(p: &Program<'_, '_>) -> Frame {
    Frame::of(&Int {
        si: at::NAME,
        di: at::TEMP,
        ..Int::dos(p.own, 0x6000)
    })
}

/// The drive `name` begins with, `0` for A:.
fn drive_of(name: &[u8]) -> Option<u8> {
    match name {
        [letter, b':', ..] if letter.is_ascii_alphabetic() => {
            Some(letter.to_ascii_uppercase() - b'A')
        }
        _ => None,
    }
}

/// The directory `dir` names, for function `3Bh`: its last `\` dropped but
/// for a root's.
fn directory_of(dir: &[u8]) -> &[u8] {
    match dir {
        [.., b':', b'\\'] | [b'\\'] => dir,
        [rest @ .., b'\\'] => rest,
        _ => dir,
    }
}

/// The directory part of the whole name `name`, its last `\` kept.
fn folder_of(name: &[u8]) -> &[u8] {
    let (folder, _) = split_name(name);
    folder
}

locals! {
    /// The directory or drive the name at [`at::NAME`] names gone to: an
    /// error where it names neither.
    pub struct EditVisit {
        drive: u8,
    }
}

mod visit {
    pub const CHANGED: u8 = 1;
    pub const SELECTED: u8 = 2;
    pub const CHECKED: u8 = 3;
}

/// No drive to select.
const NO_DRIVE: u8 = u8::MAX;

impl Routine for EditVisit {
    const ID: Id = Id::EditVisit;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            visit::CHANGED => {
                take!(child);
                if self.drive == NO_DRIVE {
                    return Flow::Return(Ok(0));
                }
                *phase = visit::SELECTED;
                Flow::Call(Frame::of(&Int {
                    dx: u16::from(self.drive),
                    ..Int::dos(p.own, 0x0E00)
                }))
            }
            visit::SELECTED => {
                take!(child);
                *phase = visit::CHECKED;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x1900)))
            }
            visit::CHECKED => {
                if take!(child).to_le_bytes()[0] != self.drive {
                    return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
                }
                Flow::Return(Ok(0))
            }
            _ => {
                let name = name_from(p, at::NAME);
                self.drive = drive_of(&name).unwrap_or(NO_DRIVE);
                if name.len() == 2 && self.drive != NO_DRIVE {
                    *phase = visit::SELECTED;
                    return Flow::Call(Frame::of(&Int {
                        dx: u16::from(self.drive),
                        ..Int::dos(p.own, 0x0E00)
                    }));
                }
                *phase = visit::CHANGED;
                Flow::Call(Frame::of(&Int {
                    dx: at::NAME,
                    ..Int::dos(p.own, 0x3B00)
                }))
            }
        }
    }
}

locals! {
    /// The current directory listed for a dialog: its path, the files the
    /// pattern matches, its directories, and the drives DOS has. `clip` is
    /// the clipboard's length, the listing lying below it.
    pub struct EditList {
        count: u16,
        room: u16,
        drive: u8,
        drives: u8,
        clip: u32,
    }
}

mod list {
    pub const DRIVE: u8 = 1;
    pub const PATH: u8 = 2;
    pub const DTA_SET: u8 = 3;
    pub const FILE: u8 = 4;
    pub const DIRECTORY: u8 = 5;
    pub const DRIVES: u8 = 6;
    pub const PARSED: u8 = 7;
}

/// Whether a search's `fault` is only the end of what it finds.
fn none_left(fault: Fault) -> bool {
    matches!(
        fault,
        Fault::Error(error::NO_MORE_FILES | error::FILE_NOT_FOUND)
    )
}

impl EditList {
    /// The entry the DTA reports, of `kind`, added while there is room.
    fn add(&mut self, p: &mut Program<'_, '_>, kind: u8) {
        let mut name = [0u8; 13];
        p.g.read_bytes(p.own, at::DTA + finding::NAME, &mut name);
        let end = name
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(name.len());
        self.put(p, kind, &name[..end]);
    }

    fn put(&mut self, p: &mut Program<'_, '_>, kind: u8, name: &[u8]) {
        if self.count >= self.room {
            return;
        }
        let (clip, n) = (self.clip, self.count);
        with_text(p, |text| put_entry(text, clip, n, kind, name));
        self.count += 1;
    }

    /// The next search, of `function`, through the DTA.
    fn search(p: &Program<'_, '_>, function: u16, attributes: u16) -> Frame {
        Frame::of(&Int {
            cx: attributes,
            dx: at::NAME,
            ..Int::dos(p.own, function)
        })
    }

    /// The directories searched for, after the files.
    fn directories(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        name_at(p, at::NAME, b"*.*");
        *phase = list::DIRECTORY;
        Flow::Call(Self::search(p, 0x4E00, u16::from(attr::DIRECTORY)))
    }

    /// Drive `self.drive` asked after, as function `29h` parses its letter.
    fn next_drive(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if self.drive >= self.drives {
            let count = self.count;
            with_ui(p, |ui, text| ui.end_listing(text, count));
            return reporting(Done::Listed);
        }
        p.g.write_bytes(p.own, at::NAME, &[b'A' + self.drive, b':', 0]);
        *phase = list::PARSED;
        Flow::Call(Frame::of(&Int {
            si: at::NAME,
            di: at::BUFFER,
            ..Int::dos(p.own, 0x2900)
        }))
    }
}

impl Routine for EditList {
    const ID: Id = Id::EditList;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            list::DRIVE => match child {
                Ok(drive) => {
                    self.drive = drive.to_le_bytes()[0];
                    *phase = list::PATH;
                    Flow::Call(Frame::of(&Int {
                        dx: 0,
                        si: at::BUFFER,
                        ..Int::dos(p.own, 0x4700)
                    }))
                }
                Err(fault) => failing(p, fault),
            },
            list::PATH => {
                if let Err(fault) = child {
                    return failing(p, fault);
                }
                let mut path = alloc::vec![b'A' + self.drive, b':', b'\\'];
                path.extend(name_from(p, at::BUFFER));
                with_ui(p, |ui, _| ui.path.set(&path));
                *phase = list::DTA_SET;
                Flow::Call(Frame::of(&Int {
                    dx: at::DTA,
                    ..Int::dos(p.own, 0x1A00)
                }))
            }
            list::DTA_SET => {
                take!(child);
                (self.room, self.clip) = with_ui(p, |ui, text| (ui.begin_listing(text), ui.clip));
                let pattern = ui_of(p).pattern.text().to_vec();
                name_at(p, at::NAME, &pattern);
                *phase = list::FILE;
                Flow::Call(Self::search(p, 0x4E00, 0))
            }
            list::FILE => match child {
                Ok(_) => {
                    self.add(p, entry::FILE);
                    Flow::Call(Self::search(p, 0x4F00, 0))
                }
                Err(fault) if none_left(fault) || fault == Fault::Error(error::PATH_NOT_FOUND) => {
                    Self::directories(p, phase)
                }
                Err(fault) => {
                    let count = self.count;
                    with_ui(p, |ui, text| ui.end_listing(text, count));
                    failing(p, fault)
                }
            },
            list::DIRECTORY if child.is_ok() => {
                let attributes = p.get8(at::DTA + finding::ATTRIBUTE);
                let dot = p.get8(at::DTA + finding::NAME) == b'.'
                    && p.get8(at::DTA + finding::NAME + 1) == 0;
                if attributes & attr::DIRECTORY != 0 && !dot {
                    self.add(p, entry::DIRECTORY);
                }
                Flow::Call(Self::search(p, 0x4F00, u16::from(attr::DIRECTORY)))
            }
            list::DIRECTORY => {
                *phase = list::DRIVES;
                Flow::Call(Frame::of(&Int {
                    dx: u16::from(self.drive),
                    ..Int::dos(p.own, 0x0E00)
                }))
            }
            list::DRIVES => {
                self.drives = take!(child).to_le_bytes()[0].min(26);
                self.drive = 0;
                self.next_drive(p, phase)
            }
            list::PARSED => {
                if take!(child).to_le_bytes()[0] != 0xFF {
                    let letter = [b'A' + self.drive];
                    self.put(p, entry::DRIVE, &letter);
                }
                self.drive += 1;
                self.next_drive(p, phase)
            }
            _ => {
                *phase = list::DRIVE;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x1900)))
            }
        }
    }
}

locals! {
    /// What the dialog's name names opened: a pattern's files listed, a
    /// directory or drive gone to and listed, or else the file read in.
    /// `place` is set while only a place is named, which a failed visit
    /// fails, as a file's name it would not.
    pub struct EditOpen {
        place: bool,
    }
}

mod open {
    pub const VISITED: u8 = 1;
    pub const LISTED: u8 = 2;
    pub const LOADED: u8 = 3;
}

impl Routine for EditOpen {
    const ID: Id = Id::EditOpen;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            open::VISITED => match child {
                Ok(_) => {
                    *phase = open::LISTED;
                    Flow::Call(Frame::of(&EditList::default()))
                }
                Err(fault) if self.place => failing(p, fault),
                Err(_) => {
                    *phase = open::LOADED;
                    Flow::Call(Frame::of(&EditLoad::default()))
                }
            },
            open::LISTED | open::LOADED => Flow::Return(child),
            _ => {
                let name = ui_of(p).dialog.fields[0].text().to_vec();
                if is_pattern(&name) {
                    let (dir, pattern) = split_name(&name);
                    let pattern = pattern.to_vec();
                    with_ui(p, |ui, _| ui.pattern.set(&pattern));
                    if dir.is_empty() {
                        *phase = open::LISTED;
                        return Flow::Call(Frame::of(&EditList::default()));
                    }
                    let dir = directory_of(dir).to_vec();
                    name_at(p, at::NAME, &dir);
                    self.place = true;
                } else {
                    name_at(p, at::NAME, &name);
                    self.place = name.len() == 2 && drive_of(&name).is_some();
                }
                *phase = open::VISITED;
                Flow::Call(Frame::of(&EditVisit::default()))
            }
        }
    }
}

locals! {
    /// The file at [`at::NAME`] read in as the document, in place of the
    /// one there, and named in DOS's own spelling.
    pub struct EditLoad {
        handle: u16,
        failure: u8,
    }
}

mod loading {
    pub const OPENED: u8 = 1;
    pub const SIZED: u8 = 2;
    pub const REWOUND: u8 = 3;
    pub const READ: u8 = 4;
    pub const CLOSED: u8 = 5;
    pub const NAMED: u8 = 6;
}

impl EditLoad {
    fn close(self, p: &Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = loading::CLOSED;
        Flow::Call(Frame::of(&Int {
            bx: self.handle,
            ..Int::dos(p.own, 0x3E00)
        }))
    }

    /// The handle closed, failing with `code`, the document as it was.
    fn fail(&mut self, p: &Program<'_, '_>, phase: &mut u8, code: u8) -> Flow {
        self.failure = code;
        self.close(p, phase)
    }

    /// The same once reading has begun: what was read given up for an
    /// empty, untitled document.
    fn fail_reading(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, code: u8) -> Flow {
        with_ui(p, |ui, text| {
            let len = text.len();
            text.delete(0..len);
            ui.name.set(b"");
            ui.loaded(text);
        });
        self.fail(p, phase, code)
    }

    /// The next run read into the gap, as much as it holds.
    fn read_next(self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let layout: Layout = load(p, at::LAYOUT);
        let room = (layout.end - layout.start).min(RUN);
        let (segment, offset) = place(layout.segment, layout.start);
        *phase = loading::READ;
        Flow::Call(Frame::of(&Int {
            bx: self.handle,
            cx: u16::try_from(room).unwrap_or(0),
            dx: offset,
            ds: segment,
            ..Int::dos(p.own, 0x3F00)
        }))
    }
}

impl Routine for EditLoad {
    const ID: Id = Id::EditLoad;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            loading::OPENED => match child {
                Ok(handle) => {
                    self.handle = u16::try_from(handle).unwrap_or(0);
                    *phase = loading::SIZED;
                    Flow::Call(Frame::of(&Int {
                        bx: self.handle,
                        ..Int::dos(p.own, 0x4202)
                    }))
                }
                Err(fault) => failing(p, fault),
            },
            loading::SIZED => {
                let low = match child {
                    Ok(low) => low,
                    Err(fault) => {
                        let code = code_of(p, fault);
                        return self.fail(p, phase, code);
                    }
                };
                let size = (u32::from(p.result(Returned::Dx)) << 16) | (low & 0xFFFF);
                let room = with_ui(p, |ui, text| {
                    ui.release_listing(text);
                    text.store_len().saturating_sub(ui.clip)
                });
                if size > room {
                    return self.fail(p, phase, failure::TOO_LARGE);
                }
                *phase = loading::REWOUND;
                Flow::Call(Frame::of(&Int {
                    bx: self.handle,
                    ..Int::dos(p.own, 0x4200)
                }))
            }
            loading::REWOUND => {
                if let Err(fault) = child {
                    let code = code_of(p, fault);
                    return self.fail(p, phase, code);
                }
                with_text(p, |text| {
                    let len = text.len();
                    text.delete(0..len);
                });
                self.read_next(p, phase)
            }
            loading::READ => {
                let read = match child {
                    Ok(read) => read,
                    Err(fault) => {
                        let code = code_of(p, fault);
                        return self.fail_reading(p, phase, code);
                    }
                };
                let full = with_text(p, |text| text.admit(read).is_err() || text.room() == 0);
                if read == 0 || full {
                    return self.close(p, phase);
                }
                self.read_next(p, phase)
            }
            loading::CLOSED => {
                if self.failure != 0 {
                    return reporting(Done::Failed(self.failure));
                }
                *phase = loading::NAMED;
                Flow::Call(truename(p))
            }
            loading::NAMED => {
                let name = if child.is_ok() {
                    name_from(p, at::TEMP)
                } else {
                    name_from(p, at::NAME)
                };
                with_ui(p, |ui, _| ui.name.set(&name));
                reporting(Done::Loaded)
            }
            _ => {
                *phase = loading::OPENED;
                Flow::Call(Frame::of(&Int {
                    dx: at::NAME,
                    ..Int::dos(p.own, 0x3D00)
                }))
            }
        }
    }
}

locals! {
    /// Save As's name: a directory or drive gone to and listed, or the file
    /// it names made the target — reported where it is another already
    /// there, else saved.
    pub struct EditSaveAs {}
}

mod save_as {
    pub const VISITED: u8 = 1;
    pub const NAMED: u8 = 2;
    pub const ASKED: u8 = 3;
    pub const DONE: u8 = 4;
}

impl Routine for EditSaveAs {
    const ID: Id = Id::EditSaveAs;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            save_as::VISITED => {
                if child.is_ok() {
                    *phase = save_as::DONE;
                    return Flow::Call(Frame::of(&EditList::default()));
                }
                *phase = save_as::NAMED;
                Flow::Call(truename(p))
            }
            save_as::NAMED => {
                if let Err(fault) = child {
                    return failing(p, fault);
                }
                let target = name_from(p, at::TEMP);
                with_ui(p, |ui, _| ui.target.set(&target));
                *phase = save_as::ASKED;
                Flow::Call(Frame::of(&Int {
                    dx: at::TEMP,
                    ..Int::dos(p.own, 0x4300)
                }))
            }
            save_as::ASKED => {
                let ui = ui_of(p);
                let other = ui.target.text() != ui.name.text();
                match child {
                    Ok(_) if other => reporting(Done::Exists),
                    Ok(_) | Err(Fault::Error(error::FILE_NOT_FOUND)) => {
                        *phase = save_as::DONE;
                        Flow::Call(Frame::of(&EditSave::default()))
                    }
                    Err(fault) => failing(p, fault),
                }
            }
            save_as::DONE => Flow::Return(child),
            _ => {
                let name = ui_of(p).dialog.fields[0].text().to_vec();
                name_at(p, at::NAME, &name);
                *phase = save_as::VISITED;
                Flow::Call(Frame::of(&EditVisit::default()))
            }
        }
    }
}

locals! {
    /// The document written to its target, replacing any file there only
    /// once it is written whole.
    pub struct EditSave {
        handle: u16,
        attributes: u8,
        exists: bool,
        at: u32,
        asked: u16,
        failure: u8,
    }
}

mod saving {
    pub const ASKED: u8 = 1;
    pub const CREATED: u8 = 2;
    pub const WROTE: u8 = 3;
    pub const CLOSED: u8 = 4;
    pub const DELETED: u8 = 5;
    pub const RENAMED: u8 = 6;
    pub const MARKED: u8 = 7;
    pub const CLEANED: u8 = 8;
}

impl EditSave {
    /// The next run of the document written: the bytes before its gap,
    /// then those after it.
    fn write_next(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let layout: Layout = load(p, at::LAYOUT);
        if self.at < layout.end && self.at >= layout.start {
            self.at = layout.end;
        }
        let end = if self.at < layout.start {
            layout.start
        } else {
            layout.capacity
        };
        if self.at >= end {
            return self.close(p, phase);
        }
        let n = (end - self.at).min(RUN);
        self.asked = u16::try_from(n).unwrap_or(0);
        let (segment, offset) = place(layout.segment, self.at);
        *phase = saving::WROTE;
        Flow::Call(Frame::of(&Int {
            bx: self.handle,
            cx: self.asked,
            dx: offset,
            ds: segment,
            ..Int::dos(p.own, 0x4000)
        }))
    }

    fn close(&self, p: &Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = saving::CLOSED;
        Flow::Call(Frame::of(&Int {
            bx: self.handle,
            ..Int::dos(p.own, 0x3E00)
        }))
    }

    /// What asking for the target's attributes answered: a read-only target
    /// refuses the save, and any other has the new file made beside it.
    fn create(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match child {
            Ok(_) => {
                self.exists = true;
                self.attributes = p.result(Returned::Cx).to_le_bytes()[0];
                if self.attributes & attr::READ_ONLY != 0 {
                    return reporting(Done::Failed(error::ACCESS_DENIED.to_le_bytes()[0]));
                }
            }
            Err(Fault::Error(error::FILE_NOT_FOUND)) => {}
            Err(fault) => return failing(p, fault),
        }
        let target = name_from(p, at::NAME);
        let folder = folder_of(&target).to_vec();
        name_at(p, at::TEMP, &folder);
        *phase = saving::CREATED;
        Flow::Call(Frame::of(&Int {
            cx: 0,
            dx: at::TEMP,
            ..Int::dos(p.own, 0x5A00)
        }))
    }

    /// The new file deleted, the save failing with `code`.
    fn clean(&mut self, p: &Program<'_, '_>, phase: &mut u8, code: u8) -> Flow {
        self.failure = code;
        *phase = saving::CLEANED;
        Flow::Call(Frame::of(&Int {
            dx: at::TEMP,
            ..Int::dos(p.own, 0x4100)
        }))
    }

    /// The new file given the target's name.
    fn rename(p: &Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = saving::RENAMED;
        Flow::Call(Frame::of(&Int {
            dx: at::TEMP,
            di: at::NAME,
            ..Int::dos(p.own, 0x5600)
        }))
    }

    /// The save done: the document is the target's.
    fn saved(p: &mut Program<'_, '_>) -> Flow {
        with_ui(p, |ui, _| ui.name = ui.target);
        reporting(Done::Saved)
    }
}

impl Routine for EditSave {
    const ID: Id = Id::EditSave;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            saving::ASKED => self.create(p, phase, child),
            saving::CREATED => match child {
                Ok(handle) => {
                    self.handle = u16::try_from(handle).unwrap_or(0);
                    self.at = 0;
                    self.write_next(p, phase)
                }
                Err(fault) => failing(p, fault),
            },
            saving::WROTE => match child {
                Ok(written) if written >= u32::from(self.asked) => {
                    self.at += u32::from(self.asked);
                    self.write_next(p, phase)
                }
                Ok(_) => {
                    self.failure = failure::DISK_FULL;
                    self.close(p, phase)
                }
                Err(fault) => {
                    self.failure = code_of(p, fault);
                    self.close(p, phase)
                }
            },
            saving::CLOSED => {
                if let Err(fault) = child {
                    let code = if self.failure != 0 {
                        self.failure
                    } else {
                        code_of(p, fault)
                    };
                    return self.clean(p, phase, code);
                }
                if self.failure != 0 {
                    let code = self.failure;
                    return self.clean(p, phase, code);
                }
                if self.exists {
                    *phase = saving::DELETED;
                    return Flow::Call(Frame::of(&Int {
                        dx: at::NAME,
                        ..Int::dos(p.own, 0x4100)
                    }));
                }
                Self::rename(p, phase)
            }
            saving::DELETED => match child {
                Ok(_) => Self::rename(p, phase),
                Err(fault) => {
                    let code = code_of(p, fault);
                    self.clean(p, phase, code)
                }
            },
            saving::RENAMED => {
                if child.is_err() {
                    let kept = name_from(p, at::TEMP);
                    with_ui(p, |ui, _| ui.target.set(&kept));
                    return reporting(Done::Failed(failure::KEPT));
                }
                if self.exists && self.attributes & KEPT_ATTRIBUTES != 0 {
                    *phase = saving::MARKED;
                    let attributes = (self.attributes & KEPT_ATTRIBUTES) | attr::ARCHIVE;
                    return Flow::Call(Frame::of(&Int {
                        cx: u16::from(attributes),
                        dx: at::NAME,
                        ..Int::dos(p.own, 0x4301)
                    }));
                }
                Self::saved(p)
            }
            saving::MARKED => Self::saved(p),
            saving::CLEANED => reporting(Done::Failed(self.failure)),
            _ => {
                let target = ui_of(p).target.text().to_vec();
                name_at(p, at::NAME, &target);
                *phase = saving::ASKED;
                Flow::Call(Frame::of(&Int {
                    dx: at::NAME,
                    ..Int::dos(p.own, 0x4300)
                }))
            }
        }
    }
}

locals! {
    /// The document, or its selection, sent to the printer, a form feed
    /// after it.
    pub struct EditPrint {
        selection: bool,
        from: u32,
        to: u32,
        asked: u16,
        ended: bool,
    }
}

mod printing {
    pub const WROTE: u8 = 1;
    pub const FED: u8 = 2;
}

impl EditPrint {
    /// The next run sent: the document's bytes as far as the gap or the
    /// end, whichever is nearer.
    fn next(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if self.from >= self.to {
            p.set8(at::BUFFER, FORM_FEED);
            *phase = printing::FED;
            return Flow::Call(Frame::of(&Int {
                bx: PRINTER,
                cx: 1,
                dx: at::BUFFER,
                ..Int::dos(p.own, 0x4000)
            }));
        }
        let layout: Layout = load(p, at::LAYOUT);
        let gap = layout.end - layout.start;
        let (physical, end) = if self.from < layout.start {
            (self.from, self.to.min(layout.start))
        } else {
            (self.from + gap, self.to)
        };
        let n = (end - self.from).min(RUN);
        self.asked = u16::try_from(n).unwrap_or(0);
        let (segment, offset) = place(layout.segment, physical);
        *phase = printing::WROTE;
        Flow::Call(Frame::of(&Int {
            bx: PRINTER,
            cx: self.asked,
            dx: offset,
            ds: segment,
            ..Int::dos(p.own, 0x4000)
        }))
    }
}

impl Routine for EditPrint {
    const ID: Id = Id::EditPrint;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            printing::WROTE => match child {
                Ok(written) if written >= u32::from(self.asked) => {
                    self.from += u32::from(self.asked);
                    self.next(p, phase)
                }
                Ok(_) => reporting(Done::Failed(failure::DEVICE)),
                Err(fault) => failing(p, fault),
            },
            printing::FED => match child {
                Ok(_) => reporting(Done::Printed),
                Err(fault) => failing(p, fault),
            },
            _ => {
                let selection = self.selection;
                let range = with_ui(p, |ui, text| {
                    let len = text.len();
                    let chosen = if selection {
                        ui.edit(text).selection()
                    } else {
                        None
                    };
                    chosen.unwrap_or(0..len)
                });
                (self.from, self.to) = (range.start, range.end);
                self.next(p, phase)
            }
        }
    }
}

/// The program's own path, as DOS put it after its environment.
fn own_path(p: &mut Program<'_, '_>) -> Option<Vec<u8>> {
    let environment = p.get16(psp::ENVIRONMENT);
    if environment == 0 {
        return None;
    }
    let end = environment_end(p.g, environment)?;
    let mut path = alloc::vec![0u8; usize::from(at::NAME_LEN)];
    p.g.read_bytes(environment, end.checked_add(2)?, &mut path);
    let len = path.iter().position(|&byte| byte == 0)?;
    path.truncate(len);
    (!path.is_empty()).then_some(path)
}

locals! {
    /// The settings read from `EDIT.INI` beside the program, or, `write`,
    /// kept there.
    pub struct EditSettings {
        write: bool,
        handle: u16,
        failure: u8,
    }
}

mod setting {
    pub const OPENED: u8 = 1;
    pub const READ: u8 = 2;
    pub const CREATED: u8 = 3;
    pub const WRITTEN: u8 = 4;
    pub const CLOSED: u8 = 5;
}

impl EditSettings {
    fn close(self, p: &Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = setting::CLOSED;
        Flow::Call(Frame::of(&Int {
            bx: self.handle,
            ..Int::dos(p.own, 0x3E00)
        }))
    }
}

impl Routine for EditSettings {
    const ID: Id = Id::EditSettings;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let length = u16::try_from(settings::LEN).unwrap_or(0);
        match *phase {
            setting::OPENED => match child {
                Ok(handle) => {
                    self.handle = u16::try_from(handle).unwrap_or(0);
                    *phase = setting::READ;
                    Flow::Call(Frame::of(&Int {
                        bx: self.handle,
                        cx: length,
                        dx: at::BUFFER,
                        ..Int::dos(p.own, 0x3F00)
                    }))
                }
                Err(_) => reporting(Done::Stored),
            },
            setting::READ => {
                let read = usize::try_from(child.unwrap_or(0))
                    .unwrap_or(0)
                    .min(settings::LEN);
                let mut text = alloc::vec![0u8; read];
                p.g.read_bytes(p.own, at::BUFFER, &mut text);
                with_ui(p, |ui, _| {
                    ui.settings = settings::parse(&text, ui.settings);
                    ui.editor.tab = ui.settings.tab;
                });
                self.close(p, phase)
            }
            setting::CREATED => match child {
                Ok(handle) => {
                    self.handle = u16::try_from(handle).unwrap_or(0);
                    let text = settings::compose(ui_of(p).settings);
                    p.g.write_bytes(p.own, at::BUFFER, &text);
                    *phase = setting::WRITTEN;
                    let len = u16::try_from(text.len()).unwrap_or(0);
                    Flow::Call(Frame::of(&Int {
                        bx: self.handle,
                        cx: len,
                        dx: at::BUFFER,
                        ..Int::dos(p.own, 0x4000)
                    }))
                }
                Err(fault) => failing(p, fault),
            },
            setting::WRITTEN => {
                if let Err(fault) = child {
                    self.failure = code_of(p, fault);
                }
                self.close(p, phase)
            }
            setting::CLOSED => {
                if self.failure != 0 {
                    return reporting(Done::Failed(self.failure));
                }
                reporting(Done::Stored)
            }
            _ => {
                let Some(path) =
                    own_path(p).and_then(|path| settings::beside(&path, usize::from(at::NAME_LEN)))
                else {
                    return reporting(Done::Stored);
                };
                name_at(p, at::NAME, &path);
                if self.write {
                    *phase = setting::CREATED;
                    return Flow::Call(Frame::of(&Int {
                        cx: 0,
                        dx: at::NAME,
                        ..Int::dos(p.own, 0x3C00)
                    }));
                }
                *phase = setting::OPENED;
                Flow::Call(Frame::of(&Int {
                    dx: at::NAME,
                    ..Int::dos(p.own, 0x3D00)
                }))
            }
        }
    }
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
