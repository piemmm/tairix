//! The real roots of quadratic and cubic polynomials.
//!
//! Segment bounds, line crossings and winding numbers all come down to where
//! a polynomial in the curve parameter vanishes, so these are written for the
//! coefficients Bézier curves produce: a leading coefficient that is tiny
//! beside the rest is treated as absent rather than divided by, the quadratic
//! takes the cancellation-free form, and every cubic root is polished by
//! Newton steps on the polynomial itself.

use core::f64::consts::TAU;

use tairix_inline::ArrayVec;
use tairix_util::mathf::{acos, cos, fabs, fmax, power, sqrt};

/// A leading coefficient this small relative to the polynomial's largest is
/// rounding noise, and dividing by it would throw a root to infinity.
const NEGLIGIBLE: f64 = 1e-12;

/// Newton steps spent polishing each cubic root.
const POLISH_STEPS: u32 = 2;

/// The real roots of `a·t² + b·t + c`, in ascending order, a double root
/// once.
#[allow(
    clippy::float_cmp,
    reason = "only an exactly repeated root is one root; any two distinct doubles are two"
)]
#[must_use]
pub fn quadratic(a: f64, b: f64, c: f64) -> ArrayVec<f64, 2> {
    let mut roots = ArrayVec::new();
    let scale = fmax(fmax(fabs(a), fabs(b)), fabs(c));
    if !scale.is_finite() || scale == 0.0 {
        return roots;
    }
    if fabs(a) <= NEGLIGIBLE * scale {
        if fabs(b) > NEGLIGIBLE * scale {
            push(&mut roots, -c / b);
        }
        return roots;
    }
    let discriminant = b * b - 4.0 * a * c;
    if discriminant < 0.0 {
        // A tangency rounds either side of zero; treat a hair below as one.
        if discriminant > -NEGLIGIBLE * b * b {
            push(&mut roots, -b / (2.0 * a));
        }
        return roots;
    }
    let q = -0.5 * (b + sign_of(b) * sqrt(discriminant));
    if q == 0.0 {
        push(&mut roots, 0.0);
        return roots;
    }
    let (first, second) = (q / a, c / q);
    let (low, high) = if first <= second {
        (first, second)
    } else {
        (second, first)
    };
    push(&mut roots, low);
    if high != low {
        push(&mut roots, high);
    }
    roots
}

/// The real roots of `a·t³ + b·t² + c·t + d`, in ascending order, a repeated
/// root once.
#[allow(
    clippy::many_single_char_names,
    reason = "the coefficients and Cardano's terms, named as the formulas write them"
)]
#[must_use]
pub fn cubic(a: f64, b: f64, c: f64, d: f64) -> ArrayVec<f64, 3> {
    let scale = fmax(fmax(fabs(a), fabs(b)), fmax(fabs(c), fabs(d)));
    if !scale.is_finite() || scale == 0.0 {
        return ArrayVec::new();
    }
    if fabs(a) <= NEGLIGIBLE * scale {
        let mut roots = ArrayVec::new();
        for root in quadratic(b, c, d) {
            push(&mut roots, root);
        }
        return roots;
    }
    let (b, c, d) = (b / a, c / a, d / a);
    // Depressed by t = x - b/3: x³ + p·x + q.
    let shift = b / 3.0;
    let p = c - b * shift;
    let q = 2.0 * shift * shift * shift - shift * c + d;
    let half_q = q / 2.0;
    let third_p = p / 3.0;
    let discriminant = half_q * half_q + third_p * third_p * third_p;

    let mut found: ArrayVec<f64, 3> = ArrayVec::new();
    if discriminant > 0.0 {
        let root = sqrt(discriminant);
        // Taking the larger-magnitude term first keeps the sum free of the
        // cancellation the textbook form suffers when p is small.
        let u = cube_root(-half_q - sign_of(half_q) * root);
        let v = if u == 0.0 { 0.0 } else { -third_p / u };
        push(&mut found, u + v - shift);
    } else if discriminant == 0.0 {
        let u = cube_root(-half_q);
        push(&mut found, 2.0 * u - shift);
        push(&mut found, -u - shift);
    } else {
        let r = sqrt(-third_p * third_p * third_p);
        let phi = acos(clamp_unit(-half_q / r));
        let m = 2.0 * sqrt(-third_p);
        push(&mut found, m * cos(phi / 3.0) - shift);
        push(&mut found, m * cos((phi + TAU) / 3.0) - shift);
        push(&mut found, m * cos((phi + 2.0 * TAU) / 3.0) - shift);
    }
    for root in &mut found {
        *root = polished(*root, (b, c, d));
    }
    found.sort_unstable_by(f64::total_cmp);
    let mut roots = ArrayVec::new();
    for root in found {
        if roots.last() != Some(&root) {
            push(&mut roots, root);
        }
    }
    roots
}

/// The roots of `a·t³ + b·t² + c·t + d` within `0..=1`, ascending; a root
/// a hair outside the interval is held to its end.
#[must_use]
pub fn cubic_in_unit(a: f64, b: f64, c: f64, d: f64) -> ArrayVec<f64, 3> {
    let mut within = ArrayVec::new();
    for root in cubic(a, b, c, d) {
        if let Some(t) = in_unit(root) {
            if within.last() != Some(&t) {
                push(&mut within, t);
            }
        }
    }
    within
}

/// The roots of `a·t² + b·t + c` within `0..=1`, ascending; a root a hair
/// outside the interval is held to its end.
#[must_use]
pub fn quadratic_in_unit(a: f64, b: f64, c: f64) -> ArrayVec<f64, 2> {
    let mut within = ArrayVec::new();
    for root in quadratic(a, b, c) {
        if let Some(t) = in_unit(root) {
            if within.last() != Some(&t) {
                push(&mut within, t);
            }
        }
    }
    within
}

/// How far outside `0..=1` a root may land by rounding and still count.
const UNIT_SLACK: f64 = 1e-9;

/// `t` held to `0..=1`, or `None` past the slack either side.
fn in_unit(t: f64) -> Option<f64> {
    if (-UNIT_SLACK..=1.0 + UNIT_SLACK).contains(&t) {
        Some(t.clamp(0.0, 1.0))
    } else {
        None
    }
}

/// `root` of the monic cubic `t³ + b·t² + c·t + d` after Newton steps, each
/// kept only where it improves the residual.
fn polished(mut root: f64, (b, c, d): (f64, f64, f64)) -> f64 {
    for _ in 0..POLISH_STEPS {
        let value = ((root + b) * root + c) * root + d;
        let slope = (3.0 * root + 2.0 * b) * root + c;
        if slope == 0.0 || !slope.is_finite() {
            break;
        }
        let next = root - value / slope;
        let next_value = ((next + b) * next + c) * next + d;
        if next.is_finite() && fabs(next_value) < fabs(value) {
            root = next;
        } else {
            break;
        }
    }
    root
}

/// The real cube root, odd in its argument.
fn cube_root(x: f64) -> f64 {
    if x < 0.0 {
        -power(-x, 1.0 / 3.0)
    } else {
        power(x, 1.0 / 3.0)
    }
}

/// `-1.0` for a negative `x`, else `1.0`.
fn sign_of(x: f64) -> f64 {
    if x < 0.0 {
        -1.0
    } else {
        1.0
    }
}

/// `x` held to `-1..=1`, where rounding can carry a cosine just past.
fn clamp_unit(x: f64) -> f64 {
    x.clamp(-1.0, 1.0)
}

/// Push `root` where it is finite; every caller sizes its array for the
/// polynomial's degree, so a full array is never met.
fn push<const N: usize>(roots: &mut ArrayVec<f64, N>, root: f64) {
    if root.is_finite() {
        let _ = roots.try_push(root);
    }
}

#[cfg(test)]
#[path = "roots_tests.rs"]
mod tests;
