//! `FC /B`: the two files read side by side, each byte that differs shown
//! at its offset as `00000000: 41 42`; then whichever is longer said, or,
//! with nothing different, that there was nothing.

use super::{at, file_at, have_read, name, read_into, text, DIFFERENT};
use crate::dos::message::{hex, NEW_LINE};
use crate::dos::program::{say, Flow, Id, Program, Routine};
use crate::frames::{locals, take, Outcome};
use alloc::vec::Vec;

/// The differences one write takes: as many lines as fit the scratch.
const PER_WRITE: usize = 14;
const LINE: usize = 17;

locals! {
    /// The byte compare: the offset reached, and whether a byte has differed.
    pub struct FcBytes {
        position: u32,
        differed: bool,
        reading: u8,
        code: u8,
    }
}

mod phase {
    pub const READ: u8 = 1;
    pub const SAID: u8 = 2;
    pub const ENDED: u8 = 3;
}

/// A file's unread bytes, and whether it has ended.
fn unread(p: &mut Program<'_, '_>, file: u16) -> (u16, u16, bool) {
    let base = file_at(file);
    (
        p.get16(base + at::AT),
        p.get16(base + at::END),
        p.get8(base + at::DONE) != 0,
    )
}

/// The line one differing byte is shown by.
fn difference(position: u32, first: u8, second: u8) -> [u8; LINE] {
    let mut line = *b"00000000: 00 00\r\n";
    hex(position, &mut line[..8]);
    hex(u32::from(first), &mut line[10..12]);
    hex(u32::from(second), &mut line[13..15]);
    line
}

impl FcBytes {
    /// Compares what both buffers hold, then reads on whichever ran out, or
    /// ends once one file has.
    fn compare(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let ((at1, end1, done1), (at2, end2, done2)) = (unread(p, 0), unread(p, 1));
        let both = (end1 - at1).min(end2 - at2);
        if both > 0 {
            let mut first = alloc::vec![0u8; usize::from(both)];
            let mut second = alloc::vec![0u8; usize::from(both)];
            p.g.read_bytes(p.own, file_at(0) + at::BUFFER + at1, &mut first);
            p.g.read_bytes(p.own, file_at(1) + at::BUFFER + at2, &mut second);
            let mut said = Vec::new();
            let mut taken = 0u16;
            for (&a, &b) in first.iter().zip(&second) {
                taken += 1;
                if a != b {
                    self.differed = true;
                    said.extend_from_slice(&difference(self.position, a, b));
                }
                self.position = self.position.wrapping_add(1);
                if said.len() >= PER_WRITE * LINE {
                    break;
                }
            }
            p.set16(file_at(0) + at::AT, at1 + taken);
            p.set16(file_at(1) + at::AT, at2 + taken);
            if !said.is_empty() {
                *phase = phase::SAID;
                return Flow::Call(say(p, &said));
            }
            return self.compare(p, phase);
        }
        for (file, (at, end, done)) in [(0u16, (at1, end1, done1)), (1, (at2, end2, done2))] {
            if at == end && !done {
                self.reading = u8::try_from(file).unwrap_or(0);
                *phase = phase::READ;
                return Flow::Call(read_into(p, file));
            }
        }
        self.code = u8::from(self.differed || at1 != end1 || at2 != end2);
        let said = match (at1 == end1, at2 == end2) {
            (true, true) if !self.differed => text::SAME.to_vec(),
            (true, true) => return Flow::Return(Ok(DIFFERENT)),
            (first_ended, _) => {
                let (longer, shorter) = if first_ended { (1, 0) } else { (0, 1) };
                [
                    &b"fc: "[..],
                    &name(p, longer),
                    b" longer than ",
                    &name(p, shorter),
                    NEW_LINE,
                ]
                .concat()
            }
        };
        *phase = phase::ENDED;
        Flow::Call(say(p, &said))
    }
}

impl Routine for FcBytes {
    const ID: Id = Id::FcBytes;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            phase::READ => {
                let read = take!(child);
                have_read(p, u16::from(self.reading), read);
                self.compare(p, phase)
            }
            phase::SAID | 0 => {
                take!(child);
                self.compare(p, phase)
            }
            _ => {
                take!(child);
                Flow::Return(Ok(u32::from(self.code)))
            }
        }
    }
}
