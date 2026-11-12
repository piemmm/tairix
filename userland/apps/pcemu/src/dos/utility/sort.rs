//! `SORT [/R] [/+n]`: the standard input's lines written to the standard
//! output in the country's collating order, compared from column `n` on,
//! reversed with `/R`. Lines end at CR LF; equal lines keep their order,
//! and a line too short to reach the column sorts as a long one of spaces,
//! as DOS's `SORT` sorts it. The input must fit 64 KiB.

use super::{refuse, Refusal};
use crate::dos::message;
use crate::dos::program::{say_error, Flow, Frame, Id, Int, Program, Returned, Routine};
use crate::dos::shell::parse;
use crate::frames::{locals, take, Outcome};
use alloc::vec::Vec;

/// Where `SORT`'s state lies in its memory.
pub mod at {
    use crate::dos::program::at::STATE;

    /// The column comparisons begin at, from `1`, and whether the order is
    /// reversed.
    pub const COLUMN: u16 = STATE;
    pub const REVERSE: u16 = STATE + 0x02;
    /// The block the input is read into and the sorted lines written out
    /// of: its segment, the paragraphs asked for, the bytes it holds, and
    /// those read or still to write.
    pub const SEGMENT: u16 = STATE + 0x04;
    pub const ASKED: u16 = STATE + 0x06;
    pub const ROOM: u16 = STATE + 0x08;
    pub const FILLED: u16 = STATE + 0x0C;
    pub const WRITTEN: u16 = STATE + 0x10;
    /// The country's collating sequence.
    pub const TABLE: u16 = STATE + 0x20;
    /// Bytes of `SORT`'s state.
    pub const LEN: u16 = 0x120;
}

const NAME: &[u8] = b"SORT: ";
const FAILED: u32 = 1;
/// The most the input may take: 64 KiB in paragraphs.
const BLOCK: u16 = 0x1000;
/// A line that does not reach the column is compared as this many spaces.
const SHORT_KEY: usize = 256;
/// The most one write moves.
const CHUNK: u32 = 0x8000;

/// What `SORT`'s command line asks for: the column, and whether reversed.
pub fn request(tail: &[u8]) -> Result<(u16, bool), Refusal<'_>> {
    let (mut column, mut reverse) = (1, false);
    for word in parse::arguments(tail) {
        let Some(switch) = word.strip_prefix(b"/") else {
            return Err(Refusal::TooMany(word));
        };
        if switch.eq_ignore_ascii_case(b"R") {
            reverse = true;
        } else if let Some(digits) = switch.strip_prefix(b"+") {
            let value = digits
                .iter()
                .try_fold(0u32, |n, &b| {
                    b.is_ascii_digit()
                        .then(|| n.saturating_mul(10).saturating_add(u32::from(b - b'0')))
                })
                .filter(|_| !digits.is_empty())
                .ok_or(Refusal::Format(word))?;
            column = u16::try_from(value)
                .ok()
                .filter(|&n| n >= 1)
                .ok_or(Refusal::Range(word))?;
        } else {
            return Err(Refusal::Switch(word));
        }
    }
    Ok((column, reverse))
}

/// The lines of `input`, as far as a Ctrl+Z, sorted through `table` from
/// `column`, each written with its CR LF.
#[must_use]
pub fn sorted(input: &[u8], table: &[u8; 256], column: u16, reverse: bool) -> Vec<u8> {
    let input = &input[..input.iter().position(|&b| b == 0x1A).unwrap_or(input.len())];
    let input = input.strip_suffix(b"\r\n").unwrap_or(input);
    if input.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&[u8]> = Vec::new();
    let mut rest = input;
    while let Some(at) = rest.windows(2).position(|pair| pair == b"\r\n") {
        lines.push(&rest[..at]);
        rest = &rest[at + 2..];
    }
    lines.push(rest);
    let skip = usize::from(column) - 1;
    let key = |line: &[u8]| -> Vec<u8> {
        match line.get(skip..) {
            Some(from) if !from.is_empty() => from.iter().map(|&b| table[usize::from(b)]).collect(),
            _ => alloc::vec![table[usize::from(b' ')]; SHORT_KEY],
        }
    };
    let mut keyed: Vec<(Vec<u8>, &[u8])> =
        lines.into_iter().map(|line| (key(line), line)).collect();
    if reverse {
        keyed.sort_by(|a, b| b.0.cmp(&a.0));
    } else {
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
    }
    let mut out = Vec::with_capacity(input.len() + 2 * keyed.len());
    for (_, line) in keyed {
        out.extend_from_slice(line);
        out.extend_from_slice(b"\r\n");
    }
    out
}

locals! {
    /// `SORT`'s main routine.
    pub struct Sort {}
}

/// [`Sort`]'s phases; any past these, a refusal or a failure said, ends it.
mod phase {
    pub const REFUSED: u8 = 1;
    pub const SHRUNK: u8 = 2;
    pub const ALLOCATED: u8 = 3;
    pub const TABLE: u8 = 4;
    pub const READ: u8 = 5;
    pub const WRITTEN: u8 = 6;
    pub const FAILED: u8 = 7;
}

impl Sort {
    /// Reads on into the block, keeping room for the CR LF a last line
    /// without one is written with.
    fn read_on(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let (room, filled) = (p.get32(at::ROOM), p.get32(at::FILLED));
        if filled + 2 >= room {
            *phase = phase::FAILED;
            return Flow::Call(say_error(p, &[NAME, message::MEMORY].concat()));
        }
        *phase = phase::READ;
        Flow::Call(Frame::of(&Int {
            cx: u16::try_from(room - 2 - filled).unwrap_or(u16::MAX),
            dx: u16::try_from(filled).unwrap_or(0),
            ds: p.get16(at::SEGMENT),
            ..Int::dos(p.own, 0x3F00)
        }))
    }

    /// Sorts the lines read into the block in place, for writing from its
    /// start.
    fn sort_block(p: &mut Program<'_, '_>) {
        let (segment, filled) = (p.get16(at::SEGMENT), p.get32(at::FILLED));
        let mut input = alloc::vec![0u8; usize::try_from(filled).unwrap_or(0)];
        p.g.read_bytes(segment, 0, &mut input);
        let mut table = [0u8; 256];
        p.g.read_bytes(p.own, at::TABLE, &mut table);
        let out = sorted(
            &input,
            &table,
            p.get16(at::COLUMN),
            p.get8(at::REVERSE) != 0,
        );
        p.g.write_bytes(segment, 0, &out);
        p.set32(at::FILLED, u32::try_from(out.len()).unwrap_or(0));
        p.set32(at::WRITTEN, 0);
    }

    /// Writes the next chunk of the sorted lines, or ends once all are.
    fn write_on(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let (filled, written) = (p.get32(at::FILLED), p.get32(at::WRITTEN));
        if written >= filled {
            return Flow::Return(Ok(0));
        }
        let len = (filled - written).min(CHUNK);
        *phase = phase::WRITTEN;
        Flow::Call(Frame::of(&Int {
            bx: 1,
            cx: u16::try_from(len).unwrap_or(0),
            dx: u16::try_from(written).unwrap_or(0),
            ds: p.get16(at::SEGMENT),
            ..Int::dos(p.own, 0x4000)
        }))
    }
}

impl Routine for Sort {
    const ID: Id = Id::Sort;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                let tail = p.tail();
                let (column, reverse) = match request(&tail) {
                    Ok(request) => request,
                    Err(refusal) => {
                        *phase = phase::REFUSED;
                        return Flow::Call(refuse(p, NAME, refusal));
                    }
                };
                p.set16(at::COLUMN, column);
                p.set8(at::REVERSE, u8::from(reverse));
                *phase = phase::SHRUNK;
                Flow::Call(p.shrink())
            }
            phase::SHRUNK => {
                take!(child);
                p.set16(at::ASKED, BLOCK);
                *phase = phase::ALLOCATED;
                Flow::Call(Frame::of(&Int {
                    bx: BLOCK,
                    ..Int::dos(p.own, 0x4800)
                }))
            }
            // Short of 64 KiB, what there is will do.
            phase::ALLOCATED => {
                let asked = p.get16(at::ASKED);
                let Ok(segment) = child else {
                    let largest = p.result(Returned::Bx);
                    if largest == 0 || largest >= asked {
                        *phase = phase::FAILED;
                        return Flow::Call(say_error(p, &[NAME, message::MEMORY].concat()));
                    }
                    p.set16(at::ASKED, largest);
                    return Flow::Call(Frame::of(&Int {
                        bx: largest,
                        ..Int::dos(p.own, 0x4800)
                    }));
                };
                p.set16(at::SEGMENT, u16::try_from(segment).unwrap_or(0));
                p.set32(at::ROOM, u32::from(asked) * 16);
                p.set32(at::FILLED, 0);
                *phase = phase::TABLE;
                Flow::Call(Frame::of(&Int {
                    bx: 0xFFFF,
                    cx: 5,
                    dx: 0xFFFF,
                    di: p.scratch(0),
                    ..Int::dos(p.own, 0x6506)
                }))
            }
            phase::TABLE => {
                let mut table = [0u8; 256];
                if child.is_ok() {
                    let record = p.scratch(0);
                    let pointer = p.get_far(record + 1);
                    p.g.read_bytes(pointer.segment, pointer.offset.wrapping_add(2), &mut table);
                } else {
                    table
                        .iter_mut()
                        .zip(0u8..)
                        .for_each(|(slot, byte)| *slot = byte);
                }
                p.g.write_bytes(p.own, at::TABLE, &table);
                Self::read_on(p, phase)
            }
            phase::READ => {
                let read = take!(child);
                if read > 0 {
                    let filled = p.get32(at::FILLED) + read;
                    p.set32(at::FILLED, filled);
                    return Self::read_on(p, phase);
                }
                Self::sort_block(p);
                Self::write_on(p, phase)
            }
            phase::WRITTEN => {
                let Ok(wrote) = child else {
                    return Flow::Return(Ok(FAILED));
                };
                let (filled, written) = (p.get32(at::FILLED), p.get32(at::WRITTEN));
                let expected = (filled - written).min(CHUNK);
                if wrote != expected {
                    *phase = phase::FAILED;
                    return Flow::Call(say_error(p, &[NAME, message::DISK_FULL].concat()));
                }
                p.set32(at::WRITTEN, written + expected);
                Self::write_on(p, phase)
            }
            _ => Flow::Return(Ok(FAILED)),
        }
    }
}
