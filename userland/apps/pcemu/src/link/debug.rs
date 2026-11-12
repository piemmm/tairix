//! The debugger's part of the session (`plans/PCEMU.md` §3.1).
//!
//! The front-end puts the probe on the board ([`Debug::Attach`]), adds and
//! removes its breakpoints one at a time, stops the processor and sends it
//! on, and asks what its window shows ([`Look`]). Every request is a few
//! bytes, as all credited input is. The worker says why the
//! processor stopped and how it stands ([`Said::Stopped`]) and answers each
//! look ([`Said::Seen`]): the registers, the 8087's values, a listing and
//! memory are read and written out there, so the front-end decodes nothing
//! a guest controls.

use alloc::vec::Vec;

use tairix_inline::ArrayString;
use tairix_sandbox::wire::{Reader, Room, WireError, Writer, BYTES_PREFIX};

use super::{flag, nth, number, tag};
use crate::cpu::Regs;
use crate::debug::disasm::{MOST_BYTES, MOST_TEXT};
use crate::debug::probe::{Access, Breakpoint, Cycle, Direction, Go, Hit, Master, Reason, SPACE};
use crate::debug::{Activity, Coprocessor, Registers, MOST_VALUE};

/// Lines a look lists before its code address at most.
pub const MOST_BEFORE: u8 = 16;
/// Lines it lists from there at most.
pub const MOST_AFTER: u8 = 48;
/// Bytes of memory a look reads at most.
pub const MOST_MEMORY: u16 = 2048;
/// Words a look reads from the stack at most.
pub const MOST_STACK: u8 = 64;
/// Bytes a listing's lines take at most as they cross: each its offset, its
/// bytes and its text.
const MOST_LISTING: usize =
    (MOST_BEFORE as usize + MOST_AFTER as usize) * (2 + 2 * BYTES_PREFIX + MOST_BYTES + MOST_TEXT);

/// Bytes the longest report takes after its event's tag: a look's answer
/// at its fullest.
pub(super) const MOST_SAID: usize = 7
    + BYTES_PREFIX
    + MOST_LISTING
    + BYTES_PREFIX
    + MOST_MEMORY as usize
    + BYTES_PREFIX
    + 2 * MOST_STACK as usize;

/// What the front-end asks of the debugger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Debug {
    /// Put the probe on the board: the debugger's window opened.
    Attach,
    /// Take it off, the processor going on from any stop.
    Detach,
    /// Hold this breakpoint too.
    Add(Breakpoint),
    /// Hold this one no more.
    Remove(Breakpoint),
    /// Stop the running processor at its next boundary.
    Break,
    /// Send the stopped processor on.
    Go(Go),
    /// Read what the window shows.
    Look(Look),
}

/// What the debugger's window asks to be shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    /// Its number, which the answer carries.
    pub number: u32,
    /// Where code is listed from: CS and IP.
    pub code: (u16, u16),
    /// Lines listed before it, at most [`MOST_BEFORE`], and from it, at most
    /// [`MOST_AFTER`].
    pub lines: (u8, u8),
    /// Where memory is read from: a segment and an offset in it.
    pub memory: (u16, u16),
    /// Bytes read there, at most [`MOST_MEMORY`].
    pub bytes: u16,
    /// Words read from SS:SP, at most [`MOST_STACK`].
    pub stack: u8,
}

/// What the worker tells of the debugger.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "a report is decoded onto the stack one at a time and never kept in bulk"
)]
pub enum Said<'a> {
    /// The processor stopped.
    Stopped {
        /// Why.
        reason: Reason,
        /// How it stands.
        registers: Registers,
    },
    /// The answer to a look.
    Seen(Seen<'a>),
    /// A reset was taken with the probe on: any stop made before it is
    /// gone, and the machine runs.
    Reset,
    /// The probe is off the board, as the front-end asked: every report
    /// before this was of the debugger taken off.
    Detached,
}

/// What a look asked for, read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seen<'a> {
    /// The look's number.
    pub number: u32,
    /// The listing: the lines found before the code address, then those
    /// from it.
    pub listing: Listing<'a>,
    /// The memory asked for, as many bytes as asked.
    pub memory: &'a [u8],
    /// The stack's words from SS:SP, as many as asked.
    pub stack: &'a [u8],
}

/// A listing's lines as they cross, each checked as it was decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Listing<'a> {
    /// The lines before the code address.
    pub before: u8,
    /// The lines from it.
    pub after: u8,
    encoded: &'a [u8],
}

/// One line of a listing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line<'a> {
    /// Its offset in the code segment.
    pub ip: u16,
    /// Its bytes.
    pub bytes: &'a [u8],
    /// What they say: printable ASCII.
    pub text: &'a str,
}

impl<'a> Listing<'a> {
    /// `lines` as they cross, the first `before` of them ahead of the code
    /// address, the rest from it: none past [`MOST_BEFORE`] and
    /// [`MOST_AFTER`].
    #[must_use]
    pub fn encode(lines: &[crate::debug::disasm::Line], before: usize) -> (u8, u8, Vec<u8>) {
        let before = before.min(lines.len()).min(usize::from(MOST_BEFORE));
        let after = (lines.len() - before).min(usize::from(MOST_AFTER));
        let mut w = Writer::new();
        for line in &lines[..before + after] {
            w.u16(line.ip);
            w.bytes(&line.bytes);
            w.str(&line.text);
        }
        let count = |n: usize| u8::try_from(n).unwrap_or(u8::MAX);
        (count(before), count(after), w.finish())
    }

    /// The listing of `before` and `after` lines `encoded` holds.
    #[must_use]
    pub const fn new(before: u8, after: u8, encoded: &'a [u8]) -> Self {
        Self {
            before,
            after,
            encoded,
        }
    }

    /// The lines as they cross.
    #[must_use]
    pub const fn encoded(&self) -> &'a [u8] {
        self.encoded
    }

    /// The lines, in order.
    pub fn lines(&self) -> impl Iterator<Item = Line<'a>> + 'a {
        let mut r = Reader::new(self.encoded);
        (0..self.before + self.after).map_while(move |_| line(&mut r).ok())
    }

    /// `encoded`, checked to hold `before` and then `after` lines and
    /// nothing else, each line beginning where the one before ends.
    fn decode(before: u8, after: u8, encoded: &'a [u8]) -> Result<Self, WireError> {
        if before > MOST_BEFORE || after > MOST_AFTER {
            return Err(WireError::Malformed);
        }
        let mut r = Reader::new(encoded);
        let mut next: Option<u16> = None;
        for _ in 0..before + after {
            let line = line(&mut r)?;
            if next.is_some_and(|ip| ip != line.ip) {
                return Err(WireError::Malformed);
            }
            let len = u16::try_from(line.bytes.len()).map_err(|_| WireError::Malformed)?;
            next = Some(line.ip.wrapping_add(len));
        }
        if !r.is_exhausted() {
            return Err(WireError::Malformed);
        }
        Ok(Self::new(before, after, encoded))
    }
}

/// A line's text: printable ASCII, as the listing writes it.
fn printable(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| (0x20..0x7F).contains(&b))
}

fn line<'a>(r: &mut Reader<'a>) -> Result<Line<'a>, WireError> {
    let ip = r.u16()?;
    let bytes = r.bytes(MOST_BYTES)?;
    let text = r.str(MOST_TEXT)?;
    if bytes.is_empty() || !printable(text) {
        return Err(WireError::Malformed);
    }
    Ok(Line { ip, bytes, text })
}

const ACCESSES: [Access; 2] = [Access::Write, Access::Any];
const DIRECTIONS: [Direction; 3] = [Direction::In, Direction::Out, Direction::Both];
const MASTERS: [Master; 3] = [Master::Cpu, Master::Coprocessor, Master::Dma];
const CYCLES: [Cycle; 4] = [Cycle::Read, Cycle::Write, Cycle::In, Cycle::Out];
const GOES: [Go; 4] = [Go::Continue, Go::Step, Go::StepOver, Go::StepOut];
const ACTIVITIES: [Activity; 3] = [Activity::Executing, Activity::Halted, Activity::Waiting];

mod kind {
    pub const ATTACH: u8 = 0;
    pub const DETACH: u8 = 1;
    pub const ADD: u8 = 2;
    pub const REMOVE: u8 = 3;
    pub const BREAK: u8 = 4;
    pub const GO: u8 = 5;
    pub const LOOK: u8 = 6;

    pub const STOPPED: u8 = 0;
    pub const SEEN: u8 = 1;
    pub const RESET: u8 = 2;
    pub const DETACHED: u8 = 3;
}

fn put_breakpoint<R: Room>(w: &mut Writer<R>, breakpoint: &Breakpoint) {
    match *breakpoint {
        Breakpoint::Code(addr) => {
            w.u8(0);
            w.u32(addr);
        }
        Breakpoint::Memory { first, len, access } => {
            w.u8(1);
            w.u32(first);
            w.u32(len);
            w.u8(number(&ACCESSES, &access));
        }
        Breakpoint::Port {
            first,
            last,
            direction,
        } => {
            w.u8(2);
            w.u16(first);
            w.u16(last);
            w.u8(number(&DIRECTIONS, &direction));
        }
        Breakpoint::Interrupt { vector, ah } => {
            w.raw(&[3, vector, u8::from(ah.is_some()), ah.unwrap_or(0)]);
        }
    }
}

/// A breakpoint, valid and as an honest encoder writes it.
fn breakpoint(r: &mut Reader<'_>) -> Result<Breakpoint, WireError> {
    let breakpoint = match r.u8()? {
        0 => Breakpoint::Code(r.u32()?),
        1 => Breakpoint::Memory {
            first: r.u32()?,
            len: r.u32()?,
            access: nth(&ACCESSES, r.u8()?)?,
        },
        2 => Breakpoint::Port {
            first: r.u16()?,
            last: r.u16()?,
            direction: nth(&DIRECTIONS, r.u8()?)?,
        },
        3 => {
            let vector = r.u8()?;
            let given = flag(r)?;
            let ah = r.u8()?;
            if !given && ah != 0 {
                return Err(WireError::Malformed);
            }
            Breakpoint::Interrupt {
                vector,
                ah: given.then_some(ah),
            }
        }
        _ => return Err(WireError::Malformed),
    };
    if breakpoint.is_valid() {
        Ok(breakpoint)
    } else {
        Err(WireError::Malformed)
    }
}

impl Debug {
    /// Appends the request, its command's tag first.
    pub(super) fn encode<R: Room>(&self, w: &mut Writer<R>) {
        w.u8(tag::DEBUG);
        match self {
            Self::Attach => w.u8(kind::ATTACH),
            Self::Detach => w.u8(kind::DETACH),
            Self::Add(breakpoint) => {
                w.u8(kind::ADD);
                put_breakpoint(w, breakpoint);
            }
            Self::Remove(breakpoint) => {
                w.u8(kind::REMOVE);
                put_breakpoint(w, breakpoint);
            }
            Self::Break => w.u8(kind::BREAK),
            Self::Go(go) => w.raw(&[kind::GO, number(&GOES, go)]),
            Self::Look(look) => {
                w.u8(kind::LOOK);
                w.u32(look.number);
                w.u16(look.code.0);
                w.u16(look.code.1);
                w.raw(&[look.lines.0, look.lines.1]);
                w.u16(look.memory.0);
                w.u16(look.memory.1);
                w.u16(look.bytes);
                w.u8(look.stack);
            }
        }
    }

    /// The request after its command's tag.
    pub(super) fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok(match r.u8()? {
            kind::ATTACH => Self::Attach,
            kind::DETACH => Self::Detach,
            kind::ADD => Self::Add(breakpoint(r)?),
            kind::REMOVE => Self::Remove(breakpoint(r)?),
            kind::BREAK => Self::Break,
            kind::GO => Self::Go(nth(&GOES, r.u8()?)?),
            kind::LOOK => {
                let look = Look {
                    number: r.u32()?,
                    code: (r.u16()?, r.u16()?),
                    lines: (r.u8()?, r.u8()?),
                    memory: (r.u16()?, r.u16()?),
                    bytes: r.u16()?,
                    stack: r.u8()?,
                };
                if look.lines.0 > MOST_BEFORE
                    || look.lines.1 > MOST_AFTER
                    || look.bytes > MOST_MEMORY
                    || look.stack > MOST_STACK
                {
                    return Err(WireError::Malformed);
                }
                Self::Look(look)
            }
            _ => return Err(WireError::Malformed),
        })
    }
}

fn put_hit<R: Room>(w: &mut Writer<R>, hit: Hit) {
    w.raw(&[number(&MASTERS, &hit.master), number(&CYCLES, &hit.cycle)]);
    w.u32(hit.at);
    w.u8(hit.value);
}

/// A watch's hit, as the watch it is reported by could see it.
fn hit(r: &mut Reader<'_>, watch: &Breakpoint) -> Result<Hit, WireError> {
    let hit = Hit {
        master: nth(&MASTERS, r.u8()?)?,
        cycle: nth(&CYCLES, r.u8()?)?,
        at: r.u32()?,
        value: r.u8()?,
    };
    let seen = match *watch {
        Breakpoint::Memory { first, len, access } => {
            hit.at.wrapping_sub(first) < len
                && match hit.cycle {
                    Cycle::Write => true,
                    Cycle::Read => access == Access::Any,
                    Cycle::In | Cycle::Out => false,
                }
        }
        Breakpoint::Port {
            first,
            last,
            direction,
        } => {
            u16::try_from(hit.at).is_ok_and(|port| (first..=last).contains(&port))
                && matches!(
                    (direction, hit.cycle),
                    (Direction::Both, Cycle::In | Cycle::Out)
                        | (Direction::In, Cycle::In)
                        | (Direction::Out, Cycle::Out)
                )
        }
        Breakpoint::Code(_) | Breakpoint::Interrupt { .. } => false,
    };
    if seen {
        Ok(hit)
    } else {
        Err(WireError::Malformed)
    }
}

fn put_reason<R: Room>(w: &mut Writer<R>, reason: &Reason) {
    match reason {
        Reason::Break => w.u8(0),
        Reason::Stepped => w.u8(1),
        Reason::Breakpoint(breakpoint) => {
            w.u8(2);
            put_breakpoint(w, breakpoint);
        }
        Reason::Watched { watch, hit } => {
            w.u8(3);
            put_breakpoint(w, watch);
            put_hit(w, *hit);
        }
    }
}

fn reason(r: &mut Reader<'_>) -> Result<Reason, WireError> {
    Ok(match r.u8()? {
        0 => Reason::Break,
        1 => Reason::Stepped,
        2 => {
            let breakpoint = breakpoint(r)?;
            if !matches!(
                breakpoint,
                Breakpoint::Code(_) | Breakpoint::Interrupt { .. }
            ) {
                return Err(WireError::Malformed);
            }
            Reason::Breakpoint(breakpoint)
        }
        3 => {
            let watch = breakpoint(r)?;
            let hit = hit(r, &watch)?;
            Reason::Watched { watch, hit }
        }
        _ => return Err(WireError::Malformed),
    })
}

fn put_registers<R: Room>(w: &mut Writer<R>, registers: &Registers) {
    let regs = &registers.regs;
    for value in regs.gpr.iter().chain(&regs.seg) {
        w.u16(*value);
    }
    w.u16(regs.ip);
    w.u16(regs.flags);
    w.u64(registers.cycles);
    w.raw(&[
        number(&ACTIVITIES, &registers.activity),
        u8::from(registers.emulating),
        u8::from(registers.coprocessor.is_some()),
    ]);
    if let Some(fpu) = &registers.coprocessor {
        w.u16(fpu.control);
        w.u16(fpu.status);
        w.u16(fpu.tags);
        w.u32(fpu.ip);
        w.u16(fpu.opcode);
        w.u32(fpu.dp);
        for value in &fpu.stack {
            w.str(value);
        }
    }
}

fn registers(r: &mut Reader<'_>) -> Result<Registers, WireError> {
    let mut regs = Regs::default();
    for value in regs.gpr.iter_mut().chain(&mut regs.seg) {
        *value = r.u16()?;
    }
    regs.ip = r.u16()?;
    regs.flags = r.u16()?;
    let cycles = r.u64()?;
    let activity = nth(&ACTIVITIES, r.u8()?)?;
    let emulating = flag(r)?;
    let coprocessor = if flag(r)? {
        let mut fpu = Coprocessor {
            control: r.u16()?,
            status: r.u16()?,
            tags: r.u16()?,
            ip: r.u32()?,
            opcode: r.u16()?,
            dp: r.u32()?,
            stack: [ArrayString::new(); 8],
        };
        if fpu.ip >= SPACE || fpu.dp >= SPACE || fpu.opcode > 0x7FF {
            return Err(WireError::Malformed);
        }
        for value in &mut fpu.stack {
            let text = r.str(MOST_VALUE)?;
            if !printable(text) {
                return Err(WireError::Malformed);
            }
            *value = ArrayString::try_from(text).map_err(|_| WireError::Malformed)?;
        }
        Some(fpu)
    } else {
        None
    };
    Ok(Registers {
        regs,
        cycles,
        activity,
        emulating,
        coprocessor,
    })
}

impl Said<'_> {
    /// Appends the report, its event's tag first.
    pub(super) fn encode<R: Room>(&self, w: &mut Writer<R>) {
        w.u8(tag::SAID);
        match self {
            Self::Stopped { reason, registers } => {
                w.u8(kind::STOPPED);
                put_reason(w, reason);
                put_registers(w, registers);
            }
            Self::Seen(seen) => {
                w.u8(kind::SEEN);
                w.u32(seen.number);
                w.raw(&[seen.listing.before, seen.listing.after]);
                w.bytes(seen.listing.encoded);
                w.bytes(seen.memory);
                w.bytes(seen.stack);
            }
            Self::Reset => w.u8(kind::RESET),
            Self::Detached => w.u8(kind::DETACHED),
        }
    }
}

impl<'a> Said<'a> {
    /// The report after its event's tag.
    pub(super) fn decode(r: &mut Reader<'a>) -> Result<Self, WireError> {
        Ok(match r.u8()? {
            kind::STOPPED => Self::Stopped {
                reason: reason(r)?,
                registers: registers(r)?,
            },
            kind::SEEN => {
                let number = r.u32()?;
                let (before, after) = (r.u8()?, r.u8()?);
                let listing = Listing::decode(before, after, r.bytes(MOST_LISTING)?)?;
                Self::Seen(Seen {
                    number,
                    listing,
                    memory: r.bytes(usize::from(MOST_MEMORY))?,
                    stack: r.bytes(usize::from(MOST_STACK) * 2)?,
                })
            }
            kind::RESET => Self::Reset,
            kind::DETACHED => Self::Detached,
            _ => return Err(WireError::Malformed),
        })
    }
}

#[cfg(test)]
#[path = "debug_tests.rs"]
mod tests;
