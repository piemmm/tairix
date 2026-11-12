//! The calls programs make: INT 21h and the INT 20h, 25h, 26h, 27h and 2Fh
//! beside it. Each runs as a root whose caller's registers lie on the
//! caller's stack, as DOS leaves them; the answer is written there and the
//! ROM's exit pops it.

use super::char::{self, GetChar, InputStatus, LineInput, PutChar};
use super::data::{driver, level, sda, sft, sysvars, SDA, SDA_END, SEGMENT, SYSVARS};
use super::device::{attribute, command};
use super::dirops::{
    self, ChangeDir, CurrentDir, Delete, ExtendedOpen, Find, MakeDir, RemoveDir, Rename, TempFile,
};
use super::engine::{self, locals, Far, Fault, Flow, Frame, Id, Kernel, Outcome, Routine, Saved};
use super::exec::{self, ending, mode, Exec, Terminate};
use super::fcb::{self, FcbClose, FcbDelete, FcbIo, FcbOpen, FcbRename, FcbSearch, FcbSize};
use super::file::{access, info, Close, Commit, Create, Open, Read, Write};
use super::fs::{self, area, dpb_at, error, BlockIo, Flush, Free, Media, FREE};
use super::ioctl::Ioctl;
use super::memory::{psp, Refusal};
use super::redirect;
use super::rom::entry;
use crate::asm::Reg16;
use crate::cpu::{flag, Seg};

/// What made the call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// INT 21h.
    Int21,
    /// INT 20h: the end of the current program.
    Int20,
    /// INT 27h: the current program ends, staying resident.
    Int27,
    /// INT 25h: absolute disk read.
    Int25,
    /// INT 26h: absolute disk write.
    Int26,
    /// INT 2Fh: the multiplex interrupt, at the end of its chain.
    Int2F,
}

/// The OEM number function `30h` reports: TAIRiX DOS's own.
const OEM: u8 = 0x5A;
/// Function `30h` with AL `01h`, and `3306h`: DOS runs from ROM.
const IN_ROM: u8 = 0x08;
/// The version reported: 5.00.
const VERSION: u16 = 0x0005;
/// The function code the root runs INT 27h as.
const INT27: u8 = 0xFE;

/// Begins a call made through `entry`.
pub fn enter(k: &mut Kernel<'_, '_>, entry: Entry) {
    let caller = Far::new(k.g.seg(Seg::Ss), k.g.r16(Reg16::Sp));
    let [_, ah] = k.g.r16(Reg16::Ax).to_le_bytes();
    if matches!(entry, Entry::Int21 | Entry::Int20 | Entry::Int27) {
        let process = k.psp();
        k.g.write16(process, psp::STACK, caller.offset);
        k.g.write16(process, psp::STACK + 2, caller.segment);
    }
    let (root, exit) = match entry {
        Entry::Int25 | Entry::Int26 => (
            Frame::of(&Int25 {
                write: entry == Entry::Int26,
                drive: 0,
            }),
            entry::EXIT25,
        ),
        Entry::Int2F => (Frame::of(&Int2F { function: ah }), entry::EXIT),
        Entry::Int21 | Entry::Int20 | Entry::Int27 => {
            let function = match entry {
                Entry::Int20 => 0x00,
                Entry::Int27 => INT27,
                _ => ah,
            };
            (
                Frame::of(&Int21 {
                    function,
                    stage: 0,
                    value: 0,
                }),
                entry::EXIT,
            )
        }
    };
    if root.id != Id::Int2F {
        let indos = k.sda8(sda::INDOS);
        k.set_sda8(sda::INDOS, indos.saturating_add(1));
    }
    engine::enter(k, &root, exit);
}

/// An INT 23h handler returned to the kernel. A return that leaves the
/// stack as the handler found it restarts the interrupted call, its AX
/// given back; a far return that leaves the flags restarts it with the
/// carry clear and ends the program with it set; any other stack ends it.
pub fn break_returned(k: &mut Kernel<'_, '_>) {
    let expected = k.sda16(sda::BREAK_SP);
    let segment = k.sda16(sda::BREAK_SS);
    let sp = k.g.r16(Reg16::Sp);
    let restart = k.g.seg(Seg::Ss) == segment
        && if sp == expected {
            true
        } else {
            sp == expected.wrapping_sub(2) && !k.g.regs().flag(flag::CF)
        };
    k.g.set_stack(segment, expected);
    if restart {
        let ax = k.sda16(sda::BREAK_AX);
        k.g.set_r16(Reg16::Ax, ax);
    } else {
        k.set_sda8(sda::BREAK_END, 1);
        k.g.set_r16(Reg16::Ax, 0x4C00);
    }
    k.g.jump(crate::bios::layout::SEGMENT, entry::INT21);
}

/// A program called DOS the CP/M way, through the far call at its PSP's
/// offset 5: the far return that call pushed becomes an interrupt frame
/// returning to the program's own near return, and the function in CL is
/// taken as INT 21h's; one CP/M has no counterpart for answers AL zero.
pub fn call5(g: &mut crate::bios::guest::Guest<'_>) {
    let (ss, sp) = (g.seg(Seg::Ss), g.r16(Reg16::Sp));
    let program = g.read16(ss, sp.wrapping_add(2));
    let back = g.read16(ss, sp.wrapping_add(4));
    let flags = g.regs().flags;
    g.write16(ss, sp, back);
    g.write16(ss, sp.wrapping_add(2), program);
    g.write16(ss, sp.wrapping_add(4), flags);
    let function = g.r8(crate::asm::Reg8::Cl);
    if function > CPM_FUNCTIONS {
        g.set_r8(crate::asm::Reg8::Al, 0);
        g.jump(
            crate::bios::layout::SEGMENT,
            crate::bios::layout::entry::IRET,
        );
        return;
    }
    g.set_r8(crate::asm::Reg8::Ah, function);
    g.jump(crate::bios::layout::SEGMENT, entry::INT21);
}

/// The highest function a CP/M-style call may make.
const CPM_FUNCTIONS: u8 = 0x24;

/// The class, suggested action and locus DOS reports for error `code`.
#[must_use]
pub const fn error_info(code: u16) -> (u8, u8, u8) {
    match code {
        error::INVALID_FUNCTION | error::INVALID_HANDLE | error::INVALID_ACCESS => (7, 4, 1),
        error::FILE_NOT_FOUND
        | error::PATH_NOT_FOUND
        | error::NO_MORE_FILES
        | error::INVALID_DRIVE => (8, 3, 2),
        error::TOO_MANY_OPEN | error::NO_MEMORY => (1, 4, 1),
        error::ACCESS_DENIED | error::CURRENT_DIRECTORY | error::CANNOT_MAKE => (3, 3, 2),
        error::ARENA_TRASHED | error::BAD_BLOCK | error::BAD_ENVIRONMENT => (7, 5, 5),
        error::BAD_FORMAT | error::INVALID_DATA | error::INVALID_PARAMETER => (9, 3, 1),
        error::NOT_SAME_DEVICE => (13, 3, 2),
        error::FILE_EXISTS => (12, 3, 2),
        0x13..=0x1F | error::DISK_CHANGE | error::FAIL_ON_24 => (11, 1, 2),
        0x20 | 0x21 => (10, 2, 2),
        _ => (13, 4, 1),
    }
}

impl Kernel<'_, '_> {
    /// Records `code` as the last error, with its class, action and locus.
    pub fn record_error(&mut self, code: u16) {
        let (class, action, locus) = error_info(code);
        self.set_sda16(sda::ERROR, code);
        self.set_sda8(sda::CLASS, class);
        self.set_sda8(sda::ACTION, action);
        self.set_sda8(sda::LOCUS, locus);
    }

    /// The far pointer the caller's `segment` and `offset` registers hold.
    pub(super) fn caller_far(&mut self, segment: Saved, offset: Saved) -> Far {
        Far::new(self.caller(segment), self.caller(offset))
    }

    /// Copies the caller's ASCIZ path at `from` into the running call's
    /// path buffer `which`.
    fn take_path(&mut self, which: u16, from: Far) {
        let (path, len) = dirops::caller_path(self, from);
        self.set_path(which, &path[..len]);
    }
}

locals! {
    /// An INT 21h call of function `function`: `stage` and `value` are
    /// what the functions that take more than one routine keep.
    pub struct Int21 {
        function: u8,
        stage: u8,
        value: u32,
    }
}

const BREAK_LOOKED: u8 = 1;
const CALLED: u8 = 2;
const ENDING: u8 = 3;
const BREAK_ECHOED: u8 = 4;

impl Routine for Int21 {
    const ID: Id = Id::Int21;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            ENDING => Flow::Transfer,
            BREAK_ECHOED => Self::break_out(k),
            CALLED => match child {
                Ok(value) => self.finish(k, phase, value),
                Err(fault) => self.faulted(k, phase, fault),
            },
            BREAK_LOOKED => match child {
                Err(Fault::Break) => Self::echo_break(k, phase),
                _ => self.start(k, phase),
            },
            _ => {
                let checks = k.sda8(sda::BREAK) != 0
                    && self.function > 0x0C
                    && !matches!(self.function, 0x31 | 0x4C | 0x59 | INT27);
                if checks {
                    *phase = BREAK_LOOKED;
                    return Flow::Call(Frame::of(&char::BreakCheck { paused: false }));
                }
                self.start(k, phase)
            }
        }
    }
}

/// How a function answers a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    /// The carry set and AX the error.
    Carry,
    /// AL `FFh`.
    Al,
    /// AX `FFFFh`.
    Ax,
}

const fn failure_of(function: u8) -> Failure {
    match function {
        0x0F..=0x17 | 0x1B | 0x1C | 0x1F | 0x21..=0x24 | 0x27..=0x29 | 0x2B | 0x2D | 0x32 => {
            Failure::Al
        }
        0x36 => Failure::Ax,
        _ => Failure::Carry,
    }
}

impl Int21 {
    /// The call is answered: `InDOS` drops and the root returns.
    fn leave(k: &mut Kernel<'_, '_>) -> Flow {
        exec::leave_dos(k);
        Flow::Return(Ok(0))
    }

    /// The call succeeded: the carry clear.
    fn done(k: &mut Kernel<'_, '_>) -> Flow {
        k.set_caller_carry(false);
        Self::leave(k)
    }

    /// The call failed with `code`, answered the function's way and kept as
    /// the last error, unless its critical-error handler failed it: the
    /// caller still gets the code, and the last error says why.
    fn fail(self, k: &mut Kernel<'_, '_>, code: u16) -> Flow {
        match failure_of(self.function) {
            Failure::Al => k.set_caller8(Saved::Ax, false, 0xFF),
            Failure::Ax => k.set_caller(Saved::Ax, 0xFFFF),
            Failure::Carry => {
                k.set_caller(Saved::Ax, code);
                k.set_caller_carry(true);
            }
        }
        let at = k.scratch(level::FAILED);
        let last = if k.get8(at) != 0 {
            error::FAIL_ON_24
        } else {
            code
        };
        k.record_error(last);
        Self::leave(k)
    }

    /// The call failed with `fault`: an error answered as [`Self::fail`]
    /// answers it, an abort ending the program, a break its break.
    fn faulted(self, k: &mut Kernel<'_, '_>, phase: &mut u8, fault: Fault) -> Flow {
        match fault {
            Fault::Error(code) => self.fail(k, code),
            Fault::Abort => Self::terminate(k, phase, 0, ending::CRITICAL, 0),
            Fault::Break => Self::echo_break(k, phase),
        }
    }

    fn call(phase: &mut u8, frame: Frame) -> Flow {
        *phase = CALLED;
        Flow::Call(frame)
    }

    fn terminate(k: &mut Kernel<'_, '_>, phase: &mut u8, code: u8, how: u8, keep: u16) -> Flow {
        let how = if k.sda8(sda::BREAK_END) != 0 {
            k.set_sda8(sda::BREAK_END, 0);
            ending::BREAK
        } else {
            how
        };
        *phase = ENDING;
        Flow::Call(Frame::of(&Terminate {
            code,
            how,
            keep,
            handle: 0,
        }))
    }

    /// Ctrl+C broke the call: `^C` and a new line go to the console first.
    fn echo_break(k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let at = u16::try_from(i32::from(SYSVARS) + i32::from(sysvars::CON)).unwrap_or(0);
        let device = k.get_far(at);
        *phase = BREAK_ECHOED;
        Flow::Call(Frame::of(&char::CharIo {
            device,
            command: command::OUTPUT,
            buffer: Far::new(crate::bios::layout::SEGMENT, entry::BREAK_TEXT),
            count: u16::try_from(super::rom::BREAK_TEXT.len()).unwrap_or(0),
            done: 0,
            status: 0,
        }))
    }

    /// Ctrl+C broke the call: the caller's registers and stack are put back
    /// as they were at its INT 21h, and INT 23h taken from there.
    fn break_out(k: &mut Kernel<'_, '_>) -> Flow {
        let frame = k.caller_frame();
        let mut saved = [0u16; 9];
        for (n, slot) in (0u16..).zip(saved.iter_mut()) {
            *slot = k.read16_at(frame.add(n * 2));
        }
        exec::leave_dos(k);
        let stack = frame.add(Saved::Ip as u16 * 2);
        k.set_sda16(sda::BREAK_SP, stack.offset);
        k.set_sda16(sda::BREAK_SS, stack.segment);
        k.set_sda16(sda::BREAK_AX, saved[Saved::Ax as usize]);
        let g = &mut *k.g;
        for (reg, value) in [
            Reg16::Ax,
            Reg16::Bx,
            Reg16::Cx,
            Reg16::Dx,
            Reg16::Si,
            Reg16::Di,
            Reg16::Bp,
        ]
        .into_iter()
        .zip(saved)
        {
            g.set_r16(reg, value);
        }
        g.regs_mut().set_sreg(Seg::Ds, saved[Saved::Ds as usize]);
        g.regs_mut().set_sreg(Seg::Es, saved[Saved::Es as usize]);
        g.set_stack(stack.segment, stack.offset);
        g.regs_mut().set_flag(flag::CF, false);
        g.interrupt(0x23, (crate::bios::layout::SEGMENT, entry::BREAK_RETURN));
        Flow::Transfer
    }

    /// Starts the function.
    fn start(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        match self.function {
            0x01..=0x0C => self.console(k, phase),
            0x0F..=0x17 | 0x21..=0x24 | 0x27..=0x29 => self.fcb(k, phase),
            0x0D | 0x0E | 0x19 | 0x1A | 0x1B | 0x1C | 0x1F | 0x2E | 0x2F | 0x32 | 0x36 | 0x54 => {
                self.disk(k, phase)
            }
            0x39..=0x3B | 0x41 | 0x43 | 0x47 | 0x4E | 0x4F | 0x56 | 0x60 => {
                self.directory(k, phase)
            }
            0x3C..=0x40 | 0x42 | 0x44..=0x46 | 0x57 | 0x5A | 0x5B | 0x67 | 0x68 | 0x6A | 0x6C => {
                self.handles(k, phase)
            }
            0x00 | 0x26 | 0x31 | 0x4B..=0x4D | 0x50 | 0x51 | 0x55 | 0x62 | INT27 => {
                self.process(k, phase)
            }
            0x48..=0x4A | 0x58 => self.memory(k),
            _ => self.system(k, phase),
        }
    }

    /// Functions `01h`–`0Ch`: the console and the standard handles.
    fn console(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let dl = k.caller8(Saved::Dx, false);
        let get = |cooked, echo, wait| {
            Frame::of(&GetChar {
                cooked,
                echo,
                wait,
                byte: 0,
            })
        };
        match self.function {
            0x01 => Self::call(phase, get(true, true, true)),
            0x02 => Self::put(phase, dl),
            0x03 => self.byte_io(k, phase, char::handle::AUX, None),
            0x04 => self.byte_io(k, phase, char::handle::AUX, Some(dl)),
            0x05 => self.byte_io(k, phase, char::handle::PRINTER, Some(dl)),
            0x06 if dl == 0xFF => Self::call(phase, get(false, false, false)),
            0x06 => self.byte_io(k, phase, char::handle::OUTPUT, Some(dl)),
            0x07 => Self::call(phase, get(false, false, true)),
            0x08 => Self::call(phase, get(true, false, true)),
            0x09 => self.next_string_byte(k, phase),
            0x0A => self.line_input(k, phase),
            0x0B => Self::call(phase, Frame::of(&InputStatus {})),
            _ => self.flush_input(k, phase),
        }
    }

    fn put(phase: &mut u8, byte: u8) -> Flow {
        Self::call(
            phase,
            Frame::of(&PutChar {
                byte,
                spaces: 0,
                index: 0,
            }),
        )
    }

    /// Functions `03h`–`06h`: a byte through a standard handle, read when
    /// `byte` is `None`.
    fn byte_io(
        self,
        k: &mut Kernel<'_, '_>,
        phase: &mut u8,
        handle: u16,
        byte: Option<u8>,
    ) -> Flow {
        let Ok((index, _)) = k.handle(handle) else {
            return self.fail(k, error::INVALID_HANDLE);
        };
        let at = Far::new(SEGMENT, k.scratch(level::BYTE));
        let index = u16::from(index);
        match byte {
            Some(byte) => {
                k.set8(at.offset, byte);
                Self::call(
                    phase,
                    Frame::of(&Write {
                        index,
                        buffer: at,
                        count: 1,
                        done: 0,
                        kept: 0,
                        next: 0,
                    }),
                )
            }
            None => Self::call(
                phase,
                Frame::of(&Read {
                    index,
                    buffer: at,
                    count: 1,
                    done: 0,
                    steps: 0,
                }),
            ),
        }
    }

    /// Function `09h`'s next byte, or its end at `$`.
    fn next_string_byte(self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let string = k.caller_far(Saved::Ds, Saved::Dx);
        let at = string.add(u16::try_from(self.value).unwrap_or(0));
        let byte = k.g.read8(at.segment, at.offset);
        if byte == b'$' || self.value >= 0xFFFF {
            k.set_caller8(Saved::Ax, false, b'$');
            return Self::leave(k);
        }
        Self::put(phase, byte)
    }

    /// Function `0Ah`: a line edited at the console, or read from the
    /// standard input when it is no console.
    fn line_input(self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let buffer = k.caller_far(Saved::Ds, Saved::Dx);
        let max = k.g.read8(buffer.segment, buffer.offset);
        if max == 0 {
            return Self::leave(k);
        }
        let Ok((_, entry)) = k.handle(char::handle::INPUT) else {
            return self.fail(k, error::INVALID_HANDLE);
        };
        let console = k
            .sft_device(entry)
            .filter(|&device| k.attributes(device) & attribute::STDIN != 0);
        Self::call(
            phase,
            Frame::of(&LineInput {
                device: console.unwrap_or_default(),
                buffer: buffer.add(2),
                count: u16::from(max),
                entry: Far::default(),
                piped: console.is_none(),
            }),
        )
    }

    /// Function `0Ch`: the standard input's waiting bytes dropped, then the
    /// input function AL names run.
    fn flush_input(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let Ok((_, entry)) = k.handle(char::handle::INPUT) else {
            return self.fail(k, error::INVALID_HANDLE);
        };
        let Some(device) = k.sft_device(entry) else {
            return self.after_flush(k, phase);
        };
        if k.attributes(device) & attribute::STDIN != 0 {
            k.set8(driver::CON_LINE, 0);
            k.set8(driver::CON_LINE + 1, 0);
        }
        Self::call(
            phase,
            Frame::of(&char::CharIo {
                device,
                command: command::INPUT_FLUSH,
                buffer: Far::default(),
                count: 0,
                done: 0,
                status: 0,
            }),
        )
    }

    fn after_flush(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let al = k.caller8(Saved::Ax, false);
        if matches!(al, 0x01 | 0x06 | 0x07 | 0x08 | 0x0A) {
            self.function = al;
            return self.start(k, phase);
        }
        k.set_caller8(Saved::Ax, false, 0);
        Self::leave(k)
    }

    /// The FCB functions.
    fn fcb(self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let pointer = k.caller_far(Saved::Ds, Saved::Dx);
        let named = fcb::named(k, pointer);
        let at = named.at;
        let attributes = named.attributes;
        match self.function {
            0x0F | 0x16 => Self::call(
                phase,
                Frame::of(&FcbOpen {
                    fcb: at,
                    attributes,
                    create: self.function == 0x16,
                    ..FcbOpen::default()
                }),
            ),
            0x10 => Self::call(phase, Frame::of(&FcbClose { fcb: at })),
            0x11 | 0x12 => Self::call(
                phase,
                Frame::of(&FcbSearch {
                    fcb: at,
                    extended: named.extended.is_some(),
                    attributes,
                    next: self.function == 0x12,
                    drive: 0,
                    directory: 0,
                    index: 0,
                }),
            ),
            0x13 => Self::call(
                phase,
                Frame::of(&FcbDelete {
                    fcb: at,
                    attributes,
                    drive: 0,
                    directory: 0,
                    index: 0,
                    cluster: 0,
                    deleted: 0,
                }),
            ),
            0x17 => Self::call(
                phase,
                Frame::of(&FcbRename {
                    fcb: at,
                    attributes,
                    drive: 0,
                    directory: 0,
                    index: 0,
                    sector: 0,
                    slot: 0,
                    renamed: 0,
                    outcome: 0,
                }),
            ),
            0x23 => Self::call(
                phase,
                Frame::of(&FcbSize {
                    fcb: at,
                    attributes,
                    drive: 0,
                }),
            ),
            0x24 => {
                fcb::set_random(k, at);
                Self::leave(k)
            }
            0x29 => {
                let text = k.caller_far(Saved::Ds, Saved::Si);
                let target = k.caller_far(Saved::Es, Saved::Di);
                let control = k.caller8(Saved::Ax, false);
                let parsed = fcb::parse_name(k, text, target, control);
                let si = k.caller(Saved::Si).wrapping_add(parsed.taken);
                k.set_caller(Saved::Si, si);
                k.set_caller8(Saved::Ax, false, parsed.result);
                Self::leave(k)
            }
            function => Self::call(
                phase,
                Frame::of(&FcbIo {
                    function,
                    fcb: at,
                    file: 0,
                    records: 0,
                    done: 0,
                    wrapped: false,
                }),
            ),
        }
    }

    /// The drive and disk functions.
    fn disk(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let dl = k.caller8(Saved::Dx, false);
        let al = k.caller8(Saved::Ax, false);
        match self.function {
            0x0D => Self::call(
                phase,
                Frame::of(&Flush {
                    segment: 0,
                    drive: FREE,
                    copy: 0,
                }),
            ),
            0x0E => {
                let drives = k.lastdrive();
                if dl < drives && k.is_drive(dl) {
                    k.set_sda8(sda::DRIVE, dl);
                }
                k.set_caller8(Saved::Ax, false, drives);
                Self::leave(k)
            }
            0x19 => {
                let drive = k.current_drive();
                k.set_caller8(Saved::Ax, false, drive);
                Self::leave(k)
            }
            0x1A => {
                let dta = k.caller_far(Saved::Ds, Saved::Dx);
                k.set_dta(dta);
                Self::leave(k)
            }
            0x2E => {
                k.set_sda8(sda::VERIFY, al & 1);
                Self::leave(k)
            }
            0x2F => {
                let dta = k.dta();
                k.set_caller(Saved::Es, dta.segment);
                k.set_caller(Saved::Bx, dta.offset);
                Self::leave(k)
            }
            0x54 => {
                let verify = k.sda8(sda::VERIFY);
                k.set_caller8(Saved::Ax, false, verify);
                Self::leave(k)
            }
            _ => {
                let drive = match (self.function, dl) {
                    (0x1B | 0x1F, _) | (_, 0) => k.current_drive(),
                    (_, n) => n - 1,
                };
                if let Some(slot) = k.share_of(drive) {
                    return self.share_info(k, phase, slot);
                }
                if k.dpb(drive).is_none() {
                    return self.fail(k, error::INVALID_DRIVE);
                }
                self.value = u32::from(drive);
                if self.function == 0x36 {
                    return Self::call(
                        phase,
                        Frame::of(&Free {
                            drive,
                            cluster: 0,
                            counted: 0,
                        }),
                    );
                }
                Self::call(phase, Frame::of(&Media { drive, status: 0 }))
            }
        }
    }

    /// The directory functions.
    fn directory(self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let path = k.caller_far(Saved::Ds, Saved::Dx);
        let cl = k.caller8(Saved::Cx, false);
        match self.function {
            0x47 => {
                let drive = match k.caller8(Saved::Dx, false) {
                    0 => k.current_drive(),
                    n => n - 1,
                };
                if !k.is_drive(drive) {
                    return self.fail(k, error::INVALID_DRIVE);
                }
                Self::call(phase, Frame::of(&CurrentDir { drive }))
            }
            0x60 => {
                let from = k.caller_far(Saved::Ds, Saved::Si);
                let to = k.caller_far(Saved::Es, Saved::Di);
                match dirops::truename(k, from, to) {
                    Ok(()) => Self::done(k),
                    Err(code) => self.fail(k, code),
                }
            }
            0x4F => Self::call(
                phase,
                Frame::of(&Find {
                    next: true,
                    attributes: 0,
                    drive: 0,
                    directory: 0,
                    index: 0,
                }),
            ),
            _ => {
                k.take_path(level::PATH, path);
                let frame = match self.function {
                    0x39 => Frame::of(&MakeDir {
                        drive: 0,
                        parent: 0,
                        stamp: 0,
                        cluster: 0,
                        code: 0,
                    }),
                    0x3A => Frame::of(&RemoveDir {
                        drive: 0,
                        cluster: 0,
                        sector: 0,
                        slot: 0,
                    }),
                    0x3B => Frame::of(&ChangeDir {}),
                    0x41 => Frame::of(&Delete {
                        drive: 0,
                        cluster: 0,
                    }),
                    0x43 => {
                        let al = k.caller8(Saved::Ax, false);
                        if al > 1 {
                            return self.fail(k, error::INVALID_FUNCTION);
                        }
                        Frame::of(&dirops::Attributes {
                            set: al == 1,
                            value: cl,
                        })
                    }
                    0x4E => Frame::of(&Find {
                        next: false,
                        attributes: cl,
                        drive: 0,
                        directory: 0,
                        index: 0,
                    }),
                    _ => {
                        let new = k.caller_far(Saved::Es, Saved::Di);
                        k.take_path(level::PATH2, new);
                        Frame::of(&Rename {
                            drive: 0,
                            directory: 0,
                            sector: 0,
                            slot: 0,
                            is_directory: false,
                        })
                    }
                };
                Self::call(phase, frame)
            }
        }
    }

    /// The handle functions.
    fn handles(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let al = k.caller8(Saved::Ax, false);
        let bx = k.caller(Saved::Bx);
        let cx = k.caller(Saved::Cx);
        let buffer = k.caller_far(Saved::Ds, Saved::Dx);
        match self.function {
            0x3C | 0x5B => {
                k.take_path(level::PATH, buffer);
                Self::call(
                    phase,
                    Frame::of(&Create {
                        attributes: cx.to_le_bytes()[0],
                        new: self.function == 0x5B,
                        mode: access::BOTH,
                        handle: 0,
                    }),
                )
            }
            0x3D => {
                k.take_path(level::PATH, buffer);
                Self::call(
                    phase,
                    Frame::of(&Open {
                        mode: al,
                        keep_handle: false,
                        index: 0,
                        handle: 0,
                        drive: 0,
                    }),
                )
            }
            0x5A => Self::call(
                phase,
                Frame::of(&TempFile {
                    attributes: cx.to_le_bytes()[0],
                    path: buffer,
                    tries: 0,
                    seed: 0,
                }),
            ),
            0x6C => {
                let path = k.caller_far(Saved::Ds, Saved::Si);
                k.take_path(level::PATH, path);
                let [mode, _] = bx.to_le_bytes();
                let action = k.caller8(Saved::Dx, false);
                Self::call(
                    phase,
                    Frame::of(&ExtendedOpen {
                        mode,
                        attributes: cx.to_le_bytes()[0],
                        action,
                        handle: 0,
                    }),
                )
            }
            0x3E => Self::call(
                phase,
                Frame::of(&Close {
                    handle: bx,
                    index: 0,
                    by_index: false,
                }),
            ),
            0x44 => Self::call(
                phase,
                Frame::of(&Ioctl {
                    function: al,
                    media: false,
                }),
            ),
            0x45 => self.duplicate(k, phase, bx, None),
            0x46 => self.duplicate(k, phase, bx, Some(cx)),
            0x67 => match dirops::set_handle_count(k, bx) {
                Ok(()) => Self::done(k),
                Err(code) => self.fail(k, code),
            },
            _ => self.handle_io(k, phase, bx),
        }
    }

    /// Functions `3Fh`, `40h`, `42h`, `57h`, `68h` and `6Ah`, on an open
    /// handle: read, write, seek, its stamp, and commit.
    fn handle_io(self, k: &mut Kernel<'_, '_>, phase: &mut u8, handle: u16) -> Flow {
        let (index, entry) = match k.handle(handle) {
            Ok(found) => found,
            Err(Fault::Error(code)) => return self.fail(k, code),
            Err(_) => return self.fail(k, error::INVALID_HANDLE),
        };
        let index = u16::from(index);
        let al = k.caller8(Saved::Ax, false);
        let count = k.caller(Saved::Cx);
        let buffer = k.caller_far(Saved::Ds, Saved::Dx);
        match self.function {
            0x3F => Self::call(
                phase,
                Frame::of(&Read {
                    index,
                    buffer,
                    count,
                    done: 0,
                    steps: 0,
                }),
            ),
            0x40 => Self::call(
                phase,
                Frame::of(&Write {
                    index,
                    buffer,
                    count,
                    done: 0,
                    kept: 0,
                    next: 0,
                }),
            ),
            0x42 => self.seek(k, phase, al, entry),
            0x57 => self.file_time(k, al, entry),
            _ => Self::call(phase, Frame::of(&Commit { index })),
        }
    }

    /// Function `42h`: the position moved, a device's staying at zero; a
    /// shared file's end is where the share says it is now, which another
    /// writer may have moved.
    fn seek(self, k: &mut Kernel<'_, '_>, phase: &mut u8, origin: u8, entry: Far) -> Flow {
        let offset = u32::from(k.caller(Saved::Cx)) << 16 | u32::from(k.caller(Saved::Dx));
        let device = k.sft_device(entry).is_some();
        let position = if device {
            (origin <= 2).then_some(0)
        } else {
            let at = k.sft32(entry, sft::POSITION);
            let size = if origin == 2 && k.sft_remote(entry).is_some() {
                match redirect::size(k, entry) {
                    Ok(size) => size,
                    Err(fault) => return self.faulted(k, phase, fault),
                }
            } else {
                k.sft32(entry, sft::SIZE)
            };
            super::file::seek(origin, offset, at, size)
        };
        let Some(position) = position else {
            return self.fail(k, error::INVALID_FUNCTION);
        };
        if !device {
            k.set_sft32(entry, sft::POSITION, position);
        }
        let (low, high) = engine::words(position);
        k.set_caller(Saved::Ax, low);
        k.set_caller(Saved::Dx, high);
        Self::done(k)
    }

    /// Function `57h`: a file's date and time, read or set; set, they are
    /// kept when it closes.
    fn file_time(self, k: &mut Kernel<'_, '_>, al: u8, entry: Far) -> Flow {
        match al {
            0x00 => {
                let time = k.sft16(entry, sft::TIME);
                let date = k.sft16(entry, sft::DATE);
                k.set_caller(Saved::Cx, time);
                k.set_caller(Saved::Dx, date);
                Self::done(k)
            }
            0x01 => {
                // A share kept from being changed keeps its files' stamps too.
                if k.sft_remote(entry).is_some() && !redirect::writable(k, entry) {
                    return self.fail(k, error::ACCESS_DENIED);
                }
                if k.sft_device(entry).is_none() {
                    let time = k.caller(Saved::Cx);
                    let date = k.caller(Saved::Dx);
                    k.set_sft16(entry, sft::TIME, time);
                    k.set_sft16(entry, sft::DATE, date);
                    let word = k.sft16(entry, sft::INFO);
                    k.set_sft16(entry, sft::INFO, (word | info::STAMPED) & !info::CLEAN);
                }
                Self::done(k)
            }
            _ => self.fail(k, error::INVALID_FUNCTION),
        }
    }

    /// Functions `45h` and `46h`: another handle to the handle's file — the
    /// next free one, or `to`, closed first if open.
    fn duplicate(
        &mut self,
        k: &mut Kernel<'_, '_>,
        phase: &mut u8,
        handle: u16,
        to: Option<u16>,
    ) -> Flow {
        let (index, entry) = match k.handle(handle) {
            Ok(found) => found,
            Err(Fault::Error(code)) => return self.fail(k, code),
            Err(_) => return self.fail(k, error::INVALID_HANDLE),
        };
        let target = match to {
            Some(target) => {
                let process = k.psp();
                let (_, size) = k.jft(process);
                if target >= size {
                    return self.fail(k, error::INVALID_HANDLE);
                }
                if target == handle {
                    return Self::done(k);
                }
                if self.stage == 0 && k.handle(target).is_ok() {
                    self.stage = 1;
                    return Self::call(
                        phase,
                        Frame::of(&Close {
                            handle: target,
                            index: 0,
                            by_index: false,
                        }),
                    );
                }
                target
            }
            None => match k.free_handle() {
                Some(free) => free,
                None => return self.fail(k, error::TOO_MANY_OPEN),
            },
        };
        let handles = k.sft16(entry, sft::HANDLES);
        k.set_sft16(entry, sft::HANDLES, handles.saturating_add(1));
        k.set_handle(target, index);
        if to.is_none() {
            k.set_caller(Saved::Ax, target);
        }
        Self::done(k)
    }

    /// The process functions.
    fn process(self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let al = k.caller8(Saved::Ax, false);
        let bx = k.caller(Saved::Bx);
        let dx = k.caller(Saved::Dx);
        match self.function {
            0x00 => Self::terminate(k, phase, 0, ending::NORMAL, 0),
            INT27 => Self::terminate(k, phase, 0, ending::RESIDENT, dx.div_ceil(16)),
            0x31 => Self::terminate(k, phase, al, ending::RESIDENT, dx),
            0x4C => Self::terminate(k, phase, al, ending::NORMAL, 0),
            0x26 => {
                let current = k.psp();
                let mut image = [0u8; 0x100];
                k.g.read_bytes(current, 0, &mut image);
                k.g.write_bytes(dx, 0, &image);
                for (field, vector) in [
                    (psp::TERMINATE, 0x22u16),
                    (psp::BREAK, 0x23),
                    (psp::CRITICAL, 0x24),
                ] {
                    let offset = k.g.read16(0, vector * 4);
                    let segment = k.g.read16(0, vector * 4 + 2);
                    k.g.write16(dx, field, offset);
                    k.g.write16(dx, field + 2, segment);
                }
                Self::leave(k)
            }
            0x4B => match al {
                mode::RUN | mode::LOAD | mode::OVERLAY => {
                    let path = k.caller_far(Saved::Ds, Saved::Dx);
                    k.take_path(level::PATH, path);
                    let block = k.caller_far(Saved::Es, Saved::Bx);
                    Self::call(
                        phase,
                        Frame::of(&Exec {
                            mode: al,
                            block,
                            file: 0,
                            environment: 0,
                            psp: 0,
                            load: 0,
                            done: 0,
                            relocated: 0,
                            code: 0,
                        }),
                    )
                }
                mode::SET_STATE => Self::done(k),
                _ => self.fail(k, error::INVALID_FUNCTION),
            },
            0x4D => {
                let code = k.sda16(sda::RETURN_CODE);
                k.set_caller(Saved::Ax, code);
                k.set_sda16(sda::RETURN_CODE, 0);
                Self::done(k)
            }
            0x50 => {
                k.set_sda16(sda::PSP, bx);
                Self::leave(k)
            }
            0x55 => {
                let current = k.psp();
                let top = k.caller(Saved::Si);
                k.create_psp(dx, top, current, true);
                k.set_sda16(sda::PSP, dx);
                Self::leave(k)
            }
            _ => {
                let process = k.psp();
                k.set_caller(Saved::Bx, process);
                Self::leave(k)
            }
        }
    }

    /// The memory functions.
    fn memory(self, k: &mut Kernel<'_, '_>) -> Flow {
        let bx = k.caller(Saved::Bx);
        let es = k.caller(Saved::Es);
        let answer = match self.function {
            0x48 => {
                let rule = k.strategy();
                let process = k.psp();
                k.allocate(bx, process, rule)
                    .map(|segment| k.set_caller(Saved::Ax, segment))
            }
            0x49 => k.free(es),
            0x4A => k.resize(es, bx),
            _ => return self.strategy(k),
        };
        match answer {
            Ok(()) => Self::done(k),
            Err(refusal) => {
                if let Refusal::Short(largest) = refusal {
                    k.set_caller(Saved::Bx, largest);
                }
                self.fail(k, refusal.error())
            }
        }
    }

    /// Function `58h`: the allocation strategy, and the upper-memory link a
    /// machine without upper memory cannot make.
    fn strategy(self, k: &mut Kernel<'_, '_>) -> Flow {
        let al = k.caller8(Saved::Ax, false);
        let bx = k.caller(Saved::Bx);
        match al {
            0x00 => {
                let rule = k.strategy();
                k.set_caller(Saved::Ax, u16::from(rule));
                Self::done(k)
            }
            0x01 if bx <= u16::from(super::memory::strategy::LAST) => {
                k.set_sda8(sda::STRATEGY, bx.to_le_bytes()[0]);
                Self::done(k)
            }
            0x02 => {
                k.set_caller8(Saved::Ax, false, 0);
                Self::done(k)
            }
            0x03 if bx == 0 => Self::done(k),
            _ => self.fail(k, error::INVALID_FUNCTION),
        }
    }

    /// The system functions: vectors, the clock, versions and flags,
    /// countries, errors and the kernel's own structures.
    fn system(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let al = k.caller8(Saved::Ax, false);
        let bx = k.caller(Saved::Bx);
        let dx = k.caller(Saved::Dx);
        match self.function {
            0x18 | 0x1D | 0x1E | 0x20 | 0x61 | 0x6B => {
                k.set_caller8(Saved::Ax, false, 0);
                Self::leave(k)
            }
            0x25 => {
                let vector = u16::from(al) * 4;
                let segment = k.caller(Saved::Ds);
                k.g.write16(0, vector, dx);
                k.g.write16(0, vector + 2, segment);
                Self::leave(k)
            }
            0x35 => {
                let vector = u16::from(al) * 4;
                let offset = k.g.read16(0, vector);
                let segment = k.g.read16(0, vector + 2);
                k.set_caller(Saved::Es, segment);
                k.set_caller(Saved::Bx, offset);
                Self::leave(k)
            }
            0x2A | 0x2C => Self::call(phase, Frame::of(&char::Clock { read: true })),
            0x2B | 0x2D => self.set_clock(k, phase),
            0x30 => {
                k.set_caller(Saved::Ax, VERSION);
                k.set_caller(
                    Saved::Bx,
                    u16::from_le_bytes([0, if al == 1 { IN_ROM } else { OEM }]),
                );
                k.set_caller(Saved::Cx, 0);
                Self::leave(k)
            }
            0x33 => Self::break_state(k),
            0x34 => {
                k.set_caller(Saved::Es, SEGMENT);
                k.set_caller(Saved::Bx, SDA + sda::INDOS);
                Self::leave(k)
            }
            0x37 => Self::switch_character(k),
            0x38 => self.country(k),
            0x52 => {
                k.set_caller(Saved::Es, SEGMENT);
                k.set_caller(Saved::Bx, SYSVARS);
                Self::leave(k)
            }
            0x53 => Self::bpb_to_dpb(k),
            0x59 => {
                let code = k.sda16(sda::ERROR);
                let class = k.sda8(sda::CLASS);
                let action = k.sda8(sda::ACTION);
                let locus = k.sda8(sda::LOCUS);
                let pointer =
                    Far::new(k.sda16(sda::ERROR_POINTER + 2), k.sda16(sda::ERROR_POINTER));
                k.set_caller(Saved::Ax, code);
                k.set_caller(Saved::Bx, u16::from_le_bytes([action, class]));
                k.set_caller8(Saved::Cx, true, locus);
                k.set_caller(Saved::Es, pointer.segment);
                k.set_caller(Saved::Di, pointer.offset);
                Self::leave(k)
            }
            0x5D => self.server(k),
            0x5E if al == 0 => {
                k.set_caller8(Saved::Cx, true, 0);
                Self::done(k)
            }
            0x5F if al == 0x02 => self.fail(k, error::NO_MORE_FILES),
            0x63 if al == 0 => {
                k.set_caller(Saved::Ds, crate::bios::layout::SEGMENT);
                k.set_caller(Saved::Si, entry::DBCS + 2);
                k.set_caller8(Saved::Ax, false, 0);
                Self::leave(k)
            }
            0x65 => self.extended_country(k),
            0x66 => match al {
                0x01 => {
                    k.set_caller(Saved::Bx, super::country::CODE_PAGE);
                    k.set_caller(Saved::Dx, super::country::CODE_PAGE);
                    Self::done(k)
                }
                0x02 if bx == super::country::CODE_PAGE => Self::done(k),
                0x02 => self.fail(k, error::FILE_NOT_FOUND),
                _ => self.fail(k, error::INVALID_FUNCTION),
            },
            0x69 => Self::call(
                phase,
                Frame::of(&Ioctl {
                    function: al,
                    media: true,
                }),
            ),
            _ => self.fail(k, error::INVALID_FUNCTION),
        }
    }

    /// Function `65h`: extended country information.
    fn extended_country(self, k: &mut Kernel<'_, '_>) -> Flow {
        let al = k.caller8(Saved::Ax, false);
        let dx = k.caller(Saved::Dx);
        let buffer = match al {
            0x21 | 0x22 | 0xA1 | 0xA2 => k.caller_far(Saved::Ds, Saved::Dx),
            _ => k.caller_far(Saved::Es, Saved::Di),
        };
        let request = super::country::Extended {
            country: dx,
            page: k.caller(Saved::Bx),
            buffer,
            size: k.caller(Saved::Cx),
            character: dx.to_le_bytes()[0],
        };
        match super::country::extended(k, al, request) {
            Ok(answer) => {
                match al {
                    0x20 | 0xA0 => k.set_caller8(Saved::Dx, false, answer.to_le_bytes()[0]),
                    0x23 => k.set_caller(Saved::Ax, answer),
                    0x01..=0x07 => k.set_caller(Saved::Cx, answer),
                    _ => {}
                }
                Self::done(k)
            }
            Err(code) => self.fail(k, code),
        }
    }

    /// Functions `2Bh` and `2Dh`: the date or time checked, the record read
    /// so the half not set is kept, and written back.
    fn set_clock(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let year = k.caller(Saved::Cx);
        let [cl, ch] = year.to_le_bytes();
        let [dl, dh] = k.caller(Saved::Dx).to_le_bytes();
        if self.stage == 0 {
            let valid = if self.function == 0x2B {
                (1980..=2099).contains(&year) && char::days_of(year, dh, dl).is_some()
            } else {
                ch < 24 && cl < 60 && dh < 60 && dl < 100
            };
            if !valid {
                return self.fail(k, error::INVALID_DATA);
            }
            self.stage = 1;
            return Self::call(phase, Frame::of(&char::Clock { read: true }));
        }
        let at = k.scratch(level::CLOCK);
        let mut record = [0u8; 6];
        k.g.read_bytes(SEGMENT, at, &mut record);
        if self.function == 0x2B {
            let days = char::days_of(year, dh, dl).unwrap_or(0);
            record[..2].copy_from_slice(&days.to_le_bytes());
        } else {
            record[2..].copy_from_slice(&[cl, ch, dl, dh]);
        }
        k.g.write_bytes(SEGMENT, at, &record);
        self.stage = 2;
        Self::call(phase, Frame::of(&char::Clock { read: false }))
    }

    /// Function `33h`: the break flag, the boot drive, the true version.
    fn break_state(k: &mut Kernel<'_, '_>) -> Flow {
        let al = k.caller8(Saved::Ax, false);
        let dl = k.caller8(Saved::Dx, false);
        match al {
            0x00 => {
                let state = k.sda8(sda::BREAK);
                k.set_caller8(Saved::Dx, false, state);
            }
            0x01 => k.set_sda8(sda::BREAK, u8::from(dl != 0)),
            0x02 => {
                let state = k.sda8(sda::BREAK);
                k.set_sda8(sda::BREAK, u8::from(dl != 0));
                k.set_caller8(Saved::Dx, false, state);
            }
            0x05 => {
                let at =
                    u16::try_from(i32::from(SYSVARS) + i32::from(sysvars::BOOT_DRIVE)).unwrap_or(0);
                let boot = k.get8(at);
                k.set_caller8(Saved::Dx, false, boot);
            }
            0x06 => {
                k.set_caller(Saved::Bx, VERSION);
                k.set_caller(Saved::Dx, u16::from_le_bytes([0, IN_ROM]));
            }
            _ => k.set_caller8(Saved::Ax, false, 0xFF),
        }
        Self::leave(k)
    }

    /// Function `37h`: the switch character, and whether devices need a
    /// `\DEV\` prefix, which they never do.
    fn switch_character(k: &mut Kernel<'_, '_>) -> Flow {
        let al = k.caller8(Saved::Ax, false);
        let dl = k.caller8(Saved::Dx, false);
        match al {
            0x00 => {
                let switch = k.sda8(sda::SWITCH);
                k.set_caller8(Saved::Dx, false, switch);
                k.set_caller8(Saved::Ax, false, 0);
            }
            0x01 => {
                k.set_sda8(sda::SWITCH, dl);
                k.set_caller8(Saved::Ax, false, 0);
            }
            0x02 => {
                k.set_caller8(Saved::Dx, false, 0xFF);
                k.set_caller8(Saved::Ax, false, 0);
            }
            0x03 => k.set_caller8(Saved::Ax, false, 0),
            _ => k.set_caller8(Saved::Ax, false, 0xFF),
        }
        Self::leave(k)
    }

    /// Function `38h`: a country's information, or the country chosen.
    fn country(self, k: &mut Kernel<'_, '_>) -> Flow {
        let al = k.caller8(Saved::Ax, false);
        let code = if al == 0xFF {
            k.caller(Saved::Bx)
        } else {
            u16::from(al)
        };
        let set = k.caller(Saved::Dx) == 0xFFFF;
        let buffer = k.caller_far(Saved::Ds, Saved::Dx);
        match super::country::country_call(k, code, buffer, set) {
            Ok(country) => {
                k.set_caller(Saved::Bx, country);
                k.set_caller(Saved::Ax, country);
                Self::done(k)
            }
            Err(code) => self.fail(k, code),
        }
    }

    /// Function `53h`: the DPB at ES:BP filled from the BPB at DS:SI, its
    /// drive, unit, driver and link kept.
    fn bpb_to_dpb(k: &mut Kernel<'_, '_>) -> Flow {
        use super::data::dpb;
        let from = k.caller_far(Saved::Ds, Saved::Si);
        let to = k.caller_far(Saved::Es, Saved::Bp);
        let mut bpb = [0u8; tairix_fat::boot::DOS_BPB];
        k.g.read_bytes(from.segment, from.offset, &mut bpb);
        if let Some(fields) = fs::dpb_fields(&bpb) {
            for (start, end) in [
                (dpb::SECTOR, dpb::DRIVER),
                (dpb::MEDIA, dpb::NEXT),
                (dpb::FREE_HINT, super::data::DPB_SIZE),
            ] {
                let part = &fields[usize::from(start)..usize::from(end)];
                k.g.write_bytes(to.segment, to.offset.wrapping_add(start), part);
            }
        }
        Self::leave(k)
    }

    /// Function `5Dh`: the swappable data area's place and size, and the
    /// extended error set from a parameter list.
    fn server(self, k: &mut Kernel<'_, '_>) -> Flow {
        let al = k.caller8(Saved::Ax, false);
        match al {
            0x06 => {
                k.set_caller(Saved::Ds, SEGMENT);
                k.set_caller(Saved::Si, SDA);
                k.set_caller(Saved::Cx, SDA_END - SDA);
                k.set_caller(Saved::Dx, sda::ALWAYS);
                Self::done(k)
            }
            0x0A => {
                let list = k.caller_far(Saved::Ds, Saved::Dx);
                let code = k.read16_at(list);
                let [action, class] = k.read16_at(list.add(2)).to_le_bytes();
                let [_, locus] = k.read16_at(list.add(4)).to_le_bytes();
                k.set_sda16(sda::ERROR, code);
                k.set_sda8(sda::CLASS, class);
                k.set_sda8(sda::ACTION, action);
                k.set_sda8(sda::LOCUS, locus);
                Self::done(k)
            }
            _ => self.fail(k, error::INVALID_FUNCTION),
        }
    }

    /// The function's routine is done with `value`.
    fn finish(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, value: u32) -> Flow {
        let low = u16::try_from(value & 0xFFFF).unwrap_or(0);
        let [byte, _] = low.to_le_bytes();
        match self.function {
            0x01 | 0x07 | 0x08 | 0x10..=0x15 | 0x17 | 0x21..=0x23 | 0x27 | 0x28 => {
                k.set_caller8(Saved::Ax, false, byte);
                Self::leave(k)
            }
            0x02 => {
                let dl = k.caller8(Saved::Dx, false);
                k.set_caller8(Saved::Ax, false, dl);
                Self::leave(k)
            }
            0x06 => Self::direct_console(k, value),
            0x03 => {
                let at = k.scratch(level::BYTE);
                let read = k.get8(at);
                k.set_caller8(
                    Saved::Ax,
                    false,
                    if value == 0 { char::CTRL_Z } else { read },
                );
                Self::leave(k)
            }
            0x09 => {
                self.value += 1;
                self.next_string_byte(k, phase)
            }
            0x0A => {
                let buffer = k.caller_far(Saved::Ds, Saved::Dx);
                let typed = u8::try_from(value.saturating_sub(1)).unwrap_or(0);
                k.g.write8(buffer.segment, buffer.offset.wrapping_add(1), typed);
                Self::leave(k)
            }
            0x0B => {
                k.set_caller8(Saved::Ax, false, if value == 0 { 0 } else { 0xFF });
                Self::leave(k)
            }
            0x0C => self.after_flush(k, phase),
            0x0D => {
                k.discard_buffers(FREE);
                Self::leave(k)
            }
            0x1B | 0x1C | 0x1F | 0x32 | 0x36 => self.drive_info(k, low),
            0x2A => Self::date(k),
            0x2C => {
                let at = k.scratch(level::CLOCK);
                let mut record = [0u8; 6];
                k.g.read_bytes(SEGMENT, at, &mut record);
                k.set_caller(Saved::Cx, u16::from_le_bytes([record[2], record[3]]));
                k.set_caller(Saved::Dx, u16::from_le_bytes([record[4], record[5]]));
                Self::leave(k)
            }
            0x2B | 0x2D if self.stage == 1 => self.set_clock(k, phase),
            0x0F | 0x16 | 0x2B | 0x2D => {
                k.set_caller8(Saved::Ax, false, 0);
                Self::leave(k)
            }
            0x3C | 0x3D | 0x3F | 0x40 | 0x5A | 0x5B => {
                k.set_caller(Saved::Ax, low);
                Self::done(k)
            }
            0x6C => {
                k.set_caller(Saved::Ax, low);
                k.set_caller(Saved::Cx, u16::try_from(value >> 16).unwrap_or(0));
                Self::done(k)
            }
            0x43 if k.caller8(Saved::Ax, false) == 0 => {
                k.set_caller(Saved::Cx, low);
                k.set_caller(Saved::Ax, low);
                Self::done(k)
            }
            0x47 => {
                k.set_caller(Saved::Ax, 0x0100);
                Self::done(k)
            }
            0x45 | 0x46 => {
                let bx = k.caller(Saved::Bx);
                let to = (self.function == 0x46).then(|| k.caller(Saved::Cx));
                self.duplicate(k, phase, bx, to)
            }
            _ => Self::done(k),
        }
    }

    /// Function `06h`'s answer: output echoes DL; input sets ZF when nothing
    /// was waiting.
    fn direct_console(k: &mut Kernel<'_, '_>, value: u32) -> Flow {
        let dl = k.caller8(Saved::Dx, false);
        if dl != 0xFF {
            k.set_caller8(Saved::Ax, false, dl);
            return Self::leave(k);
        }
        let flags = k.caller(Saved::Flags);
        if value == char::NONE {
            k.set_caller(Saved::Flags, flags | flag::ZF);
            k.set_caller8(Saved::Ax, false, 0);
        } else {
            k.set_caller(Saved::Flags, flags & !flag::ZF);
            k.set_caller8(Saved::Ax, false, u8::try_from(value & 0xFF).unwrap_or(0));
        }
        Self::leave(k)
    }

    /// Functions `1Bh`, `1Ch`, `1Fh`, `32h` and `36h` on shared drive
    /// `slot`: its geometry or its free space as the share counts them,
    /// and no DPB, which a share has none of.
    fn share_info(self, k: &mut Kernel<'_, '_>, phase: &mut u8, slot: u8) -> Flow {
        if matches!(self.function, 0x1F | 0x32) {
            return self.fail(k, error::INVALID_DRIVE);
        }
        let (per_cluster, free, sector, clusters) = match redirect::space(k, slot) {
            Ok(space) => space,
            Err(fault) => return self.faulted(k, phase, fault),
        };
        if self.function == 0x36 {
            k.set_caller(Saved::Ax, per_cluster);
            k.set_caller(Saved::Bx, free);
            k.set_caller(Saved::Cx, sector);
            k.set_caller(Saved::Dx, clusters);
        } else {
            k.set8(driver::SHARE_MEDIA, tairix_fat::format::FIXED_MEDIA);
            k.set_caller8(Saved::Ax, false, u8::try_from(per_cluster).unwrap_or(0));
            k.set_caller(Saved::Cx, sector);
            k.set_caller(Saved::Dx, clusters);
            k.set_caller(Saved::Ds, SEGMENT);
            k.set_caller(Saved::Bx, driver::SHARE_MEDIA);
        }
        Self::leave(k)
    }

    /// Functions `1Bh`, `1Ch`, `1Fh`, `32h` and `36h`, once the drive's
    /// medium is checked: its geometry, its DPB, or its free space.
    fn drive_info(self, k: &mut Kernel<'_, '_>, free: u16) -> Flow {
        let drive = u8::try_from(self.value).unwrap_or(0);
        let Some(d) = k.dpb(drive) else {
            return self.fail(k, error::INVALID_DRIVE);
        };
        let clusters = d.max_cluster.saturating_sub(1);
        match self.function {
            0x1F | 0x32 => {
                k.set_caller(Saved::Ds, SEGMENT);
                k.set_caller(Saved::Bx, d.at);
                k.set_caller8(Saved::Ax, false, 0);
            }
            0x36 => {
                k.set_caller(Saved::Ax, d.cluster_sectors());
                k.set_caller(Saved::Bx, free);
                k.set_caller(Saved::Cx, d.sector_bytes);
                k.set_caller(Saved::Dx, clusters);
            }
            _ => {
                k.set_caller8(
                    Saved::Ax,
                    false,
                    u8::try_from(d.cluster_sectors()).unwrap_or(0),
                );
                k.set_caller(Saved::Cx, d.sector_bytes);
                k.set_caller(Saved::Dx, clusters);
                k.set_caller(Saved::Ds, SEGMENT);
                k.set_caller(Saved::Bx, dpb_at(drive) + super::data::dpb::MEDIA);
            }
        }
        Self::leave(k)
    }

    /// Function `2Ah`'s answer from the record `CLOCK$` gave.
    fn date(k: &mut Kernel<'_, '_>) -> Flow {
        let at = k.scratch(level::CLOCK);
        let days = k.get16(at);
        let (year, month, day) = char::civil_of(days);
        // 1980-01-01 was a Tuesday.
        let weekday = u8::try_from((u32::from(days) + 2) % 7).unwrap_or(0);
        k.set_caller(Saved::Cx, year);
        k.set_caller(Saved::Dx, u16::from_le_bytes([day, month]));
        k.set_caller8(Saved::Ax, false, weekday);
        Self::leave(k)
    }
}

locals! {
    /// INT 25h or 26h: a drive's sectors read or written directly, its
    /// medium checked first and, for a read, its changed buffers written so
    /// the disk holds what was last written; a write drops the buffers it
    /// overwrites and the free count it may change. A failure is the
    /// caller's to report: AL the driver's error, AH the BIOS's.
    pub struct Int25 {
        write: bool,
        drive: u8,
    }
}

const CHECKED: u8 = 1;
const FLUSHED: u8 = 2;
const MOVED: u8 = 3;

impl Routine for Int25 {
    const ID: Id = Id::Int25;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            CHECKED => match child {
                Ok(_) if !self.write && k.has_dirty(self.drive) => {
                    *phase = FLUSHED;
                    Flow::Call(Frame::of(&Flush {
                        segment: 0,
                        drive: self.drive,
                        copy: 0,
                    }))
                }
                Ok(_) => self.transfer(k, phase),
                Err(fault) => Self::answer(k, Err(fault)),
            },
            FLUSHED => match child {
                Ok(_) => self.transfer(k, phase),
                Err(fault) => Self::answer(k, Err(fault)),
            },
            MOVED => Self::answer(k, child),
            _ => {
                self.drive = k.caller8(Saved::Ax, false);
                if k.dpb(self.drive).is_none() {
                    let unknown_unit = 0x13 + u16::from(super::device::error::UNKNOWN_UNIT);
                    return Self::answer(k, Err(Fault::Error(unknown_unit)));
                }
                *phase = CHECKED;
                Flow::Call(Frame::of(&Media {
                    drive: self.drive,
                    status: 0,
                }))
            }
        }
    }
}

impl Int25 {
    fn transfer(self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let cx = k.caller(Saved::Cx);
        let ds_bx = k.caller_far(Saved::Ds, Saved::Bx);
        let (sector, count, buffer) = if cx == 0xFFFF {
            let low = k.read16_at(ds_bx);
            let high = k.read16_at(ds_bx.add(2));
            let count = k.read16_at(ds_bx.add(4));
            let buffer = k.far_at(ds_bx.add(6));
            (u32::from(low) | u32::from(high) << 16, count, buffer)
        } else {
            (u32::from(k.caller(Saved::Dx)), cx, ds_bx)
        };
        if self.write {
            for n in 0..u32::from(count) {
                if let Some(segment) = k.find_buffer(self.drive, sector.wrapping_add(n)) {
                    k.g.write8(segment, super::data::buffer::DRIVE, FREE);
                    k.g.write8(segment, super::data::buffer::FLAGS, 0);
                }
            }
            if let Some(d) = k.dpb(self.drive) {
                k.set16(d.at + super::data::dpb::FREE, 0xFFFF);
            }
        }
        *phase = MOVED;
        Flow::Call(Frame::of(&BlockIo {
            drive: self.drive,
            sector,
            count,
            address: buffer,
            write: self.write,
            area: area::ABSOLUTE,
            status: 0,
        }))
    }

    /// Answers the caller: the carry, and AX.
    fn answer(k: &mut Kernel<'_, '_>, outcome: Outcome) -> Flow {
        exec::leave_dos(k);
        let (carry, ax) = match outcome {
            Ok(_) => (false, 0),
            Err(Fault::Error(code)) => {
                let driver = code.saturating_sub(0x13).to_le_bytes()[0];
                let bios: u8 = match driver {
                    0x00 => 0x03,
                    0x02 => 0x80,
                    0x04 => 0x10,
                    0x06 => 0x40,
                    0x07 => 0x02,
                    0x08 => 0x04,
                    0x01 | 0x03 | 0x05 => 0x01,
                    _ => 0x20,
                };
                (true, u16::from_le_bytes([driver, bios]))
            }
            Err(_) => (true, 0x200C),
        };
        k.g.regs_mut().set_flag(flag::CF, carry);
        k.set_caller(Saved::Ax, ax);
        Flow::Return(Ok(0))
    }
}

locals! {
    /// INT 2Fh at the end of its chain: the kernel's own functions (AH
    /// `12h`), and the installation checks of what is not installed, which
    /// leave AL as it came.
    pub struct Int2F {
        function: u8,
    }
}

impl Routine for Int2F {
    const ID: Id = Id::Int2F;

    fn step(&mut self, k: &mut Kernel<'_, '_>, _phase: &mut u8, _child: Outcome) -> Flow {
        if self.function == 0x12 {
            internal(k);
        }
        Flow::Return(Ok(0))
    }
}

/// INT 2Fh function `12h`, the services DOS offers its own extensions; a
/// word one takes lies above the caller's flags.
fn internal(k: &mut Kernel<'_, '_>) {
    let al = k.caller8(Saved::Ax, false);
    let pushed = k.caller_frame().add(Saved::Flags as u16 * 2 + 2);
    let flags = k.caller(Saved::Flags);
    let mut carry = false;
    match al {
        0x00 => k.set_caller8(Saved::Ax, false, 0xFF),
        0x12 | 0x25 => {
            let string = if al == 0x12 {
                k.caller_far(Saved::Es, Saved::Di)
            } else {
                k.caller_far(Saved::Ds, Saved::Si)
            };
            let len = (0..0xFFFFu16)
                .find(|&n| {
                    let at = string.add(n);
                    k.g.read8(at.segment, at.offset) == 0
                })
                .unwrap_or(0xFFFF);
            k.set_caller(Saved::Cx, len);
        }
        0x13 => {
            let [byte, _] = k.read16_at(pushed).to_le_bytes();
            k.set_caller8(Saved::Ax, false, super::name::upper(byte));
        }
        0x16 => {
            let index = k.caller(Saved::Bx);
            match k.sft(index) {
                Some(entry) => {
                    k.set_caller(Saved::Es, entry.segment);
                    k.set_caller(Saved::Di, entry.offset);
                }
                None => carry = true,
            }
        }
        0x17 => {
            let [drive, _] = k.read16_at(pushed).to_le_bytes();
            if drive < k.lastdrive() {
                k.set_caller(Saved::Ds, SEGMENT);
                k.set_caller(Saved::Si, fs::cds_at(drive));
            } else {
                carry = true;
            }
        }
        0x20 => {
            let process = k.psp();
            let (table, size) = k.jft(process);
            let handle = k.caller(Saved::Bx);
            if handle < size {
                let at = table.add(handle);
                k.set_caller(Saved::Es, at.segment);
                k.set_caller(Saved::Di, at.offset);
            } else {
                k.set_caller(Saved::Ax, error::INVALID_HANDLE);
                carry = true;
            }
        }
        0x21 => {
            let from = k.caller_far(Saved::Ds, Saved::Si);
            let to = k.caller_far(Saved::Es, Saved::Di);
            if let Err(code) = dirops::truename(k, from, to) {
                k.set_caller(Saved::Ax, code);
                carry = true;
            }
        }
        _ => {}
    }
    k.set_caller(
        Saved::Flags,
        if carry {
            flags | flag::CF
        } else {
            flags & !flag::CF
        },
    );
}
