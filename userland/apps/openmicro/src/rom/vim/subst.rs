//! `:s/old/new/` and `:%s/old/new/g`: the range's lines, the cursor's where
//! none is named, each have their first `old`, or with `g` every one, made
//! `new`, as one change `u` undoes. An empty `old` is the last search's;
//! `old` becomes the search for `n`. A range without `old` changes nothing.

use super::ops::{cmp16, inc16, mov16};
use super::ws::{page, zp as z};
use crate::rom::asm::{abs, hi, imm, lo, zp, Asm, Label};
use crate::rom::far;

/// Where the parts' lengths and the `g` flag are kept while substituting:
/// the delimiter's place is taken by the flag, read once the range is found
/// since finding it walks lines with that byte.
const OLD_LEN: u16 = z::T2;
const NEW_LEN: u16 = z::T3;
const DELIMITER: u16 = z::T4;
const EVERY: u16 = z::T4;

pub(super) fn emit(a: &mut Asm) {
    let some = parsed(a);
    ranged(a, some);
    let found = first_match(a);
    substituted(a, found);
    parts(a);
    lengths(a);
}

/// `ex_subst`: the delimiter, then old and new to the next of it, and old's
/// length; the label where old is not empty.
fn parsed(a: &mut Asm) -> Label {
    a.here("ex_subst");
    let bad = a.local();
    a.ldx(zp(z::EX_AT));
    a.lda(abs(page::INPUT).x());
    a.sta(zp(DELIMITER));
    a.beq(bad);
    a.inx();
    a.lda(abs(page::INPUT).x());
    a.cmp(zp(DELIMITER));
    let old_kept = a.local();
    a.beq(old_kept);
    a.lda(imm(lo(page::PATTERN)));
    a.ldy(imm(hi(page::PATTERN)));
    a.jsr("sub_part");
    a.bind(old_kept);
    a.lda(imm(lo(page::REPLACE)));
    a.ldy(imm(hi(page::REPLACE)));
    a.jsr("sub_part_after");
    a.sty(zp(NEW_LEN));
    a.stx(zp(z::EX_AT));
    // The old part's length; none is refused as a search with none is.
    a.ldy(imm(0));
    let measure = a.here_local();
    let measured = a.local();
    a.lda(abs(page::PATTERN).y());
    a.beq(measured);
    a.iny();
    a.bne(measure);
    a.bind(measured);
    a.sty(zp(OLD_LEN));
    a.tya();
    let some = a.local();
    a.bne(some);
    a.jmp("say_no_pattern");
    a.bind(bad);
    a.call_with_text("say", &["Bad substitution"]);
    a.rts();
    some
}

/// The range: from its first line's start to past its last line's CR.
fn ranged(a: &mut Asm, some: Label) {
    a.bind(some);
    a.lda(zp(z::RANGE));
    let ranged = a.local();
    a.bne(ranged);
    mov16(a, z::LINE_A, z::LINE);
    mov16(a, z::LINE_B, z::LINE);
    a.bind(ranged);
    cmp16(a, z::LINE_B, z::LINE_A);
    far(a, Asm::bcs, "ex_bad_range");
    mov16(a, z::COUNT, z::LINE_A);
    a.jsr("goto_line");
    a.jsr("rstart_n0");
    mov16(a, z::COUNT, z::LINE_B);
    a.jsr("goto_line");
    a.jsr("line_end");
    a.jsr("n0_vs_len");
    let at_end = a.local();
    a.bcs(at_end);
    inc16(a, z::N0);
    a.bind(at_end);
    a.jsr("rend_n0");
}

/// A first match, or the text left as it was; the label for one found.
fn first_match(a: &mut Asm) -> Label {
    a.jsr("n0_rstart");
    let find = a.here_local();
    let found = a.local();
    cmp16(a, z::N0, z::REND);
    let missing = a.local();
    a.bcs(missing);
    a.jsr("match_at");
    a.bcs(found);
    inc16(a, z::N0);
    a.jmp(find);
    a.bind(missing);
    a.jmp("not_found");
    found
}

/// The range kept for undoing, then each match from the first made new.
fn substituted(a: &mut Asm, found: Label) {
    a.bind(found);
    a.jsr("hold_n0");
    a.jsr("n0_rstart");
    a.jsr("range_len");
    a.jsr("snapshot");
    mov16(a, z::UINS, z::N1);
    a.ldx(zp(z::EX_AT));
    a.ldy(imm(0));
    a.lda(abs(page::INPUT).x());
    let flagged = a.local();
    a.beq(flagged);
    a.lda(abs(page::INPUT + 1).x());
    a.cmp(imm(b'g'));
    a.bne(flagged);
    a.iny();
    a.bind(flagged);
    a.sty(zp(EVERY));
    a.jsr("n0_hold");
    let each = a.here_local();
    let done = a.local();
    let next = a.local();
    cmp16(a, z::N0, z::REND);
    a.bcs(done);
    a.jsr("match_at");
    a.bcc(next);
    a.lda(zp(OLD_LEN));
    a.sta(zp(z::N1));
    a.lda(imm(0));
    a.sta(zp(z::N1 + 1));
    a.jsr("del_range");
    a.lda(zp(OLD_LEN));
    a.jsr("sub_shrink");
    a.lda(zp(NEW_LEN));
    a.sta(zp(z::N1));
    a.lda(imm(0));
    a.sta(zp(z::N1 + 1));
    a.lda(imm(lo(page::REPLACE)));
    a.sta(zp(z::P3));
    a.lda(imm(hi(page::REPLACE)));
    a.sta(zp(z::P3 + 1));
    a.jsr("ins_bytes");
    a.jsr("hold_n0");
    a.lda(zp(NEW_LEN));
    a.jsr("sub_grow");
    a.lda(zp(EVERY));
    a.bne(each);
    a.jsr("next_line");
    a.bcc(each);
    a.bind(done);
    a.jsr("lines_fit");
    a.jsr("n0_hold");
    a.jsr("line_start");
    a.jmp("land");
    a.bind(next);
    inc16(a, z::N0);
    a.jmp(each);
}

/// `sub_part` and `sub_part_after`.
fn parts(a: &mut Asm) {
    // `sub_part`: the input from X to the delimiter or its end into the
    // page at A (low) and Y (high), ending in 0; its length in Y, X at what
    // ended it. `sub_part_after` the same past the delimiter at X, where
    // one is, or an empty part where the input has ended.
    a.here("sub_part_after");
    a.sta(zp(z::P2));
    a.lda(abs(page::INPUT).x());
    let past = a.local();
    a.beq(past);
    a.inx();
    a.bind(past);
    a.lda(zp(z::P2));
    a.here("sub_part");
    a.sta(zp(z::P2));
    a.sty(zp(z::P2 + 1));
    a.ldy(imm(0));
    let copy = a.here_local();
    let copied = a.local();
    a.lda(abs(page::INPUT).x());
    a.beq(copied);
    a.cmp(zp(DELIMITER));
    a.beq(copied);
    a.sta(zp(z::P2).ind_y());
    a.inx();
    a.iny();
    a.bne(copy);
    a.bind(copied);
    a.lda(imm(0));
    a.sta(zp(z::P2).ind_y());
    a.rts();
}

/// `sub_shrink` and `sub_grow`.
fn lengths(a: &mut Asm) {
    // `sub_shrink`: the range's end and the change's length A less.
    // `sub_grow`: both, and N0, A more.
    a.here("sub_shrink");
    a.sta(zp(z::T0));
    for v in [z::REND, z::UINS] {
        a.lda(zp(v));
        a.sec();
        a.sbc(zp(z::T0));
        a.sta(zp(v));
        let kept = a.local();
        a.bcs(kept);
        a.dec(zp(v + 1));
        a.bind(kept);
    }
    a.rts();
    a.here("sub_grow");
    a.sta(zp(z::T0));
    for v in [z::REND, z::UINS, z::N0] {
        a.lda(zp(v));
        a.clc();
        a.adc(zp(z::T0));
        a.sta(zp(v));
        let kept = a.local();
        a.bcc(kept);
        a.inc(zp(v + 1));
        a.bind(kept);
    }
    a.rts();
}
