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
        (FpaMonadicOp::Atn, ZERO, ZERO),
    ] {
        assert_eq!(monadic(op, E, argument), (expected, 0), "{op:?}");
    }
    let (power, raised) = dyadic(FpaDyadicOp::Pow, E, NEAREST, TWO, [0x4002, 0xA000_0000, 0]);
    assert_eq!((power, raised), ([0x4009, 0x8000_0000, 0], 0), "2^10");
    // An infinity is taken as a number with its exponent: the angle is π/2.
    let (angle, _) = monadic(FpaMonadicOp::Atn, E, INFINITY);
    assert_eq!(angle[..2], [0x3FFF, 0xC90F_DAA2], "ATN of infinity");
    let (angle, _) = dyadic(FpaDyadicOp::Pol, E, NEAREST, ONE, INFINITY);
    assert_eq!(angle[..2], [0x3FFF, 0xC90F_DAA2], "the point (1, infinity)");
}

#[test]
fn the_transcendental_functions_refuse_what_has_no_value() {
    let invalid = Exception::InvalidOperation.flag();
    assert_eq!(
        monadic(FpaMonadicOp::Sin, E, INFINITY),
        ([0x4000_7FFF, 0x4000_1100, 0], invalid),
        "SIN of infinity, reason 17"
    );
    assert_eq!(
        monadic(FpaMonadicOp::Log, E, [0x8000_3FFF, 0x8000_0000, 0]),
        ([0x4000_7FFF, 0x4000_1800, 0], invalid),
        "LOG of -1, reason 24, signed positive"
    );
    assert_eq!(
        monadic(FpaMonadicOp::Log, E, ZERO),
        ([0xC000_7FFF, 0, 0], Exception::DivisionByZero.flag()),
        "LOG of 0: minus infinity"
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
