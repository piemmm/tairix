//! The clean-room Plus 1 ROM: a service ROM for slot 12 driving the Plus
//! 1's parallel printer and ADC from the Electron's 100 Hz poll, written to
//! the interface the Electron Advanced User Guide gives the Plus 1
//! (`plans/BEEB.md` §3).
//!
//! It claims no workspace, so the byte the OS keeps for its private page
//! holds its flags. Each reset counts it into the poll and starts a
//! conversion; *FX163,128,1 turns both ports off and counts it out, and
//! *FX163,128,0 turns them on again. An OSBYTE it makes inside another call
//! or the poll keeps that call's A, X and Y at &EF–&F1.

use alloc::format;

use crate::cpu::STACK_PAGE;
use crate::machine::electron::plus1::{self as port, command, status};
use crate::machine::{FIRE_BUTTONS, ROM_SIZE, SIDEWAYS_BASE};
use crate::rom::asm::{abs, imm, zp, Acc, Asm, AsmError, Image, RTS, V_SET};
use crate::rom::os::{
    buffer, entry, event, osbyte, page2, page_d, printer, service, var, vector, zp as os,
    PRINTER_BUSY,
};

/// The ROM's version byte.
const VERSION: u8 = 1;
/// The title and the version string.
const TITLE: &str = "OpenMOS Plus 1";
const VERSION_TEXT: &str = "1.00";

/// The flags byte's bit set while both ports are off.
const OFF: u8 = 0x80;
/// *FX163's X naming the Plus 1, and its Y turning the ports on and off.
const FX163_PLUS1: u8 = 0x80;
const FX163_ON: u8 = 0;
const FX163_OFF: u8 = 1;
// The on setting is tested as zero.
const _: () = assert!(FX163_ON == 0);
/// The most characters one poll sends, so that it holds interrupts off for
/// well under a millisecond.
const PER_POLL: u8 = 8;
// BIT reads the printer's busy line into N and a conversion's into V.
const _: () = assert!(status::BUSY == 0x80 && status::CONVERTING == 0x40);
const CR: u8 = 0x0D;

/// Build the Plus 1 ROM's image.
///
/// # Errors
///
/// The builder's first [`AsmError`].
pub fn build() -> Result<Image, AsmError> {
    let mut a = Asm::new(SIDEWAYS_BASE, ROM_SIZE);
    crate::rom::service_header(&mut a, VERSION, TITLE, VERSION_TEXT);
    dispatch(&mut a);
    reset_and_poll(&mut a);
    calls(&mut a);
    switching(&mut a);
    adc(&mut a);
    printer_driver(&mut a);
    help(&mut a);
    a.finish()
}

/// The service entry: each call the Plus 1 answers, A its number, returned
/// zero once claimed.
fn dispatch(a: &mut Asm) {
    a.here("service");
    for (number, handler) in [
        (service::ABSOLUTE_WORKSPACE, "service_reset"),
        (service::OSBYTE, "service_osbyte"),
        (service::HELP, "service_help"),
        (service::PRINTER, "service_printer"),
        (service::POLL, "service_poll"),
    ] {
        a.cmp(imm(number));
        let other = a.local();
        a.bne(other);
        a.jmp(handler);
        a.bind(other);
    }
    a.rts();

    // A the flags, N set while the ports are off; X this ROM.
    a.here("flags");
    a.ldx(zp(os::ROM));
    a.lda(abs(page_d::PRIVATE_WORKSPACE).x());
    a.rts();
}

/// Every reset, the first call it is offered; and the poll, Y the ROMs
/// still to be polled, claimed by the last of them.
fn reset_and_poll(a: &mut Asm) {
    a.here("service_reset");
    a.tya();
    a.pha();
    a.jsr("flags");
    let off = a.local();
    a.bmi(off);
    a.jsr("ports_on");
    a.bind(off);
    a.pla();
    a.tay();
    a.lda(imm(service::ABSOLUTE_WORKSPACE));
    a.rts();

    a.here("service_poll");
    a.jsr("flags");
    let pass = a.local();
    a.bmi(pass);
    a.tya();
    a.pha();
    a.jsr("adc_poll");
    a.jsr("printer_poll");
    a.pla();
    a.tay();
    a.dey();
    a.bne(pass);
    a.lda(imm(0));
    a.rts();
    a.bind(pass);
    a.lda(imm(service::POLL));
    a.rts();

    // A character for the parallel printer, its driver dormant until now.
    a.here("service_printer");
    a.jsr("flags");
    let pass = a.local();
    a.bmi(pass);
    a.lda(abs(var::PRINTER));
    a.cmp(imm(printer::PARALLEL));
    a.bne(pass);
    a.jsr("printer_send");
    a.lda(imm(0));
    a.rts();
    a.bind(pass);
    a.lda(imm(service::PRINTER));
    a.rts();
}

/// The OSBYTE calls the Plus 1 answers: *FX163,128 always, and while its
/// ports are on the ADC's selection, a forced conversion and ADVAL(0).
fn calls(a: &mut Asm) {
    a.here("service_osbyte");
    a.lda(zp(os::CALL_A));
    a.cmp(imm(osbyte::APPLICATION));
    let others = a.local();
    a.bne(others);
    a.jmp("fx163");
    a.bind(others);
    a.jsr("flags");
    a.bmi("osbyte_pass");
    a.lda(zp(os::CALL_A));
    for (number, routine) in [
        (osbyte::SELECT_ADC, "fx16"),
        (osbyte::FORCE_ADC, "fx17"),
        (osbyte::ADVAL, "fx128"),
    ] {
        a.cmp(imm(number));
        let other = a.local();
        a.bne(other);
        a.jmp(routine);
        a.bind(other);
    }
    a.here("osbyte_pass");
    a.lda(imm(service::OSBYTE));
    a.rts();
    a.here("osbyte_claimed");
    a.lda(imm(0));
    a.rts();

    // X channels converted in turn from the highest, none for 0; X the
    // number before.
    a.here("fx16");
    a.php();
    a.sei();
    a.ldy(abs(var::ADC_MAXIMUM));
    a.lda(zp(os::CALL_X));
    a.sta(abs(var::ADC_MAXIMUM));
    a.sta(abs(var::ADC_CHANNEL));
    a.sty(zp(os::CALL_X));
    a.jsr("adc_start");
    a.plp();
    a.jmp("osbyte_claimed");

    // Channel X converted next, ADVAL(0) naming no channel until it is.
    a.here("fx17");
    a.ldx(zp(os::CALL_X));
    a.beq("osbyte_claimed");
    a.cpx(imm(page2::ADC_CHANNELS + 1));
    a.bcs("osbyte_claimed");
    a.php();
    a.sei();
    a.stx(abs(var::ADC_CHANNEL));
    a.lda(imm(0));
    a.sta(abs(page2::ADC_LAST));
    a.jsr("adc_start");
    a.plp();
    a.jmp("osbyte_claimed");

    // ADVAL(0): the fire buttons held, and the channel last converted. The
    // OS reads the channels' readings from page 2 itself.
    a.here("fx128");
    a.lda(zp(os::CALL_X));
    a.bne("osbyte_pass");
    a.lda(abs(port::STATUS));
    a.eor(imm(0xFF));
    for _ in 0..status::FIRE_1.trailing_zeros() {
        a.lsr(Acc);
    }
    a.and(imm(FIRE_BUTTONS));
    a.sta(zp(os::CALL_X));
    a.lda(abs(page2::ADC_LAST));
    a.sta(zp(os::CALL_Y));
    a.jmp("osbyte_claimed");
}

/// *FX163,128,0 and 1: the ports on and off, and the OSBYTE every call made
/// from within another goes through.
fn switching(a: &mut Asm) {
    a.here("fx163");
    a.lda(zp(os::CALL_X));
    a.cmp(imm(FX163_PLUS1));
    a.bne("osbyte_pass");
    a.lda(zp(os::CALL_Y));
    let on = a.local();
    a.beq(on);
    a.cmp(imm(FX163_OFF));
    a.bne("osbyte_pass");
    a.jsr("flags");
    a.bmi("osbyte_claimed");
    a.jsr("ports_off");
    a.jmp("osbyte_claimed");
    a.bind(on);
    a.jsr("flags");
    a.bpl("osbyte_claimed");
    a.jsr("ports_on");
    a.jmp("osbyte_claimed");

    // A the flags and X this ROM: the ports on, a conversion started and
    // the ROM counted into the poll.
    a.here("ports_on");
    a.php();
    a.sei();
    a.and(imm(!OFF));
    a.sta(abs(page_d::PRIVATE_WORKSPACE).x());
    a.jsr("adc_start");
    a.lda(imm(osbyte::POLL_UP));
    a.jsr("osbyte_within");
    a.plp();
    a.rts();

    // A the flags and X this ROM: the ports off, the ROM counted out of the
    // poll, and what waits for the parallel printer, which nothing will now
    // send, purged.
    a.here("ports_off");
    a.php();
    a.sei();
    a.ora(imm(OFF));
    a.sta(abs(page_d::PRIVATE_WORKSPACE).x());
    a.lda(imm(osbyte::POLL_DOWN));
    a.jsr("osbyte_within");
    a.plp();
    a.lda(abs(var::PRINTER));
    a.cmp(imm(printer::PARALLEL));
    a.bne(RTS);
    a.ldx(imm(buffer::PRINTER));
    a.lda(imm(osbyte::FLUSH_BUFFER));
    a.fall_into("osbyte_within");

    // OSBYTE A with X, Y zero.
    a.here("osbyte_within");
    a.tay();
    for kept in [os::CALL_Y, os::CALL_X, os::CALL_A] {
        a.lda(zp(kept));
        a.pha();
    }
    a.tya();
    a.ldy(imm(0));
    a.jsr(entry::OSBYTE);
    for kept in [os::CALL_A, os::CALL_X, os::CALL_Y] {
        a.pla();
        a.sta(zp(kept));
    }
    a.rts();
}

/// The ADC: the channels converted in turn from the highest selected down,
/// one a poll, each reading kept in page 2 with the event. A channel
/// setting above the four ADVAL reads is taken as the fourth.
fn adc(a: &mut Asm) {
    // X = A, but at most the last channel; Z set for none.
    a.here("adc_clamp");
    a.cmp(imm(page2::ADC_CHANNELS + 1));
    let within = a.local();
    a.bcc(within);
    a.lda(imm(page2::ADC_CHANNELS));
    a.bind(within);
    a.tax();
    a.rts();

    // The channel converting started, if any.
    a.here("adc_start");
    a.lda(abs(var::ADC_CHANNEL));
    a.jsr("adc_clamp");
    a.beq(RTS);
    a.dex();
    a.txa();
    a.ora(imm(command::SINGLE_ENDED));
    a.sta(abs(port::ADC));
    a.rts();

    // An ended conversion's reading, eight bits high in sixteen, its event,
    // and the next channel down started, the highest again after channel 1.
    a.here("adc_poll");
    a.lda(abs(var::ADC_CHANNEL));
    a.jsr("adc_clamp");
    a.beq(RTS);
    a.bit(abs(port::STATUS));
    a.bvs(RTS);
    a.lda(abs(port::ADC));
    a.sta(abs(page2::ADC_HIGH - 1).x());
    a.lda(imm(0));
    a.sta(abs(page2::ADC_LOW - 1).x());
    a.stx(abs(page2::ADC_LAST));
    a.txa();
    a.ldy(imm(event::CONVERTED));
    a.jsr(entry::OSEVEN);
    a.ldx(abs(page2::ADC_LAST));
    a.dex();
    let next = a.local();
    a.bne(next);
    a.lda(abs(var::ADC_MAXIMUM));
    a.jsr("adc_clamp");
    a.bind(next);
    a.stx(abs(var::ADC_CHANNEL));
    a.jmp("adc_start");
}

/// The parallel printer, which raises no interrupt: woken by the OS with
/// its first character, then fed each poll until its buffer is empty, when
/// it goes dormant again.
fn printer_driver(a: &mut Asm) {
    a.here("printer_poll");
    a.lda(abs(var::PRINTER));
    a.cmp(imm(printer::PARALLEL));
    a.bne(RTS);
    a.bit(abs(PRINTER_BUSY));
    a.bmi(RTS);
    a.fall_into("printer_send");

    // Up to PER_POLL characters, each taken from the buffer only once the
    // printer is ready for it, the count kept on the stack.
    a.here("printer_send");
    a.lda(imm(PER_POLL));
    a.pha();
    let next = a.here_local();
    // V set: REMV examines rather than removes.
    a.ldx(imm(buffer::PRINTER));
    a.bit(abs("v_set"));
    a.jsr("remv");
    let empty = a.local();
    a.bcs(empty);
    a.bit(abs(port::STATUS));
    let busy = a.local();
    a.bmi(busy);
    a.ldx(imm(buffer::PRINTER));
    a.clv();
    a.jsr("remv");
    a.sta(abs(port::PRINTER));
    a.tsx();
    a.dec(abs(STACK_PAGE + 1).x());
    a.bne(next);
    a.bind(busy);
    a.pla();
    a.rts();
    a.bind(empty);
    a.pla();
    a.ldx(imm(buffer::PRINTER));
    a.lda(imm(osbyte::PRINTER_DORMANT));
    a.jmp("osbyte_within");

    a.here("remv");
    a.jmp(abs(vector::REM).ind());
    a.here("v_set");
    a.byte(V_SET);
}

/// *HELP with no topic names the ROM, before the OS names itself.
fn help(a: &mut Asm) {
    a.here("service_help");
    a.lda(zp(os::TEXT).ind_y());
    a.cmp(imm(CR));
    let pass = a.local();
    a.bne(pass);
    a.ldx(imm(0));
    let print = a.here_local();
    a.lda(abs("help_text").x());
    a.beq(pass);
    a.jsr(entry::OSASCI);
    a.inx();
    a.bne(print);
    a.bind(pass);
    a.lda(imm(service::HELP));
    a.rts();
    a.here("help_text");
    a.text(&format!("\r{TITLE} {VERSION_TEXT}\r"));
    a.byte(0);
}

#[cfg(test)]
#[path = "plus1_tests.rs"]
mod tests;
