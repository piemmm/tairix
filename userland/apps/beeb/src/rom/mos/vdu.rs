//! The VDU drivers' text side: the queue that gathers each control code's
//! parameters, the control codes, character output in every mode, the
//! cursor, windows, scrolling, colours, the palette and MODE.
//!
//! The text cursor is held in absolute screen columns and rows; `&D8` holds
//! the address of its cell's top line, which wraps by the screen's size
//! once hardware scrolling has moved the screen's start.

use super::ws::{var, vdu, vdu_zp};
use super::{svia, DELETE};
use crate::machine::sheila;
use crate::machine::via::reg;
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
    ("vdu_7", 0),
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

pub fn emit(a: &mut Asm) {
    queue(a);
    simple_controls(a);
    printing(a);
    cursor(a);
    scrolling(a);
    clearing(a);
    colours(a);
    windows(a);
    modes(a);
    tables(a);
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
    a.eor(imm(0xFF));
    a.clc();
    a.adc(imm(1));
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

fn simple_controls(a: &mut Asm) {
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
    a.lda(abs(vdu::COLOURS));
    a.beq(RTS);
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

    a.here("vdu_7");
    a.jmp("bell");
}

/// Printing a character: its glyph drawn in the text colours at the cursor,
/// or in mode 7 its code stored, then the cursor moved on.
fn printing(a: &mut Asm) {
    a.here("vdu_print");
    a.tay();
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(status::GRAPHICS_CURSOR));
    a.beq("vdu_print_text");
    a.tya();
    a.jmp("vdu5_print");
    a.here("vdu_print_text");
    a.tya();
    a.ldx(abs(vdu::COLOURS));
    a.bne("vdu_print_bitmap");
    a.jsr("teletext_code");
    a.ldy(imm(0));
    a.sta(zp(vdu_zp::CURSOR_LINE).ind_y());
    a.jmp("vdu_9");

    a.here("vdu_print_bitmap");
    a.jsr("glyph_address");
    a.jsr("vdu_draw_glyph");
    a.jmp("vdu_9");

    // Teletext's codes for £, # and _ are #, _ and £'s places in ASCII.
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

    // (&DE) = the definition of character A: a zone in RAM where &367 says
    // so, otherwise the ROM font, which 160–255 repeat.
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
    vdu_draw_glyph(a);
    vdu_delete(a);
}

/// Draw the glyph at (&DE) into the cell at (&D8): each of the cell's
/// columns of bytes takes the glyph's top bits, expanded to its pixels,
/// in the text colours. A glyph's copy at &330 is shifted as it goes.
fn vdu_draw_glyph(a: &mut Asm) {
    a.here("vdu_draw_glyph");
    a.ldy(imm(7));
    a.here("draw_copy");
    a.lda(zp(vdu_zp::TEMP + 4).ind_y());
    a.sta(abs(vdu::SCRATCH).y());
    a.dey();
    a.bpl("draw_copy");
    a.lda(zp(vdu_zp::CURSOR_LINE));
    a.sta(zp(vdu_zp::TEMP));
    a.lda(zp(vdu_zp::CURSOR_LINE + 1));
    a.sta(zp(vdu_zp::TEMP + 1));
    a.lda(abs(vdu::BYTES_PER_CHARACTER));
    for _ in 0..3 {
        a.lsr(Acc);
    }
    a.sta(zp(vdu_zp::TEMP + 2));
    a.here("draw_column");
    a.ldy(imm(7));
    a.here("draw_row");
    a.ldx(abs(vdu::PIXELS_PER_BYTE));
    a.lda(abs(vdu::SCRATCH).y());
    a.cpx(imm(7));
    a.beq("draw_pixels_ready");
    a.jsr("draw_take_bits");
    a.here("draw_pixels_ready");
    a.and(zp(vdu_zp::TEXT_OR));
    a.eor(zp(vdu_zp::TEXT_EOR));
    a.sta(zp(vdu_zp::TEMP).ind_y());
    a.dey();
    a.bpl("draw_row");
    a.dec(zp(vdu_zp::TEMP + 2));
    a.beq(RTS);
    a.lda(imm(8));
    a.jsr("screen_pointer_add");
    a.jmp("draw_column");

    // A = the scratch row Y's top bits expanded for this mode, the row
    // shifted on past them.
    a.here("draw_take_bits");
    a.cpx(imm(3));
    a.bne("draw_two_bits");
    a.lsr(Acc);
    a.lsr(Acc);
    a.lsr(Acc);
    a.lsr(Acc);
    a.tax();
    a.lda(abs(vdu::SCRATCH).y());
    for _ in 0..4 {
        a.asl(Acc);
    }
    a.sta(abs(vdu::SCRATCH).y());
    a.lda(abs("expand_4").x());
    a.rts();
    a.here("draw_two_bits");
    a.lsr(Acc);
    for _ in 0..5 {
        a.lsr(Acc);
    }
    a.tax();
    a.lda(abs(vdu::SCRATCH).y());
    a.asl(Acc);
    a.asl(Acc);
    a.sta(abs(vdu::SCRATCH).y());
    a.lda(abs("expand_2").x());
    a.rts();

    // (&DA) += A, wrapped back into the screen past &8000.
    a.here("screen_pointer_add");
    a.clc();
    a.adc(zp(vdu_zp::TEMP));
    a.sta(zp(vdu_zp::TEMP));
    a.bcc(RTS);
    a.inc(zp(vdu_zp::TEMP + 1));
    a.bpl(RTS);
    a.lda(zp(vdu_zp::TEMP + 1));
    a.sec();
    a.sbc(abs(vdu::SCREEN_SIZE));
    a.sta(zp(vdu_zp::TEMP + 1));
    a.rts();
}

/// DEL: back a character, then a space over it, or in VDU 5 a cell in the
/// background colour.
fn vdu_delete(a: &mut Asm) {
    a.here("vdu_delete");
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(status::GRAPHICS_CURSOR));
    a.beq("vdu_delete_text");
    a.jmp("vdu5_delete");
    a.here("vdu_delete_text");
    a.jsr("vdu_8");
    a.lda(imm(b' '));
    a.ldx(abs(vdu::COLOURS));
    a.bne("vdu_delete_bitmap");
    a.ldy(imm(0));
    a.sta(zp(vdu_zp::CURSOR_LINE).ind_y());
    a.rts();
    a.here("vdu_delete_bitmap");
    a.jsr("glyph_address");
    a.jmp("vdu_draw_glyph");
}

/// The cursor: moving it, and following it with its address and the
/// 6845's cursor.
fn cursor(a: &mut Asm) {
    // Forward one: off the right edge to the next line's start.
    a.here("vdu_9");
    a.ldx(abs(vdu::CURSOR_X));
    a.cpx(abs(vdu::TEXT_RIGHT));
    a.bcs("vdu_9_wrap");
    a.inc(abs(vdu::CURSOR_X));
    a.jmp("vdu_cursor_update");
    a.here("vdu_9_wrap");
    a.lda(abs(vdu::TEXT_LEFT));
    a.sta(abs(vdu::CURSOR_X));
    a.jmp("vdu_10");

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

    a.here("vdu_10");
    a.jsr("vdu_paging");
    a.ldx(abs(vdu::CURSOR_Y));
    a.cpx(abs(vdu::TEXT_BOTTOM));
    a.bcs("vdu_10_scroll");
    a.inc(abs(vdu::CURSOR_Y));
    a.jmp("vdu_cursor_update");
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
    vdu_cursor_update(a);
    vdu_cursor_hide(a);
}

/// &D8 and &34A follow the cursor, and so does the 6845's.
fn vdu_cursor_update(a: &mut Asm) {
    a.here("vdu_cursor_update");
    a.ldx(abs(vdu::CURSOR_X));
    a.ldy(abs(vdu::CURSOR_Y));
    a.jsr("cell_address");
    a.lda(zp(vdu_zp::TEMP));
    a.sta(zp(vdu_zp::CURSOR_LINE));
    a.sta(abs(vdu::CURSOR_ADDRESS));
    a.lda(zp(vdu_zp::TEMP + 1));
    a.sta(zp(vdu_zp::CURSOR_LINE + 1));
    a.sta(abs(vdu::CURSOR_ADDRESS + 1));
    a.lda(imm(14));
    a.jmp("crtc_address");

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
    a.jmp("cell_shift");
    a.here("cell_shifted");
    a.tya();
    a.asl(Acc);
    a.tay();
    a.lda(zp(vdu_zp::TEMP));
    a.clc();
    a.adc(zp(vdu_zp::ROW_TABLE).ind_y());
    a.sta(zp(vdu_zp::TEMP));
    a.iny();
    a.lda(zp(vdu_zp::TEMP + 1));
    a.adc(zp(vdu_zp::ROW_TABLE).ind_y());
    a.sta(zp(vdu_zp::TEMP + 1));
    a.lda(abs(vdu::SCREEN_START));
    a.jmp("screen_pointer_add_high");

    // (&DA) += A + 256 × &351, wrapped.
    a.here("screen_pointer_add_high");
    a.clc();
    a.adc(zp(vdu_zp::TEMP));
    a.sta(zp(vdu_zp::TEMP));
    a.lda(zp(vdu_zp::TEMP + 1));
    a.adc(abs(vdu::SCREEN_START + 1));
    a.sta(zp(vdu_zp::TEMP + 1));
    a.bpl(RTS);
    a.sec();
    a.sbc(abs(vdu::SCREEN_SIZE));
    a.sta(zp(vdu_zp::TEMP + 1));
    a.rts();

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

/// The cursor's shape: off, or the mode's own.
fn vdu_cursor_hide(a: &mut Asm) {
    a.here("vdu_cursor_hide");
    a.lda(imm(0x20));
    a.bne("vdu_cursor_set");
    a.here("vdu_cursor_show");
    a.lda(abs(vdu::CURSOR_START));
    a.here("vdu_cursor_set");
    a.ldx(imm(10));
    a.stx(abs(CRTC_ADDRESS));
    a.sta(abs(CRTC_DATA));
    a.rts();

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
/// starts, a window by copying its rows.
fn scrolling(a: &mut Asm) {
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

    // &351 = A, wrapped; the 6845 told.
    a.here("screen_start_set");
    a.bpl("screen_start_in");
    a.sec();
    a.sbc(abs(vdu::SCREEN_SIZE));
    a.here("screen_start_in");
    a.sta(abs(vdu::SCREEN_START + 1));
    a.sta(zp(vdu_zp::TEMP + 1));
    a.lda(abs(vdu::SCREEN_START));
    a.sta(zp(vdu_zp::TEMP));
    a.lda(imm(12));
    a.jmp("crtc_address");

    // A window scrolls by copying each row's window bytes from the row
    // below, or above, then clearing the row that comes in.
    a.here("scroll_window_up");
    a.ldy(abs(vdu::TEXT_TOP));
    a.here("scroll_window_up_row");
    a.cpy(abs(vdu::TEXT_BOTTOM));
    a.bcs("scroll_window_up_done");
    a.tya();
    a.pha();
    a.iny();
    a.jsr("copy_row_from");
    a.pla();
    a.tay();
    a.iny();
    a.bne("scroll_window_up_row");
    a.here("scroll_window_up_done");
    a.jmp("clear_row");

    a.here("scroll_window_down");
    a.ldy(abs(vdu::TEXT_BOTTOM));
    a.here("scroll_window_down_row");
    a.cpy(abs(vdu::TEXT_TOP));
    a.beq("scroll_window_down_done");
    a.bcc("scroll_window_down_done");
    a.tya();
    a.pha();
    a.dey();
    a.jsr("copy_row_from");
    a.pla();
    a.tay();
    a.dey();
    a.jmp("scroll_window_down_row");
    a.here("scroll_window_down_done");
    a.jmp("clear_row");
    copy_row_from(a);
}

/// Copy row Y's window bytes into the row pushed on the stack before the
/// caller changed Y: (&DC) source, (&DA) destination.
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
    a.ldy(imm(0));
    a.here("copy_row_byte");
    a.lda(zp(vdu_zp::TEMP + 2).ind_y());
    a.sta(zp(vdu_zp::TEMP).ind_y());
    a.jsr("row_pointers_step");
    a.bne("copy_row_byte");
    a.rts();

    // Step (&DA) and (&DC) a byte, each wrapped, and count the bytes at
    // (&DE) down; Z once they are done.
    a.here("row_pointers_step");
    a.inc(zp(vdu_zp::TEMP + 2));
    a.bne("row_step_destination");
    a.inc(zp(vdu_zp::TEMP + 3));
    a.bpl("row_step_destination");
    a.lda(zp(vdu_zp::TEMP + 3));
    a.sec();
    a.sbc(abs(vdu::SCREEN_SIZE));
    a.sta(zp(vdu_zp::TEMP + 3));
    a.here("row_step_destination");
    a.inc(zp(vdu_zp::TEMP));
    a.bne("row_step_count");
    a.inc(zp(vdu_zp::TEMP + 1));
    a.bpl("row_step_count");
    a.lda(zp(vdu_zp::TEMP + 1));
    a.sec();
    a.sbc(abs(vdu::SCREEN_SIZE));
    a.sta(zp(vdu_zp::TEMP + 1));
    a.here("row_step_count");
    a.lda(zp(vdu_zp::TEMP + 4));
    a.bne("row_step_low");
    a.dec(zp(vdu_zp::TEMP + 5));
    a.here("row_step_low");
    a.dec(zp(vdu_zp::TEMP + 4));
    a.lda(zp(vdu_zp::TEMP + 4));
    a.ora(zp(vdu_zp::TEMP + 5));
    a.rts();
}

/// Clearing: a row's window bytes, the window, or the whole screen.
fn clearing(a: &mut Asm) {
    // Fill row Y's window bytes with the text background.
    a.here("clear_row");
    a.jsr("row_window");
    a.jsr("background_byte");
    a.sta(abs(vdu::SCRATCH));
    a.ldy(imm(0));
    a.here("clear_row_byte");
    a.lda(abs(vdu::SCRATCH));
    a.sta(zp(vdu_zp::TEMP).ind_y());
    a.jsr("row_pointers_step");
    a.bne("clear_row_byte");
    a.rts();

    // The byte a cleared cell holds: a space in mode 7.
    a.here("background_byte");
    a.lda(abs(vdu::COLOURS));
    a.beq("background_teletext");
    a.lda(abs(vdu::TEXT_BACKGROUND));
    a.rts();
    a.here("background_teletext");
    a.lda(imm(b' '));
    a.rts();

    // CLS: a window row by row; the whole screen with the 6845 back at its
    // start, every byte cleared.
    a.here("vdu_12");
    a.lda(imm(0));
    a.sta(abs(var::PAGED_LINES));
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
    a.jmp("vdu_30");
    a.here("cls_screen");
    a.lda(imm(0));
    a.sta(abs(vdu::SCREEN_START));
    a.lda(abs(vdu::SCREEN_BOTTOM));
    a.jsr("screen_start_set");
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
    a.jmp("vdu_30");
}

/// Colours: COLOUR, GCOL, the palette, and the defaults VDU 20 restores.
fn colours(a: &mut Asm) {
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
    a.lda(abs(vdu::COLOURS));
    a.beq(RTS);
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
    a.lda(abs(vdu::COLOURS));
    a.beq(RTS);
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

    // VDU 19, logical, physical: the palette entry, then the ULA rewritten.
    a.here("vdu_19");
    a.lda(abs(QUEUE_LAST - 4));
    a.and(abs(vdu::COLOURS));
    a.tax();
    a.lda(abs(QUEUE_LAST - 3));
    a.and(imm(0x0F));
    a.sta(abs(vdu::PALETTE).x());
    ula_palette(a);
}

/// Write every ULA palette entry from the logical palette: each entry is
/// the colour of the pixel value its index forms.
fn ula_palette(a: &mut Asm) {
    a.here("ula_palette");
    a.ldy(imm(15));
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
    a.lda(abs(vdu::PALETTE).x());
    a.eor(imm(0x07));
    a.sta(zp(vdu_zp::TEMP));
    a.tya();
    for _ in 0..4 {
        a.asl(Acc);
    }
    a.ora(zp(vdu_zp::TEMP));
    a.sta(abs(sheila::VIDEO_PALETTE));
    a.dey();
    a.bpl("ula_palette_entry");
    a.rts();

    // VDU 20: white on black text and graphics, plotting outright, and the
    // mode's own palette.
    a.here("vdu_20");
    a.ldx(abs(vdu::COLOURS));
    a.beq(RTS);
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

/// MODE: the mode's variables, the 6845, the ULA and the latch, then the
/// defaults and a clear screen.
fn modes(a: &mut Asm) {
    a.here("vdu_init");
    a.jsr("font_implode");
    a.lda(abs(var::STARTUP_OPTIONS));
    a.and(imm(7));
    a.sta(abs(QUEUE_LAST));
    a.lda(imm(0));
    a.sta(zp(vdu_zp::STATUS));

    a.here("vdu_22");
    a.lda(abs(QUEUE_LAST));
    a.and(imm(7));
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
    a.lda(abs("mode_size").x());
    a.sta(abs(vdu::SCREEN_SIZE));
    a.lda(abs("mode_map_type").x());
    a.sta(abs(vdu::MAP_TYPE));
    a.lda(abs("mode_row_low").x());
    a.sta(abs(vdu::BYTES_PER_ROW));
    a.lda(abs("mode_row_high").x());
    a.sta(abs(vdu::BYTES_PER_ROW + 1));
    a.lda(abs("mode_rows_table").x());
    a.sta(zp(vdu_zp::ROW_TABLE));
    a.lda(abs("mode_rows_table_high").x());
    a.sta(zp(vdu_zp::ROW_TABLE + 1));
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
    a.lda(imm(0));
    a.sta(abs(vdu::SCREEN_START));
    a.lda(abs(vdu::SCREEN_BOTTOM));
    a.sta(abs(vdu::SCREEN_START + 1));
    // The 6845's registers 0–11, *TV's vertical shift and interlace applied.
    a.ldx(abs(vdu::MODE));
    a.ldy(abs("mode_crtc").x());
    a.ldx(imm(0));
    a.here("vdu_22_crtc");
    a.lda(abs("crtc_registers").y());
    a.cpx(imm(7));
    a.bne("vdu_22_not_sync");
    a.clc();
    a.adc(abs(super::ws::page2::TV_SHIFT));
    a.here("vdu_22_not_sync");
    a.cpx(imm(8));
    a.bne("vdu_22_not_interlace");
    a.pha();
    a.lda(abs(super::ws::page2::TV_INTERLACE));
    a.lsr(Acc);
    a.pla();
    a.bcc("vdu_22_not_interlace");
    a.and(imm(0xFE));
    a.here("vdu_22_not_interlace");
    a.cpx(imm(10));
    a.bne("vdu_22_register");
    a.sta(abs(vdu::CURSOR_START));
    a.here("vdu_22_register");
    a.stx(abs(CRTC_ADDRESS));
    a.sta(abs(CRTC_DATA));
    a.iny();
    a.inx();
    a.cpx(imm(12));
    a.bne("vdu_22_crtc");
    a.lda(imm(0x7F));
    a.sta(abs(vdu::TELETEXT_CURSOR));
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(status::PRINTER | status::PAGED));
    a.sta(zp(vdu_zp::STATUS));
    a.jsr("vdu_20");
    a.jsr("vdu_26");
    a.jmp("vdu_12");
    pixel_masks(a);
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

fn tables(a: &mut Asm) {
    a.here("vdu_parameters");
    for (_, parameters) in CONTROLS {
        a.byte(parameters);
    }
    a.here("vdu_handler_low");
    for (handler, _) in CONTROLS {
        a.byte(lo(handler));
    }
    a.here("vdu_handler_high");
    for (handler, _) in CONTROLS {
        a.byte(hi(handler));
    }
    // Four bits of a glyph row as four two-bit pixels; two as two four-bit.
    a.here("expand_4");
    for nibble in 0u8..16 {
        let high = (0..4).fold(0u8, |byte, k| byte | (((nibble >> (3 - k)) & 1) << (7 - k)));
        a.byte(high | (high >> 4));
    }
    a.here("expand_2");
    for pair in 0u8..4 {
        let left = if pair & 2 != 0 { 0xAA } else { 0 };
        let right = if pair & 1 != 0 { 0x55 } else { 0 };
        a.byte(left | right);
    }
    a.here("expand_4_colours");
    a.bytes(&[0x00, 0x0F, 0xF0, 0xFF]);
    a.here("expand_16_colours");
    for colour in 0u8..16 {
        let pixel = (0..4).fold(0u8, |byte, k| {
            byte | (((colour >> (3 - k)) & 1) << (7 - 2 * k))
        });
        a.byte(pixel | (pixel >> 1));
    }
    // The logical colour a ULA palette index is, in the four-colour modes.
    a.here("palette_index_4");
    for index in 0u8..16 {
        a.byte(((index >> 2) & 2) | ((index >> 1) & 1));
    }
    a.here("default_palette_2");
    a.bytes(&[0, 7]);
    a.here("default_palette_4");
    a.bytes(&[0, 1, 3, 7]);
}
