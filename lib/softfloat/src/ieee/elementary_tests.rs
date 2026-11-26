extern crate std;

use super::*;
use crate::binary::Binary;
use crate::ieee::{Nans, Policy, Tininess};
use crate::mode::Mode;

fn under(format: Binary, mode: Mode, nans: Nans) -> Ieee {
    Ieee {
        format,
        mode,
        policy: Policy {
            tininess: Tininess::BeforeRounding,
            nans,
            flush_to_zero: false,
        },
    }
}

fn double() -> Ieee {
    under(Binary::DOUBLE, Mode::Nearest, Nans::Propagate)
}

fn d(x: f64) -> u64 {
    x.to_bits()
}

const DEFAULT_NAN: u64 = 0x7FF8_0000_0000_0000;
const QUIET: u64 = 0x7FF8_0000_0000_1234;
const SIGNALLING: u64 = 0x7FF0_0000_0000_1234;
const INFINITY: u64 = 0x7FF0_0000_0000_0000;
const MINUS_INFINITY: u64 = 0xFFF0_0000_0000_0000;
const HALF_PI: u64 = 0x3FF9_21FB_5444_2D18;
const PI: u64 = 0x4009_21FB_5444_2D18;
const QUARTER_PI: u64 = 0x3FE9_21FB_5444_2D18;
const THREE_QUARTER_PI: u64 = 0x4002_D97C_7F33_21D2;
const SIGN: u64 = 1 << 63;

const NONE: Flags = Flags::NONE;
const INVALID: Flags = Flags::INVALID;
const INEXACT: Flags = Flags::INEXACT;
const BY_ZERO: Flags = Flags::DIVIDE_BY_ZERO;

/// sin, cos and tan an exact reduction and series computed to 2600 bits,
/// correctly rounded: the far operands reduce with nothing lost, the
/// hardest of them lying within 2^-61 of a multiple of π/2.
#[test]
fn far_operands_reduce_with_nothing_lost() {
    let ieee = double();
    for (x, sin, cos, tan) in [
        (
            1e22,
            0xBFEB_453A_B76B_F397,
            0x3FE0_BE2C_EF01_C8F4,
            0xBFFA_0F79_C1B6_B257,
        ),
        (
            2f64.powi(1023),
            0x3FE2_0524_8CBD_B760,
            0xBFEA_719F_26C2_32BF,
            0xBFE5_CE6B_4C0D_02A3,
        ),
        (
            f64::MAX,
            0x3F74_52FC_98B3_4E97,
            0xBFEF_FFE6_2ECF_AB75,
            0xBF74_530C_FE72_9484,
        ),
        (
            5.319_372_648_326_541e255,
            0x3FF0_0000_0000_0000,
            0xBC21_4AE7_2E6B_A22F,
            0xC3BD_9BA9_A797_5636,
        ),
    ] {
        for (name, outcome, want) in [
            ("sin", ieee.sin(d(x)), sin),
            ("cos", ieee.cos(d(x)), cos),
            ("tan", ieee.tan(d(x)), tan),
        ] {
            assert_eq!(
                outcome,
                Outcome {
                    bits: want,
                    flags: INEXACT
                },
                "{name} {x:e}"
            );
        }
    }
    let single = ieee_single();
    let x = u64::from((1.5f32 * 2f32.powi(127)).to_bits());
    assert_eq!(single.sin(x).bits, 0x3F58_BBF1);
    assert_eq!(single.cos(x).bits, 0x3F08_3E5B);
    assert_eq!(single.tan(x).bits, 0x3FCB_9ED2);
}

fn ieee_single() -> Ieee {
    under(Binary::SINGLE, Mode::Nearest, Nans::Propagate)
}

#[test]
fn a_single_rounds_once_to_single_precision() {
    let single = ieee_single();
    let one = u64::from(1f32.to_bits());
    assert_eq!(
        single.sin(one),
        Outcome {
            bits: 0x3F57_6AA4,
            flags: INEXACT
        }
    );
    assert_eq!(
        single.cos(one),
        Outcome {
            bits: 0x3F0A_5140,
            flags: INEXACT
        }
    );
    assert_eq!(
        single.tan(one),
        Outcome {
            bits: 0x3FC7_5923,
            flags: INEXACT
        }
    );
}

#[test]
fn the_circular_and_arc_functions_take_the_standard_s_special_cases() {
    let ieee = double();
    let pi_signed = |bits: u64, negative: bool| if negative { bits | SIGN } else { bits };
    for (name, got, bits, flags) in [
        ("sin +0", ieee.sin(0), 0, NONE),
        ("sin -0", ieee.sin(SIGN), SIGN, NONE),
        ("sin ∞", ieee.sin(INFINITY), DEFAULT_NAN, INVALID),
        ("sin sNaN", ieee.sin(SIGNALLING), QUIET, INVALID),
        ("sin qNaN", ieee.sin(QUIET), QUIET, NONE),
        ("cos -0", ieee.cos(SIGN), d(1.0), NONE),
        ("cos -∞", ieee.cos(MINUS_INFINITY), DEFAULT_NAN, INVALID),
        ("tan -0", ieee.tan(SIGN), SIGN, NONE),
        ("tan ∞", ieee.tan(INFINITY), DEFAULT_NAN, INVALID),
        ("asin -0", ieee.asin(SIGN), SIGN, NONE),
        ("asin 1", ieee.asin(d(1.0)), HALF_PI, INEXACT),
        (
            "asin -1",
            ieee.asin(d(-1.0)),
            pi_signed(HALF_PI, true),
            INEXACT,
        ),
        ("asin past 1", ieee.asin(d(1.0) + 1), DEFAULT_NAN, INVALID),
        ("asin ∞", ieee.asin(INFINITY), DEFAULT_NAN, INVALID),
        ("acos 1", ieee.acos(d(1.0)), 0, NONE),
        ("acos -1", ieee.acos(d(-1.0)), PI, INEXACT),
        ("acos -0", ieee.acos(SIGN), HALF_PI, INEXACT),
        ("acos 2", ieee.acos(d(2.0)), DEFAULT_NAN, INVALID),
        ("atan -0", ieee.atan(SIGN), SIGN, NONE),
        ("atan ∞", ieee.atan(INFINITY), HALF_PI, INEXACT),
        (
            "atan -∞",
            ieee.atan(MINUS_INFINITY),
            pi_signed(HALF_PI, true),
            INEXACT,
        ),
        ("atan2 +0 -0", ieee.atan2(0, SIGN), PI, INEXACT),
        (
            "atan2 -0 -0",
            ieee.atan2(SIGN, SIGN),
            pi_signed(PI, true),
            INEXACT,
        ),
        ("atan2 +0 +0", ieee.atan2(0, 0), 0, NONE),
        ("atan2 -0 +0", ieee.atan2(SIGN, 0), SIGN, NONE),
        (
            "atan2 -0 -1",
            ieee.atan2(SIGN, d(-1.0)),
            pi_signed(PI, true),
            INEXACT,
        ),
        ("atan2 1 +0", ieee.atan2(d(1.0), 0), HALF_PI, INEXACT),
        (
            "atan2 -1 -0",
            ieee.atan2(d(-1.0), SIGN),
            pi_signed(HALF_PI, true),
            INEXACT,
        ),
        (
            "atan2 ∞ -∞",
            ieee.atan2(INFINITY, MINUS_INFINITY),
            THREE_QUARTER_PI,
            INEXACT,
        ),
        (
            "atan2 -∞ ∞",
            ieee.atan2(MINUS_INFINITY, INFINITY),
            pi_signed(QUARTER_PI, true),
            INEXACT,
        ),
        ("atan2 ∞ 5", ieee.atan2(INFINITY, d(5.0)), HALF_PI, INEXACT),
        (
            "atan2 1 -∞",
            ieee.atan2(d(1.0), MINUS_INFINITY),
            PI,
            INEXACT,
        ),
        ("atan2 -1 ∞", ieee.atan2(d(-1.0), INFINITY), SIGN, NONE),
        ("atan2 1 1", ieee.atan2(d(1.0), d(1.0)), QUARTER_PI, INEXACT),
        ("atan2 qNaN 1", ieee.atan2(QUIET, d(1.0)), QUIET, NONE),
    ] {
        assert_eq!(got, Outcome { bits, flags }, "{name}");
    }
}

#[test]
fn the_logarithms_and_the_exponential_take_the_standard_s_special_cases() {
    let ieee = double();
    for (name, got, bits, flags) in [
        ("log +0", ieee.log(0), MINUS_INFINITY, BY_ZERO),
        ("log -0", ieee.log(SIGN), MINUS_INFINITY, BY_ZERO),
        ("log -1", ieee.log(d(-1.0)), DEFAULT_NAN, INVALID),
        ("log -∞", ieee.log(MINUS_INFINITY), DEFAULT_NAN, INVALID),
        ("log ∞", ieee.log(INFINITY), INFINITY, NONE),
        ("log 1", ieee.log(d(1.0)), 0, NONE),
        ("log e", ieee.log(d(core::f64::consts::E)), d(1.0), INEXACT),
        ("log10 1000", ieee.log10(d(1000.0)), d(3.0), NONE),
        ("log10 0.001", ieee.log10(d(0.001)), d(-3.0), INEXACT),
        ("log10 1", ieee.log10(d(1.0)), 0, NONE),
        ("log10 -0", ieee.log10(SIGN), MINUS_INFINITY, BY_ZERO),
        ("exp -0", ieee.exp(SIGN), d(1.0), NONE),
        ("exp -∞", ieee.exp(MINUS_INFINITY), 0, NONE),
        ("exp ∞", ieee.exp(INFINITY), INFINITY, NONE),
        ("exp 1", ieee.exp(d(1.0)), d(core::f64::consts::E), INEXACT),
        (
            "exp 1000",
            ieee.exp(d(1000.0)),
            INFINITY,
            Flags::OVERFLOW | INEXACT,
        ),
        (
            "exp -1000",
            ieee.exp(d(-1000.0)),
            0,
            Flags::UNDERFLOW | INEXACT,
        ),
        ("exp sNaN", ieee.exp(SIGNALLING), QUIET, INVALID),
    ] {
        assert_eq!(got, Outcome { bits, flags }, "{name}");
    }
    let toward_zero = under(Binary::DOUBLE, Mode::Zero, Nans::Propagate);
    assert_eq!(
        toward_zero.exp(d(1000.0)),
        Outcome {
            bits: d(f64::MAX),
            flags: Flags::OVERFLOW | INEXACT
        },
        "an overflow toward zero answers the largest finite value"
    );
}

#[test]
fn pow_takes_the_standard_s_special_cases() {
    let ieee = double();
    let one = d(1.0);
    for (name, got, bits, flags) in [
        ("qNaN^0", ieee.pow(QUIET, 0), one, NONE),
        ("∞^-0", ieee.pow(INFINITY, SIGN), one, NONE),
        ("1^qNaN", ieee.pow(one, QUIET), one, NONE),
        ("1^∞", ieee.pow(one, INFINITY), one, NONE),
        ("sNaN^0", ieee.pow(SIGNALLING, 0), QUIET, INVALID),
        ("1^sNaN", ieee.pow(one, SIGNALLING), QUIET, INVALID),
        ("2^qNaN", ieee.pow(d(2.0), QUIET), QUIET, NONE),
        ("-0^-3", ieee.pow(SIGN, d(-3.0)), MINUS_INFINITY, BY_ZERO),
        ("+0^-3", ieee.pow(0, d(-3.0)), INFINITY, BY_ZERO),
        ("-0^-2", ieee.pow(SIGN, d(-2.0)), INFINITY, BY_ZERO),
        ("-0^-0.5", ieee.pow(SIGN, d(-0.5)), INFINITY, BY_ZERO),
        ("-0^-∞", ieee.pow(SIGN, MINUS_INFINITY), INFINITY, NONE),
        ("-0^3", ieee.pow(SIGN, d(3.0)), SIGN, NONE),
        ("-0^2", ieee.pow(SIGN, d(2.0)), 0, NONE),
        ("-0^∞", ieee.pow(SIGN, INFINITY), 0, NONE),
        ("-1^∞", ieee.pow(d(-1.0), INFINITY), one, NONE),
        ("-1^-∞", ieee.pow(d(-1.0), MINUS_INFINITY), one, NONE),
        ("0.5^∞", ieee.pow(d(0.5), INFINITY), 0, NONE),
        ("0.5^-∞", ieee.pow(d(0.5), MINUS_INFINITY), INFINITY, NONE),
        ("2^∞", ieee.pow(d(2.0), INFINITY), INFINITY, NONE),
        ("-2^-∞", ieee.pow(d(-2.0), MINUS_INFINITY), 0, NONE),
        ("-∞^-3", ieee.pow(MINUS_INFINITY, d(-3.0)), SIGN, NONE),
        ("-∞^-2", ieee.pow(MINUS_INFINITY, d(-2.0)), 0, NONE),
        (
            "-∞^3",
            ieee.pow(MINUS_INFINITY, d(3.0)),
            MINUS_INFINITY,
            NONE,
        ),
        ("-∞^2.5", ieee.pow(MINUS_INFINITY, d(2.5)), INFINITY, NONE),
        ("∞^-1", ieee.pow(INFINITY, d(-1.0)), 0, NONE),
        ("∞^0.5", ieee.pow(INFINITY, d(0.5)), INFINITY, NONE),
        ("-2^0.5", ieee.pow(d(-2.0), d(0.5)), DEFAULT_NAN, INVALID),
        ("-1^3", ieee.pow(d(-1.0), d(3.0)), d(-1.0), NONE),
        ("-1^10^300", ieee.pow(d(-1.0), d(1e300)), one, NONE),
    ] {
        assert_eq!(got, Outcome { bits, flags }, "{name}");
    }
}

#[test]
fn an_exact_power_raises_nothing_and_an_inexact_one_rounds_once() {
    let ieee = double();
    for (x, y, want) in [
        (-2.0, 3.0, -8.0),
        (4.0, 0.5, 2.0),
        (2.0, -3.0, 0.125),
        (9.0, 1.5, 27.0),
        (0.25, -1.5, 8.0),
        (2.0, -1074.0, f64::from_bits(1)),
        (65536.0, 0.0625, 2.0),
    ] {
        assert_eq!(
            ieee.pow(d(x), d(y)),
            Outcome {
                bits: d(want),
                flags: NONE
            },
            "{x}^{y}"
        );
    }
    assert_eq!(
        ieee.pow(d(2.0), d(0.5)),
        Outcome {
            bits: d(core::f64::consts::SQRT_2),
            flags: INEXACT
        }
    );
    assert_eq!(
        ieee.pow(d(3.0), d(0.5)).flags,
        INEXACT,
        "three has no exact root"
    );
    assert_eq!(
        ieee.pow(d(10.0), d(400.0)),
        Outcome {
            bits: INFINITY,
            flags: Flags::OVERFLOW | INEXACT
        }
    );
    assert_eq!(
        ieee.pow(d(10.0), d(-400.0)),
        Outcome {
            bits: 0,
            flags: Flags::UNDERFLOW | INEXACT
        }
    );
    assert_eq!(
        ieee.pow(d(2.0), d(-1075.0)),
        Outcome {
            bits: 0,
            flags: Flags::UNDERFLOW | INEXACT
        },
        "half the least subnormal rounds to even, nought"
    );
}

#[test]
fn each_rounding_mode_brackets_the_value_from_its_side() {
    for function in [Ieee::sin, Ieee::cos, Ieee::exp, Ieee::log, Ieee::atan] {
        let at = |mode| function(under(Binary::DOUBLE, mode, Nans::Propagate), d(1.5)).bits;
        let (down, up, nearest) = (at(Mode::Down), at(Mode::Up), at(Mode::Nearest));
        assert_eq!(up - down, 1, "a place apart");
        assert!(nearest == up || nearest == down);
        assert_eq!(
            at(Mode::Zero),
            down,
            "positive results round down toward zero"
        );
    }
    let pow = |mode| Ieee::pow(under(Binary::DOUBLE, mode, Nans::Propagate), d(1.5), d(2.5)).bits;
    assert_eq!(pow(Mode::Up) - pow(Mode::Down), 1);
}

#[test]
fn the_default_nan_replaces_a_payload_where_the_policy_asks() {
    let ieee = under(Binary::DOUBLE, Mode::Nearest, Nans::Default);
    assert_eq!(
        ieee.sin(QUIET),
        Outcome {
            bits: DEFAULT_NAN,
            flags: NONE
        }
    );
    assert_eq!(
        ieee.pow(QUIET, d(2.0)),
        Outcome {
            bits: DEFAULT_NAN,
            flags: NONE
        }
    );
    assert_eq!(
        ieee.atan2(d(1.0), SIGNALLING),
        Outcome {
            bits: DEFAULT_NAN,
            flags: INVALID
        }
    );
}
