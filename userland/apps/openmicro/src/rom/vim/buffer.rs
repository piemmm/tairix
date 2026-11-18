//! The text: a gap buffer from OSHWM, with the undo record and, unless
//! sideways RAM holds it, the register against HIMEM, and the walks across
//! it.
//!
//! A place in the text is its offset from the start, whatever side of the
//! gap it lies on; [`phys`] turns one into an address. The gap moves only
//! when an edit needs it where the edit is.

use super::ops::{add16, cmp16, dec16, inc16, mov16, sub16, zero16};
use super::paging::Ram;
use super::ws::{page, zp as z};
use crate::rom::asm::{abs, imm, zp, Asm, RTS};
use crate::rom::far;
use crate::rom::os::{zp as os, CR};

/// The line [`fetch_line`] copies a line into, ending in CR.
pub(super) const LINE_BUF: u16 = page::CONVERT;

pub(super) fn emit(a: &mut Asm, ram: &Ram) {
    reset(a);
    places(a);
    phys(a);
    steps(a);
    fetch_line(a);
    lines(a);
    memmove(a, ram);
    gap(a);
    regions(a);
    edits(a, ram);
    register_edits(a, ram);
}

/// `buf_reset` empties the text, the undo record and the register;
/// `buf_empty` the text and the undo record, keeping the register.
fn reset(a: &mut Asm) {
    a.here("buf_reset");
    mov16(a, z::REG, z::LIMIT);
    a.lda(imm(0));
    a.sta(zp(z::LINEWISE));
    a.sta(abs(page::REG_LEN));
    a.sta(abs(page::REG_LEN + 1));
    a.here("buf_empty");
    mov16(a, z::GAP, z::BUF);
    mov16(a, z::TEND, z::REG);
    mov16(a, z::GEND, z::TEND);
    a.lda(imm(0));
    for v in [z::LEN, z::GOFF, z::CUR, z::LSTART, z::TOP] {
        a.sta(zp(v));
        a.sta(zp(v + 1));
    }
    for v in [z::COL, z::WANT, z::LEFT, z::UNDO_OK] {
        a.sta(zp(v));
    }
    a.sta(zp(z::LINE + 1));
    a.sta(zp(z::TOPLN + 1));
    a.lda(imm(1));
    a.sta(zp(z::LINE));
    a.sta(zp(z::TOPLN));
    a.rts();
}

/// The moves the editor makes most, each named for its destination and its
/// source and acting as `mov16` would; `range_len`: N1 the length from
/// RSTART to REND; `n0_vs_len`: carry set where N0 is at or past the text's
/// end. X and Y kept.
fn places(a: &mut Asm) {
    for (name, to, from) in [
        ("n0_cur", z::N0, z::CUR),
        ("n0_lstart", z::N0, z::LSTART),
        ("n0_rstart", z::N0, z::RSTART),
        ("n0_top", z::N0, z::TOP),
        ("n0_len", z::N0, z::LEN),
        ("n0_upos", z::N0, z::UPOS),
        ("n0_n2", z::N0, z::N2),
        ("n1_n2", z::N1, z::N2),
        ("n2_n0", z::N2, z::N0),
        ("rend_n0", z::REND, z::N0),
        ("rstart_n0", z::RSTART, z::N0),
        ("p1_gend", z::P1, z::GEND),
        ("p2_gap", z::P2, z::GAP),
        ("n1_count", z::N1, z::COUNT),
        ("n0_hold", z::N0, z::HOLD),
        ("hold_n1", z::HOLD, z::N1),
        ("hold_n0", z::HOLD, z::N0),
        ("cur_n0", z::CUR, z::N0),
    ] {
        a.here(name);
        mov16(a, to, from);
        a.rts();
    }
    a.here("range_len");
    sub16(a, z::N1, z::REND, z::RSTART);
    a.rts();
    a.here("n0_vs_len");
    cmp16(a, z::N0, z::LEN);
    a.rts();
}

/// `phys`: the address of the text's byte at N0, in P0. `getc`: that
/// byte, in A, with Y 0.
fn phys(a: &mut Asm) {
    a.here("phys");
    cmp16(a, z::N0, z::GOFF);
    let after = a.local();
    a.bcs(after);
    add16(a, z::P0, z::BUF, z::N0);
    a.rts();
    a.bind(after);
    sub16(a, z::P0, z::N0, z::GOFF);
    add16(a, z::P0, z::P0, z::GEND);
    a.rts();

    a.here("getc");
    a.jsr("phys");
    a.ldy(imm(0));
    a.lda(zp(z::P0).ind_y());
    a.rts();
}

/// `fwd` steps P0 to the next byte's address, over the gap; `back` to the
/// one before.
fn steps(a: &mut Asm) {
    a.here("fwd");
    inc16(a, z::P0);
    a.lda(zp(z::P0));
    a.cmp(zp(z::GAP));
    a.bne(RTS);
    a.lda(zp(z::P0 + 1));
    a.cmp(zp(z::GAP + 1));
    a.bne(RTS);
    mov16(a, z::P0, z::GEND);
    a.rts();

    a.here("back");
    let down = a.local();
    a.lda(zp(z::P0));
    a.cmp(zp(z::GEND));
    a.bne(down);
    a.lda(zp(z::P0 + 1));
    a.cmp(zp(z::GEND + 1));
    a.bne(down);
    mov16(a, z::P0, z::GAP);
    a.bind(down);
    dec16(a, z::P0);
    a.rts();
}

/// `fetch_line`: the line starting at N0 into [`LINE_BUF`], to its CR or
/// the text's end, ending in CR; its length in [`z::LLEN`] and X. N0 is
/// kept.
fn fetch_line(a: &mut Asm) {
    a.here("fetch_line");
    a.ldx(imm(0));
    let end = a.local();
    a.jsr("n0_vs_len");
    a.bcs(end);
    a.jsr("phys");
    let next = a.here_local();
    a.ldy(imm(0));
    a.lda(zp(z::P0).ind_y());
    a.cmp(imm(CR));
    a.beq(end);
    a.sta(abs(LINE_BUF).x());
    a.inx();
    a.cpx(imm(0xFF));
    a.beq(end);
    a.jsr("fwd");
    a.lda(zp(z::P0));
    a.cmp(zp(z::TEND));
    a.bne(next);
    a.lda(zp(z::P0 + 1));
    a.cmp(zp(z::TEND + 1));
    a.bne(next);
    a.bind(end);
    a.lda(imm(CR));
    a.sta(abs(LINE_BUF).x());
    a.stx(zp(z::LLEN));
    a.rts();
}

/// The line walks, each on N0.
fn lines(a: &mut Asm) {
    // `line_start`: the start of N0's line.
    a.here("line_start");
    zero16(a, z::N0);
    a.beq(RTS);
    a.jsr("phys");
    let step = a.here_local();
    a.jsr("back");
    a.ldy(imm(0));
    a.lda(zp(z::P0).ind_y());
    a.cmp(imm(CR));
    a.beq(RTS);
    dec16(a, z::N0);
    zero16(a, z::N0);
    a.bne(step);
    a.rts();

    // `line_end`: the CR ending N0's line, or the text's end.
    a.here("line_end");
    a.jsr("n0_vs_len");
    a.bcs(RTS);
    a.jsr("phys");
    let scan = a.here_local();
    a.ldy(imm(0));
    a.lda(zp(z::P0).ind_y());
    a.cmp(imm(CR));
    a.beq(RTS);
    inc16(a, z::N0);
    a.jsr("n0_vs_len");
    a.bcs(RTS);
    a.jsr("fwd");
    a.jmp(scan);

    // `next_line`: the start of the line after N0's, carry clear; carry
    // set where N0's line is the last.
    a.here("next_line");
    a.jsr("line_end");
    inc16(a, z::N0);
    a.jsr("n0_vs_len");
    a.bcs(RTS);
    a.clc();
    a.rts();

    // `prev_line`: the start of the line before the one starting at N0,
    // carry clear; carry set where there is none.
    a.here("prev_line");
    zero16(a, z::N0);
    let some = a.local();
    a.bne(some);
    a.sec();
    a.rts();
    a.bind(some);
    dec16(a, z::N0);
    a.jsr("line_start");
    a.clc();
    a.rts();

    // `count_crs`: the CRs from N0 up to N1, added to N2; N0 left at N1.
    a.here("count_crs");
    cmp16(a, z::N0, z::N1);
    a.bcs(RTS);
    a.jsr("phys");
    let count = a.here_local();
    a.ldy(imm(0));
    a.lda(zp(z::P0).ind_y());
    a.cmp(imm(CR));
    let other = a.local();
    a.bne(other);
    inc16(a, z::N2);
    a.bind(other);
    a.jsr("fwd");
    inc16(a, z::N0);
    cmp16(a, z::N0, z::N1);
    a.bcc(count);
    a.rts();
}

/// `memmove`: N1 bytes from P1 to P2, overlapping either way.
fn memmove(a: &mut Asm, ram: &Ram) {
    a.here("memmove");
    cmp16(a, z::P2, z::P1);
    let down = a.local();
    a.bcs(down);
    a.jmp(abs(ram.copy));

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
    let pages = a.local();
    a.beq(pages);
    let byte_down = a.here_local();
    a.dey();
    a.lda(zp(z::P1).ind_y());
    a.sta(zp(z::P2).ind_y());
    a.tya();
    a.bne(byte_down);
    a.bind(pages);
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
}

/// `move_gap`: the gap moved to N0, N1 kept. `gap_room`: carry set where
/// the gap holds fewer than N1 bytes.
fn gap(a: &mut Asm) {
    a.here("move_gap");
    a.lda(zp(z::N1));
    a.pha();
    a.lda(zp(z::N1 + 1));
    a.pha();
    a.jsr("shift_gap");
    a.pla();
    a.sta(zp(z::N1 + 1));
    a.pla();
    a.sta(zp(z::N1));
    a.rts();

    a.here("shift_gap");
    a.lda(zp(z::N0));
    a.cmp(zp(z::GOFF));
    let differ = a.local();
    a.bne(differ);
    a.lda(zp(z::N0 + 1));
    a.cmp(zp(z::GOFF + 1));
    a.beq(RTS);
    a.bind(differ);
    cmp16(a, z::N0, z::GOFF);
    let left = a.local();
    a.bcc(left);
    // Right: the bytes after the gap come before it.
    sub16(a, z::N1, z::N0, z::GOFF);
    a.jsr("p1_gend");
    a.jsr("p2_gap");
    a.jsr("memmove");
    sub16(a, z::N1, z::N0, z::GOFF);
    add16(a, z::GAP, z::GAP, z::N1);
    add16(a, z::GEND, z::GEND, z::N1);
    mov16(a, z::GOFF, z::N0);
    a.rts();
    // Left: the bytes before the gap go after it.
    a.bind(left);
    sub16(a, z::N1, z::GOFF, z::N0);
    sub16(a, z::P1, z::GAP, z::N1);
    sub16(a, z::P2, z::GEND, z::N1);
    a.jsr("memmove");
    sub16(a, z::N1, z::GOFF, z::N0);
    sub16(a, z::GAP, z::GAP, z::N1);
    sub16(a, z::GEND, z::GEND, z::N1);
    mov16(a, z::GOFF, z::N0);
    a.rts();

    a.here("gap_room");
    sub16(a, z::P0, z::GEND, z::GAP);
    cmp16(a, z::P0, z::N1);
    let room = a.local();
    a.bcs(room);
    a.sec();
    a.rts();
    a.bind(room);
    a.clc();
    a.rts();
}

/// The undo record and the register resized, the text after the gap moved
/// to keep each against the next: carry set where the gap cannot give the
/// room, nothing changed.
fn regions(a: &mut Asm) {
    // `block_down`: the bytes from GEND to P3 moved N2 down into the gap,
    // GEND and TEND with them.
    a.here("block_down");
    a.jsr("n1_n2");
    a.jsr("gap_room");
    a.bcs(RTS);
    a.jsr("p1_gend");
    sub16(a, z::P2, z::GEND, z::N2);
    sub16(a, z::N1, z::P3, z::GEND);
    a.jsr("memmove");
    sub16(a, z::GEND, z::GEND, z::N2);
    sub16(a, z::TEND, z::TEND, z::N2);
    a.clc();
    a.rts();

    // `block_up`: the same moved N2 up.
    a.here("block_up");
    a.jsr("p1_gend");
    add16(a, z::P2, z::GEND, z::N2);
    sub16(a, z::N1, z::P3, z::GEND);
    a.jsr("memmove");
    add16(a, z::GEND, z::GEND, z::N2);
    add16(a, z::TEND, z::TEND, z::N2);
    a.clc();
    a.rts();

    // `resize_undo`: the undo record made N1 long.
    a.here("resize_undo");
    sub16(a, z::N2, z::REG, z::TEND);
    mov16(a, z::P3, z::TEND);
    cmp16(a, z::N1, z::N2);
    let smaller = a.local();
    a.bcc(smaller);
    sub16(a, z::N2, z::N1, z::N2);
    a.jmp("block_down");
    a.bind(smaller);
    sub16(a, z::N2, z::N2, z::N1);
    a.jmp("block_up");

    // `resize_reg`: the register made N1 long.
    a.here("resize_reg");
    sub16(a, z::N2, z::LIMIT, z::REG);
    mov16(a, z::P3, z::REG);
    cmp16(a, z::N1, z::N2);
    let less = a.local();
    a.bcc(less);
    sub16(a, z::N2, z::N1, z::N2);
    a.jsr("block_down");
    a.bcs(RTS);
    sub16(a, z::REG, z::REG, z::N2);
    a.clc();
    a.rts();
    a.bind(less);
    sub16(a, z::N2, z::N2, z::N1);
    a.jsr("block_up");
    add16(a, z::REG, z::REG, z::N2);
    a.clc();
    a.rts();
}

/// The edits: each keeps TOP valid, starting the screen from the text's
/// start where an edit falls before it.
fn edits(a: &mut Asm, ram: &Ram) {
    // `edit_at`: an edit at N0 is about to be made.
    a.here("edit_at");
    cmp16(a, z::N0, z::TOP);
    a.bcs(RTS);
    a.lda(imm(0));
    a.sta(zp(z::TOP));
    a.sta(zp(z::TOP + 1));
    a.sta(zp(z::TOPLN + 1));
    a.lda(imm(1));
    a.sta(zp(z::TOPLN));
    a.rts();

    // `ins_char`: A put in at the cursor, the cursor past it; carry set
    // where the text is full.
    a.here("ins_char");
    a.pha();
    a.jsr("n0_cur");
    a.jsr("edit_at");
    a.jsr("move_gap");
    a.lda(zp(z::GAP));
    a.cmp(zp(z::GEND));
    let room = a.local();
    a.bne(room);
    a.lda(zp(z::GAP + 1));
    a.cmp(zp(z::GEND + 1));
    a.bne(room);
    a.pla();
    a.sec();
    a.rts();
    a.bind(room);
    a.pla();
    a.ldy(imm(0));
    a.sta(zp(z::GAP).ind_y());
    inc16(a, z::GAP);
    inc16(a, z::GOFF);
    inc16(a, z::LEN);
    inc16(a, z::CUR);
    a.clc();
    a.rts();

    // `ins_bytes`: N1 bytes from P3, which lies past the text, put in at
    // N0, the command abandoned where the gap cannot hold them. `ins_from`:
    // the same from the bank in SRC_BANK.
    a.here("ins_bytes");
    a.lda(zp(os::ROM));
    a.sta(abs(page::SRC_BANK));
    a.here("ins_from");
    a.jsr("edit_at");
    a.jsr("move_gap");
    mov16(a, z::N2, z::N1);
    a.jsr("gap_room");
    far(a, Asm::bcc, "no_room");
    mov16(a, z::P1, z::P3);
    a.jsr("p2_gap");
    a.jsr("n1_n2");
    a.ldx(abs(page::SRC_BANK));
    a.jsr(ram.bank_copy);
    add16(a, z::GAP, z::GAP, z::N2);
    add16(a, z::GOFF, z::GOFF, z::N2);
    add16(a, z::LEN, z::LEN, z::N2);
    a.rts();

    // `del_range`: the N1 bytes from N0 taken out, carry clear.
    a.here("del_range");
    a.jsr("edit_at");
    a.jsr("move_gap");
    add16(a, z::GEND, z::GEND, z::N1);
    sub16(a, z::LEN, z::LEN, z::N1);
    a.clc();
    a.rts();
}

/// The register and the undo record filled from the text, and a cut.
fn register_edits(a: &mut Asm, ram: &Ram) {
    // `yank`: the N1 bytes from N0 into the register, A whether they are
    // whole lines; the gap left at N0. The command is abandoned where there
    // is no room.
    a.here("yank");
    a.sta(zp(z::T4));
    a.jsr("move_gap");
    mov16(a, z::N0, z::N1);
    // Whole lines end in a CR, which a last line without one is given.
    a.lda(zp(z::T4));
    let sized = a.local();
    a.beq(sized);
    add16(a, z::P0, z::GEND, z::N1);
    zero16(a, z::N1);
    let given = a.local();
    a.beq(given);
    dec16(a, z::P0);
    a.ldy(imm(0));
    a.lda(zp(z::P0).ind_y());
    a.cmp(imm(CR));
    a.beq(sized);
    a.bind(given);
    inc16(a, z::N1);
    a.bind(sized);
    a.jsr("reg_room");
    far(a, Asm::bcc, "no_room");
    a.lda(zp(z::T4));
    a.sta(zp(z::LINEWISE));
    a.lda(zp(z::N1));
    a.sta(abs(page::REG_LEN));
    a.lda(zp(z::N1 + 1));
    a.sta(abs(page::REG_LEN + 1));
    a.jsr("reg_source");
    mov16(a, z::P2, z::P3);
    a.jsr("p1_gend");
    mov16(a, z::N1, z::N0);
    a.jsr(ram.bank_copy);
    // Lengths differ by the CR alone.
    a.lda(abs(page::REG_LEN));
    a.cmp(zp(z::N0));
    let whole = a.local();
    a.beq(whole);
    a.jmp("reg_cr");
    a.bind(whole);
    a.rts();

    // `keep_undo`: the N1 bytes at the gap's end, the text after N0, made
    // the undo record.
    a.here("keep_undo");
    mov16(a, z::N0, z::N1);
    a.jsr("resize_undo");
    a.bcs(RTS);
    a.jsr("p1_gend");
    mov16(a, z::P2, z::TEND);
    mov16(a, z::N1, z::N0);
    a.jsr("memmove");
    a.clc();
    a.rts();

    // `cut`: the N1 bytes from N0 into the register, A whether whole lines,
    // and into the undo record, then taken out, a change made at N0. Where
    // the free memory cannot hold both, the command is abandoned with
    // nothing changed; where it can, neither can fail.
    a.here("cut");
    a.pha();
    a.jsr("cut_room");
    a.jsr("begin_change");
    a.jsr("n2_n0");
    for v in [z::N2, z::N2 + 1, z::N1, z::N1 + 1] {
        a.lda(zp(v));
        a.pha();
    }
    a.tsx();
    a.lda(abs(0x0105).x());
    a.jsr("yank");
    a.tsx();
    a.lda(abs(0x0101).x());
    a.sta(zp(z::N1 + 1));
    a.lda(abs(0x0102).x());
    a.sta(zp(z::N1));
    a.jsr("keep_undo");
    a.pla();
    a.sta(zp(z::N1 + 1));
    a.pla();
    a.sta(zp(z::N1));
    a.pla();
    a.sta(zp(z::N0 + 1));
    a.pla();
    a.sta(zp(z::N0));
    a.pla();
    a.jmp("del_range");
}
