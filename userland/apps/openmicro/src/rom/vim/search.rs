//! Searching: `/` and `?` read a text to find after or before the cursor,
//! `n` and `N` find the last one again either way, each a count's times,
//! wrapping round the text's ends.

use super::ops::{cmp16, dec16, inc16, mov16, zero16};
use super::ws::{page, zp as z};
use crate::rom::asm::{abs, imm, zp, Asm, RTS};

pub(super) fn emit(a: &mut Asm) {
    commands(a);
    finding(a);
    matching(a);
}

fn commands(a: &mut Asm) {
    a.here("cmd_search_forward");
    a.ldx(imm(0));
    a.lda(imm(b'/'));
    let typed = a.local();
    a.bne(typed);
    a.here("cmd_search_backward");
    a.ldx(imm(1));
    a.lda(imm(b'?'));
    a.bind(typed);
    a.stx(zp(z::BACKWARD));
    a.jsr("read_line");
    a.bcs(RTS);
    // An empty text finds the last one again.
    a.lda(abs(page::INPUT));
    let again = a.local();
    a.beq(again);
    a.ldx(imm(0));
    let copy = a.here_local();
    a.lda(abs(page::INPUT).x());
    a.sta(abs(page::PATTERN).x());
    a.beq(again);
    a.inx();
    a.bpl(copy);
    a.bind(again);
    a.lda(zp(z::BACKWARD));
    a.jmp("search_go");

    a.here("cmd_search_next");
    a.lda(zp(z::BACKWARD));
    a.jmp("search_go");

    a.here("cmd_search_previous");
    a.lda(zp(z::BACKWARD));
    a.eor(imm(1));
    a.fall_into("search_go");
}

/// `search_go`: the pattern found a count's times from the cursor, after
/// it where A is 0, before it otherwise, the cursor left on the last found.
/// `match_at`: carry set where the pattern is the text at N0.
fn finding(a: &mut Asm) {
    a.here("search_go");
    a.sta(zp(z::T2));
    a.lda(abs(page::PATTERN));
    let none = a.local();
    let missing = a.local();
    let patterned = a.local();
    a.bne(patterned);
    a.jmp(none);
    a.bind(patterned);
    zero16(a, z::LEN);
    let some = a.local();
    a.bne(some);
    a.jmp(missing);
    a.bind(some);
    a.jsr("count1");
    a.jsr("hold_n1");
    mov16(a, z::REND, z::CUR);
    a.lda(imm(0));
    a.sta(zp(z::T3));
    let next_match = a.here_local();
    mov16(a, z::N0, z::REND);
    mov16(a, z::RSTART, z::REND);
    let step = a.here_local();
    let check = a.local();
    a.lda(zp(z::T2));
    let back = a.local();
    a.bne(back);
    inc16(a, z::N0);
    a.jsr("n0_vs_len");
    a.bcc(check);
    a.lda(imm(0));
    a.sta(zp(z::N0));
    a.sta(zp(z::N0 + 1));
    a.inc(zp(z::T3));
    a.jmp(check);
    a.bind(back);
    zero16(a, z::N0);
    let low = a.local();
    a.bne(low);
    a.jsr("n0_len");
    a.inc(zp(z::T3));
    a.bind(low);
    dec16(a, z::N0);
    // Back where this search started: only the match there is left.
    a.bind(check);
    let found = a.local();
    let try_here = a.local();
    a.lda(zp(z::N0));
    a.cmp(zp(z::RSTART));
    a.bne(try_here);
    a.lda(zp(z::N0 + 1));
    a.cmp(zp(z::RSTART + 1));
    a.bne(try_here);
    a.jsr("match_at");
    a.bcs(found);
    a.jmp(missing);
    a.bind(try_here);
    a.jsr("match_at");
    a.bcc(step);
    a.bind(found);
    a.jsr("rend_n0");
    dec16(a, z::HOLD);
    zero16(a, z::HOLD);
    a.bne(next_match);
    mov16(a, z::N0, z::REND);
    a.jsr("set_cursor");
    a.lda(zp(z::COL));
    a.sta(zp(z::WANT));
    a.lda(zp(z::T3));
    a.beq(RTS);
    a.call_with_text("say", &["Search wrapped"]);
    a.rts();
    a.bind(missing);
    a.here("not_found");
    a.jsr("msg_begin");
    a.call_with_text("msg_text", &["Not found: "]);
    a.ldx(imm(0));
    let copy = a.here_local();
    a.lda(abs(page::PATTERN).x());
    let copied = a.local();
    a.beq(copied);
    a.stx(zp(z::T4));
    a.jsr("msg_put");
    a.ldx(zp(z::T4));
    a.inx();
    a.bne(copy);
    a.bind(copied);
    a.jsr("msg_end");
    a.jmp("main");
    a.bind(none);
    a.jsr("say_no_pattern");
    a.jmp("main");
}

/// `match_at`: carry set where the pattern is the text at N0.
fn matching(a: &mut Asm) {
    a.here("match_at");
    a.jsr("phys");
    mov16(a, z::N1, z::N0);
    a.ldx(imm(0));
    let each = a.here_local();
    let differ = a.local();
    a.lda(abs(page::PATTERN).x());
    let matched = a.local();
    a.beq(matched);
    cmp16(a, z::N1, z::LEN);
    a.bcs(differ);
    a.jsr("rd0");
    a.cmp(abs(page::PATTERN).x());
    a.bne(differ);
    a.inx();
    inc16(a, z::N1);
    a.jsr("fwd");
    a.jmp(each);
    a.bind(matched);
    a.sec();
    a.rts();
    a.bind(differ);
    a.clc();
    a.rts();
}
