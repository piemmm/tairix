//! ResourceFS as a filing system, `Resources:`, as RISC OS 5's answers it:
//! read-only, its directories the leading parts of its files' names. A
//! directory's own information is the first file beneath it in name order;
//! listed, a child is named and described by the first registered file that
//! implies it, and the children come in name order, case ignored.

use alloc::vec::Vec;

use super::{below_root, own, Entry, Object, NAME_MOST};
use crate::error::Error;
use crate::filing::fs::{self, attribute, file_info, kind};
use crate::filing::UNSUPPORTED;
use crate::os::{len32, Fail, Os, Regs};

/// "Resources: cannot be changed" and "ResourceFS does not do that".
const READ_ONLY: Error = Error {
    number: 0x113,
    token: "ReadOnly",
};
const BAD_OPERATION: Error = Error {
    number: 0x112,
    token: "BadOperation",
};

/// The root's attributes, as RISC OS 5 gives them.
const ROOT_ATTRIBUTES: u32 = attribute::OWNER_READ | attribute::LOCKED | attribute::PUBLIC_READ;
/// The buffer FileSwitch keeps an open file in: the most it allows, the data
/// being in memory already.
const BUFFER: u32 = 1024;
/// A listed entry's words before its name: its load and execution
/// addresses, length, attributes and kind.
const RECORD_WORDS: u32 = 5;

/// A file or directory FileSwitch has open, as it was when opened.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Opened {
    data: u32,
    length: u32,
    load: u32,
    exec: u32,
}

impl Opened {
    const fn of(entry: &Entry) -> Self {
        Self {
            data: entry.data,
            length: entry.length,
            load: entry.load,
            exec: entry.exec,
        }
    }

    const DIRECTORY: Self = Self {
        data: 0,
        length: 0,
        load: 0,
        exec: 0,
    };
}

/// A child of a listed directory: its name as registered, the entry that
/// implies it, and whether it is a file.
struct Child {
    name: Vec<u8>,
    entry: Entry,
    file: bool,
}

impl Os<'_> {
    /// The path FileSwitch hands at `at`, to its terminator.
    fn resource_path(&mut self, at: u32) -> Result<Vec<u8>, Fail> {
        match self.read_until(at, NAME_MOST as usize, |b| b == 0)? {
            (path, true) => Ok(path),
            (_, false) => Err(self.own_error(own, BAD_OPERATION, &[])),
        }
    }

    /// The file FileSwitch's `handle` names.
    fn resource_handle(&mut self, handle: u32) -> Result<Opened, Fail> {
        let slot = (handle as usize).wrapping_sub(1);
        match self.world.resource_fs.open.get(slot).copied().flatten() {
            Some(opened) => Ok(opened),
            None => Err(self.own_error(own, BAD_OPERATION, &[])),
        }
    }

    /// `opened` kept under a new handle, a freed one first.
    fn keep_resource_open(&mut self, opened: Opened) -> Result<u32, Fail> {
        let open = &mut self.world.resource_fs.open;
        let slot = if let Some(free) = open.iter().position(Option::is_none) {
            free
        } else {
            if open.try_reserve(1).is_err() {
                return Err(self.no_memory());
            }
            open.push(None);
            open.len() - 1
        };
        self.world.resource_fs.open[slot] = Some(opened);
        Ok(len32(slot) + 1)
    }

    /// The children of the directory at `path`: grouped by their folded
    /// names, each described by the first registered entry implying it.
    fn resource_children(&mut self, path: &[u8]) -> Result<Vec<Child>, Fail> {
        let mut prefix = Vec::new();
        if let Some(below) = below_root(path) {
            prefix
                .try_reserve_exact(below.len() + 1)
                .map_err(|_| self.no_memory())?;
            prefix.extend(below.iter().map(|&b| self.world.resource_fs.fold(b)));
            prefix.push(b'.');
        }
        let mut under = self.entries_under(&prefix)?;
        under.sort_by(|(a, a_order, _), (b, b_order, _)| {
            child_of(a, prefix.len())
                .cmp(child_of(b, prefix.len()))
                .then(a_order.cmp(b_order))
        });
        under.dedup_by(|(later, ..), (first, ..)| {
            child_of(later, prefix.len()) == child_of(first, prefix.len())
        });
        let mut children = Vec::new();
        children
            .try_reserve_exact(under.len())
            .map_err(|_| self.no_memory())?;
        for (folded, _, entry) in under {
            let part = child_of(&folded, prefix.len());
            let name = self.read_vec(entry.name.wrapping_add(len32(prefix.len())), part.len())?;
            let file = folded.len() == prefix.len() + part.len();
            children.push(Child { name, entry, file });
        }
        Ok(children)
    }
}

/// The part of the folded `name` after `prefix` bytes, to its next full stop.
fn child_of(name: &[u8], prefix: usize) -> &[u8] {
    let rest = name.get(prefix..).unwrap_or_default();
    let end = rest.iter().position(|&b| b == b'.').unwrap_or(rest.len());
    &rest[..end]
}

/// The catalogue information `FSEntry_File` 5 gives in R0 to R5.
fn give_info(regs: &mut Regs, found: Option<Object>, root: bool) {
    let (kind, load, exec, length, attributes) = match found {
        Some(Object::File(entry)) => (
            kind::FILE,
            entry.load,
            entry.exec,
            entry.length,
            entry.attributes,
        ),
        Some(Object::Directory { .. }) if root => (kind::DIRECTORY, 0, 0, 0, ROOT_ATTRIBUTES),
        Some(Object::Directory { load, exec }) => (kind::DIRECTORY, load, exec, 0, 0),
        None => (kind::NOT_FOUND, 0, 0, 0, 0),
    };
    regs.r[0] = kind;
    regs.r[2..6].copy_from_slice(&[load, exec, length, attributes]);
}

/// `FSEntry_Open`: a file or directory opened to be read, nothing found
/// being handle nought; anything else refused.
pub(super) fn open(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[0] != fs::open::READ {
        return Err(os.own_error(own, READ_ONLY, &[]));
    }
    let path = os.resource_path(regs.r[1])?;
    let (word, opened) = match os.resource(&path)? {
        Some(Object::File(entry)) => (file_info::READ, Opened::of(&entry)),
        Some(Object::Directory { .. }) => {
            (file_info::READ | file_info::DIRECTORY, Opened::DIRECTORY)
        }
        None => {
            regs.r[1] = 0;
            return Ok(());
        }
    };
    let handle = os.keep_resource_open(opened)?;
    regs.r[..5].copy_from_slice(&[word, handle, BUFFER, opened.length, opened.length]);
    Ok(())
}

/// `FSEntry_GetBytes`: R3 bytes from R4 into R2, zeros past the file's end.
pub(super) fn get_bytes(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let opened = os.resource_handle(regs.r[1])?;
    let (buffer, count, offset) = (regs.r[2], regs.r[3], regs.r[4]);
    let there = opened.length.saturating_sub(offset).min(count);
    os.copy_bytes(opened.data.wrapping_add(offset), buffer, there)?;
    os.fill(buffer.wrapping_add(there), count - there, 0)
}

/// `FSEntry_PutBytes`: refused.
pub(super) fn put_bytes(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    Err(os.own_error(own, READ_ONLY, &[]))
}

/// `FSEntry_Args`: a file's extent, allocation and stamp read; a flush, an
/// image stamp and an I/O control taken; space never ensured.
pub(super) fn args(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let opened = os.resource_handle(regs.r[1])?;
    match regs.r[0] {
        fs::args::READ_EXTENT | fs::args::READ_ALLOCATED => regs.r[2] = opened.length,
        fs::args::READ_STAMP => {
            regs.r[2] = opened.load;
            regs.r[3] = opened.exec;
        }
        fs::args::FLUSH | fs::args::IMAGE_STAMP | fs::args::IOCTL => {}
        fs::args::ENSURE => return Err(os.error(UNSUPPORTED)),
        fs::args::WRITE_EXTENT | fs::args::ZEROS => return Err(os.own_error(own, READ_ONLY, &[])),
        _ => return Err(os.own_error(own, BAD_OPERATION, &[])),
    }
    Ok(())
}

/// `FSEntry_Close`: the handle let go.
pub(super) fn close(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.resource_handle(regs.r[1])?;
    let slot = (regs.r[1] as usize).wrapping_sub(1);
    if let Some(opened) = os.world.resource_fs.open.get_mut(slot) {
        *opened = None;
    }
    Ok(())
}

/// `FSEntry_File`: an object read or loaded, a file's block size; every
/// change refused.
pub(super) fn file(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match regs.r[0] {
        fs::file::READ_INFO => {
            let path = os.resource_path(regs.r[1])?;
            let found = os.resource(&path)?;
            give_info(regs, found, below_root(&path).is_none());
        }
        fs::file::LOAD => {
            let path = os.resource_path(regs.r[1])?;
            let Some(Object::File(entry)) = os.resource(&path)? else {
                return Err(os.own_error(own, BAD_OPERATION, &[]));
            };
            os.copy_bytes(entry.data, regs.r[2], entry.length)?;
            regs.r[2..6].copy_from_slice(&[entry.load, entry.exec, entry.length, entry.attributes]);
        }
        fs::file::BLOCK_SIZE => regs.r[2] = BUFFER,
        fs::file::SAVE..=fs::file::WRITE_ATTRIBUTES
        | fs::file::DELETE
        | fs::file::CREATE
        | fs::file::CREATE_DIRECTORY => return Err(os.own_error(own, READ_ONLY, &[])),
        _ => return Err(os.own_error(own, BAD_OPERATION, &[])),
    }
    Ok(())
}

/// `FSEntry_Func`: a directory set, an option and a shutdown taken, the
/// disc read as having no name, directories listed; a rename and an access
/// change refused, and anything else not done.
pub(super) fn func(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match regs.r[0] {
        fs::func::SET_DIRECTORY | fs::func::OPT | fs::func::SHUT_DOWN => Ok(()),
        fs::func::READ_DISC => os.write_bytes(regs.r[2], &[0, 0]),
        fs::func::READ_NAMES => list(os, regs, false),
        fs::func::READ_ENTRIES => list(os, regs, true),
        fs::func::RENAME | fs::func::ACCESS => Err(os.own_error(own, READ_ONLY, &[])),
        _ => Err(os.own_error(own, BAD_OPERATION, &[])),
    }
}

/// `FSEntry_Func` 14 and 15: from the child at R4, up to R3 of the children
/// of the directory at R1 into R2's R5 bytes, as names alone or each after
/// its record; R3 how many, R4 the next child's place, or -1 for none read.
fn list(os: &mut Os<'_>, regs: &mut Regs, records: bool) -> Result<(), Fail> {
    let path = os.resource_path(regs.r[1])?;
    let children = os.resource_children(&path)?;
    let (buffer, wanted, offset, room) = (regs.r[2], regs.r[3], regs.r[4], regs.r[5]);
    let mut at = buffer;
    let mut used: u32 = 0;
    let mut read: u32 = 0;
    for child in children.iter().skip(offset as usize).take(wanted as usize) {
        let named = len32(child.name.len()) + 1;
        let size = if records {
            (4 * RECORD_WORDS + named).next_multiple_of(4)
        } else {
            named
        };
        if used.saturating_add(size) > room {
            break;
        }
        if records {
            let entry = &child.entry;
            let kind = if child.file {
                kind::FILE
            } else {
                kind::DIRECTORY
            };
            for (n, word) in
                (0..).zip([entry.load, entry.exec, entry.length, entry.attributes, kind])
            {
                os.write_word(at.wrapping_add(4 * n), word)?;
            }
            os.write_terminated(
                at.wrapping_add(4 * RECORD_WORDS),
                &child.name,
                0,
                child.name.len() + 1,
            )?;
        } else {
            os.write_terminated(at, &child.name, 0, child.name.len() + 1)?;
        }
        at = at.wrapping_add(size);
        used += size;
        read += 1;
    }
    regs.r[3] = read;
    regs.r[4] = if read == 0 { u32::MAX } else { offset + read };
    Ok(())
}
