//! The resident copy's handlers. INT 21h's function `0Ah` is edited by
//! `DOSKEY` while the standard input is the console. INT 2Fh's `4800h`
//! answers that it is installed, and `4810h` reads a command line with its
//! macros expanded, the commands a `$T` divides it into given at the calls
//! that follow. A Ctrl+C typed is put back at the head of the keyboard's
//! buffer for DOS's own line input to meet, so it breaks as it would.

use super::edit::{ended, give, DosKeyEdit, DosKeyWrite};
use super::{at, load, multiplex};
use crate::bios::{bda, Ring, CTRL_C_KEY};
use crate::dos::char::CR;
use crate::dos::engine::Saved;
use crate::dos::file::info;
use crate::dos::program::{hand_back, Flow, Frame, Id, Int, Program, Returned, Routine};
use crate::frames::{locals, take, Far, Outcome};
use alloc::vec::Vec;

/// The call asking whether the standard input is the console.
fn ask_input(p: &Program<'_, '_>) -> Frame {
    Frame::of(&Int::dos(p.own, 0x4400))
}

fn console(p: &mut Program<'_, '_>, child: Outcome) -> bool {
    child.is_ok() && info::console_input(p.result(Returned::Dx))
}

/// The caller's line buffer, DS:DX.
fn callers_buffer(p: &mut Program<'_, '_>) -> Far {
    Far::new(p.caller(Saved::Ds), p.caller(Saved::Dx))
}

/// Ctrl+C put back at the head of the keyboard's buffer, read next; not
/// where the buffer is full.
fn unread_break(p: &mut Program<'_, '_>) {
    let ring = Ring::read(p.g);
    if !ring.is_sound() {
        return;
    }
    let before = ring.before(ring.head);
    if before != ring.tail {
        p.g.write16(bda::SEGMENT, before, CTRL_C_KEY);
        p.g.write16(bda::SEGMENT, bda::KEY_HEAD, before);
    }
}

/// The call passing the caller's call on to the handler at `old`.
fn pass_on(p: &mut Program<'_, '_>, vector: u8, old: u16) -> Frame {
    let target = p.get_far(old);
    let ax = p.caller(Saved::Ax);
    Frame::of(&Int::passed_on(p, vector, target, ax))
}

locals! {
    /// INT 21h function `0Ah`: the line edited, or, from no console, the
    /// call passed on.
    pub struct DosKey21 {}
}

mod buffered {
    pub const CHECKED: u8 = 1;
    pub const EDITED: u8 = 2;
    pub const PASSED: u8 = 3;
}

impl Routine for DosKey21 {
    const ID: Id = Id::DosKey21;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            buffered::CHECKED => {
                if !console(p, child) {
                    *phase = buffered::PASSED;
                    return Flow::Call(pass_on(p, 0x21, at::OLD_21));
                }
                *phase = buffered::EDITED;
                let buffer = callers_buffer(p);
                Flow::Call(Frame::of(&DosKeyEdit::of(buffer)))
            }
            buffered::EDITED => {
                if take!(child) != ended::BREAK {
                    return Flow::Return(Ok(0));
                }
                unread_break(p);
                *phase = buffered::PASSED;
                Flow::Call(pass_on(p, 0x21, at::OLD_21))
            }
            buffered::PASSED => {
                take!(child);
                hand_back(p);
                Flow::Return(Ok(0))
            }
            _ => {
                *phase = buffered::CHECKED;
                Flow::Call(ask_input(p))
            }
        }
    }
}

/// The next command a `$T` left, taken from the queue.
fn dequeue(p: &mut Program<'_, '_>) -> Option<Vec<u8>> {
    let len = p.get16(at::QUEUED_LEN).min(at::QUEUED_ROOM);
    if len == 0 {
        return None;
    }
    let mut queued = alloc::vec![0u8; usize::from(len)];
    p.g.read_bytes(p.own, at::QUEUED, &mut queued);
    let end = queued
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(queued.len());
    let rest = queued.get(end + 1..).unwrap_or_default().to_vec();
    p.g.write_bytes(p.own, at::QUEUED, &rest);
    p.set16(at::QUEUED_LEN, u16::try_from(rest.len()).unwrap_or(0));
    Some(queued[..end].to_vec())
}

/// `commands` queued for the calls that follow, as many as the queue holds.
fn enqueue(p: &mut Program<'_, '_>, commands: &[Vec<u8>]) {
    let mut queued = Vec::new();
    for command in commands {
        if queued.len() + command.len() + 1 > usize::from(at::QUEUED_ROOM) {
            break;
        }
        queued.extend_from_slice(command);
        queued.push(0);
    }
    p.g.write_bytes(p.own, at::QUEUED, &queued);
    p.set16(at::QUEUED_LEN, u16::try_from(queued.len()).unwrap_or(0));
}

/// The line the caller's buffer holds.
fn given(p: &mut Program<'_, '_>, buffer: Far) -> Vec<u8> {
    let len = p.g.read8(buffer.segment, buffer.offset.wrapping_add(1));
    let mut line = alloc::vec![0u8; usize::from(len)];
    p.g.read_bytes(buffer.segment, buffer.offset.wrapping_add(2), &mut line);
    line
}

locals! {
    /// INT 2Fh function `48h`: `4800h`, the installation check, answered
    /// with AL `FFh` and the resident copy's segment in ES; `4810h`, a
    /// command line read with its macros expanded; any other passed on.
    pub struct DosKey2F {}
}

mod multiplexed {
    pub const CHECKED: u8 = 1;
    pub const EDITED: u8 = 2;
    pub const ECHOED: u8 = 3;
    pub const PASSED: u8 = 4;
}

impl DosKey2F {
    fn answer(p: &mut Program<'_, '_>, ax: u16) -> Flow {
        p.set_caller(Saved::Ax, ax);
        Flow::Return(Ok(0))
    }
}

impl Routine for DosKey2F {
    const ID: Id = Id::DosKey2F;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            multiplexed::CHECKED => {
                if !console(p, child) {
                    return Self::answer(p, 1);
                }
                let buffer = callers_buffer(p);
                let Some(command) = dequeue(p) else {
                    *phase = multiplexed::EDITED;
                    return Flow::Call(Frame::of(&DosKeyEdit::of(buffer)));
                };
                let len = give(p, buffer, &command);
                let shown = [&command[..usize::from(len)], &[CR]].concat();
                p.g.write_bytes(p.own, at::TEXT, &shown);
                let page = p.g.read8(bda::SEGMENT, bda::ACTIVE_PAGE);
                *phase = multiplexed::ECHOED;
                Flow::Call(Frame::of(&DosKeyWrite::of(at::TEXT, len + 1, 0, page)))
            }
            multiplexed::EDITED => {
                if take!(child) == ended::BREAK {
                    unread_break(p);
                    return Self::answer(p, 1);
                }
                let buffer = callers_buffer(p);
                let line = given(p, buffer);
                if let Some(commands) = load(p, p.own).expand(&line) {
                    let (first, rest) = commands
                        .split_first()
                        .map_or((&[][..], &[][..]), |(first, rest)| (&first[..], rest));
                    give(p, buffer, first);
                    enqueue(p, rest);
                }
                Self::answer(p, 0)
            }
            multiplexed::ECHOED => {
                take!(child);
                Self::answer(p, 0)
            }
            multiplexed::PASSED => {
                take!(child);
                hand_back(p);
                Flow::Return(Ok(0))
            }
            _ => match p.caller(Saved::Ax).to_le_bytes()[0] {
                0x00 => {
                    p.set_caller(Saved::Es, p.own);
                    Self::answer(p, u16::from_le_bytes([multiplex::INSTALLED, multiplex::AH]))
                }
                multiplex::READ => {
                    *phase = multiplexed::CHECKED;
                    Flow::Call(ask_input(p))
                }
                _ => {
                    *phase = multiplexed::PASSED;
                    Flow::Call(pass_on(p, 0x2F, at::OLD_2F))
                }
            },
        }
    }
}
