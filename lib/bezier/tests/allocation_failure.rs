//! The geometry under real allocation failure.
//!
//! Every operation that makes a path runs with the shared metered allocator
//! refusing each of its requests in turn: each refusal is answered as one,
//! never an abort, and nothing the work held outlives its answer.

use core::f64::consts::TAU;

use tairix_bezier::boolean::{boolean, components, divide, simplified, BooleanError, Operation};
use tairix_bezier::fit::{fitted, FitError};
use tairix_bezier::offset::{dashed, grown, outline, Cap, Join, StrokeStyle};
use tairix_bezier::{Affine, EllipseArc, FillRule, Path, PathEl, Point, Subpath};
use tairix_fuzzseed::meter::refused_at_each;

#[global_allocator]
static ALLOCATOR: tairix_fuzzseed::meter::Metered = tairix_fuzzseed::meter::Metered;

fn disc(centre: Point, radius: f64) -> Subpath {
    let arc = EllipseArc {
        centre,
        radii: Point::new(radius, radius),
        x_rotation: 0.0,
        start: 0.0,
        sweep: TAU,
    };
    let mut disc = Subpath::new(arc.start_point());
    arc.to_cubics(|c| disc.elements.push(PathEl::Cubic(c.p1, c.p2, c.p3)));
    disc.closed = true;
    disc
}

fn square(min: Point, side: f64) -> Subpath {
    let mut square = Subpath::new(min);
    for corner in [
        Point::new(min.x + side, min.y),
        Point::new(min.x + side, min.y + side),
        Point::new(min.x, min.y + side),
    ] {
        square.elements.push(PathEl::Line(corner));
    }
    square.closed = true;
    square
}

/// A ring and a square across its rim: curves crossing lines, and a hole.
fn operands() -> (Path, Path) {
    let ring = Path {
        subpaths: vec![
            disc(Point::new(50.0, 50.0), 40.0),
            disc(Point::new(50.0, 50.0), 15.0).reversed().expect("hole"),
        ],
    };
    let block = Path {
        subpaths: vec![square(Point::new(60.0, 30.0), 50.0)],
    };
    (ring, block)
}

/// An open curve with a sharp corner, then a path of no length.
fn stroked() -> Path {
    let mut wave = Subpath::new(Point::new(0.0, 0.0));
    wave.elements.push(PathEl::Cubic(
        Point::new(20.0, -30.0),
        Point::new(40.0, 30.0),
        Point::new(60.0, 0.0),
    ));
    wave.elements.push(PathEl::Line(Point::new(60.0, 40.0)));
    let mut dot = Subpath::new(Point::new(90.0, 90.0));
    dot.elements.push(PathEl::Line(Point::new(90.0, 90.0)));
    Path {
        subpaths: vec![wave, dot],
    }
}

/// Whether a refusal is the heap's.
fn out_of_memory() -> impl Fn(&BooleanError) -> bool {
    |error| *error == BooleanError::OutOfMemory
}

#[test]
fn a_boolean_as_the_heap_runs_out_is_refused_never_aborted() {
    let (ring, block) = operands();
    for operation in [
        Operation::Union,
        Operation::Intersection,
        Operation::Difference,
        Operation::Exclusion,
    ] {
        refused_at_each(
            || {
                boolean(
                    &ring,
                    FillRule::NonZero,
                    &block,
                    FillRule::EvenOdd,
                    operation,
                )
            },
            out_of_memory(),
        );
    }
    refused_at_each(|| simplified(&ring, FillRule::NonZero), out_of_memory());
    refused_at_each(
        || divide(&ring, FillRule::NonZero, &block, FillRule::NonZero),
        out_of_memory(),
    );
    let pieces = boolean(
        &ring,
        FillRule::NonZero,
        &block,
        FillRule::NonZero,
        Operation::Exclusion,
    )
    .expect("room to spare");
    refused_at_each(|| components(&pieces), out_of_memory());
}

#[test]
fn a_stroke_outlined_dashed_or_grown_as_the_heap_runs_out_is_refused_never_aborted() {
    let path = stroked();
    for (join, cap) in [
        (Join::Round, Cap::Round),
        (Join::Miter, Cap::Square),
        (Join::Bevel, Cap::Butt),
    ] {
        let style = StrokeStyle {
            width: 6.0,
            join,
            miter_limit: 4.0,
            start_cap: cap,
            end_cap: cap,
        };
        refused_at_each(|| outline(&path, &style, 0.05), out_of_memory());
    }
    refused_at_each(|| dashed(&path, &[8.0, 3.0, 0.0], 2.0), out_of_memory());
    refused_at_each(|| dashed(&path, &[], 0.0), out_of_memory());
    let (ring, _) = operands();
    for distance in [4.0, -4.0] {
        refused_at_each(
            || grown(&ring, FillRule::NonZero, distance, Join::Round, 4.0, 0.05),
            out_of_memory(),
        );
    }
}

#[test]
fn a_path_copied_moved_reversed_or_flattened_as_the_heap_runs_out_is_refused() {
    let (ring, _) = operands();
    let any = |_: &_| true;
    refused_at_each(|| ring.try_clone(), any);
    refused_at_each(|| ring.transformed(Affine::rotate_degrees(30.0)), any);
    refused_at_each(|| ring.reversed(), any);
    refused_at_each(|| Path::single(ring.subpaths[0].try_clone()?), any);
    refused_at_each(
        || {
            let mut points = Vec::new();
            ring.subpaths[0].flatten(0.1, &mut points).map(|()| points)
        },
        any,
    );
}

#[test]
fn a_fit_as_the_heap_runs_out_is_refused_never_aborted() {
    let points: Vec<Point> = (0..=64)
        .map(|step| {
            let t = f64::from(step) / 64.0 * TAU * 0.75;
            Point::new(
                50.0 * tairix_util::mathf::cos(t),
                50.0 * tairix_util::mathf::sin(t),
            )
        })
        .collect();
    refused_at_each(
        || fitted(&points, 0.1, 60.0),
        |error| *error == FitError::OutOfMemory,
    );
}
