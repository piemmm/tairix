//! Interrupts: the IRQ and BRK entry, the jiffy interrupt's handler and its
//! work (the clock, the STOP key, the cassette's switch, the cursor's flash,
//! the keyboard), and the clock's and STOP's own entries.

use super::keyboard::{STOP_COLUMN, STOP_ROW};
use super::ws::{page, vector, zp};
use super::{fixed, restore_and_return};
use crate::cpu::{flag, STACK_PAGE};
use crate::machine::c64::cia::reg;
use crate::machine::c64::io;
use crate::machine::c64::memory::line;
use crate::rom::asm::{abs, imm, zp as z, Acc, Asm, Label, RTS};

/// The jiffies a day holds, where the clock goes back to 0.
const DAY: [u8; 4] = (24 * 60 * 60 * 60u32).to_be_bytes();

const _: () = assert!(
    DAY[0] == 0 && DAY[3] == 0,
    "a day ends only as the clock's low byte wraps"
);
const _: () = assert!(
    line::CASSETTE_SENSE << 1 == line::CASSETTE_MOTOR,
    "the sense line's bit moved up is the motor's"
);

/// Jiffies the cursor shows, and is hidden, for.
pub const BLINK_JIFFIES: u8 = 20;

/// Where the interrupted P is, once the entry has pushed A, X and Y.
const PUSHED_FLAGS: u16 = STACK_PAGE + 4;

const _: () = assert!(
    fixed::IRQ_ACK + 3 == fixed::IRQ_EXIT,
    "the acknowledge runs on into the exit"
);

/// CINV's default, the jiffy interrupt's work, filling the room to the
/// acknowledge and exit software jumps to itself and running on into it.
pub fn pin(a: &mut Asm) {
    a.pin(fixed::IRQ, |a| {
        let (switch, keys) = (a.local(), a.local());
        a.jsr("udtim");
        blink(a, switch);
        a.bind(switch);
        cassette_switch(a, keys);
        a.bind(keys);
        a.jsr("scnkey");
        a.fall_into(fixed::IRQ_ACK);
    });
    a.pin(fixed::IRQ_ACK, |a| {
        a.lda(abs(io::cia1(reg::ICR)));
        restore_and_return(a);
    });
}

pub fn emit(a: &mut Asm) {
    entry(a);
    clock(a);
    stop(a);
}

/// The IRQ vector's entry: A, X and Y pushed, then CBINV for a BRK or
/// CINV for an interrupt.
fn entry(a: &mut Asm) {
    a.here("irq_entry");
    a.pha();
    a.txa();
    a.pha();
    a.tya();
    a.pha();
    a.tsx();
    a.lda(abs(PUSHED_FLAGS).x());
    a.and(imm(flag::B));
    let interrupt = a.local();
    a.beq(interrupt);
    a.jmp(abs(vector::CBINV).ind());
    a.bind(interrupt);
    a.jmp(abs(vector::CINV).ind());
}

/// UDTIM, the clock a jiffy on, a day back to 0, and the STOP key's column
/// read into STKEY; RDTIM and SETTIM, the clock's least significant byte in
/// A, its middle in X and its most significant in Y.
fn clock(a: &mut Asm) {
    a.here("udtim");
    let wrap = a.local();
    a.inc(z(zp::TIME + 2));
    // A day ends only as the low byte wraps.
    a.bne("scan_stop");
    a.inc(z(zp::TIME + 1));
    a.bne(wrap);
    a.inc(z(zp::TIME));
    a.bind(wrap);
    for (byte, &value) in (0..).zip(&DAY[1..3]) {
        a.lda(z(zp::TIME + byte));
        a.cmp(imm(value));
        a.bne("scan_stop");
    }
    a.lda(imm(0));
    for byte in 0..2 {
        a.sta(z(zp::TIME + byte));
    }
    a.here("scan_stop");
    a.lda(imm(STOP_COLUMN));
    a.sta(abs(io::cia1(reg::PRA)));
    a.lda(abs(io::cia1(reg::PRB)));
    a.sta(z(zp::STKEY));
    a.rts();

    a.here("rdtim");
    a.php();
    a.sei();
    a.lda(z(zp::TIME + 2));
    a.ldx(z(zp::TIME + 1));
    a.ldy(z(zp::TIME));
    a.plp();
    a.rts();

    a.here("settim");
    a.php();
    a.sei();
    a.sta(z(zp::TIME + 2));
    a.stx(z(zp::TIME + 1));
    a.sty(z(zp::TIME));
    a.plp();
    a.rts();
}

/// STOP: Z set with STOP held at the last UDTIM, the channels then cleared
/// and the keyboard buffer emptied; else A the row it read.
fn stop(a: &mut Asm) {
    a.here("stop");
    a.lda(z(zp::STKEY));
    a.cmp(imm(STOP_ROW));
    a.bne(RTS);
    a.php();
    a.jsr("CLRCHN");
    a.lda(imm(0));
    a.sta(z(zp::NDX));
    a.plp();
    a.rts();
}

/// The cassette's motor follows its keys: with none down it stops and the
/// tape routines' claim on it ends; with one down it runs unless they hold
/// it. Then on to `done`.
fn cassette_switch(a: &mut Asm, done: Label) {
    a.here("cassette_switch");
    let (down, store) = (a.local(), a.local());
    a.lda(z(1));
    a.and(imm(line::CASSETTE_SENSE));
    a.beq(down);
    // The sense line's bit moved up is the motor's.
    a.asl(Acc);
    a.ldx(imm(0));
    a.stx(z(zp::CAS1));
    a.ora(z(1));
    a.bne(store);
    a.bind(down);
    a.lda(z(zp::CAS1));
    a.bne(done);
    a.lda(z(1));
    a.and(imm(!line::CASSETTE_MOTOR));
    a.bind(store);
    a.sta(z(1));
}

/// The cursor's flash while BLNSW is 0: every [`BLINK_JIFFIES`] the
/// character under it reversed in the text colour, or given back with its
/// own colour. Then on to `done`.
fn blink(a: &mut Asm, done: Label) {
    a.here("blink");
    let hide = a.local();
    a.lda(z(zp::BLNSW));
    a.bne(done);
    a.dec(z(zp::BLNCT));
    a.bne(done);
    a.lda(imm(BLINK_JIFFIES));
    a.sta(z(zp::BLNCT));
    a.ldy(z(zp::PNTR));
    a.lsr(z(zp::BLNON));
    a.ldx(abs(page::GDCOL));
    a.lda(z(zp::PNT).ind_y());
    a.bcs(hide);
    a.inc(z(zp::BLNON));
    a.sta(z(zp::GDBLN));
    a.lda(z(zp::USER).ind_y());
    a.and(imm(0x0F));
    a.sta(abs(page::GDCOL));
    a.ldx(abs(page::COLOR));
    a.lda(z(zp::GDBLN));
    a.bind(hide);
    a.eor(imm(0x80));
    a.sta(z(zp::PNT).ind_y());
    a.txa();
    a.sta(z(zp::USER).ind_y());
}
