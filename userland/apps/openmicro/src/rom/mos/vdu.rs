//! The VDU drivers' text side: the queue that gathers each control code's
//! parameters, the control codes, character output in every mode, the
//! cursor, windows, scrolling, colours, the palette and MODE.
//!
//! The text cursor is held in absolute screen columns and rows; `&D8` holds
//! the address of its cell's top line, which wraps by the screen's size
//! once hardware scrolling has moved the screen's start.

use super::ws::{os, page2, var, vdu, vdu_zp};
use super::{has_teletext, return_in_teletext, svia, DELETE};
use crate::machine::bbc::sheila;
use crate::machine::bbc::via::reg;
use crate::machine::electron::control;
use crate::machine::electron::sheila as ula;
use crate::machine::electron::video::{register_bits, SETS};
use crate::machine::Model;
use crate::rom::asm::{abs, hi, imm, lo, zp, Acc, Asm, RTS};

/// Status byte bits.
pub mod status {
    pub const PRINTER: u8 = 0x01;
    pub const PAGED: u8 = 0x04;
    pub const WINDOWED: u8 = 0x08;
    pub const GRAPHICS_CURSOR: u8 = 0x20;
    pub const EDITING: u8 = 0x40;
    pub const DISABLED: u8 = 0x80;
}

/// The 6845's ports.
const CRTC_ADDRESS: u16 = sheila::CRTC_ADDRESS;
const CRTC_DATA: u16 = sheila::CRTC_DATA;
/// Teletext's screen address bias: a mode 7 address less this is the 6845's.
const TELETEXT_BIAS: u16 = 0x5400;

/// Each control code's handler, and how many parameters it gathers first.
const CONTROLS: [(&str, u8); 32] = [
    ("return", 0),
    ("vdu_1", 1),
    ("vdu_2", 0),
    ("vdu_3", 0),
    ("vdu_4", 0),
    ("vdu_5", 0),
    ("vdu_6", 0),
    ("bell", 0),
    ("vdu_8", 0),
    ("vdu_9", 0),
    ("vdu_10", 0),
    ("vdu_11", 0),
    ("vdu_12", 0),
    ("vdu_13", 0),
    ("vdu_14", 0),
    ("vdu_15", 0),
    ("vdu_16", 0),
    ("vdu_17", 1),
    ("vdu_18", 2),
    ("vdu_19", 5),
    ("vdu_20", 0),
    ("vdu_21", 0),
    ("vdu_22", 1),
    ("vdu_23", 9),
    ("vdu_24", 8),
    ("vdu_25", 5),
    ("vdu_26", 0),
    ("return", 0),
    ("vdu_28", 4),
    ("vdu_29", 4),
    ("vdu_30", 0),
    ("vdu_31", 2),
];

/// Where a queued command's last parameter lands; the queue fills up to it.
const QUEUE_LAST: u16 = vdu::QUEUE_END;

/// The Electron's cursor, which it draws itself: drawn on the screen, the
/// VDU drivers at work so it is left alone, and turned off.
mod cursor_state {
    pub const DRAWN: u8 = 0x80;
    pub const BUSY: u8 = 0x40;
    pub const OFF: u8 = 0x20;
}
/// TIME's bit the Electron's cursor flashes on: shown for 16 centiseconds,
/// hidden for 16.
const CURSOR_FLASH: u8 = 0x10;

pub fn emit(a: &mut Asm, model: Model) {
    queue(a);
    simple_controls(a, model);
    printing(a, model);
    cursor(a);
    vdu_cursor_update(a, model);
    vdu_cursor_hide(a, model);
    match model {
        Model::ModelB => a.later(crtc_write),
        Model::Electron => electron_cursor(a),
    }
    scrolling(a, model);
    runs(a);
    clearing(a);
    colours(a, model);
    match model {
        Model::ModelB => ula_palette(a),
        Model::Electron => electron_palette(a),
    }
    vdu_20(a, model);
    windows(a);
    modes(a, model);
    tables(a, model);
}

/// The queue: a control code gathers its parameters, each landing so the
/// last is at `&323`, then its handler runs through the jump vector.
fn queue(a: &mut Asm) {
    a.here("vdu_write_core");
    a.ldx(abs(var::VDU_QUEUE));
    a.bne("vdu_parameter");
    a.bit(zp(vdu_zp::STATUS));
    a.bmi("vdu_while_disabled");
    a.cmp(imm(0x20));
    a.bcc("vdu_control");
    a.cmp(imm(DELETE));
    a.beq("vdu_write_delete");
    a.jmp("vdu_print");
    a.here("vdu_write_delete");
    a.jmp("vdu_delete");

    a.here("vdu_while_disabled");
    a.cmp(imm(0x20));
    a.bcs(RTS);
    a.cmp(imm(6));
    a.beq("vdu_control");
    a.tax();
    a.lda(abs("vdu_parameters").x());
    a.beq(RTS);
    // Its parameters are gathered; only VDU 1 does anything with them.
    a.jsr("vdu_queue_count");
    a.ldy(imm(lo("return")));
    a.lda(imm(hi("return")));
    a.cpx(imm(1));
    a.bne("vdu_disabled_jump");
    a.ldy(imm(lo("vdu_1_disabled")));
    a.lda(imm(hi("vdu_1_disabled")));
    a.here("vdu_disabled_jump");
    a.sty(abs(vdu::JUMP));
    a.sta(abs(vdu::JUMP + 1));
    a.rts();

    a.here("vdu_control");
    a.tax();
    // In VDU 5 the cursor controls move the graphics cursor.
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(status::GRAPHICS_CURSOR));
    a.beq("vdu_control_text");
    a.cpx(imm(30));
    a.bne("vdu_control_graphics");
    a.jmp("vdu5_home");
    a.here("vdu_control_graphics");
    a.cpx(imm(8));
    a.bcc("vdu_control_text");
    a.cpx(imm(14));
    a.bcs("vdu_control_text");
    a.jmp("vdu5_control");
    a.here("vdu_control_text");
    a.lda(abs("vdu_handler_low").x());
    a.sta(abs(vdu::JUMP));
    a.lda(abs("vdu_handler_high").x());
    a.sta(abs(vdu::JUMP + 1));
    a.lda(abs("vdu_parameters").x());
    a.beq("vdu_dispatch");
    a.here("vdu_queue_count");
    a.sta(abs(var::VDU_QUEUE));
    a.rts();

    a.here("vdu_parameter");
    a.sta(abs(QUEUE_LAST - 0xFF).x());
    a.inx();
    a.stx(abs(var::VDU_QUEUE));
    a.bne(RTS);
    a.here("vdu_dispatch");
    a.jmp(abs(vdu::JUMP).ind());
}

fn simple_controls(a: &mut Asm, model: Model) {
    a.here("vdu_1");
    a.lda(abs(QUEUE_LAST));
    a.jmp("printer_send");
    // While the VDU drivers are disabled the printer has VDU 1's character
    // twice.
    a.here("vdu_1_disabled");
    a.jsr("vdu_1");
    a.jmp("vdu_1");

    // VDU 2 and 3 tell UPTV, then turn the printer on or off.
    a.here("vdu_2");
    a.lda(imm(super::serial::upt::VDU_2));
    a.jsr("printer_user");
    a.lda(imm(status::PRINTER));
    a.bne("vdu_set_status");
    a.here("vdu_3");
    a.lda(imm(super::serial::upt::VDU_3));
    a.jsr("printer_user");
    a.lda(imm(!status::PRINTER));
    a.bne("vdu_clear_status");
    a.here("vdu_14");
    a.lda(imm(0));
    a.sta(abs(var::PAGED_LINES));
    a.lda(imm(status::PAGED));
    a.bne("vdu_set_status");
    a.here("vdu_15");
    a.lda(imm(!status::PAGED));
    a.bne("vdu_clear_status");
    a.here("vdu_21");
    a.lda(imm(status::DISABLED));
    a.bne("vdu_set_status");
    a.here("vdu_6");
    a.lda(imm(!status::DISABLED));
    a.bne("vdu_clear_status");
    a.here("vdu_4");
    a.jsr("vdu_cursor_show");
    a.lda(imm(!status::GRAPHICS_CURSOR));
    a.here("vdu_clear_status");
    a.and(zp(vdu_zp::STATUS));
    a.sta(zp(vdu_zp::STATUS));
    a.rts();
    a.here("vdu_5");
    return_in_teletext(a, model);
    a.lda(abs(vdu::MODE));
    a.cmp(imm(3));
    a.beq(RTS);
    a.cmp(imm(6));
    a.beq(RTS);
    a.jsr("vdu_cursor_hide");
    a.lda(imm(status::GRAPHICS_CURSOR));
    a.here("vdu_set_status");
    a.ora(zp(vdu_zp::STATUS));
    a.sta(zp(vdu_zp::STATUS));
    a.rts();
}

/// Printing a character: its glyph drawn in the text colours at the cursor,
/// or in mode 7 its code stored, then the cursor moved on.
fn printing(a: &mut Asm, model: Model) {
    a.here("vdu_print");
    a.tay();
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(status::GRAPHICS_CURSOR));
    a.beq("vdu_print_text");
    a.tya();
    a.jmp("vdu5_print");
    a.here("vdu_print_text");
    a.tya();
    if has_teletext(model) {
        a.ldx(abs(vdu::COLOURS));
        a.bne("vdu_print_bitmap");
        a.jsr("teletext_code");
        a.ldy(imm(0));
        a.sta(zp(vdu_zp::CURSOR_LINE).ind_y());
        a.jmp("vdu_9");
        a.here("vdu_print_bitmap");
    }
    a.jsr("glyph_address");
    a.jsr("vdu_draw_glyph");
    a.jmp("vdu_9");
    if has_teletext(model) {
        teletext_code(a);
    }
    glyphs(a);
    vdu_draw_glyph(a);
    vdu_delete(a, model);
}

/// Teletext's codes for £, # and _ are #, _ and £'s places in ASCII.
fn teletext_code(a: &mut Asm) {
    a.here("teletext_code");
    a.cmp(imm(b'#'));
    a.bne("teletext_not_hash");
    a.lda(imm(0x5F));
    a.rts();
    a.here("teletext_not_hash");
    a.cmp(imm(b'_'));
    a.bne("teletext_not_underscore");
    a.lda(imm(0x60));
    a.rts();
    a.here("teletext_not_underscore");
    a.cmp(imm(0x60));
    a.bne(RTS);
    a.lda(imm(b'#'));
    a.rts();
}

/// (&DE) = the definition of character A: a zone in RAM where &367 says so,
/// otherwise the ROM font, which 160–255 repeat.
fn glyphs(a: &mut Asm) {
    a.here("glyph_address");
    a.pha();
    for _ in 0..3 {
        a.asl(Acc);
    }
    a.sta(zp(vdu_zp::TEMP + 4));
    a.pla();
    a.pha();
    for _ in 0..5 {
        a.lsr(Acc);
    }
    a.tax();
    a.lda(abs("zone_bit").x());
    a.and(abs(vdu::FONT_FLAGS));
    a.beq("glyph_in_rom");
    a.lda(abs(vdu::FONT_ZONES - 1).x());
    a.bne("glyph_page");
    a.here("glyph_in_rom");
    a.jsr("font_rom_page");
    a.here("glyph_page");
    a.sta(zp(vdu_zp::TEMP + 5));
    a.pla();
    a.rts();
    a.here("zone_bit");
    a.bytes(&[0x00, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01]);
}

/// Draw the glyph at (&DE) into the cell at (&D8) in the text colours. A
/// cell is aligned to its size, so none crosses a page. In two colours a
/// row is a byte; in four each row's nibbles fill the cell's two columns;
/// in sixteen each column takes the top two bits of every row of a copy at
/// &330, shifted on as it goes.
fn vdu_draw_glyph(a: &mut Asm) {
    const GLYPH: u16 = vdu_zp::TEMP + 4;
    a.here("vdu_draw_glyph");
    a.ldy(imm(7));
    a.lda(abs(vdu::COLOURS));
    a.cmp(imm(3));
    a.beq("draw_4");
    a.bcs("draw_16");
    a.here("draw_2");
    a.lda(zp(GLYPH).ind_y());
    a.and(zp(vdu_zp::TEXT_OR));
    a.eor(zp(vdu_zp::TEXT_EOR));
    a.sta(zp(vdu_zp::CURSOR_LINE).ind_y());
    a.dey();
    a.bpl("draw_2");
    a.rts();

    a.here("draw_4");
    a.lda(zp(vdu_zp::CURSOR_LINE));
    a.ora(imm(8));
    a.sta(zp(vdu_zp::TEMP));
    a.lda(zp(vdu_zp::CURSOR_LINE + 1));
    a.sta(zp(vdu_zp::TEMP + 1));
    a.here("draw_4_row");
    a.lda(zp(GLYPH).ind_y());
    a.pha();
    for _ in 0..4 {
        a.lsr(Acc);
    }
    a.tax();
    a.lda(abs("expand_4").x());
    a.and(zp(vdu_zp::TEXT_OR));
    a.eor(zp(vdu_zp::TEXT_EOR));
    a.sta(zp(vdu_zp::CURSOR_LINE).ind_y());
    a.pla();
    a.and(imm(0x0F));
    a.tax();
    a.lda(abs("expand_4").x());
    a.and(zp(vdu_zp::TEXT_OR));
    a.eor(zp(vdu_zp::TEXT_EOR));
    a.sta(zp(vdu_zp::TEMP).ind_y());
    a.dey();
    a.bpl("draw_4_row");
    a.rts();

    a.here("draw_16");
    a.lda(zp(GLYPH).ind_y());
    a.sta(abs(vdu::SCRATCH).y());
    a.dey();
    a.bpl("draw_16");
    a.lda(zp(vdu_zp::CURSOR_LINE));
    a.sta(zp(vdu_zp::TEMP));
    a.lda(zp(vdu_zp::CURSOR_LINE + 1));
    a.sta(zp(vdu_zp::TEMP + 1));
    a.here("draw_16_column");
    a.ldy(imm(7));
    a.here("draw_16_row");
    a.lda(abs(vdu::SCRATCH).y());
    // The row's top two bits, whatever the carry rotates in below them.
    for _ in 0..3 {
        a.rol(Acc);
    }
    a.and(imm(3));
    a.tax();
    a.lda(abs(vdu::SCRATCH).y());
    a.asl(Acc);
    a.asl(Acc);
    a.sta(abs(vdu::SCRATCH).y());
    a.lda(abs("expand_2").x());
    a.and(zp(vdu_zp::TEXT_OR));
    a.eor(zp(vdu_zp::TEXT_EOR));
    a.sta(zp(vdu_zp::TEMP).ind_y());
    a.dey();
    a.bpl("draw_16_row");
    // Four columns of eight bytes each, the cell aligned to its 32.
    a.lda(zp(vdu_zp::TEMP));
    a.clc();
    a.adc(imm(8));
    a.sta(zp(vdu_zp::TEMP));
    a.and(imm(0x18));
    a.bne("draw_16_column");
    a.rts();
}

/// DEL: back a character, then a space over it, or in VDU 5 a cell in the
/// background colour.
fn vdu_delete(a: &mut Asm, model: Model) {
    a.here("vdu_delete");
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(status::GRAPHICS_CURSOR));
    a.beq("vdu_delete_text");
    a.jmp("vdu5_delete");
    a.here("vdu_delete_text");
    a.jsr("vdu_8");
    a.lda(imm(b' '));
    if has_teletext(model) {
        a.ldx(abs(vdu::COLOURS));
        a.bne("vdu_delete_bitmap");
        a.ldy(imm(0));
        a.sta(zp(vdu_zp::CURSOR_LINE).ind_y());
        a.rts();
        a.here("vdu_delete_bitmap");
    }
    a.jsr("glyph_address");
    a.jmp("vdu_draw_glyph");
}

/// The cursor: moving it, and following it with its address and the
/// 6845's cursor.
fn cursor(a: &mut Asm) {
    // Back one: off the left edge to the previous line's end.
    a.here("vdu_8");
    a.ldx(abs(vdu::CURSOR_X));
    a.cpx(abs(vdu::TEXT_LEFT));
    a.beq("vdu_8_wrap");
    a.bcc("vdu_8_wrap");
    a.dec(abs(vdu::CURSOR_X));
    a.jmp("vdu_cursor_update");
    a.here("vdu_8_wrap");
    a.lda(abs(vdu::TEXT_RIGHT));
    a.sta(abs(vdu::CURSOR_X));
    a.jmp("vdu_11");

    // Forward one, a cell on; off the right edge to the next line's start.
    a.here("vdu_9");
    a.ldx(abs(vdu::CURSOR_X));
    a.cpx(abs(vdu::TEXT_RIGHT));
    a.bcs("vdu_9_wrap");
    a.inc(abs(vdu::CURSOR_X));
    a.lda(abs(vdu::BYTES_PER_CHARACTER));
    a.ldx(imm(0));
    a.beq("cursor_forward");
    a.here("vdu_9_wrap");
    a.lda(abs(vdu::TEXT_LEFT));
    a.sta(abs(vdu::CURSOR_X));
    a.jsr("vdu_cursor_update");

    // Down one, a row on; off the bottom the window scrolls.
    a.here("vdu_10");
    a.jsr("vdu_paging");
    a.ldx(abs(vdu::CURSOR_Y));
    a.cpx(abs(vdu::TEXT_BOTTOM));
    a.bcs("vdu_10_scroll");
    a.inc(abs(vdu::CURSOR_Y));
    a.lda(abs(vdu::BYTES_PER_ROW));
    a.ldx(abs(vdu::BYTES_PER_ROW + 1));
    a.here("cursor_forward");
    a.pha();
    a.lda(zp(vdu_zp::CURSOR_LINE));
    a.sta(zp(vdu_zp::TEMP));
    a.lda(zp(vdu_zp::CURSOR_LINE + 1));
    a.sta(zp(vdu_zp::TEMP + 1));
    a.pla();
    a.jsr("pointer_add");
    a.jmp("cursor_set");
    a.here("vdu_10_scroll");
    a.jsr("vdu_scroll_up");
    a.jmp("vdu_cursor_update");

    a.here("vdu_11");
    a.ldx(abs(vdu::CURSOR_Y));
    a.cpx(abs(vdu::TEXT_TOP));
    a.beq("vdu_11_scroll");
    a.bcc("vdu_11_scroll");
    a.dec(abs(vdu::CURSOR_Y));
    a.jmp("vdu_cursor_update");
    a.here("vdu_11_scroll");
    a.jsr("vdu_scroll_down");
    a.jmp("vdu_cursor_update");

    a.here("vdu_13");
    a.lda(abs(vdu::TEXT_LEFT));
    a.sta(abs(vdu::CURSOR_X));
    a.jmp("vdu_cursor_update");

    a.here("vdu_30");
    a.lda(abs(vdu::TEXT_LEFT));
    a.sta(abs(vdu::CURSOR_X));
    a.lda(abs(vdu::TEXT_TOP));
    a.sta(abs(vdu::CURSOR_Y));
    a.jmp("vdu_cursor_update");

    // TAB(x, y) within the window, ignored outside it.
    a.here("vdu_31");
    a.lda(abs(QUEUE_LAST - 1));
    a.clc();
    a.adc(abs(vdu::TEXT_LEFT));
    a.bcs(RTS);
    a.cmp(abs(vdu::TEXT_RIGHT));
    a.beq("vdu_31_x");
    a.bcs(RTS);
    a.here("vdu_31_x");
    a.tax();
    a.lda(abs(QUEUE_LAST));
    a.clc();
    a.adc(abs(vdu::TEXT_TOP));
    a.bcs(RTS);
    a.cmp(abs(vdu::TEXT_BOTTOM));
    a.beq("vdu_31_y");
    a.bcs(RTS);
    a.here("vdu_31_y");
    a.stx(abs(vdu::CURSOR_X));
    a.sta(abs(vdu::CURSOR_Y));
}

/// (&DA) += the row table's entry Y bytes in, its high byte first.
fn row_add(a: &mut Asm) {
    a.here("row_add");
    a.iny();
    a.lda(zp(vdu_zp::TEMP));
    a.clc();
    a.adc(zp(vdu_zp::ROW_TABLE).ind_y());
    a.sta(zp(vdu_zp::TEMP));
    a.dey();
    a.lda(zp(vdu_zp::TEMP + 1));
    a.adc(zp(vdu_zp::ROW_TABLE).ind_y());
    a.sta(zp(vdu_zp::TEMP + 1));
    a.rts();
}

/// &D8 and &34A follow the cursor, and so does the Model B's 6845's; the
/// Electron's cursor is drawn there once the drivers are done.
fn vdu_cursor_update(a: &mut Asm, model: Model) {
    a.here("vdu_cursor_update");
    a.ldx(abs(vdu::CURSOR_X));
    a.ldy(abs(vdu::CURSOR_Y));
    a.jsr("cell_address");
    a.here("cursor_set");
    a.lda(zp(vdu_zp::TEMP));
    a.sta(zp(vdu_zp::CURSOR_LINE));
    a.sta(abs(vdu::CURSOR_ADDRESS));
    a.lda(zp(vdu_zp::TEMP + 1));
    a.sta(zp(vdu_zp::CURSOR_LINE + 1));
    a.sta(abs(vdu::CURSOR_ADDRESS + 1));
    match model {
        Model::ModelB => {
            a.lda(imm(14));
            a.bne("crtc_address");
        }
        Model::Electron => a.rts(),
    }

    // (&DA) = the address of row Y's window, &DE its width in bytes.
    a.here("row_window");
    a.ldx(abs(vdu::TEXT_LEFT));
    a.jsr("cell_address");
    a.lda(abs(vdu::WINDOW_WIDTH));
    a.sta(zp(vdu_zp::TEMP + 4));
    a.lda(abs(vdu::WINDOW_WIDTH + 1));
    a.sta(zp(vdu_zp::TEMP + 5));
    a.rts();

    // (&DA) = the address of the cell at column X, row Y.
    a.here("cell_address");
    a.lda(imm(0));
    a.sta(zp(vdu_zp::TEMP + 1));
    a.stx(zp(vdu_zp::TEMP));
    a.lda(abs(vdu::BYTES_PER_CHARACTER));
    a.here("cell_shift");
    a.lsr(Acc);
    a.beq("cell_shifted");
    a.asl(zp(vdu_zp::TEMP));
    a.rol(zp(vdu_zp::TEMP + 1));
    // No column's offset reaches &8000, so the carry is clear.
    a.bcc("cell_shift");
    a.here("cell_shifted");
    a.tya();
    a.asl(Acc);
    a.tay();
    a.jsr("row_add");
    a.lda(abs(vdu::SCREEN_START));
    a.ldx(abs(vdu::SCREEN_START + 1));
    a.fall_into("pointer_add");

    // (&DA) += A + 256 × X, wrapped back into the screen past &8000.
    a.here("pointer_add");
    a.clc();
    a.adc(zp(vdu_zp::TEMP));
    a.sta(zp(vdu_zp::TEMP));
    a.txa();
    a.adc(zp(vdu_zp::TEMP + 1));
    a.bpl("pointer_add_in");
    a.sec();
    a.sbc(abs(vdu::SCREEN_SIZE));
    a.here("pointer_add_in");
    a.sta(zp(vdu_zp::TEMP + 1));
    a.rts();

    row_add(a);

    if model == Model::Electron {
        return;
    }
    // Point 6845 register pair A, A+1 at the screen address (&DA): an
    // address over 8 in the bitmap modes, less the bias in mode 7.
    a.here("crtc_address");
    a.pha();
    a.lda(abs(vdu::COLOURS));
    a.bne("crtc_address_bitmap");
    a.lda(zp(vdu_zp::TEMP));
    a.sec();
    a.sbc(imm(TELETEXT_BIAS.to_le_bytes()[0]));
    a.tax();
    a.lda(zp(vdu_zp::TEMP + 1));
    a.sbc(imm(TELETEXT_BIAS.to_le_bytes()[1]));
    a.jmp("crtc_address_write");
    a.here("crtc_address_bitmap");
    a.lda(zp(vdu_zp::TEMP + 1));
    a.sta(zp(vdu_zp::TEMP + 3));
    a.lda(zp(vdu_zp::TEMP));
    for _ in 0..3 {
        a.lsr(zp(vdu_zp::TEMP + 3));
        a.ror(Acc);
    }
    a.tax();
    a.lda(zp(vdu_zp::TEMP + 3));
    a.here("crtc_address_write");
    a.tay();
    a.pla();
    a.sta(abs(CRTC_ADDRESS));
    a.sty(abs(CRTC_DATA));
    a.clc();
    a.adc(imm(1));
    a.sta(abs(CRTC_ADDRESS));
    a.stx(abs(CRTC_DATA));
    a.rts();
}

/// 6845 register X set to A as MODE and VDU 23,0 set them: R7 moved by
/// *TV's shift and one more, as MOS 1.20 is seen to, and R10 kept as the
/// cursor's shape. `crtc_register` writes A as it is.
fn crtc_write(a: &mut Asm) {
    a.here("crtc_write");
    a.cpx(imm(7));
    a.bne("crtc_not_sync");
    // The compare leaves C set: the one more.
    a.adc(abs(super::ws::page2::TV_SHIFT));
    a.here("crtc_not_sync");
    a.cpx(imm(10));
    a.bne("crtc_register");
    a.sta(abs(vdu::CURSOR_START));
    a.here("crtc_register");
    a.stx(abs(CRTC_ADDRESS));
    a.sta(abs(CRTC_DATA));
    a.rts();
}

/// The cursor's shape: off, or the mode's own. The Electron's is turned
/// off or on, drawn again as the drivers finish.
fn vdu_cursor_hide(a: &mut Asm, model: Model) {
    a.here("vdu_cursor_hide");
    match model {
        Model::ModelB => {
            a.lda(imm(0x20));
            a.bne("vdu_cursor_set");
            a.here("vdu_cursor_show");
            a.lda(abs(vdu::CURSOR_START));
            a.here("vdu_cursor_set");
            a.ldx(imm(10));
            a.jmp("crtc_register");
        }
        Model::Electron => {
            a.lda(abs(vdu::CURSOR_STATE));
            a.ora(imm(cursor_state::OFF));
            a.bne("vdu_cursor_set");
            a.here("vdu_cursor_show");
            a.lda(abs(vdu::CURSOR_STATE));
            a.and(imm(!cursor_state::OFF));
            a.here("vdu_cursor_set");
            a.sta(abs(vdu::CURSOR_STATE));
            a.rts();
        }
    }

    // Paged mode stops after a window's worth of lines until SHIFT is
    // pressed, and CTRL with SHIFT holds output while both are down.
    a.here("vdu_paging");
    a.jsr("keyboard_modifiers");
    a.bpl("vdu_paging_ctrl_up");
    a.bvc("vdu_paging_ctrl_up");
    a.here("vdu_paging_hold");
    a.jsr("keyboard_modifiers");
    a.bpl("vdu_paging_ctrl_up");
    a.bvs("vdu_paging_hold");
    a.here("vdu_paging_ctrl_up");
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(status::PAGED));
    a.beq(RTS);
    a.lda(abs(vdu::TEXT_BOTTOM));
    a.sec();
    a.sbc(abs(vdu::TEXT_TOP));
    a.cmp(abs(var::PAGED_LINES));
    a.beq("vdu_paging_wait");
    a.bcc("vdu_paging_wait");
    a.inc(abs(var::PAGED_LINES));
    a.rts();
    a.here("vdu_paging_wait");
    a.bit(zp(super::ws::os::ESCAPE));
    a.bmi("vdu_paging_resume");
    a.jsr("keyboard_modifiers");
    a.bvc("vdu_paging_wait");
    a.here("vdu_paging_resume");
    a.lda(imm(0));
    a.sta(abs(var::PAGED_LINES));
    a.rts();
}

/// Scrolling the window a line: the whole screen by moving where the 6845
/// or the Electron's ULA starts, a window by copying its rows.
fn scrolling(a: &mut Asm, model: Model) {
    a.here("vdu_scroll_up");
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(status::WINDOWED));
    a.bne("scroll_window_up");
    a.lda(abs(vdu::SCREEN_START));
    a.clc();
    a.adc(abs(vdu::BYTES_PER_ROW));
    a.sta(abs(vdu::SCREEN_START));
    a.lda(abs(vdu::SCREEN_START + 1));
    a.adc(abs(vdu::BYTES_PER_ROW + 1));
    a.jsr("screen_start_set");
    a.ldy(abs(vdu::TEXT_BOTTOM));
    a.jmp("clear_row");

    a.here("vdu_scroll_down");
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(status::WINDOWED));
    a.bne("scroll_window_down");
    a.lda(abs(vdu::SCREEN_START));
    a.sec();
    a.sbc(abs(vdu::BYTES_PER_ROW));
    a.sta(abs(vdu::SCREEN_START));
    a.lda(abs(vdu::SCREEN_START + 1));
    a.sbc(abs(vdu::BYTES_PER_ROW + 1));
    a.cmp(abs(vdu::SCREEN_BOTTOM));
    a.bcs("scroll_down_start");
    a.adc(abs(vdu::SCREEN_SIZE));
    a.here("scroll_down_start");
    a.jsr("screen_start_set");
    a.ldy(abs(vdu::TEXT_TOP));
    a.jmp("clear_row");

    screen_start(a, model);

    // A window scrolls by copying each row's window bytes from the row
    // below, or above, then clearing the row that comes in.
    a.here("scroll_window_up");
    a.ldy(abs(vdu::TEXT_TOP));
    a.here("scroll_window_up_row");
    a.cpy(abs(vdu::TEXT_BOTTOM));
    a.bcs("clear_row");
    a.tya();
    a.pha();
    a.iny();
    a.jsr("copy_row_from");
    a.pla();
    a.tay();
    a.iny();
    a.bne("scroll_window_up_row");

    a.here("scroll_window_down");
    a.ldy(abs(vdu::TEXT_BOTTOM));
    a.here("scroll_window_down_row");
    a.cpy(abs(vdu::TEXT_TOP));
    a.beq("clear_row");
    a.bcc("clear_row");
    a.tya();
    a.pha();
    a.dey();
    a.jsr("copy_row_from");
    a.pla();
    a.tay();
    a.dey();
    // Y is still a row below the top.
    a.bpl("scroll_window_down_row");
    copy_row_from(a);

    // Fill row Y's window bytes with the text background.
    a.here("clear_row");
    a.jsr("row_window");
    a.jsr("background_byte");
    a.jmp("run_fill");

    // The byte a cleared cell holds: a space in mode 7.
    a.here("background_byte");
    if has_teletext(model) {
        a.lda(abs(vdu::COLOURS));
        a.beq("background_teletext");
    }
    a.lda(abs(vdu::TEXT_BACKGROUND));
    a.rts();
    if has_teletext(model) {
        a.here("background_teletext");
        a.lda(imm(b' '));
        a.rts();
    }
}

/// Copy row Y's window bytes into the row pushed on the stack before the
/// caller changed Y.
fn copy_row_from(a: &mut Asm) {
    a.here("copy_row_from");
    a.ldx(abs(vdu::TEXT_LEFT));
    a.jsr("cell_address");
    a.lda(zp(vdu_zp::TEMP));
    a.sta(zp(vdu_zp::TEMP + 2));
    a.lda(zp(vdu_zp::TEMP + 1));
    a.sta(zp(vdu_zp::TEMP + 3));
    a.tsx();
    a.ldy(abs(0x0103).x());
    a.jsr("row_window");
    a.jmp("run_copy");
}

/// The screen's start: `screen_start_reset` its base, as CLS and MODE leave
/// it, and `screen_start_set` &351 = A, wrapped into the screen's memory;
/// the 6845 or the ULA told.
fn screen_start(a: &mut Asm, model: Model) {
    a.here("screen_start_reset");
    a.lda(imm(0));
    a.sta(abs(vdu::SCREEN_START));
    a.lda(abs(vdu::SCREEN_BOTTOM));
    a.here("screen_start_set");
    a.bpl("screen_start_in");
    a.sec();
    a.sbc(abs(vdu::SCREEN_SIZE));
    a.here("screen_start_in");
    a.sta(abs(vdu::SCREEN_START + 1));
    a.sta(zp(vdu_zp::TEMP + 1));
    a.lda(abs(vdu::SCREEN_START));
    a.sta(zp(vdu_zp::TEMP));
    match model {
        Model::ModelB => {
            a.lda(imm(12));
            a.jmp("crtc_address");
        }
        Model::Electron => {
            // A9–A14 into `&FE03`, A6–A8 into `&FE02`'s top three bits.
            a.lda(zp(vdu_zp::TEMP + 1));
            a.lsr(Acc);
            a.sta(abs(ula::START_HIGH));
            a.lda(zp(vdu_zp::TEMP));
            a.ror(Acc);
            a.sta(abs(ula::START_LOW));
            a.rts();
        }
    }
}

/// A run of &DE/&DF screen bytes from (&DA): filled with A, or copied from
/// (&DC). Each pointer wraps past the screen's end, so the run goes a
/// segment at a time, none crossing a 128-byte boundary of either pointer;
/// &DE/&DF counts what is left less one.
fn runs(a: &mut Asm) {
    const FILL: u16 = vdu::SCRATCH;
    const FILLING: u16 = vdu::SCRATCH + 1;
    const LAST: u16 = vdu::SCRATCH + 2;
    const DESTINATION: u16 = vdu_zp::TEMP;
    const SOURCE: u16 = DESTINATION + 2;
    const LEFT: u16 = vdu_zp::TEMP + 4;

    a.here("run_fill");
    a.sta(abs(FILL));
    // The source follows the destination, so the segments stay its own.
    a.ldx(imm(1));
    let follow = a.here_local();
    a.lda(zp(DESTINATION).x());
    a.sta(zp(SOURCE).x());
    a.dex();
    a.bpl(follow);
    a.sec();
    a.bcs("run_start");
    a.here("run_copy");
    a.clc();
    a.here("run_start");
    a.ror(abs(FILLING));
    a.lda(zp(LEFT));
    a.bne("run_count");
    a.dec(zp(LEFT + 1));
    a.here("run_count");
    a.dec(zp(LEFT));

    a.here("run_segment");
    // Y = the segment's last offset: up to the boundary the further-on
    // pointer reaches first, or to the run's end.
    a.lda(zp(DESTINATION));
    a.cmp(zp(SOURCE));
    let further = a.local();
    a.bcs(further);
    a.lda(zp(SOURCE));
    a.bind(further);
    a.eor(imm(0xFF));
    a.and(imm(0x7F));
    let sized = a.local();
    a.ldx(zp(LEFT + 1));
    a.bne(sized);
    a.cmp(zp(LEFT));
    a.bcc(sized);
    a.lda(zp(LEFT));
    a.bind(sized);
    a.tay();
    a.sta(abs(LAST));
    // What is left less one, less the segment: the borrow out of it ends
    // the run.
    a.eor(imm(0xFF));
    a.clc();
    a.adc(zp(LEFT));
    a.sta(zp(LEFT));
    let kept = a.local();
    a.bcs(kept);
    a.dec(zp(LEFT + 1));
    a.bind(kept);
    a.bit(abs(FILLING));
    a.bmi("run_fill_bytes");
    a.here("run_copy_bytes");
    a.lda(zp(SOURCE).ind_y());
    a.sta(zp(DESTINATION).ind_y());
    a.dey();
    a.bpl("run_copy_bytes");
    a.bmi("run_advance");
    a.here("run_fill_bytes");
    a.lda(abs(FILL));
    let fill = a.here_local();
    a.sta(zp(DESTINATION).ind_y());
    a.dey();
    a.bpl(fill);

    // Both pointers past the segment, each wrapped: X 2, the source, then 0.
    a.here("run_advance");
    a.ldx(imm(2));
    let advance = a.here_local();
    a.lda(abs(LAST));
    a.sec();
    a.adc(zp(DESTINATION).x());
    a.sta(zp(DESTINATION).x());
    let next = a.local();
    a.bcc(next);
    a.inc(zp(DESTINATION + 1).x());
    a.bpl(next);
    a.lda(zp(DESTINATION + 1).x());
    // The carry is the add's.
    a.sbc(abs(vdu::SCREEN_SIZE));
    a.sta(zp(DESTINATION + 1).x());
    a.bind(next);
    a.dex();
    a.dex();
    a.bpl(advance);
    a.bit(zp(LEFT + 1));
    a.bpl("run_segment");
    a.rts();
}

/// Clearing the window or the whole screen.
fn clearing(a: &mut Asm) {
    // CLS: a window row by row; the whole screen with the 6845 back at its
    // start, every byte cleared.
    a.here("vdu_12");
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(status::WINDOWED));
    a.beq("cls_screen");
    a.ldy(abs(vdu::TEXT_TOP));
    a.here("cls_window_row");
    a.tya();
    a.pha();
    a.jsr("clear_row");
    a.pla();
    a.tay();
    a.cpy(abs(vdu::TEXT_BOTTOM));
    a.iny();
    a.bcc("cls_window_row");
    a.bcs("cls_home");
    a.here("cls_screen");
    a.jsr("screen_start_reset");
    // MODE's clear: the screen's start is already set.
    a.here("screen_clear");
    a.lda(imm(0));
    a.sta(zp(vdu_zp::TEMP));
    a.lda(abs(vdu::SCREEN_BOTTOM));
    a.sta(zp(vdu_zp::TEMP + 1));
    a.jsr("background_byte");
    a.ldy(imm(0));
    a.here("cls_screen_byte");
    a.sta(zp(vdu_zp::TEMP).ind_y());
    a.iny();
    a.bne("cls_screen_byte");
    a.inc(zp(vdu_zp::TEMP + 1));
    a.bpl("cls_screen_byte");
    a.here("cls_home");
    a.lda(imm(0));
    a.sta(abs(var::PAGED_LINES));
    a.jmp("vdu_30");
}

/// Colours: COLOUR, GCOL, the palette, and the defaults VDU 20 restores.
fn colours(a: &mut Asm, model: Model) {
    // A logical colour's byte: every pixel of a byte that colour.
    a.here("colour_byte");
    a.and(abs(vdu::COLOURS));
    a.ldx(abs(vdu::COLOURS));
    a.cpx(imm(3));
    a.beq("colour_byte_4");
    a.bcs("colour_byte_16");
    a.tax();
    a.beq(RTS);
    a.lda(imm(0xFF));
    a.rts();
    a.here("colour_byte_4");
    a.tax();
    a.lda(abs("expand_4_colours").x());
    a.rts();
    a.here("colour_byte_16");
    a.tax();
    a.lda(abs("expand_16_colours").x());
    a.rts();

    a.here("vdu_17");
    return_in_teletext(a, model);
    a.lda(abs(QUEUE_LAST));
    a.bmi("vdu_17_background");
    a.jsr("colour_byte");
    a.sta(abs(vdu::TEXT_FOREGROUND));
    a.jmp("text_masks");
    a.here("vdu_17_background");
    a.jsr("colour_byte");
    a.sta(abs(vdu::TEXT_BACKGROUND));

    // The masks a glyph's pixels are drawn through.
    a.here("text_masks");
    a.lda(abs(vdu::TEXT_FOREGROUND));
    a.eor(abs(vdu::TEXT_BACKGROUND));
    a.sta(zp(vdu_zp::TEXT_OR));
    a.lda(abs(vdu::TEXT_BACKGROUND));
    a.sta(zp(vdu_zp::TEXT_EOR));
    a.rts();

    a.here("vdu_18");
    return_in_teletext(a, model);
    a.lda(abs(QUEUE_LAST));
    a.bmi("vdu_18_background");
    a.jsr("colour_byte");
    a.sta(abs(vdu::GRAPHICS_FOREGROUND));
    a.lda(abs(QUEUE_LAST - 1));
    a.sta(abs(vdu::FOREGROUND_ACTION));
    a.rts();
    a.here("vdu_18_background");
    a.jsr("colour_byte");
    a.sta(abs(vdu::GRAPHICS_BACKGROUND));
    a.lda(abs(QUEUE_LAST - 1));
    a.sta(abs(vdu::BACKGROUND_ACTION));
    a.rts();

    // VDU 19, logical, physical: the palette entry, then the ULA's entries
    // for it, as MOS 1.20 leaves the others; the Electron's registers each
    // hold several colours, so it writes them all.
    a.here("vdu_19");
    a.lda(abs(QUEUE_LAST - 4));
    a.and(abs(vdu::COLOURS));
    a.tax();
    a.lda(abs(QUEUE_LAST - 3));
    a.and(imm(0x0F));
    a.sta(abs(vdu::PALETTE).x());
    match model {
        Model::ModelB => {
            a.txa();
            a.fall_into("ula_palette_colour");
        }
        Model::Electron => a.fall_into("ula_palette"),
    }
}

/// Write the ULA palette entries of logical colour A, lowest first, from
/// the logical palette: each entry is the colour of the pixel value its
/// index forms. `ula_palette` writes every colour's, the highest colour
/// first, in MOS 1.20's order; &249 keeps the last written, as OSBYTE &B9
/// reads it.
fn ula_palette(a: &mut Asm) {
    let selected = zp(vdu_zp::TEMP + 1);
    a.here("ula_palette_colour");
    a.sta(selected);
    a.ldy(imm(0));
    a.here("ula_palette_entry");
    a.tya();
    a.ldx(abs(vdu::COLOURS));
    a.cpx(imm(1));
    a.bne("ula_palette_not_2");
    a.lsr(Acc);
    a.lsr(Acc);
    a.lsr(Acc);
    a.jmp("ula_palette_logical");
    a.here("ula_palette_not_2");
    a.cpx(imm(3));
    a.bne("ula_palette_logical");
    a.lda(abs("palette_index_4").y());
    a.here("ula_palette_logical");
    a.tax();
    a.cpx(selected);
    a.bne("ula_palette_next");
    a.lda(abs(vdu::PALETTE).x());
    a.eor(imm(0x07));
    a.sta(zp(vdu_zp::TEMP));
    a.tya();
    for _ in 0..4 {
        a.asl(Acc);
    }
    a.ora(zp(vdu_zp::TEMP));
    a.sta(abs(sheila::VIDEO_PALETTE));
    a.sta(abs(var::ULA_PALETTE));
    a.here("ula_palette_next");
    a.iny();
    a.cpy(imm(16));
    a.bne("ula_palette_entry");
    a.rts();
    a.here("ula_palette");
    a.lda(abs(vdu::COLOURS));
    a.here("ula_palette_all");
    a.pha();
    a.jsr("ula_palette_colour");
    a.pla();
    a.sec();
    a.sbc(imm(1));
    a.bpl("ula_palette_all");
    a.rts();
}

/// The Electron's palette: its eight registers, which cannot be read, each
/// written whole from the logical palette, the flashing colours' state
/// applied, with interrupts off, since the VSYNC's flash writes them too.
/// Each entry is the colour of the pixel value its index forms, the second
/// of a flashing pair while the flash bit is set.
fn electron_palette(a: &mut Asm) {
    let (even, odd) = (zp(os::SCRATCH), zp(os::SCRATCH + 1));
    a.here("ula_palette");
    a.here("palette_write");
    a.php();
    a.sei();
    a.ldy(imm(15));
    a.here("palette_pair");
    a.lda(imm(0));
    a.sta(even);
    a.sta(odd);
    a.here("palette_entry");
    a.lda(abs("palette_entries").y());
    a.jsr("entry_colour");
    a.ora(abs("palette_places").y());
    a.tax();
    a.lda(even);
    a.ora(abs("palette_even").x());
    a.sta(even);
    a.lda(odd);
    a.ora(abs("palette_odd").x());
    a.sta(odd);
    a.tya();
    a.and(imm(3));
    a.bne("palette_next");
    a.tya();
    a.lsr(Acc);
    a.tax();
    a.lda(even);
    a.sta(abs(ula::PALETTE).x());
    a.lda(odd);
    a.sta(abs(ula::PALETTE + 1).x());
    a.dey();
    a.bpl("palette_pair");
    a.plp();
    a.rts();
    a.here("palette_next");
    a.dey();
    a.bpl("palette_entry");

    // A = the colour palette entry A shows; Y kept.
    a.here("entry_colour");
    a.ldx(abs(vdu::COLOURS));
    a.cpx(imm(1));
    a.bne("entry_colour_not_2");
    a.lsr(Acc);
    a.lsr(Acc);
    a.lsr(Acc);
    a.jmp("entry_colour_logical");
    a.here("entry_colour_not_2");
    a.cpx(imm(3));
    a.bne("entry_colour_logical");
    a.tax();
    a.lda(abs("palette_index_4").x());
    a.here("entry_colour_logical");
    a.tax();
    a.lda(abs(vdu::PALETTE).x());
    a.cmp(imm(8));
    a.and(imm(7));
    a.bcc(RTS);
    a.pha();
    a.lda(abs(var::ULA_CONTROL));
    a.lsr(Acc);
    a.pla();
    a.bcc(RTS);
    a.eor(imm(7));
    a.rts();
}

/// VDU 20 and the mode's palette.
fn vdu_20(a: &mut Asm, model: Model) {
    // VDU 20: white on black text and graphics, plotting outright, and the
    // mode's own palette.
    a.here("vdu_20");
    a.ldx(abs(vdu::COLOURS));
    if has_teletext(model) {
        a.beq(RTS);
    }
    a.ldy(abs(vdu::COLOURS));
    a.here("vdu_20_palette");
    a.lda(abs("default_palette_2").y());
    a.cpx(imm(1));
    a.beq("vdu_20_entry");
    a.lda(abs("default_palette_4").y());
    a.cpx(imm(3));
    a.beq("vdu_20_entry");
    a.tya();
    a.here("vdu_20_entry");
    a.sta(abs(vdu::PALETTE).y());
    a.dey();
    a.bpl("vdu_20_palette");
    // White: colour 7, or the mode's highest where it has fewer.
    a.lda(imm(7));
    a.jsr("colour_byte");
    a.sta(abs(vdu::TEXT_FOREGROUND));
    a.sta(abs(vdu::GRAPHICS_FOREGROUND));
    a.lda(imm(0));
    a.sta(abs(vdu::TEXT_BACKGROUND));
    a.sta(abs(vdu::GRAPHICS_BACKGROUND));
    a.sta(abs(vdu::FOREGROUND_ACTION));
    a.sta(abs(vdu::BACKGROUND_ACTION));
    a.jsr("text_masks");
    a.jmp("ula_palette");
}

/// The text and graphics windows.
fn windows(a: &mut Asm) {
    // VDU 26: the windows, the origin and both cursors back to their defaults.
    a.here("vdu_26");
    a.lda(imm(0));
    a.ldx(imm(3));
    let next = a.here_local();
    for point in [
        vdu::ORIGIN,
        vdu::GRAPHICS_CURSOR,
        vdu::OLD_GRAPHICS_CURSOR,
        vdu::GRAPHICS_INTERNAL,
    ] {
        a.sta(abs(point).x());
    }
    a.dex();
    a.bpl(next);
    a.ldx(abs(vdu::MODE));
    a.sta(abs(vdu::TEXT_LEFT));
    a.sta(abs(vdu::TEXT_TOP));
    a.lda(abs("mode_right").x());
    a.sta(abs(vdu::TEXT_RIGHT));
    a.lda(abs("mode_bottom").x());
    a.sta(abs(vdu::TEXT_BOTTOM));
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(!status::WINDOWED));
    a.sta(zp(vdu_zp::STATUS));
    a.jsr("graphics_window_default");
    a.jsr("window_width");
    a.jmp("vdu_30");

    // VDU 28, left, bottom, right, top: refused unless it fits the screen
    // the right way up.
    a.here("vdu_28");
    a.ldx(abs(vdu::MODE));
    a.lda(abs(QUEUE_LAST - 1));
    a.cmp(abs(QUEUE_LAST - 3));
    a.bcc(RTS);
    a.cmp(abs("mode_right").x());
    a.beq("vdu_28_right");
    a.bcs(RTS);
    a.here("vdu_28_right");
    a.lda(abs(QUEUE_LAST - 2));
    a.cmp(abs(QUEUE_LAST));
    a.bcc(RTS);
    a.cmp(abs("mode_bottom").x());
    a.beq("vdu_28_bottom");
    a.bcs(RTS);
    a.here("vdu_28_bottom");
    a.ldy(imm(3));
    a.here("vdu_28_copy");
    a.lda(abs(QUEUE_LAST - 3).y());
    a.sta(abs(vdu::TEXT_WINDOW).y());
    a.dey();
    a.bpl("vdu_28_copy");
    a.lda(imm(status::WINDOWED));
    a.ora(zp(vdu_zp::STATUS));
    a.sta(zp(vdu_zp::STATUS));
    a.jsr("window_width");
    // The cursor stays where it is if it is inside the window.
    a.lda(abs(vdu::CURSOR_X));
    a.cmp(abs(vdu::TEXT_LEFT));
    a.bcc("vdu_28_home");
    a.cmp(abs(vdu::TEXT_RIGHT));
    a.beq("vdu_28_x");
    a.bcs("vdu_28_home");
    a.here("vdu_28_x");
    a.lda(abs(vdu::CURSOR_Y));
    a.cmp(abs(vdu::TEXT_TOP));
    a.bcc("vdu_28_home");
    a.cmp(abs(vdu::TEXT_BOTTOM));
    a.beq("vdu_28_inside");
    a.bcs("vdu_28_home");
    a.here("vdu_28_inside");
    a.jmp("vdu_cursor_update");
    a.here("vdu_28_home");
    a.jmp("vdu_30");

    // &34C = the text window's width in bytes.
    a.here("window_width");
    a.lda(abs(vdu::TEXT_RIGHT));
    a.sec();
    a.sbc(abs(vdu::TEXT_LEFT));
    a.tax();
    a.inx();
    a.ldy(imm(0));
    a.jsr("cell_address");
    a.lda(zp(vdu_zp::TEMP));
    a.sec();
    a.sbc(abs(vdu::SCREEN_START));
    a.sta(abs(vdu::WINDOW_WIDTH));
    a.lda(zp(vdu_zp::TEMP + 1));
    a.sbc(abs(vdu::SCREEN_START + 1));
    a.bcs("window_width_high");
    a.adc(abs(vdu::SCREEN_SIZE));
    a.here("window_width_high");
    a.sta(abs(vdu::WINDOW_WIDTH + 1));
    a.rts();
}

/// Where MOS 1.20's MODE routine takes the mode in A, and where it starts to
/// clear the screen: software calls the first, and runs a copy of the
/// routine ended there with an RTS to set a mode and keep the screen.
pub const MODE_ENTRY: u16 = 0xCB1D;
pub const MODE_CLEAR: u16 = 0xCBF0;

/// The Model B's MODE, at the addresses software reaches it by.
pub fn pin_mode(a: &mut Asm) {
    const QUEUE_LOAD: u16 = 3;
    a.pin(MODE_ENTRY - QUEUE_LOAD, |a| mode(a, Model::ModelB));
}

/// VDU's own state at reset: no font exploded, the start-up mode, then
/// MODE's work. The Model B's MODE is pinned by `pin_mode`.
fn modes(a: &mut Asm, model: Model) {
    a.here("vdu_init");
    a.jsr("font_implode");
    a.lda(abs(var::STARTUP_OPTIONS));
    a.and(imm(7));
    a.sta(abs(QUEUE_LAST));
    a.lda(imm(0));
    a.sta(zp(vdu_zp::STATUS));
    match model {
        Model::ModelB => a.jmp("vdu_22"),
        Model::Electron => {
            a.sta(abs(vdu::CURSOR_STATE));
            mode(a, model);
        }
    }
}

/// MODE: the mode's variables, the Model B's 6845, ULA and latch or the
/// Electron's ULA, then the defaults and a clear screen. The Electron's MODE
/// 7 is MODE 6. Up to `mode_clear` it branches only within itself, so a
/// copy of it runs.
fn mode(a: &mut Asm, model: Model) {
    a.here("vdu_22");
    a.lda(abs(QUEUE_LAST));
    a.here("mode_set");
    a.and(imm(7));
    if model == Model::Electron {
        a.cmp(imm(7));
        a.bne("vdu_22_mode");
        a.lda(imm(6));
        a.here("vdu_22_mode");
    }
    a.tax();
    a.stx(abs(vdu::MODE));
    a.lda(abs("mode_bytes_per_character").x());
    a.sta(abs(vdu::BYTES_PER_CHARACTER));
    a.lda(abs("mode_colours").x());
    a.sta(abs(vdu::COLOURS));
    a.lda(abs("mode_pixels").x());
    a.sta(abs(vdu::PIXELS_PER_BYTE));
    a.lda(abs("mode_screen").x());
    a.sta(abs(vdu::SCREEN_BOTTOM));
    a.lda(abs("mode_map_type").x());
    a.sta(abs(vdu::MAP_TYPE));
    a.tay();
    a.lda(abs("map_size").y());
    a.sta(abs(vdu::SCREEN_SIZE));
    a.lda(abs("map_row_low").y());
    a.sta(abs(vdu::BYTES_PER_ROW));
    a.lda(abs("map_row_high").y());
    a.sta(abs(vdu::BYTES_PER_ROW + 1));
    a.lda(abs("map_rows_table").y());
    a.sta(zp(vdu_zp::ROW_TABLE));
    a.lda(abs("map_rows_table_high").y());
    a.sta(zp(vdu_zp::ROW_TABLE + 1));
    match model {
        Model::ModelB => model_b_mode(a),
        Model::Electron => {
            // The mode into `&FE07`, the flashing colours at their first,
            // and &35F as the Model B's 6845 cursor is set out, though the
            // Electron draws its own.
            a.lda(abs("mode_ula").x());
            a.ldx(imm(!control::MODE));
            a.jsr("ula_control");
            a.jsr("pixel_masks");
            a.lda(imm(0));
            a.sta(abs(var::ULA_CONTROL));
            a.lda(imm(ELECTRON_CURSOR_START));
            a.sta(abs(vdu::CURSOR_START));
        }
    }
    // The screen's start, before MODE_CLEAR, as a copy ended there leaves it.
    a.jsr("screen_start_reset");
    if has_teletext(model) {
        a.lda(imm(0x7F));
        a.sta(abs(vdu::TELETEXT_CURSOR));
    }
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(status::PRINTER | status::PAGED));
    a.sta(zp(vdu_zp::STATUS));
    a.jsr("vdu_20");
    a.jsr("vdu_26");
    a.clv();
    a.bvc("mode_clear");
    pixel_masks(a);
    if model == Model::ModelB {
        a.room_to(MODE_CLEAR);
    }
    a.here("mode_clear");
    a.jmp("screen_clear");
}

/// The Model B's MODE: the video ULA, the latch's screen size and the
/// 6845's registers from the last down, as MOS 1.20 writes them.
fn model_b_mode(a: &mut Asm) {
    a.lda(abs("mode_ula").x());
    a.sta(abs(var::ULA_CONTROL));
    a.sta(abs(sheila::VIDEO_CONTROL));
    a.jsr("pixel_masks");
    // The latch's two screen-size bits.
    a.lda(abs(vdu::MAP_TYPE));
    a.asl(Acc);
    a.tay();
    a.lda(abs("latch_size").y());
    a.sta(abs(svia(reg::ORB)));
    a.lda(abs(crate::rom::asm::sym("latch_size") + 1).y());
    a.sta(abs(svia(reg::ORB)));
    // *TV's interlace applies to MODE's R8 alone.
    a.ldx(abs(vdu::MAP_TYPE));
    a.ldy(abs("map_crtc").x());
    a.ldx(imm(super::modes::LAST_CRTC_REGISTER));
    a.here("vdu_22_crtc");
    a.lda(abs("crtc_registers").y());
    a.cpx(imm(8));
    a.bne("vdu_22_register");
    a.pha();
    a.lda(abs(super::ws::page2::TV_INTERLACE));
    // The teletext mode, with no colours, keeps the interlaced video its
    // characters need; every other mode's highest colour is odd.
    a.and(abs(vdu::COLOURS));
    a.lsr(Acc);
    a.pla();
    a.bcc("vdu_22_register");
    a.and(imm(0xFE));
    a.here("vdu_22_register");
    a.jsr("crtc_write");
    a.dey();
    a.dex();
    a.bpl("vdu_22_crtc");
}

/// The colour masks of the left and right pixels of a byte.
fn pixel_masks(a: &mut Asm) {
    a.here("pixel_masks");
    a.lda(abs(vdu::PIXELS_PER_BYTE));
    a.cmp(imm(3));
    a.beq("pixel_masks_4");
    a.cmp(imm(1));
    a.bne("pixel_masks_8");
    a.lda(imm(0xAA));
    a.ldx(imm(0x55));
    a.bne("pixel_masks_set");
    a.here("pixel_masks_4");
    a.lda(imm(0x88));
    a.ldx(imm(0x11));
    a.bne("pixel_masks_set");
    a.here("pixel_masks_8");
    a.lda(imm(0x80));
    a.ldx(imm(0x01));
    a.here("pixel_masks_set");
    a.sta(abs(vdu::LEFT_MASK));
    a.stx(abs(vdu::RIGHT_MASK));
    a.rts();
}

/// What the Electron's MODE leaves &35F holding: the Model B's 6845 cursor
/// start, its last raster and blinking.
const ELECTRON_CURSOR_START: u8 = 0x67;

fn tables(a: &mut Asm, model: Model) {
    // Each code's parameters, as the count &26A starts from: negated.
    a.later(|a| {
        a.here("vdu_parameters");
        for (_, parameters) in CONTROLS {
            a.byte(parameters.wrapping_neg());
        }
    });
    a.later(|a| {
        a.here("vdu_handler_low");
        for (handler, _) in CONTROLS {
            a.byte(lo(handler));
        }
    });
    a.later(|a| {
        a.here("vdu_handler_high");
        for (handler, _) in CONTROLS {
            a.byte(hi(handler));
        }
    });
    // Four bits of a glyph row as four two-bit pixels; two as two four-bit.
    a.later(|a| {
        a.here("expand_4");
        for nibble in 0u8..16 {
            let high = (0..4).fold(0u8, |byte, k| byte | (((nibble >> (3 - k)) & 1) << (7 - k)));
            a.byte(high | (high >> 4));
        }
    });
    a.later(|a| {
        a.here("expand_2");
        for pair in 0u8..4 {
            let left = if pair & 2 != 0 { 0xAA } else { 0 };
            let right = if pair & 1 != 0 { 0x55 } else { 0 };
            a.byte(left | right);
        }
    });
    a.later(|a| {
        a.here("expand_4_colours");
        a.bytes(&[0x00, 0x0F, 0xF0, 0xFF]);
    });
    a.later(|a| {
        a.here("expand_16_colours");
        for colour in 0u8..16 {
            let pixel = (0..4).fold(0u8, |byte, k| {
                byte | (((colour >> (3 - k)) & 1) << (7 - 2 * k))
            });
            a.byte(pixel | (pixel >> 1));
        }
    });
    // The logical colour a ULA palette index is, in the four-colour modes.
    a.later(|a| {
        a.here("palette_index_4");
        for index in 0u8..16 {
            a.byte(((index >> 2) & 2) | ((index >> 1) & 1));
        }
    });
    a.later(|a| {
        a.here("default_palette_2");
        a.bytes(&[0, 7]);
    });
    a.later(|a| {
        a.here("default_palette_4");
        a.bytes(&[0, 1, 3, 7]);
    });
    if model == Model::Electron {
        // Entry by entry, a pair's four together: its number, its place in
        // its pair eight times over, and what each colour sets at each place.
        a.later(|a| {
            a.here("palette_entries");
            for set in SETS {
                a.bytes(&set);
            }
        });
        a.later(|a| {
            a.here("palette_places");
            for _ in SETS {
                for place in 0..4u8 {
                    a.byte(place * 8);
                }
            }
        });
        a.later(|a| {
            a.here("palette_even");
            for place in 0..4 {
                for colour in 0..8 {
                    a.byte(register_bits(place, colour).0);
                }
            }
        });
        a.later(|a| {
            a.here("palette_odd");
            for place in 0..4 {
                for colour in 0..8 {
                    a.byte(register_bits(place, colour).1);
                }
            }
        });
    }
}

/// The Electron's own cursor: taken off the screen as the drivers start, and
/// put back where it now is as they finish, unless it is off; between,
/// display end flashes it, inverting the bottom line of its cell.
fn electron_cursor(a: &mut Asm) {
    // A kept.
    a.here("cursor_remove");
    a.php();
    a.sei();
    a.pha();
    a.lda(abs(vdu::CURSOR_STATE));
    a.ora(imm(cursor_state::BUSY));
    a.sta(abs(vdu::CURSOR_STATE));
    a.bpl("cursor_remove_done");
    a.jsr("cursor_toggle");
    a.here("cursor_remove_done");
    a.pla();
    a.plp();
    a.rts();

    // A kept.
    a.here("cursor_restore");
    a.php();
    a.sei();
    a.pha();
    a.lda(abs(vdu::CURSOR_STATE));
    a.and(imm(!cursor_state::BUSY));
    a.sta(abs(vdu::CURSOR_STATE));
    a.and(imm(cursor_state::OFF));
    a.bne("cursor_restore_done");
    a.jsr("cursor_toggle");
    a.here("cursor_restore_done");
    a.pla();
    a.plp();
    a.rts();

    // Display end: shown while TIME's flash bit is clear, unless the
    // drivers are at work or it is off.
    a.here("cursor_flash");
    a.lda(abs(vdu::CURSOR_STATE));
    a.and(imm(cursor_state::BUSY | cursor_state::OFF));
    a.bne(RTS);
    a.ldx(abs(var::TIME_SWITCH));
    a.lda(abs(page2::CLOCKS - 1).x());
    a.and(imm(CURSOR_FLASH));
    for _ in 0..3 {
        a.asl(Acc);
    }
    a.eor(abs(vdu::CURSOR_STATE));
    a.bmi(RTS);

    // The cursor's cell's bottom line inverted, and its drawn bit turned
    // over. The IRQ scratch at &FA points there.
    a.here("cursor_toggle");
    a.lda(abs(vdu::CURSOR_ADDRESS));
    a.clc();
    a.adc(imm(7));
    a.sta(zp(os::SCRATCH));
    a.lda(abs(vdu::CURSOR_ADDRESS + 1));
    a.adc(imm(0));
    a.sta(zp(os::SCRATCH + 1));
    a.lda(abs(vdu::BYTES_PER_CHARACTER));
    a.lsr(Acc);
    a.lsr(Acc);
    a.lsr(Acc);
    a.tax();
    a.ldy(imm(0));
    a.here("cursor_toggle_byte");
    a.lda(zp(os::SCRATCH).ind_y());
    a.eor(imm(0xFF));
    a.sta(zp(os::SCRATCH).ind_y());
    a.tya();
    a.clc();
    a.adc(imm(8));
    a.tay();
    a.dex();
    a.bne("cursor_toggle_byte");
    a.lda(abs(vdu::CURSOR_STATE));
    a.eor(imm(cursor_state::DRAWN));
    a.sta(abs(vdu::CURSOR_STATE));
    a.rts();
}
