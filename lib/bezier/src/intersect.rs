//! Where segments meet: crossings and coincident runs.
//!
//! Every answer is a pair of parameters, one on each segment, so a caller
//! splitting both at a crossing splits them at the same point. A line meets
//! a line exactly; a line meets a cubic at the roots of one polynomial, the
//! cubic's distance from the line; two cubics meet where subdividing both
//! leaves overlapping hulls too flat to tell from their chords, refined by
//! Newton's method in both parameters. Two segments lying along each other
//! do not cross at points at all, so a coincident run is found first and
//! answered as the parameter ranges it spans.

use tairix_inline::ArrayVec;
use tairix_util::mathf::{fabs, fmax, fmin};

use crate::bounds::Bounds;
use crate::cubic::CubicBez;
use crate::line::Line;
use crate::point::Point;
use crate::roots;
use crate::segment::Segment;

/// The most crossings two segments report: two cubics meet at most nine
/// times unless they overlap, which is answered as a run instead.
pub const MAX_CROSSINGS: usize = 9;

/// How many pairs of sub-curves a cubic–cubic search examines before it
/// gives up: a containment bound on the work, which only curves that run
/// alongside each other for their whole length without coinciding reach.
const MAX_PAIRS: u32 = 4096;

/// How deeply the cubic–cubic search subdivides.
const MAX_DEPTH: u32 = 40;

/// Newton steps polishing each cubic–cubic crossing.
const POLISH_STEPS: u32 = 6;

/// Parameter slack within which an end of one segment counts as on the
/// other's end.
const PARAMETER_SLACK: f64 = 1e-9;

/// Where two segments meet: a parameter on each.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Crossing {
    /// The parameter on the first segment.
    pub t: f64,
    /// The parameter on the second segment.
    pub u: f64,
}

/// A run along which two segments coincide: the parameter range it spans on
/// each, `u0` meeting `t0` and `u1` meeting `t1` — so `u1 < u0` where they
/// run in opposite directions.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Overlap {
    /// Where the run begins on the first segment.
    pub t0: f64,
    /// Where it ends on the first segment.
    pub t1: f64,
    /// The second segment's parameter meeting `t0`.
    pub u0: f64,
    /// The second segment's parameter meeting `t1`.
    pub u1: f64,
}

/// What two segments share.
#[derive(Clone, Debug, PartialEq)]
pub enum Meeting {
    /// They cross or touch at these points.
    Crossings(ArrayVec<Crossing, MAX_CROSSINGS>),
    /// They lie along each other over this run, and share nothing else.
    Overlap(Overlap),
}

/// The search ran past its bound on the work: the two curves run so close
/// along each other without coinciding that no crossing can be told apart.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TooClose;

/// What segments `a` and `b` share, where `tolerance` is the distance
/// within which two points count as one.
///
/// # Errors
///
/// [`TooClose`] when two cubics run alongside each other too closely for
/// the search to separate their crossings within its bound.
pub fn meet(a: Segment, b: Segment, tolerance: f64) -> Result<Meeting, TooClose> {
    if !a.bounds().inflated(tolerance).intersects(b.bounds()) {
        return Ok(Meeting::Crossings(ArrayVec::new()));
    }
    if let Some(overlap) = overlap(a, b, tolerance) {
        return Ok(Meeting::Overlap(overlap));
    }
    let mut crossings = ArrayVec::new();
    match (a, b) {
        (Segment::Line(a), Segment::Line(b)) => {
            if let Some(crossing) = line_line(a, b, tolerance) {
                let _ = crossings.try_push(crossing);
            }
        }
        (Segment::Line(line), Segment::Cubic(cubic)) => {
            for crossing in line_cubic(line, cubic, tolerance) {
                let _ = crossings.try_push(crossing);
            }
        }
        (Segment::Cubic(cubic), Segment::Line(line)) => {
            for crossing in line_cubic(line, cubic, tolerance) {
                let _ = crossings.try_push(Crossing {
                    t: crossing.u,
                    u: crossing.t,
                });
            }
        }
        (Segment::Cubic(a), Segment::Cubic(b)) => {
            crossings = cubic_cubic(a, b, tolerance)?;
        }
    }
    crossings.sort_unstable_by(|x, y| x.t.total_cmp(&y.t));
    Ok(Meeting::Crossings(crossings))
}

/// Where two lines cross or touch, to within `tolerance` at their ends.
/// Parallel lines report nothing: a collinear run is an [`Overlap`].
#[must_use]
pub fn line_line(a: Line, b: Line, tolerance: f64) -> Option<Crossing> {
    let (da, db) = (a.deriv(), b.deriv());
    let denominator = da.cross(db);
    let scale = da.length() * db.length();
    if scale == 0.0 || fabs(denominator) <= 1e-12 * scale {
        return None;
    }
    let gap = b.p0 - a.p0;
    let t = gap.cross(db) / denominator;
    let u = gap.cross(da) / denominator;
    let slack_t = tolerance / da.length();
    let slack_u = tolerance / db.length();
    let within = |value: f64, slack: f64| value >= -slack && value <= 1.0 + slack;
    (within(t, slack_t) && within(u, slack_u)).then(|| Crossing {
        t: t.clamp(0.0, 1.0),
        u: u.clamp(0.0, 1.0),
    })
}

/// Where a line meets a cubic, `t` on the line and `u` on the cubic.
#[must_use]
pub fn line_cubic(line: Line, cubic: CubicBez, tolerance: f64) -> ArrayVec<Crossing, 3> {
    let mut found = ArrayVec::new();
    let direction = line.deriv();
    let length = direction.length();
    if length == 0.0 {
        return found;
    }
    // The cubic's signed distance from the line, a cubic polynomial in u.
    let normal = direction.perpendicular() / length;
    let (cubed, squared, linear, constant) = cubic.coefficients();
    let distance = |p: Point| p.dot(normal);
    let roots = roots::cubic_in_unit(
        distance(cubed),
        distance(squared),
        distance(linear),
        distance(constant) - line.p0.dot(normal),
    );
    let slack = tolerance / length;
    for u in roots {
        let point = cubic.eval(u);
        let t = (point - line.p0).dot(direction) / (length * length);
        if t >= -slack && t <= 1.0 + slack {
            let _ = found.try_push(Crossing {
                t: t.clamp(0.0, 1.0),
                u,
            });
        }
    }
    found
}

/// Where two cubics cross.
///
/// # Errors
///
/// [`TooClose`] past the bound on sub-curve pairs examined.
pub fn cubic_cubic(
    a: CubicBez,
    b: CubicBez,
    tolerance: f64,
) -> Result<ArrayVec<Crossing, MAX_CROSSINGS>, TooClose> {
    let mut search = Search {
        tolerance: fmax(tolerance, f64::EPSILON),
        pairs: 0,
        found: ArrayVec::new(),
        whole: (a, b),
    };
    search.recurse(Piece::whole(a), Piece::whole(b), 0)?;
    Ok(search.found)
}

/// A sub-curve of a cubic and the parameter range of the whole it spans.
#[derive(Copy, Clone)]
struct Piece {
    curve: CubicBez,
    from: f64,
    to: f64,
}

impl Piece {
    const fn whole(curve: CubicBez) -> Self {
        Self {
            curve,
            from: 0.0,
            to: 1.0,
        }
    }

    fn halves(self) -> (Self, Self) {
        let (left, right) = self.curve.split(0.5);
        let middle = f64::midpoint(self.from, self.to);
        (
            Self {
                curve: left,
                from: self.from,
                to: middle,
            },
            Self {
                curve: right,
                from: middle,
                to: self.to,
            },
        )
    }

    fn at(self, local: f64) -> f64 {
        self.from + (self.to - self.from) * local
    }
}

struct Search {
    tolerance: f64,
    pairs: u32,
    found: ArrayVec<Crossing, MAX_CROSSINGS>,
    whole: (CubicBez, CubicBez),
}

impl Search {
    fn recurse(&mut self, a: Piece, b: Piece, depth: u32) -> Result<(), TooClose> {
        self.pairs += 1;
        if self.pairs > MAX_PAIRS {
            return Err(TooClose);
        }
        let (hull_a, hull_b) = (a.curve.control_bounds(), b.curve.control_bounds());
        if !hull_a.inflated(self.tolerance).intersects(hull_b) {
            return Ok(());
        }
        let flat_a = a.curve.flatness() <= self.tolerance;
        let flat_b = b.curve.flatness() <= self.tolerance;
        if depth >= MAX_DEPTH || (flat_a && flat_b) {
            let chord_a = Line::new(a.curve.p0, a.curve.p3);
            let chord_b = Line::new(b.curve.p0, b.curve.p3);
            if let Some(crossing) = line_line(chord_a, chord_b, self.tolerance) {
                self.record(a.at(crossing.t), b.at(crossing.u));
            }
            return Ok(());
        }
        if !flat_a && (flat_b || extent(hull_a) >= extent(hull_b)) {
            let (left, right) = a.halves();
            self.recurse(left, b, depth + 1)?;
            self.recurse(right, b, depth + 1)
        } else {
            let (left, right) = b.halves();
            self.recurse(a, left, depth + 1)?;
            self.recurse(a, right, depth + 1)
        }
    }

    /// Polish the crossing at `(t, u)` and keep it unless it repeats one
    /// already found.
    fn record(&mut self, t: f64, u: f64) {
        let (t, u) = polish(self.whole, t, u);
        let at = self.whole.0.eval(t);
        let repeats = self.found.iter().any(|found| {
            self.whole.0.eval(found.t).distance(at) <= self.tolerance
                || (fabs(found.t - t) <= PARAMETER_SLACK && fabs(found.u - u) <= PARAMETER_SLACK)
        });
        if !repeats {
            let _ = self.found.try_push(Crossing { t, u });
        }
    }
}

/// The larger side of `bounds`.
fn extent(bounds: Bounds) -> f64 {
    fmax(bounds.width(), bounds.height())
}

/// Newton's method on `a(t) - b(u) = 0` from `(t, u)`, kept to the unit
/// square and only while each step brings the two points closer.
fn polish((a, b): (CubicBez, CubicBez), mut t: f64, mut u: f64) -> (f64, f64) {
    let mut gap = a.eval(t).distance_squared(b.eval(u));
    for _ in 0..POLISH_STEPS {
        let residual = a.eval(t) - b.eval(u);
        let (da, db) = (a.deriv(t), b.deriv(u));
        // Solve [da, -db]·(Δt, Δu) = -residual.
        let determinant = -da.cross(db);
        if determinant == 0.0 || !determinant.is_finite() {
            break;
        }
        let step_t = (residual.x * db.y - residual.y * db.x) / determinant;
        let step_u = (residual.x * da.y - residual.y * da.x) / determinant;
        let (next_t, next_u) = ((t + step_t).clamp(0.0, 1.0), (u + step_u).clamp(0.0, 1.0));
        let next_gap = a.eval(next_t).distance_squared(b.eval(next_u));
        if next_gap >= gap || !next_gap.is_finite() {
            break;
        }
        (t, u, gap) = (next_t, next_u, next_gap);
    }
    (t, u)
}

/// The run along which `a` and `b` coincide, to within `tolerance`, if they
/// do over more than a point.
///
/// Two segments coincide over a run exactly when each end of the run is an
/// end of one segment lying on the other, and the two pieces between agree:
/// so the candidate ends are found by projecting each segment's ends onto
/// the other, and the pieces compared point for point.
#[must_use]
pub fn overlap(a: Segment, b: Segment, tolerance: f64) -> Option<Overlap> {
    let straight_a = is_straight(a, tolerance);
    let straight_b = is_straight(b, tolerance);
    if straight_a != straight_b {
        return None;
    }
    let mut ends: ArrayVec<(f64, f64), 4> = ArrayVec::new();
    let mut add = |t: f64, u: f64| {
        let repeated = ends
            .iter()
            .any(|&(et, eu)| fabs(et - t) <= PARAMETER_SLACK && fabs(eu - u) <= PARAMETER_SLACK);
        if !repeated {
            let _ = ends.try_push((t, u));
        }
    };
    let on = |segment: Segment, point: Point| {
        let (t, distance_squared) = segment.nearest(point);
        (distance_squared <= tolerance * tolerance).then_some(t)
    };
    for (t, point) in [(0.0, a.start()), (1.0, a.end())] {
        if let Some(u) = on(b, point) {
            add(t, u);
        }
    }
    for (u, point) in [(0.0, b.start()), (1.0, b.end())] {
        if let Some(t) = on(a, point) {
            add(t, u);
        }
    }
    if ends.len() < 2 {
        return None;
    }
    ends.sort_unstable_by(|x, y| x.0.total_cmp(&y.0));
    let (first, last) = (ends[0], ends[ends.len() - 1]);
    if fabs(last.0 - first.0) <= PARAMETER_SLACK || fabs(last.1 - first.1) <= PARAMETER_SLACK {
        return None;
    }
    let piece_a = a.subsegment(first.0, last.0);
    let piece_b = b.subsegment(first.1, last.1);
    if !same_curve(piece_a, piece_b, tolerance) {
        return None;
    }
    Some(Overlap {
        t0: first.0,
        t1: last.0,
        u0: first.1,
        u1: last.1,
    })
}

/// Whether `segment` is a straight line to within `tolerance`.
fn is_straight(segment: Segment, tolerance: f64) -> bool {
    match segment {
        Segment::Line(_) => true,
        Segment::Cubic(cubic) => cubic.is_line(tolerance),
    }
}

/// Whether two pieces trace the same points, run the same way, to within
/// `tolerance`: their ends meet and so do their points between.
fn same_curve(a: Segment, b: Segment, tolerance: f64) -> bool {
    const PROBES: u32 = 8;
    (0..=PROBES).all(|step| {
        let t = f64::from(step) / f64::from(PROBES);
        // A point of one must lie on the other, wherever its parameter puts
        // it: two traces of one curve need not share a parameterisation.
        let point = a.eval(t);
        b.nearest(point).1 <= tolerance * tolerance * 4.0
    }) && a.start().distance(b.start()) <= tolerance * 2.0
        && a.end().distance(b.end()) <= tolerance * 2.0
}

/// The parameter range of `t0..t1` held to the unit interval and ordered.
#[must_use]
pub fn ordered(t0: f64, t1: f64) -> (f64, f64) {
    (fmin(t0, t1).clamp(0.0, 1.0), fmax(t0, t1).clamp(0.0, 1.0))
}

#[cfg(test)]
#[path = "intersect_tests.rs"]
mod tests;
