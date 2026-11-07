//! Unit tests for curve flattening.

use alloc::vec;
use alloc::vec::Vec;

use core::f64::consts::PI;

use tairix_util::mathf::sqrt;

use super::{
    cubic_segments_wanted, flatten_cubic, flatten_ellipse_arc, flatten_quadratic, MAX_SEGMENTS,
};
use crate::cubic::CubicBez;
use crate::line::Line;
use crate::point::Point;
use crate::quad::QuadBez;

/// The flattening accuracy the tests ask for, in the same units as their
/// coordinates.
const TOL: f64 = 0.01;

fn p(x: f64, y: f64) -> Point {
    Point::new(x, y)
}

#[track_caller]
fn close_point(actual: Point, expected: Point) {
    assert!(
        actual.distance(expected) <= 1e-9,
        "{actual:?} vs {expected:?}"
    );
}

/// The flattened polyline must stay within the tolerance of the true curve,
/// which is the whole promise of an error-bounded subdivision.
#[test]
fn a_flattened_cubic_stays_within_tolerance_of_the_true_curve() {
    let curve = CubicBez::new(p(0.0, 0.0), p(0.0, 100.0), p(100.0, 100.0), p(100.0, 0.0));
    let mut flat = vec![curve.p0];
    flatten_cubic(curve, TOL, |point| flat.push(point));
    close_point(flat[flat.len() - 1], curve.p3);

    for step in 0..=200 {
        let exact = curve.eval(f64::from(step) / 200.0);
        let nearest = flat
            .windows(2)
            .map(|pair| Line::new(pair[0], pair[1]).nearest(exact).1)
            .fold(f64::MAX, |a, b| if b < a { b } else { a });
        assert!(
            sqrt(nearest) <= TOL * 1.5,
            "curve departs by {}",
            sqrt(nearest)
        );
    }
}

#[test]
fn a_tighter_tolerance_never_gives_fewer_points() {
    let curve = CubicBez::new(p(0.0, 0.0), p(0.0, 50.0), p(50.0, 50.0), p(50.0, 0.0));
    let (mut coarse, mut fine) = (0, 0);
    flatten_cubic(curve, 1.0, |_| coarse += 1);
    flatten_cubic(curve, 0.01, |_| fine += 1);
    assert!(fine >= coarse);
}

#[test]
fn a_straight_cubic_needs_only_one_segment() {
    let mut flat = Vec::new();
    let straight = CubicBez::new(p(0.0, 0.0), p(10.0, 0.0), p(20.0, 0.0), p(30.0, 0.0));
    flatten_cubic(straight, TOL, |point| flat.push(point));
    assert_eq!(flat.len(), 1);
}

#[test]
fn a_quadratic_passes_through_its_endpoints() {
    let mut flat = Vec::new();
    let curve = QuadBez::new(p(0.0, 0.0), p(50.0, 100.0), p(100.0, 0.0));
    flatten_quadratic(curve, TOL, |point| flat.push(point));
    close_point(flat[flat.len() - 1], p(100.0, 0.0));
    let mid = flat[flat.len() / 2 - 1];
    assert!(mid.y > 40.0 && mid.y < 50.0, "midpoint height {}", mid.y);
}

/// A whole turn is a legitimate sweep, which is what draws a circle.
#[test]
fn a_full_sweep_closes_on_its_own_start() {
    let mut points = Vec::new();
    flatten_ellipse_arc(
        p(0.0, 0.0),
        p(10.0, 10.0),
        0.0,
        0.0,
        2.0 * PI,
        TOL,
        |point| {
            points.push(point);
        },
    );
    close_point(points[points.len() - 1], p(10.0, 0.0));
    assert!(points.len() > 8);
}

/// However hostile the input, one curve can never ask for an unbounded
/// subdivision.
#[test]
fn no_single_curve_exceeds_the_segment_bound() {
    let mut count = 0_usize;
    let hostile = CubicBez::new(p(0.0, 0.0), p(1e12, 1e12), p(-1e12, 1e12), p(1.0, 0.0));
    flatten_cubic(hostile, 1e-9, |_| count += 1);
    assert!(count <= MAX_SEGMENTS as usize);
    assert!(cubic_segments_wanted(hostile, 1e-9) > f64::from(MAX_SEGMENTS));

    let mut arc = 0_usize;
    flatten_ellipse_arc(p(0.0, 0.0), p(1e12, 1e12), 0.0, 0.0, 1e6, 1e-12, |_| {
        arc += 1;
    });
    assert!(arc <= MAX_SEGMENTS as usize);
}

/// A degenerate tolerance must not ask for an endless subdivision.
#[test]
fn a_degenerate_tolerance_is_floored() {
    let curve = QuadBez::new(p(0.0, 0.0), p(10.0, 10.0), p(20.0, 0.0));
    for tolerance in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let mut count = 0_usize;
        flatten_quadratic(curve, tolerance, |_| count += 1);
        assert!(count > 0 && count <= MAX_SEGMENTS as usize);
    }
}
