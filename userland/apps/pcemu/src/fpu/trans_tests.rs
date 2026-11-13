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

/// The true values, as double-doubles summed by their series: the same on
/// every host, where a platform libm's doubles are not.
mod exact {
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

    fn sin(x: Dd) -> Dd {
        let square = x.mul(x);
        series(x, |term, n| {
            term.mul(square)
                .div(Dd::of(-f64::from((2 * n) * (2 * n + 1))))
        })
    }

    fn cos(x: Dd) -> Dd {
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
    fn ln(x: f64) -> Dd {
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

    pub fn tan(x: f64) -> f64 {
        sin(Dd::of(x)).div(cos(Dd::of(x))).double()
    }

    /// atan2(y, x) for 0 < y < x.
    pub fn atan2(y: f64, x: f64) -> f64 {
        atan(Dd::of(y).div(Dd::of(x))).double()
    }

    pub fn exp2m1(x: f64) -> f64 {
        expm1(Dd::of(x).mul(ln2())).double()
    }

    pub fn log2(x: f64) -> f64 {
        ln(x).div(ln2()).double()
    }

    /// log2(1 + x), as 2·atanh(x / (2 + x)) / ln 2.
    pub fn log2_1p(x: f64) -> f64 {
        let half = odd_powers(Dd::of(x).div(Dd::of(2.0).add(Dd::of(x))), false);
        half.add(half).div(ln2()).double()
    }
}

#[test]
fn the_exact_values_are_the_correctly_rounded_ones_at_known_points() {
    for (exact, rounded) in [
        (
            exact::tan(7.031_503_549_713_639e-1),
            0.847_688_099_932_854_f64,
        ),
        (exact::atan2(1.0, 1.0), core::f64::consts::FRAC_PI_4),
        (exact::exp2m1(0.5), 0.414_213_562_373_095_03),
        (exact::log2(3.0), 1.584_962_500_721_156),
        (exact::log2_1p(0.25), 0.321_928_094_887_362_35),
    ] {
        assert_eq!(exact.to_bits(), rounded.to_bits(), "{exact:e}");
    }
}

/// The regression: these were checked against the host's doubles, and a
/// libm a few ulps out, as macOS's tangent is, failed them.
#[test]
fn inside_the_8087s_ranges_they_agree_with_the_true_values() {
    let mut rng = Prng::new(0x8087_7A11);
    let mut fraction = || {
        // A double in [0, 1) of 53 random bits.
        f64::from_bits(0x3FF0_0000_0000_0000 | (rng.next_u64() >> 12)) - 1.0
    };
    for _ in 0..2_000 {
        let x = fraction() * core::f64::consts::FRAC_PI_4;
        if x != 0.0 {
            assert!(near(double(tan(parts(x))), exact::tan(x), 1), "tan {x:e}");
        }
        let (y, x) = (fraction(), fraction());
        if y < x && y != 0.0 {
            assert!(
                near(double(atan2(parts(y), parts(x))), exact::atan2(y, x), 1),
                "atan {y:e}/{x:e}"
            );
        }
        let x = fraction() * 0.5;
        if x != 0.0 {
            assert!(
                near(double(exp2m1(parts(x))), exact::exp2m1(x), 2),
                "2^{x:e} - 1"
            );
        }
        let x = fraction() * 4.0;
        if x > 0.0 && x.to_bits() != 1.0f64.to_bits() {
            assert!(
                near(double(y_log2_x(parts(1.0), parts(x))), exact::log2(x), 2),
                "log2 {x:e}"
            );
        }
        let x = (fraction() - 0.5) * 0.58;
        if x != 0.0 {
            assert!(
                near(
                    double(y_log2_1p_x(parts(1.0), parts(x))),
                    exact::log2_1p(x),
                    2
                ),
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
