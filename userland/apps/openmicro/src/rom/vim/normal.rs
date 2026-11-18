//! Normal mode: a count, then a command from the table; a motion moves the
//! cursor, a change is recorded for `.`. And the cursor's rules: where a
//! place puts it, and where normal mode lets it stand.

use super::ops::{add16, cmp16, dec16, inc16, mov16, sub16, zero16};
use super::ws::{page, show, zp as z};
use crate::rom::asm::{abs, hi, imm, lo, zp, Acc, Asm, RTS};
use crate::rom::far;
use crate::rom::os::CR;

/// A command's flags in the table.
pub(super) mod flag {
    /// It changes the text, and `.` repeats it.
    pub const CHANGE: u8 = 0x80;
    /// It is a motion, which an operator may take.
    pub const MOTION: u8 = 0x40;
    /// It keeps the column a vertical move wants.
    pub const KEEP_WANT: u8 = 0x20;
}

/// The cursor keys' codes, while they give codes.
pub(super) mod cursor_key {
    pub const LEFT: u8 = 0x88;
    pub const RIGHT: u8 = 0x89;
    pub const DOWN: u8 = 0x8A;
    pub const UP: u8 = 0x8B;
}

/// DELETE, and the control keys the commands use.
pub(super) const DELETE: u8 = 0x7F;
const fn ctrl(letter: u8) -> u8 {
    letter & 0x1F
}

/// The commands: each key, its flags, and its routine.
const COMMANDS: &[(u8, u8, &str)] = &[
    (b'h', flag::MOTION, "mo_left"),
    (cursor_key::LEFT, flag::MOTION, "mo_left"),
    (DELETE, flag::MOTION, "mo_left"),
    (ctrl(b'H'), flag::MOTION, "mo_left"),
    (b'l', flag::MOTION, "mo_right"),
    (cursor_key::RIGHT, flag::MOTION, "mo_right"),
    (b' ', flag::MOTION, "mo_right"),
    (b'j', flag::MOTION | flag::KEEP_WANT, "mo_down"),
    (cursor_key::DOWN, flag::MOTION | flag::KEEP_WANT, "mo_down"),
    (ctrl(b'N'), flag::MOTION | flag::KEEP_WANT, "mo_down"),
    (b'k', flag::MOTION | flag::KEEP_WANT, "mo_up"),
    (cursor_key::UP, flag::MOTION | flag::KEEP_WANT, "mo_up"),
    (ctrl(b'P'), flag::MOTION | flag::KEEP_WANT, "mo_up"),
    (CR, flag::MOTION, "mo_down_first"),
    (b'+', flag::MOTION, "mo_down_first"),
    (b'-', flag::MOTION, "mo_up_first"),
    (b'0', flag::MOTION, "mo_bol"),
    (b'^', flag::MOTION, "mo_first"),
    (b'$', flag::MOTION | flag::KEEP_WANT, "mo_eol"),
    (b'w', flag::MOTION, "mo_word"),
    (b'b', flag::MOTION, "mo_back"),
    (b'e', flag::MOTION, "mo_end"),
    (b'G', flag::MOTION, "mo_goto"),
    (b'g', flag::MOTION, "mo_g"),
    (b'H', flag::MOTION, "mo_top"),
    (b'M', flag::MOTION, "mo_middle"),
    (b'L', flag::MOTION, "mo_bottom"),
    (b'x', flag::CHANGE, "cmd_x"),
    (b'X', flag::CHANGE, "cmd_big_x"),
    (b'd', flag::CHANGE, "cmd_operator"),
    (b'c', flag::CHANGE, "cmd_operator"),
    (b'y', 0, "cmd_operator"),
    (b'D', flag::CHANGE, "cmd_big_d"),
    (b'C', flag::CHANGE, "cmd_big_c"),
    (b'Y', 0, "cmd_big_y"),
    (b'p', flag::CHANGE, "cmd_put_after"),
    (b'P', flag::CHANGE, "cmd_put_before"),
    (b'J', flag::CHANGE, "cmd_join"),
    (b'r', flag::CHANGE, "cmd_replace"),
    (b'~', flag::CHANGE, "cmd_case"),
    (b'i', flag::CHANGE, "cmd_insert"),
    (b'a', flag::CHANGE, "cmd_append"),
    (b'I', flag::CHANGE, "cmd_insert_first"),
    (b'A', flag::CHANGE, "cmd_append_end"),
    (b'o', flag::CHANGE, "cmd_open_below"),
    (b'O', flag::CHANGE, "cmd_open_above"),
    (b'u', 0, "cmd_undo"),
    (b'.', 0, "cmd_repeat"),
    (b'/', 0, "cmd_search_forward"),
    (b'?', 0, "cmd_search_backward"),
    (b'n', 0, "cmd_search_next"),
    (b'N', 0, "cmd_search_previous"),
    (b':', 0, "ex_line"),
    (b'Z', 0, "cmd_z"),
    (ctrl(b'F'), 0, "cmd_page_down"),
    (ctrl(b'B'), 0, "cmd_page_up"),
    (ctrl(b'D'), 0, "cmd_half_down"),
    (ctrl(b'U'), 0, "cmd_half_up"),
    (ctrl(b'G'), 0, "cmd_info"),
    (ctrl(b'L'), 0, "cmd_redraw"),
];

/// The most a count reaches; digits past it are not taken.
const MOST_COUNT: u16 = 9999;

pub(super) fn emit(a: &mut Asm) {
    dispatch(a);
    counting(a);
    cursor(a);
    paging(a);
    screens(a);
    table(a);
}

/// `normal_key`: the key in A, after a count, done as its command.
/// `find_command`: the entry for A, carry set where there is none, its
/// routine in P3 and its flags in T4.
fn dispatch(a: &mut Asm) {
    const _: () = assert!(flag::CHANGE == 0x80);
    a.here("normal_key");
    a.sta(zp(z::KEY));
    let digits = a.here_local();
    let command = a.local();
    let digit = a.local();
    a.lda(zp(z::KEY));
    a.cmp(imm(b'1'));
    let maybe_zero = a.local();
    a.bcc(maybe_zero);
    a.cmp(imm(b'9' + 1));
    a.bcc(digit);
    a.bind(maybe_zero);
    a.cmp(imm(b'0'));
    a.bne(command);
    zero16(a, z::COUNT);
    a.beq(command);
    a.bind(digit);
    a.jsr("add_digit");
    a.jsr("read_key");
    a.sta(zp(z::KEY));
    a.jmp(digits);
    a.bind(command);
    a.here("dispatch_key");
    a.lda(zp(z::KEY));
    a.jsr("find_command");
    a.bcs("fail_far");
    a.lda(zp(z::T4));
    a.and(imm(flag::MOTION));
    let not_motion = a.local();
    a.beq(not_motion);
    a.lda(zp(z::T4));
    a.pha();
    a.jsr("call_p3");
    a.pla();
    a.bcs("fail_far");
    a.sta(zp(z::T4));
    a.jsr("place_cursor");
    a.lda(zp(z::T4));
    a.and(imm(flag::KEEP_WANT));
    a.bne(RTS);
    a.lda(zp(z::COL));
    a.sta(zp(z::WANT));
    a.rts();
    a.bind(not_motion);
    a.lda(zp(z::T4));
    a.bpl("call_p3");
    a.jsr("begin_record");
    a.here("call_p3");
    a.jmp(abs(z::P3).ind());

    a.here("fail_far");
    a.jmp("fail");

    a.here("find_command");
    a.ldx(imm(0));
    let scan = a.here_local();
    let found = a.local();
    a.pha();
    a.lda(abs("commands").x());
    let none = a.local();
    a.beq(none);
    a.pla();
    a.cmp(abs("commands").x());
    a.beq(found);
    a.inx();
    a.inx();
    a.inx();
    a.inx();
    a.bne(scan);
    a.bind(none);
    a.pla();
    a.sec();
    a.rts();
    a.bind(found);
    a.lda(abs(crate::rom::asm::sym("commands") + 1).x());
    a.sta(zp(z::T4));
    a.lda(abs(crate::rom::asm::sym("commands") + 2).x());
    a.sta(zp(z::P3));
    a.lda(abs(crate::rom::asm::sym("commands") + 3).x());
    a.sta(zp(z::P3 + 1));
    a.clc();
    a.rts();
}

/// A count's digits, and a change recorded for `.`.
fn counting(a: &mut Asm) {
    // `add_digit`: the digit KEY added to the count, ten times over, to
    // its most.
    a.here("add_digit");
    let take = a.local();
    let refused = MOST_COUNT / 10 + 1;
    a.lda(zp(z::COUNT + 1));
    a.cmp(imm(hi(refused)));
    a.bcc(take);
    a.bne(RTS);
    a.lda(zp(z::COUNT));
    a.cmp(imm(lo(refused)));
    a.bcs(RTS);
    a.bind(take);
    // COUNT × 10 is COUNT × 2 + COUNT × 8.
    a.asl(zp(z::COUNT));
    a.rol(zp(z::COUNT + 1));
    a.jsr("n1_count");
    a.asl(zp(z::N1));
    a.rol(zp(z::N1 + 1));
    a.asl(zp(z::N1));
    a.rol(zp(z::N1 + 1));
    add16(a, z::COUNT, z::N1, z::COUNT);
    a.lda(zp(z::KEY));
    a.and(imm(0x0F));
    a.clc();
    a.adc(zp(z::COUNT));
    a.sta(zp(z::COUNT));
    a.bcc(RTS);
    a.inc(zp(z::COUNT + 1));
    a.rts();

    // `begin_record`: the change about to be made recorded for `.`: its
    // count and key, then the keys it reads; not while one is played back.
    a.here("begin_record");
    a.lda(zp(z::DOT_AT));
    a.cmp(imm(0xFF));
    a.bne(RTS);
    a.lda(zp(z::COUNT));
    a.sta(abs(page::DOT_COUNT));
    a.lda(zp(z::COUNT + 1));
    a.sta(abs(page::DOT_COUNT + 1));
    a.lda(zp(z::KEY));
    a.sta(abs(page::DOT));
    a.lda(imm(1));
    a.sta(zp(z::DOT_LEN));
    a.sta(zp(z::RECORD));
    a.rts();
}

/// The cursor's rules. `set_cursor`: the cursor at N0, its line, column
/// and line number found. `fix_cursor`: the cursor where normal mode lets
/// it stand: on a character of its line, or the start of an empty one;
/// `place_cursor` the same for N0.
fn cursor(a: &mut Asm) {
    a.here("set_cursor");
    a.jsr("cur_n0");
    a.jsr("line_start");
    mov16(a, z::LSTART, z::N0);
    sub16(a, z::N1, z::CUR, z::LSTART);
    a.lda(zp(z::N1 + 1));
    let narrow = a.local();
    a.beq(narrow);
    a.lda(imm(0xFF));
    a.sta(zp(z::N1));
    a.bind(narrow);
    a.lda(zp(z::N1));
    a.sta(zp(z::COL));
    cmp16(a, z::LSTART, z::TOP);
    let from_start = a.local();
    a.bcc(from_start);
    mov16(a, z::N2, z::TOPLN);
    a.jsr("n0_top");
    let counted = a.local();
    a.jmp(counted);
    a.bind(from_start);
    a.lda(imm(0));
    a.sta(zp(z::N0));
    a.sta(zp(z::N0 + 1));
    a.sta(zp(z::N2 + 1));
    a.lda(imm(1));
    a.sta(zp(z::N2));
    a.bind(counted);
    mov16(a, z::N1, z::LSTART);
    a.jsr("count_crs");
    mov16(a, z::LINE, z::N2);
    a.rts();

    a.here("place_cursor");
    a.jsr("cur_n0");
    a.here("fix_cursor");
    a.jsr("n0_cur");
    a.jsr("n0_vs_len");
    let in_text = a.local();
    let place = a.local();
    a.bcc(in_text);
    zero16(a, z::LEN);
    let some = a.local();
    a.bne(some);
    a.lda(imm(0));
    a.sta(zp(z::N0));
    a.sta(zp(z::N0 + 1));
    a.beq(place);
    a.bind(some);
    a.jsr("n0_len");
    dec16(a, z::N0);
    a.bind(in_text);
    a.jsr("getc");
    a.cmp(imm(CR));
    a.bne(place);
    zero16(a, z::N0);
    a.beq(place);
    dec16(a, z::N0);
    a.jsr("getc");
    a.cmp(imm(CR));
    a.bne(place);
    inc16(a, z::N0);
    a.bind(place);
    a.jmp("set_cursor");
}

/// The screen's commands: CTRL-F, CTRL-B, CTRL-D, CTRL-U, CTRL-L and
/// CTRL-G.
fn paging(a: &mut Asm) {
    // `rows_less`: N1 the text rows less A, at least 1.
    a.here("rows_less");
    a.sta(zp(z::T0));
    a.lda(zp(z::ROWS));
    a.sec();
    a.sbc(zp(z::T0));
    let one = a.local();
    let some = a.local();
    a.bcc(one);
    a.bne(some);
    a.bind(one);
    a.lda(imm(1));
    a.bind(some);
    a.sta(zp(z::N1));
    a.lda(imm(0));
    a.sta(zp(z::N1 + 1));
    a.rts();

    // `top_by`: the top line moved T4 lines, down or up as A is 0 or not,
    // as far as there are.
    a.here("top_by");
    a.pha();
    a.lda(zp(z::T4));
    a.sta(zp(z::N1));
    a.lda(imm(0));
    a.sta(zp(z::N1 + 1));
    a.jsr("n0_top");
    a.pla();
    a.pha();
    let up = a.local();
    let moved = a.local();
    a.bne(up);
    a.jsr("lines_down");
    a.jmp(moved);
    a.bind(up);
    a.jsr("lines_up");
    a.bind(moved);
    a.pla();
    a.fall_into("top_moved");

    // `top_moved`: the top line set to N0, T4 lines down or up as A is 0
    // or not.
    a.here("top_moved");
    a.tax();
    mov16(a, z::TOP, z::N0);
    a.txa();
    let up = a.local();
    a.bne(up);
    a.lda(zp(z::TOPLN));
    a.clc();
    a.adc(zp(z::T4));
    a.sta(zp(z::TOPLN));
    a.bcc(RTS);
    a.inc(zp(z::TOPLN + 1));
    a.rts();
    a.bind(up);
    a.lda(zp(z::TOPLN));
    a.sec();
    a.sbc(zp(z::T4));
    a.sta(zp(z::TOPLN));
    a.bcs(RTS);
    a.dec(zp(z::TOPLN + 1));
    a.rts();

    // `land`: the cursor on N0's line's first character that is not a
    // space, everything owed.
    a.here("land");
    a.jsr("first_not_blank");
    a.jsr("place_cursor");
    a.lda(imm(show::ALL));
    a.jmp("owe");
}

/// The commands that page, redraw, and tell of the file.
fn screens(a: &mut Asm) {
    // CTRL-F: a screen less two rows on, the cursor on the top line.
    a.here("cmd_page_down");
    a.lda(imm(2));
    a.jsr("rows_less");
    a.jsr("n0_top");
    a.jsr("lines_down");
    far(a, Asm::bcc, "fail");
    a.lda(imm(0));
    a.jsr("top_moved");
    a.jsr("n0_top");
    a.jmp("land");

    // CTRL-B: a screen less two rows back, the cursor on the bottom line.
    a.here("cmd_page_up");
    a.lda(imm(2));
    a.jsr("rows_less");
    a.jsr("n0_top");
    a.jsr("lines_up");
    far(a, Asm::bcc, "fail");
    a.lda(imm(1));
    a.jsr("top_moved");
    a.lda(imm(1));
    a.jsr("rows_less");
    a.jsr("n0_top");
    a.jsr("lines_down");
    a.jmp("land");

    // CTRL-D, CTRL-U: the cursor and the screen half a screen on or back.
    for (name, step, back) in [
        ("cmd_half_down", "lines_down", 0),
        ("cmd_half_up", "lines_up", 1),
    ] {
        a.here(name);
        a.lda(zp(z::ROWS));
        a.lsr(Acc);
        a.sta(zp(z::N1));
        a.lda(imm(0));
        a.sta(zp(z::N1 + 1));
        a.jsr("n0_lstart");
        a.jsr(step);
        far(a, Asm::bcc, "fail");
        a.jsr("hold_n0");
        a.lda(imm(back));
        a.jsr("top_by");
        a.jsr("n0_hold");
        a.jmp("land");
    }

    a.here("cmd_redraw");
    a.lda(imm(show::ALL));
    a.jmp("owe");

    // CTRL-G: the file's name and its lines.
    a.here("cmd_info");
    a.jsr("msg_begin");
    a.jsr("msg_name");
    a.lda(imm(b' '));
    a.jsr("msg_put");
    a.jsr("count_lines");
    a.jsr("msg_number");
    a.call_with_text("msg_text", &[" lines"]);
    a.jmp("msg_end");

    // `count_lines`: the text's lines in N1: its CRs, and one more for a
    // last line without one.
    a.here("count_lines");
    a.lda(imm(0));
    a.sta(zp(z::N0));
    a.sta(zp(z::N0 + 1));
    a.sta(zp(z::N2));
    a.sta(zp(z::N2 + 1));
    mov16(a, z::N1, z::LEN);
    a.jsr("count_crs");
    a.jsr("n1_n2");
    zero16(a, z::LEN);
    a.beq(RTS);
    a.jsr("n0_len");
    dec16(a, z::N0);
    a.jsr("getc");
    a.cmp(imm(CR));
    a.beq(RTS);
    inc16(a, z::N1);
    a.rts();
}

fn table(a: &mut Asm) {
    // `find_command` steps X four bytes an entry, the terminator included.
    const _: () = assert!(COMMANDS.len() * 4 < 0x100);
    a.later(|a| {
        a.here("commands");
        for &(key, flags, routine) in COMMANDS {
            a.byte(key);
            a.byte(flags);
            a.word(routine);
        }
        a.byte(0);
    });
}
