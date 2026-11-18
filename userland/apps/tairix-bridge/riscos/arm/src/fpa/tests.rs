//! FPEmulator's rules, one at a time: the formats, the round stage, the
//! exceptions and their traps, and the transcendental functions' exact cases.
//! The recorded vectors pin the bits of every operation besides.

use super::{fpsr, Exception, Format, Fpa, Internal, Precision, Rounding};
use crate::instruction::{FpaCompareOp, FpaDyadicOp, FpaMonadicOp, FpaOperand};

const ZERO: [u32; 3] = [0, 0, 0];
const ONE: [u32; 3] = [0x3FFF, 0x8000_0000, 0];
const TWO: [u32; 3] = [0x4000, 0x8000_0000, 0];
const SEVEN: [u32; 3] = [0x4001, 0xE000_0000, 0];
const HALF: [u32; 3] = [0x3FFE, 0x8000_0000, 0];
const INFINITY: [u32; 3] = [0x4000_7FFF, 0, 0];

const F1: FpaOperand = FpaOperand::Register(1);
const E: Precision = Precision::Extended;
const NEAREST: Rounding = Rounding::Nearest;

/// An FPA with `registers` set and its FPSR at `status`.
fn fpa(registers: &[(u8, [u32; 3])], status: u32) -> Fpa {
    let mut fpa = Fpa::new();
    for &(n, words) in registers {
        fpa.set_register(n, Internal::from_words(words));
    }
    fpa.write_fpsr(status);
    fpa
}

fn flags(fpa: &Fpa) -> u32 {
    fpa.fpsr() & fpsr::FLAGS
}

/// `fd = op(F1)` at `precision`, rounded to nearest.
fn monadic(op: FpaMonadicOp, precision: Precision, a: [u32; 3]) -> ([u32; 3], u32) {
    let mut fpa = fpa(&[(1, a)], 0);
    fpa.monadic(op, precision, NEAREST, 0, F1)
        .expect("no trap enabled");
    (fpa.register(0).words(), flags(&fpa))
}

/// `F0 = F1 op F2` at `precision` in `rounding`.
fn dyadic(
    op: FpaDyadicOp,
    precision: Precision,
    rounding: Rounding,
    a: [u32; 3],
    b: [u32; 3],
) -> ([u32; 3], u32) {
    let mut fpa = fpa(&[(1, a), (2, b)], 0);
    fpa.dyadic(op, precision, rounding, 0, 1, FpaOperand::Register(2))
        .expect("no trap enabled");
    (fpa.register(0).words(), flags(&fpa))
}

#[test]
fn the_eight_constants_are_the_ones_the_fpa_names() {
    let expected = [
        ZERO,
        ONE,
        TWO,
        [0x4000, 0xC000_0000, 0],
        [0x4001, 0x8000_0000, 0],
    ];
    for (n, words) in (0..).zip(expected) {
        assert_eq!(Internal::constant(n).words(), words, "#{n}");
    }
    assert_eq!(Internal::constant(5).words(), [0x4001, 0xA000_0000, 0]);
    assert_eq!(Internal::constant(6).words(), HALF);
    assert_eq!(Internal::constant(7).words(), [0x4002, 0xA000_0000, 0]);
}

#[test]
fn a_result_rounds_at_its_precision_and_flags_what_it_loses() {
    // 1 + 2^-30: exact in extended, rounded to 1 in single.
    let tiny = [0x3FE1, 0x8000_0000, 0];
    let (sum, raised) = dyadic(FpaDyadicOp::Adf, E, NEAREST, ONE, tiny);
    assert_eq!((sum, raised), ([0x3FFF, 0x8000_0002, 0], 0));
    let (sum, raised) = dyadic(FpaDyadicOp::Adf, Precision::Single, NEAREST, ONE, tiny);
    assert_eq!((sum, raised), (ONE, Exception::Inexact.flag()));
    let (sum, _) = dyadic(
        FpaDyadicOp::Adf,
        Precision::Single,
        Rounding::PlusInfinity,
        ONE,
        tiny,
    );
    assert_eq!(sum, [0x3FFF, 0x8000_0100, 0], "up to the next single");
}

#[test]
fn overflow_gives_the_extended_infinity_or_the_precision_s_largest_number() {
    let huge = [0x7FFE, 0x8000_0000, 0];
    let lost = Exception::Overflow.flag() | Exception::Inexact.flag();
    let (product, raised) = dyadic(FpaDyadicOp::Muf, Precision::Single, NEAREST, huge, huge);
    assert_eq!((product, raised), (INFINITY, lost));
    let (product, raised) = dyadic(
        FpaDyadicOp::Muf,
        Precision::Single,
        Rounding::Zero,
        huge,
        huge,
    );
    assert_eq!((product, raised), ([0x407E, 0xFFFF_FF00, 0], lost));
}

#[test]
fn underflow_is_flagged_only_where_denormalising_loses_bits() {
    // 2^-126 halved is a single denormal exactly; three times 2^-150 is
    // half way between two of them.
    let least_normal = [0x3F81, 0x8000_0000, 0];
    let (half, raised) = dyadic(
        FpaDyadicOp::Dvf,
        Precision::Single,
        NEAREST,
        least_normal,
        TWO,
    );
    assert_eq!(
        half,
        [0x4000_3F80, 0xC000_0000, 0],
        "kept as a single denormal"
    );
    assert_eq!(raised, 0);
    let three = [0x4000, 0xC000_0000, 0];
    let (product, raised) = dyadic(
        FpaDyadicOp::Muf,
        Precision::Single,
        NEAREST,
        [0x3F69, 0x8000_0000, 0],
        three,
    );
    assert_eq!(
        product,
        [0x4000_3F80, 0x8000_0200, 0],
        "to even: two of the least"
    );
    assert_eq!(
        raised,
        Exception::Underflow.flag() | Exception::Inexact.flag()
    );
}

#[test]
fn an_invalid_operation_leaves_a_nan_carrying_its_reason() {
    let (quotient, raised) = dyadic(FpaDyadicOp::Dvf, E, NEAREST, ZERO, ZERO);
    assert_eq!(quotient, [0x4000_7FFF, 0x4000_0700, 0], "0/0, reason 7");
    assert_eq!(raised, Exception::InvalidOperation.flag());
    let (root, _) = monadic(FpaMonadicOp::Sqt, E, [0x8000_3FFF, 0x8000_0000, 0]);
    assert_eq!(
        root,
        [0xC000_7FFF, 0x4000_0B00, 0],
        "√-1, reason 11, negative"
    );
}

#[test]
fn an_enabled_trap_writes_nothing_but_the_flags_raised_before_it() {
    let huge = [0x7FFE, 0x8000_0000, 0];
    let status = fpsr::SYSTEM_ID | Exception::Inexact.enable();
    let mut fpa = fpa(&[(0, SEVEN), (1, huge)], status);
    let trapped = fpa.dyadic(FpaDyadicOp::Muf, E, NEAREST, 0, 1, F1);
    assert_eq!(trapped, Err(Exception::Inexact));
    assert_eq!(fpa.register(0).words(), SEVEN, "the destination untouched");
    assert_eq!(
        fpa.fpsr(),
        status | Exception::Overflow.flag(),
        "overflow flagged before the inexact trap; inexact itself not"
    );
}

#[test]
fn the_fpsr_keeps_its_system_id() {
    let mut fpa = Fpa::new();
    assert_eq!(fpa.fpsr(), fpsr::SYSTEM_ID);
    fpa.write_fpsr(u32::MAX);
    assert_eq!(fpa.fpsr(), fpsr::SYSTEM_ID | fpsr::WRITABLE);
}

#[test]
fn fix_rounds_in_its_mode_saturates_out_of_range_and_gives_zero_for_a_nan() {
    let two_and_a_half = [0x4000, 0xA000_0000, 0];
    let mut fpa = fpa(&[(1, two_and_a_half)], 0);
    assert_eq!(fpa.fix(NEAREST, F1), Ok(2), "to even");
    assert_eq!(fpa.fix(Rounding::PlusInfinity, F1), Ok(3));
    assert_eq!(flags(&fpa), Exception::Inexact.flag());
    let mut fpa = self::fpa(&[(1, [0x401E, 0x8000_0000, 0])], 0);
    assert_eq!(fpa.fix(NEAREST, F1), Ok(0x7FFF_FFFF), "2^31");
    assert_eq!(flags(&fpa), Exception::InvalidOperation.flag());
    let mut fpa = self::fpa(&[(1, [0x8000_401E, 0x8000_0000, 0])], 0);
    assert_eq!(fpa.fix(NEAREST, F1), Ok(0x8000_0000), "-2^31 fits");
    assert_eq!(flags(&fpa), 0);
    let mut fpa = self::fpa(&[(1, [0x4000_7FFF, 0xC000_0000, 0])], 0);
    assert_eq!(fpa.fix(NEAREST, F1), Ok(0), "a NaN");
}

#[test]
fn a_comparison_orders_or_signals_as_its_variant_says() {
    let quiet = [0x4000_7FFF, 0xC000_0000, 0];
    let signalling = [0x4000_7FFF, 0xA000_0000, 0];
    let order = |op, b: [u32; 3], status| {
        let mut fpa = fpa(&[(0, ONE), (1, b)], status);
        let flags = fpa.compare(op, 0, F1).expect("no trap enabled");
        (flags >> 28, self::flags(&fpa))
    };
    let invalid = Exception::InvalidOperation.flag();
    assert_eq!(order(FpaCompareOp::Cmf, TWO, 0), (0b1000, 0), "less");
    assert_eq!(
        order(FpaCompareOp::Cnf, TWO, 0),
        (0b0010, 0),
        "greater than -2"
    );
    assert_eq!(
        order(FpaCompareOp::Cmf, ONE, 0),
        (0b0110, 0),
        "equal, C set"
    );
    assert_eq!(order(FpaCompareOp::Cmf, quiet, 0), (0b0001, 0), "unordered");
    assert_eq!(order(FpaCompareOp::Cmf, quiet, fpsr::AC), (0b0011, 0));
    assert_eq!(order(FpaCompareOp::Cmfe, quiet, 0), (0b0001, invalid));
    assert_eq!(order(FpaCompareOp::Cmf, signalling, 0), (0b0001, invalid));
}

#[test]
fn single_and_double_denormals_load_in_their_own_form_and_store_back_unchanged() {
    let mut fpa = Fpa::new();
    fpa.load(Format::Single, 0, [1, 0, 0, 0])
        .expect("raises nothing");
    assert_eq!(fpa.register(0).words(), [0x4000_3F80, 0x8000_0100, 0]);
    assert_eq!(fpa.store(Format::Single, 0), Ok([1, 0, 0, 0]));
    fpa.load(Format::Double, 1, [0x8000_0000, 1, 0, 0])
        .expect("raises nothing");
    assert_eq!(fpa.register(1).words(), [0xC000_3C00, 0x8000_0000, 0x800]);
    assert_eq!(fpa.store(Format::Double, 1), Ok([0x8000_0000, 1, 0, 0]));
    assert_eq!(flags(&fpa), 0);
}

#[test]
fn lfm_clears_the_reserved_bits_and_ldfe_marks_what_is_not_normalised() {
    let mut fpa = Fpa::new();
    fpa.load_multiple(0, [0x3FFF_BFFF, 0x8000_0000, 0]);
    assert_eq!(fpa.store_multiple(0), ONE, "bits 29 to 15 cleared");
    fpa.load_multiple(0, [0x7FFF_BFFF, 0x8000_0000, 0]);
    assert_eq!(
        fpa.store_multiple(0),
        [0x4000_3FFF, 0x8000_0000, 0],
        "kept uncommon"
    );
    fpa.load(Format::Extended, 1, [0x7FFF, 0, 0, 0])
        .expect("raises nothing");
    assert_eq!(fpa.register(1).words(), INFINITY);
    fpa.load(Format::Extended, 2, [0x1234, 0x4000_0000, 0, 0])
        .expect("raises nothing");
    assert_eq!(
        fpa.register(2).words(),
        [0x4000_1234, 0x4000_0000, 0],
        "an unnormal"
    );
}

#[test]
fn packed_decimal_stores_seventeen_digits_and_loads_them_back() {
    let quarter = [0x8000_3FFD, 0x8000_0000, 0];
    for (value, packed) in [
        (ONE, [0x0000_0100, 0, 0]),
        ([0x3FFF, 0xC000_0000, 0], [0x0000_0150, 0, 0]),
        (quarter, [0xC000_1250, 0, 0]),
    ] {
        let mut fpa = fpa(&[(0, value)], 0);
        let stored = fpa.store(Format::Packed, 0).expect("no trap enabled");
        assert_eq!(stored[..3], packed, "{value:08X?}");
        fpa.load(Format::Packed, 1, stored)
            .expect("no trap enabled");
        assert_eq!(fpa.register(1).words(), value);
        assert_eq!(flags(&fpa), 0, "exact both ways");
    }
    let mut fpa = fpa(&[(0, [0x3FFF, 0xC000_0000, 0])], fpsr::EP);
    assert_eq!(fpa.transfer_words(Format::Packed), 4, "expanded");
    let stored = fpa.store(Format::Packed, 0).expect("no trap enabled");
    fpa.load(Format::Packed, 1, stored)
        .expect("no trap enabled");
    assert_eq!(fpa.register(1).words(), [0x3FFF, 0xC000_0000, 0]);
}

#[test]
fn urd_leaves_an_integer_unnormalised_and_nrm_finishes_it() {
    let two_and_a_half = [0x4000, 0xA000_0000, 0];
    let (rounded, raised) = monadic(FpaMonadicOp::Urd, Precision::Single, two_and_a_half);
    assert_eq!(
        rounded,
        [0x4016, 0x0000_0200, 0],
        "2 at single's binary point"
    );
    assert_eq!(raised, Exception::Inexact.flag());
    let (normalised, _) = monadic(FpaMonadicOp::Nrm, E, rounded);
    assert_eq!(normalised, TWO);
    let (rounded, _) = monadic(FpaMonadicOp::Rnd, E, [0x4000, 0xE000_0000, 0]);
    assert_eq!(rounded, [0x4001, 0x8000_0000, 0], "3.5 to even: 4");
}

#[test]
fn an_unnormal_is_taken_at_its_value_anywhere_but_nrm() {
    // 1 + 2^-20 with eight leading zeros, as only URD leaves a register.
    let unnormal = Internal {
        head: 0x3FFF + 8,
        mantissa: 1 << 55 | 1 << 35,
    }
    .words();
    let one_and_a_bit = [0x3FFF, 0x8000_0800, 0];
    assert_eq!(
        monadic(FpaMonadicOp::Mvf, Precision::Single, unnormal),
        (one_and_a_bit, 0)
    );
    let mut fpa = fpa(&[(1, unnormal)], 0);
    assert_eq!(fpa.store(Format::Single, 1), Ok([0x3F80_0008, 0, 0, 0]));
    // 2.5 with a leading zero: rounded to even.
    let two_and_a_half = [0x3FFF + 2, 0x5000_0000, 0];
    assert_eq!(monadic(FpaMonadicOp::Rnd, E, two_and_a_half).0, TWO);
    // A zero stays one whichever way RND rounds.
    let mut fpa = self::fpa(&[(1, ZERO)], 0);
    fpa.monadic(FpaMonadicOp::Rnd, E, Rounding::PlusInfinity, 0, F1)
        .expect("no trap enabled");
    assert_eq!(fpa.register(0).words(), ZERO);
}

#[test]
fn the_transcendental_functions_are_exact_where_their_values_are() {
    let hundred = [0x4005, 0xC800_0000, 0];
    for (op, argument, expected) in [
        (FpaMonadicOp::Log, hundred, TWO),
        (FpaMonadicOp::Lgn, ONE, ZERO),
        (FpaMonadicOp::Exp, ZERO, ONE),
        (FpaMonadicOp::Sin, ZERO, ZERO),
        (FpaMonadicOp::Cos, ZERO, ONE),
        (FpaMonadicOp::Acs, ONE, ZERO),
        (FpaMonadicOp::Atn, ZERO, ZERO),
    ] {
        assert_eq!(monadic(op, E, argument), (expected, 0), "{op:?}");
    }
    // Every power of ten an extended value holds.
    let mut ten_to = 1u128;
    for n in 1..=27u32 {
        ten_to *= 10;
        let lead = ten_to.ilog2();
        let mantissa = u64::try_from(ten_to << (127 - lead) >> 64).expect("the top 64 bits");
        let argument = Internal {
            head: 0x3FFF + lead,
            mantissa,
        }
        .words();
        let (log, raised) = monadic(FpaMonadicOp::Log, E, argument);
        let lead = n.ilog2();
        assert_eq!(log, [0x3FFF + lead, n << (31 - lead), 0], "10^{n}");
        assert_eq!(raised, 0, "10^{n}");
    }
    let (power, raised) = dyadic(FpaDyadicOp::Pow, E, NEAREST, TWO, [0x4002, 0xA000_0000, 0]);
    assert_eq!((power, raised), ([0x4009, 0x8000_0000, 0], 0), "2^10");
}

#[test]
fn a_transcendental_value_is_correctly_rounded() {
    let inexact = Exception::Inexact.flag();
    // π/4 is C90FDAA2 2168C234 C4C6…: to nearest, the last word rounds up,
    // where FPEmulator's answer is a place below.
    assert_eq!(
        monadic(FpaMonadicOp::Atn, E, ONE),
        ([0x3FFE, 0xC90F_DAA2, 0x2168_C235], inexact)
    );
    assert_eq!(
        monadic(FpaMonadicOp::Atn, E, INFINITY),
        ([0x3FFF, 0xC90F_DAA2, 0x2168_C235], inexact),
        "ATN of an infinity: π/2"
    );
    let (angle, _) = dyadic(FpaDyadicOp::Pol, E, NEAREST, ONE, INFINITY);
    assert_eq!(
        angle,
        [0x3FFF, 0xC90F_DAA2, 0x2168_C235],
        "the point (1, infinity)"
    );
}

#[test]
fn the_transcendental_functions_refuse_what_has_no_value() {
    let invalid = Exception::InvalidOperation.flag();
    for (op, argument, nan, reason) in [
        (FpaMonadicOp::Sin, INFINITY, 0x4000_7FFF, 17),
        (FpaMonadicOp::Tan, INFINITY, 0x4000_7FFF, 19),
        (FpaMonadicOp::Asn, INFINITY, 0x4000_7FFF, 21),
        (FpaMonadicOp::Asn, TWO, 0xC000_7FFF, 20),
        (FpaMonadicOp::Acs, TWO, 0x4000_7FFF, 20),
        (
            FpaMonadicOp::Log,
            [0x8000_3FFF, 0x8000_0000, 0],
            0x4000_7FFF,
            24,
        ),
    ] {
        assert_eq!(
            monadic(op, E, argument),
            ([nan, 0x4000_0000 | reason << 8, 0], invalid),
            "{op:?} of {argument:08X?}: reason {reason}"
        );
    }
    assert_eq!(
        monadic(FpaMonadicOp::Log, E, ZERO),
        ([0xC000_7FFF, 0, 0], Exception::DivisionByZero.flag()),
        "LOG of 0: minus infinity"
    );
}

#[test]
fn sin_cos_and_tan_refuse_arguments_from_fpemulator_s_bounds() {
    let invalid = Exception::InvalidOperation.flag();
    // The least magnitudes FPEmulator refuses, found by bisection, and the
    // value a place below each.
    for (op, negative, exponent, refused, reason) in [
        (
            FpaMonadicOp::Sin,
            false,
            0x401F,
            0xC90F_DAA1_5858_E793_u64,
            16,
        ),
        (FpaMonadicOp::Sin, true, 0x401F, 0xC90F_DAA1_5858_E793, 16),
        (FpaMonadicOp::Cos, true, 0x401F, 0xC90F_DAA0_8F49_0CF1, 16),
        (FpaMonadicOp::Tan, false, 0x401E, 0xC90F_DAA1_5858_E793, 18),
        (FpaMonadicOp::Tan, true, 0x401E, 0xC90F_DAA2_EA78_9CD8, 18),
    ] {
        let head = u32::from(negative) << 31 | exponent;
        let at = |mantissa: u64| Internal { head, mantissa }.words();
        let (result, raised) = monadic(op, E, at(refused));
        let sign = if op == FpaMonadicOp::Tan && negative {
            0xC000_7FFF
        } else {
            0x4000_7FFF
        };
        assert_eq!(
            (result, raised),
            ([sign, 0x4000_0000 | reason << 8, 0], invalid),
            "{op:?} {negative}"
        );
        let (result, raised) = monadic(op, E, at(refused - 1));
        assert_eq!(raised, Exception::Inexact.flag(), "{op:?} {negative} below");
        assert_ne!(result[0] & 0x7FFF, 0x7FFF, "{op:?} {negative} below");
    }
}

#[test]
fn a_power_of_a_negative_number_or_an_infinity_takes_fpemulator_s_special_cases() {
    let minus_two = [0x8000_4000, 0x8000_0000, 0];
    let three = [0x4000, 0xC000_0000, 0];
    let inexact = Exception::Inexact.flag();
    let lost = Exception::Overflow.flag() | inexact;
    let minus_zero = [0x8000_0000, 0, 0];
    let minus_one = [0x8000_3FFF, 0x8000_0000, 0];
    let minus_infinity = [0xC000_7FFF, 0, 0];
    for (base, exponent, rounding, expected, why) in [
        (
            minus_two,
            three,
            NEAREST,
            ([0x8000_4002, 0x8000_0000, 0], inexact),
            "an odd power of a negative number is inexact",
        ),
        (
            minus_two,
            three,
            Rounding::MinusInfinity,
            ([0x8000_4002, 0x8000_0000, 1], inexact),
            "as though bits lay below it",
        ),
        (
            INFINITY,
            TWO,
            NEAREST,
            (INFINITY, lost),
            "infinity squared overflows",
        ),
        (
            INFINITY,
            TWO,
            Rounding::Zero,
            ([0x7FFE, 0xFFFF_FFFF, 0xFFFF_FFFF], lost),
            "to the largest number",
        ),
        (
            minus_infinity,
            minus_one,
            NEAREST,
            (minus_zero, inexact),
            "1/-infinity",
        ),
    ] {
        assert_eq!(
            dyadic(FpaDyadicOp::Pow, E, rounding, base, exponent),
            expected,
            "{why}"
        );
    }
}

#[test]
fn a_power_of_zero_or_one_takes_fpemulator_s_special_cases() {
    let minus_two = [0x8000_4000, 0x8000_0000, 0];
    let zero_divide = Exception::DivisionByZero.flag();
    let invalid = Exception::InvalidOperation.flag();
    let minus_zero = [0x8000_0000, 0, 0];
    let minus_one = [0x8000_3FFF, 0x8000_0000, 0];
    let minus_infinity = [0xC000_7FFF, 0, 0];
    for (base, exponent, rounding, expected, why) in [
        (
            minus_zero,
            HALF,
            NEAREST,
            (minus_zero, 0),
            "-0 to a power not an even integer",
        ),
        (
            minus_zero,
            minus_one,
            NEAREST,
            (minus_infinity, zero_divide),
            "-0 to an odd negative power",
        ),
        (
            minus_zero,
            [0x8000_4000, 0x8000_0000, 0],
            NEAREST,
            (INFINITY, zero_divide),
            "-0 to -2",
        ),
        (
            ONE,
            INFINITY,
            NEAREST,
            ([0x4000_7FFF, 0x4000_1B00, 0], invalid),
            "1 to infinity, reason 27",
        ),
        (
            minus_one,
            INFINITY,
            NEAREST,
            ([0x4000_7FFF, 0x4000_1900, 0], invalid),
            "-1 to infinity, reason 25",
        ),
        (
            minus_two,
            HALF,
            NEAREST,
            ([0x4000_7FFF, 0x4000_1900, 0], invalid),
            "a negative number to a fraction",
        ),
        (
            TWO,
            minus_infinity,
            NEAREST,
            (ZERO, 0),
            "2 to minus infinity",
        ),
        (ZERO, ZERO, NEAREST, (ONE, 0), "anything to zero"),
    ] {
        assert_eq!(
            dyadic(FpaDyadicOp::Pow, E, rounding, base, exponent),
            expected,
            "{why}"
        );
    }
}

#[test]
fn integer_powers_below_2_to_the_16_are_multiplied_out() {
    let lost = Exception::Overflow.flag() | Exception::Inexact.flag();
    // An infinity's overflow.
    let below = [0x400E, 0xFFFE_0000, 0];
    let at = [0x400F, 0x8000_0000, 0];
    assert_eq!(
        dyadic(FpaDyadicOp::Pow, E, NEAREST, INFINITY, below),
        (INFINITY, lost),
        "65535"
    );
    assert_eq!(
        dyadic(FpaDyadicOp::Pow, E, NEAREST, INFINITY, at),
        (INFINITY, 0),
        "65536"
    );
    // RPW raises its second operand to the first.
    assert_eq!(
        dyadic(FpaDyadicOp::Rpw, E, NEAREST, [0x4002, 0xA000_0000, 0], TWO),
        ([0x4009, 0x8000_0000, 0], 0),
        "2^10"
    );
}

#[test]
fn a_polar_angle_takes_fpemulator_s_special_cases() {
    let invalid = Exception::InvalidOperation.flag();
    let inexact = Exception::Inexact.flag();
    let minus_zero = [0x8000_0000, 0, 0];
    let minus_one = [0x8000_3FFF, 0x8000_0000, 0];
    for (x, y, expected, why) in [
        (
            ZERO,
            minus_zero,
            ([0x4000_7FFF, 0x4000_1600, 0], invalid),
            "two zeros, reason 22",
        ),
        (
            INFINITY,
            [0xC000_7FFF, 0, 0],
            ([0x4000_7FFF, 0x4000_1700, 0], invalid),
            "two infinities, reason 23",
        ),
        (
            minus_one,
            minus_zero,
            ([0x8000_4000, 0xC90F_DAA2, 0x2168_C235], inexact),
            "(-1, -0): -π",
        ),
        (INFINITY, minus_zero, (minus_zero, 0), "(infinity, -0)"),
        (ONE, minus_zero, (minus_zero, 0), "(1, -0)"),
    ] {
        assert_eq!(
            dyadic(FpaDyadicOp::Pol, E, NEAREST, x, y),
            expected,
            "{why}"
        );
    }
}

#[test]
fn rmf_is_the_remainder_of_the_quotient_rounded_to_even() {
    let (remainder, raised) = dyadic(FpaDyadicOp::Rmf, E, NEAREST, SEVEN, TWO);
    assert_eq!(
        (remainder, raised),
        ([0x8000_3FFF, 0x8000_0000, 0], 0),
        "7 - 2×4"
    );
    let five = [0x4001, 0xA000_0000, 0];
    let (remainder, _) = dyadic(FpaDyadicOp::Rmf, E, NEAREST, five, TWO);
    assert_eq!(remainder, ONE, "5 - 2×2");
    let (remainder, raised) = dyadic(
        FpaDyadicOp::Rmf,
        E,
        NEAREST,
        [0x8000_4000, 0x8000_0000, 0],
        ONE,
    );
    assert_eq!(
        (remainder, raised),
        ([0x8000_0000, 0, 0], 0),
        "-2 - 1×-2: the dividend's zero"
    );
}

#[test]
fn a_denormal_marker_with_no_fraction_is_taken_as_the_zero_it_is() {
    // Only LFM of hand-made words makes one; dividing by it, storing it
    // extended and taking a remainder by it once divided by zero, shifted a
    // zero mantissa out of its width, and computed a remainder by nothing.
    for marker in [[0x4000_3F80, 0x8000_0000, 0], [0xC000_3C00, 0x8000_0000, 0]] {
        let mut fpa = Fpa::new();
        fpa.load_multiple(1, ONE);
        fpa.load_multiple(2, marker);
        fpa.dyadic(FpaDyadicOp::Dvf, E, NEAREST, 0, 1, FpaOperand::Register(2))
            .expect("no trap enabled");
        assert_eq!(fpa.register(0).words()[1..], [0, 0], "an infinity");
        assert_eq!(flags(&fpa), Exception::DivisionByZero.flag());
        let stored = fpa.store(Format::Extended, 2).expect("no trap enabled");
        assert_eq!(stored, [marker[0] & 0x8000_0000, 0, 0, 0]);
        fpa.dyadic(FpaDyadicOp::Rmf, E, NEAREST, 0, 1, FpaOperand::Register(2))
            .expect("no trap enabled");
        assert_eq!(
            fpa.register(0).words(),
            [0x4000_7FFF, 0x4000_0A00, 0],
            "reason 10"
        );
    }
}

#[test]
fn packed_decimal_reads_a_digit_above_nine_at_its_face_value() {
    let mut fpa = fpa(&[], 0);
    fpa.load(Format::Packed, 0, [0x0000_000A, 0, 0, 0])
        .expect("no trap enabled");
    assert_eq!(
        fpa.register(0).words(),
        [0x3FFB, 0xCCCC_CCCC, 0xCCCC_CCCD],
        "ten hundredths"
    );
    assert_eq!(flags(&fpa), Exception::Inexact.flag());
}

#[test]
fn a_packed_exponent_past_fpemulator_s_bound_reads_as_an_infinity_or_a_nan() {
    let load = |status, words| {
        let mut fpa = fpa(&[], status);
        fpa.load(Format::Packed, 0, words).expect("no trap enabled");
        (fpa.register(0).words(), flags(&fpa))
    };
    let lost = Exception::Overflow.flag() | Exception::Inexact.flag();
    assert_eq!(
        load(0, [0x0FFD_9100, 0, 0, 0]),
        (INFINITY, lost),
        "16639: a number"
    );
    assert_eq!(
        load(0, [0x0FFD_A100, 0, 0, 0]),
        ([0x4000_7FFF, 0x2400_0016, 0xD674_E9BC], 0),
        "16640: a NaN carrying 10^18"
    );
    assert_eq!(
        load(0, [0x0FFD_A000, 0, 0, 0]),
        (INFINITY, 0),
        "no digits: an infinity"
    );
    assert_eq!(
        load(fpsr::EP, [0x0FFE_6143, 0x1000_0000, 0, 0]),
        (INFINITY, lost),
        "16646143"
    );
    assert_eq!(
        load(fpsr::EP, [0x0FFE_6144, 0x1000_0000, 0, 0]),
        ([0x4000_7FFF, 0x0000_00FC, 0x295E_D058], 0),
        "16646144"
    );
}

#[test]
fn a_nan_s_packed_payload_is_its_fraction_in_fpemulator_s_order() {
    let store = |status, value| {
        let mut fpa = fpa(&[(0, value)], status);
        let words = fpa.store(Format::Packed, 0).expect("no trap enabled");
        (words, flags(&fpa))
    };
    let nan = [0x4000_7FFF, 0x7FFF_FFFF, 0xFFFF_FFFF];
    assert_eq!(
        store(0, nan),
        ([0x0FFF_FC61, 0x1686_0184, 0x2738_7903, 0], 0)
    );
    assert_eq!(
        store(fpsr::EP, nan),
        ([0x0FFF_FFFF, 0x8000_0461, 0x1686_0184, 0x2738_7903], 0)
    );
    // A signalling NaN is written quiet and signals nothing; a single
    // infinity is written as a NaN.
    let signalling = [0x4000_7FFF, 0x0000_0001, 0];
    assert_eq!(
        store(0, signalling),
        ([0x0FFF_F800, 0x0008_7960, 0x9302_2208, 0], 0)
    );
    let single_infinity = [0x4000_407F, 0x8000_0000, 0];
    assert_eq!(store(0, single_infinity), ([0x0FFF_F800, 0, 0, 0], 0));
    // Loaded back, a payload keeps its bits; expanded, a quiet one gains
    // 1013 × 2^52.
    let mut fpa = fpa(&[], 0);
    fpa.load(
        Format::Packed,
        0,
        [0x0FFF_FC61, 0x1686_0184, 0x2738_7903, 0],
    )
    .expect("no trap enabled");
    assert_eq!(fpa.register(0).words(), nan);
    let mut fpa = self::fpa(&[], fpsr::EP);
    fpa.load(Format::Packed, 0, [0x0FFF_FFFF, 0x8000_0000, 0, 1])
        .expect("no trap enabled");
    assert_eq!(
        fpa.register(0).words(),
        [0x4000_7FFF, 0x4000_0100, 0x0000_07EA]
    );
}

#[test]
fn a_packed_store_converts_exactly_and_rounds_to_nearest() {
    let store = |status, value| {
        let mut fpa = fpa(&[(0, value)], status);
        let words = fpa.store(Format::Packed, 0).expect("no trap enabled");
        (words, flags(&fpa))
    };
    let inexact = Exception::Inexact.flag();
    let third = [0x3FFD, 0xAAAA_AAAA, 0xAAAA_AAAB];
    assert_eq!(
        store(0, third),
        ([0x4000_1333, 0x3333_3333, 0x3333_3300, 0], inexact)
    );
    // 2^-16382 is 3.36210314311209350626…e-4932, where FPEmulator writes
    // …349869.
    let least_normal = [1, 0x8000_0000, 0];
    assert_eq!(
        store(fpsr::EP, least_normal),
        (
            [0x4000_4932, 0x3362_1031, 0x4311_2093, 0x5062_6000],
            inexact
        )
    );
    // 10^17 + 15 lies half way: to even.
    let tie = Internal {
        head: 0x4037,
        mantissa: 100_000_000_000_000_015 << 7,
    }
    .words();
    assert_eq!(store(0, tie), ([0x0001_7100, 0, 0x0000_0200, 0], inexact));
}
