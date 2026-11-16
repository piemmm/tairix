//! RS-232 on the user port, as the Programmer's Reference Guide gives it:
//! OPEN's name sets the control and command registers and a rate of the
//! user's own; two buffers of a page each come off the top of memory, which
//! BASIC follows with a CLR; bytes go out on PA2 and come in on PB0 a bit at
//! a time on CIA2's NMI, timer A pacing the transmitter, FLAG's start bit
//! then timer B the receiver.

use super::files::error;
use super::fixed;
use super::ws::{page, zp};
use crate::machine::c64::cia::{cr, icr, reg};
use crate::machine::c64::io::cia2;
use crate::machine::c64::Standard;
use crate::rom::asm::{abs, byte, imm, sym, zp as z, Acc, Asm, RTS};

/// CIA2's port A line that sends.
pub const TXD: u8 = 0x04;
/// CIA2's port B line that receives, and the two port B lines RS-232
/// drives: request to send and data terminal ready.
const RXD: u8 = 0x01;
const HANDSHAKE_OUT: u8 = 0x06;

/// The control register: its rate, its word length and its two stop bits.
const RATE: u8 = 0x0F;
const WORD_LENGTH: u8 = 0x60;
const TWO_STOP_BITS: u8 = 0x80;
/// The command register's parity: on, then even rather than odd, or with
/// bit 7 a fixed bit, space rather than mark.
const PARITY_ON: u8 = 0x20;
const PARITY_EVEN: u8 = 0x40;

/// RSSTAT's bits.
mod rs_status {
    /// A byte's parity was wrong.
    pub const PARITY: u8 = 0x01;
    /// A byte's stop bit was missing.
    pub const FRAMING: u8 = 0x02;
    /// A byte came with the input buffer full.
    pub const OVERRUN: u8 = 0x04;
    /// The input buffer was empty.
    pub const EMPTY: u8 = 0x08;
}

/// The rates the control register's low four bits name, from 1.
const RATES: [(u32, u32); 10] = [
    (50, 1),
    (75, 1),
    (110, 1),
    (269, 2),
    (150, 1),
    (300, 1),
    (600, 1),
    (1200, 1),
    (1800, 1),
    (2400, 1),
];
/// The user's rate: a bit's cycles twice M51AJB less this.
const USER_RATE_OFFSET: u8 = 100;

/// RSSTAT to RODBE, by the last's offset: what OPEN clears.
const OPEN_CLEARS: u8 = byte((page::RODBE - page::RSSTAT) as usize);

// The bits shifted or BIT out rather than masked: the NMI's causes from
// timer A's up, ENABL's timer A, RXD, the stop bits and the fixed or even
// parity in bits 7 and 6, and the word length in 6 and 5.
const _: () = assert!(
    icr::TA == 0x01
        && icr::TB == 0x02
        && RXD == 0x01
        && TWO_STOP_BITS == 0x80
        && PARITY_EVEN == 0x40
        && WORD_LENGTH == 0x60,
    "the bits RS-232 shifts out"
);

/// A bit's cycles at each rate on a standard.
const fn bit_times(standard: Standard) -> [u8; 2 * RATES.len()] {
    let mut table = [0; 2 * RATES.len()];
    let mut rate = 0;
    while rate < RATES.len() {
        let (numerator, denominator) = RATES[rate];
        let cycles = standard.clock() * denominator as u64 / numerator as u64;
        assert!(cycles < 0x1_0000, "a bit fits the timer");
        let bytes = cycles.to_le_bytes();
        table[2 * rate] = bytes[0];
        table[2 * rate + 1] = bytes[1];
        rate += 1;
    }
    table
}

// Every rate's time has a high byte, so OPEN's branch past the user's
// rate is certain.
const _: () = {
    let (ntsc, pal) = (bit_times(Standard::Ntsc), bit_times(Standard::Pal));
    let mut high = 1;
    while high < ntsc.len() {
        assert!(
            ntsc[high] != 0 && pal[high] != 0,
            "a bit's time within a page"
        );
        high += 2;
    }
};

pub fn emit(a: &mut Asm) {
    open(a);
    close(a);
    transfer(a);
    nmi(a);
    sending(a);
    frames(a);
    parity(a);
    receive_bit(a);
    a.later(|a| {
        a.here("rs232_bit_times");
        a.bytes(&bit_times(Standard::Ntsc));
        a.bytes(&bit_times(Standard::Pal));
    });
}

/// OPEN: the registers from the name; RSSTAT to RODBE cleared, the
/// buffers' indices among them; the bit's time and the frame's bits; the
/// lines; and the buffers taken from the top of memory unless they are
/// there already, the top then reported as CLOSE reports it. The receiver
/// waits for CHKIN: `rs232_listen` has it listen for a start bit on FLAG,
/// unless it already does or a byte is coming in. ENABL holds the CIA2
/// interrupts enabled as written, SET among them.
fn open(a: &mut Asm) {
    a.here("rs232_open");
    let (copy, copied, clear) = (a.local(), a.local(), a.local());
    a.ldy(imm(0));
    a.bind(copy);
    a.cpy(z(zp::FNLEN));
    a.beq(copied);
    a.lda(z(zp::FNADR).ind_y());
    a.sta(abs(page::M51CTR).y());
    a.iny();
    a.cpy(imm(4));
    a.bne(copy);
    a.bind(copied);
    // BITNUM and BAUDOF too, set below.
    a.lda(imm(0));
    a.ldx(imm(OPEN_CLEARS));
    a.bind(clear);
    a.sta(abs(page::RSSTAT).x());
    a.dex();
    a.bpl(clear);
    a.sta(z(zp::BITTS));
    bit_time(a);
    // BITNUM as the original keeps it: the word and one.
    a.jsr("word_bits");
    a.tax();
    a.inx();
    a.stx(abs(page::BITNUM));
    a.lda(imm(HANDSHAKE_OUT));
    a.sta(abs(cia2(reg::DDRB)));
    a.sta(abs(cia2(reg::PRB)));
    a.ldx(abs(page::MEMSIZ));
    a.ldy(abs(page::MEMSIZ + 1));
    a.lda(z(zp::RIBUF + 1));
    a.bne("rs232_top_reported");
    a.dey();
    a.stx(z(zp::RIBUF));
    a.sty(z(zp::RIBUF + 1));
    a.dey();
    a.stx(z(zp::ROBUF));
    a.sty(z(zp::ROBUF + 1));
    // The top of memory, in X and Y, reported whether it moved or not.
    a.here("rs232_top_reported");
    a.sty(abs(page::MEMSIZ + 1));
    a.lda(imm(error::MEMORY_MOVED));
    a.sec();
    a.rts();

    a.here("rs232_listen");
    a.lda(abs(page::ENABL));
    a.and(imm(icr::TB | icr::FLAG));
    a.bne(RTS);
    a.lda(imm(icr::SET | icr::FLAG));
    a.sta(abs(cia2(reg::ICR)));
    a.ora(abs(page::ENABL));
    a.sta(abs(page::ENABL));
    a.rts();
}

/// BAUDOF from the control register's rate, for the machine's standard,
/// or from the user's rate in M51AJB: part of OPEN, which runs on.
fn bit_time(a: &mut Asm) {
    a.here("rs232_bit_time");
    let (user, in_table, ntsc, set) = (a.local(), a.local(), a.local(), a.local());
    a.lda(abs(page::M51CTR));
    a.and(imm(RATE));
    a.beq(user);
    a.cmp(imm(byte(RATES.len())));
    a.bcc(in_table);
    a.lda(imm(byte(RATES.len())));
    a.bind(in_table);
    a.asl(Acc);
    a.tax();
    a.lda(abs(page::TVSFLG));
    a.beq(ntsc);
    // C clear from the ASL; PAL's times follow NTSC's.
    a.txa();
    a.adc(imm(byte(2 * RATES.len())));
    a.tax();
    a.bind(ntsc);
    a.lda(abs(sym("rs232_bit_times") + -2).x());
    a.ldy(abs(sym("rs232_bit_times") + -1).x());
    a.bne(set);
    a.bind(user);
    a.lda(abs(page::M51AJB));
    a.asl(Acc);
    a.tax();
    a.lda(abs(page::M51AJB + 1));
    a.rol(Acc);
    a.tay();
    a.txa();
    a.clc();
    a.adc(imm(2 * USER_RATE_OFFSET));
    a.bcc(set);
    a.iny();
    a.bind(set);
    a.sta(abs(page::BAUDOF));
    a.sty(abs(page::BAUDOF + 1));
}

/// A word's bits: in A from the control register, the bits it falls short
/// of a byte in X, C set; and in X with its parity bit.
fn frames(a: &mut Asm) {
    a.here("word_bits");
    a.lda(abs(page::M51CTR));
    a.and(imm(WORD_LENGTH));
    // Bits 6 and 5 rotated down to 1 and 0, C clear.
    a.asl(Acc);
    a.rol(Acc);
    a.rol(Acc);
    a.rol(Acc);
    a.tax();
    a.eor(imm(0xFF));
    a.adc(imm(9));
    a.rts();

    a.here("word_and_parity");
    a.jsr("word_bits");
    a.tax();
    a.lda(abs(page::M51CDR));
    a.and(imm(PARITY_ON));
    a.beq(RTS);
    a.inx();
    a.rts();
}

/// CLOSE: what is still to send sent, the NMIs and timers off, and the
/// buffers given back to the top of memory where they were taken, the top
/// then reported as OPEN reports it.
fn close(a: &mut Asm) {
    a.here("rs232_close");
    let freed = a.local();
    let draining = a.here_local();
    a.lda(abs(page::ENABL));
    a.lsr(Acc);
    a.bcs(draining);
    a.lda(imm(icr::TA | icr::TB | icr::FLAG));
    a.sta(abs(cia2(reg::ICR)));
    a.lda(imm(cr::ONE_SHOT));
    a.sta(abs(cia2(reg::CRA)));
    a.sta(abs(cia2(reg::CRB)));
    a.lda(imm(0));
    a.sta(abs(page::ENABL));
    a.ldy(abs(page::MEMSIZ + 1));
    a.ldx(z(zp::RIBUF + 1));
    a.beq(freed);
    a.sta(z(zp::RIBUF + 1));
    a.sta(z(zp::ROBUF + 1));
    a.iny();
    a.iny();
    a.bind(freed);
    a.ldx(abs(page::MEMSIZ));
    // C clear since the drain.
    a.bcc("rs232_top_reported");
}

/// GETIN and CHRIN: the next byte in, or 0 with RSSTAT saying the buffer
/// is empty, N and Z as the byte; CHROUT: the byte queued once there is
/// room, the transmitter started where it is idle. X and Y are kept, Y
/// through XSAV, which neither interrupt uses.
fn transfer(a: &mut Asm) {
    a.here("rs232_get");
    let (have, got) = (a.local(), a.local());
    a.sty(z(zp::XSAV));
    a.ldy(abs(page::RIDBS));
    a.cpy(abs(page::RIDBE));
    a.bne(have);
    a.lda(imm(rs_status::EMPTY));
    a.jsr("rs232_status");
    a.lda(imm(0));
    a.beq(got);
    a.bind(have);
    a.lda(z(zp::RIBUF).ind_y());
    a.inc(abs(page::RIDBS));
    a.bind(got);
    a.ldy(z(zp::XSAV));
    a.and(imm(0xFF));
    a.clc();
    a.rts();

    a.here("rs232_put");
    let (room, running) = (a.local(), a.local());
    a.pha();
    a.sty(z(zp::XSAV));
    a.bind(room);
    a.ldy(abs(page::RODBE));
    a.iny();
    a.cpy(abs(page::RODBS));
    a.beq(room);
    a.dey();
    a.pla();
    a.pha();
    a.sta(z(zp::ROBUF).ind_y());
    a.inc(abs(page::RODBE));
    a.lda(abs(page::ENABL));
    a.lsr(Acc);
    a.bcs(running);
    a.lda(abs(page::BAUDOF));
    a.sta(abs(cia2(reg::TA_LO)));
    a.lda(abs(page::BAUDOF + 1));
    a.sta(abs(cia2(reg::TA_HI)));
    a.lda(imm(cr::FORCE_LOAD | cr::START));
    a.sta(abs(cia2(reg::CRA)));
    a.lda(imm(icr::SET | icr::TA));
    a.sta(abs(cia2(reg::ICR)));
    a.ora(abs(page::ENABL));
    a.sta(abs(page::ENABL));
    a.bind(running);
    a.ldy(z(zp::XSAV));
    a.pla();
    a.clc();
    a.rts();
}

/// CIA2's NMI, its causes in A shifted out in turn: a bit sent on timer A,
/// a bit received on timer B, then a start bit on FLAG, run on into: its
/// timer starts six cycles sooner after the edge than a call's would, the
/// samples the nearer the bits' middles.
fn nmi(a: &mut Asm) {
    a.here("rs232_nmi");
    let (no_send, no_receive, exit) = (a.local(), a.local(), a.local());
    a.lsr(Acc);
    a.pha();
    a.bcc(no_send);
    a.jsr("rs232_send_bit");
    a.bind(no_send);
    a.pla();
    a.lsr(Acc);
    a.pha();
    a.bcc(no_receive);
    a.jsr("rs232_receive_bit");
    a.bind(no_receive);
    a.pla();
    a.and(imm(icr::FLAG >> 2));
    a.beq(exit);
    start_bit(a);
    a.bind(exit);
    a.jmp(abs(fixed::NMI_EXIT));
}

/// Sending: the frame's next bit onto TXD; at the frame's end the next
/// byte's start bit, sent before the rest of its frame is built so it is
/// a whole bit long, or the transmitter stopped.
fn sending(a: &mut Asm) {
    a.here("rs232_send_bit");
    let (shift, idle, mark, data, no_parity, bits) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.lda(z(zp::BITTS));
    a.bne(shift);
    a.ldy(abs(page::RODBS));
    a.cpy(abs(page::RODBE));
    a.beq(idle);
    a.lda(abs(cia2(reg::PRA)));
    a.and(imm(!TXD));
    a.sta(abs(cia2(reg::PRA)));
    // The rest of the frame in NXTBIT and RODATA, its first bit lowest: the
    // word, its parity, then stop bits and the idle line above.
    a.lda(z(zp::ROBUF).ind_y());
    a.inc(abs(page::RODBS));
    a.sta(z(zp::ROPRTY));
    a.jsr("word_bits");
    a.sta(z(zp::BITTS));
    a.txa();
    a.beq(data);
    let align = a.here_local();
    a.asl(z(zp::ROPRTY));
    a.dex();
    a.bne(align);
    a.bind(data);
    a.lda(imm(0xFF));
    a.sta(z(zp::RODATA));
    a.sta(z(zp::NXTBIT));
    a.lda(abs(page::M51CDR));
    a.and(imm(PARITY_ON));
    a.beq(no_parity);
    a.lda(z(zp::ROPRTY));
    a.jsr("rs232_parity");
    a.rol(z(zp::RODATA));
    a.rol(z(zp::NXTBIT));
    a.bind(no_parity);
    a.ldx(z(zp::BITTS));
    a.bind(bits);
    a.asl(z(zp::ROPRTY));
    a.rol(z(zp::RODATA));
    a.rol(z(zp::NXTBIT));
    a.dex();
    a.bne(bits);
    // The bits left: the word, its parity, a stop bit and C the second.
    a.jsr("word_and_parity");
    a.lda(abs(page::M51CTR));
    a.asl(Acc);
    a.txa();
    a.adc(imm(1));
    a.sta(z(zp::BITTS));
    a.rts();
    a.bind(shift);
    a.sec();
    a.ror(z(zp::NXTBIT));
    a.ror(z(zp::RODATA));
    a.lda(abs(cia2(reg::PRA)));
    a.and(imm(!TXD));
    a.bcc(mark);
    a.ora(imm(TXD));
    a.bind(mark);
    a.sta(abs(cia2(reg::PRA)));
    a.dec(z(zp::BITTS));
    a.rts();
    a.bind(idle);
    a.lda(imm(icr::TA));
    a.sta(abs(cia2(reg::ICR)));
    a.lsr(abs(page::ENABL));
    a.asl(abs(page::ENABL));
    a.lda(imm(cr::ONE_SHOT));
    a.sta(abs(cia2(reg::CRA)));
    a.rts();
}

/// C: the parity bit the command register asks for over A, whose bits
/// outside the word are clear; its bit 7 asks for a fixed bit, mark or
/// space, rather than odd or even. A and X clobbered.
fn parity(a: &mut Asm) {
    a.here("rs232_parity");
    let (fixed_bit, count, zero, odd_even) = (a.local(), a.local(), a.local(), a.local());
    // X 1 for odd parity and mark, 0 for even and space.
    a.ldx(imm(1));
    a.bit(abs(page::M51CDR));
    a.bmi(fixed_bit);
    a.bvc(count);
    a.dex();
    a.bind(count);
    // A 1 shifted in below the word ends the count once it is shifted out.
    a.sec();
    a.rol(Acc);
    let bit = a.here_local();
    a.bcc(zero);
    a.inx();
    a.bind(zero);
    a.asl(Acc);
    a.bne(bit);
    a.bind(odd_even);
    a.txa();
    a.lsr(Acc);
    a.rts();
    a.bind(fixed_bit);
    a.bvc(odd_even);
    a.clc();
    a.rts();
}

/// FLAG: a start bit, run on into from the NMI and on out to its end. FLAG
/// off, timer B to the middle of the word's first bit and then a bit at a
/// time, the samples counted: the word, its parity bit and a stop bit.
/// RIDATA is shifted through whole by the word and its alignment, so it
/// starts as it is.
fn start_bit(a: &mut Asm) {
    a.here("rs232_start_bit");
    a.lda(imm(icr::FLAG));
    a.sta(abs(cia2(reg::ICR)));
    a.lda(abs(page::BAUDOF + 1));
    a.lsr(Acc);
    a.tax();
    a.lda(abs(page::BAUDOF));
    a.ror(Acc);
    a.clc();
    a.adc(abs(page::BAUDOF));
    a.sta(abs(cia2(reg::TB_LO)));
    a.txa();
    a.adc(abs(page::BAUDOF + 1));
    a.sta(abs(cia2(reg::TB_HI)));
    a.lda(imm(cr::FORCE_LOAD | cr::START));
    a.sta(abs(cia2(reg::CRB)));
    a.lda(abs(page::BAUDOF));
    a.sta(abs(cia2(reg::TB_LO)));
    a.lda(abs(page::BAUDOF + 1));
    a.sta(abs(cia2(reg::TB_HI)));
    a.lda(imm(icr::SET | icr::TB));
    a.sta(abs(cia2(reg::ICR)));
    a.jsr("word_and_parity");
    a.inx();
    a.stx(z(zp::BITCI));
}

/// Timer B: RXD sampled into C. The word's bits go into RIDATA from the
/// top, the parity bit into INBIT's bit 0; the stop bit ends the frame,
/// the byte aligned, checked and put in the input buffer, and FLAG listens
/// again. `rs232_status`: A's bits set in RSSTAT.
fn receive_bit(a: &mut Asm) {
    a.here("rs232_receive_bit");
    let (data_bit, stop, framed, aligned, checked, overrun) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.lda(abs(cia2(reg::PRB)));
    a.lsr(Acc);
    a.dec(z(zp::BITCI));
    a.beq(stop);
    a.ldy(z(zp::BITCI));
    a.dey();
    a.bne(data_bit);
    a.lda(abs(page::M51CDR));
    a.and(imm(PARITY_ON));
    a.beq(data_bit);
    a.rol(z(zp::INBIT));
    a.rts();
    a.bind(data_bit);
    a.ror(z(zp::RIDATA));
    a.rts();
    a.bind(stop);
    a.bcs(framed);
    a.lda(imm(rs_status::FRAMING));
    a.jsr("rs232_status");
    a.bind(framed);
    a.lda(imm(icr::TB));
    a.sta(abs(cia2(reg::ICR)));
    a.lda(imm(cr::ONE_SHOT));
    a.sta(abs(cia2(reg::CRB)));
    a.lda(imm(icr::SET | icr::FLAG));
    a.sta(abs(cia2(reg::ICR)));
    a.jsr("word_bits");
    a.txa();
    a.beq(aligned);
    let shift = a.here_local();
    a.lsr(z(zp::RIDATA));
    a.dex();
    a.bne(shift);
    a.bind(aligned);
    a.lda(abs(page::M51CDR));
    a.and(imm(PARITY_ON));
    a.beq(checked);
    a.lda(z(zp::RIDATA));
    a.jsr("rs232_parity");
    // Bit 0 of INBIT plus C is set where the two differ.
    a.lda(z(zp::INBIT));
    a.adc(imm(0));
    a.lsr(Acc);
    a.bcc(checked);
    a.lda(imm(rs_status::PARITY));
    a.jsr("rs232_status");
    a.bind(checked);
    a.ldy(abs(page::RIDBE));
    a.iny();
    a.cpy(abs(page::RIDBS));
    a.beq(overrun);
    a.dey();
    a.lda(z(zp::RIDATA));
    a.sta(z(zp::RIBUF).ind_y());
    a.inc(abs(page::RIDBE));
    a.rts();
    a.bind(overrun);
    a.lda(imm(rs_status::OVERRUN));
    a.here("rs232_status");
    a.ora(abs(page::RSSTAT));
    a.sta(abs(page::RSSTAT));
    a.rts();
}
