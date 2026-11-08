//! Deterministic fuzz harness for the geometry kernel's algorithms that take
//! geometry from untrusted documents — boolean operations, stroke outlines,
//! dashes, offsets, curve fitting and segment intersection: none panics,
//! none answers a non-finite point, and each stays within its bounds. A plain
//! `cargo test` runs a fixed sweep from a fresh, logged seed; `cargo xtask
//! fuzz` extends it to a wall-clock budget.

use tairix_bezier::boolean::{boolean, simplified, Operation};
use tairix_bezier::fit::fit;
use tairix_bezier::intersect::meet;
use tairix_bezier::offset::{dashed, grown, outline, Cap, Join, StrokeStyle};
use tairix_bezier::{CubicBez, FillRule, Line, Path, PathEl, Point, Segment, Subpath};
use tairix_fuzzseed::Prng;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 400;

/// The scales coordinates are drawn at, so the kernel meets the tiny, the
/// ordinary and the huge.
const SCALES: [f64; 5] = [1e-6, 1.0, 100.0, 1e4, 1e8];

/// A coordinate at `scale`, sometimes a value an earlier one repeats exactly
/// so that points, edges and curves coincide.
fn coordinate(rng: &mut Prng, scale: f64, reuse: &mut Vec<f64>) -> f64 {
    if !reuse.is_empty() && rng.below(3) == 0 {
        return *rng.pick(reuse);
    }
    let value = (f64::from(rng.next_u32()) / f64::from(u32::MAX) - 0.5) * scale;
    reuse.push(value);
    value
}

fn random_path(rng: &mut Prng) -> Path {
    let scale = *rng.pick(&SCALES);
    let mut reuse = Vec::new();
    let mut path = Path::new();
    for _ in 0..=rng.below(3) {
        let mut point = |rng: &mut Prng| {
            Point::new(
                coordinate(rng, scale, &mut reuse),
                coordinate(rng, scale, &mut reuse),
            )
        };
        let mut subpath = Subpath::new(point(rng));
        for _ in 0..rng.below(8) {
            subpath.elements.push(if rng.below(2) == 0 {
                PathEl::Line(point(rng))
            } else {
                PathEl::Cubic(point(rng), point(rng), point(rng))
            });
        }
        subpath.closed = rng.below(2) == 0;
        path.subpaths.push(subpath);
    }
    path
}

fn rule(rng: &mut Prng) -> FillRule {
    if rng.below(2) == 0 {
        FillRule::NonZero
    } else {
        FillRule::EvenOdd
    }
}

#[track_caller]
fn assert_finite(path: &Path) {
    for subpath in &path.subpaths {
        assert!(subpath.start.is_finite());
        for element in &subpath.elements {
            let points = match *element {
                PathEl::Line(to) => [to, to, to],
                PathEl::Cubic(c1, c2, to) => [c1, c2, to],
            };
            assert!(points.iter().all(|p| p.is_finite()), "{element:?}");
        }
    }
}

#[test]
fn the_geometry_kernel_never_panics_for_any_geometry() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_geometry_kernel_never_panics_for_any_geometry",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let operations = [
        Operation::Union,
        Operation::Intersection,
        Operation::Difference,
        Operation::Exclusion,
    ];
    let mut iteration: u64 = 0;
    loop {
        let a = random_path(&mut rng);
        // Sometimes the second operand is the first, or the first moved by
        // nothing at all, so whole outlines coincide.
        let b = match rng.below(4) {
            0 => a.clone(),
            1 => a.reversed().expect("reversed"),
            _ => random_path(&mut rng),
        };
        let operation = *rng.pick(&operations);
        if let Ok(result) = boolean(&a, rule(&mut rng), &b, rule(&mut rng), operation) {
            assert_finite(&result);
        }
        if let Ok(result) = simplified(&a, rule(&mut rng)) {
            assert_finite(&result);
        }
        let style = StrokeStyle {
            width: f64::from(rng.next_u32() % 100) / 10.0,
            join: *rng.pick(&[Join::Miter, Join::Round, Join::Bevel]),
            miter_limit: f64::from(rng.next_u32() % 10),
            start_cap: *rng.pick(&[Cap::Butt, Cap::Round, Cap::Square]),
            end_cap: *rng.pick(&[Cap::Butt, Cap::Round, Cap::Square]),
        };
        if let Ok(result) = outline(&a, &style, 1e-3) {
            assert_finite(&result);
        }
        let pattern = [
            f64::from(rng.next_u32() % 50),
            f64::from(rng.next_u32() % 50),
        ];
        if let Ok(result) = dashed(&a, &pattern, f64::from(rng.next_u32() % 100)) {
            assert_finite(&result);
        }
        let distance = f64::from(rng.next_u32() % 200) / 10.0 - 10.0;
        if let Ok(result) = grown(&a, rule(&mut rng), distance, style.join, 4.0, 1e-3) {
            assert_finite(&result);
        }
        let points: Vec<Point> = a
            .subpaths
            .iter()
            .flat_map(Subpath::segments)
            .flat_map(|s| (0..=8).map(move |i| s.eval(f64::from(i) / 8.0)))
            .collect();
        let _ = fit(&points, f64::from(rng.next_u32() % 10), 60.0, |curve, _| {
            assert!(curve.p0.is_finite() && curve.p3.is_finite());
        });
        let segments: Vec<Segment> = a.segments().chain(b.segments()).collect();
        if segments.len() >= 2 {
            let (x, y) = (*rng.pick(&segments), *rng.pick(&segments));
            let _ = meet(x, y, 1e-9);
        }
        let _ = meet(
            Segment::Line(Line::new(Point::ZERO, Point::new(1.0, 1.0))),
            Segment::Cubic(CubicBez::new(
                Point::ZERO,
                Point::new(1.0, 1.0),
                Point::ZERO,
                Point::new(1.0, 1.0),
            )),
            1e-9,
        );
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
