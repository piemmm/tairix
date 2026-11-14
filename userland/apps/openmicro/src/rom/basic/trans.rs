//! The transcendental functions, RND and `^`, each computed as BASIC II
//! computes it so its results have BASIC II's bits.
//!
//! LN, EXP, ATN and the sine of a reduced angle are continued fractions in
//! one variable: from a first constant, each step divides the variable by
//! the running value and adds the next constant. SIN, COS and TAN take the
//! angle less a whole number of quarter turns, the quarter turn in two
//! parts, and a cosine as the root of one less the sine squared. A power
//! below 64 multiplies the base in; any other is the base to the nearest
//! whole power times EXP of the rest times LN of the base.

use super::float::{FAC, FAC2, PACKED};
use super::ws::{kind, page, zp as z};
use crate::rom::asm::byte;
use crate::rom::asm::{abs, hi, imm, lo, zp, Acc, Asm, RTS};

/// The steps a routine has left.
const COUNT: u16 = z::TRANS;
/// The offset of a continued fraction's next constant.
const TABLE: u16 = z::TRANS + 1;
/// The quarter turns a trigonometric argument was reduced by.
const QUADRANT: u16 = z::TRANS + 2;
/// LN's power of two, and EXP's whole power.
const EXPONENT: u16 = z::TRANS + 3;
/// The bits below 64: an integral power that fits takes the multiplying.
const SMALL_BITS: u8 = 6;
/// The exponent of 64: a power from it on is not split.
const SPLIT_EXP: u8 = 0x87;
/// Where ASN keeps its argument: the packed temporaries from &46C are
/// where BASIC II keeps its own.
const ASN_ARGUMENT: u16 = page::TEMPS + 3 * PACKED as u16;
/// Each load or store made on the temporaries, and its routine for each
/// temporary it is made on.
const TEMPORARY_ROUTINES: [(&str, &[(u16, &str)]); 3] = [
    (
        "fac_store",
        &[
            (0, "temp_0_store"),
            (1, "temp_1_store"),
            (2, "temp_2_store"),
            (3, "temp_3_store"),
        ],
    ),
    (
        "fac2_load",
        &[
            (0, "temp_0_load2"),
            (1, "temp_1_load2"),
            (2, "temp_2_load2"),
            (3, "temp_3_load2"),
        ],
    ),
    (
        "fac_load",
        &[(1, "temp_1_load"), (2, "temp_2_load"), (3, "temp_3_load")],
    ),
];
/// Newton's steps to a square root.
const ROOT_STEPS: u8 = 5;
/// A mantissa's top byte from which LN takes it as it is, else doubled.
const LN_DOUBLING: u8 = 0xB5;
/// 89.5's exponent and top byte: EXP's argument limit.
const EXP_LIMIT: (u8, u8) = (0x87, 0xB3);
/// 2^23's exponent: no angle from it on keeps its accuracy.
const ACCURACY_EXP: u8 = 0x98;
/// 2^-13's exponent: ATN is its argument below it.
const ATN_IDENTITY_EXP: u8 = 0x73;

/// The single constants, packed, in their order in the table.
const SINGLES: [[u8; 5]; 11] = [
    [0x82, 0x49, 0x0F, 0xDA, 0xA2], // PI
    [0x80, 0x49, 0x0F, 0xDA, 0xA2], // PI / 4
    [0x81, 0x49, 0x0F, 0xDA, 0xA2], // PI / 2
    [0x81, 0xC9, 0x10, 0x00, 0x00], // -PI / 2 to sixteen bits
    [0x6F, 0x15, 0x77, 0x7A, 0x61], // what that falls short by
    [0x80, 0x80, 0x00, 0x00, 0x00], // -0.5
    [0x80, 0x31, 0x72, 0x17, 0xF8], // LN 2
    [0x7F, 0x5E, 0x5B, 0xD8, 0xAA], // 1 / LN 10
    [0x7B, 0x0E, 0xFA, 0x35, 0x12], // PI / 180
    [0x86, 0x65, 0x2E, 0xE0, 0xD3], // 180 / PI
    [0x82, 0x2D, 0xF8, 0x54, 0x58], // e
];
/// The continued fractions' constants, first to last.
const SINE: [[u8; 5]; 6] = [
    [0x84, 0x8A, 0xEA, 0x0C, 0x1B],
    [0x84, 0x1A, 0xBE, 0xBB, 0x2B],
    [0x84, 0x37, 0x45, 0x55, 0xAB],
    [0x82, 0xD5, 0x55, 0x57, 0x7C],
    [0x83, 0xC0, 0x00, 0x00, 0x05],
    [0x81, 0x00, 0x00, 0x00, 0x00],
];
const LOGARITHM: [[u8; 5]; 7] = [
    [0x7A, 0x12, 0x38, 0xA5, 0x0B],
    [0x88, 0x79, 0x0E, 0x9F, 0xF3],
    [0x7C, 0x2A, 0xAC, 0x3F, 0xB5],
    [0x86, 0x34, 0x01, 0xA2, 0x7A],
    [0x7F, 0x63, 0x8E, 0x37, 0xEC],
    [0x82, 0x3F, 0xFF, 0xFF, 0xC1],
    [0x7F, 0xFF, 0xFF, 0xFF, 0xFF],
];
const EXPONENTIAL: [[u8; 5]; 8] = [
    [0x83, 0xE0, 0x20, 0x86, 0x5B],
    [0x82, 0x80, 0x53, 0x93, 0xB8],
    [0x83, 0x20, 0x00, 0x06, 0xA1],
    [0x82, 0x00, 0x00, 0x21, 0x63],
    [0x82, 0xC0, 0x00, 0x00, 0x02],
    [0x82, 0x80, 0x00, 0x00, 0x0C],
    [0x81, 0x00, 0x00, 0x00, 0x00],
    [0x81, 0x00, 0x00, 0x00, 0x00],
];
const ARCTANGENT: [[u8; 5]; 10] = [
    [0x85, 0xA3, 0x59, 0xE8, 0x67],
    [0x80, 0x1C, 0x9D, 0x07, 0x36],
    [0x80, 0x57, 0xBB, 0x78, 0xDF],
    [0x80, 0xCA, 0x9A, 0x0E, 0x83],
    [0x84, 0x8C, 0xBB, 0xCA, 0x6E],
    [0x81, 0x95, 0x96, 0x06, 0xDE],
    [0x81, 0x0A, 0xC7, 0x6C, 0x52],
    [0x7F, 0x7D, 0xAD, 0x90, 0xA1],
    [0x82, 0xFB, 0x62, 0x57, 0x2F],
    [0x80, 0x6D, 0x63, 0x38, 0x2C],
];

/// The offset into the table of its `index`th constant.
const fn at(index: usize) -> u8 {
    byte(index * PACKED as usize)
}
const PI: u8 = at(0);
const QUARTER_PI: u8 = at(1);
const HALF_PI: u8 = at(2);
const HALF_PI_HIGH: u8 = at(3);
const HALF_PI_LOW: u8 = at(4);
const MINUS_HALF: u8 = at(5);
const LN_TWO: u8 = at(6);
const LOG_E: u8 = at(7);
const RADIAN: u8 = at(8);
const DEGREE: u8 = at(9);
const E: u8 = at(10);
const SINE_AT: u8 = at(SINGLES.len());
/// 1, the sine's last constant.
const ONE: u8 = SINE_AT + at(SINE.len() - 1);
const _: () = assert!(matches!(SINE[SINE.len() - 1], [0x81, 0, 0, 0, 0]));
const LOGARITHM_AT: u8 = SINE_AT + at(SINE.len());
const EXPONENTIAL_AT: u8 = LOGARITHM_AT + at(LOGARITHM.len());
const ARCTANGENT_AT: u8 = EXPONENTIAL_AT + at(EXPONENTIAL.len());
const TABLE_LENGTH: usize =
    SINGLES.len() + SINE.len() + LOGARITHM.len() + EXPONENTIAL.len() + ARCTANGENT.len();
const _: () = assert!(TABLE_LENGTH * (PACKED as usize) <= 256);

/// A continued fraction's steps: one per constant after the first.
const fn steps(constants: &[[u8; 5]]) -> u8 {
    byte(constants.len() - 1)
}

pub fn emit(a: &mut Asm) {
    constants(a);
    temporaries(a);
    continued_fraction(a);
    power(a);
    whole_powers(a);
    root(a);
    logarithm(a);
    exponential(a);
    trigonometry(a);
    arctangent(a);
    scaling(a);
    random(a);
}

fn constants(a: &mut Asm) {
    a.here("constants");
    for group in [&SINGLES[..], &SINE, &LOGARITHM, &EXPONENTIAL, &ARCTANGENT] {
        for constant in group {
            a.bytes(constant);
        }
    }

    // FAC2 or FAC = 1, or the constant X bytes into the table.
    a.here("fac2_one");
    a.ldx(imm(ONE));
    a.here("fac2_constant");
    a.jsr("constant_at");
    a.jmp("fac2_load");
    a.here("fac_one");
    a.ldx(imm(ONE));
    a.here("fac_constant");
    a.jsr("constant_at");
    a.jmp("fac_load");
    a.here("constant_at");
    a.txa();
    a.clc();
    a.adc(imm(lo("constants")));
    a.sta(zp(z::VAR));
    a.lda(imm(hi("constants")));
    a.adc(imm(0));
    a.sta(zp(z::VAR + 1));
    a.rts();
}

/// The load or store `operation` on temporary `n`.
///
/// # Panics
///
/// If `TEMPORARY_ROUTINES` makes no such routine.
fn temp(a: &mut Asm, n: u16, operation: &'static str) {
    let routine = TEMPORARY_ROUTINES
        .iter()
        .filter(|&&(made, _)| made == operation)
        .flat_map(|&(_, routines)| routines)
        .find(|&&(temporary, _)| temporary == n);
    match routine {
        Some(&(_, name)) => a.jsr(name),
        None => panic!("no routine for {operation} on temp_{n}"),
    }
}

fn temporaries(a: &mut Asm) {
    for (operation, routines) in TEMPORARY_ROUTINES {
        let set = a.local();
        for (i, &(n, name)) in routines.iter().enumerate() {
            a.here(name);
            a.lda(imm(lo(page::TEMPS + n * u16::from(PACKED))));
            if i + 1 < routines.len() {
                a.bne(set);
            }
        }
        a.bind(set);
        a.jsr("temp_at");
        a.jmp(operation);
    }
    // VAR at the temporary A is the low byte of; X kept.
    a.here("temp_at");
    a.sta(zp(z::VAR));
    a.lda(imm(hi(page::TEMPS)));
    a.sta(zp(z::VAR + 1));
    a.rts();

    // FAC = 1 - FAC^2.
    a.here("one_less_square");
    a.jsr("fac_to_fac2");
    a.jsr("fac_multiply");
    a.jsr("fac_negate");
    a.jsr("fac_to_fac2");
    a.jsr("fac_one");
    a.jmp("fac_add");
}

fn continued_fraction(a: &mut Asm) {
    // FAC = the fraction over FAC, which waits at temp_0, whose first
    // constant is X bytes into the table: Y steps of the variable over the
    // running value plus the next constant.
    a.here("continued_fraction");
    a.sty(zp(COUNT));
    a.stx(zp(TABLE));
    temp(a, 0, "fac_store");
    a.jsr("fac_constant");
    let step = a.here_local();
    temp(a, 0, "fac2_load");
    a.jsr("fac_divide");
    a.lda(zp(TABLE));
    a.clc();
    a.adc(imm(PACKED));
    a.sta(zp(TABLE));
    a.tax();
    a.jsr("fac2_constant");
    a.jsr("fac_add");
    a.dec(zp(COUNT));
    a.bne(step);
    a.rts();
}

fn power(a: &mut Asm) {
    // FAC = FAC2 ^ FAC. Otherwise than to a small whole power, the base
    // waits at temp_2, the power's remainder at temp_1 and the base to
    // the whole power at temp_3.
    a.here("fac_power");
    a.jsr("small_integral");
    let general = a.local();
    a.bcs(general);
    a.pha();
    a.jsr("fac_from_fac2");
    a.pla();
    a.jsr("power_by");
    a.lda(imm(kind::REAL));
    a.rts();
    a.bind(general);
    temp(a, 1, "fac_store");
    a.jsr("fac_from_fac2");
    temp(a, 2, "fac_store");
    temp(a, 1, "fac_load");
    a.lda(imm(0));
    a.ldx(zp(FAC.exp()));
    a.cpx(imm(SPLIT_EXP));
    let whole = a.local();
    a.bcs(whole);
    a.jsr("split");
    a.bind(whole);
    a.pha();
    temp(a, 1, "fac_store");
    temp(a, 2, "fac_load");
    a.pla();
    a.jsr("power_by");
    temp(a, 3, "fac_store");
    temp(a, 2, "fac_load");
    a.jsr("fac_ln");
    temp(a, 1, "fac2_load");
    a.jsr("fac_multiply");
    a.jsr("fac_exp");
    temp(a, 3, "fac2_load");
    a.jsr("fac_multiply");
    a.lda(imm(kind::REAL));
    a.rts();
}

fn whole_powers(a: &mut Asm) {
    // FAC = FAC ^ A, A from -127 to 127: the base multiplied in, or its
    // reciprocal for a negative power.
    a.here("power_by");
    a.sta(zp(COUNT));
    a.tax();
    let positive = a.local();
    a.bpl(positive);
    a.eor(imm(0xFF));
    a.clc();
    a.adc(imm(1));
    a.sta(zp(COUNT));
    a.jsr("fac2_one");
    a.jsr("fac_divide");
    a.bind(positive);
    let (one, again) = (a.local(), a.local());
    a.lda(zp(COUNT));
    a.beq(one);
    temp(a, 0, "fac_store");
    a.bind(again);
    a.dec(zp(COUNT));
    a.beq(RTS);
    temp(a, 0, "fac2_load");
    a.jsr("fac_multiply");
    a.jmp(again);
    a.bind(one);
    a.jmp("fac_one");

    // FAC = FAC less its nearest whole number, a half going away from
    // zero, and A that number.
    a.here("split");
    temp(a, 0, "fac_store");
    a.jsr("int_from_fac");
    a.lda(zp(z::IAC));
    a.pha();
    a.jsr("fac_from_int");
    temp(a, 0, "fac2_load");
    a.jsr("fac_subtract");
    a.pla();
    a.ldx(zp(FAC.exp()));
    a.cpx(imm(0x80));
    a.bne(RTS);
    a.pha();
    a.jsr("fac_to_fac2");
    a.jsr("fac_one");
    a.lda(zp(FAC2.sign()));
    a.pha();
    a.eor(imm(0x80));
    a.sta(zp(FAC.sign()));
    a.jsr("fac_add");
    a.pla();
    a.asl(Acc);
    a.pla();
    let less = a.local();
    a.bcs(less);
    a.adc(imm(1));
    a.rts();
    a.bind(less);
    a.sbc(imm(1));
    a.rts();

    // C clear if FAC is an integer of magnitude below 64, A then that
    // integer.
    a.here("small_integral");
    a.lda(zp(FAC.m(0)));
    let (not, integral) = (a.local(), a.local());
    a.beq(integral);
    a.lda(zp(FAC.m(1)));
    for i in 2..5 {
        a.ora(zp(FAC.m(i)));
    }
    a.bne(not);
    a.lda(zp(FAC.exp()));
    a.sec();
    a.sbc(imm(0x81));
    a.bcc(not);
    a.cmp(imm(SMALL_BITS));
    a.bcs(not);
    // A + 1 integer bits at the top of the first byte; the rest must be
    // clear.
    a.eor(imm(7));
    a.tax();
    a.lda(zp(FAC.m(0)));
    let shift = a.here_local();
    a.lsr(Acc);
    a.bcs(not);
    a.dex();
    a.bne(shift);
    a.ldx(zp(FAC.sign()));
    let positive = a.local();
    a.bpl(positive);
    a.eor(imm(0xFF));
    a.adc(imm(1));
    a.bind(positive);
    a.bind(integral);
    a.clc();
    a.rts();
    a.bind(not);
    a.sec();
    a.rts();
}

fn root(a: &mut Asm) {
    a.here("fn_sqr");
    a.jsr("arg_real");
    a.jsr("fac_sqr");
    a.lda(imm(kind::REAL));
    a.rts();

    // FAC = SQR FAC: Newton's steps x = (A / x + x) / 2, A at temp_0 and x
    // at temp_1, from A with its exponent halved, the halving rounding up.
    a.here("fac_sqr");
    a.lda(zp(FAC.m(0)));
    a.beq(RTS);
    a.lda(zp(FAC.sign()));
    let positive = a.local();
    a.bpl(positive);
    a.jmp("err_negative_root");
    a.bind(positive);
    temp(a, 0, "fac_store");
    a.lda(zp(FAC.exp()));
    a.clc();
    a.adc(imm(1));
    a.ror(Acc);
    a.clc();
    a.adc(imm(0x40));
    a.sta(zp(FAC.exp()));
    a.lda(imm(ROOT_STEPS));
    a.sta(zp(COUNT));
    let step = a.here_local();
    temp(a, 1, "fac_store");
    temp(a, 0, "fac2_load");
    a.jsr("fac_divide");
    temp(a, 1, "fac2_load");
    a.jsr("fac_add");
    a.dec(zp(FAC.exp()));
    a.dec(zp(COUNT));
    a.bne(step);
    a.rts();
}

fn logarithm(a: &mut Asm) {
    a.here("fn_log");
    a.jsr("fn_ln");
    a.ldx(imm(LOG_E));
    a.jmp("times_constant");

    a.here("fn_ln");
    a.jsr("arg_real");
    a.jsr("fac_ln");
    a.lda(imm(kind::REAL));
    a.rts();

    let range = a.here_local();
    a.jmp("err_log_range");

    // FAC = LN FAC: the mantissa m, doubled below &B5, its power of two
    // e; z = m - 1 at temp_0, then z + z^2 times the fraction, plus e LN 2.
    a.here("fac_ln");
    a.ldy(zp(FAC.m(0)));
    a.beq(range);
    a.lda(zp(FAC.sign()));
    a.bmi(range);
    a.lda(zp(FAC.exp()));
    a.ldx(imm(0x80));
    a.cpy(imm(LN_DOUBLING));
    let kept = a.local();
    a.bcs(kept);
    a.inx();
    a.bind(kept);
    a.stx(zp(FAC.exp()));
    a.stx(zp(EXPONENT));
    a.sec();
    a.sbc(zp(EXPONENT));
    a.sta(zp(EXPONENT));
    a.jsr("fac_to_fac2");
    a.jsr("fac_one");
    a.jsr("fac_subtract");
    a.ldx(imm(LOGARITHM_AT));
    a.ldy(imm(steps(&LOGARITHM)));
    a.jsr("continued_fraction");
    for _ in 0..2 {
        temp(a, 0, "fac2_load");
        a.jsr("fac_multiply");
    }
    temp(a, 0, "fac2_load");
    a.jsr("fac_add");
    temp(a, 0, "fac_store");
    a.lda(zp(EXPONENT));
    a.jsr("return_signed");
    a.jsr("fac_from_int");
    a.ldx(imm(LN_TWO));
    a.fall_into("times_constant_plus");

    // FAC = FAC times the constant X bytes into the table, plus temp_0.
    a.here("times_constant_plus");
    a.jsr("fac2_constant");
    a.jsr("fac_multiply");
    temp(a, 0, "fac2_load");
    a.jmp("fac_add");
}

fn exponential(a: &mut Asm) {
    a.here("fn_exp");
    a.jsr("arg_real");
    a.jsr("fac_exp");
    a.lda(imm(kind::REAL));
    a.rts();

    // FAC = EXP FAC: e to the nearest whole power times the fraction over
    // the rest. From 89.5 on: Exp range above, nothing below.
    a.here("fac_exp");
    a.lda(zp(FAC.exp()));
    a.cmp(imm(EXP_LIMIT.0));
    let (fits, beyond) = (a.local(), a.local());
    a.bcc(fits);
    a.bne(beyond);
    a.lda(zp(FAC.m(0)));
    a.cmp(imm(EXP_LIMIT.1));
    a.bcc(fits);
    a.bind(beyond);
    a.lda(zp(FAC.sign()));
    let range = a.local();
    a.bpl(range);
    a.jmp("fac_zero");
    a.bind(range);
    a.jmp("err_exp_range");
    a.bind(fits);
    a.jsr("split");
    a.sta(zp(EXPONENT));
    a.ldx(imm(EXPONENTIAL_AT));
    a.ldy(imm(steps(&EXPONENTIAL)));
    a.jsr("continued_fraction");
    temp(a, 2, "fac_store");
    a.ldx(imm(E));
    a.jsr("fac_constant");
    a.lda(zp(EXPONENT));
    a.jsr("power_by");
    temp(a, 2, "fac2_load");
    a.jmp("fac_multiply");
}

fn trigonometry(a: &mut Asm) {
    // SIN and COS: the reduced angle's sine in its quadrant, COS a quarter
    // turn on.
    a.here("fn_cos");
    a.lda(imm(1));
    let trig = a.local();
    a.bne(trig);
    a.here("fn_sin");
    a.lda(imm(0));
    a.bind(trig);
    a.pha();
    a.jsr("sine_of_argument");
    a.pla();
    a.clc();
    a.adc(zp(QUADRANT));
    a.jsr("quadrant");
    a.lda(imm(kind::REAL));
    a.rts();

    // TAN: the sine over the cosine, the reduced angle's sine at temp_3
    // and the cosine at temp_2.
    a.here("fn_tan");
    a.jsr("sine_of_argument");
    temp(a, 3, "fac_store");
    a.ldx(zp(QUADRANT));
    a.inx();
    a.txa();
    a.jsr("quadrant");
    temp(a, 2, "fac_store");
    temp(a, 3, "fac_load");
    a.lda(zp(QUADRANT));
    a.jsr("quadrant");
    a.jsr("fac_to_fac2");
    temp(a, 2, "fac_load");
    a.jsr("fac_divide");
    a.lda(imm(kind::REAL));
    a.rts();

    // FAC = the sine of the argument less n quarter turns, n its quarter
    // turns to the nearest and QUADRANT n's low byte; the angle at temp_0,
    // n at temp_1, the reduced angle at temp_2. Accuracy lost from 2^23.
    a.here("sine_of_argument");
    a.jsr("arg_real");
    a.lda(zp(FAC.exp()));
    a.cmp(imm(ACCURACY_EXP));
    let fits = a.local();
    a.bcc(fits);
    a.jmp("err_accuracy_lost");
    a.bind(fits);
    temp(a, 0, "fac_store");
    a.ldx(imm(QUARTER_PI));
    a.jsr("fac2_constant");
    a.lda(zp(FAC.sign()));
    a.sta(zp(FAC2.sign()));
    a.jsr("fac_add");
    a.jsr("fac_to_fac2");
    a.ldx(imm(HALF_PI));
    a.jsr("fac_constant");
    a.jsr("fac_divide");
    a.jsr("int_from_fac");
    a.lda(zp(z::IAC));
    a.sta(zp(QUADRANT));
    a.jsr("fac_from_int");
    temp(a, 1, "fac_store");
    a.ldx(imm(HALF_PI_HIGH));
    a.jsr("times_constant_plus");
    temp(a, 0, "fac_store");
    temp(a, 1, "fac_load");
    a.ldx(imm(HALF_PI_LOW));
    a.jsr("times_constant_plus");
    temp(a, 2, "fac_store");
    // The angle times the fraction over its square.
    a.jsr("fac_to_fac2");
    a.jsr("fac_multiply");
    a.ldx(imm(SINE_AT));
    a.ldy(imm(steps(&SINE)));
    a.jsr("continued_fraction");
    temp(a, 2, "fac2_load");
    a.jmp("fac_multiply");

    // FAC = the sine of the reduced angle moved on A quarter turns, from
    // its sine in FAC: odd quadrants take the root of one less its square,
    // the upper two the negative.
    a.here("quadrant");
    a.lsr(Acc);
    a.pha();
    let even = a.local();
    a.bcc(even);
    a.jsr("one_less_square");
    a.jsr("fac_sqr");
    a.bind(even);
    a.pla();
    a.lsr(Acc);
    a.bcc(RTS);
    a.jmp("fac_negate");
}

fn arctangent(a: &mut Asm) {
    a.here("fn_atn");
    a.jsr("arg_real");
    a.jsr("fac_atn");
    a.lda(imm(kind::REAL));
    a.rts();

    a.here("fn_acs");
    a.jsr("fn_asn");
    a.jsr("half_pi_less");
    a.lda(imm(kind::REAL));
    a.rts();

    a.here("fn_asn");
    a.jsr("arg_real");
    a.jsr("fac_asn");
    a.lda(imm(kind::REAL));
    a.rts();

    // FAC = ASN FAC: ATN of x over the root of 1 - x^2, x at temp_3, or a
    // quarter turn with x's sign where that is nothing.
    a.here("fac_asn");
    temp(a, 3, "fac_store");
    a.jsr("one_less_square");
    a.lda(zp(FAC.m(0)));
    let root = a.local();
    a.bne(root);
    a.ldx(imm(HALF_PI));
    a.jsr("fac_constant");
    a.lda(abs(ASN_ARGUMENT + 1));
    a.and(imm(0x80));
    a.sta(zp(FAC.sign()));
    a.rts();
    a.bind(root);
    a.jsr("fac_sqr");
    temp(a, 3, "fac2_load");
    a.jsr("fac_divide");
    a.fall_into("fac_atn");

    // FAC = ATN FAC: x itself below 2^-13; for |x| from 1 a quarter turn
    // less that of 1 / |x|; x' at temp_2 times the fraction over x' - 0.5,
    // the sign put back.
    a.here("fac_atn");
    a.ldx(zp(FAC.exp()));
    a.cpx(imm(ATN_IDENTITY_EXP));
    a.bcc(RTS);
    a.lda(zp(FAC.sign()));
    a.pha();
    a.lda(imm(0));
    a.sta(zp(FAC.sign()));
    a.cpx(imm(0x81));
    a.php();
    let small = a.local();
    a.bcc(small);
    a.jsr("fac2_one");
    a.jsr("fac_divide");
    a.bind(small);
    temp(a, 2, "fac_store");
    a.ldx(imm(MINUS_HALF));
    a.jsr("fac2_constant");
    a.jsr("fac_add");
    a.ldx(imm(ARCTANGENT_AT));
    a.ldy(imm(steps(&ARCTANGENT)));
    a.jsr("continued_fraction");
    temp(a, 2, "fac2_load");
    a.jsr("fac_multiply");
    a.plp();
    let direct = a.local();
    a.bcc(direct);
    a.jsr("half_pi_less");
    a.bind(direct);
    a.pla();
    a.bpl(RTS);
    a.jmp("fac_negate");

    // FAC = a quarter turn less FAC, the quarter turn in its two parts.
    a.here("half_pi_less");
    a.ldx(imm(HALF_PI_HIGH));
    a.jsr("fac2_constant");
    a.jsr("fac_add");
    a.ldx(imm(HALF_PI_LOW));
    a.jsr("fac2_constant");
    a.jsr("fac_add");
    a.jmp("fac_negate");
}

fn scaling(a: &mut Asm) {
    // RAD and DEG: the argument times the constant's ratio.
    a.here("fn_rad");
    a.lda(imm(RADIAN));
    let scaled = a.local();
    a.bne(scaled);
    a.here("fn_deg");
    a.lda(imm(DEGREE));
    a.bind(scaled);
    a.pha();
    a.jsr("arg_real");
    a.pla();
    a.tax();
    a.here("times_constant");
    a.jsr("fac2_constant");
    a.jsr("fac_multiply");
    a.lda(imm(kind::REAL));
    a.rts();

    a.here("fn_pi");
    a.ldx(imm(PI));
    a.jsr("fac_constant");
    a.lda(imm(kind::REAL));
    a.rts();
}

/// The seed's bytes whose bits feed back: bit 32 and bit 19.
const SEED_TOP: u16 = z::SEED + 4;
const SEED_TAP: u16 = z::SEED + 2;
/// The seed's fifth byte when it is set from an integer.
const SEED_FILL: u8 = 0x40;

fn random(a: &mut Asm) {
    // RND: the seed's low four bytes after a step. RND(n): n < 0 seeds
    // and returns n; 0 the current seed as a fraction; 1 a step's
    // fraction; above, a step's fraction times n, whole, plus one.
    a.here("fn_rnd");
    a.ldy(zp(z::OFFSET_A));
    a.lda(zp(z::TEXT_A).ind_y());
    a.cmp(imm(b'('));
    let (argument, positive, fraction, integer) = (a.local(), a.local(), a.local(), a.local());
    a.beq(argument);
    a.jsr("rnd_step");
    a.ldx(imm(3));
    let copy = a.here_local();
    a.lda(zp(z::SEED).x());
    a.sta(zp(z::IAC).x());
    a.dex();
    a.bpl(copy);
    a.bmi(integer);
    a.bind(argument);
    a.inc(zp(z::OFFSET_A));
    a.jsr("expr_integer");
    a.jsr("closing_bracket");
    a.lda(zp(z::IAC + 3));
    a.bpl(positive);
    a.ldx(imm(3));
    let seed = a.here_local();
    a.lda(zp(z::IAC).x());
    a.sta(zp(z::SEED).x());
    a.dex();
    a.bpl(seed);
    a.lda(imm(SEED_FILL));
    a.sta(zp(SEED_TOP));
    a.bne(integer);
    a.bind(positive);
    a.ora(zp(z::IAC + 2));
    a.ora(zp(z::IAC + 1));
    a.bne("rnd_scaled");
    a.lda(zp(z::IAC));
    a.beq(fraction);
    a.cmp(imm(1));
    a.bne("rnd_scaled");
    a.jsr("rnd_step");
    a.bind(fraction);
    a.jsr("rnd_fraction");
    a.lda(imm(kind::REAL));
    a.rts();
    a.here("rnd_scaled");
    a.jsr("rnd_step");
    a.jsr("rnd_fraction");
    a.jsr("fac_to_fac2");
    a.jsr("fac_from_int");
    a.jsr("fac_multiply");
    a.jsr("int_from_fac");
    a.inc(zp(z::IAC));
    let done = a.local();
    a.bne(done);
    a.inc(zp(z::IAC + 1));
    a.bne(done);
    a.inc(zp(z::IAC + 2));
    a.bne(done);
    a.inc(zp(z::IAC + 3));
    a.bind(done);
    a.bind(integer);
    a.lda(imm(kind::INTEGER));
    a.rts();

    // The forty-bit seed shifted left thirty-two times, bit 32 exclusive-or
    // bit 19 into bit 0 each time.
    a.here("rnd_step");
    a.ldy(imm(32));
    let bit = a.here_local();
    a.lda(zp(SEED_TAP));
    a.lsr(Acc);
    a.lsr(Acc);
    a.lsr(Acc);
    a.eor(zp(SEED_TOP));
    a.ror(Acc);
    for at in z::SEED..z::SEED + z::SEED_LEN {
        a.rol(zp(at));
    }
    a.dey();
    a.bne(bit);
    a.rts();

    // FAC = the seed's first four bytes as a fraction, the first highest.
    a.here("rnd_fraction");
    a.jsr("fac_zero");
    a.ldx(imm(3));
    let copy = a.here_local();
    a.lda(zp(z::SEED).x());
    a.sta(zp(FAC.m(0)).x());
    a.dex();
    a.bpl(copy);
    a.lda(imm(0x80));
    a.sta(zp(FAC.exp()));
    a.jmp("fac_normalise");
}
