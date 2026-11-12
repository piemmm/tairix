//! The walk of the tree, as DOS 4.0's `CHKDSK` walks it: in each directory
//! an extended FCB search finds every entry, its `.` and `..` first; each
//! chain is followed and marked, a subdirectory entered by `CHDIR` and left
//! by `CHDIR ..`. A fix is the entry rewritten in its sector, found by the
//! index the search keeps in its FCB. The second walk, made only when a
//! cluster was crossed, names each file that holds one.

use super::cells::{self, mark, Ending};
use super::{
    absolute, add, at, cluster_bytes, count, current_directory, failed, flag, has, held, level,
    path_with, set, spelt, switch, switched, text, volume, Ask, LEVELS,
};
use crate::dos::program::{say, say_error, Flow, Frame, Id, Int, Program, Returned, Routine};
use crate::dos::shell::set_asciz;
use crate::frames::{locals, take, Far, Outcome};
use alloc::vec::Vec;
use tairix_fat::dir::{attr, ShortEntry, DELETED, DOT, DOTDOT, ENTRY};

/// Where the entry a search found lies in the DTA: after an extended FCB's
/// header and the drive.
const FOUND: u16 = at::DTA + 8;
/// Where a search FCB keeps the index of the entry it found last, past an
/// extended header.
const SEARCH_INDEX: u16 = 7 + 0x0D;
/// What a search looks for: every entry, the hidden, the system's, labels
/// and directories among them.
const EVERY: u8 = attr::HIDDEN | attr::SYSTEM | attr::VOLUME_ID | attr::DIRECTORY;

/// Where level `n` of the walk lies in the block.
fn level_at(n: u8) -> u16 {
    u16::from(n) * level::LEN
}

/// The problems `CHKDSK` says of an entry or a directory.
pub mod said {
    pub const BAD_CHAIN: u8 = 0;
    pub const BAD_SUBDIRECTORY: u8 = 1;
    pub const MISSING: u8 = 2;
    pub const FIRST_INVALID: u8 = 3;
    pub const SIZE_ADJUSTED: u8 = 4;
    pub const DOT_LOST: u8 = 5;
    pub const EMPTY: u8 = 6;
    pub const JOINED: u8 = 7;
    pub const DOTDOT_LOST: u8 = 8;
    pub const BAD_LINK: u8 = 9;
    pub const BAD_ATTRIBUTE: u8 = 10;
    pub const BAD_SIZE: u8 = 11;
    pub const CROSSED: u8 = 12;
    pub const NO_CHDIR: u8 = 13;
    pub const UP_FAILED: u8 = 14;
    pub const FILE: u8 = 15;
    pub const DIRECTORY: u8 = 16;
    pub const ROOT_FULL: u8 = 17;
    /// Only the errors left unwritten, if not yet said.
    pub const ERRORS: u8 = 18;
}

/// Where a message puts the path it is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Path {
    /// On the standard error, a line before it.
    Above,
    /// An empty line before it, for a message about no one entry.
    Blank,
    /// Within it, after these words.
    Within(&'static [u8]),
    /// Nowhere.
    Without,
}

#[derive(Clone, Copy, Debug)]
struct Message {
    words: &'static [u8],
    handle: u16,
    path: Path,
    /// The errors left unwritten are said once before it.
    errors: bool,
    /// "tree past this point not processed" follows it.
    abandoned: bool,
    /// The walk it is said on: the second's, or the first's.
    second: bool,
}

const fn message(words: &'static [u8], handle: u16, path: Path, errors: bool) -> Message {
    Message {
        words,
        handle,
        path,
        errors,
        abandoned: false,
        second: false,
    }
}

const fn abandoning(message: Message) -> Message {
    Message {
        abandoned: true,
        ..message
    }
}

const MESSAGES: [Message; 19] = [
    message(b"   Has invalid allocation unit, file truncated", 1, Path::Above, true),
    message(b"   Invalid sub-directory entry", 2, Path::Above, true),
    message(b"   Does not exist", 2, Path::Above, true),
    message(b"   First allocation unit is invalid, entry truncated", 1, Path::Above, true),
    message(b"   Allocation error, size adjusted", 1, Path::Above, true),
    message(b"   Cannot recover .. entry, processing continued", 1, Path::Above, true),
    abandoning(message(b"   Directory is totally empty, no . or ..", 1, Path::Above, true)),
    abandoning(message(b"   Directory is joined", 2, Path::Without, false)),
    abandoning(message(b"   Cannot recover .. entry", 1, Path::Above, true)),
    message(b"   Entry has a bad link", 1, Path::Above, true),
    message(b"   Entry has a bad attribute", 1, Path::Above, true),
    message(b"   Entry has a bad size", 1, Path::Above, true),
    Message { second: true, ..message(b"   Is cross linked on allocation unit ", 1, Path::Above, false) },
    message(b",\r\ntree past this point not processed", 1, Path::Within(b"   Cannot CHDIR to "), true),
    message(b"   CHDIR .. failed, trying alternate method", 1, Path::Above, true),
    message(b"", 1, Path::Within(b"        "), false),
    message(b"", 1, Path::Within(b"Directory "), false),
    message(b"   Insufficient room in root directory\r\n   Move files from root directory and repeat CHKDSK", 2, Path::Blank, true),
    message(b"", 1, Path::Without, true),
];

const NOT_PROCESSED: &[u8] = b"   tree past this point not processed\r\n";

locals! {
    /// A problem said as DOS's `CHKDSK` says it, on its walk alone: the
    /// path it is about — the current directory, and [`at::NAME`] in it —
    /// on a line of its own or within the message, and `number` after it,
    /// the errors left unwritten said once first.
    pub struct Tell {
        what: u8,
        number: u16,
    }
}

impl Tell {
    #[must_use]
    pub const fn of(what: u8) -> Self {
        Self { what, number: 0 }
    }

    #[must_use]
    pub const fn with(what: u8, number: u16) -> Self {
        Self { what, number }
    }

    fn message(self) -> Message {
        MESSAGES[usize::from(self.what).min(MESSAGES.len() - 1)]
    }

    fn on(p: &mut Program<'_, '_>, handle: u16, words: &[u8]) -> Frame {
        if handle == 2 {
            say_error(p, words)
        } else {
            say(p, words)
        }
    }

    /// The message itself, once its path is said or within it.
    fn words(self, p: &mut Program<'_, '_>, phase: &mut u8, path: &[u8]) -> Flow {
        let message = self.message();
        let mut line = Vec::new();
        if let Path::Within(lead) = message.path {
            line.extend_from_slice(lead);
            line.extend_from_slice(path);
        }
        line.extend_from_slice(message.words);
        if message.second {
            line.extend_from_slice(&crate::dos::message::decimal(u32::from(self.number)));
        }
        if line.is_empty() {
            return Flow::Return(Ok(0));
        }
        line.extend_from_slice(text::NEW_LINE);
        *phase = telling::SAID;
        Flow::Call(Self::on(p, message.handle, &line))
    }
}

mod telling {
    pub const ERRORS_SAID: u8 = 1;
    pub const AT: u8 = 2;
    pub const ABOVE: u8 = 3;
    pub const SAID: u8 = 4;
    pub const ABANDONED: u8 = 5;
}

impl Routine for Tell {
    const ID: Id = Id::ChkdskTell;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let message = self.message();
        match *phase {
            0 => {
                if message.second != has(p, flag::SECOND_PASS) {
                    return Flow::Return(Ok(0));
                }
                *phase = telling::ERRORS_SAID;
                if message.errors && !has(p, flag::ERRORS_SAID) {
                    set(p, flag::ERRORS_SAID);
                    if !switched(p, switch::FIX) {
                        return Flow::Call(say(p, text::ERRORS));
                    }
                }
                self.step(p, phase, Ok(0))
            }
            telling::ERRORS_SAID => match message.path {
                Path::Above | Path::Within(_) => {
                    *phase = telling::AT;
                    Flow::Call(current_directory(p))
                }
                Path::Blank => {
                    *phase = telling::ABOVE;
                    Flow::Call(say_error(p, text::NEW_LINE))
                }
                Path::Without => self.words(p, phase, &[]),
            },
            telling::AT => {
                let name = crate::dos::shell::asciz_at(p, at::NAME, 13);
                let path = path_with(p, &name);
                if message.path == Path::Above {
                    *phase = telling::ABOVE;
                    return Flow::Call(say_error(p, &[&path[..], text::NEW_LINE].concat()));
                }
                self.words(p, phase, &path)
            }
            telling::ABOVE => {
                take!(child);
                self.words(p, phase, &[])
            }
            telling::SAID if message.abandoned => {
                *phase = telling::ABANDONED;
                Flow::Call(say(p, NOT_PROCESSED))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

locals! {
    /// Entry `index` of the directory whose first cluster is `directory`,
    /// in its sector, read and, as `mode` asks, changed and written back:
    /// `1` when it was there to take.
    pub struct FixEntry {
        directory: u16,
        index: u16,
        mode: u8,
        cluster: u16,
        sector: u32,
        offset: u16,
    }
}

/// What a [`FixEntry`] does with its entry.
pub mod fix {
    /// The searched entry in the DTA written over it, given `/F`.
    pub const WRITE: u8 = 0;
    /// Made `.` or `..`, linked to `cluster`, if its slot is free; written
    /// given `/F`.
    pub const DOT: u8 = 1;
    pub const DOTDOT: u8 = 2;
    /// Read into the DTA.
    pub const READ: u8 = 3;
}

mod fixing {
    pub const TIME: u8 = 1;
    pub const DATE: u8 = 2;
    pub const READ: u8 = 3;
    pub const WRITTEN: u8 = 4;
    pub const FAULT_SAID: u8 = 5;
}

impl FixEntry {
    #[must_use]
    pub const fn of(directory: u16, index: u16, mode: u8, cluster: u16) -> Self {
        Self {
            directory,
            index,
            mode,
            cluster,
            sector: 0,
            offset: 0,
        }
    }

    /// The sector the entry lies in and its offset there: the root's run of
    /// sectors, or a subdirectory's chain in the FAT held, followed as far
    /// as the entry.
    fn place(self, p: &mut Program<'_, '_>) -> Option<(u32, u16)> {
        let sector = p.get16(at::SECTOR_BYTES);
        let per_sector = sector / 32;
        let (nth, slot) = (self.index / per_sector, self.index % per_sector);
        if self.directory == 0 {
            return (nth < p.get16(at::ROOT_ENTRIES).div_ceil(per_sector)).then(|| {
                (
                    u32::from(p.get16(at::ROOT_SECTOR)) + u32::from(nth),
                    slot * 32,
                )
            });
        }
        let per_cluster = u16::from(p.get8(at::CLUSTER_SECTORS));
        let volume = volume(p);
        let first_data = u32::from(p.get16(at::FIRST_DATA));
        let mut cells = held(p);
        let mut cluster = self.directory;
        for _ in 0..nth / per_cluster {
            if !volume.holds(cluster) {
                return None;
            }
            cluster = cells::Cells::entry(&mut cells, cluster);
        }
        volume.holds(cluster).then(|| {
            let start = first_data + u32::from(cluster - 2) * u32::from(per_cluster);
            (start + u32::from(nth % per_cluster), slot * 32)
        })
    }

    fn write(self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = fixing::WRITTEN;
        Flow::Call(absolute(
            p,
            true,
            self.sector,
            1,
            Far::new(p.own, at::BUFFER),
        ))
    }

    /// The sector is in the buffer: the entry changed as asked.
    fn change(self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let at = at::BUFFER + self.offset;
        match self.mode {
            fix::READ => {
                let mut raw = [0u8; ENTRY];
                p.g.read_bytes(p.own, at, &mut raw);
                p.g.write_bytes(p.own, FOUND, &raw);
                Flow::Return(Ok(1))
            }
            fix::DOT | fix::DOTDOT => {
                if p.get8(at) != DELETED {
                    return Flow::Return(Ok(0));
                }
                let stamp = p.get32(at::STAMP);
                let mut entry = ShortEntry {
                    name: if self.mode == fix::DOT { DOT } else { DOTDOT },
                    attributes: attr::DIRECTORY,
                    cluster: u32::from(self.cluster),
                    ..ShortEntry::default()
                };
                entry.stamps.written.date = u16::try_from(stamp >> 16).unwrap_or(0);
                entry.stamps.written.time = u16::try_from(stamp & 0xFFFF).unwrap_or(0);
                p.g.write_bytes(p.own, at, &entry.encode());
                if !switched(p, switch::FIX) {
                    return Flow::Return(Ok(1));
                }
                self.write(p, phase)
            }
            _ => {
                let mut raw = [0u8; ENTRY];
                p.g.read_bytes(p.own, FOUND, &mut raw);
                p.g.write_bytes(p.own, at, &raw);
                self.write(p, phase)
            }
        }
    }
}

impl Routine for FixEntry {
    const ID: Id = Id::ChkdskFix;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 if self.mode == fix::WRITE && !switched(p, switch::FIX) => Flow::Return(Ok(1)),
            0 if matches!(self.mode, fix::DOT | fix::DOTDOT) => {
                *phase = fixing::TIME;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x2C00)))
            }
            fixing::TIME => {
                take!(child);
                super::keep_clock(p);
                *phase = fixing::DATE;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x2A00)))
            }
            fixing::DATE => {
                take!(child);
                let (cx, dx) = (p.result(Returned::Cx), p.result(Returned::Dx));
                let stamp = super::now(p, cx, dx);
                p.set32(at::STAMP, stamp);
                self.locate(p, phase)
            }
            0 => self.locate(p, phase),
            fixing::READ => {
                if failed(p) {
                    p.set32(at::BUFFERED, u32::MAX);
                    *phase = fixing::FAULT_SAID;
                    return Flow::Call(say_error(p, text::READ_FAULT));
                }
                p.set32(at::BUFFERED, self.sector);
                self.change(p, phase)
            }
            fixing::WRITTEN => {
                if failed(p) {
                    p.set32(at::BUFFERED, u32::MAX);
                    *phase = fixing::FAULT_SAID;
                    return Flow::Call(say_error(p, text::WRITE_FAULT));
                }
                Flow::Return(Ok(1))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

impl FixEntry {
    /// The entry's sector read, unless the buffer holds it.
    fn locate(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let Some((sector, offset)) = self.place(p) else {
            return Flow::Return(Ok(0));
        };
        (self.sector, self.offset) = (sector, offset);
        if p.get32(at::BUFFERED) == sector {
            return self.change(p, phase);
        }
        *phase = fixing::READ;
        Flow::Call(absolute(p, false, sector, 1, Far::new(p.own, at::BUFFER)))
    }
}

/// The entry the last search found.
fn found(p: &mut Program<'_, '_>) -> ShortEntry {
    let mut raw = [0u8; ENTRY];
    p.g.read_bytes(p.own, FOUND, &mut raw);
    ShortEntry::decode(&raw)
}

fn set_found(p: &mut Program<'_, '_>, entry: &ShortEntry) {
    p.g.write_bytes(p.own, FOUND, &entry.encode());
}

/// An entry's first cluster, as DOS keeps it: the low word alone.
fn first_of(entry: &ShortEntry) -> u16 {
    u16::try_from(entry.cluster & 0xFFFF).unwrap_or(0)
}

fn set_first(entry: &mut ShortEntry, cluster: u16) {
    entry.cluster = (entry.cluster & 0xFFFF_0000) | u32::from(cluster);
}

fn depth(p: &mut Program<'_, '_>) -> u8 {
    p.get8(at::DEPTH)
}

fn level8(p: &mut Program<'_, '_>, field: u16) -> u8 {
    let (block, depth) = (p.get16(at::BLOCK), depth(p));
    p.g.read8(block, level_at(depth) + field)
}

fn set_level8(p: &mut Program<'_, '_>, field: u16, value: u8) {
    let (block, depth) = (p.get16(at::BLOCK), depth(p));
    p.g.write8(block, level_at(depth) + field, value);
}

fn level16(p: &mut Program<'_, '_>, field: u16) -> u16 {
    let (block, depth) = (p.get16(at::BLOCK), depth(p));
    p.g.read16(block, level_at(depth) + field)
}

/// The search call, first or next, through the level's FCB.
fn search(p: &mut Program<'_, '_>, next: bool) -> Frame {
    let (block, depth) = (p.get16(at::BLOCK), depth(p));
    Frame::of(&Int {
        dx: level_at(depth) + level::FCB,
        ds: block,
        ..Int::dos(p.own, if next { 0x1200 } else { 0x1100 })
    })
}

/// The invalid entry of the directory, said once: what is said of its `.`
/// and `..` without `/V`.
fn invalid_once(p: &mut Program<'_, '_>) -> Option<u8> {
    if level8(p, level::SAID) != 0 {
        return None;
    }
    set_level8(p, level::SAID, 1);
    set_asciz(p, at::NAME, b"", 13);
    Some(said::BAD_SUBDIRECTORY)
}

/// What is said of a `.` or `..`: `what`, with `/V`; else the directory's
/// invalid entry, once.
fn of_dot(p: &mut Program<'_, '_>, what: u8, name: &[u8]) -> Option<u8> {
    if switched(p, switch::VERBOSE) {
        set_asciz(p, at::NAME, name, 13);
        return Some(what);
    }
    invalid_once(p)
}

locals! {
    /// The walk of the tree, the first or, once a cluster is found crossed,
    /// the second: a level of its block for each directory it is in, the
    /// entry it is at in the DTA.
    pub struct ChkdskTree {
        /// The chain the entry holds, how following it ended, and where.
        clusters: u32,
        ending: u8,
        last: u16,
        /// The last search found an entry.
        found: bool,
        /// What a splice check decides between, and which of a `.` or `..`
        /// entry's checks is next.
        context: u8,
        check: u8,
        /// The entry is to be written once its problem is said, and was
        /// truncated to nothing.
        write: bool,
        emptied: bool,
    }
}

/// How a chain ended, kept in [`ChkdskTree`]'s locals.
mod ended {
    pub const END: u8 = 0;
    pub const CROSSED: u8 = 1;
    pub const BAD_FIRST: u8 = 2;
    pub const BAD_LINK: u8 = 3;
}

/// What a splice check is made for.
mod context {
    pub const EMPTY: u8 = 0;
    pub const NO_DOT: u8 = 1;
}

mod walking {
    pub const ANNOUNCED: u8 = 1;
    pub const FIRST: u8 = 2;
    pub const SPLICE_AT: u8 = 3;
    pub const SPLICED: u8 = 4;
    pub const LEAVE: u8 = 5;
    pub const ABANDON: u8 = 6;
    pub const DOT_SAID: u8 = 7;
    pub const DOT_FILLED: u8 = 8;
    pub const DOT_LOST_SAID: u8 = 9;
    pub const AFTER_DOT: u8 = 10;
    pub const DOTDOT_SAID: u8 = 11;
    pub const DOTDOT_FILLED: u8 = 12;
    pub const CHECK_SAID: u8 = 13;
    pub const CHECK_FIXED: u8 = 14;
    pub const NEXT_FOUND: u8 = 15;
    pub const ROSE: u8 = 16;
    pub const ROSE_SAID: u8 = 17;
    pub const ROSE_AGAIN: u8 = 18;
    pub const FILE_SAID: u8 = 19;
    pub const CHAIN_SAID: u8 = 20;
    pub const ENTRY_FIXED: u8 = 21;
    pub const CROSS_SAID: u8 = 22;
    pub const ENTERED: u8 = 23;
    pub const BOGUS_SAID: u8 = 24;
    pub const NO_CHDIR_SAID: u8 = 25;
    pub const UNRECOVERABLE_SAID: u8 = 26;
    pub const CONVERT_ASKED: u8 = 27;
    pub const CONVERT_READ: u8 = 28;
    pub const CONVERTED: u8 = 29;
}

impl ChkdskTree {
    fn second(p: &mut Program<'_, '_>) -> bool {
        has(p, flag::SECOND_PASS)
    }

    /// Says `what`, if anything, then goes on at `then`.
    fn tell(
        &mut self,
        p: &mut Program<'_, '_>,
        phase: &mut u8,
        what: Option<u8>,
        then: u8,
    ) -> Flow {
        *phase = then;
        match what {
            Some(what) => Flow::Call(Frame::of(&Tell::of(what))),
            None => self.step(p, phase, Ok(0)),
        }
    }

    /// The directory the walk has just come to: its level readied, its
    /// line said with `/V`, then its first entry searched for.
    fn enter(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let (block, depth) = (p.get16(at::BLOCK), depth(p));
        let mut fcb = [0u8; level::SELF as usize];
        fcb[0] = 0xFF;
        fcb[6] = EVERY;
        fcb[7] = p.get8(at::DRIVE) + 1;
        fcb[8..19].fill(b'?');
        p.g.write_bytes(block, level_at(depth) + level::FCB, &fcb);
        set_level8(p, level::SAID, 0);
        set_level8(p, level::DOTS_BAD, 0);
        *phase = walking::ANNOUNCED;
        if switched(p, switch::VERBOSE) && !Self::second(p) {
            set_asciz(p, at::NAME, b"", 13);
            return Flow::Call(Frame::of(&Tell::of(said::DIRECTORY)));
        }
        *phase = walking::FIRST;
        Flow::Call(search(p, false))
    }

    /// Whether the directory is the walk's first, the root.
    fn at_root(p: &mut Program<'_, '_>) -> bool {
        level16(p, level::SELF) == 0
    }

    /// The directory is left for its parent, its `.` or `..` past mending
    /// when `bad`.
    fn leave(p: &mut Program<'_, '_>, phase: &mut u8, bad: bool) -> Flow {
        if bad {
            set_level8(p, level::DOTS_BAD, 1);
        }
        *phase = walking::ROSE;
        Flow::Call(Frame::of(&Int {
            dx: at::UP,
            ..Int::dos(p.own, 0x3B00)
        }))
    }

    /// Back in the parent, whose search goes on, or which asks to make the
    /// directory left a file.
    fn popped(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let bad = level8(p, level::DOTS_BAD) != 0;
        let up = depth(p).saturating_sub(1);
        p.set8(at::DEPTH, up);
        if bad {
            return Self::ask_convert(p, phase);
        }
        Self::next(p, phase)
    }

    fn next(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = walking::NEXT_FOUND;
        Flow::Call(search(p, true))
    }

    /// The directory's next entry, or its end.
    fn entries(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if self.found {
            return self.entry(p, phase);
        }
        if Self::at_root(p) {
            return Flow::Return(Ok(0));
        }
        Self::leave(p, phase, false)
    }

    /// Whether the current directory is another drive's root `JOIN`
    /// spliced on, from its true name.
    fn spliced(p: &mut Program<'_, '_>) -> bool {
        let mut name = [0u8; 4];
        p.g.read_bytes(p.own, at::BUFFER, &mut name);
        p.set32(at::BUFFERED, u32::MAX);
        name[1..] == *b":\\\0" && name[0] != p.get8(at::PATH)
    }

    /// The `.` entry checked: its attribute, its link to the directory
    /// itself and its size, each put right when wrong; then `..`'s with the
    /// parent.
    fn dot_checks(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        while self.check < 6 {
            let dot = self.check < 3;
            let link = level16(p, if dot { level::SELF } else { level::PARENT });
            let mut entry = found(p);
            let problem = match self.check % 3 {
                0 if !entry.is_directory() => {
                    entry.attributes |= attr::DIRECTORY;
                    Some(said::BAD_ATTRIBUTE)
                }
                1 if first_of(&entry) != link => {
                    set_first(&mut entry, link);
                    Some(said::BAD_LINK)
                }
                2 if entry.size != 0 => {
                    entry.size = 0;
                    Some(said::BAD_SIZE)
                }
                _ => None,
            };
            if let Some(problem) = problem {
                set_found(p, &entry);
                let what = of_dot(p, problem, if dot { b"." } else { b".." });
                return self.tell(p, phase, what, walking::CHECK_SAID);
            }
            self.check += 1;
            if self.check == 3 {
                *phase = walking::AFTER_DOT;
                return Flow::Call(search(p, true));
            }
        }
        Self::next(p, phase)
    }

    /// The entry the search found next to `.` taken as `..`.
    fn dotdot(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if self.found && found(p).name == DOTDOT {
            self.check = 3;
            return self.dot_checks(p, phase);
        }
        let what = of_dot(p, said::MISSING, b"..");
        self.tell(p, phase, what, walking::DOTDOT_SAID)
    }

    /// A directory's first entry, or the root's.
    fn first(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if Self::at_root(p) {
            return self.entries(p, phase);
        }
        if self.found && found(p).name == DOT {
            self.check = 0;
            return self.dot_checks(p, phase);
        }
        self.context = if self.found {
            context::NO_DOT
        } else {
            context::EMPTY
        };
        *phase = walking::SPLICE_AT;
        Flow::Call(super::current_directory(p))
    }

    /// The entry the search found: a file's chain followed, or a
    /// directory's and the directory walked.
    fn entry(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let entry = found(p);
        self.write = false;
        self.emptied = false;
        if entry.is_directory() {
            if Self::second(p) {
                return self.cross_look(p, phase);
            }
            return self.chain(p, phase, mark::DIRECTORY);
        }
        if Self::second(p) {
            return self.cross_look(p, phase);
        }
        if switched(p, switch::VERBOSE) {
            let name = spelt(&entry.name);
            set_asciz(p, at::NAME, &name, 13);
            *phase = walking::FILE_SAID;
            return Flow::Call(Frame::of(&Tell::of(said::FILE)));
        }
        self.file(p, phase)
    }

    fn file(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if found(p).attributes & attr::VOLUME_ID != 0 {
            self.clusters = 0;
            return self.counted(p, phase);
        }
        self.chain(p, phase, mark::FILE)
    }

    /// The entry's chain followed and marked, and what is wrong with it
    /// said and put right.
    fn chain(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, kind: u8) -> Flow {
        let mut entry = found(p);
        let first = first_of(&entry);
        let volume = volume(p);
        let chain = cells::mark_chain(&mut held(p), volume, first, kind);
        self.clusters = chain.clusters;
        (self.ending, self.last) = match chain.ending {
            Ending::End => (ended::END, 0),
            Ending::Crossed => (ended::CROSSED, 0),
            Ending::BadFirst => (ended::BAD_FIRST, 0),
            Ending::BadLink { last } => (ended::BAD_LINK, last),
        };
        let name = spelt(&entry.name);
        set_asciz(p, at::NAME, &name, 13);
        let first_bad = self.ending == ended::BAD_FIRST
            || (self.ending == ended::BAD_LINK && self.last == first);
        let said = match self.ending {
            ended::CROSSED => {
                add(p, count::CROSSINGS, 1);
                None
            }
            _ if first_bad => {
                if first == 0 && entry.size == 0 {
                    None
                } else {
                    self.write = true;
                    self.emptied = first != 0;
                    set_first(&mut entry, 0);
                    entry.size = 0;
                    set_found(p, &entry);
                    Some(if first != 0 {
                        said::FIRST_INVALID
                    } else {
                        said::ERRORS
                    })
                }
            }
            ended::BAD_LINK => {
                let end = volume.end();
                cells::Cells::set_entry(&mut held(p), self.last, end);
                set(p, flag::DIRTY);
                Some(said::BAD_CHAIN)
            }
            _ if kind == mark::FILE => {
                let bytes = self.clusters.saturating_mul(cluster_bytes(p));
                let wrong = bytes < entry.size || bytes - entry.size >= cluster_bytes(p);
                if wrong {
                    entry.size = bytes;
                    set_found(p, &entry);
                    self.write = true;
                }
                wrong.then_some(said::SIZE_ADJUSTED)
            }
            _ => None,
        };
        self.tell(p, phase, said, walking::CHAIN_SAID)
    }

    /// The entry looked at is done: counted, and a directory walked.
    fn counted(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let entry = found(p);
        if !entry.is_directory() {
            let (number, clusters) = if entry.attributes & attr::HIDDEN != 0
                || entry.attributes & attr::VOLUME_ID != 0
            {
                (count::HIDDEN, count::HIDDEN_CLUSTERS)
            } else {
                (count::FILES, count::FILE_CLUSTERS)
            };
            add(p, number, 1);
            add(p, clusters, self.clusters);
            return Self::next(p, phase);
        }
        add(p, count::DIRECTORIES, 1);
        if self.emptied {
            return Self::convert(p, phase);
        }
        add(p, count::DIRECTORY_CLUSTERS, self.clusters);
        Self::descend(p, phase)
    }

    /// The second walk: the first crossed cluster the entry's chain holds,
    /// said.
    fn cross_look(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let entry = found(p);
        let volume = volume(p);
        let crossed = cells::first_crossed(&mut held(p), volume, first_of(&entry));
        *phase = walking::CROSS_SAID;
        match crossed {
            Some(cluster) => {
                let name = spelt(&entry.name);
                set_asciz(p, at::NAME, &name, 13);
                Flow::Call(Frame::of(&Tell::with(said::CROSSED, cluster)))
            }
            None => self.step(p, phase, Ok(0)),
        }
    }

    /// Into the subdirectory the entry names, by its name.
    fn descend(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let entry = found(p);
        let name = spelt(&entry.name);
        set_asciz(p, at::NAME, &name, 13);
        let first = first_of(&entry);
        if first == 0 {
            *phase = walking::BOGUS_SAID;
            return Flow::Call(Frame::of(&Tell::of(said::BAD_SUBDIRECTORY)));
        }
        if !volume(p).holds(first) {
            return Self::next(p, phase);
        }
        if depth(p) + 1 >= LEVELS {
            return Self::unreachable(p, phase);
        }
        *phase = walking::ENTERED;
        Flow::Call(Frame::of(&Int {
            dx: at::NAME,
            ..Int::dos(p.own, 0x3B00)
        }))
    }

    /// A subdirectory the walk cannot go into: it is said, and the tree
    /// below it goes unwalked.
    fn unreachable(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        set(p, flag::TRUNCATED);
        *phase = walking::NO_CHDIR_SAID;
        Flow::Call(Frame::of(&Tell::of(said::NO_CHDIR)))
    }

    /// The directory entry the walk is at made a file, once the user agrees,
    /// its size its chain's.
    fn ask_convert(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if Self::second(p) {
            return Self::next(p, phase);
        }
        if switched(p, switch::VERBOSE) {
            *phase = walking::UNRECOVERABLE_SAID;
            return Flow::Call(say(p, text::UNRECOVERABLE));
        }
        *phase = walking::CONVERT_ASKED;
        Flow::Call(Frame::of(&Ask::of(super::question::CONVERT)))
    }

    /// The directory entry in the DTA turned into a file's.
    fn convert(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let mut entry = found(p);
        entry.attributes &= !attr::DIRECTORY;
        set_found(p, &entry);
        *phase = walking::CONVERTED;
        Flow::Call(Frame::of(&Self::fix(p, fix::WRITE)))
    }

    /// The fix of the entry the walk is at.
    fn fix(p: &mut Program<'_, '_>, mode: u8) -> FixEntry {
        let index = level16(p, level::FCB + SEARCH_INDEX);
        FixEntry::of(level16(p, level::SELF), index, mode, 0)
    }
}

impl Routine for ChkdskTree {
    const ID: Id = Id::ChkdskTree;

    #[allow(
        clippy::too_many_lines,
        reason = "one arm a phase, in the order DOS's CHKDSK walks a directory"
    )]
    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                p.set8(at::DEPTH, 0);
                let block = p.get16(at::BLOCK);
                p.g.write16(block, level_at(0) + level::SELF, 0);
                p.g.write16(block, level_at(0) + level::PARENT, 0);
                Self::enter(p, phase)
            }
            walking::ANNOUNCED => {
                take!(child);
                *phase = walking::FIRST;
                Flow::Call(search(p, false))
            }
            walking::FIRST => {
                self.found = take!(child).to_le_bytes()[0] == 0;
                self.first(p, phase)
            }
            walking::SPLICE_AT => {
                *phase = walking::SPLICED;
                Flow::Call(Frame::of(&Int {
                    si: at::PATH,
                    di: at::BUFFER,
                    ..Int::dos(p.own, 0x6000)
                }))
            }
            walking::SPLICED => {
                if child.is_ok() && Self::spliced(p) {
                    set(p, flag::TRUNCATED);
                    return self.tell(p, phase, Some(said::JOINED), walking::LEAVE);
                }
                if self.context == context::EMPTY {
                    let what = if switched(p, switch::VERBOSE) {
                        set_asciz(p, at::NAME, b"", 13);
                        Some(said::EMPTY)
                    } else {
                        invalid_once(p)
                    };
                    return self.tell(p, phase, what, walking::ABANDON);
                }
                let what = of_dot(p, said::MISSING, b".");
                self.tell(p, phase, what, walking::DOT_SAID)
            }
            walking::LEAVE => Self::leave(p, phase, false),
            walking::ABANDON => Self::leave(p, phase, true),
            walking::DOT_SAID => {
                let own = level16(p, level::SELF);
                *phase = walking::DOT_FILLED;
                Flow::Call(Frame::of(&FixEntry::of(own, 0, fix::DOT, own)))
            }
            walking::DOT_FILLED => {
                if child == Ok(1) {
                    return self.dotdot(p, phase);
                }
                set_level8(p, level::DOTS_BAD, 1);
                let what = switched(p, switch::VERBOSE).then_some(said::DOT_LOST);
                self.tell(p, phase, what, walking::DOT_LOST_SAID)
            }
            walking::DOT_LOST_SAID => self.dotdot(p, phase),
            walking::AFTER_DOT => {
                self.found = take!(child).to_le_bytes()[0] == 0;
                self.dotdot(p, phase)
            }
            walking::DOTDOT_SAID => {
                let (own, parent) = (level16(p, level::SELF), level16(p, level::PARENT));
                *phase = walking::DOTDOT_FILLED;
                Flow::Call(Frame::of(&FixEntry::of(own, 1, fix::DOTDOT, parent)))
            }
            walking::DOTDOT_FILLED => {
                if child == Ok(1) {
                    return self.entries(p, phase);
                }
                set(p, flag::TRUNCATED);
                let what = switched(p, switch::VERBOSE).then(|| {
                    set_asciz(p, at::NAME, b"..", 13);
                    said::DOTDOT_LOST
                });
                self.tell(p, phase, what, walking::ABANDON)
            }
            walking::CHECK_SAID => {
                *phase = walking::CHECK_FIXED;
                Flow::Call(Frame::of(&Self::fix(p, fix::WRITE)))
            }
            walking::CHECK_FIXED => {
                self.check += 1;
                if self.check == 3 {
                    *phase = walking::AFTER_DOT;
                    return Flow::Call(search(p, true));
                }
                self.dot_checks(p, phase)
            }
            walking::NEXT_FOUND => {
                self.found = take!(child).to_le_bytes()[0] == 0;
                self.entries(p, phase)
            }
            walking::ROSE => {
                if child.is_ok() {
                    return Self::popped(p, phase);
                }
                let what = switched(p, switch::VERBOSE).then(|| {
                    set_asciz(p, at::NAME, b"", 13);
                    said::UP_FAILED
                });
                self.tell(p, phase, what, walking::ROSE_SAID)
            }
            walking::ROSE_SAID => {
                *phase = walking::ROSE_AGAIN;
                Flow::Call(Frame::of(&Int {
                    dx: at::UP,
                    ..Int::dos(p.own, 0x3B00)
                }))
            }
            walking::ROSE_AGAIN => Self::popped(p, phase),
            walking::FILE_SAID => {
                take!(child);
                self.file(p, phase)
            }
            walking::CHAIN_SAID => {
                take!(child);
                if self.write {
                    *phase = walking::ENTRY_FIXED;
                    return Flow::Call(Frame::of(&Self::fix(p, fix::WRITE)));
                }
                self.counted(p, phase)
            }
            walking::ENTRY_FIXED => {
                take!(child);
                self.counted(p, phase)
            }
            walking::CROSS_SAID => {
                take!(child);
                if found(p).is_directory() {
                    return Self::descend(p, phase);
                }
                Self::next(p, phase)
            }
            walking::ENTERED => {
                if child.is_err() {
                    return Self::unreachable(p, phase);
                }
                let parent = level16(p, level::SELF);
                let first = first_of(&found(p));
                let down = depth(p) + 1;
                p.set8(at::DEPTH, down);
                let block = p.get16(at::BLOCK);
                p.g.write16(block, level_at(down) + level::SELF, first);
                p.g.write16(block, level_at(down) + level::PARENT, parent);
                Self::enter(p, phase)
            }
            walking::BOGUS_SAID => {
                take!(child);
                Self::ask_convert(p, phase)
            }
            walking::NO_CHDIR_SAID | walking::CONVERTED => {
                take!(child);
                Self::next(p, phase)
            }
            walking::UNRECOVERABLE_SAID => {
                take!(child);
                *phase = walking::CONVERT_ASKED;
                Flow::Call(Frame::of(&Ask::of(super::question::CONVERT)))
            }
            walking::CONVERT_ASKED => {
                if take!(child) == 0 {
                    return Self::next(p, phase);
                }
                *phase = walking::CONVERT_READ;
                Flow::Call(Frame::of(&Self::fix(p, fix::READ)))
            }
            walking::CONVERT_READ => {
                if take!(child) == 0 {
                    return Self::next(p, phase);
                }
                let mut entry = found(p);
                let volume = volume(p);
                let clusters = cells::chain_length(&mut held(p), volume, first_of(&entry));
                entry.size = clusters.saturating_mul(cluster_bytes(p));
                set_found(p, &entry);
                Self::convert(p, phase)
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}
