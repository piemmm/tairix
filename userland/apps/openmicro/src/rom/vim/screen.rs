//! The screen: the text's rows above a status line, drawn only where they
//! are owed, each row a line from the first column shown, coloured as the
//! mode's colours allow; the hardware scrolls a row on where it can.
//!
//! A row writes one column fewer than the mode has, so that writing its
//! last never scrolls the screen.

use super::buffer::LINE_BUF;
use super::ops::{cmp16, dec16, inc16, mov16, sub16};
use super::ws::{option, page, show, zp as z};
use crate::rom::asm::{abs, byte, imm, zp, Acc, Asm, RTS};
use crate::rom::os::entry;

/// Each mode's columns, rows and colours, by number.
pub(super) const MODES: [(u8, u8, Colours); 8] = [
    (80, 32, Colours::Two),
    (40, 32, Colours::Four),
    (20, 32, Colours::Sixteen),
    (80, 25, Colours::Two),
    (40, 32, Colours::Two),
    (20, 32, Colours::Four),
    (40, 25, Colours::Two),
    (40, 25, Colours::Teletext),
];

/// The colours a mode has to highlight with.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Colours {
    /// Two: none to spare.
    Two,
    /// Four, the palette set for three classes of text.
    Four,
    /// Sixteen.
    Sixteen,
    /// Teletext's seven, each change a control code in a cell.
    Teletext,
}

/// The classes of text highlighting tells apart.
pub(super) mod class {
    pub const NORMAL: u8 = 0;
    pub const KEYWORD: u8 = 1;
    pub const STRING: u8 = 2;
    pub const NUMBER: u8 = 3;
    pub const COMMENT: u8 = 4;
    pub const COMMAND: u8 = 5;
    pub const COUNT: usize = 6;
}

/// Each class's colour: teletext's control code, the sixteen colour
/// modes' colour, and the four colour modes' with the palette below.
const TELETEXT: [u8; class::COUNT] = [0x87, 0x83, 0x86, 0x82, 0x85, 0x81];
const SIXTEEN: [u8; class::COUNT] = [7, 3, 6, 2, 5, 1];
const FOUR: [u8; class::COUNT] = [3, 1, 2, 2, 2, 1];
/// The four colour modes' palette: logical 1, 2 and 3 as yellow, cyan and
/// white.
const PALETTE: [(u8, u8); 3] = [(1, 3), (2, 6), (3, 7)];
/// The VDU 19 calls that set [`PALETTE`]: each logical colour, its physical
/// colour and three zeros.
const PALETTE_VDU: [u8; 6 * PALETTE.len()] = {
    let mut bytes = [0; 6 * PALETTE.len()];
    let mut at = 0;
    while at < PALETTE.len() {
        bytes[6 * at] = VDU_PALETTE;
        bytes[6 * at + 1] = PALETTE[at].0;
        bytes[6 * at + 2] = PALETTE[at].1;
        at += 1;
    }
    bytes
};

/// The gutter `number` draws: five digits and a space.
pub(super) const GUTTER: u8 = 6;
/// VDU codes.
const VDU_COLOUR: u8 = 17;
const VDU_PALETTE: u8 = 19;
const VDU_MODE: u8 = 22;
const VDU_TAB: u8 = 31;
const VDU_HOME: u8 = 30;
const VDU_UP: u8 = 11;
const VDU_DOWN: u8 = 10;
/// The walk's flag that it draws, not measures.
const DRAWS: u8 = 0x80;

pub(super) fn emit(a: &mut Asm) {
    modes(a);
    refresh(a);
    scrolling(a);
    keeping_rows(a);
    columns(a);
    rows(a);
    walk(a);
    classes(a);
    status(a);
    output(a);
    tables(a);
}

/// `set_mode`: the mode in [`z::MODE`] selected, its geometry and palette
/// taken, everything owed.
fn modes(a: &mut Asm) {
    a.here("set_mode");
    a.lda(imm(VDU_MODE));
    a.jsr(entry::OSWRCH);
    a.lda(zp(z::MODE));
    a.jsr(entry::OSWRCH);
    a.ldx(zp(z::MODE));
    a.lda(abs("mode_rows").x());
    a.sec();
    a.sbc(imm(1));
    a.sta(zp(z::ROWS));
    a.lda(abs("mode_cols").x());
    a.sec();
    a.sbc(imm(1));
    a.sta(zp(z::WIDTH));
    a.lda(abs("mode_colours").x());
    a.cmp(imm(Colours::Four as u8));
    let palette_done = a.local();
    a.bne(palette_done);
    a.ldx(imm(0));
    let palette = a.here_local();
    a.lda(abs("palette_vdu").x());
    a.jsr(entry::OSWRCH);
    a.inx();
    a.cpx(imm(byte(PALETTE_VDU.len())));
    a.bne(palette);
    a.bind(palette_done);
    a.lda(imm(show::ALL));
    a.ora(zp(z::SHOW));
    a.sta(zp(z::SHOW));
    a.rts();

    // `owe`: what the bits of SHOW in A name owed to the next refresh.
    a.here("owe");
    a.ora(zp(z::SHOW));
    a.sta(zp(z::SHOW));
    a.rts();
    a.later(|a| {
        a.here("palette_vdu");
        a.bytes(&PALETTE_VDU);
    });

    // `colours`: the mode's colours, as [`Colours`], in A; zero set for
    // two. X kept.
    a.here("colours");
    a.ldy(zp(z::MODE));
    a.lda(abs("mode_colours").y());
    a.rts();
}

/// `refresh`: the cursor kept on the screen, what is owed drawn, the
/// status line, and the cursor placed.
fn refresh(a: &mut Asm) {
    a.here("refresh");
    a.jsr("keep_rows");
    a.jsr("keep_columns");
    a.lda(zp(z::SHOW));
    let from = a.local();
    let line = a.local();
    let drawn = a.local();
    a.bpl(from);
    a.lda(imm(0));
    a.jsr("draw_rows");
    a.jmp(drawn);
    a.bind(from);
    a.and(imm(show::FROM));
    a.beq(line);
    a.lda(zp(z::FROM_ROW));
    a.jsr("draw_rows");
    a.jmp(drawn);
    a.bind(line);
    a.lda(zp(z::SHOW));
    a.and(imm(show::LINE));
    a.beq(drawn);
    a.jsr("n0_lstart");
    a.lda(zp(z::CROW));
    a.jsr("draw_one");
    a.bind(drawn);
    a.lda(zp(z::SHOW));
    a.and(imm(!(show::ALL | show::FROM | show::LINE)));
    a.sta(zp(z::SHOW));
    a.jsr("status");
    a.lda(imm(VDU_TAB));
    a.jsr(entry::OSWRCH);
    a.lda(zp(z::WAT));
    a.jsr(entry::OSWRCH);
    a.lda(zp(z::CROW));
    a.jmp(abs(entry::OSWRCH));
}

/// The rows kept on the cursor.
fn scrolling(a: &mut Asm) {
    // Further: the cursor's line half the rows down, lower where the text
    // ends before the rows under it, higher where it starts before those
    // above it.
    a.here("centre_rows");
    a.jsr("n0_lstart");
    a.lda(zp(z::ROWS));
    a.lsr(Acc);
    a.sta(zp(z::T0));
    a.lda(zp(z::ROWS));
    a.clc();
    a.sbc(zp(z::T0));
    a.sta(zp(z::T1));
    a.lda(imm(0));
    a.sta(zp(z::T2));
    let down = a.here_local();
    let counted = a.local();
    a.lda(zp(z::T2));
    a.cmp(zp(z::T1));
    a.bcs(counted);
    a.jsr("next_line");
    a.bcs(counted);
    a.inc(zp(z::T2));
    a.bne(down);
    a.bind(counted);
    a.lda(zp(z::ROWS));
    a.clc();
    a.sbc(zp(z::T2));
    a.sta(zp(z::T0));
    a.jsr("n0_lstart");
    mov16(a, z::TOPLN, z::LINE);
    let up = a.here_local();
    let placed = a.local();
    a.lda(zp(z::T0));
    a.beq(placed);
    a.jsr("prev_line");
    a.bcs(placed);
    dec16(a, z::TOPLN);
    a.dec(zp(z::T0));
    a.jmp(up);
    a.bind(placed);
    mov16(a, z::TOP, z::N0);
    a.lda(imm(show::ALL));
    a.ora(zp(z::SHOW));
    a.sta(zp(z::SHOW));
    a.jmp("cursor_row");
}

/// `keep_rows` and `cursor_row`: the top line moved to keep the cursor's
/// line on the screen, and the cursor's row.
fn keeping_rows(a: &mut Asm) {
    // `keep_rows`: the top line moved so that the cursor's line is on the
    // screen: a row's scroll where it is one off, the screen centred on
    // it where it is further.
    a.here("keep_rows");
    cmp16(a, z::LINE, z::TOPLN);
    let below_top = a.local();
    a.bcs(below_top);
    // Above the top: one row is scrolled down.
    sub16(a, z::N0, z::TOPLN, z::LINE);
    a.lda(zp(z::N0 + 1));
    let centre = a.local();
    a.bne(centre);
    a.lda(zp(z::N0));
    a.cmp(imm(1));
    a.bne(centre);
    a.bit(zp(z::SHOW));
    a.bmi(centre);
    mov16(a, z::TOP, z::LSTART);
    dec16(a, z::TOPLN);
    a.lda(imm(VDU_HOME));
    a.jsr(entry::OSWRCH);
    a.lda(imm(VDU_UP));
    a.jsr(entry::OSWRCH);
    let owed_line = a.local();
    a.jmp(owed_line);
    a.bind(centre);
    a.jmp("centre_rows");
    a.bind(below_top);
    sub16(a, z::N0, z::LINE, z::TOPLN);
    a.lda(zp(z::N0 + 1));
    a.bne(centre);
    a.lda(zp(z::N0));
    a.cmp(zp(z::ROWS));
    let on_screen = a.local();
    a.bcc(on_screen);
    a.bne(centre);
    a.bit(zp(z::SHOW));
    a.bmi(centre);
    // One past the bottom: the screen scrolls up a row.
    a.jsr("n0_top");
    a.jsr("next_line");
    mov16(a, z::TOP, z::N0);
    inc16(a, z::TOPLN);
    a.lda(imm(VDU_TAB));
    a.jsr(entry::OSWRCH);
    a.lda(imm(0));
    a.jsr(entry::OSWRCH);
    a.lda(zp(z::ROWS));
    a.jsr(entry::OSWRCH);
    a.lda(imm(VDU_DOWN));
    a.jsr(entry::OSWRCH);
    // The rows owed have moved up with the rest.
    a.lda(zp(z::FROM_ROW));
    let top = a.local();
    a.beq(top);
    a.dec(zp(z::FROM_ROW));
    a.bind(top);
    a.bind(owed_line);
    a.lda(imm(show::LINE));
    a.ora(zp(z::SHOW));
    a.sta(zp(z::SHOW));
    let rows_kept = a.local();
    a.jmp(rows_kept);
    a.bind(rows_kept);
    a.bind(on_screen);
    a.here("cursor_row");
    a.sec();
    a.lda(zp(z::LINE));
    a.sbc(zp(z::TOPLN));
    a.sta(zp(z::CROW));
    a.rts();
}

/// The columns kept on the cursor.
fn columns(a: &mut Asm) {
    // `keep_columns`: the first column shown moved so that the cursor's
    // column is on the screen, its screen column measured into WAT.
    a.here("keep_columns");
    a.lda(zp(z::COL));
    a.cmp(zp(z::LEFT));
    let shift = a.local();
    a.bcc(shift);
    a.jsr("measure");
    a.bcc(RTS);
    a.bind(shift);
    a.lda(zp(z::WIDTH));
    a.lsr(Acc);
    a.sta(zp(z::T0));
    a.lda(zp(z::COL));
    a.sec();
    a.sbc(zp(z::T0));
    let clamped = a.local();
    a.bcs(clamped);
    a.lda(imm(0));
    a.bind(clamped);
    a.sta(zp(z::LEFT));
    a.lda(imm(show::ALL));
    a.ora(zp(z::SHOW));
    a.sta(zp(z::SHOW));
    a.jsr("measure");
    a.bcc(RTS);
    // Still off: the cursor's column first.
    a.lda(zp(z::COL));
    a.sta(zp(z::LEFT));
    a.jmp("measure");

    // `measure`: the cursor's screen column into WAT; carry set where it
    // falls past the row.
    a.here("measure");
    a.jsr("n0_lstart");
    a.jsr("fetch_line");
    a.lda(zp(z::COL));
    a.sta(zp(z::WSTOP));
    mov16(a, z::N2, z::LINE);
    a.lda(imm(0));
    a.jmp("walk");
}

/// `draw_rows`: the rows from A down. `draw_one`: row A as the line at N0.
fn rows(a: &mut Asm) {
    a.here("draw_rows");
    a.sta(zp(z::ROW_FIRST));
    a.jsr("n0_top");
    mov16(a, z::N2, z::TOPLN);
    a.lda(imm(0));
    a.sta(zp(z::ROW));
    // The line starting row A, past the rows above it.
    let find = a.here_local();
    let found = a.local();
    let beyond = a.local();
    a.lda(zp(z::ROW));
    a.cmp(zp(z::ROW_FIRST));
    a.bcs(found);
    a.jsr("next_line");
    a.bcs(beyond);
    inc16(a, z::N2);
    a.inc(zp(z::ROW));
    a.bne(find);
    let row = a.here_local();
    a.bind(found);
    a.lda(zp(z::ROW));
    a.cmp(zp(z::ROWS));
    a.bcs(RTS);
    a.jsr("draw_row");
    a.jsr("next_line");
    a.bcs(beyond);
    inc16(a, z::N2);
    a.inc(zp(z::ROW));
    a.bne(row);
    // The rows past the text's last line.
    a.bind(beyond);
    a.inc(zp(z::ROW));
    let tilde = a.here_local();
    a.lda(zp(z::ROW));
    a.cmp(zp(z::ROWS));
    a.bcs(RTS);
    a.jsr("tab_row");
    a.lda(imm(b'~'));
    a.jsr(entry::OSWRCH);
    a.lda(imm(1));
    a.sta(zp(z::WX));
    a.jsr("pad");
    a.inc(zp(z::ROW));
    a.bne(tilde);
    a.rts();

    // `draw_row`: row ROW as the line at N0, numbered N2; N0 and N2 kept.
    a.here("draw_row");
    a.jsr("tab_row");
    a.jsr("fetch_line");
    a.lda(zp(z::N0));
    a.pha();
    a.lda(zp(z::N0 + 1));
    a.pha();
    a.lda(imm(DRAWS));
    a.jsr("walk");
    a.pla();
    a.sta(zp(z::N0 + 1));
    a.pla();
    a.sta(zp(z::N0));
    a.rts();

    a.here("draw_one");
    a.sta(zp(z::ROW));
    mov16(a, z::N2, z::LINE);
    a.jmp("draw_row");

    // `tab_row`: the text cursor to row ROW's start.
    a.here("tab_row");
    a.lda(imm(VDU_TAB));
    a.jsr(entry::OSWRCH);
    a.lda(imm(0));
    a.jsr(entry::OSWRCH);
    a.lda(zp(z::ROW));
    a.jmp(abs(entry::OSWRCH));
}

/// `walk`: the line [`LINE_BUF`] holds walked from its start, its runs
/// coloured as the lexer finds them, from the first column shown to the
/// row's end: drawn where A has [`DRAWS`], numbered N2 where `number` is
/// on; otherwise measured, the screen column of column WSTOP left in WAT,
/// carry set where it falls past the row.
fn walk(a: &mut Asm) {
    const _: () = assert!(option::NUMBER == 0x80);
    a.here("walk");
    a.sta(zp(z::WDRAW));
    a.lda(imm(0));
    a.sta(zp(z::WX));
    a.sta(zp(z::WCOL));
    a.sta(zp(z::RLEN));
    a.sta(zp(z::WCLASS));
    a.jsr("lex_start");
    a.bit(zp(z::OPTIONS));
    let numbered = a.local();
    a.bpl(numbered);
    a.lda(imm(GUTTER));
    a.sta(zp(z::WX));
    a.bit(zp(z::WDRAW));
    a.bpl(numbered);
    a.jsr("print_gutter");
    a.bind(numbered);
    let next = a.here_local();
    let ended = a.local();
    let hidden = a.local();
    a.ldy(zp(z::WCOL));
    a.cpy(zp(z::LLEN));
    a.bcs(ended);
    a.lda(zp(z::RLEN));
    let in_run = a.local();
    a.bne(in_run);
    a.jsr("lex_run");
    a.bind(in_run);
    a.lda(zp(z::WCOL));
    a.cmp(zp(z::LEFT));
    a.bcc(hidden);
    a.lda(zp(z::RCLASS));
    a.cmp(zp(z::WCLASS));
    let same = a.local();
    a.beq(same);
    a.jsr("set_class");
    a.bind(same);
    let full = a.local();
    a.lda(zp(z::WX));
    a.cmp(zp(z::WIDTH));
    a.bcs(full);
    a.bit(zp(z::WDRAW));
    let draw = a.local();
    a.bmi(draw);
    a.lda(zp(z::WCOL));
    a.cmp(zp(z::WSTOP));
    let measured = a.local();
    a.beq(measured);
    let counted = a.local();
    a.bne(counted);
    a.bind(draw);
    a.ldy(zp(z::WCOL));
    a.lda(abs(LINE_BUF).y());
    a.jsr("show_char");
    a.bind(counted);
    a.inc(zp(z::WX));
    a.bind(hidden);
    a.inc(zp(z::WCOL));
    a.dec(zp(z::RLEN));
    a.jmp(next);
    // The line's end: a measure finds its column there.
    a.bind(ended);
    a.bit(zp(z::WDRAW));
    let pad = a.local();
    a.bmi(pad);
    a.lda(zp(z::WX));
    a.cmp(zp(z::WIDTH));
    a.bcs(RTS);
    a.bind(measured);
    a.lda(zp(z::WX));
    a.sta(zp(z::WAT));
    a.clc();
    a.rts();
    a.bind(full);
    a.bit(zp(z::WDRAW));
    a.bmi(pad);
    a.sec();
    a.rts();
    a.bind(pad);
    a.jsr("pad");
    a.lda(imm(class::NORMAL));
    a.jmp("set_class");
}

/// `set_class`, `print_gutter` and `pad`.
fn classes(a: &mut Asm) {
    // `set_class`: the colour of class A from here, a teletext code
    // taking a cell where it is drawn or measured.
    a.here("set_class");
    a.sta(zp(z::WCLASS));
    a.tax();
    a.jsr("colours");
    a.beq(RTS);
    a.cmp(imm(Colours::Teletext as u8));
    let palette = a.local();
    a.bne(palette);
    a.lda(zp(z::WX));
    a.cmp(zp(z::WIDTH));
    a.bcs(RTS);
    a.inc(zp(z::WX));
    a.bit(zp(z::WDRAW));
    a.bpl(RTS);
    a.lda(abs("teletext_colours").x());
    a.jmp(abs(entry::OSWRCH));
    a.bind(palette);
    a.bit(zp(z::WDRAW));
    a.bpl(RTS);
    a.tay();
    a.lda(imm(VDU_COLOUR));
    a.jsr(entry::OSWRCH);
    a.cpy(imm(Colours::Four as u8));
    let sixteen = a.local();
    a.bne(sixteen);
    a.lda(abs("four_colours").x());
    a.jmp(abs(entry::OSWRCH));
    a.bind(sixteen);
    a.lda(abs("sixteen_colours").x());
    a.jmp(abs(entry::OSWRCH));

    // `print_gutter`: N2 in five columns and a space.
    a.here("print_gutter");
    a.jsr("n1_n2");
    a.jsr("decimal");
    a.lda(imm(5));
    a.sec();
    a.sbc(zp(z::T0));
    a.tax();
    let spaces = a.here_local();
    let digits = a.local();
    a.beq(digits);
    a.lda(imm(b' '));
    a.jsr(entry::OSWRCH);
    a.dex();
    a.jmp(spaces);
    a.bind(digits);
    a.jsr("print_decimal");
    a.lda(imm(b' '));
    a.jmp(abs(entry::OSWRCH));

    // `pad`: spaces from screen column WX to the row's end.
    a.here("pad");
    let more = a.here_local();
    a.lda(zp(z::WX));
    a.cmp(zp(z::WIDTH));
    a.bcs(RTS);
    a.lda(imm(b' '));
    a.jsr(entry::OSWRCH);
    a.inc(zp(z::WX));
    a.bne(more);
    a.rts();
}

/// `status`: the status line: the message where one is kept, or the mode
/// and the cursor's line and column.
fn status(a: &mut Asm) {
    a.here("status");
    a.lda(zp(z::ROWS));
    a.sta(zp(z::ROW));
    a.jsr("tab_row");
    a.lda(imm(0));
    a.sta(zp(z::WX));
    a.lda(zp(z::SHOW));
    a.and(imm(show::MESSAGE));
    let mode = a.local();
    a.beq(mode);
    a.ldx(imm(0));
    let message = a.here_local();
    let shown = a.local();
    a.lda(abs(page::MESSAGE).x());
    a.beq(shown);
    a.ldy(zp(z::WX));
    a.cpy(zp(z::WIDTH));
    a.bcs(shown);
    a.jsr("show_char");
    a.inc(zp(z::WX));
    a.inx();
    a.bne(message);
    a.bind(shown);
    a.jmp("pad");
    a.bind(mode);
    a.lda(zp(z::SHOW));
    a.and(imm(show::INSERT));
    let ruler = a.local();
    a.beq(ruler);
    a.ldx(imm(0));
    let named = a.here_local();
    a.lda(abs("insert_name").x());
    a.beq(ruler);
    a.jsr(entry::OSWRCH);
    a.inc(zp(z::WX));
    a.inx();
    a.bne(named);
    // The ruler at its column, where the mode's name leaves room for it.
    a.bind(ruler);
    a.lda(zp(z::WIDTH));
    a.sec();
    a.sbc(imm(RULER));
    a.cmp(zp(z::WX));
    a.bcc(shown);
    a.sta(zp(z::T3));
    let at = a.here_local();
    let placed = a.local();
    a.lda(zp(z::WX));
    a.cmp(zp(z::T3));
    a.bcs(placed);
    a.lda(imm(b' '));
    a.jsr(entry::OSWRCH);
    a.inc(zp(z::WX));
    a.bne(at);
    a.bind(placed);
    mov16(a, z::N1, z::LINE);
    a.jsr("print_number");
    a.lda(imm(b','));
    a.jsr(entry::OSWRCH);
    a.inc(zp(z::WX));
    a.lda(zp(z::COL));
    a.clc();
    a.adc(imm(1));
    a.sta(zp(z::N1));
    a.lda(imm(0));
    a.adc(imm(0));
    a.sta(zp(z::N1 + 1));
    a.jsr("print_number");
    a.jmp("pad");
    a.later(|a| {
        a.here("insert_name");
        a.text("-- INSERT --");
        a.byte(0);
    });
}

/// The columns the ruler takes: a line and a column, as `65535,255`.
const RULER: u8 = 10;

/// Output: `show_char` draws A, a dot for a character the screen cannot
/// show as itself; `decimal` turns N1 into its digits; `print_decimal`
/// prints them, and `print_number` both, counting them into WX.
fn output(a: &mut Asm) {
    a.here("show_char");
    a.cmp(imm(b' '));
    let odd = a.local();
    a.bcc(odd);
    a.cmp(imm(0x7F));
    a.bcs(odd);
    a.jmp(abs(entry::OSWRCH));
    a.bind(odd);
    a.lda(imm(b'.'));
    a.jmp(abs(entry::OSWRCH));

    // `decimal`: N1's digits into [`page::DIGITS`], most significant first,
    // without leading zeros; their count in T0. X kept.
    a.here("decimal");
    a.txa();
    a.pha();
    a.lda(imm(0));
    a.sta(zp(z::T0));
    a.ldx(imm(0));
    let power = a.here_local();
    a.ldy(imm(b'0'));
    let take = a.here_local();
    let counted = a.local();
    a.lda(zp(z::N1));
    a.sec();
    a.sbc(abs("powers_low").x());
    a.sta(zp(z::T1));
    a.lda(zp(z::N1 + 1));
    a.sbc(abs("powers_high").x());
    a.bcc(counted);
    a.sta(zp(z::N1 + 1));
    a.lda(zp(z::T1));
    a.sta(zp(z::N1));
    a.iny();
    a.bne(take);
    a.bind(counted);
    let store = a.local();
    let skip = a.local();
    a.cpy(imm(b'0'));
    a.bne(store);
    a.lda(zp(z::T0));
    a.bne(store);
    a.cpx(imm(byte(POWERS.len()) - 1));
    a.bne(skip);
    a.bind(store);
    a.tya();
    a.ldy(zp(z::T0));
    a.sta(abs(page::DIGITS).y());
    a.inc(zp(z::T0));
    a.bind(skip);
    a.inx();
    a.cpx(imm(byte(POWERS.len())));
    a.bne(power);
    a.pla();
    a.tax();
    a.rts();

    a.here("print_decimal");
    a.ldx(imm(0));
    let digit = a.here_local();
    a.cpx(zp(z::T0));
    a.beq(RTS);
    a.lda(abs(page::DIGITS).x());
    a.jsr(entry::OSWRCH);
    a.inx();
    a.bne(digit);
    a.rts();

    a.here("print_number");
    a.jsr("decimal");
    a.jsr("print_decimal");
    a.lda(zp(z::WX));
    a.clc();
    a.adc(zp(z::T0));
    a.sta(zp(z::WX));
    a.rts();
}

/// The powers of ten `decimal` counts in, from the highest.
const POWERS: [u16; 5] = [10_000, 1_000, 100, 10, 1];

fn tables(a: &mut Asm) {
    a.later(|a| {
        a.here("mode_cols");
        for (cols, _, _) in MODES {
            a.byte(cols);
        }
        a.here("mode_rows");
        for (_, rows, _) in MODES {
            a.byte(rows);
        }
        a.here("mode_colours");
        for (_, _, colours) in MODES {
            a.byte(colours as u8);
        }
        a.here("teletext_colours");
        a.bytes(&TELETEXT);
        a.here("sixteen_colours");
        a.bytes(&SIXTEEN);
        a.here("four_colours");
        a.bytes(&FOUR);
        a.here("powers_low");
        for power in POWERS {
            a.byte(power.to_le_bytes()[0]);
        }
        a.here("powers_high");
        for power in POWERS {
            a.byte(power.to_le_bytes()[1]);
        }
    });
}
