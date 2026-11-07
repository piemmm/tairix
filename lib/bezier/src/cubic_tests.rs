//! Unit tests for the cubic segment.

#![allow(
    clippy::float_cmp,
    reason = "the ends of a parameter range are compared exactly: they are answered as the constants"
)]

use core::f64::consts::PI;

use tairix_fuzzseed::Prng;
use tairix_util::mathf::{fmax, sqrt};

use super::CubicBez;
use crate::point::Point;

/// The control distance that draws a quarter circle of unit radius.
const KAPPA: f64 = 0.552_284_749_830_793_4;

fn wavy() -> CubicBez {
    CubicBez::new(
        Point::new(0.0, 0.0),
        Point::new(10.0, 40.0),
        Point::new(30.0, -40.0),
        Point::new(40.0, 0.0),
    )
}

fn random_cubic(rng: &mut Prng) -> CubicBez {
    let mut coordinate = || f64::from(rng.next_u32() % 2001) / 10.0 - 100.0;
    let mut point = || Point::new(coordinate(), coordinate());
    CubicBez::new(point(), point(), point(), point())
}

#[track_caller]
fn assert_near(a: Point, b: Point, tolerance: f64) {
    assert!(a.distance(b) <= tolerance, "{a:?} vs {b:?}");
}

#[test]
fn split_halves_meet_and_trace_the_whole() {
    let curve = wavy();
    let (left, right) = curve.split(0.3);
    assert_eq!(left.p3, right.p0);
    assert_near(left.eval(0.5), curve.eval(0.15), 1e-9);
    assert_near(right.eval(0.5), curve.eval(0.65), 1e-9);
}

#[test]
fn a_subsegment_lands_on_the_curve_and_can_run_backwards() {
    let curve = wavy();
    let part = curve.subsegment(0.2, 0.7);
    assert_eq!(part.p0, curve.eval(0.2));
    assert_eq!(part.p3, curve.eval(0.7));
    assert_near(part.eval(0.5), curve.eval(0.45), 1e-9);
    let back = curve.subsegment(0.7, 0.2);
    assert_eq!(back.p0, curve.eval(0.7));
    assert_eq!(back.p3, curve.eval(0.2));
    assert_eq!(curve.subsegment(0.0, 1.0), curve);
}

#[test]
fn bounds_are_tight_around_every_sample() {
    let mut rng = Prng::new(0x0c0b_1c00);
    for _ in 0..300 {
        let curve = random_cubic(&mut rng);
        let bounds = curve.bounds();
        let mut sampled = crate::bounds::Bounds::EMPTY;
        for step in 0..=1000 {
            let point = curve.eval(f64::from(step) / 1000.0);
            assert!(
                bounds.inflated(1e-9).contains(point),
                "{point:?} outside {bounds:?}"
            );
            sampled = sampled.union_point(point);
        }
        // Tight: no more than a sample step's worth beyond what was sampled.
        let slack = fmax(bounds.width(), bounds.height()) * 1e-3 + 1e-9;
        assert!(sampled.inflated(slack).contains_bounds(bounds));
    }
}

#[test]
fn nearest_matches_a_dense_search() {
    let mut rng = Prng::new(0x00ea_7e57);
    for _ in 0..300 {
        let curve = random_cubic(&mut rng);
        let p = Point::new(
            f64::from(rng.next_u32() % 2001) / 10.0 - 100.0,
            f64::from(rng.next_u32() % 2001) / 10.0 - 100.0,
        );
        let (_, found) = curve.nearest(p);
        let dense = (0..=20_000)
            .map(|step| curve.eval(f64::from(step) / 20_000.0).distance_squared(p))
            .fold(f64::INFINITY, |a, b| if b < a { b } else { a });
        assert!(
            sqrt(found) <= sqrt(dense) + 1e-3,
            "found {} but a dense search reached {}",
            sqrt(found),
            sqrt(dense)
        );
    }
}

#[test]
fn a_quarter_circle_has_its_length_and_area() {
    let quarter = CubicBez::new(
        Point::new(1.0, 0.0),
        Point::new(1.0, KAPPA),
        Point::new(KAPPA, 1.0),
        Point::new(0.0, 1.0),
    );
    let length = quarter.arclen(1e-12);
    assert!((length - PI / 2.0).abs() < 1e-3, "length {length}");
    // Four quarters about the origin enclose (very nearly) the unit circle.
    let turn = |c: CubicBez| {
        CubicBez::new(
            Point::new(-c.p0.y, c.p0.x),
            Point::new(-c.p1.y, c.p1.x),
            Point::new(-c.p2.y, c.p2.x),
            Point::new(-c.p3.y, c.p3.x),
        )
    };
    let mut piece = quarter;
    let mut area = 0.0;
    for _ in 0..4 {
        area += piece.signed_area();
        piece = turn(piece);
    }
    assert!((area - PI).abs() < 1e-3, "area {area}");
}

#[test]
fn a_straight_cubic_is_a_line_of_its_chord_length() {
    let line = CubicBez::new(
        Point::new(0.0, 0.0),
        Point::new(1.0, 1.0),
        Point::new(2.0, 2.0),
        Point::new(3.0, 3.0),
    );
    assert!(line.is_line(1e-9));
    assert!((line.arclen(1e-9) - 3.0 * sqrt(2.0)).abs() < 1e-9);
    assert!(!wavy().is_line(1.0));
    // Straight but doubling back past its end is not a line.
    let back = CubicBez::new(
        Point::new(0.0, 0.0),
        Point::new(5.0, 0.0),
        Point::new(5.0, 0.0),
        Point::new(1.0, 0.0),
    );
    assert!(!back.is_line(1e-9));
}

#[test]
fn inverse_arc_length_finds_the_parameter_it_measured() {
    let curve = wavy();
    let total = curve.arclen(1e-9);
    for share in [0.1, 0.25, 0.5, 0.9] {
        let t = curve.inv_arclen(total * share, 1e-9);
        let reached = curve.subsegment(0.0, t).arclen(1e-9);
        assert!(
            (reached - total * share).abs() < 1e-6,
            "{reached} vs {}",
            total * share
        );
    }
    assert_eq!(curve.inv_arclen(-1.0, 1e-9), 0.0);
    assert_eq!(curve.inv_arclen(total * 2.0, 1e-9), 1.0);
}

#[test]
fn tangents_fall_back_through_coincident_controls() {
    let curve = CubicBez::new(
        Point::new(0.0, 0.0),
        Point::new(0.0, 0.0),
        Point::new(5.0, 5.0),
        Point::new(10.0, 0.0),
    );
    let (leaving, arriving) = curve.tangents();
    assert_eq!(leaving, Point::new(5.0, 5.0));
    assert_eq!(arriving, Point::new(5.0, -5.0));
}

#[test]
fn coefficients_reproduce_the_curve() {
    let curve = wavy();
    let (cubed, squared, linear, constant) = curve.coefficients();
    for step in 0..=8 {
        let at = f64::from(step) / 8.0;
        let point = ((cubed * at + squared) * at + linear) * at + constant;
        assert_near(point, curve.eval(at), 1e-9);
    }
}
