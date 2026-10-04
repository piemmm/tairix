//! A selection: how much of each pixel is chosen, from wholly to not at all.
//!
//! A rectangle is held as its bounds alone, so choosing the whole of a large
//! picture costs nothing; any other selection holds one alpha a pixel over
//! the pixels it may cover, allocated fallibly and shared, so a copy of a
//! selection — a floating selection's, a worker's — copies nothing. An ellipse,
//! a lasso or a polygon is traced through `lib/raster`'s one scan converter
//! exactly as a brush is, a magic wand's region is the flood fill's own, and
//! feathering is `lib/raster`'s soften: nothing here rasterises or blurs on
//! its own.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_raster::{div255, soften_coverage, CoverageRows, SOFTEN_PASSES};
use tairix_util::fallible;

use crate::canvas::{Canvas, OutOfMemory};
use crate::fill::{self, Region};
use crate::shape::{polygon_rows, Bounds, Point, Shape, ShapeScratch};

/// The least a pixel is chosen and still counts as inside the selection: its
/// outline, a press that lifts it.
const HALF: u8 = 128;

/// How a selection made now meets the one already held.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Combine {
    /// It takes the held one's place.
    #[default]
    Replace,
    /// It is added to it.
    Add,
    /// It is taken from it.
    Subtract,
    /// Only what both choose stays.
    Intersect,
}

impl Combine {
    /// Every way, in the order the choice lists them.
    pub const ALL: [Self; 4] = [Self::Replace, Self::Add, Self::Subtract, Self::Intersect];

    /// What the choice calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Replace => "New",
            Self::Add => "Add",
            Self::Subtract => "Subtract",
            Self::Intersect => "Intersect",
        }
    }
}

/// What a selection is made from.
#[derive(Debug)]
pub enum Recipe {
    /// A rectangle or an ellipse.
    Shape(Shape),
    /// The closed outline through these points: a lasso's path or a
    /// polygon's corners.
    Outline(Vec<Point>),
    /// The pixels of a picture joined to one through colours like it.
    Wand {
        /// The picture, as it stood.
        canvas: Canvas,
        /// The pixel chosen from.
        at: (u32, u32),
        /// How far a colour may differ and still be chosen.
        tolerance: u8,
    },
}

/// The selection `recipe` makes within `within`, its edges smoothed when
/// `smooth` and softened over `feather` pixels, met with `before` as
/// `combine` says: the work of a worker. `None` where nothing is chosen.
///
/// # Errors
///
/// [`OutOfMemory`] when a mask or an outline cannot be held.
pub fn select(
    recipe: &Recipe,
    before: Option<&Mask>,
    combine: Combine,
    (feather, smooth): (u32, bool),
    within: Bounds,
) -> Result<Option<Mask>, OutOfMemory> {
    let mut scratch = ShapeScratch::default();
    let made = match recipe {
        Recipe::Shape(shape) => Mask::shape(shape, smooth, within, &mut scratch)?,
        Recipe::Outline(points) => Mask::outline(points, smooth, within, &mut scratch)?,
        Recipe::Wand {
            canvas,
            at,
            tolerance,
        } => match fill::region(canvas, at.0, at.1, *tolerance)? {
            Some(region) => Mask::region(&region)?,
            None => None,
        },
    };
    let made = match made {
        Some(mask) if feather > 0 => mask.feathered(feather, within)?,
        made => made,
    };
    match (before, made) {
        (Some(before), Some(made)) => before.combined(&made, combine),
        (Some(before), None) => {
            Ok(matches!(combine, Combine::Add | Combine::Subtract).then(|| before.clone()))
        }
        (None, made) => Ok(made.filter(|_| matches!(combine, Combine::Replace | Combine::Add))),
    }
}

/// The share of each pixel a selection chooses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mask {
    bounds: Bounds,
    /// One alpha a pixel of `bounds`, row by row; `None` where every one of
    /// them is chosen wholly.
    alpha: Option<Arc<Vec<u8>>>,
}

/// Pixels `bounds` holds, as a buffer's length.
fn area(bounds: Bounds) -> Result<usize, OutOfMemory> {
    let width = usize::try_from(bounds.x1 - bounds.x0).map_err(|_| OutOfMemory)?;
    let height = usize::try_from(bounds.y1 - bounds.y0).map_err(|_| OutOfMemory)?;
    width.checked_mul(height).ok_or(OutOfMemory)
}

impl Mask {
    /// Every pixel of `bounds` wholly; `None` for no pixel at all.
    #[must_use]
    pub fn rect(bounds: Bounds) -> Option<Self> {
        (!bounds.is_empty()).then_some(Self {
            bounds,
            alpha: None,
        })
    }

    /// `alpha`, one value a pixel of `bounds`, trimmed to the pixels it
    /// chooses at all; `None` where it chooses none.
    fn of(bounds: Bounds, alpha: Vec<u8>) -> Result<Option<Self>, OutOfMemory> {
        let width = usize::try_from(bounds.x1 - bounds.x0).map_err(|_| OutOfMemory)?;
        if width == 0 {
            return Ok(None);
        }
        let mut tight: Option<Bounds> = None;
        for (y, row) in (bounds.y0..).zip(alpha.chunks_exact(width)) {
            let Some(first) = row.iter().position(|&a| a != 0) else {
                continue;
            };
            let last = row.iter().rposition(|&a| a != 0).unwrap_or(first);
            let at = |column: usize| bounds.x0 + i64::try_from(column).unwrap_or(0);
            let line = Bounds {
                x0: at(first),
                y0: y,
                x1: at(last) + 1,
                y1: y + 1,
            };
            tight = Some(tight.map_or(line, |held| held.union(&line)));
        }
        let Some(tight) = tight else {
            return Ok(None);
        };
        if tight == bounds {
            return Ok(Some(Self {
                bounds,
                alpha: Some(Arc::new(alpha)),
            }));
        }
        let mut trimmed = fallible::filled(area(tight)?, 0u8).ok_or(OutOfMemory)?;
        let inner = usize::try_from(tight.x1 - tight.x0).map_err(|_| OutOfMemory)?;
        let from = usize::try_from(tight.x0 - bounds.x0).map_err(|_| OutOfMemory)?;
        for (y, into) in (tight.y0..).zip(trimmed.chunks_exact_mut(inner)) {
            let row = usize::try_from(y - bounds.y0).map_err(|_| OutOfMemory)? * width;
            into.copy_from_slice(&alpha[row + from..row + from + inner]);
        }
        Ok(Some(Self {
            bounds: tight,
            alpha: Some(Arc::new(trimmed)),
        }))
    }

    /// What `shape` covers within `within`, its edges smoothed when `aa`.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the mask or the outline cannot be held.
    pub fn shape(
        shape: &Shape,
        aa: bool,
        within: Bounds,
        scratch: &mut ShapeScratch,
    ) -> Result<Option<Self>, OutOfMemory> {
        if let Shape::Rect {
            span,
            outline: None,
        } = shape
        {
            return Ok(Self::rect(span.bounds().intersection(&within)));
        }
        let bounds = shape.bounds().intersection(&within);
        let Some(rows) = shape.rows(aa, scratch)? else {
            return Ok(None);
        };
        Self::traced(rows, bounds)
    }

    /// What the closed outline through `points` encloses within `within`:
    /// a lasso's path or a polygon's corners.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the mask or the outline cannot be held.
    pub fn outline(
        points: &[Point],
        aa: bool,
        within: Bounds,
        scratch: &mut ShapeScratch,
    ) -> Result<Option<Self>, OutOfMemory> {
        let Some((rows, bounds)) = polygon_rows(points, aa, scratch)? else {
            return Ok(None);
        };
        Self::traced(rows, bounds.intersection(&within))
    }

    fn traced(mut rows: CoverageRows<'_>, bounds: Bounds) -> Result<Option<Self>, OutOfMemory> {
        if bounds.is_empty() {
            return Ok(None);
        }
        let mut alpha = fallible::filled(area(bounds)?, 0u8).ok_or(OutOfMemory)?;
        let width = usize::try_from(bounds.x1 - bounds.x0).map_err(|_| OutOfMemory)?;
        let x0 = u32::try_from(bounds.x0).map_err(|_| OutOfMemory)?;
        for (y, row) in (bounds.y0..).zip(alpha.chunks_exact_mut(width)) {
            let y = u32::try_from(y).map_err(|_| OutOfMemory)?;
            rows.row(y, x0, row);
        }
        Self::of(bounds, alpha)
    }

    /// The pixels a magic wand's flood reached.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the mask cannot be held.
    pub fn region(region: &Region) -> Result<Option<Self>, OutOfMemory> {
        let bounds = region.bounds();
        if bounds.is_empty() {
            return Ok(None);
        }
        let mut alpha = fallible::filled(area(bounds)?, 0u8).ok_or(OutOfMemory)?;
        let width = usize::try_from(bounds.x1 - bounds.x0).map_err(|_| OutOfMemory)?;
        for (y, row) in (bounds.y0..).zip(alpha.chunks_exact_mut(width)) {
            for (x, chosen) in (bounds.x0..).zip(row.iter_mut()) {
                if let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) {
                    if region.contains(x, y) {
                        *chosen = u8::MAX;
                    }
                }
            }
        }
        Self::of(bounds, alpha)
    }

    /// The pixels it may choose any of.
    #[must_use]
    pub const fn bounds(&self) -> Bounds {
        self.bounds
    }

    /// The selection held to `within`: a rectangle cut to it, any other
    /// kept as it is while it reaches it at all; `None` where it does not.
    #[must_use]
    pub fn within(self, within: Bounds) -> Option<Self> {
        let on = self.bounds.intersection(&within);
        if on.is_empty() {
            None
        } else if self.is_rect() {
            Self::rect(on)
        } else {
            Some(self)
        }
    }

    /// The same selection moved `(dx, dy)` pixels.
    #[must_use]
    pub fn shifted(&self, dx: i64, dy: i64) -> Self {
        let b = self.bounds;
        Self {
            bounds: Bounds {
                x0: b.x0 + dx,
                y0: b.y0 + dy,
                x1: b.x1 + dx,
                y1: b.y1 + dy,
            },
            alpha: self.alpha.clone(),
        }
    }

    /// Whether it is a rectangle, every pixel of its bounds wholly chosen.
    #[must_use]
    pub const fn is_rect(&self) -> bool {
        self.alpha.is_none()
    }

    /// Whether pixel `(x, y)` counts as inside it: chosen at least half.
    #[must_use]
    pub fn chooses(&self, x: i64, y: i64) -> bool {
        self.at(x, y) >= HALF
    }

    /// How much of pixel `(x, y)` it chooses.
    #[must_use]
    pub fn at(&self, x: i64, y: i64) -> u8 {
        let b = self.bounds;
        if x < b.x0 || x >= b.x1 || y < b.y0 || y >= b.y1 {
            return 0;
        }
        let Some(alpha) = &self.alpha else {
            return u8::MAX;
        };
        let width = b.x1 - b.x0;
        usize::try_from((y - b.y0) * width + (x - b.x0))
            .ok()
            .and_then(|at| alpha.get(at))
            .copied()
            .unwrap_or(0)
    }

    /// Write how much of row `y` it chooses, from column `x`, into `out`.
    pub fn row(&self, y: i64, x: i64, out: &mut [u8]) {
        out.fill(0);
        let b = self.bounds;
        if y < b.y0 || y >= b.y1 {
            return;
        }
        let end = x.saturating_add(i64::try_from(out.len()).unwrap_or(i64::MAX));
        let (from, to) = (b.x0.max(x), b.x1.min(end));
        if from >= to {
            return;
        }
        let (Ok(start), Ok(len)) = (usize::try_from(from - x), usize::try_from(to - from)) else {
            return;
        };
        let into = &mut out[start..start + len];
        match &self.alpha {
            None => into.fill(u8::MAX),
            Some(alpha) => {
                let width = usize::try_from(b.x1 - b.x0).unwrap_or(0);
                let row = usize::try_from(y - b.y0).unwrap_or(0) * width;
                let column = usize::try_from(from - b.x0).unwrap_or(0);
                if let Some(held) = alpha.get(row + column..row + column + len) {
                    into.copy_from_slice(held);
                }
            }
        }
    }

    /// This selection with `other` made `how`; `None` where nothing is left.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the result cannot be held.
    pub fn combined(&self, other: &Self, how: Combine) -> Result<Option<Self>, OutOfMemory> {
        let bounds = match how {
            Combine::Replace => return Ok(Some(other.clone())),
            Combine::Add => self.bounds.union(&other.bounds),
            Combine::Subtract => self.bounds,
            Combine::Intersect => self.bounds.intersection(&other.bounds),
        };
        if bounds.is_empty() {
            return Ok(None);
        }
        // Two rectangles meet in one.
        if how == Combine::Intersect && self.is_rect() && other.is_rect() {
            return Ok(Self::rect(bounds));
        }
        // A rectangle added to one it holds, or holding it, is the larger.
        if how == Combine::Add {
            for (outer, inner) in [(self, other), (other, self)] {
                if outer.is_rect() && outer.bounds.union(&inner.bounds) == outer.bounds {
                    return Ok(Some(outer.clone()));
                }
            }
        }
        let width = usize::try_from(bounds.x1 - bounds.x0).map_err(|_| OutOfMemory)?;
        let mut alpha = fallible::filled(area(bounds)?, 0u8).ok_or(OutOfMemory)?;
        let mut theirs = fallible::filled(width, 0u8).ok_or(OutOfMemory)?;
        for (y, row) in (bounds.y0..).zip(alpha.chunks_exact_mut(width)) {
            self.row(y, bounds.x0, row);
            other.row(y, bounds.x0, &mut theirs);
            for (mine, &their) in row.iter_mut().zip(theirs.iter()) {
                *mine = match how {
                    Combine::Add => (*mine).max(their),
                    Combine::Subtract => scale(*mine, u8::MAX - their),
                    Combine::Intersect => (*mine).min(their),
                    Combine::Replace => their,
                };
            }
        }
        Self::of(bounds, alpha)
    }

    /// This selection with its edge softened over `radius` pixels, held to
    /// `within`.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the softened mask cannot be held.
    pub fn feathered(&self, radius: u32, within: Bounds) -> Result<Option<Self>, OutOfMemory> {
        if radius == 0 {
            return Ok(Some(self.clone()));
        }
        let reach = i64::from(radius);
        let grown = Bounds {
            x0: self.bounds.x0 - reach,
            y0: self.bounds.y0 - reach,
            x1: self.bounds.x1 + reach,
            y1: self.bounds.y1 + reach,
        };
        let bounds = grown.intersection(&within);
        if bounds.is_empty() {
            return Ok(None);
        }
        let width = usize::try_from(bounds.x1 - bounds.x0).map_err(|_| OutOfMemory)?;
        let height = usize::try_from(bounds.y1 - bounds.y0).map_err(|_| OutOfMemory)?;
        let mut alpha = fallible::filled(area(bounds)?, 0u8).ok_or(OutOfMemory)?;
        for (y, row) in (bounds.y0..).zip(alpha.chunks_exact_mut(width)) {
            self.row(y, bounds.x0, row);
        }
        let mut aux = fallible::filled(alpha.len(), 0u8).ok_or(OutOfMemory)?;
        // Soften's passes together reach their radius times their count.
        let pass = usize::try_from(radius.div_ceil(SOFTEN_PASSES))
            .unwrap_or(1)
            .max(1);
        soften_coverage(&mut alpha, width, height, pass, &mut aux);
        Self::of(bounds, alpha)
    }
}

/// `value` scaled by `by`, both out of 255, rounded to the nearest.
#[must_use]
pub fn scale(value: u8, by: u8) -> u8 {
    div255(u32::from(value) * u32::from(by))
}

#[cfg(test)]
#[path = "mask_tests.rs"]
mod tests;
