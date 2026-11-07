//! Flattening curves to polylines within a tolerance.
//!
//! Subdivision is *error bounded*, never a fixed segment count: the number of
//! segments is computed from the curve's own second derivative and the
//! caller's `tolerance`, so a long sweeping curve gets more segments than a
//! tiny one and neither is over-tessellated. The bound used is the standard
//! one — a chord over a parameter step `h` departs from the curve by at most
//! `h²·max|C''|/8` — solved for the step that keeps that below `tolerance`.
//!
//! This is the one flattener: the SVG decoder, the icon symbols and the
//! drawing editor draw through it, so every consumer lands on the same bits.

use tairix_util::mathf::{ceil, cos, fmax, hypot, sin, sqrt};

use crate::cubic::CubicBez;
use crate::point::Point;
use crate::quad::QuadBez;

/// The most segments any single curve or arc may be flattened into.
///
/// A fixed safety bound, not an accuracy knob: the error-bounded formulas
/// below already choose the segment count, and this is only what stops a
/// hostile radius or coordinate from asking for an unbounded one. A caller
/// drawing a curve far larger than an asset splits it first.
pub const MAX_SEGMENTS: u32 = 256;

/// The smallest flattening tolerance honoured.
///
/// A tolerance of zero (or a negative or non-finite one) has no finite
/// segment count that satisfies it, so it is floored here rather than
/// allowed to ask for an unbounded subdivision.
pub const MIN_TOLERANCE: f64 = 1e-4;

/// `tolerance` floored to a value a finite subdivision can satisfy.
#[must_use]
pub fn clamp_tolerance(tolerance: f64) -> f64 {
    if tolerance.is_finite() && tolerance > MIN_TOLERANCE {
        tolerance
    } else {
        MIN_TOLERANCE
    }
}

/// The number of uniform segments that keeps a curve whose greatest second
/// derivative is `curvature` within `tolerance` of its chords.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is positive and finite here, and a float-to-integer \
              cast saturates rather than wrapping, so the following clamp \
              still lands inside the segment bound"
)]
fn segments_for(curvature: f64, tolerance: f64) -> u32 {
    if !curvature.is_finite() || curvature <= 0.0 {
        return 1;
    }
    let count = ceil(sqrt(curvature / (8.0 * tolerance)));
    if count.is_finite() && count >= 1.0 {
        (count as u32).clamp(1, MAX_SEGMENTS)
    } else {
        1
    }
}

/// The number of segments [`flatten_cubic`] draws `curve` in at
/// `tolerance`, before the [`MAX_SEGMENTS`] bound: what a caller asks to
/// decide whether to split a curve first.
#[must_use]
pub fn cubic_segments_wanted(curve: CubicBez, tolerance: f64) -> f64 {
    let curvature = 6.0 * cubic_curvature(curve);
    let wanted = ceil(sqrt(curvature / (8.0 * clamp_tolerance(tolerance))));
    if wanted.is_finite() {
        wanted
    } else {
        f64::INFINITY
    }
}

/// The larger second difference of a cubic's control points, which bounds
/// its curvature over the whole span.
fn cubic_curvature(curve: CubicBez) -> f64 {
    let first = second_difference(curve.p0, curve.p1, curve.p2);
    let second = second_difference(curve.p1, curve.p2, curve.p3);
    fmax(first, second)
}

/// Flatten a cubic Bézier, handing `emit` the points *after* its start.
pub fn flatten_cubic(curve: CubicBez, tolerance: f64, mut emit: impl FnMut(Point)) {
    let tolerance = clamp_tolerance(tolerance);
    let n = segments_for(6.0 * cubic_curvature(curve), tolerance);
    for step in 1..=n {
        let t = f64::from(step) / f64::from(n);
        emit(curve.eval(t));
    }
}

/// Flatten a quadratic Bézier, handing `emit` the points *after* its start.
pub fn flatten_quadratic(curve: QuadBez, tolerance: f64, mut emit: impl FnMut(Point)) {
    let tolerance = clamp_tolerance(tolerance);
    let n = segments_for(
        2.0 * second_difference(curve.p0, curve.p1, curve.p2),
        tolerance,
    );
    for step in 1..=n {
        let t = f64::from(step) / f64::from(n);
        emit(curve.eval(t));
    }
}

/// Flatten an elliptical arc, handing `emit` the points *after* its start.
///
/// The arc runs `sweep_angle` radians from `start_angle`, measured in the
/// ellipse's own frame before `x_rotation_radians` turns it. A full turn is a
/// legitimate sweep, so this also draws whole circles and ellipses.
pub fn flatten_ellipse_arc(
    centre: Point,
    radii: Point,
    x_rotation_radians: f64,
    start_angle: f64,
    sweep_angle: f64,
    tolerance: f64,
    mut emit: impl FnMut(Point),
) {
    let tolerance = clamp_tolerance(tolerance);
    let bulge = fmax(radii.x.abs(), radii.y.abs());
    let span = sweep_angle.abs();
    // Angle is the parameter here, so the step that meets the tolerance
    // scales with the arc's length as well as its radius.
    let n = segments_for(bulge * span * span, tolerance);
    let (cos_r, sin_r) = (cos(x_rotation_radians), sin(x_rotation_radians));
    for step in 1..=n {
        let angle = start_angle + sweep_angle * f64::from(step) / f64::from(n);
        let (x, y) = (radii.x * cos(angle), radii.y * sin(angle));
        emit(Point::new(
            centre.x + cos_r * x - sin_r * y,
            centre.y + sin_r * x + cos_r * y,
        ));
    }
}

/// The magnitude of the second difference of three control points, which is
/// what bounds a Bézier's curvature.
fn second_difference(a: Point, b: Point, c: Point) -> f64 {
    hypot(a.x - 2.0 * b.x + c.x, a.y - 2.0 * b.y + c.y)
}

#[cfg(test)]
#[path = "flatten_tests.rs"]
mod tests;
