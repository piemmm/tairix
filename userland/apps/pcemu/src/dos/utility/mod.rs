//! The DOS utilities: programs on the program host, each its own file on a
//! system disk, reaching DOS only through its interrupts as any program
//! does. What they share is here: their numbers and files, the messages
//! DOS's common parser gives a command line they cannot take, and an
//! input's lines read through a buffer.

pub(crate) mod attrib;
pub(crate) mod chkdsk;
pub(crate) mod deltree;
pub(crate) mod diskcopy;
pub(crate) mod doskey;
pub(crate) mod edit;
pub(crate) mod fc;
pub(crate) mod fdisk;
pub(crate) mod find;
pub(crate) mod format;
mod handlers;
pub(crate) mod label;
pub(crate) mod mem;
pub(crate) mod mode;
pub(crate) mod more;
pub(crate) mod move_files;
pub(crate) mod sort;
pub(crate) mod sys;
pub(crate) mod tree;
pub(crate) mod xcopy;

use super::char::CTRL_Z;
use super::device::generic;
use super::message::{self, decimal_in, named};
use super::program::{say_error, Flow, Frame, Id, Int, Program, Returned, Routine};
use crate::cpu::flag as cpu_flag;
use crate::frames::{locals, Far, Outcome};
use alloc::vec::Vec;
pub use handlers::{AbandonOnBreak, FailCritical, IgnoreBreak};
use tairix_fat::boot::Kind;
use tairix_fat::table;

/// The utilities' program numbers.
pub mod number {
    pub const FIND: u16 = 2;
    pub const MORE: u16 = 3;
    pub const SORT: u16 = 4;
    pub const ATTRIB: u16 = 5;
    pub const LABEL: u16 = 6;
    pub const TREE: u16 = 7;
    pub const MEM: u16 = 8;
    pub const MODE: u16 = 9;
    pub const FC: u16 = 10;
    pub const DELTREE: u16 = 11;
    pub const MOVE: u16 = 12;
    pub const XCOPY: u16 = 13;
    pub const CHKDSK: u16 = 14;
    pub const SYS: u16 = 15;
    pub const FORMAT: u16 = 16;
    pub const DISKCOPY: u16 = 17;
    pub const FDISK: u16 = 18;
    pub const DOSKEY: u16 = 19;
    pub const EDIT: u16 = 20;
}

/// Each utility's program number, and the file it is on a system disk.
pub const FILES: &[(u16, &str)] = &[
    (number::FIND, "FIND.EXE"),
    (number::MORE, "MORE.COM"),
    (number::SORT, "SORT.EXE"),
    (number::ATTRIB, "ATTRIB.EXE"),
    (number::LABEL, "LABEL.EXE"),
    (number::TREE, "TREE.COM"),
    (number::MEM, "MEM.EXE"),
    (number::MODE, "MODE.COM"),
    (number::FC, "FC.EXE"),
    (number::DELTREE, "DELTREE.EXE"),
    (number::MOVE, "MOVE.EXE"),
    (number::XCOPY, "XCOPY.EXE"),
    (number::CHKDSK, "CHKDSK.EXE"),
    (number::SYS, "SYS.COM"),
    (number::FORMAT, "FORMAT.COM"),
    (number::DISKCOPY, "DISKCOPY.COM"),
    (number::FDISK, "FDISK.COM"),
    (number::DOSKEY, "DOSKEY.COM"),
    (number::EDIT, "EDIT.COM"),
];

/// The code a utility ends with when it could not do what it was asked.
pub const FAILED: u32 = 2;

/// What DOS's common parser says of a command line a utility cannot take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal<'a> {
    TooMany(&'a [u8]),
    Missing,
    Switch(&'a [u8]),
    Parameter(&'a [u8]),
    Format(&'a [u8]),
    Range(&'a [u8]),
    Value(&'a [u8]),
}

impl Refusal<'_> {
    /// The line it says, after `prefix`: the utility's name for those that
    /// name themselves.
    #[must_use]
    pub fn text(self, prefix: &[u8]) -> Vec<u8> {
        let (line, what): (&[u8], &[u8]) = match self {
            Self::TooMany(what) => (message::TOO_MANY, what),
            Self::Missing => (message::MISSING, b""),
            Self::Switch(what) => (message::BAD_SWITCH, what),
            Self::Parameter(what) => (message::BAD_PARAMETER, what),
            Self::Format(what) => (message::BAD_FORMAT, what),
            Self::Range(what) => (message::OUT_OF_RANGE, what),
            Self::Value(what) => (message::NOT_ALLOWED, what),
        };
        let mut out = prefix.to_vec();
        if what.is_empty() {
            out.extend_from_slice(line);
        } else {
            out.extend_from_slice(&named(line, &what.to_ascii_uppercase()));
        }
        out
    }
}

/// A run of a drive's sectors, from 0 the drive, that INT 25h reads or
/// INT 26h writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sectors {
    pub drive: u8,
    pub first: u32,
    pub count: u16,
    pub buffer: Far,
}

/// INT 21h function `440Dh`: the disk generic IOCTL `function` on drive
/// `drive`, `0` for A:, with its packet at `packet` in the program's memory.
#[must_use]
pub fn disk_ioctl(p: &Program<'_, '_>, drive: u8, function: u8, packet: u16) -> Frame {
    Frame::of(&Int {
        bx: u16::from(drive) + 1,
        cx: u16::from_le_bytes([function, generic::DISK]),
        dx: packet,
        ..Int::dos(p.own, 0x440D)
    })
}

/// INT 25h, or with `write` INT 26h, moving `sectors` through the request
/// laid at `packet` in the program's memory.
pub fn absolute(p: &mut Program<'_, '_>, packet: u16, write: bool, sectors: Sectors) -> Frame {
    p.set32(packet, sectors.first);
    p.set16(packet + 4, sectors.count);
    p.set_far(packet + 6, sectors.buffer);
    Frame::of(&Int {
        bx: packet,
        cx: 0xFFFF,
        ..Int::bios(
            p.own,
            if write { 0x26 } else { 0x25 },
            u16::from(sectors.drive),
        )
    })
}

/// Whether the interrupt just taken came back with its carry set, as the
/// BIOS and INT 25h and 26h report a failure.
pub fn carried(p: &mut Program<'_, '_>) -> bool {
    p.result(Returned::Flags) & cpu_flag::CF != 0
}

/// A FAT a utility holds whole in its memory, from segment `segment`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeldFat {
    pub segment: u16,
    pub kind: Kind,
}

impl HeldFat {
    /// Where the bytes of `cluster`'s entry begin.
    fn span(self, cluster: u16) -> (u16, u16) {
        let (at, _) = table::span(self.kind, u32::from(cluster));
        place(self.segment, u32::try_from(at).unwrap_or(u32::MAX))
    }

    /// `cluster`'s entry.
    pub fn entry(self, p: &mut Program<'_, '_>, cluster: u16) -> u16 {
        let (segment, offset) = self.span(cluster);
        let mut bytes = [0u8; 2];
        p.g.read_bytes(segment, offset, &mut bytes);
        table::read(self.kind, u32::from(cluster), &bytes)
            .map_or(0, |value| u16::try_from(value).unwrap_or(0))
    }

    /// Sets `cluster`'s entry to `value`, keeping the bits a FAT12 entry
    /// shares with its neighbour.
    pub fn set_entry(self, p: &mut Program<'_, '_>, cluster: u16, value: u16) {
        let (segment, offset) = self.span(cluster);
        let mut bytes = [0u8; 2];
        p.g.read_bytes(segment, offset, &mut bytes);
        if table::write(self.kind, u32::from(cluster), &mut bytes, u32::from(value)) {
            p.g.write_bytes(segment, offset, &bytes);
        }
    }
}

/// The segment and offset of byte `at` of a table at segment `base`.
#[must_use]
pub fn place(base: u16, at: u32) -> (u16, u16) {
    let paragraphs = u16::try_from(at >> 4).unwrap_or(u16::MAX);
    (
        base.wrapping_add(paragraphs),
        u16::try_from(at & 0xF).unwrap_or(0),
    )
}

/// Adds a report's line to `out`: `figure` in ten columns, then `words`.
pub fn figure_line(out: &mut Vec<u8>, figure: u32, words: &[&[u8]]) {
    out.extend_from_slice(&decimal_in(figure, 10));
    for word in words {
        out.extend_from_slice(word);
    }
    out.extend_from_slice(message::NEW_LINE);
}

/// The words after the figures of the disk-space lines `FORMAT` and
/// `CHKDSK` both report.
pub mod space {
    pub const TOTAL: &[u8] = b" bytes total disk space";
    pub const BAD: &[u8] = b" bytes in bad sectors";
    pub const AVAILABLE: &[u8] = b" bytes available on disk";
    pub const UNIT: &[u8] = b" bytes in each allocation unit";
}

/// A switch's name and what follows its colon, if it has one: `T:80` is
/// `T` and `80`.
#[must_use]
pub fn switch_parts(switch: &[u8]) -> (&[u8], Option<&[u8]>) {
    match switch.iter().position(|&b| b == b':') {
        Some(at) => (&switch[..at], Some(&switch[at + 1..])),
        None => (switch, None),
    }
}

/// The number a switch's `value` gives, if in `range`: one that is no
/// number is in the wrong format and one past the range out of it, `word`
/// the switch either way.
///
/// # Errors
///
/// The refusal of `word`.
pub fn value_in<'a>(
    value: &[u8],
    range: core::ops::RangeInclusive<u16>,
    word: &'a [u8],
) -> Result<u16, Refusal<'a>> {
    if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
        return Err(Refusal::Format(word));
    }
    super::shell::parse::number(value)
        .and_then(|n| u16::try_from(n).ok())
        .filter(|n| range.contains(n))
        .ok_or(Refusal::Range(word))
}

/// The call saying `refusal` on the standard error, after `prefix`.
pub fn refuse(p: &mut Program<'_, '_>, prefix: &[u8], refusal: Refusal<'_>) -> Frame {
    let text = refusal.text(prefix);
    say_error(p, &text)
}

/// Where a [`NextLine`] reader keeps its state, from the base its utility
/// gives it.
pub mod reader {
    /// Where the bytes not yet taken begin and end in the buffer.
    pub const AT: u16 = 0x00;
    pub const END: u16 = 0x02;
    /// Whether the input's end was met: a read that brought nothing, or a
    /// Ctrl+Z.
    pub const DONE: u16 = 0x04;
    /// The line found: where in the buffer, and its length.
    pub const LINE_AT: u16 = 0x06;
    pub const LINE_LEN: u16 = 0x08;
    pub const BUFFER: u16 = 0x10;
    pub const BUFFER_LEN: u16 = 0x1000;
    /// Bytes a reader takes.
    pub const LEN: u16 = BUFFER + BUFFER_LEN;
}

/// Readies the reader at `base` for an input from its start.
pub fn begin_reading(p: &mut Program<'_, '_>, base: u16) {
    for field in [
        reader::AT,
        reader::END,
        reader::DONE,
        reader::LINE_AT,
        reader::LINE_LEN,
    ] {
        p.set16(base + field, 0);
    }
}

/// The line the reader at `base` last found.
pub fn line(p: &mut Program<'_, '_>, base: u16) -> Vec<u8> {
    let (at, len) = (
        p.get16(base + reader::LINE_AT),
        p.get16(base + reader::LINE_LEN),
    );
    let mut out = alloc::vec![0u8; usize::from(len.min(reader::BUFFER_LEN))];
    p.g.read_bytes(p.own, base + reader::BUFFER + at, &mut out);
    out
}

/// The call writing the line the reader at `base` last found, as it is in
/// the buffer, to the standard output.
pub fn write_line(p: &mut Program<'_, '_>, base: u16) -> Frame {
    let (at, len) = (
        p.get16(base + reader::LINE_AT),
        p.get16(base + reader::LINE_LEN),
    );
    Frame::of(&Int {
        bx: 1,
        cx: len.min(reader::BUFFER_LEN),
        dx: base + reader::BUFFER + at,
        ..Int::dos(p.own, 0x4000)
    })
}

locals! {
    /// The next line of handle `handle`, read through the buffer of the
    /// reader at `base`: `1` once one is found, its LF and a CR before
    /// that taken off, or `0` at the input's end, which a Ctrl+Z also
    /// makes. A line longer than the buffer comes a buffer at a time.
    pub struct NextLine {
        handle: u16,
        base: u16,
    }
}

impl NextLine {
    #[must_use]
    pub const fn of(handle: u16, base: u16) -> Self {
        Self { handle, base }
    }

    /// Where the next LF lies in the buffer from `at` to `end`.
    fn find_lf(p: &mut Program<'_, '_>, base: u16, at: u16, end: u16) -> Option<u16> {
        let mut chunk = [0u8; 256];
        let mut from = at;
        while from < end {
            let len = (end - from).min(256);
            let piece = &mut chunk[..usize::from(len)];
            p.g.read_bytes(p.own, base + reader::BUFFER + from, piece);
            if let Some(n) = piece.iter().position(|&b| b == b'\n') {
                return Some(from + u16::try_from(n).unwrap_or(0));
            }
            from += len;
        }
        None
    }

    /// Takes `[at, end)` as the line, and the reader on from `next`.
    fn found(p: &mut Program<'_, '_>, base: u16, at: u16, end: u16, next: u16) -> Flow {
        let cr = end > at && p.get8(base + reader::BUFFER + end - 1) == b'\r';
        p.set16(base + reader::LINE_AT, at);
        p.set16(base + reader::LINE_LEN, end - at - u16::from(cr));
        p.set16(base + reader::AT, next);
        Flow::Return(Ok(1))
    }
}

const READ: u8 = 1;

impl Routine for NextLine {
    const ID: Id = Id::NextLine;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let base = self.base;
        if *phase == READ {
            let read = match child {
                Ok(n) => u16::try_from(n).unwrap_or(0),
                Err(fault) => return Flow::Return(Err(fault)),
            };
            let end = p.get16(base + reader::END);
            let read = read.min(reader::BUFFER_LEN - end);
            let mut fresh = alloc::vec![0u8; usize::from(read)];
            p.g.read_bytes(p.own, base + reader::BUFFER + end, &mut fresh);
            let kept = fresh
                .iter()
                .position(|&b| b == CTRL_Z)
                .unwrap_or(fresh.len());
            if read == 0 || kept < fresh.len() {
                p.set16(base + reader::DONE, 1);
            }
            p.set16(base + reader::END, end + u16::try_from(kept).unwrap_or(0));
            *phase = 0;
        }
        let (at, end) = (p.get16(base + reader::AT), p.get16(base + reader::END));
        if let Some(lf) = Self::find_lf(p, base, at, end) {
            return Self::found(p, base, at, lf, lf + 1);
        }
        if p.get16(base + reader::DONE) != 0 {
            if at < end {
                return Self::found(p, base, at, end, end);
            }
            return Flow::Return(Ok(0));
        }
        if at == 0 && end == reader::BUFFER_LEN {
            p.set16(base + reader::LINE_AT, 0);
            p.set16(base + reader::LINE_LEN, end);
            p.set16(base + reader::AT, end);
            return Flow::Return(Ok(1));
        }
        if at > 0 {
            let mut rest = alloc::vec![0u8; usize::from(end - at)];
            p.g.read_bytes(p.own, base + reader::BUFFER + at, &mut rest);
            p.g.write_bytes(p.own, base + reader::BUFFER, &rest);
            p.set16(base + reader::AT, 0);
            p.set16(base + reader::END, end - at);
        }
        let end = p.get16(base + reader::END);
        *phase = READ;
        Flow::Call(Frame::of(&Int {
            bx: self.handle,
            cx: reader::BUFFER_LEN - end,
            dx: base + reader::BUFFER + end,
            ..Int::dos(p.own, 0x3F00)
        }))
    }
}

#[cfg(test)]
#[path = "utility_tests.rs"]
mod tests;
