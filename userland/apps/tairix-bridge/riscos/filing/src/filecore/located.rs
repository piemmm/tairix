//! `OS_FSControl` 52: what holds a byte of a disc, named as RISC OS 5 names
//! it. The root's path is written first; the search then goes depth first,
//! entering each directory as it meets it and writing that directory's path,
//! and ends at the first entry the byte's fragment holds, writing its path
//! terminated. Every write starts at the buffer's start, so what lies past a
//! shorter one is an earlier, longer one's.
//!
//! The room the buffer gives is spent as RISC OS 5 spends it: each byte of
//! the root's path needs two more past it, else "Buffer overflow"; a byte of
//! the search's needs one more. A separator or terminator without it is
//! FileCore's "Buffer" error, while a name without it ends the search there,
//! answering an object, as RISC OS 5 answers.
//!
//! Each directory is entered once: a sound disc names each from one entry,
//! and a broken one naming a directory from within itself would otherwise
//! send the search round its loop for as long as names fit.

use alloc::vec::Vec;

use tairix_collections::OrderedSet;
use tairix_filecore::dir::MAX_NAME_LEN;
use tairix_filecore::volume::{Directory, Holder, Volume};
use tairix_filecore::{Disc, Error, Fault, Structure};

use super::names::{self, NAME_MOST};

/// The search's answer.
pub(super) enum Answer {
    /// R2's: [`FREE`], [`UNOWNED`] or [`OBJECT`].
    Kind(u32),
    /// The root's path did not fit.
    RootOverflow,
    /// A separator or terminator did not fit.
    Overflow,
}

/// Free space or a defect.
const FREE: u32 = 0;
/// Space no object holds: the map, the root, a fragment no entry names.
const UNOWNED: u32 = 1;
/// An object.
pub(super) const OBJECT: u32 = 2;

/// The name as written so far.
pub(super) struct Name {
    /// A path FileSwitch can name and one leaf past it.
    bytes: [u8; NAME_MOST + MAX_NAME_LEN + 2],
    written: usize,
    room: u32,
}

impl Name {
    /// Nothing written yet, in a buffer of `room` bytes.
    pub(super) const fn new(room: u32) -> Self {
        Self {
            bytes: [0; NAME_MOST + MAX_NAME_LEN + 2],
            written: 0,
            room,
        }
    }

    /// The bytes the search wrote, from the buffer's start.
    pub(super) fn written(&self) -> &[u8] {
        &self.bytes[..self.written]
    }

    /// `byte` at `at`, while the buffer has `spare` bytes of room past it.
    fn put(&mut self, at: usize, byte: u8, spare: u32) -> bool {
        let fits =
            u64::try_from(at).is_ok_and(|at| at + 1 + u64::from(spare) < u64::from(self.room));
        match self.bytes.get_mut(at) {
            Some(slot) if fits => {
                *slot = byte;
                self.written = self.written.max(at + 1);
                true
            }
            _ => false,
        }
    }

    /// `<system>::<disc>.$`, the filing system's name terminated as it is
    /// written: where it ends, or none where it does not fit.
    fn root(&mut self, system: &[u8], disc: &[u8]) -> Option<usize> {
        let mut at = 0;
        for &byte in system {
            self.put(at, byte, 1).then_some(())?;
            at += 1;
        }
        self.put(at, 0, 0).then_some(())?;
        for &byte in b"::".iter().chain(disc).chain(b".$") {
            self.put(at, byte, 1).then_some(())?;
            at += 1;
        }
        Some(at)
    }

    /// `.leaf` at `at`, terminated for the object found: where it ends, or
    /// the answer the search stops with.
    fn part(&mut self, at: usize, leaf: &[u8], found: bool) -> Result<usize, Answer> {
        if !self.put(at, b'.', 0) {
            return Err(Answer::Overflow);
        }
        let mut end = at + 1;
        for &byte in leaf {
            if !self.put(end, byte, 0) {
                return Err(Answer::Kind(OBJECT));
            }
            end += 1;
        }
        if found && !self.put(end, 0, 0) {
            return Err(Answer::Overflow);
        }
        Ok(end)
    }
}

/// A directory the search is in.
struct Level {
    addr: u32,
    dir: Directory,
    next: u32,
    /// Where its path ends in the name.
    at: usize,
}

/// What holds the byte `offset` into `volume`, filing system `system`'s,
/// its name written to `name`.
pub(super) fn find<D: Disc>(
    volume: &mut Volume<D>,
    system: &[u8],
    offset: u64,
    name: &mut Name,
) -> Result<Answer, Error<D::Error>> {
    let (disc, len) = names::disc_name(volume)?;
    let Some(root) = name.root(system, &disc[..len]) else {
        return Ok(Answer::RootOverflow);
    };
    let holder = volume.holder_at(offset)?;
    if matches!(holder, Holder::Free | Holder::Defect) {
        return Ok(Answer::Kind(FREE));
    }
    let mut levels = Vec::new();
    let mut entered = OrderedSet::new();
    enter(volume, &mut levels, &mut entered, volume.root_addr(), root)?;
    while let Some(level) = levels.last_mut() {
        if level.next == Volume::<D>::dir_len(&level.dir) {
            levels.pop();
            continue;
        }
        let index = level.next;
        level.next += 1;
        let at = level.at;
        let object = volume
            .dir_entry(&level.dir, level.addr, index)?
            .ok_or(Fault::Broken(Structure::Directory))?;
        let leaf = object.name();
        if holder.holds(&object) {
            return Ok(match name.part(at, leaf, true) {
                Ok(_) => Answer::Kind(OBJECT),
                Err(stop) => stop,
            });
        }
        // A path FileSwitch could not name holds nothing it could be told of.
        if object.is_dir() && at + 1 + leaf.len() <= NAME_MOST && !entered.contains(&object.indaddr)
        {
            match name.part(at, leaf, false) {
                Ok(end) => enter(volume, &mut levels, &mut entered, object.indaddr, end)?,
                Err(stop) => return Ok(stop),
            }
        }
    }
    Ok(Answer::Kind(UNOWNED))
}

/// The directory at `addr`, its path ending at `at`, entered, and noted in
/// `entered`.
fn enter<D: Disc>(
    volume: &mut Volume<D>,
    levels: &mut Vec<Level>,
    entered: &mut OrderedSet<u32>,
    addr: u32,
    at: usize,
) -> Result<(), Error<D::Error>> {
    let dir = volume.load_dir(addr)?;
    entered.try_insert(addr).map_err(|_| Fault::NoMemory)?;
    levels.try_reserve(1).map_err(|_| Fault::NoMemory)?;
    levels.push(Level {
        addr,
        dir,
        next: 0,
        at,
    });
    Ok(())
}

#[cfg(test)]
mod tests;
