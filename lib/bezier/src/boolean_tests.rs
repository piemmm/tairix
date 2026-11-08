//! Unit tests for boolean operations.

use alloc::vec;
use alloc::vec::Vec;

use core::f64::consts::{PI, TAU};

use tairix_fuzzseed::Prng;
use tairix_util::mathf::sqrt;

use super::{boolean, components, divide, simplified, BooleanError, Operation};
use crate::arc::EllipseArc;
use crate::path::{FillRule, Path, PathEl, Subpath};
use crate::point::Point;

const OPERATIONS: [Operation; 4] = [
    Operation::Union,
    Operation::Intersection,
    Operation::Difference,
    Operation::Exclusion,
];

fn p(x: f64, y: f64) -> Point {
    Point::new(x, y)
}

fn polygon(corners: &[Point]) -> Subpath {
    Subpath {
        start: corners[0],
        elements: corners[1..].iter().map(|&c| PathEl::Line(c)).collect(),
        closed: true,
    }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> Path {
    Path {
        subpaths: vec![polygon(&[
            p(x, y),
            p(x + w, y),
            p(x + w, y + h),
            p(x, y + h),
        ])],
    }
}

fn circle(centre: Point, radius: f64) -> Path {
    let arc = EllipseArc {
        centre,
        radii: p(radius, radius),
        x_rotation: 0.0,
        start: 0.0,
        sweep: TAU,
    };
    let mut subpath = Subpath::new(arc.start_point());
    arc.to_cubics(|c| subpath.elements.push(PathEl::Cubic(c.p1, c.p2, c.p3)));
    subpath.closed = true;
    Path {
        subpaths: vec![subpath],
    }
}

fn star(centre: Point, outer: f64, inner: f64, points: u32, turn: f64) -> Path {
    let mut corners = Vec::new();
    for i in 0..points * 2 {
        let radius = if i % 2 == 0 { outer } else { inner };
        let angle = turn + f64::from(i) * PI / f64::from(points);
        corners.push(centre + Point::from_angle(angle) * radius);
    }
    Path {
        subpaths: vec![polygon(&corners)],
    }
}

/// The distance from `q` to the nearest segment of either path.
fn boundary_distance(q: Point, paths: &[&Path]) -> f64 {
    paths
        .iter()
        .flat_map(|path| path.subpaths.iter().flat_map(Subpath::filled_segments))
        .map(|segment| sqrt(segment.nearest(q).1))
        .fold(f64::INFINITY, |a, b| if b < a { b } else { a })
}

/// Check that `result` covers exactly what `operation` says of `a` and `b`,
/// at random points well clear of either operand's outline.
#[track_caller]
fn check(a: &Path, rule_a: FillRule, b: &Path, rule_b: FillRule, operation: Operation, seed: u64) {
    let result = boolean(a, rule_a, b, rule_b, operation).expect("the operation completes");
    let bounds = a.bounds().union(b.bounds()).inflated(5.0);
    let mut rng = Prng::new(seed);
    let mut checked = 0;
    for _ in 0..1500 {
        let q = p(
            bounds.min.x + f64::from(rng.next_u32() % 10_000) / 10_000.0 * bounds.width(),
            bounds.min.y + f64::from(rng.next_u32() % 10_000) / 10_000.0 * bounds.height(),
        );
        if boundary_distance(q, &[a, b]) < 0.05 {
            continue;
        }
        let want = operation.keeps(a.contains(q, rule_a), b.contains(q, rule_b));
        assert_eq!(
            result.contains(q, FillRule::NonZero),
            want,
            "{operation:?} at {q:?}"
        );
        assert_eq!(
            result.contains(q, FillRule::EvenOdd),
            want,
            "{operation:?} at {q:?} under even-odd"
        );
        checked += 1;
    }
    assert!(checked > 500);
}

#[test]
fn overlapping_squares_combine_by_every_operation() {
    let a = rect(0.0, 0.0, 10.0, 10.0);
    let b = rect(5.0, 5.0, 10.0, 10.0);
    let area = |operation| {
        boolean(&a, FillRule::NonZero, &b, FillRule::NonZero, operation)
            .expect("completes")
            .signed_area()
    };
    assert!((area(Operation::Union) - 175.0).abs() < 1e-9);
    assert!((area(Operation::Intersection) - 25.0).abs() < 1e-9);
    assert!((area(Operation::Difference) - 75.0).abs() < 1e-9);
    assert!((area(Operation::Exclusion) - 150.0).abs() < 1e-9);
    for (seed, operation) in OPERATIONS.into_iter().enumerate() {
        check(
            &a,
            FillRule::NonZero,
            &b,
            FillRule::NonZero,
            operation,
            seed as u64,
        );
    }
}

#[test]
fn squares_sharing_an_edge_unite_into_one_rectangle() {
    let a = rect(0.0, 0.0, 10.0, 10.0);
    let b = rect(10.0, 0.0, 10.0, 10.0);
    let union = boolean(
        &a,
        FillRule::NonZero,
        &b,
        FillRule::NonZero,
        Operation::Union,
    )
    .expect("completes");
    assert_eq!(union.subpaths.len(), 1, "one contour round both");
    assert!((union.signed_area() - 200.0).abs() < 1e-9);
    let none = boolean(
        &a,
        FillRule::NonZero,
        &b,
        FillRule::NonZero,
        Operation::Intersection,
    )
    .expect("completes");
    assert!(none.signed_area().abs() < 1e-9);
}

#[test]
fn a_shape_united_with_itself_is_itself() {
    let shape = circle(p(0.0, 0.0), 10.0);
    let union = boolean(
        &shape,
        FillRule::NonZero,
        &shape,
        FillRule::NonZero,
        Operation::Union,
    )
    .expect("completes");
    assert_eq!(union.subpaths.len(), 1);
    assert!((union.signed_area() - shape.signed_area()).abs() < 1e-6);
    let nothing = boolean(
        &shape,
        FillRule::NonZero,
        &shape,
        FillRule::NonZero,
        Operation::Difference,
    )
    .expect("completes");
    assert!(nothing.subpaths.is_empty(), "nothing is left");
}

#[test]
fn circles_keep_their_curves() {
    let a = circle(p(0.0, 0.0), 10.0);
    let b = circle(p(12.0, 0.0), 10.0);
    let union = boolean(
        &a,
        FillRule::NonZero,
        &b,
        FillRule::NonZero,
        Operation::Union,
    )
    .expect("completes");
    assert_eq!(union.subpaths.len(), 1);
    let elements = &union.subpaths[0].elements;
    assert!(
        elements.iter().all(|e| matches!(e, PathEl::Cubic(..))),
        "curves stay curves"
    );
    assert!(elements.len() <= 10, "{} segments", elements.len());
    for (seed, operation) in OPERATIONS.into_iter().enumerate() {
        check(
            &a,
            FillRule::NonZero,
            &b,
            FillRule::NonZero,
            operation,
            100 + seed as u64,
        );
    }
    // The lens two unit-spaced circles share has the area geometry gives.
    let lens = boolean(
        &a,
        FillRule::NonZero,
        &b,
        FillRule::NonZero,
        Operation::Intersection,
    )
    .expect("completes");
    let (r, d): (f64, f64) = (10.0, 12.0);
    let want =
        2.0 * r * r * tairix_util::mathf::acos(d / (2.0 * r)) - 0.5 * d * sqrt(4.0 * r * r - d * d);
    assert!(
        (lens.signed_area() - want).abs() < want * 2e-3,
        "{} vs {want}",
        lens.signed_area()
    );
}

#[test]
fn a_hole_cut_out_is_wound_against_its_surround() {
    let outer = rect(0.0, 0.0, 30.0, 30.0);
    let inner = circle(p(15.0, 15.0), 5.0);
    let ring = boolean(
        &outer,
        FillRule::NonZero,
        &inner,
        FillRule::NonZero,
        Operation::Difference,
    )
    .expect("completes");
    assert_eq!(ring.subpaths.len(), 2);
    let areas: Vec<f64> = ring.subpaths.iter().map(Subpath::signed_area).collect();
    assert!(areas.iter().any(|&a| a > 0.0) && areas.iter().any(|&a| a < 0.0));
    let groups = components(&ring).expect("groups");
    assert_eq!(groups.len(), 1, "the hole belongs to its surround");
    assert_eq!(groups[0].subpaths.len(), 2);
}

#[test]
fn stars_under_both_rules_combine_correctly() {
    let a = star(p(0.0, 0.0), 20.0, 8.0, 5, 0.1);
    let b = star(p(6.0, 4.0), 18.0, 7.0, 7, 0.4);
    for rule in [FillRule::NonZero, FillRule::EvenOdd] {
        for (seed, operation) in OPERATIONS.into_iter().enumerate() {
            check(&a, rule, &b, rule, operation, 200 + seed as u64);
        }
    }
}

#[test]
fn a_self_crossing_path_simplifies_to_what_it_covers() {
    // A pentagram drawn as one contour: even-odd leaves the middle empty,
    // non-zero fills it.
    let mut corners = Vec::new();
    for i in 0..5_u32 {
        corners.push(Point::from_angle(f64::from(i) * 4.0 * PI / 5.0) * 20.0);
    }
    let pentagram = Path {
        subpaths: vec![polygon(&corners)],
    };
    for rule in [FillRule::NonZero, FillRule::EvenOdd] {
        let clean = simplified(&pentagram, rule).expect("completes");
        assert_eq!(
            clean.contains(p(0.0, 0.0), FillRule::NonZero),
            rule == FillRule::NonZero
        );
        check(
            &pentagram,
            rule,
            &Path::new(),
            FillRule::NonZero,
            Operation::Union,
            300,
        );
    }
}

#[test]
fn random_polygons_combine_correctly() {
    let mut rng = Prng::new(0x0b00_1ea5);
    for round in 0..25 {
        let random_polygon = |rng: &mut Prng| {
            let count = 3 + rng.below(6);
            let corners: Vec<Point> = (0..count)
                .map(|_| {
                    p(
                        f64::from(rng.next_u32() % 400) / 10.0,
                        f64::from(rng.next_u32() % 400) / 10.0,
                    )
                })
                .collect();
            Path {
                subpaths: vec![polygon(&corners)],
            }
        };
        let (a, b) = (random_polygon(&mut rng), random_polygon(&mut rng));
        for (seed, operation) in OPERATIONS.into_iter().enumerate() {
            check(
                &a,
                FillRule::EvenOdd,
                &b,
                FillRule::NonZero,
                operation,
                1000 + round * 10 + seed as u64,
            );
        }
    }
}

#[test]
fn divide_cuts_both_into_their_faces() {
    let a = rect(0.0, 0.0, 10.0, 10.0);
    let b = rect(5.0, 0.0, 10.0, 10.0);
    let faces = divide(&a, FillRule::NonZero, &b, FillRule::NonZero).expect("completes");
    assert_eq!(faces.len(), 3);
    let total: f64 = faces.iter().map(Path::signed_area).sum();
    assert!((total - 150.0).abs() < 1e-9);
}

#[test]
fn degenerate_operands_come_to_nothing_or_themselves() {
    let empty = Path::new();
    let square = rect(0.0, 0.0, 4.0, 4.0);
    let union = boolean(
        &empty,
        FillRule::NonZero,
        &square,
        FillRule::NonZero,
        Operation::Union,
    )
    .expect("completes");
    assert!((union.signed_area() - 16.0).abs() < 1e-12);
    let nothing = boolean(
        &empty,
        FillRule::NonZero,
        &empty,
        FillRule::NonZero,
        Operation::Union,
    )
    .expect("completes");
    assert!(nothing.subpaths.is_empty());
    let huge = Path {
        subpaths: vec![Subpath {
            start: p(0.0, 0.0),
            elements: vec![PathEl::Line(p(1.0, 1.0)); super::MAX_BOOLEAN_SEGMENTS + 1],
            closed: true,
        }],
    };
    assert_eq!(
        boolean(
            &huge,
            FillRule::NonZero,
            &empty,
            FillRule::NonZero,
            Operation::Union
        ),
        Err(BooleanError::TooComplex)
    );
}

#[test]
fn a_cut_at_or_past_an_end_sorts_with_that_end() {
    for start in [-0.0, 0.0, -1e-12, f64::NAN, f64::NEG_INFINITY] {
        assert_eq!(super::along_piece(start).to_bits(), 0.0_f64.to_bits());
    }
    for end in [1.0, 1.0 + 1e-12, f64::INFINITY] {
        assert_eq!(super::along_piece(end).to_bits(), 1.0_f64.to_bits());
    }
    assert_eq!(super::along_piece(0.25).to_bits(), 0.25_f64.to_bits());
}

#[test]
fn every_traced_contour_is_closed_and_a_component_keeps_its_holes() {
    let ring = Path {
        subpaths: vec![
            circle(p(0.0, 0.0), 10.0).subpaths[0].clone(),
            circle(p(0.0, 0.0), 4.0).subpaths[0]
                .reversed()
                .expect("hole"),
        ],
    };
    let far = circle(p(40.0, 0.0), 3.0);
    let both = boolean(
        &ring,
        FillRule::NonZero,
        &far,
        FillRule::NonZero,
        Operation::Union,
    )
    .expect("completes");
    assert!(both.subpaths.iter().all(|subpath| subpath.closed));
    let parts = components(&both).expect("grouped");
    assert_eq!(parts.len(), 2);
    let holed = parts
        .iter()
        .find(|part| part.subpaths.len() == 2)
        .expect("the ring keeps its hole");
    assert!(holed.contains(p(7.0, 0.0), FillRule::NonZero));
    assert!(!holed.contains(p(0.0, 0.0), FillRule::NonZero));
    assert!(parts
        .iter()
        .any(|part| part.subpaths.len() == 1 && part.contains(p(40.0, 0.0), FillRule::NonZero)));
}
