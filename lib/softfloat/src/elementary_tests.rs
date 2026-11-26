extern crate std;

use tairix_fuzzseed::Prng;

use super::*;
use crate::binary::Binary;
use crate::ieee::{Ieee, Nans, Policy, Tininess};
use crate::mode::{Mode, Rounding};

/// A value's parts from its unbiased exponent and significand.
fn at(exp: i32, sig: u64) -> Parts {
    Parts {
        negative: false,
        exp,
        sig,
    }
}

fn negative(p: Parts) -> Parts {
    Parts {
        negative: true,
        ..p
    }
}

/// A double's parts.
fn parts(x: f64) -> Parts {
    let bits = x.to_bits();
    let field = i32::try_from((bits >> 52) & 0x7FF).expect("eleven bits");
    let fraction = bits & ((1 << 52) - 1);
    let (exp, sig) = if field == 0 {
        (-1022, fraction << 11)
    } else {
        (field - 1023, (fraction | 1 << 52) << 11)
    };
    Parts {
        negative: x.is_sign_negative(),
        exp,
        sig,
    }
    .normalized()
}

/// `x` rounded to 64 bits, to nearest: sign, unbiased exponent, significand.
fn nearest(x: Exact) -> (bool, i32, u64) {
    let x = x.normalized(true);
    let rounded = x.rounded(Rounding::extended(Mode::Nearest));
    (x.negative, x.exp + i32::from(rounded.carried), rounded.sig)
}

/// The double `x` rounds to.
fn double(x: Exact) -> f64 {
    let ieee = Ieee {
        format: Binary::DOUBLE,
        mode: Mode::Nearest,
        policy: Policy {
            tininess: Tininess::BeforeRounding,
            nans: Nans::Propagate,
            flush_to_zero: false,
        },
    };
    f64::from_bits(ieee.round(x).bits)
}

/// Whether two doubles of one sign lie within `ulps` places of each other.
fn near(a: f64, b: f64, ulps: u64) -> bool {
    a.is_sign_negative() == b.is_sign_negative() && a.to_bits().abs_diff(b.to_bits()) <= ulps
}

/// π/2's 64 bits rounded up: the operand nearest a multiple of π/2 at each
/// scale it is given.
const HALF_PI_UP: u64 = 0xC90F_DAA2_2168_C235;

/// sin, cos and tan values a reference computed to 160 digits, correctly
/// rounded to 64 bits.
#[test]
fn each_circular_function_is_correctly_rounded_at_known_points() {
    for (name, got, want) in [
        (
            "tan 0.5",
            tan(parts(0.5)),
            (false, -1, 0x8BDA_7ADF_9A3A_5219),
        ),
        (
            "tan 0.75",
            tan(parts(0.75)),
            (false, -1, 0xEE7D_1B08_8777_5F06),
        ),
        (
            "tan 2^-20",
            tan(parts(2f64.powi(-20))),
            (false, -20, 0x8000_0000_002A_AAAB),
        ),
        (
            "sin 1",
            sin(at(0, 1 << 63)),
            (false, -1, 0xD76A_A478_4867_7021),
        ),
        (
            "sin 2^-20",
            sin(at(-20, 1 << 63)),
            (false, -21, 0xFFFF_FFFF_FFD5_5555),
        ),
        (
            "sin 3",
            sin(at(1, 3 << 62)),
            (false, -3, 0x9081_C36D_B6AA_DA79),
        ),
        (
            "sin 100",
            sin(at(6, 100 << 57)),
            (true, -1, 0x81A1_2DBC_626D_C038),
        ),
        (
            "sin -100",
            sin(negative(at(6, 100 << 57))),
            (false, -1, 0x81A1_2DBC_626D_C038),
        ),
        (
            "sin 1.5×2^40",
            sin(at(40, 3 << 62)),
            (false, -1, 0xCF5C_E558_39A5_AACB),
        ),
        (
            "sin 1.7×2^62",
            sin(at(62, 0xD999_9999_9999_999A)),
            (false, -1, 0xF6D3_AC75_F9D6_48EF),
        ),
        (
            "sin π/2 × 2^62",
            sin(at(62, HALF_PI_UP)),
            (false, -4, 0xEC5F_4DEE_A233_7239),
        ),
        (
            "sin π/2 × 2^31",
            sin(at(31, HALF_PI_UP)),
            (false, -35, 0xECE6_75D1_FC8F_8CBB),
        ),
        (
            "cos 0.5",
            cos(at(-1, 1 << 63)),
            (false, -1, 0xE0A9_4032_DBEA_7CEE),
        ),
        (
            "cos 1",
            cos(at(0, 1 << 63)),
            (false, -1, 0x8A51_407D_A834_5C92),
        ),
        (
            "cos 10^10",
            cos(at(33, 10_000_000_000 << 30)),
            (false, -1, 0xDF84_C480_E498_CC19),
        ),
        (
            "cos π/2",
            cos(at(0, HALF_PI_UP)),
            (true, -66, 0xECE6_75D1_FC8F_8CBB),
        ),
        (
            "cos π/2 × 2^20",
            cos(at(20, HALF_PI_UP)),
            (false, 0, 1 << 63),
        ),
        (
            "tan π/2 × 2^61",
            tan(at(61, HALF_PI_UP)),
            (false, -5, 0xED2A_2C89_9629_E691),
        ),
    ] {
        assert_eq!(nearest(got), want, "{name}");
    }
}

/// Angles a reference computed to 160 digits, correctly rounded to 64 bits.
#[test]
fn each_arc_is_correctly_rounded_at_known_points() {
    for (name, got, want) in [
        (
            "atan 0.5",
            atan2(parts(0.5), parts(1.0)),
            (false, -2, 0xED63_382B_0DDA_7B45),
        ),
        (
            "atan 1",
            atan2(parts(1.0), parts(1.0)),
            (false, -1, 0xC90F_DAA2_2168_C235),
        ),
        (
            "atan 2^-30",
            atan2(parts(2f64.powi(-30)), parts(1.0)),
            (false, -31, 0xFFFF_FFFF_FFFF_FFFB),
        ),
        ("π", quarter_pis(4), (false, 1, 0xC90F_DAA2_2168_C235)),
        (
            "sin 0.5",
            sin(at(-1, 1 << 63)),
            (false, -2, 0xF577_43A2_582F_7F44),
        ),
        (
            "asin 0.5",
            asin(at(-1, 1 << 63)),
            (false, -1, 0x860A_91C1_6B9B_2C23),
        ),
        (
            "asin 0.999",
            asin(at(-1, 0xFFBE_76C8_B439_5810)),
            (false, 0, 0xC356_4D68_C7C0_1B87),
        ),
        (
            "asin 2^-30",
            asin(at(-30, 1 << 63)),
            (false, -30, 0x8000_0000_0000_0001),
        ),
        (
            "acos 0.5",
            acos(at(-1, 1 << 63)),
            (false, 0, 0x860A_91C1_6B9B_2C23),
        ),
        (
            "acos -0.75",
            acos(negative(at(-1, 3 << 62))),
            (false, 1, 0x9ACE_937C_9DB6_192B),
        ),
        (
            "acos 2^-30",
            acos(at(-30, 1 << 63)),
            (false, 0, 0xC90F_DAA0_2168_C235),
        ),
    ] {
        assert_eq!(nearest(got), want, "{name}");
    }
}

/// Exponentials and powers a reference computed to 160 digits, correctly
/// rounded to 64 bits.
#[test]
fn each_exponential_and_power_is_correctly_rounded_at_known_points() {
    for (name, got, want) in [
        (
            "2^0.25 - 1",
            exp2m1(parts(0.25)),
            (false, -3, 0xC1BF_828C_6DC5_4B7A),
        ),
        (
            "2^0.5 - 1",
            exp2m1(parts(0.5)),
            (false, -2, 0xD413_CCCF_E779_9211),
        ),
        (
            "2^-40 - 1",
            exp2m1(parts(2f64.powi(-40))),
            (false, -41, 0xB172_17F7_D20C_F928),
        ),
        ("e", exp(at(0, 1 << 63)), (false, 1, 0xADF8_5458_A2BB_4A9B)),
        (
            "e^-1",
            exp(negative(at(0, 1 << 63))),
            (false, -2, 0xBC5A_B1B1_6779_BE35),
        ),
        (
            "e^0.5",
            exp(at(-1, 1 << 63)),
            (false, 0, 0xD309_4C70_F034_DE4C),
        ),
        (
            "e^100",
            exp(at(6, 100 << 57)),
            (false, 144, 0x9A4A_54D8_B8DF_A566),
        ),
        (
            "e^-1000",
            exp(negative(at(9, 1000 << 54))),
            (false, -1443, 0x9E21_0CF2_0C4A_A07A),
        ),
        (
            "e^2^-40",
            exp(at(-40, 1 << 63)),
            (false, 0, 0x8000_0000_0080_0000),
        ),
        (
            "2^0.5",
            pow(at(1, 1 << 63), at(-1, 1 << 63)),
            (false, 0, 0xB504_F333_F9DE_6484),
        ),
        (
            "10^2.5",
            pow(at(3, 10 << 60), at(1, 5 << 61)),
            (false, 8, 0x9E1D_276F_D4BA_C411),
        ),
        (
            "1.5^100",
            pow(at(0, 3 << 62), at(6, 100 << 57)),
            (false, 58, 0xB48C_A794_CE6E_D0AD),
        ),
    ] {
        assert_eq!(nearest(got), want, "{name}");
    }
}

/// Logarithms a reference computed to 160 digits, correctly rounded to 64
/// bits.
#[test]
fn each_logarithm_is_correctly_rounded_at_known_points() {
    for (name, got, want) in [
        (
            "log2 3",
            y_log2_x(parts(1.0), parts(3.0)),
            (false, 0, 0xCAE0_0D1C_FDEB_43D0),
        ),
        (
            "log2 0.75",
            y_log2_x(parts(1.0), parts(0.75)),
            (true, -2, 0xD47F_CB8C_0852_F0C1),
        ),
        (
            "log2 1+2^-20",
            y_log2_1p_x(parts(1.0), parts(2f64.powi(-20))),
            (false, -20, 0xB8AA_3564_0A7C_33EC),
        ),
        (
            "ln 2",
            ln(at(1, 1 << 63)),
            (false, -1, 0xB172_17F7_D1CF_79AC),
        ),
        (
            "ln 3",
            ln(at(1, 3 << 62)),
            (false, 0, 0x8C9F_53D5_6818_54BB),
        ),
        (
            "ln 0.75",
            ln(at(-1, 3 << 62)),
            (true, -2, 0x934B_1089_A6DC_93C2),
        ),
        (
            "ln 1+2^-40",
            ln(at(0, (1 << 63) + (1 << 23))),
            (false, -41, 0xFFFF_FFFF_FF80_0000),
        ),
        (
            "ln 10^300",
            ln(at(996, 0xBE48_8C4A_9C22_9E0A)),
            (false, 9, 0xACB1_594F_6C0C_890E),
        ),
        (
            "log10 2",
            log10(at(1, 1 << 63)),
            (false, -2, 0x9A20_9A84_FBCF_F799),
        ),
        (
            "log10 7",
            log10(at(2, 7 << 61)),
            (false, -1, 0xD858_585B_C661_F94B),
        ),
    ] {
        assert_eq!(nearest(got), want, "{name}");
    }
}

#[test]
fn a_power_of_ten_s_logarithm_and_an_integer_power_that_fits_are_exact() {
    let mut power: u128 = 1;
    for n in 0..28u32 {
        // 10^n's 5^n fits 64 bits throughout; its 2^n is the exponent's.
        let bits = 128 - power.leading_zeros();
        let sig = if bits > 64 {
            power >> (bits - 64)
        } else {
            power << (64 - bits)
        };
        let ten = at(
            i32::try_from(bits).unwrap() - 1,
            u64::try_from(sig).unwrap(),
        );
        let got = log10(ten);
        assert!(!got.sticky, "log10 10^{n}");
        assert_eq!(
            nearest(got),
            nearest(Exact::of(at(63, u64::from(n)).normalized())),
            "10^{n}"
        );
        power *= 10;
    }
    for (x, y, want) in [
        (at(1, 3 << 62), at(2, 5 << 61), at(7, 243 << 56)),
        (at(1, 1 << 63), negative(at(1, 3 << 62)), at(-3, 1 << 63)),
        (at(0, 3 << 62), at(1, 1 << 63), at(1, 9 << 60)),
        (at(-1, 1 << 63), at(3, 10 << 60), at(-10, 1 << 63)),
    ] {
        let got = pow(x, y);
        assert!(!got.sticky, "{x:?}^{y:?}");
        assert_eq!(nearest(got), nearest(Exact::of(want)), "{x:?}^{y:?}");
    }
    // 3^41 needs 65 bits: exact, and rounded to 64 a tie, to even.
    let got = pow(at(1, 3 << 62), at(5, 41 << 58));
    assert!(!got.sticky, "3^41 exact");
    assert_eq!(
        nearest(got),
        nearest(Exact::of(at(64, 18_236_498_188_585_393_202)))
    );
}

#[test]
fn a_root_s_power_that_fits_is_exact_and_a_root_that_is_not_is_rounded() {
    for (x, y, want) in [
        (parts(4.0), parts(0.5), parts(2.0)),
        (parts(9.0), parts(1.5), parts(27.0)),
        (parts(0.25), parts(-1.5), parts(8.0)),
        (parts(65536.0), parts(0.0625), parts(2.0)),
        (parts(1.0 / 64.0), parts(-0.5), parts(8.0)),
    ] {
        let got = pow(x, y);
        assert!(!got.sticky, "{x:?}^{y:?}");
        assert_eq!(nearest(got), nearest(Exact::of(want)), "{x:?}^{y:?}");
    }
    assert!(pow(parts(2.0), parts(0.5)).sticky, "√2 is irrational");
    assert!(
        pow(parts(8.0), parts(0.5)).sticky,
        "an odd power of two has no square root"
    );
    assert!(
        pow(parts(12.0), parts(0.5)).sticky,
        "nor has twelve's odd part"
    );
}

/// sin, cos and tan at operands past 2^63, from an exact reduction and series
/// to 2600 bits, correctly rounded to 64: every word of the reduction's
/// window comes into play, and the last operand lies within 2^-61 of a
/// multiple of π/2.
#[test]
fn far_operands_reduce_by_their_nearest_multiple_with_nothing_lost() {
    for (x, sin_want, cos_want, tan_want) in [
        (
            1e22,
            (true, -1, 0xDA29_D5BB_5F9C_B87D),
            (false, -1, 0x85F1_6778_0E47_9C9A),
            (true, 0, 0xD07B_CE0D_B592_BBA5),
        ),
        (
            2f64.powi(1023),
            (false, -1, 0x9029_2465_EDBA_FF2D),
            (true, -1, 0xD38C_F936_1195_F50B),
            (true, -1, 0xAE73_5A60_6815_1A9E),
        ),
        (
            f64::MAX,
            (false, -8, 0xA297_E4C5_9A74_B5B1),
            (true, -1, 0xFFFF_3176_7D5B_A9E0),
            (true, -8, 0xA298_67F3_94A4_1DC7),
        ),
        (
            5.319_372_648_326_541e255,
            (false, 0, 0x8000_0000_0000_0000),
            (true, -61, 0x8A57_3973_5D11_77A3),
            (true, 60, 0xECDD_4D3C_BAB1_AD1D),
        ),
    ] {
        assert_eq!(nearest(sin(parts(x))), sin_want, "sin {x:e}");
        assert_eq!(nearest(cos(parts(x))), cos_want, "cos {x:e}");
        assert_eq!(nearest(tan(parts(x))), tan_want, "tan {x:e}");
    }
}

#[test]
fn a_value_beside_its_leading_term_rounds_to_the_side_it_lies() {
    let tiny = at(-100, 1 << 63);
    let toward_zero = |x: Exact| {
        let x = x.normalized(true);
        let rounded = x.rounded(Rounding::extended(Mode::Zero));
        (x.negative, x.exp + i32::from(rounded.carried), rounded.sig)
    };
    let up = |x: Exact| {
        let x = x.normalized(true);
        let mode = if x.negative { Mode::Down } else { Mode::Up };
        let rounded = x.rounded(Rounding::extended(mode));
        (x.negative, x.exp + i32::from(rounded.carried), rounded.sig)
    };
    for (name, value, under) in [
        ("sin", sin(tiny), true),
        ("sin -", sin(negative(tiny)), true),
        ("atan", atan2(tiny, at(0, 1 << 63)), true),
        ("tan", tan(tiny), false),
        ("asin", asin(negative(tiny)), false),
    ] {
        let leading = (value.negative, -100, 1 << 63);
        assert_eq!(nearest(value), leading, "{name} to nearest");
        if under {
            assert_eq!(
                toward_zero(value),
                (value.negative, -101, u64::MAX),
                "{name} under its argument"
            );
        } else {
            assert_eq!(toward_zero(value), leading, "{name}");
            assert_eq!(
                up(value),
                (value.negative, -100, (1 << 63) | 1),
                "{name} over its argument"
            );
        }
    }
    assert_eq!(
        toward_zero(cos(tiny)),
        (false, -1, u64::MAX),
        "cos under one"
    );
    let small = at(-200, 1 << 63);
    assert_eq!(
        toward_zero(super::exp(negative(small))),
        (false, -1, u64::MAX),
        "e^-x under one"
    );
    assert_eq!(
        up(super::exp(small)),
        (false, 0, (1 << 63) | 1),
        "e^x over one"
    );
}

#[test]
fn an_exponential_past_every_exponent_keeps_its_sign_and_side() {
    for exp in [62, 16000] {
        let huge = at(exp, 1 << 63);
        for (name, value, below) in [
            ("e^x", super::exp(huge), false),
            ("e^-x", super::exp(negative(huge)), true),
            ("2^x - 1", exp2m1(huge), false),
        ] {
            assert!(!value.negative, "{name} at 2^{exp}");
            assert_eq!(value.exp < 0, below, "{name} at 2^{exp}: {value:?}");
            assert!(value.exp.abs() >= 1 << 19, "{name} at 2^{exp}: {value:?}");
        }
        let minus_one = exp2m1(negative(huge));
        let toward_zero = minus_one
            .normalized(true)
            .rounded(Rounding::extended(Mode::Zero));
        assert_eq!(
            (minus_one.negative, nearest(minus_one)),
            (true, (true, 0, 1 << 63)),
            "2^-x - 1 at 2^{exp} rounds to -1"
        );
        assert_eq!(toward_zero.sig, u64::MAX, "and lies above it");
    }
}

#[test]
fn a_square_root_squares_back_within_the_last_bits() {
    let mut rng = Prng::new(0x5051_0001);
    for _ in 0..2_000 {
        let x = Wide {
            negative: false,
            exp: i32::try_from(rng.below(400)).unwrap() - 200,
            sig: (u128::from(rng.next_u64()) << 64 | u128::from(rng.next_u64())) | 1 << 127,
        };
        let root = x.sqrt();
        let back = root.mul(root);
        assert_eq!(back.exp, x.exp, "{x:?}");
        assert!(back.sig.abs_diff(x.sig) < 1 << 8, "{x:?}: {back:?}");
    }
    assert_eq!(Wide::small(4).sqrt(), Wide::small(2));
}

#[test]
fn the_nearest_integer_rounds_halves_away_from_zero_across_every_place() {
    let wide = |x: f64| Wide::of(parts(x));
    for (x, n) in [
        (0.25, 0),
        (0.5, 1),
        (0.75, 1),
        (-0.5, -1),
        (1.5, 2),
        (2.5, 3),
        (-2.49, -2),
        (1e15, 1_000_000_000_000_000),
    ] {
        assert_eq!(wide(x).nearest_integer(), n, "{x}");
    }
    assert_eq!(wide(1e30).nearest_integer(), 1 << 62, "saturated");
    let k = (1i64 << 61) + 3;
    assert_eq!(Wide::from_i64(k).nearest_integer(), k);
}

/// The regression: these were checked against the host's doubles, and a
/// libm a few ulps out, as macOS's tangent is, failed them.
#[test]
fn across_their_ranges_they_agree_with_the_true_values() {
    let mut rng = Prng::new(0x8087_7A11);
    let mut fraction = || {
        // A double in [0, 1) of 53 random bits.
        f64::from_bits(0x3FF0_0000_0000_0000 | (rng.next_u64() >> 12)) - 1.0
    };
    for _ in 0..2_000 {
        let x = fraction() * core::f64::consts::FRAC_PI_4;
        if x != 0.0 {
            assert!(
                near(double(tan(parts(x))), reference::tan(x), 1),
                "tan {x:e}"
            );
            assert!(
                near(double(sin(parts(x))), reference::sin(x), 1),
                "sin {x:e}"
            );
            assert!(
                near(double(cos(parts(x))), reference::cos(x), 1),
                "cos {x:e}"
            );
        }
        let (y, x) = (fraction(), fraction());
        if y < x && y != 0.0 {
            assert!(
                near(double(atan2(parts(y), parts(x))), reference::atan2(y, x), 1),
                "atan {y:e}/{x:e}"
            );
        }
        let x = fraction() * 0.5;
        if x != 0.0 {
            assert!(
                near(double(exp2m1(parts(x))), reference::exp2m1(x), 2),
                "2^{x:e} - 1"
            );
            assert!(near(double(exp(parts(x))), reference::exp(x), 1), "e^{x:e}");
        }
        let x = fraction() * 4.0;
        if x > 0.0 && x.to_bits() != 1.0f64.to_bits() {
            assert!(
                near(
                    double(y_log2_x(parts(1.0), parts(x))),
                    reference::log2(x),
                    2
                ),
                "log2 {x:e}"
            );
            assert!(near(double(ln(parts(x))), reference::ln(x), 1), "ln {x:e}");
        }
        let x = (fraction() - 0.5) * 0.58;
        if x != 0.0 {
            assert!(
                near(
                    double(y_log2_1p_x(parts(1.0), parts(x))),
                    reference::log2_1p(x),
                    2
                ),
                "log2(1+{x:e})"
            );
        }
    }
    for x in [1.0f64, 2.0, 3.0, -0.5, -3.0, 100.0, 1e10] {
        assert!(near(double(tan(parts(x))), x.tan(), 4), "tan {x}");
    }
    for x in [
        2.0f64.powi(61) * 1.7,
        2.0f64.powi(62) * 1.99,
        -(2.0f64.powi(63) - 2.0f64.powi(11)),
    ] {
        assert!(double(tan(parts(x))).is_finite(), "tan {x:e}");
    }
    for (y, x) in [
        (1.0f64, -1.0f64),
        (-1.0, -1.0),
        (-1.0, 1.0),
        (3.0, 1.0),
        (-2.0, 0.5),
    ] {
        assert!(
            near(double(atan2(parts(y), parts(x))), y.atan2(x), 1),
            "atan2 {y} {x}"
        );
    }
    for x in [1.5f64, -1.0, 2.5, -10.0] {
        assert!(
            near(double(exp2m1(parts(x))), x.exp2() - 1.0, 2),
            "2^{x} - 1"
        );
    }
    for x in [1.0f64, 3.0, 0.5] {
        let expected = x.ln_1p() / core::f64::consts::LN_2;
        assert!(
            near(double(y_log2_1p_x(parts(1.0), parts(x))), expected, 2),
            "log2(1+{x})"
        );
    }
    assert!(near(
        double(y_log2_x(parts(-3.0), parts(10.0))),
        -3.0 * 10.0f64.log2(),
        2
    ));
}

/// The true values, as double-doubles summed by their series: the same on
/// every host, where a platform libm's doubles are not.
mod reference {
    /// The unevaluated sum of two doubles, about 106 bits.
    #[derive(Clone, Copy)]
    struct Dd(f64, f64);

    impl Dd {
        const fn of(x: f64) -> Self {
            Self(x, 0.0)
        }

        fn quick(a: f64, b: f64) -> Self {
            let sum = a + b;
            Self(sum, b - (sum - a))
        }

        fn add(self, other: Self) -> Self {
            let sum = self.0 + other.0;
            let part = sum - self.0;
            let error = (self.0 - (sum - part)) + (other.0 - part);
            Self::quick(sum, error + self.1 + other.1)
        }

        fn sub(self, other: Self) -> Self {
            self.add(Self(-other.0, -other.1))
        }

        fn mul(self, other: Self) -> Self {
            let product = self.0 * other.0;
            let error = self.0.mul_add(other.0, -product) + (self.0 * other.1 + self.1 * other.0);
            Self::quick(product, error)
        }

        fn div(self, other: Self) -> Self {
            let first = self.0 / other.0;
            let rest = self.sub(other.mul(Self::of(first)));
            let second = rest.0 / other.0;
            let rest = rest.sub(other.mul(Self::of(second)));
            Self::quick(first, second).add(Self::of(rest.0 / other.0))
        }

        fn double(self) -> f64 {
            self.0 + self.1
        }
    }

    /// The sum of a series from `first`, each term made from the last and its
    /// index, until a term no longer reaches the sum's last bit.
    fn series(first: Dd, mut next: impl FnMut(Dd, u32) -> Dd) -> Dd {
        let (mut sum, mut term) = (first, first);
        for n in 1..400 {
            term = next(term, n);
            sum = sum.add(term);
            if term.0.abs() <= sum.0.abs() * 1e-33 {
                break;
            }
        }
        sum
    }

    fn sin_dd(x: Dd) -> Dd {
        let square = x.mul(x);
        series(x, |term, n| {
            term.mul(square)
                .div(Dd::of(-f64::from((2 * n) * (2 * n + 1))))
        })
    }

    fn cos_dd(x: Dd) -> Dd {
        let square = x.mul(x);
        series(Dd::of(1.0), |term, n| {
            term.mul(square)
                .div(Dd::of(-f64::from((2 * n - 1) * (2 * n))))
        })
    }

    /// The sum of x^(2k+1)/(2k+1), alternating in sign if `alternating`:
    /// atanh or atan.
    fn odd_powers(x: Dd, alternating: bool) -> Dd {
        let square = x.mul(x);
        let square = if alternating {
            Dd(-square.0, -square.1)
        } else {
            square
        };
        let mut power = x;
        series(x, |_, n| {
            power = power.mul(square);
            power.div(Dd::of(f64::from(2 * n + 1)))
        })
    }

    fn ln2() -> Dd {
        let half = odd_powers(Dd::of(1.0).div(Dd::of(3.0)), false);
        half.add(half)
    }

    /// e^x - 1.
    fn expm1(x: Dd) -> Dd {
        series(x, |term, n| term.mul(x).div(Dd::of(f64::from(n + 1))))
    }

    /// ln x for a positive normal `x`, as e·ln 2 + ln m with m within √2 of 1.
    fn ln_dd(x: f64) -> Dd {
        let bits = x.to_bits();
        let mut exponent = i32::try_from((bits >> 52) & 0x7FF).expect("eleven bits") - 1023;
        let mut m = f64::from_bits((bits & ((1 << 52) - 1)) | (1023 << 52));
        if m > core::f64::consts::SQRT_2 {
            m /= 2.0;
            exponent += 1;
        }
        let half = odd_powers(Dd::of(m - 1.0).div(Dd::of(m + 1.0)), false);
        ln2().mul(Dd::of(f64::from(exponent))).add(half.add(half))
    }

    fn quarter_pi() -> Dd {
        let fifth = odd_powers(Dd::of(1.0).div(Dd::of(5.0)), true);
        let four = fifth.add(fifth).add(fifth.add(fifth));
        four.sub(odd_powers(Dd::of(1.0).div(Dd::of(239.0)), true))
    }

    /// atan t for t in [0, 1]: near 1 as π/4 + atan((t - 1)/(t + 1)).
    fn atan(t: Dd) -> Dd {
        if t.double() <= 0.4 {
            odd_powers(t, true)
        } else {
            let one = Dd::of(1.0);
            quarter_pi().add(odd_powers(t.sub(one).div(t.add(one)), true))
        }
    }

    pub fn sin(x: f64) -> f64 {
        sin_dd(Dd::of(x)).double()
    }

    pub fn cos(x: f64) -> f64 {
        cos_dd(Dd::of(x)).double()
    }

    pub fn tan(x: f64) -> f64 {
        sin_dd(Dd::of(x)).div(cos_dd(Dd::of(x))).double()
    }

    /// atan2(y, x) for 0 < y < x.
    pub fn atan2(y: f64, x: f64) -> f64 {
        atan(Dd::of(y).div(Dd::of(x))).double()
    }

    pub fn exp(x: f64) -> f64 {
        expm1(Dd::of(x)).add(Dd::of(1.0)).double()
    }

    pub fn exp2m1(x: f64) -> f64 {
        expm1(Dd::of(x).mul(ln2())).double()
    }

    pub fn ln(x: f64) -> f64 {
        ln_dd(x).double()
    }

    pub fn log2(x: f64) -> f64 {
        ln_dd(x).div(ln2()).double()
    }

    /// log2(1 + x), as 2·atanh(x / (2 + x)) / ln 2.
    pub fn log2_1p(x: f64) -> f64 {
        let half = odd_powers(Dd::of(x).div(Dd::of(2.0).add(Dd::of(x))), false);
        half.add(half).div(ln2()).double()
    }
}
