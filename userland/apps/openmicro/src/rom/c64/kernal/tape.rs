//! The datasette: LOAD, SAVE and data files on tape in the original's
//! format, which its tapes and this KERNAL's read alike.
//!
//! A pulse is a cycle of the write line, short, medium or long; a bit is a
//! short then a medium pulse for 0, the reverse for 1; a byte is a long and
//! a medium pulse, eight bits from the least significant and an odd parity
//! bit. A block is a leader of short pulses, a countdown, its bytes and their
//! XOR, recorded twice: the first copy counting down from `$89` and ending
//! in a long and a short pulse, the second from `$09` after a short gap,
//! correcting the first's bad bytes. A file is a header block (its kind,
//! start, end and name in 192 bytes) and a block of its bytes; a data file
//! is a header and blocks of a type byte and 191 bytes, its last byte
//! followed by a zero.
//!
//! Interrupts stay off while the tape moves: pulses are written on timer B's
//! one-shot halves and timed from FLAG against it free-running, the screen
//! blanked so the VIC-II takes no cycles. STOP is polled, and unwinds to the
//! operation's caller through the stack pointer XSAV keeps from its entry.

use super::files::{message, status, NO_SECONDARY};
use super::keyboard::{shift, STOP_COLUMN, STOP_ROW};
use super::ws::{page, zp};
use crate::cpu::STACK_PAGE;
use crate::machine::c64::cia::{cr, icr, reg};
use crate::machine::c64::io::{self, cia1};
use crate::machine::c64::keyboard::Key;
use crate::machine::c64::memory::line;
use crate::machine::c64::serial::command;
use crate::machine::c64::vic::reg as vic;
use crate::rom::asm::{abs, imm, sym, zp as z, Acc, Asm, RTS};

/// The cycles between the write line's rising edges of a short, a medium
/// and a long pulse, as the original writes them.
const PULSES: [u16; 3] = [380, 534, 702];
/// The cycles a half's restart takes beyond its timer's count.
const HALF_OVERHEAD: u16 = 32;
/// Pulses' classes as `pulse` reads them, and its noise.
const SHORT: u8 = 0;
const MEDIUM: u8 = 1;
const LONG: u8 = 2;
/// Short pulses before a header, before a data block, and between a
/// block's copies.
const HEADER_LEADER: u16 = 27136;
const DATA_LEADER: u16 = 5376;
const GAP: u16 = 79;

/// Block kinds, a header's first byte.
pub mod kind {
    /// A program loaded where LOAD says.
    pub const RELOCATABLE: u8 = 1;
    /// A data file's block.
    pub const DATA: u8 = 2;
    /// A program loaded where it was saved.
    pub const ABSOLUTE: u8 = 3;
    /// A data file's header.
    pub const DATA_HEADER: u8 = 4;
    /// The tape's end, which SAVE with secondary address 2 marks.
    pub const END_OF_TAPE: u8 = 5;
}

/// The tape buffer's block: a header, or a data file's.
const BLOCK_BYTES: u8 = 192;
/// A header's start and end, and its name.
const ADDRESSES_AT: u8 = 1;
const NAME_AT: u8 = 5;
const NAME_BYTES: u8 = 16;
/// A data block's bytes, after its type.
const DATA_START: u8 = 1;
/// The countdowns' first bytes: a block's first copy's and its second's.
const FIRST_COPY: u8 = 0x89;
const SECOND_COPY: u8 = 0x09;
/// The first copy's bad bytes' addresses at the stack's foot, for the
/// second to replace; then the first copy's checksum and whether it was
/// sound.
const ERROR_LOG: u16 = STACK_PAGE;
const MAX_ERRORS: u8 = 30;
const LOG_FULL: u8 = 0xFF;
const KEPT_CHECKSUM: u16 = ERROR_LOG + 2 * MAX_ERRORS as u16;
const FIRST_PASS: u16 = KEPT_CHECKSUM + 1;
/// Jiffies FOUND waits for the Commodore key or the space bar.
const FOUND_PAUSE: u16 = 704;
/// A leader's short pulses in a row, and how many of the next are averaged
/// to time the tape, in the band of four-cycle units they fall in.
const LEADER_RUN: u8 = 16;
const CALIBRATION: u8 = 8;
const LEADER_LOW: u8 = 70;
const LEADER_HIGH: u8 = 125;
/// The read thresholds, in four-cycle units: short to medium, medium to
/// long, and long to noise.
const T_MEDIUM: u16 = zp::SVXT;
const T_LONG: u16 = zp::CMP0;
const T_NOISE: u16 = zp::CMP0 + 1;
/// A byte read whose parity failed, or one of whose bits was no bit: in
/// PRTY.
const BAD_BYTE: u8 = 0x81;
/// The screen's display enable in the VIC-II's first control register.
const DISPLAY: u8 = 0x10;

const CR1: u16 = io::vic(vic::CR1);

/// A pulse's halves in timer B's count.
const fn halves() -> [u8; 6] {
    let mut table = [0; 6];
    let mut pulse = 0;
    while pulse < PULSES.len() {
        let [low, high] = (PULSES[pulse] / 2 - HALF_OVERHEAD).to_le_bytes();
        table[2 * pulse] = low;
        table[2 * pulse + 1] = high;
        pulse += 1;
    }
    table
}

pub fn emit(a: &mut Asm) {
    prompts(a);
    hardware(a);
    writing(a);
    byte_writing(a);
    block_writing(a);
    reading(a);
    leader(a);
    byte_reading(a);
    blocks(a);
    copy_data(a);
    take_byte(a);
    headers(a);
    find_header(a);
    header_writing(a);
    tape_load(a);
    tape_save(a);
    tape_open(a);
    tape_get(a);
    tape_put(a);
    tape_close(a);
    a.later(|a| {
        a.here("tape_halves");
        a.bytes(&halves());
        a.here("quarter_high");
        a.bytes(&[0x00, 0x40, 0x80, 0xC0]);
    });
}

/// PRESS PLAY ON TAPE, or RECORD and PLAY, until a key of the deck is
/// down, STOP abandoning; FOUND and the header's name, and its pause.
fn prompts(a: &mut Asm) {
    for (name, prompt) in [
        ("press_play", message::PRESS_PLAY),
        ("press_record", message::PRESS_RECORD),
    ] {
        a.here(name);
        let (wait, pressed) = (a.local(), a.local());
        a.jsr("deck_key_down");
        a.beq(pressed);
        a.ldy(imm(prompt));
        a.jsr("control_message");
        a.bind(wait);
        a.jsr("STOP");
        a.beq("tape_prompt_stopped");
        a.jsr("deck_key_down");
        a.bne(wait);
        a.ldy(imm(message::OK));
        a.jsr("control_message");
        a.bind(pressed);
        a.rts();
    }
    a.here("tape_prompt_stopped");
    a.ldx(z(zp::XSAV));
    a.txs();
    a.jmp("error_stopped");

    // Z set while a key of the deck is down.
    a.here("deck_key_down");
    a.lda(z(1));
    a.and(imm(line::CASSETTE_SENSE));
    a.rts();

    a.here("found");
    let name = a.local();
    a.bit(z(zp::MSGFLG));
    a.bpl(RTS);
    a.ldy(imm(message::FOUND));
    a.jsr("print_message");
    a.ldy(imm(NAME_AT));
    a.bind(name);
    a.lda(z(zp::TAPE1).ind_y());
    a.jsr("screen_out");
    a.iny();
    a.cpy(imm(NAME_AT + NAME_BYTES));
    a.bne(name);
    a.rts();

    // While control messages show, [`FOUND_PAUSE`] jiffies or until the
    // Commodore key or the space bar.
    a.here("found_pause");
    let (jiffy, tick, borrowed, waited) = (a.local(), a.local(), a.local(), a.local());
    a.bit(z(zp::MSGFLG));
    a.bpl(RTS);
    let [low, high] = FOUND_PAUSE.to_le_bytes();
    a.lda(imm(low));
    a.sta(z(zp::TEMP));
    a.lda(imm(high));
    a.sta(z(zp::TEMP + 1));
    a.bind(jiffy);
    a.lda(abs(page::SHFLAG));
    a.and(imm(shift::COMMODORE));
    a.bne(waited);
    a.lda(z(zp::SFDX));
    a.cmp(imm(Key::SPACE.code()));
    a.beq(waited);
    a.jsr("STOP");
    a.beq("tape_prompt_stopped");
    a.lda(z(zp::TIME + 2));
    a.bind(tick);
    a.cmp(z(zp::TIME + 2));
    a.beq(tick);
    a.lda(z(zp::TEMP));
    a.bne(borrowed);
    a.dec(z(zp::TEMP + 1));
    a.bind(borrowed);
    a.dec(z(zp::TEMP));
    a.lda(z(zp::TEMP));
    a.ora(z(zp::TEMP + 1));
    a.bne(jiffy);
    a.bind(waited);
    a.rts();
}

/// The tape's hardware taken for a block and given back: interrupts and
/// CIA1's causes off, the motor on and the screen blanked; afterwards the
/// motor off and held off while the deck's key stays down, the screen and
/// the jiffy interrupt back. STOP while the tape moves gives them back and
/// unwinds.
fn hardware(a: &mut Asm) {
    a.here("tape_begin");
    a.sei();
    a.lda(imm(!icr::SET));
    a.sta(abs(cia1(reg::ICR)));
    a.lda(imm(1));
    a.sta(z(zp::CAS1));
    a.lda(z(1));
    a.and(imm(!line::CASSETTE_MOTOR));
    a.sta(z(1));
    a.lda(abs(CR1));
    a.and(imm(!DISPLAY));
    a.sta(abs(CR1));
    a.lda(imm(STOP_COLUMN));
    a.sta(abs(cia1(reg::PRA)));
    a.lda(imm(cr::ONE_SHOT));
    a.sta(abs(cia1(reg::CRB)));
    // The blanking takes from the next frame: until then bad lines still
    // take cycles.
    let (lower, upper) = (a.local(), a.local());
    a.bind(lower);
    a.bit(abs(CR1));
    a.bpl(lower);
    a.bind(upper);
    a.bit(abs(CR1));
    a.bmi(upper);
    a.rts();

    a.here("tape_end");
    a.lda(z(1));
    a.ora(imm(line::CASSETTE_MOTOR));
    a.sta(z(1));
    a.lda(abs(CR1));
    a.ora(imm(DISPLAY));
    a.sta(abs(CR1));
    a.lda(imm(cr::ONE_SHOT));
    a.sta(abs(cia1(reg::CRB)));
    a.lda(abs(cia1(reg::ICR)));
    a.jsr("start_jiffies");
    a.cli();
    a.rts();

    a.here("tape_stop_check");
    a.lda(abs(cia1(reg::PRB)));
    a.cmp(imm(STOP_ROW));
    a.bne(RTS);
    a.jsr("tape_end");
    a.lda(imm(0));
    a.sta(z(zp::NDX));
    a.jmp("tape_prompt_stopped");
}

/// A pulse written as two halves on timer B, the write line toggled as each
/// starts, and a bit as two pulses.
fn writing(a: &mut Asm) {
    // Pulse class A.
    a.here("write_pulse");
    a.asl(Acc);
    a.tax();
    a.ldy(abs(sym("tape_halves") + 1).x());
    a.lda(abs("tape_halves").x());
    a.tax();
    a.jsr("write_half");
    a.here("write_half");
    let wait = a.local();
    a.bind(wait);
    a.lda(abs(cia1(reg::CRB)));
    a.lsr(Acc);
    a.bcs(wait);
    a.lda(z(1));
    a.eor(imm(line::CASSETTE_WRITE));
    a.sta(z(1));
    a.stx(abs(cia1(reg::TB_LO)));
    a.sty(abs(cia1(reg::TB_HI)));
    a.lda(imm(cr::FORCE_LOAD | cr::ONE_SHOT | cr::START));
    a.sta(abs(cia1(reg::CRB)));
    a.rts();

    // The bit in C, a one counted in PRTY.
    a.here("write_bit");
    let one = a.local();
    a.bcs(one);
    a.lda(imm(SHORT));
    a.jsr("write_pulse");
    a.lda(imm(MEDIUM));
    a.jmp("write_pulse");
    a.bind(one);
    a.inc(z(zp::PRTY));
    a.lda(imm(MEDIUM));
    a.jsr("write_pulse");
    a.lda(imm(SHORT));
    a.jmp("write_pulse");
}

/// The byte in A written after its marker, then the parity bit making the
/// ones odd; and a leader.
fn byte_writing(a: &mut Asm) {
    a.here("write_byte");
    let bits = a.local();
    a.sta(z(zp::MYCH));
    a.jsr("tape_stop_check");
    a.lda(imm(LONG));
    a.jsr("write_pulse");
    a.lda(imm(MEDIUM));
    a.jsr("write_pulse");
    a.lda(imm(1));
    a.sta(z(zp::PRTY));
    a.lda(imm(8));
    a.sta(z(zp::CNTDN));
    a.bind(bits);
    a.lsr(z(zp::MYCH));
    a.jsr("write_bit");
    a.dec(z(zp::CNTDN));
    a.bne(bits);
    a.lda(z(zp::PRTY));
    a.lsr(Acc);
    a.jmp("write_bit");

    // TEMP short pulses.
    a.here("write_leader");
    let (pulse, borrowed) = (a.local(), a.local());
    a.bind(pulse);
    a.jsr("tape_stop_check");
    a.lda(imm(SHORT));
    a.jsr("write_pulse");
    a.lda(z(zp::TEMP));
    a.bne(borrowed);
    a.dec(z(zp::TEMP + 1));
    a.bind(borrowed);
    a.dec(z(zp::TEMP));
    a.lda(z(zp::TEMP));
    a.ora(z(zp::TEMP + 1));
    a.bne(pulse);
    a.rts();
}

/// STAL to EAL after a leader of A and X short pulses: the first copy,
/// its end marker, the gap, the second copy and a pulse ending it.
fn block_writing(a: &mut Asm) {
    a.here("write_block");
    a.sta(z(zp::TEMP));
    a.stx(z(zp::TEMP + 1));
    a.jsr("write_leader");
    a.lda(imm(FIRST_COPY));
    a.jsr("write_copy");
    a.lda(imm(LONG));
    a.jsr("write_pulse");
    a.lda(imm(SHORT));
    a.jsr("write_pulse");
    let [gap_low, gap_high] = GAP.to_le_bytes();
    a.lda(imm(gap_low));
    a.sta(z(zp::TEMP));
    a.lda(imm(gap_high));
    a.sta(z(zp::TEMP + 1));
    a.jsr("write_leader");
    a.lda(imm(SECOND_COPY));
    a.jsr("write_copy");
    a.lda(imm(SHORT));
    a.jmp("write_pulse");

    // The countdown from A, the bytes from STAL to EAL and their XOR.
    a.here("write_copy");
    let (countdown, data, sum, stepped) = (a.local(), a.local(), a.local(), a.local());
    a.sta(z(zp::SYNO));
    a.bind(countdown);
    a.lda(z(zp::SYNO));
    a.jsr("write_byte");
    a.dec(z(zp::SYNO));
    a.lda(z(zp::SYNO));
    a.and(imm(0x7F));
    a.bne(countdown);
    a.lda(z(zp::STAL));
    a.sta(z(zp::SAL));
    a.lda(z(zp::STAL + 1));
    a.sta(z(zp::SAL + 1));
    a.lda(imm(0));
    a.sta(z(zp::DPSW));
    a.bind(data);
    a.lda(z(zp::SAL));
    a.cmp(z(zp::EAL));
    a.lda(z(zp::SAL + 1));
    a.sbc(z(zp::EAL + 1));
    a.bcs(sum);
    a.ldy(imm(0));
    a.lda(z(zp::SAL).ind_y());
    a.pha();
    a.eor(z(zp::DPSW));
    a.sta(z(zp::DPSW));
    a.pla();
    a.jsr("write_byte");
    a.inc(z(zp::SAL));
    a.bne(stepped);
    a.inc(z(zp::SAL + 1));
    a.bind(stepped);
    a.jmp(data);
    a.bind(sum);
    a.lda(z(zp::DPSW));
    a.jmp("write_byte");
}

/// A pulse timed from FLAG on timer B, STOP polled while none comes, and
/// classed.
fn reading(a: &mut Asm) {
    // A: the pulse's length in four-cycle units, `$FF` for longer.
    a.here("pulse_length");
    let (wait, edge, long) = (a.local(), a.local(), a.local());
    a.ldx(imm(0));
    a.bind(wait);
    a.lda(abs(cia1(reg::ICR)));
    a.and(imm(icr::FLAG));
    a.bne(edge);
    a.dex();
    a.bne(wait);
    a.jsr("tape_stop_check");
    a.jmp(wait);
    a.bind(edge);
    let reread = a.here_local();
    a.ldy(abs(cia1(reg::TB_HI)));
    a.lda(abs(cia1(reg::TB_LO)));
    a.cpy(abs(cia1(reg::TB_HI)));
    a.bne(reread);
    a.tax();
    a.sec();
    a.lda(z(zp::TEMP));
    a.stx(z(zp::TEMP));
    a.sbc(z(zp::TEMP));
    a.pha();
    a.lda(z(zp::TEMP + 1));
    a.sty(z(zp::TEMP + 1));
    a.sbc(z(zp::TEMP + 1));
    a.tax();
    a.pla();
    a.cpx(imm(4));
    a.bcs(long);
    a.lsr(Acc);
    a.lsr(Acc);
    a.ora(abs("quarter_high").x());
    a.rts();
    a.bind(long);
    a.lda(imm(0xFF));
    a.rts();

    // The next pulse's class in A, 3 for noise.
    a.here("pulse");
    let class = a.local();
    a.jsr("pulse_length");
    a.ldx(imm(SHORT));
    a.cmp(z(T_MEDIUM));
    a.bcc(class);
    a.inx();
    a.cmp(z(T_LONG));
    a.bcc(class);
    a.inx();
    a.cmp(z(T_NOISE));
    a.bcc(class);
    a.inx();
    a.bind(class);
    a.txa();
    a.rts();
}

/// A leader's run of short pulses, then the tape timed from eight more:
/// their mean S sets the thresholds at 1.20 S, 1.62 S and 2.25 S.
fn leader(a: &mut Asm) {
    a.here("find_leader");
    let (restart, run, sum, carried, set) = (a.local(), a.local(), a.local(), a.local(), a.local());
    a.bind(restart);
    a.lda(imm(0));
    a.sta(z(zp::CNTDN));
    a.bind(run);
    a.jsr("pulse_length");
    a.cmp(imm(LEADER_LOW));
    a.bcc(restart);
    a.cmp(imm(LEADER_HIGH));
    a.bcs(restart);
    a.inc(z(zp::CNTDN));
    a.lda(z(zp::CNTDN));
    a.cmp(imm(LEADER_RUN));
    a.bne(run);
    a.lda(imm(0));
    a.sta(z(zp::MYCH));
    a.sta(z(zp::PRTY));
    a.lda(imm(CALIBRATION));
    a.sta(z(zp::CNTDN));
    a.bind(sum);
    a.jsr("pulse_length");
    a.cmp(imm(LEADER_LOW));
    a.bcc(restart);
    a.cmp(imm(LEADER_HIGH));
    a.bcs(restart);
    a.clc();
    a.adc(z(zp::MYCH));
    a.sta(z(zp::MYCH));
    a.bcc(carried);
    a.inc(z(zp::PRTY));
    a.bind(carried);
    a.dec(z(zp::CNTDN));
    a.bne(sum);
    for _ in 0..3 {
        a.lsr(z(zp::PRTY));
        a.ror(z(zp::MYCH));
    }
    // PRTY: S / 8.
    a.lda(z(zp::MYCH));
    a.lsr(Acc);
    a.lsr(Acc);
    a.lsr(Acc);
    a.sta(z(zp::PRTY));
    a.lsr(Acc);
    a.pha();
    a.lsr(Acc);
    a.lsr(Acc);
    a.clc();
    a.adc(z(zp::MYCH));
    a.adc(z(zp::PRTY));
    a.sta(z(T_MEDIUM));
    a.pla();
    a.clc();
    a.adc(z(T_MEDIUM));
    a.sta(z(T_MEDIUM));
    a.lda(z(zp::MYCH));
    a.lsr(Acc);
    a.clc();
    a.adc(z(zp::MYCH));
    a.adc(z(zp::PRTY));
    a.sta(z(T_LONG));
    let overflow = a.local();
    a.lda(z(zp::MYCH));
    a.asl(Acc);
    a.bcs(overflow);
    a.adc(z(zp::PRTY));
    a.bcs(overflow);
    a.adc(z(zp::PRTY));
    a.bcc(set);
    a.bind(overflow);
    a.lda(imm(0xFF));
    a.bind(set);
    a.sta(z(T_NOISE));
    a.rts();
}

/// A byte's marker found after a leader, the marker or the block's end
/// read between bytes, and a byte read.
fn byte_reading(a: &mut Asm) {
    // Pulses until a byte's marker, a long then a medium one.
    a.here("find_marker");
    a.jsr("pulse");
    a.cmp(imm(LONG));
    a.bne("find_marker");
    a.jsr("pulse");
    a.cmp(imm(MEDIUM));
    a.bne("find_marker");
    a.rts();

    // The next two pulses: X 0 where they are a byte's marker, 1 where
    // they end the block.
    a.here("next_marker");
    let ended = a.local();
    a.jsr("pulse");
    a.cmp(imm(LONG));
    a.bne(ended);
    a.jsr("pulse");
    a.cmp(imm(MEDIUM));
    a.bne(ended);
    a.ldx(imm(0));
    a.rts();
    a.bind(ended);
    a.ldx(imm(1));
    a.rts();

    // The byte after a marker into MYCH, PRTY 0 where it read soundly.
    a.here("read_byte");
    let (bit, one, zero, shift_in, parity) =
        (a.local(), a.local(), a.local(), a.local(), a.local());
    a.lda(imm(1));
    a.sta(z(zp::PRTY));
    a.lda(imm(9));
    a.sta(z(zp::CNTDN));
    a.bind(bit);
    a.jsr("pulse");
    a.asl(Acc);
    a.asl(Acc);
    a.sta(z(zp::SYNO));
    a.jsr("pulse");
    a.ora(z(zp::SYNO));
    a.cmp(imm(4 * SHORT + MEDIUM));
    a.beq(zero);
    a.cmp(imm(4 * MEDIUM + SHORT));
    a.beq(one);
    a.lda(imm(0x80));
    a.ora(z(zp::PRTY));
    a.sta(z(zp::PRTY));
    a.bind(zero);
    a.clc();
    a.bcc(shift_in);
    a.bind(one);
    a.inc(z(zp::PRTY));
    a.sec();
    a.bind(shift_in);
    a.dec(z(zp::CNTDN));
    a.beq(parity);
    a.ror(z(zp::MYCH));
    a.jmp(bit);
    a.bind(parity);
    a.lda(z(zp::PRTY));
    a.and(imm(BAD_BYTE));
    a.sta(z(zp::PRTY));
    a.rts();
}

/// A block read from STAL to EAL: its first copy stored, or compared in a
/// verify, its bad bytes logged; its second copy replacing them, the XOR
/// of what memory then holds checked. STATUS records what failed.
fn blocks(a: &mut Asm) {
    a.here("read_block");
    let (first, second) = (a.local(), a.local());
    a.lda(imm(0xFF));
    a.sta(abs(cia1(reg::TB_LO)));
    a.sta(abs(cia1(reg::TB_HI)));
    a.lda(imm(cr::FORCE_LOAD | cr::START));
    a.sta(abs(cia1(reg::CRB)));
    a.lda(imm(0));
    a.sta(z(zp::PTR1));
    a.sta(z(zp::PTR2));
    a.lda(imm(1));
    a.sta(abs(FIRST_PASS));
    a.bind(first);
    a.jsr("copy_head");
    a.lda(z(zp::FSBLK));
    a.bpl(first);
    a.jsr("copy_data");
    a.bind(second);
    a.jsr("copy_head");
    a.lda(z(zp::FSBLK));
    a.bmi(second);
    a.jmp("copy_data");

    // A copy's leader and countdown: FSBLK its first byte.
    a.here("copy_head");
    let (start, counting) = (a.local(), a.local());
    a.bind(start);
    a.jsr("find_leader");
    a.jsr("find_marker");
    a.jsr("read_byte");
    a.lda(z(zp::PRTY));
    a.bne(start);
    a.lda(z(zp::MYCH));
    a.cmp(imm(FIRST_COPY));
    a.beq(counting);
    a.cmp(imm(SECOND_COPY));
    a.bne(start);
    a.bind(counting);
    a.sta(z(zp::FSBLK));
    a.and(imm(0x7F));
    a.sta(z(zp::DPSW));
    let rest = a.here_local();
    a.dec(z(zp::DPSW));
    a.beq(RTS);
    a.jsr("next_marker");
    a.cpx(imm(0));
    a.bne(start);
    a.jsr("read_byte");
    a.jmp(rest);
}

/// The copy's bytes and its checksum, each after its marker.
fn copy_data(a: &mut Asm) {
    a.here("copy_data");
    let (data, short, sum, stepped, long, first_end, unsound, compare, kept, sound) = (
        a.local(),
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
    a.lda(z(zp::STAL));
    a.sta(z(zp::SAL));
    a.lda(z(zp::STAL + 1));
    a.sta(z(zp::SAL + 1));
    a.lda(imm(0));
    a.sta(z(zp::DPSW));
    a.bind(data);
    a.lda(z(zp::SAL));
    a.cmp(z(zp::EAL));
    a.lda(z(zp::SAL + 1));
    a.sbc(z(zp::EAL + 1));
    a.bcs(sum);
    a.jsr("next_marker");
    a.cpx(imm(0));
    a.bne(short);
    a.jsr("read_byte");
    a.jsr("take_byte");
    a.inc(z(zp::SAL));
    a.bne(stepped);
    a.inc(z(zp::SAL + 1));
    a.bind(stepped);
    a.jmp(data);
    a.bind(short);
    a.lda(imm(status::SHORT_BLOCK));
    a.jmp("set_status");
    a.bind(sum);
    a.jsr("next_marker");
    a.cpx(imm(0));
    a.bne(short);
    a.jsr("read_byte");
    a.bit(z(zp::FSBLK));
    a.bpl(compare);
    a.jsr("next_marker");
    a.cpx(imm(0));
    a.beq(long);
    a.bind(first_end);
    a.lda(z(zp::MYCH));
    a.sta(abs(KEPT_CHECKSUM));
    a.lda(z(zp::PRTY));
    a.ora(z(zp::PTR1));
    a.bne(unsound);
    a.lda(z(zp::MYCH));
    a.cmp(z(zp::DPSW));
    a.bne(unsound);
    a.lda(imm(0));
    a.sta(abs(FIRST_PASS));
    a.rts();
    a.bind(unsound);
    a.lda(imm(1));
    a.sta(abs(FIRST_PASS));
    a.rts();
    a.bind(long);
    a.lda(imm(status::LONG_BLOCK));
    a.jsr("set_status");
    a.jmp(first_end);
    // The second copy's end: unless the first was sound, memory's XOR
    // against this copy's checksum, or the first's where this one's is
    // bad.
    a.bind(compare);
    a.lda(abs(FIRST_PASS));
    a.beq(sound);
    a.lda(z(zp::PRTY));
    a.beq(kept);
    a.lda(abs(KEPT_CHECKSUM));
    a.sta(z(zp::MYCH));
    a.bind(kept);
    a.lda(z(zp::MYCH));
    a.cmp(z(zp::DPSW));
    a.beq(sound);
    a.lda(imm(status::CHECKSUM));
    a.jsr("set_status");
    a.bind(sound);
    a.rts();
}

/// The byte read at SAL, by copy: the first's stored or logged, the
/// second's taken where the first's was bad; X kept.
fn take_byte(a: &mut Asm) {
    a.here("take_byte");
    let (second_copy, log, full, needed, fixed, summed) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.bit(z(zp::FSBLK));
    a.bpl(second_copy);
    a.lda(z(zp::PRTY));
    a.bne(log);
    a.lda(z(zp::MYCH));
    a.eor(z(zp::DPSW));
    a.sta(z(zp::DPSW));
    a.jmp("deliver_byte");
    a.bind(log);
    a.ldy(z(zp::PTR1));
    a.cpy(imm(2 * MAX_ERRORS));
    a.bcs(full);
    a.lda(z(zp::SAL));
    a.sta(abs(ERROR_LOG).y());
    a.lda(z(zp::SAL + 1));
    a.sta(abs(ERROR_LOG + 1).y());
    a.iny();
    a.iny();
    a.sty(z(zp::PTR1));
    a.rts();
    a.bind(full);
    a.lda(imm(LOG_FULL));
    a.sta(z(zp::PTR1));
    a.rts();
    a.bind(second_copy);
    a.lda(abs(FIRST_PASS));
    a.beq(summed);
    a.lda(z(zp::PTR1));
    a.cmp(imm(LOG_FULL));
    a.beq(needed);
    a.ldy(z(zp::PTR2));
    a.cpy(z(zp::PTR1));
    a.bcs(summed);
    a.lda(abs(ERROR_LOG).y());
    a.cmp(z(zp::SAL));
    a.bne(summed);
    a.lda(abs(ERROR_LOG + 1).y());
    a.cmp(z(zp::SAL + 1));
    a.bne(summed);
    a.iny();
    a.iny();
    a.sty(z(zp::PTR2));
    a.bind(needed);
    a.lda(z(zp::PRTY));
    a.beq(fixed);
    a.lda(imm(status::MISMATCH));
    a.jsr("set_status");
    a.jmp(summed);
    a.bind(fixed);
    a.jsr("deliver_byte");
    a.bind(summed);
    a.ldy(imm(0));
    a.lda(z(zp::SAL).ind_y());
    a.eor(z(zp::DPSW));
    a.sta(z(zp::DPSW));
    a.rts();

    // MYCH at SAL: stored, or compared in a verify.
    a.here("deliver_byte");
    let verify = a.local();
    a.lda(z(zp::MYCH));
    a.ldy(z(zp::VERCK));
    a.bne(verify);
    a.sta(z(zp::SAL).ind_y());
    a.rts();
    a.bind(verify);
    a.ldy(imm(0));
    a.cmp(z(zp::SAL).ind_y());
    a.beq(RTS);
    a.lda(imm(status::MISMATCH));
    a.jmp("set_status");
}

/// The tape buffer made the range a block covers, the program's range kept
/// aside meanwhile, and the program's range back.
fn headers(a: &mut Asm) {
    // STAL and EAL pushed under the return address, the buffer's range in
    // their place.
    a.here("buffer_range");
    a.pla();
    a.sta(z(zp::MYCH));
    a.pla();
    a.sta(z(zp::PRTY));
    for at in [zp::STAL, zp::STAL + 1, zp::EAL, zp::EAL + 1] {
        a.lda(z(at));
        a.pha();
    }
    a.lda(z(zp::PRTY));
    a.pha();
    a.lda(z(zp::MYCH));
    a.pha();
    a.lda(z(zp::TAPE1));
    a.sta(z(zp::STAL));
    a.clc();
    a.adc(imm(BLOCK_BYTES));
    a.sta(z(zp::EAL));
    a.lda(z(zp::TAPE1 + 1));
    a.sta(z(zp::STAL + 1));
    a.adc(imm(0));
    a.sta(z(zp::EAL + 1));
    a.rts();

    a.here("program_range");
    a.pla();
    a.sta(z(zp::MYCH));
    a.pla();
    a.sta(z(zp::PRTY));
    for at in [zp::EAL + 1, zp::EAL, zp::STAL + 1, zp::STAL] {
        a.pla();
        a.sta(z(at));
    }
    a.lda(z(zp::PRTY));
    a.pha();
    a.lda(z(zp::MYCH));
    a.pha();
    a.rts();
}

/// A header of A's kind, a data file's or 0 for a program's, its name
/// the one SETNAM gave where there is one; C set at the tape's end. A
/// header is stored even in a verify, VERCK kept for what follows.
fn find_header(a: &mut Asm) {
    a.here("find_header");
    let (search, wanted, program, name, matched, end_of_tape) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.tax();
    a.lda(z(zp::VERCK));
    a.pha();
    a.jsr("buffer_range");
    a.txa();
    a.pha();
    a.bind(search);
    a.lda(z(zp::STATUS));
    a.and(imm(!(status::SHORT_BLOCK
        | status::LONG_BLOCK
        | status::MISMATCH
        | status::CHECKSUM)));
    a.sta(z(zp::STATUS));
    a.lda(imm(0));
    a.sta(z(zp::VERCK));
    a.jsr("read_block");
    a.lda(z(zp::STATUS));
    a.and(imm(status::SHORT_BLOCK
        | status::LONG_BLOCK
        | status::MISMATCH
        | status::CHECKSUM));
    a.bne(search);
    a.ldy(imm(0));
    a.lda(z(zp::TAPE1).ind_y());
    a.cmp(imm(kind::END_OF_TAPE));
    a.beq(end_of_tape);
    a.tax();
    a.pla();
    a.pha();
    a.bne(wanted);
    a.cpx(imm(kind::RELOCATABLE));
    a.beq(program);
    a.cpx(imm(kind::ABSOLUTE));
    a.bne(search);
    a.beq(program);
    a.bind(wanted);
    a.stx(z(zp::SYNO));
    a.cmp(z(zp::SYNO));
    a.bne(search);
    a.bind(program);
    a.jsr("found");
    a.ldy(imm(0));
    a.bind(name);
    a.cpy(z(zp::FNLEN));
    a.beq(matched);
    a.cpy(imm(NAME_BYTES));
    a.beq(matched);
    a.lda(z(zp::FNADR).ind_y());
    a.sty(z(zp::SYNO));
    a.pha();
    a.tya();
    a.clc();
    a.adc(imm(NAME_AT));
    a.tay();
    a.pla();
    a.cmp(z(zp::TAPE1).ind_y());
    a.bne(search);
    a.ldy(z(zp::SYNO));
    a.iny();
    a.bne(name);
    a.bind(matched);
    a.pla();
    a.jsr("program_range");
    a.pla();
    a.sta(z(zp::VERCK));
    a.clc();
    a.rts();
    a.bind(end_of_tape);
    a.pla();
    a.jsr("program_range");
    a.pla();
    a.sta(z(zp::VERCK));
    a.sec();
    a.rts();
}

/// A header made in the tape buffer, its kind in A, STAL and EAL, the name
/// SETNAM gave padded with spaces and spaces to the block's end; and the
/// buffer written as a header.
fn header_writing(a: &mut Asm) {
    a.here("make_header");
    let (name, padded, rest) = (a.local(), a.local(), a.local());
    a.ldy(imm(0));
    a.sta(z(zp::TAPE1).ind_y());
    for (offset, at) in (ADDRESSES_AT..).zip([zp::STAL, zp::STAL + 1, zp::EAL, zp::EAL + 1]) {
        a.ldy(imm(offset));
        a.lda(z(at));
        a.sta(z(zp::TAPE1).ind_y());
    }
    a.ldx(imm(0));
    a.ldy(imm(NAME_AT));
    a.bind(name);
    a.cpx(z(zp::FNLEN));
    a.beq(padded);
    a.cpx(imm(NAME_BYTES));
    a.beq(padded);
    a.sty(z(zp::SYNO));
    a.txa();
    a.tay();
    a.lda(z(zp::FNADR).ind_y());
    a.ldy(z(zp::SYNO));
    a.sta(z(zp::TAPE1).ind_y());
    a.inx();
    a.iny();
    a.bne(name);
    a.bind(padded);
    a.lda(imm(b' '));
    a.bind(rest);
    a.sta(z(zp::TAPE1).ind_y());
    a.iny();
    a.cpy(imm(BLOCK_BYTES));
    a.bne(rest);
    a.rts();

    // The tape buffer written as a header.
    a.here("write_header");
    a.jsr("buffer_range");
    a.jsr("tape_begin");
    let [low, high] = HEADER_LEADER.to_le_bytes();
    a.lda(imm(low));
    a.ldx(imm(high));
    a.jsr("write_block");
    a.jsr("tape_end");
    a.jsr("program_range");
    a.rts();
}

/// LOAD from tape: the program's header found and FOUND's pause, then its
/// block where the header or MEMUSS says.
fn tape_load(a: &mut Asm) {
    a.here("tape_load");
    let (found, absolute, read) = (a.local(), a.local(), a.local());
    a.tsx();
    a.stx(z(zp::XSAV));
    a.jsr("press_play");
    a.jsr("searching");
    a.jsr("tape_begin");
    a.lda(imm(0));
    a.jsr("find_header");
    a.bcc(found);
    a.jsr("tape_end");
    a.jmp("error_file_not_found");
    a.bind(found);
    a.jsr("tape_end");
    a.jsr("found_pause");
    a.jsr("loading_message");
    for (offset, at) in (ADDRESSES_AT..).zip([zp::STAL, zp::STAL + 1, zp::EAL, zp::EAL + 1]) {
        a.ldy(imm(offset));
        a.lda(z(zp::TAPE1).ind_y());
        a.sta(z(at));
    }
    a.ldy(imm(0));
    a.lda(z(zp::TAPE1).ind_y());
    a.cmp(imm(kind::ABSOLUTE));
    a.beq(absolute);
    a.lda(z(zp::SA));
    a.bne(absolute);
    a.sec();
    a.lda(z(zp::EAL));
    a.sbc(z(zp::STAL));
    a.tax();
    a.lda(z(zp::EAL + 1));
    a.sbc(z(zp::STAL + 1));
    a.tay();
    a.clc();
    a.txa();
    a.adc(z(zp::MEMUSS));
    a.sta(z(zp::EAL));
    a.tya();
    a.adc(z(zp::MEMUSS + 1));
    a.sta(z(zp::EAL + 1));
    a.lda(z(zp::MEMUSS));
    a.sta(z(zp::STAL));
    a.lda(z(zp::MEMUSS + 1));
    a.sta(z(zp::STAL + 1));
    a.bind(absolute);
    a.bind(read);
    a.jsr("tape_begin");
    a.jsr("read_block");
    a.jsr("tape_end");
    a.ldx(z(zp::EAL));
    a.ldy(z(zp::EAL + 1));
    a.clc();
    a.rts();
}

/// SAVE to tape: the header, kind 3 for secondary address 1 and else 1,
/// the program's block, and with secondary address 2 the tape's end.
fn tape_save(a: &mut Asm) {
    a.here("tape_save");
    let no_end = a.local();
    a.tsx();
    a.stx(z(zp::XSAV));
    a.jsr("press_record");
    a.jsr("saving_message");
    a.lda(z(zp::SA));
    a.and(imm(1));
    a.asl(Acc);
    a.ora(imm(kind::RELOCATABLE));
    a.jsr("make_header");
    a.jsr("write_header");
    a.jsr("tape_begin");
    let [low, high] = DATA_LEADER.to_le_bytes();
    a.lda(imm(low));
    a.ldx(imm(high));
    a.jsr("write_block");
    a.jsr("tape_end");
    a.lda(z(zp::SA));
    a.and(imm(2));
    a.beq(no_end);
    a.lda(imm(kind::END_OF_TAPE));
    a.ldy(imm(0));
    a.sta(z(zp::TAPE1).ind_y());
    a.jsr("write_header");
    a.bind(no_end);
    a.clc();
    a.rts();
}

/// OPEN: secondary address 0 or none reads, its header found; 1 writes,
/// 2 writes and marks the tape's end at CLOSE.
fn tape_open(a: &mut Asm) {
    a.here("tape_open");
    let (mode, writing, found_data) = (a.local(), a.local(), a.local());
    a.tsx();
    a.stx(z(zp::XSAV));
    a.lda(z(zp::SA));
    a.cmp(imm(NO_SECONDARY));
    a.bne(mode);
    a.lda(imm(command::DATA));
    a.sta(z(zp::SA));
    a.ldx(z(zp::LDTND));
    a.sta(abs(page::SAT - 1).x());
    a.bind(mode);
    a.lda(z(zp::SA));
    a.and(imm(0x0F));
    a.bne(writing);
    a.jsr("press_play");
    a.jsr("searching");
    a.jsr("tape_begin");
    a.lda(imm(kind::DATA_HEADER));
    a.jsr("find_header");
    a.bcc(found_data);
    a.jsr("tape_end");
    a.jmp("error_file_not_found");
    a.bind(found_data);
    a.jsr("tape_end");
    a.jsr("found_pause");
    a.lda(imm(BLOCK_BYTES));
    a.sta(z(zp::BUFPT));
    a.clc();
    a.rts();
    a.bind(writing);
    a.jsr("press_record");
    a.lda(imm(kind::DATA_HEADER));
    a.jsr("make_header");
    a.jsr("write_header");
    a.jsr("data_block_reset");
    a.clc();
    a.rts();

    a.here("data_block_reset");
    a.ldy(imm(0));
    a.lda(imm(kind::DATA));
    a.sta(z(zp::TAPE1).ind_y());
    a.lda(imm(DATA_START));
    a.sta(z(zp::BUFPT));
    a.rts();
}

/// CHRIN: the next byte, a block read when the buffer is spent; the end
/// flagged with the last byte before the zero that follows it. X and Y
/// are kept.
fn tape_get(a: &mut Asm) {
    a.here("tape_get");
    let (have, more, data_block, returned) = (a.local(), a.local(), a.local(), a.local());
    a.stx(z(zp::SYNO));
    a.tsx();
    a.stx(z(zp::XSAV));
    a.lda(z(zp::SYNO));
    a.pha();
    a.tya();
    a.pha();
    a.ldy(z(zp::BUFPT));
    a.cpy(imm(BLOCK_BYTES));
    a.bcc(have);
    a.jsr("buffer_range");
    a.jsr("tape_begin");
    a.lda(imm(0));
    a.sta(z(zp::VERCK));
    a.jsr("read_block");
    a.jsr("tape_end");
    a.jsr("program_range");
    a.ldy(imm(0));
    a.lda(z(zp::TAPE1).ind_y());
    a.cmp(imm(kind::DATA));
    a.beq(data_block);
    a.lda(imm(status::END));
    a.jsr("set_status");
    a.lda(imm(b'\r'));
    a.jmp(returned);
    a.bind(data_block);
    a.ldy(imm(DATA_START));
    a.sty(z(zp::BUFPT));
    a.bind(have);
    a.lda(z(zp::TAPE1).ind_y());
    a.inc(z(zp::BUFPT));
    a.pha();
    a.iny();
    a.cpy(imm(BLOCK_BYTES));
    a.bcs(more);
    a.lda(z(zp::TAPE1).ind_y());
    a.bne(more);
    a.lda(imm(status::END));
    a.jsr("set_status");
    a.bind(more);
    a.pla();
    a.bind(returned);
    a.sta(z(zp::MYCH));
    a.pla();
    a.tay();
    a.pla();
    a.tax();
    a.lda(z(zp::MYCH));
    a.clc();
    a.rts();
}

/// CHROUT: the byte into the buffer, the buffer written as a block when
/// full; A, X and Y kept.
fn tape_put(a: &mut Asm) {
    a.here("tape_put");
    let kept = a.local();
    a.sta(z(zp::MYCH));
    a.stx(z(zp::SYNO));
    a.tsx();
    a.stx(z(zp::XSAV));
    a.pha();
    a.lda(z(zp::SYNO));
    a.pha();
    a.tya();
    a.pha();
    a.lda(z(zp::MYCH));
    a.ldy(z(zp::BUFPT));
    a.sta(z(zp::TAPE1).ind_y());
    a.iny();
    a.sty(z(zp::BUFPT));
    a.cpy(imm(BLOCK_BYTES));
    a.bcc(kept);
    a.jsr("write_data_block");
    a.bind(kept);
    a.pla();
    a.tay();
    a.pla();
    a.tax();
    a.pla();
    a.clc();
    a.rts();

    a.here("write_data_block");
    a.jsr("buffer_range");
    a.jsr("tape_begin");
    let [low, high] = DATA_LEADER.to_le_bytes();
    a.lda(imm(low));
    a.ldx(imm(high));
    a.jsr("write_block");
    a.jsr("tape_end");
    a.jsr("program_range");
    a.jmp("data_block_reset");
}

/// CLOSE of a file being written: a zero after its bytes, the last block,
/// and with secondary address 2 the tape's end.
fn tape_close(a: &mut Asm) {
    a.here("tape_close");
    let no_mark = a.local();
    a.tsx();
    a.stx(z(zp::XSAV));
    a.lda(z(zp::SA));
    a.and(imm(0x0F));
    a.beq(RTS);
    a.lda(imm(0));
    a.ldy(z(zp::BUFPT));
    a.sta(z(zp::TAPE1).ind_y());
    a.jsr("write_data_block");
    a.lda(z(zp::SA));
    a.and(imm(2));
    a.beq(no_mark);
    a.lda(imm(kind::END_OF_TAPE));
    a.ldy(imm(0));
    a.sta(z(zp::TAPE1).ind_y());
    a.jsr("write_header");
    a.bind(no_mark);
    a.clc();
    a.rts();
}
