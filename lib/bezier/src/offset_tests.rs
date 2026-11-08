//! Unit tests for offsets, stroke outlines, dashes and growing fills.

use alloc::vec;
use alloc::vec::Vec;

use core::f64::consts::{PI, TAU};

use tairix_fuzzseed::Prng;
use tairix_util::mathf::sqrt;

use super::{dashed, grown, offset_cubic, outline, Cap, Join, StrokeStyle};
use crate::arc::EllipseArc;
use crate::cubic::CubicBez;
use crate::path::{FillRule, Path, PathEl, Subpath};
use crate::point::Point;

fn p(x: f64, y: f64) -> Point {
    Point::new(x, y)
}

fn polyline(points: &[Point], closed: bool) -> Path {
    Path {
        subpaths: vec![Subpath {
            start: points[0],
            elements: points[1..].iter().map(|&c| PathEl::Line(c)).collect(),
            closed,
        }],
    }
}

fn circle(radius: f64) -> Path {
    let arc = EllipseArc {
        centre: Point::ZERO,
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

fn style(width: f64, join: Join, cap: Cap) -> StrokeStyle {
    StrokeStyle {
        width,
        join,
        miter_limit: 4.0,
        start_cap: cap,
        end_cap: cap,
    }
}

#[track_caller]
fn assert_area(path: &Path, want: f64, slack: f64) {
    let got = path.signed_area();
    assert!((got - want).abs() <= slack, "area {got}, want {want}");
}

#[test]
fn an_arc_offset_inwards_keeps_its_centre() {
    // A clockwise quarter circle of radius 10, offset right as drawn, runs
    // nearer the centre.
    let arc = EllipseArc {
        centre: Point::ZERO,
        radii: p(10.0, 10.0),
        x_rotation: 0.0,
        start: 0.0,
        sweep: PI / 2.0,
    };
    let mut quarter = None;
    arc.to_cubics(|c| quarter = Some(c));
    let quarter = quarter.expect("one cubic");
    let mut pieces: Vec<CubicBez> = Vec::new();
    offset_cubic(quarter, 3.0, 1e-4, |piece| {
        pieces.push(piece);
        Ok::<(), ()>(())
    })
    .expect("every piece taken");
    for piece in &pieces {
        for step in 0..=8 {
            let point = piece.eval(f64::from(step) / 8.0);
            // Three from the cubic itself, which strays from the circle by
            // its own small error, and on the centre's side.
            let distance = sqrt(quarter.nearest(point).1);
            assert!((distance - 3.0).abs() < 2e-4, "distance {distance}");
            assert!(point.length() < 7.01, "inside, at {}", point.length());
        }
    }
}

#[test]
fn a_straight_stroke_outlines_its_rectangle() {
    let line = polyline(&[p(0.0, 0.0), p(10.0, 0.0)], false);
    let butt = outline(&line, &style(2.0, Join::Miter, Cap::Butt), 1e-6).expect("outline");
    assert_area(&butt, 20.0, 1e-9);
    let square = outline(&line, &style(2.0, Join::Miter, Cap::Square), 1e-6).expect("outline");
    assert_area(&square, 24.0, 1e-9);
    let round = outline(&line, &style(2.0, Join::Miter, Cap::Round), 1e-6).expect("outline");
    assert_area(&round, 20.0 + PI, 2e-3);
}

#[test]
fn a_stroked_circle_outlines_its_ring() {
    let ring = outline(&circle(10.0), &style(2.0, Join::Round, Cap::Butt), 1e-5).expect("outline");
    assert_eq!(ring.subpaths.len(), 2, "outer and inner edges");
    assert_area(&ring, TAU * 10.0 * 2.0, 0.05);
}

#[test]
fn joins_meet_as_their_style_says() {
    let corner = polyline(&[p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0)], false);
    // Two 10×2 bars overlapping in a unit square inside the corner cover 39;
    // a bevel adds the outer corner's half square…
    let bevel = outline(&corner, &style(2.0, Join::Bevel, Cap::Butt), 1e-6).expect("outline");
    assert_area(&bevel, 39.5, 1e-9);
    // …a miter its whole square…
    let miter = outline(&corner, &style(2.0, Join::Miter, Cap::Butt), 1e-6).expect("outline");
    assert_area(&miter, 40.0, 1e-9);
    // …and a round join a quarter disc.
    let round = outline(&corner, &style(2.0, Join::Round, Cap::Butt), 1e-6).expect("outline");
    assert_area(&round, 39.0 + PI / 4.0, 2e-3);
    // Past the miter limit the miter is bevelled.
    let mut limited = style(2.0, Join::Miter, Cap::Butt);
    limited.miter_limit = 1.0;
    assert_area(
        &outline(&corner, &limited, 1e-6).expect("outline"),
        39.5,
        1e-9,
    );
}

#[test]
fn an_outline_covers_exactly_what_lies_within_half_its_width() {
    let mut rng = Prng::new(0x0ff5_e7ed);
    let path = Path {
        subpaths: vec![Subpath {
            start: p(0.0, 0.0),
            elements: vec![
                PathEl::Cubic(p(10.0, 40.0), p(40.0, -20.0), p(50.0, 20.0)),
                PathEl::Line(p(70.0, 0.0)),
            ],
            closed: false,
        }],
    };
    let shape = outline(&path, &style(6.0, Join::Round, Cap::Round), 1e-4).expect("outline");
    for _ in 0..3000 {
        let q = p(
            f64::from(rng.next_u32() % 9000) / 100.0 - 10.0,
            f64::from(rng.next_u32() % 7000) / 100.0 - 30.0,
        );
        let distance = path
            .segments()
            .map(|segment| sqrt(segment.nearest(q).1))
            .fold(f64::INFINITY, |a, b| if b < a { b } else { a });
        if (distance - 3.0).abs() < 0.05 {
            continue;
        }
        assert_eq!(
            shape.contains(q, FillRule::NonZero),
            distance < 3.0,
            "{q:?} at {distance}"
        );
    }
}

#[test]
fn dashes_cut_the_path_into_its_pattern() {
    let line = polyline(&[p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0)], false);
    let pieces = dashed(&line, &[3.0, 2.0], 1.0).expect("dashes");
    // 20 long, a dash in: 0–2, 4–7, 9–12 (round the corner), 14–17, 19–20.
    assert_eq!(pieces.subpaths.len(), 5);
    let length: f64 = pieces.segments().map(|segment| segment.arclen(1e-9)).sum();
    assert!((length - 12.0).abs() < 1e-9, "dashed length {length}");
    assert_eq!(
        pieces.subpaths[2].elements.len(),
        2,
        "the third dash turns the corner"
    );
    // An odd pattern repeats twice over, and a dash through the seam of a
    // closed path is one dash.
    let square = polyline(
        &[p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0), p(0.0, 10.0)],
        true,
    );
    let odd = dashed(&square, &[3.0], 1.5).expect("dashes");
    assert_eq!(odd.subpaths.len(), 7);
    let whole = dashed(&square, &[], 0.0).expect("solid");
    assert_eq!(whole, square, "no pattern draws it whole");
}

#[test]
fn a_dash_of_no_length_keeps_the_direction_its_caps_need() {
    let line = polyline(&[p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0)], false);
    let dots = dashed(&line, &[0.0, 4.0], 0.0).expect("dots");
    // At 0, 4, 8, 12 and 16 along: the third and later on the second leg.
    assert_eq!(dots.subpaths.len(), 5);
    for (dot, along) in dots.subpaths.iter().zip([
        p(1.0, 0.0),
        p(1.0, 0.0),
        p(1.0, 0.0),
        p(0.0, 1.0),
        p(0.0, 1.0),
    ]) {
        let [PathEl::Line(to)] = dot.elements.as_slice() else {
            panic!("one step: {dot:?}");
        };
        let step = *to - dot.start;
        assert!(step.length() > 0.0 && step.length() < 1e-9, "{step:?}");
        assert!(
            step.normalised().expect("a direction").distance(along) < 1e-9,
            "{step:?}"
        );
    }
}

#[test]
fn growing_a_square_keeps_its_corners_as_the_join_says() {
    let square = polyline(
        &[p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0), p(0.0, 10.0)],
        true,
    );
    let mitred = grown(&square, FillRule::NonZero, 2.0, Join::Miter, 4.0, 1e-6).expect("grown");
    assert_area(&mitred, 196.0, 1e-9);
    let rounded = grown(&square, FillRule::NonZero, 2.0, Join::Round, 4.0, 1e-6).expect("grown");
    assert_area(&rounded, 100.0 + 80.0 + PI * 4.0, 0.01);
    let shrunk = grown(&square, FillRule::NonZero, -2.0, Join::Miter, 4.0, 1e-6).expect("shrunk");
    assert_area(&shrunk, 36.0, 1e-9);
    let gone = grown(&square, FillRule::NonZero, -6.0, Join::Miter, 4.0, 1e-6).expect("shrunk");
    assert!(
        gone.signed_area().abs() < 1e-9,
        "shrunk past its middle, nothing is left"
    );
}
