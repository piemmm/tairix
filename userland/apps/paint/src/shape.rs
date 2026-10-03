//! Shapes as the share of each pixel they cover.
//!
//! Positions are in 256ths of a pixel, so a pointer between pixel centres at
//! a high zoom lands where it was. A shape is traced as closed contours —
//! a curve to within a sixteenth of a pixel — and its coverage is answered by
//! `lib/raster`'s one scan converter: the exact share of each pixel's area,
//! or, without antialiasing, whether the pixel's centre is inside. Paint has
//! no rasteriser of its own.

use alloc::vec::Vec;

use tairix_raster::{Coverage, CoverageRows, FillRule, ScanScratch};
use tairix_util::mathf;

use crate::canvas::OutOfMemory;

/// One pixel, in the units positions are given in.
pub const FX: i64 = 256;

const HALF: i64 = FX / 2;

/// Past half a pixel's diagonal, rounded up: a pixel whose centre is further
/// than this inside or outside an edge lies wholly on that side.
const HALF_DIAGONAL: i64 = 182;

/// How far a traced curve may stray from the true one, in 256ths of a pixel:
/// a sixty-fourth, so the area a chord cuts off is lost in the rounding.
const TOLERANCE: f64 = 4.0;

/// The fewest and the most vertices a traced closed curve takes: few enough
/// that the largest picture's ellipse is cheap, many enough for the
/// tolerance at that size.
const FEWEST_VERTICES: usize = 8;
const MOST_VERTICES: usize = 1 << 14;

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
    /// A box of pixels with each corner rounded to `radius` pixels, whole
    /// or as a border `outline` pixels wide.
    Rounded {
        /// Pixels the box spans, inclusive.
        span: Span,
        /// The border's width, or `None` for the whole box.
        outline: Option<u32>,
        /// The corners' radius, in pixels; held to half the box's shorter
        /// side.
        radius: u32,
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
            Self::Rect { span, .. } | Self::Rounded { span, .. } | Self::Ellipse { span, .. } => {
                span.bounds()
            }
        }
    }

    /// Trace the shape into `scratch` and answer its coverage a row at a
    /// time: a pixel's exact share when `aa`, else whole where its centre is
    /// inside. `None` for a shape that covers nothing.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the outline cannot be held.
    pub fn rows<'s>(
        &self,
        aa: bool,
        scratch: &'s mut ShapeScratch,
    ) -> Result<Option<CoverageRows<'s>>, OutOfMemory> {
        let ShapeScratch { scan, outer, inner } = scratch;
        outer.clear();
        inner.clear();
        let coverage = if aa { Coverage::Area } else { Coverage::Centre };
        match *self {
            Self::Capsule { a, b, radius } => trace_capsule(a, b, radius, coverage, outer)?,
            Self::Rect { span, outline } => {
                let (x0, y0, x1, y1) = span.corners();
                trace_box((x0, y0, x1 + 1, y1 + 1), outer)?;
                if let Some(width) = outline.map(i64::from) {
                    if x0 + width <= x1 - width && y0 + width <= y1 - width {
                        trace_box(
                            (x0 + width, y0 + width, x1 + 1 - width, y1 + 1 - width),
                            inner,
                        )?;
                    }
                }
            }
            Self::Rounded {
                span,
                outline,
                radius,
            } => {
                let (x0, y0, x1, y1) = span.corners();
                let radius = i64::from(radius) * FX;
                trace_rounded((x0, y0, x1 + 1, y1 + 1), radius, coverage, outer)?;
                if let Some(width) = outline.map(i64::from) {
                    if x0 + width <= x1 - width && y0 + width <= y1 - width {
                        let hollow = (x0 + width, y0 + width, x1 + 1 - width, y1 + 1 - width);
                        trace_rounded(hollow, radius - width * FX, coverage, inner)?;
                    }
                }
            }
            Self::Ellipse { span, outline } => {
                let Some(oval) = Oval::of(span, 0) else {
                    return Ok(None);
                };
                oval.trace(coverage, outer)?;
                if let Some(hole) = outline.and_then(|width| Oval::of(span, i64::from(width))) {
                    hole.trace(coverage, inner)?;
                }
            }
        }
        let units = u32::try_from(FX).map_err(|_| OutOfMemory)?;
        // A hollow lies inside its outline, so the even-odd rule makes it a
        // hole however it is wound.
        let contours: [&[(i32, i32)]; 2] = [outer, inner];
        Ok(scan.coverage_rows(&contours, units, FillRule::EvenOdd, coverage))
    }
}

impl Shape {
    /// The shape's outer outline as it is traced for its exact area, in
    /// 256ths of a pixel: what a selection being marked out draws.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the outline cannot be held.
    pub fn outline<'s>(
        &self,
        scratch: &'s mut ShapeScratch,
    ) -> Result<&'s [(i32, i32)], OutOfMemory> {
        let outer = &mut scratch.outer;
        outer.clear();
        match *self {
            Self::Capsule { a, b, radius } => trace_capsule(a, b, radius, Coverage::Area, outer)?,
            Self::Rect { span, .. } => {
                let (x0, y0, x1, y1) = span.corners();
                trace_box((x0, y0, x1 + 1, y1 + 1), outer)?;
            }
            Self::Rounded { span, radius, .. } => {
                let (x0, y0, x1, y1) = span.corners();
                let radius = i64::from(radius) * FX;
                trace_rounded((x0, y0, x1 + 1, y1 + 1), radius, Coverage::Area, outer)?;
            }
            Self::Ellipse { span, .. } => {
                if let Some(oval) = Oval::of(span, 0) {
                    oval.trace(Coverage::Area, outer)?;
                }
            }
        }
        Ok(outer)
    }
}

/// The coverage of the closed outline through `points`, a lasso's path or
/// a polygon's corners, and the pixels it may cover: whole where a pixel's
/// centre is inside unless `aa`. `None` for an outline enclosing nothing.
///
/// # Errors
///
/// [`OutOfMemory`] when the outline cannot be held.
pub fn polygon_rows<'s>(
    points: &[Point],
    aa: bool,
    scratch: &'s mut ShapeScratch,
) -> Result<Option<(CoverageRows<'s>, Bounds)>, OutOfMemory> {
    let ShapeScratch { scan, outer, inner } = scratch;
    outer.clear();
    inner.clear();
    room(outer, points.len())?;
    let mut bounds: Option<Bounds> = None;
    for point in points {
        outer.push(vertex(real(point.x), real(point.y)));
        let (x, y) = point.pixel();
        let pixel = Bounds {
            x0: x,
            y0: y,
            x1: x + 1,
            y1: y + 1,
        };
        bounds = Some(bounds.map_or(pixel, |held| held.union(&pixel)));
    }
    let Some(bounds) = bounds else {
        return Ok(None);
    };
    let coverage = if aa { Coverage::Area } else { Coverage::Centre };
    let units = u32::try_from(FX).map_err(|_| OutOfMemory)?;
    let contours: [&[(i32, i32)]; 1] = [outer];
    Ok(scan
        .coverage_rows(&contours, units, FillRule::NonZero, coverage)
        .map(|rows| (rows, bounds)))
}

/// What tracing a shape works in: the converter's own buffers and the
/// outlines, held across shapes so a stroke of many dabs allocates once.
#[derive(Debug, Default)]
pub struct ShapeScratch {
    scan: ScanScratch,
    outer: Vec<(i32, i32)>,
    inner: Vec<(i32, i32)>,
}

/// A position in 256ths of a pixel as the converter takes one.
fn vertex(x: f64, y: f64) -> (i32, i32) {
    (mathf::round_i32(x), mathf::round_i32(y))
}

/// Room for `count` more vertices in `out`.
fn room(out: &mut Vec<(i32, i32)>, count: usize) -> Result<(), OutOfMemory> {
    out.try_reserve(count).map_err(|_| OutOfMemory)
}

/// The pixels `[x0, x1) × [y0, y1)` as a contour along their edges.
fn trace_box(
    (x0, y0, x1, y1): (i64, i64, i64, i64),
    out: &mut Vec<(i32, i32)>,
) -> Result<(), OutOfMemory> {
    room(out, 4)?;
    let corner = |x: i64, y: i64| {
        let at = |v: i64| i32::try_from(v * FX).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX });
        (at(x), at(y))
    };
    out.extend([
        corner(x0, y0),
        corner(x1, y0),
        corner(x1, y1),
        corner(x0, y1),
    ]);
    Ok(())
}

/// The pixels `[x0, x1) × [y0, y1)` with each corner rounded to `radius`
/// 256ths of a pixel — held to half the shorter side — as a contour: a
/// quarter curve about each corner, all four mirrored from one so the box is
/// exactly symmetric, joined by the straight sides.
fn trace_rounded(
    (x0, y0, x1, y1): (i64, i64, i64, i64),
    radius: i64,
    coverage: Coverage,
    out: &mut Vec<(i32, i32)>,
) -> Result<(), OutOfMemory> {
    let (left, top, right, bottom) = (x0 * FX, y0 * FX, x1 * FX, y1 * FX);
    let radius = radius.min((right - left) / 2).min((bottom - top) / 2);
    if radius <= 0 {
        return trace_box((x0, y0, x1, y1), out);
    }
    let count = vertices_for(real(radius));
    let (factor, pad) = placement(count, coverage);
    let r = real(radius) * factor + pad;
    let quarter = count / 4;
    room(out, 4 * (quarter + 1))?;
    let step = core::f64::consts::FRAC_PI_2 / real_count(quarter);
    let offset = |index: usize| {
        let angle = step * real_count(index);
        (
            i64::from(mathf::round_i32(r * mathf::cos(angle))),
            i64::from(mathf::round_i32(r * mathf::sin(angle))),
        )
    };
    let at = |v: i64| i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX });
    let (near_x, far_x) = (left + radius, right - radius);
    let (near_y, far_y) = (top + radius, bottom - radius);
    for index in (0..=quarter).rev() {
        let (dx, dy) = offset(index);
        out.push((at(far_x + dx), at(near_y - dy)));
    }
    for index in 0..=quarter {
        let (dx, dy) = offset(index);
        out.push((at(far_x + dx), at(far_y + dy)));
    }
    for index in (0..=quarter).rev() {
        let (dx, dy) = offset(index);
        out.push((at(near_x - dx), at(far_y + dy)));
    }
    for index in 0..=quarter {
        let (dx, dy) = offset(index);
        out.push((at(near_x - dx), at(near_y - dy)));
    }
    Ok(())
}

/// Vertices a closed curve of greatest radius `radius` is traced with, each
/// chord within [`TOLERANCE`] of the curve: a multiple of four, so a curve
/// symmetric about both axes is traced so.
fn vertices_for(radius: f64) -> usize {
    if radius <= TOLERANCE {
        return FEWEST_VERTICES;
    }
    let step = 2.0 * mathf::acos(1.0 - TOLERANCE / radius);
    let count = mathf::clamp(
        mathf::ceil(core::f64::consts::TAU / step),
        real_count(FEWEST_VERTICES),
        real_count(MOST_VERTICES),
    );
    usize::try_from(mathf::round_i32(count))
        .unwrap_or(FEWEST_VERTICES)
        .next_multiple_of(4)
}

/// Where a curve traced with `count` vertices is placed, as a factor of its
/// radius and a pad in 256ths of a pixel. An area is traced on the curve, so
/// a shape never leaves its own extent; a centre test a hair past the polygon
/// circumscribing it, so every pixel centre on or inside the curve is
/// strictly inside, on either side alike.
fn placement(count: usize, coverage: Coverage) -> (f64, f64) {
    match coverage {
        Coverage::Area => (1.0, 0.0),
        Coverage::Centre => (
            1.0 / mathf::cos(core::f64::consts::PI / real_count(count)),
            1.0,
        ),
    }
}

/// A position in 256ths of a pixel as `f64`, held to what the converter
/// can place.
fn real(value: i64) -> f64 {
    f64::from(i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX }))
}

/// A vertex count as `f64`.
fn real_count(count: usize) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

/// The stadium a disc of `radius` sweeps from `a` to `b`: a half circle about
/// each end, facing away from the other, joined by the two straight sides.
fn trace_capsule(
    a: Point,
    b: Point,
    radius: i64,
    coverage: Coverage,
    out: &mut Vec<(i32, i32)>,
) -> Result<(), OutOfMemory> {
    if radius <= 0 {
        return Ok(());
    }
    let (ax, ay, bx, by) = (real(a.x), real(a.y), real(b.x), real(b.y));
    let count = vertices_for(real(radius));
    let (factor, pad) = placement(count, coverage);
    let r = real(radius) * factor + pad;
    let half = count / 2;
    room(out, 2 * (half + 1))?;
    let facing = if a == b {
        0.0
    } else {
        mathf::atan2(by - ay, bx - ax)
    };
    let normal = facing + core::f64::consts::FRAC_PI_2;
    let step = core::f64::consts::PI / real_count(half);
    for (centre, from) in [
        ((bx, by), normal),
        ((ax, ay), normal - core::f64::consts::PI),
    ] {
        for index in 0..=half {
            let angle = from - step * real_count(index);
            out.push(vertex(
                centre.0 + r * mathf::cos(angle),
                centre.1 + r * mathf::sin(angle),
            ));
        }
    }
    Ok(())
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

    /// Trace the ellipse from one quadrant mirrored into the others, so it
    /// is exactly symmetric about both its axes however the offsets round.
    fn trace(&self, coverage: Coverage, out: &mut Vec<(i32, i32)>) -> Result<(), OutOfMemory> {
        let count = vertices_for(real(self.rx.max(self.ry)));
        let (factor, pad) = placement(count, coverage);
        let (rx, ry) = (real(self.rx) * factor + pad, real(self.ry) * factor + pad);
        let quarter = count / 4;
        room(out, count)?;
        let step = core::f64::consts::FRAC_PI_2 / real_count(quarter);
        let offset = |index: usize| {
            let angle = step * real_count(index);
            (
                i64::from(mathf::round_i32(rx * mathf::cos(angle))),
                i64::from(mathf::round_i32(ry * mathf::sin(angle))),
            )
        };
        let corner = |dx: i64, dy: i64| {
            let at = |v: i64| i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX });
            (at(self.cx + dx), at(self.cy + dy))
        };
        for index in 0..quarter {
            let (dx, dy) = offset(index);
            out.push(corner(dx, dy));
        }
        for index in 0..quarter {
            let (dx, dy) = offset(quarter - index);
            out.push(corner(-dx, dy));
        }
        for index in 0..quarter {
            let (dx, dy) = offset(index);
            out.push(corner(-dx, -dy));
        }
        for index in 0..quarter {
            let (dx, dy) = offset(quarter - index);
            out.push(corner(dx, -dy));
        }
        Ok(())
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
