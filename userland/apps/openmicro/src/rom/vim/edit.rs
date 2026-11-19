//! The changes: deleting, the operators over a motion, putting the
//! register back, joining, replacing and case, and undoing the last change
//! or repeating it.
//!
//! The undo record is the last change: where it was made, how much it left
//! there, and what it took away. Undoing it swaps the two, so a second undo
//! redoes it.

use super::entry::ESCAPE;
use super::normal::flag;
use super::ops::{add16, cmp16, dec16, inc16, mov16, sub16, zero16};
use super::ws::{file, motion, page, show, zp as z};
use crate::rom::asm::{abs, imm, zp, Acc, Asm, Label, RTS};
use crate::rom::far;
use crate::rom::os::CR;

pub(super) fn emit(a: &mut Asm) {
    changes(a);
    characters(a);
    replacing(a);
    operators(a);
    operating(a);
    operator_terms(a);
    putting(a);
    joining(a);
    undoing(a);
}

/// `begin_change`: a change starting at N0, N0 and N1 kept: the undo
/// record emptied and pointed there, the text marked changed.
/// `snapshot`: the N1 bytes from N0 kept in the undo record, the change
/// pointed there; the command abandoned where there is no room. `edited`: every row
/// owed, the cursor at N0 where normal mode lets it stand.
fn changes(a: &mut Asm) {
    a.here("begin_change");
    mov16(a, z::UPOS, z::N0);
    a.lda(zp(z::N1));
    a.pha();
    a.lda(zp(z::N1 + 1));
    a.pha();
    a.lda(imm(0));
    a.sta(zp(z::N1));
    a.sta(zp(z::N1 + 1));
    a.sta(zp(z::UINS));
    a.sta(zp(z::UINS + 1));
    a.jsr("resize_undo");
    a.pla();
    a.sta(zp(z::N1 + 1));
    a.pla();
    a.sta(zp(z::N1));
    a.jsr("n0_upos");
    a.fall_into("mark_changed");

    a.here("mark_changed");
    a.lda(imm(1));
    a.sta(zp(z::UNDO_OK));
    a.lda(zp(z::FILE));
    a.ora(imm(file::MODIFIED));
    a.sta(zp(z::FILE));
    a.rts();

    a.here("snapshot");
    a.jsr("move_gap");
    a.jsr("keep_undo");
    a.bcs("no_room");
    mov16(a, z::UPOS, z::GOFF);
    a.lda(imm(0));
    a.sta(zp(z::UINS));
    a.sta(zp(z::UINS + 1));
    a.jmp("mark_changed");

    a.here("edited");
    a.jsr("place_cursor");
    a.lda(imm(show::ALL));
    a.jmp("owe");

    // `no_room`: a change refused for want of memory.
    a.here("no_room");
    a.jsr("say_no_room");
    a.jmp("main");
}

/// `x`, `X`, `r` and `~`.
fn characters(a: &mut Asm) {
    // `x`: a count's characters from the cursor, within its line.
    a.here("cmd_x");
    a.jsr("to_line_end");
    far(a, Asm::bne, "fail");
    a.jsr("n0_cur");
    a.lda(imm(0));
    a.jsr("cut");
    a.jsr("n0_cur");
    a.jmp("edited");

    // `X`: a count's characters before the cursor.
    a.here("cmd_big_x");
    a.lda(zp(z::COL));
    far(a, Asm::bne, "fail");
    a.jsr("count1");
    a.lda(zp(z::N1 + 1));
    let few = a.local();
    let take = a.local();
    a.bne(few);
    a.lda(zp(z::N1));
    a.cmp(zp(z::COL));
    a.bcc(take);
    a.bind(few);
    a.lda(zp(z::COL));
    a.sta(zp(z::N1));
    a.lda(imm(0));
    a.sta(zp(z::N1 + 1));
    a.bind(take);
    sub16(a, z::N0, z::CUR, z::N1);
    a.lda(imm(0));
    a.jsr("cut");
    a.jsr("n0_upos");
    a.jmp("edited");

    // `to_line_end`: N1 a count's characters from the cursor, as many as
    // its line holds; zero set where it holds none there.
    a.here("to_line_end");
    a.jsr("n0_cur");
    a.jsr("line_end");
    sub16(a, z::N2, z::N0, z::CUR);
    zero16(a, z::N2);
    a.beq(RTS);
    a.jsr("count1");
    cmp16(a, z::N2, z::N1);
    let within = a.local();
    a.bcs(within);
    a.jsr("n1_n2");
    a.bind(within);
    a.lda(imm(1));
    a.rts();
}

/// `r` and `~`.
fn replacing(a: &mut Asm) {
    // `r`: a count's characters made the key read next.
    a.here("cmd_replace");
    a.jsr("read_key");
    a.cmp(imm(ESCAPE));
    far(a, Asm::bne, "fail");
    a.cmp(imm(CR));
    far(a, Asm::bne, "fail");
    a.sta(zp(z::T3));
    a.lda(imm(0));
    a.sta(zp(z::T2));
    let in_place = a.local();
    a.jmp(in_place);

    // `~`: a count's characters' case changed, the cursor past them.
    a.here("cmd_case");
    a.lda(imm(1));
    a.sta(zp(z::T2));
    a.bind(in_place);
    a.jsr("to_line_end");
    far(a, Asm::bne, "fail");
    a.jsr("hold_n1");
    a.jsr("n0_cur");
    a.jsr("snapshot");
    mov16(a, z::UINS, z::HOLD);
    a.jsr("n0_cur");
    let each = a.here_local();
    a.jsr("getc");
    a.ldx(zp(z::T2));
    let set = a.local();
    a.beq(set);
    // The case turned over, a letter's alone.
    a.pha();
    a.and(imm(0xDF));
    a.cmp(imm(b'A'));
    let not_letter = a.local();
    a.bcc(not_letter);
    a.cmp(imm(b'Z' + 1));
    a.bcs(not_letter);
    a.pla();
    a.eor(imm(0x20));
    let store = a.local();
    a.jmp(store);
    a.bind(not_letter);
    a.pla();
    a.jmp(store);
    a.bind(set);
    a.lda(zp(z::T3));
    a.bind(store);
    a.jsr("wr0");
    inc16(a, z::N0);
    dec16(a, z::HOLD);
    zero16(a, z::HOLD);
    a.bne(each);
    // `~` leaves the cursor past what it changed, `r` on its last.
    a.lda(zp(z::T2));
    let after = a.local();
    a.bne(after);
    dec16(a, z::N0);
    a.bind(after);
    a.jmp("edited");
}

/// `d`, `c` and `y` over a motion or doubled for whole lines, and `D`, `C`
/// and `Y`.
fn operators(a: &mut Asm) {
    a.here("cmd_big_d");
    a.lda(imm(b'd'));
    let to_end = a.local();
    a.bne(to_end);
    a.here("cmd_big_c");
    a.lda(imm(b'c'));
    a.bind(to_end);
    a.sta(zp(z::OP));
    a.lda(imm(b'$'));
    a.sta(zp(z::KEY));
    let motion_key = a.local();
    a.jmp(motion_key);

    a.here("cmd_big_y");
    a.lda(imm(b'y'));
    a.sta(zp(z::OP));
    a.sta(zp(z::KEY));
    let lines = a.local();
    a.jmp(lines);

    a.here("cmd_operator");
    a.lda(zp(z::KEY));
    a.sta(zp(z::OP));
    mov16(a, z::HOLD, z::COUNT);
    a.lda(imm(0));
    a.sta(zp(z::COUNT));
    a.sta(zp(z::COUNT + 1));
    let digits = a.here_local();
    let counted = a.local();
    a.jsr("read_key");
    a.sta(zp(z::KEY));
    a.cmp(imm(b'1'));
    let maybe_zero = a.local();
    a.bcc(maybe_zero);
    a.cmp(imm(b'9' + 1));
    let digit = a.local();
    a.bcc(digit);
    a.bind(maybe_zero);
    a.cmp(imm(b'0'));
    a.bne(counted);
    zero16(a, z::COUNT);
    a.beq(counted);
    a.bind(digit);
    a.jsr("add_digit");
    a.jmp(digits);
    a.bind(counted);
    a.jsr("combine_counts");
    a.lda(zp(z::KEY));
    a.cmp(zp(z::OP));
    a.beq(lines);
    changed_word(a, motion_key);
    a.bind(motion_key);
    a.lda(zp(z::KEY));
    a.jsr("find_command");
    far(a, Asm::bcc, "fail");
    a.lda(zp(z::T4));
    a.and(imm(flag::MOTION));
    far(a, Asm::bne, "fail");
    a.jsr("call_p3");
    far(a, Asm::bcc, "fail");
    a.jmp("operate");

    // Doubled: whole lines, from the cursor's, a count's.
    a.bind(lines);
    a.jsr("count1");
    dec16(a, z::N1);
    a.jsr("n0_lstart");
    a.jsr("lines_down");
    a.lda(imm(motion::LINEWISE));
    a.sta(zp(z::MKIND));
    a.fall_into("operate");
}

/// `cw` on a word: to its end, not past the spaces after it; on its last
/// character, that character alone, and a count's words past it the rest.
fn changed_word(a: &mut Asm, motion_key: Label) {
    a.cmp(imm(b'w'));
    a.bne(motion_key);
    a.lda(zp(z::OP));
    a.cmp(imm(b'c'));
    a.bne(motion_key);
    a.jsr("n0_cur");
    a.jsr("char_class");
    a.beq(motion_key);
    a.sta(zp(z::T0));
    inc16(a, z::N0);
    a.jsr("n0_vs_len");
    let at_end = a.local();
    let to_end = a.local();
    a.bcs(at_end);
    a.jsr("char_class");
    a.cmp(zp(z::T0));
    a.beq(to_end);
    a.bind(at_end);
    a.lda(zp(z::COUNT + 1));
    let more = a.local();
    a.bne(more);
    a.lda(zp(z::COUNT));
    a.cmp(imm(2));
    let alone = a.local();
    a.bcc(alone);
    a.bind(more);
    dec16(a, z::COUNT);
    a.bind(to_end);
    a.lda(imm(b'e'));
    a.sta(zp(z::KEY));
    a.bne(motion_key);
    a.bind(alone);
    a.jsr("n0_cur");
    a.lda(imm(motion::INCLUSIVE));
    a.sta(zp(z::MKIND));
    a.jmp("operate");
}

/// `operate`, and the flags and counts the operators take.
fn operating(a: &mut Asm) {
    // `operate`: OP over the cursor to N0, as MKIND has it.
    a.here("operate");
    cmp16(a, z::N0, z::CUR);
    let forward = a.local();
    a.bcs(forward);
    a.jsr("rstart_n0");
    mov16(a, z::REND, z::CUR);
    let ordered = a.local();
    a.jmp(ordered);
    a.bind(forward);
    mov16(a, z::RSTART, z::CUR);
    a.jsr("rend_n0");
    a.bind(ordered);
    a.lda(zp(z::MKIND));
    a.cmp(imm(motion::LINEWISE));
    let charwise = a.local();
    a.bne(charwise);
    a.jsr("n0_rstart");
    a.jsr("line_start");
    a.jsr("rstart_n0");
    mov16(a, z::N0, z::REND);
    a.jsr("line_end");
    a.jsr("n0_vs_len");
    let ended = a.local();
    a.bcs(ended);
    inc16(a, z::N0);
    a.bind(ended);
    a.jsr("rend_n0");
    let ranged = a.local();
    a.jmp(ranged);
    a.bind(charwise);
    a.cmp(imm(motion::INCLUSIVE));
    a.bne(ranged);
    cmp16(a, z::REND, z::LEN);
    a.bcs(ranged);
    inc16(a, z::REND);
    a.bind(ranged);
    a.jsr("range_len");
    a.jsr("n0_rstart");
    a.lda(zp(z::OP));
    a.cmp(imm(b'y'));
    let not_yank = a.local();
    a.bne(not_yank);
    a.jsr("linewise_flag");
    a.jsr("yank");
    a.lda(zp(z::MKIND));
    a.cmp(imm(motion::LINEWISE));
    a.beq(RTS);
    a.jsr("n0_rstart");
    a.jmp("set_cursor");
    a.bind(not_yank);
    a.jsr("linewise_flag");
    a.jsr("cut");
    a.lda(zp(z::OP));
    a.cmp(imm(b'c'));
    let deleted = a.local();
    a.bne(deleted);
    // A change: whole lines keep one empty line to type into.
    a.lda(zp(z::MKIND));
    a.cmp(imm(motion::LINEWISE));
    let typed = a.local();
    a.bne(typed);
    mov16(a, z::CUR, z::RSTART);
    a.lda(imm(CR));
    a.jsr("ins_char");
    far(a, Asm::bcc, "no_room");
    a.lda(imm(1));
    a.sta(zp(z::UINS));
    a.bind(typed);
    a.jsr("n0_rstart");
    a.jsr("set_cursor");
    a.lda(imm(show::ALL));
    a.jsr("owe");
    a.jmp("insert_mode");
    a.bind(deleted);
    a.jsr("n0_rstart");
    a.lda(zp(z::MKIND));
    a.cmp(imm(motion::LINEWISE));
    a.bne("edited_here");
    // The last lines gone: the new last line's.
    a.jsr("n0_vs_len");
    let within = a.local();
    a.bcc(within);
    zero16(a, z::N0);
    a.beq(within);
    dec16(a, z::N0);
    a.bind(within);
    a.jsr("line_start");
    a.jsr("first_not_blank");
    a.here("edited_here");
    a.jmp("edited");
}

/// `linewise_flag` and `combine_counts`.
fn operator_terms(a: &mut Asm) {
    // `linewise_flag`: A whether the range is whole lines; N0 and N1 kept.
    a.here("linewise_flag");
    a.lda(zp(z::MKIND));
    a.cmp(imm(motion::LINEWISE));
    a.lda(imm(0));
    a.rol(Acc);
    a.rts();

    // `combine_counts`: the counts before an operator and before its
    // motion, multiplied, where either was typed.
    a.here("combine_counts");
    zero16(a, z::HOLD);
    a.beq(RTS);
    zero16(a, z::COUNT);
    let both = a.local();
    a.bne(both);
    mov16(a, z::COUNT, z::HOLD);
    a.rts();
    a.bind(both);
    a.jsr("n1_count");
    a.lda(imm(0));
    a.sta(zp(z::COUNT));
    a.sta(zp(z::COUNT + 1));
    let bit = a.here_local();
    let skip = a.local();
    a.lsr(zp(z::HOLD + 1));
    a.ror(zp(z::HOLD));
    a.bcc(skip);
    add16(a, z::COUNT, z::COUNT, z::N1);
    a.bind(skip);
    a.asl(zp(z::N1));
    a.rol(zp(z::N1 + 1));
    zero16(a, z::HOLD);
    a.bne(bit);
    a.rts();
}

/// `p` and `P`: the register put after or before the cursor, a count's
/// times, whole lines after or before its line.
fn putting(a: &mut Asm) {
    a.here("cmd_put_after");
    a.lda(imm(0));
    let chosen = a.local();
    a.beq(chosen);
    a.here("cmd_put_before");
    a.lda(imm(1));
    a.bind(chosen);
    a.sta(zp(z::T2));
    a.lda(imm(0));
    a.sta(zp(z::T3));
    sub16(a, z::N1, z::LIMIT, z::REG);
    zero16(a, z::N1);
    far(a, Asm::bne, "fail");
    a.lda(zp(z::LINEWISE));
    let charwise = a.local();
    a.beq(charwise);
    // Whole lines: before the cursor's line, or after its CR; a last line
    // without one is given one first.
    a.jsr("n0_lstart");
    a.lda(zp(z::T2));
    let place = a.local();
    a.bne(place);
    a.jsr("n0_cur");
    a.jsr("line_end");
    a.jsr("n0_vs_len");
    let has_cr = a.local();
    a.bcc(has_cr);
    a.jsr("begin_change");
    a.jsr("cur_n0");
    a.lda(imm(CR));
    a.jsr("ins_char");
    far(a, Asm::bcc, "no_room");
    a.lda(imm(1));
    a.sta(zp(z::UINS));
    a.sta(zp(z::T3));
    let put = a.local();
    a.jmp(put);
    a.bind(has_cr);
    inc16(a, z::N0);
    a.bind(place);
    a.jsr("begin_change");
    a.bind(put);
    a.jsr("put_count");
    // The first line put: past the CR a last line was given.
    a.lda(zp(z::UPOS));
    a.clc();
    a.adc(zp(z::T3));
    a.sta(zp(z::N0));
    a.lda(zp(z::UPOS + 1));
    a.adc(imm(0));
    a.sta(zp(z::N0 + 1));
    a.jsr("first_not_blank");
    a.jmp("edited");
    // Characters: after or at the cursor, which lands on the last put.
    a.bind(charwise);
    a.jsr("n0_cur");
    a.lda(zp(z::T2));
    let at = a.local();
    a.bne(at);
    a.jsr("n0_vs_len");
    a.bcs(at);
    a.jsr("getc");
    a.cmp(imm(CR));
    a.beq(at);
    inc16(a, z::N0);
    a.bind(at);
    a.jsr("begin_change");
    a.jsr("put_count");
    a.jsr("lines_fit");
    add16(a, z::N0, z::UPOS, z::UINS);
    dec16(a, z::N0);
    a.jmp("edited");

    // `put_count`: the register put at N0, a count's times, the change's
    // length counted.
    a.here("put_count");
    a.jsr("count1");
    a.jsr("hold_n1");
    let again = a.here_local();
    add16(a, z::N0, z::UPOS, z::UINS);
    a.jsr("ins_reg");
    add16(a, z::UINS, z::UINS, z::N1);
    dec16(a, z::HOLD);
    zero16(a, z::HOLD);
    a.bne(again);
    a.rts();
}

/// `J`: a count's lines, at least two, joined, a space between each
/// unless the line joined is empty, its leading spaces dropped.
fn joining(a: &mut Asm) {
    a.here("cmd_join");
    a.jsr("count1");
    dec16(a, z::N1);
    zero16(a, z::N1);
    let some = a.local();
    a.bne(some);
    a.inc(zp(z::N1));
    a.bind(some);
    a.jsr("hold_n1");
    // What the joins replace: from the cursor line's CR to the last line's
    // first character that is not a space.
    a.jsr("n0_cur");
    a.jsr("line_end");
    a.jsr("rstart_n0");
    a.jsr("rend_n0");
    let line = a.here_local();
    let found = a.local();
    let no_more = a.local();
    inc16(a, z::N0);
    a.jsr("n0_vs_len");
    a.bcs(no_more);
    a.jsr("first_not_blank");
    a.jsr("rend_n0");
    dec16(a, z::HOLD);
    zero16(a, z::HOLD);
    a.beq(found);
    a.jsr("line_end");
    a.jmp(line);
    // No line after: those found are joined, a lone line refused.
    a.bind(no_more);
    a.lda(zp(z::RSTART));
    a.cmp(zp(z::REND));
    a.bne(found);
    a.lda(zp(z::RSTART + 1));
    a.cmp(zp(z::REND + 1));
    far(a, Asm::bne, "fail");
    a.bind(found);
    a.jsr("range_len");
    a.jsr("n0_rstart");
    a.jsr("snapshot");
    // Each CR and the spaces after it become one space, or nothing before
    // an empty line.
    a.jsr("n0_rstart");
    let join = a.here_local();
    let joined = a.local();
    cmp16(a, z::N0, z::REND);
    a.bcs(joined);
    a.jsr("getc");
    a.cmp(imm(CR));
    let past = a.local();
    a.bne(past);
    a.jsr("hold_n0");
    inc16(a, z::N0);
    a.jsr("first_not_blank");
    sub16(a, z::N1, z::N0, z::HOLD);
    sub16(a, z::REND, z::REND, z::N1);
    a.jsr("n0_hold");
    a.jsr("del_range");
    a.jsr("n0_vs_len");
    let spaced = a.local();
    a.bcs(spaced);
    a.jsr("getc");
    a.cmp(imm(CR));
    a.beq(spaced);
    a.jsr("cur_n0");
    a.lda(imm(b' '));
    a.jsr("ins_char");
    far(a, Asm::bcc, "no_room");
    inc16(a, z::REND);
    a.jsr("n0_cur");
    a.jmp(join);
    a.bind(spaced);
    a.jmp(join);
    a.bind(past);
    inc16(a, z::N0);
    a.jmp(join);
    a.bind(joined);
    sub16(a, z::UINS, z::REND, z::RSTART);
    a.jsr("lines_fit");
    a.jsr("n0_rstart");
    a.jmp("edited");
}

/// `u`: the last change undone, which a second undo redoes.
/// `.`: the last change made again, with its count or the one typed.
fn undoing(a: &mut Asm) {
    a.here("cmd_undo");
    a.lda(zp(z::UNDO_OK));
    far(a, Asm::bne, "fail");
    // Room for whichever of the two is longer.
    sub16(a, z::N1, z::REG, z::TEND);
    cmp16(a, z::N1, z::UINS);
    let longer = a.local();
    a.bcs(longer);
    mov16(a, z::N1, z::UINS);
    a.bind(longer);
    a.jsr("gap_room");
    far(a, Asm::bcc, "no_room");
    a.jsr("n0_upos");
    a.jsr("edit_at");
    a.jsr("move_gap");
    // What was taken away put back before what the change left.
    sub16(a, z::HOLD, z::REG, z::TEND);
    mov16(a, z::P1, z::TEND);
    a.jsr("p2_gap");
    mov16(a, z::N1, z::HOLD);
    a.jsr("memmove");
    add16(a, z::GAP, z::GAP, z::HOLD);
    add16(a, z::GOFF, z::GOFF, z::HOLD);
    add16(a, z::LEN, z::LEN, z::HOLD);
    // What the change left kept, then taken out.
    mov16(a, z::N1, z::UINS);
    a.jsr("resize_undo");
    a.jsr("p1_gend");
    mov16(a, z::P2, z::TEND);
    mov16(a, z::N1, z::UINS);
    a.jsr("memmove");
    add16(a, z::GEND, z::GEND, z::UINS);
    sub16(a, z::LEN, z::LEN, z::UINS);
    mov16(a, z::UINS, z::HOLD);
    a.lda(zp(z::FILE));
    a.ora(imm(file::MODIFIED));
    a.sta(zp(z::FILE));
    a.jsr("n0_upos");
    a.jmp("edited");

    // `lines_fit`: the change just made undone and refused where a line it
    // touches has grown past 255 characters. REND used.
    a.here("lines_fit");
    add16(a, z::REND, z::UPOS, z::UINS);
    a.jsr("n0_upos");
    a.jsr("line_start");
    let line = a.here_local();
    let too_long = a.local();
    a.jsr("n2_n0");
    a.jsr("line_end");
    sub16(a, z::N1, z::N0, z::N2);
    a.lda(zp(z::N1 + 1));
    a.bne(too_long);
    cmp16(a, z::N0, z::REND);
    a.bcs(RTS);
    inc16(a, z::N0);
    a.jsr("n0_vs_len");
    a.bcc(line);
    a.rts();
    a.bind(too_long);
    a.jsr("cmd_undo");
    a.lda(imm(0));
    a.sta(zp(z::UNDO_OK));
    a.jsr("say_too_long");
    a.jmp("main");

    a.here("cmd_repeat");
    a.lda(zp(z::DOT_LEN));
    far(a, Asm::bne, "fail");
    zero16(a, z::COUNT);
    let counted = a.local();
    a.bne(counted);
    a.lda(abs(page::DOT_COUNT));
    a.sta(zp(z::COUNT));
    a.lda(abs(page::DOT_COUNT + 1));
    a.sta(zp(z::COUNT + 1));
    a.bind(counted);
    a.lda(imm(1));
    a.sta(zp(z::DOT_AT));
    a.lda(abs(page::DOT));
    a.sta(zp(z::KEY));
    a.jmp("dispatch_key");
}
