//! Unit tests for paths: winding, containment, area and reversal.

use alloc::vec;
use alloc::vec::Vec;

use core::f64::consts::{PI, TAU};

use tairix_fuzzseed::Prng;

use super::{FillRule, Path, PathEl, Subpath};
use crate::arc::EllipseArc;
use crate::point::Point;

fn p(x: f64, y: f64) -> Point {
    Point::new(x, y)
}

/// A closed polygon through `corners`.
fn polygon(corners: &[Point]) -> Subpath {
    Subpath {
        start: corners[0],
        elements: corners[1..].iter().map(|&c| PathEl::Line(c)).collect(),
        closed: true,
    }
}

/// A closed circle of four cubics, clockwise as drawn unless `reverse`.
fn circle(centre: Point, radius: f64, reverse: bool) -> Subpath {
    let arc = EllipseArc {
        centre,
        radii: p(radius, radius),
        x_rotation: 0.0,
        start: 0.0,
        sweep: if reverse { -TAU } else { TAU },
    };
    let mut subpath = Subpath::new(arc.start_point());
    arc.to_cubics(|c| subpath.elements.push(PathEl::Cubic(c.p1, c.p2, c.p3)));
    subpath.closed = true;
    subpath
}

fn square(x: f64, y: f64, side: f64) -> Subpath {
    polygon(&[
        p(x, y),
        p(x + side, y),
        p(x + side, y + side),
        p(x, y + side),
    ])
}

#[test]
fn a_clockwise_square_winds_once_around_its_inside() {
    let path = Path {
        subpaths: vec![square(0.0, 0.0, 10.0)],
    };
    assert_eq!(path.winding(p(5.0, 5.0)), 1);
    assert_eq!(path.winding(p(15.0, 5.0)), 0);
    assert_eq!(path.winding(p(-1.0, 5.0)), 0);
    let reversed = path.reversed().expect("reversed");
    assert_eq!(reversed.winding(p(5.0, 5.0)), -1);
    assert!((path.signed_area() - 100.0).abs() < 1e-12);
    assert!((reversed.signed_area() + 100.0).abs() < 1e-12);
}

#[test]
fn a_ray_through_a_vertex_counts_it_once() {
    let diamond = Path {
        subpaths: vec![polygon(&[
            p(5.0, 0.0),
            p(10.0, 5.0),
            p(5.0, 10.0),
            p(0.0, 5.0),
        ])],
    };
    // The ray from the centre passes exactly through the right vertex.
    assert_eq!(diamond.winding(p(5.0, 5.0)), 1);
    // And from the left of the shape, through both side vertices.
    assert_eq!(diamond.winding(p(-3.0, 5.0)), 0);
    // A ray grazing the top vertex counts nothing.
    assert_eq!(diamond.winding(p(1.0, 0.0)), 0);
}

#[test]
fn a_hole_is_a_hole_by_either_rule_when_wound_against() {
    let path = Path {
        subpaths: vec![
            square(0.0, 0.0, 30.0),
            square(10.0, 10.0, 10.0).reversed().expect("reversed"),
        ],
    };
    assert!(path.contains(p(5.0, 5.0), FillRule::NonZero));
    assert!(!path.contains(p(15.0, 15.0), FillRule::NonZero));
    assert!(!path.contains(p(15.0, 15.0), FillRule::EvenOdd));
    // Wound the same way, only even-odd makes it a hole.
    let same = Path {
        subpaths: vec![square(0.0, 0.0, 30.0), square(10.0, 10.0, 10.0)],
    };
    assert!(same.contains(p(15.0, 15.0), FillRule::NonZero));
    assert!(!same.contains(p(15.0, 15.0), FillRule::EvenOdd));
}

#[test]
fn a_circle_of_cubics_holds_what_its_radius_says() {
    let disc = Path {
        subpaths: vec![circle(p(50.0, 50.0), 20.0, false)],
    };
    let mut random = Prng::new(0x00c1_7c1e);
    for _ in 0..2000 {
        let probe = p(
            f64::from(random.next_u32() % 10_000) / 100.0,
            f64::from(random.next_u32() % 10_000) / 100.0,
        );
        let distance = probe.distance(p(50.0, 50.0));
        if (distance - 20.0).abs() < 0.02 {
            continue;
        }
        let inside = disc.contains(probe, FillRule::NonZero);
        assert_eq!(inside, distance < 20.0, "{probe:?} at {distance}");
    }
    assert!((disc.signed_area() - PI * 400.0).abs() < 0.5);
    assert_eq!(
        Path {
            subpaths: vec![circle(p(0.0, 0.0), 1.0, true)]
        }
        .winding(p(0.0, 0.0)),
        -1
    );
}

#[test]
fn a_figure_eight_winds_opposite_ways_in_its_loops() {
    let eight = Path {
        subpaths: vec![polygon(&[
            p(0.0, 0.0),
            p(10.0, 10.0),
            p(10.0, 0.0),
            p(0.0, 10.0),
        ])],
    };
    let left = eight.winding(p(2.0, 5.0));
    let right = eight.winding(p(8.0, 5.0));
    assert_eq!(left, -right);
    assert_eq!(left.abs(), 1);
}

#[test]
fn an_open_subpath_fills_as_if_closed() {
    let mut open = square(0.0, 0.0, 10.0);
    open.closed = false;
    let path = Path {
        subpaths: vec![open],
    };
    assert_eq!(path.winding(p(5.0, 5.0)), 1);
    assert_eq!(
        path.segments().count(),
        3,
        "an open path strokes no closing side"
    );
}

#[test]
fn reversing_twice_is_the_identity() {
    let mut subpath = Subpath::new(p(0.0, 0.0));
    subpath
        .elements
        .push(PathEl::Cubic(p(1.0, 5.0), p(4.0, 5.0), p(5.0, 0.0)));
    subpath.elements.push(PathEl::Line(p(9.0, 2.0)));
    let back = subpath.reversed().expect("reversed");
    assert_eq!(back.start, p(9.0, 2.0));
    assert_eq!(back.elements[0], PathEl::Line(p(5.0, 0.0)));
    assert_eq!(
        back.elements[1],
        PathEl::Cubic(p(4.0, 5.0), p(1.0, 5.0), p(0.0, 0.0))
    );
    assert_eq!(back.reversed().expect("back"), subpath);
}

#[test]
fn flattening_starts_at_the_start_and_ends_at_the_end() {
    let subpath = circle(p(0.0, 0.0), 10.0, false);
    let mut points: Vec<Point> = Vec::new();
    subpath.flatten(0.01, &mut points).expect("flattened");
    assert_eq!(points[0], subpath.start);
    assert!(points.len() > 16);
    for point in &points {
        assert!((point.length() - 10.0).abs() < 0.02);
    }
}

#[test]
fn a_copy_is_the_path_and_a_move_moves_every_point_of_it() {
    let path = Path {
        subpaths: vec![circle(p(5.0, 5.0), 3.0, false), square(0.0, 0.0, 2.0)],
    };
    let copy = path.try_clone().expect("copied");
    assert_eq!(copy, path);
    let moved = path
        .transformed(crate::affine::Affine::translate(10.0, -4.0))
        .expect("moved");
    assert_eq!(moved.subpaths.len(), path.subpaths.len());
    for (moved, original) in moved.subpaths.iter().zip(&path.subpaths) {
        assert_eq!(moved.start, original.start + p(10.0, -4.0));
        assert_eq!(moved.closed, original.closed);
        assert_eq!(moved.end(), original.end() + p(10.0, -4.0));
        assert_eq!(moved.elements.len(), original.elements.len());
    }
    let single = Path::single(square(1.0, 1.0, 1.0)).expect("one");
    assert_eq!(single.subpaths, vec![square(1.0, 1.0, 1.0)]);
}

#[test]
fn a_step_made_of_a_segment_starts_that_segment_again() {
    let subpath = circle(p(0.0, 0.0), 4.0, true);
    let mut from = subpath.start;
    for &element in &subpath.elements {
        let segment = element.segment_from(from);
        assert_eq!(PathEl::of(segment), element);
        from = element.end();
    }
    let line = PathEl::Line(p(3.0, 4.0));
    assert_eq!(PathEl::of(line.segment_from(p(1.0, 1.0))), line);
}
