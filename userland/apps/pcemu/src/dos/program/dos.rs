//! The calls a program makes to DOS and the BIOS: one interrupt with the
//! registers given, the text it writes to the standard output, and the keys
//! and lines it reads knowing whether the standard input has ended.

use super::{at, Flow, Frame, Id, Program, Returned, Routine};
use crate::asm::Reg16;
use crate::cpu::{flag, Seg};
use crate::dos::char::CTRL_Z;
use crate::dos::device::DEVICE_ERRORS;
use crate::dos::engine::Saved;
use crate::dos::file::info;
use crate::dos::fs::error;
use crate::frames::{locals, Far, Fault, Outcome};

locals! {
    /// One interrupt taken with these registers: AX as it returns, or, from
    /// DOS, the error it holds when the carry comes back set. Every register
    /// the call returns is kept for the caller to read through
    /// [`Program::result`]. A device's error is asked about through function
    /// `59h`, whose registers are then the ones kept: one a critical-error
    /// handler failed comes back as `FAIL_ON_24`, whoever's handler it was.
    pub struct Int {
        vector: u8,
        ax: u16,
        bx: u16,
        cx: u16,
        dx: u16,
        si: u16,
        di: u16,
        bp: u16,
        ds: u16,
        es: u16,
        target: Far,
    }
}

impl Int {
    /// DOS function `ax`, DS and ES the program's own segment `own`, the
    /// other registers zero.
    #[must_use]
    pub fn dos(own: u16, ax: u16) -> Self {
        Self::bios(own, 0x21, ax)
    }

    /// INT `vector` with AX `ax`, DS and ES `own`, the rest zero.
    #[must_use]
    pub fn bios(own: u16, vector: u8, ax: u16) -> Self {
        Self {
            vector,
            ax,
            ds: own,
            es: own,
            ..Self::default()
        }
    }

    /// The same, taken to `target`, the handler vector `vector` held
    /// before the program's own: the interrupt the program's handler chains
    /// to.
    #[must_use]
    pub fn chained(own: u16, vector: u8, ax: u16, target: Far) -> Self {
        Self {
            target,
            ..Self::bios(own, vector, ax)
        }
    }

    /// From a handler: the interrupt taken on to `target` with the
    /// registers the handler's caller gave, AX `ax`.
    #[must_use]
    pub fn passed_on(p: &mut Program<'_, '_>, vector: u8, target: Far, ax: u16) -> Self {
        Self {
            bx: p.caller(Saved::Bx),
            cx: p.caller(Saved::Cx),
            dx: p.caller(Saved::Dx),
            si: p.caller(Saved::Si),
            di: p.caller(Saved::Di),
            bp: p.caller(Saved::Bp),
            ds: p.caller(Saved::Ds),
            es: p.caller(Saved::Es),
            ..Self::chained(p.own, vector, ax, target)
        }
    }
}

const CALLED: u8 = 1;
const ASKED: u8 = 2;

impl Routine for Int {
    const ID: Id = Id::Int;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, _child: Outcome) -> Flow {
        match *phase {
            CALLED => {
                let ax = p.result(Returned::Ax);
                let failed = self.vector == 0x21 && p.result(Returned::Flags) & flag::CF != 0;
                if !failed {
                    return Flow::Return(Ok(u32::from(ax)));
                }
                // Only a device's error can have been raised as a critical
                // error, so no other failure costs a call.
                if !DEVICE_ERRORS.contains(&ax) {
                    return Flow::Return(Err(Fault::Error(ax)));
                }
                self.ax = ax;
                *phase = ASKED;
                p.g.set_r16(Reg16::Ax, 0x5900);
                p.g.set_r16(Reg16::Bx, 0);
                return p.int(0x21);
            }
            ASKED => {
                let failed = p.result(Returned::Ax) == error::FAIL_ON_24;
                return Flow::Return(Err(Fault::Error(if failed {
                    error::FAIL_ON_24
                } else {
                    self.ax
                })));
            }
            _ => {}
        }
        for (reg, value) in [
            (Reg16::Ax, self.ax),
            (Reg16::Bx, self.bx),
            (Reg16::Cx, self.cx),
            (Reg16::Dx, self.dx),
            (Reg16::Si, self.si),
            (Reg16::Di, self.di),
            (Reg16::Bp, self.bp),
        ] {
            p.g.set_r16(reg, value);
        }
        p.g.regs_mut().set_sreg(Seg::Ds, self.ds);
        p.g.regs_mut().set_sreg(Seg::Es, self.es);
        // A function that reports no error leaves the carry as it found it.
        p.g.regs_mut().set_flag(flag::CF, false);
        *phase = CALLED;
        if self.target != Far::default() {
            return p.int_at(self.target);
        }
        p.int(self.vector)
    }
}

/// From a handler: the registers the call it passed on returned given to
/// its caller.
pub fn hand_back(p: &mut Program<'_, '_>) {
    for (saved, returned) in [
        (Saved::Ax, Returned::Ax),
        (Saved::Bx, Returned::Bx),
        (Saved::Cx, Returned::Cx),
        (Saved::Dx, Returned::Dx),
        (Saved::Si, Returned::Si),
        (Saved::Es, Returned::Es),
    ] {
        let value = p.result(returned);
        p.set_caller(saved, value);
    }
}

/// The call writing `text` to the standard output, from the running root's
/// scratch; text past the scratch's length is not written.
pub fn say(p: &mut Program<'_, '_>, text: &[u8]) -> Frame {
    say_on(p, 1, text)
}

/// The call writing `text` to the standard error, as [`say`] writes.
pub fn say_error(p: &mut Program<'_, '_>, text: &[u8]) -> Frame {
    say_on(p, 2, text)
}

fn say_on(p: &mut Program<'_, '_>, handle: u16, text: &[u8]) -> Frame {
    let len = text.len().min(usize::from(at::SCRATCH_LEN));
    let at = p.scratch(0);
    p.g.write_bytes(p.own, at, &text[..len]);
    Frame::of(&Int {
        bx: handle,
        cx: u16::try_from(len).unwrap_or(0),
        dx: at,
        ..Int::dos(p.own, 0x4000)
    })
}

/// The call writing `text` through function `09h`, which a critical-error
/// handler may make; `text` holds no `$`, which ends it.
pub fn say_console(p: &mut Program<'_, '_>, text: &[u8]) -> Frame {
    let len = text.len().min(usize::from(at::SCRATCH_LEN) - 1);
    let at = p.scratch(0);
    p.g.write_bytes(p.own, at, &text[..len]);
    p.set8(at + u16::try_from(len).unwrap_or(0), b'$');
    Frame::of(&Int {
        dx: at,
        ..Int::dos(p.own, 0x0900)
    })
}

/// What [`Key`] and [`Line`] give once the standard input has ended: no
/// byte's value and no line's length, so nothing a user could type.
pub const ENDED: u32 = 0x100;

locals! {
    /// A key read through DOS function `ax` — `01h`, `07h` or `08h`, or
    /// `0Ch` with one of them: the key, or [`ENDED`] when none can come — a
    /// read that failed, or a Ctrl+Z from a standard input that is no
    /// console, which is how a redirected input ends. A question asked until
    /// it is answered reads through this, so an input with no answers left
    /// cannot keep it asking.
    pub struct Key {
        ax: u16,
    }
}

impl Key {
    /// The key function `ax` reads.
    #[must_use]
    pub const fn of(ax: u16) -> Self {
        Self { ax }
    }
}

mod key {
    pub const READ: u8 = 1;
    pub const ASKED: u8 = 2;
}

impl Routine for Key {
    const ID: Id = Id::Key;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            key::READ => {
                let key = match child {
                    Ok(key) => key & 0xFF,
                    Err(fault) => return Flow::Return(unread(fault)),
                };
                if key != u32::from(CTRL_Z) {
                    return Flow::Return(Ok(key));
                }
                *phase = key::ASKED;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x4400)))
            }
            key::ASKED => Flow::Return(Ok(if console(p, child) {
                u32::from(CTRL_Z)
            } else {
                ENDED
            })),
            _ => {
                *phase = key::READ;
                Flow::Call(Frame::of(&Int::dos(p.own, self.ax)))
            }
        }
    }
}

/// The key a [`Key`] read gave a question, upper-cased, as an answer: `N`
/// once the input has ended, so having no answer left answers no.
#[must_use]
pub fn answer_key(read: u32) -> u8 {
    if read == ENDED {
        b'N'
    } else {
        read.to_le_bytes()[0].to_ascii_uppercase()
    }
}

locals! {
    /// A line read through function `0Ah` into the buffer at `buffer`, its
    /// room already in its first byte: the bytes typed, or [`ENDED`] when
    /// none can come — a read that failed, or the empty line a standard
    /// input that is no console gives at its end: a device's, or a file's
    /// with nothing left in it, since a file's blank line is a line.
    pub struct Line {
        buffer: u16,
    }
}

impl Line {
    /// The line read into the buffer at `buffer`.
    #[must_use]
    pub const fn at(buffer: u16) -> Self {
        Self { buffer }
    }
}

mod line {
    pub const READ: u8 = 1;
    pub const ASKED: u8 = 2;
    pub const LEFT: u8 = 3;
}

impl Routine for Line {
    const ID: Id = Id::Line;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            line::READ => {
                if let Err(fault) = child {
                    return Flow::Return(unread(fault));
                }
                let typed = p.get8(self.buffer + 1);
                if typed != 0 {
                    return Flow::Return(Ok(u32::from(typed)));
                }
                *phase = line::ASKED;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x4400)))
            }
            line::ASKED => {
                if console(p, child) {
                    return Flow::Return(Ok(0));
                }
                if child.is_err() || p.result(Returned::Dx) & info::DEVICE != 0 {
                    return Flow::Return(Ok(ENDED));
                }
                *phase = line::LEFT;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x4406)))
            }
            line::LEFT => {
                let left = child.is_ok_and(|status| status & 0xFF == 0xFF);
                Flow::Return(Ok(if left { 0 } else { ENDED }))
            }
            _ => {
                *phase = line::READ;
                Flow::Call(Frame::of(&Int {
                    dx: self.buffer,
                    ..Int::dos(p.own, 0x0A00)
                }))
            }
        }
    }
}

/// What a read that met `fault` gives: [`ENDED`] for a failure to read; a
/// break or an abort is the program's to take.
fn unread(fault: Fault) -> Outcome {
    match fault {
        Fault::Error(_) => Ok(ENDED),
        kept => Err(kept),
    }
}

/// Whether function `4400h`'s `outcome` found the standard input the
/// console's.
fn console(p: &mut Program<'_, '_>, outcome: Outcome) -> bool {
    outcome.is_ok() && info::console_input(p.result(Returned::Dx))
}
