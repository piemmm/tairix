//! Shapes as the share of each pixel they cover.
//!
//! Positions are in 256ths of a pixel, so a pointer between pixel centres at
//! a high zoom lands where it was. A pixel wholly inside or outside a shape is
//! answered exactly from where its edges lie; one an edge crosses is sampled
//! sixteen times. Everything is integer arithmetic, so a shape covers the same
//! pixels on every machine.

/// One pixel, in the units positions are given in.
pub const FX: i64 = 256;

const HALF: i64 = FX / 2;

/// Past half a pixel's diagonal, rounded up: a pixel whose centre is further
/// than this inside or outside an edge lies wholly on that side.
const HALF_DIAGONAL: i64 = 182;

/// The offsets of a pixel's sixteen samples from its left or top edge.
const SAMPLES: [i64; 4] = [32, 96, 160, 224];

/// A position, in 256ths of a pixel.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Point {
    /// Across.
    pub x: i64,
    /// Down.
    pub y: i64,
}

impl Point {
    /// The centre of pixel `(x, y)`.
    #[must_use]
    pub const fn centre_of(x: i64, y: i64) -> Self {
        Self {
            x: x * FX + HALF,
            y: y * FX + HALF,
        }
    }

    /// The pixel this position falls in.
    #[must_use]
    pub const fn pixel(self) -> (i64, i64) {
        (self.x.div_euclid(FX), self.y.div_euclid(FX))
    }
}

/// Pixels `[x0, x1) × [y0, y1)`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Bounds {
    /// First column.
    pub x0: i64,
    /// First row.
    pub y0: i64,
    /// Column past the last.
    pub x1: i64,
    /// Row past the last.
    pub y1: i64,
}

impl Bounds {
    /// Whether no pixel lies inside.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }

    /// The pixels in both.
    #[must_use]
    pub fn intersection(&self, other: &Self) -> Self {
        Self {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        }
    }

    /// The least bounds holding both.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    /// A `width`×`height` picture's own pixels.
    #[must_use]
    pub fn picture(width: u32, height: u32) -> Self {
        Self {
            x0: 0,
            y0: 0,
            x1: i64::from(width),
            y1: i64::from(height),
        }
    }
}

/// Something drawn, as the share of each pixel it covers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Shape {
    /// A disc of `radius` swept from `a` to `b`: a brush's path, a thick
    /// line, one dab where the two ends meet.
    Capsule {
        /// Where the sweep starts.
        a: Point,
        /// Where it ends.
        b: Point,
        /// The disc's radius.
        radius: i64,
    },
    /// The pixels of a box, whole or as a border `outline` pixels wide.
    Rect {
        /// Pixels the box spans, inclusive.
        span: Span,
        /// The border's width, or `None` for the whole box.
        outline: Option<u32>,
    },
    /// The ellipse inscribed in a box of pixels, whole or as a ring
    /// `outline` pixels wide.
    Ellipse {
        /// Pixels the box spans, inclusive.
        span: Span,
        /// The ring's width, or `None` for the whole ellipse.
        outline: Option<u32>,
    },
}

/// A box of pixels from one corner to the other, inclusive, in either order.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Span {
    /// One corner.
    pub from: (i64, i64),
    /// The opposite corner.
    pub to: (i64, i64),
}

impl Span {
    /// The box's first and last column and row.
    #[must_use]
    pub fn corners(&self) -> (i64, i64, i64, i64) {
        (
            self.from.0.min(self.to.0),
            self.from.1.min(self.to.1),
            self.from.0.max(self.to.0),
            self.from.1.max(self.to.1),
        )
    }

    /// This box made square about `from`, its side the longer of the two.
    #[must_use]
    pub fn squared(self) -> Self {
        let (dx, dy) = (self.to.0 - self.from.0, self.to.1 - self.from.1);
        let side = dx.abs().max(dy.abs());
        let step = |d: i64| if d < 0 { -side } else { side };
        Self {
            from: self.from,
            to: (self.from.0 + step(dx), self.from.1 + step(dy)),
        }
    }

    /// The pixels the box covers.
    #[must_use]
    pub fn bounds(&self) -> Bounds {
        let (x0, y0, x1, y1) = self.corners();
        Bounds {
            x0,
            y0,
            x1: x1 + 1,
            y1: y1 + 1,
        }
    }
}

impl Shape {
    /// The pixels the shape may cover.
    #[must_use]
    pub fn bounds(&self) -> Bounds {
        match *self {
            Self::Capsule { a, b, radius } => {
                let reach = radius + HALF_DIAGONAL;
                Bounds {
                    x0: (a.x.min(b.x) - reach).div_euclid(FX),
                    y0: (a.y.min(b.y) - reach).div_euclid(FX),
                    x1: (a.x.max(b.x) + reach).div_euclid(FX) + 1,
                    y1: (a.y.max(b.y) + reach).div_euclid(FX) + 1,
                }
            }
            Self::Rect { span, .. } | Self::Ellipse { span, .. } => span.bounds(),
        }
    }

    /// Write the coverage of row `y` from column `x` into `out`, a pixel an
    /// entry, `255` wholly covered. With `aa` off a pixel is covered or not,
    /// by whether its centre is inside.
    pub fn row(&self, y: i64, x: i64, aa: bool, out: &mut [u8]) {
        match *self {
            Self::Capsule { a, b, radius } => capsule_row(a, b, radius, y, x, aa, out),
            Self::Rect { span, outline } => rect_row(span, outline, y, x, out),
            Self::Ellipse { span, outline } => ellipse_row(span, outline, y, x, aa, out),
        }
    }
}

/// The squared distance from `p` to the segment from `a` to `b`.
fn segment_distance2(p: Point, a: Point, b: Point) -> i128 {
    let (vx, vy) = (i128::from(b.x - a.x), i128::from(b.y - a.y));
    let (wx, wy) = (i128::from(p.x - a.x), i128::from(p.y - a.y));
    let length2 = vx * vx + vy * vy;
    let dot = wx * vx + wy * vy;
    if length2 == 0 || dot <= 0 {
        return wx * wx + wy * wy;
    }
    if dot >= length2 {
        let (ex, ey) = (i128::from(p.x - b.x), i128::from(p.y - b.y));
        return ex * ex + ey * ey;
    }
    let cross = wx * vy - wy * vx;
    cross * cross / length2
}

/// The pixels of row `y` whose centres may lie within `reach` of the segment
/// from `a` to `b`, as `[first, last]`, a pixel wider each side than exact;
/// `None` for a row the capsule misses. The capsule is convex, so a row meets
/// it in one run: the hull of where it meets the two end discs and the band
/// the segment sweeps.
fn capsule_span(a: Point, b: Point, reach: i64, y: i64) -> Option<(i64, i64)> {
    let centre = i128::from(y * FX + HALF);
    let reach = i128::from(reach);
    let mut hull: Option<(i128, i128)> = None;
    let mut take = |left: i128, right: i128| {
        if left <= right {
            hull = Some(hull.map_or((left, right), |(l, r)| (l.min(left), r.max(right))));
        }
    };
    for end in [a, b] {
        let dy = centre - i128::from(end.y);
        if dy * dy <= reach * reach {
            let half = (reach * reach - dy * dy).isqrt();
            take(i128::from(end.x) - half, i128::from(end.x) + half);
        }
    }
    let (vx, vy) = (i128::from(b.x - a.x), i128::from(b.y - a.y));
    let length2 = vx * vx + vy * vy;
    if length2 > 0 {
        // Within the band: |(p - a) × v| ≤ reach·|v|, and 0 ≤ (p - a)·v ≤ |v|²,
        // each linear in the row's x; the root is rounded up, so the span only
        // ever widens.
        let width = reach * (length2.isqrt() + 1);
        let dy = centre - i128::from(a.y);
        let across = solve_between(vy, dy * vx - width, dy * vx + width);
        let along = solve_between(vx, -dy * vy, length2 - dy * vy);
        if let (Some((l0, r0)), Some((l1, r1))) = (across, along) {
            let ax = i128::from(a.x);
            take(ax + l0.max(l1), ax + r0.min(r1));
        }
    }
    let (left, right) = hull?;
    let pixel = |at: i128| i64::try_from((at - i128::from(HALF)).div_euclid(i128::from(FX))).ok();
    Some((pixel(left)? - 1, pixel(right)? + 1))
}

/// The `t` for which `low ≤ t·slope ≤ high`, as an interval: every `t` for a
/// flat `slope` that already meets it, none for one that cannot.
fn solve_between(slope: i128, low: i128, high: i128) -> Option<(i128, i128)> {
    match slope.signum() {
        0 => (low <= 0 && 0 <= high).then_some((i128::MIN / 4, i128::MAX / 4)),
        1 => Some((low.div_euclid(slope), high.div_euclid(slope) + 1)),
        _ => Some(((-high).div_euclid(-slope), (-low).div_euclid(-slope) + 1)),
    }
}

fn capsule_row(a: Point, b: Point, radius: i64, y: i64, x: i64, aa: bool, out: &mut [u8]) {
    let reach = if aa { radius + HALF_DIAGONAL } else { radius };
    // Only the run the capsule can reach is measured; the rest is uncovered.
    let Some((first, last)) = capsule_span(a, b, reach, y) else {
        out.fill(0);
        return;
    };
    let end = x.saturating_add(i64::try_from(out.len()).unwrap_or(i64::MAX));
    let from = usize::try_from(first.clamp(x, end) - x).unwrap_or(0);
    let to = usize::try_from((last + 1).clamp(x, end) - x)
        .unwrap_or(0)
        .max(from);
    out[..from].fill(0);
    out[to..].fill(0);
    let x = x + i64::try_from(from).unwrap_or(0);
    for (at, cover) in (x..).zip(out[from..to].iter_mut()) {
        *cover = capsule_cover(a, b, radius, at, y, aa);
    }
}

/// The share of pixel `(x, y)` the capsule about the segment from `a` to `b`
/// covers: whole pixels only unless `aa`.
fn capsule_cover(a: Point, b: Point, radius: i64, x: i64, y: i64, aa: bool) -> u8 {
    let r2 = i128::from(radius) * i128::from(radius);
    let d2 = segment_distance2(Point::centre_of(x, y), a, b);
    if !aa {
        return if d2 <= r2 { 255 } else { 0 };
    }
    let inner = i128::from((radius - HALF_DIAGONAL).max(0));
    let outer = i128::from(radius + HALF_DIAGONAL);
    if radius > HALF_DIAGONAL && d2 <= inner * inner {
        255
    } else if d2 >= outer * outer {
        0
    } else {
        sampled(x, y, |p| segment_distance2(p, a, b) <= r2)
    }
}

/// The share of pixel `(x, y)` whose sixteen samples `inside` accepts.
fn sampled(x: i64, y: i64, inside: impl Fn(Point) -> bool) -> u8 {
    let mut count = 0u32;
    for dy in SAMPLES {
        for dx in SAMPLES {
            count += u32::from(inside(Point {
                x: x * FX + dx,
                y: y * FX + dy,
            }));
        }
    }
    u8::try_from((count * 255 + 8) / 16).unwrap_or(u8::MAX)
}

/// `out[i]`, column `x + i`, for the columns `from..=to`, clipped to `out`.
fn columns(out: &mut [u8], x: i64, from: i64, to: i64) -> &mut [u8] {
    let len = i64::try_from(out.len()).unwrap_or(i64::MAX);
    let start = from.saturating_sub(x).clamp(0, len);
    let end = to.saturating_sub(x).saturating_add(1).clamp(start, len);
    let (Ok(start), Ok(end)) = (usize::try_from(start), usize::try_from(end)) else {
        return &mut [];
    };
    &mut out[start..end]
}

/// A rectangle's row is whole runs: the span across, or its two walls on a
/// row its hollow crosses.
fn rect_row(span: Span, outline: Option<u32>, y: i64, x: i64, out: &mut [u8]) {
    out.fill(0);
    let (x0, y0, x1, y1) = span.corners();
    if !(y0..=y1).contains(&y) {
        return;
    }
    match outline.map(i64::from) {
        Some(w) if (y0 + w..=y1 - w).contains(&y) => {
            columns(out, x, x0, (x0 + w - 1).min(x1)).fill(u8::MAX);
            columns(out, x, (x1 - w + 1).max(x0), x1).fill(u8::MAX);
        }
        _ => columns(out, x, x0, x1).fill(u8::MAX),
    }
}

/// An ellipse's centre and radii, in 256ths of a pixel.
#[derive(Copy, Clone)]
struct Oval {
    cx: i64,
    cy: i64,
    rx: i64,
    ry: i64,
}

impl Oval {
    fn of(span: Span, inset: i64) -> Option<Self> {
        let (x0, y0, x1, y1) = span.corners();
        let oval = Self {
            cx: (x0 + x1 + 1) * HALF,
            cy: (y0 + y1 + 1) * HALF,
            rx: (x1 - x0 + 1) * HALF - inset * FX,
            ry: (y1 - y0 + 1) * HALF - inset * FX,
        };
        (oval.rx > 0 && oval.ry > 0).then_some(oval)
    }

    fn inside(&self, p: Point) -> bool {
        let dx = i128::from(p.x - self.cx) * i128::from(self.ry);
        let dy = i128::from(p.y - self.cy) * i128::from(self.rx);
        let r = i128::from(self.rx) * i128::from(self.ry);
        dx * dx + dy * dy <= r * r
    }

    /// Half the ellipse's width at `dy` from its centre line, rounded down,
    /// or `None` past its top or bottom.
    fn half_width(&self, dy: i64) -> Option<i64> {
        let dy = dy.abs();
        if dy >= self.ry {
            return None;
        }
        let root = (self.ry * self.ry - dy * dy).isqrt();
        Some(self.rx * root / self.ry)
    }

    /// Half the ellipse's width at `dy`, rounded up so it never falls short.
    fn half_width_up(&self, dy: i64) -> Option<i64> {
        let dy = dy.abs();
        if dy >= self.ry {
            return None;
        }
        let root = (self.ry * self.ry - dy * dy).isqrt() + 1;
        Some(((self.rx * root + self.ry - 1) / self.ry).min(self.rx))
    }

    /// The coverage of row `y` from column `x`. Only the columns the row
    /// meets the ellipse in are looked at, and only those its edge crosses
    /// are tested: the cost is the ellipse's, not the row's.
    fn row(&self, y: i64, x: i64, aa: bool, out: &mut [u8]) {
        out.fill(0);
        let (top, bottom) = (y * FX - self.cy, (y + 1) * FX - self.cy);
        let within = self.half_width(top).zip(self.half_width(bottom));
        let inner = within.map(|(a, b)| a.min(b));
        let outer = if top <= 0 && bottom >= 0 {
            Some(self.rx)
        } else {
            match (self.half_width_up(top), self.half_width_up(bottom)) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (one, other) => one.or(other),
            }
        };
        let Some(outer) = outer else {
            return;
        };
        let (first, last) = (
            (self.cx - outer).div_euclid(FX),
            (self.cx + outer).div_euclid(FX),
        );
        for (at, cover) in (first.max(x)..).zip(columns(out, x, first, last)) {
            let (left, right) = (at * FX - self.cx, (at + 1) * FX - self.cx);
            *cover = if inner.is_some_and(|h| -h <= left && right <= h) {
                255
            } else if !aa {
                if self.inside(Point::centre_of(at, y)) {
                    255
                } else {
                    0
                }
            } else if right <= -outer || left >= outer {
                0
            } else {
                sampled(at, y, |p| self.inside(p))
            };
        }
    }
}

fn ellipse_row(span: Span, outline: Option<u32>, y: i64, x: i64, aa: bool, out: &mut [u8]) {
    let Some(oval) = Oval::of(span, 0) else {
        out.fill(0);
        return;
    };
    oval.row(y, x, aa, out);
    let Some(width) = outline else {
        return;
    };
    let Some(hole) = Oval::of(span, i64::from(width)) else {
        return;
    };
    // The ring is the ellipse less the one inside it, pixel by pixel: what
    // the inner one covers, the outer covers too.
    let mut inner = [0u8; 64];
    for (index, chunk) in out.chunks_mut(inner.len()).enumerate() {
        let start = x + i64::try_from(index * inner.len()).unwrap_or(i64::MAX);
        let inner = &mut inner[..chunk.len()];
        hole.row(y, start, aa, inner);
        for (cover, taken) in chunk.iter_mut().zip(inner.iter()) {
            *cover = cover.saturating_sub(*taken);
        }
    }
}

/// Every pixel on the one-pixel line from `a` to `b`, `a` first: what a
/// pencil marks.
pub fn line_pixels(a: (i64, i64), b: (i64, i64), mut mark: impl FnMut(i64, i64)) {
    let (dx, dy) = ((b.0 - a.0).abs(), -(b.1 - a.1).abs());
    let (sx, sy) = (
        if a.0 < b.0 { 1 } else { -1 },
        if a.1 < b.1 { 1 } else { -1 },
    );
    let (mut x, mut y) = a;
    let mut error = dx + dy;
    loop {
        mark(x, y);
        if (x, y) == b {
            return;
        }
        let twice = 2 * error;
        if twice >= dy {
            error += dy;
            x += sx;
        }
        if twice <= dx {
            error += dx;
            y += sy;
        }
    }
}

#[cfg(test)]
#[path = "shape_tests.rs"]
mod tests;
