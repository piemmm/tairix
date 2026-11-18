//! The motions: each finds its target from the cursor, a count's times, in
//! N0, with its kind in MKIND; carry set where it cannot move at all. A
//! motion moves the cursor, or gives an operator its range.

use super::ops::{add16, cmp16, dec16, inc16, mov16, sub16, zero16};
use super::ws::{motion, zp as z, END_COLUMN};
use crate::rom::asm::{imm, zp, Asm, RTS};
use crate::rom::os::CR;

pub(super) fn emit(a: &mut Asm) {
    helpers(a);
    horizontal(a);
    vertical(a);
    screen_lines(a);
    words(a);
    word_ends(a);
}

/// `count1`: the count in N1, 1 where none was typed. `first_not_blank`:
/// N0, a line's start, moved to its first character that is not a space,
/// or its end. `want_col`: N0, a line's start, moved to the column WANT
/// asks for, or the line's last character. `char_class`: the character at
/// N0 as 0 for a space or a line's end, 1 for a name's, 2 for any other;
/// zero set for 0. `exclusive`, `inclusive`, `linewise`: carry clear with
/// that kind.
fn helpers(a: &mut Asm) {
    a.here("count1");
    a.jsr("n1_count");
    zero16(a, z::N1);
    a.bne(RTS);
    a.inc(zp(z::N1));
    a.rts();

    a.here("first_not_blank");
    let skip = a.here_local();
    a.jsr("n0_vs_len");
    a.bcs(RTS);
    a.jsr("getc");
    a.cmp(imm(b' '));
    a.bne(RTS);
    inc16(a, z::N0);
    a.jmp(skip);

    a.here("want_col");
    a.jsr("n2_n0");
    a.jsr("line_end");
    sub16(a, z::N1, z::N0, z::N2);
    a.jsr("n0_n2");
    zero16(a, z::N1);
    a.beq(RTS);
    dec16(a, z::N1);
    let wanted = a.local();
    let last = a.local();
    a.lda(zp(z::N1 + 1));
    a.bne(wanted);
    a.lda(zp(z::WANT));
    a.cmp(zp(z::N1));
    a.bcs(last);
    a.bind(wanted);
    a.lda(zp(z::WANT));
    a.sta(zp(z::N1));
    a.lda(imm(0));
    a.sta(zp(z::N1 + 1));
    a.bind(last);
    add16(a, z::N0, z::N0, z::N1);
    a.rts();

    a.here("char_class");
    a.jsr("getc");
    a.cmp(imm(b' '));
    let blank = a.local();
    a.beq(blank);
    a.cmp(imm(CR));
    a.beq(blank);
    a.jsr("is_name");
    let other = a.local();
    a.bcc(other);
    a.lda(imm(1));
    a.rts();
    a.bind(other);
    a.lda(imm(2));
    a.rts();
    a.bind(blank);
    a.lda(imm(0));
    a.rts();

    for (name, kind) in [
        ("exclusive", motion::EXCLUSIVE),
        ("inclusive", motion::INCLUSIVE),
        ("linewise", motion::LINEWISE),
    ] {
        a.here(name);
        a.lda(imm(kind));
        a.sta(zp(z::MKIND));
        a.clc();
        a.rts();
    }
}

/// Along the line: `h`, `l`, `0`, `^` and `$`.
fn horizontal(a: &mut Asm) {
    a.here("mo_left");
    let no = a.local();
    a.lda(zp(z::COL));
    a.beq(no);
    a.jsr("count1");
    let to_start = a.local();
    a.lda(zp(z::N1 + 1));
    a.bne(to_start);
    a.lda(zp(z::N1));
    a.cmp(zp(z::COL));
    a.bcs(to_start);
    sub16(a, z::N0, z::CUR, z::N1);
    a.jmp("exclusive");
    a.bind(to_start);
    a.jsr("n0_lstart");
    a.jmp("exclusive");
    a.bind(no);
    a.sec();
    a.rts();

    // Right, to the line's last character, or for an operator past it.
    a.here("mo_right");
    let no = a.local();
    a.jsr("n0_cur");
    a.jsr("line_end");
    sub16(a, z::N2, z::N0, z::CUR);
    zero16(a, z::N2);
    a.beq(no);
    a.lda(zp(z::OP));
    let operator = a.local();
    a.bne(operator);
    dec16(a, z::N2);
    zero16(a, z::N2);
    a.beq(no);
    a.bind(operator);
    a.jsr("count1");
    cmp16(a, z::N2, z::N1);
    let within = a.local();
    a.bcs(within);
    a.jsr("n1_n2");
    a.bind(within);
    add16(a, z::N0, z::CUR, z::N1);
    a.jmp("exclusive");
    a.bind(no);
    a.sec();
    a.rts();

    a.here("mo_bol");
    a.jsr("n0_lstart");
    a.jmp("exclusive");

    a.here("mo_first");
    a.jsr("n0_lstart");
    a.jsr("first_not_blank");
    a.jmp("exclusive");

    // To the end of the line a count less one below: its CR, which the
    // cursor stops short of and an operator takes the line up to.
    a.here("mo_eol");
    a.jsr("count1");
    a.jsr("n0_lstart");
    let down = a.here_local();
    let reached = a.local();
    dec16(a, z::N1);
    zero16(a, z::N1);
    a.beq(reached);
    a.jsr("n2_n0");
    a.jsr("next_line");
    a.bcc(down);
    a.jsr("n0_n2");
    a.bind(reached);
    a.jsr("line_end");
    a.lda(imm(END_COLUMN));
    a.sta(zp(z::WANT));
    a.jmp("exclusive");
}

/// Across lines: `j`, `k`, RETURN, `+`, `-`, `G`, `gg`, `H`, `M` and `L`.
fn vertical(a: &mut Asm) {
    // `lines_down`: N0, a line's start, N1 lines down, as far as there
    // are, T4 the lines moved to 255; carry set where it moved none of a
    // count above 0. `lines_up` the same up.
    for (name, step) in [("lines_down", "next_line"), ("lines_up", "prev_line")] {
        a.here(name);
        zero16(a, z::N1);
        a.beq(RTS);
        a.lda(imm(0));
        a.sta(zp(z::T4));
        let again = a.here_local();
        let out = a.local();
        a.jsr("n2_n0");
        a.jsr(step);
        a.bcs(out);
        a.inc(zp(z::T4));
        let counted = a.local();
        a.bne(counted);
        a.dec(zp(z::T4));
        a.bind(counted);
        dec16(a, z::N1);
        zero16(a, z::N1);
        a.bne(again);
        a.clc();
        a.rts();
        a.bind(out);
        a.jsr("n0_n2");
        a.lda(zp(z::T4));
        let moved = a.local();
        a.bne(moved);
        a.sec();
        a.rts();
        a.bind(moved);
        a.clc();
        a.rts();
    }

    for (name, step, column) in [
        ("mo_down", "lines_down", "want_col"),
        ("mo_up", "lines_up", "want_col"),
        ("mo_down_first", "lines_down", "first_not_blank"),
        ("mo_up_first", "lines_up", "first_not_blank"),
    ] {
        a.here(name);
        a.jsr("count1");
        a.jsr("n0_lstart");
        a.jsr(step);
        a.bcs(RTS);
        a.jsr(column);
        a.jmp("linewise");
    }

    // `G`: the line counted, or the last.
    a.here("mo_goto");
    zero16(a, z::COUNT);
    let numbered = a.local();
    a.bne(numbered);
    a.jsr("n0_len");
    let first = a.local();
    zero16(a, z::N0);
    a.beq(first);
    dec16(a, z::N0);
    a.jsr("line_start");
    a.jmp(first);
    a.bind(numbered);
    a.jsr("goto_line");
    a.bind(first);
    a.jsr("first_not_blank");
    a.jmp("linewise");

    // `gg`: the line counted, or the first.
    a.here("mo_g");
    a.jsr("read_key");
    a.cmp(imm(b'g'));
    let no = a.local();
    a.bne(no);
    zero16(a, z::COUNT);
    let counted = a.local();
    a.bne(counted);
    a.inc(zp(z::COUNT));
    a.bind(counted);
    a.jsr("goto_line");
    a.jsr("first_not_blank");
    a.jmp("linewise");
    a.bind(no);
    a.sec();
    a.rts();

    // `goto_line`: N0 the start of line COUNT, or of the last line before it.
    a.here("goto_line");
    a.jsr("n0_lstart");
    cmp16(a, z::COUNT, z::LINE);
    let forward = a.local();
    a.bcs(forward);
    sub16(a, z::N1, z::LINE, z::COUNT);
    a.jmp("lines_up");
    a.bind(forward);
    sub16(a, z::N1, z::COUNT, z::LINE);
    a.jmp("lines_down");
}

/// `H`, `M` and `L`.
fn screen_lines(a: &mut Asm) {
    // `H`, `L`: a count's lines from the screen's top or bottom; `M` its
    // middle.
    a.here("mo_top");
    a.jsr("count1");
    dec16(a, z::N1);
    let screen = a.local();
    a.jmp(screen);
    a.here("mo_bottom");
    a.jsr("count1");
    a.sec();
    a.lda(zp(z::ROWS));
    a.sbc(zp(z::N1));
    a.sta(zp(z::N1));
    a.lda(imm(0));
    a.sta(zp(z::N1 + 1));
    let low = a.local();
    a.bcs(low);
    a.sta(zp(z::N1));
    a.bind(low);
    a.jmp(screen);
    a.here("mo_middle");
    a.lda(zp(z::ROWS));
    a.sec();
    a.sbc(imm(1));
    a.lsr(crate::rom::asm::Acc);
    a.sta(zp(z::N1));
    a.lda(imm(0));
    a.sta(zp(z::N1 + 1));
    a.bind(screen);
    a.jsr("n0_top");
    a.jsr("lines_down");
    a.jsr("first_not_blank");
    a.jmp("linewise");
}

/// The word motions: `w`, `b` and `e`, a word a run of name characters or
/// of other characters that are not spaces.
fn words(a: &mut Asm) {
    // `w`: to the next word's start. An operator whose last word ends its
    // line stops there, not at the next line's first word.
    a.here("mo_word");
    a.jsr("count1");
    a.jsr("n0_cur");
    mov16(a, z::N2, z::CUR);
    a.lda(imm(0));
    a.sta(zp(z::T2));
    let each = a.here_local();
    let end = a.local();
    let blanks = a.local();
    a.jsr("n0_vs_len");
    a.bcs(end);
    a.jsr("char_class");
    a.beq(blanks);
    a.sta(zp(z::T1));
    let run = a.here_local();
    inc16(a, z::N0);
    a.jsr("n0_vs_len");
    a.bcs(end);
    a.jsr("char_class");
    a.cmp(zp(z::T1));
    a.beq(run);
    a.bind(blanks);
    a.jsr("n2_n0");
    a.lda(imm(0));
    a.sta(zp(z::T2));
    let skip = a.here_local();
    let word = a.local();
    a.jsr("n0_vs_len");
    a.bcs(end);
    a.jsr("getc");
    a.cmp(imm(CR));
    let not_cr = a.local();
    a.bne(not_cr);
    a.sta(zp(z::T2));
    a.bind(not_cr);
    a.jsr("char_class");
    a.bne(word);
    inc16(a, z::N0);
    a.jmp(skip);
    a.bind(word);
    dec16(a, z::N1);
    zero16(a, z::N1);
    a.bne(each);
    a.bind(end);
    a.lda(zp(z::OP));
    a.beq("exclusive_here");
    a.lda(zp(z::T2));
    a.beq("exclusive_here");
    a.jsr("n0_n2");
    a.here("exclusive_here");
    a.jmp("exclusive");
}

/// `e` and `b`.
fn word_ends(a: &mut Asm) {
    // `e`: to the end of the next word.
    a.here("mo_end");
    a.jsr("count1");
    a.jsr("n0_cur");
    a.lda(imm(0));
    a.sta(zp(z::T4));
    let each = a.here_local();
    let no = a.local();
    let within = a.local();
    inc16(a, z::N0);
    a.jsr("n0_vs_len");
    a.bcs(no);
    a.jsr("char_class");
    a.beq(each);
    a.bind(within);
    a.sta(zp(z::T1));
    let run = a.here_local();
    let last = a.local();
    inc16(a, z::N0);
    a.jsr("n0_vs_len");
    a.bcs(last);
    a.jsr("char_class");
    a.cmp(zp(z::T1));
    a.beq(run);
    a.bind(last);
    dec16(a, z::N0);
    a.jsr("n2_n0");
    a.inc(zp(z::T4));
    let counted = a.local();
    a.bne(counted);
    a.dec(zp(z::T4));
    a.bind(counted);
    dec16(a, z::N1);
    zero16(a, z::N1);
    a.bne(each);
    a.jmp("inclusive");
    // The text's end: the last word's end reached counts.
    a.bind(no);
    a.lda(zp(z::T4));
    let none = a.local();
    a.beq(none);
    a.jsr("n0_n2");
    a.jmp("inclusive");
    a.bind(none);
    a.sec();
    a.rts();

    // `b`: to the start of the word before.
    a.here("mo_back");
    a.jsr("count1");
    a.jsr("n0_cur");
    let no = a.local();
    let each = a.here_local();
    let done = a.local();
    zero16(a, z::N0);
    a.beq(done);
    dec16(a, z::N0);
    let skip = a.here_local();
    let in_word = a.local();
    a.jsr("char_class");
    a.bne(in_word);
    zero16(a, z::N0);
    a.beq(done);
    dec16(a, z::N0);
    a.jmp(skip);
    a.bind(in_word);
    a.sta(zp(z::T1));
    let run = a.here_local();
    let started = a.local();
    zero16(a, z::N0);
    a.beq(started);
    dec16(a, z::N0);
    a.jsr("char_class");
    a.cmp(zp(z::T1));
    a.beq(run);
    inc16(a, z::N0);
    a.bind(started);
    dec16(a, z::N1);
    zero16(a, z::N1);
    a.bne(each);
    a.bind(done);
    let moved = a.local();
    a.lda(zp(z::N0));
    a.cmp(zp(z::CUR));
    a.bne(moved);
    a.lda(zp(z::N0 + 1));
    a.cmp(zp(z::CUR + 1));
    a.bne(moved);
    a.bind(no);
    a.sec();
    a.rts();
    a.bind(moved);
    a.jmp("exclusive");
}
