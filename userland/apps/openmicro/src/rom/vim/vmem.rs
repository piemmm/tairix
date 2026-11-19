//! The text's memory: sixteen-bit addresses from OSHWM, main RAM up to
//! HIMEM and then each bank of sideways RAM the text holds, its pages from
//! [`BANK_FIRST`] in turn. The bank's first page keeps the mark its claim
//! leaves, so a bank's part starts a page in and whole pages map to whole
//! pages: an address past HIMEM becomes a bank's by its high byte alone.
//!
//! Every byte of the text, the undo record and the register is reached
//! through `rd0`, `rdp` and `wrp`, which page a bank in from
//! [`page::RAM_CODE`] where the address lies in one.

use super::ops::{cmp16, inc16, mov16, zero16};
use super::paging::Ram;
use super::ws::{page, zp as z};
use crate::machine::{OS_BASE, SIDEWAYS_BASE};
use crate::rom::asm::{abs, imm, lo, zp, Asm, RTS};
use crate::rom::far;
use crate::rom::os::zp as os;

/// The first page of a bank the text uses, past its claim's page, and the
/// pages a bank gives.
pub(super) const BANK_FIRST: u8 = SIDEWAYS_BASE.to_be_bytes()[0] + 1;
pub(super) const BANK_PAGES: u8 = OS_BASE.to_be_bytes()[0] - BANK_FIRST;

pub(super) fn emit(a: &mut Asm, ram: &Ram) {
    access(a, ram);
    moving(a);
    parts(a, ram);
    mapping(a);
    shifting(a);
}

/// `rd0`: A the byte at P0, X kept and Y 0. `rdp`: A the byte at the
/// pointer whose zero page address A holds, plus Y; X and Y kept. `wrp`: A
/// written at the pointer X names, plus Y; A, X and Y kept. `wr0`: A
/// written at P0, Y 0.
fn access(a: &mut Asm, ram: &Ram) {
    a.here("rd0");
    a.ldy(imm(0));
    a.lda(zp(z::P0 + 1));
    a.cmp(zp(z::MAIN_TOP));
    let banked = a.local();
    a.bcs(banked);
    a.lda(zp(z::P0).ind_y());
    a.rts();
    a.bind(banked);
    a.lda(imm(lo(z::P0)));
    a.fall_into("rdp");

    a.here("rdp");
    a.stx(abs(page::VX));
    a.sty(abs(page::VY));
    a.tax();
    a.jsr("vat");
    let main = a.local();
    let done = a.local();
    a.bcc(main);
    a.jsr("vmap");
    a.jsr(ram.get);
    a.lda(abs(page::VA));
    a.jmp(done);
    a.bind(main);
    a.lda(zp(z::VP).ind_y());
    a.bind(done);
    a.ldx(abs(page::VX));
    a.ldy(abs(page::VY));
    a.rts();

    a.here("wr0");
    a.ldx(imm(lo(z::P0)));
    a.ldy(imm(0));
    a.fall_into("wrp");

    a.here("wrp");
    a.sta(abs(page::VA));
    a.stx(abs(page::VX));
    a.sty(abs(page::VY));
    a.jsr("vat");
    let main = a.local();
    let done = a.local();
    a.bcc(main);
    a.jsr("vmap");
    a.jsr(ram.put);
    a.jmp(done);
    a.bind(main);
    a.lda(abs(page::VA));
    a.sta(zp(z::VP).ind_y());
    a.bind(done);
    a.ldx(abs(page::VX));
    a.ldy(abs(page::VY));
    a.lda(abs(page::VA));
    a.rts();

    // `rdin`: A the byte at BASIC_IN plus Y; X and Y kept. `wrout`: A put at
    // BASIC_OUT, BASIC_OUT moved past it; A and X kept, Y 0.
    a.here("rdin");
    a.lda(imm(lo(z::BASIC_IN)));
    a.jmp("rdp");

    a.here("wrout");
    a.stx(abs(page::VW));
    a.ldx(imm(lo(z::BASIC_OUT)));
    a.ldy(imm(0));
    a.jsr("wrp");
    inc16(a, z::BASIC_OUT);
    a.ldx(abs(page::VW));
    a.rts();

    // `vat`: VP the pointer X names plus Y, Y 0, carry set where it lies
    // past HIMEM.
    a.here("vat");
    a.tya();
    a.clc();
    a.adc(zp(0).x());
    a.sta(zp(z::VP));
    a.lda(zp(1).x());
    a.adc(imm(0));
    a.sta(zp(z::VP + 1));
    a.ldy(imm(0));
    a.cmp(zp(z::MAIN_TOP));
    a.rts();
}

/// `memmove`: N1 bytes from P1 to P2, overlapping either way; N0, N1 and N2
/// kept, P1, P2 and P3 changed. With no bank held the move is main
/// memory's alone; otherwise it goes a part at a time, each ending where
/// either end next crosses a page, so each part lies in main memory or one
/// bank at each end.
fn moving(a: &mut Asm) {
    a.here("memmove");
    a.lda(abs(page::SEGMENTS));
    a.bne("memmove_parts");
    cmp16(a, z::P2, z::P1);
    let down = a.local();
    a.bcs(down);
    // Up the addresses: the whole pages, then the part page.
    a.ldy(imm(0));
    a.ldx(zp(z::N1 + 1));
    let part = a.local();
    a.beq(part);
    let pages = a.here_local();
    a.lda(zp(z::P1).ind_y());
    a.sta(zp(z::P2).ind_y());
    a.iny();
    a.bne(pages);
    a.inc(zp(z::P1 + 1));
    a.inc(zp(z::P2 + 1));
    a.dex();
    a.bne(pages);
    a.bind(part);
    a.ldx(zp(z::N1));
    a.beq(RTS);
    let bytes = a.here_local();
    a.lda(zp(z::P1).ind_y());
    a.sta(zp(z::P2).ind_y());
    a.iny();
    a.dex();
    a.bne(bytes);
    a.rts();

    // Down the addresses: the part page past the whole pages first, then
    // each page below it.
    a.bind(down);
    a.clc();
    a.lda(zp(z::P1 + 1));
    a.adc(zp(z::N1 + 1));
    a.sta(zp(z::P1 + 1));
    a.clc();
    a.lda(zp(z::P2 + 1));
    a.adc(zp(z::N1 + 1));
    a.sta(zp(z::P2 + 1));
    a.ldy(zp(z::N1));
    let whole = a.local();
    a.beq(whole);
    let byte_down = a.here_local();
    a.dey();
    a.lda(zp(z::P1).ind_y());
    a.sta(zp(z::P2).ind_y());
    a.tya();
    a.bne(byte_down);
    a.bind(whole);
    a.ldx(zp(z::N1 + 1));
    a.beq(RTS);
    let page_down = a.here_local();
    a.dec(zp(z::P1 + 1));
    a.dec(zp(z::P2 + 1));
    a.ldy(imm(0));
    let in_page = a.here_local();
    a.dey();
    a.lda(zp(z::P1).ind_y());
    a.sta(zp(z::P2).ind_y());
    a.tya();
    a.bne(in_page);
    a.dex();
    a.bne(page_down);
    a.rts();
    in_parts(a);
}

/// `memmove_parts`: `memmove`'s parts, N1 kept across them; placed after
/// `memmove`, whose branch reaches it.
fn in_parts(a: &mut Asm) {
    a.here("memmove_parts");
    a.lda(zp(z::N1));
    a.pha();
    a.lda(zp(z::N1 + 1));
    a.pha();
    let done = a.local();
    cmp16(a, z::P2, z::P1);
    let backward = a.local();
    far(a, Asm::bcc, backward);
    // First part first, from P1 and P2 to the nearer page's end.
    let forward = a.here_local();
    zero16(a, z::N1);
    far(a, Asm::bne, done);
    a.lda(zp(z::P1));
    a.cmp(zp(z::P2));
    let larger = a.local();
    a.bcs(larger);
    a.lda(zp(z::P2));
    a.bind(larger);
    a.eor(imm(0xFF));
    a.jsr("part_size");
    for (at, bank) in [(z::P2, page::DB), (z::P1, page::SB)] {
        mov16(a, z::VP, at);
        a.jsr("vmap_any");
        a.stx(abs(bank));
        if at == z::P2 {
            mov16(a, z::P3, z::VP);
        }
    }
    a.jsr("part_up");
    a.lda(abs(page::XC));
    let short = a.local();
    a.bne(short);
    a.inc(zp(z::P1 + 1));
    a.inc(zp(z::P2 + 1));
    a.dec(zp(z::N1 + 1));
    a.jmp(forward);
    a.bind(short);
    for at in [z::P1, z::P2] {
        a.clc();
        a.lda(zp(at));
        a.adc(abs(page::XC));
        a.sta(zp(at));
        a.lda(zp(at + 1));
        a.adc(imm(0));
        a.sta(zp(at + 1));
    }
    a.jsr("n1_less_part");
    a.jmp(forward);
    // Last part first, back from both ends to the nearer page's start.
    a.bind(backward);
    let back = a.here_local();
    zero16(a, z::N1);
    far(a, Asm::bne, done);
    for (at, keep) in [(z::P1, true), (z::P2, false)] {
        a.lda(zp(at));
        a.clc();
        a.adc(zp(z::N1));
        a.sec();
        a.sbc(imm(1));
        if keep {
            a.sta(abs(page::VA));
        }
    }
    a.cmp(abs(page::VA));
    let nearer = a.local();
    a.bcc(nearer);
    a.lda(abs(page::VA));
    a.bind(nearer);
    a.jsr("part_size");
    for (at, bank) in [(z::P2, page::DB), (z::P1, page::SB)] {
        a.ldx(imm(lo(at)));
        a.jsr("part_start");
        a.jsr("vmap_any");
        a.stx(abs(bank));
        if at == z::P2 {
            mov16(a, z::P3, z::VP);
        }
    }
    a.jsr("part_down");
    a.lda(abs(page::XC));
    let shorter = a.local();
    a.bne(shorter);
    a.dec(zp(z::N1 + 1));
    a.jmp(back);
    a.bind(shorter);
    a.jsr("n1_less_part");
    a.jmp(back);
    a.bind(done);
    a.pla();
    a.sta(zp(z::N1 + 1));
    a.pla();
    a.sta(zp(z::N1));
    a.rts();
}

/// A move's parts: `part_size` makes A + 1 the part, held no longer than
/// N1, its length in [`page::XC`], 0 for 256; `part_start` VP the part's
/// start back from the end of the pointer X names; `n1_less_part` N1 the
/// part less, the part short of 256. `part_up` and `part_down` copy it with
/// what each end lies in paged in.
fn parts(a: &mut Asm, ram: &Ram) {
    a.here("part_size");
    a.ldx(zp(z::N1 + 1));
    let fits = a.local();
    a.bne(fits);
    a.ldx(zp(z::N1));
    a.dex();
    a.stx(abs(page::XC));
    a.cmp(abs(page::XC));
    a.bcc(fits);
    a.txa();
    a.bind(fits);
    a.clc();
    a.adc(imm(1));
    a.sta(abs(page::XC));
    a.rts();

    a.here("part_start");
    a.clc();
    a.lda(zp(0).x());
    a.adc(zp(z::N1));
    a.sta(zp(z::VP));
    a.lda(zp(1).x());
    a.adc(zp(z::N1 + 1));
    a.sta(zp(z::VP + 1));
    a.sec();
    a.lda(zp(z::VP));
    a.sbc(abs(page::XC));
    a.sta(zp(z::VP));
    a.lda(zp(z::VP + 1));
    a.sbc(imm(0));
    a.sta(zp(z::VP + 1));
    a.lda(abs(page::XC));
    a.bne(RTS);
    a.dec(zp(z::VP + 1));
    a.rts();

    a.here("n1_less_part");
    a.sec();
    a.lda(zp(z::N1));
    a.sbc(abs(page::XC));
    a.sta(zp(z::N1));
    a.lda(zp(z::N1 + 1));
    a.sbc(imm(0));
    a.sta(zp(z::N1 + 1));
    a.rts();

    // Read from main memory, the write's bank paged; written to it, the
    // read's; within one bank, that one; between two, each in turn.
    for (name, one, two) in [
        ("part_up", ram.up, ram.up2),
        ("part_down", ram.down, ram.down2),
    ] {
        a.here(name);
        a.ldx(abs(page::SB));
        a.cpx(zp(os::ROM));
        let by_write = a.local();
        let single = a.local();
        a.beq(by_write);
        a.lda(abs(page::DB));
        a.cmp(zp(os::ROM));
        a.beq(single);
        a.cmp(abs(page::SB));
        a.beq(single);
        a.jmp(abs(two));
        a.bind(by_write);
        a.ldx(abs(page::DB));
        a.bind(single);
        a.jmp(abs(one));
    }
}

/// `vmap_any`: VP made its address in main memory or its bank, X that bank
/// or the editor's own. `vmap`: the same for VP at or past HIMEM.
fn mapping(a: &mut Asm) {
    a.here("vmap_any");
    a.lda(zp(z::VP + 1));
    a.cmp(zp(z::MAIN_TOP));
    a.bcs("vmap");
    a.ldx(zp(os::ROM));
    a.rts();

    a.here("vmap");
    a.lda(zp(z::VP + 1));
    a.sec();
    a.sbc(zp(z::MAIN_TOP));
    a.ldx(imm(0));
    let next = a.here_local();
    let found = a.local();
    a.cmp(imm(BANK_PAGES));
    a.bcc(found);
    a.sbc(imm(BANK_PAGES));
    a.inx();
    a.bne(next);
    a.bind(found);
    a.adc(imm(BANK_FIRST));
    a.sta(zp(z::VP + 1));
    a.lda(abs(page::BANKS).x());
    a.tax();
    a.rts();
}

/// `past_gap_by`: GEND, TEND, REG and LIMIT moved by A pages, the count kept
/// on the stack since every temporary may be held across a bank's taking.
fn shifting(a: &mut Asm) {
    a.here("past_gap_by");
    a.pha();
    for v in [z::GEND, z::TEND, z::REG, z::LIMIT] {
        a.pla();
        a.pha();
        a.clc();
        a.adc(zp(v + 1));
        a.sta(zp(v + 1));
    }
    a.pla();
    a.rts();
}
