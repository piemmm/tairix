//! The serial bus, the KERNAL as its controller: commands under ATN, bytes
//! sent as the talker with CIOUT holding one back so the last goes with EOI,
//! bytes received as the listener with ACPTR, and the turnaround TKSA makes,
//! timed as the 1541 User's Guide and the Programmer's Reference Guide give
//! the protocol.
//!
//! Each line is open collector: CIA2's port drives it low through an
//! inverter, and reads it as it stands. A bit is DATA high for 1, valid
//! while CLOCK is released.

use super::files::status;
use super::ws::{page, zp};
use crate::machine::c64::cia::reg;
use crate::machine::c64::serial::command;
use crate::machine::c64::{io, serial};
use crate::rom::asm::{abs, imm, zp as z, Acc, Asm};

/// CIA2's port A, where the bus is.
const PORT: u16 = io::cia2(reg::PRA);

/// Waits counted in passes of a polling loop of about eleven cycles: a
/// millisecond for a listener to answer ATN or take a byte, and the 200
/// microseconds a talker leaves CLOCK released to signal EOI.
const ANSWER_PASSES: u8 = 100;
const EOI_PASSES: u8 = 20;
/// Delays counted in passes of five cycles: a bit set up on DATA, held
/// valid, and the listener's acknowledge of EOI.
const BIT_SETUP: u8 = 10;
const BIT_VALID: u8 = 6;
const EOI_HOLD: u8 = 16;
/// Passes of five cycles after ATN's release: the quarter millisecond a
/// device it did not address takes to let DATA go, a 1541 about half that,
/// so a listener's DATA is not mistaken for a device's acknowledging ATN.
const LET_GO_PASSES: u8 = 50;
const _: () = assert!(
    LET_GO_PASSES != 0,
    "release_atn branches on its count into the wait it shares"
);

pub fn emit(a: &mut Asm) {
    lines(a);
    commands(a);
    send(a);
    receive(a);
}

/// Each line pulled low or released, the port's other lines kept: an
/// entry loads its line's bit and reads past the later entries' loads, so
/// A and V are clobbered and the write comes up to eleven cycles later
/// than a read of the port's would, inside handshakes that wait hundreds.
fn lines(a: &mut Asm) {
    let store = a.local();
    for (names, low) in [
        (["atn_low", "clock_low", "data_low"], true),
        (["atn_high", "clock_high", "data_high"], false),
    ] {
        let [first, rest @ ..] = names;
        a.here(first);
        a.lda(imm(serial::ATN_OUT));
        for (name, bit) in rest.into_iter().zip([serial::CLOCK_OUT, serial::DATA_OUT]) {
            a.skip_over(|a| {
                a.here(name);
                a.lda(imm(bit));
            });
        }
        if low {
            a.ora(abs(PORT));
            // A line's bit is set: not zero.
            a.bne(store);
        } else {
            a.eor(imm(0xFF));
            a.and(abs(PORT));
            a.bind(store);
            a.sta(abs(PORT));
            a.rts();
        }
    }

    a.here("set_status");
    a.ora(z(zp::STATUS));
    a.sta(z(zp::STATUS));
    a.rts();
}

/// LISTEN and TALK, the device in A; SECOND and TKSA, a channel's command
/// in A after them; UNLSN and UNTLK; and ATN released with no channel.
/// Each keeps X and Y, as the original's do.
fn commands(a: &mut Asm) {
    a.here("listen");
    a.ora(imm(command::LISTEN));
    a.skip_over(|a| {
        a.here("talk");
        a.ora(imm(command::TALK));
    });
    a.here("attention");
    let (clear, sent) = (a.local(), a.local());
    a.pha();
    a.bit(z(zp::C3PO));
    a.bpl(clear);
    a.sec();
    a.jsr("send_byte");
    a.lsr(z(zp::C3PO));
    a.bind(clear);
    a.sei();
    a.jsr("data_high");
    a.jsr("atn_low");
    a.jsr("clock_low");
    a.jsr("answer_time");
    a.pla();
    a.jsr("send_a");
    a.lda(z(zp::STATUS));
    a.bpl(sent);
    a.jsr("release_bus");
    a.bind(sent);
    a.cli();
    a.rts();

    a.here("second");
    a.jsr("send_a");
    a.here("release_atn");
    a.jsr("atn_high");
    let waits = a.local();
    a.txa();
    a.pha();
    a.ldx(imm(LET_GO_PASSES));
    a.bne(waits);
    // The millisecond devices have to answer ATN: 256 passes.
    a.here("answer_time");
    a.txa();
    a.pha();
    a.ldx(imm(0));
    a.bind(waits);
    a.jsr("delay");
    a.pla();
    a.tax();
    a.rts();
    // X passes of five cycles; A and Y kept.
    a.here("delay");
    let pass = a.here_local();
    a.dex();
    a.bne(pass);
    a.rts();

    a.here("tksa");
    a.jsr("send_a");
    a.fall_into("turnaround");

    // The controller becomes the listener: it holds DATA, lets ATN and
    // CLOCK go, and waits for the device to take CLOCK as the talker.
    a.here("turnaround");
    let (taken, waiting) = (a.local(), a.local());
    a.txa();
    a.pha();
    a.sei();
    a.jsr("data_low");
    a.jsr("atn_high");
    a.jsr("clock_high");
    a.ldx(imm(ANSWER_PASSES));
    a.bind(waiting);
    a.bit(abs(PORT));
    a.bvc(taken);
    a.dex();
    a.bne(waiting);
    a.lda(imm(status::READ_TIMEOUT));
    a.jsr("set_status");
    a.bind(taken);
    a.pla();
    a.tax();
    a.cli();
    a.rts();

    a.here("unlsn");
    a.lda(imm(command::UNLISTEN));
    a.skip_over(|a| {
        a.here("untlk");
        a.lda(imm(command::UNTALK));
    });
    a.jsr("attention");
    a.here("release_bus");
    a.jsr("release_atn");
    a.jsr("clock_high");
    a.jmp("data_high");

    a.here("settmo");
    a.sta(abs(page::TIMOUT));
    a.rts();
}

/// The talker's side: BSOUR sent, EOI first with C set, or from `send_a`
/// A sent without. A listener must hold DATA low within a millisecond, or
/// the device is absent; it must take the byte within a millisecond, or
/// the write timed out. X and Y are kept. CIOUT holds each byte back until
/// the next, C3PO's bit 7 set while it does.
fn send(a: &mut Asm) {
    a.here("send_a");
    a.sta(z(zp::BSOUR));
    a.clc();
    a.here("send_byte");
    let (present, ready, eoi_ready, eoi_ack, bit, one, acknowledged, failed, done) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.ror(z(zp::TEMP));
    a.sei();
    a.txa();
    a.pha();
    a.jsr("data_high");
    a.ldx(imm(ANSWER_PASSES));
    a.bind(present);
    a.bit(abs(PORT));
    a.bpl(ready);
    a.dex();
    a.bne(present);
    a.lda(imm(status::ABSENT));
    a.bne(failed);
    a.bind(ready);
    a.jsr("clock_high");
    a.bit(z(zp::TEMP));
    let listening = a.local();
    a.bpl(listening);
    a.bind(eoi_ready);
    a.bit(abs(PORT));
    a.bpl(eoi_ready);
    a.bind(eoi_ack);
    a.bit(abs(PORT));
    a.bmi(eoi_ack);
    a.bind(listening);
    let wait_ready = a.here_local();
    a.bit(abs(PORT));
    a.bpl(wait_ready);
    a.jsr("clock_low");
    a.lda(imm(8));
    a.sta(z(zp::TEMP + 1));
    a.bind(bit);
    a.lda(abs(PORT));
    a.and(imm(!serial::DATA_OUT));
    a.lsr(z(zp::BSOUR));
    a.bcs(one);
    a.ora(imm(serial::DATA_OUT));
    a.bind(one);
    a.sta(abs(PORT));
    a.ldx(imm(BIT_SETUP));
    a.jsr("delay");
    a.and(imm(!serial::CLOCK_OUT));
    a.sta(abs(PORT));
    a.ldx(imm(BIT_VALID));
    a.jsr("delay");
    a.ora(imm(serial::CLOCK_OUT));
    a.sta(abs(PORT));
    a.dec(z(zp::TEMP + 1));
    a.bne(bit);
    a.jsr("data_high");
    a.ldx(imm(ANSWER_PASSES));
    a.bind(acknowledged);
    a.bit(abs(PORT));
    a.bpl(done);
    a.dex();
    a.bne(acknowledged);
    a.lda(imm(status::WRITE_TIMEOUT));
    a.bind(failed);
    a.jsr("set_status");
    a.bind(done);
    a.pla();
    a.tax();
    a.cli();
    a.rts();

    a.here("ciout");
    let first = a.local();
    a.bit(z(zp::C3PO));
    a.bpl(first);
    a.pha();
    a.clc();
    a.jsr("send_byte");
    a.pla();
    a.bind(first);
    a.sec();
    a.ror(z(zp::C3PO));
    a.sta(z(zp::BSOUR));
    a.clc();
    a.rts();
}

/// ACPTR, the listener's side: the talker's ready to send waited for,
/// DATA released for the byte, and the bits read while CLOCK is released.
/// A talker that leaves CLOCK released 200 microseconds is sending its
/// last byte, which STATUS records and DATA held low acknowledges; one
/// that does so twice has nothing to send, and the byte returned is the
/// port's. Y is kept.
fn receive(a: &mut Asm) {
    a.here("acptr");
    let (ready, data_ready, watch, gone, bits, valid, next, received) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.sei();
    a.lda(imm(0));
    a.sta(z(zp::TEMP));
    a.jsr("clock_high");
    a.bind(ready);
    a.bit(abs(PORT));
    a.bvc(ready);
    a.bind(data_ready);
    a.jsr("data_high");
    a.ldx(imm(EOI_PASSES));
    a.bind(watch);
    a.bit(abs(PORT));
    a.bvc(bits);
    a.dex();
    a.bne(watch);
    a.lda(z(zp::TEMP));
    a.bne(gone);
    a.lda(imm(status::END));
    a.jsr("set_status");
    a.jsr("data_low");
    a.ldx(imm(EOI_HOLD));
    a.jsr("delay");
    a.inc(z(zp::TEMP));
    a.bne(data_ready);
    // A byte nothing sent reads as the port stands, as the original's does.
    a.bind(gone);
    a.lda(imm(status::READ_TIMEOUT));
    a.jsr("set_status");
    a.lda(abs(PORT));
    a.sta(z(zp::TEMP + 1));
    a.jmp(received);
    a.bind(bits);
    a.ldx(imm(8));
    a.bind(valid);
    a.bit(abs(PORT));
    a.bvc(valid);
    a.lda(abs(PORT));
    a.asl(Acc);
    a.ror(z(zp::TEMP + 1));
    a.bind(next);
    a.bit(abs(PORT));
    a.bvs(next);
    a.dex();
    a.bne(valid);
    a.bind(received);
    a.jsr("data_low");
    a.lda(z(zp::TEMP + 1));
    a.cli();
    a.clc();
    a.rts();
}
