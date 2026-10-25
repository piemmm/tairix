//! The special functions the statistical battery's p-values are expressed in.
//!
//! Every test in the battery reduces its statistic to a tail probability, and
//! each tail is the regularised upper incomplete gamma `Q(a, x)`: `erfc(x)` is
//! `Q(1/2, x²)` (DLMF §7.11), so there is one series and one continued
//! fraction to be right about rather than two of each. `ln Γ` is Lanczos's
//! approximation (Lanczos, 1964) with Godfrey's coefficients for `g = 7`;
//! `P(a, x)` is its power series (DLMF §8.7) and `Q(a, x)` the continued
//! fraction of DLMF §8.9, evaluated by the modified Lentz method (Thompson and
//! Barnett, 1986). Host test scaffolding that never enters a TAIRiX build.

use core::f64::consts::PI;

/// Relative change below which a series or fraction has converged: further
/// terms cannot move an `f64` result.
const EPSILON: f64 = 3.0e-16;

/// The terms a series or fraction may take; the battery's arguments converge
/// in far fewer, so the bound only stops a runaway.
const MAX_TERMS: u32 = 10_000;

/// What the Lentz method puts in place of a zero it would divide by.
const TINY: f64 = 1.0e-300;

/// Godfrey's Lanczos coefficients for `g = 7`, nine terms.
const LANCZOS: [f64; 9] = [
    0.999_999_999_999_809_9,
    676.520_368_121_885_1,
    -1_259.139_216_722_402_8,
    771.323_428_777_653_1,
    -176.615_029_162_140_6,
    12.507_343_278_686_905,
    -0.138_571_095_265_720_12,
    9.984_369_578_019_572e-6,
    1.505_632_735_149_311_6e-7,
];

/// The `g` the [`LANCZOS`] coefficients were fitted for.
const LANCZOS_G: f64 = 7.0;

/// `ln Γ(x)` for `x > 0`, by Lanczos's approximation, reflected below one
/// half where it loses accuracy.
fn ln_gamma(x: f64) -> f64 {
    if x < 0.5 {
        return (PI / (PI * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let z = x - 1.0;
    let series = LANCZOS[1..]
        .iter()
        .zip(1u32..)
        .fold(LANCZOS[0], |sum, (&c, k)| sum + c / (z + f64::from(k)));
    let t = z + LANCZOS_G + 0.5;
    0.5 * (2.0 * PI).ln() + (z + 0.5) * t.ln() - t + series.ln()
}

/// `ln(x^a e^(−x))`, the factor both expansions of the incomplete gamma share.
fn ln_kernel(a: f64, x: f64) -> f64 {
    a * x.ln() - x
}

/// `P(a, x)` by its power series,
/// `x^a e^(−x) / Γ(a + 1) · Σ x^k / ((a + 1)…(a + k))`: every term is positive
/// and they shrink once `k` passes `x − a`, so it suits `x < a + 1`.
fn lower_by_series(a: f64, x: f64) -> f64 {
    let (mut term, mut sum, mut next) = (1.0, 1.0, a);
    for _ in 0..MAX_TERMS {
        next += 1.0;
        term *= x / next;
        sum += term;
        if term < sum * EPSILON {
            break;
        }
    }
    (ln_kernel(a, x) - ln_gamma(a + 1.0)).exp() * sum
}

/// `Q(a, x)` by the continued fraction
/// `Γ(a, x) = x^a e^(−x) / (x + (1 − a)/(1 + 1/(x + (2 − a)/(1 + 2/(x + …)))))`,
/// which converges fast for `x ≥ a + 1`.
fn upper_by_continued_fraction(a: f64, x: f64) -> f64 {
    // The fraction's partial numerators and denominators: 1 over x, then
    // m − a over 1 and m over x in turn.
    let part = |j: u32| -> (f64, f64) {
        let m = f64::from(j / 2);
        match j {
            1 => (1.0, x),
            _ if j.is_multiple_of(2) => (m - a, 1.0),
            _ => (m, x),
        }
    };
    let nonzero = |value: f64| if value == 0.0 { TINY } else { value };
    let (mut value, mut c, mut d) = (TINY, TINY, 0.0);
    for j in 1..=MAX_TERMS {
        let (numerator, denominator) = part(j);
        d = 1.0 / nonzero(denominator + numerator * d);
        c = nonzero(denominator + numerator / c);
        let step = c * d;
        value *= step;
        if (step - 1.0).abs() < EPSILON {
            break;
        }
    }
    (ln_kernel(a, x) - ln_gamma(a)).exp() * value
}

/// `Q(a, x)`, the regularised upper incomplete gamma: the probability that a
/// chi-square variate with `2a` degrees of freedom exceeds `2x`.
///
/// Returns `1.0` for a non-positive `x` (nothing has been observed, so
/// nothing is surprising) and `0.0` for a non-positive `a`, which no caller
/// in the battery passes.
#[must_use]
pub fn gamma_q(a: f64, x: f64) -> f64 {
    if a <= 0.0 {
        return 0.0;
    }
    if x <= 0.0 {
        return 1.0;
    }
    if x < a + 1.0 {
        (1.0 - lower_by_series(a, x)).clamp(0.0, 1.0)
    } else {
        upper_by_continued_fraction(a, x).clamp(0.0, 1.0)
    }
}

/// `erfc(x)`, the complementary error function.
///
/// Expressed through [`gamma_q`] (`erfc(x) = Q(1/2, x²)` for `x >= 0`) and
/// reflected for negative arguments, so the battery carries one tail
/// implementation rather than two.
#[must_use]
pub fn erfc(x: f64) -> f64 {
    if x < 0.0 {
        2.0 - gamma_q(0.5, x * x)
    } else {
        gamma_q(0.5, x * x)
    }
}

/// The standard normal cumulative distribution `Φ(z)`.
#[must_use]
pub fn normal_cdf(z: f64) -> f64 {
    0.5 * erfc(-z / core::f64::consts::SQRT_2)
}

/// The upper-tail probability of a chi-square statistic with `df` degrees of
/// freedom — the form every goodness-of-fit test in the battery reports.
#[must_use]
pub fn chi_square_q(chi_square: f64, df: f64) -> f64 {
    gamma_q(df / 2.0, chi_square / 2.0)
}

#[cfg(test)]
mod tests {
    use super::{chi_square_q, erfc, gamma_q, ln_gamma, normal_cdf};

    fn close(got: f64, want: f64, tol: f64) {
        assert!(
            (got - want).abs() <= tol,
            "got {got}, want {want} (tolerance {tol})"
        );
    }

    #[test]
    fn ln_gamma_matches_known_values() {
        // Γ(1) = Γ(2) = 1, Γ(1/2) = √π, Γ(6) = 120.
        close(ln_gamma(1.0), 0.0, 1e-12);
        close(ln_gamma(2.0), 0.0, 1e-12);
        close(ln_gamma(0.5), core::f64::consts::PI.sqrt().ln(), 1e-12);
        close(ln_gamma(6.0), 120.0_f64.ln(), 1e-11);
        // A large argument, as approximate entropy's 2^(m-1) supplies.
        close(ln_gamma(512.0), 2_679.822_147_001_309, 1e-6);
        // Below one half, through the reflection: Γ(1/4) = 3.625 609 908 2…
        close(ln_gamma(0.25), 3.625_609_908_221_908_f64.ln(), 1e-12);
    }

    #[test]
    fn erfc_matches_known_values() {
        close(erfc(0.0), 1.0, 1e-14);
        close(erfc(1.0), 0.157_299_207_050_285_13, 1e-12);
        close(erfc(-1.0), 1.842_700_792_949_715, 1e-12);
        close(erfc(2.0), 0.004_677_734_981_047_266, 1e-14);
        close(erfc(3.0), 2.209_049_699_858_544_3e-5, 1e-16);
        close(erfc(5.0), 1.537_459_794_428_035_7e-12, 1e-22);
    }

    /// Both branches of `gamma_q` must agree with each other where they meet,
    /// or a p-value would jump at `x = a + 1`.
    #[test]
    fn the_two_gamma_expansions_agree_at_their_boundary() {
        for a in [0.5, 1.0, 4.5, 24.5, 512.0] {
            let boundary = a + 1.0;
            let below = gamma_q(a, boundary - 1e-9);
            let above = gamma_q(a, boundary + 1e-9);
            close(below, above, 1e-9);
        }
    }

    #[test]
    fn chi_square_tails_match_published_values() {
        // A chi-square of 3.841 with 1 df is the 5% point; 16.919 with 9 df
        // is the 5% point; 0.0 is never surprising.
        close(chi_square_q(3.841_458_820_694_124, 1.0), 0.05, 1e-9);
        close(chi_square_q(16.918_977_604_620_448, 9.0), 0.05, 1e-9);
        close(chi_square_q(0.0, 9.0), 1.0, 1e-15);
        // Many degrees of freedom: 124.342 is the 5% point for 100.
        close(chi_square_q(124.342_113_404_066_9, 100.0), 0.05, 1e-9);
        // The far tail, where a rejected generator lands.
        assert!(chi_square_q(500.0, 9.0) < 1e-100);
    }

    #[test]
    fn a_tail_probability_is_always_a_probability() {
        for a in [0.5, 1.0, 4.5, 512.0] {
            for x in [0.0, 1e-12, 0.1, 1.0, 10.0, 1e3, 1e6] {
                let q = gamma_q(a, x);
                assert!((0.0..=1.0).contains(&q), "Q({a}, {x}) = {q}");
            }
        }
    }

    #[test]
    fn normal_cdf_matches_known_values() {
        close(normal_cdf(0.0), 0.5, 1e-14);
        close(normal_cdf(1.959_963_984_540_054), 0.975, 1e-12);
        close(normal_cdf(-1.959_963_984_540_054), 0.025, 1e-12);
        // Monotone and bounded across a wide span.
        let mut previous = 0.0;
        for step in -80..=80 {
            let value = normal_cdf(f64::from(step) / 10.0);
            assert!(value >= previous, "Φ is not monotone at {step}");
            assert!((0.0..=1.0).contains(&value));
            previous = value;
        }
    }
}
