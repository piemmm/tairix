//! Paths of sub-paths, and which points they enclose.

use alloc::vec::Vec;

use crate::affine::Affine;
use crate::bounds::Bounds;
use crate::cubic::CubicBez;
use crate::flatten::flatten_cubic;
use crate::line::Line;
use crate::point::Point;
use crate::roots;
use crate::segment::Segment;

/// Steps spent finding where a y-monotone piece of a curve meets a ray: each
/// at least halves the bracket, so this reaches the double's resolution.
const CROSSING_STEPS: u32 = 64;

/// Which points enclosed by a set of contours count as inside.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum FillRule {
    /// Inside where the signed number of times the contours wind around the
    /// point is not zero, so a hole must be wound against its enclosing
    /// contour. SVG's initial value.
    #[default]
    NonZero,
    /// Inside where the number of contour crossings is odd, so any nested
    /// contour is a hole whichever way it is wound.
    EvenOdd,
}

impl FillRule {
    /// Whether a point the contours wind around `winding` times is inside.
    #[must_use]
    pub const fn inside(self, winding: i64) -> bool {
        match self {
            Self::NonZero => winding != 0,
            Self::EvenOdd => winding % 2 != 0,
        }
    }
}

/// One step of a sub-path, to the point it ends at.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum PathEl {
    /// A straight segment to the point.
    Line(Point),
    /// A cubic Bézier through the two controls to the last point.
    Cubic(Point, Point, Point),
}

impl PathEl {
    /// Where the step ends.
    #[must_use]
    pub const fn end(self) -> Point {
        match self {
            Self::Line(to) | Self::Cubic(_, _, to) => to,
        }
    }

    /// The step as a segment from `from`.
    #[must_use]
    pub const fn segment_from(self, from: Point) -> Segment {
        match self {
            Self::Line(to) => Segment::Line(Line::new(from, to)),
            Self::Cubic(c1, c2, to) => Segment::Cubic(CubicBez::new(from, c1, c2, to)),
        }
    }
}

/// A run of segments from one point, open or closed.
#[derive(Clone, Debug, PartialEq)]
pub struct Subpath {
    /// Where it starts.
    pub start: Point,
    /// Its steps, in order.
    pub elements: Vec<PathEl>,
    /// Whether a straight segment joins its end back to its start.
    pub closed: bool,
}

impl Subpath {
    /// A sub-path at `start` with no steps yet.
    #[must_use]
    pub const fn new(start: Point) -> Self {
        Self {
            start,
            elements: Vec::new(),
            closed: false,
        }
    }

    /// Where its last step ends, or its start when it has none.
    #[must_use]
    pub fn end(&self) -> Point {
        self.elements
            .last()
            .map_or(self.start, |element| element.end())
    }

    /// Its segments in order, the join back to the start included when it is
    /// closed and the ends differ.
    pub fn segments(&self) -> impl Iterator<Item = Segment> + '_ {
        let mut from = self.start;
        let steps = self.elements.iter().map(move |element| {
            let segment = element.segment_from(from);
            from = element.end();
            segment
        });
        let end = self.end();
        let closing =
            (self.closed && end != self.start).then(|| Segment::Line(Line::new(end, self.start)));
        steps.chain(closing)
    }

    /// Its segments as though it were closed, as a fill takes every
    /// sub-path.
    pub fn filled_segments(&self) -> impl Iterator<Item = Segment> + '_ {
        let mut from = self.start;
        let steps = self.elements.iter().map(move |element| {
            let segment = element.segment_from(from);
            from = element.end();
            segment
        });
        let end = self.end();
        let closing = (end != self.start).then(|| Segment::Line(Line::new(end, self.start)));
        steps.chain(closing)
    }

    /// The tight bounds of every point on it.
    #[must_use]
    pub fn bounds(&self) -> Bounds {
        self.segments()
            .fold(Bounds::of_point(self.start), |bounds, segment| {
                bounds.union(segment.bounds())
            })
    }

    /// The sub-path mapped through `transform`.
    #[must_use]
    pub fn transformed(&self, transform: Affine) -> Self {
        let elements = self
            .elements
            .iter()
            .map(|element| match *element {
                PathEl::Line(to) => PathEl::Line(transform.map(to)),
                PathEl::Cubic(c1, c2, to) => {
                    PathEl::Cubic(transform.map(c1), transform.map(c2), transform.map(to))
                }
            })
            .collect();
        Self {
            start: transform.map(self.start),
            elements,
            closed: self.closed,
        }
    }

    /// The sub-path run the other way: the same points, each step's controls
    /// swapped.
    #[must_use]
    pub fn reversed(&self) -> Self {
        let mut elements = Vec::with_capacity(self.elements.len());
        for (index, element) in self.elements.iter().enumerate().rev() {
            let from = match index {
                0 => self.start,
                _ => self.elements[index - 1].end(),
            };
            elements.push(match *element {
                PathEl::Line(_) => PathEl::Line(from),
                PathEl::Cubic(c1, c2, _) => PathEl::Cubic(c2, c1, from),
            });
        }
        Self {
            start: self.end(),
            elements,
            closed: self.closed,
        }
    }

    /// Its signed area as a fill takes it — closed whether it says so or
    /// not — positive clockwise as drawn.
    #[must_use]
    pub fn signed_area(&self) -> f64 {
        self.filled_segments().map(Segment::signed_area).sum()
    }

    /// The number of times it winds around `p`, as a fill takes it,
    /// positive clockwise as drawn.
    #[must_use]
    pub fn winding(&self, p: Point) -> i64 {
        self.filled_segments()
            .map(|segment| segment_winding(segment, p))
            .sum()
    }

    /// Append its flattened polyline to `out`: its start, then every point
    /// each segment flattens to within `tolerance`.
    pub fn flatten(&self, tolerance: f64, out: &mut Vec<Point>) {
        out.push(self.start);
        for segment in self.segments() {
            match segment {
                Segment::Line(line) => out.push(line.p1),
                Segment::Cubic(cubic) => flatten_cubic(cubic, tolerance, |point| out.push(point)),
            }
        }
    }
}

/// A shape: sub-paths filled and stroked together.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path {
    /// Its sub-paths, in order.
    pub subpaths: Vec<Subpath>,
}

impl Path {
    /// The path of no sub-paths.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            subpaths: Vec::new(),
        }
    }

    /// Whether it draws nothing: no sub-path has a step.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.subpaths
            .iter()
            .all(|subpath| subpath.elements.is_empty())
    }

    /// Every segment of every sub-path, in order.
    pub fn segments(&self) -> impl Iterator<Item = Segment> + '_ {
        self.subpaths.iter().flat_map(Subpath::segments)
    }

    /// The tight bounds of every point on it.
    #[must_use]
    pub fn bounds(&self) -> Bounds {
        self.subpaths.iter().fold(Bounds::EMPTY, |bounds, subpath| {
            bounds.union(subpath.bounds())
        })
    }

    /// The path mapped through `transform`.
    #[must_use]
    pub fn transformed(&self, transform: Affine) -> Self {
        Self {
            subpaths: self
                .subpaths
                .iter()
                .map(|subpath| subpath.transformed(transform))
                .collect(),
        }
    }

    /// The path with every sub-path run the other way.
    #[must_use]
    pub fn reversed(&self) -> Self {
        Self {
            subpaths: self.subpaths.iter().map(Subpath::reversed).collect(),
        }
    }

    /// Its signed area as a fill takes it, positive clockwise as drawn.
    #[must_use]
    pub fn signed_area(&self) -> f64 {
        self.subpaths.iter().map(Subpath::signed_area).sum()
    }

    /// The number of times it winds around `p`, as a fill takes it.
    #[must_use]
    pub fn winding(&self, p: Point) -> i64 {
        self.subpaths.iter().map(|subpath| subpath.winding(p)).sum()
    }

    /// Whether a fill under `rule` covers `p`.
    #[must_use]
    pub fn contains(&self, p: Point, rule: FillRule) -> bool {
        rule.inside(self.winding(p))
    }
}

/// The signed crossings of `segment` with the ray from `p` towards positive
/// x: `+1` for each crossing running down the page, `-1` for each running up.
///
/// Every crossing is counted over a half-open span of y — the lower end
/// included, the upper excluded — so a ray through a vertex shared by two
/// segments counts it once, and one grazing an extremum counts it not at all
/// or twice with opposite signs.
#[must_use]
pub fn segment_winding(segment: Segment, p: Point) -> i64 {
    match segment {
        Segment::Line(line) => line_winding(line.p0, line.p1, p),
        Segment::Cubic(cubic) => cubic_winding(cubic, p),
    }
}

fn line_winding(a: Point, b: Point, p: Point) -> i64 {
    let (low, high, direction) = if a.y < b.y { (a, b, 1) } else { (b, a, -1) };
    if !(low.y <= p.y && p.y < high.y) {
        return 0;
    }
    // The crossing lies right of p exactly where p lies left of the edge
    // as it runs down the page.
    if (high - low).cross(p - low) > 0.0 {
        direction
    } else {
        0
    }
}

fn cubic_winding(cubic: CubicBez, p: Point) -> i64 {
    let hull = cubic.control_bounds();
    if p.y < hull.min.y || p.y >= hull.max.y || p.x >= hull.max.x {
        return 0;
    }
    if p.x < hull.min.x {
        // Wholly right of p, the pieces' crossings telescope to the ends'.
        return span_winding(cubic.p0.y, cubic.p3.y, p.y);
    }
    let (a, b, c, _) = cubic.coefficients();
    let mut cuts = [0.0_f64; 4];
    let mut count = 1;
    for t in roots::quadratic(3.0 * a.y, 2.0 * b.y, c.y) {
        if t > 0.0 && t < 1.0 {
            cuts[count] = t;
            count += 1;
        }
    }
    cuts[count] = 1.0;
    count += 1;
    let mut winding = 0;
    for pair in cuts[..count].windows(2) {
        let (ta, tb) = (pair[0], pair[1]);
        let (ya, yb) = (cubic.eval(ta).y, cubic.eval(tb).y);
        let direction = span_winding(ya, yb, p.y);
        if direction != 0 && cubic.eval(crossing(cubic, (ta, tb), (ya, yb), p.y)).x > p.x {
            winding += direction;
        }
    }
    winding
}

/// `+1` or `-1` where the half-open span from `from` to `to` holds `y`, by
/// the direction it runs, and `0` otherwise.
fn span_winding(from: f64, to: f64, y: f64) -> i64 {
    if from < to && from <= y && y < to {
        1
    } else if to < from && to <= y && y < from {
        -1
    } else {
        0
    }
}

/// The parameter within `range` where the y-monotone piece of `cubic` there
/// reaches height `y`, which `heights` brackets: Newton's method held to the
/// bracket, bisecting wherever a step would leave it, so it always converges
/// on the piece's one crossing.
fn crossing(cubic: CubicBez, (ta, tb): (f64, f64), (ya, yb): (f64, f64), y: f64) -> f64 {
    // Orient the bracket so the height rises from `low` to `high`.
    let (mut low, mut high) = if ya < yb { (ta, tb) } else { (tb, ta) };
    let span = yb - ya;
    let mut t = ta + (tb - ta) * ((y - ya) / span);
    for _ in 0..CROSSING_STEPS {
        let height = cubic.eval(t).y - y;
        if height == 0.0 {
            return t;
        }
        if height < 0.0 {
            low = t;
        } else {
            high = t;
        }
        let slope = cubic.deriv(t).y;
        let stepped = t - height / slope;
        let (left, right) = if low < high { (low, high) } else { (high, low) };
        t = if slope != 0.0 && stepped > left && stepped < right {
            stepped
        } else {
            f64::midpoint(low, high)
        };
        if right - left <= f64::EPSILON * 4.0 {
            break;
        }
    }
    t
}

#[cfg(test)]
#[path = "path_tests.rs"]
mod tests;
