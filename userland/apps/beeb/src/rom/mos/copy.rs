//! Copy editing's two cursors, and reading a character back off the
//! screen.
//!
//! While editing, &318 is the copy cursor the cursor keys move and &364 the
//! input position, marked by &366's character in mode 7 and an inverted cell
//! otherwise. Output goes to the input position: the two are exchanged
//! around each character.

use super::vdu::status;
use super::ws::{vdu, vdu_zp};
use crate::rom::asm::{abs, imm, zp, Acc, Asm, RTS};

/// Where mode 7's marked character waits, as the original keeps it.
const MARKED: u16 = vdu::SCRATCH + 8;
/// A character cell's pattern, being matched against the font.
const PATTERN: u16 = vdu::SCRATCH;
/// The first character a cell is matched against.
const FIRST_CHARACTER: u8 = b' ';
// The two cursors are exchanged an axis at a time, y after x.
const _: () = assert!(vdu::CURSOR_Y == vdu::CURSOR_X + 1 && vdu::EDIT_Y == vdu::EDIT_X + 1);

pub fn emit(a: &mut Asm) {
    output(a);
    marking(a);
    reading(a);
}

fn output(a: &mut Asm) {
    // VDU output, at the input position while editing.
    a.here("vduchr");
    a.here("vdu_write");
    a.pha();
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(status::EDITING));
    a.bne("vdu_write_editing");
    a.pla();
    a.jmp("vdu_write_core");
    a.here("vdu_write_editing");
    a.jsr("edit_mark");
    a.jsr("edit_exchange");
    a.pla();
    a.jsr("vdu_write_core");
    // A MODE or the like ends editing with the cursor where output left it.
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(status::EDITING));
    a.beq(RTS);
    a.jsr("edit_exchange");
    a.jmp("edit_mark");

    // Editing begins at the cursor, which the copy cursor then leaves.
    a.here("edit_begin");
    a.lda(abs(vdu::CURSOR_X));
    a.sta(abs(vdu::EDIT_X));
    a.lda(abs(vdu::CURSOR_Y));
    a.sta(abs(vdu::EDIT_Y));
    a.lda(abs(vdu::TELETEXT_CURSOR));
    a.sta(abs(MARKED));
    a.lda(zp(vdu_zp::STATUS));
    a.ora(imm(status::EDITING));
    a.sta(zp(vdu_zp::STATUS));
    a.bne("edit_mark");

    // Editing ends with the cursor back at the input position.
    a.here("edit_end");
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(status::EDITING));
    a.beq(RTS);
    a.eor(zp(vdu_zp::STATUS));
    a.sta(zp(vdu_zp::STATUS));
    a.jsr("edit_mark");

    // The cursor's cell follows it, since output draws there.
    a.here("edit_exchange");
    a.ldx(imm(1));
    let axis = a.here_local();
    a.ldy(abs(vdu::CURSOR_X).x());
    a.lda(abs(vdu::EDIT_X).x());
    a.sta(abs(vdu::CURSOR_X).x());
    a.tya();
    a.sta(abs(vdu::EDIT_X).x());
    a.dex();
    a.bpl(axis);
    a.jmp("vdu_cursor_update");
}

/// The input position's mark, toggled: mode 7's character exchanged with
/// the one it covers, or every byte of the cell inverted.
fn marking(a: &mut Asm) {
    a.here("edit_mark");
    a.ldx(abs(vdu::EDIT_X));
    a.ldy(abs(vdu::EDIT_Y));
    a.jsr("cell_address");
    a.ldy(imm(0));
    a.lda(abs(vdu::COLOURS));
    a.bne("edit_mark_cell");
    a.lda(zp(vdu_zp::TEMP).ind_y());
    a.tax();
    a.lda(abs(MARKED));
    a.sta(zp(vdu_zp::TEMP).ind_y());
    a.stx(abs(MARKED));
    a.rts();
    a.here("edit_mark_cell");
    a.lda(zp(vdu_zp::TEMP).ind_y());
    a.eor(imm(0xFF));
    a.sta(zp(vdu_zp::TEMP).ind_y());
    a.iny();
    a.cpy(abs(vdu::BYTES_PER_CHARACTER));
    a.bne("edit_mark_cell");
    a.rts();
}

/// OSBYTE &87 and COPY: A = the character at column X, row Y, or 0 if it
/// is none the font draws.
fn reading(a: &mut Asm) {
    a.here("read_character");
    a.jsr("cell_address");
    a.ldy(imm(0));
    a.lda(abs(vdu::COLOURS));
    a.bne("read_bitmap");
    // Mode 7 stores what it shows, but for the three it exchanges.
    a.lda(zp(vdu_zp::TEMP).ind_y());
    a.cmp(imm(0x5F));
    a.bne("read_not_hash");
    a.lda(imm(b'#'));
    a.rts();
    a.here("read_not_hash");
    a.cmp(imm(0x60));
    a.bne("read_not_underscore");
    a.lda(imm(b'_'));
    a.rts();
    a.here("read_not_underscore");
    a.cmp(imm(b'#'));
    a.bne(RTS);
    a.lda(imm(0x60));
    a.rts();

    // Elsewhere each row's pixels not in the text background become its
    // bits, then the font is searched for the pattern.
    a.here("read_bitmap");
    a.lda(abs(vdu::BYTES_PER_CHARACTER));
    a.lsr(Acc);
    a.lsr(Acc);
    a.lsr(Acc);
    a.sta(zp(vdu_zp::TEMP + 2));
    a.ldx(imm(0));
    a.here("read_row");
    a.lda(imm(0));
    a.sta(abs(PATTERN).x());
    a.txa();
    a.tay();
    a.lda(zp(vdu_zp::TEMP + 2));
    a.sta(zp(vdu_zp::TEMP + 3));
    a.here("read_row_byte");
    a.lda(zp(vdu_zp::TEMP).ind_y());
    a.eor(abs(vdu::TEXT_BACKGROUND));
    a.jsr("read_pixels");
    a.tya();
    a.clc();
    a.adc(imm(8));
    a.tay();
    a.dec(zp(vdu_zp::TEMP + 3));
    a.bne("read_row_byte");
    a.inx();
    a.cpx(imm(8));
    a.bne("read_row");
    a.lda(imm(FIRST_CHARACTER));
    a.here("read_match");
    a.pha();
    a.jsr("glyph_address");
    a.ldy(imm(7));
    a.here("read_match_row");
    a.lda(zp(vdu_zp::TEMP + 4).ind_y());
    a.cmp(abs(PATTERN).y());
    a.bne("read_match_next");
    a.dey();
    a.bpl("read_match_row");
    a.pla();
    a.rts();
    a.here("read_match_next");
    a.pla();
    a.clc();
    a.adc(imm(1));
    a.bne("read_match");
    a.rts();

    // A byte's pixels, differences from the background in A, shifted into
    // the row's pattern at X: eight, four or two of them, as the mode packs
    // them.
    a.here("read_pixels");
    a.sta(zp(vdu_zp::TEMP + 4));
    a.lda(abs(vdu::PIXELS_PER_BYTE));
    a.cmp(imm(7));
    a.beq("read_pixels_eight");
    a.cmp(imm(3));
    a.beq("read_pixels_four");
    // Two pixels: the even bits and the odd ones.
    for mask in [0xAAu8, 0x55] {
        a.lda(zp(vdu_zp::TEMP + 4));
        a.and(imm(mask));
        a.cmp(imm(1));
        a.rol(abs(PATTERN).x());
    }
    a.rts();
    // Four: each pixel's two bits are four apart.
    a.here("read_pixels_four");
    a.lda(zp(vdu_zp::TEMP + 4));
    for _ in 0..4 {
        a.lsr(Acc);
    }
    a.ora(zp(vdu_zp::TEMP + 4));
    a.asl(Acc);
    a.asl(Acc);
    a.asl(Acc);
    a.asl(Acc);
    for _ in 0..4 {
        a.asl(Acc);
        a.rol(abs(PATTERN).x());
    }
    a.rts();
    a.here("read_pixels_eight");
    a.lda(zp(vdu_zp::TEMP + 4));
    a.sta(abs(PATTERN).x());
    a.rts();
}
