//! Glyphs as a paint draws them: each character's outline carried through
//! its font's size and matrix and the paint's to the pixels of the font's
//! resolution, measured from its origin pixel, then scan converted by
//! TAIRiX's scan converter into sixteen levels of coverage, or into whole
//! pixels where the colours give no anti-aliasing, or handed to Draw as the
//! path a glyph too big to anti-alias is filled by.
//!
//! An outlines file's stroke paths are drawn a pixel wide over its fill.

use alloc::vec::Vec;

use tairix_bezier::{flatten::flatten_cubic, CubicBez, PathEl, Point as Vector};
use tairix_raster::scan::{Coverage, FillRule, ScanScratch, SCAN_UNIT};
use tairix_riscos_draw::pixels::{Element, Point};
use tairix_riscos_vdu::render::WHOLE;

use crate::fontdata::Outline;
use crate::outlines;
use crate::transform::Transform;
use crate::units;

/// A curve's furthest from its chords, in pixels.
const FLATNESS: f64 = 1.0 / 16.0;
/// A stroke's width, in pixels.
const STROKE: f64 = 1.0;
/// Sub-units a pixel the scan converter works in.
const UNITS: f64 = SCAN_UNIT as f64;
/// What a wholly covered pixel's coverage is.
const COVERED: u32 = 255;

/// How a glyph is drawn from its font's units: the font's size in
/// sixteenths of a point, its matrix and the paint's, the dots an inch of
/// the pixels it is drawn in, across and up, and the fraction of a pixel it
/// is moved by past its origin.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Placing {
    pub(crate) points: (i32, i32),
    pub(crate) transform: Transform,
    pub(crate) resolution: (i32, i32),
    pub(crate) shift: (f64, f64),
}

/// A point of a traced glyph: in its pixels from its origin pixel's bottom
/// left corner, up.
type Pixel = (f64, f64);

/// A step of a traced outline.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Piece {
    Move(Pixel),
    Line(Pixel),
    Curve(Pixel, Pixel, Pixel),
}

/// A glyph's outline traced into its pixels: what it fills, each subpath
/// closed, and what it strokes.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Traced {
    fill: Vec<Piece>,
    strokes: Vec<Piece>,
}

/// The pixels a traced glyph reaches, from its origin pixel: left and
/// bottom inclusive, right and top exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Reach {
    pub(crate) left: i32,
    pub(crate) bottom: i32,
    pub(crate) right: i32,
    pub(crate) top: i32,
}

impl Reach {
    /// Its pixels.
    pub(crate) fn area(&self) -> u64 {
        let width = u64::try_from(i64::from(self.right) - i64::from(self.left)).unwrap_or(0);
        let height = u64::try_from(i64::from(self.top) - i64::from(self.bottom)).unwrap_or(0);
        width.saturating_mul(height)
    }

    /// Its part within `other`; none where they do not meet.
    pub(crate) fn within(&self, other: &Self) -> Option<Self> {
        let cut = Self {
            left: self.left.max(other.left),
            bottom: self.bottom.max(other.bottom),
            right: self.right.min(other.right),
            top: self.top.min(other.top),
        };
        (cut.left < cut.right && cut.bottom < cut.top).then_some(cut)
    }
}

/// A glyph's levels over a rectangle of its pixels, row after row from the
/// top: its top left pixel from the glyph's origin pixel, up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Bitmap {
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) width: usize,
    pub(crate) levels: Vec<u8>,
}

/// The memory a glyph's levels need was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Refused;

impl Bitmap {
    /// Roughly what it holds in memory.
    pub(crate) fn bytes(&self) -> usize {
        core::mem::size_of::<Self>() + self.levels.capacity()
    }

    /// Its pixels with its blank rows and columns at each edge cut away;
    /// none where every pixel is blank.
    pub(crate) fn trimmed(&self) -> Result<Option<Self>, Refused> {
        let width = self.width;
        let row = |at: usize| &self.levels[at * width..(at + 1) * width];
        let rows = self.levels.len() / width.max(1);
        let inked = |at: &usize| row(*at).iter().any(|&level| level != 0);
        let (Some(first_row), Some(last_row)) =
            ((0..rows).find(inked), (0..rows).rev().find(inked))
        else {
            return Ok(None);
        };
        let inked_column = |column: &usize| (first_row..=last_row).any(|at| row(at)[*column] != 0);
        let (Some(first_column), Some(last_column)) = (
            (0..width).find(inked_column),
            (0..width).rev().find(inked_column),
        ) else {
            return Ok(None);
        };
        let kept = last_column - first_column + 1;
        let mut levels = Vec::new();
        levels
            .try_reserve_exact(kept * (last_row - first_row + 1))
            .map_err(|_| Refused)?;
        for at in first_row..=last_row {
            levels.extend_from_slice(&row(at)[first_column..=last_column]);
        }
        let offset = |v: usize| i32::try_from(v).unwrap_or(i32::MAX);
        Ok(Some(Self {
            left: self.left.saturating_add(offset(first_column)),
            top: self.top.saturating_sub(offset(first_row)),
            width: kept,
            levels,
        }))
    }
}

/// A guest font's pixels are the guest's data: zeroed when let go.
impl Drop for Bitmap {
    fn drop(&mut self) {
        self.levels.fill(0);
    }
}

/// The scan converter's working buffers, kept from glyph to glyph.
#[derive(Debug, Default)]
pub(crate) struct Scratch {
    scan: ScanScratch,
    contours: Vec<Vec<(i32, i32)>>,
    row: Vec<u8>,
}

impl Placing {
    /// Where the point `millems` thousandths of an em across and up is
    /// drawn.
    fn place(&self, (x, y): (f64, f64)) -> Pixel {
        let millipoints = (
            x * f64::from(self.points.0) / f64::from(units::SIXTEENTHS),
            y * f64::from(self.points.1) / f64::from(units::SIXTEENTHS),
        );
        let (tx, ty) = self.transform.exact(millipoints);
        (
            tx * f64::from(self.resolution.0) / f64::from(units::MILLIPOINTS_AN_INCH)
                + self.shift.0,
            ty * f64::from(self.resolution.1) / f64::from(units::MILLIPOINTS_AN_INCH)
                + self.shift.1,
        )
    }
}

/// `outline` traced through `placing`.
pub(crate) fn trace(outline: &Outline<'_>, placing: &Placing) -> Traced {
    match outline {
        Outline::Design { glyph, design_size } => {
            let millems = f64::from(units::MILLEMS) / f64::from(*design_size);
            let place = |p: outlines::Point| {
                placing.place((f64::from(p.x) * millems, f64::from(p.y) * millems))
            };
            let pieces = |elements: &[outlines::Element]| {
                elements
                    .iter()
                    .map(|element| match *element {
                        outlines::Element::Move(p) => Piece::Move(place(p)),
                        outlines::Element::Line(p) => Piece::Line(place(p)),
                        outlines::Element::Curve(a, b, c) => {
                            Piece::Curve(place(a), place(b), place(c))
                        }
                    })
                    .collect()
            };
            Traced {
                fill: pieces(&glyph.fill),
                strokes: pieces(&glyph.strokes),
            }
        }
        Outline::Face(path) => {
            let millems = f64::from(units::MILLEMS);
            let place = |p: Vector| placing.place((p.x * millems, p.y * millems));
            let mut fill = Vec::new();
            for subpath in &path.subpaths {
                fill.push(Piece::Move(place(subpath.start)));
                fill.extend(subpath.elements.iter().map(|element| match *element {
                    PathEl::Line(p) => Piece::Line(place(p)),
                    PathEl::Cubic(a, b, c) => Piece::Curve(place(a), place(b), place(c)),
                }));
            }
            Traced {
                fill,
                strokes: Vec::new(),
            }
        }
    }
}

impl Traced {
    /// The rectangle with corners `low` and `high`.
    pub(crate) fn rectangle(low: Pixel, high: Pixel) -> Self {
        Self {
            fill: alloc::vec![
                Piece::Move(low),
                Piece::Line((high.0, low.1)),
                Piece::Line(high),
                Piece::Line((low.0, high.1)),
            ],
            strokes: Vec::new(),
        }
    }

    /// The pixels it reaches, a stroke's half width beyond its points; none
    /// for one with no points.
    pub(crate) fn reach(&self) -> Option<Reach> {
        let pad = |stroked: bool| if stroked { STROKE / 2.0 } else { 0.0 };
        let mut extent: Option<(f64, f64, f64, f64)> = None;
        for (pieces, stroked) in [(&self.fill, false), (&self.strokes, true)] {
            for (x, y) in pieces.iter().flat_map(points) {
                let p = pad(stroked);
                let (x0, y0, x1, y1) = extent.unwrap_or((x, y, x, y));
                extent = Some((x0.min(x - p), y0.min(y - p), x1.max(x + p), y1.max(y + p)));
            }
        }
        let (x0, y0, x1, y1) = extent?;
        let pixel =
            |v: f64| tairix_util::mathf::floor(v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)));
        let ceiling =
            |v: f64| tairix_util::mathf::ceil(v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)));
        Some(Reach {
            left: saturated(pixel(x0)),
            bottom: saturated(pixel(y0)),
            right: saturated(ceiling(x1)).max(saturated(pixel(x0)).saturating_add(1)),
            top: saturated(ceiling(y1)).max(saturated(pixel(y0)).saturating_add(1)),
        })
    }

    /// Its levels over `reach`, filled by `rule`, by area or, where
    /// `whole`, by whether each pixel's centre is inside; none where it
    /// covers nothing there.
    pub(crate) fn levels(
        &self,
        reach: &Reach,
        rule: FillRule,
        whole: bool,
        scratch: &mut Scratch,
    ) -> Result<Option<Bitmap>, Refused> {
        let span =
            |from: i32, to: i32| usize::try_from(i64::from(to) - i64::from(from)).unwrap_or(0);
        let (width, height) = (span(reach.left, reach.right), span(reach.bottom, reach.top));
        let pixels = width.checked_mul(height).ok_or(Refused)?;
        let mut levels = Vec::new();
        levels.try_reserve_exact(pixels).map_err(|_| Refused)?;
        levels.resize(pixels, 0);
        let coverage = if whole {
            Coverage::Centre
        } else {
            Coverage::Area
        };
        let mut any = cover(
            &self.fill,
            reach,
            rule,
            coverage,
            false,
            &mut levels,
            width,
            scratch,
        );
        if !self.strokes.is_empty() {
            any |= cover(
                &self.strokes,
                reach,
                FillRule::NonZero,
                coverage,
                true,
                &mut levels,
                width,
                scratch,
            );
        }
        Ok(any.then_some(Bitmap {
            left: reach.left,
            top: reach.top - 1,
            width,
            levels,
        }))
    }

    /// Its fill as the path Draw fills, in Draw's user units: each pixel
    /// `pixel` maps a pixel's point to.
    pub(crate) fn draw_path(&self, pixel: &impl Fn(Pixel) -> Point) -> Vec<Element> {
        let mut path = Vec::new();
        if path.try_reserve(self.fill.len()).is_err() {
            return path;
        }
        path.extend(self.fill.iter().map(|piece| match *piece {
            Piece::Move(p) => Element::Move {
                to: pixel(p),
                winds: true,
            },
            Piece::Line(p) => Element::Line { to: pixel(p) },
            Piece::Curve(a, b, c) => Element::Curve {
                c1: pixel(a),
                c2: pixel(b),
                to: pixel(c),
            },
        }));
        path
    }
}

/// `pieces`' coverage over `reach` laid into `levels` where it is the
/// greater, as strokes a pixel wide where `stroked`; whether any pixel
/// was covered.
#[expect(
    clippy::too_many_arguments,
    reason = "one scan's inputs and the buffers it writes"
)]
fn cover(
    pieces: &[Piece],
    reach: &Reach,
    rule: FillRule,
    coverage: Coverage,
    stroked: bool,
    levels: &mut [u8],
    width: usize,
    scratch: &mut Scratch,
) -> bool {
    let Scratch {
        scan,
        contours,
        row,
    } = scratch;
    contours.clear();
    let unit = |(x, y): Pixel| {
        let across = (x - f64::from(reach.left)) * UNITS;
        let down = (f64::from(reach.top) - y) * UNITS;
        (rounded(across), rounded(down))
    };
    if stroked {
        stroke_contours(pieces, &unit, contours);
    } else {
        fill_contours(pieces, &unit, contours);
    }
    let Some(mut rows) = scan.coverage_rows(contours, SCAN_UNIT, rule, coverage) else {
        return false;
    };
    if row.try_reserve(width.saturating_sub(row.len())).is_err() {
        return false;
    }
    row.resize(width, 0);
    let mut any = false;
    for (y, line) in (0u32..).zip(levels.chunks_mut(width)) {
        let covered = rows.row(y, 0, row);
        for (level, &alpha) in line[covered.clone()].iter_mut().zip(&row[covered]) {
            let shade = level_of(alpha);
            any |= shade != 0;
            *level = (*level).max(shade);
        }
    }
    any
}

/// A piece's points.
fn points(piece: &Piece) -> impl Iterator<Item = Pixel> {
    let (held, count) = match *piece {
        Piece::Move(p) | Piece::Line(p) => ([p; 3], 1),
        Piece::Curve(a, b, c) => ([a, b, c], 3),
    };
    held.into_iter().take(count)
}

/// The level of coverage `alpha` of 255 is.
fn level_of(alpha: u8) -> u8 {
    let level = (u32::from(alpha) * u32::from(WHOLE) + COVERED / 2) / COVERED;
    u8::try_from(level).unwrap_or(WHOLE)
}

/// `pieces`' subpaths, curves flattened, as the closed contours a fill
/// scans, each point `unit` placed.
fn fill_contours(
    pieces: &[Piece],
    unit: &impl Fn(Pixel) -> (i32, i32),
    contours: &mut Vec<Vec<(i32, i32)>>,
) {
    let mut at: Pixel = (0.0, 0.0);
    for piece in pieces {
        match *piece {
            Piece::Move(p) => {
                contours.push(alloc::vec![unit(p)]);
                at = p;
            }
            Piece::Line(p) => {
                if let Some(contour) = contours.last_mut() {
                    contour.push(unit(p));
                }
                at = p;
            }
            Piece::Curve(a, b, c) => {
                if let Some(contour) = contours.last_mut() {
                    let curve = CubicBez::new(vector(at), vector(a), vector(b), vector(c));
                    flatten_cubic(curve, FLATNESS, |p| contour.push(unit((p.x, p.y))));
                }
                at = c;
            }
        }
    }
}

/// `pieces`' subpaths, curves flattened, as a pixel-wide band along each
/// segment, its ends square: contours of one winding, so their union is
/// what they cover.
fn stroke_contours(
    pieces: &[Piece],
    unit: &impl Fn(Pixel) -> (i32, i32),
    contours: &mut Vec<Vec<(i32, i32)>>,
) {
    let mut line: Vec<Pixel> = Vec::new();
    let mut lines: Vec<Vec<Pixel>> = Vec::new();
    for piece in pieces {
        match *piece {
            Piece::Move(p) => {
                if line.len() > 1 {
                    lines.push(core::mem::take(&mut line));
                }
                line.clear();
                line.push(p);
            }
            Piece::Line(p) => line.push(p),
            Piece::Curve(a, b, c) => {
                let from = line.last().copied().unwrap_or(a);
                let curve = CubicBez::new(vector(from), vector(a), vector(b), vector(c));
                flatten_cubic(curve, FLATNESS, |p| line.push((p.x, p.y)));
            }
        }
    }
    if line.len() > 1 {
        lines.push(line);
    }
    for points in &lines {
        for pair in points.windows(2) {
            let [(x0, y0), (x1, y1)] = [pair[0], pair[1]];
            let (dx, dy) = (x1 - x0, y1 - y0);
            let length = tairix_util::mathf::sqrt(dx * dx + dy * dy);
            if length <= 0.0 {
                continue;
            }
            let (nx, ny) = (-dy / length * STROKE / 2.0, dx / length * STROKE / 2.0);
            let band = [
                (x0 + nx, y0 + ny),
                (x1 + nx, y1 + ny),
                (x1 - nx, y1 - ny),
                (x0 - nx, y0 - ny),
            ];
            contours.push(band.iter().map(|&p| unit(p)).collect());
        }
    }
}

const fn vector((x, y): Pixel) -> Vector {
    Vector::new(x, y)
}

/// `value` to the nearest whole, held within a word.
pub(crate) fn rounded(value: f64) -> i32 {
    saturated(tairix_util::mathf::round(value))
}

/// A whole `value` held within a word, nought for none.
pub(crate) fn saturated(value: f64) -> i32 {
    if value.is_nan() {
        return 0;
    }
    #[expect(clippy::cast_possible_truncation, reason = "clamped into a word first")]
    let held = value.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    held
}

#[cfg(test)]
mod tests;
