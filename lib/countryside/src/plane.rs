//! The plan of the land: places, rectangles, polylines and convex polygons,
//! in metres.

use core::ops::{Add, Mul, Neg, Sub};

use alloc::vec::Vec;

use tairix_util::mathf;

use crate::Error;

/// A place on the plan of the land, in metres.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Point {
    /// How far along the plan's first axis.
    pub x: f64,
    /// How far along its second.
    pub y: f64,
}

impl Point {
    /// The place `(x, y)`.
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// The dot product.
    #[must_use]
    pub fn dot(self, other: Self) -> f64 {
        self.x * other.x + self.y * other.y
    }

    /// The `z` of the cross product: positive where `other` lies
    /// anticlockwise of `self`.
    #[must_use]
    pub fn cross(self, other: Self) -> f64 {
        self.x * other.y - self.y * other.x
    }

    /// How long it is.
    #[must_use]
    pub fn length(self) -> f64 {
        mathf::hypot(self.x, self.y)
    }

    /// The way `self` points, or nought for nought.
    #[must_use]
    pub fn normalized(self) -> Self {
        let length = self.length();
        if length > 0.0 {
            self * (1.0 / length)
        } else {
            Self::default()
        }
    }

    /// `self` turned a quarter anticlockwise.
    #[must_use]
    pub const fn left(self) -> Self {
        Self::new(-self.y, self.x)
    }

    /// The point `t` of the way from `self` to `other`.
    #[must_use]
    pub fn lerp(self, other: Self, t: f64) -> Self {
        self + (other - self) * t
    }

    /// The unit way at `heading` radians anticlockwise of `x`.
    #[must_use]
    pub fn toward(heading: f64) -> Self {
        Self::new(mathf::cos(heading), mathf::sin(heading))
    }

    /// The heading of `self`, in radians anticlockwise of `x`.
    #[must_use]
    pub fn heading(self) -> f64 {
        mathf::atan2(self.y, self.x)
    }
}

impl Add for Point {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y)
    }
}

impl Sub for Point {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y)
    }
}

impl Mul<f64> for Point {
    type Output = Self;
    fn mul(self, scale: f64) -> Self {
        Self::new(self.x * scale, self.y * scale)
    }
}

impl Neg for Point {
    type Output = Self;
    fn neg(self) -> Self {
        Self::new(-self.x, -self.y)
    }
}

/// A rectangle from its low corner to its high, its edges inclusive.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Rect {
    /// Its corner least along both axes.
    pub low: Point,
    /// Its corner furthest along both.
    pub high: Point,
}

impl Rect {
    /// The square `reach` either way of `centre`.
    #[must_use]
    pub fn around(centre: Point, reach: f64) -> Self {
        Self {
            low: Point::new(centre.x - reach, centre.y - reach),
            high: Point::new(centre.x + reach, centre.y + reach),
        }
    }

    /// The least rectangle holding every point of `points`; `None` for none.
    pub fn of(points: impl IntoIterator<Item = Point>) -> Option<Self> {
        points.into_iter().fold(None, |held: Option<Self>, point| {
            Some(held.map_or(
                Self {
                    low: point,
                    high: point,
                },
                |rect| rect.including(point),
            ))
        })
    }

    /// The least rectangle holding it and `point`.
    #[must_use]
    pub fn including(self, point: Point) -> Self {
        Self {
            low: Point::new(self.low.x.min(point.x), self.low.y.min(point.y)),
            high: Point::new(self.high.x.max(point.x), self.high.y.max(point.y)),
        }
    }

    /// The rectangle grown `by` every way.
    #[must_use]
    pub fn grown(self, by: f64) -> Self {
        Self {
            low: Point::new(self.low.x - by, self.low.y - by),
            high: Point::new(self.high.x + by, self.high.y + by),
        }
    }

    /// Whether `point` lies within it.
    #[must_use]
    pub fn contains(self, point: Point) -> bool {
        (self.low.x..=self.high.x).contains(&point.x) && (self.low.y..=self.high.y).contains(&point.y)
    }

    /// Whether it and `other` share any place.
    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        self.low.x <= other.high.x
            && other.low.x <= self.high.x
            && self.low.y <= other.high.y
            && other.low.y <= self.high.y
    }
}

/// Where along a polyline the point nearest another lies.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Nearest {
    /// How far the point is from the polyline.
    pub distance: f64,
    /// How far along the polyline the nearest point lies, from its start.
    pub along: f64,
    /// The segment the nearest point lies on, from its start vertex.
    pub segment: usize,
    /// Which side of the polyline the point lies: positive to the left of its
    /// way.
    pub side: f64,
}

/// The nearest point of segment `a`–`b` to `p`: how far along it, as a share,
/// and the squared distance to it.
#[must_use]
pub fn onto_segment(p: Point, a: Point, b: Point) -> (f64, f64) {
    let span = b - a;
    let reach = span.dot(span);
    let t = if reach > 0.0 {
        ((p - a).dot(span) / reach).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let gap = p - a.lerp(b, t);
    (t, gap.dot(gap))
}

/// The point of `line` nearest `p`; `None` for a line of no segment.
#[must_use]
pub fn nearest(line: &[Point], p: Point) -> Option<Nearest> {
    let mut best: Option<(f64, Nearest)> = None;
    let mut walked = 0.0;
    for (segment, pair) in line.windows(2).enumerate() {
        let [a, b] = [pair[0], pair[1]];
        let (t, squared) = onto_segment(p, a, b);
        let length = (b - a).length();
        if best.is_none_or(|(least, _)| squared < least) {
            best = Some((
                squared,
                Nearest {
                    distance: mathf::sqrt(squared),
                    along: walked + t * length,
                    segment,
                    side: (b - a).cross(p - a),
                },
            ));
        }
        walked += length;
    }
    best.map(|(_, nearest)| nearest)
}

/// How long `line` runs.
#[must_use]
pub fn length(line: &[Point]) -> f64 {
    line.windows(2).map(|pair| (pair[1] - pair[0]).length()).sum()
}

/// The point `along` the way along `line` from its start, and its way there;
/// its ends past either end.
#[must_use]
pub fn at(line: &[Point], along: f64) -> Option<(Point, Point)> {
    Walk::new(line).at(along)
}

/// A walk along a line, read at places along it in turn: each read goes on
/// from the segment the last stopped in, so reading a line from its start to
/// its end walks it once.
#[derive(Clone, Debug)]
pub struct Walk<'a> {
    line: &'a [Point],
    segment: usize,
    /// How long the line runs before `segment`.
    walked: f64,
}

impl<'a> Walk<'a> {
    /// A walk from the start of `line`.
    #[must_use]
    pub const fn new(line: &'a [Point]) -> Self {
        Self {
            line,
            segment: 0,
            walked: 0.0,
        }
    }

    /// The point `along` the line and its way there, exactly as [`at`] has
    /// them: a place no further on than where the walk stands walks again
    /// from the line's start.
    pub fn at(&mut self, along: f64) -> Option<(Point, Point)> {
        if self.segment > 0 && along <= self.walked {
            (self.segment, self.walked) = (0, 0.0);
        }
        let last = self.line.len().checked_sub(2)?;
        while let Some(&[a, b]) = self.line.get(self.segment..self.segment + 2) {
            let span = (b - a).length();
            if self.walked + span >= along || self.segment == last {
                let t = if span > 0.0 {
                    ((along - self.walked) / span).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                return Some((a.lerp(b, t), (b - a).normalized()));
            }
            self.walked += span;
            self.segment += 1;
        }
        None
    }
}

/// Where segments `a0`–`a1` and `b0`–`b1` cross, as how far along each, as a
/// share; `None` where they do not, or run parallel.
#[must_use]
pub fn crossing((a0, a1): (Point, Point), (b0, b1): (Point, Point)) -> Option<(f64, f64)> {
    let (r, s) = (a1 - a0, b1 - b0);
    let denominator = r.cross(s);
    if denominator.abs() <= 1e-12 * r.length() * s.length() {
        return None;
    }
    let gap = b0 - a0;
    let (t, u) = (gap.cross(s) / denominator, gap.cross(r) / denominator);
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)).then_some((t, u))
}

/// A convex polygon, its corners anticlockwise.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Convex {
    /// Its corners, anticlockwise.
    pub corners: Vec<Point>,
}

impl Convex {
    /// Its area.
    #[must_use]
    pub fn area(&self) -> f64 {
        self.edges().map(|(a, b)| a.cross(b)).sum::<f64>() * 0.5
    }

    /// Its middle by area; its first corner for a polygon of no area.
    #[must_use]
    pub fn centroid(&self) -> Point {
        let area = self.area();
        if area.abs() <= 1e-12 {
            return self.corners.first().copied().unwrap_or_default();
        }
        let (sx, sy) = self.edges().fold((0.0, 0.0), |(sx, sy), (a, b)| {
            let weight = a.cross(b);
            (sx + (a.x + b.x) * weight, sy + (a.y + b.y) * weight)
        });
        Point::new(sx / (6.0 * area), sy / (6.0 * area))
    }

    /// Each edge from one corner to the next, the last back to the first.
    pub fn edges(&self) -> impl Iterator<Item = (Point, Point)> + '_ {
        let count = self.corners.len();
        (0..count).map(move |index| (self.corners[index], self.corners[(index + 1) % count]))
    }

    /// Whether `p` lies within it, its edges included.
    #[must_use]
    pub fn contains(&self, p: Point) -> bool {
        self.corners.len() >= 3 && self.edges().all(|(a, b)| (b - a).cross(p - a) >= 0.0)
    }

    /// The part of it on the side of the line through `on` that `normal`
    /// points away from: where `(p - on) · normal <= 0`. `None` where the
    /// heap will not hold it.
    #[must_use]
    pub fn behind(&self, on: Point, normal: Point) -> Option<Self> {
        let side = |p: Point| (p - on).dot(normal);
        let mut corners = Vec::new();
        corners.try_reserve(self.corners.len() + 1).ok()?;
        for (a, b) in self.edges() {
            let (sa, sb) = (side(a), side(b));
            if sa <= 0.0 {
                corners.push(a);
            }
            if (sa <= 0.0) != (sb <= 0.0) {
                corners.push(a.lerp(b, sa / (sa - sb)));
            }
        }
        Some(Self { corners })
    }

    /// The chord of the line through `on` across `normal` within it, in the
    /// order the line runs along `normal.left()`; `None` where the line
    /// misses it.
    #[must_use]
    pub fn chord(&self, on: Point, normal: Point) -> Option<(Point, Point)> {
        let side = |p: Point| (p - on).dot(normal);
        let along = |p: Point| (p - on).dot(normal.left());
        let mut ends: Option<(Point, Point)> = None;
        for (a, b) in self.edges() {
            let (sa, sb) = (side(a), side(b));
            if (sa <= 0.0) == (sb <= 0.0) {
                continue;
            }
            let crossing = a.lerp(b, sa / (sa - sb));
            ends = Some(ends.map_or((crossing, crossing), |(first, last)| {
                (
                    if along(crossing) < along(first) { crossing } else { first },
                    if along(crossing) > along(last) { crossing } else { last },
                )
            }));
        }
        ends
    }

    /// How far `p` lies from it: nought within it.
    #[must_use]
    pub fn distance(&self, p: Point) -> f64 {
        if self.contains(p) {
            return 0.0;
        }
        mathf::sqrt(
            self.edges()
                .map(|(a, b)| onto_segment(p, a, b).1)
                .fold(f64::INFINITY, f64::min),
        )
    }

    /// The bounding rectangle; `None` for no corners.
    #[must_use]
    pub fn bounds(&self) -> Option<Rect> {
        Rect::of(self.corners.iter().copied())
    }

    /// Whether it and `other` share any place but their edges: no edge of
    /// either has the other wholly on its outside.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        let parted = |by: &Self, them: &Self| {
            by.edges().any(|(a, b)| {
                let normal = (b - a).left();
                them.corners.iter().all(|&corner| (corner - a).dot(normal) <= 0.0)
            })
        };
        !(parted(self, other) || parted(other, self))
    }
}

/// Items filed by the squares of a grid their rectangles reach, so the items
/// that may reach a place are found among its own square's alone.
#[derive(Clone, Debug, Default)]
pub(crate) struct Buckets {
    origin: Point,
    size: f64,
    columns: u32,
    rows: u32,
    /// Each filing's square, then its item, in that order once sorted.
    filed: Vec<(u32, u32)>,
}

impl Buckets {
    /// Empty squares `size` across over `extent`.
    pub(crate) fn over(extent: Rect, size: f64) -> Self {
        let count = |span: f64| u32::try_from(mathf::round_i32(mathf::ceil(span / size).clamp(1.0, 1.0e6))).unwrap_or(1);
        Self {
            origin: extent.low,
            size,
            columns: count(extent.high.x - extent.low.x),
            rows: count(extent.high.y - extent.low.y),
            filed: Vec::new(),
        }
    }

    /// The column or row a coordinate `offset` from the first square's
    /// corner falls in, held to the `count` there are.
    fn square(&self, offset: f64, count: u32) -> u32 {
        let whole = mathf::round_i32(mathf::floor(offset / self.size).clamp(0.0, 1.0e6));
        u32::try_from(whole).unwrap_or(0).min(count.saturating_sub(1))
    }

    /// File `item` in every square `rect` reaches.
    pub(crate) fn file(&mut self, rect: Rect, item: u32) -> Result<(), Error> {
        let (high_x, high_y) = (self.origin.x + self.size * f64::from(self.columns), self.origin.y + self.size * f64::from(self.rows));
        if rect.high.x < self.origin.x || rect.high.y < self.origin.y || rect.low.x > high_x || rect.low.y > high_y {
            return Ok(());
        }
        let (x0, x1) = (self.square(rect.low.x - self.origin.x, self.columns), self.square(rect.high.x - self.origin.x, self.columns));
        let (y0, y1) = (self.square(rect.low.y - self.origin.y, self.rows), self.square(rect.high.y - self.origin.y, self.rows));
        for y in y0..=y1 {
            for x in x0..=x1 {
                self.filed.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                self.filed.push((y * self.columns + x, item));
            }
        }
        Ok(())
    }

    /// Put the filings in order, ready to be found.
    pub(crate) fn sort(&mut self) {
        self.filed.sort_unstable();
    }

    /// The items filed in the square `at` lies in, in order; none outside
    /// the squares.
    pub(crate) fn at(&self, at: Point) -> impl Iterator<Item = u32> + '_ {
        let (x, y) = (at.x - self.origin.x, at.y - self.origin.y);
        let inside = x >= 0.0
            && y >= 0.0
            && x < self.size * f64::from(self.columns)
            && y < self.size * f64::from(self.rows);
        let bucket = inside.then(|| self.square(y, self.rows) * self.columns + self.square(x, self.columns));
        let from = bucket.map_or(self.filed.len(), |bucket| self.filed.partition_point(|entry| entry.0 < bucket));
        self.filed[from..]
            .iter()
            .take_while(move |entry| Some(entry.0) == bucket)
            .map(|entry| entry.1)
    }
}

#[cfg(test)]
#[path = "plane_tests.rs"]
mod tests;
