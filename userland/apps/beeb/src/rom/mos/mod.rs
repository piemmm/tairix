//! The clean-room MOS: the operating system ROM, equivalent to MOS 1.20 at
//! its documented interfaces and workspace (`plans/BEEB.md` §3).

mod adc;
mod buffers;
mod cfs;
mod cli;
mod copy;
mod font;
mod graphics;
mod high;
mod irq;
mod keyboard;
mod modes;
mod osbyte;
mod osword;
mod reset;
mod serial;
mod sound;
mod vdu;
mod ws;

#[cfg(test)]
mod tests;

use crate::machine::{sheila, IO_BASE, IO_END, OS_BASE, ROM_SIZE};
use crate::rom::asm::{Asm, AsmError, Image, ERASED};

/// The routines each vector holds after a reset, in vector order.
const VECTOR_DEFAULTS: [&str; 27] = [
    "bad_command",
    "brk_default",
    "irq1_default",
    "irq2_default",
    "oscli_default",
    "osbyte_default",
    "osword_default",
    "oswrch_default",
    "osrdch_default",
    "cfs_file",
    "cfs_args",
    "cfs_bget",
    "cfs_bput",
    "return",
    "cfs_find",
    "cfs_fsc",
    "return",
    "return",
    "return",
    "return",
    "keyv_default",
    "insv_default",
    "remv_default",
    "cnpv_default",
    "return",
    "return",
    "return",
];

/// A System VIA register's address.
const fn svia(register: u8) -> u16 {
    sheila::SYSTEM_VIA + register as u16
}

/// A User VIA register's address.
const fn uvia(register: u8) -> u16 {
    sheila::USER_VIA + register as u16
}

/// The System VIA port B value setting addressable latch bit `bit`, one of
/// [`crate::machine::latch`]'s, to `high`: the bit's number, its level in
/// bit 3.
const fn latch(bit: u8, high: bool) -> u8 {
    assert!(bit.is_power_of_two(), "one latch bit");
    let mut number = 0;
    while 1 << number != bit {
        number += 1;
    }
    if high {
        number | LATCH_HIGH
    } else {
        number
    }
}

/// The level bit of a latch write.
const LATCH_HIGH: u8 = 0x08;

/// DELETE's character code.
const DELETE: u8 = 0x7F;

/// Build the MOS image.
///
/// # Errors
///
/// The builder's first [`AsmError`].
pub fn build() -> Result<Image, AsmError> {
    let mut a = Asm::new(OS_BASE, ROM_SIZE);
    font::emit(&mut a);
    reset::emit(&mut a);
    common(&mut a);
    irq::emit(&mut a);
    buffers::emit(&mut a);
    keyboard::emit(&mut a);
    copy::emit(&mut a);
    vdu::emit(&mut a);
    graphics::emit(&mut a);
    modes::emit(&mut a);
    osbyte::emit(&mut a);
    osword::emit(&mut a);
    cli::emit(&mut a);
    serial::emit(&mut a);
    sound::emit(&mut a);
    adc::emit(&mut a);
    cfs::emit(&mut a);
    // The hole is reserved, so code running into it is refused.
    a.org(IO_BASE);
    a.fill(usize::from(IO_END - IO_BASE), ERASED);
    high::emit(&mut a);
    a.finish()
}

/// `count` bytes from each source to each of its destinations, through an
/// X-indexed loop that leaves X negative.
fn copy(a: &mut Asm, sources: &[(u16, &[u16])], count: u8) {
    use crate::rom::asm::{abs, imm};
    a.ldx(imm(count - 1));
    let top = a.here_local();
    for &(from, to) in sources {
        a.lda(abs(from).x());
        for &destination in to {
            a.sta(abs(destination).x());
        }
    }
    a.dex();
    a.bpl(top);
}

/// Routines shared by every part of the MOS.
fn common(a: &mut Asm) {
    use crate::rom::asm::{imm, zp};
    use ws::fs;

    a.here("return");
    a.rts();

    // Print the zero-terminated text following the JSR, then carry on after
    // it, leaving the error pointer naming the last error.
    a.here("print_inline");
    a.pla();
    a.sta(zp(fs::PRINT_POINTER));
    a.pla();
    a.sta(zp(fs::PRINT_POINTER + 1));
    a.here("print_inline_next");
    a.inc(zp(fs::PRINT_POINTER));
    a.bne("print_inline_same_page");
    a.inc(zp(fs::PRINT_POINTER + 1));
    a.here("print_inline_same_page");
    a.ldy(imm(0));
    a.lda(zp(fs::PRINT_POINTER).ind_y());
    a.beq("print_inline_done");
    a.jsr("oswrch");
    a.jmp("print_inline_next");
    a.here("print_inline_done");
    a.lda(zp(fs::PRINT_POINTER + 1));
    a.pha();
    a.lda(zp(fs::PRINT_POINTER));
    a.pha();
    a.rts();
}
