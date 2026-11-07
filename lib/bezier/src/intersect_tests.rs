//! Unit tests for segment intersection.

use tairix_fuzzseed::Prng;

use super::{cubic_cubic, line_cubic, line_line, meet, overlap, Meeting};
use crate::cubic::CubicBez;
use crate::line::Line;
use crate::point::Point;
use crate::segment::Segment;

const TOL: f64 = 1e-9;

fn p(x: f64, y: f64) -> Point {
    Point::new(x, y)
}

fn random_cubic(rng: &mut Prng) -> CubicBez {
    let mut coordinate = || f64::from(rng.next_u32() % 2001) / 10.0 - 100.0;
    let mut point = || p(coordinate(), coordinate());
    CubicBez::new(point(), point(), point(), point())
}

#[test]
fn crossing_lines_meet_where_both_say() {
    let a = Line::new(p(0.0, 0.0), p(10.0, 10.0));
    let b = Line::new(p(0.0, 10.0), p(10.0, 0.0));
    let crossing = line_line(a, b, TOL).expect("they cross");
    assert!((crossing.t - 0.5).abs() < 1e-12 && (crossing.u - 0.5).abs() < 1e-12);
    let apart = Line::new(p(20.0, 0.0), p(30.0, 10.0));
    assert_eq!(line_line(a, apart, TOL), None, "parallel lines never cross");
    let short = Line::new(p(0.0, 10.0), p(4.0, 6.0));
    assert_eq!(line_line(a, short, TOL), None, "it stops short");
}

#[test]
fn lines_touching_end_to_end_meet_at_their_ends() {
    let a = Line::new(p(0.0, 0.0), p(5.0, 0.0));
    let b = Line::new(p(5.0, 0.0), p(5.0, 5.0));
    let crossing = line_line(a, b, TOL).expect("they share an end");
    assert_eq!((crossing.t, crossing.u), (1.0, 0.0));
}

#[test]
fn a_line_meets_an_s_curve_three_times() {
    let curve = CubicBez::new(p(0.0, 0.0), p(10.0, 30.0), p(20.0, -30.0), p(30.0, 0.0));
    let line = Line::new(p(-5.0, 0.0), p(35.0, 0.0));
    let found = line_cubic(line, curve, TOL);
    assert_eq!(found.len(), 3);
    for crossing in &found {
        assert!(line.eval(crossing.t).distance(curve.eval(crossing.u)) < 1e-9);
    }
}

#[test]
fn two_arcs_cross_twice() {
    let a = CubicBez::new(p(0.0, 0.0), p(0.0, 20.0), p(20.0, 20.0), p(20.0, 0.0));
    let b = CubicBez::new(p(0.0, 10.0), p(0.0, -10.0), p(20.0, -10.0), p(20.0, 10.0));
    let found = cubic_cubic(a, b, TOL).expect("separable");
    assert_eq!(found.len(), 2);
    for crossing in &found {
        assert!(a.eval(crossing.t).distance(b.eval(crossing.u)) < 1e-7);
    }
}

#[test]
fn every_reported_crossing_of_random_cubics_is_a_crossing() {
    let mut rng = Prng::new(0x1a7e_75ec);
    for _ in 0..500 {
        let (a, b) = (random_cubic(&mut rng), random_cubic(&mut rng));
        let Ok(found) = cubic_cubic(a, b, 1e-7) else {
            continue;
        };
        for crossing in &found {
            let gap = a.eval(crossing.t).distance(b.eval(crossing.u));
            assert!(gap < 1e-5, "gap {gap} at {crossing:?}");
        }
    }
}

#[test]
fn random_cubics_cross_as_often_as_their_polylines_do() {
    let mut rng = Prng::new(0x0c0f_fee5);
    let polyline = |c: CubicBez| {
        let mut points = alloc::vec::Vec::new();
        for step in 0..=400 {
            points.push(c.eval(f64::from(step) / 400.0));
        }
        points
    };
    let mut agreeing = 0;
    let mut trials = 0;
    for _ in 0..300 {
        let (a, b) = (random_cubic(&mut rng), random_cubic(&mut rng));
        let Ok(found) = cubic_cubic(a, b, 1e-7) else {
            continue;
        };
        let (pa, pb) = (polyline(a), polyline(b));
        let mut count = 0;
        for sa in pa.windows(2) {
            for sb in pb.windows(2) {
                if line_line(Line::new(sa[0], sa[1]), Line::new(sb[0], sb[1]), 0.0).is_some() {
                    count += 1;
                }
            }
        }
        trials += 1;
        if count == found.len() {
            agreeing += 1;
        }
    }
    // A polyline can miss or double a grazing crossing; transversal ones
    // agree, which is nearly all of them.
    assert!(agreeing * 100 >= trials * 97, "{agreeing} of {trials}");
}

#[test]
fn a_cubic_and_a_piece_of_it_overlap() {
    let whole = CubicBez::new(p(0.0, 0.0), p(10.0, 30.0), p(40.0, 30.0), p(50.0, 0.0));
    let piece = whole.subsegment(0.25, 0.75);
    let run = overlap(Segment::Cubic(whole), Segment::Cubic(piece), 1e-7).expect("coincident");
    assert!((run.t0 - 0.25).abs() < 1e-6 && (run.t1 - 0.75).abs() < 1e-6);
    assert!(run.u0.abs() < 1e-6 && (run.u1 - 1.0).abs() < 1e-6);
    // Run backwards, the second's parameters run backwards too.
    let back = overlap(
        Segment::Cubic(whole),
        Segment::Cubic(piece.reversed()),
        1e-7,
    )
    .expect("coincident backwards");
    assert!(back.u0 > back.u1);
}

#[test]
fn collinear_lines_overlap_over_their_shared_run() {
    let a = Segment::Line(Line::new(p(0.0, 0.0), p(10.0, 0.0)));
    let b = Segment::Line(Line::new(p(5.0, 0.0), p(15.0, 0.0)));
    match meet(a, b, TOL) {
        Ok(Meeting::Overlap(run)) => {
            assert!((run.t0 - 0.5).abs() < 1e-12 && (run.t1 - 1.0).abs() < 1e-12);
            assert!(run.u0.abs() < 1e-12 && (run.u1 - 0.5).abs() < 1e-12);
        }
        other => panic!("expected an overlap, got {other:?}"),
    }
}

#[test]
fn curves_sharing_only_their_ends_do_not_overlap() {
    let a = CubicBez::new(p(0.0, 0.0), p(0.0, 10.0), p(10.0, 10.0), p(10.0, 0.0));
    let b = CubicBez::new(p(0.0, 0.0), p(0.0, -10.0), p(10.0, -10.0), p(10.0, 0.0));
    assert_eq!(overlap(Segment::Cubic(a), Segment::Cubic(b), 1e-7), None);
    match meet(Segment::Cubic(a), Segment::Cubic(b), 1e-7) {
        Ok(Meeting::Crossings(found)) => assert_eq!(found.len(), 2, "{found:?}"),
        other => panic!("expected crossings at the ends, got {other:?}"),
    }
}
