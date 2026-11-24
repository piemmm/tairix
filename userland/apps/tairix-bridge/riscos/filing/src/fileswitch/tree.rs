//! Objects and the trees beneath them (`plans/RISCOS-EMULATOR.md` §6):
//! `OS_FSControl` 24 to 28 — access, rename, copy, wipe and count — as RISC
//! OS 5 performs them (`tests/corpus/filing-control.scenarios`).
//!
//! - A directory is read an entry at a time through `FSEntry_Func` 15; after
//!   an entry is deleted the same offset is read again, since what followed
//!   it has moved up.
//! - A wildcarded leaf names every entry of its directory it matches; for a
//!   copy, only where the destination's leaf is `*`.
//! - Image files are files here: copied, wiped and counted whole.

use alloc::vec::Vec;
use core::cell::Cell;

use tairix_riscos_kernel::error::{Error, BAD_PARAMETERS};
use tairix_riscos_kernel::filing::filetype::DATA;
use tairix_riscos_kernel::filing::fs;
use tairix_riscos_kernel::filing::NOT_FOUND;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::READ_C;

use super::call::attribute::{LOCKED, OWNER_READ, OWNER_WRITE, PUBLIC_READ, PUBLIC_WRITE};
use super::call::{self, Info, Need, DIRECTORY, NOT_FOUND as NOTHING};
use super::directory::{batch, walk, Fate, READS_MOST};
use super::name::{self, Resolved, Search};
use super::open::status;
use super::system::info;
use super::{error, file, gbpb, image, open};

/// The flags `OS_FSControl` 26 to 28 take in R3. A quick copy and one that
/// prompts for each change of medium are taken and change nothing here: no
/// copy uses application space, and no medium changes.
pub(crate) mod flag {
    pub(crate) const RECURSE: u32 = 1 << 0;
    pub(crate) const FORCE: u32 = 1 << 1;
    pub(crate) const TIMES: u32 = 1 << 2;
    pub(crate) const CONFIRM: u32 = 1 << 3;
    pub(crate) const VERBOSE: u32 = 1 << 4;
    pub(crate) const QUICK: u32 = 1 << 5;
    pub(crate) const PROMPT: u32 = 1 << 6;
    pub(crate) const DELETE: u32 = 1 << 7;
    pub(crate) const NO_ATTRIBUTES: u32 = 1 << 9;
    pub(crate) const STAMP: u32 = 1 << 10;
    pub(crate) const STRUCTURE: u32 = 1 << 11;
    pub(crate) const NEWER: u32 = 1 << 12;
    pub(crate) const LOOK: u32 = 1 << 14;
}

/// "Bad rename".
const BAD_RENAME: Error = Error {
    number: 0xB0,
    token: "BadRename",
};
/// A copy's destination leaf wildcarded but for `*`.
const WILD_DESTINATION: Error = Error {
    number: 0xB1,
    token: "WildDestination",
};

/// The attributes RISC OS 5 gives a locked object before writing over it or
/// deleting it.
const UNLOCKED: u32 = OWNER_READ | OWNER_WRITE;
/// The load and execution addresses RISC OS 5 writes with that unlocking
/// when a copy is to replace the object anyway.
const NO_STAMP: u32 = 0xDEAD_DEAD;

/// The most of a file a copy holds at once; a larger file is streamed
/// through it with both files open.
const COPY_BUFFER: u32 = 64 * 1024;
/// The deepest a recursive operation goes: a containment bound on a filing
/// system whose directories hold themselves.
const DEEPEST: u32 = 64;
/// The entries a directory a copy makes is given, as RISC OS 5 gives them.
const COPIED_DIRECTORY_ENTRIES: u32 = 35;

/// The attributes an access string gives: `R`, `W` and `L` the owner's,
/// `R` and `W` after a `/` the public's, case alike; none for any other.
fn parse_attributes(text: &[u8]) -> Option<u32> {
    let mut bits = 0;
    let mut public = false;
    for &byte in text {
        bits |= match (byte.to_ascii_uppercase(), public) {
            (b'/', false) => {
                public = true;
                0
            }
            (b'R', false) => OWNER_READ,
            (b'W', false) => OWNER_WRITE,
            (b'L', false) => LOCKED,
            (b'R', true) => PUBLIC_READ,
            (b'W', true) => PUBLIC_WRITE,
            _ => return None,
        };
    }
    Some(bits)
}

/// The stamps R4 to R7 bound where R3 asks for a range, as RISC OS 5 reads
/// them: R4 and R6 the low words, R5 and R7 the high bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Range {
    from: u64,
    to: u64,
}

impl Range {
    fn of(regs: &Regs) -> Option<Self> {
        (regs.r[3] & flag::TIMES != 0).then(|| Self {
            from: fs::stamp::of(regs.r[5], regs.r[4]),
            to: fs::stamp::of(regs.r[7], regs.r[6]),
        })
    }

    fn holds(range: Option<Self>, info: &Info) -> bool {
        range.is_none_or(|range| (range.from..=range.to).contains(&info.stamp()))
    }
}

/// Whether `info` is a directory these operations go into: an image file is
/// a file to them.
const fn is_directory(info: &Info) -> bool {
    info.kind == DIRECTORY
}

/// `parent`'s entry `leaf`, on the same filing system.
fn child(os: &mut Os<'_>, parent: &Resolved, leaf: &[u8]) -> Result<Resolved, Fail> {
    let mut path = Vec::new();
    if path
        .try_reserve_exact(parent.path.len() + 1 + leaf.len())
        .is_err()
    {
        return Err(os.no_memory());
    }
    path.extend_from_slice(&parent.path);
    path.push(b'.');
    path.extend_from_slice(leaf);
    Ok(Resolved {
        path,
        ..parent.clone()
    })
}

/// The directory `resolved` lies in.
fn parent(resolved: &Resolved) -> Resolved {
    let end = resolved.path.iter().rposition(|&b| b == b'.').unwrap_or(0);
    let mut above = resolved.clone();
    above.path.truncate(end);
    above
}

/// Whether two resolved names are the one object: the same path, case
/// alike, on the same filing system, special field and image.
fn same_object(a: &Resolved, b: &Resolved) -> bool {
    a.calls.number == b.calls.number
        && a.special == b.special
        && a.inside.map(|i| i.image) == b.inside.map(|i| i.image)
        && a.path.eq_ignore_ascii_case(&b.path)
}

/// "Not found", naming the name at `at`.
fn not_found(os: &mut Os<'_>, at: u32) -> Fail {
    match name::read(os, at) {
        Ok(given) => name::named_error(os, NOT_FOUND, &given),
        Err(fail) => fail,
    }
}

/// The object the name at `at` gives, as a wipe or a count reads it: found
/// by its name, or read once more where that found nothing.
fn existing(os: &mut Os<'_>, at: u32) -> Result<(Resolved, Info), Fail> {
    let (resolved, found) = name::find(os, at, Search::Plain, Need::Length)?;
    if found.kind != NOTHING {
        return Ok((resolved, found));
    }
    let again = name::read_object(os, &resolved.calls, resolved.named(), Need::Length)?;
    if again.kind == NOTHING {
        return Err(not_found(os, at));
    }
    Ok((resolved, again))
}

/// A directory a wipe or count was named or matched to read once more before
/// it is walked, by `FSEntry_File` 9, where its filing system has that call:
/// RISC OS 5 asks no other filing system again.
fn reread(os: &mut Os<'_>, directory: &Resolved) -> Result<(), Fail> {
    if directory.calls.has(info::FILE_9) {
        name::read_object(os, &directory.calls, directory.named(), Need::Kind)?;
    }
    Ok(())
}

/// The leaf pattern of the name at `at`, where it is wildcarded.
fn wild_leaf(os: &mut Os<'_>, at: u32) -> Result<Option<Vec<u8>>, Fail> {
    let leaf = name::given_leaf(os, at)?;
    Ok(name::wild(&leaf).then_some(leaf))
}

/// `FSEntry_File` 1 writing `load`, `exec` and `attributes` to what
/// `resolved` names.
fn write_info(
    os: &mut Os<'_>,
    resolved: &Resolved,
    load: u32,
    exec: u32,
    attributes: u32,
) -> Result<(), Fail> {
    let mut regs = Regs::default();
    regs.r[2] = load;
    regs.r[3] = exec;
    regs.r[5] = attributes;
    call::file_modifying(os, resolved, fs::file::WRITE_INFO, &mut regs)
}

/// `FSEntry_File` 6 deleting what `resolved` names, any image open on it
/// closed first.
fn delete(os: &mut Os<'_>, resolved: &Resolved, found: &Info) -> Result<(), Fail> {
    image::close_at(os, resolved)?;
    let mut gone = Regs::default();
    gone.r[2] = found.load;
    gone.r[3] = found.exec;
    call::file_modifying(os, resolved, fs::file::DELETE, &mut gone)
}

/// `OS_FSControl` 24: the attributes R2's string gives, and no others, set
/// on the object R1 names — on each its wildcarded leaf matches — through
/// `FSEntry_File` 1 with its other information as it has it.
pub(crate) fn access(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (resolved, found) = name::find(os, regs.r[1], Search::Plain, Need::Length)?;
    if found.kind == NOTHING {
        return Err(not_found(os, regs.r[1]));
    }
    let bits = if regs.r[2] == 0 {
        None
    } else {
        parse_attributes(&name::read(os, regs.r[2])?)
    };
    let Some(bits) = bits else {
        return Err(os.error(BAD_PARAMETERS));
    };
    let Some(pattern) = wild_leaf(os, regs.r[1])? else {
        return write_info(os, &resolved, found.load, found.exec, bits);
    };
    let directory = parent(&resolved);
    let mut offset = 0;
    for _ in 0..READS_MOST {
        let (entries, next) = batch(os, &directory, offset)?;
        for (leaf, entry) in entries {
            if name::matches(&pattern, &leaf) {
                let object = child(os, &directory, &leaf)?;
                write_info(os, &object, entry.load, entry.exec, bits)?;
            }
        }
        if next == u32::MAX || next == offset {
            break;
        }
        offset = next;
    }
    Ok(())
}

/// `OS_FSControl` 25: the object R1 names renamed to R2's name, which must
/// not exist unless it is the object itself and must be on the same filing
/// system, special field and image, by `FSEntry_Func` 8.
pub(crate) fn rename(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (from, found) = name::find(os, regs.r[1], Search::Plain, Need::Kind)?;
    if found.kind == NOTHING {
        return Err(not_found(os, regs.r[1]));
    }
    let mut to = name::locate(os, regs.r[2], Search::Plain)?;
    if !same_object(&from, &to) {
        let there = name::info(os, &mut to, Need::Kind)?;
        let alike = to.calls.number == from.calls.number
            && to.special == from.special
            && to.inside.map(|i| i.image) == from.inside.map(|i| i.image);
        if there.kind != NOTHING || !alike {
            return Err(error::fail(os, BAD_RENAME, &[]));
        }
    }
    image::close_at(os, &from)?;
    if !call::rename(os, &from, &to)? {
        return Err(error::fail(os, BAD_RENAME, &[]));
    }
    Ok(())
}

/// What an operation asks and tells as it goes, as R3's flags ask: before
/// each object, where it confirms, a question — Yes going ahead, No passing
/// the object over, Quiet going ahead and asking no more, Abandon leaving
/// the rest of its directory, Escape ending the operation; after each, where
/// it is verbose, a line.
struct Talk {
    confirm: Cell<bool>,
    verbose: bool,
}

/// A question's answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Answer {
    Yes,
    No,
    Abandon,
}

impl Answer {
    /// What a question's answer leaves of an object it was not to touch.
    const fn fate(self) -> Fate {
        match self {
            Self::Abandon => Fate::Abandoned,
            Self::Yes | Self::No => Fate::Kept,
        }
    }
}

impl Talk {
    fn of(flags: u32) -> Self {
        Self {
            confirm: Cell::new(flags & flag::CONFIRM != 0),
            verbose: flags & flag::VERBOSE != 0,
        }
    }

    /// The question before `verb` acts on `object`, and on to `to`.
    fn ask(
        &self,
        os: &mut Os<'_>,
        verb: &[u8],
        object: &[u8],
        to: Option<&[u8]>,
    ) -> Result<Answer, Fail> {
        if !self.confirm.get() {
            return Ok(Answer::Yes);
        }
        let mut question = Vec::new();
        for part in [verb, b" ", object] {
            question.extend_from_slice(part);
        }
        if let Some(to) = to {
            question.extend_from_slice(b" to ");
            question.extend_from_slice(to);
        }
        question.extend_from_slice(b" (Y/N/Q/A)? ");
        os.write_text(&question)?;
        loop {
            let mut regs = Regs::default();
            os.perform(tairix_riscos_kernel::swi::X | READ_C, &mut regs)?;
            if regs.flag(tairix_riscos_arm::psr::C) {
                os.write_line(b"")?;
                return Err(os.escape_acknowledged());
            }
            let key = regs.byte(0);
            let answer = match key.to_ascii_uppercase() {
                b'Y' => Answer::Yes,
                b'N' => Answer::No,
                b'Q' => {
                    self.confirm.set(false);
                    Answer::Yes
                }
                b'A' => Answer::Abandon,
                _ => continue,
            };
            os.write_line(&[key])?;
            return Ok(answer);
        }
    }

    /// The line after an object is acted on.
    fn tell(&self, os: &mut Os<'_>, parts: &[&[u8]]) -> Result<(), Fail> {
        if !self.verbose {
            return Ok(());
        }
        let line: Vec<u8> = parts.concat();
        os.write_line(&line)
    }
}

/// What a count totals: bytes as a 32-bit register sums them, and files.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Totals {
    bytes: u32,
    files: u32,
}

impl Totals {
    /// The file `object` names counted, where its stamp is in range and the
    /// count is to go ahead.
    fn add(
        &mut self,
        os: &mut Os<'_>,
        talk: &Talk,
        object: &Resolved,
        info: &Info,
        range: Option<Range>,
    ) -> Result<Fate, Fail> {
        if !Range::holds(range, info) {
            return Ok(Fate::Kept);
        }
        let answer = talk.ask(os, b"Count", &object.path, None)?;
        if answer != Answer::Yes {
            return Ok(answer.fate());
        }
        self.bytes = self.bytes.wrapping_add(info.length);
        self.files += 1;
        let mut length = Vec::new();
        super::listing::decimal(&mut length, info.length);
        talk.tell(os, &[&object.path, b" ", &length, b" bytes"])?;
        Ok(Fate::Kept)
    }
}

/// The files of `directory` counted into `totals`, those beneath it too
/// where recursive, each subdirectory read before it is entered.
fn count_in(
    os: &mut Os<'_>,
    directory: &Resolved,
    regs: &Regs,
    talk: &Talk,
    totals: &mut Totals,
    depth: u32,
) -> Result<(), Fail> {
    let range = Range::of(regs);
    walk(os, directory, |os, (leaf, entry)| {
        let below = child(os, directory, &leaf)?;
        if !is_directory(&entry) {
            return totals.add(os, talk, &below, &entry, range);
        }
        if regs.r[3] & flag::RECURSE != 0 && depth < DEEPEST {
            name::read_object(os, &below.calls, below.named(), Need::Kind)?;
            count_in(os, &below, regs, talk, totals, depth + 1)?;
        }
        Ok(Fate::Kept)
    })
    .map(drop)
}

/// `OS_FSControl` 28: the files R1 names counted — a file itself, a
/// directory's files, each its wildcarded leaf matches — R2 their bytes and
/// R3 how many.
pub(crate) fn count(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let counted = count_named(os, regs);
    let totals = counted.as_ref().copied().unwrap_or_default();
    regs.r[2] = totals.bytes;
    regs.r[3] = totals.files;
    counted.map(drop)
}

fn count_named(os: &mut Os<'_>, regs: &Regs) -> Result<Totals, Fail> {
    let talk = Talk::of(regs.r[3]);
    let mut totals = Totals::default();
    if let Some(pattern) = wild_leaf(os, regs.r[1])? {
        let directory = parent(&name::find(os, regs.r[1], Search::Plain, Need::Length)?.0);
        let range = Range::of(regs);
        walk(os, &directory, |os, (leaf, entry)| {
            if !name::matches(&pattern, &leaf) {
                return Ok(Fate::Kept);
            }
            let below = child(os, &directory, &leaf)?;
            if !is_directory(&entry) {
                return totals.add(os, &talk, &below, &entry, range);
            }
            if regs.r[3] & flag::RECURSE != 0 {
                reread(os, &below)?;
                count_in(os, &below, regs, &talk, &mut totals, 1)?;
            }
            Ok(Fate::Kept)
        })?;
        return Ok(totals);
    }
    let (resolved, found) = existing(os, regs.r[1])?;
    if is_directory(&found) {
        reread(os, &resolved)?;
        count_in(os, &resolved, regs, &talk, &mut totals, 0)?;
    } else {
        totals.add(os, &talk, &resolved, &found, Range::of(regs))?;
    }
    Ok(totals)
}

/// What a wipe does with each object: its flags and range, and what it asks
/// and tells.
struct Wipe {
    flags: u32,
    range: Option<Range>,
    talk: Talk,
}

impl Wipe {
    /// A file deleted where its stamp is in range: what became of it.
    fn file(&self, os: &mut Os<'_>, object: &Resolved, found: &Info) -> Result<Fate, Fail> {
        if !Range::holds(self.range, found) {
            return Ok(Fate::Kept);
        }
        self.delete(os, object, found)
    }

    /// An object deleted, a locked one only where forced, its lock taken off
    /// first: what became of it.
    fn delete(&self, os: &mut Os<'_>, object: &Resolved, found: &Info) -> Result<Fate, Fail> {
        let locked = found.attributes & LOCKED != 0;
        if locked && self.flags & flag::FORCE == 0 {
            return Ok(Fate::Kept);
        }
        let answer = self.talk.ask(os, b"Wipe", &object.path, None)?;
        if answer != Answer::Yes {
            return Ok(answer.fate());
        }
        if locked {
            write_info(os, object, found.load, found.exec, UNLOCKED)?;
        }
        delete(os, object, found)?;
        self.talk.tell(os, &[b"Deleted ", &object.path])?;
        Ok(Fate::Gone)
    }

    /// A directory's files deleted and, where recursive, its directories
    /// with all they hold, each read first: whether it was emptied.
    fn contents(&self, os: &mut Os<'_>, directory: &Resolved, depth: u32) -> Result<bool, Fail> {
        walk(os, directory, |os, (leaf, entry)| {
            let below = child(os, directory, &leaf)?;
            self.object(os, &below, &entry, depth + 1, true)
        })
    }

    /// An object within a wipe: a file, or where recursive a directory —
    /// read first, as RISC OS 5 reads one: by its kind where `read`, else
    /// only as [`reread`] does — wiped, then deleted if emptied.
    fn object(
        &self,
        os: &mut Os<'_>,
        object: &Resolved,
        found: &Info,
        depth: u32,
        read: bool,
    ) -> Result<Fate, Fail> {
        if !is_directory(found) {
            return self.file(os, object, found);
        }
        if self.flags & flag::RECURSE == 0 || depth > DEEPEST {
            return Ok(Fate::Kept);
        }
        let found = if read {
            name::read_object(os, &object.calls, object.named(), Need::Kind)?
        } else {
            reread(os, object)?;
            *found
        };
        if !self.contents(os, object, depth)? {
            return Ok(Fate::Kept);
        }
        self.delete(os, object, &found)
    }
}

/// `OS_FSControl` 27: the object R1 names wiped — each its wildcarded leaf
/// matches: a file deleted; a directory's files deleted, its directories too
/// where recursive, and the directory itself if that emptied it, a disc's
/// root but for itself. As RISC OS 5's, a wildcarded leaf is looked up as
/// any name is, then its directory listed by its filing system, which may
/// refuse one that is not there; a directory that lists nothing at all is
/// refused, one none of whose entries match is not.
pub(crate) fn wipe(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let wipe = Wipe {
        flags: regs.r[3],
        range: Range::of(regs),
        talk: Talk::of(regs.r[3]),
    };
    if let Some(pattern) = wild_leaf(os, regs.r[1])? {
        let directory = parent(&name::find(os, regs.r[1], Search::Plain, Need::Length)?.0);
        let mut listed = false;
        walk(os, &directory, |os, (leaf, entry)| {
            listed = true;
            if !name::matches(&pattern, &leaf) {
                return Ok(Fate::Kept);
            }
            let object = child(os, &directory, &leaf)?;
            wipe.object(os, &object, &entry, 1, false)
        })?;
        if listed {
            return Ok(());
        }
        let given = name::read(os, regs.r[1])?;
        return Err(error::fail(os, error::NOTHING_MATCHED, &[&given]));
    }
    let (resolved, found) = existing(os, regs.r[1])?;
    if !is_directory(&found) {
        return wipe.file(os, &resolved, &found).map(drop);
    }
    let root = resolved.inside.is_none() && name::is_root(&resolved.path);
    reread(os, &resolved)?;
    if wipe.contents(os, &resolved, 0)? && !root {
        wipe.delete(os, &resolved, &found)?;
    }
    Ok(())
}

/// A copy in progress: its flags and range, its buffer in the RMA, and what
/// it asks and tells.
struct Copy {
    flags: u32,
    range: Option<Range>,
    buffer: u32,
    size: u32,
    talk: Talk,
}

impl Copy {
    /// Whether a copy of `source` over `there` goes ahead: where nothing is
    /// there, or where forced — and then, where the newer alone is asked
    /// for, only if `source` is newer; "wrong kind" over a directory.
    fn over(
        &self,
        os: &mut Os<'_>,
        to: &Resolved,
        source: &Info,
        there: &Info,
    ) -> Result<bool, Fail> {
        if is_directory(there) {
            return Err(name::named_error(os, error::WRONG_KIND, &to.path));
        }
        let newer = self.flags & flag::NEWER == 0 || source.stamp() > there.stamp();
        Ok(there.kind == NOTHING || (self.flags & flag::FORCE != 0 && newer))
    }

    /// `source`'s information as its copy takes it: stamped now, keeping its
    /// type, where the copy asks.
    fn info(&self, os: &mut Os<'_>, source: &Info) -> Info {
        let mut info = *source;
        let stamped = fs::stamp::type_of(source.load).filter(|_| self.flags & flag::STAMP != 0);
        if let Some(kind) = stamped {
            (info.load, info.exec) = file::stamp_now(os, kind);
        }
        info
    }

    /// The file `from` copied to `to`, its destination looked at before or
    /// after the source is read as the copy asks: whether it was copied.
    fn file(
        &self,
        os: &mut Os<'_>,
        from: &Resolved,
        source: &Info,
        to: &mut Resolved,
    ) -> Result<bool, Fail> {
        if !Range::holds(self.range, source) || self.flags & flag::STRUCTURE != 0 {
            return Ok(false);
        }
        let look = self.flags & flag::LOOK != 0;
        let mut there = Info::default();
        if look {
            there = name::info(os, to, Need::Kind)?;
            if !self.over(os, to, source, &there)? {
                return Ok(false);
            }
        }
        let held = source.length <= self.size;
        if held {
            let handle = open_read(os, from)?;
            let read = gbpb::read_file(os, handle, self.buffer, source.length, None);
            let closed = open::close(os, handle);
            read?;
            closed?;
        }
        if !look {
            there = name::info(os, to, Need::Kind)?;
            if !self.over(os, to, source, &there)? {
                return Ok(false);
            }
        }
        if there.attributes & LOCKED != 0 && there.kind != NOTHING {
            write_info(os, to, NO_STAMP, NO_STAMP, UNLOCKED)?;
        }
        let info = self.info(os, source);
        if !held {
            self.stream(os, from, &info, to)?;
        } else if to.calls.has(info::SAVE_BY_OPEN) {
            make(os, to, &info)?;
            let handle = open_update(os, to)?;
            let written = gbpb::write_file(os, handle, self.buffer, info.length, None);
            let closed = open::close_stamped(os, handle, false);
            written?;
            closed?;
        } else {
            let mut save = Regs::default();
            let end = self.buffer.wrapping_add(info.length);
            save.r[2..6].copy_from_slice(&[info.load, info.exec, self.buffer, end]);
            call::file_modifying(os, to, fs::file::SAVE, &mut save)?;
        }
        self.finish(os, &info, to)?;
        Ok(true)
    }

    /// A file larger than the copy's buffer copied a buffer at a time, both
    /// files open, the destination made first.
    fn stream(
        &self,
        os: &mut Os<'_>,
        from: &Resolved,
        info: &Info,
        to: &Resolved,
    ) -> Result<(), Fail> {
        let read = open_read(os, from)?;
        let write = match make(os, to, info).and_then(|()| open_update(os, to)) {
            Ok(write) => write,
            Err(fail) => {
                open::close(os, read)?;
                return Err(fail);
            }
        };
        let mut done = 0;
        let mut moved = Ok(());
        while done < info.length && moved.is_ok() {
            let piece = (info.length - done).min(self.size);
            moved = gbpb::read_file(os, read, self.buffer, piece, None)
                .and_then(|_| gbpb::write_file(os, write, self.buffer, piece, None))
                .map(drop);
            done += piece;
        }
        let closed_read = open::close(os, read);
        let closed_write = open::close_stamped(os, write, false);
        moved?;
        closed_read?;
        closed_write
    }

    /// A copied object's information set as its source's, unless the copy
    /// asks for no attributes.
    fn finish(&self, os: &mut Os<'_>, info: &Info, to: &Resolved) -> Result<(), Fail> {
        if self.flags & flag::NO_ATTRIBUTES != 0 {
            return Ok(());
        }
        write_info(os, to, info.load, info.exec, info.attributes)
    }

    /// The directory `from` copied to `to`: `to` made where it is not — known
    /// to be there where it is `from` itself — given `from`'s information,
    /// and `from`'s files copied into it, its directories too where
    /// recursive; whether everything in `from` was moved out of it.
    fn directory(
        &self,
        os: &mut Os<'_>,
        from: &Resolved,
        source: &Info,
        to: &mut Resolved,
        same: bool,
    ) -> Result<bool, Fail> {
        let attributes = self.flags & flag::NO_ATTRIBUTES == 0;
        let there = if same {
            *source
        } else {
            name::info(os, to, Need::Kind)?
        };
        if there.kind == NOTHING {
            let (load, exec) = file::stamp_now(os, DATA);
            let mut made = Regs::default();
            made.r[2..5].copy_from_slice(&[load, exec, COPIED_DIRECTORY_ENTRIES]);
            call::file_modifying(os, to, fs::file::CREATE_DIRECTORY, &mut made)?;
            if attributes {
                name::read_object(os, &to.calls, to.named(), Need::Length)?;
            }
        } else if !is_directory(&there) {
            return Err(name::named_error(os, error::WRONG_KIND, &to.path));
        }
        self.finish(os, source, to)?;
        name::read_object(os, &from.calls, from.named(), Need::Kind)?;
        walk(os, from, |os, (leaf, entry)| {
            self.member(os, from, &leaf, &entry, to)
        })
    }

    /// The entry `leaf` of `from`, whose information is `entry`, copied into
    /// `to`: a file copied; a directory, where recursive, copied with all it
    /// holds and deleted after where the copy asks and emptied it.
    fn member(
        &self,
        os: &mut Os<'_>,
        from: &Resolved,
        leaf: &[u8],
        entry: &Info,
        to: &Resolved,
    ) -> Result<Fate, Fail> {
        let below = child(os, from, leaf)?;
        let mut target = child(os, to, leaf)?;
        if !is_directory(entry) {
            return self.entry(os, &below, entry, &mut target);
        }
        if self.flags & flag::RECURSE == 0 || depth_of(from) >= DEEPEST {
            return Ok(Fate::Kept);
        }
        let emptied = self.directory(os, &below, entry, &mut target, false)?;
        if emptied && self.flags & flag::DELETE != 0 {
            delete(os, &below, entry)?;
            return Ok(Fate::Gone);
        }
        Ok(Fate::Kept)
    }

    /// A file entry copied, and deleted after where the copy asks: what
    /// became of it.
    fn entry(
        &self,
        os: &mut Os<'_>,
        from: &Resolved,
        source: &Info,
        to: &mut Resolved,
    ) -> Result<Fate, Fail> {
        let answer = self.talk.ask(os, b"Copy", &from.path, Some(&to.path))?;
        if answer != Answer::Yes {
            return Ok(answer.fate());
        }
        if !self.file(os, from, source, to)? {
            return Ok(Fate::Kept);
        }
        self.talk
            .tell(os, &[b"Copied ", &from.path, b" to ", &to.path])?;
        if self.flags & flag::DELETE != 0 {
            delete(os, from, source)?;
            return Ok(Fate::Gone);
        }
        Ok(Fate::Kept)
    }
}

/// How deep `resolved` lies: its path's parts, a bound on recursion.
fn depth_of(resolved: &Resolved) -> u32 {
    resolved
        .path
        .iter()
        .fold(0, |parts, &b| parts + u32::from(b == b'.'))
}

/// `resolved` opened to be read.
fn open_read(os: &mut Os<'_>, resolved: &Resolved) -> Result<u32, Fail> {
    let handle = open::next_handle(os)?;
    let Some(opened) = call::open(
        os,
        &resolved.calls,
        fs::open::READ,
        resolved.named(),
        handle,
    )?
    else {
        return Err(name::named_error(os, NOT_FOUND, &resolved.path));
    };
    open::record(os, resolved.clone(), opened, 0, handle)
}

/// `resolved` opened for update, after `UpCall_ModifyingFile` tells of it.
fn open_update(os: &mut Os<'_>, resolved: &Resolved) -> Result<u32, Fail> {
    let handle = open::next_handle(os)?;
    let Some(opened) = call::open_for_update(os, resolved, handle)? else {
        return Err(name::named_error(os, NOT_FOUND, &resolved.path));
    };
    open::record(
        os,
        resolved.clone(),
        opened,
        status::WRITE | status::MODIFIED,
        handle,
    )
}

/// The file `to` made empty to `info`'s length, stamped as `info` is, by
/// `FSEntry_File` 7.
fn make(os: &mut Os<'_>, to: &Resolved, info: &Info) -> Result<(), Fail> {
    let mut regs = Regs::default();
    regs.r[2..6].copy_from_slice(&[info.load, info.exec, 0, info.length]);
    call::file_modifying(os, to, fs::file::CREATE, &mut regs)
}

/// `OS_FSControl` 26: the object R1 names copied to R2's name, as R3's
/// flags ask. A destination leaf of `*` takes the source's, and with a
/// wildcarded source every entry of its directory it matches; a wildcarded
/// source is otherwise named as it was given.
pub(crate) fn copy(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (from, found) = name::find(os, regs.r[1], Search::Plain, Need::Length)?;
    let mut to = name::locate(os, regs.r[2], Search::Plain)?;
    let same = same_object(&from, &to);
    if !same {
        name::info(os, &mut to, Need::Length)?;
    }
    let target_leaf = name::given_leaf(os, regs.r[2])?;
    let into = target_leaf == b"*";
    if !into && name::wild(&target_leaf) {
        return Err(error::fail(os, WILD_DESTINATION, &[]));
    }
    let pattern = wild_leaf(os, regs.r[1])?;
    let size = if found.kind == DIRECTORY || pattern.is_some() {
        COPY_BUFFER
    } else {
        found.length.clamp(4, COPY_BUFFER)
    };
    let buffer = os.rma_claim(size)?;
    let copy = Copy {
        flags: regs.r[3],
        range: Range::of(regs),
        buffer,
        size,
        talk: Talk::of(regs.r[3]),
    };
    let copied = copy_named(
        os,
        &copy,
        regs.r[1],
        &from,
        &found,
        to,
        into,
        pattern.as_deref(),
        same,
    );
    os.rma_free(buffer)?;
    copied
}

/// [`copy`] once its buffer is held.
#[expect(
    clippy::too_many_arguments,
    reason = "the parts of one copy's naming, resolved once by copy"
)]
fn copy_named(
    os: &mut Os<'_>,
    copy: &Copy,
    at: u32,
    from: &Resolved,
    found: &Info,
    mut to: Resolved,
    into: bool,
    pattern: Option<&[u8]>,
    same: bool,
) -> Result<(), Fail> {
    if let (true, Some(pattern)) = (into, pattern) {
        let directory = parent(from);
        let destination = parent(&to);
        return walk(os, &directory, |os, (leaf, entry)| {
            if !name::matches(pattern, &leaf) {
                return Ok(Fate::Kept);
            }
            copy.member(os, &directory, &leaf, &entry, &destination)
        })
        .map(drop);
    }
    let source_path = match pattern {
        Some(pattern) => child(os, &parent(from), pattern)?,
        None => from.clone(),
    };
    let root = source_path.inside.is_none() && name::is_root(&source_path.path);
    let source = if same || root {
        *found
    } else {
        name::read_object(os, &source_path.calls, source_path.named(), Need::Length)?
    };
    if source.kind == NOTHING {
        return Err(not_found(os, at));
    }
    if into {
        to = child(os, &parent(&to), name::leaf(&source_path.path))?;
    }
    if is_directory(&source) {
        let emptied = copy.directory(os, &source_path, &source, &mut to, same)?;
        if emptied && copy.flags & flag::DELETE != 0 {
            delete(os, &source_path, &source)?;
        }
        return Ok(());
    }
    copy.entry(os, &source_path, &source, &mut to).map(drop)
}

#[cfg(test)]
mod tests;
