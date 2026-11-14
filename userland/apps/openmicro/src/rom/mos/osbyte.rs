//! OSBYTE: the calls below &A6 by table, and from &A6 the variables at
//! `&190 + A`, each read and written as `(old AND Y) EOR X` with the old
//! value returned in X and the next variable in Y. A call nothing here
//! knows is offered to the ROMs, and if none takes it OSBYTE returns with X
//! &FF and N and V set. On the Electron the calls for hardware an expansion
//! brings — RS423, the ADC, speech — are offered to the ROMs, and those for
//! hardware it has none of do nothing.

use super::ws::{os, page2, var, vdu, vector};
use super::{masked_calls, svia};
use crate::machine::bbc::sheila;
use crate::machine::bbc::via::reg;
use crate::machine::Model;
use crate::rom::asm::{abs, hi, imm, lo, zp, Acc, Asm, RTS};
use crate::rom::os::osbyte;

/// The version OSBYTE 0 returns in X, with Y zero, when X is non-zero: the
/// Model B's, and the Electron's.
const fn version(model: Model) -> u8 {
    match model {
        Model::ModelB => 1,
        Model::Electron => 0,
    }
}

/// How many calls from 0 the first table holds: the Electron's two more are
/// its paged ROMs' polling semaphore's.
const fn low_calls(model: Model) -> u8 {
    match model {
        Model::ModelB => 22,
        Model::Electron => 24,
    }
}
/// The first call of the second table, and the first after it; those up to
/// the variables are unknown to the MOS.
const HIGH_FIRST: u8 = 0x75;
const HIGH_END: u8 = 0xA1;

/// The calls 0 to 21.
const LOW: [&str; 22] = [
    "osbyte_identify",
    "osbyte_1",
    "osbyte_input",
    "osbyte_3",
    "osbyte_4",
    "osbyte_printer",
    "osbyte_6",
    "osbyte_receive_baud",
    "osbyte_transmit_baud",
    "osbyte_9",
    "osbyte_10",
    "osbyte_11",
    "osbyte_12",
    "osbyte_disable_event",
    "osbyte_enable_event",
    "osbyte_flush_input",
    "osbyte_16",
    "adc_force",
    "osbyte_reset_keys",
    "osbyte_wait_vsync",
    "font_explode",
    "osbyte_flush",
];

/// The calls &75 to &A0.
const HIGH: [&str; (HIGH_END - HIGH_FIRST) as usize] = [
    "osbyte_vdu_status",
    "keyboard_leds_call",
    "close_spool_exec",
    "osbyte_keys_pressed",
    "osbyte_keyboard_scan",
    "osbyte_keyboard_scan_16",
    "osbyte_printer_dormant",
    "osbyte_clear_escape",
    "osbyte_set_escape",
    "osbyte_acknowledge_escape",
    "osbyte_eof",
    "osbyte_adc_or_buffer",
    "osbyte_inkey",
    "osbyte_high_order",
    "osbyte_oshwm",
    "osbyte_himem",
    "osbyte_himem_mode",
    "osbyte_cursor_position",
    "osbyte_read_character",
    "osbyte_code",
    "cassette_motor",
    "osbyte_insert",
    "osbyte_opt",
    "select_tape",
    "select_rom",
    "osbyte_language",
    "osbyte_service",
    "osbyte_tv",
    "osbyte_remove",
    "osbyte_read_fred",
    "osbyte_write_fred",
    "osbyte_read_jim",
    "osbyte_write_jim",
    "osbyte_read_sheila",
    "osbyte_write_sheila",
    "osbyte_examine",
    "osbyte_insert_input",
    "osbyte_ula_control",
    "osbyte_ula_palette",
    "osbyte_acia_control",
    "osbyte_fast_bput",
    "return",
    "return",
    "osbyte_vdu_variable",
];

/// What the Electron takes otherwise: OSBYTE number, and its routine.
const ELECTRON_CALLS: [(u8, &str); 11] = [
    (0x07, "osbyte_unknown"),
    (0x08, "osbyte_unknown"),
    (osbyte::SELECT_ADC, "osbyte_unknown"),
    (osbyte::FORCE_ADC, "osbyte_unknown"),
    (osbyte::ADVAL, "osbyte_electron_adc"),
    (0x90, "return"),
    (0x9A, "return"),
    (0x9B, "return"),
    (0x9C, "osbyte_unknown"),
    (0x9E, "osbyte_unknown"),
    (0x9F, "osbyte_unknown"),
];

/// The routine of each call below the variables, low calls then high.
fn routines(model: Model) -> alloc::vec::Vec<&'static str> {
    let mut low: alloc::vec::Vec<&str> = LOW.to_vec();
    if model == Model::Electron {
        low.extend(["osbyte_poll_up", "osbyte_poll_down"]);
    }
    let mut calls: alloc::vec::Vec<(u8, &str)> =
        (0..).zip(low).chain((HIGH_FIRST..).zip(HIGH)).collect();
    if model == Model::Electron {
        for (number, routine) in ELECTRON_CALLS {
            if let Some(call) = calls.iter_mut().find(|(n, _)| *n == number) {
                call.1 = routine;
            }
        }
    }
    calls.into_iter().map(|(_, routine)| routine).collect()
}

pub fn emit(a: &mut Asm, model: Model) {
    dispatch(a, model);
    variables(a);
    simple(a, model);
    streams(a);
    timing(a, model);
    input(a, model);
    machine(a, model);
    tables(a, model);
}

fn dispatch(a: &mut Asm, model: Model) {
    masked_calls(a);
    a.cmp(imm(var::FIRST));
    a.bcs("osbyte_variable");
    a.cmp(imm(HIGH_END));
    a.bcs("osbyte_unrecognised");
    a.cmp(imm(low_calls(model)));
    a.bcc("osbyte_low");
    a.cmp(imm(HIGH_FIRST));
    a.bcc("osbyte_unrecognised");
    a.sbc(imm(HIGH_FIRST - low_calls(model)));
    a.here("osbyte_low");
    // The call returns here with X and Y set, C as it leaves it.
    a.jsr("osbyte_call");
    a.lda(zp(os::CALL_A));
    a.clv();
    a.rts();
    a.here("osbyte_call");
    a.tax();
    a.lda(abs("osbyte_high_table").x());
    a.pha();
    a.lda(abs("osbyte_low_table").x());
    a.pha();
    a.ldx(zp(os::CALL_X));
    a.ldy(zp(os::CALL_Y));
    a.lda(zp(os::CALL_A));
    a.rts();

    // A call the tables hold no routine for, met inside the dispatch.
    a.here("osbyte_unknown");
    a.pla();
    a.pla();

    // Unknown to the MOS: the ROMs' turn. If none takes it, the call is
    // refused, Y kept from the second table's first call on and 0 below it.
    a.here("osbyte_unrecognised");
    a.ldx(zp(os::CALL_X));
    a.ldy(zp(os::CALL_Y));
    a.lda(imm(crate::rom::os::service::OSBYTE));
    a.jsr("service_call");
    a.tax();
    a.beq("osbyte_claimed");
    a.ldy(zp(os::CALL_Y));
    a.lda(zp(os::CALL_A));
    a.cmp(imm(HIGH_FIRST));
    a.bcs("osbyte_refused");
    a.ldy(imm(0));
    a.here("osbyte_refused");
    a.jmp("call_refused");
    a.here("osbyte_claimed");
    a.ldx(zp(os::CALL_X));
    a.ldy(zp(os::CALL_Y));
    a.lda(zp(os::CALL_A));
    a.clv();
    a.rts();
}

/// &A6–&FF: the variable at &190 + A.
fn variables(a: &mut Asm) {
    a.here("osbyte_variable");
    a.tax();
    a.lda(abs(var::BASE).x());
    a.tay();
    a.and(zp(os::CALL_Y));
    a.eor(zp(os::CALL_X));
    a.sta(abs(var::BASE).x());
    a.lda(abs(var::BASE + 1).x());
    a.sty(zp(os::CALL_X));
    a.tay();
    a.ldx(zp(os::CALL_X));
    a.lda(zp(os::CALL_A));
    a.clv();
    a.rts();

    // A call that writes variable A with X, returning the old value: as
    // OSBYTE A with Y zero.
    a.here("osbyte_write_variable");
    a.ldy(imm(0));
    a.sty(zp(os::CALL_Y));
    a.jmp("osbyte_variable");
}

fn simple(a: &mut Asm, model: Model) {
    for (call, variable) in [
        ("osbyte_1", 0xF1u8),
        ("osbyte_3", 0xEC),
        ("osbyte_4", 0xED),
        ("osbyte_6", 0xF6),
        ("osbyte_9", 0xC3),
        ("osbyte_10", 0xC2),
        ("osbyte_11", 0xC4),
    ] {
        a.here(call);
        a.lda(imm(variable));
        a.bne("osbyte_write_variable");
    }

    // *FX12,0 restores both auto-repeat settings; otherwise the period.
    a.here("osbyte_12");
    a.txa();
    a.bne("osbyte_12_period");
    a.lda(imm(0x32));
    a.sta(abs(var::REPEAT_DELAY));
    a.ldx(imm(0x08));
    a.here("osbyte_12_period");
    a.lda(imm(0xC5));
    a.bne("osbyte_write_variable");

    // OSBYTE 0: with X zero an error naming the MOS, otherwise its version.
    a.here("osbyte_identify");
    a.txa();
    a.bne("osbyte_version");
    a.error(
        0xF7,
        match model {
            Model::ModelB => "OS 1.20",
            Model::Electron => "OS 1.00",
        },
    );
    a.here("osbyte_version");
    a.ldx(imm(version(model)));
    a.ldy(imm(0));
    a.rts();

    a.here("osbyte_vdu_status");
    a.ldx(zp(super::ws::vdu_zp::STATUS));
    a.rts();

    // *FX118 leaves X 1, Y &7F and C clear, as MOS 1.20 does.
    a.here("keyboard_leds_call");
    a.jsr("keyboard_leds");
    a.ldx(imm(1));
    a.ldy(imm(0x7F));
    a.clc();
    a.rts();

    // &78: X into &ED, Y into &EC.
    a.here("osbyte_keys_pressed");
    a.stx(zp(os::KEY_FIRST));
    a.sty(zp(os::KEY_LAST));
    a.rts();

    a.here("osbyte_keyboard_scan_16");
    a.ldx(imm(0x10));
    a.here("osbyte_keyboard_scan");
    // Y as MOS 1.20 leaves it: &7F once a key is tested, &EE once scanned.
    let scanning = a.local();
    a.ldy(imm(0xEE));
    a.txa();
    a.bpl(scanning);
    a.ldy(imm(0x7F));
    a.bind(scanning);
    a.sec();
    a.clv();
    a.here("keyv_call");
    a.jmp(abs(vector::KEY).ind());

    a.here("osbyte_high_order");
    a.ldx(imm(0xFF));
    a.ldy(imm(0xFF));
    a.rts();

    a.here("osbyte_oshwm");
    a.ldx(imm(0));
    a.ldy(abs(var::OSHWM));
    a.rts();

    a.here("osbyte_himem");
    a.ldx(imm(0));
    a.ldy(abs(vdu::SCREEN_BOTTOM));
    a.rts();

    a.here("osbyte_himem_mode");
    a.txa();
    a.and(imm(7));
    if model == Model::Electron {
        // MODE 7 is MODE 6.
        a.cmp(imm(7));
        a.bne("osbyte_himem_mode_known");
        a.lda(imm(6));
        a.here("osbyte_himem_mode_known");
    }
    a.tax();
    a.ldy(abs("mode_screen").x());
    a.ldx(imm(0));
    a.rts();
    osbyte_cursor_position(a, model);
}

/// OSBYTE &86 and &87: the text cursor in the window, and the character at
/// it with the mode.
fn osbyte_cursor_position(a: &mut Asm, model: Model) {
    a.here("osbyte_cursor_position");
    a.lda(abs(vdu::CURSOR_X));
    a.sec();
    a.sbc(abs(vdu::TEXT_LEFT));
    a.tax();
    a.lda(abs(vdu::CURSOR_Y));
    a.sec();
    a.sbc(abs(vdu::TEXT_TOP));
    a.tay();
    a.rts();

    a.here("osbyte_read_character");
    a.ldx(abs(vdu::CURSOR_X));
    a.ldy(abs(vdu::CURSOR_Y));
    a.jsr("read_character");
    a.tax();
    a.ldy(abs(vdu::MODE));
    a.rts();

    a.here("osbyte_vdu_variable");
    a.lda(abs(vdu::GRAPHICS_WINDOW + 1).x());
    a.tay();
    a.lda(abs(vdu::GRAPHICS_WINDOW).x());
    a.tax();
    a.rts();

    // *TV: the vertical shift and interlace the Model B's next MODE applies.
    if model == Model::ModelB {
        a.here("osbyte_tv");
        a.lda(abs(page2::TV_SHIFT));
        a.stx(abs(page2::TV_SHIFT));
        a.tax();
        a.lda(abs(page2::TV_INTERLACE));
        a.sty(abs(page2::TV_INTERLACE));
        a.tay();
        a.rts();
    }
}

/// Streams, buffers and the Escape condition.
fn streams(a: &mut Asm) {
    // OSBYTE 2: input from RS423 for X = 1, otherwise the keyboard; RS423's
    // receiver on for any X but 0. X returns the old input buffer.
    a.here("osbyte_input");
    a.lda(abs(var::INPUT_SOURCE));
    a.pha();
    a.jsr("rs423_input_select");
    a.pla();
    a.tax();
    a.rts();

    // OSBYTE 5: the printer's destination, once its driver has gone
    // dormant; UPTV is told the new type in X and the old in Y.
    a.here("osbyte_printer");
    a.bit(zp(os::ESCAPE));
    a.bmi("osbyte_printer_set");
    a.lda(abs(super::buffers::PRINTER_BUSY));
    a.bmi("osbyte_printer_set");
    a.jsr("interrupt_window");
    a.bpl("osbyte_printer");
    a.here("osbyte_printer_set");
    a.lda(imm(super::serial::upt::TYPE));
    a.ldx(zp(os::CALL_X));
    a.ldy(abs(var::PRINTER));
    a.jsr("printer_user_call");
    a.ldx(zp(os::CALL_X));
    a.lda(imm(0xF5));
    a.jmp("osbyte_write_variable");

    a.here("osbyte_disable_event");
    a.lda(imm(0));
    a.beq("osbyte_event");
    a.here("osbyte_enable_event");
    a.lda(imm(1));
    a.here("osbyte_event");
    a.cpx(imm(10));
    a.bcs(RTS);
    a.ldy(abs(page2::EVENTS).x());
    a.sta(abs(page2::EVENTS).x());
    a.tya();
    a.tax();
    a.rts();

    // OSBYTE 15: every buffer flushed, or with X non-zero the input buffer.
    a.here("osbyte_flush_input");
    a.txa();
    a.bne("osbyte_flush_current");
    a.jmp("buffers_flush_all");
    a.here("osbyte_flush_current");
    a.ldx(abs(var::INPUT_SOURCE));
    a.bpl("osbyte_flush_buffer");

    // OSBYTE 21, and 15's one buffer: Y 0 after, as MOS 1.20 leaves it.
    a.here("osbyte_flush");
    a.cpx(imm(super::buffers::number::COUNT));
    a.bcs(RTS);
    a.here("osbyte_flush_buffer");
    a.ldy(imm(0));
    a.jmp("buffer_flush");

    a.here("osbyte_clear_escape");
    a.lsr(zp(os::ESCAPE));
    a.rts();

    a.here("osbyte_set_escape");
    a.lda(imm(0x80));
    a.sta(zp(os::ESCAPE));
    a.rts();

    // &7E: X = &FF if an Escape was pending; the condition cleared and,
    // unless *FX230 says otherwise, its effects: the input flushed, sound
    // stopped, the soft key ended, the EXEC file closed.
    a.here("osbyte_acknowledge_escape");
    a.ldx(imm(0));
    a.bit(zp(os::ESCAPE));
    a.bpl("osbyte_acknowledged");
    a.lda(abs(var::ESCAPE_EFFECTS));
    a.bne("osbyte_escape_quiet");
    a.jsr("buffers_flush_all");
    a.jsr("close_exec");
    a.here("osbyte_escape_quiet");
    a.ldx(imm(0xFF));
    a.here("osbyte_acknowledged");
    a.lsr(zp(os::ESCAPE));
    a.rts();
    osbyte_insert(a);
}

/// The buffer calls: OSBYTE &8A and &99 insert, &91 removes and &98 examines.
fn osbyte_insert(a: &mut Asm) {
    a.here("osbyte_insert");
    a.tya();
    a.jmp("buffer_insert");

    a.here("osbyte_insert_input");
    a.tya();
    a.jmp("input_insert");

    a.here("osbyte_remove");
    a.jsr("buffer_remove");
    a.ldx(zp(os::CALL_X));
    a.rts();

    a.here("osbyte_examine");
    a.jsr("buffer_examine");
    a.ldx(zp(os::CALL_X));
    a.rts();

    a.here("osbyte_printer_dormant");
    a.ldx(imm(super::buffers::number::PRINTER));
    a.lda(imm(0xFF));
    a.sta(abs(super::buffers::PRINTER_BUSY));
    a.rts();

    a.here("osbyte_eof");
    a.lda(imm(1));
    a.fall_into("fsc_call");
    a.here("fsc_call");
    a.jmp(abs(vector::FSC).ind());

    a.here("osbyte_fast_bput");
    a.txa();
    a.jmp("osbput");
}

/// Timing, the ADC and fonts.
fn timing(a: &mut Asm, model: Model) {
    // OSBYTE 19: until the next VSYNC, which moves the cassette's timeout.
    a.here("osbyte_wait_vsync");
    a.lda(abs(var::CFS_TIMEOUT));
    a.here("osbyte_wait_vsync_loop");
    a.jsr("interrupt_window");
    a.cmp(abs(var::CFS_TIMEOUT));
    a.beq("osbyte_wait_vsync_loop");
    a.rts();

    if model == Model::ModelB {
        a.here("osbyte_16");
        a.lda(imm(0xBD));
        a.jsr("osbyte_write_variable");
        a.txa();
        a.pha();
        a.jsr("adc_restart");
        a.pla();
        a.tax();
        a.rts();
    }

    // &80: X 0 the fire buttons and the last channel; 1–4 a channel's value;
    // &F7–&FF a buffer's count.
    match model {
        Model::ModelB => {
            a.here("osbyte_adc_or_buffer");
            a.txa();
            a.bmi("osbyte_buffer_status");
            a.bne("osbyte_adc_channel");
            a.lda(abs(svia(reg::ORB)));
            a.eor(imm(0xFF));
            a.lsr(Acc);
            a.lsr(Acc);
            a.lsr(Acc);
            a.lsr(Acc);
            a.and(imm(3));
            a.tax();
            a.ldy(abs(page2::ADC_LAST));
            // C set, as MOS 1.20 leaves it.
            a.sec();
            a.rts();
        }
        Model::Electron => {
            // The ADC is the Plus 1's, so its calls are offered to the ROMs
            // first; with none taking it the readings in page 2 answer, and
            // no fire button is held.
            a.here("osbyte_electron_adc");
            a.txa();
            a.bmi("osbyte_buffer_status");
            a.lda(imm(crate::rom::os::service::OSBYTE));
            a.ldy(zp(os::CALL_Y));
            a.jsr("service_call");
            a.tax();
            a.beq("osbyte_electron_adc_claimed");
            a.ldx(zp(os::CALL_X));
            a.bne("osbyte_adc_channel");
            a.ldy(abs(page2::ADC_LAST));
            a.sec();
            a.rts();
            a.here("osbyte_electron_adc_claimed");
            a.ldx(zp(os::CALL_X));
            a.ldy(zp(os::CALL_Y));
            a.rts();

            // OSBYTE &16 and &17: the paged ROMs' polling semaphore counted
            // up and down.
            a.here("osbyte_poll_up");
            a.inc(abs(var::POLL_SEMAPHORE));
            a.rts();
            a.here("osbyte_poll_down");
            a.dec(abs(var::POLL_SEMAPHORE));
            a.rts();
        }
    }
    a.here("osbyte_adc_channel");
    a.cpx(imm(5));
    a.bcs(RTS);
    a.ldy(abs(page2::ADC_HIGH - 1).x());
    a.lda(abs(page2::ADC_LOW - 1).x());
    a.tax();
    a.rts();
    a.here("osbyte_buffer_status");
    a.eor(imm(0xFF));
    a.tax();
    a.cpx(imm(super::buffers::number::COUNT));
    a.bcs(RTS);
    // The input buffers count what they hold; the others what they have room
    // for.
    a.cpx(imm(2));
    a.jmp("buffer_count");

    if model == Model::ModelB {
        a.here("osbyte_receive_baud");
        a.lda(imm(0xC7));
        a.ldy(imm(3));
        a.bne("osbyte_baud");
        a.here("osbyte_transmit_baud");
        a.lda(imm(0xF8));
        a.ldy(imm(0));
        a.here("osbyte_baud");
        a.jmp("serial_baud");
    }
}

/// Input: INKEY.
fn input(a: &mut Asm, model: Model) {
    // &81: Y &FF with X zero reads the OS's version; with X's top bit set it
    // tests key X EOR &FF, and without it, as MOS 1.20 has it, is TRUE while
    // no key from key X EOR &7F on is down. Otherwise a character within
    // X + 256 Y centiseconds.
    a.here("osbyte_inkey");
    a.cpy(imm(0xFF));
    a.bne("osbyte_inkey_timed");
    // INKEY(-256): MOS 1.20's identity is &FF as a key held is, Electron OS
    // 1.00's 1.
    a.txa();
    match model {
        Model::ModelB => {
            a.beq("osbyte_inkey_down");
            // KEYV's scan from X, or test of key X & &7F, both TRUE with X's
            // top bit set on return.
            a.eor(imm(0x7F));
            a.tax();
            a.jsr("osbyte_keyboard_scan");
            a.txa();
        }
        Model::Electron => {
            a.beq("osbyte_inkey_identity");
            a.eor(imm(0xFF));
            a.tax();
            a.jsr("key_down");
        }
    }
    a.bpl("osbyte_inkey_up");
    // C, as X and Y, says the answer is TRUE.
    a.here("osbyte_inkey_down");
    a.ldx(imm(0xFF));
    a.ldy(imm(0xFF));
    a.sec();
    a.rts();
    a.here("osbyte_inkey_up");
    a.ldx(imm(0));
    a.ldy(imm(0));
    a.clc();
    a.rts();
    if model == Model::Electron {
        a.here("osbyte_inkey_identity");
        a.ldx(imm(1));
        a.ldy(imm(0));
        a.rts();
    }
    a.here("osbyte_inkey_timed");
    a.stx(abs(page2::INKEY_COUNT));
    a.sty(abs(page2::INKEY_COUNT + 1));
    // The count runs down in the interrupts `rdch_once` lets in.
    a.here("osbyte_inkey_wait");
    a.jsr("rdch_once");
    a.bvc("osbyte_inkey_got");
    a.lda(abs(page2::INKEY_COUNT));
    a.ora(abs(page2::INKEY_COUNT + 1));
    a.bne("osbyte_inkey_wait");
    a.ldy(imm(0xFF));
    a.sec();
    a.rts();
    a.here("osbyte_inkey_got");
    a.tax();
    a.ldy(imm(0));
    a.bcc(RTS);
    a.ldy(imm(0x1B));
    a.rts();
}

/// The machine: the Model B's video ULA and 6850, FRED, JIM and SHEILA,
/// ROMs and the filing system's calls.
fn machine(a: &mut Asm, model: Model) {
    if model == Model::ModelB {
        a.here("osbyte_ula_control");
        a.stx(abs(var::ULA_CONTROL));
        a.stx(abs(sheila::VIDEO_CONTROL));
        a.rts();

        // &9B: X is a logical colour and a physical, which the ULA takes
        // inverted, as the copy keeps it.
        a.here("osbyte_ula_palette");
        a.txa();
        a.eor(imm(0x07));
        a.sta(abs(var::ULA_PALETTE));
        a.sta(abs(sheila::VIDEO_PALETTE));
        a.rts();

        // &9C: the 6850's control register, as its copy, written through.
        a.here("osbyte_acia_control");
        a.lda(imm(0xC0));
        a.jsr("osbyte_variable");
        a.jmp("acia_control");
    }

    for (name, page, write) in [
        ("osbyte_read_fred", sheila::FRED, false),
        ("osbyte_write_fred", sheila::FRED, true),
        ("osbyte_read_jim", sheila::JIM, false),
        ("osbyte_write_jim", sheila::JIM, true),
        ("osbyte_read_sheila", sheila::SHEILA, false),
        ("osbyte_write_sheila", sheila::SHEILA, true),
    ] {
        a.here(name);
        if write {
            a.tya();
            a.sta(abs(page).x());
        } else {
            a.lda(abs(page).x());
            a.tay();
        }
        a.rts();
    }

    a.here("osbyte_service");
    a.txa();
    a.jsr("service_call");
    a.tax();
    a.rts();

    a.here("osbyte_language");
    a.lda(abs(page2::ROM_TYPES).x());
    a.and(imm(crate::rom::os::rom_type::LANGUAGE));
    a.beq("osbyte_not_language");
    a.jmp("enter_language");
    a.here("osbyte_not_language");
    a.error(0xF9, "This is not a language");

    a.here("osbyte_code");
    a.lda(imm(0));
    a.fall_into("user_vector");
    a.here("user_vector");
    a.jmp(abs(vector::USER).ind());

    a.here("osbyte_opt");
    a.lda(imm(0));
    a.jmp("fsc_call");
}

fn tables(a: &mut Asm, model: Model) {
    a.later(move |a| {
        a.here("osbyte_low_table");
        for name in &routines(model) {
            a.byte(lo(crate::rom::asm::sym(name) + -1));
        }
    });
    a.later(move |a| {
        a.here("osbyte_high_table");
        for name in &routines(model) {
            a.byte(hi(crate::rom::asm::sym(name) + -1));
        }
    });
}
