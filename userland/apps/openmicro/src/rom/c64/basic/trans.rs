//! The functions of reals: the polynomial they share, SIN, COS, TAN, ATN,
//! LOG, EXP, SQR, the power operator and RND, each to the original's bits
//! (`plans/C64.md` §5's pinned behaviour); the coefficients are those the
//! original is seen loading into ARG.

use super::float::{LONG_EXPONENT, M1, M2, M3, M4};
use super::ws::zp;
use crate::machine::c64::cia::reg;
use crate::machine::c64::io;
use crate::rom::asm::{abs, hi, imm, lo, zp as z, Acc, Asm};

/// TAN's sine while it forms the cosine, and the power operator's
/// exponent while it takes the logarithm and its exponential: room in the
/// page `fac_to_text` writes into, which neither calls.
const TAN_SINE: u16 = zp::NUMBER_BUFFER + 1;
const POWER_EXPONENT: u16 = zp::NUMBER_BUFFER + 6;
/// EXP's integer part, a signed byte, while its polynomial forms.
const EXP_INTEGER: u16 = zp::TANSGN;
/// SIN's argument's turn in its second or third quarter, which makes TAN's
/// cosine negative.
const COSINE_NEGATIVE: u16 = zp::TANSGN;

/// CIA1's timer A, low and high, and its time of day, tenths and seconds:
/// RND(0)'s mantissa.
const TIMER_A_LOW: u16 = io::cia1(reg::TA_LO);
const TIMER_A_HIGH: u16 = io::cia1(reg::TA_HI);
const TOD_TENTHS: u16 = io::cia1(reg::TOD_TENTHS);
const TOD_SECONDS: u16 = io::cia1(reg::TOD_SECONDS);

/// The reals the functions use, by label; 1 and 2π end two of the
/// polynomials, and −½ and ½ share [`tables`]' run of zeros.
const REALS: [(&str, [u8; 5]); 9] = [
    ("real_quarter", [0x7F, 0x00, 0x00, 0x00, 0x00]),
    ("real_half_pi", [0x81, 0x49, 0x0F, 0xDA, 0xA2]),
    ("real_pi", [0x82, 0x49, 0x0F, 0xDA, 0xA2]),
    ("real_sqrt2", [0x81, 0x35, 0x04, 0xF3, 0x34]),
    ("real_inv_sqrt2", [0x80, 0x35, 0x04, 0xF3, 0x34]),
    ("real_ln2", [0x80, 0x31, 0x72, 0x17, 0xF8]),
    ("real_log2e", [0x81, 0x38, 0xAA, 0x3B, 0x29]),
    ("rnd_multiplier", [0x98, 0x35, 0x44, 0x7A, 0x00]),
    ("rnd_addend", [0x68, 0x28, 0xB1, 0x46, 0x00]),
];

/// The polynomials, each its coefficients from the highest power down.
const SIN_COEFFICIENTS: [[u8; 5]; 6] = [
    [0x84, 0xE6, 0x1A, 0x2D, 0x1B],
    [0x86, 0x28, 0x07, 0xFB, 0xF8],
    [0x87, 0x99, 0x68, 0x89, 0x01],
    [0x87, 0x23, 0x35, 0xDF, 0xE1],
    [0x86, 0xA5, 0x5D, 0xE7, 0x28],
    TWO_PI,
];
/// 2π and 1, the last coefficients of SIN's and ATN's polynomials.
const TWO_PI: [u8; 5] = [0x83, 0x49, 0x0F, 0xDA, 0xA2];
const ONE: [u8; 5] = [0x81, 0x00, 0x00, 0x00, 0x00];
const ATN_COEFFICIENTS: [[u8; 5]; 12] = [
    [0x76, 0xB3, 0x83, 0xBD, 0xD3],
    [0x79, 0x1E, 0xF4, 0xA6, 0xF5],
    [0x7B, 0x83, 0xFC, 0xB0, 0x10],
    [0x7C, 0x0C, 0x1F, 0x67, 0xCA],
    [0x7C, 0xDE, 0x53, 0xCB, 0xC1],
    [0x7D, 0x14, 0x64, 0x70, 0x4C],
    [0x7D, 0xB7, 0xEA, 0x51, 0x7A],
    [0x7D, 0x63, 0x30, 0x88, 0x7E],
    [0x7E, 0x92, 0x44, 0x99, 0x3A],
    [0x7E, 0x4C, 0xCC, 0x91, 0xC7],
    [0x7F, 0xAA, 0xAA, 0xAA, 0x13],
    ONE,
];
const LOG_COEFFICIENTS: [[u8; 5]; 4] = [
    [0x7F, 0x5E, 0x56, 0xCB, 0x79],
    [0x80, 0x13, 0x9B, 0x0B, 0x64],
    [0x80, 0x76, 0x38, 0x93, 0x16],
    [0x82, 0x38, 0xAA, 0x3B, 0x20],
];
const EXP_COEFFICIENTS: [[u8; 5]; 8] = [
    [0x71, 0x34, 0x58, 0x3E, 0x56],
    [0x74, 0x16, 0x7E, 0xB3, 0x1B],
    [0x77, 0x2F, 0xEE, 0xE3, 0x85],
    [0x7A, 0x1D, 0x84, 0x1C, 0x2A],
    [0x7C, 0x63, 0x59, 0x58, 0x0A],
    [0x7E, 0x75, 0xFD, 0xE7, 0xC6],
    [0x80, 0x31, 0x72, 0x18, 0x10],
    [0x81, 0x00, 0x00, 0x00, 0x00],
];

/// The exponent at which |x·log₂e| reaches 128.
const EXP_LIMIT: u8 = 0x88;
/// The addend EXP's fraction takes beneath x·log₂e's mantissa.
const EXP_FRACTION_BIAS: u8 = 0x50;
/// The integer below which EXP is zero.
const EXP_UNDERFLOW: u8 = 0x80;

pub fn emit(a: &mut Asm) {
    a.movable(polynomials);
    a.movable(sine);
    a.movable(tangent);
    a.movable(arctangent);
    a.movable(logarithm);
    a.movable(exponential);
    a.movable(power);
    a.movable(random);
    a.later(tables);
}

fn tables(a: &mut Asm) {
    for (name, value) in REALS {
        a.here(name);
        a.bytes(&value);
    }
    // −½, ½ from its second byte, and from its third a zero for a variable
    // not made.
    a.here("real_neg_half");
    a.byte(0x80);
    a.here("real_half");
    a.byte(0x80);
    a.here("zero_value");
    a.bytes(&[0; 5]);
    for (name, coefficients, last) in [
        ("sin_table", &SIN_COEFFICIENTS[..], Some("real_two_pi")),
        ("atn_table", &ATN_COEFFICIENTS[..], Some("real_one")),
        ("log_table", &LOG_COEFFICIENTS[..], None),
        ("exp_table", &EXP_COEFFICIENTS[..], None),
    ] {
        a.here(name);
        a.byte(crate::rom::asm::byte(coefficients.len() - 1));
        let (body, end) = coefficients.split_at(coefficients.len() - 1);
        for coefficient in body {
            a.bytes(coefficient);
        }
        if let Some(last) = last {
            a.here(last);
        }
        for coefficient in end {
            a.bytes(coefficient);
        }
    }
}

/// `poly`: the polynomial at A (low) and Y — its additions' count, then
/// its coefficients — in FAC rounded, F: F × c₀, then each next
/// coefficient added and the sum times F but the last. `poly_odd`: z
/// rounded to Z, the same polynomial in Z² rounded, times Z.
fn polynomials(a: &mut Asm) {
    a.here("poly");
    let next = a.local();
    let again = a.local();
    let multiply = a.local();
    a.sta(z(zp::FBUFPT));
    a.sty(z(zp::FBUFPT + 1));
    let pointed = a.here_local();
    a.ldx(imm(lo(zp::TEMPF2)));
    a.ldy(imm(hi(zp::TEMPF2)));
    a.jsr("fac_store");
    a.ldy(imm(0));
    a.lda(z(zp::FBUFPT).ind_y());
    a.sta(z(zp::SGNFLG));
    a.inc(z(zp::FBUFPT));
    a.bne(next);
    a.inc(z(zp::FBUFPT + 1));
    a.bind(next);
    a.lda(z(zp::FBUFPT));
    a.ldy(z(zp::FBUFPT + 1));
    a.jmp(multiply);
    a.bind(again);
    a.lda(imm(lo(zp::TEMPF2)));
    a.ldy(imm(hi(zp::TEMPF2)));
    a.bind(multiply);
    a.jsr("fmul_mem");
    let advanced = a.local();
    a.lda(z(zp::FBUFPT));
    a.clc();
    a.adc(imm(5));
    a.sta(z(zp::FBUFPT));
    a.bcc(advanced);
    a.inc(z(zp::FBUFPT + 1));
    a.bind(advanced);
    a.ldy(z(zp::FBUFPT + 1));
    a.jsr("fadd_mem");
    a.dec(z(zp::SGNFLG));
    a.bne(again);
    a.rts();

    a.here("poly_odd");
    a.sta(z(zp::FBUFPT));
    a.sty(z(zp::FBUFPT + 1));
    a.ldx(imm(lo(zp::TEMPF1)));
    a.ldy(imm(hi(zp::TEMPF1)));
    a.jsr("fac_store");
    a.lda(imm(lo(zp::TEMPF1)));
    a.ldy(imm(hi(zp::TEMPF1)));
    a.jsr("fmul_mem");
    a.jsr(pointed);
    a.lda(imm(lo(zp::TEMPF1)));
    a.ldy(imm(hi(zp::TEMPF1)));
    a.jmp("fmul_mem");
}

/// `fn_sin`, `fn_cos` and `sin_fold`: x rounded over 2π is y; f, y less
/// INT y, folds to z, 0.25 less |0.25 − f|, through 0.5 − f in the
/// middle quarters, which also mark the cosine negative.
fn sine(a: &mut Asm) {
    a.here("fn_cos");
    a.lda(imm(lo("real_half_pi")));
    a.ldy(imm(hi("real_half_pi")));
    a.jsr("fadd_mem");
    a.here("fn_sin");
    a.jsr("sin_fold");
    a.here("sin_poly");
    a.lda(imm(lo("sin_table")));
    a.ldy(imm(hi("sin_table")));
    a.jmp("poly_odd");

    a.here("sin_fold");
    let unflipped = a.local();
    let positive_cosine = a.local();
    let folded = a.local();
    a.jsr("fac_to_arg");
    a.lda(imm(lo("real_two_pi")));
    a.ldy(imm(hi("real_two_pi")));
    a.jsr("fac_load");
    a.jsr("fdiv");
    a.jsr("fac_to_arg");
    a.jsr("fn_int");
    // INT y − y as −(y − INT y): the sum treats its signs alike.
    a.jsr("fsub");
    a.jsr("negate");
    a.lda(imm(lo("real_quarter")));
    a.ldy(imm(hi("real_quarter")));
    a.jsr("fadd_mem");
    a.lda(imm(0));
    a.sta(z(COSINE_NEGATIVE));
    a.lda(z(zp::FACSGN));
    a.pha();
    a.bpl(unflipped);
    a.lda(imm(lo("real_half")));
    a.ldy(imm(hi("real_half")));
    a.jsr("fadd_mem");
    a.lda(z(zp::FACEXP));
    a.beq(positive_cosine);
    a.lda(z(zp::FACSGN));
    a.bmi(positive_cosine);
    a.dec(z(COSINE_NEGATIVE));
    a.bind(positive_cosine);
    a.bind(unflipped);
    a.pla();
    a.fall_into("quarter_fold");

    // FAC made 0.25 less its magnitude, negated where A's top bit is set.
    a.here("quarter_fold");
    a.pha();
    a.lsr(z(zp::FACSGN));
    a.jsr("negate");
    a.lda(imm(lo("real_quarter")));
    a.ldy(imm(hi("real_quarter")));
    a.jsr("fadd_mem");
    a.pla();
    a.bpl(folded);
    a.jsr("negate");
    a.bind(folded);
    a.rts();
}

/// `fn_tan`: the sine of x, rounded, over the cosine of the same folded
/// argument: 0.25 less |Z|, negative in the middle quarters.
fn tangent(a: &mut Asm) {
    a.here("fn_tan");
    a.jsr("fn_sin");
    a.ldx(imm(lo(TAN_SINE)));
    a.ldy(imm(hi(TAN_SINE)));
    a.jsr("fac_store");
    a.lda(imm(lo(zp::TEMPF1)));
    a.ldy(imm(hi(zp::TEMPF1)));
    a.jsr("fac_load");
    a.lda(z(COSINE_NEGATIVE));
    a.jsr("quarter_fold");
    a.jsr("sin_poly");
    a.lda(imm(lo(TAN_SINE)));
    a.ldy(imm(hi(TAN_SINE)));
    a.jmp("fdiv_mem");
}

/// `fn_atn`: the polynomial in |x|, or in its reciprocal above 1 and the
/// result taken from π/2; then x's sign.
fn arctangent(a: &mut Asm) {
    a.here("fn_atn");
    let small = a.local();
    let not_big = a.local();
    a.lda(z(zp::FACSGN));
    a.pha();
    a.lsr(z(zp::FACSGN));
    a.lda(imm(lo("real_one")));
    a.ldy(imm(hi("real_one")));
    a.jsr("compare_fac");
    a.cmp(imm(1));
    a.php();
    a.bne(small);
    a.lda(imm(lo("real_one")));
    a.ldy(imm(hi("real_one")));
    a.jsr("fdiv_mem");
    a.bind(small);
    a.lda(imm(lo("atn_table")));
    a.ldy(imm(hi("atn_table")));
    a.jsr("poly_odd");
    a.plp();
    a.bne(not_big);
    a.jsr("negate");
    a.lda(imm(lo("real_half_pi")));
    a.ldy(imm(hi("real_half_pi")));
    a.jsr("fadd_mem");
    a.bind(not_big);
    let positive = a.local();
    a.pla();
    a.bpl(positive);
    a.jmp("negate");
    a.bind(positive);
    a.rts();
}

/// `fn_log`: x's mantissa m with its exponent e: the polynomial in
/// 1 − √2 ÷ (m + 1/√2), less a half, then e plus that rounded, times ln 2.
fn logarithm(a: &mut Asm) {
    a.here("fn_log");
    let illegal = a.local();
    let positive = a.local();
    a.jsr("sign");
    a.beq(illegal);
    a.bpl(positive);
    a.bind(illegal);
    a.jmp("illegal_quantity_error");
    a.bind(positive);
    a.lda(z(zp::FACEXP));
    a.sec();
    a.sbc(imm(0x80));
    a.pha();
    a.lda(imm(0x80));
    a.sta(z(zp::FACEXP));
    a.lda(imm(lo("real_inv_sqrt2")));
    a.ldy(imm(hi("real_inv_sqrt2")));
    a.jsr("fadd_mem");
    a.lda(imm(lo("real_sqrt2")));
    a.ldy(imm(hi("real_sqrt2")));
    a.jsr("fdiv_mem");
    a.jsr("negate");
    a.lda(imm(lo("real_one")));
    a.ldy(imm(hi("real_one")));
    a.jsr("fadd_mem");
    a.lda(imm(lo("log_table")));
    a.ldy(imm(hi("log_table")));
    a.jsr("poly_odd");
    a.lda(imm(lo("real_neg_half")));
    a.ldy(imm(hi("real_neg_half")));
    a.jsr("fadd_mem");
    a.jsr("fac_to_arg");
    a.pla();
    a.jsr("signed_byte_to_fac");
    a.jsr("signs_differ");
    a.jsr("fadd");
    a.lda(imm(lo("real_ln2")));
    a.ldy(imm(hi("real_ln2")));
    a.jmp("fmul_mem");
}

/// `fn_exp`: y = x·log₂e; past ±128 an overflow or zero. The fraction is
/// y with [`EXP_FRACTION_BIAS`] added beneath its mantissa, less INT y;
/// its polynomial times 2 to INT y, zero below 2⁻¹²⁷.
fn exponential(a: &mut Asm) {
    a.here("fn_exp");
    let in_range = a.local();
    let zero = a.local();
    let save = a.local();
    let carry = a.local();
    let biased = a.local();
    let overflow = a.local();
    let store = a.local();
    a.lda(imm(lo("real_log2e")));
    a.ldy(imm(hi("real_log2e")));
    a.jsr("fmul_mem");
    a.lda(z(zp::FACEXP));
    a.cmp(imm(EXP_LIMIT));
    a.bcc(in_range);
    a.bit(z(zp::FACSGN));
    a.bmi(zero);
    a.bind(overflow);
    a.jmp("overflow_error");
    a.bind(zero);
    a.jmp("fac_zero");
    // y kept in ARG, its byte beneath on the stack, while INT y is taken.
    a.bind(in_range);
    a.lda(z(zp::FACOV));
    a.pha();
    a.ldx(imm(5));
    a.bind(save);
    a.lda(z(zp::FACEXP).x());
    a.sta(z(zp::ARGEXP).x());
    a.dex();
    a.bpl(save);
    a.jsr("fn_int");
    a.ldx(imm(lo(zp::TEMPF1)));
    a.ldy(imm(hi(zp::TEMPF1)));
    a.jsr("fac_store");
    a.jsr("fac_to_integer");
    a.lda(z(M4));
    a.sta(z(EXP_INTEGER));
    a.jsr("arg_to_fac");
    a.pla();
    a.clc();
    a.adc(imm(EXP_FRACTION_BIAS));
    a.sta(z(zp::FACOV));
    a.ldx(imm(3));
    a.bind(carry);
    a.lda(z(M1).x());
    a.adc(imm(0));
    a.sta(z(M1).x());
    a.dex();
    a.bpl(carry);
    a.bcc(biased);
    a.jsr("carry_in");
    a.bind(biased);
    a.jsr("negate");
    a.lda(imm(lo(zp::TEMPF1)));
    a.ldy(imm(hi(zp::TEMPF1)));
    a.jsr("fadd_mem");
    a.jsr("negate");
    a.lda(imm(lo("exp_table")));
    a.ldy(imm(hi("exp_table")));
    a.jsr("poly");
    // A negative integer cannot take the exponent below 1.
    a.lda(z(EXP_INTEGER));
    a.cmp(imm(EXP_UNDERFLOW));
    a.beq(zero);
    a.clc();
    a.adc(z(zp::FACEXP));
    a.bit(z(EXP_INTEGER));
    a.bmi(store);
    a.bcs(overflow);
    a.bind(store);
    a.sta(z(zp::FACEXP));
    a.rts();
}

/// `power`: ARG to the power of FAC rounded: 1 for a zero power, 0 for a
/// zero base, a negative base needing a whole power whose parity signs
/// the result; otherwise EXP of the power times LOG of the base. `fn_sqr`:
/// the rounded argument to the power of a half.
fn power(a: &mut Asm) {
    a.here("fn_sqr");
    a.jsr("fac_to_arg");
    a.lda(imm(lo("real_half")));
    a.ldy(imm(hi("real_half")));
    a.jsr("fac_load");
    a.here("power");
    let nonzero_power = a.local();
    let nonzero_base = a.local();
    let positive_base = a.local();
    let whole = a.local();
    let even = a.local();
    let signed = a.local();
    a.jsr("round");
    a.lda(z(zp::FACEXP));
    a.bne(nonzero_power);
    a.ldy(imm(1));
    a.jmp("byte_to_fac");
    a.bind(nonzero_power);
    a.lda(z(zp::ARGEXP));
    a.bne(nonzero_base);
    a.jmp("fac_zero");
    a.bind(nonzero_base);
    a.ldx(imm(lo(POWER_EXPONENT)));
    a.ldy(imm(hi(POWER_EXPONENT)));
    a.jsr("fac_store");
    a.lda(imm(0));
    a.pha();
    a.bit(z(zp::ARGSGN));
    a.bpl(positive_base);
    a.jsr("fn_int");
    a.lda(imm(lo(POWER_EXPONENT)));
    a.ldy(imm(hi(POWER_EXPONENT)));
    a.jsr("compare_fac");
    a.beq(whole);
    a.jmp("illegal_quantity_error");
    a.bind(whole);
    a.lda(z(zp::FACEXP));
    a.cmp(imm(LONG_EXPONENT + 1));
    a.bcs(even);
    a.lda(imm(LONG_EXPONENT));
    a.jsr("shift_mantissa");
    a.lda(z(M4));
    a.lsr(Acc);
    a.bcc(even);
    a.pla();
    a.lda(imm(0xFF));
    a.pha();
    a.bind(even);
    a.bind(positive_base);
    a.jsr("arg_to_fac");
    a.lsr(z(zp::FACSGN));
    a.jsr("fn_log");
    a.lda(imm(lo(POWER_EXPONENT)));
    a.ldy(imm(hi(POWER_EXPONENT)));
    a.jsr("fmul_mem");
    a.jsr("fn_exp");
    a.pla();
    a.beq(signed);
    a.jsr("negate");
    a.bind(signed);
    a.rts();
}

/// `fn_rnd`: the seed times 11879546 plus 3.927677739E-8 for x above zero,
/// x itself below; then the mantissa's bytes reversed. For zero, timer A
/// and the time of day are the mantissa. The old exponent goes beneath,
/// the exponent becomes 2⁰, and the normalised, rounded result is the seed.
fn random(a: &mut Asm) {
    a.here("fn_rnd");
    let not_zero = a.local();
    let reverse = a.local();
    let tail = a.local();
    a.jsr("sign");
    a.bne(not_zero);
    a.lda(abs(TIMER_A_LOW));
    a.sta(z(M1));
    a.lda(abs(TIMER_A_HIGH));
    a.sta(z(M3));
    a.lda(abs(TOD_TENTHS));
    a.sta(z(M2));
    a.lda(abs(TOD_SECONDS));
    a.sta(z(M4));
    // The seconds' top bit reads 0.
    a.bpl(tail);
    a.bind(not_zero);
    a.bmi(reverse);
    a.lda(imm(lo(zp::RNDX)));
    a.ldy(imm(hi(zp::RNDX)));
    a.jsr("fac_load");
    a.lda(imm(lo("rnd_multiplier")));
    a.ldy(imm(hi("rnd_multiplier")));
    a.jsr("fmul_mem");
    a.lda(imm(lo("rnd_addend")));
    a.ldy(imm(hi("rnd_addend")));
    a.jsr("fadd_mem");
    a.bind(reverse);
    a.ldx(z(M1));
    a.lda(z(M4));
    a.sta(z(M1));
    a.stx(z(M4));
    a.ldx(z(M2));
    a.lda(z(M3));
    a.sta(z(M2));
    a.stx(z(M3));
    a.bind(tail);
    a.lda(z(zp::FACEXP));
    a.sta(z(zp::FACOV));
    a.lda(imm(0x80));
    a.sta(z(zp::FACEXP));
    a.lsr(z(zp::FACSGN));
    a.jsr("normalise");
    a.ldx(imm(lo(zp::RNDX)));
    a.ldy(imm(hi(zp::RNDX)));
    a.jmp("fac_store");
}
