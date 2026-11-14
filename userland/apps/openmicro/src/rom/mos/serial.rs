//! OSWRCH's output streams, the printer drivers, and RS423 — over the Model
//! B's 6850, and on the Electron an expansion's.
//!
//! A character goes to RS423, the printer, the VDU drivers and the spool
//! file as OSBYTE 3 selects. Through the VDU drivers the printer takes the
//! characters they would display and codes 8–13, never a control code's
//! parameters; OSBYTE 3's independent printer takes every character. Each
//! printer type but the sink buffers its characters; the Model B's parallel
//! printer is driven by its acknowledge interrupt, the serial one by the
//! 6850's transmitter, and the user's through UPTV. On the Electron an
//! expansion drives the parallel printer and RS423, told of each by a
//! service call as its buffer fills; with none to take them they are
//! purged.

use super::buffers::{number, PRINTER_BUSY};
use super::vdu::status;
use super::ws::{os, var, vdu_zp, vector};
use super::{uvia, DELETE};
use crate::cpu::STACK_PAGE;
use crate::machine::bbc::acia::{control, status as acia_status};
use crate::machine::bbc::serial;
use crate::machine::bbc::sheila;
use crate::machine::bbc::via::{irq, reg};
use crate::machine::Model;
use crate::rom::asm::{abs, imm, zp, Acc, Asm, RTS};
use crate::rom::os::{printer, service};

/// OSBYTE 3's bits.
mod stream {
    pub const RS423: u8 = 0x01;
    pub const NO_VDU: u8 = 0x02;
    pub const NO_PRINTER: u8 = 0x04;
    pub const PRINTER_EVERYTHING: u8 = 0x08;
    pub const NO_SPOOL: u8 = 0x10;
    pub const PRINTER_VDU_1_ONLY: u8 = 0x40;
}

/// UPTV's reasons, A on entry; X is the printer buffer and Y the printer
/// type, but for a change of type, where X is the new type.
pub mod upt {
    pub const POLL: u8 = 0;
    pub const WAKE: u8 = 1;
    pub const VDU_2: u8 = 2;
    pub const VDU_3: u8 = 3;
    pub const TYPE: u8 = 5;
    // Loading it branches as nonzero.
    const _: () = assert!(WAKE != 0);
}

/// The rates OSBYTE 7 and 8 select, X = 0 to 8.
const RATE_CODES: u8 = 9;
/// The serial ULA's field for each.
const RATE_FIELDS: [u8; RATE_CODES as usize] =
    rate_fields([9_600, 75, 150, 300, 1_200, 2_400, 4_800, 9_600, 19_200]);

/// Each rate's place in the ULA's own table; a rate it has none for fails
/// the build.
const fn rate_fields(rates: [u64; RATE_CODES as usize]) -> [u8; RATE_CODES as usize] {
    let mut fields = [0; RATE_CODES as usize];
    let mut i = 0;
    while i < rates.len() {
        let mut field: u8 = 0;
        while serial::BAUD[field as usize] != rates[i] {
            field += 1;
        }
        fields[i] = field;
        i += 1;
    }
    fields
}

/// The User VIA's PCR strobing the printer: CA2 driven low, then high.
const PCR_CA2: u8 = 0x0E;
const PCR_CA2_LOW: u8 = 0x0C;

pub fn emit(a: &mut Asm, model: Model) {
    oswrch(a);
    printers(a, model);
    match model {
        Model::ModelB => {
            rs423(a);
            rs423_interrupts(a);
            setup(a);
            a.later(tables);
        }
        Model::Electron => electron_rs423(a),
    }
}

/// The Electron's RS423: its bytes for an expansion, and OSBYTE 2's input.
fn electron_rs423(a: &mut Asm) {
    a.here("rs423_put");
    a.ldx(imm(number::RS423_OUTPUT));
    a.here("rs423_put_try");
    a.jsr("buffer_insert");
    a.bcc(RTS);
    a.jsr("interrupt_window");
    a.bcs("rs423_put_try");

    // X is kept.
    a.here("rs423_wake");
    a.lda(imm(service::RS423));
    a.ldy(imm(number::RS423_OUTPUT));
    a.jmp("expansion_wake");

    // OSBYTE 2: input from RS423 only for X = 1.
    a.here("rs423_input_select");
    a.lda(imm(0));
    a.cpx(imm(1));
    a.bne("rs423_input_keyboard");
    a.lda(imm(number::RS423_INPUT));
    a.here("rs423_input_keyboard");
    a.sta(abs(var::INPUT_SOURCE));
    a.rts();
}

/// The character OSWRCH was given, from a routine it calls directly.
fn character(a: &mut Asm) {
    a.tsx();
    a.lda(abs(STACK_PAGE + 5).x());
}

/// WRCHV's default: the character to each output stream in turn.
pub(super) fn oswrch_default(a: &mut Asm) {
    a.here("oswrch_default");
    a.here("nvwrch");
    a.pha();
    a.txa();
    a.pha();
    a.tya();
    a.pha();
    a.jsr("wrch_rs423");
    a.jsr("wrch_printer");
    a.jsr("wrch_vdu");
    a.jsr("wrch_spool");
    a.pla();
    a.tay();
    a.pla();
    a.tax();
    a.pla();
    a.rts();
}

fn oswrch(a: &mut Asm) {
    a.here("wrch_rs423");
    a.lda(abs(var::OUTPUT_STREAMS));
    a.and(imm(stream::RS423));
    a.beq(RTS);
    character(a);
    a.jmp("rs423_put");

    a.here("wrch_vdu");
    a.lda(abs(var::OUTPUT_STREAMS));
    a.and(imm(stream::NO_VDU));
    a.bne(RTS);
    character(a);
    a.jmp("vdu_write");

    a.here("wrch_printer");
    a.lda(abs(var::OUTPUT_STREAMS));
    a.and(imm(stream::NO_PRINTER | stream::PRINTER_VDU_1_ONLY));
    a.bne(RTS);
    a.lda(abs(var::OUTPUT_STREAMS));
    a.and(imm(stream::PRINTER_EVERYTHING));
    a.bne("wrch_printer_take");
    // Through the VDU drivers: what they display, and codes 8–13.
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(status::PRINTER));
    a.beq(RTS);
    a.lda(abs(var::OUTPUT_STREAMS));
    a.and(imm(stream::NO_VDU));
    a.bne(RTS);
    a.lda(abs(var::VDU_QUEUE));
    a.bne(RTS);
    character(a);
    a.cmp(imm(DELETE));
    a.beq(RTS);
    a.cmp(imm(b' '));
    a.bcs("wrch_printer_check");
    a.cmp(imm(0x08));
    a.bcc(RTS);
    a.cmp(imm(0x0E));
    a.bcs(RTS);
    a.here("wrch_printer_take");
    character(a);
    a.here("wrch_printer_check");
    a.cmp(abs(var::PRINTER_IGNORE));
    a.beq(RTS);
    a.bne("printer_put");

    // The handle is clear while the byte is written, so an error writing it
    // closes the spool rather than recurring.
    a.here("wrch_spool");
    a.lda(abs(var::OUTPUT_STREAMS));
    a.and(imm(stream::NO_SPOOL));
    a.bne(RTS);
    a.ldy(abs(var::SPOOL_HANDLE));
    a.beq(RTS);
    a.lda(imm(0));
    a.sta(abs(var::SPOOL_HANDLE));
    character(a);
    a.jsr("osbput");
    a.sty(abs(var::SPOOL_HANDLE));
    a.rts();
}

fn printers(a: &mut Asm, model: Model) {
    // A into the printer buffer, waiting for room; the sink takes nothing.
    a.here("printer_put");
    a.ldx(abs(var::PRINTER));
    a.beq(RTS);
    a.ldx(imm(number::PRINTER));
    a.here("printer_put_try");
    a.jsr("buffer_insert");
    a.bcc(RTS);
    a.jsr("interrupt_window");
    a.bcs("printer_put_try");

    // VDU 1's parameter: printed raw, while VDU 2 has the printer on.
    a.here("printer_send");
    a.pha();
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(status::PRINTER));
    a.beq("printer_send_none");
    a.lda(abs(var::OUTPUT_STREAMS));
    a.and(imm(stream::NO_PRINTER));
    a.bne("printer_send_none");
    a.pla();
    a.jmp("printer_put");
    a.here("printer_send_none");
    a.pla();
    a.rts();

    // The printer buffer's first character since its driver went dormant.
    a.here("printer_wake");
    a.lsr(abs(PRINTER_BUSY));
    a.lda(abs(var::PRINTER));
    a.cmp(imm(printer::SERIAL));
    a.beq("rs423_wake");
    a.cmp(imm(printer::PARALLEL));
    a.beq("printer_parallel_wake");
    a.lda(imm(upt::WAKE));
    a.bne("printer_user");

    if model == Model::Electron {
        a.here("printer_parallel_wake");
        a.lda(imm(service::PRINTER));
        a.ldy(imm(number::PRINTER));
        // Service call A for the expansion whose buffer Y has a character;
        // with none to take it the buffer is purged, its driver dormant. X
        // is kept.
        a.here("expansion_wake");
        a.pha();
        a.txa();
        a.pha();
        a.tya();
        a.pha();
        a.tsx();
        a.lda(abs(STACK_PAGE + 3).x());
        a.jsr("service_call");
        a.tax();
        a.pla();
        a.cpx(imm(0));
        a.beq("expansion_taken");
        a.tax();
        a.jsr("buffer_flush");
        a.here("expansion_taken");
        a.pla();
        a.tax();
        a.pla();
        a.rts();
    } else {
        model_b_parallel(a);
    }

    // Each centisecond a user printer with characters is given its turn.
    a.here("printer_poll");
    a.bit(abs(PRINTER_BUSY));
    a.bmi(RTS);
    a.lda(abs(var::PRINTER));
    a.cmp(imm(printer::SERIAL + 1));
    a.bcc(RTS);
    a.lda(imm(upt::POLL));

    // UPTV with reason A.
    a.here("printer_user");
    a.ldx(imm(number::PRINTER));
    a.ldy(abs(var::PRINTER));
    a.here("printer_user_call");
    a.jmp(abs(vector::UPT).ind());
}

/// The Model B's parallel printer, on the User VIA: each character strobed
/// out as the last is acknowledged.
fn model_b_parallel(a: &mut Asm) {
    a.here("printer_parallel_wake");
    a.lda(imm(irq::ANY | irq::CA1));
    a.sta(abs(uvia(reg::IER)));
    // The next character onto the port, strobed; none left, and the driver
    // sleeps until the next arrives.
    a.here("printer_next");
    a.txa();
    a.pha();
    a.ldx(imm(number::PRINTER));
    a.jsr("buffer_remove");
    a.bcs("printer_parallel_dormant");
    a.sta(abs(uvia(reg::ORA)));
    a.lda(abs(uvia(reg::PCR)));
    a.and(imm(!PCR_CA2));
    a.ora(imm(PCR_CA2_LOW));
    a.sta(abs(uvia(reg::PCR)));
    a.ora(imm(PCR_CA2));
    a.sta(abs(uvia(reg::PCR)));
    a.pla();
    a.tax();
    a.rts();
    a.here("printer_parallel_dormant");
    a.lda(imm(irq::CA1));
    a.sta(abs(uvia(reg::IFR)));
    a.sta(abs(uvia(reg::IER)));
    a.lda(imm(0xFF));
    a.sta(abs(PRINTER_BUSY));
    a.pla();
    a.tax();
    a.rts();

    // The acknowledge: the parallel printer took its character.
    a.here("printer_interrupt");
    a.lda(abs(var::PRINTER));
    a.cmp(imm(printer::PARALLEL));
    a.beq("printer_next");
    a.lda(imm(irq::CA1));
    a.sta(abs(uvia(reg::IFR)));
    a.sta(abs(uvia(reg::IER)));
    a.rts();
}

/// RS423's half of the 6850: its output and input buffers, and the
/// transmitter control bits, kept in the OSBYTE &9C copy and written to the
/// chip only while RS423 has it: the serial ULA set for RS423 and the
/// cassette filing system not using it.
fn rs423(a: &mut Asm) {
    // A into the RS423 output buffer, waiting for room.
    a.here("rs423_put");
    a.ldx(imm(number::RS423_OUTPUT));
    a.here("rs423_put_try");
    a.jsr("buffer_insert");
    a.bcc(RTS);
    a.jsr("interrupt_window");
    a.bcs("rs423_put_try");

    // Something to send: the transmitter interrupt on, unless the input
    // handshake holds RTS high. X is kept.
    a.here("rs423_wake");
    a.txa();
    a.pha();
    a.jsr("rs423_room");
    a.lda(imm(control::TRANSMIT_INTERRUPT));
    a.bcs("rs423_wake_set");
    a.lda(imm(control::NOT_READY));
    a.here("rs423_wake_set");
    a.jsr("rs423_transmitter");
    a.pla();
    a.tax();
    a.rts();

    // RS423's input changed: RTS high while it has less room than OSBYTE
    // &CB's extent, and low again once it has. X is kept.
    a.here("rs423_lines");
    a.txa();
    a.pha();
    a.lda(abs(var::ACIA_CONTROL));
    a.bpl("rs423_lines_done");
    a.jsr("rs423_room");
    a.bcs("rs423_lines_room");
    a.lda(imm(control::NOT_READY));
    a.jsr("rs423_transmitter");
    a.jmp("rs423_lines_done");
    a.here("rs423_lines_room");
    a.lda(abs(var::ACIA_CONTROL));
    a.and(imm(control::TRANSMIT));
    a.cmp(imm(control::NOT_READY));
    a.bne("rs423_lines_done");
    a.jsr("rs423_pending");
    a.lda(imm(control::TRANSMIT_INTERRUPT));
    a.bcs("rs423_lines_set");
    a.lda(imm(control::READY));
    a.here("rs423_lines_set");
    a.jsr("rs423_transmitter");
    a.here("rs423_lines_done");
    a.pla();
    a.tax();
    a.rts();

    // C set if RS423's input may take more: it is not receiving, or it has
    // OSBYTE &CB's extent of room.
    a.here("rs423_room");
    a.lda(abs(var::ACIA_CONTROL));
    a.bmi("rs423_room_count");
    a.sec();
    a.rts();
    a.here("rs423_room_count");
    a.ldx(imm(number::RS423_INPUT));
    a.sec();
    a.jsr("buffer_count");
    a.cpx(abs(var::RS423_HANDSHAKE));
    a.rts();

    // C set if RS423's output, or an active serial printer, has characters.
    a.here("rs423_pending");
    a.ldx(imm(number::RS423_OUTPUT));
    a.clc();
    a.jsr("buffer_count");
    a.cpx(imm(1));
    a.bcs(RTS);
    a.lda(abs(var::PRINTER));
    a.cmp(imm(printer::SERIAL));
    a.clc();
    a.bne(RTS);
    a.lda(abs(PRINTER_BUSY));
    a.eor(imm(0x80));
    a.asl(Acc);
    a.rts();
    rs423_transmitter(a);
}

/// The transmitter control bits A into the copy, and the chip if RS423
/// has it.
fn rs423_transmitter(a: &mut Asm) {
    a.here("rs423_transmitter");
    a.php();
    a.sei();
    a.sta(zp(os::SCRATCH));
    a.lda(abs(var::ACIA_CONTROL));
    a.and(imm(!control::TRANSMIT));
    a.ora(zp(os::SCRATCH));
    a.cmp(abs(var::ACIA_CONTROL));
    a.beq("rs423_transmitter_same");
    a.sta(abs(var::ACIA_CONTROL));
    a.jsr("rs423_chip");
    a.here("rs423_transmitter_same");
    a.plp();
    a.rts();

    a.here("rs423_chip");
    a.bit(zp(os::CFS_CRITICAL));
    a.bmi(RTS);
    a.lda(abs(var::SERIAL_ULA));
    a.and(imm(serial::RS423));
    a.beq(RTS);
    // The 6850's control register from its copy at &250.
    a.here("acia_control");
    a.lda(abs(var::ACIA_CONTROL));
    a.sta(abs(sheila::ACIA_CONTROL));
    a.rts();

    // OSBYTE 2: input from RS423 only for X = 1; its receiver on for any X
    // but 0, with RTS lowered.
    a.here("rs423_input_select");
    a.lda(imm(0));
    a.cpx(imm(1));
    a.bne("rs423_input_keyboard");
    a.lda(imm(number::RS423_INPUT));
    a.here("rs423_input_keyboard");
    a.sta(abs(var::INPUT_SOURCE));
    a.lda(abs(var::ACIA_CONTROL));
    a.and(imm(!control::RECEIVE_INTERRUPT));
    a.cpx(imm(0));
    a.beq("rs423_input_set");
    a.ora(imm(control::RECEIVE_INTERRUPT));
    a.here("rs423_input_set");
    a.sta(abs(var::ACIA_CONTROL));
    a.jsr("rs423_chip");
    a.cpx(imm(0));
    a.beq(RTS);
    a.jmp("rs423_wake");
}

fn rs423_interrupts(a: &mut Asm) {
    // The 6850 interrupted: the cassette's while the filing system holds
    // it, otherwise RS423's.
    a.here("acia_interrupt");
    a.bit(zp(os::CFS_CRITICAL));
    a.bpl("rs423_interrupt");
    a.jmp("cfs_interrupt");
    a.here("rs423_interrupt");
    a.lda(abs(sheila::ACIA_CONTROL));
    a.pha();
    a.and(imm(acia_status::RDRF));
    a.beq("rs423_no_receive");
    a.jsr("rs423_receive");
    a.here("rs423_no_receive");
    a.pla();
    a.and(imm(acia_status::TDRE));
    a.beq(RTS);
    a.lda(abs(var::ACIA_CONTROL));
    a.and(imm(control::TRANSMIT));
    a.cmp(imm(control::TRANSMIT_INTERRUPT));
    a.bne(RTS);
    // An active serial printer goes first; with nothing to send the
    // transmitter interrupt goes off, RTS left as receiving needs it.
    a.lda(abs(var::PRINTER));
    a.cmp(imm(printer::SERIAL));
    a.bne("rs423_send_output");
    a.bit(abs(PRINTER_BUSY));
    a.bmi("rs423_send_output");
    a.ldx(imm(number::PRINTER));
    a.jsr("buffer_remove");
    a.bcc("rs423_send");
    a.lda(imm(0xFF));
    a.sta(abs(PRINTER_BUSY));
    a.here("rs423_send_output");
    a.ldx(imm(number::RS423_OUTPUT));
    a.jsr("buffer_remove");
    a.bcs("rs423_idle");
    a.here("rs423_send");
    a.sta(abs(sheila::ACIA_DATA));
    a.rts();
    a.here("rs423_idle");
    a.lda(imm(control::READY));
    a.bit(abs(var::ACIA_CONTROL));
    a.bmi("rs423_idle_set");
    a.lda(imm(control::NOT_READY));
    a.here("rs423_idle_set");
    a.jmp("rs423_transmitter");

    // A received byte, unless OSBYTE &CC suppresses RS423's input; with
    // OSBYTE &B5 zero it is input like the keyboard's, Escape and all.
    a.here("rs423_receive");
    a.lda(abs(sheila::ACIA_DATA));
    a.ldy(abs(var::RS423_SUPPRESS));
    a.bne(RTS);
    a.ldx(imm(number::RS423_INPUT));
    a.ldy(abs(var::RS423_MODE));
    a.bne("rs423_receive_raw");
    a.jsr("input_insert");
    a.jmp("rs423_lines");
    a.here("rs423_receive_raw");
    a.jsr("buffer_insert");
    a.jmp("rs423_lines");
}

fn setup(a: &mut Asm) {
    a.here("serial_init");
    a.lda(imm(control::MASTER_RESET));
    a.sta(abs(sheila::ACIA_CONTROL));
    // The serial ULA as set, then the 6850's control, which ends its reset.
    a.here("serial_restore");
    a.lda(abs(var::SERIAL_ULA));
    a.sta(abs(sheila::SERIAL_ULA));
    a.jmp("acia_control");

    // OSBYTE 7 and 8: rate X into the field A keeps the rest of, Y the
    // field's shift; X and Y return the serial ULA's old setting.
    a.here("serial_baud");
    a.cpx(imm(RATE_CODES));
    a.bcs(RTS);
    a.and(abs(var::SERIAL_ULA));
    a.pha();
    a.lda(abs("serial_rates").x());
    a.cpy(imm(0));
    a.beq("serial_baud_field");
    for _ in 0..serial::RECEIVE_SHIFT {
        a.asl(Acc);
    }
    a.here("serial_baud_field");
    a.tsx();
    a.ora(abs(STACK_PAGE + 1).x());
    a.tax();
    a.pla();
    a.ldy(abs(var::SERIAL_ULA));
    a.stx(abs(var::SERIAL_ULA));
    a.stx(abs(sheila::SERIAL_ULA));
    a.tya();
    a.tax();
    a.rts();
}

fn tables(a: &mut Asm) {
    a.here("serial_rates");
    a.bytes(&RATE_FIELDS);
}
