//! The IEEE layer held to the host's IEEE 754 arithmetic, and the formats'
//! layouts.

extern crate std;

use core::cmp::Ordering;

use tairix_fuzzseed::Prng;

use crate::{
    fpa_words, from_fpa_words, Binary, Class, Extended, Flags, Ieee, Integral, Mode, Nans, Policy,
    Tininess,
};

const ARM: Policy = Policy {
    tininess: Tininess::BeforeRounding,
    nans: Nans::Propagate,
    flush_to_zero: false,
};

const X86: Policy = Policy {
    tininess: Tininess::AfterRounding,
    nans: Nans::Propagate,
    flush_to_zero: false,
};

fn double(mode: Mode) -> Ieee {
    Ieee {
        format: Binary::DOUBLE,
        mode,
        policy: ARM,
    }
}

fn single(mode: Mode) -> Ieee {
    Ieee {
        format: Binary::SINGLE,
        mode,
        policy: ARM,
    }
}

/// Doubles spread over every class and exponent: raw bits, with values close
/// to one another often enough that sums cancel.
fn random_double(rng: &mut Prng) -> f64 {
    let bits = rng.next_u64();
    match bits % 8 {
        0 => f64::from_bits(bits & 0x800F_FFFF_FFFF_FFFF),
        1 => f64::from_bits(bits | 0x7FF0_0000_0000_0000),
        2..=4 => {
            let exponent = (bits >> 52) % 400 + 823;
            f64::from_bits(bits & 0x800F_FFFF_FFFF_FFFF | exponent << 52)
        }
        _ => f64::from_bits(bits),
    }
}

fn random_single(rng: &mut Prng) -> f32 {
    let bits = rng.next_u32();
    match bits % 8 {
        0 => f32::from_bits(bits & 0x807F_FFFF),
        1 => f32::from_bits(bits | 0x7F80_0000),
        2..=4 => f32::from_bits(bits & 0x807F_FFFF | ((bits >> 23) % 60 + 97) << 23),
        _ => f32::from_bits(bits),
    }
}

/// Whether two results agree: bit for bit, or both NaNs, the host choosing
/// its NaN by rules of its own.
fn agree(ours: u64, host: f64) -> bool {
    if host.is_nan() {
        f64::from_bits(ours).is_nan()
    } else {
        ours == host.to_bits()
    }
}

fn agree_single(ours: u64, host: f32) -> bool {
    if host.is_nan() {
        f32::from_bits(u32::try_from(ours).expect("a single")).is_nan()
    } else {
        ours == u64::from(host.to_bits())
    }
}

#[test]
fn double_arithmetic_rounds_to_nearest_as_the_host_does() {
    let mut rng = Prng::new(0x5EED_0064);
    let fp = double(Mode::Nearest);
    for _ in 0..40_000 {
        let (a, b, addend) = (
            random_double(&mut rng),
            random_double(&mut rng),
            random_double(&mut rng),
        );
        let (x, y) = (a.to_bits(), b.to_bits());
        assert!(agree(fp.add(x, y).bits, a + b), "{a:e} + {b:e}");
        assert!(agree(fp.sub(x, y).bits, a - b), "{a:e} - {b:e}");
        assert!(agree(fp.mul(x, y).bits, a * b), "{a:e} * {b:e}");
        assert!(agree(fp.div(x, y).bits, a / b), "{a:e} / {b:e}");
        assert!(agree(fp.sqrt(x).bits, a.sqrt()), "sqrt {a:e}");
        assert!(
            agree(
                fp.fused_multiply_add(addend.to_bits(), x, y).bits,
                a.mul_add(b, addend)
            ),
            "{a:e} * {b:e} + {addend:e}"
        );
    }
}

#[test]
fn single_arithmetic_rounds_to_nearest_as_the_host_does() {
    let mut rng = Prng::new(0x5EED_0032);
    let fp = single(Mode::Nearest);
    for _ in 0..40_000 {
        let (a, b, addend) = (
            random_single(&mut rng),
            random_single(&mut rng),
            random_single(&mut rng),
        );
        let (x, y) = (u64::from(a.to_bits()), u64::from(b.to_bits()));
        assert!(agree_single(fp.add(x, y).bits, a + b), "{a:e} + {b:e}");
        assert!(agree_single(fp.sub(x, y).bits, a - b), "{a:e} - {b:e}");
        assert!(agree_single(fp.mul(x, y).bits, a * b), "{a:e} * {b:e}");
        assert!(agree_single(fp.div(x, y).bits, a / b), "{a:e} / {b:e}");
        assert!(agree_single(fp.sqrt(x).bits, a.sqrt()), "sqrt {a:e}");
        assert!(
            agree_single(
                fp.fused_multiply_add(u64::from(addend.to_bits()), x, y)
                    .bits,
                a.mul_add(b, addend)
            ),
            "{a:e} * {b:e} + {addend:e}"
        );
    }
}

/// Doubles whose sums, products, quotients and roots stay well inside the
/// normal range, so the host's error terms below are exact.
fn moderate_double(rng: &mut Prng) -> f64 {
    let bits = rng.next_u64();
    let exponent = (bits >> 52) % 200 + 923;
    f64::from_bits(bits & 0x800F_FFFF_FFFF_FFFF | exponent << 52)
}

/// What `mode` rounds an exact value to, the host rounding it to nearest as
/// `nearest` with an error, `exact − nearest`, of sign `error`.
fn expected(nearest: f64, error: Ordering, mode: Mode) -> f64 {
    let exact_above = match error {
        Ordering::Equal => return nearest,
        Ordering::Greater => true,
        Ordering::Less => false,
    };
    let upward = match mode {
        Mode::Up => true,
        Mode::Down => false,
        Mode::Zero => nearest < 0.0,
        Mode::Nearest | Mode::NearestAway => return nearest,
    };
    match (upward, exact_above) {
        (true, true) => up_from(nearest),
        (false, false) => down_from(nearest),
        _ => nearest,
    }
}

/// The next double toward plus infinity.
fn up_from(x: f64) -> f64 {
    if x == 0.0 {
        f64::from_bits(1)
    } else if x > 0.0 {
        f64::from_bits(x.to_bits() + 1)
    } else {
        f64::from_bits(x.to_bits() - 1)
    }
}

/// The next double toward minus infinity.
fn down_from(x: f64) -> f64 {
    -up_from(-x)
}

fn sign_of(x: f64) -> Ordering {
    x.partial_cmp(&0.0).expect("finite")
}

#[test]
fn directed_roundings_take_the_neighbour_the_exact_error_points_to() {
    let mut rng = Prng::new(0x5EED_D1EC);
    for _ in 0..20_000 {
        let (a, b) = (moderate_double(&mut rng), moderate_double(&mut rng));
        let (x, y) = (a.to_bits(), b.to_bits());
        let sum = a + b;
        let back = sum - a;
        let sum_error = (a - (sum - back)) + (b - back);
        let product = a * b;
        let product_error = a.mul_add(b, -product);
        let quotient = a / b;
        let quotient_error = (-quotient).mul_add(b, a) * b.signum();
        let root = a.abs().sqrt();
        let root_error = (-root).mul_add(root, a.abs());
        for mode in [Mode::Down, Mode::Up, Mode::Zero] {
            let fp = double(mode);
            for (ours, nearest, error, what) in [
                (fp.add(x, y), sum, sum_error, "sum"),
                (fp.mul(x, y), product, product_error, "product"),
                (fp.div(x, y), quotient, quotient_error, "quotient"),
                (fp.sqrt(a.abs().to_bits()), root, root_error, "root"),
            ] {
                if sum == 0.0 && what == "sum" {
                    continue;
                }
                let want = expected(nearest, sign_of(error), mode);
                assert_eq!(
                    ours.bits,
                    want.to_bits(),
                    "{what} of {a:e}, {b:e} rounded {mode:?}"
                );
                assert_eq!(
                    ours.flags.contains(Flags::INEXACT),
                    error != 0.0,
                    "{what} of {a:e}, {b:e} {mode:?}"
                );
            }
        }
    }
}

#[test]
fn an_exact_zero_sum_is_negative_only_rounding_down() {
    let one = 1.0f64.to_bits();
    for mode in Mode::ALL {
        let zero = double(mode).sub(one, one).bits;
        assert_eq!(
            zero,
            if mode == Mode::Down {
                (-0.0f64).to_bits()
            } else {
                0
            }
        );
    }
    let negative_zero = (-0.0f64).to_bits();
    assert_eq!(
        double(Mode::Nearest).add(negative_zero, negative_zero).bits,
        negative_zero
    );
}

#[test]
fn an_overflow_is_infinity_or_the_largest_finite_value_by_mode() {
    let big = f64::MAX.to_bits();
    for (mode, positive, negative) in [
        (Mode::Nearest, f64::INFINITY, f64::NEG_INFINITY),
        (Mode::NearestAway, f64::INFINITY, f64::NEG_INFINITY),
        (Mode::Up, f64::INFINITY, -f64::MAX),
        (Mode::Down, f64::MAX, f64::NEG_INFINITY),
        (Mode::Zero, f64::MAX, -f64::MAX),
    ] {
        let fp = double(mode);
        let up = fp.add(big, big);
        let down = fp.add((-f64::MAX).to_bits(), (-f64::MAX).to_bits());
        assert_eq!(up.bits, positive.to_bits(), "{mode:?}");
        assert_eq!(down.bits, negative.to_bits(), "{mode:?}");
        assert_eq!(up.flags, Flags::OVERFLOW | Flags::INEXACT);
    }
}

#[test]
fn an_underflow_is_tiny_before_or_after_rounding_as_the_policy_judges() {
    // (1 + 2^-52) × (2^-1022 − 2^-1074) = 2^-1022 − 2^-1126: tiny exactly,
    // but 2^-1022 once rounded with the exponent unbounded.
    let a = (1.0f64 + f64::EPSILON).to_bits();
    let b = 0x000F_FFFF_FFFF_FFFF;
    for (policy, underflow) in [(ARM, true), (X86, false)] {
        let fp = Ieee {
            format: Binary::DOUBLE,
            mode: Mode::Nearest,
            policy,
        };
        let product = fp.mul(a, b);
        assert_eq!(product.bits, f64::MIN_POSITIVE.to_bits(), "{policy:?}");
        let mut flags = Flags::INEXACT;
        if underflow {
            flags |= Flags::UNDERFLOW;
        }
        assert_eq!(product.flags, flags, "{policy:?}");
    }
    // An exact subnormal result underflows under neither.
    let halved = double(Mode::Nearest).mul(f64::MIN_POSITIVE.to_bits(), 0.5f64.to_bits());
    assert_eq!(
        halved,
        crate::Outcome {
            bits: (f64::MIN_POSITIVE / 2.0).to_bits(),
            flags: Flags::NONE,
        }
    );
}

#[test]
fn invalid_operations_answer_the_default_nan() {
    let fp = double(Mode::Nearest);
    let (inf, zero, one) = (f64::INFINITY.to_bits(), 0u64, 1.0f64.to_bits());
    let quiet = f64::NAN.to_bits();
    let invalid = |outcome: crate::Outcome| {
        outcome.bits == Binary::DOUBLE.default_nan() && outcome.flags == Flags::INVALID
    };
    assert!(invalid(fp.sub(inf, inf)));
    assert!(invalid(fp.mul(inf, zero)));
    assert!(invalid(fp.div(zero, zero)));
    assert!(invalid(fp.div(inf, inf)));
    assert!(invalid(fp.sqrt((-1.0f64).to_bits())));
    assert!(invalid(fp.rem(one, zero)));
    assert!(invalid(fp.rem(inf, one)));
    assert!(invalid(fp.fused_multiply_add(quiet, inf, zero)));
    // A signalling addend is the NaN chosen, as beside any other product.
    let signalling = 0xFFF0_0000_0000_0005;
    assert_eq!(
        fp.fused_multiply_add(signalling, inf, zero),
        crate::Outcome {
            bits: 0xFFF8_0000_0000_0005,
            flags: Flags::INVALID,
        }
    );
    assert_eq!(
        fp.div(one, zero),
        crate::Outcome {
            bits: inf,
            flags: Flags::DIVIDE_BY_ZERO,
        }
    );
}

#[test]
fn a_nan_result_is_the_first_signalling_operand_quieted_or_the_default() {
    let format = Binary::DOUBLE;
    let signalling = 0x7FF0_0000_0000_0005;
    let quiet = 0xFFF8_0000_0000_0007;
    let one = 1.0f64.to_bits();
    let fp = double(Mode::Nearest);
    assert_eq!(
        fp.add(quiet, signalling),
        crate::Outcome {
            bits: signalling | format.quiet_bit(),
            flags: Flags::INVALID,
        },
        "a signalling NaN is chosen before a quiet one"
    );
    assert_eq!(fp.mul(one, quiet).bits, quiet);
    assert_eq!(fp.mul(one, quiet).flags, Flags::NONE);
    let default = Ieee {
        policy: Policy {
            nans: Nans::Default,
            ..ARM
        },
        ..fp
    };
    assert_eq!(default.add(quiet, one).bits, format.default_nan());
    // Narrowed, a payload keeps its top bits.
    let narrowed = fp.convert(signalling | 0x0004_0000_0000_0000, Binary::SINGLE);
    assert_eq!(narrowed.bits, 0x7FE0_0000 | 0x0040_0000);
    assert_eq!(narrowed.flags, Flags::INVALID);
}

#[test]
#[allow(
    clippy::float_cmp,
    reason = "the remainder is exact, so exact equality is the property under test"
)]
fn a_remainder_is_exact_and_rounds_its_quotient_to_the_even_integer() {
    let fp = double(Mode::Nearest);
    let rem = |a: f64, b: f64| f64::from_bits(fp.rem(a.to_bits(), b.to_bits()).bits);
    assert_eq!(rem(7.0, 2.0), -1.0, "3.5 goes to 4");
    assert_eq!(rem(5.0, 2.0), 1.0, "2.5 goes to 2");
    assert_eq!(rem(6.5, 2.0), 0.5);
    assert_eq!(rem(-7.0, 2.0), 1.0);
    assert_eq!(rem(1.0, 4.0), 1.0);
    assert_eq!(rem(3.0, 4.0), -1.0);
    assert_eq!(rem(2.0, 4.0), 2.0, "a half with the even quotient zero");
    assert_eq!(rem(6.0, 4.0), -2.0, "a half with the even quotient two");
    assert_eq!(rem(-0.0, 3.0).to_bits(), (-0.0f64).to_bits());
    assert_eq!(rem(4.0, 2.0).to_bits(), 0);
    assert_eq!(rem(-4.0, 2.0).to_bits(), (-0.0f64).to_bits());
    let mut rng = Prng::new(0x5EED_4E4D);
    for _ in 0..20_000 {
        let a = f64::from(rng.next_u32() >> 4) * f64::from(1u32 << (rng.next_u8() % 20));
        let b = f64::from((rng.next_u32() >> 12).max(1));
        let r = rem(a, b);
        assert!(r.abs() <= b / 2.0, "{a} rem {b} = {r}");
        let n = (a - r) / b;
        assert_eq!(n.fract(), 0.0, "{a} rem {b} = {r}");
        if r.abs() == b / 2.0 {
            assert_eq!(n % 2.0, 0.0, "{a} rem {b}: a tie keeps the even quotient");
        }
    }
    let huge = rem(f64::MAX, 3.0);
    assert!(huge.abs() <= 1.5 && huge.fract() == 0.0);
}

#[test]
fn rounding_to_an_integral_value_follows_the_mode() {
    let cases = [
        (2.5, [2.0, 2.0, 3.0, 2.0, 3.0]),
        (-2.5, [-2.0, -3.0, -2.0, -2.0, -3.0]),
        (3.5, [4.0, 3.0, 4.0, 3.0, 4.0]),
        (0.3, [0.0, 0.0, 1.0, 0.0, 0.0]),
        (-0.3, [-0.0, -1.0, -0.0, -0.0, -0.0]),
        (1e300, [1e300; 5]),
    ];
    for (value, wants) in cases {
        for (mode, want) in Mode::ALL.into_iter().zip(wants) {
            let fp = double(mode);
            let exact = fp.round_to_integral(f64::to_bits(value), true);
            assert_eq!(exact.bits, f64::to_bits(want), "{value} {mode:?}");
            assert_eq!(
                exact.flags.contains(Flags::INEXACT),
                value.to_bits() != want.to_bits(),
                "{value} {mode:?}"
            );
            assert!(fp
                .round_to_integral(f64::to_bits(value), false)
                .flags
                .is_empty());
        }
    }
}

#[test]
fn integers_convert_in_each_mode() {
    let fp = double(Mode::Nearest);
    assert_eq!(
        fp.to_fixed(f64::to_bits(-2.5), 0, Mode::NearestAway),
        Integral::Value {
            negative: true,
            magnitude: 3,
            inexact: true,
        }
    );
    assert_eq!(
        fp.to_fixed(f64::to_bits(2.5), 0, Mode::Nearest),
        Integral::Value {
            negative: false,
            magnitude: 2,
            inexact: true,
        }
    );
    assert_eq!(
        fp.to_fixed(f64::NAN.to_bits(), 0, Mode::Zero),
        Integral::Nan
    );
    assert_eq!(
        fp.to_fixed(f64::to_bits(1e30), 0, Mode::Zero),
        Integral::OutOfRange { negative: false }
    );
    assert_eq!(fp.from_fixed(true, 3, 0).bits, f64::to_bits(-3.0));
    let big = fp.from_fixed(false, u64::MAX, 0);
    assert_eq!(big.bits, f64::to_bits(18_446_744_073_709_551_616.0));
    assert_eq!(big.flags, Flags::INEXACT);
    assert_eq!(
        fp.from_fixed(true, 0, 0).bits,
        0,
        "an integer zero is positive"
    );
}

#[test]
fn a_double_narrows_to_a_single_as_the_host_narrows_it_and_widens_exactly() {
    let mut rng = Prng::new(0x5EED_C0DE);
    let narrow = double(Mode::Nearest);
    let widen = single(Mode::Nearest);
    for _ in 0..40_000 {
        let a = random_double(&mut rng);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the host's own narrowing is the oracle"
        )]
        let host = a as f32;
        assert!(
            agree_single(narrow.convert(a.to_bits(), Binary::SINGLE).bits, host),
            "{a:e}"
        );
        let s = random_single(&mut rng);
        let wide = widen.convert(u64::from(s.to_bits()), Binary::DOUBLE);
        assert!(agree(wide.bits, f64::from(s)), "{s:e}");
    }
}

#[test]
fn comparisons_order_values_and_raise_invalid_on_nans_as_asked() {
    let fp = double(Mode::Nearest);
    let bits = f64::to_bits;
    assert_eq!(
        fp.compare(bits(1.0), bits(2.0), false),
        (Some(Ordering::Less), Flags::NONE)
    );
    assert_eq!(
        fp.compare(bits(-0.0), bits(0.0), false),
        (Some(Ordering::Equal), Flags::NONE)
    );
    assert_eq!(
        fp.compare(bits(-3.0), bits(-2.0), false),
        (Some(Ordering::Less), Flags::NONE)
    );
    assert_eq!(
        fp.compare(bits(f64::NEG_INFINITY), bits(-f64::MAX), false)
            .0,
        Some(Ordering::Less)
    );
    assert_eq!(
        fp.compare(bits(f64::NAN), bits(1.0), false),
        (None, Flags::NONE)
    );
    assert_eq!(
        fp.compare(bits(f64::NAN), bits(1.0), true),
        (None, Flags::INVALID)
    );
    assert_eq!(
        fp.compare(0x7FF0_0000_0000_0001, bits(1.0), false),
        (None, Flags::INVALID)
    );
    let mut rng = Prng::new(0x5EED_0C3F);
    for _ in 0..20_000 {
        let (a, b) = (random_double(&mut rng), random_double(&mut rng));
        assert_eq!(
            fp.compare(a.to_bits(), b.to_bits(), false).0,
            a.partial_cmp(&b)
        );
    }
}

#[test]
fn the_extended_value_has_the_x87_memory_layout() {
    let x = Extended::new(true, 0x4000, 0xC000_0000_0000_0001);
    assert_eq!(
        x.to_bytes(),
        [0x01, 0, 0, 0, 0, 0, 0, 0xC0, 0x00, 0xC0],
        "the significand first, then sign and exponent"
    );
    assert_eq!(Extended::from_bytes(x.to_bytes()), x);
    assert_eq!(
        Extended::from_int(-6),
        Extended::new(true, 0x4001, 0xC000_0000_0000_0000)
    );
    assert_eq!(Extended::from_magnitude(false, 0), Extended::ZERO);
}

#[test]
fn the_fpa_keeps_a_double_high_word_first() {
    assert_eq!(fpa_words(1.0f64.to_bits()), [0x3FF0_0000, 0]);
    let mut rng = Prng::new(0x5EED_F9A0);
    for _ in 0..1000 {
        let bits = rng.next_u64();
        assert_eq!(from_fpa_words(fpa_words(bits)), bits);
    }
}

#[test]
fn flush_to_zero_makes_a_result_tiny_before_rounding_zero_raising_underflow_alone() {
    let flushing = Ieee {
        policy: Policy {
            flush_to_zero: true,
            ..ARM
        },
        ..single(Mode::Nearest)
    };
    let least_normal = 0x0080_0000;
    let half = 0x3F00_0000;
    let flushed = flushing.mul(least_normal, half);
    assert_eq!(
        flushed,
        crate::Outcome {
            bits: 0,
            flags: Flags::UNDERFLOW
        }
    );
    // 2^-126 × (1 − 2^-24): tiny before rounding, though it rounds up to the
    // least normal.
    let just_below = flushing.mul(least_normal, 0x3F7F_FFFF);
    assert_eq!(just_below.bits, 0);
    assert_eq!(
        single(Mode::Nearest).mul(least_normal, 0x3F7F_FFFF).bits,
        least_normal
    );
    let negative = flushing.mul(least_normal | 0x8000_0000, half);
    assert_eq!(negative.bits, 0x8000_0000, "a zero of its sign");
    assert_eq!(
        single(Mode::Nearest).mul(least_normal, half).bits,
        0x0040_0000
    );
    assert_eq!(flushing.mul(least_normal, 0x3F80_0000).bits, least_normal);
}

#[test]
fn binary16_holds_its_own_range_and_converts_with_the_others() {
    let half = Ieee {
        format: Binary::HALF,
        mode: Mode::Nearest,
        policy: ARM,
    };
    assert_eq!(Binary::HALF.infinity(false), 0x7C00);
    assert_eq!(Binary::HALF.default_nan(), 0x7E00);
    assert_eq!(Binary::HALF.largest(true), 0xFBFF);
    assert_eq!(half.add(0x3C00, 0x3C00).bits, 0x4000, "1 + 1");
    let to_half = single(Mode::Nearest).convert(0x3F80_0000, Binary::HALF);
    assert_eq!(to_half.bits, 0x3C00);
    let overflowed = single(Mode::Nearest).convert(0x4780_0000, Binary::HALF);
    assert_eq!(overflowed.bits, 0x7C00, "65536 overflows");
    assert!(overflowed.flags.contains(Flags::OVERFLOW));
    assert_eq!(
        half.convert(0x0001, Binary::SINGLE).bits,
        0x3380_0000,
        "the least subnormal"
    );
}

#[test]
fn fixed_point_values_convert_with_one_rounding() {
    let fp = single(Mode::Nearest);
    // 3 with one fraction bit is 1.5; 0x1_0000_0001 × 2^-32 rounds to 1.
    assert_eq!(fp.from_fixed(false, 3, 1).bits, 0x3FC0_0000);
    let rounded = fp.from_fixed(true, 0x1_0000_0001, 32);
    assert_eq!(rounded.bits, 0xBF80_0000);
    assert_eq!(rounded.flags, Flags::INEXACT);
    assert_eq!(fp.from_fixed(true, 0, 16).bits, 0, "zero is positive");
    let quarter = 0x3E80_0000;
    assert_eq!(
        fp.to_fixed(quarter, 4, Mode::Nearest),
        Integral::Value {
            negative: false,
            magnitude: 4,
            inexact: false
        },
        "a quarter with four fraction bits"
    );
    assert_eq!(
        fp.to_fixed(quarter, 1, Mode::Zero),
        Integral::Value {
            negative: false,
            magnitude: 0,
            inexact: true
        }
    );
}

#[test]
fn a_value_s_class_and_the_nan_its_operation_answers() {
    let format = Binary::SINGLE;
    for (bits, class) in [
        (0x8000_0000, Class::Zero),
        (0x0000_0001, Class::Finite),
        (0x7F7F_FFFF, Class::Finite),
        (0xFF80_0000, Class::Infinity),
        (0x7FC0_0000, Class::Quiet),
        (0x7F80_0001, Class::Signalling),
    ] {
        assert_eq!(format.class(bits), class, "{bits:08X}");
    }
    let fp = single(Mode::Nearest);
    assert_eq!(fp.nan_among(&[0x3F80_0000, 0xFF80_0000]), None, "no NaN");
    let quiet = fp.nan_among(&[0x3F80_0000, 0x7FC0_0001, 0x7F80_0002]);
    assert_eq!(
        quiet.map(|outcome| (outcome.bits, outcome.flags)),
        Some((0x7FC0_0002, Flags::INVALID)),
        "the signalling one quieted, and invalid"
    );
    let fp = Ieee {
        policy: Policy {
            nans: Nans::Default,
            ..ARM
        },
        ..fp
    };
    assert_eq!(
        fp.nan_among(&[0xFFC1_2345]).map(|outcome| outcome.bits),
        Some(Binary::SINGLE.default_nan())
    );
}
