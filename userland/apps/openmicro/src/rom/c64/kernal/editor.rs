//! The screen editor: CINT, the screen's clear and home, the logical lines
//! of up to two rows the line link table records, printing with the control
//! codes and quote and insert modes, scrolling, line input from the keyboard
//! and from the screen, PLOT and SCREEN.
//!
//! The cursor is TBLX, its row, and PNTR, its column in the logical line
//! that PNT and USER address in screen and colour memory and whose last
//! column LNMX holds.

use super::files::device;
use super::fixed;
use super::irq::BLINK_JIFFIES;
use super::keyboard::{DELAY_AT_RESET, KOUNT_JIFFIES, LOWER_CASE, NO_KEY};
use super::ws::{page, zp};
use crate::machine::c64::cia::reg;
use crate::machine::c64::io;
use crate::machine::c64::keyboard::Key;
use crate::machine::c64::vic::{irq as vic_irq, reg as vic};
use crate::rom::asm::{abs, byte, hi, imm, lo, sym, zp as z, Acc, Asm, Label, RTS};
use crate::rom::c64::petscii::{CURSOR_RIGHT, REVERSE_ON};

const COLUMNS: u8 = 40;
const ROWS: u8 = 25;
const LAST_ROW: u8 = ROWS - 1;
/// A logical line's last column: of one row, and of two.
const SHORT_LINE: u8 = COLUMNS - 1;
const LONG_LINE: u8 = 2 * COLUMNS - 1;
/// LDTB1's bit marking a row that starts a logical line.
const LINE_START: u8 = 0x80;
/// Colour RAM's first page.
const COLOUR_PAGE: u8 = io::COLOUR.to_be_bytes()[0];
const SPACE: u8 = 0x20;
const QUOTE: u8 = 0x22;
const RETURN: u8 = 0x0D;
const SHIFT_RETURN: u8 = 0x8D;
const DELETE: u8 = 0x14;
const INSERT: u8 = 0x94;
/// MODE's bit that stops SHIFT and the Commodore key changing the set.
const MODE_LOCKED: u8 = 0x80;
/// SHIFT-RUN/STOP's code, which types [`LOAD_RUN`].
const LOAD_RUN_KEY: u8 = 0x83;
const LOAD_RUN: &[u8] = b"LOAD\rRUN\r";
/// The screen code of π, which PETSCII `$FF` prints as.
const PI: u8 = 0x5E;
/// The text colour CINT sets: light blue.
const TEXT_COLOUR: u8 = 14;
/// The keyboard buffer's size CINT sets.
const KEYBOARD_BUFFER: u8 = 10;
/// The control register with bit 8 of the raster line to interrupt at
/// set, and its value from CINT.
const RASTER_HIGH: u8 = 0x80;
/// The raster line only a PAL frame reaches: its last.
const PAL_LAST_LINE: u16 = 311;
/// Each row's pause while CTRL slows a scroll, in 1280-cycle steps.
const SLOW_SCROLL: u8 = 22;
/// CTRL's row, read with the STOP key's column selected.
const CONTROL_ROW: u8 = 1 << Key::CONTROL.row();

/// The VIC-II's registers as CINT sets them from `$D000`: sprites off at
/// the origin, 25 rows of 40 columns showing the screen at `$0400` in the
/// upper-case set, a light blue border on blue, and the other colours,
/// the sprites' among them, counting up.
#[rustfmt::skip]
const VIC_DEFAULTS: [u8; 47] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0x1B, 0, 0, 0, 0, 0x08, 0, 0x14, 0, 0, 0, 0, 0, 0, 0,
    14, 6, 1, 2, 3, 4, 0, 1, 2, 3, 4, 5, 6, 7, 12,
];
const CR1: u8 = VIC_DEFAULTS[vic::CR1 as usize];
const _: () = assert!(
    vic::IRQ_ENABLE > vic::CR1
        && vic::IRQ_ENABLE > vic::RASTER
        && VIC_DEFAULTS[vic::IRQ_ENABLE as usize] == 0,
    "CINT finds the standard with the VIC-II's interrupts off, its raster still to set"
);

/// What CINT sets from XMAX and from KEYLOG on: the keyboard buffer's size,
/// the repeat for the keys that repeat and its delays, KEYLOG's default,
/// MODE and AUTODN.
const SETTINGS: [u8; 8] = [
    KEYBOARD_BUFFER,
    0,
    KOUNT_JIFFIES,
    DELAY_AT_RESET,
    fixed::KEYLOG.to_le_bytes()[0],
    fixed::KEYLOG.to_le_bytes()[1],
    0,
    0,
];
const _: () = assert!(
    page::RPTFLG == page::XMAX + 1
        && page::KOUNT == page::XMAX + 2
        && page::DELAY == page::XMAX + 3
        && page::MODE == page::KEYLOG + 2
        && page::AUTODN == page::KEYLOG + 3,
    "CINT's settings lie in two runs of four"
);

/// Each row's offset from the screen's start: its low byte, and its page.
const ROW_OFFSETS: ([u8; ROWS as usize], [u8; ROWS as usize]) = {
    let (mut low, mut page) = ([0; ROWS as usize], [0; ROWS as usize]);
    let mut row = 0;
    while row < ROWS {
        let [l, p] = (row as u16 * COLUMNS as u16).to_le_bytes();
        low[row as usize] = l;
        page[row as usize] = p;
        row += 1;
    }
    (low, page)
};

/// The control codes the editor carries out, each with its routine; the
/// colour codes are [`COLOUR_CODES`]'.
const CONTROLS: [(u8, &str); 16] = [
    (RETURN, "edit_return"),
    (SHIFT_RETURN, "edit_return"),
    (0x08, "edit_lock"),
    (0x09, "edit_unlock"),
    (0x0E, "edit_lower"),
    (0x8E, "edit_upper"),
    (0x11, "edit_down"),
    (0x91, "edit_up"),
    (CURSOR_RIGHT, "edit_right"),
    (0x9D, "edit_left"),
    (REVERSE_ON, "edit_reverse_on"),
    (0x92, "edit_reverse_off"),
    (0x13, "home"),
    (0x93, "clear_screen"),
    (DELETE, "edit_delete"),
    (INSERT, "edit_insert"),
];

/// The codes that set the text colour, in colour order: black, white, red,
/// cyan, purple, green, blue, yellow, orange, brown, light red, dark grey,
/// grey, light green, light blue, light grey.
pub const COLOUR_CODES: [u8; 16] = [
    0x90, 0x05, 0x1C, 0x9F, 0x9C, 0x1E, 0x1F, 0x9E, 0x81, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0x9B,
];

/// A screen code's PETSCII by the code's top three bits, as what it is
/// exclusive-ored with; then the same for a reversed code in quotes, a
/// control code where its symbol shows one.
const FROM_SCREEN: [u8; 12] = [
    0x40, 0x00, 0x80, 0xC0, 0xC0, 0x80, 0x00, 0x40, 0x80, 0x80, 0x40, 0x40,
];

/// The editor's tables, each held apart for the room a pin leaves.
const TABLES: [(&str, &[u8]); 6] = [
    ("vic_defaults", &VIC_DEFAULTS),
    ("settings", &SETTINGS),
    ("row_low", &ROW_OFFSETS.0),
    ("row_page", &ROW_OFFSETS.1),
    ("from_screen", &FROM_SCREEN),
    ("load_run", LOAD_RUN),
];

pub fn emit(a: &mut Asm) {
    cint(a);
    lines(a);
    rows(a);
    scrolling(a);
    output(a);
    control(a);
    mode_controls(a);
    cursor_controls(a);
    editing_controls(a);
    input(a);
    screen(a);
    for (name, table) in TABLES {
        a.later(move |a| {
            a.here(name);
            a.bytes(table);
        });
    }
    // The colour codes then the control codes, searched as one run.
    a.later(|a| {
        a.here("edit_codes");
        a.bytes(&COLOUR_CODES);
        for (code, _) in CONTROLS {
            a.byte(code);
        }
    });
    a.later(|a| {
        a.here("control_low");
        for (_, routine) in CONTROLS {
            a.byte(lo(sym(routine) + -1));
        }
    });
    a.later(|a| {
        a.here("control_high");
        for (_, routine) in CONTROLS {
            a.byte(hi(sym(routine) + -1));
        }
    });
}

/// CINT: the VIC-II's registers, the standard found as they are set, the
/// editor's and the keyboard's state, the channels, the screen cleared and
/// the jiffy interrupt started at the standard's rate.
fn cint(a: &mut Asm) {
    a.here("cint");
    let (registers, next, settings) = (a.local(), a.local(), a.local());
    a.ldx(imm(byte(VIC_DEFAULTS.len() - 1)));
    a.bind(registers);
    a.lda(abs("vic_defaults").x());
    a.sta(abs(io::VIC).x());
    // The interrupts off, the standard is found before the raster's
    // registers, lower down, are set.
    a.cpx(imm(vic::IRQ_ENABLE));
    a.bne(next);
    detect_standard(a);
    a.bind(next);
    a.dex();
    a.bpl(registers);
    let half = SETTINGS.len() / 2;
    a.ldx(imm(byte(half - 1)));
    a.bind(settings);
    a.lda(abs("settings").x());
    a.sta(abs(page::XMAX).x());
    a.lda(abs(sym("settings") + i32::from(byte(half))).x());
    a.sta(abs(page::KEYLOG).x());
    a.dex();
    a.bpl(settings);
    a.lda(imm(0));
    for at in [zp::DFLTN, zp::RVS, zp::QTSW, zp::INSRT, zp::CRSW, zp::NDX] {
        a.sta(z(at));
    }
    a.lda(imm(TEXT_COLOUR));
    a.sta(abs(page::COLOR));
    a.lda(imm(device::SCREEN));
    a.sta(z(zp::DFLTO));
    a.lda(imm(BLINK_JIFFIES));
    a.sta(z(zp::BLNCT));
    a.sta(z(zp::BLNSW));
    a.lda(imm(NO_KEY));
    a.sta(z(zp::LSTX));
    a.sta(z(zp::SFDX));
    a.jsr("clear_screen");
    a.jmp("start_jiffies");
}

/// TVSFLG set where the raster reaches the last PAL line in a frame; the
/// raster's registers are left to CINT's loop.
fn detect_standard(a: &mut Asm) {
    let [line_low, line_high] = PAL_LAST_LINE.to_le_bytes();
    let cr1 = io::vic(vic::CR1);
    let (low_half, high_half, frame_end) = (a.local(), a.local(), a.local());
    a.lda(imm(line_low));
    a.sta(abs(io::vic(vic::RASTER)));
    a.lda(imm(CR1 | (RASTER_HIGH * line_high)));
    a.sta(abs(cr1));
    a.bind(low_half);
    a.lda(abs(cr1));
    a.bmi(low_half);
    a.lda(imm(vic_irq::RASTER));
    a.sta(abs(io::vic(vic::IRQ)));
    a.bind(high_half);
    a.lda(abs(cr1));
    a.bpl(high_half);
    a.bind(frame_end);
    a.lda(abs(cr1));
    a.bmi(frame_end);
    a.lda(abs(io::vic(vic::IRQ)));
    a.and(imm(vic_irq::RASTER));
    a.sta(abs(page::TVSFLG));
}

/// PLOT; the clear, which starts every row as its own logical line; RETURN
/// and the next line; the home; and the cursor's logical line found from
/// TBLX with PNTR its column in that row.
fn lines(a: &mut Asm) {
    // PLOT: with C set the cursor read, its row in X and its column in the
    // logical line in Y; with C clear moved to row X, column Y.
    a.here("plot");
    let set = a.local();
    a.bcc(set);
    a.ldx(z(zp::TBLX));
    a.ldy(z(zp::PNTR));
    a.rts();

    a.here("clear_screen");
    let clear = a.local();
    a.ldx(imm(0));
    a.bind(clear);
    a.jsr("clear_row");
    a.lda(z(zp::SAL + 1));
    a.ora(imm(LINE_START));
    a.sta(z(zp::LDTB1).x());
    a.inx();
    a.cpx(imm(ROWS));
    a.bne(clear);
    a.sta(z(zp::LDTB1).x());
    a.beq("home");

    // RETURN and SHIFT-RETURN: the modes ended, then the next line.
    a.here("edit_return");
    a.lda(imm(0));
    a.sta(z(zp::QTSW));
    a.sta(z(zp::RVS));
    a.sta(z(zp::INSRT));
    a.fall_into("next_line");

    // The cursor to the start of the next logical line.
    a.here("next_line");
    let (one_row, placed) = (a.local(), a.local());
    a.jsr("line_start");
    a.inx();
    a.lda(z(zp::LNMX));
    a.cmp(imm(LONG_LINE));
    a.bne(one_row);
    a.inx();
    a.bind(one_row);
    a.stx(z(zp::TBLX));
    a.cpx(imm(ROWS));
    a.bcc(placed);
    a.jsr("scroll_up");
    a.ldx(z(zp::TBLX));
    a.bind(placed);
    // Column 0 of row X; the home enters inside the BIT, at row 0.
    a.skip_over(|a| {
        a.here("home");
        a.ldx(imm(0));
    });
    a.ldy(imm(0));
    a.bind(set);
    a.stx(z(zp::TBLX));
    a.sty(z(zp::PNTR));
    a.fall_into("fix_line");

    // The row TBLX is in is its logical line's second when that row does
    // not start one; PNTR then counts on from the first.
    a.here("fix_line");
    a.jsr("line_start");
    a.cpx(z(zp::TBLX));
    a.beq("line_pointers");
    a.lda(z(zp::PNTR));
    a.clc();
    a.adc(imm(COLUMNS));
    a.sta(z(zp::PNTR));

    // PNT, USER and LNMX at the line whose first row is X.
    a.here("line_pointers");
    let last = a.local();
    a.lda(abs("row_low").x());
    a.sta(z(zp::PNT));
    a.sta(z(zp::USER));
    a.lda(abs("row_page").x());
    a.ora(imm(COLOUR_PAGE));
    a.sta(z(zp::USER + 1));
    a.lda(abs("row_page").x());
    a.clc();
    a.adc(abs(page::HIBASE));
    a.sta(z(zp::PNT + 1));
    a.lda(imm(SHORT_LINE));
    a.cpx(imm(LAST_ROW));
    a.beq(last);
    a.ldy(z(zp::LDTB1 + 1).x());
    a.bmi(last);
    a.lda(imm(LONG_LINE));
    a.bind(last);
    a.sta(z(zp::LNMX));
    a.rts();

    // X the first row of the cursor's logical line.
    a.here("line_start");
    let (find, found) = (a.local(), a.local());
    a.ldx(z(zp::TBLX));
    a.bind(find);
    a.lda(z(zp::LDTB1).x());
    a.bmi(found);
    a.dex();
    a.bpl(find);
    a.inx();
    a.bind(found);
    a.rts();
}

/// Rows as wholes: SAL at a row's screen memory, a row cleared to spaces in
/// the text colour, and a row moved, its link with it.
fn rows(a: &mut Asm) {
    // SAL at row X's screen memory.
    a.here("row_screen");
    a.lda(abs("row_low").x());
    a.sta(z(zp::SAL));
    a.lda(abs("row_page").x());
    a.clc();
    a.adc(abs(page::HIBASE));
    a.sta(z(zp::SAL + 1));
    a.rts();

    // Row X cleared, SAL left at its screen memory and TEMP at its colour
    // memory; X kept.
    a.here("clear_row");
    let fill = a.local();
    a.jsr("row_screen");
    a.lda(z(zp::SAL));
    a.sta(z(zp::TEMP));
    a.lda(abs("row_page").x());
    a.ora(imm(COLOUR_PAGE));
    a.sta(z(zp::TEMP + 1));
    a.ldy(imm(COLUMNS - 1));
    a.bind(fill);
    a.lda(imm(SPACE));
    a.sta(z(zp::SAL).ind_y());
    a.lda(abs(page::COLOR));
    a.sta(z(zp::TEMP).ind_y());
    a.dey();
    a.bpl(fill);
    a.rts();

    // Row Y moved to the row below or above; Y kept.
    a.here("move_down");
    a.tya();
    a.tax();
    a.inx();
    let moved = a.local();
    a.bne(moved);
    a.here("move_up");
    a.tya();
    a.tax();
    a.dex();
    a.bind(moved);
    a.jsr("row_screen");
    a.lda(abs("row_low").y());
    a.sta(z(zp::TEMP));
    a.lda(abs("row_page").y());
    a.clc();
    a.adc(abs(page::HIBASE));
    a.sta(z(zp::TEMP + 1));
    a.jsr("copy_row");
    a.lda(abs("row_page").x());
    a.ora(imm(COLOUR_PAGE));
    a.sta(z(zp::SAL + 1));
    a.lda(abs("row_page").y());
    a.ora(imm(COLOUR_PAGE));
    a.sta(z(zp::TEMP + 1));
    a.jsr("copy_row");
    // The link bit from row Y, the rest from row X.
    a.lda(abs(zp::LDTB1).y());
    a.eor(z(zp::LDTB1).x());
    a.and(imm(LINE_START));
    a.eor(z(zp::LDTB1).x());
    a.sta(z(zp::LDTB1).x());
    a.rts();

    // A row's bytes from (TEMP) to (SAL); Y kept.
    a.here("copy_row");
    let copy = a.local();
    a.tya();
    a.pha();
    a.ldy(imm(COLUMNS - 1));
    a.bind(copy);
    a.lda(z(zp::TEMP).ind_y());
    a.sta(z(zp::SAL).ind_y());
    a.dey();
    a.bpl(copy);
    a.pla();
    a.tay();
    a.rts();
}

/// The screen scrolled up a row, and again while its top row continues a
/// line scrolled off, CTRL slowing it; and a line grown into a second row.
fn scrolling(a: &mut Asm) {
    a.here("scroll_up");
    let (moving, slow) = (a.local(), a.local());
    a.ldy(imm(1));
    a.bind(moving);
    a.jsr("move_up");
    a.iny();
    a.cpy(imm(ROWS));
    a.bne(moving);
    a.ldx(imm(LAST_ROW));
    a.jsr("clear_row");
    a.lda(z(zp::LDTB1).x());
    a.ora(imm(LINE_START));
    a.sta(z(zp::LDTB1).x());
    a.dec(z(zp::TBLX));
    a.dec(z(zp::LXSP));
    a.lda(z(zp::LDTB1));
    a.bpl("scroll_up");
    a.lda(abs(io::cia1(reg::PRB)));
    a.and(imm(CONTROL_ROW));
    a.bne(RTS);
    a.ldx(imm(SLOW_SCROLL));
    a.ldy(imm(0));
    a.bind(slow);
    a.dey();
    a.bne(slow);
    a.dex();
    a.bne(slow);
    a.rts();

    // The cursor's line, a row at TBLX, given a second row, the screen
    // first scrolled where it is the bottom one: with AUTODN 0 the rows
    // below move down to make room, the bottom one lost. The cursor stays.
    a.here("extend_line");
    let (room, shift, opened, link) = (a.local(), a.local(), a.local(), a.local());
    a.lda(z(zp::TBLX));
    a.cmp(imm(LAST_ROW));
    a.bne(room);
    a.jsr("scroll_up");
    a.bind(room);
    a.ldx(z(zp::TBLX));
    a.inx();
    a.lda(abs(page::AUTODN));
    a.bne(link);
    a.ldy(imm(LAST_ROW - 1));
    a.bind(shift);
    a.cpy(z(zp::TBLX));
    a.beq(opened);
    a.jsr("move_down");
    a.dey();
    a.bpl(shift);
    a.bind(opened);
    a.ldx(z(zp::TBLX));
    a.inx();
    a.jsr("clear_row");
    a.bind(link);
    a.lda(z(zp::LDTB1).x());
    a.and(imm(!LINE_START));
    a.sta(z(zp::LDTB1).x());
    a.dex();
    a.jmp("line_pointers");
}

/// Printing: a printable character as its screen code in the text colour,
/// reversed in RVS; a control code carried out, or in quote or insert mode
/// shown reversed (DEL still carried out in quote mode); then the cursor on,
/// a line full at its first row growing a second.
fn output(a: &mut Asm) {
    a.here("screen_out");
    let (high, control_low, control_high, put, quoted_low, quoted_high, not_quote, below_graphics) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    let (insert_key, execute) = (a.local(), a.local());
    a.sta(z(zp::DATA));
    a.pha();
    a.txa();
    a.pha();
    a.tya();
    a.pha();
    a.lda(z(zp::DATA));
    a.bmi(high);
    a.cmp(imm(SPACE));
    a.bcc(control_low);
    a.cmp(imm(QUOTE));
    a.bne(not_quote);
    a.jsr("toggle_quote");
    a.bind(not_quote);
    a.cmp(imm(0x60));
    a.bcc(below_graphics);
    a.and(imm(0xDF));
    a.bne(put);
    a.bind(below_graphics);
    a.and(imm(0x3F));
    a.bpl(put);

    a.bind(high);
    let not_pi = a.local();
    a.cmp(imm(0xA0));
    a.bcc(control_high);
    a.cmp(imm(0xFF));
    a.bne(not_pi);
    a.lda(imm(PI));
    a.bne(put);
    a.bind(not_pi);
    a.and(imm(0x7F));
    a.cmp(imm(0x40));
    a.bcs(put);
    a.ora(imm(0x40));
    a.bne(put);

    a.bind(control_low);
    a.cmp(imm(RETURN));
    a.beq(execute);
    a.ldx(z(zp::INSRT));
    a.bne(quoted_low);
    a.ldx(z(zp::QTSW));
    a.beq(execute);
    a.cmp(imm(DELETE));
    a.beq(execute);
    a.bind(quoted_low);
    a.ora(imm(0x80));
    a.bne(put);

    a.bind(control_high);
    a.cmp(imm(SHIFT_RETURN));
    a.beq(execute);
    a.cmp(imm(INSERT));
    a.beq(insert_key);
    a.ldx(z(zp::INSRT));
    a.bne(quoted_high);
    a.ldx(z(zp::QTSW));
    a.beq(execute);
    a.bind(quoted_high);
    // $80 to $9F show as $C0 to $DF.
    a.ora(imm(0x40));
    a.bne(put);
    // INST inserts in insert mode too, but shows in quotes.
    a.bind(insert_key);
    a.ldx(z(zp::QTSW));
    a.beq(execute);
    a.ldx(z(zp::INSRT));
    a.beq(quoted_high);
    // The control's routine returns here.
    a.bind(execute);
    a.jsr("edit_control");
    a.jmp("edit_done");
    screen_put(a, put);
}

/// `screen_out` run on at `put` with a screen code: put at the cursor in
/// the text colour, reversed in RVS, an insert counted off. `edit_done`:
/// the registers restored, C clear.
fn screen_put(a: &mut Asm, put: Label) {
    let (plain, counted, within, wrap) = (a.local(), a.local(), a.local(), a.local());
    a.bind(put);
    a.ldx(z(zp::RVS));
    a.beq(plain);
    a.ora(imm(0x80));
    a.bind(plain);
    a.ldx(z(zp::INSRT));
    a.beq(counted);
    a.dec(z(zp::INSRT));
    a.bind(counted);
    a.ldy(z(zp::PNTR));
    a.sta(z(zp::PNT).ind_y());
    a.lda(abs(page::COLOR));
    a.sta(z(zp::USER).ind_y());
    // The cursor a column on: past a line's last column a line of one row
    // grows a second and one of two goes on to the next line; the cursor's
    // row follows it onto column 40.
    a.inc(z(zp::PNTR));
    a.lda(z(zp::LNMX));
    a.cmp(z(zp::PNTR));
    a.bcs(within);
    a.cmp(imm(LONG_LINE));
    a.beq(wrap);
    a.jsr("extend_line");
    a.bind(within);
    a.lda(z(zp::PNTR));
    a.cmp(imm(COLUMNS));
    a.bne("edit_done");
    a.inc(z(zp::TBLX));
    a.bne("edit_done");
    a.bind(wrap);
    a.jsr("next_line");
    a.here("edit_done");
    a.pla();
    a.tay();
    a.pla();
    a.tax();
    a.pla();
    a.clc();
    a.rts();
}

/// A control code carried out, called from `screen_out`: its routine, which
/// returns there, the text colour, or nothing; and quote mode's toggle.
fn control(a: &mut Asm) {
    a.here("edit_control");
    let (find, found, routine) = (a.local(), a.local(), a.local());
    a.ldx(imm(byte(COLOUR_CODES.len() + CONTROLS.len() - 1)));
    a.bind(find);
    a.cmp(abs("edit_codes").x());
    a.beq(found);
    a.dex();
    a.bpl(find);
    a.rts();
    a.bind(found);
    a.cpx(imm(byte(COLOUR_CODES.len())));
    a.bcs(routine);
    a.stx(abs(page::COLOR));
    a.rts();
    a.bind(routine);
    let first = -i32::from(byte(COLOUR_CODES.len()));
    a.lda(abs(sym("control_high") + first).x());
    a.pha();
    a.lda(abs(sym("control_low") + first).x());
    a.pha();
    a.rts();

    a.here("toggle_quote");
    a.pha();
    a.lda(z(zp::QTSW));
    a.eor(imm(1));
    a.sta(z(zp::QTSW));
    a.pla();
    a.rts();
}

/// The control codes' routines that set a mode: the lock and the character
/// set, a bit each, and reversing.
fn mode_controls(a: &mut Asm) {
    let memory = io::vic(vic::MEMORY);
    for (set, clear, at, bit) in [
        ("edit_lock", "edit_unlock", page::MODE, MODE_LOCKED),
        ("edit_lower", "edit_upper", memory, LOWER_CASE),
    ] {
        let store = a.local();
        a.here(set);
        a.lda(abs(at));
        a.ora(imm(bit));
        a.bne(store);
        a.here(clear);
        a.lda(abs(at));
        a.and(imm(!bit));
        a.bind(store);
        a.sta(abs(at));
        a.rts();
    }
    a.here("edit_reverse_on");
    a.lda(imm(REVERSE_ON));
    a.skip_over(|a| {
        a.here("edit_reverse_off");
        a.lda(imm(0));
    });
    a.sta(z(zp::RVS));
    a.rts();
}

/// The cursor's moves: a column, a row, wrapping at the screen's edges and
/// scrolling at its foot.
fn cursor_controls(a: &mut Asm) {
    // The cursor's column in its row in A, Z set at the first; C set.
    a.here("row_column");
    let in_row = a.local();
    a.lda(z(zp::PNTR));
    a.sec();
    a.sbc(imm(COLUMNS));
    a.bcs(in_row);
    a.adc(imm(COLUMNS));
    a.bind(in_row);
    a.rts();

    let (set_column, up, wrap, down_column, moved) =
        (a.local(), a.local(), a.local(), a.local(), a.local());
    a.here("edit_right");
    a.jsr("row_column");
    // C set: the next column.
    a.adc(imm(0));
    a.cmp(imm(COLUMNS));
    a.bcc(set_column);
    a.lda(imm(0));
    a.beq(down_column);

    a.here("edit_left");
    a.jsr("row_column");
    a.beq(wrap);
    a.sbc(imm(1));
    a.bcs(set_column);
    // From the first column to the row above's last, but not off the top.
    a.bind(wrap);
    a.ldx(z(zp::TBLX));
    a.beq(set_column);
    a.lda(imm(COLUMNS - 1));
    a.bne(up);

    a.here("edit_up");
    a.jsr("row_column");
    a.ldx(z(zp::TBLX));
    a.beq(set_column);
    a.bind(up);
    a.dec(z(zp::TBLX));
    a.bind(set_column);
    a.sta(z(zp::PNTR));
    a.jmp("fix_line");

    // Down a row, the screen scrolling at the bottom.
    a.here("edit_down");
    a.jsr("row_column");
    a.bind(down_column);
    a.sta(z(zp::PNTR));
    a.inc(z(zp::TBLX));
    a.lda(z(zp::TBLX));
    a.cmp(imm(ROWS));
    a.bcc(moved);
    a.jsr("scroll_up");
    a.bind(moved);
    a.jmp("fix_line");
}

/// DEL and INST.
fn editing_controls(a: &mut Asm) {
    // DEL: the character left of the cursor taken out, the rest of the line
    // closing up; at the line's start the cursor moves left.
    a.here("edit_delete");
    let (shift, blank) = (a.local(), a.local());
    a.ldy(z(zp::PNTR));
    a.beq("edit_left");
    a.dey();
    a.sty(z(zp::PNTR));
    a.cpy(imm(COLUMNS - 1));
    a.bne(shift);
    a.dec(z(zp::TBLX));
    a.bind(shift);
    a.cpy(z(zp::LNMX));
    a.bcs(blank);
    a.iny();
    a.lda(z(zp::PNT).ind_y());
    a.dey();
    a.sta(z(zp::PNT).ind_y());
    a.iny();
    a.lda(z(zp::USER).ind_y());
    a.dey();
    a.sta(z(zp::USER).ind_y());
    a.iny();
    a.bne(shift);
    // A space in the text colour at Y, INST's too.
    a.bind(blank);
    a.lda(imm(SPACE));
    a.sta(z(zp::PNT).ind_y());
    a.lda(abs(page::COLOR));
    a.sta(z(zp::USER).ind_y());
    a.rts();

    // INST: a space opened at the cursor, the line growing a second row
    // when its last column is taken, refused when two rows are full.
    a.here("edit_insert");
    let (room, shift) = (a.local(), a.local());
    a.ldy(z(zp::LNMX));
    a.lda(z(zp::PNT).ind_y());
    a.cmp(imm(SPACE));
    a.beq(room);
    a.cpy(imm(LONG_LINE));
    a.beq(RTS);
    a.jsr("extend_line");
    a.ldy(z(zp::LNMX));
    a.bind(room);
    a.inc(z(zp::INSRT));
    a.bind(shift);
    a.cpy(z(zp::PNTR));
    a.beq(blank);
    a.dey();
    a.lda(z(zp::PNT).ind_y());
    a.iny();
    a.sta(z(zp::PNT).ind_y());
    a.dey();
    a.lda(z(zp::USER).ind_y());
    a.iny();
    a.sta(z(zp::USER).ind_y());
    a.dey();
    a.jmp(shift);
}

/// Line input: from the keyboard, keys printed and edited until RETURN,
/// then the logical line it was pressed on returned a character at a time,
/// from where input began if it began on that line, its trailing spaces
/// dropped, and RETURN; from the screen, the cursor's line from the cursor.
/// X and Y are kept.
fn input(a: &mut Asm) {
    a.here("editor_in");
    let (key, wait, hidden, not_load_run, enter, copy) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    // C set from the keyboard, clear from the screen.
    a.sec();
    a.skip_over(|a| {
        a.here("screen_in");
        a.clc();
    });
    a.txa();
    a.pha();
    a.tya();
    a.pha();
    a.ldy(z(zp::PNTR));
    a.lda(z(zp::CRSW));
    a.bne("from_line");
    a.lda(z(zp::TBLX));
    a.sta(z(zp::LXSP));
    a.sty(z(zp::LXSP + 1));
    a.bcc(enter);
    // A key from the buffer, the cursor flashing while none is there.
    a.bind(key);
    a.lda(imm(0));
    a.sta(z(zp::BLNSW));
    a.bind(wait);
    a.lda(z(zp::NDX));
    a.beq(wait);
    a.sei();
    a.ldx(imm(0));
    a.lda(z(zp::BLNON));
    a.beq(hidden);
    a.ldy(z(zp::PNTR));
    a.lda(z(zp::GDBLN));
    a.sta(z(zp::PNT).ind_y());
    a.lda(abs(page::GDCOL));
    a.sta(z(zp::USER).ind_y());
    a.stx(z(zp::BLNON));
    a.bind(hidden);
    a.inx();
    a.stx(z(zp::BLNSW));
    a.jsr("take_key");
    a.cmp(imm(LOAD_RUN_KEY));
    a.bne(not_load_run);
    a.ldx(imm(byte(LOAD_RUN.len() - 1)));
    a.bind(copy);
    a.lda(abs("load_run").x());
    a.sta(abs(page::KEYD).x());
    a.dex();
    a.bpl(copy);
    a.lda(imm(byte(LOAD_RUN.len())));
    a.sta(z(zp::NDX));
    a.bne(key);
    a.bind(not_load_run);
    a.cmp(imm(RETURN));
    a.beq(enter);
    a.jsr("screen_out");
    // screen_out returns C clear.
    a.bcc(key);
    a.bind(enter);
    enter_line(a);
    from_line(a);
}

/// The line the cursor is on entered: its end past its last character
/// other than a space, its start, in Y and PNTR, where input began when
/// that was on the cursor's row, else its first column.
fn enter_line(a: &mut Asm) {
    let (trim, ended, start) = (a.local(), a.local(), a.local());
    a.ldy(z(zp::LNMX));
    a.bind(trim);
    a.lda(z(zp::PNT).ind_y());
    a.cmp(imm(SPACE));
    a.bne(ended);
    a.dey();
    a.bpl(trim);
    a.bind(ended);
    a.iny();
    a.sty(z(zp::INDX));
    a.ldx(imm(0));
    a.stx(z(zp::QTSW));
    // Where input began, on the cursor's row; else the line's start.
    a.lda(z(zp::TBLX));
    a.cmp(z(zp::LXSP));
    a.beq(start);
    a.stx(z(zp::LXSP + 1));
    a.bind(start);
    a.ldy(z(zp::LXSP + 1));
    a.sty(z(zp::PNTR));
    a.inx();
    a.stx(z(zp::CRSW));
}

/// The entered line's next character as PETSCII, from the column in Y and
/// PNTR, or RETURN at its end. A reversed control symbol in quotes is its
/// code.
fn from_line(a: &mut Asm) {
    a.here("from_line");
    let (line_end, index, unquoted, returned) = (a.local(), a.local(), a.local(), a.local());
    a.cpy(z(zp::INDX));
    a.bcs(line_end);
    a.lda(z(zp::PNT).ind_y());
    a.pha();
    // The top three bits pick the change, a reversed code in quotes from
    // the four after.
    for _ in 0..5 {
        a.lsr(Acc);
    }
    a.ldx(z(zp::QTSW));
    a.beq(index);
    a.cmp(imm(0x80 >> 5));
    a.bcc(index);
    // C set: four on.
    a.adc(imm(3));
    a.bind(index);
    a.tax();
    a.pla();
    a.eor(abs("from_screen").x());
    a.cmp(imm(QUOTE));
    a.bne(unquoted);
    a.jsr("toggle_quote");
    a.bind(unquoted);
    a.sta(z(zp::DATA));
    a.inc(z(zp::PNTR));
    a.bind(returned);
    a.pla();
    a.tay();
    a.pla();
    a.tax();
    a.lda(z(zp::DATA));
    a.clc();
    a.rts();

    // The line's end: the cursor a column past it, its row as it was, and
    // RETURN; the caller prints the newline, which goes to the screen only
    // while the output does, so the editor moves to the next line itself
    // otherwise.
    a.bind(line_end);
    a.ldx(z(zp::INDX));
    a.stx(z(zp::PNTR));
    a.lda(imm(0));
    a.sta(z(zp::CRSW));
    a.lda(imm(RETURN));
    a.ldx(z(zp::DFLTO));
    a.cpx(imm(device::SCREEN));
    a.beq(unquoted);
    a.jsr("screen_out");
    a.bcc(returned);
}

/// SCREEN: the screen's 40 columns in X and 25 rows in Y.
fn screen(a: &mut Asm) {
    a.here("screen");
    a.ldx(imm(COLUMNS));
    a.ldy(imm(ROWS));
    a.rts();
}
