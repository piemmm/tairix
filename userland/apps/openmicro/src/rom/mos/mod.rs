//! The clean-room MOS: the operating system ROM, equivalent to MOS 1.20 at
//! its documented interfaces and workspace on the Model B, and to Electron OS
//! 1.00 on the Electron (`plans/BEEB.md` §3). One source builds both: every
//! part their interfaces share is one routine, and only the hardware's own
//! parts differ.

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
pub(crate) mod ws;

#[cfg(test)]
mod tests;

use crate::machine::bbc::sheila;
use crate::machine::{Model, IO_BASE, IO_END, OS_BASE, ROM_SIZE};
use crate::rom::asm::{Asm, AsmError, Image, ERASED};

/// The routine each vector holds after a reset, in vector order, and the
/// address MOS 1.20's table gives for it: the Model B's table gives those
/// addresses, where software calls them, and each leads to the routine.
const VECTOR_DEFAULTS: [(&str, u16); 27] = [
    ("bad_command", 0xE310),
    ("brk_default", 0xDC54),
    ("irq1_default", 0xDC93),
    ("irq2_default", 0xDE89),
    ("oscli_default", 0xDF89),
    ("osbyte_default", 0xE772),
    ("osword_default", 0xE7EB),
    ("oswrch_default", 0xE0A4),
    ("osrdch_default", 0xDEC5),
    ("cfs_file", 0xF27D),
    ("cfs_args", 0xF18E),
    ("cfs_bget", 0xF4C9),
    ("cfs_bput", 0xF529),
    (NOTHING, 0xFFA6),
    ("cfs_find", 0xF3CA),
    ("cfs_fsc", 0xF1B1),
    (NOTHING, 0xFFA6),
    (NOTHING, 0xFFA6),
    (NOTHING, 0xFFA6),
    (NOTHING, 0xFFA6),
    ("keyv_default", 0xEF02),
    ("insv_default", 0xE4B3),
    ("remv_default", 0xE464),
    ("cnpv_default", 0xE1D1),
    (NOTHING, 0xFFA6),
    (NOTHING, 0xFFA6),
    (NOTHING, 0xFFA6),
];

/// The routine of a vector that does nothing: a return.
const NOTHING: &str = "return";

/// What emits a routine for a model.
type Emit = fn(&mut Asm, Model);

/// The default routines the Model B places whole at MOS 1.20's address for
/// them: each short and self-contained, so the code flowing past it is not
/// pushed beyond its branches' reach. The Electron places them with the
/// rest.
const PLACED: [(&str, Emit); 10] = [
    ("oswrch_default", |a, _| serial::oswrch_default(a)),
    ("osrdch_default", |a, _| keyboard::osrdch_default(a)),
    ("brk_default", |a, _| reset::brk_default(a)),
    ("irq2_default", irq::irq2_default),
    ("cnpv_default", buffers::count_purge),
    ("remv_default", buffers::remove),
    ("insv_default", buffers::insert),
    ("cfs_file", |a, _| cfs::cfs_file(a)),
    ("cfs_args", |a, _| cfs::cfs_args(a)),
    ("cfs_fsc", |a, _| cfs::cfs_fsc(a)),
];

/// The Model B's vector entries at MOS 1.20's addresses: each routine
/// placed there whole, or a jump to it, and a return where the vector does
/// nothing.
fn vector_entries(a: &mut Asm) {
    for (index, &(routine, address)) in VECTOR_DEFAULTS.iter().enumerate() {
        if VECTOR_DEFAULTS[..index]
            .iter()
            .any(|&(_, earlier)| earlier == address)
        {
            continue;
        }
        let placed = PLACED.iter().find(|&&(name, _)| name == routine);
        a.pin(address, |a| match placed {
            Some(&(_, emit)) => emit(a, Model::ModelB),
            None if routine == NOTHING => {
                a.here(NOTHING);
                a.rts();
            }
            None => a.jmp(routine),
        });
    }
}

/// Whether `model` has the teletext mode, MODE 7, whose screen holds
/// characters rather than pixels: the one mode whose COLOURS is zero.
const fn has_teletext(model: Model) -> bool {
    matches!(model, Model::ModelB)
}

/// Return at once in the teletext mode, which has no pixels or colours to
/// set; a model without it needs no test.
fn return_in_teletext(a: &mut Asm, model: Model) {
    if has_teletext(model) {
        a.lda(crate::rom::asm::abs(ws::vdu::COLOURS));
        a.beq(crate::rom::asm::RTS);
    }
}

/// A System VIA register's address.
const fn svia(register: u8) -> u16 {
    sheila::SYSTEM_VIA + register as u16
}

/// A User VIA register's address.
const fn uvia(register: u8) -> u16 {
    sheila::USER_VIA + register as u16
}

/// The System VIA port B value setting addressable latch bit `bit`, one of
/// [`crate::machine::bbc::latch`]'s, to `high`: the bit's number, its level in
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

/// Build the Model B's MOS image.
///
/// # Errors
///
/// The builder's first [`AsmError`].
pub fn model_b() -> Result<Image, AsmError> {
    build(Model::ModelB)
}

/// Build the Electron's OS image.
///
/// # Errors
///
/// The builder's first [`AsmError`].
pub fn electron() -> Result<Image, AsmError> {
    build(Model::Electron)
}

/// Build the operating system for `model`.
///
/// # Errors
///
/// The builder's first [`AsmError`].
pub fn build(model: Model) -> Result<Image, AsmError> {
    let mut a = Asm::new(OS_BASE, ROM_SIZE);
    if model == Model::ModelB {
        vector_entries(&mut a);
        reset::pin_vector_defaults(&mut a);
        modes::pin_rows_640(&mut a);
        vdu::pin_mode(&mut a);
    }
    font::emit(&mut a);
    if model == Model::Electron {
        a.here(NOTHING);
        a.rts();
        for (_, emit) in PLACED {
            emit(&mut a, model);
        }
    }
    reset::emit(&mut a, model);
    common(&mut a);
    if model == Model::Electron {
        electron_hardware(&mut a);
    }
    irq::emit(&mut a, model);
    buffers::emit(&mut a, model);
    keyboard::emit(&mut a, model);
    copy::emit(&mut a, model);
    vdu::emit(&mut a, model);
    graphics::emit(&mut a, model);
    modes::emit(&mut a, model);
    osbyte::emit(&mut a, model);
    osword::emit(&mut a, model);
    cli::emit(&mut a, model);
    serial::emit(&mut a, model);
    sound::emit(&mut a, model);
    if model == Model::ModelB {
        adc::emit(&mut a);
    }
    cfs::emit(&mut a, model);
    // The top page first, so what it leaves free takes held tables.
    let flow_end = a.pc();
    high::emit(&mut a, model);
    a.org(flow_end);
    a.place_later();
    // The hole is reserved, so code running into it is refused.
    a.org(IO_BASE);
    a.fill(usize::from(IO_END - IO_BASE), ERASED);
    a.finish()
}

/// A register holding a ROM number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    A,
    X,
    Y,
}

/// OSBYTE's and OSWORD's entries: each call is dispatched with interrupts
/// disabled, as MOS 1.20 dispatches them, so an interrupt routine's own calls
/// meet it only where it waits for an interrupt. A, X and Y are kept in the
/// call registers and the caller's interrupt state comes back with the
/// call's result flags. OSWORD goes on to `osword_dispatch` with A reloaded;
/// OSBYTE's dispatch follows this.
fn masked_calls(a: &mut Asm) {
    use crate::cpu::flag;
    use crate::rom::asm::{imm, zp};
    use ws::os;
    a.here("osbyte_default");
    a.php();
    a.sei();
    a.clc();
    a.bcc("masked_call");
    a.here("osword_default");
    a.php();
    a.sei();
    a.sec();
    // C says which call it is, from here to the dispatch.
    a.here("masked_call");
    a.sta(zp(os::CALL_A));
    a.stx(zp(os::CALL_X));
    a.sty(zp(os::CALL_Y));
    a.pla();
    a.and(imm(flag::I));
    a.bne("masked_dispatch");
    a.jsr("masked_dispatch");
    a.cli();
    a.rts();
    a.here("masked_dispatch");
    a.lda(zp(os::CALL_A));
    a.bcc("osbyte_dispatch");
    a.jmp("osword_dispatch");
    a.here("osbyte_dispatch");
}

/// Page in the ROM `held` holds, noting it at &F4 first, where an interrupt
/// reads it: inline on the Model B, through ROM select; on the Electron
/// through `page_f4`, every register kept.
fn page(a: &mut Asm, model: Model, held: Held) {
    use crate::rom::asm::{abs, zp};
    use ws::os;
    match (model, held) {
        (Model::ModelB, Held::A) => {
            a.sta(zp(os::ROM));
            a.sta(abs(sheila::ROMSEL));
        }
        (Model::ModelB, Held::X) => {
            a.stx(zp(os::ROM));
            a.stx(abs(sheila::ROMSEL));
        }
        (Model::ModelB, Held::Y) => {
            a.sty(zp(os::ROM));
            a.sty(abs(sheila::ROMSEL));
        }
        (Model::Electron, Held::A) => a.jsr("page_a"),
        (Model::Electron, Held::X) => a.jsr("page_x"),
        (Model::Electron, Held::Y) => a.jsr("page_y"),
    }
}

/// The Electron's paging and `&FE07`. The ROM at &F4 is paged through ROM
/// 12, since one of the keyboard's and BASIC's numbers may only be left for
/// another of 8–15, and any may be left from 12. `&FE07`, which cannot be
/// read, is written from its copy: `ula_control` keeps the bits X has set
/// and sets those A has, interrupts off for the while. A, X and Y are kept.
fn electron_hardware(a: &mut Asm) {
    use crate::machine::electron::sheila::{CONTROL, PAGING};
    use crate::machine::electron::FREE_ROM;
    use crate::rom::asm::{abs, imm, zp};
    use ws::{os, var};
    a.here("ula_control");
    a.php();
    a.sei();
    a.pha();
    a.txa();
    a.pha();
    a.tsx();
    a.lda(abs(var::ULA_CONTROL_COPY));
    a.and(abs(0x0101).x());
    a.ora(abs(0x0102).x());
    a.sta(abs(var::ULA_CONTROL_COPY));
    a.sta(abs(CONTROL));
    a.pla();
    a.tax();
    a.pla();
    a.plp();
    a.rts();

    a.here("page_x");
    a.stx(zp(os::ROM));
    a.jmp("page_f4");
    a.here("page_y");
    a.sty(zp(os::ROM));
    a.jmp("page_f4");
    a.here("page_a");
    a.sta(zp(os::ROM));
    a.fall_into("page_f4");
    a.here("page_f4");
    a.pha();
    a.lda(imm(FREE_ROM));
    a.sta(abs(PAGING));
    a.lda(zp(os::ROM));
    a.sta(abs(PAGING));
    a.pla();
    a.rts();
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
