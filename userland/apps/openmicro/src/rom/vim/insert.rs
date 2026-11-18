//! Insert mode: what is typed goes in before the cursor, DELETE rubs out
//! what this insert typed, RETURN breaks the line, ESCAPE goes back to
//! normal mode. And the commands that start it.

use super::entry::ESCAPE;
use super::normal::{cursor_key, DELETE};
use super::ops::{dec16, inc16, mov16, sub16};
use super::ws::{show, zp as z};
use crate::rom::asm::{abs, imm, zp, Asm, Label};
use crate::rom::far;
use crate::rom::os::{entry, CR};

/// The longest a line may grow.
const MOST_LINE: u8 = 0xFF;

pub(super) fn emit(a: &mut Asm) {
    commands(a);
    typing(a);
}

/// `i`, `a`, `I`, `A`, `o` and `O`.
fn commands(a: &mut Asm) {
    a.here("cmd_insert");
    a.jsr("n0_cur");
    let start = a.local();
    a.jmp(start);

    a.here("cmd_append");
    a.jsr("n0_cur");
    a.jsr("n0_vs_len");
    a.bcs(start);
    a.jsr("getc");
    a.cmp(imm(CR));
    a.beq(start);
    inc16(a, z::N0);
    a.jmp(start);

    a.here("cmd_insert_first");
    a.jsr("n0_lstart");
    a.jsr("first_not_blank");
    a.jmp(start);

    a.here("cmd_append_end");
    a.jsr("n0_cur");
    a.jsr("line_end");
    a.bind(start);
    a.jsr("set_cursor");
    a.jsr("n0_cur");
    a.jsr("begin_change");
    a.jmp("insert_mode");

    // `o`: a new line after the cursor's; `O` before it.
    a.here("cmd_open_below");
    a.jsr("n0_cur");
    a.jsr("line_end");
    a.jsr("set_cursor");
    a.jsr("n0_cur");
    a.jsr("begin_change");
    a.lda(imm(CR));
    a.jsr("ins_char");
    far(a, Asm::bcc, "no_room");
    let opened = a.local();
    a.jmp(opened);

    a.here("cmd_open_above");
    a.jsr("n0_lstart");
    a.jsr("set_cursor");
    a.jsr("n0_cur");
    a.jsr("begin_change");
    a.lda(imm(CR));
    a.jsr("ins_char");
    far(a, Asm::bcc, "no_room");
    dec16(a, z::CUR);
    a.bind(opened);
    inc16(a, z::UINS);
    a.jsr("n0_cur");
    a.jsr("set_cursor");
    a.lda(imm(show::ALL));
    a.jsr("owe");
    a.fall_into("insert_mode");
}

/// `insert_mode`: keys typed in at the cursor until ESCAPE, the change
/// begun before it counting what they leave.
fn typing(a: &mut Asm) {
    a.here("insert_mode");
    a.lda(imm(show::INSERT));
    a.jsr("owe");
    let key = a.here_local();
    a.jsr("refresh");
    a.jsr("read_key");
    a.cmp(imm(ESCAPE));
    let leave = a.local();
    let typed = a.local();
    a.bne(typed);
    a.jmp(leave);
    a.bind(typed);
    let newline = a.local();
    let rubout = a.local();
    for (code, handler) in [(CR, newline), (DELETE, rubout)] {
        a.cmp(imm(code));
        let other = a.local();
        a.bne(other);
        a.jmp(handler);
        a.bind(other);
    }
    a.cmp(imm(cursor_key::LEFT - 1));
    let other = a.local();
    a.bcc(other);
    a.cmp(imm(cursor_key::UP + 1));
    a.bcc(key);
    a.bind(other);
    let no_room = a.local();
    typed_character(a, key, no_room);
    broken_line(a, key, newline, no_room);
    rubbed_out(a, key, rubout);
    left(a, leave);
    owed(a, key);
}

/// A character typed in: the line must have room for it.
fn typed_character(a: &mut Asm, key: Label, no_room: Label) {
    a.pha();
    a.jsr("n0_lstart");
    a.jsr("line_end");
    sub16(a, z::N1, z::N0, z::LSTART);
    a.lda(zp(z::N1 + 1));
    let full = a.local();
    a.bne(full);
    a.lda(zp(z::N1));
    a.cmp(imm(MOST_LINE));
    a.bcs(full);
    a.pla();
    a.jsr("ins_char");
    a.bcs(no_room);
    inc16(a, z::UINS);
    a.inc(zp(z::COL));
    a.lda(imm(show::LINE));
    a.jsr("owe");
    a.jmp(key);
    a.bind(full);
    a.pla();
    a.jsr("bell");
    a.jmp(key);
    a.bind(no_room);
    a.jsr("say_no_room");
    a.jmp(key);
}

/// RETURN: the line broken, the rows from the cursor's owed.
fn broken_line(a: &mut Asm, key: Label, newline: Label, no_room: Label) {
    a.bind(newline);
    a.jsr("ins_char");
    a.bcs(no_room);
    inc16(a, z::UINS);
    inc16(a, z::LINE);
    mov16(a, z::LSTART, z::CUR);
    a.lda(imm(0));
    a.sta(zp(z::COL));
    a.lda(zp(z::CROW));
    a.sta(zp(z::FROM_ROW));
    a.lda(imm(show::FROM));
    a.jsr("owe");
    a.jmp(key);
}

/// DELETE: back over what this insert typed, no further.
fn rubbed_out(a: &mut Asm, key: Label, rubout: Label) {
    a.bind(rubout);
    a.lda(zp(z::CUR));
    a.cmp(zp(z::UPOS));
    let back = a.local();
    a.bne(back);
    a.lda(zp(z::CUR + 1));
    a.cmp(zp(z::UPOS + 1));
    a.bne(back);
    a.jsr("bell");
    a.jmp(key);
    a.bind(back);
    dec16(a, z::CUR);
    a.jsr("n0_cur");
    a.jsr("getc");
    a.pha();
    a.lda(imm(1));
    a.sta(zp(z::N1));
    a.lda(imm(0));
    a.sta(zp(z::N1 + 1));
    a.jsr("n0_cur");
    a.jsr("del_range");
    dec16(a, z::UINS);
    a.jsr("n0_cur");
    a.jsr("set_cursor");
    a.pla();
    a.cmp(imm(CR));
    a.beq("all_owed");
    a.lda(imm(show::LINE));
    a.jsr("owe");
    a.jmp(key);
}

/// ESCAPE: back to normal mode, the cursor on the last character typed.
fn left(a: &mut Asm, leave: Label) {
    a.bind(leave);
    a.lda(zp(z::SHOW));
    a.and(imm(!show::INSERT));
    a.ora(imm(show::LINE));
    a.sta(zp(z::SHOW));
    a.lda(zp(z::COL));
    let at_start = a.local();
    a.beq(at_start);
    dec16(a, z::CUR);
    a.bind(at_start);
    a.jsr("fix_cursor");
    a.lda(zp(z::COL));
    a.sta(zp(z::WANT));
    a.rts();
}

/// `all_owed`: every row owed, and the next key; `bell`: the bell rung.
fn owed(a: &mut Asm, key: Label) {
    a.here("all_owed");
    a.lda(imm(show::ALL));
    a.jsr("owe");
    a.jmp(key);

    a.here("bell");
    a.lda(imm(7));
    a.jmp(abs(entry::OSWRCH));
}
