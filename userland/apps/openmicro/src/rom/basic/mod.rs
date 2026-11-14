//! The clean-room BASIC: a language ROM equivalent to BBC BASIC II at its
//! documented interfaces and workspace (`plans/BEEB.md` §3).

mod assembler;
mod control;
mod crunch;
mod decimal;
mod editor;
mod errors;
mod exec;
mod expr;
mod files;
mod float;
mod func;
mod io;
mod list;
mod procs;
mod system;
mod tokens;
mod trans;
mod vars;
mod ws;

#[cfg(test)]
mod tests;

use crate::machine::{ROM_SIZE, SIDEWAYS_BASE};
use crate::rom::asm::{abs, byte, imm, lo, zp, Asm, AsmError, Image, RTS};
use crate::rom::os::{entry, rom_type, vector, COPYRIGHT};
use ws::zp as z;

/// BASIC II's ROM type: a language with a relocation address.
const ROM_TYPE: u8 = rom_type::LANGUAGE | rom_type::RELOCATION;
/// BASIC II's version byte.
const VERSION: u8 = 1;
/// Where a second processor would copy the ROM to.
const RELOCATION: u32 = 0x0000_8000;
/// What MOS reason a language is entered with.
const ENTER_LANGUAGE: u8 = 1;

/// The execution address SAVE gives a program file, as BASIC II's does:
/// the cold start the language entry makes.
pub(crate) const SAVED_EXEC: u16 = 0x8023;

/// Build the BASIC image.
///
/// # Errors
///
/// The builder's first [`AsmError`].
pub fn build() -> Result<Image, AsmError> {
    let mut a = Asm::new(SIDEWAYS_BASE, ROM_SIZE);
    header(&mut a);
    text(&mut a);
    editor::emit(&mut a);
    crunch::emit(&mut a);
    list::emit(&mut a);
    exec::emit(&mut a);
    control::emit(&mut a);
    procs::emit(&mut a);
    system::emit(&mut a);
    files::emit(&mut a);
    io::emit(&mut a);
    expr::emit(&mut a);
    func::emit(&mut a);
    vars::emit(&mut a);
    float::emit(&mut a);
    decimal::emit(&mut a);
    trans::emit(&mut a);
    assembler::emit(&mut a);
    errors::emit(&mut a);
    a.finish()
}

/// The language header: the entry code starts the ROM, then the type,
/// copyright offset and version, the title, and the copyright REPORT shows
/// before any error; the header ends where a saved program's entry begins.
fn header(a: &mut Asm) {
    a.cmp(imm(ENTER_LANGUAGE));
    a.beq("entry");
    a.rts();
    a.byte(0);
    a.byte(ROM_TYPE);
    a.byte(lo("copyright"));
    a.byte(VERSION);
    a.text("BASIC");
    a.here("copyright");
    a.byte(0);
    a.text(COPYRIGHT);
    a.bytes(b"\n\r");
    a.byte(0);
    a.bytes(&RELOCATION.to_le_bytes());

    a.org(SAVED_EXEC);
    a.here("entry");
    a.cld();
    a.ldx(imm(0xFF));
    a.txs();
    a.lda(imm(lo("error")));
    a.sta(abs(vector::BRK));
    a.lda(imm(crate::rom::asm::hi("error")));
    a.sta(abs(vector::BRK + 1));
    a.cli();
    a.jmp("cold");
}

/// Reading program text through the two pointers.
fn text(a: &mut Asm) {
    // A = the next character at the statement pointer that is not a space,
    // the pointer past it and Y its offset.
    a.here("next_a");
    a.ldy(zp(z::OFFSET_A));
    let again = a.here_local();
    a.lda(zp(z::TEXT_A).ind_y());
    a.iny();
    a.cmp(imm(b' '));
    a.beq(again);
    a.sty(zp(z::OFFSET_A));
    a.rts();

    // The message X bytes into the messages, to its zero.
    a.here("print_message");
    a.lda(abs("messages").x());
    a.beq(RTS);
    a.jsr(entry::OSASCI);
    a.inx();
    a.bne("print_message");

    a.here("messages");
    for message in MESSAGES {
        a.text(message);
        a.byte(0);
    }
}

/// What `print_message` prints, each ended by a zero, and where each is.
const MESSAGES: [&str; 3] = [" at line ", "Failed at ", "Bad program"];
pub(super) const AT_LINE: u8 = message_at(0);
pub(super) const FAILED_AT: u8 = message_at(1);
pub(super) const BAD_PROGRAM: u8 = message_at(2);

const fn message_at(index: usize) -> u8 {
    let mut at = 0;
    let mut i = 0;
    while i < index {
        at += MESSAGES[i].len() + 1;
        i += 1;
    }
    byte(at)
}
