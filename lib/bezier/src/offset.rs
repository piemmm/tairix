//! Offset curves, a stroke's outline as a path, dashes, and growing or
//! shrinking a filled path.
//!
//! A cubic's offset is not a cubic, so it is approximated: each end moved
//! along its normal, each handle kept in direction and scaled by the
//! curvature there (`1 − d·κ`, which is how much an offset at distance `d`
//! speeds up or slows down), the approximation measured against the true
//! offset at three points and the curve halved wherever it strays past the
//! tolerance (after Tiller and Hanson, "Offsets of two-dimensional profiles",
//! IEEE CG&A, 1984). A stroke's outline is both sides of its path joined and
//! capped as the style says, and then simplified, which removes what an inner
//! side and a loop of a tight curve's offset leave crossing over.

use alloc::collections::TryReserveError;
use alloc::vec::Vec;

use core::f64::consts::PI;

use tairix_util::fallible::{self, try_push};
use tairix_util::mathf::{fabs, fmax, sqrt};

use crate::arc::{angle_between, EllipseArc};
use crate::boolean::{boolean, simplified, BooleanError, Operation};
use crate::cubic::CubicBez;
use crate::measure::PathMeasure;
use crate::path::{FillRule, Path, PathEl, Subpath};
use crate::point::Point;
use crate::segment::Segment;

/// How deeply an offset halves a curve to meet its tolerance: a bound on the
/// work, met only near a cusp, where no approximation converges and the
/// stroke's simplification removes what is left.
const MAX_OFFSET_DEPTH: u32 = 10;

/// The most pieces dashing may cut a path into: as many as a boolean
/// operation takes, since an outline stroke's dashes are simplified by one.
pub const MAX_DASHES: usize = crate::boolean::MAX_BOOLEAN_SEGMENTS;

/// How a stroke turns a corner on its outer side.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Join {
    /// Its two edges extended until they meet, unless that reaches past the
    /// miter limit, when bevelled.
    #[default]
    Miter,
    /// An arc round the corner.
    Round,
    /// A straight cut across.
    Bevel,
}

/// How a stroke ends.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Cap {
    /// Square at the end.
    #[default]
    Butt,
    /// A half disc past the end.
    Round,
    /// A half square past the end.
    Square,
}

/// What a stroke's outline is built from.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct StrokeStyle {
    /// The stroke's width.
    pub width: f64,
    /// How it turns corners.
    pub join: Join,
    /// The longest miter, in widths, before a miter join is bevelled.
    pub miter_limit: f64,
    /// How an open path's start is capped.
    pub start_cap: Cap,
    /// How an open path's end is capped.
    pub end_cap: Cap,
}

impl Default for StrokeStyle {
    fn default() -> Self {
        Self {
            width: 1.0,
            join: Join::Miter,
            miter_limit: 4.0,
            start_cap: Cap::Butt,
            end_cap: Cap::Butt,
        }
    }
}

/// Hand `emit` the cubics approximating `curve` offset by `distance` —
/// right of it as drawn where positive — to within `tolerance`, in order.
///
/// # Errors
///
/// The first refusal `emit` answers, at which the offset stops.
pub fn offset_cubic<E>(
    curve: CubicBez,
    distance: f64,
    tolerance: f64,
    mut emit: impl FnMut(CubicBez) -> Result<(), E>,
) -> Result<(), E> {
    let tolerance = fmax(tolerance, f64::EPSILON);
    let mut pending: [(CubicBez, u32); MAX_OFFSET_DEPTH as usize + 1] =
        [(curve, 0); MAX_OFFSET_DEPTH as usize + 1];
    let mut held = 1;
    while held > 0 {
        held -= 1;
        let (piece, depth) = pending[held];
        let approximation = approximate_offset(piece, distance);
        if depth >= MAX_OFFSET_DEPTH || offset_error(piece, approximation, distance) <= tolerance {
            emit(approximation)?;
            continue;
        }
        let (left, right) = piece.split(0.5);
        // The left half is taken first, so the pieces leave in order. Each
        // depth holds at most one pending right half, so the stack is never
        // deeper than the depth bound.
        pending[held] = (right, depth + 1);
        pending[held + 1] = (left, depth + 1);
        held += 2;
    }
    Ok(())
}

/// One cubic approximating `curve` offset by `distance`.
fn approximate_offset(curve: CubicBez, distance: f64) -> CubicBez {
    let (leaving, arriving) = curve.tangents();
    let (Some(leaving), Some(arriving)) = (leaving.normalised(), arriving.normalised()) else {
        return curve;
    };
    let start = curve.p0 + leaving.perpendicular() * distance;
    let end = curve.p3 + arriving.perpendicular() * distance;
    let scale_at = |velocity: Point, acceleration: Point| {
        let speed = velocity.length();
        if speed == 0.0 {
            return 1.0;
        }
        let curvature = velocity.cross(acceleration) / (speed * speed * speed);
        fmax(1.0 - distance * curvature, 0.0)
    };
    let scale_start = scale_at(curve.deriv(0.0), curve.second_deriv(0.0));
    let scale_end = scale_at(curve.deriv(1.0), curve.second_deriv(1.0));
    CubicBez::new(
        start,
        start + (curve.p1 - curve.p0) * scale_start,
        end + (curve.p2 - curve.p3) * scale_end,
        end,
    )
}

/// How far `approximation` strays from `curve`'s true offset by `distance`,
/// measured where a quarter, half and three quarters along.
fn offset_error(curve: CubicBez, approximation: CubicBez, distance: f64) -> f64 {
    let mut worst = 0.0_f64;
    for t in [0.25, 0.5, 0.75] {
        let Some(normal) = curve.deriv(t).normalised().map(Point::perpendicular) else {
            continue;
        };
        let target = curve.eval(t) + normal * distance;
        worst = fmax(worst, sqrt(approximation.nearest(target).1));
    }
    worst
}

/// The outline of `path` stroked as `style` says, as a path that fills
/// exactly what the stroke paints, within `tolerance`.
///
/// # Errors
///
/// As [`boolean`]: the simplification that resolves where the outline
/// crosses itself is bounded and may refuse.
pub fn outline(path: &Path, style: &StrokeStyle, tolerance: f64) -> Result<Path, BooleanError> {
    let half = style.width / 2.0;
    if half <= 0.0 || !half.is_finite() {
        return Ok(Path::new());
    }
    let mut contours = Path::new();
    let mut segments: Vec<Segment> = Vec::new();
    for subpath in &path.subpaths {
        segments.clear();
        for segment in subpath.segments() {
            if !is_degenerate(segment) && !fallible::push(&mut segments, segment) {
                return Err(BooleanError::OutOfMemory);
            }
        }
        if segments.is_empty() {
            if let Some(dot) = dot(subpath.start, half, style.start_cap)? {
                try_push(&mut contours.subpaths, dot)?;
            }
            continue;
        }
        let mut reversed: Vec<Segment> = Vec::new();
        if !fallible::reserve(&mut reversed, segments.len()) {
            return Err(BooleanError::OutOfMemory);
        }
        reversed.extend(segments.iter().rev().map(|segment| segment.reversed()));
        let sides = Sides {
            half,
            join: style.join,
            miter_limit: style.miter_limit,
            tolerance,
        };
        if subpath.closed {
            try_push(&mut contours.subpaths, sides.side(&segments, true)?)?;
            try_push(&mut contours.subpaths, sides.side(&reversed, true)?)?;
        } else {
            let mut contour = sides.side(&segments, false)?;
            let end_point = segments[segments.len() - 1].end();
            let end_direction = direction_at_end(segments[segments.len() - 1]);
            cap(&mut contour, end_point, end_direction, half, style.end_cap)?;
            let back = sides.side(&reversed, false)?;
            append(&mut contour, &back)?;
            let start_direction = direction_at_end(reversed[reversed.len() - 1]);
            cap(
                &mut contour,
                segments[0].start(),
                start_direction,
                half,
                style.start_cap,
            )?;
            contour.closed = true;
            try_push(&mut contours.subpaths, contour)?;
        }
    }
    simplified(&contours, FillRule::NonZero)
}

/// `path` with every dash of `pattern`, starting `offset` into it, as an open
/// sub-path of its own: what a dashed stroke paints, before its width.
///
/// An odd-length pattern repeats twice over, as SVG's does; a pattern that
/// draws nothing — empty, negative, non-finite or of no length — leaves the
/// path whole. Each segment is measured once ([`crate::ArcTable`]), so a
/// dash boundary costs a lookup, and a pattern that would cut the path into
/// more than [`MAX_DASHES`] pieces is refused before any is cut.
///
/// # Errors
///
/// [`BooleanError::TooComplex`] past [`MAX_DASHES`] pieces, and
/// [`BooleanError::OutOfMemory`] when they cannot be held.
pub fn dashed(path: &Path, pattern: &[f64], offset: f64) -> Result<Path, BooleanError> {
    let total: f64 = pattern.iter().sum();
    let valid = !pattern.is_empty()
        && pattern.iter().all(|dash| dash.is_finite() && *dash >= 0.0)
        && total > 0.0
        && total.is_finite();
    if !valid {
        return Ok(path.try_clone()?);
    }
    let mut lengths: Vec<f64> = Vec::new();
    let cycle = if pattern.len() % 2 == 1 { 2 } else { 1 };
    for _ in 0..cycle {
        if !fallible::reserve(&mut lengths, pattern.len()) {
            return Err(BooleanError::OutOfMemory);
        }
        lengths.extend_from_slice(pattern);
    }
    let period = total * f64::from(cycle);
    let measure = PathMeasure::new(path)?;
    let mut expected = 0.0;
    let dashes_per_period = count_as_f64(lengths.len() / 2);
    let mut index = 0;
    for subpath in &path.subpaths {
        let segments = subpath.segments().count();
        let along: f64 = (index..index + segments)
            .map(|segment| measure.segment_length(segment))
            .sum();
        index += segments;
        expected += (along / period + 1.0) * dashes_per_period;
        if expected > count_as_f64(MAX_DASHES) || expected.is_nan() {
            return Err(BooleanError::TooComplex);
        }
    }
    let into = offset % period;
    let into = if into < 0.0 { into + period } else { into };
    let mut out = Path::new();
    let mut next = 0;
    for subpath in &path.subpaths {
        let first_dash = out.subpaths.len();
        let mut dasher = Dasher::new(&lengths, into);
        let starts_drawing = dasher.drawing();
        for segment in subpath.segments() {
            dasher.walk(segment, &measure, next, &mut out)?;
            next += 1;
        }
        // A dash running through a closed sub-path's seam is one dash.
        let through_seam = subpath.closed && starts_drawing && out.subpaths.len() > first_dash;
        match dasher.current.take() {
            Some(mut last) if through_seam => {
                let first = &out.subpaths[first_dash];
                if !fallible::reserve(&mut last.elements, first.elements.len()) {
                    return Err(BooleanError::OutOfMemory);
                }
                last.elements.extend_from_slice(&first.elements);
                out.subpaths[first_dash] = last;
            }
            Some(last) => try_push(&mut out.subpaths, last)?,
            None => {}
        }
    }
    Ok(out)
}

/// A count as a float, exactly while it is below 2^53.
#[allow(
    clippy::cast_precision_loss,
    reason = "the counts here are bounded far below 2^53"
)]
const fn count_as_f64(count: usize) -> f64 {
    count as f64
}

/// `path` under `rule` grown by `distance` — shrunk where negative — with
/// its corners joined as `join` says: what an offset of the fill covers.
///
/// # Errors
///
/// As [`boolean`].
pub fn grown(
    path: &Path,
    rule: FillRule,
    distance: f64,
    join: Join,
    miter_limit: f64,
    tolerance: f64,
) -> Result<Path, BooleanError> {
    let filled = simplified(path, rule)?;
    if distance == 0.0 || !distance.is_finite() {
        return Ok(filled);
    }
    // Every contour a boolean traces is closed, so its outline is a band
    // round each with no caps.
    let band = outline(
        &filled,
        &StrokeStyle {
            width: 2.0 * fabs(distance),
            join,
            miter_limit,
            start_cap: Cap::Butt,
            end_cap: Cap::Butt,
        },
        tolerance,
    )?;
    let operation = if distance > 0.0 {
        Operation::Union
    } else {
        Operation::Difference
    };
    boolean(
        &filled,
        FillRule::NonZero,
        &band,
        FillRule::NonZero,
        operation,
    )
}

/// What one side of a stroke is built with.
struct Sides {
    half: f64,
    join: Join,
    miter_limit: f64,
    tolerance: f64,
}

impl Sides {
    /// The right side of `segments` as drawn, offset by half the width and
    /// joined at each vertex; closed round to its start where `closed`.
    fn side(&self, segments: &[Segment], closed: bool) -> Result<Subpath, BooleanError> {
        let first = offset_start(segments[0], self.half);
        let mut side = Subpath::new(first);
        for (index, &segment) in segments.iter().enumerate() {
            self.offset_into(&mut side, segment)?;
            let next = match segments.get(index + 1) {
                Some(&next) => Some(next),
                None if closed => Some(segments[0]),
                None => None,
            };
            if let Some(next) = next {
                self.join_into(&mut side, segment, next)?;
            }
        }
        side.closed = closed;
        Ok(side)
    }

    /// Append `segment` offset by half the width.
    fn offset_into(&self, side: &mut Subpath, segment: Segment) -> Result<(), TryReserveError> {
        match segment {
            Segment::Line(line) => {
                let Some(normal) = line.deriv().normalised().map(Point::perpendicular) else {
                    return Ok(());
                };
                try_push(
                    &mut side.elements,
                    PathEl::Line(line.p1 + normal * self.half),
                )
            }
            Segment::Cubic(cubic) => offset_cubic(cubic, self.half, self.tolerance, |piece| {
                try_push(
                    &mut side.elements,
                    PathEl::Cubic(piece.p1, piece.p2, piece.p3),
                )
            }),
        }
    }

    /// Join the offset of `before` to the offset of `after` at the vertex
    /// they share.
    fn join_into(
        &self,
        side: &mut Subpath,
        before: Segment,
        after: Segment,
    ) -> Result<(), TryReserveError> {
        let vertex = before.end();
        let incoming = direction_at_end(before);
        let outgoing = direction_at_start(after);
        let from = vertex + incoming.perpendicular() * self.half;
        let to = vertex + outgoing.perpendicular() * self.half;
        if from.distance(to) <= self.tolerance {
            return Ok(());
        }
        if incoming.cross(outgoing) > 0.0 {
            // A right turn: this side is the inner one, and the overlap it
            // leaves is the simplification's to remove.
            try_push(&mut side.elements, PathEl::Line(vertex))?;
            return try_push(&mut side.elements, PathEl::Line(to));
        }
        match self.join {
            Join::Bevel => try_push(&mut side.elements, PathEl::Line(to)),
            Join::Miter => {
                // Where the two edges, carried on in their own directions,
                // meet.
                let denominator = incoming.cross(outgoing);
                let apex = (denominator != 0.0).then(|| {
                    let along = (to - from).cross(outgoing) / denominator;
                    from + incoming * along
                });
                match apex {
                    Some(apex) if apex.distance(vertex) / self.half <= self.miter_limit => {
                        try_push(&mut side.elements, PathEl::Line(apex))?;
                        try_push(&mut side.elements, PathEl::Line(to))
                    }
                    _ => try_push(&mut side.elements, PathEl::Line(to)),
                }
            }
            Join::Round => {
                let (start, end) = (from - vertex, to - vertex);
                let arc = EllipseArc {
                    centre: vertex,
                    radii: Point::new(self.half, self.half),
                    x_rotation: 0.0,
                    start: start.angle(),
                    sweep: angle_between(start, end),
                };
                push_arc(side, arc, to)
            }
        }
    }
}

/// Where the right side of `segment` begins, half the width out.
fn offset_start(segment: Segment, half: f64) -> Point {
    segment.start() + direction_at_start(segment).perpendicular() * half
}

/// The unit direction `segment` leaves its start in.
fn direction_at_start(segment: Segment) -> Point {
    segment
        .tangents()
        .0
        .normalised()
        .unwrap_or(Point::new(1.0, 0.0))
}

/// The unit direction `segment` arrives at its end in.
fn direction_at_end(segment: Segment) -> Point {
    segment
        .tangents()
        .1
        .normalised()
        .unwrap_or(Point::new(1.0, 0.0))
}

/// Cap the outline at `point`, where the path arrives running `direction`:
/// from the right side's end to the left side's start.
fn cap(
    contour: &mut Subpath,
    point: Point,
    direction: Point,
    half: f64,
    style: Cap,
) -> Result<(), TryReserveError> {
    let normal = direction.perpendicular();
    let left = point - normal * half;
    match style {
        Cap::Butt => try_push(&mut contour.elements, PathEl::Line(left)),
        Cap::Square => {
            let ahead = direction * half;
            try_push(
                &mut contour.elements,
                PathEl::Line(point + normal * half + ahead),
            )?;
            try_push(&mut contour.elements, PathEl::Line(left + ahead))?;
            try_push(&mut contour.elements, PathEl::Line(left))
        }
        Cap::Round => {
            let arc = EllipseArc {
                centre: point,
                radii: Point::new(half, half),
                x_rotation: 0.0,
                start: normal.angle(),
                sweep: -PI,
            };
            push_arc(contour, arc, left)
        }
    }
}

/// The outline a stroke of a path with no length paints at `point`: a disc
/// for a round cap, a square for a square one, nothing for a butt.
fn dot(point: Point, half: f64, style: Cap) -> Result<Option<Subpath>, TryReserveError> {
    let mut dot = match style {
        Cap::Butt => return Ok(None),
        Cap::Square => {
            let corners = [
                point + Point::new(-half, -half),
                point + Point::new(half, -half),
                point + Point::new(half, half),
                point + Point::new(-half, half),
            ];
            let mut square = Subpath::new(corners[0]);
            square.elements.try_reserve_exact(corners.len() - 1)?;
            square
                .elements
                .extend(corners[1..].iter().map(|&c| PathEl::Line(c)));
            square
        }
        Cap::Round => {
            let arc = EllipseArc {
                centre: point,
                radii: Point::new(half, half),
                x_rotation: 0.0,
                start: 0.0,
                sweep: 2.0 * PI,
            };
            let mut disc = Subpath::new(arc.start_point());
            push_arc(&mut disc, arc, arc.start_point())?;
            disc
        }
    };
    dot.closed = true;
    Ok(Some(dot))
}

/// Append the cubics of `arc`, landing exactly on `end`.
fn push_arc(side: &mut Subpath, arc: EllipseArc, end: Point) -> Result<(), TryReserveError> {
    let mut cubics: tairix_inline::ArrayVec<CubicBez, 4> = tairix_inline::ArrayVec::new();
    arc.to_cubics(|c| {
        let _ = cubics.try_push(c);
    });
    let count = cubics.len();
    for (index, cubic) in cubics.into_iter().enumerate() {
        let to = if index + 1 == count { end } else { cubic.p3 };
        try_push(&mut side.elements, PathEl::Cubic(cubic.p1, cubic.p2, to))?;
    }
    if count == 0 {
        try_push(&mut side.elements, PathEl::Line(end))?;
    }
    Ok(())
}

/// Append `other`'s steps to `contour`, joined by a line from the one's end
/// to the other's start.
fn append(contour: &mut Subpath, other: &Subpath) -> Result<(), TryReserveError> {
    if contour.end() != other.start {
        try_push(&mut contour.elements, PathEl::Line(other.start))?;
    }
    contour.elements.try_reserve_exact(other.elements.len())?;
    contour.elements.extend_from_slice(&other.elements);
    Ok(())
}

/// Whether `segment` has no length and turns nowhere: every point the same.
fn is_degenerate(segment: Segment) -> bool {
    match segment {
        Segment::Line(line) => line.p0 == line.p1,
        Segment::Cubic(cubic) => {
            cubic.p0 == cubic.p1 && cubic.p1 == cubic.p2 && cubic.p2 == cubic.p3
        }
    }
}

/// Where a dash pattern stands along one sub-path.
struct Dasher<'p> {
    lengths: &'p [f64],
    /// The dash being drawn or the gap being skipped.
    index: usize,
    /// How much of it is left.
    remaining: f64,
    /// The dash being drawn, if one is.
    current: Option<Subpath>,
}

impl<'p> Dasher<'p> {
    fn new(lengths: &'p [f64], offset: f64) -> Self {
        // The caller holds the offset within one period of a pattern that
        // draws something, so this passes each entry at most once.
        let mut index = 0;
        let mut into = offset;
        // A dash of no length where the walk begins is drawn, not passed.
        while into >= lengths[index]
            && (into > 0.0 || lengths[index] > 0.0)
            && index + 1 < lengths.len()
        {
            into -= lengths[index];
            index += 1;
        }
        Self {
            lengths,
            index,
            remaining: lengths[index] - into,
            current: None,
        }
    }

    const fn drawing(&self) -> bool {
        self.index.is_multiple_of(2)
    }

    /// Follow `segment`, measured as `measure` holds segment `index`,
    /// cutting it where dashes begin and end.
    fn walk(
        &mut self,
        segment: Segment,
        measure: &PathMeasure,
        index: usize,
        out: &mut Path,
    ) -> Result<(), BooleanError> {
        let length = measure.segment_length(index);
        let mut along = 0.0;
        while along < length {
            let step = fmax(self.remaining, 0.0);
            let reach = along + step;
            let ends_here = reach >= length;
            let to = if ends_here { length } else { reach };
            if self.drawing() {
                let (t0, t1) = (measure.param_at(index, along), measure.param_at(index, to));
                let piece = segment.subsegment(t0, t1);
                let dash = self
                    .current
                    .get_or_insert_with(|| Subpath::new(piece.start()));
                let element = if to > along || !dash.elements.is_empty() {
                    PathEl::of(piece)
                } else {
                    point_dash(segment, t0, piece.start())
                };
                try_push(&mut dash.elements, element)?;
            }
            if ends_here {
                self.remaining = reach - length;
                break;
            }
            self.end_part(out)?;
            along = to;
        }
        Ok(())
    }

    /// The dash or gap is used up: move to the next, ending a dash being
    /// drawn.
    fn end_part(&mut self, out: &mut Path) -> Result<(), BooleanError> {
        if let Some(dash) = self.current.take() {
            if out.subpaths.len() >= MAX_DASHES {
                return Err(BooleanError::TooComplex);
            }
            try_push(&mut out.subpaths, dash)?;
        }
        self.index = (self.index + 1) % self.lengths.len();
        self.remaining = self.lengths[self.index];
        Ok(())
    }
}

/// A dash of no length, as a step along the path too short to see: SVG draws
/// such a dash as its caps alone, which need the path's direction there.
fn point_dash(segment: Segment, t: f64, at: Point) -> PathEl {
    // Relative to the point's magnitude, so the step survives rounding at
    // any coordinate a drawing holds.
    const STEP: f64 = 1.0 / 68_719_476_736.0;
    let (start, end) = segment.tangents();
    let direction = segment
        .deriv(t)
        .normalised()
        .or_else(|| if t < 0.5 { start } else { end }.normalised())
        .unwrap_or(Point::new(1.0, 0.0));
    let scale = fmax(fmax(fabs(at.x), fabs(at.y)), 1.0);
    PathEl::Line(at + direction * (scale * STEP))
}

#[cfg(test)]
#[path = "offset_tests.rs"]
mod tests;
