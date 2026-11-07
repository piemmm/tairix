//! The cubic Bézier segment.

use tairix_inline::ArrayVec;
use tairix_util::mathf::{fabs, fmax};

use crate::affine::Affine;
use crate::bounds::Bounds;
use crate::point::Point;
use crate::quad::first_direction;
use crate::roots;

/// Gauss–Legendre abscissae and weights on `-1..=1`, eight points, each
/// abscissa standing for itself and its negation.
const GAUSS_LEGENDRE: [(f64, f64); 4] = [
    (0.183_434_642_495_649_8, 0.362_683_783_378_362),
    (0.525_532_409_916_329, 0.313_706_645_877_887_27),
    (0.796_666_477_413_626_7, 0.222_381_034_453_374_48),
    (0.960_289_856_497_536_3, 0.101_228_536_290_376_26),
];

/// How deeply arc length splits a curve to meet its accuracy: a bound on the
/// work, past which the best estimate so far is answered.
const MAX_ARCLEN_DEPTH: u32 = 16;

/// Samples taken along a curve before refining its nearest point.
const NEAREST_SAMPLES: u32 = 16;

/// Newton steps spent refining each candidate nearest point.
const NEAREST_STEPS: u32 = 8;

/// Steps spent inverting arc length: each at least halves the bracket.
const INVERSE_ARCLEN_STEPS: u32 = 52;

/// The spans an [`ArcTable`] measures a curve in.
const ARC_TABLE_SPANS: u32 = 32;

/// The lengths an [`ArcTable`] holds: one at each span's start, and the whole.
const ARC_TABLE_LENGTHS: usize = ARC_TABLE_SPANS as usize + 1;

/// Newton steps an [`ArcTable`] spends within a span.
const ARC_TABLE_STEPS: u32 = 4;

/// A cubic Bézier from `p0` to `p3` drawn towards `p1` and then `p2`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CubicBez {
    /// Where it starts.
    pub p0: Point,
    /// The control it leaves towards.
    pub p1: Point,
    /// The control it arrives from.
    pub p2: Point,
    /// Where it ends.
    pub p3: Point,
}

impl CubicBez {
    /// The cubic from `p0` to `p3` drawn towards `p1` and then `p2`.
    #[must_use]
    pub const fn new(p0: Point, p1: Point, p2: Point, p3: Point) -> Self {
        Self { p0, p1, p2, p3 }
    }

    /// The point at parameter `t`.
    #[must_use]
    pub fn eval(self, t: f64) -> Point {
        let rest = 1.0 - t;
        let weights = [
            rest * rest * rest,
            3.0 * rest * rest * t,
            3.0 * rest * t * t,
            t * t * t,
        ];
        Point::new(
            weights[0] * self.p0.x
                + weights[1] * self.p1.x
                + weights[2] * self.p2.x
                + weights[3] * self.p3.x,
            weights[0] * self.p0.y
                + weights[1] * self.p1.y
                + weights[2] * self.p2.y
                + weights[3] * self.p3.y,
        )
    }

    /// The derivative at parameter `t`.
    #[must_use]
    pub fn deriv(self, t: f64) -> Point {
        let rest = 1.0 - t;
        ((self.p1 - self.p0) * (rest * rest)
            + (self.p2 - self.p1) * (2.0 * rest * t)
            + (self.p3 - self.p2) * (t * t))
            * 3.0
    }

    /// The second derivative at parameter `t`.
    #[must_use]
    pub fn second_deriv(self, t: f64) -> Point {
        let first = self.p2 - self.p1 * 2.0 + self.p0;
        let second = self.p3 - self.p2 * 2.0 + self.p1;
        (first * (1.0 - t) + second * t) * 6.0
    }

    /// The two halves either side of `t`, by de Casteljau's construction.
    #[must_use]
    pub fn split(self, t: f64) -> (Self, Self) {
        let first = [
            self.p0.lerp(self.p1, t),
            self.p1.lerp(self.p2, t),
            self.p2.lerp(self.p3, t),
        ];
        let second = [first[0].lerp(first[1], t), first[1].lerp(first[2], t)];
        let at = second[0].lerp(second[1], t);
        (
            Self::new(self.p0, first[0], second[0], at),
            Self::new(at, second[1], first[2], self.p3),
        )
    }

    /// The part from `t0` to `t1`, run backwards where `t1 < t0`.
    #[must_use]
    pub fn subsegment(self, t0: f64, t1: f64) -> Self {
        if t1 < t0 {
            return self.subsegment(t1, t0).reversed();
        }
        let tail = if t0 <= 0.0 { self } else { self.split(t0).1 };
        if t1 >= 1.0 {
            return tail;
        }
        let rest = 1.0 - t0;
        let within = if rest > 0.0 { (t1 - t0) / rest } else { 0.0 };
        let mut part = tail.split(within).0;
        // The split points are the curve's own; landing them exactly keeps
        // neighbouring pieces continuous.
        part.p0 = self.eval(t0);
        part.p3 = self.eval(t1);
        part
    }

    /// The parameters within `0..1` where either coordinate turns, ascending.
    #[must_use]
    pub fn extrema(self) -> ArrayVec<f64, 4> {
        let mut found: ArrayVec<f64, 4> = ArrayVec::new();
        let (a, b, c) = self.derivative_coefficients();
        for (a, b, c) in [(a.x, b.x, c.x), (a.y, b.y, c.y)] {
            for t in roots::quadratic(a, b, c) {
                if t > 0.0 && t < 1.0 {
                    let _ = found.try_push(t);
                }
            }
        }
        found.sort_unstable_by(f64::total_cmp);
        found
    }

    /// The tight bounds of every point on the curve.
    #[must_use]
    pub fn bounds(self) -> Bounds {
        let mut bounds = Bounds::new(self.p0, self.p3);
        for t in self.extrema() {
            bounds = bounds.union_point(self.eval(t));
        }
        bounds
    }

    /// The bounds of the control polygon, which hold the curve.
    #[must_use]
    pub fn control_bounds(self) -> Bounds {
        Bounds::from_points([self.p0, self.p1, self.p2, self.p3])
    }

    /// The curve run the other way.
    #[must_use]
    pub const fn reversed(self) -> Self {
        Self::new(self.p3, self.p2, self.p1, self.p0)
    }

    /// The curve mapped through `transform`.
    #[must_use]
    pub fn transformed(self, transform: Affine) -> Self {
        Self::new(
            transform.map(self.p0),
            transform.map(self.p1),
            transform.map(self.p2),
            transform.map(self.p3),
        )
    }

    /// The direction it leaves `p0` in and the direction it arrives at `p3`
    /// in.
    ///
    /// The true tangent is the control direction; a control coincident with
    /// its end gives none, so the next point along stands in, which is the
    /// limit of the curve's own direction there.
    #[must_use]
    pub fn tangents(self) -> (Point, Point) {
        (
            first_direction([self.p1 - self.p0, self.p2 - self.p0, self.p3 - self.p0]),
            first_direction([self.p3 - self.p2, self.p3 - self.p1, self.p3 - self.p0]),
        )
    }

    /// The parameter of the point on the curve nearest `p`, and that point's
    /// squared distance from `p`.
    ///
    /// Samples along the curve bracket each local minimum of the distance,
    /// and Newton's method on the distance's derivative refines each within
    /// its bracket, so a curve that passes near `p` twice reports the nearer
    /// pass.
    #[must_use]
    pub fn nearest(self, p: Point) -> (f64, f64) {
        const COUNT: usize = NEAREST_SAMPLES as usize + 1;
        let distance = |t: f64| self.eval(t).distance_squared(p);
        let mut params = [0.0; COUNT];
        let mut distances = [0.0; COUNT];
        for (step, (t, d)) in (0..=NEAREST_SAMPLES).zip(params.iter_mut().zip(distances.iter_mut()))
        {
            *t = f64::from(step) / f64::from(NEAREST_SAMPLES);
            *d = distance(*t);
        }
        let mut best = (0.0, distances[0]);
        for i in 0..COUNT {
            let before = if i == 0 {
                f64::INFINITY
            } else {
                distances[i - 1]
            };
            let after = distances.get(i + 1).copied().unwrap_or(f64::INFINITY);
            if distances[i] < best.1 {
                best = (params[i], distances[i]);
            }
            if distances[i] <= before && distances[i] <= after {
                let low = if i == 0 { 0.0 } else { params[i - 1] };
                let high = if i + 1 == COUNT { 1.0 } else { params[i + 1] };
                let t = self.refine_nearest(p, params[i], (low, high));
                let found = distance(t);
                if found < best.1 {
                    best = (t, found);
                }
            }
        }
        best
    }

    /// Newton's method on `(B(t) - p)·B'(t)` from `t`, held to `bracket`.
    #[allow(
        clippy::float_cmp,
        reason = "a step that lands where it began has converged to the double's resolution"
    )]
    fn refine_nearest(self, p: Point, mut t: f64, (low, high): (f64, f64)) -> f64 {
        for _ in 0..NEAREST_STEPS {
            let offset = self.eval(t) - p;
            let velocity = self.deriv(t);
            let slope = velocity.length_squared() + offset.dot(self.second_deriv(t));
            if slope <= 0.0 || !slope.is_finite() {
                break;
            }
            let next = (t - offset.dot(velocity) / slope).clamp(low, high);
            if next == t {
                break;
            }
            t = next;
        }
        t
    }

    /// Its length, to within `accuracy`.
    #[must_use]
    pub fn arclen(self, accuracy: f64) -> f64 {
        self.arclen_within(fmax(accuracy, f64::EPSILON), MAX_ARCLEN_DEPTH)
    }

    fn arclen_within(self, accuracy: f64, depth: u32) -> f64 {
        let whole = self.gauss_length();
        let (left, right) = self.split(0.5);
        let halves = left.gauss_length() + right.gauss_length();
        if depth == 0 || fabs(whole - halves) <= accuracy {
            return halves;
        }
        left.arclen_within(accuracy / 2.0, depth - 1)
            + right.arclen_within(accuracy / 2.0, depth - 1)
    }

    /// Eight-point Gauss–Legendre quadrature of the speed over `0..=1`.
    fn gauss_length(self) -> f64 {
        self.gauss_length_over(0.0, 1.0)
    }

    /// The parameter `length` along the curve from its start, to within
    /// `accuracy`: `0.0` at or before the start and `1.0` at or past the end.
    ///
    /// A guess from the curve's [`ArcTable`] is refined by Newton's method on
    /// the measured length, held to a bracket that halves wherever a step
    /// would leave it.
    #[must_use]
    pub fn inv_arclen(self, length: f64, accuracy: f64) -> f64 {
        if length <= 0.0 {
            return 0.0;
        }
        let total = self.arclen(accuracy);
        if length >= total {
            return 1.0;
        }
        let (mut low, mut high) = (0.0, 1.0);
        let mut t = self.arc_table().param_at(length);
        for _ in 0..INVERSE_ARCLEN_STEPS {
            let reached = self.subsegment(0.0, t).arclen(accuracy);
            let miss = reached - length;
            if fabs(miss) <= accuracy {
                return t;
            }
            if miss < 0.0 {
                low = t;
            } else {
                high = t;
            }
            let speed = self.deriv(t).length();
            let stepped = t - miss / speed;
            t = if speed > 0.0 && stepped > low && stepped < high {
                stepped
            } else {
                f64::midpoint(low, high)
            };
        }
        t
    }

    /// The curve's length against its parameter, for finding many points
    /// along it in turn.
    #[must_use]
    pub fn arc_table(self) -> ArcTable {
        let mut lengths = [0.0; ARC_TABLE_LENGTHS];
        let mut along = 0.0;
        for (span, slot) in (0..ARC_TABLE_SPANS).zip(lengths.iter_mut().skip(1)) {
            along += self.gauss_length_over(span_start(span), span_start(span + 1));
            *slot = along;
        }
        ArcTable {
            curve: self,
            lengths,
        }
    }

    /// Eight-point Gauss–Legendre quadrature of the speed over `t0..=t1`.
    fn gauss_length_over(self, t0: f64, t1: f64) -> f64 {
        let (middle, half) = (f64::midpoint(t0, t1), (t1 - t0) / 2.0);
        GAUSS_LEGENDRE
            .iter()
            .map(|&(x, w)| {
                let speed = |t: f64| self.deriv(t).length();
                w * (speed(middle + half * x) + speed(middle - half * x))
            })
            .sum::<f64>()
            * half
    }

    /// This segment's share of the signed area of a closed path it belongs
    /// to, positive clockwise as drawn: Green's theorem in closed form over
    /// the Bernstein basis.
    #[must_use]
    pub fn signed_area(self) -> f64 {
        let (p0, p1, p2, p3) = (self.p0, self.p1, self.p2, self.p3);
        (6.0 * p0.cross(p1)
            + 3.0 * p0.cross(p2)
            + p0.cross(p3)
            + 3.0 * p1.cross(p2)
            + 3.0 * p1.cross(p3)
            + 6.0 * p2.cross(p3))
            / 20.0
    }

    /// The greatest distance of either control from the chord, or from the
    /// start where the chord has no length: how far the curve can stray from
    /// a straight line.
    #[must_use]
    pub fn flatness(self) -> f64 {
        let chord = self.p3 - self.p0;
        let length = chord.length();
        if length == 0.0 {
            return fmax(self.p0.distance(self.p1), self.p0.distance(self.p2));
        }
        let off = |control: Point| fabs(chord.cross(control - self.p0)) / length;
        fmax(off(self.p1), off(self.p2))
    }

    /// Whether the curve is straight to within `tolerance` and runs from its
    /// start to its end without doubling back.
    #[must_use]
    pub fn is_line(self, tolerance: f64) -> bool {
        if self.flatness() > tolerance {
            return false;
        }
        let chord = self.p3 - self.p0;
        let length_squared = chord.length_squared();
        if length_squared == 0.0 {
            return true;
        }
        let along = |control: Point| (control - self.p0).dot(chord) / length_squared;
        let within = |t: f64| (0.0..=1.0).contains(&t);
        within(along(self.p1)) && within(along(self.p2))
    }

    /// The power-basis coefficients `(a, b, c, d)` of both coordinates, so
    /// the point at `t` is `a·t³ + b·t² + c·t + d`.
    #[must_use]
    pub fn coefficients(self) -> (Point, Point, Point, Point) {
        let (p0, p1, p2, p3) = (self.p0, self.p1, self.p2, self.p3);
        (
            p3 - p0 + (p1 - p2) * 3.0,
            (p0 - p1 * 2.0 + p2) * 3.0,
            (p1 - p0) * 3.0,
            p0,
        )
    }

    /// The derivative's power-basis coefficients `(a, b, c)` of both
    /// coordinates, up to the common factor of three: `a·t² + b·t + c`.
    fn derivative_coefficients(self) -> (Point, Point, Point) {
        let (p0, p1, p2, p3) = (self.p0, self.p1, self.p2, self.p3);
        (
            p3 - p0 + (p1 - p2) * 3.0,
            (p0 - p1 * 2.0 + p2) * 2.0,
            p1 - p0,
        )
    }
}

/// Where span `span` of an [`ArcTable`] begins in the parameter.
fn span_start(span: u32) -> f64 {
    f64::from(span) / f64::from(ARC_TABLE_SPANS)
}

/// A cubic's length measured against its parameter over a fixed number of
/// spans, each by Gauss–Legendre quadrature, so the parameter a length along
/// it falls at is found by a search of the table and a few Newton steps
/// within one span ([`CubicBez::arc_table`]).
#[derive(Copy, Clone, Debug)]
pub struct ArcTable {
    curve: CubicBez,
    /// The length from the start to each span's start, and the whole length
    /// last.
    lengths: [f64; ARC_TABLE_LENGTHS],
}

impl ArcTable {
    /// The curve's whole length.
    #[must_use]
    pub const fn length(&self) -> f64 {
        self.lengths[ARC_TABLE_LENGTHS - 1]
    }

    /// The parameter `length` along the curve: `0.0` at or before the start
    /// and `1.0` at or past the end.
    #[must_use]
    pub fn param_at(&self, length: f64) -> f64 {
        if length <= 0.0 {
            return 0.0;
        }
        if length >= self.length() {
            return 1.0;
        }
        let index = self
            .lengths
            .partition_point(|&reached| reached <= length)
            .clamp(1, ARC_TABLE_LENGTHS - 1)
            - 1;
        let span = u32::try_from(index).unwrap_or(0);
        let (low, high) = (span_start(span), span_start(span + 1));
        let within = length - self.lengths[index];
        let width = self.lengths[index + 1] - self.lengths[index];
        let mut t = if width > 0.0 {
            low + (high - low) * (within / width)
        } else {
            low
        };
        for _ in 0..ARC_TABLE_STEPS {
            let miss = self.curve.gauss_length_over(low, t) - within;
            let speed = self.curve.deriv(t).length();
            if speed <= 0.0 || !speed.is_finite() {
                break;
            }
            t = (t - miss / speed).clamp(low, high);
        }
        t
    }
}

#[cfg(test)]
#[path = "cubic_tests.rs"]
mod tests;
