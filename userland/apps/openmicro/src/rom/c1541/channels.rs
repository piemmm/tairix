//! The interface's side of the bus: what a channel's request readies, the
//! listen and talk that follow ATN, and the OPEN, CLOSE or command an
//! UNLISTEN leaves. Channel 15 is the command channel: a name opened on it
//! or data sent to it is a command, and what it sends is the error channel.
//! The others are data channels, each with a file of its own.

use super::bus::{LISTENER, TALKER, TURNAROUND_HOLD};
use super::ws::{page, zp};
use crate::machine::c64::serial::command;
use crate::rom::asm::{abs, imm, zp as z, Asm, RTS};

/// The command channel's number.
pub const COMMAND_CHANNEL: u8 = 15;

const _: () = assert!(LISTENER == 0x80, "a load reads the listener as N");

pub fn emit(a: &mut Asm) {
    requested(a);
    after(a);
    transfer(a);
    carry_out(a);
}

/// `channel_requested`, from the ATN service: an OPEN, or the command
/// channel's data to come, starts the command buffer afresh.
fn requested(a: &mut Asm) {
    a.here("channel_requested");
    let start = a.local();
    a.lda(z(zp::REQUEST));
    a.cmp(imm(command::OPEN));
    a.beq(start);
    a.bit(z(zp::ADDRESSED));
    a.bpl(RTS);
    a.lda(z(zp::CHANNEL));
    a.cmp(imm(COMMAND_CHANNEL));
    a.bne(RTS);
    a.bind(start);
    a.lda(imm(0));
    a.sta(z(zp::COMMAND_LENGTH));
    a.rts();
}

/// `after_attention`: a pending request carried out, then the drive
/// listens, talks, or lets the bus go and waits for ATN.
fn after(a: &mut Asm) {
    a.here("after_attention");
    let (listening, talking, next, carried) = (a.local(), a.local(), a.local(), a.local());
    a.lda(z(zp::PENDING));
    a.beq(carried);
    a.ldx(imm(0));
    a.stx(z(zp::PENDING));
    a.jsr("carry_out");
    // Not BIT and V: BYTE READY sets V through SO whenever the disk turns.
    a.bind(carried);
    a.lda(z(zp::ADDRESSED));
    a.bmi(listening);
    a.and(imm(TALKER));
    a.bne(talking);
    a.here("let_bus_go");
    a.jsr("data_high");
    a.jsr("clock_high");
    a.jmp("idle");

    // Each byte the controller sends goes to the request's channel, until
    // ATN.
    a.bind(listening);
    a.jsr("receive_byte");
    a.jsr("take_byte");
    a.jmp(listening);

    // The turnaround: the controller holds DATA and lets CLOCK go; the
    // drive takes CLOCK and lets DATA go, holds CLOCK the acknowledge's
    // time, then sends while it has bytes and the controller takes them.
    a.bind(talking);
    a.jsr("data_high");
    a.jsr("clock_low");
    a.ldy(imm(TURNAROUND_HOLD));
    let hold = a.here_local();
    a.dey();
    a.bne(hold);
    a.bind(next);
    a.jsr("next_byte");
    a.bcs("let_bus_go");
    a.jsr("send_byte");
    a.bcs("let_bus_go");
    a.jsr("byte_taken");
    a.jmp(next);
}

/// `take_byte`: A, received, kept in the command buffer where the request
/// was an OPEN or the command channel's data, else written to the data
/// channel's file; a command too long for the buffer leaves its length one
/// past the room. `next_byte`: the talked channel's next byte in A, EOI's
/// bit 7 set for the last, C set where it has none; `byte_taken` moves the
/// channel past it once the listener took it.
fn transfer(a: &mut Asm) {
    a.here("take_byte");
    let (keep, room) = (a.local(), a.local());
    a.ldx(z(zp::REQUEST));
    a.cpx(imm(command::OPEN));
    a.beq(keep);
    a.ldx(z(zp::CHANNEL));
    a.cpx(imm(COMMAND_CHANNEL));
    a.beq(keep);
    a.jmp("file_write");
    a.bind(keep);
    a.ldx(z(zp::COMMAND_LENGTH));
    a.cpx(imm(page::COMMAND_BYTES));
    a.bcc(room);
    a.ldx(imm(page::COMMAND_BYTES + 1));
    a.stx(z(zp::COMMAND_LENGTH));
    a.rts();
    a.bind(room);
    a.sta(abs(page::COMMAND).x());
    a.inc(z(zp::COMMAND_LENGTH));
    a.rts();

    a.here("next_byte");
    let data = a.local();
    a.ldx(z(zp::CHANNEL));
    a.cpx(imm(COMMAND_CHANNEL));
    a.bne(data);
    a.jmp("message_byte");
    a.bind(data);
    a.jmp("file_byte");

    a.here("byte_taken");
    let taken = a.local();
    a.ldx(z(zp::CHANNEL));
    a.cpx(imm(COMMAND_CHANNEL));
    a.bne(taken);
    a.jmp("message_taken");
    a.bind(taken);
    a.jmp("file_taken");
}

/// `carry_out`: UNLISTEN's request and channel in A, that channel served
/// while it is carried out, the buffers it held freed after. The command
/// channel's OPEN or data is a command, and its CLOSE closes every
/// channel; a data channel's OPEN opens its file and its CLOSE closes it.
fn carry_out(a: &mut Asm) {
    a.here("carry_out");
    let (not_open, command, done) = (a.local(), a.local(), a.local());
    a.tax();
    a.lda(z(zp::CHANNEL));
    a.pha();
    a.txa();
    a.and(imm(0x0F));
    a.sta(z(zp::CHANNEL));
    a.txa();
    a.and(imm(0xF0));
    a.ldx(z(zp::CHANNEL));
    a.cpx(imm(COMMAND_CHANNEL));
    a.beq(command);
    a.cmp(imm(command::OPEN));
    a.bne(not_open);
    a.jsr("open_file");
    a.jmp(done);
    a.bind(not_open);
    a.cmp(imm(command::CLOSE));
    a.bne(done);
    a.jsr("close_channel");
    a.jmp(done);
    a.bind(command);
    let close_all = a.local();
    a.cmp(imm(command::CLOSE));
    a.beq(close_all);
    a.jsr("execute");
    a.jmp(done);
    a.bind(close_all);
    a.jsr("close_all");
    a.bind(done);
    a.jsr("release_held");
    a.pla();
    a.sta(z(zp::CHANNEL));
    a.rts();
}
