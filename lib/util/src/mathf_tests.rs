//! Unit tests for the shared `no_std` `f64` maths.
//!
//! The reference values are the IEEE-754 doubles a correctly-rounded libm
//! produces, quoted to their full precision, so a regression in a series or a
//! reduction shows up as a numeric difference rather than as artwork that
//! merely looks a bit wrong.

extern crate std;

use core::f64::consts::{FRAC_PI_2, FRAC_PI_3, FRAC_PI_4, PI, SQRT_2};

use super::{
    acos, acosh, asin, asinh, atan, atan2, atanh, bilinear, byte, ceil, clamp, cos, cosh,
    double_to_half, exp, exp2, fabs, float_bits, float_of, floor, fmax, fmin, from_half, hypot, ln,
    log2, power, quantized_to_half, real, reduce, round, round_even, round_i32, share, sin, single,
    sinh, smoothstep, smoothstepf, sqrt, tan, tanh, to_half, trunc, Phasor, EXP_MAX_ARG,
    EXP_MIN_ARG, REDUCIBLE,
};
use super::{expm1, log1p};

/// The accuracy every transcendental function is held to: far finer than the
/// sub-pixel grid any consumer rasterises onto, and tight enough that a wrong
/// coefficient or a mis-folded quadrant cannot pass.
const EPS: f64 = 1e-9;

#[track_caller]
fn close(actual: f64, expected: f64) {
    assert!(
        fabs(actual - expected) <= EPS,
        "expected {expected}, got {actual}"
    );
}

/// `actual` is `expected` to the bit, its sign of zero included.
#[track_caller]
fn same(actual: f64, expected: f64) {
    assert_eq!(
        actual.to_bits(),
        expected.to_bits(),
        "expected {expected}, got {actual}"
    );
}

/// How many representable doubles lie between `a` and `b`.
fn ulps_apart(a: f64, b: f64) -> u64 {
    let ordered = |x: f64| {
        let bits = i64::from_ne_bytes(x.to_bits().to_ne_bytes());
        if bits < 0 {
            i64::MIN - bits
        } else {
            bits
        }
    };
    ordered(a).abs_diff(ordered(b))
}

/// How far `ours` lies from a true value quoted as its correctly rounded
/// double and the remainder past that, in ulps at the true value: below the
/// double's magnitude the spacing is the one beneath it, which halves just
/// under a power of two.
fn ulps_from_true(ours: f64, (rounded, rest): (f64, f64)) -> f64 {
    let magnitude = fabs(rounded);
    // Signs, not a product, which underflows for small values.
    let truth_below = rest != 0.0 && rest.is_sign_negative() != rounded.is_sign_negative();
    let spacing = if truth_below {
        magnitude - f64::from_bits(magnitude.to_bits() - 1)
    } else {
        f64::from_bits(magnitude.to_bits() + 1) - magnitude
    };
    fabs((ours - rounded) - rest) / spacing
}

/// Hold `ours` within `ulps` of the host libm's `theirs` at every point of
/// `range` stepped by `step`, reporting the worst point either way.
#[track_caller]
fn tracks_the_host(
    name: &str,
    ours: fn(f64) -> f64,
    theirs: fn(f64) -> f64,
    (from, to, step): (f64, f64, f64),
    ulps: u64,
) {
    let mut worst = (0, from);
    let mut x = from;
    while x <= to {
        let apart = ulps_apart(ours(x), theirs(x));
        if apart > worst.0 {
            worst = (apart, x);
        }
        x += step;
    }
    assert!(
        worst.0 <= ulps,
        "{name} is {} ulps from the host at {:e}",
        worst.0,
        worst.1
    );
}

// --- rounding -----------------------------------------------------------

#[test]
fn floor_and_ceil_bracket_a_fraction() {
    close(floor(2.1), 2.0);
    close(ceil(2.1), 3.0);
    close(floor(-2.1), -3.0);
    close(ceil(-2.1), -2.0);
    close(floor(4.0), 4.0);
    close(ceil(4.0), 4.0);
}

#[test]
fn round_takes_halves_upward() {
    close(round(2.5), 3.0);
    close(round(-2.5), -2.0);
    close(round(2.49), 2.0);
}

/// `floor(x + 0.5)` rounds the sum before it floors, which carries the
/// largest double below a half, and every odd integer past 2^52, up by one.
#[allow(
    clippy::float_cmp,
    reason = "a rounding result is an exact integer, so a tolerance would \
              accept the off-by-one this pins"
)]
#[test]
fn round_decides_on_the_exact_fraction() {
    let below_half = 0.499_999_999_999_999_94;
    assert!(below_half < 0.5 && below_half + 0.5 == 1.0);
    assert_eq!(round(below_half), 0.0);
    assert_eq!(round(-below_half), 0.0);
    assert_eq!(round_i32(below_half), 0);
    let odd = 4_503_599_627_370_497.0;
    assert_eq!(round(odd), odd);
    assert_eq!(round(-odd), -odd);
    assert_eq!(round(1.5), 2.0);
    assert_eq!(round(-1.5), -1.0);
    assert_eq!(round(-0.5), 0.0);
}

/// Integer rounding answers `0` for a `NaN` and keeps its exact meaning past
/// the range of any integer type.
#[allow(
    clippy::float_cmp,
    reason = "these are exact integers, which is the property under test"
)]
#[test]
fn integer_rounding_is_total_and_exact_at_any_magnitude() {
    for f in [floor, ceil, round] {
        assert_eq!(f(f64::NAN), 0.0);
        assert_eq!(f(1e300), 1e300);
        assert_eq!(f(-1e300), -1e300);
    }
    assert_eq!(round_i32(f64::NAN), 0);
    assert_eq!(floor(-0.5), -1.0);
    assert_eq!(ceil(0.5), 1.0);
    assert_eq!(floor(1.5e19), 1.5e19);
    assert_eq!(ceil(-1.5e19), -1.5e19);
}

#[test]
fn magnitude_and_ordering_helpers_agree_with_their_names() {
    close(fabs(-3.5), 3.5);
    close(fmax(-3.5, 2.0), 2.0);
    close(fmin(-3.5, 2.0), -3.5);
    close(clamp(9.0, 0.0, 4.0), 4.0);
    close(clamp(-9.0, 0.0, 4.0), 0.0);
    close(clamp(2.0, 0.0, 4.0), 2.0);
    same(clamp(f64::NAN, -1.0, 4.0), -1.0);
    same(fmax(f64::NAN, 2.0), 2.0);
    same(fmin(f64::NAN, 2.0), 2.0);
}

#[test]
fn smoothstep_is_clamped_monotone_and_flat_at_its_ends() {
    for (t, expected) in [(-1.0, 0.0), (0.0, 0.0), (0.5, 0.5), (1.0, 1.0), (7.0, 1.0)] {
        assert_eq!(smoothstep(t).to_bits(), f64::to_bits(expected), "{t}");
    }
    for (t, expected) in [
        (0.25, 0.156_25),
        (f64::NAN, 0.0),
        (f64::INFINITY, 1.0),
        (f64::NEG_INFINITY, 0.0),
    ] {
        assert_eq!(smoothstep(t).to_bits(), f64::to_bits(expected), "{t}");
    }
    let mut previous = 0.0;
    for step in 0..=1000 {
        let value = smoothstep(f64::from(step) / 1000.0);
        assert!(value >= previous, "{step}");
        previous = value;
    }
    // Flat at both ends: a step's rise there is second order in the step.
    assert!(smoothstep(1e-4) < 1e-7);
    assert!(1.0 - smoothstep(1.0 - 1e-4) < 1e-7);
}

/// The single-precision twin is the same polynomial, so it agrees with the
/// double one to within the precision it keeps.
#[test]
fn single_precision_smoothstep_tracks_the_double_one() {
    for step in -10..=1010 {
        let t = f64::from(step) / 1000.0;
        #[allow(clippy::cast_possible_truncation, reason = "a test input within f32")]
        let single = smoothstepf(t as f32);
        assert!(fabs(f64::from(single) - smoothstep(t)) < 1e-6, "{t}");
    }
    assert_eq!(smoothstepf(f32::NAN).to_bits(), 0.0_f32.to_bits());
    assert_eq!(smoothstepf(2.0).to_bits(), 1.0_f32.to_bits());
}

/// Rounding to single precision takes the nearest value, a tie to the even
/// one, and overflows to infinity rather than wrapping.
#[test]
fn single_precision_rounds_to_the_nearest_and_ties_to_even() {
    assert_eq!(single(0.1).to_bits(), 0x3dcc_cccd);
    // Halfway between 1 and the next single, `1 + 2^-23`: the tie goes to 1,
    // and the next tie up to `1 + 2^-22`, both even.
    let half_ulp = 1.0 / 16_777_216.0;
    assert_eq!(single(1.0 + half_ulp).to_bits(), 1.0_f32.to_bits());
    assert_eq!(
        single(1.0 + 3.0 * half_ulp).to_bits(),
        (1.0_f32 + 2.0 * f32::EPSILON).to_bits()
    );
    assert_eq!(single(1e300).to_bits(), f32::INFINITY.to_bits());
    assert_eq!(single(-1e300).to_bits(), f32::NEG_INFINITY.to_bits());
    assert_eq!(single(-0.0).to_bits(), (-0.0_f32).to_bits());
}

/// A bilinear blend is each corner at its corner, the mean at the middle, and
/// a straight line along each edge.
#[test]
fn a_bilinear_blend_meets_its_corners_and_runs_straight_along_its_edges() {
    let corners = [1.0, 3.0, 5.0, 11.0];
    for (at, blend) in [
        ((0.0, 0.0), 1.0),
        ((1.0, 0.0), 3.0),
        ((0.0, 1.0), 5.0),
        ((1.0, 1.0), 11.0),
        ((0.5, 0.5), 5.0),
        ((0.25, 0.0), 1.5),
        ((1.0, 0.5), 7.0),
    ] {
        assert_eq!(
            bilinear(corners, at).to_bits(),
            f64::to_bits(blend),
            "{at:?}"
        );
    }
}

/// A coordinate a hostile document drove far out of range must clamp to the
/// extreme rather than wrap to the opposite side of the canvas.
#[test]
fn rounding_to_i32_saturates_instead_of_wrapping() {
    assert_eq!(round_i32(2.5), 3);
    assert_eq!(round_i32(-2.5), -2);
    assert_eq!(round_i32(1e300), i32::MAX);
    assert_eq!(round_i32(-1e300), i32::MIN);
}

// --- roots --------------------------------------------------------------

#[test]
fn square_roots_are_exact_to_the_last_bits() {
    close(sqrt(2.0), SQRT_2);
    close(sqrt(4.0), 2.0);
    close(sqrt(1e12), 1e6);
    close(sqrt(1e-12), 1e-6);
}

/// A negative or absent magnitude has no root to take; answering zero keeps
/// the caller's geometry finite instead of poisoning it with a `NaN`.
#[test]
fn a_non_positive_square_root_is_zero_not_a_nan() {
    close(sqrt(0.0), 0.0);
    close(sqrt(-4.0), 0.0);
    close(sqrt(f64::NAN), 0.0);
    assert_eq!(sqrt(-0.0).to_bits(), 0.0_f64.to_bits());
    assert_eq!(sqrt(f64::NEG_INFINITY).to_bits(), 0.0_f64.to_bits());
    assert_eq!(sqrt(f64::INFINITY).to_bits(), f64::INFINITY.to_bits());
}

/// The square root is correctly rounded, so it is the one IEEE 754 answer —
/// the same bits the host's own square root gives, for normal, subnormal and
/// extreme inputs alike. That agreement is what every target's instruction or
/// runtime routine is held to, and what the cross-target digests rest on.
#[test]
fn the_square_root_is_correctly_rounded() {
    // A stride through the whole positive range, co-prime to the mantissa
    // width so it lands on every exponent with a different fraction each time.
    let stride = 0x0000_1d3a_71f9_0b5d_u64;
    let mut bits = 1_u64;
    while bits < f64::INFINITY.to_bits() {
        let x = f64::from_bits(bits);
        assert_eq!(sqrt(x).to_bits(), f64::sqrt(x).to_bits(), "sqrt({x:e})");
        bits += stride;
    }
    for x in [f64::MIN_POSITIVE, f64::MAX, 2.0, 0.5, 1e-310] {
        assert_eq!(sqrt(x).to_bits(), f64::sqrt(x).to_bits(), "sqrt({x:e})");
    }
}

#[test]
fn hypotenuse_matches_the_triangle_and_handles_the_origin() {
    close(hypot(3.0, 4.0), 5.0);
    close(hypot(-3.0, -4.0), 5.0);
    close(hypot(0.0, 0.0), 0.0);
}

/// A length past the double's range saturates as every other answer here
/// does, and a `NaN` on either side answers alike.
#[test]
fn a_hypotenuse_past_the_range_saturates_and_a_nan_either_side_is_nought() {
    for (x, y) in [
        (f64::MAX, f64::MAX),
        (1.3e308, 1.3e308),
        (f64::INFINITY, 2.0),
        (-3.0, f64::NEG_INFINITY),
        (f64::INFINITY, f64::INFINITY),
    ] {
        same(hypot(x, y), f64::MAX);
    }
    same(hypot(f64::NAN, 1.0), 0.0);
    same(hypot(1.0, f64::NAN), 0.0);
    same(hypot(f64::MAX, 0.0), f64::MAX);
}

// --- angles -------------------------------------------------------------

#[test]
fn sine_matches_the_reference_across_every_quadrant() {
    close(sin(0.0), 0.0);
    close(sin(0.3), 0.295_520_206_661_339_55);
    close(sin(1.0), 0.841_470_984_807_896_5);
    close(sin(2.0), 0.909_297_426_825_681_7);
    close(sin(3.0), 0.141_120_008_059_867_2);
    close(sin(-2.5), -0.598_472_144_103_956_5);
    close(sin(10.0), -0.544_021_110_889_369_8);
}

#[test]
fn cosine_matches_the_reference_across_every_quadrant() {
    close(cos(0.0), 1.0);
    close(cos(0.7), 0.764_842_187_284_488_5);
    close(cos(2.4), -0.737_393_715_541_245_4);
    close(cos(-3.9), -0.725_932_304_200_140_2);
}

/// The identity holds everywhere, including far outside one turn where the
/// range reduction is doing the work.
#[test]
fn sine_and_cosine_stay_on_the_unit_circle() {
    let mut angle = -40.0;
    while angle < 40.0 {
        let unit = sin(angle) * sin(angle) + cos(angle) * cos(angle);
        close(unit, 1.0);
        angle += 0.37;
    }
}

/// A row of a thousand samples, the length a screen-wide sweep runs to, stays
/// on the sine it started from.
#[test]
fn a_phasor_sweeps_the_sine_it_was_turned_from() {
    let (amplitude, angle, step) = (2.5, 0.7, 0.013);
    let mut phasor = Phasor::new(amplitude, angle, step);
    for n in 0..1_000u32 {
        close(phasor.value(), amplitude * sin(angle + f64::from(n) * step));
        phasor.advance();
    }
}

#[test]
fn a_phasor_that_does_not_turn_holds_its_value() {
    let mut phasor = Phasor::new(-1.5, 2.0, 0.0);
    let held = phasor.value();
    close(held, -1.5 * sin(2.0));
    for _ in 0..100 {
        phasor.advance();
    }
    close(phasor.value(), held);
}

#[test]
fn tangent_matches_the_reference_and_survives_its_pole() {
    close(tan(0.4), 0.422_793_218_738_161_8);
    close(tan(-1.2), -2.572_151_622_126_318_8);
    assert!(tan(FRAC_PI_2).is_finite());
    assert!(tan(-FRAC_PI_2).is_finite());
}

#[test]
fn arctangent_matches_the_reference_on_both_sides_of_its_reduction() {
    close(atan(0.0), 0.0);
    close(atan(0.1), 0.099_668_652_491_162_04);
    close(atan(0.5), 0.463_647_609_000_806_1);
    close(atan(1.0), PI / 4.0);
    close(atan(3.0), 1.249_045_772_398_254_4);
    close(atan(-3.0), -1.249_045_772_398_254_4);
}

#[test]
fn atan2_names_the_right_quadrant() {
    close(atan2(1.0, 1.0), FRAC_PI_4);
    close(atan2(1.0, -1.0), 2.356_194_490_192_345);
    close(atan2(-1.0, -1.0), -2.356_194_490_192_345);
    close(atan2(-3.0, 4.0), -0.643_501_108_793_284_4);
    close(atan2(1.0, 0.0), FRAC_PI_2);
    close(atan2(-1.0, 0.0), -FRAC_PI_2);
}

/// A degenerate segment has no direction; answering zero keeps a stroke's
/// normal finite rather than erasing the shape.
#[test]
fn atan2_at_the_origin_is_zero_not_a_nan() {
    close(atan2(0.0, 0.0), 0.0);
}

/// fdlibm's kernels track a correctly rounded libm to within an ulp; the
/// tangent, a quotient of two of them, `atan2`, the arctangent of a rounded
/// quotient, and the hyperbolic functions, each one of those kernels and a
/// further rounding or two, carry the more the module allows. A host's own
/// inverse hyperbolic functions may be an ulp and a half out, so theirs are
/// held to two, and [`QUOTED`] holds the points that showed it to the truth.
#[test]
fn the_transcendentals_track_a_correctly_rounded_libm() {
    tracks_the_host("sin", sin, f64::sin, (-40.0, 40.0, 0.000_97), 1);
    tracks_the_host("cos", cos, f64::cos, (-40.0, 40.0, 0.000_97), 1);
    tracks_the_host("tan", tan, f64::tan, (-1.5, 1.5, 0.000_97), 3);
    tracks_the_host("atan", atan, f64::atan, (-60.0, 60.0, 0.000_97), 1);
    tracks_the_host("asin", asin, f64::asin, (-1.0, 1.0, 0.000_097), 1);
    tracks_the_host("acos", acos, f64::acos, (-1.0, 1.0, 0.000_097), 1);
    tracks_the_host("exp2", exp2, f64::exp2, (-1022.0, 1023.9, 0.013), 1);
    tracks_the_host("log2", log2, f64::log2, (0.000_97, 8.0, 0.000_097), 1);
    for y in [-1e6, -3.5, -0.1, -1e-9, 0.0, 1e-9, 0.25, 7.0, 1e6] {
        for x in [-1e6, -7.0, -1.0, -1e-3, 1e-3, 0.5, 3.0, 1e6] {
            let apart = ulps_apart(atan2(y, x), y.atan2(x));
            assert!(apart <= 2, "atan2({y}, {x}) is {apart} ulps from the host");
        }
    }
    let mut x = 0.013;
    while x < 1e4 {
        for p in [-300.5, -17.0, -1.5, -0.3, 0.1, 1.7, 3.0, 9.25, 60.0] {
            let apart = ulps_apart(power(x, p), x.powf(p));
            assert!(apart <= 1, "power({x}, {p}) is {apart} ulps from the host");
        }
        x *= 1.37;
    }
    tracks_the_host("exp", exp, f64::exp, (-708.39, 709.78, 0.013), 1);
    tracks_the_host("expm1", expm1, f64::exp_m1, (-50.0, 709.78, 0.013), 1);
    tracks_the_host("log1p", log1p, f64::ln_1p, (-0.999, 1e3, 0.000_97), 1);
    // Past e^709.78 the two halves of e^(x/2) are squared, so three ulps.
    tracks_the_host("sinh", sinh, f64::sinh, (-710.47, 710.47, 0.013), 3);
    tracks_the_host("cosh", cosh, f64::cosh, (-710.47, 710.47, 0.013), 3);
    tracks_the_host("sinh", sinh, f64::sinh, (-709.7, 709.7, 0.013), 2);
    tracks_the_host("cosh", cosh, f64::cosh, (-709.7, 709.7, 0.013), 2);
    tracks_the_host("tanh", tanh, f64::tanh, (-25.0, 25.0, 0.000_97), 2);
    tracks_the_host("asinh", asinh, f64::asinh, (-1e3, 1e3, 0.000_97), 2);
    tracks_the_host("acosh", acosh, f64::acosh, (1.0, 1e3, 0.000_97), 2);
    // The host's `atanh` takes `ln_1p` of a sum that cancels for a negative
    // argument near -1, so it is held only on its accurate, positive half.
    let odd = |x: f64| f64::atanh(fabs(x)).copysign(x);
    tracks_the_host("atanh", atanh, odd, (-0.999, 0.999, 0.000_097), 2);
}

/// Near zero `e^x - 1` and `ln(1 + x)` cancel all but their last few digits,
/// so the hyperbolic functions are taken through `expm1` and `log1p` there:
/// each held to an ulp of the correctly rounded value, quoted.
#[test]
fn the_hyperbolic_functions_keep_every_digit_near_zero() {
    // A function, an argument and the correctly rounded value there.
    type Case = (fn(f64) -> f64, f64, f64);
    let cases: [Case; 72] = [
        (sinh, 1.0e-300, 1.0e-300),
        (sinh, 1.0e-17, 1.0e-17),
        (sinh, 1.0e-10, 1.0e-10),
        (sinh, 3.0e-5, 3.000_000_000_450_000_2e-5),
        (sinh, 0.01, 0.010_000_166_667_500_003),
        (sinh, 0.3, 0.304_520_293_447_142_6),
        (sinh, -1.0e-300, -1.0e-300),
        (sinh, -1.0e-17, -1.0e-17),
        (sinh, -1.0e-10, -1.0e-10),
        (sinh, -3.0e-5, -3.000_000_000_450_000_2e-5),
        (sinh, -0.01, -0.010_000_166_667_500_003),
        (sinh, -0.3, -0.304_520_293_447_142_6),
        (tanh, 1.0e-300, 1.0e-300),
        (tanh, 1.0e-17, 1.0e-17),
        (tanh, 1.0e-10, 1.0e-10),
        (tanh, 3.0e-5, 2.999_999_999_1e-5),
        (tanh, 0.01, 0.009_999_666_679_999_46),
        (tanh, 0.3, 0.291_312_612_451_590_9),
        (tanh, -1.0e-300, -1.0e-300),
        (tanh, -1.0e-17, -1.0e-17),
        (tanh, -1.0e-10, -1.0e-10),
        (tanh, -3.0e-5, -2.999_999_999_1e-5),
        (tanh, -0.01, -0.009_999_666_679_999_46),
        (tanh, -0.3, -0.291_312_612_451_590_9),
        (asinh, 1.0e-300, 1.0e-300),
        (asinh, 1.0e-17, 1.0e-17),
        (asinh, 1.0e-10, 1.0e-10),
        (asinh, 3.0e-5, 2.999_999_999_55e-5),
        (asinh, 0.01, 0.009_999_833_340_832_888),
        (asinh, 0.3, 0.295_673_047_563_422_44),
        (asinh, -1.0e-300, -1.0e-300),
        (asinh, -1.0e-17, -1.0e-17),
        (asinh, -1.0e-10, -1.0e-10),
        (asinh, -3.0e-5, -2.999_999_999_55e-5),
        (asinh, -0.01, -0.009_999_833_340_832_888),
        (asinh, -0.3, -0.295_673_047_563_422_44),
        (atanh, 1.0e-300, 1.0e-300),
        (atanh, 1.0e-17, 1.0e-17),
        (atanh, 1.0e-10, 1.0e-10),
        (atanh, 3.0e-5, 3.000_000_000_9e-5),
        (atanh, 0.01, 0.010_000_333_353_334_763),
        (atanh, 0.3, 0.309_519_604_203_111_7),
        (atanh, -1.0e-300, -1.0e-300),
        (atanh, -1.0e-17, -1.0e-17),
        (atanh, -1.0e-10, -1.0e-10),
        (atanh, -3.0e-5, -3.000_000_000_9e-5),
        (atanh, -0.01, -0.010_000_333_353_334_763),
        (atanh, -0.3, -0.309_519_604_203_111_7),
        (expm1, 1.0e-300, 1.0e-300),
        (expm1, 1.0e-17, 1.0e-17),
        (expm1, 1.0e-10, 1.000_000_000_05e-10),
        (expm1, 3.0e-5, 3.000_045_000_450_003_4e-5),
        (expm1, 0.01, 0.010_050_167_084_168_058),
        (expm1, 0.3, 0.349_858_807_576_003_1),
        (expm1, -1.0e-300, -1.0e-300),
        (expm1, -1.0e-17, -1.0e-17),
        (expm1, -1.0e-10, -9.999_999_999_500_001e-11),
        (expm1, -3.0e-5, -2.999_955_000_449_996_7e-5),
        (expm1, -0.01, -0.009_950_166_250_831_947),
        (expm1, -0.3, -0.259_181_779_318_282_1),
        (log1p, 1.0e-300, 1.0e-300),
        (log1p, 1.0e-17, 1.0e-17),
        (log1p, 1.0e-10, 9.999_999_999_500_001e-11),
        (log1p, 3.0e-5, 2.999_955_000_899_98e-5),
        (log1p, 0.01, 0.009_950_330_853_168_083),
        (log1p, 0.3, 0.262_364_264_467_491_06),
        (log1p, -1.0e-300, -1.0e-300),
        (log1p, -1.0e-17, -1.0e-17),
        (log1p, -1.0e-10, -1.000_000_000_05e-10),
        (log1p, -3.0e-5, -3.000_045_000_900_020_5e-5),
        (log1p, -0.01, -0.010_050_335_853_501_442),
        (log1p, -0.3, -0.356_674_943_938_732_34),
    ];
    for (function, x, expected) in cases {
        let apart = ulps_apart(function(x), expected);
        assert!(apart <= 1, "{apart} ulps at {x:e}");
    }
    same(cosh(1e-20), 1.0);
}

/// Next to a multiple of `PI/2` the remainder is tiny, and only a reduction
/// that takes more of `PI/2`'s bits as the first ones cancel keeps it accurate
/// to its own last bit: with one part the answer here is off by millions of
/// ulps while still looking like zero.
#[test]
fn angles_beside_a_quarter_turn_stay_accurate_to_the_last_bit() {
    let quarter_turns = (1..=4_000_u32).chain([65_535, 262_143, 1_000_000]);
    for k in quarter_turns {
        let near = f64::from(k) * FRAC_PI_2;
        for bits in near.to_bits() - 2..=near.to_bits() + 2 {
            for x in [f64::from_bits(bits), -f64::from_bits(bits)] {
                assert!(ulps_apart(sin(x), f64::sin(x)) <= 1, "sin({x:e})");
                assert!(ulps_apart(cos(x), f64::cos(x)) <= 1, "cos({x:e})");
            }
        }
    }
}

/// Past 2^20 quarter turns the parts of `PI/2` no longer reduce exactly, and
/// an angle there once answered the value at that bound: a sway driven by
/// uptime froze after eleven days. Each reference is the correctly rounded
/// value, worked out in exact rational arithmetic; the first angle is the
/// double nearest a quarter turn of all, which leaves a remainder of 2^-61.
#[test]
fn angles_past_a_million_quarter_turns_reduce_exactly() {
    let worst = 6_381_956_970_095_103.0 * f64::from_bits((1023 + 797) << 52);
    let references = [
        (worst, 1.0, -4.687_165_924_254_628e-19),
        (
            1_647_100.0,
            0.621_640_037_921_421_2,
            0.783_303_046_880_997_4,
        ),
        (2.0e6, -0.655_714_315_563_47, 0.755_009_096_875_746_4),
        (3.0e9, 0.987_004_886_474_355_4, -0.160_690_242_627_687_05),
        (5.0e15, -0.901_711_760_523_585, -0.432_337_716_297_638),
        (1e22, -0.852_200_849_767_188_8, 0.523_214_785_395_139),
        (-7.5e30, -0.965_150_598_936_113_2, 0.261_695_092_375_195_1),
        (1e100, -0.380_637_731_005_028_7, 0.924_724_238_751_933_8),
        (
            f64::MAX,
            0.004_961_954_789_184_062,
            -0.999_987_689_426_559_9,
        ),
    ];
    for (x, sine, cosine) in references {
        assert!(ulps_apart(sin(x), sine) <= 1, "sin({x:e}) = {:e}", sin(x));
        assert!(ulps_apart(cos(x), cosine) <= 1, "cos({x:e}) = {:e}", cos(x));
    }
    let (head, tail, quadrant) = reduce(worst);
    assert_eq!(head.to_bits(), 4.687_165_924_254_628e-19_f64.to_bits());
    assert!(fabs(tail) <= f64::EPSILON * head);
    assert_eq!(quadrant, 1);
}

/// Every exponent a double can carry past the moderate reduction, several
/// mantissas each, held to the host's libm: a wrong word of `2/PI` would
/// throw every angle whose reduction reads it off by a whole remainder.
#[test]
fn angles_past_a_million_quarter_turns_track_a_correctly_rounded_libm() {
    let stride = 0x0000_28f5_c28f_5c29_u64;
    let mut bits = REDUCIBLE.to_bits() + 1;
    while bits < f64::INFINITY.to_bits() {
        for x in [f64::from_bits(bits), -f64::from_bits(bits)] {
            assert!(ulps_apart(sin(x), f64::sin(x)) <= 1, "sin({x:e})");
            assert!(ulps_apart(cos(x), f64::cos(x)) <= 1, "cos({x:e})");
            assert!(ulps_apart(tan(x), f64::tan(x)) <= 3, "tan({x:e})");
        }
        bits += stride;
    }
}

/// Sine is odd and cosine even however large the angle, to the last bit.
#[test]
fn a_large_angle_and_its_negation_share_a_reduction() {
    for x in [REDUCIBLE.next_up(), 2.0e6, 7.3e19, 1e200, f64::MAX] {
        assert_eq!(sin(-x).to_bits(), (-sin(x)).to_bits(), "sin(-{x:e})");
        assert_eq!(cos(-x).to_bits(), cos(x).to_bits(), "cos(-{x:e})");
    }
}

/// The oddness the kernels would lose on their own: the sign of a zero angle
/// is kept, so a rotation by `-0` reflects nothing.
#[test]
fn a_negative_zero_angle_keeps_its_sign() {
    assert_eq!(sin(-0.0).to_bits(), (-0.0_f64).to_bits());
    assert_eq!(atan(-0.0).to_bits(), (-0.0_f64).to_bits());
    assert_eq!(cos(-0.0).to_bits(), 1.0_f64.to_bits());
}

/// A `NaN`, an infinity or an angle past any meaningful turn still answers a
/// finite value: a degenerate transform draws, rather than erasing a shape.
/// An infinite or `NaN` angle has no direction, so it turns nothing.
#[allow(
    clippy::float_cmp,
    reason = "a direction-less angle answers exactly the zero angle's values"
)]
#[test]
fn angles_are_total() {
    for x in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!((sin(x), cos(x), tan(x)), (0.0, 1.0, 0.0), "angle {x}");
    }
    for x in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        1e300,
        -1e300,
        1e17,
    ] {
        for (name, f) in [("sin", sin as fn(f64) -> f64), ("cos", cos), ("tan", tan)] {
            assert!(f(x).is_finite(), "{name}({x}) = {}", f(x));
        }
        assert!(atan(x).is_finite(), "atan({x})");
        assert!(
            atan2(x, 1.0).is_finite() && atan2(1.0, x).is_finite(),
            "atan2 over {x}"
        );
    }
    close(atan(f64::NAN), 0.0);
    close(atan(f64::INFINITY), FRAC_PI_2);
    close(atan(f64::NEG_INFINITY), -FRAC_PI_2);
    close(sin(f64::NAN), 0.0);
    close(cos(f64::NAN), 1.0);
}

#[test]
fn arccosine_matches_the_reference_and_clamps_its_domain() {
    close(acos(1.0), 0.0);
    close(acos(0.5), FRAC_PI_3);
    close(acos(-0.25), 1.823_476_581_936_975_4);
    close(acos(-1.0), PI);
    // An arc conversion can overshoot the domain by a rounding step; the
    // endpoint is the honest answer there, never a `NaN`.
    close(acos(1.000_000_1), 0.0);
    close(acos(-1.000_000_1), PI);
    same(acos(f64::NAN), PI);
}

#[test]
fn arcsine_matches_the_reference_and_clamps_its_domain() {
    close(asin(0.0), 0.0);
    close(asin(1.0), FRAC_PI_2);
    close(asin(-1.0), -FRAC_PI_2);
    close(asin(0.5), PI / 6.0);
    close(asin(-0.25), -0.252_680_255_142_078_64);
    close(asin(1.000_000_1), FRAC_PI_2);
    close(asin(-1.000_000_1), -FRAC_PI_2);
    same(asin(f64::NAN), -FRAC_PI_2);
}

/// The two inverses answer the same triangle, so their sum is the right angle
/// between the sides they each name.
#[test]
fn arcsine_and_arccosine_are_complementary() {
    let mut x = -1.0;
    while x <= 1.0 {
        close(asin(x) + acos(x), FRAC_PI_2);
        x += 0.05;
    }
}

/// Round-tripping an angle through its own tangent is the sharpest check that
/// the two reductions agree with one another.
#[test]
fn arctangent_inverts_the_tangent() {
    let mut angle = -1.5;
    while angle < 1.5 {
        close(atan(tan(angle)), angle);
        angle += 0.13;
    }
}

#[test]
fn exponential_matches_the_reference_across_its_reduction() {
    close(exp(0.0), 1.0);
    close(exp(1.0), core::f64::consts::E);
    close(exp(-1.0), 0.367_879_441_171_442_33);
    close(exp(0.25), 1.284_025_416_687_741_4);
    close(exp(-0.25), 0.778_800_783_071_404_9);
    // Either side of the half-`ln(2)` fold, where `k` steps.
    close(exp(0.34), 1.404_947_591_288_49);
    close(exp(0.35), 1.419_067_548_593_257);
    close(exp(10.0), 22_026.465_794_806_718);
    close(exp(-10.0), 4.539_992_976_248_485_e-5);
}

/// A gain curve is the consumer, so the relative error is what matters over
/// the whole range rather than the absolute one a fixed epsilon measures.
#[test]
fn exponential_is_accurate_relative_to_its_own_magnitude() {
    let mut x = -300.0;
    while x < 300.0 {
        let got = exp(x);
        let halved = exp(x / 2.0);
        // `exp(x)` and `exp(x/2)^2` are computed through different reductions,
        // so agreement between them is a check on both.
        assert!(
            fabs(got - halved * halved) <= 1e-12 * got,
            "exp({x}) = {got} disagrees with exp({})^2 = {}",
            x / 2.0,
            halved * halved
        );
        x += 7.3;
    }
}

/// The evaluated range is every argument whose exponential rounds to a
/// nonzero double, the subnormals included, and its two ends are pinned
/// exactly. The references are the correctly rounded values, worked out in
/// exact rational arithmetic.
#[allow(
    clippy::float_cmp,
    reason = "the saturation endpoints are exact values, which is what is pinned"
)]
#[test]
fn exponential_reaches_both_ends_of_the_double_range() {
    let references = [
        (709.0, 8.218_407_461_554_972e307),
        (709.5, 1.354_986_319_314_632_8e308),
        (709.7, 1.654_984_027_680_264_4e308),
        (EXP_MAX_ARG, 1.797_693_134_862_273_2e308),
        (-708.0, 3.307_553_003_638_408e-308),
        (-708.2, 2.707_995_361_514_091_3e-308),
        (-708.396_418_532_264_1, 2.225_073_858_507_262_6e-308),
        // Past `ln(f64::MIN_POSITIVE)` the results are subnormal.
        (-709.0, 1.216_780_750_623_423e-308),
        (-720.0, 2.032_230_802_4e-313),
        (-740.0, 4.2e-322),
    ];
    for (x, expected) in references {
        assert!(ulps_apart(exp(x), expected) <= 1, "exp({x}) = {:e}", exp(x));
    }
    // Among the least subnormals one ulp is the whole answer, so these are
    // exact: the threshold, fdlibm's, is where the true value passes half the
    // least subnormal.
    same(exp(-744.0), 1e-323);
    same(exp(-745.133_219_101_941_1), 5e-324);
    same(exp(-745.133_219_101_941_2), 0.0);
    assert!(exp(EXP_MAX_ARG) < f64::MAX);
    assert_eq!(exp(EXP_MAX_ARG.next_up()), f64::MAX);
    assert_eq!(exp(EXP_MIN_ARG.next_down()), 0.0);
}

/// Total like the rest of the module: the answer saturates rather than
/// becoming an infinity or a `NaN` a caller would have to guard against.
#[allow(
    clippy::float_cmp,
    reason = "the saturation endpoints are exact values, so a tolerance here \
              would accept the infinity the function exists to avoid"
)]
#[test]
fn exponential_saturates_instead_of_overflowing() {
    assert_eq!(exp(1e9), f64::MAX);
    assert_eq!(exp(f64::INFINITY), f64::MAX);
    assert_eq!(exp(-1e9), 0.0);
    assert_eq!(exp(f64::NEG_INFINITY), 0.0);
    assert_eq!(exp(f64::NAN), 0.0);
    // Everything it does answer is finite and non-negative.
    let mut x = -750.0;
    while x < 750.0 {
        let got = exp(x);
        assert!(got.is_finite() && got >= 0.0, "exp({x}) = {got}");
        x += 11.0;
    }
}

/// The references are the correctly rounded values: either side of the `√2`
/// fold, next to one where the short series takes over, at the subnormal
/// floor and at both ends of the normal range.
#[allow(
    clippy::float_cmp,
    reason = "the logarithm of one is exactly zero, which is what is pinned"
)]
#[test]
fn logarithm_matches_the_reference_across_its_reduction() {
    assert_eq!(ln(1.0), 0.0);
    let references = [
        (core::f64::consts::E, 1.0),
        (2.0, core::f64::consts::LN_2),
        (10.0, core::f64::consts::LN_10),
        (0.5, -core::f64::consts::LN_2),
        (1.5, 0.405_465_108_108_164_4),
        (SQRT_2, 0.346_573_590_279_972_7),
        (core::f64::consts::FRAC_1_SQRT_2, -0.346_573_590_279_972_6),
        (1.000_000_1, 9.999_999_505_838_704e-8),
        (0.999_999_9, -1.000_000_049_473_647_4e-7),
        (5e-324, -744.440_071_921_381_2),
        (f64::MIN_POSITIVE, -708.396_418_532_264_1),
        (f64::MAX, 709.782_712_893_384),
    ];
    for (x, expected) in references {
        assert!(ulps_apart(ln(x), expected) <= 1, "ln({x}) = {:e}", ln(x));
    }
}

/// Every double from the subnormals to the largest, a few hundred a binade,
/// against the host's own logarithm.
#[test]
fn logarithm_agrees_with_the_host_to_within_an_ulp() {
    let mut x = 1e-310_f64;
    while x < 1e308 {
        let host = std::primitive::f64::ln(x);
        assert!(
            ulps_apart(ln(x), host) <= 1,
            "ln({x:e}) = {:e}, host {host:e}",
            ln(x)
        );
        x *= 1.003_7;
    }
}

#[test]
fn logarithm_inverts_the_exponential() {
    let mut x = -700.0;
    while x < 700.0 {
        assert!(
            fabs(ln(exp(x)) - x) <= 1e-12 * fmax(fabs(x), 1.0),
            "ln(exp({x}))"
        );
        x += 3.7;
    }
}

/// Total like the rest of the module: the answer saturates rather than
/// becoming an infinity or a `NaN` a caller would have to guard against.
#[allow(
    clippy::float_cmp,
    reason = "the saturation endpoints are exact values, which is what is pinned"
)]
#[test]
fn logarithm_saturates_instead_of_diverging() {
    for x in [0.0, -0.0, -1.0, f64::NEG_INFINITY, f64::NAN] {
        assert_eq!(ln(x), f64::MIN, "ln({x})");
    }
    assert_eq!(ln(f64::INFINITY), f64::MAX);
}

#[test]
fn a_power_of_a_positive_base_is_accurate_and_nought_below_it() {
    for (x, p, expected) in [
        (2.0_f64, 3.0_f64, 8.0),
        (0.5, 0.25, 0.840_896_415_253_714_5),
        (9.0, 0.5, 3.0),
        (1e-3, 2.0 / 3.0, 0.010_000_000_000_000_002),
    ] {
        assert!(
            (power(x, p) - expected).abs() <= 4.0 * f64::EPSILON * expected,
            "power({x}, {p}) = {:e}",
            power(x, p)
        );
    }
    assert!(power(0.0, 2.0).abs() < f64::EPSILON);
    assert!(power(-4.0, 0.5).abs() < f64::EPSILON);
    for (x, p, expected) in [
        (f64::NAN, 2.0, 0.0),
        (2.0, f64::NAN, 0.0),
        (-0.0, 3.0, 0.0),
        (5.0, 0.0, 1.0),
        (1.0, f64::INFINITY, 1.0),
        (10.0, 400.0, f64::MAX),
        (10.0, -400.0, 0.0),
        (0.5, f64::INFINITY, 0.0),
        (0.5, f64::NEG_INFINITY, f64::MAX),
        (f64::INFINITY, 0.5, f64::MAX),
        (f64::INFINITY, -0.5, 0.0),
        (1.0 + f64::EPSILON, 1e300, f64::MAX),
        (5e-324, -1.0, f64::MAX),
        (1e200, 2.0, f64::MAX),
        (2.0, 1024.0, f64::MAX),
        (2.0, -1075.0, 0.0),
        (2.0, -1074.0, 5e-324),
    ] {
        same(power(x, p), expected);
    }
    same(power(3.7, 1.0), 3.7);
    same(power(3.7, -1.0), 1.0 / 3.7);
    same(power(3.7, 2.0), 3.7 * 3.7);
    same(power(3.7, 0.5), sqrt(3.7));
}

/// fdlibm's promise for its `pow`: an integer to an integer power is exact
/// wherever a double holds the result, and so is a half to one.
#[test]
fn an_integer_power_is_exact_wherever_a_double_holds_it() {
    for base in 2_u32..=40 {
        let odd = u64::from(base >> base.trailing_zeros());
        let base = f64::from(base);
        // Each product is exact while its odd part fits in 53 bits.
        let (mut whole, mut odd_part, mut p) = (base, odd, 1.0);
        while odd_part < 1 << 53 && whole.is_finite() {
            same(power(base, p), whole);
            whole *= base;
            odd_part = odd_part.saturating_mul(odd);
            p += 1.0;
        }
    }
    for p in 1..=1074 {
        let fraction = if p > 1022 {
            f64::from_bits(1 << (1074 - p))
        } else {
            f64::from_bits(u64::try_from(1023 - p).expect("normal") << 52)
        };
        same(power(0.5, f64::from(p)), fraction);
    }
}

/// A function, an argument, the correctly rounded value there and the
/// remainder past it, and the function's bound in ulps.
type Quoted = (fn(f64) -> f64, f64, (f64, f64), f64);

/// [`Quoted`] for a function of two arguments.
type QuotedPair = (fn(f64, f64) -> f64, (f64, f64), (f64, f64), f64);

/// Arguments where digits are hardest to keep — `log2` near one and `√2` and
/// the inverse sine and cosine near ±1, where leading digits cancel, `exp2`
/// where its fraction's product with `ln(2)` rounds, and the tangent beside
/// its pole — with `exp2` where its error nears an ulp, a subnormal `log2`,
/// the three main branches of each of the inverse sine and cosine, an `asinh`
/// whose rounded argument carries it past an ulp, and an `acosh` where a host
/// libm is an ulp and a half out.
const QUOTED: [Quoted; 26] = [
    (
        log2,
        0.99,
        (-0.014_499_569_695_115_089, -4.778_441_699_988_693e-19),
        1.0,
    ),
    (
        log2,
        0.999_999_999_999_999_9,
        (-1.601_713_251_907_458_8e-16, -1.115_118_401_354_741_9e-32),
        1.0,
    ),
    (
        log2,
        0.999_999_999_068_677_4,
        (-1.343_614_460_491_361_7e-9, 2.400_046_270_942_970_3e-26),
        1.0,
    ),
    (
        log2,
        0.999_999_999_999_090_5,
        (-1.312_123_495_963_187e-12, 5.430_074_273_467_878e-29),
        1.0,
    ),
    (
        log2,
        1.404_368_936_632_551_6,
        (0.489_921_990_578_100_93, -2.451_379_916_741_448_5e-17),
        1.0,
    ),
    (
        log2,
        3.0e-320,
        (-1_061.432_043_924_584_6, 7.092_051_981_930_708e-14),
        1.0,
    ),
    (
        exp2,
        37.997_790_639_275_21,
        (274_457_277_736.099_46, 6.061_240_414_292_837e-6),
        1.0,
    ),
    (
        exp2,
        0.516_509_162_737_210_5,
        (1.430_489_752_473_499, 5.249_176_838_100_378e-17),
        1.0,
    ),
    (
        acos,
        0.999_999_9,
        (0.000_447_213_599_109_041_26, -1.883_034_288_214_827_2e-20),
        1.0,
    ),
    (
        acos,
        0.999_999_999_068_677_4,
        (4.315_837_287_850_502e-5, 2.135_903_439_481_360_7e-22),
        1.0,
    ),
    (
        acos,
        0.999,
        (0.044_725_087_168_733_454, -3.353_025_118_183_681_8e-18),
        1.0,
    ),
    (
        acos,
        -0.999_999_9,
        (3.141_145_439_990_684, 1.222_014_931_584_166_5e-17),
        1.0,
    ),
    (
        acos,
        0.500_000_1,
        (1.047_197_435_726_540_1, 4.352_612_786_859_431e-18),
        1.0,
    ),
    (
        acos,
        -0.500_000_1,
        (2.094_395_217_863_253_2, -1.039_325_377_971_554_2e-16),
        1.0,
    ),
    (
        acos,
        0.3,
        (1.266_103_672_779_499_2, -7.783_137_368_524_88e-17),
        1.0,
    ),
    (
        asin,
        0.999_999_9,
        (1.570_349_113_195_787_6, -4.901_219_064_152_599_5e-17),
        1.0,
    ),
    (
        asin,
        0.999_999_999_068_677_4,
        (1.570_753_168_422_018_2, -6.136_403_428_677_47e-17),
        1.0,
    ),
    (
        asin,
        0.97,
        (1.325_230_809_279_604_6, -6.967_089_538_754_512e-17),
        1.0,
    ),
    (
        asin,
        0.98,
        (1.370_461_484_471_776_8, 8.990_356_958_594_073e-17),
        1.0,
    ),
    (
        asin,
        0.500_000_1,
        (0.523_598_891_068_356_6, -5.414_257_529_200_742e-17),
        1.0,
    ),
    (
        asin,
        -0.3,
        (-0.304_692_654_015_397_5, 2.746_974_005_115_701_7e-17),
        1.0,
    ),
    (
        tan,
        FRAC_PI_2,
        (1.633_123_935_319_537e16, -0.244_032_262_958_471_08),
        3.0,
    ),
    (
        tan,
        1.570_796_326_794_896_3,
        (3_530_114_321_217_157.5, 0.115_752_150_702_472_27),
        3.0,
    ),
    (
        tan,
        5.319_372_648_326_541e255,
        (-2.133_485_385_753_704e18, 92.325_147_336_655_52),
        3.0,
    ),
    (
        asinh,
        -0.491_629_975_921_862,
        (-0.473_712_944_027_579_63, -9.452_051_695_522_863e-18),
        2.0,
    ),
    (
        acosh,
        1.401_579_999_999_965,
        (0.868_625_452_983_623_8, -5.122_878_494_758_94e-17),
        1.0,
    ),
];

/// Powers an earlier form lost digits at, its logarithm's rounding magnified,
/// a mild power beside them, and `atan2` across the negative axis.
const QUOTED_PAIRS: [QuotedPair; 9] = [
    (
        power,
        (10.0, 300.0),
        (1.0e300, -5.250_476_025_520_442e283),
        1.0,
    ),
    (power, (10.0, -300.0), (1.0e-300, -2.505_909_4e-317), 1.0),
    (
        power,
        (2.0, 1_000.0),
        (1.071_508_607_186_267_3e301, 0.0),
        1.0,
    ),
    (
        power,
        (0.999_999_911_242_88, 7_809_585_272.272_457),
        (9.247_657_377_920_838e-302, 4.320_446e-318),
        1.0,
    ),
    (
        power,
        (1.000_000_000_208_563_4, 1_248_177_917_816.278),
        (1.141_223_356_390_988e113, 4.278_435_624_377_224e96),
        1.0,
    ),
    (
        power,
        (1.910_550_484_765_584_8e-230, -0.078_030_645_864_640_7),
        (8.416_074_439_237_999e17, -26.697_379_977_713_513),
        1.0,
    ),
    (
        power,
        (7.0, 0.333_333_333_333_333_3),
        (1.912_931_182_772_389, 9.400_309_345_900_771e-17),
        1.0,
    ),
    (
        atan2,
        (1.0, -3.0),
        (2.819_842_099_193_151, 5.903_613_615_775_535e-17),
        2.0,
    ),
    (
        atan2,
        (-0.373_690_681_438_032, -0.153_115_302_035_608_66),
        (-1.959_669_309_194_205_2, 4.997_867_166_613_957e-17),
        2.0,
    ),
];

/// Just under a power of two the doubles are half as far apart as above it,
/// so an answer there is measured in the spacing below, however small.
#[test]
fn ulps_are_counted_in_the_spacing_at_the_truth() {
    let shortfall = 6.931_471_805_599_453e-18;
    let below_one = f64::from_bits(1.0_f64.to_bits() - 1);
    let near_one = ulps_from_true(below_one, (1.0, -shortfall));
    assert!((near_one - 0.9375).abs() < 1e-3, "{near_one}");
    // At 2^-600 a product of the two parts underflows to zero.
    let tiny = f64::from_bits((1023 - 600) << 52);
    let below_tiny = f64::from_bits(tiny.to_bits() - 1);
    let near_tiny = ulps_from_true(below_tiny, (tiny, -tiny * shortfall));
    assert!((near_tiny - 0.9375).abs() < 1e-3, "{near_tiny}");
    let above_one = f64::from_bits(1.0_f64.to_bits() + 1);
    let above = ulps_from_true(above_one, (1.0, shortfall));
    assert!((above - 0.968_75).abs() < 1e-3, "{above}");
}

#[test]
fn each_function_meets_its_bound_at_quoted_values() {
    for (function, x, truth, bound) in QUOTED {
        let apart = ulps_from_true(function(x), truth);
        assert!(apart < bound, "{apart} ulps from the truth at {x:e}");
    }
    for (function, (a, b), truth, bound) in QUOTED_PAIRS {
        let apart = ulps_from_true(function(a, b), truth);
        assert!(
            apart < bound,
            "{apart} ulps from the truth at ({a:e}, {b:e})"
        );
    }
}

/// `atan2` with `x < 0` is measured from `PI`, and `PI`'s own rounding put
/// back: a hair below a half-turn rounds to `PI`, not the double under it.
#[test]
fn an_angle_a_hair_short_of_a_half_turn_rounds_to_pi() {
    same(atan2(2.5e-16, -1.0), PI);
    same(atan2(-2.5e-16, -1.0), -PI);
}

#[test]
fn the_hyperbolic_tangent_is_odd_bounded_and_accurate() {
    for (x, expected) in [
        (0.0_f64, 0.0),
        (0.25, 0.244_918_662_403_709_13),
        (1.5, 0.905_148_253_644_866_4),
        (20.0, 1.0),
    ] {
        for (x, expected) in [(x, expected), (-x, -expected)] {
            assert!(
                (tanh(x) - expected).abs() <= 4.0 * f64::EPSILON,
                "tanh({x}) = {:e}",
                tanh(x)
            );
            assert!(tanh(x).abs() <= 1.0);
        }
        assert!((tanh(-x) + tanh(x)).abs() < f64::EPSILON);
    }
}

#[test]
fn a_count_is_exact_until_it_saturates() {
    assert!((real(0)).abs() < f64::EPSILON);
    assert!((real(1_048_577) - 1_048_577.0).abs() < f64::EPSILON);
    assert!((real(usize::MAX) - f64::from(u32::MAX)).abs() < f64::EPSILON);
}

#[test]
fn a_share_is_held_to_whole_and_whole_with_nothing_to_do() {
    assert!((share(1, 4) - 0.25).abs() < f64::EPSILON);
    assert!((share(9, 4) - 1.0).abs() < f64::EPSILON);
    assert!((share(0, 0) - 1.0).abs() < f64::EPSILON);
}

#[test]
fn a_byte_rounds_a_share_and_holds_it_to_the_unit() {
    assert_eq!(byte(0.0), 0);
    assert_eq!(byte(1.0), 255);
    assert_eq!(byte(0.5), 128);
    assert_eq!(byte(-3.0), 0);
    assert_eq!(byte(7.0), 255);
    assert_eq!(byte(f64::NAN), 0);
}

#[test]
fn a_half_rounds_to_nearest_even() {
    assert_eq!(to_half(1.0), 0x3c00);
    assert_eq!(to_half(-2.0), 0xc000);
    assert_eq!(to_half(65504.0), 0x7bff);
    assert_eq!(
        to_half(65520.0),
        0x7c00,
        "a tie past the largest half rounds to infinity"
    );
    assert_eq!(to_half(f32::INFINITY), 0x7c00);
    assert_eq!(to_half(f32::NAN) & 0x7e00, 0x7e00);
    assert_eq!(to_half(5.960_464_5e-8), 0x0001, "the smallest subnormal");
    assert_eq!(
        to_half(2.980_232_2e-8),
        0x0000,
        "half of it ties to even zero"
    );
    assert_eq!(to_half(6.103_515_6e-5), 0x0400, "the smallest normal");
    assert_eq!(to_half(1.000_488_3), 0x3c00, "a tie below rounds to even");
    assert_eq!(to_half(1.001_464_8), 0x3c02, "a tie above rounds to even");
    assert_eq!(to_half(-0.0), 0x8000);
}

#[test]
fn a_double_rounds_to_a_half_once() {
    for magnitude in 0..0x7bff_u16 {
        for sign in [0, 0x8000] {
            let (low, high) = (magnitude | sign, (magnitude + 1) | sign);
            let exact = f64::from(from_half(low));
            assert_eq!(double_to_half(exact), low, "{low:#06x} is a half");
            let tie = f64::midpoint(exact, f64::from(from_half(high)));
            let even = if low & 1 == 0 { low } else { high };
            assert_eq!(double_to_half(tie), even, "the tie above {low:#06x}");
            let past = f64::from_bits(tie.to_bits() + 1);
            assert_eq!(
                double_to_half(past),
                high,
                "just past the tie above {low:#06x}"
            );
            let short = f64::from_bits(tie.to_bits() - 1);
            assert_eq!(
                double_to_half(short),
                low,
                "just short of the tie above {low:#06x}"
            );
        }
    }
    assert_eq!(
        double_to_half(65_520.0),
        0x7c00,
        "a tie past the largest half"
    );
    assert_eq!(double_to_half(65_519.999), 0x7bff);
    assert_eq!(double_to_half(1e300), 0x7c00);
    assert_eq!(double_to_half(-1e300), 0xfc00);
    assert_eq!(double_to_half(1e-300), 0x0000);
    assert_eq!(double_to_half(-1e-300), 0x8000);
    assert_eq!(double_to_half(f64::NAN) & 0x7e00, 0x7e00);
}

#[test]
fn every_half_survives_a_round_trip_through_a_single() {
    for bits in 0..=u16::MAX {
        let single = from_half(bits);
        if single.is_nan() {
            assert_eq!(bits & 0x7c00, 0x7c00);
            assert_ne!(bits & 0x3ff, 0);
            continue;
        }
        assert_eq!(to_half(single), bits, "{bits:#06x} came back changed");
    }
}

#[test]
fn a_half_reads_as_the_value_it_encodes() {
    let exactly = |bits: u16, value: f32| assert_eq!(from_half(bits).to_bits(), value.to_bits());
    exactly(0x3c00, 1.0);
    exactly(0xc000, -2.0);
    exactly(0x0001, 5.960_464_5e-8);
    exactly(0x03ff, 6.097_555e-5);
    exactly(0x7bff, 65504.0);
    exactly(0x7c00, f32::INFINITY);
}

#[test]
fn trunc_and_round_even_round_as_they_say() {
    same(trunc(-2.7), -2.0);
    same(trunc(2.7), 2.0);
    same(trunc(f64::NAN), 0.0);
    same(round_even(2.5), 2.0);
    same(round_even(3.5), 4.0);
    same(round_even(-2.5), -2.0);
    same(round_even(2.500_000_000_000_001), 3.0);
    same(round_even(f64::NAN), 0.0);
}

#[test]
fn powers_of_two_are_exact_both_ways() {
    for k in -1074..=1023 {
        let power = f64::from_bits(if k < -1022 {
            1 << (k + 1074)
        } else {
            u64::try_from(k + 1023).expect("normal") << 52
        });
        assert_eq!(exp2(f64::from(k)).to_bits(), power.to_bits(), "2^{k}");
        same(log2(power), f64::from(k));
    }
    same(exp2(1024.0), f64::MAX);
    same(exp2(-2000.0), 0.0);
    same(exp2(f64::NAN), 0.0);
    same(log2(0.0), f64::MIN);
    same(log2(f64::INFINITY), f64::MAX);
    close(exp2(0.5), SQRT_2);
    close(log2(10.0), core::f64::consts::LOG2_10);
}

#[test]
fn the_hyperbolic_functions_invert_each_other() {
    for &x in &[-20.0, -3.5, -1.0, -0.25, 0.0, 1e-6, 0.5, 2.0, 7.25] {
        close(asinh(sinh(x)), x);
        if x >= 0.0 {
            close(acosh(cosh(x)), x);
        }
        if x.abs() < 3.0 {
            close(atanh(tanh(x)), x);
        }
    }
    close(sinh(1.0), 1.175_201_193_643_801_4);
    close(cosh(1.0), 1.543_080_634_815_243_7);
    close(asinh(1e300), 691.468_675_078_773_7);
    close(acosh(1e300), 691.468_675_078_773_7);
    same(acosh(0.5), 0.0);
    same(atanh(1.0), f64::MAX);
    same(atanh(-1.0), f64::MIN);
    same(asinh(-0.0), -0.0);
}

#[test]
fn the_hyperbolic_functions_saturate_and_keep_a_zeros_sign() {
    for zero_of_its_sign in [sinh, tanh, asinh, atanh] {
        same(zero_of_its_sign(-0.0), -0.0);
        same(zero_of_its_sign(0.0), 0.0);
    }
    same(cosh(-0.0), 1.0);
    same(acosh(1.0), 0.0);
    // Finite past e^709.78, where the result still fits a double.
    for big in [sinh(710.0), cosh(-710.0)] {
        assert!(big > f64::MAX * 0.5 && big < f64::MAX, "{big:e}");
    }
    for x in [710.5, 1e6, f64::INFINITY] {
        same(sinh(x), f64::MAX);
        same(sinh(-x), f64::MIN);
        same(cosh(x), f64::MAX);
        same(cosh(-x), f64::MAX);
        same(tanh(x), 1.0);
        same(tanh(-x), -1.0);
    }
    for function in [sinh, cosh, tanh, asinh, acosh, atanh] {
        same(function(f64::NAN), 0.0);
    }
    close(sinh(-2.0), -3.626_860_407_847_019);
    close(cosh(-2.0), 3.762_195_691_083_631_4);
}

/// A float read at its width is exact, and one written at a width is rounded
/// once to nearest, every `NaN` as that width's one quiet `NaN` whatever its
/// sign and payload.
#[test]
fn a_float_of_a_width_is_read_exactly_and_written_rounded_once() {
    assert_eq!(float_of(16, 0x3e00).to_bits(), 1.5f64.to_bits());
    assert_eq!(
        float_of(32, 0xdead_0000_3fc0_0000).to_bits(),
        1.5f64.to_bits()
    );
    assert_eq!(float_of(64, 1.5f64.to_bits()).to_bits(), 1.5f64.to_bits());
    assert_eq!(float_bits(16, 1.000_488_281_250_000_2), 0x3c01);
    assert_eq!(float_bits(32, 0.1), u64::from(0.1f32.to_bits()));
    assert_eq!(float_bits(64, 0.1), 0.1f64.to_bits());
    for nan in [0x7fc0_2000, 0xffc0_0000, 0x7f80_0001] {
        let value = float_of(32, nan);
        assert_eq!(float_bits(16, value), 0x7e00, "{nan:#x} as a half");
        assert_eq!(float_bits(32, value), 0x7fc0_0000, "{nan:#x} as a single");
        assert_eq!(
            float_bits(64, value),
            0x7ff8_0000_0000_0000,
            "{nan:#x} as a double"
        );
    }
}

/// A single quantized to a half keeps what a normal half holds, rounds the
/// rest to nearest, flushes what no normal half holds to zero of its sign,
/// and keeps an infinity.
#[test]
fn a_single_quantized_to_a_half_flushes_what_no_normal_half_holds() {
    let quantized = |x: f32| f32::from_bits(quantized_to_half(x.to_bits()));
    assert_eq!(quantized(1.5).to_bits(), 1.5f32.to_bits());
    assert_eq!(quantized(1.000_7).to_bits(), from_half(0x3c01).to_bits());
    assert_eq!(quantized(1e-6).to_bits(), 0, "a half subnormal flushes");
    assert_eq!(quantized(-1e-6).to_bits(), (-0.0f32).to_bits());
    assert_eq!(quantized(1e6).to_bits(), f32::INFINITY.to_bits());
    assert!(quantized(f32::NAN).is_nan());
}
