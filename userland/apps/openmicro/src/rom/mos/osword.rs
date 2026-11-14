//! OSWORD: calls 0 to 13 on the parameter block at XY, &E0 and above to
//! USERV, and the rest offered to the ROMs.

use super::has_teletext;
use super::ws::{os, page2, var, vdu, vector};
use crate::machine::Model;
use crate::rom::asm::{abs, hi, imm, lo, sym, zp, Asm, RTS};

const CALL_COUNT: u8 = 14;
const CALLS: [&str; CALL_COUNT as usize] = [
    "osword_0",
    "osword_read_clock",
    "osword_write_clock",
    "osword_read_interval",
    "osword_write_interval",
    "osword_read_memory",
    "osword_write_memory",
    "sound_command",
    "envelope_command",
    "osword_read_pixel",
    "osword_read_character",
    "osword_read_palette",
    "osword_write_palette",
    "osword_graphics_cursors",
];

/// Service call for an unrecognised OSWORD.
/// The first call passed to USERV.
const USER_FIRST: u8 = 0xE0;

pub fn emit(a: &mut Asm, model: Model) {
    a.here("osword_default");
    a.sta(zp(os::CALL_A));
    a.stx(zp(os::CALL_X));
    a.sty(zp(os::CALL_Y));
    a.cmp(imm(CALL_COUNT));
    a.bcs("osword_unrecognised");
    a.jsr("osword_call");
    a.here("osword_return");
    a.ldx(zp(os::CALL_X));
    a.ldy(zp(os::CALL_Y));
    a.lda(zp(os::CALL_A));
    a.rts();
    a.here("osword_unrecognised");
    a.cmp(imm(USER_FIRST));
    a.bcs("osword_user");
    a.lda(imm(crate::rom::os::service::OSWORD));
    a.jsr("service_call");
    a.tax();
    a.beq("osword_return");
    a.ldy(zp(os::CALL_Y));
    a.lda(zp(os::CALL_A));
    // A call none took: A and Y as given, X &FF with N and V set and C
    // clear, as MOS 1.20 returns.
    a.here("call_refused");
    let all_ones = a.here_local();
    a.ldx(imm(0xFF));
    a.bit(abs(all_ones + 1));
    a.clc();
    a.rts();
    a.here("osword_user");
    a.jmp(abs(vector::USER).ind());
    // The call, with Y zero for its block.
    a.here("osword_call");
    a.tax();
    a.lda(abs("osword_high").x());
    a.pha();
    a.lda(abs("osword_low").x());
    a.pha();
    a.ldy(imm(0));
    a.rts();

    clocks(a);
    memory(a);
    graphics(a, model);
    a.here("osword_low");
    for call in CALLS {
        a.byte(lo(sym(call) + -1));
    }
    a.here("osword_high");
    for call in CALLS {
        a.byte(hi(sym(call) + -1));
    }
}

/// TIME and the interval timer: kept most significant byte first, read and
/// written least significant first. A written TIME goes into both copies.
fn clocks(a: &mut Asm) {
    a.here("osword_read_clock");
    a.ldx(abs(var::TIME_SWITCH));
    a.jmp("osword_read_five");
    a.here("osword_read_interval");
    a.ldx(imm(15));
    a.here("osword_read_five");
    a.php();
    a.sei();
    a.here("osword_read_byte");
    a.lda(abs(page2::CLOCKS - 1).x());
    a.sta(zp(os::CALL_X).ind_y());
    a.dex();
    a.iny();
    a.cpy(imm(5));
    a.bne("osword_read_byte");
    a.plp();
    a.rts();

    a.here("osword_write_clock");
    a.php();
    a.sei();
    a.ldx(imm(5));
    a.here("osword_write_clock_byte");
    a.lda(zp(os::CALL_X).ind_y());
    a.sta(abs(page2::CLOCKS - 1).x());
    a.sta(abs(page2::CLOCKS + 4).x());
    a.dex();
    a.iny();
    a.cpy(imm(5));
    a.bne("osword_write_clock_byte");
    a.plp();
    a.rts();

    a.here("osword_write_interval");
    a.php();
    a.sei();
    a.ldx(imm(15));
    a.here("osword_write_interval_byte");
    a.lda(zp(os::CALL_X).ind_y());
    a.sta(abs(page2::CLOCKS - 1).x());
    a.dex();
    a.iny();
    a.cpy(imm(5));
    a.bne("osword_write_interval_byte");
    a.plp();
    a.rts();
}

/// The I/O processor's memory, and sound.
fn memory(a: &mut Asm) {
    a.here("osword_read_memory");
    a.php();
    a.jsr("osword_address");
    a.lda(zp(os::SCRATCH).ind_y());
    a.ldy(imm(4));
    a.sta(zp(os::CALL_X).ind_y());
    a.plp();
    a.rts();

    a.here("osword_write_memory");
    a.php();
    a.ldy(imm(4));
    a.lda(zp(os::CALL_X).ind_y());
    a.pha();
    a.jsr("osword_address");
    a.pla();
    a.sta(zp(os::SCRATCH).ind_y());
    a.plp();
    a.rts();

    // (&FA) = the block's address, Y zero, with interrupts off while it is
    // used, as the interrupt routines use &FA too; the caller restores I.
    a.here("osword_address");
    a.sei();
    a.ldy(imm(0));
    a.lda(zp(os::CALL_X).ind_y());
    a.sta(zp(os::SCRATCH));
    a.iny();
    a.lda(zp(os::CALL_X).ind_y());
    a.sta(zp(os::SCRATCH + 1));
    a.ldy(imm(0));
    a.rts();
}

/// POINT, character definitions, the palette and the graphics cursors.
fn graphics(a: &mut Asm, model: Model) {
    a.here("osword_read_pixel");
    a.ldy(imm(3));
    a.here("osword_read_pixel_point");
    a.lda(zp(os::CALL_X).ind_y());
    a.sta(abs(vdu::SCRATCH).y());
    a.dey();
    a.bpl("osword_read_pixel_point");
    if has_teletext(model) {
        a.lda(abs(vdu::COLOURS));
        a.beq("osword_pixel_none");
    }
    a.jsr("external_to_internal");
    a.jsr("read_pixel");
    if has_teletext(model) {
        a.jmp("osword_pixel_store");
        a.here("osword_pixel_none");
        a.lda(imm(0xFF));
    }
    a.here("osword_pixel_store");
    a.ldy(imm(4));
    a.sta(zp(os::CALL_X).ind_y());
    a.rts();

    a.here("osword_read_character");
    a.lda(zp(os::CALL_X).ind_y());
    a.cmp(imm(0x20));
    a.bcc(RTS);
    a.jsr("glyph_address");
    a.ldy(imm(0));
    a.here("osword_character_byte");
    a.lda(zp(super::ws::vdu_zp::TEMP + 4).ind_y());
    a.iny();
    a.sta(zp(os::CALL_X).ind_y());
    a.cpy(imm(8));
    a.bne("osword_character_byte");
    a.rts();

    a.here("osword_read_palette");
    a.lda(zp(os::CALL_X).ind_y());
    a.and(abs(vdu::COLOURS));
    a.tax();
    a.lda(abs(vdu::PALETTE).x());
    a.iny();
    a.sta(zp(os::CALL_X).ind_y());
    a.lda(imm(0));
    for _ in 0..3 {
        a.iny();
        a.sta(zp(os::CALL_X).ind_y());
    }
    a.rts();

    a.here("osword_write_palette");
    a.lda(zp(os::CALL_X).ind_y());
    a.sta(abs(vdu::QUEUE_END - 4));
    a.iny();
    a.lda(zp(os::CALL_X).ind_y());
    a.sta(abs(vdu::QUEUE_END - 3));
    a.jmp("vdu_19");

    // The previous graphics cursor, then the current one.
    a.here("osword_graphics_cursors");
    a.ldx(imm(3));
    a.here("osword_cursor_previous");
    a.lda(abs(vdu::OLD_GRAPHICS_CURSOR).x());
    a.sta(abs(vdu::SCRATCH).x());
    a.dex();
    a.bpl("osword_cursor_previous");
    a.jsr("internal_to_external");
    a.ldy(imm(7));
    a.here("osword_cursor_byte");
    a.cpy(imm(4));
    a.bcc("osword_cursor_previous_byte");
    a.lda(abs(vdu::GRAPHICS_CURSOR - 4).y());
    a.bcs("osword_cursor_put");
    a.here("osword_cursor_previous_byte");
    a.lda(abs(vdu::SCRATCH).y());
    a.here("osword_cursor_put");
    a.sta(zp(os::CALL_X).ind_y());
    a.dey();
    a.bpl("osword_cursor_byte");
    a.rts();
}
