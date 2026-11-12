//! The clusters no chain reached, as DOS 4.0's `CHKDSK` deals with them:
//! gathered into chains and counted, then, as the user answers, each chain
//! made a file `FILEnnnn.CHK` in a free slot of the root, or all of them
//! freed. Nothing reaches the disk without `/F`, nor when part of the tree
//! went unwalked, whose clusters these may be.

use super::cells;
use super::{
    absolute, add, at, cluster_bytes, count, counted, failed, fixing, flag, has, held, set,
    set_count, switch, switched, text, volume, Ask, Tell,
};
use crate::dos::message::{decimal, decimal_in};
use crate::dos::program::{say, say_error, Flow, Frame, Id, Int, Program, Returned, Routine};
use crate::dos::shell::{letter, set_asciz};
use crate::frames::{locals, take, Far, Outcome};
use tairix_fat::dir::{ShortEntry, DELETED, END, ENTRY};

/// What a found name is searched for as: anything a file could be told
/// from but a label.
const ANY_FILE: u16 = 0x16;

locals! {
    /// The lost clusters dealt with.
    pub struct ChkdskLost {}
}

mod phase {
    pub const ERRORS_SAID: u8 = 1;
    pub const BLANK: u8 = 2;
    pub const COUNTED: u8 = 3;
    pub const ASKED: u8 = 4;
    pub const TIME: u8 = 5;
    pub const DATE: u8 = 6;
    pub const SECTOR: u8 = 7;
    pub const PROBED: u8 = 8;
    pub const WRITTEN: u8 = 9;
    pub const SAID: u8 = 10;
}

impl ChkdskLost {
    /// The name the next file made of a chain takes, after the one it has.
    fn next_name(p: &mut Program<'_, '_>) {
        for at in (at::ORPHAN + 4..at::ORPHAN + 8).rev() {
            let digit = p.get8(at);
            if digit < b'9' {
                p.set8(at, digit + 1);
                return;
            }
            p.set8(at, b'0');
        }
    }

    /// The search for a file of the name the next chain would take.
    fn probe(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let mut field = [0u8; 11];
        p.g.read_bytes(p.own, at::ORPHAN, &mut field);
        let path = [
            &[letter(p.get8(at::DRIVE)), b':', b'\\'][..],
            &super::spelt(&field),
        ]
        .concat();
        set_asciz(p, at::PROBE, &path, 0x14);
        *phase = phase::PROBED;
        Flow::Call(Frame::of(&Int {
            cx: ANY_FILE,
            dx: at::PROBE,
            ..Int::dos(p.own, 0x4E00)
        }))
    }

    /// The root's next free slot taken for a chain, its sector read first.
    fn scan(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let per_sector = p.get16(at::SECTOR_BYTES) / 32;
        loop {
            let index = p.get16(at::ROOT_INDEX);
            if index >= p.get16(at::ROOT_ENTRIES) {
                return Self::root_full(p, phase);
            }
            let sector = u32::from(p.get16(at::ROOT_SECTOR)) + u32::from(index / per_sector);
            if p.get32(at::BUFFERED) != sector {
                *phase = phase::SECTOR;
                return Flow::Call(absolute(p, false, sector, 1, Far::new(p.own, at::BUFFER)));
            }
            let lead = p.get8(at::BUFFER + index % per_sector * 32);
            if matches!(lead, DELETED | END) {
                if fixing(p) {
                    return Self::probe(p, phase);
                }
                if let Some(flow) = Self::placed(p) {
                    return flow;
                }
                continue;
            }
            p.set16(at::ROOT_INDEX, index + 1);
        }
    }

    /// A chain has its slot: on to the next chain, if any is left.
    fn placed(p: &mut Program<'_, '_>) -> Option<Flow> {
        let left = p.get32(at::LEFT).saturating_sub(1);
        p.set32(at::LEFT, left);
        if left == 0 {
            return Some(Flow::Return(Ok(0)));
        }
        let volume = volume(p);
        let from = p.get16(at::HEAD).saturating_add(1);
        let head = cells::next_head(&mut held(p), volume, from).unwrap_or(0);
        p.set16(at::HEAD, head);
        let index = p.get16(at::ROOT_INDEX) + 1;
        p.set16(at::ROOT_INDEX, index);
        None
    }

    /// The chain made a file in the slot found: its name, the stamp, its
    /// first cluster and its size.
    fn make(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let per_sector = p.get16(at::SECTOR_BYTES) / 32;
        let index = p.get16(at::ROOT_INDEX);
        let head = p.get16(at::HEAD);
        let volume = volume(p);
        let clusters = cells::chain_length(&mut held(p), volume, head);
        let stamp = p.get32(at::STAMP);
        let mut entry = ShortEntry {
            cluster: u32::from(head),
            size: clusters.saturating_mul(cluster_bytes(p)),
            ..ShortEntry::default()
        };
        p.g.read_bytes(p.own, at::ORPHAN, &mut entry.name);
        entry.stamps.written.date = u16::try_from(stamp >> 16).unwrap_or(0);
        entry.stamps.written.time = u16::try_from(stamp & 0xFFFF).unwrap_or(0);
        let raw: [u8; ENTRY] = entry.encode();
        p.g.write_bytes(p.own, at::BUFFER + index % per_sector * 32, &raw);
        Self::next_name(p);
        let sector = p.get32(at::BUFFERED);
        *phase = phase::WRITTEN;
        Flow::Call(absolute(p, true, sector, 1, Far::new(p.own, at::BUFFER)))
    }

    /// The answer was no: the lost clusters freed, or said to be.
    fn free(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let lost = counted(p, count::LOST);
        let fix = fixing(p);
        if fix {
            let volume = volume(p);
            cells::free_lost(&mut held(p), volume);
            set(p, flag::DIRTY);
        } else {
            set_count(p, count::UNRECOVERED, lost);
        }
        set_count(p, count::LOST, 0);
        let bytes = lost.saturating_mul(cluster_bytes(p));
        let words: &[u8] = if fix {
            b" bytes disk space freed\r\n"
        } else {
            b" bytes disk space would be freed\r\n"
        };
        *phase = phase::SAID;
        Flow::Call(say(p, &[&decimal_in(bytes, 10)[..], words].concat()))
    }

    /// The answer was yes: each chain made a file in the root, or its slot
    /// found for it, from the first chain's.
    fn recover(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        p.set16(at::ROOT_INDEX, 0);
        let chains = counted(p, count::CHAINS);
        p.set32(at::LEFT, chains);
        let volume = volume(p);
        let head = cells::next_head(&mut held(p), volume, 2).unwrap_or(0);
        p.set16(at::HEAD, head);
        if !fixing(p) {
            return Self::scan(p, phase);
        }
        set(p, flag::RECOVERED);
        *phase = phase::TIME;
        Flow::Call(Frame::of(&Int::dos(p.own, 0x2C00)))
    }

    /// The root has no slot for the chains left: they stay lost, counted
    /// neither as files made nor as free.
    fn root_full(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let volume = volume(p);
        let (mut head, mut unplaced, mut chains) = (p.get16(at::HEAD), 0u32, 0u32);
        while head != 0 {
            unplaced += cells::chain_length(&mut held(p), volume, head);
            chains += 1;
            head = cells::next_head(&mut held(p), volume, head.saturating_add(1)).unwrap_or(0);
        }
        let lost = counted(p, count::LOST);
        set_count(p, count::LOST, lost.saturating_sub(unplaced));
        add(p, count::UNRECOVERED, unplaced);
        let made = counted(p, count::CHAINS).saturating_sub(chains);
        set_count(p, count::CHAINS, made);
        set_asciz(p, at::NAME, b"", 13);
        *phase = phase::SAID;
        Flow::Call(Frame::of(&Tell::of(super::tree::said::ROOT_FULL)))
    }
}

impl Routine for ChkdskLost {
    const ID: Id = Id::ChkdskLost;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                *phase = phase::ERRORS_SAID;
                if !has(p, flag::ERRORS_SAID) {
                    set(p, flag::ERRORS_SAID);
                    if !switched(p, switch::FIX) {
                        return Flow::Call(say(p, text::ERRORS));
                    }
                }
                self.step(p, phase, Ok(0))
            }
            phase::ERRORS_SAID => {
                take!(child);
                *phase = phase::BLANK;
                Flow::Call(say(p, text::NEW_LINE))
            }
            phase::BLANK => {
                take!(child);
                let volume = volume(p);
                let fix = fixing(p);
                let (chains, changed) = cells::gather(&mut held(p), volume, fix);
                if changed {
                    set(p, flag::DIRTY);
                }
                set_count(p, count::CHAINS, chains);
                let lost = counted(p, count::LOST);
                let line = [
                    &b"   "[..],
                    &decimal(lost),
                    b" lost allocation units found in ",
                    &decimal(chains),
                    b" chains.\r\n",
                ]
                .concat();
                *phase = phase::COUNTED;
                Flow::Call(say_error(p, &line))
            }
            phase::COUNTED => {
                take!(child);
                *phase = phase::ASKED;
                Flow::Call(Frame::of(&Ask::of(super::question::LOST)))
            }
            phase::ASKED if take!(child) == 0 => Self::free(p, phase),
            phase::ASKED => Self::recover(p, phase),
            phase::TIME => {
                take!(child);
                super::keep_clock(p);
                *phase = phase::DATE;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x2A00)))
            }
            phase::DATE => {
                take!(child);
                let (cx, dx) = (p.result(Returned::Cx), p.result(Returned::Dx));
                let stamp = super::now(p, cx, dx);
                p.set32(at::STAMP, stamp);
                Self::scan(p, phase)
            }
            phase::SECTOR => {
                if failed(p) {
                    p.set32(at::BUFFERED, u32::MAX);
                    *phase = phase::SAID;
                    return Flow::Call(say_error(p, text::READ_FAULT));
                }
                let per_sector = p.get16(at::SECTOR_BYTES) / 32;
                let sector = u32::from(p.get16(at::ROOT_SECTOR))
                    + u32::from(p.get16(at::ROOT_INDEX) / per_sector);
                p.set32(at::BUFFERED, sector);
                Self::scan(p, phase)
            }
            phase::PROBED => {
                if child.is_ok() {
                    Self::next_name(p);
                    return Self::probe(p, phase);
                }
                Self::make(p, phase)
            }
            phase::WRITTEN => {
                if failed(p) {
                    p.set32(at::BUFFERED, u32::MAX);
                    *phase = phase::SAID;
                    return Flow::Call(say_error(p, text::WRITE_FAULT));
                }
                match Self::placed(p) {
                    Some(flow) => flow,
                    None => Self::scan(p, phase),
                }
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}
