//! `MEM [/PROGRAM] [/DEBUG] [/CLASSIFY]`: how much memory there is, how
//! much a program may have, and the expanded memory board's pages; with
//! `/PROGRAM` every block of the memory chain by its owner, with `/DEBUG`
//! the device drivers and the expanded memory handles too, and with
//! `/CLASSIFY` each program's share. The chains are read where DOS keeps
//! them, as DOS's `MEM` reads them, a line at a time, and every walk is
//! bounded, so a chain a program has damaged ends the listing rather than
//! running on.

use super::{refuse, Refusal};
use crate::bios::layout::EMS_DEVICE;
use crate::dos::data::sysvars;
use crate::dos::device::{attribute, header};
use crate::dos::memory::{mcb, psp};
use crate::dos::message::{self, decimal, decimal_in};
use crate::dos::program::{say, Flow, Frame, Id, Int, Program, Returned, Routine};
use crate::dos::shell::{letter, parse};
use crate::ems;
use crate::frames::{locals, take, Far, Fault, Outcome};
use alloc::vec::Vec;

/// Where `MEM`'s state lies in its memory.
pub mod at {
    use crate::dos::program::at::STATE;

    /// The [`super::switch`]es given, and what the listing shows next.
    pub const SWITCHES: u16 = STATE;
    pub const CURSOR: u16 = STATE + 0x01;
    /// Conventional memory in KiB, `MEM`'s own PSP, the first MCB, and the
    /// `NUL` device's header.
    pub const KILOBYTES: u16 = STATE + 0x02;
    pub const OWN: u16 = STATE + 0x04;
    pub const FIRST: u16 = STATE + 0x06;
    pub const NUL: u16 = STATE + 0x08;
    /// The device or block the listing is at, the steps left before a chain
    /// is taken to be damaged, and the drive letter the next block device's
    /// units begin at.
    pub const NEXT: u16 = STATE + 0x0C;
    pub const STEPS: u16 = STATE + 0x10;
    pub const LETTER: u16 = STATE + 0x12;
    /// Whether an expanded memory manager answers, and its pages.
    pub const EMS: u16 = STATE + 0x13;
    pub const EMS_TOTAL: u16 = STATE + 0x14;
    pub const EMS_FREE: u16 = STATE + 0x16;
    /// The handles the manager reported, the one or the class the listing
    /// is at, and a handle's name.
    pub const HANDLES: u16 = STATE + 0x18;
    pub const INDEX: u16 = STATE + 0x1A;
    pub const HANDLE_NAME: u16 = STATE + 0x1C;
    /// The handles' pages, as the manager answers them; until then, text
    /// longer than one say's scratch.
    pub const PAGES: u16 = STATE + 0x30;
    pub const PAGES_LEN: u16 = 0x400;
    /// Bytes of `MEM`'s state.
    pub const LEN: u16 = PAGES + PAGES_LEN - STATE;
}

/// What [`at::SWITCHES`] holds.
pub mod switch {
    pub const PROGRAM: u8 = 0x01;
    pub const DEBUG: u8 = 0x02;
    pub const CLASSIFY: u8 = 0x04;
}

/// What the listing shows next.
mod cursor {
    pub const DEVICES: u8 = 0;
    pub const BLOCKS: u8 = 1;
    pub const HANDLES: u8 = 2;
    pub const CLASSES: u8 = 3;
    pub const SUMMARY: u8 = 4;
}

/// The most blocks or devices a chain is followed through.
const STEPS: u16 = 0xA000;
/// Where the system ROMs begin: a driver there is the system's own.
const ROM: u16 = 0xA000;
/// The kernel's name for the memory it keeps, eight bytes at most.
const DOS: &[u8] = b"TAIRIX";
const SYSTEM_DATA: &[u8] = b"System Data";
/// The horizontal line the headings are underlined with.
const LINE: u8 = 0xC4;
const DAMAGED: &[u8] = b"\r\nMemory control blocks damaged\r\n";

/// What `MEM`'s command line asks for.
pub fn request(tail: &[u8]) -> Result<u8, Refusal<'_>> {
    let mut switches = 0;
    for word in parse::arguments(tail) {
        let Some(switch) = word.strip_prefix(b"/") else {
            return Err(Refusal::TooMany(word));
        };
        switches |= match switch.to_ascii_uppercase().as_slice() {
            b"P" | b"PROGRAM" => switch::PROGRAM,
            b"D" | b"DEBUG" => switch::DEBUG | switch::PROGRAM,
            b"C" | b"CLASSIFY" => switch::CLASSIFY,
            _ => return Err(Refusal::Switch(word)),
        };
    }
    Ok(switches)
}

/// `n` in upper-case hex, at least `width` digits.
#[must_use]
pub fn hex(n: u32, width: usize) -> Vec<u8> {
    let significant = usize::try_from(8 - n.leading_zeros() / 4)
        .unwrap_or(8)
        .max(1);
    let mut out = alloc::vec![0; significant.max(width)];
    message::hex(n, &mut out);
    out
}

/// `text` right-aligned in `width`.
fn right(text: &[u8], width: usize) -> Vec<u8> {
    let mut out = alloc::vec![b' '; width.saturating_sub(text.len())];
    out.extend_from_slice(text);
    out
}

/// `bytes` in KiB to a tenth, its whole part in three columns.
fn kilobytes(bytes: u32) -> Vec<u8> {
    let tenths = (u64::from(bytes) * 10 + 512) / 1024;
    let tenths = u32::try_from(tenths).unwrap_or(u32::MAX);
    [&decimal_in(tenths / 10, 3)[..], b".", &decimal(tenths % 10)].concat()
}

/// A line of the memory map: where, whose, how big, what.
#[must_use]
pub fn map_line(address: u32, name: &[u8], size: u32, kind: &[u8]) -> Vec<u8> {
    let mut name = name[..name.len().min(8)].to_vec();
    name.resize(8, b' ');
    [
        &b"  "[..],
        &hex(address, 6),
        b"      ",
        &name,
        b"     ",
        &hex(size, 6),
        b"     ",
        kind,
        b"\r\n",
    ]
    .concat()
}

/// A line of the summary: a count of bytes right-aligned, and what it is.
fn summary_line(bytes: u32, what: &[u8]) -> Vec<u8> {
    [&decimal_in(bytes, 10)[..], b" ", what, b"\r\n"].concat()
}

/// A program's line of `/CLASSIFY`: its name and its bytes.
#[must_use]
pub fn class_line(name: &[u8], bytes: u32) -> Vec<u8> {
    let mut name = name.to_vec();
    name.resize(name.len().max(12), b' ');
    [
        &b"  "[..],
        &name,
        &decimal_in(bytes, 12),
        b"   (",
        &kilobytes(bytes),
        b"K)",
        &right(&hex(bytes, 1), 13),
        b"\r\n",
    ]
    .concat()
}

/// A memory control block, as the chain holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Block {
    segment: u16,
    last: bool,
    owner: u16,
    paragraphs: u16,
}

impl Block {
    /// The block whose MCB lies at `segment`, if a chain's MCB is there.
    fn at(p: &mut Program<'_, '_>, segment: u16) -> Option<Self> {
        let kind = p.g.read8(segment, mcb::KIND);
        if kind != mcb::MORE && kind != mcb::LAST {
            return None;
        }
        Some(Self {
            segment,
            last: kind == mcb::LAST,
            owner: p.g.read16(segment, mcb::OWNER),
            paragraphs: p.g.read16(segment, mcb::SIZE),
        })
    }

    /// The next block's MCB, if this is not the last.
    fn next(self) -> Option<u16> {
        if self.last {
            return None;
        }
        self.segment.checked_add(self.paragraphs)?.checked_add(1)
    }

    fn bytes(self) -> u32 {
        u32::from(self.paragraphs) * 16
    }

    /// Who owns it, as its owner's MCB names it.
    fn owner_name(self, p: &mut Program<'_, '_>) -> Vec<u8> {
        match self.owner {
            mcb::FREE => b"FREE".to_vec(),
            mcb::DOS => DOS.to_vec(),
            owner => {
                let mut name = [0u8; 8];
                p.g.read_bytes(owner.wrapping_sub(1), mcb::NAME, &mut name);
                name[..name.iter().position(|&b| b == 0).unwrap_or(8)].to_vec()
            }
        }
    }

    /// What it holds: its owner's program or environment, DOS's data, or
    /// nothing.
    fn kind(self, p: &mut Program<'_, '_>) -> &'static [u8] {
        let data = self.segment.wrapping_add(1);
        match self.owner {
            mcb::FREE => b"-- Free --",
            mcb::DOS => SYSTEM_DATA,
            owner if owner == data => b"Program",
            owner if p.g.read16(owner, psp::ENVIRONMENT) == data => b"Environment",
            _ => b"Data",
        }
    }
}

/// The blocks of the chain from `first`, as far as its last or a damaged
/// MCB, with whether it was damaged.
fn chain(p: &mut Program<'_, '_>, first: u16) -> (Vec<Block>, bool) {
    let (mut out, mut segment) = (Vec::new(), first);
    for _ in 0..STEPS {
        let Some(block) = Block::at(p, segment) else {
            return (out, true);
        };
        out.push(block);
        let Some(next) = block.next() else {
            return (out, false);
        };
        segment = next;
    }
    (out, true)
}

/// The owners of `blocks` in the order they first own one, the free
/// blocks' last: each one's name and bytes.
fn classes(p: &mut Program<'_, '_>, blocks: &[Block]) -> Vec<(Vec<u8>, u32)> {
    let mut owners: Vec<u16> = Vec::new();
    for block in blocks {
        if block.owner != mcb::FREE && !owners.contains(&block.owner) {
            owners.push(block.owner);
        }
    }
    owners.push(mcb::FREE);
    owners
        .into_iter()
        .map(|owner| {
            let mine = blocks.iter().filter(|b| b.owner == owner);
            let bytes = mine.clone().map(|b| b.bytes()).sum();
            let name = mine
                .clone()
                .next()
                .map_or_else(|| b"FREE".to_vec(), |b| b.owner_name(p));
            (name, bytes)
        })
        .collect()
}

locals! {
    /// `MEM`'s main routine.
    pub struct Mem {}
}

mod phase {
    pub const REFUSED: u8 = 1;
    pub const SIZED: u8 = 2;
    pub const OWN: u8 = 3;
    pub const LISTS: u8 = 4;
    pub const PAGES: u8 = 5;
    pub const SHOWN: u8 = 6;
    pub const HANDLES: u8 = 7;
    pub const HEADED: u8 = 8;
    pub const NAMED: u8 = 9;
    pub const DONE: u8 = 10;
}

impl Mem {
    /// The lines above the memory chain: the areas below DOS, and DOS's own.
    fn heading(p: &mut Program<'_, '_>) -> Vec<u8> {
        let first = p.get16(at::FIRST);
        let underline = [
            &b"  "[..],
            &[LINE; 7],
            b"     ",
            &[LINE; 8],
            b"     ",
            &[LINE; 6],
            b"     ",
            &[LINE; 6],
            b"\r\n",
        ]
        .concat();
        let mut text = [
            &b"\r\n  Address     Name          Size       Type \r\n"[..],
            &underline,
        ]
        .concat();
        text.extend(map_line(0, b"", 0x400, b"Interrupt Vector"));
        text.extend(map_line(0x400, b"", 0x100, b"ROM Communication Area"));
        text.extend(map_line(0x500, b"", 0x200, b"DOS Communication Area"));
        text.extend_from_slice(b"\r\n");
        text.extend(map_line(
            0x700,
            DOS,
            u32::from(first.saturating_sub(0x70)) * 16,
            SYSTEM_DATA,
        ));
        text
    }

    /// The listing begun as the switches ask: the classes, the map, or
    /// straight to the summary.
    fn begin_listing(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let switches = p.get8(at::SWITCHES);
        *phase = phase::SHOWN;
        if switches & switch::CLASSIFY != 0 {
            p.set8(at::CURSOR, cursor::CLASSES);
            p.set16(at::INDEX, 0);
            let rule = [
                &[b'-'; 13][..],
                b"      ",
                &[b'-'; 21],
                b"   ",
                &[b'-'; 13],
                b"\r\n",
            ]
            .concat();
            let heading = [&b"\r\nConventional Memory :\r\n\r\n  Name                Size in Decimal       Size in Hex\r\n"[..], &rule].concat();
            return Flow::Call(say(p, &heading));
        }
        if switches & switch::PROGRAM == 0 {
            p.set8(at::CURSOR, cursor::SUMMARY);
            return Self::listed(p, phase);
        }
        p.set8(at::LETTER, 0);
        if switches & switch::DEBUG != 0 {
            p.set8(at::CURSOR, cursor::DEVICES);
            let nul = p.get_far(at::NUL);
            p.set_far(at::NEXT, nul);
        } else {
            p.set8(at::CURSOR, cursor::BLOCKS);
            let first = p.get16(at::FIRST);
            p.set_far(at::NEXT, Far::new(first, 0));
        }
        let heading = Self::heading(p);
        Flow::Call(write_text(p, &heading))
    }

    /// The listing's next line, or on to the handles or the summary.
    fn listed(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if p.get8(at::CURSOR) == cursor::HANDLES {
            *phase = phase::HANDLES;
            return Flow::Call(Frame::of(&Int {
                di: at::PAGES,
                ..Int::bios(p.own, 0x67, 0x4D00)
            }));
        }
        if let Some(line) = Self::next_line(p) {
            return Flow::Call(say(p, &line));
        }
        *phase = phase::DONE;
        let summary = Self::summary(p);
        Flow::Call(say(p, &summary))
    }

    /// The next line of the listing, and where it goes on from.
    fn next_line(p: &mut Program<'_, '_>) -> Option<Vec<u8>> {
        let steps = p.get16(at::STEPS);
        if steps == 0 && p.get8(at::CURSOR) != cursor::SUMMARY {
            p.set8(at::CURSOR, cursor::SUMMARY);
            return Some(DAMAGED.to_vec());
        }
        p.set16(at::STEPS, steps.saturating_sub(1));
        match p.get8(at::CURSOR) {
            cursor::DEVICES => Some(Self::device_line(p)),
            cursor::BLOCKS => Some(Self::block_line(p)),
            cursor::CLASSES => Self::next_class(p),
            _ => None,
        }
    }

    /// The device the listing is at, and the chain on to the next.
    fn device_line(p: &mut Program<'_, '_>) -> Vec<u8> {
        let device = p.get_far(at::NEXT);
        let next = Far::new(
            p.g.read16(device.segment, device.offset.wrapping_add(2)),
            p.g.read16(device.segment, device.offset),
        );
        let attributes = p.g.read16(
            device.segment,
            device.offset.wrapping_add(header::ATTRIBUTES),
        );
        let mut name = [0u8; 8];
        p.g.read_bytes(
            device.segment,
            device.offset.wrapping_add(header::NAME),
            &mut name,
        );
        let mut shown: Vec<u8> = if attributes & attribute::CHARACTER != 0 {
            name.iter()
                .copied()
                .take_while(|&b| b != b' ' && b != 0)
                .collect()
        } else {
            let (first, units) = (p.get8(at::LETTER), name[0]);
            p.set8(at::LETTER, first.saturating_add(units));
            let last = first.saturating_add(units.saturating_sub(1));
            if units <= 1 {
                alloc::vec![letter(first), b':']
            } else {
                alloc::vec![letter(first), b':', b' ', b'-', b' ', letter(last), b':']
            }
        };
        shown.resize(shown.len().max(8), b' ');
        let installed = (p.get16(at::FIRST)..ROM).contains(&device.segment);
        let kind: &[u8] = if installed {
            b"Installed Device Driver"
        } else {
            b"System Device Driver"
        };
        if next.offset == 0xFFFF {
            p.set8(at::CURSOR, cursor::BLOCKS);
            let first = p.get16(at::FIRST);
            p.set_far(at::NEXT, Far::new(first, 0));
        } else {
            p.set_far(at::NEXT, next);
        }
        [&[b' '; 18][..], &shown, &[b' '; 14], kind, b"\r\n"].concat()
    }

    /// The block the listing is at, and the chain on to the next.
    fn block_line(p: &mut Program<'_, '_>) -> Vec<u8> {
        let segment = p.get_far(at::NEXT).segment;
        let Some(block) = Block::at(p, segment) else {
            p.set8(at::CURSOR, cursor::SUMMARY);
            return DAMAGED.to_vec();
        };
        let name = if block.owner == mcb::FREE {
            Vec::new()
        } else {
            block.owner_name(p)
        };
        let kind = block.kind(p);
        let line = map_line(u32::from(segment) * 16, &name, block.bytes(), kind);
        if let Some(next) = block.next() {
            p.set_far(at::NEXT, Far::new(next, 0));
        } else {
            let ems = p.get8(at::SWITCHES) & switch::DEBUG != 0 && p.get8(at::EMS) != 0;
            p.set8(
                at::CURSOR,
                if ems {
                    cursor::HANDLES
                } else {
                    cursor::SUMMARY
                },
            );
        }
        line
    }

    /// The next program's share of conventional memory, then the free
    /// memory's.
    fn next_class(p: &mut Program<'_, '_>) -> Option<Vec<u8>> {
        let first = p.get16(at::FIRST);
        let (blocks, damaged) = chain(p, first);
        if damaged {
            p.set8(at::CURSOR, cursor::SUMMARY);
            return Some(DAMAGED.to_vec());
        }
        let index = p.get16(at::INDEX);
        let Some((name, bytes)) = classes(p, &blocks).into_iter().nth(usize::from(index)) else {
            p.set8(at::CURSOR, cursor::SUMMARY);
            return None;
        };
        p.set16(at::INDEX, index + 1);
        Some(class_line(&name, bytes))
    }

    /// The summary: conventional memory, then expanded memory's pages;
    /// for `/CLASSIFY`, the free memory's total first.
    fn summary(p: &mut Program<'_, '_>) -> Vec<u8> {
        let total = u32::from(p.get16(at::KILOBYTES)) * 1024;
        let own = p.get16(at::OWN);
        let top = p.g.read16(own, psp::TOP);
        let largest = u32::from(top.saturating_sub(own)) * 16;
        let mut text = b"\r\n".to_vec();
        if p.get8(at::SWITCHES) & switch::CLASSIFY != 0 {
            let first = p.get16(at::FIRST);
            let (blocks, _) = chain(p, first);
            let free: u32 = blocks
                .iter()
                .filter(|b| b.owner == mcb::FREE)
                .map(|b| b.bytes())
                .sum();
            text.extend(
                [
                    &b"Total  FREE :  "[..],
                    &decimal_in(free, 12),
                    b"   (",
                    &kilobytes(free),
                    b"K)\r\n\r\n",
                ]
                .concat(),
            );
        }
        text.extend(summary_line(total, b"bytes total conventional memory"));
        text.extend(summary_line(total, b"bytes available to TAIRiX DOS"));
        text.extend(summary_line(largest, b"largest executable program size"));
        if p.get8(at::EMS) != 0 {
            text.extend_from_slice(b"\r\n");
            text.extend(summary_line(
                u32::from(p.get16(at::EMS_TOTAL)) * ems::PAGE,
                b"bytes total EMS memory",
            ));
            text.extend(summary_line(
                u32::from(p.get16(at::EMS_FREE)) * ems::PAGE,
                b"bytes free EMS memory",
            ));
        }
        text
    }

    /// The heading over the expanded memory manager's handles.
    fn handles_heading() -> Vec<u8> {
        let underline = [
            &b"  "[..],
            &[LINE; 7],
            b"     ",
            &[LINE; 8],
            b"     ",
            &[LINE; 6],
            b"  \r\n",
        ]
        .concat();
        [
            &b"\r\n  Handle      EMS Name      Size   \r\n"[..],
            &underline,
        ]
        .concat()
    }

    /// The expanded memory manager's line for the handle the listing is at.
    fn handle_line(p: &mut Program<'_, '_>, named: bool) -> Vec<u8> {
        let n = p.get16(at::INDEX);
        let (handle, pages) = (p.get16(at::PAGES + n * 4), p.get16(at::PAGES + n * 4 + 2));
        let mut name = [b' '; 8];
        if named {
            p.g.read_bytes(p.own, at::HANDLE_NAME, &mut name);
        }
        let shown: Vec<u8> = name
            .iter()
            .map(|&b| if b < b' ' { b' ' } else { b })
            .collect();
        p.set16(at::INDEX, n + 1);
        [
            &b"     "[..],
            &right(&hex(u32::from(handle), 2), 4),
            b"      ",
            &shown,
            b"     ",
            &hex(u32::from(pages) * ems::PAGE, 6),
            b"\r\n",
        ]
        .concat()
    }
}

impl Routine for Mem {
    const ID: Id = Id::Mem;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let answered = |child: Outcome| child.is_ok_and(|ax| ax & 0xFF00 == 0);
        match *phase {
            0 => {
                let tail = p.tail();
                let switches = match request(&tail) {
                    Ok(switches) => switches,
                    Err(refusal) => {
                        *phase = phase::REFUSED;
                        return Flow::Call(refuse(p, b"", refusal));
                    }
                };
                p.set8(at::SWITCHES, switches);
                p.set8(at::EMS, 0);
                p.set16(at::STEPS, STEPS);
                *phase = phase::SIZED;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x12, 0)))
            }
            phase::REFUSED => Flow::Return(Ok(1)),
            phase::SIZED => {
                let kilobytes = u16::try_from(take!(child)).unwrap_or(0);
                p.set16(at::KILOBYTES, kilobytes);
                *phase = phase::OWN;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x6200)))
            }
            phase::OWN => {
                let own = p.result(Returned::Bx);
                p.set16(at::OWN, own);
                *phase = phase::LISTS;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x5200)))
            }
            phase::LISTS => {
                let (segment, offset) = (p.result(Returned::Es), p.result(Returned::Bx));
                let first =
                    p.g.read16(segment, offset.wrapping_add_signed(sysvars::FIRST_MCB));
                p.set16(at::FIRST, first);
                p.set_far(
                    at::NUL,
                    Far::new(segment, offset.wrapping_add_signed(sysvars::NUL)),
                );
                let vector = p.g.read16(0, 0x67 * 4 + 2);
                let mut name = [0u8; 8];
                p.g.read_bytes(vector, header::NAME, &mut name);
                *phase = phase::PAGES;
                if &name != EMS_DEVICE {
                    return self.step(p, phase, Err(Fault::Error(0)));
                }
                Flow::Call(Frame::of(&Int::bios(p.own, 0x67, 0x4200)))
            }
            phase::PAGES => {
                if answered(child) {
                    p.set8(at::EMS, 1);
                    let (free, total) = (p.result(Returned::Bx), p.result(Returned::Dx));
                    p.set16(at::EMS_FREE, free);
                    p.set16(at::EMS_TOTAL, total);
                }
                Self::begin_listing(p, phase)
            }
            phase::SHOWN => {
                take!(child);
                Self::listed(p, phase)
            }
            phase::HANDLES => {
                let count = if answered(child) {
                    p.result(Returned::Bx)
                } else {
                    0
                };
                p.set16(at::HANDLES, count.min(at::PAGES_LEN / 4));
                p.set16(at::INDEX, 0);
                p.set8(at::CURSOR, cursor::SUMMARY);
                *phase = phase::HEADED;
                Flow::Call(say(p, &Self::handles_heading()))
            }
            phase::HEADED => {
                let n = p.get16(at::INDEX);
                if n >= p.get16(at::HANDLES) {
                    *phase = phase::SHOWN;
                    return Self::listed(p, phase);
                }
                let handle = p.get16(at::PAGES + n * 4);
                *phase = phase::NAMED;
                Flow::Call(Frame::of(&Int {
                    dx: handle,
                    di: at::HANDLE_NAME,
                    ..Int::bios(p.own, 0x67, 0x5300)
                }))
            }
            phase::NAMED => {
                let line = Self::handle_line(p, answered(child));
                *phase = phase::HEADED;
                Flow::Call(say(p, &line))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

/// The call writing `text`, which may outrun one say's scratch, through the
/// pages area, which holds nothing until the handles are listed.
fn write_text(p: &mut Program<'_, '_>, text: &[u8]) -> Frame {
    let len = text.len().min(usize::from(at::PAGES_LEN));
    p.g.write_bytes(p.own, at::PAGES, &text[..len]);
    Frame::of(&Int {
        bx: 1,
        cx: u16::try_from(len).unwrap_or(0),
        dx: at::PAGES,
        ..Int::dos(p.own, 0x4000)
    })
}
