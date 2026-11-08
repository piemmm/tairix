//! Boolean operations on filled paths, keeping curves as curves.
//!
//! Both operands are taken as fills — every sub-path closed — under their
//! own fill rules. The algorithm:
//!
//! 1. **Pieces.** Every segment is split at its horizontal and vertical
//!    turning points, so no piece can cross itself, and a piece straight to
//!    within the tolerance becomes a line.
//! 2. **Meetings.** Every pair of pieces whose bounds touch is intersected
//!    ([`crate::intersect::meet`]), found by sweeping the pieces in order of
//!    their left edges; a crossing splits both pieces at one shared vertex,
//!    and a coincident run splits both at its ends.
//! 3. **Edges.** The pieces cut at every split are the edges of the overlay,
//!    their ends merged into vertices wherever they fall within the
//!    tolerance; edges tracing the same curve between the same vertices are
//!    one edge, remembering every original segment that ran along it.
//! 4. **Classification.** Each operand's winding is taken just left of each
//!    edge by casting a ray against the original segments, and the winding
//!    just right follows by adding the edge's own contributions, so the two
//!    sides always differ by exactly the edge; an edge is kept where the
//!    operation's result differs across it, run so the result lies on its
//!    right.
//! 5. **Tracing.** Kept edges are followed vertex to vertex, taking the
//!    sharpest right turn wherever several leave one vertex, so each face is
//!    traced as its own contour; consecutive edges cut from one original
//!    curve are joined back into one.
//!
//! The result is closed contours, wound clockwise as drawn around what it
//! covers and against that around its holes, so it fills under either rule.

use alloc::collections::TryReserveError;
use alloc::vec::Vec;

use tairix_util::fallible;
use tairix_util::mathf::{atan2, fabs, fmax, fmin, sqrt};

use crate::bounds::Bounds;
use crate::cubic::CubicBez;
use crate::intersect::{self, Meeting};
use crate::line::Line;
use crate::path::{segment_winding, FillRule, Path, PathEl, Subpath};
use crate::point::Point;
use crate::segment::Segment;

/// The most segments both operands may hold together: a containment bound on
/// the work, which grows with the product of the pieces that meet.
pub const MAX_BOOLEAN_SEGMENTS: usize = 1 << 18;

/// The most splits the meetings may make across both operands.
pub const MAX_BOOLEAN_SPLITS: usize = 1 << 21;

/// The tolerance relative to the operands' extent: points this close count as
/// one.
const RELATIVE_TOLERANCE: f64 = 1e-9;

/// How far beside an edge, in tolerances, its sides are sampled.
const SIDE_OFFSET: f64 = 1e3;

/// The operation a boolean performs.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Operation {
    /// What either covers.
    Union,
    /// What both cover.
    Intersection,
    /// What the first covers and the second does not.
    Difference,
    /// What exactly one covers.
    Exclusion,
}

impl Operation {
    /// Whether a point inside the first operand as `a` says and the second
    /// as `b` says is in the result.
    #[must_use]
    pub const fn keeps(self, a: bool, b: bool) -> bool {
        match self {
            Self::Union => a || b,
            Self::Intersection => a && b,
            Self::Difference => a && !b,
            Self::Exclusion => a != b,
        }
    }
}

/// Why a boolean operation was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BooleanError {
    /// The operands hold more than [`MAX_BOOLEAN_SEGMENTS`], meet more than
    /// [`MAX_BOOLEAN_SPLITS`] times, or run alongside each other too closely
    /// to separate.
    TooComplex,
    /// The memory the operation needs could not be had.
    OutOfMemory,
}

impl From<TryReserveError> for BooleanError {
    fn from(_: TryReserveError) -> Self {
        Self::OutOfMemory
    }
}

/// What `a` under `rule_a` and `b` under `rule_b` come to under `operation`,
/// as one path.
///
/// # Errors
///
/// [`BooleanError::TooComplex`] past the containment bounds, and
/// [`BooleanError::OutOfMemory`] when the working buffers are refused.
pub fn boolean(
    a: &Path,
    rule_a: FillRule,
    b: &Path,
    rule_b: FillRule,
    operation: Operation,
) -> Result<Path, BooleanError> {
    let mut overlay = Overlay::new([(a, rule_a), (b, rule_b)])?;
    overlay.meet()?;
    overlay.split_edges()?;
    overlay.classify(operation);
    overlay.trace()
}

/// `path` under `rule` as the outline of exactly what it covers: crossings
/// resolved, overlaps merged, holes wound against what holds them.
///
/// # Errors
///
/// As [`boolean`].
pub fn simplified(path: &Path, rule: FillRule) -> Result<Path, BooleanError> {
    boolean(
        path,
        rule,
        &Path::new(),
        FillRule::NonZero,
        Operation::Union,
    )
}

/// The regions the two operands cut each other into — what both cover, what
/// only the first covers and what only the second does — each connected
/// region one path holding its own holes.
///
/// # Errors
///
/// As [`boolean`].
pub fn divide(
    a: &Path,
    rule_a: FillRule,
    b: &Path,
    rule_b: FillRule,
) -> Result<Vec<Path>, BooleanError> {
    let mut regions = Vec::new();
    let mut gather = |found: &Path| -> Result<(), BooleanError> {
        let mut parts = components(found)?;
        regions.try_reserve(parts.len())?;
        regions.append(&mut parts);
        Ok(())
    };
    for operation in [Operation::Intersection, Operation::Difference] {
        gather(&boolean(a, rule_a, b, rule_b, operation)?)?;
    }
    gather(&boolean(b, rule_b, a, rule_a, Operation::Difference)?)?;
    Ok(regions)
}

/// A simplified path's contours grouped into connected regions: each
/// clockwise contour with the anticlockwise ones directly inside it.
///
/// The path must be wound as [`boolean`] winds its results.
///
/// # Errors
///
/// [`BooleanError::OutOfMemory`] when the groups cannot be held.
pub fn components(path: &Path) -> Result<Vec<Path>, BooleanError> {
    let mut outers: Vec<Path> = Vec::new();
    // Each outer contour's area and bounds, the bounds sparing a hole the
    // winding of every contour nowhere near it.
    let mut extents: Vec<(f64, Bounds)> = Vec::new();
    let mut holes: Vec<&Subpath> = Vec::new();
    for subpath in &path.subpaths {
        let area = subpath.signed_area();
        if area > 0.0 {
            fallible::try_push(&mut outers, Path::single(subpath.try_clone()?)?)?;
            fallible::try_push(&mut extents, (area, subpath.bounds()))?;
        } else {
            fallible::try_push(&mut holes, subpath)?;
        }
    }
    for hole in holes {
        let probe = interior_point(hole);
        // The smallest outer contour round the hole holds it.
        let mut owner: Option<usize> = None;
        for (index, outer) in outers.iter().enumerate() {
            let (area, bounds) = extents[index];
            if !bounds.contains(probe) || outer.subpaths[0].winding(probe) == 0 {
                continue;
            }
            if owner.is_none_or(|best| area < extents[best].0) {
                owner = Some(index);
            }
        }
        if let Some(owner) = owner {
            fallible::try_push(&mut outers[owner].subpaths, hole.try_clone()?)?;
        }
    }
    Ok(outers)
}

/// A point just inside the contour `subpath` traces, beside the middle of
/// its first segment on the side it covers.
fn interior_point(subpath: &Subpath) -> Point {
    let Some(segment) = subpath.filled_segments().next() else {
        return subpath.start;
    };
    let middle = segment.eval(0.5);
    let along = segment
        .deriv(0.5)
        .normalised()
        .unwrap_or(Point::new(1.0, 0.0));
    let scale = fmax(subpath.bounds().width(), subpath.bounds().height());
    // Inside lies right of a clockwise contour and left of an anticlockwise
    // one; a hole is anticlockwise, so step left.
    let step = fmax(scale * 1e-6, f64::MIN_POSITIVE);
    if subpath.signed_area() < 0.0 {
        middle - along.perpendicular() * step
    } else {
        middle + along.perpendicular() * step
    }
}

/// One segment of an operand, cut to a piece that cannot cross itself.
#[derive(Copy, Clone)]
struct Piece {
    segment: Segment,
    operand: usize,
    /// The vertex its start and end were first given.
    start: usize,
    end: usize,
}

/// Where a piece is cut, and the vertex the cut lies at.
#[derive(Copy, Clone)]
struct Cut {
    piece: u32,
    t: f64,
    vertex: u32,
}

/// An edge of the overlay: the part of a piece between two cuts.
#[derive(Clone)]
struct Edge {
    segment: Segment,
    from: usize,
    to: usize,
    /// The piece it was cut from and the parameters it spans on it.
    piece: usize,
    span: (f64, f64),
    /// Each operand's segments along it: `+1` for each running the edge's
    /// way, `-1` for each running against it.
    runs: [i64; 2],
    /// Whether the result keeps it, and run which way.
    kept: Keep,
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum Keep {
    Dropped,
    Forward,
    Backward,
}

/// Everything one boolean operation works over.
struct Overlay<'p> {
    operands: [(&'p Path, FillRule); 2],
    tolerance: f64,
    pieces: Vec<Piece>,
    vertices: Vec<Point>,
    cuts: Vec<Cut>,
    edges: Vec<Edge>,
    /// The original segments by y band, which the side rays are cast against.
    rays: RayIndex,
}

impl<'p> Overlay<'p> {
    fn new(operands: [(&'p Path, FillRule); 2]) -> Result<Self, BooleanError> {
        let count: usize = operands
            .iter()
            .map(|(path, _)| {
                path.subpaths
                    .iter()
                    .map(|s| s.elements.len() + 1)
                    .sum::<usize>()
            })
            .sum();
        if count > MAX_BOOLEAN_SEGMENTS {
            return Err(BooleanError::TooComplex);
        }
        let extent = operands.iter().fold(Bounds::EMPTY, |bounds, (path, _)| {
            bounds.union(path.bounds())
        });
        let scale = if extent.is_finite() {
            fmax(
                fmax(fabs(extent.min.x), fabs(extent.max.x)),
                fmax(fabs(extent.min.y), fabs(extent.max.y)),
            )
        } else {
            0.0
        };
        if !scale.is_finite() {
            return Err(BooleanError::TooComplex);
        }
        let tolerance = fmax(1.0, scale) * RELATIVE_TOLERANCE;
        let mut overlay = Self {
            operands,
            tolerance,
            pieces: Vec::new(),
            vertices: Vec::new(),
            cuts: Vec::new(),
            edges: Vec::new(),
            rays: RayIndex::default(),
        };
        overlay.gather()?;
        overlay.rays = RayIndex::build(&overlay.operands)?;
        Ok(overlay)
    }

    /// Cut every operand segment into pieces that cannot cross themselves.
    fn gather(&mut self) -> Result<(), BooleanError> {
        let (tolerance, operands) = (self.tolerance, self.operands);
        for (operand, (path, _)) in operands.iter().enumerate() {
            for subpath in &path.subpaths {
                let first = self.vertices.len();
                let mut from = self.vertex(subpath.start)?;
                for segment in subpath.filled_segments() {
                    if segment.start().distance(segment.end()) <= tolerance
                        && segment.bounds().width() <= tolerance
                        && segment.bounds().height() <= tolerance
                    {
                        continue;
                    }
                    for piece in monotone_pieces(segment, tolerance) {
                        let is_last = piece.end() == subpath.start && first < self.vertices.len();
                        let to = if is_last {
                            first
                        } else {
                            self.vertex(piece.end())?
                        };
                        fallible::try_push(
                            &mut self.pieces,
                            Piece {
                                segment: piece,
                                operand,
                                start: from,
                                end: to,
                            },
                        )?;
                        from = to;
                    }
                }
            }
        }
        Ok(())
    }

    fn vertex(&mut self, point: Point) -> Result<usize, BooleanError> {
        fallible::try_push(&mut self.vertices, point)?;
        Ok(self.vertices.len() - 1)
    }

    /// Intersect every pair of pieces whose bounds touch, recording where
    /// each is cut.
    fn meet(&mut self) -> Result<(), BooleanError> {
        let tolerance = self.tolerance;
        let mut order: Vec<(f64, u32)> = Vec::new();
        order.try_reserve_exact(self.pieces.len())?;
        let mut bounds: Vec<Bounds> = Vec::new();
        bounds.try_reserve_exact(self.pieces.len())?;
        for (index, piece) in self.pieces.iter().enumerate() {
            let piece_bounds = piece.segment.bounds().inflated(tolerance);
            bounds.push(piece_bounds);
            order.push((piece_bounds.min.x, index_u32(index)?));
        }
        order.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        for (position, &(_, i)) in order.iter().enumerate() {
            let reach = bounds[i as usize].max.x;
            for &(left, j) in &order[position + 1..] {
                if left > reach {
                    break;
                }
                if !bounds[i as usize].intersects(bounds[j as usize]) {
                    continue;
                }
                self.meet_pair(i, j)?;
            }
        }
        Ok(())
    }

    fn meet_pair(&mut self, i: u32, j: u32) -> Result<(), BooleanError> {
        let (a, b) = (self.pieces[i as usize], self.pieces[j as usize]);
        // Consecutive pieces share their common vertex already.
        let meeting = intersect::meet(a.segment, b.segment, self.tolerance)
            .map_err(|_| BooleanError::TooComplex)?;
        match meeting {
            Meeting::Crossings(found) => {
                for crossing in found {
                    let shared_end = (crossing.t >= 1.0 && crossing.u <= 0.0 && a.end == b.start)
                        || (crossing.t <= 0.0 && crossing.u >= 1.0 && a.start == b.end);
                    if shared_end {
                        continue;
                    }
                    let at = a
                        .segment
                        .eval(crossing.t)
                        .midpoint(b.segment.eval(crossing.u));
                    let vertex = index_u32(self.vertex(at)?)?;
                    self.record_cut(i, crossing.t, vertex)?;
                    self.record_cut(j, crossing.u, vertex)?;
                }
            }
            Meeting::Overlap(run) => {
                for (t, u) in [(run.t0, run.u0), (run.t1, run.u1)] {
                    let at = a.segment.eval(t);
                    let vertex = index_u32(self.vertex(at)?)?;
                    self.record_cut(i, t, vertex)?;
                    self.record_cut(j, u, vertex)?;
                }
            }
        }
        Ok(())
    }

    fn record_cut(&mut self, piece: u32, t: f64, vertex: u32) -> Result<(), BooleanError> {
        if self.cuts.len() >= MAX_BOOLEAN_SPLITS {
            return Err(BooleanError::TooComplex);
        }
        let t = along_piece(t);
        fallible::try_push(&mut self.cuts, Cut { piece, t, vertex })?;
        Ok(())
    }

    /// Merge vertices within the tolerance and cut every piece into edges.
    fn split_edges(&mut self) -> Result<(), BooleanError> {
        let merged = merge_vertices(&self.vertices, self.tolerance * 16.0)?;
        // Each vertex now stands at its representative's position.
        let positions = fallible::collected(merged.len(), merged.iter().map(|&r| self.vertices[r]))
            .ok_or(BooleanError::OutOfMemory)?;
        self.cuts
            .sort_unstable_by(|a, b| a.piece.cmp(&b.piece).then(a.t.total_cmp(&b.t)));
        let mut cursor = 0;
        let mut stops: Vec<(f64, usize)> = Vec::new();
        for (index, piece) in self.pieces.iter().enumerate() {
            let cut_here = self.cuts[cursor..]
                .iter()
                .take_while(|cut| cut.piece as usize == index)
                .count();
            // In order by construction: every cut lies within the piece's ends,
            // and the cuts were sorted along it.
            stops.clear();
            stops.try_reserve(cut_here + 2)?;
            stops.push((0.0, merged[piece.start]));
            for cut in &self.cuts[cursor..cursor + cut_here] {
                stops.push((cut.t, merged[cut.vertex as usize]));
            }
            cursor += cut_here;
            stops.push((1.0, merged[piece.end]));
            for pair in stops.windows(2) {
                let ((t0, from), (t1, to)) = (pair[0], pair[1]);
                if from == to || t1 <= t0 {
                    continue;
                }
                let mut segment = piece.segment.subsegment(t0, t1);
                snap(&mut segment, positions[from], positions[to]);
                let mut runs = [0, 0];
                runs[piece.operand] = 1;
                fallible::try_push(
                    &mut self.edges,
                    Edge {
                        segment,
                        from,
                        to,
                        piece: index,
                        span: (t0, t1),
                        runs,
                        kept: Keep::Dropped,
                    },
                )?;
            }
        }
        self.vertices = positions;
        self.merge_coincident_edges()
    }

    /// Fold edges tracing the same curve between the same vertices into one,
    /// summing which operands run along it which way.
    fn merge_coincident_edges(&mut self) -> Result<(), BooleanError> {
        let mut keys: Vec<(usize, usize, u32)> = Vec::new();
        keys.try_reserve_exact(self.edges.len())?;
        for (index, edge) in self.edges.iter().enumerate() {
            let (low, high) = if edge.from <= edge.to {
                (edge.from, edge.to)
            } else {
                (edge.to, edge.from)
            };
            keys.push((low, high, index_u32(index)?));
        }
        keys.sort_unstable();
        let mut gone =
            fallible::filled(self.edges.len(), false).ok_or(BooleanError::OutOfMemory)?;
        let mut start = 0;
        while start < keys.len() {
            let mut end = start + 1;
            while end < keys.len() && keys[end].0 == keys[start].0 && keys[end].1 == keys[start].1 {
                end += 1;
            }
            for x in start..end {
                let keep = keys[x].2 as usize;
                if gone[keep] {
                    continue;
                }
                for &(_, _, other) in &keys[x + 1..end] {
                    let other = other as usize;
                    if gone[other] || !self.same_trace(keep, other) {
                        continue;
                    }
                    let reversed = self.edges[keep].from != self.edges[other].from;
                    let sign = if reversed { -1 } else { 1 };
                    for operand in 0..2 {
                        self.edges[keep].runs[operand] += sign * self.edges[other].runs[operand];
                    }
                    gone[other] = true;
                }
            }
            start = end;
        }
        let mut index = 0;
        self.edges.retain(|_| {
            let keep = !gone[index];
            index += 1;
            keep
        });
        Ok(())
    }

    /// Whether edges `a` and `b`, which join the same two vertices, trace the
    /// same curve.
    fn same_trace(&self, a: usize, b: usize) -> bool {
        let (ea, eb) = (&self.edges[a].segment, &self.edges[b].segment);
        let limit = self.tolerance * 64.0;
        [0.25, 0.5, 0.75]
            .iter()
            .all(|&t| eb.nearest(ea.eval(t)).1 <= limit * limit)
    }

    /// Decide which edges bound the result, and which way each runs.
    fn classify(&mut self, operation: Operation) {
        let offset = self.tolerance * SIDE_OFFSET;
        let rules = [self.operands[0].1, self.operands[1].1];
        for index in 0..self.edges.len() {
            let edge = &self.edges[index];
            let middle = edge.segment.eval(0.5);
            let along = edge
                .segment
                .deriv(0.5)
                .normalised()
                .or_else(|| (edge.segment.end() - edge.segment.start()).normalised());
            let Some(along) = along else {
                continue;
            };
            let length = edge.segment.start().distance(edge.segment.end());
            let step = if length > 0.0 {
                fmax(fmin(offset, length * 0.25), self.tolerance)
            } else {
                offset
            };
            let left = middle - along.perpendicular() * step;
            let winding_left = self.rays.winding(&self.operands, left);
            let winding_right = [
                winding_left[0] + edge.runs[0],
                winding_left[1] + edge.runs[1],
            ];
            let inside = |winding: [i64; 2]| {
                operation.keeps(rules[0].inside(winding[0]), rules[1].inside(winding[1]))
            };
            let (left_in, right_in) = (inside(winding_left), inside(winding_right));
            self.edges[index].kept = match (left_in, right_in) {
                (false, true) => Keep::Forward,
                (true, false) => Keep::Backward,
                _ => Keep::Dropped,
            };
        }
    }

    /// Follow the kept edges into closed contours.
    fn trace(self) -> Result<Path, BooleanError> {
        let mut outgoing: Vec<Vec<usize>> = Vec::new();
        outgoing.try_reserve_exact(self.vertices.len())?;
        outgoing.resize_with(self.vertices.len(), Vec::new);
        let kept = self
            .edges
            .iter()
            .enumerate()
            .filter_map(|(index, edge)| match edge.kept {
                Keep::Dropped => None,
                Keep::Forward => Some(Directed {
                    edge: index,
                    from: edge.from,
                    to: edge.to,
                    segment: edge.segment,
                    forward: true,
                }),
                Keep::Backward => Some(Directed {
                    edge: index,
                    from: edge.to,
                    to: edge.from,
                    segment: edge.segment.reversed(),
                    forward: false,
                }),
            });
        let count = self
            .edges
            .iter()
            .filter(|edge| edge.kept != Keep::Dropped)
            .count();
        let directed = fallible::collected(count, kept).ok_or(BooleanError::OutOfMemory)?;
        for (index, directed) in directed.iter().enumerate() {
            fallible::try_push(&mut outgoing[directed.from], index)?;
        }
        let mut used = fallible::filled(directed.len(), false).ok_or(BooleanError::OutOfMemory)?;
        let mut result = Path::new();
        let mut contour: Vec<usize> = Vec::new();
        for first in 0..directed.len() {
            if used[first] {
                continue;
            }
            contour.clear();
            let mut current = first;
            let mut returned = false;
            loop {
                used[current] = true;
                fallible::try_push(&mut contour, current)?;
                let at = directed[current].to;
                if at == directed[first].from {
                    returned = true;
                    break;
                }
                let arriving = arriving_direction(directed[current].segment);
                let next = outgoing[at]
                    .iter()
                    .copied()
                    .filter(|&candidate| !used[candidate])
                    .max_by(|&x, &y| {
                        let turn = |candidate: usize| {
                            turn_angle(arriving, leaving_direction(directed[candidate].segment))
                        };
                        turn(x).total_cmp(&turn(y))
                    });
                match next {
                    Some(next) => current = next,
                    None => break,
                }
            }
            if let Some(subpath) = self.contour(&directed, &contour, returned)? {
                fallible::try_push(&mut result.subpaths, subpath)?;
            }
        }
        Ok(result)
    }

    /// The closed sub-path tracing `contour`, consecutive edges cut from one
    /// piece joined back into one segment. A contour that did not return to
    /// its start is closed by a straight join rather than bent onto it.
    #[allow(
        clippy::float_cmp,
        reason = "two edges cut at one cut share its parameter to the bit, and only those join"
    )]
    fn contour(
        &self,
        directed: &[Directed],
        contour: &[usize],
        returned: bool,
    ) -> Result<Option<Subpath>, BooleanError> {
        let Some(&first) = contour.first() else {
            return Ok(None);
        };
        let mut subpath = Subpath::new(self.vertices[directed[first].from]);
        let mut pending: Option<Run> = None;
        let flush = |subpath: &mut Subpath, run: Run| {
            let mut joined = self.pieces[run.piece]
                .segment
                .subsegment(run.start, run.end);
            snap(&mut joined, subpath.end(), self.vertices[run.to]);
            fallible::try_push(&mut subpath.elements, PathEl::of(joined))
        };
        for &index in contour {
            let step = directed[index];
            let edge = &self.edges[step.edge];
            let (t0, t1) = edge.span;
            let (from_t, to_t) = if step.forward { (t0, t1) } else { (t1, t0) };
            match pending {
                Some(ref mut run)
                    if run.piece == edge.piece
                        && run.forward == step.forward
                        && run.end == from_t =>
                {
                    run.end = to_t;
                    run.to = step.to;
                }
                _ => {
                    if let Some(run) = pending.take() {
                        flush(&mut subpath, run)?;
                    }
                    pending = Some(Run {
                        piece: edge.piece,
                        start: from_t,
                        end: to_t,
                        forward: step.forward,
                        to: step.to,
                    });
                }
            }
        }
        if let Some(run) = pending {
            flush(&mut subpath, run)?;
        }
        if returned {
            let start = subpath.start;
            if let Some(last) = subpath.elements.last_mut() {
                *last = match *last {
                    PathEl::Line(_) => PathEl::Line(start),
                    PathEl::Cubic(c1, c2, _) => PathEl::Cubic(c1, c2, start),
                };
            }
        }
        subpath.closed = true;
        Ok((!subpath.elements.is_empty()).then_some(subpath))
    }
}

/// A run of edges cut from one piece, followed one way: where along the
/// piece it starts and ends, and the vertex it ends at.
#[derive(Copy, Clone)]
struct Run {
    piece: usize,
    start: f64,
    end: f64,
    forward: bool,
    to: usize,
}

/// A kept edge, run the way the result needs it.
#[derive(Copy, Clone)]
struct Directed {
    edge: usize,
    from: usize,
    to: usize,
    segment: Segment,
    forward: bool,
}

/// Move `segment`'s ends to `start` and `end`, which lie within the
/// tolerance of where they were.
fn snap(segment: &mut Segment, start: Point, end: Point) {
    match segment {
        Segment::Line(line) => {
            line.p0 = start;
            line.p1 = end;
        }
        Segment::Cubic(cubic) => {
            cubic.p0 = start;
            cubic.p3 = end;
        }
    }
}

/// The direction a segment arrives at its end in.
fn arriving_direction(segment: Segment) -> Point {
    segment.tangents().1
}

/// The direction a segment leaves its start in.
fn leaving_direction(segment: Segment) -> Point {
    segment.tangents().0
}

/// How far, in radians, turning from `arriving` to `leaving` turns
/// clockwise as drawn: positive for a right turn.
fn turn_angle(arriving: Point, leaving: Point) -> f64 {
    atan2(arriving.cross(leaving), arriving.dot(leaving))
}

/// `segment` cut at its horizontal and vertical turning points, a cubic
/// straight to within `tolerance` answered as its line.
fn monotone_pieces(segment: Segment, tolerance: f64) -> impl Iterator<Item = Segment> {
    let mut pieces: tairix_inline::ArrayVec<Segment, 5> = tairix_inline::ArrayVec::new();
    match segment {
        Segment::Line(_) => {
            let _ = pieces.try_push(segment);
        }
        Segment::Cubic(cubic) => {
            let mut from = 0.0;
            let extrema = cubic.extrema();
            for t in extrema.iter().copied().chain(core::iter::once(1.0)) {
                if t - from <= 1e-12 {
                    continue;
                }
                let piece = cubic.subsegment(from, t);
                let _ = pieces.try_push(straightened(piece, tolerance));
                from = t;
            }
        }
    }
    pieces.into_iter()
}

/// `cubic` as a line where it is one to within `tolerance`.
fn straightened(cubic: CubicBez, tolerance: f64) -> Segment {
    if cubic.is_line(tolerance) {
        Segment::Line(Line::new(cubic.p0, cubic.p3))
    } else {
        Segment::Cubic(cubic)
    }
}

/// Each vertex's representative: the first vertex within `distance` of it,
/// found through a grid of cells `distance` wide so each vertex compares
/// itself with its neighbours alone.
fn merge_vertices(vertices: &[Point], distance: f64) -> Result<Vec<usize>, BooleanError> {
    let mut parent: Vec<usize> = Vec::new();
    parent.try_reserve_exact(vertices.len())?;
    parent.extend(0..vertices.len());
    let mut cells: Vec<(i64, i64, usize)> = Vec::new();
    cells.try_reserve_exact(vertices.len())?;
    let cell_of = |point: Point| {
        (
            tairix_util::mathf::floor(point.x / distance),
            tairix_util::mathf::floor(point.y / distance),
        )
    };
    for (index, &vertex) in vertices.iter().enumerate() {
        let (cx, cy) = cell_of(vertex);
        cells.push((cell_index(cx), cell_index(cy), index));
    }
    cells.sort_unstable();
    let find = |parent: &mut [usize], mut x: usize| {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    };
    for &(cx, cy, index) in &cells {
        for dx in -1..=1 {
            for dy in -1..=1 {
                let key = (cx.saturating_add(dx), cy.saturating_add(dy));
                let begin = cells.partition_point(|&(x, y, _)| (x, y) < key);
                for &(x, y, other) in &cells[begin..] {
                    if (x, y) != key {
                        break;
                    }
                    if other < index && vertices[other].distance(vertices[index]) <= distance {
                        let (a, b) = (find(&mut parent, other), find(&mut parent, index));
                        if a != b {
                            let (low, high) = if a < b { (a, b) } else { (b, a) };
                            parent[high] = low;
                        }
                    }
                }
            }
        }
    }
    for index in 0..parent.len() {
        let root = find(&mut parent, index);
        parent[index] = root;
    }
    Ok(parent)
}

/// A cell coordinate as an integer, saturating far from the origin, where
/// cells are so wide no two distinct vertices share one by accident.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a float-to-integer cast saturates, which only merges cells beyond \
              any coordinate the bounds admit"
)]
fn cell_index(cell: f64) -> i64 {
    cell as i64
}

/// `t` held to a piece's parameters: anything not past its start — either
/// zero, or no number at all — is the start's own zero, so the cuts sort
/// along the piece between its two ends.
fn along_piece(t: f64) -> f64 {
    if t > 0.0 {
        fmin(t, 1.0)
    } else {
        0.0
    }
}

/// `index` as the 32-bit index the working tables hold, refused past them.
fn index_u32(index: usize) -> Result<u32, BooleanError> {
    u32::try_from(index).map_err(|_| BooleanError::TooComplex)
}

/// The original segments of both operands, filed by the horizontal bands of
/// the plane each spans, so a side ray is cast against the segments its band
/// holds rather than against every one.
#[derive(Default)]
struct RayIndex {
    top: f64,
    band_height: f64,
    bands: Vec<Vec<(usize, Segment)>>,
    /// Segments spanning too many bands to file, cast against every time.
    tall: Vec<(usize, Segment)>,
}

/// The most bands a segment is filed in before it counts as tall.
const MOST_BANDS_FILED: usize = 4;

impl RayIndex {
    fn build(operands: &[(&Path, FillRule); 2]) -> Result<Self, BooleanError> {
        let mut count = 0_usize;
        let mut extent = Bounds::EMPTY;
        for (path, _) in operands {
            for segment in path.subpaths.iter().flat_map(Subpath::filled_segments) {
                count += 1;
                extent = extent.union(segment.bounds());
            }
        }
        if count == 0 || extent.is_empty() {
            return Ok(Self::default());
        }
        let band_count = sqrt(count_as_f64(count)).clamp(1.0, 1024.0);
        let height = extent.height();
        let band_height = if height > 0.0 {
            height / band_count
        } else {
            1.0
        };
        let mut index = Self {
            top: extent.min.y,
            band_height,
            bands: Vec::new(),
            tall: Vec::new(),
        };
        let bands = index.band_of(extent.max.y) + 1;
        index.bands.try_reserve_exact(bands)?;
        index.bands.resize_with(bands, Vec::new);
        for (operand, (path, _)) in operands.iter().enumerate() {
            for segment in path.subpaths.iter().flat_map(Subpath::filled_segments) {
                let bounds = segment.bounds();
                let (low, high) = (index.band_of(bounds.min.y), index.band_of(bounds.max.y));
                if high - low < MOST_BANDS_FILED {
                    for band in low..=high {
                        fallible::try_push(&mut index.bands[band], (operand, segment))?;
                    }
                } else {
                    fallible::try_push(&mut index.tall, (operand, segment))?;
                }
            }
        }
        Ok(index)
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the quotient is held to the band range before the cast"
    )]
    fn band_of(&self, y: f64) -> usize {
        let at = ((y - self.top) / self.band_height).clamp(0.0, 1023.0);
        at as usize
    }

    /// Each operand's winding at `p`.
    fn winding(&self, operands: &[(&Path, FillRule); 2], p: Point) -> [i64; 2] {
        let mut winding = [0, 0];
        if self.bands.is_empty() {
            for (operand, (path, _)) in operands.iter().enumerate() {
                winding[operand] = path.winding(p);
            }
            return winding;
        }
        let band = self.band_of(p.y).min(self.bands.len() - 1);
        for &(operand, segment) in self.bands[band].iter().chain(&self.tall) {
            winding[operand] += segment_winding(segment, p);
        }
        winding
    }
}

/// A count as a float, exactly while it is below 2^53.
#[allow(
    clippy::cast_precision_loss,
    reason = "the count is bounded by the segment bound, far below 2^53"
)]
const fn count_as_f64(count: usize) -> f64 {
    count as f64
}

#[cfg(test)]
#[path = "boolean_tests.rs"]
mod tests;
