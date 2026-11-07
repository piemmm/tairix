//! Fitting cubics to sampled points: a freehand stroke, or a path simplified.
//!
//! Schneider's algorithm ("An Algorithm for Automatically Fitting Digitized
//! Curves", Graphics Gems, 1990): parameterise the points by chord length,
//! solve the least-squares cubic whose end tangents are fixed, polish the
//! parameters by Newton's method while the fit is close, and otherwise split
//! at the worst point with a tangent shared by both halves, so the chain is
//! smooth wherever it was not cut at a corner.
//!
//! The work is an explicit stack rather than recursion — a stroke of many
//! thousand points may split as many times — and every split strictly
//! shortens a run, so the output is at most one cubic per input point.

use alloc::vec::Vec;

use tairix_util::fallible;
use tairix_util::mathf::fabs;

use crate::cubic::CubicBez;
use crate::point::Point;

/// The most points one fit takes: a containment bound on the work, far past
/// any stroke a hand draws or a path holds.
pub const MAX_FIT_POINTS: usize = 1 << 20;

/// Newton passes spent polishing a fit's parameters before it is split.
const REPARAMETERISE: u32 = 4;

/// Why a fit was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FitError {
    /// More points than [`MAX_FIT_POINTS`].
    TooComplex,
    /// The memory the fit needs could not be had.
    OutOfMemory,
}

/// How one fitted cubic meets the next.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Joint {
    /// Smoothly: the two share a tangent.
    Smooth,
    /// At a corner the points turned, where each piece keeps its own
    /// tangent.
    Corner,
    /// It is the last.
    End,
}

/// Fit cubics through `points` within `tolerance`, handing each to `emit`
/// in order with how it meets the next.
///
/// Points closer than half the tolerance to the last kept are dropped as
/// noise; a turn sharper than `corner_degrees` between the chords either side
/// of a point cuts the run there, so a corner stays a corner. Fewer than two
/// distinct points fit nothing.
///
/// # Errors
///
/// [`FitError::TooComplex`] past [`MAX_FIT_POINTS`], and
/// [`FitError::OutOfMemory`] when the working buffers are refused.
pub fn fit(
    points: &[Point],
    tolerance: f64,
    corner_degrees: f64,
    mut emit: impl FnMut(CubicBez, Joint),
) -> Result<(), FitError> {
    if points.len() > MAX_FIT_POINTS {
        return Err(FitError::TooComplex);
    }
    let tolerance = if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        f64::EPSILON
    };
    let kept = thinned(points, tolerance * 0.5)?;
    if kept.len() < 2 {
        return Ok(());
    }
    let corner_cosine = tairix_util::mathf::cos(corner_degrees.clamp(0.0, 180.0).to_radians());
    let cuts = corners(&kept, corner_cosine)?;
    let mut fitter = Fitter {
        points: &kept,
        error: tolerance * tolerance,
        params: Vec::new(),
        polished: Vec::new(),
        pending: Vec::new(),
    };
    let last_run = cuts.len() - 2;
    for (run, pair) in cuts.windows(2).enumerate() {
        let (first, last) = (pair[0], pair[1]);
        let joint = if run == last_run {
            Joint::End
        } else {
            Joint::Corner
        };
        fitter.run(first, last, joint, &mut emit)?;
    }
    Ok(())
}

/// `points` with every point nearer than `spacing` to the last kept dropped,
/// the final point kept whatever its distance.
fn thinned(points: &[Point], spacing: f64) -> Result<Vec<Point>, FitError> {
    let mut kept = Vec::new();
    if !fallible::reserve(&mut kept, points.len()) {
        return Err(FitError::OutOfMemory);
    }
    for &point in points {
        if !point.is_finite() {
            continue;
        }
        match kept.last() {
            Some(&last) if point.distance(last) < spacing => {}
            _ => kept.push(point),
        }
    }
    // The stroke ends where it ended, though the last point fell within the
    // spacing of the one before.
    let held = kept.len();
    if let (Some(&final_point), Some(&last)) = (points.last(), kept.last()) {
        if final_point.is_finite() && last != final_point {
            if held > 1 {
                kept[held - 1] = final_point;
            } else {
                kept.push(final_point);
            }
        }
    }
    Ok(kept)
}

/// The indices runs are cut at: the first point, every corner, the last.
fn corners(points: &[Point], corner_cosine: f64) -> Result<Vec<usize>, FitError> {
    let mut cuts = Vec::new();
    if !fallible::push(&mut cuts, 0) {
        return Err(FitError::OutOfMemory);
    }
    for i in 1..points.len() - 1 {
        let incoming = (points[i] - points[i - 1]).normalised();
        let outgoing = (points[i + 1] - points[i]).normalised();
        if let (Some(incoming), Some(outgoing)) = (incoming, outgoing) {
            if incoming.dot(outgoing) < corner_cosine && !fallible::push(&mut cuts, i) {
                return Err(FitError::OutOfMemory);
            }
        }
    }
    if !fallible::push(&mut cuts, points.len() - 1) {
        return Err(FitError::OutOfMemory);
    }
    Ok(cuts)
}

/// One fit's working state, reused across its runs.
struct Fitter<'p> {
    points: &'p [Point],
    /// The squared tolerance.
    error: f64,
    params: Vec<f64>,
    polished: Vec<f64>,
    pending: Vec<Span>,
}

/// A run of points still to fit, and the tangents its ends must keep.
#[derive(Copy, Clone)]
struct Span {
    first: usize,
    last: usize,
    leaving: Point,
    arriving: Point,
    /// How the cubic ending this span meets the next.
    joint: Joint,
}

impl Fitter<'_> {
    /// Fit the run `first..=last`, its last cubic meeting the next as
    /// `joint`.
    fn run(
        &mut self,
        first: usize,
        last: usize,
        joint: Joint,
        emit: &mut impl FnMut(CubicBez, Joint),
    ) -> Result<(), FitError> {
        let points = self.points;
        let leaving = (points[first + 1] - points[first])
            .normalised()
            .unwrap_or(Point::new(1.0, 0.0));
        let arriving = (points[last - 1] - points[last])
            .normalised()
            .unwrap_or(Point::new(-1.0, 0.0));
        self.pending.clear();
        if !fallible::push(
            &mut self.pending,
            Span {
                first,
                last,
                leaving,
                arriving,
                joint,
            },
        ) {
            return Err(FitError::OutOfMemory);
        }
        while let Some(span) = self.pending.pop() {
            match self.fit_span(span)? {
                Fitted::Done(curve) => emit(curve, span.joint),
                Fitted::Split(split) => {
                    // The tangent shared by both halves, pointing back
                    // along the run as an arriving tangent does.
                    let centre = (points[split - 1] - points[split + 1])
                        .normalised()
                        .unwrap_or(-span.leaving);
                    let right = Span {
                        first: split,
                        last: span.last,
                        leaving: -centre,
                        arriving: span.arriving,
                        joint: span.joint,
                    };
                    let left = Span {
                        first: span.first,
                        last: split,
                        leaving: span.leaving,
                        arriving: centre,
                        joint: Joint::Smooth,
                    };
                    // The left half is popped first, so cubics leave in order.
                    if !fallible::push(&mut self.pending, right)
                        || !fallible::push(&mut self.pending, left)
                    {
                        return Err(FitError::OutOfMemory);
                    }
                }
            }
        }
        Ok(())
    }

    /// The cubic for `span` where one fits within the tolerance, else the
    /// point to split it at.
    fn fit_span(&mut self, span: Span) -> Result<Fitted, FitError> {
        let points = &self.points[span.first..=span.last];
        if points.len() == 2 {
            return Ok(Fitted::Done(two_point(points, span)));
        }
        self.chord_lengths(points)?;
        // Polish the parameters while each pass brings the fit closer, and
        // split at the worst point once one does not.
        let mut previous = f64::INFINITY;
        let mut split = points.len() / 2;
        for pass in 0..=REPARAMETERISE {
            let curve = least_squares(points, &self.params, span);
            let (worst, at) = max_error(points, &self.params, curve);
            if worst < self.error {
                return Ok(Fitted::Done(curve));
            }
            split = at;
            if worst >= previous || pass == REPARAMETERISE {
                break;
            }
            previous = worst;
            if !self.reparameterise(points, curve)? {
                break;
            }
            core::mem::swap(&mut self.params, &mut self.polished);
        }
        Ok(Fitted::Split(span.first + split))
    }

    /// Parameterise `points` by the share of the chord length to each.
    fn chord_lengths(&mut self, points: &[Point]) -> Result<(), FitError> {
        self.params.clear();
        if !fallible::reserve(&mut self.params, points.len()) {
            return Err(FitError::OutOfMemory);
        }
        let mut along = 0.0;
        self.params.push(0.0);
        for pair in points.windows(2) {
            along += pair[0].distance(pair[1]);
            self.params.push(along);
        }
        if along > 0.0 {
            for param in &mut self.params {
                *param /= along;
            }
        }
        Ok(())
    }

    /// One Newton step on each parameter towards its point's nearest on
    /// `curve`, into `self.polished`, answering whether they still run in
    /// order: parameters out of order would fold the curve back on itself.
    fn reparameterise(&mut self, points: &[Point], curve: CubicBez) -> Result<bool, FitError> {
        self.polished.clear();
        if !fallible::reserve(&mut self.polished, points.len()) {
            return Err(FitError::OutOfMemory);
        }
        for (&point, &u) in points.iter().zip(&self.params) {
            let offset = curve.eval(u) - point;
            let velocity = curve.deriv(u);
            let slope = velocity.length_squared() + offset.dot(curve.second_deriv(u));
            let next = if slope == 0.0 || !slope.is_finite() {
                u
            } else {
                (u - offset.dot(velocity) / slope).clamp(0.0, 1.0)
            };
            self.polished.push(next);
        }
        Ok(self.polished.windows(2).all(|pair| pair[0] <= pair[1]))
    }
}

/// What fitting one span came to.
enum Fitted {
    /// One cubic fits it.
    Done(CubicBez),
    /// It splits at this point.
    Split(usize),
}

/// The cubic between two points along their tangents, a third of the chord
/// each.
fn two_point(points: &[Point], span: Span) -> CubicBez {
    let (start, end) = (points[0], points[points.len() - 1]);
    let third = start.distance(end) / 3.0;
    CubicBez::new(
        start,
        start + span.leaving * third,
        end + span.arriving * third,
        end,
    )
}

/// The least-squares cubic through `points` at `params` whose end tangents
/// are the span's, falling back to a third of the chord where the solution
/// would turn a handle backwards.
fn least_squares(points: &[Point], params: &[f64], span: Span) -> CubicBez {
    let (start, end) = (points[0], points[points.len() - 1]);
    let (mut c00, mut c01, mut c11, mut x0, mut x1) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (&point, &u) in points.iter().zip(params) {
        let rest = 1.0 - u;
        let (b0, b1, b2, b3) = (
            rest * rest * rest,
            3.0 * rest * rest * u,
            3.0 * rest * u * u,
            u * u * u,
        );
        let a0 = span.leaving * b1;
        let a1 = span.arriving * b2;
        c00 += a0.dot(a0);
        c01 += a0.dot(a1);
        c11 += a1.dot(a1);
        let residual = point - (start * (b0 + b1) + end * (b2 + b3));
        x0 += a0.dot(residual);
        x1 += a1.dot(residual);
    }
    let determinant = c00 * c11 - c01 * c01;
    let (mut alpha_left, mut alpha_right) = if fabs(determinant) > f64::EPSILON * c00 * c11 {
        (
            (x0 * c11 - x1 * c01) / determinant,
            (c00 * x1 - c01 * x0) / determinant,
        )
    } else {
        (0.0, 0.0)
    };
    let chord = start.distance(end);
    let floor = 1e-6 * chord;
    if alpha_left < floor
        || alpha_right < floor
        || !alpha_left.is_finite()
        || !alpha_right.is_finite()
    {
        alpha_left = chord / 3.0;
        alpha_right = chord / 3.0;
    }
    CubicBez::new(
        start,
        start + span.leaving * alpha_left,
        end + span.arriving * alpha_right,
        end,
    )
}

/// The greatest squared distance from a point to `curve` at its parameter,
/// and the index of that point, never an end.
fn max_error(points: &[Point], params: &[f64], curve: CubicBez) -> (f64, usize) {
    let mut worst = (0.0, points.len() / 2);
    for (index, (&point, &u)) in points.iter().zip(params).enumerate() {
        if index == 0 || index + 1 == points.len() {
            continue;
        }
        let distance = curve.eval(u).distance_squared(point);
        if distance > worst.0 {
            worst = (distance, index);
        }
    }
    worst
}

#[cfg(test)]
#[path = "fit_tests.rs"]
mod tests;
