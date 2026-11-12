extern crate std;

use tairix_fuzzseed::Prng;

use super::*;
use crate::fpu::arith::{self, flag, Mode, Rounding};
use crate::fpu::real::{Binary, Real, BIAS};

fn parts(x: f64) -> Parts {
    Parts::of(Real::from_double(x.to_bits()).0)
}

fn nearest(x: Exact) -> Real {
    arith::round(x, Rounding::extended(Mode::Nearest), flag::ALL)
        .value
        .unwrap()
}

/// The value with this sign, unbiased exponent and significand.
fn real(negative: bool, exp: i32, sig: u64) -> Real {
    Real::new(negative, u16::try_from(exp + BIAS).unwrap(), sig)
}

/// The double a result rounds to.
fn double(x: Exact) -> f64 {
    let p = Parts::of(nearest(x)).normalized();
    let (bits, _) = arith::round_binary(p, Binary::DOUBLE, Mode::Nearest, flag::ALL);
    f64::from_bits(bits.unwrap())
}

/// Whether two doubles of one sign lie within `ulps` places of each other.
fn near(a: f64, b: f64, ulps: u64) -> bool {
    a.is_sign_negative() == b.is_sign_negative() && a.to_bits().abs_diff(b.to_bits()) <= ulps
}

/// Values the reference computed to 80 digits, correctly rounded: each
/// function's 64-bit answer at points inside the 8087's ranges.
#[test]
fn each_function_is_correctly_rounded_at_known_points() {
    let two = |n: i32| 2.0f64.powi(n);
    assert_eq!(
        nearest(tan(parts(0.5))),
        real(false, -1, 0x8BDA_7ADF_9A3A_5219)
    );
    assert_eq!(
        nearest(tan(parts(0.75))),
        real(false, -1, 0xEE7D_1B08_8777_5F06)
    );
    assert_eq!(
        nearest(tan(parts(two(-20)))),
        real(false, -20, 0x8000_0000_002A_AAAB)
    );
    assert_eq!(
        nearest(atan2(parts(0.5), parts(1.0))),
        real(false, -2, 0xED63_382B_0DDA_7B45)
    );
    assert_eq!(
        nearest(atan2(parts(0.375), parts(0.75))),
        real(false, -2, 0xED63_382B_0DDA_7B45)
    );
    assert_eq!(
        nearest(atan2(parts(two(-30)), parts(1.0))),
        real(false, -31, 0xFFFF_FFFF_FFFF_FFFB)
    );
    assert_eq!(
        nearest(exp2m1(parts(0.25))),
        real(false, -3, 0xC1BF_828C_6DC5_4B7A)
    );
    assert_eq!(
        nearest(exp2m1(parts(0.5))),
        real(false, -2, 0xD413_CCCF_E779_9211)
    );
    assert_eq!(
        nearest(exp2m1(parts(two(-40)))),
        real(false, -41, 0xB172_17F7_D20C_F928)
    );
    let one = parts(1.0);
    assert_eq!(
        nearest(y_log2_x(one, parts(3.0))),
        real(false, 0, 0xCAE0_0D1C_FDEB_43D0)
    );
    assert_eq!(
        nearest(y_log2_x(one, parts(1.5))),
        real(false, -1, 0x95C0_1A39_FBD6_87A0)
    );
    assert_eq!(
        nearest(y_log2_x(one, parts(0.75))),
        real(true, -2, 0xD47F_CB8C_0852_F0C1)
    );
    assert_eq!(
        nearest(y_log2_1p_x(one, parts(two(-20)))),
        real(false, -20, 0xB8AA_3564_0A7C_33EC)
    );
    // atan(1) is π/4, its 65th bit set: rounded up.
    assert_eq!(
        nearest(atan2(one, one)),
        real(false, -1, 0xC90F_DAA2_2168_C235)
    );
    assert_eq!(
        nearest(quarter_pis(4)),
        real(false, 1, 0xC90F_DAA2_2168_C235)
    );
}

#[test]
fn inside_the_8087s_ranges_they_agree_with_the_hosts_doubles() {
    let mut rng = Prng::new(0x8087_7A11);
    let mut fraction = || {
        // A double in [0, 1) of 53 random bits.
        f64::from_bits(0x3FF0_0000_0000_0000 | (rng.next_u64() >> 12)) - 1.0
    };
    for _ in 0..2_000 {
        let x = fraction() * core::f64::consts::FRAC_PI_4;
        if x != 0.0 {
            assert!(near(double(tan(parts(x))), x.tan(), 1), "tan {x:e}");
        }
        let (y, x) = (fraction(), fraction());
        if y < x && y != 0.0 {
            assert!(
                near(double(atan2(parts(y), parts(x))), y.atan2(x), 1),
                "atan {y:e}/{x:e}"
            );
        }
        let x = fraction() * 0.5;
        if x != 0.0 {
            let expected = (x * core::f64::consts::LN_2).exp_m1();
            assert!(near(double(exp2m1(parts(x))), expected, 2), "2^{x:e} - 1");
        }
        let x = fraction() * 4.0;
        if x > 0.0 && x.to_bits() != 1.0f64.to_bits() {
            assert!(
                near(double(y_log2_x(parts(1.0), parts(x))), x.log2(), 2),
                "log2 {x:e}"
            );
        }
        let x = (fraction() - 0.5) * 0.58;
        if x != 0.0 {
            let expected = x.ln_1p() / core::f64::consts::LN_2;
            assert!(
                near(double(y_log2_1p_x(parts(1.0), parts(x))), expected, 2),
                "log2(1+{x:e})"
            );
        }
    }
}

#[test]
fn outside_them_they_answer_as_the_80387_does() {
    for x in [1.0f64, 2.0, 3.0, -0.5, -3.0, 100.0, 1e10] {
        assert!(near(double(tan(parts(x))), x.tan(), 4), "tan {x}");
    }
    // Up to the 80387's 2^63 every operand is reduced and finite.
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
    // y scales the logarithm exactly as a product would.
    assert!(near(
        double(y_log2_x(parts(-3.0), parts(10.0))),
        -3.0 * 10.0f64.log2(),
        2
    ));
}

#[test]
fn the_nearest_integer_rounds_halves_away_from_zero_across_every_place() {
    let big = |x: f64| Big::of(parts(x));
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
        assert_eq!(big(x).nearest_integer(), n, "{x}");
    }
    assert_eq!(big(1e30).nearest_integer(), 1 << 62, "saturated");
    // Past 2^61, still exact: tan's reduction reaches every operand below 2^63.
    let k = (1i64 << 61) + 3;
    assert_eq!(Big::from_i64(k).nearest_integer(), k);
}
