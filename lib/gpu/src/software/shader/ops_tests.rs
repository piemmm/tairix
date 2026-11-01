use super::*;

fn op1(op: Op1, num: Num, x: u64) -> u64 {
    let mut lanes = [x];
    unary(op)(num, &mut lanes);
    let [value] = lanes;
    value
}

fn op2(op: Op2, num: Num, x: u64, y: u64) -> u64 {
    one_lane(binary(op), num, x, y)
}

fn compare(op: Cmp, num: Num, x: u64, y: u64) -> u64 {
    one_lane(comparer(op), num, x, y)
}

fn shift(op: Shift, num: Num, by: Num, x: u64, y: u64) -> u64 {
    let mut lanes = [x];
    shifter(op)(num, by, &mut lanes, &[y]);
    let [value] = lanes;
    value
}

fn convert(op: Conv, from: Num, to: Num, x: u64) -> u64 {
    let mut lanes = [x];
    converter(op)(from, to, &mut lanes);
    let [value] = lanes;
    value
}

const F32_NAN: u64 = 0x7fc0_0000;
const I32_MIN: u64 = 0x8000_0000;
const MINUS_ONE_32: u64 = 0xffff_ffff;

fn f32_bits(value: f32) -> u64 {
    u64::from(value.to_bits())
}

#[test]
fn integer_division_by_zero_gives_zero_and_min_over_minus_one_gives_min() {
    for num in [Num::I16, Num::I32, Num::I64] {
        for op in [Op2::SDiv, Op2::UDiv, Op2::SRem, Op2::SMod, Op2::UMod] {
            assert_eq!(op2(op, num, 7, 0), 0, "{op:?} of {num:?} by zero");
        }
    }
    assert_eq!(op2(Op2::SDiv, Num::I32, I32_MIN, MINUS_ONE_32), I32_MIN);
    assert_eq!(op2(Op2::SRem, Num::I32, I32_MIN, MINUS_ONE_32), 0);
    assert_eq!(op2(Op2::SMod, Num::I32, I32_MIN, MINUS_ONE_32), 0);
    assert_eq!(op2(Op2::SDiv, Num::I64, 1 << 63, u64::MAX), 1 << 63);
    assert_eq!(op2(Op2::SDiv, Num::I16, 0x8000, 0xffff), 0x8000);
}

#[test]
fn remainders_take_the_sign_their_operation_names() {
    let minus = |v: i64| v.cast_unsigned() & mask(32);
    assert_eq!(
        op2(Op2::SRem, Num::I32, minus(-7), 3),
        minus(-1),
        "of the dividend"
    );
    assert_eq!(op2(Op2::SMod, Num::I32, minus(-7), 3), 2, "of the divisor");
    assert_eq!(op2(Op2::SMod, Num::I32, 7, minus(-3)), minus(-2));
    assert_eq!(
        op2(Op2::FRem, Num::F32, f32_bits(-7.0), f32_bits(3.0)),
        f32_bits(-1.0)
    );
    assert_eq!(
        op2(Op2::FMod, Num::F32, f32_bits(-7.0), f32_bits(3.0)),
        f32_bits(2.0)
    );
}

#[test]
fn integer_arithmetic_wraps_at_its_width() {
    assert_eq!(op2(Op2::IAdd, Num::I32, MINUS_ONE_32, 1), 0);
    assert_eq!(op2(Op2::IMul, Num::I16, 0x8000, 2), 0);
    assert_eq!(op2(Op2::ISub, Num::I64, 0, 1), u64::MAX);
    assert_eq!(op1(Op1::SNegate, Num::I32, I32_MIN), I32_MIN);
    assert_eq!(op1(Op1::SAbs, Num::I32, I32_MIN), I32_MIN);
}

#[test]
fn a_shift_by_the_width_or_more_gives_zero() {
    for op in [Shift::Left, Shift::RightLogical, Shift::RightArithmetic] {
        assert_eq!(shift(op, Num::I32, Num::I32, MINUS_ONE_32, 32), 0, "{op:?}");
        assert_eq!(shift(op, Num::I16, Num::I32, 0xffff, 16), 0, "{op:?}");
        assert_eq!(
            shift(op, Num::I32, Num::I64, MINUS_ONE_32, 1 << 40),
            0,
            "{op:?} by a wide count"
        );
    }
    assert_eq!(
        shift(Shift::RightArithmetic, Num::I32, Num::I32, I32_MIN, 31),
        MINUS_ONE_32
    );
    assert_eq!(
        shift(Shift::RightLogical, Num::I32, Num::I32, I32_MIN, 31),
        1
    );
    assert_eq!(
        shift(Shift::Left, Num::I32, Num::I16, 1, 0x1_001f),
        I32_MIN,
        "the count is the low bits of its own width"
    );
}

#[test]
fn a_float_converts_to_an_integer_saturating_and_a_nan_to_zero() {
    let from = |value: f32| f32_bits(value);
    let unsigned = |value, to| convert(Conv::FloatToUnsigned, Num::F32, to, from(value));
    let signed_to = |value, to| convert(Conv::FloatToSigned, Num::F32, to, from(value));
    assert_eq!(unsigned(1e10, Num::I32), MINUS_ONE_32);
    assert_eq!(unsigned(-1.0, Num::I32), 0);
    assert_eq!(unsigned(f32::NAN, Num::I32), 0);
    assert_eq!(unsigned(70000.0, Num::I16), 0xffff);
    assert_eq!(unsigned(1e30, Num::I64), u64::MAX);
    assert_eq!(signed_to(1e10, Num::I32), 0x7fff_ffff);
    assert_eq!(signed_to(-1e10, Num::I32), I32_MIN);
    assert_eq!(signed_to(f32::NAN, Num::I32), 0);
    assert_eq!(
        signed_to(-2.9, Num::I32),
        mask(32) - 1,
        "truncated toward zero"
    );
    assert_eq!(signed_to(f32::NEG_INFINITY, Num::I64), 1 << 63);
}

#[test]
fn an_integer_converts_to_a_float_rounded_once() {
    assert_eq!(
        convert(Conv::UnsignedToFloat, Num::I32, Num::F32, MINUS_ONE_32),
        f32_bits(4_294_967_296.0)
    );
    assert_eq!(
        convert(Conv::SignedToFloat, Num::I64, Num::F32, (1u64 << 24) + 1),
        f32_bits(16_777_216.0),
        "a tie rounds to even"
    );
    assert_eq!(
        convert(Conv::SignedToFloat, Num::I32, Num::F16, MINUS_ONE_32),
        0xbc00
    );
    assert_eq!(
        convert(Conv::UnsignedToFloat, Num::I32, Num::F16, 70000),
        0x7c00,
        "past a half's range"
    );
}

#[test]
fn a_float_is_computed_wide_and_rounded_once_to_its_width() {
    let one = f32_bits(1.0);
    assert_eq!(
        op2(Op2::FAdd, Num::F32, one, f32_bits(f32::EPSILON / 2.0)),
        one,
        "a tie rounds to even"
    );
    assert_eq!(
        op2(Op2::FAdd, Num::F32, one, f32_bits(f32::EPSILON)),
        f32_bits(1.0 + f32::EPSILON)
    );
    assert_eq!(op2(Op2::FAdd, Num::F16, 0x3c00, 0x3c00), 0x4000);
    assert_eq!(
        op2(Op2::FAdd, Num::F16, 0x7bff, 0x7bff),
        0x7c00,
        "past a half's range"
    );
    assert_eq!(op2(Op2::FDiv, Num::F32, one, 0), f32_bits(f32::INFINITY));
    let quotient = op2(Op2::FDiv, Num::F32, 0, 0);
    assert!(f32::from_bits(u32::try_from(quotient).expect("a word")).is_nan());
}

#[test]
fn bit_searches_answer_minus_one_where_no_bit_is_set() {
    assert_eq!(op1(Op1::FindUMsb, Num::I32, 0), MINUS_ONE_32);
    assert_eq!(op1(Op1::FindILsb, Num::I32, 0), MINUS_ONE_32);
    assert_eq!(op1(Op1::FindSMsb, Num::I32, 0), MINUS_ONE_32);
    assert_eq!(op1(Op1::FindSMsb, Num::I32, MINUS_ONE_32), MINUS_ONE_32);
    assert_eq!(op1(Op1::FindUMsb, Num::I32, I32_MIN), 31);
    assert_eq!(
        op1(Op1::FindSMsb, Num::I32, I32_MIN),
        30,
        "the highest bit unlike the sign"
    );
    assert_eq!(op1(Op1::FindILsb, Num::I64, 1 << 40), 40);
    assert_eq!(op1(Op1::BitReverse, Num::I16, 1), 0x8000);
    assert_eq!(
        op1(Op1::BitCount, Num::I16, 0xffff_ffff),
        16,
        "only the bits of its width"
    );
}

#[test]
fn an_unordered_comparison_holds_where_either_is_a_nan() {
    let equal = |ordered| Cmp::Float {
        relation: Relation::Equal,
        ordered,
    };
    let not_equal = |ordered| Cmp::Float {
        relation: Relation::NotEqual,
        ordered,
    };
    let one = f32_bits(1.0);
    assert_eq!(compare(equal(true), Num::F32, F32_NAN, F32_NAN), 0);
    assert_eq!(compare(equal(false), Num::F32, F32_NAN, F32_NAN), 1);
    assert_eq!(compare(not_equal(true), Num::F32, F32_NAN, one), 0);
    assert_eq!(compare(not_equal(false), Num::F32, F32_NAN, one), 1);
    assert_eq!(
        compare(equal(true), Num::F32, f32_bits(-0.0), 0),
        1,
        "the zeros are equal"
    );
}

#[test]
fn quantizing_to_a_half_flushes_what_a_half_cannot_hold_normally() {
    let quantized = |value: f32| convert(Conv::QuantizeToF16, Num::F32, Num::F32, f32_bits(value));
    assert_eq!(quantized(1e-6), 0, "too small for a normal half");
    assert_eq!(quantized(-1e-6), f32_bits(-0.0), "its sign kept");
    assert_eq!(quantized(70000.0), f32_bits(f32::INFINITY));
    assert_eq!(quantized(1.0 / 3.0), f32_bits(0.333_251_95));
    let nan = quantized(f32::NAN);
    assert!(f32::from_bits(u32::try_from(nan).expect("a word")).is_nan());
}

#[test]
fn a_bit_field_changes_and_reads_only_bits_within_its_width() {
    assert_eq!(
        bit_insert(Num::I32, 0, MINUS_ONE_32, 28, 8),
        0xf000_0000,
        "a field past the width is cut"
    );
    assert_eq!(
        bit_insert(Num::I32, 5, MINUS_ONE_32, 32, 1),
        5,
        "a field wholly past it changes nothing"
    );
    assert_eq!(
        bit_insert(Num::I32, 5, MINUS_ONE_32, 0, 0),
        5,
        "nor does one of no bits"
    );
    assert_eq!(
        bit_extract(Num::I32, 0xf000_0000, 28, 8, true),
        MINUS_ONE_32,
        "sign-extended from the field's top"
    );
    assert_eq!(bit_extract(Num::I32, 0xf000_0000, 28, 8, false), 0xf);
    assert_eq!(bit_extract(Num::I32, MINUS_ONE_32, 32, 4, false), 0);
    assert_eq!(bit_extract(Num::I32, MINUS_ONE_32, 3, 0, true), 0);
}

#[test]
fn ldexp_scales_exactly_and_rounds_once_into_the_subnormals() {
    let double = |value: f64| value.to_bits();
    assert_eq!(ldexp(Num::F64, double(1.5), 3), double(12.0));
    assert_eq!(
        ldexp(Num::F64, double(1.0), -1074),
        double(f64::from_bits(1)),
        "the least subnormal"
    );
    assert_eq!(
        ldexp(Num::F64, double(1.5), -1075),
        double(f64::from_bits(1)),
        "rounded once, to even"
    );
    assert_eq!(ldexp(Num::F64, double(1.0), -1076), 0);
    assert_eq!(
        ldexp(Num::F64, double(-1.0), 2000),
        double(f64::NEG_INFINITY)
    );
    assert_eq!(
        ldexp(Num::F32, f32_bits(1.0), -149),
        1,
        "a single's least subnormal"
    );
    assert_eq!(ldexp(Num::F32, f32_bits(1.0), i64::MIN), 0);
    assert_eq!(ldexp(Num::F32, f32_bits(-0.0), 7), f32_bits(-0.0));
}

#[test]
fn frexp_and_modf_keep_zeros_infinities_and_nans() {
    assert_eq!(frexp(Num::F32, f32_bits(8.0)), (f32_bits(0.5), 4));
    assert_eq!(frexp(Num::F32, f32_bits(-0.75)), (f32_bits(-0.75), 0));
    assert_eq!(
        frexp(Num::F32, 1),
        (f32_bits(0.5), (-148i64).cast_unsigned()),
        "a subnormal's own exponent"
    );
    assert_eq!(
        frexp(Num::F32, f32_bits(f32::INFINITY)),
        (f32_bits(f32::INFINITY), 0)
    );
    assert_eq!(
        modf(Num::F32, f32_bits(-2.5)),
        (f32_bits(-0.5), f32_bits(-2.0))
    );
    assert_eq!(
        modf(Num::F32, f32_bits(f32::NEG_INFINITY)),
        (f32_bits(-0.0), f32_bits(f32::NEG_INFINITY))
    );
    assert_eq!(
        modf(Num::F32, f32_bits(-2.0)),
        (f32_bits(-0.0), f32_bits(-2.0)),
        "a whole number's fraction keeps its sign"
    );
    assert_eq!(
        modf(Num::F32, f32_bits(-0.0)),
        (f32_bits(-0.0), f32_bits(-0.0))
    );
    let (fraction, whole) = modf(Num::F32, F32_NAN);
    assert!(f32::from_bits(u32::try_from(fraction).expect("a word")).is_nan() && whole == F32_NAN);
}

#[test]
fn a_nan_result_is_its_width_s_one_quiet_nan_whatever_its_operands() {
    let payload = 0xffc0_1234;
    assert_eq!(op2(Op2::FAdd, Num::F32, payload, f32_bits(1.0)), F32_NAN);
    assert_eq!(op1(Op1::FNegate, Num::F32, payload), F32_NAN);
    let infinity = f32_bits(f32::INFINITY);
    assert_eq!(op2(Op2::FSub, Num::F32, infinity, infinity), F32_NAN);
    assert_eq!(
        op1(Op1::Sqrt, Num::F32, f32_bits(-1.0)),
        0,
        "below its domain, zero"
    );
    assert_eq!(
        op2(Op2::FMul, Num::F64, 0xfff8_0000_0000_0042, 0),
        0x7ff8_0000_0000_0000
    );
    assert_eq!(op2(Op2::FAdd, Num::F16, 0xfe42, 0x3c00), 0x7e00);
    assert_eq!(
        convert(Conv::Float, Num::F32, Num::F64, payload),
        0x7ff8_0000_0000_0000
    );
    assert_eq!(
        single_bits(f64::from_bits(0xfff8_0000_0000_0042)),
        0x7fc0_0000
    );
}

#[test]
fn a_kernel_over_a_span_computes_each_lane_as_that_lane_alone() {
    const LANES: usize = 64;
    let xs: [u64; LANES] =
        core::array::from_fn(|i| (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (i % 40));
    let ys: [u64; LANES] = core::array::from_fn(|i| xs[LANES - 1 - i].rotate_left(17));
    let zs: [u64; LANES] = core::array::from_fn(|i| xs[i] ^ 0x3c00_3f80_0000);
    let alone = |f: &dyn Fn(usize) -> u64| -> [u64; LANES] { core::array::from_fn(f) };

    let pairs = [
        (binary(Op2::IMul), Num::I32),
        (binary(Op2::SMod), Num::I16),
        (binary(Op2::FDiv), Num::F32),
        (binary(Op2::Pow), Num::F64),
        (comparer(Cmp::SLess), Num::I64),
        (
            comparer(Cmp::Float {
                relation: Relation::Less,
                ordered: false,
            }),
            Num::F16,
        ),
    ];
    for (at, (kernel, num)) in pairs.into_iter().enumerate() {
        let mut spanned = xs;
        kernel(num, &mut spanned, &ys);
        let expected = alone(&|lane| one_lane(kernel, num, xs[lane], ys[lane]));
        assert_eq!(spanned, expected, "pair kernel {at}");
    }

    for (op, num) in [
        (Op1::BitCount, Num::I16),
        (Op1::Sqrt, Num::F32),
        (Op1::IsNan, Num::F64),
    ] {
        let mut spanned = xs;
        unary(op)(num, &mut spanned);
        assert_eq!(spanned, alone(&|lane| op1(op, num, xs[lane])), "{op:?}");
    }

    for (op, num) in [(Op3::FMix, Num::F32), (Op3::SClamp, Num::I32)] {
        let mut spanned = xs;
        ternary(op)(num, &mut spanned, &ys, &zs);
        let expected = alone(&|lane| {
            let mut one = [xs[lane]];
            ternary(op)(num, &mut one, &[ys[lane]], &[zs[lane]]);
            one[0]
        });
        assert_eq!(spanned, expected, "{op:?}");
    }

    let mut spanned = xs;
    shifter(Shift::RightArithmetic)(Num::I32, Num::I16, &mut spanned, &ys);
    let expected = alone(&|lane| {
        shift(
            Shift::RightArithmetic,
            Num::I32,
            Num::I16,
            xs[lane],
            ys[lane],
        )
    });
    assert_eq!(spanned, expected, "a shift");

    let mut spanned = xs;
    converter(Conv::FloatToSigned)(Num::F64, Num::I16, &mut spanned);
    let expected = alone(&|lane| convert(Conv::FloatToSigned, Num::F64, Num::I16, xs[lane]));
    assert_eq!(spanned, expected, "a conversion");
}

/// A float rounded toward zero never lands past its value: one between two
/// halves takes the one nearer zero, a tie the same, an overflow the largest
/// finite half, and what a half holds exactly, an infinity and a zero stay.
#[test]
fn a_float_rounded_toward_zero_never_lands_past_its_value() {
    let rtz = |value: f32| convert(Conv::FloatTowardZero, Num::F32, Num::F16, f32_bits(value));
    let rte = |value: f32| convert(Conv::Float, Num::F32, Num::F16, f32_bits(value));
    assert_eq!((rte(1.0007), rtz(1.0007)), (0x3c01, 0x3c00));
    assert_eq!((rte(-1.0007), rtz(-1.0007)), (0xbc01, 0xbc00));
    // 1 + 3 * 2^-11, halfway between 0x3c01 and 0x3c02.
    let tie = f32::from_bits(0x3f80_3000);
    assert_eq!((rte(tie), rtz(tie)), (0x3c02, 0x3c01));
    assert_eq!((rte(70000.0), rtz(70000.0)), (0x7c00, 0x7bff));
    assert_eq!((rte(-70000.0), rtz(-70000.0)), (0xfc00, 0xfbff));
    assert_eq!(rtz(65510.0), 0x7bff);
    assert_eq!(rtz(1.5), 0x3e00);
    assert_eq!(rtz(f32::INFINITY), 0x7c00);
    assert_eq!(rtz(f32::NEG_INFINITY), 0xfc00);
    assert_eq!(rtz(f32::NAN), 0x7e00);
    assert_eq!(rtz(0.0), 0x0000);
    assert_eq!(rtz(-0.0), 0x8000);
    // Past half the least subnormal, so the nearest is the least subnormal.
    assert_eq!((rte(4e-8), rtz(4e-8)), (0x0001, 0x0000));
    assert_eq!(rtz(-4e-8), 0x8000, "a zero keeps its sign");
    assert_eq!((rte(1e-7), rtz(1e-7)), (0x0002, 0x0001));
}
