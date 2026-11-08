//! Tests for measuring a path along its length.

use alloc::vec;

use super::PathMeasure;
use crate::cubic::CubicBez;
use crate::path::{Path, PathEl, Subpath};
use crate::point::Point;

fn polyline(points: &[(f64, f64)]) -> Subpath {
    let mut subpath = Subpath::new(Point::new(points[0].0, points[0].1));
    for &(x, y) in &points[1..] {
        subpath.elements.push(PathEl::Line(Point::new(x, y)));
    }
    subpath
}

fn near(a: Point, b: Point) -> bool {
    a.distance(b) < 1e-9
}

#[test]
fn a_distance_along_lines_falls_where_their_lengths_say() {
    let path = Path {
        subpaths: vec![polyline(&[(0.0, 0.0), (10.0, 0.0), (10.0, 5.0)])],
    };
    let measure = PathMeasure::new(&path).expect("measured");
    assert!((measure.length() - 15.0).abs() < 1e-12);
    let (point, direction) = measure.at(4.0).expect("on the first leg");
    assert!(near(point, Point::new(4.0, 0.0)));
    assert!(near(direction, Point::new(1.0, 0.0)));
    let (point, direction) = measure.at(12.0).expect("on the second leg");
    assert!(near(point, Point::new(10.0, 2.0)));
    assert!(near(direction, Point::new(0.0, 1.0)));
    assert!(near(
        measure.at(15.0).expect("its end").0,
        Point::new(10.0, 5.0)
    ));
    assert_eq!(measure.at(-0.001), None, "before the start");
    assert_eq!(measure.at(15.001), None, "past the end");
}

/// A move between sub-paths adds no length, and a distance landing on a
/// segment of no length is answered by the next that has some.
#[test]
fn a_distance_runs_on_into_the_next_sub_path() {
    let mut first = polyline(&[(0.0, 0.0), (3.0, 0.0), (3.0, 0.0)]);
    first.closed = false;
    let path = Path {
        subpaths: vec![first, polyline(&[(100.0, 100.0), (100.0, 104.0)])],
    };
    let measure = PathMeasure::new(&path).expect("measured");
    assert!((measure.length() - 7.0).abs() < 1e-12);
    let (point, direction) = measure.at(5.0).expect("on the second sub-path");
    assert!(near(point, Point::new(100.0, 102.0)));
    assert!(near(direction, Point::new(0.0, 1.0)));
    assert!(near(
        measure.at(3.0).expect("the seam").0,
        Point::new(3.0, 0.0)
    ));
}

#[test]
fn a_curve_is_measured_by_its_arc_length_and_runs_along_its_tangent() {
    let quarter = 0.552_284_749_830_793_4 * 10.0;
    let mut arc = Subpath::new(Point::new(10.0, 0.0));
    arc.elements.push(PathEl::Cubic(
        Point::new(10.0, quarter),
        Point::new(quarter, 10.0),
        Point::new(0.0, 10.0),
    ));
    let path = Path {
        subpaths: vec![arc],
    };
    let measure = PathMeasure::new(&path).expect("measured");
    let circle = core::f64::consts::FRAC_PI_2 * 10.0;
    assert!((measure.length() - circle).abs() < 0.01, "a quarter circle");
    let (point, direction) = measure.at(measure.length() / 2.0).expect("half way");
    let diagonal = core::f64::consts::FRAC_1_SQRT_2;
    assert!(point.distance(Point::new(10.0 * diagonal, 10.0 * diagonal)) < 0.01);
    assert!(direction.distance(Point::new(-diagonal, diagonal)) < 0.01);
    assert!((direction.length() - 1.0).abs() < 1e-12, "a unit direction");
}

/// Coincident controls give a cusp no derivative points along, so the
/// direction is the tangent the curve leaves or arrives on.
#[test]
fn a_cusp_of_coincident_controls_still_has_a_direction() {
    let cubic = CubicBez::new(
        Point::new(0.0, 0.0),
        Point::new(0.0, 0.0),
        Point::new(10.0, 0.0),
        Point::new(10.0, 0.0),
    );
    let mut subpath = Subpath::new(cubic.p0);
    subpath
        .elements
        .push(PathEl::Cubic(cubic.p1, cubic.p2, cubic.p3));
    let measure = PathMeasure::new(&Path {
        subpaths: vec![subpath],
    })
    .expect("measured");
    let (_, leaving) = measure.at(0.0).expect("the start");
    let (_, arriving) = measure.at(measure.length()).expect("the end");
    assert!(near(leaving, Point::new(1.0, 0.0)));
    assert!(near(arriving, Point::new(1.0, 0.0)));
}

#[test]
fn a_path_of_no_length_has_no_point_along_it() {
    let measure = PathMeasure::new(&Path::new()).expect("measured");
    assert_eq!(measure.at(0.0), None);
    let dot = Path {
        subpaths: vec![polyline(&[(1.0, 1.0), (1.0, 1.0)])],
    };
    assert_eq!(PathMeasure::new(&dot).expect("measured").at(0.0), None);
}
