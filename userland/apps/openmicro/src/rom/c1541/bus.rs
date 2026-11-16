//! The serial bus, the drive as a device: commands under ATN, bytes taken
//! as the listener and sent as the talker with EOI, and the turnaround
//! after TALK, the KERNAL's controller mirrored.
//!
//! VIA1's port B reads each line inverted, 1 while it is held low, and
//! drives CLOCK and DATA low through inverters. While ATN and ATNA differ
//! the hardware holds DATA, so a drive answers ATN before its DOS looks.
//! A bit is DATA high for 1, valid while CLOCK is released.

use super::ws::{zp, STACK_TOP};
use crate::machine::c64::drive::{bus, VIA1};
use crate::machine::c64::serial::command;
use crate::machine::via::{irq, reg};
use crate::rom::asm::{abs, imm, zp as z, Acc, Asm};

/// VIA1's port B: the bus's lines.
pub const PORT: u16 = VIA1 + reg::ORB as u16;
/// VIA1's interrupt flags.
pub const FLAGS: u16 = VIA1 + reg::IFR as u16;

/// Passes of thirteen cycles: the 200 microseconds a listener lets a talker
/// leave CLOCK released before taking the byte as the last, and the
/// millisecond a talker waits for a byte to be taken.
const EOI_PASSES: u8 = 16;
const ACK_PASSES: u8 = 77;
/// Delays in passes of five cycles: EOI's acknowledgement held, and a bit
/// set up on DATA with CLOCK held about a hundred microseconds, then valid
/// with CLOCK released about seventy-five, as the original's are: a
/// controller the VIC-II stalls between its loops still sees every edge.
const EOI_HOLD: u8 = 16;
const BIT_SETUP: u8 = 14;
const BIT_VALID: u8 = 14;
/// Passes of fifteen cycles: the millisecond the controller has to take
/// CLOCK after asserting ATN.
const CLOCK_TAKEN_PASSES: u8 = 67;
/// Passes of five cycles: the 80 microseconds at least the bus's timing has
/// a new talker hold CLOCK before it is ready to send. The original holds
/// less with nothing to send, which a controller the VIC-II stalls misses.
pub const TURNAROUND_HOLD: u8 = 16;

/// The command's top bits that name a channel: data, close or open.
const CHANNEL_REQUEST: u8 = 0x60;
/// Bit 7 of ADDRESSED: the drive listens; bit 6: it talks.
pub const LISTENER: u8 = 0x80;
pub const TALKER: u8 = 0x40;

const _: () = assert!(bus::ATN_IN == 0x80, "N reads ATN off the port");

pub fn emit(a: &mut Asm) {
    lines(a);
    receive(a);
    send(a);
    attention(a);
}

/// CLOCK and DATA pulled low or released, the port's other outputs kept.
fn lines(a: &mut Asm) {
    for (name, line, low) in [
        ("data_low", bus::DATA_OUT, true),
        ("data_high", bus::DATA_OUT, false),
        ("clock_low", bus::CLOCK_OUT, true),
        ("clock_high", bus::CLOCK_OUT, false),
    ] {
        a.here(name);
        a.lda(abs(PORT));
        if low {
            a.ora(imm(line));
        } else {
            a.and(imm(!line));
        }
        a.sta(abs(PORT));
        a.rts();
    }
}

/// A byte taken as the listener into A, interrupts off while it comes:
/// the talker's ready to send waited for, DATA released, and the bits read
/// while CLOCK is released. A talker that leaves CLOCK released 200
/// microseconds sends its last byte, EOI's bit 7 then set and DATA held
/// to acknowledge it. Under ATN, ATN released first returns C set; out of
/// it, ATN asserted at any point, a byte begun or not, enters the ATN
/// service.
fn receive(a: &mut Asm) {
    a.here("receive_byte");
    let (ready, watch, held, bits, bit, valid, changed) = (
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
    a.sta(z(zp::EOI));
    a.bind(ready);
    a.lda(abs(PORT));
    a.eor(z(zp::ATN_MODE));
    a.bmi(changed);
    a.and(imm(bus::CLOCK_IN));
    a.bne(ready);
    a.jsr("data_high");
    a.ldx(imm(EOI_PASSES));
    a.bind(watch);
    a.lda(abs(PORT));
    a.eor(z(zp::ATN_MODE));
    a.bmi(changed);
    a.and(imm(bus::CLOCK_IN));
    a.bne(bits);
    a.dex();
    a.bne(watch);
    a.lda(imm(0x80));
    a.sta(z(zp::EOI));
    a.jsr("data_low");
    a.ldx(imm(EOI_HOLD));
    let eoi_wait = a.here_local();
    a.dex();
    a.bne(eoi_wait);
    a.jsr("data_high");
    a.bind(held);
    a.lda(abs(PORT));
    a.eor(z(zp::ATN_MODE));
    a.bmi(changed);
    a.and(imm(bus::CLOCK_IN));
    a.beq(held);
    a.bind(bits);
    a.ldx(imm(8));
    // ATN is watched through every bit too: a controller reset mid-byte
    // comes back with ATN, and waits for DATA with no timeout.
    a.bind(bit);
    a.lda(abs(PORT));
    a.eor(z(zp::ATN_MODE));
    a.bmi(changed);
    a.and(imm(bus::CLOCK_IN));
    a.bne(bit);
    // DATA_IN reads 1 for a held line, a 0 bit: the byte comes inverted.
    a.lda(abs(PORT));
    a.lsr(Acc);
    a.ror(z(zp::BYTE));
    a.bind(valid);
    a.lda(abs(PORT));
    a.eor(z(zp::ATN_MODE));
    a.bmi(changed);
    a.and(imm(bus::CLOCK_IN));
    a.beq(valid);
    a.dex();
    a.bne(bit);
    a.jsr("data_low");
    a.lda(z(zp::BYTE));
    a.eor(imm(0xFF));
    a.sta(z(zp::BYTE));
    a.cli();
    a.clc();
    a.rts();
    a.bind(changed);
    a.bit(z(zp::ATN_MODE));
    a.bpl("atn_service");
    a.sec();
    a.rts();
}

/// A sent as the talker, with EOI where EOI's bit 7 is set, interrupts off
/// while it goes: CLOCK released to offer it, the listener's DATA released
/// waited for, EOI's acknowledgement waited out, then each bit set on DATA
/// and held valid with CLOCK released. C is set where no listener took the
/// byte within a millisecond. ATN asserted at any point enters the ATN
/// service, the byte not taken: a controller lets DATA go just before it
/// asserts ATN, and ATN's hardware acknowledgement holds DATA as a taken
/// byte would.
fn send(a: &mut Asm) {
    a.here("send_byte");
    let (ready, acknowledging, acknowledged, bit, one, waiting, taken, attention) = (
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
    a.sta(z(zp::BYTE));
    a.jsr("clock_high");
    a.bind(ready);
    a.lda(abs(PORT));
    a.bmi(attention);
    a.and(imm(bus::DATA_IN));
    a.bne(ready);
    a.bit(z(zp::EOI));
    let start = a.local();
    a.bpl(start);
    a.bind(acknowledging);
    a.lda(abs(PORT));
    a.bmi(attention);
    a.and(imm(bus::DATA_IN));
    a.beq(acknowledging);
    a.bind(acknowledged);
    a.lda(abs(PORT));
    a.bmi(attention);
    a.and(imm(bus::DATA_IN));
    a.bne(acknowledged);
    a.bind(start);
    a.jsr("clock_low");
    a.ldx(imm(8));
    a.bind(bit);
    a.lda(abs(PORT));
    a.bmi(attention);
    a.lsr(z(zp::BYTE));
    a.and(imm(!bus::DATA_OUT));
    a.bcs(one);
    a.ora(imm(bus::DATA_OUT));
    a.bind(one);
    a.sta(abs(PORT));
    a.ldy(imm(BIT_SETUP));
    let setup = a.here_local();
    a.dey();
    a.bne(setup);
    a.and(imm(!bus::CLOCK_OUT));
    a.sta(abs(PORT));
    a.ldy(imm(BIT_VALID));
    let valid = a.here_local();
    a.dey();
    a.bne(valid);
    a.ora(imm(bus::CLOCK_OUT));
    a.sta(abs(PORT));
    a.dex();
    a.bne(bit);
    a.jsr("data_high");
    a.ldx(imm(ACK_PASSES));
    a.bind(waiting);
    a.lda(abs(PORT));
    a.bmi(attention);
    a.and(imm(bus::DATA_IN));
    a.bne(taken);
    a.dex();
    a.bne(waiting);
    a.sec();
    a.cli();
    a.rts();
    a.bind(taken);
    a.cli();
    a.clc();
    a.rts();
    a.bind(attention);
    a.jmp("atn_service");
}

/// The ATN service, from the idle loop or a transfer ATN interrupted, as a
/// command is never cut short: the stack emptied, DATA held by the DOS in
/// place of the hardware, and each command read until ATN is released.
/// The controller takes CLOCK just after it asserts ATN, so the first
/// command waits up to a millisecond for that: CLOCK not yet taken is not
/// CLOCK let go to send. A drive busy when ATN came finds CLOCK taken and
/// let go already, and the wait runs out. LISTEN and TALK address this
/// drive or another; a channel's command for
/// this drive is kept for after ATN; UNLISTEN ends a listen, leaving its
/// request for the interface. Then the hardware's acknowledgement is let go
/// and the drive listens, talks, or lets the bus go.
fn attention(a: &mut Asm) {
    a.here("atn_service");
    let (taken, next, talk, other, channel, released, unlisten) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.sei();
    a.ldx(imm(STACK_TOP));
    a.txs();
    a.lda(imm(0x80));
    a.sta(z(zp::ATN_MODE));
    a.lda(imm(irq::CA1));
    a.sta(abs(FLAGS));
    a.lda(abs(PORT));
    a.and(imm(!bus::CLOCK_OUT));
    a.ora(imm(bus::DATA_OUT | bus::ATN_ACK));
    a.sta(abs(PORT));
    a.ldx(imm(CLOCK_TAKEN_PASSES));
    a.bind(taken);
    a.lda(abs(PORT));
    a.bpl(released);
    a.and(imm(bus::CLOCK_IN));
    a.bne(next);
    a.dex();
    a.bne(taken);
    a.bind(next);
    a.jsr("receive_byte");
    a.bcs(released);
    a.lda(z(zp::BYTE));
    a.cmp(imm(command::UNLISTEN));
    a.beq(unlisten);
    a.cmp(imm(command::UNTALK));
    a.beq(other);
    a.and(imm(CHANNEL_REQUEST));
    a.cmp(imm(CHANNEL_REQUEST));
    a.beq(channel);
    a.cmp(imm(command::TALK));
    a.beq(talk);
    a.cmp(imm(command::LISTEN));
    a.bne(next);
    a.lda(imm(LISTENER));
    a.skip_over(|a| {
        a.bind(talk);
        a.lda(imm(TALKER));
    });
    a.tax();
    a.lda(z(zp::BYTE));
    a.and(imm(!(command::LISTEN | command::TALK)));
    a.cmp(z(zp::DEVICE));
    a.bne(other);
    a.stx(z(zp::ADDRESSED));
    a.jmp(next);
    a.bind(unlisten);
    a.bit(z(zp::ADDRESSED));
    a.bpl(other);
    a.lda(z(zp::REQUEST));
    a.ora(z(zp::CHANNEL));
    a.sta(z(zp::PENDING));
    a.bind(other);
    a.lda(imm(0));
    a.sta(z(zp::ADDRESSED));
    a.beq(next);
    a.bind(channel);
    a.lda(z(zp::ADDRESSED));
    a.beq(next);
    a.lda(z(zp::BYTE));
    a.and(imm(0x0F));
    a.sta(z(zp::CHANNEL));
    a.lda(z(zp::BYTE));
    a.and(imm(0xF0));
    a.sta(z(zp::REQUEST));
    a.jsr("channel_requested");
    a.jmp(next);

    a.bind(released);
    a.lda(imm(0));
    a.sta(z(zp::ATN_MODE));
    a.lda(abs(PORT));
    a.and(imm(!bus::ATN_ACK));
    a.sta(abs(PORT));
    a.jmp("after_attention");
}
