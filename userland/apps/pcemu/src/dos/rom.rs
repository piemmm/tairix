//! The kernel's code in the system ROM: the entries the interrupt vectors
//! and device headers point at, each a host call, and the landing pad every
//! guest call the kernel makes returns to.

use super::call;
use super::country;
use super::data::Builtin;
use crate::asm::{Asm, Cond, Reg16, Reg8};
use crate::bios::layout::{entry as bios, Image};
use crate::bios::BuildError;
use crate::cpu::Seg;

/// What a boot record checks the ROM for.
pub const SIGNATURE: &[u8; 16] = b"TAIRiX DOS 5.00\0";

/// The kernel's entries in the system ROM's segment.
pub mod entry {
    /// [`super::SIGNATURE`].
    pub const SIGNATURE: u16 = 0x0200;
    /// Where a boot record jumps to.
    pub const LOADER: u16 = 0x0210;
    /// INT 21h.
    pub const INT21: u16 = 0x0220;
    /// Where an INT 21h, 20h, 27h or 2Fh call returns to its caller.
    pub const EXIT: u16 = 0x0234;
    /// Where every guest call the kernel makes returns.
    pub const STEP: u16 = 0x0240;
    /// INT 20h.
    pub const INT20: u16 = 0x0250;
    /// INT 27h.
    pub const INT27: u16 = 0x0270;
    /// INT 23h's first handler: a far return with the carry set, which ends
    /// the program.
    pub const INT23: u16 = 0x0290;
    /// INT 24h's first handler: it fails the call.
    pub const INT24: u16 = 0x0298;
    /// INT 25h.
    pub const INT25: u16 = 0x02A0;
    /// Where INT 25h and 26h return, leaving the caller's flags stacked.
    pub const EXIT25: u16 = 0x02B4;
    /// INT 26h.
    pub const INT26: u16 = 0x02C0;
    /// INT 29h: the fast console write.
    pub const INT29: u16 = 0x02E0;
    /// INT 2Fh.
    pub const INT2F: u16 = 0x02F0;
    /// INT 1Bh, as the console driver hooks it.
    pub const INT1B: u16 = 0x0310;
    /// Every built-in device's strategy entry.
    pub const STRATEGY: u16 = 0x0318;
    /// `NUL`'s strategy and interrupt entry.
    pub const NUL: u16 = 0x0320;
    /// Where a built-in driver's BIOS call returns; its way out follows.
    pub const DRIVER_STEP: u16 = 0x0328;
    /// A built-in driver's way out.
    pub const DRIVER_EXIT: u16 = 0x032C;
    /// Where a console waiting for a key halts until the next interrupt,
    /// then returns.
    pub const IDLE: u16 = 0x033C;
    /// Where an INT 23h handler returns.
    pub const BREAK_RETURN: u16 = 0x0460;
    /// The country's case-map routine: AL from `80h` in upper case.
    pub const CASE_MAP: u16 = 0x0470;
    /// The upper-case table, which file names use too.
    pub const UPPER: u16 = 0x0480;
    /// The file-name terminator table.
    pub const TERMINATORS: u16 = 0x0510;
    /// The collating sequence.
    pub const COLLATE: u16 = 0x0530;
    /// The double-byte lead table; its pairs follow its length.
    pub const DBCS: u16 = 0x0640;
    /// The CP/M-style entry the far call in every PSP reaches.
    pub const CALL5: u16 = 0x0650;
    /// Where a program the boot ran ends up.
    pub const BOOT_RETURN: u16 = 0x0660;
    /// What the console shows when Ctrl+C breaks a call.
    pub const BREAK_TEXT: u16 = 0x0670;
    /// `ANSI.SYS`'s interrupt entry, which its stub far-calls.
    pub const ANSI: u16 = 0x0680;
    /// Its INT 29h, which its handler far-calls.
    pub const ANSI_FAST: u16 = 0x06A0;
    /// The entries `STACKS=` points its vectors at, in the order it hooks
    /// them.
    pub const STACKS: u16 = 0x06C0;
    /// Bytes between two of them.
    pub const STACKS_STRIDE: u16 = 8;
    /// Where a handler called on one of the stacks returns.
    pub const STACK_OUT: u16 = 0x0700;
    /// Where no stack was free: the machine is halted.
    pub const STACK_OVERFLOW: u16 = 0x0710;
    /// Where a stack's bookkeeping was found damaged: the machine is halted.
    pub const STACK_FAILURE: u16 = 0x0760;
    /// `MOUSE.COM`'s INT 33h, which its handler far-calls.
    pub const MOUSE: u16 = 0x0800;
    /// Its port's interrupt, which its handler far-calls.
    pub const MOUSE_IRQ: u16 = 0x0820;
    /// Its installer's calls.
    pub const MOUSE_LOOK: u16 = 0x0840;
    pub const MOUSE_PROBE: u16 = 0x0848;
    pub const MOUSE_PROBE_STEP: u16 = 0x0850;
    pub const MOUSE_KEEP: u16 = 0x0858;
    /// What its installer says, for function `09h`.
    pub const MOUSE_INSTALLED: u16 = 0x0860;
    pub const MOUSE_ALREADY: u16 = 0x0890;
    pub const MOUSE_ABSENT: u16 = 0x08C0;
    /// Where a program's stub jumps, its number in AX.
    pub const PROGRAM: u16 = 0x0900;
    /// Where every guest call a program makes returns.
    pub const PROGRAM_STEP: u16 = 0x0910;
    /// A program's handlers' entries, which their stubs jump to with DS the
    /// program's segment.
    pub const PROGRAM_HANDLER: u16 = 0x0920;
    pub const PROGRAM_HANDLER_STRIDE: u16 = 0x20;
    /// Where a handler returns to its caller.
    pub const PROGRAM_EXIT: u16 = 0x09A0;
    /// Where a program with no room to start ends, saying so.
    pub const PROGRAM_REFUSED: u16 = 0x09B0;
    /// Where an INT 23h handler returns asking DOS to end the program: a
    /// far return leaving the flags, the carry set.
    pub const PROGRAM_EXIT_BREAK: u16 = 0x09F0;
    /// INT 13h, as DOS hooks it before the BIOS's handler.
    pub const INT13: u16 = 0x0A00;
    /// The built-in devices' interrupt entries, in [`super::Builtin`] order.
    pub const DRIVERS: u16 = 0x0340;
    /// Bytes between two of them.
    pub const DRIVER_STRIDE: u16 = 24;
}

/// What the console shows when Ctrl+C breaks a call.
pub const BREAK_TEXT: &[u8] = b"^C\r\n";

/// Bytes a built-in driver keeps on its stack across its BIOS calls.
pub const DRIVER_LOCALS: u8 = 30;

/// The ROM entry of `device`'s interrupt routine.
#[must_use]
pub const fn driver_entry(device: Builtin) -> u16 {
    entry::DRIVERS + device as u16 * entry::DRIVER_STRIDE
}

/// The registers an INT 21h-like call saves, ES first, so AX lies lowest.
const SAVED: [Reg16; 7] = [
    Reg16::Bp,
    Reg16::Di,
    Reg16::Si,
    Reg16::Dx,
    Reg16::Cx,
    Reg16::Bx,
    Reg16::Ax,
];

fn save(asm: &mut Asm) {
    asm.push_seg(Seg::Es);
    asm.push_seg(Seg::Ds);
    for reg in SAVED {
        asm.push(reg);
    }
}

fn restore(asm: &mut Asm) {
    for reg in SAVED.iter().rev() {
        asm.pop(*reg);
    }
    asm.pop_seg(Seg::Ds);
    asm.pop_seg(Seg::Es);
}

/// An entry that saves the caller's registers as INT 21h does and makes
/// host call `enter`. The kernel always continues the CPU elsewhere; should
/// it not, the machine stops.
fn kernel_entry(asm: &mut Asm, enter: u16) {
    asm.sti();
    save(asm);
    asm.host_call(enter);
    asm.jmp_far(crate::bios::layout::SEGMENT, bios::HALT);
}

/// Lays the kernel's entries into the system ROM.
pub fn place(rom: &mut Image) -> Result<(), BuildError> {
    rom.place(entry::SIGNATURE, SIGNATURE)?;
    rom.code(entry::LOADER, |asm| {
        asm.cli();
        asm.host_call(call::BOOT);
        asm.jmp_far(crate::bios::layout::SEGMENT, bios::HALT);
    })?;
    rom.code(entry::INT21, |asm| kernel_entry(asm, call::INT21))?;
    rom.code(entry::EXIT, |asm| {
        restore(asm);
        asm.iret();
    })?;
    rom.code(entry::STEP, |asm| {
        asm.host_call(call::STEP);
        asm.jmp_far(crate::bios::layout::SEGMENT, bios::HALT);
    })?;
    rom.code(entry::INT20, |asm| kernel_entry(asm, call::INT20))?;
    rom.code(entry::INT27, |asm| kernel_entry(asm, call::INT27))?;
    rom.code(entry::INT23, |asm| {
        asm.stc();
        asm.retf();
    })?;
    rom.code(entry::INT24, |asm| {
        asm.mov_imm8(Reg8::Al, 3);
        asm.iret();
    })?;
    rom.code(entry::INT25, |asm| kernel_entry(asm, call::INT25))?;
    rom.code(entry::INT26, |asm| kernel_entry(asm, call::INT26))?;
    rom.code(entry::EXIT25, |asm| {
        restore(asm);
        asm.retf();
    })?;
    rom.code(entry::INT29, |asm| {
        asm.push(Reg16::Ax);
        asm.push(Reg16::Bx);
        asm.mov_imm8(Reg8::Ah, 0x0E);
        asm.mov_imm16(Reg16::Bx, 0x0007);
        asm.int(0x10);
        asm.pop(Reg16::Bx);
        asm.pop(Reg16::Ax);
        asm.iret();
    })?;
    rom.code(entry::INT2F, |asm| kernel_entry(asm, call::INT2F))?;
    rom.code(entry::INT1B, |asm| {
        asm.host_call(call::INT1B);
        asm.iret();
    })?;
    rom.code(entry::INT13, |asm| {
        asm.host_call(call::INT13);
        asm.jmp_far(crate::bios::layout::SEGMENT, bios::HALT);
    })?;
    rom.code(entry::STRATEGY, |asm| {
        asm.host_call(call::STRATEGY);
        asm.retf();
    })?;
    rom.code(entry::NUL, |asm| {
        asm.host_call(call::NUL);
        asm.retf();
    })?;
    rom.code(entry::DRIVER_STEP, |asm| {
        asm.host_call(call::DRIVER_STEP);
    })?;
    rom.code(entry::DRIVER_EXIT, |asm| {
        asm.release(DRIVER_LOCALS);
        restore(asm);
        asm.retf();
    })?;
    rom.code(entry::IDLE, |asm| {
        asm.sti();
        asm.hlt();
        asm.retf();
    })?;
    rom.code(entry::BREAK_RETURN, |asm| {
        asm.host_call(call::BREAK_RETURN);
        asm.jmp_far(crate::bios::layout::SEGMENT, bios::HALT);
    })?;
    tables(rom)?;
    for device in Builtin::ALL {
        rom.code(driver_entry(device), |asm| {
            driver(asm, call::DRIVER + device as u16);
        })?;
    }
    rom.code(entry::ANSI, |asm| driver(asm, call::ANSI))?;
    rom.code(entry::ANSI_FAST, |asm| driver(asm, call::ANSI_FAST))?;
    stacks(rom)?;
    mouse(rom)?;
    programs(rom)
}

/// The entries the programs whose behaviour is Rust start, step and handle
/// interrupts through.
fn programs(rom: &mut Image) -> Result<(), BuildError> {
    rom.code(entry::PROGRAM, |asm| {
        asm.host_call(call::PROGRAM);
        asm.jmp_far(crate::bios::layout::SEGMENT, bios::HALT);
    })?;
    rom.code(entry::PROGRAM_STEP, |asm| {
        asm.host_call(call::PROGRAM_STEP);
        asm.jmp_far(crate::bios::layout::SEGMENT, bios::HALT);
    })?;
    for n in 0..super::program::at::HANDLER_COUNT {
        rom.code(
            entry::PROGRAM_HANDLER + n * entry::PROGRAM_HANDLER_STRIDE,
            |asm| {
                save(asm);
                asm.host_call(call::PROGRAM_HANDLER + n);
                asm.jmp_far(crate::bios::layout::SEGMENT, bios::HALT);
            },
        )?;
    }
    rom.code(entry::PROGRAM_EXIT, |asm| {
        restore(asm);
        asm.pop_seg(Seg::Ds);
        asm.iret();
    })?;
    rom.code(entry::PROGRAM_EXIT_BREAK, |asm| {
        restore(asm);
        asm.pop_seg(Seg::Ds);
        asm.stc();
        asm.retf();
    })?;
    rom.code(entry::PROGRAM_REFUSED, |asm| {
        let message = asm.label();
        asm.push_seg(Seg::Cs);
        asm.pop_seg(Seg::Ds);
        asm.mov_label(Reg16::Dx, &message);
        asm.mov_imm8(Reg8::Ah, 0x09);
        asm.int(0x21);
        asm.mov_imm16(Reg16::Ax, 0x4C08);
        asm.int(0x21);
        asm.jmp_far(crate::bios::layout::SEGMENT, bios::HALT);
        asm.bind(&message);
        asm.bytes(super::message::MEMORY);
        asm.bytes(b"$");
    })
}

/// `MOUSE.COM`'s entries and messages.
fn mouse(rom: &mut Image) -> Result<(), BuildError> {
    rom.code(entry::MOUSE, |asm| driver(asm, call::MOUSE))?;
    rom.code(entry::MOUSE_IRQ, |asm| driver(asm, call::MOUSE_IRQ))?;
    for (at, n) in [
        (entry::MOUSE_LOOK, call::MOUSE_LOOK),
        (entry::MOUSE_PROBE, call::MOUSE_PROBE),
        (entry::MOUSE_PROBE_STEP, call::MOUSE_PROBE_STEP),
        (entry::MOUSE_KEEP, call::MOUSE_KEEP),
    ] {
        rom.code(at, |asm| {
            asm.host_call(n);
            asm.retf();
        })?;
    }
    rom.place(
        entry::MOUSE_INSTALLED,
        b"TAIRiX Mouse driver installed\r\n$",
    )?;
    rom.place(
        entry::MOUSE_ALREADY,
        b"A mouse driver is already installed\r\n$",
    )?;
    rom.place(
        entry::MOUSE_ABSENT,
        b"Mouse not found: driver not installed\r\n$",
    )
}

/// The entries `STACKS=` switches its vectors through.
fn stacks(rom: &mut Image) -> Result<(), BuildError> {
    for n in 0..super::stacks::HOOKED_COUNT {
        rom.code(entry::STACKS + n * entry::STACKS_STRIDE, |asm| {
            asm.host_call(call::STACK_IN + n);
            asm.jmp_to(entry::STACK_OVERFLOW);
        })?;
    }
    rom.code(entry::STACK_OUT, |asm| {
        asm.host_call(call::STACK_OUT);
        asm.iret();
    })?;
    rom.code(entry::STACK_OVERFLOW, |asm| {
        halt_saying(asm, b"\r\nInternal stack overflow\r\nSystem halted\0");
    })?;
    rom.code(entry::STACK_FAILURE, |asm| {
        halt_saying(asm, b"\r\nInternal stack failure\r\nSystem halted\0");
    })
}

/// Writes the ASCIZ text at DS:SI through INT 10h's teletype, as code that
/// runs before DOS is there or after it has given up.
pub(crate) fn teletype(asm: &mut Asm) {
    let next = asm.label();
    let shown = asm.label();
    asm.bind(&next);
    asm.lodsb();
    asm.test8(Reg8::Al);
    asm.jcc(Cond::E, &shown);
    asm.mov_imm8(Reg8::Ah, 0x0E);
    asm.mov_imm16(Reg16::Bx, 0x0007);
    asm.int(0x10);
    asm.jmp_short(&next);
    asm.bind(&shown);
}

/// Says the ASCIZ `text` and stops the machine.
fn halt_saying(asm: &mut Asm, text: &[u8]) {
    let message = asm.label();
    asm.push_seg(Seg::Cs);
    asm.pop_seg(Seg::Ds);
    asm.cld();
    asm.mov_label(Reg16::Si, &message);
    teletype(asm);
    asm.jmp_far(crate::bios::layout::SEGMENT, bios::HALT);
    asm.bind(&message);
    asm.bytes(text);
}

/// A driver's entry: the caller's registers saved, the driver's locals
/// reserved, its host call `n`, and the way out the drivers share.
fn driver(asm: &mut Asm, n: u16) {
    save(asm);
    asm.reserve(DRIVER_LOCALS);
    asm.host_call(n);
    asm.jmp_to(entry::DRIVER_EXIT);
}

/// The country's routine and tables, and the entries the kernel reaches
/// only from its own data: the CP/M call, the boot's return, `^C`'s text.
fn tables(rom: &mut Image) -> Result<(), BuildError> {
    rom.code(entry::CASE_MAP, |asm| {
        let done = asm.label();
        asm.cmp_imm8(Reg8::Al, 0x80);
        asm.jcc(Cond::B, &done);
        asm.push(Reg16::Bx);
        asm.mov_imm16(Reg16::Bx, (entry::UPPER + 2).wrapping_sub(0x80));
        asm.xlat_cs();
        asm.pop(Reg16::Bx);
        asm.bind(&done);
        asm.retf();
    })?;
    rom.place(entry::UPPER, &country::upper_record())?;
    rom.place(entry::TERMINATORS, &country::terminator_record())?;
    rom.place(entry::COLLATE, &country::collate_record())?;
    rom.place(entry::DBCS, &country::DBCS_RECORD)?;
    rom.place(entry::BREAK_TEXT, BREAK_TEXT)?;
    rom.code(entry::CALL5, |asm| {
        asm.host_call(call::CALL5);
        asm.jmp_far(crate::bios::layout::SEGMENT, bios::HALT);
    })?;
    rom.code(entry::BOOT_RETURN, |asm| {
        asm.cli();
        asm.host_call(call::BOOT_RETURN);
        asm.jmp_far(crate::bios::layout::SEGMENT, bios::HALT);
    })
}
