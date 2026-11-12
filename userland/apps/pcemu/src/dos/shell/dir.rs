//! `DIR`: the files a pattern matches, as DOS 5.00 lists them — wide, bare,
//! lower case, sorted, filtered by attribute, paused a screen at a time,
//! and through every subdirectory — with `DIRCMD`'s switches taken first.
//! A sorted listing gathers its entries in a block of memory first; a
//! listing of subdirectories keeps a search a level in another.

use super::dates::{two_digits, Writing};
use super::internal::VolumeHeader;
use super::parse;
use super::run::Release;
use super::{
    answered, asciz_at, at, say, set_asciz, text, text_at, variable, Flow, Frame, Id, Int, Program,
    Returned, Routine,
};
use crate::dos::dir::{HIDDEN_FILES, HIDDEN_KINDS};
use crate::dos::dirops::finding;
use crate::dos::message::{self, decimal_in};
use crate::dos::program::walk::{self, Walk};
use crate::frames::{locals, take, Fault, Outcome};
use alloc::vec::Vec;
use tairix_abi::time::DosTime;
use tairix_fat::dir::attr;

/// `DIR`'s state, from [`at::DIR`].
mod field {
    pub const SWITCHES: u16 = 0x00;
    /// The attributes an entry must have, and must not.
    pub const ON: u16 = 0x02;
    pub const OFF: u16 = 0x03;
    /// The sort's keys, most significant first: a key's letter, its top
    /// bit set for a reversed key, zero past the last.
    pub const ORDER: u16 = 0x04;
    /// Entries and bytes in the directory being listed, and in all.
    pub const COUNT: u16 = 0x08;
    pub const BYTES: u16 = 0x0A;
    pub const TOTAL_COUNT: u16 = 0x0E;
    pub const TOTAL_BYTES: u16 = 0x10;
    /// Lines since the last pause, and the wide listing's column.
    pub const LINES: u16 = 0x14;
    pub const COLUMN: u16 = 0x15;
    /// The drive.
    pub const DRIVE: u16 = 0x17;
    /// Entries gathered for the sort.
    pub const GATHERED: u16 = 0x1C;
    /// The sort block's room, in entries.
    pub const ROOM: u16 = 0x20;
    /// Whether this directory's header is out, and its search begun.
    pub const HEADED: u16 = 0x22;
    pub const SEARCHED: u16 = 0x23;
    /// The walk down the subdirectories.
    pub const WALK: u16 = 0x24;
    const _: () = assert!(WALK + crate::dos::program::walk::walker::LEN <= PATTERN);
    /// The pattern files are matched against, ASCIZ.
    pub const PATTERN: u16 = 0x30;
    /// The directory being listed, `\`-ended, ASCIZ.
    pub const PATH: u16 = 0x40;
    pub const PATH_LEN: u16 = 0x60;
    /// The directory as its header names it.
    pub const SHOWN_PATH: u16 = 0xA0;
}

/// The switches.
mod switch {
    pub const PAUSE: u16 = 0x01;
    pub const WIDE: u16 = 0x02;
    pub const SUBDIRECTORIES: u16 = 0x04;
    pub const BARE: u16 = 0x08;
    pub const LOWER: u16 = 0x10;
    pub const SORTED: u16 = 0x20;
}

/// An entry gathered for the sort: its name, attribute, time, date, size.
mod entry {
    pub const NAME: u16 = 0;
    pub const ATTRIBUTE: u16 = 13;
    pub const TIME: u16 = 14;
    pub const DATE: u16 = 16;
    pub const SIZE: u16 = 18;
    pub const LEN: u16 = 22;
}

/// Attributes.
/// Lines a page shows before `/P` pauses.
const PAGE: u8 = 23;

fn get8(p: &mut Program<'_, '_>, field: u16) -> u8 {
    p.get8(at::DIR + field)
}

fn set8(p: &mut Program<'_, '_>, field: u16, value: u8) {
    p.set8(at::DIR + field, value);
}

fn get16(p: &mut Program<'_, '_>, field: u16) -> u16 {
    p.get16(at::DIR + field)
}

fn set16(p: &mut Program<'_, '_>, field: u16, value: u16) {
    p.set16(at::DIR + field, value);
}

fn get32(p: &mut Program<'_, '_>, field: u16) -> u32 {
    u32::from(get16(p, field)) | u32::from(get16(p, field + 2)) << 16
}

fn set32(p: &mut Program<'_, '_>, field: u16, value: u32) {
    let (low, high) = crate::frames::words(value);
    set16(p, field, low);
    set16(p, field + 2, high);
}

fn switches(p: &mut Program<'_, '_>) -> u16 {
    get16(p, field::SWITCHES)
}

/// One entry, as a search found it or the sort kept it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    name: Vec<u8>,
    attribute: u8,
    time: u16,
    date: u16,
    size: u32,
}

impl Entry {
    fn found(p: &mut Program<'_, '_>) -> Self {
        Self {
            name: asciz_at(p, at::DTA + finding::NAME, 13),
            attribute: p.get8(at::DTA + finding::ATTRIBUTE),
            time: p.get16(at::DTA + finding::TIME),
            date: p.get16(at::DTA + finding::DATE),
            size: u32::from(p.get16(at::DTA + finding::SIZE))
                | u32::from(p.get16(at::DTA + finding::SIZE + 2)) << 16,
        }
    }

    fn encode(&self) -> [u8; entry::LEN as usize] {
        let mut out = [0u8; entry::LEN as usize];
        let at = |field: u16| usize::from(field);
        let len = self.name.len().min(12);
        out[at(entry::NAME)..at(entry::NAME) + len].copy_from_slice(&self.name[..len]);
        out[at(entry::ATTRIBUTE)] = self.attribute;
        out[at(entry::TIME)..at(entry::TIME) + 2].copy_from_slice(&self.time.to_le_bytes());
        out[at(entry::DATE)..at(entry::DATE) + 2].copy_from_slice(&self.date.to_le_bytes());
        out[at(entry::SIZE)..at(entry::SIZE) + 4].copy_from_slice(&self.size.to_le_bytes());
        out
    }

    /// Puts the entry where a search leaves one, in the DTA.
    fn place(&self, p: &mut Program<'_, '_>) {
        p.set8(at::DTA + finding::ATTRIBUTE, self.attribute);
        p.set16(at::DTA + finding::TIME, self.time);
        p.set16(at::DTA + finding::DATE, self.date);
        let (low, high) = crate::frames::words(self.size);
        p.set16(at::DTA + finding::SIZE, low);
        p.set16(at::DTA + finding::SIZE + 2, high);
        set_asciz(p, at::DTA + finding::NAME, &self.name, 13);
    }

    fn decode(raw: &[u8]) -> Self {
        let at = |field: u16| usize::from(field);
        let name = &raw[at(entry::NAME)..at(entry::ATTRIBUTE)];
        let name_len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
        let word = |field: u16| u16::from_le_bytes([raw[at(field)], raw[at(field) + 1]]);
        let size = &raw[at(entry::SIZE)..at(entry::SIZE) + 4];
        Self {
            name: name[..name_len].to_vec(),
            attribute: raw[at(entry::ATTRIBUTE)],
            time: word(entry::TIME),
            date: word(entry::DATE),
            size: u32::from_le_bytes([size[0], size[1], size[2], size[3]]),
        }
    }

    fn directory(&self) -> bool {
        self.attribute & attr::DIRECTORY != 0
    }

    /// Its base and extension.
    fn parts(&self) -> (&[u8], &[u8]) {
        match self.name.iter().position(|&b| b == b'.') {
            Some(dot) if dot > 0 => (&self.name[..dot], &self.name[dot + 1..]),
            _ => (&self.name, &[]),
        }
    }
}

/// `entries` sorted by `order`'s keys, most significant first.
fn sort(entries: &mut [Entry], order: &[u8]) {
    entries.sort_by(|a, b| {
        for &key in order {
            let reverse = key & 0x80 != 0;
            let ordering = match key & 0x7F {
                b'N' => a.parts().0.cmp(b.parts().0),
                b'E' => a.parts().1.cmp(b.parts().1),
                b'D' => (a.date, a.time).cmp(&(b.date, b.time)),
                b'S' => a.size.cmp(&b.size),
                b'G' => b.directory().cmp(&a.directory()),
                _ => core::cmp::Ordering::Equal,
            };
            let ordering = if reverse {
                ordering.reverse()
            } else {
                ordering
            };
            if ordering.is_ne() {
                return ordering;
            }
        }
        core::cmp::Ordering::Equal
    });
}

/// The switches a text of them sets on `on` and clears on `off`, the
/// attributes and the order they ask for; `false` for one DOS does not
/// know.
fn read_switches(text: &[u8], state: &mut (u16, u8, u8, [u8; 4])) -> bool {
    for argument in parse::arguments(text) {
        let Some(switch) = argument.strip_prefix(b"/") else {
            continue;
        };
        let upper = switch.to_ascii_uppercase();
        let (negated, rest) = match upper.strip_prefix(b"-") {
            Some(rest) => (true, rest),
            None => (false, &upper[..]),
        };
        let Some((&letter, value)) = rest.split_first() else {
            return false;
        };
        let value = value.strip_prefix(b":").unwrap_or(value);
        let bit = match letter {
            b'P' => switch::PAUSE,
            b'W' => switch::WIDE,
            b'S' => switch::SUBDIRECTORIES,
            b'B' => switch::BARE,
            b'L' => switch::LOWER,
            b'O' => {
                state.3 = [0; 4];
                if !negated {
                    let keys = if value.is_empty() { &b"GN"[..] } else { value };
                    let mut at = 0;
                    let mut reverse = false;
                    for &key in keys {
                        match key {
                            b'-' => reverse = true,
                            b'N' | b'E' | b'D' | b'S' | b'G' if at < 4 => {
                                state.3[at] = key | if reverse { 0x80 } else { 0 };
                                at += 1;
                                reverse = false;
                            }
                            _ => return false,
                        }
                    }
                }
                switch::SORTED
            }
            b'A' => {
                (state.1, state.2) = (0, if negated { HIDDEN_FILES } else { 0 });
                let mut not = false;
                for &key in value {
                    let mask = match key {
                        b'-' => {
                            not = true;
                            continue;
                        }
                        b'D' => attr::DIRECTORY,
                        b'H' => 0x02,
                        b'S' => 0x04,
                        b'A' => 0x20,
                        b'R' => 0x01,
                        _ => return false,
                    };
                    if not {
                        state.2 |= mask;
                    } else {
                        state.1 |= mask;
                    }
                    not = false;
                }
                continue;
            }
            _ => return false,
        };
        state.0 = if negated {
            state.0 & !bit
        } else {
            state.0 | bit
        };
    }
    true
}

/// The pattern `DIR`'s argument names, split: its directory, `\`-ended or
/// empty, and the files' pattern. A name with no extension matches any.
fn pattern_of(argument: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let split = argument
        .iter()
        .rposition(|&b| b == b'\\' || b == b':')
        .map_or(0, |at| at + 1);
    let (directory, name) = argument.split_at(split);
    let name = match name {
        b"" | b"." => b"*.*".to_vec(),
        name if !name.contains(&b'.') => [name, b".*"].concat(),
        name => name.to_vec(),
    };
    (directory.to_vec(), name)
}

locals! {
    /// `DIR [pattern] [switches]`.
    pub struct Dir {}
}

mod phase {
    pub const ATTRIBUTES: u8 = 1;
    pub const DRIVE: u8 = 2;
    pub const VALIDATED: u8 = 3;
    pub const HEADED: u8 = 4;
    pub const STACKED: u8 = 5;
    pub const SORT_LARGEST: u8 = 6;
    pub const SORT_ALLOCATED: u8 = 7;
    pub const LISTED: u8 = 8;
    pub const WALK: u8 = 9;
    pub const WALKED: u8 = 10;
    pub const TOTALS: u8 = 11;
    pub const FREE: u8 = 12;
    pub const FREED: u8 = 13;
    pub const RELEASE: u8 = 14;
    pub const DONE: u8 = 15;
}

impl Routine for Dir {
    const ID: Id = Id::Dir;

    #[allow(
        clippy::too_many_lines,
        reason = "one arm a phase, in the order the routine runs"
    )]
    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if child == Err(Fault::Break) {
            return Flow::Return(child);
        }
        if answered(child) && *phase < phase::RELEASE {
            *phase = phase::RELEASE;
            return self.step(p, phase, Ok(0));
        }
        match *phase {
            0 => self.start(p, phase),
            phase::ATTRIBUTES => {
                let mut argument = asciz_at(p, at::PATH, 128);
                if child.is_ok() && p.result(Returned::Cx) & u16::from(attr::DIRECTORY) != 0 {
                    argument.push(b'\\');
                }
                let (directory, pattern) = pattern_of(&argument);
                set_asciz(p, at::DIR + field::PATTERN, &pattern, 16);
                set_asciz(p, at::DIR + field::PATH, &directory, field::PATH_LEN);
                *phase = phase::DRIVE;
                match directory.as_slice() {
                    [letter, b':', ..] => {
                        let drive = letter.to_ascii_uppercase().wrapping_sub(b'A');
                        self.step(p, phase, Ok(u32::from(drive)))
                    }
                    _ => Flow::Call(Frame::of(&Int::dos(p.own, 0x1900))),
                }
            }
            // The drive's directory is asked first, as DOS's DIR asks it: a
            // directory a change of disk took away gives way to the root.
            phase::DRIVE => {
                let drive = take!(child).to_le_bytes()[0];
                set8(p, field::DRIVE, drive);
                *phase = phase::VALIDATED;
                Flow::Call(Frame::of(&Int {
                    dx: u16::from(drive) + 1,
                    si: at::PATH2,
                    ..Int::dos(p.own, 0x4700)
                }))
            }
            phase::VALIDATED => {
                if child == Err(Fault::Error(crate::dos::fs::error::INVALID_DRIVE)) {
                    *phase = phase::DONE;
                    return Flow::Call(say(p, text::BAD_DRIVE));
                }
                let drive = get8(p, field::DRIVE);
                *phase = phase::HEADED;
                if switches(p) & switch::BARE != 0 {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(Frame::of(&VolumeHeader::shell(drive)))
            }
            phase::HEADED => {
                if child.is_err() {
                    return Flow::Return(Ok(0));
                }
                *phase = phase::STACKED;
                if switches(p) & switch::SUBDIRECTORIES == 0 {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(Frame::of(&Int {
                    bx: walk::PARAGRAPHS,
                    ..Int::dos(p.own, 0x4800)
                }))
            }
            phase::STACKED => {
                if switches(p) & switch::SUBDIRECTORIES != 0 {
                    let Ok(segment) = child else {
                        *phase = phase::DONE;
                        return Flow::Call(say(p, message::MEMORY));
                    };
                    let segment = u16::try_from(segment).unwrap_or(0);
                    p.set16(STACK, segment);
                    let skip = get8(p, field::OFF) & HIDDEN_FILES;
                    walk::begin_walk(
                        p,
                        at::DIR + field::WALK,
                        segment,
                        at::DIR + field::PATH,
                        field::PATH_LEN,
                        skip,
                    );
                }
                if switches(p) & switch::SORTED == 0 {
                    *phase = phase::LISTED;
                    return Flow::Call(Frame::of(&DirList::default()));
                }
                *phase = phase::SORT_LARGEST;
                Flow::Call(Frame::of(&Int {
                    bx: 0xFFFF,
                    ..Int::dos(p.own, 0x4800)
                }))
            }
            phase::SORT_LARGEST => {
                let largest = p.result(Returned::Bx).min(SORT_PARAGRAPHS);
                *phase = phase::SORT_ALLOCATED;
                Flow::Call(Frame::of(&Int {
                    bx: largest,
                    ..Int::dos(p.own, 0x4800)
                }))
            }
            phase::SORT_ALLOCATED => {
                if let Ok(segment) = child {
                    p.set16(SORT, u16::try_from(segment).unwrap_or(0));
                    let paragraphs = u32::from(p.result(Returned::Bx).min(SORT_PARAGRAPHS));
                    let room = paragraphs * 16 / u32::from(entry::LEN);
                    set16(p, field::ROOM, u16::try_from(room).unwrap_or(0));
                } else {
                    let unsorted = switches(p) & !switch::SORTED;
                    set16(p, field::SWITCHES, unsorted);
                }
                *phase = phase::LISTED;
                Flow::Call(Frame::of(&DirList::default()))
            }
            phase::LISTED => {
                *phase = if switches(p) & switch::SUBDIRECTORIES == 0 {
                    phase::FREE
                } else {
                    phase::WALK
                };
                self.step(p, phase, Ok(0))
            }
            phase::WALK => {
                *phase = phase::WALKED;
                Flow::Call(Frame::of(&Walk::of(at::DIR + field::WALK)))
            }
            phase::WALKED => match take!(child) {
                walk::step::ENTERED => {
                    *phase = phase::LISTED;
                    Flow::Call(Frame::of(&DirList::default()))
                }
                walk::step::LEAVING => {
                    *phase = phase::WALK;
                    self.step(p, phase, Ok(0))
                }
                _ => {
                    *phase = phase::TOTALS;
                    self.step(p, phase, Ok(0))
                }
            },
            phase::TOTALS => {
                *phase = phase::FREE;
                if switches(p) & switch::BARE != 0 {
                    return self.step(p, phase, Ok(0));
                }
                let (count, bytes) = (get16(p, field::TOTAL_COUNT), get32(p, field::TOTAL_BYTES));
                let line = [
                    &b"\r\n     Total files listed:\r\n"[..],
                    &summary(u32::from(count), bytes),
                ]
                .concat();
                Flow::Call(say(p, &line))
            }
            phase::FREE => {
                *phase = phase::FREED;
                if switches(p) & switch::BARE != 0 {
                    return self.step(p, phase, Err(Fault::Error(0)));
                }
                let drive = get8(p, field::DRIVE);
                Flow::Call(Frame::of(&Int {
                    dx: u16::from(drive) + 1,
                    ..Int::dos(p.own, 0x3600)
                }))
            }
            phase::FREED => {
                *phase = phase::RELEASE;
                match child {
                    Ok(per_cluster) if per_cluster != 0xFFFF => {
                        let free = per_cluster
                            * u32::from(p.result(Returned::Bx))
                            * u32::from(p.result(Returned::Cx));
                        let line = [decimal_in(free, 29), b" bytes free\r\n".to_vec()].concat();
                        Flow::Call(say(p, &line))
                    }
                    _ => self.step(p, phase, Ok(0)),
                }
            }
            phase::RELEASE => {
                *phase = phase::DONE;
                Flow::Call(Frame::of(&Release {}))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

/// The blocks the walk's levels and the sort's entries are kept in,
/// which the shell frees however `DIR` ends.
const STACK: u16 = at::HELD;
const SORT: u16 = at::HELD + 2;

/// The most a sort block takes: 64 KiB.
const SORT_PARAGRAPHS: u16 = 0x1000;

impl Dir {
    /// Reads the switches, `DIRCMD`'s first, and the pattern.
    fn start(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let mut state = (0u16, 0u8, HIDDEN_FILES, [0u8; 4]);
        let defaults = variable(p, b"DIRCMD").unwrap_or_default();
        let args = text_at(p, at::ARGS);
        if !read_switches(&defaults, &mut state) || !read_switches(&args, &mut state) {
            *phase = phase::DONE;
            return Flow::Call(say(p, text::BAD_PARAMETER));
        }
        let (switches, on, off, order) = state;
        p.g.write_bytes(p.own, at::DIR, &[0u8; at::DIR_LEN as usize]);
        set16(p, field::SWITCHES, switches);
        set8(p, field::ON, on);
        set8(p, field::OFF, off);
        p.g.write_bytes(p.own, at::DIR + field::ORDER, &order);
        let argument = parse::arguments(&args)
            .into_iter()
            .find(|a| !a.starts_with(b"/"))
            .unwrap_or_default()
            .to_vec();
        set_asciz(p, at::PATH, &argument, 128);
        *phase = phase::ATTRIBUTES;
        if argument.is_empty() || crate::dos::name::wild(&argument) {
            return self.step(p, phase, Err(Fault::Error(0)));
        }
        Flow::Call(Frame::of(&Int {
            dx: at::PATH,
            ..Int::dos(p.own, 0x4300)
        }))
    }
}

/// A summary's line: entries and their bytes.
fn summary(count: u32, bytes: u32) -> Vec<u8> {
    [
        decimal_in(count, 9),
        b" file(s)".to_vec(),
        decimal_in(bytes, 12),
        b" bytes\r\n".to_vec(),
    ]
    .concat()
}

locals! {
    /// Lists the directory `DIR`'s state names: a header once it has an
    /// entry, its entries, a summary; gathered and sorted first when the
    /// listing is sorted.
    pub struct DirList {
        shown: u16,
    }
}

mod listing {
    pub const NAMED: u8 = 1;
    pub const SEARCH: u8 = 2;
    pub const FOUND: u8 = 3;
    pub const LINE: u8 = 4;
    pub const LINE_OUT: u8 = 5;
    pub const PAUSE_SAID: u8 = 6;
    pub const PAUSE_KEYED: u8 = 7;
    pub const CONTINUING: u8 = 8;
    pub const SUMMED: u8 = 9;
}

impl Routine for DirList {
    const ID: Id = Id::DirList;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if child == Err(Fault::Break) || answered(child) {
            return Flow::Return(child);
        }
        match *phase {
            0 => {
                set8(p, field::HEADED, 0);
                set8(p, field::SEARCHED, 0);
                set16(p, field::COUNT, 0);
                set32(p, field::BYTES, 0);
                set16(p, field::GATHERED, 0);
                set8(p, field::COLUMN, 0);
                let path = [
                    asciz_at(p, at::DIR + field::PATH, field::PATH_LEN),
                    b".".to_vec(),
                ]
                .concat();
                set_asciz(p, at::PATH, &path, 128);
                *phase = listing::NAMED;
                Flow::Call(Frame::of(&Int {
                    si: at::PATH,
                    di: at::DIR + field::SHOWN_PATH,
                    ..Int::dos(p.own, 0x6000)
                }))
            }
            listing::NAMED => {
                if child.is_err() {
                    let path = asciz_at(p, at::DIR + field::PATH, field::PATH_LEN);
                    set_asciz(p, at::DIR + field::SHOWN_PATH, &path, 80);
                }
                *phase = listing::SEARCH;
                Flow::Call(Frame::of(&Int {
                    dx: at::DTA,
                    ..Int::dos(p.own, 0x1A00)
                }))
            }
            listing::SEARCH => {
                *phase = listing::FOUND;
                if get8(p, field::SEARCHED) != 0 {
                    return Flow::Call(Frame::of(&Int::dos(p.own, 0x4F00)));
                }
                set8(p, field::SEARCHED, 1);
                let pattern = [
                    asciz_at(p, at::DIR + field::PATH, field::PATH_LEN),
                    asciz_at(p, at::DIR + field::PATTERN, 16),
                ]
                .concat();
                set_asciz(p, at::PATH, &pattern, 128);
                Flow::Call(Frame::of(&Int {
                    cx: u16::from(HIDDEN_KINDS),
                    dx: at::PATH,
                    ..Int::dos(p.own, 0x4E00)
                }))
            }
            listing::FOUND if child.is_err() => self.searched(p, phase),
            listing::FOUND => self.found(p, phase),
            listing::LINE => Self::line(p, phase),
            listing::LINE_OUT => {
                if switches(p) & switch::PAUSE != 0 && get8(p, field::LINES) >= PAGE {
                    *phase = listing::PAUSE_SAID;
                    return Flow::Call(say(p, message::PRESS_ANY_KEY));
                }
                self.next(p, phase)
            }
            listing::PAUSE_SAID => {
                *phase = listing::PAUSE_KEYED;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x0C08)))
            }
            listing::PAUSE_KEYED => {
                set8(p, field::LINES, 0);
                let shown = asciz_at(p, at::DIR + field::SHOWN_PATH, 80);
                *phase = listing::CONTINUING;
                Flow::Call(say(
                    p,
                    &[&b"\r\n(continuing "[..], &shown, b")\r\n"].concat(),
                ))
            }
            listing::CONTINUING => self.next(p, phase),
            listing::SUMMED => {
                let total = get16(p, field::TOTAL_COUNT).saturating_add(get16(p, field::COUNT));
                set16(p, field::TOTAL_COUNT, total);
                let all = get32(p, field::TOTAL_BYTES).saturating_add(get32(p, field::BYTES));
                set32(p, field::TOTAL_BYTES, all);
                Flow::Return(Ok(0))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

impl DirList {
    /// The entry the search found, in the DTA: passed over unless its
    /// attributes match, gathered when sorting, else shown.
    fn found(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let found = Entry::found(p);
        let (on, off) = (get8(p, field::ON), get8(p, field::OFF));
        let dots = found.name == b"." || found.name == b"..";
        let wanted = found.attribute & on == on
            && found.attribute & off == 0
            && !(dots && switches(p) & switch::BARE != 0);
        if wanted && switches(p) & switch::SORTED != 0 {
            let gathered = get16(p, field::GATHERED);
            if gathered < get16(p, field::ROOM) {
                let block = p.get16(SORT);
                p.g.write_bytes(block, gathered * entry::LEN, &found.encode());
                set16(p, field::GATHERED, gathered + 1);
            }
        }
        if !wanted || switches(p) & switch::SORTED != 0 {
            *phase = listing::SEARCH;
            return self.step(p, phase, Ok(0));
        }
        Self::show(p, phase)
    }

    /// The search has ended: the gathered entries sorted and shown, or the
    /// summary.
    fn searched(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if switches(p) & switch::SORTED == 0 {
            return self.summarise(p, phase);
        }
        let (gathered, block) = (get16(p, field::GATHERED), p.get16(SORT));
        let mut raw = alloc::vec![0u8; usize::from(gathered) * usize::from(entry::LEN)];
        p.g.read_bytes(block, 0, &mut raw);
        let mut entries: Vec<Entry> = raw
            .chunks(usize::from(entry::LEN))
            .map(Entry::decode)
            .collect();
        let mut order = [0u8; 4];
        p.g.read_bytes(p.own, at::DIR + field::ORDER, &mut order);
        let keys: Vec<u8> = order.iter().copied().take_while(|&k| k != 0).collect();
        sort(&mut entries, &keys);
        let raw: Vec<u8> = entries.iter().flat_map(Entry::encode).collect();
        p.g.write_bytes(block, 0, &raw);
        self.shown = 0;
        self.next_sorted(p, phase)
    }

    /// The next sorted entry put where a search leaves one and shown, or
    /// the summary once all are.
    fn next_sorted(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if self.shown >= get16(p, field::GATHERED) {
            return self.summarise(p, phase);
        }
        let block = p.get16(SORT);
        let mut raw = [0u8; entry::LEN as usize];
        p.g.read_bytes(block, self.shown * entry::LEN, &mut raw);
        self.shown += 1;
        Entry::decode(&raw).place(p);
        Self::show(p, phase)
    }

    /// After a line: the next sorted entry, or the search's next.
    fn next(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if switches(p) & switch::SORTED != 0 {
            return self.next_sorted(p, phase);
        }
        *phase = listing::SEARCH;
        self.step(p, phase, Ok(0))
    }

    /// The entry in the DTA counted and shown, the directory's header first
    /// if it is the first.
    fn show(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let found = Entry::found(p);
        let count = get16(p, field::COUNT).saturating_add(1);
        set16(p, field::COUNT, count);
        if !found.directory() {
            let bytes = get32(p, field::BYTES).saturating_add(found.size);
            set32(p, field::BYTES, bytes);
        }
        *phase = listing::LINE;
        if get8(p, field::HEADED) != 0 || switches(p) & switch::BARE != 0 {
            return Self::line(p, phase);
        }
        set8(p, field::HEADED, 1);
        let shown = asciz_at(p, at::DIR + field::SHOWN_PATH, 80);
        Flow::Call(say(
            p,
            &[&b" Directory of "[..], &shown, b"\r\n\r\n"].concat(),
        ))
    }

    /// The entry in the DTA's line: full, wide, or bare.
    fn line(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let found = Entry::found(p);
        let switches = switches(p);
        let lower = switches & switch::LOWER != 0;
        let case = |text: &[u8]| {
            if lower {
                text.to_ascii_lowercase()
            } else {
                text.to_vec()
            }
        };
        let shown = if switches & switch::BARE != 0 {
            let name = if switches & switch::SUBDIRECTORIES != 0 {
                let path = asciz_at(p, at::DIR + field::SHOWN_PATH, 80);
                let separator: &[u8] = if path.ends_with(b"\\") { b"" } else { b"\\" };
                [&path[..], separator, &found.name].concat()
            } else {
                found.name.clone()
            };
            [case(&name), text::NEW_LINE.to_vec()].concat()
        } else if switches & switch::WIDE != 0 {
            let name = if found.directory() {
                [&b"["[..], &found.name, b"]"].concat()
            } else {
                found.name.clone()
            };
            let mut name = case(&name);
            let column = get8(p, field::COLUMN) + 1;
            if column == WIDE_COLUMNS {
                name.extend_from_slice(text::NEW_LINE);
                set8(p, field::COLUMN, 0);
            } else {
                name.resize(16, b' ');
                set8(p, field::COLUMN, column);
            }
            name
        } else {
            full_line(p, &found, &case)
        };
        if shown.ends_with(text::NEW_LINE) {
            let lines = get8(p, field::LINES).saturating_add(1);
            set8(p, field::LINES, lines);
        }
        *phase = listing::LINE_OUT;
        Flow::Call(say(p, &shown))
    }

    /// The directory's summary, or `File not found` when a listing of one
    /// directory found nothing.
    fn summarise(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let switches = switches(p);
        let count = get16(p, field::COUNT);
        *phase = listing::SUMMED;
        let mut text = Vec::new();
        if switches & switch::WIDE != 0 && get8(p, field::COLUMN) != 0 {
            text.extend_from_slice(text::NEW_LINE);
        }
        if switches & switch::BARE == 0 {
            if count == 0 && switches & switch::SUBDIRECTORIES == 0 {
                text.extend_from_slice(text::FILE_NOT_FOUND);
            } else if count > 0 {
                text.extend_from_slice(&summary(u32::from(count), get32(p, field::BYTES)));
            }
        }
        if text.is_empty() {
            return self.step(p, phase, Ok(0));
        }
        Flow::Call(say(p, &text))
    }
}

/// Names a wide listing shows a line.
const WIDE_COLUMNS: u8 = 5;

/// `found` as a full listing's line: name, extension, size or `<DIR>`, date
/// and time, as the country writes them.
fn full_line(p: &mut Program<'_, '_>, found: &Entry, case: &dyn Fn(&[u8]) -> Vec<u8>) -> Vec<u8> {
    let (base, extension) = found.parts();
    let mut line = case(base);
    if found.name == b"." || found.name == b".." {
        line.clone_from(&found.name);
    }
    line.resize(8, b' ');
    line.push(b' ');
    let mut ext = case(extension);
    ext.resize(3, b' ');
    line.extend_from_slice(&ext);
    if found.directory() {
        line.extend_from_slice(b" <DIR>    ");
    } else {
        line.extend_from_slice(&decimal_in(found.size, 10));
    }
    line.push(b' ');
    let writing = Writing::read(p, at::COUNTRY);
    let stamp = DosTime {
        date: found.date,
        time: found.time,
    }
    .civil();
    let field = |value: u32| u8::try_from(value).unwrap_or(0);
    let year = u16::try_from(stamp.year).unwrap_or(0);
    line.extend_from_slice(&writing.date(year, field(stamp.month), field(stamp.day), true));
    line.extend_from_slice(b"  ");
    let (shown, meridian) = writing.hour(field(stamp.hour));
    let mut hour = two_digits(u16::from(shown));
    if meridian.is_some() && shown < 10 {
        hour[0] = b' ';
    }
    line.extend_from_slice(&hour);
    line.push(writing.time_separator);
    line.extend_from_slice(&two_digits(u16::from(field(stamp.minute))));
    line.extend(meridian);
    line.extend_from_slice(text::NEW_LINE);
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn entry(name: &[u8], attribute: u8, date: u16, size: u32) -> Entry {
        Entry {
            name: name.to_vec(),
            attribute,
            time: 0,
            date,
            size,
        }
    }

    #[test]
    fn the_sort_takes_its_keys_in_order_and_reverses_them() {
        let mut entries = vec![
            entry(b"B.TXT", 0, 2, 30),
            entry(b"DIR", attr::DIRECTORY, 1, 0),
            entry(b"A.COM", 0, 3, 10),
        ];
        sort(&mut entries, b"GN");
        assert_eq!(
            entries
                .iter()
                .map(|e| e.name.as_slice())
                .collect::<Vec<_>>(),
            [&b"DIR"[..], b"A.COM", b"B.TXT"]
        );
        sort(&mut entries, &[b'S' | 0x80]);
        assert_eq!(entries[0].name, b"B.TXT");
        sort(&mut entries, b"E");
        assert_eq!(entries[0].name, b"DIR", "no extension sorts first");
        sort(&mut entries, b"D");
        assert_eq!(entries[0].name, b"DIR");
    }

    #[test]
    fn switches_are_read_dircmd_first_and_turned_off_by_a_minus() {
        let mut state = (0u16, 0u8, HIDDEN_FILES, [0u8; 4]);
        assert!(read_switches(b"/W /O:-S", &mut state));
        assert!(read_switches(b"/-W /A:H-D", &mut state));
        assert_eq!(state.0, switch::SORTED);
        assert_eq!((state.1, state.2), (0x02, attr::DIRECTORY));
        assert_eq!(state.3, [b'S' | 0x80, 0, 0, 0]);
        assert!(!read_switches(b"/Z", &mut state));
        assert!(!read_switches(b"/O:X", &mut state));
    }

    #[test]
    fn a_pattern_is_split_and_a_bare_name_matches_any_extension() {
        assert_eq!(pattern_of(b""), (vec![], b"*.*".to_vec()));
        assert_eq!(
            pattern_of(b"C:\\DOS\\"),
            (b"C:\\DOS\\".to_vec(), b"*.*".to_vec())
        );
        assert_eq!(pattern_of(b"A:FOO"), (b"A:".to_vec(), b"FOO.*".to_vec()));
        assert_eq!(pattern_of(b"\\*.TXT"), (b"\\".to_vec(), b"*.TXT".to_vec()));
    }
}
