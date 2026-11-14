//! The nine buffers behind INSV, REMV and CNPV, and events.
//!
//! A buffer's slots run from its first index to &FF, an address being its
//! base plus the index; the start index is the next to remove, the end the
//! next to fill, equal when empty, and a full buffer leaves one slot unused.
//! A busy flag of &FF marks a buffer empty, which wakes an output buffer's
//! driver when something is put in it. The Electron's sound has one queue,
//! so its four sound buffers are one: buffer 4.

use super::ws::{buffer, os, page2, var, vector};
use crate::machine::Model;
use crate::rom::asm::{abs, imm, zp, Acc, Asm, RTS};
use crate::rom::os::event;
pub use crate::rom::os::{buffer as number, PRINTER_BUSY};

/// Each buffer's first slot, base address and first index.
const BUFFERS: [(u16, u8); 9] = [
    (buffer::KEYBOARD, 0xE0),
    (buffer::INPUT, 0x00),
    (buffer::RS423_OUTPUT, 0x40),
    (buffer::PRINTER, 0xC0),
    (buffer::SOUND, 0xF0),
    (buffer::SOUND + 0x10, 0xF0),
    (buffer::SOUND + 0x20, 0xF0),
    (buffer::SOUND + 0x30, 0xF0),
    (buffer::SPEECH, 0xC0),
];

// Loading either event branches as nonzero.
const _: () = assert!(event::INPUT_FULL != 0 && event::CHARACTER != 0);

pub fn emit(a: &mut Asm, model: Model) {
    entries(a);
    if model == Model::Electron {
        // Buffers 5–7 are buffer 4; every flag kept.
        a.here("sound_alias");
        a.php();
        a.cpx(imm(number::SOUND + 1));
        a.bcc("sound_alias_done");
        a.cpx(imm(number::SOUND + 4));
        a.bcs("sound_alias_done");
        a.ldx(imm(number::SOUND));
        a.here("sound_alias_done");
        a.plp();
        a.rts();
    }
    insert(a, model);
    remove(a, model);
    count_purge(a, model);
    input(a);
    events(a);
    tables(a);
}

/// The Electron's vector entries take its sound buffers as the one.
fn alias(a: &mut Asm, model: Model) {
    if model == Model::Electron {
        a.jsr("sound_alias");
    }
}

/// The calls the rest of the MOS makes, each through its vector.
fn entries(a: &mut Asm) {
    a.here("buffer_insert");
    a.jmp(abs(vector::INS).ind());
    a.here("buffer_remove");
    a.clv();
    a.jmp(abs(vector::REM).ind());
    a.here("buffer_examine");
    a.bit(abs("rdch_v_set"));
    a.jmp(abs(vector::REM).ind());
    a.here("buffer_count");
    a.clv();
    a.jmp(abs(vector::CNP).ind());
    a.here("buffer_flush");
    a.bit(abs("rdch_v_set"));
    a.jmp(abs(vector::CNP).ind());

    a.here("buffers_flush_all");
    a.ldx(imm(number::COUNT - 1));
    a.here("buffers_flush_next");
    a.jsr("buffer_flush");
    a.dex();
    a.bpl("buffers_flush_next");
    a.rts();
}

/// INSV: A into buffer X, A kept; C set if it was full.
fn insert(a: &mut Asm, model: Model) {
    a.here("insv_default");
    alias(a, model);
    a.php();
    a.sei();
    a.pha();
    a.ldy(abs(page2::BUFFER_END).x());
    a.jsr("buffer_next");
    a.tya();
    a.cmp(abs(page2::BUFFER_START).x());
    a.beq("insv_full");
    a.jsr("buffer_pointer");
    a.ldy(abs(page2::BUFFER_END).x());
    a.pla();
    a.pha();
    a.sta(zp(os::SCRATCH).ind_y());
    a.jsr("buffer_next");
    a.tya();
    a.sta(abs(page2::BUFFER_END).x());
    // The transmitter is woken by every character; the printer only from
    // dormant, which its driver alone decides.
    a.cpx(imm(number::RS423_OUTPUT));
    a.bne("insv_not_rs423");
    a.jsr("rs423_wake");
    a.here("insv_not_rs423");
    a.cpx(imm(number::PRINTER));
    a.bne("insv_done");
    a.lda(abs(page2::BUFFER_BUSY).x());
    a.bpl("insv_done");
    a.jsr("printer_wake");
    a.here("insv_done");
    a.pla();
    a.plp();
    a.clc();
    a.rts();
    a.here("insv_full");
    a.pla();
    a.plp();
    a.sec();
    a.rts();

    // Y = the index after Y in buffer X.
    a.here("buffer_next");
    a.iny();
    a.bne(RTS);
    a.ldy(abs("buffer_first").x());
    a.rts();

    // (&FA) = buffer X's base.
    a.here("buffer_pointer");
    a.lda(abs("buffer_base_low").x());
    a.sta(zp(os::SCRATCH));
    a.lda(abs("buffer_base_high").x());
    a.sta(zp(os::SCRATCH + 1));
    a.rts();
}

/// REMV: A and Y = the next character of buffer X, taken unless V is set;
/// C set if the buffer was empty.
fn remove(a: &mut Asm, model: Model) {
    a.here("remv_default");
    alias(a, model);
    a.php();
    a.sei();
    a.lda(abs(page2::BUFFER_START).x());
    a.cmp(abs(page2::BUFFER_END).x());
    a.beq("remv_empty");
    a.tay();
    a.jsr("buffer_pointer");
    a.lda(zp(os::SCRATCH).ind_y());
    a.bvs("remv_examined");
    a.pha();
    a.jsr("buffer_next");
    a.tya();
    a.sta(abs(page2::BUFFER_START).x());
    // Room made in the Model B's RS423 input may let the far end send
    // again.
    if model == Model::ModelB {
        a.cpx(imm(number::RS423_INPUT));
        a.bne("remv_not_rs423");
        a.jsr("rs423_lines");
        a.here("remv_not_rs423");
    }
    a.lda(abs(page2::BUFFER_START).x());
    a.cmp(abs(page2::BUFFER_END).x());
    a.bne("remv_taken");
    // Empty now: an output buffer says so with an event.
    a.cpx(imm(number::RS423_OUTPUT));
    a.bcc("remv_taken");
    a.ldy(imm(event::OUTPUT_EMPTY));
    a.jsr("oseven");
    a.here("remv_taken");
    a.pla();
    a.here("remv_examined");
    a.tay();
    a.plp();
    a.clc();
    a.rts();
    a.here("remv_empty");
    a.plp();
    a.sec();
    a.rts();
}

/// CNPV: with V clear, X and Y = the characters in buffer X, or with C set
/// the space left; with V set, buffer X emptied.
fn count_purge(a: &mut Asm, model: Model) {
    a.here("cnpv_default");
    alias(a, model);
    a.bvs("cnpv_purge");
    a.php();
    a.sei();
    a.lda(abs(page2::BUFFER_END).x());
    a.sec();
    a.sbc(abs(page2::BUFFER_START).x());
    a.bcs("cnpv_counted");
    a.sec();
    a.sbc(abs("buffer_first").x());
    a.here("cnpv_counted");
    a.plp();
    a.bcc("cnpv_entries");
    // The space: the buffer's size, less the slot left unused, less what is
    // in it.
    a.eor(imm(0xFF));
    a.sec();
    a.sbc(abs("buffer_first").x());
    a.here("cnpv_entries");
    a.tax();
    a.ldy(imm(0));
    a.rts();
    a.here("cnpv_purge");
    a.php();
    a.sei();
    a.lda(imm(0xFF));
    a.sta(abs(page2::BUFFER_START).x());
    a.sta(abs(page2::BUFFER_END).x());
    a.sta(abs(page2::BUFFER_BUSY).x());
    a.cpx(imm(number::KEYBOARD));
    a.bne("cnpv_not_keyboard");
    a.lda(imm(0));
    a.sta(abs(var::SOFT_KEY_LENGTH));
    if model == Model::Electron {
        a.sta(abs(var::FIRM_LENGTH));
    }
    a.here("cnpv_not_keyboard");
    if model == Model::ModelB {
        a.cpx(imm(number::RS423_INPUT));
        a.bne("cnpv_purged");
        a.jsr("rs423_lines");
    }
    a.here("cnpv_purged");
    a.cpx(imm(number::SOUND));
    a.bcc("cnpv_done");
    a.cpx(imm(number::SOUND + 4));
    a.bcs("cnpv_done");
    a.jsr("sound_purge");
    a.here("cnpv_done");
    a.plp();
    a.rts();
}

/// A character arriving for an input buffer: the ESCAPE character, while
/// ESCAPE is live, sets the Escape condition instead; anything else goes
/// in, with an event, or another event if the buffer is full.
fn input(a: &mut Asm) {
    a.here("input_insert");
    a.cmp(abs(var::ESCAPE_CHAR));
    a.bne("input_character");
    a.ldy(abs(var::ESCAPE_ACTION));
    a.bne("input_character");
    // *FX200 can disable ESCAPE altogether.
    a.pha();
    a.lda(abs(var::BREAK_EFFECT));
    a.lsr(Acc);
    a.pla();
    a.bcs(RTS);
    a.jmp("escape_set");
    a.here("input_character");
    a.pha();
    a.jsr("buffer_insert");
    a.pla();
    a.bcs("input_full");
    a.ldy(imm(event::CHARACTER));
    a.bne("oseven");
    a.here("input_full");
    a.ldy(imm(event::INPUT_FULL));
    a.bne("oseven");

    // The Escape condition: flagged, and its event.
    a.here("escape_set");
    a.lda(imm(0x80));
    a.sta(zp(os::ESCAPE));
    a.ldy(imm(event::ESCAPE));
    a.fall_into("oseven");
}

/// OSEVEN: event Y, if enabled, through EVNTV with A the event and Y the
/// value A held; C set if the event is disabled.
fn events(a: &mut Asm) {
    a.here("oseven");
    a.php();
    a.sei();
    a.pha();
    a.lda(abs(page2::EVENTS).y());
    a.beq("oseven_disabled");
    a.sty(zp(os::SCRATCH));
    a.pla();
    a.tay();
    a.pha();
    a.lda(zp(os::SCRATCH));
    a.jsr("oseven_vector");
    a.pla();
    a.plp();
    a.clc();
    a.rts();
    a.here("oseven_disabled");
    a.pla();
    a.plp();
    a.sec();
    a.rts();
    a.here("oseven_vector");
    a.jmp(abs(vector::EVENT).ind());
}

fn tables(a: &mut Asm) {
    a.here("buffer_first");
    for (_, first) in BUFFERS {
        a.byte(first);
    }
    a.here("buffer_base_low");
    for (slot, first) in BUFFERS {
        a.byte((slot - u16::from(first)).to_le_bytes()[0]);
    }
    a.here("buffer_base_high");
    for (slot, first) in BUFFERS {
        a.byte((slot - u16::from(first)).to_le_bytes()[1]);
    }
}
