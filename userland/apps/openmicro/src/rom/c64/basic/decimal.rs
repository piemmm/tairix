//! Decimal numbers: `text_to_fac` reads one from the text through CHRGET,
//! `fac_to_text` writes FAC as STR$ and PRINT show it, `print_unsigned` prints
//! an unsigned 16-bit number, each to the original's bits (`plans/C64.md` §5's
//! pinned behaviour).

use super::float::{LONG_EXPONENT, M1, M2, M3, M4};
use super::tokens::token;
use super::ws::zp;
use crate::rom::asm::{abs, byte, hi, imm, lo, sym, zp as z, Acc, Addr, Asm, Label, RTS};

/// Where `fac_to_text` writes: the sign, then the number, then a zero.
pub const NUMBER_TEXT: u16 = zp::NUMBER_BUFFER + 1;
/// The number's text after its sign.
pub const NUMBER_DIGITS: u16 = NUMBER_TEXT + 1;

/// `text_to_fac`'s state, cleared with FAC after it.
const EXP10: u16 = zp::TEMPF2;
const POINT: u16 = zp::TEMPF2 + 1;
const NEGATIVE: u16 = zp::TEMPF2 + 2;
const EXPONENT_NEGATIVE: u16 = zp::TEMPF2 + 3;
const EXPONENT_DIGITS: u16 = zp::TEMPF2 + 4;

/// The digits `make_digits` writes. `fac_to_text`'s are a zero, which a number
/// below 0.1 starts with, then the number's nine.
const DIGITS: u16 = zp::JMPER + 2;
/// The digits `make_digits` makes.
const DIGIT_COUNT: u16 = zp::JMPER + 1;
/// The index in DIGITS of the digit `fac_to_text`'s point goes before, moved as
/// the value is scaled.
const POINT_INDEX: u16 = zp::TEMPF2 + 4;
/// Where the text's point goes, as `POINT_INDEX`: `DIGIT_COUNT`'s byte,
/// free once the digits are made.
const POINT_AT: u16 = DIGIT_COUNT;
/// `POINT_AT` in scientific notation: after one digit.
const SCIENTIFIC_POINT: u8 = 2;
/// The digits of a number `fac_to_text` writes.
const DECIMAL_DIGITS: u8 = 9;
/// The digits `fac_to_text` makes: the leading zero, then the number's.
const FOUT_DIGIT_COUNT: u8 = DECIMAL_DIGITS + 1;
/// The weights of a digit: the powers of ten from 10⁹, then the jiffies to
/// each TI$ digit — 10 hours, an hour, 10 minutes, a minute, 10 seconds,
/// a second.
pub const DIGIT_WEIGHTS: [u32; 16] = [
    1_000_000_000,
    100_000_000,
    10_000_000,
    1_000_000,
    100_000,
    10_000,
    1_000,
    100,
    10,
    1,
    2_160_000,
    216_000,
    36_000,
    3_600,
    600,
    60,
];
/// The first TI$ weight in [`DIGIT_WEIGHTS`].
pub const CLOCK_WEIGHTS: u8 = FOUT_DIGIT_COUNT;
/// TI$'s length.
pub const CLOCK_TEXT: u8 = 6;
/// DIGITS, where `make_digits` writes, for TI$.
pub const DIGIT_TEXT: u16 = DIGITS;

/// A decimal exponent past which a number's value is all overflow or all
/// underflow, where `text_to_fac`'s sum stops growing.
const EXPONENT_LIMIT: u8 = 0x7F;

/// Byte `rank` of the first weight, most significant first: each byte of
/// the weights is a row of the table, so one index reaches a weight's
/// byte in every row.
pub fn weight_row(rank: i32) -> Addr {
    sym("digit_weights") + rank * i32::from(byte(DIGIT_WEIGHTS.len()))
}

/// The offset of `text_to_fac`'s state byte at `address` from EXP10.
const fn offset(address: u16) -> u8 {
    (address - EXP10).to_le_bytes()[0]
}

pub fn emit(a: &mut Asm) {
    a.whole(scaling);
    a.whole(text_to_fac);
    a.whole(take_sign);
    a.whole(fac_to_text);
    a.whole(make_digits);
    a.whole(print_unsigned);
    a.later(constants);
}

/// The reals the conversions use, and the digits' weights.
fn constants(a: &mut Asm) {
    for (name, value) in [
        ("real_ten", [0x84, 0x20, 0, 0, 0]),
        ("real_e9", [0x9E, 0x6E, 0x6B, 0x28, 0x00]),
        ("fout_low", [0x9B, 0x3E, 0xBC, 0x1F, 0xFE]),
        ("fout_high", [0x9E, 0x6E, 0x6B, 0x27, 0xFD]),
    ] {
        a.here(name);
        a.bytes(&value);
    }
    a.here("digit_weights");
    for shift in [24, 16, 8, 0] {
        for weight in DIGIT_WEIGHTS {
            a.byte((weight >> shift).to_le_bytes()[0]);
        }
    }
}

/// `mul10`: FAC rounded, then times ten as four times plus once. `div10`:
/// FAC rounded, then divided by ten.
fn scaling(a: &mut Asm) {
    a.here("mul10");
    a.jsr("fac_to_arg");
    a.lda(z(zp::FACEXP));
    a.beq(RTS);
    a.jsr("exponent_up");
    a.jsr("exponent_up");
    a.jsr("fadd");
    a.jmp("exponent_up");

    a.here("div10");
    a.jsr("fac_to_arg");
    a.lda(imm(lo("real_ten")));
    a.ldy(imm(hi("real_ten")));
    a.jsr("fac_load");
    a.jmp("fdiv");
}

/// `text_to_fac`: FAC the number at TXTPTR, whose character CHRGOT has in A: an
/// optional sign, digits with a point, an exponent, each digit added to
/// the rounded sum times ten, then the sum scaled by tens. INDEX2 is left
/// as it was, which VAL relies on.
fn text_to_fac(a: &mut Asm) {
    a.here("text_to_fac");
    let clear = a.local();
    let digit = a.local();
    let counted = a.local();
    let not_digit = a.local();
    let not_point = a.local();
    let scale = a.local();
    a.ldy(imm(offset(zp::FACSGN)));
    a.lda(imm(0));
    a.bind(clear);
    a.sta(abs(EXP10).y());
    a.dey();
    a.bpl(clear);
    a.jsr(zp::CHRGOT);
    a.ldx(imm(offset(NEGATIVE)));
    a.jsr("take_sign");
    a.bcs(not_digit);
    a.bind(digit);
    a.sbc(imm(b'0' - 1));
    a.pha();
    a.jsr("mul10");
    a.jsr("fac_to_arg");
    a.pla();
    a.tay();
    a.jsr("byte_to_fac");
    a.jsr("signs_differ");
    a.jsr("fadd");
    a.lda(z(POINT));
    a.beq(counted);
    a.lda(z(EXP10));
    a.cmp(imm(0x80));
    a.beq(counted);
    a.dec(z(EXP10));
    a.bind(counted);
    a.jsr(zp::CHRGET);
    a.bcc(digit);

    a.bind(not_digit);
    a.cmp(imm(b'.'));
    a.bne(not_point);
    a.lda(z(POINT));
    a.bne(scale);
    a.inc(z(POINT));
    a.bne(counted);
    a.bind(not_point);
    a.cmp(imm(b'E'));
    a.bne(scale);
    exponent(a);

    // The sum scaled by EXP10's tens, then signed.
    a.bind(scale);
    let down = a.local();
    let signed = a.local();
    a.lda(z(EXP10));
    a.beq(signed);
    a.bmi(down);
    a.jsr("mul10");
    a.dec(z(EXP10));
    a.bpl(scale);
    a.bind(down);
    a.jsr("div10");
    a.inc(z(EXP10));
    a.bne(down);
    a.bind(signed);
    let positive = a.local();
    a.lda(z(NEGATIVE));
    a.bpl(positive);
    a.jmp("negate");
    a.bind(positive);
    a.rts();
}

/// The exponent after `text_to_fac`'s E: its digits summed to a limit past
/// which any value overflows or underflows, then added to EXP10, which is at
/// most 0, so only a negative exponent can take it past a signed byte.
fn exponent(a: &mut Asm) {
    let digit = a.local();
    let limited = a.local();
    let store = a.local();
    let summed = a.local();
    let add = a.local();
    let added = a.local();
    a.jsr(zp::CHRGET);
    a.ldx(imm(offset(EXPONENT_NEGATIVE)));
    a.jsr("take_sign");
    a.bcs(summed);
    a.bind(digit);
    a.sbc(imm(b'0' - 1));
    a.sta(z(zp::INDEX));
    a.lda(z(EXPONENT_DIGITS));
    a.cmp(imm(13));
    a.bcs(limited);
    a.asl(Acc);
    a.asl(Acc);
    a.adc(z(EXPONENT_DIGITS));
    a.asl(Acc);
    a.adc(z(zp::INDEX));
    a.cmp(imm(EXPONENT_LIMIT));
    a.bcc(store);
    a.bind(limited);
    a.lda(imm(EXPONENT_LIMIT));
    a.bind(store);
    a.sta(z(EXPONENT_DIGITS));
    a.jsr(zp::CHRGET);
    a.bcc(digit);

    a.bind(summed);
    a.lda(z(EXPONENT_DIGITS));
    a.bit(z(EXPONENT_NEGATIVE));
    a.bpl(add);
    a.eor(imm(0xFF));
    a.sec();
    a.skip_over(|a| {
        a.bind(add);
        a.clc();
    });
    a.adc(z(EXP10));
    a.bvc(added);
    a.lda(imm(0x80));
    a.bind(added);
    a.sta(z(EXP10));
}

/// `take_sign`: past a sign at TXTPTR, A its character, a minus
/// decrementing the flag X bytes past EXP10; CHRGET's flags for the
/// character after a sign, CHRGOT's for one that is none.
fn take_sign(a: &mut Asm) {
    a.here("take_sign");
    let minus = a.local();
    let not_minus = a.local();
    let next = a.local();
    let again = a.local();
    a.cmp(imm(b'-'));
    a.beq(minus);
    a.cmp(imm(token::MINUS));
    a.bne(not_minus);
    a.bind(minus);
    a.dec(z(EXP10).x());
    a.bne(next);
    a.bind(not_minus);
    a.cmp(imm(b'+'));
    a.beq(next);
    a.cmp(imm(token::PLUS));
    a.bne(again);
    a.bind(next);
    a.jmp(abs(zp::CHRGET));
    a.bind(again);
    a.jmp(abs(zp::CHRGOT));
}

/// `fac_to_text`: FAC as text at [`NUMBER_TEXT`], A (low) and Y pointing at it:
/// a space or minus, then the unrounded value brought to nine digits — times
/// 1E9 below 1, times ten while the rounded value is under 99999999.9375, a
/// tenth while over 999999999.25, then a half added and the integer taken.
/// Without an exponent from 0.01 to below 1E9, a leading point and zeros below
/// 1; otherwise one digit, the point, and E with a signed two-digit exponent;
/// trailing zeros after a point dropped, and a point with nothing after it.
fn fac_to_text(a: &mut Asm) {
    a.here("fac_to_text");
    let sign = a.local();
    let nonzero = a.local();
    let not_small = a.local();
    let low = a.local();
    let high = a.local();
    let halved = a.local();
    let end = a.local();
    a.lda(imm(b' '));
    a.bit(z(zp::FACSGN));
    a.bpl(sign);
    a.lda(imm(b'-'));
    a.bind(sign);
    a.sta(abs(NUMBER_TEXT));
    a.lda(z(zp::FACEXP));
    a.bne(nonzero);
    a.lda(imm(b'0'));
    a.sta(abs(NUMBER_DIGITS));
    a.ldy(imm(1));
    a.jmp(end);
    a.bind(nonzero);
    a.lsr(z(zp::FACSGN));
    a.lda(imm(FOUT_DIGIT_COUNT));
    a.sta(z(POINT_INDEX));
    a.lda(imm(lo("real_one")));
    a.ldy(imm(hi("real_one")));
    a.jsr("compare_fac");
    a.bpl(not_small);
    a.lda(imm(lo("real_e9")));
    a.ldy(imm(hi("real_e9")));
    a.jsr("fmul_mem");
    a.lda(imm(1));
    a.sta(z(POINT_INDEX));
    a.bind(not_small);
    a.bind(low);
    a.lda(imm(lo("fout_low")));
    a.ldy(imm(hi("fout_low")));
    a.jsr("compare_fac");
    a.bpl(high);
    a.jsr("mul10");
    a.dec(z(POINT_INDEX));
    a.jmp(low);
    a.bind(high);
    a.lda(imm(lo("fout_high")));
    a.ldy(imm(hi("fout_high")));
    a.jsr("compare_fac");
    a.cmp(imm(1));
    a.bne(halved);
    a.jsr("div10");
    a.inc(z(POINT_INDEX));
    a.jmp(high);
    a.bind(halved);
    a.lda(imm(lo("real_half")));
    a.ldy(imm(hi("real_half")));
    a.jsr("fadd_mem");
    a.lda(imm(LONG_EXPONENT));
    a.jsr("shift_mantissa");
    a.ldy(imm(0));
    a.lda(imm(FOUT_DIGIT_COUNT));
    a.jsr("make_digits");
    fout_text(a, end);
}

/// `fac_to_text` run on with its nine digits made: the digits copied with the
/// point among them, from the leading zero when the point goes before it;
/// then the zeros after the point dropped, a point left last, and the
/// exponent where there is one; the text ended at `end`.
fn fout_text(a: &mut Asm, end: Label) {
    let placed = a.local();
    let copy = a.local();
    let no_point = a.local();
    let strip = a.local();
    let stripped = a.local();
    a.lda(z(POINT_INDEX));
    a.cmp(imm(FOUT_DIGIT_COUNT + 1));
    a.bcc(placed);
    a.lda(imm(SCIENTIFIC_POINT));
    a.bind(placed);
    a.sta(z(POINT_AT));
    a.ldy(imm(0));
    a.tax();
    a.beq(copy);
    a.ldx(imm(1));
    a.bind(copy);
    a.cpx(z(POINT_AT));
    a.bne(no_point);
    a.lda(imm(b'.'));
    a.sta(abs(NUMBER_DIGITS).y());
    a.iny();
    a.bind(no_point);
    a.cpx(imm(FOUT_DIGIT_COUNT));
    a.beq(strip);
    a.lda(z(DIGITS).x());
    a.sta(abs(NUMBER_DIGITS).y());
    a.iny();
    a.inx();
    a.bne(copy);
    a.bind(strip);
    a.dey();
    a.lda(abs(NUMBER_DIGITS).y());
    a.cmp(imm(b'0'));
    a.beq(strip);
    a.cmp(imm(b'.'));
    a.beq(stripped);
    a.iny();
    a.bind(stripped);

    // A point moved for scientific notation leaves the exponent the
    // distance it moved.
    let positive = a.local();
    let tens = a.local();
    a.lda(z(POINT_INDEX));
    a.cmp(z(POINT_AT));
    a.beq(end);
    a.sbc(imm(SCIENTIFIC_POINT));
    a.ldx(imm(b'+'));
    a.cmp(imm(0x80));
    a.bcc(positive);
    a.eor(imm(0xFF));
    a.adc(imm(0));
    a.ldx(imm(b'-'));
    a.bind(positive);
    a.pha();
    a.lda(imm(b'E'));
    a.sta(abs(NUMBER_DIGITS).y());
    a.txa();
    a.sta(abs(NUMBER_DIGITS + 1).y());
    a.pla();
    a.ldx(imm(b'0' - 1));
    a.sec();
    a.bind(tens);
    a.inx();
    a.sbc(imm(10));
    a.bcs(tens);
    a.adc(imm(b'0' + 10));
    a.sta(abs(NUMBER_DIGITS + 3).y());
    a.txa();
    a.sta(abs(NUMBER_DIGITS + 2).y());
    for _ in 0..4 {
        a.iny();
    }

    a.bind(end);
    a.lda(imm(0));
    a.sta(abs(NUMBER_DIGITS).y());
    a.lda(imm(lo(NUMBER_TEXT)));
    a.ldy(imm(hi(NUMBER_TEXT)));
    a.rts();
}

/// `make_digits`: A digits of the integer in M1 to M4 at DIGITS, each the
/// count of a weight it holds, the weights from [`DIGIT_WEIGHTS`]' Yth.
fn make_digits(a: &mut Asm) {
    a.here("make_digits");
    let digit = a.local();
    let count = a.local();
    let restore = a.local();
    a.sta(z(DIGIT_COUNT));
    a.ldx(imm(0));
    a.bind(digit);
    a.lda(imm(b'0'));
    a.sta(z(DIGITS).x());
    a.bind(count);
    a.sec();
    for (byte, rank) in [(M4, 3), (M3, 2), (M2, 1)] {
        a.lda(z(byte));
        a.sbc(abs(weight_row(rank)).y());
        a.pha();
    }
    a.lda(z(M1));
    a.sbc(abs(weight_row(0)).y());
    a.bcc(restore);
    a.sta(z(M1));
    for byte in [M2, M3, M4] {
        a.pla();
        a.sta(z(byte));
    }
    a.inc(z(DIGITS).x());
    a.bne(count);
    a.bind(restore);
    a.pla();
    a.pla();
    a.pla();
    a.iny();
    a.inx();
    a.cpx(z(DIGIT_COUNT));
    a.bne(digit);
    a.rts();
}

/// `print_unsigned`: the unsigned number in A (high) and X printed without a
/// sign or space.
fn print_unsigned(a: &mut Asm) {
    a.here("print_unsigned");
    a.sta(z(M1));
    a.stx(z(M2));
    a.ldx(imm(super::float::WORD_EXPONENT));
    a.lda(imm(0));
    a.sta(z(zp::FACSGN));
    a.jsr("magnitude_to_fac");
    a.jsr("fac_to_text");
    a.lda(imm(lo(NUMBER_DIGITS)));
    a.ldy(imm(hi(NUMBER_DIGITS)));
    a.jmp("print_string");
}
