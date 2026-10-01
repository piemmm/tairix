//! The selection: pixels lifted out of the picture or pasted in, floating
//! over it until they are put down.
//!
//! A floating layer always holds transparency of its own, whatever the
//! picture can: what it does not cover leaves the picture showing, both while
//! it floats and when it is put down. A lift writes nothing: the layer is the
//! picture as it stood, and where it came from shows what the lift leaves
//! there until it is put down, so lifting, moving and turning it down cost
//! nothing that grows with the selection. Putting it down is one change, the
//! work of a worker, so one undo takes the whole move back.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_image::Rgba8;
use tairix_util::fallible;

use crate::canvas::{
    read_sample, write_sample, Canvas, CanvasBuilder, CanvasError, Kind, OutOfMemory, Sample, Tile,
    TILE,
};
use crate::colour::{Ink, Nearest};
use crate::quantize::OPAQUE_FROM;
use crate::shape::Bounds;
use crate::stroke::{lay_over, Blend, Change, Layer};

/// The kind a layer floating over a picture of `kind` is held as: the
/// picture's own, with a mask where it has none.
#[must_use]
pub fn floating_kind(kind: &Kind) -> Kind {
    match kind {
        Kind::Indexed { depth, palette, .. } => Kind::Indexed {
            depth: *depth,
            palette: palette.clone(),
            masked: true,
        },
        Kind::Rgba => Kind::Rgba,
    }
}

/// `sample` of a floating layer laid over `below` of the picture beneath.
#[must_use]
pub fn over(below: Sample, sample: Sample) -> Sample {
    match sample {
        // An opaque colour over a colour is that colour, exactly as the blend
        // would make it.
        Sample::Rgba([.., u8::MAX]) if matches!(below, Sample::Rgba(_)) => sample,
        Sample::Rgba(colour) => lay_over(
            below,
            Layer {
                ink: Ink::Colour(colour),
                blend: Blend::Over,
            },
            u8::MAX,
            false,
        ),
        // Over nothing at all a palette pixel is itself, its mask kept soft.
        Sample::Index(..) if matches!(below, Sample::Index(_, 0)) => sample,
        Sample::Index(index, alpha) if alpha >= OPAQUE_FROM => Sample::Index(index, u8::MAX),
        Sample::Index(..) => below,
    }
}

/// Pixels floating over the picture.
#[derive(Debug, Eq, PartialEq)]
pub struct Floating {
    /// What floats is drawn from: the picture as it stood when the layer was
    /// lifted from it, or the picture pasted.
    source: Canvas,
    /// The part of `source` that floats.
    area: Bounds,
    /// Where its top left sits on the picture.
    at: (i64, i64),
    /// Where it was lifted from, and what the lift leaves there.
    lifted: Option<(Bounds, Ink)>,
}

impl Floating {
    /// Lift `area` of `picture` into a floating layer, leaving `left` where it
    /// was once it is put down. Nothing of the picture is written or copied.
    ///
    /// # Errors
    ///
    /// [`CanvasError`] where the area covers nothing or the picture's tiles
    /// cannot be shared.
    pub fn lift(picture: &Canvas, area: Bounds, left: Ink) -> Result<Self, CanvasError> {
        let area = area.intersection(&Bounds::picture(picture.width(), picture.height()));
        if area.is_empty() {
            return Err(CanvasError::BadSize);
        }
        Ok(Self {
            source: picture.try_clone()?,
            area,
            at: (area.x0, area.y0),
            lifted: Some((area, left)),
        })
    }

    /// `canvas`, already of the floating kind for the picture, floating with
    /// its top left at `at`.
    #[must_use]
    pub fn pasted(canvas: Canvas, at: (i64, i64)) -> Self {
        let area = Bounds::picture(canvas.width(), canvas.height());
        Self {
            source: canvas,
            area,
            at,
            lifted: None,
        }
    }

    /// The same layer, the pixels it draws from shared.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the tiles cannot be shared.
    pub fn try_clone(&self) -> Result<Self, OutOfMemory> {
        Ok(Self {
            source: self.source.try_clone()?,
            area: self.area,
            at: self.at,
            lifted: self.lifted,
        })
    }

    /// The picture pixels it lies over.
    #[must_use]
    pub fn bounds(&self) -> Bounds {
        Bounds {
            x0: self.at.0,
            y0: self.at.1,
            x1: self.at.0 + (self.area.x1 - self.area.x0),
            y1: self.at.1 + (self.area.y1 - self.area.y0),
        }
    }

    /// Where it was lifted from, which shows what the lift leaves until it is
    /// put down: `None` for a layer pasted.
    #[must_use]
    pub fn lifted_from(&self) -> Option<Bounds> {
        self.lifted.map(|(area, _)| area)
    }

    /// Where it was lifted from and what the lift leaves there.
    #[must_use]
    pub const fn lifted(&self) -> Option<(Bounds, Ink)> {
        self.lifted
    }

    /// Move it by `(dx, dy)` picture pixels.
    pub fn shift(&mut self, dx: i64, dy: i64) {
        self.at = (self.at.0 + dx, self.at.1 + dy);
    }

    /// Its pixel over picture pixel `(x, y)`, if it covers it.
    #[must_use]
    pub fn sample_at(&self, x: i64, y: i64) -> Option<Sample> {
        let bounds = self.bounds();
        if !(bounds.x0..bounds.x1).contains(&x) || !(bounds.y0..bounds.y1).contains(&y) {
            return None;
        }
        let (Ok(x), Ok(y)) = (
            u32::try_from(self.area.x0 + x - self.at.0),
            u32::try_from(self.area.y0 + y - self.at.1),
        ) else {
            return None;
        };
        self.source.sample(x, y)
    }

    /// What picture pixel `(x, y)`, `below` on a picture `masked` or not,
    /// shows with the layer floating: what the lift leaves where it came
    /// from, and the layer over that.
    #[must_use]
    pub fn shows(&self, x: i64, y: i64, below: Sample, masked: bool) -> Sample {
        let below = match self.lifted {
            Some((area, ink))
                if (area.x0..area.x1).contains(&x) && (area.y0..area.y1).contains(&y) =>
            {
                lay_over(
                    below,
                    Layer {
                        ink,
                        blend: Blend::Replace,
                    },
                    u8::MAX,
                    masked,
                )
            }
            _ => below,
        };
        self.sample_at(x, y)
            .map_or(below, |above| over(below, above))
    }

    /// Its pixels alone, as the clipboard carries them.
    ///
    /// # Errors
    ///
    /// [`CanvasError`] where they cannot be held.
    pub fn pixels(&self) -> Result<Canvas, CanvasError> {
        cut_out(&self.source, self.area)
    }

    /// Put it down on `picture`, leaving what the lift leaves where it came
    /// from: the worker's half of putting a layer down, answering each tile
    /// written as it now stands.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when a tile cannot be copied for writing.
    pub fn put_down(&self, picture: &mut Canvas) -> Result<Vec<(usize, Arc<Tile>)>, OutOfMemory> {
        let mut change = Change::new();
        if let Some((area, ink)) = self.lifted {
            clear_area(picture, area, ink, &mut change)?;
        }
        let mut above = [Sample::Rgba([0; 4]); TILE as usize];
        paint_area(picture, self.bounds(), &mut change, |x, y, run| {
            // The area painted lies inside the layer, so every run is in it.
            let (Ok(x), Ok(y)) = (
                u32::try_from(self.area.x0 + x - self.at.0),
                u32::try_from(self.area.y0 + y - self.at.1),
            ) else {
                return;
            };
            let above = &mut above[..run.len()];
            self.source.row_samples(y, x, above);
            for (below, above) in run.iter_mut().zip(above.iter()) {
                *below = over(*below, *above);
            }
        })?;
        written(picture, change)
    }

    /// Compose the layer into `row` — picture row `y` from column `first`
    /// rightwards, on a picture `masked` or not — as [`shows`](Self::shows)
    /// would each pixel, reading what it covers a run at a time into
    /// `above`, which is at least as long as `row`.
    pub fn compose_row(
        &self,
        y: i64,
        first: i64,
        row: &mut [Sample],
        above: &mut [Sample],
        masked: bool,
    ) {
        let end = first.saturating_add(i64::try_from(row.len()).unwrap_or(i64::MAX));
        let span = |bounds: Bounds| {
            let covered = (bounds.y0..bounds.y1).contains(&y);
            let (from, to) = (bounds.x0.max(first), bounds.x1.min(end));
            (covered && from < to).then(|| {
                let start = usize::try_from(from - first).unwrap_or(0);
                (from, start..start + usize::try_from(to - from).unwrap_or(0))
            })
        };
        if let Some((area, ink)) = self.lifted {
            if let Some((_, columns)) = span(area) {
                let layer = Layer {
                    ink,
                    blend: Blend::Replace,
                };
                for below in &mut row[columns] {
                    *below = lay_over(*below, layer, u8::MAX, masked);
                }
            }
        }
        let Some((from, columns)) = span(self.bounds()) else {
            return;
        };
        let (Ok(x), Ok(y)) = (
            u32::try_from(self.area.x0 + from - self.at.0),
            u32::try_from(self.area.y0 + y - self.at.1),
        ) else {
            return;
        };
        let Some(above) = above.get_mut(..columns.len()) else {
            return;
        };
        self.source.row_samples(y, x, above);
        for (below, above) in row[columns].iter_mut().zip(above.iter()) {
            *below = over(*below, *above);
        }
    }
}

/// Clear `area` of `picture` to `ink`, as an eraser would: the worker's half
/// of deleting a selection, answering each tile written as it now stands.
///
/// # Errors
///
/// [`OutOfMemory`] when a tile cannot be copied for writing.
pub fn cleared(
    picture: &mut Canvas,
    area: Bounds,
    ink: Ink,
) -> Result<Vec<(usize, Arc<Tile>)>, OutOfMemory> {
    let mut change = Change::new();
    clear_area(picture, area, ink, &mut change)?;
    written(picture, change)
}

/// Lay `ink` over every pixel of `area`, the tiles written going on `change`.
fn clear_area(
    picture: &mut Canvas,
    area: Bounds,
    ink: Ink,
    change: &mut Change,
) -> Result<(), OutOfMemory> {
    let layer = Layer {
        ink,
        blend: Blend::Replace,
    };
    let masked = picture.kind().masked();
    paint_area(picture, area, change, |_, _, run| {
        for below in run {
            *below = lay_over(*below, layer, u8::MAX, masked);
        }
    })
}

/// Every tile `change` wrote on `picture`, as it now stands.
fn written(picture: &Canvas, change: Change) -> Result<Vec<(usize, Arc<Tile>)>, OutOfMemory> {
    let before = change.finish();
    fallible::collected(
        before.len(),
        before
            .into_iter()
            .map(|(index, _)| (index, Arc::clone(picture.tile(index)))),
    )
    .ok_or(OutOfMemory)
}

/// What a picture of `kind` shows where nothing is: clear.
fn clear_of(kind: &Kind) -> Sample {
    match kind {
        Kind::Indexed { .. } => Sample::Index(0, 0),
        Kind::Rgba => Sample::Rgba([0; 4]),
    }
}

/// Repaint `area` of `picture` through `paint`, handed each run of a tile's
/// row — the picture pixels from `(x, y)` rightwards — to rewrite in place,
/// the tiles written going on `change`.
fn paint_area(
    picture: &mut Canvas,
    area: Bounds,
    change: &mut Change,
    mut paint: impl FnMut(i64, i64, &mut [Sample]),
) -> Result<(), OutOfMemory> {
    let area = area.intersection(&Bounds::picture(picture.width(), picture.height()));
    if area.is_empty() {
        return Ok(());
    }
    let planes = picture.kind().planes();
    let tile = i64::from(TILE);
    for ty in area.y0.div_euclid(tile)..=(area.y1 - 1).div_euclid(tile) {
        for tx in area.x0.div_euclid(tile)..=(area.x1 - 1).div_euclid(tile) {
            let (Ok(px), Ok(py)) = (u32::try_from(tx * tile), u32::try_from(ty * tile)) else {
                continue;
            };
            let index = picture.tile_index(px, py);
            let rect = picture.tile_rect(index);
            let span = rect.bounds().intersection(&area);
            let written = change.touch(picture, index)?;
            let (samples, mask) = written.planes_mut();
            let mut run = [Sample::Rgba([0; 4]); TILE as usize];
            let run = &mut run[..usize::try_from(span.x1 - span.x0).unwrap_or(0)];
            for y in span.y0..span.y1 {
                let row = usize::try_from(y - i64::from(rect.y)).unwrap_or(0) * rect.width as usize;
                let first = row + usize::try_from(span.x0 - i64::from(rect.x)).unwrap_or(0);
                for (at, slot) in (first..).zip(run.iter_mut()) {
                    *slot = read_sample(samples, mask, planes, at);
                }
                paint(span.x0, y, run);
                for (at, sample) in (first..).zip(run.iter()) {
                    write_sample(samples, mask, at, *sample);
                }
            }
        }
    }
    change.mark(area);
    Ok(())
}

/// `pasted`, a picture decoded from the clipboard, as a floating layer over
/// a picture of `kind`: its colours the nearest the picture holds.
///
/// # Errors
///
/// [`CanvasError`] where it cannot be held.
pub fn adapt_pasted(pasted: &Canvas, kind: &Kind) -> Result<Canvas, CanvasError> {
    let target = floating_kind(kind);
    let (width, height) = (pasted.width(), pasted.height());
    let mut built = CanvasBuilder::new(width, height, target.clone(), clear_of(&target))?;
    let mut colours: Vec<Rgba8> =
        fallible::filled(width as usize, [0u8; 4]).ok_or(CanvasError::OutOfMemory)?;
    let mut nearest = match target.palette() {
        Some(palette) => Some(Nearest::new(palette).ok_or(CanvasError::OutOfMemory)?),
        None => None,
    };
    let mut row =
        fallible::filled(width as usize, Sample::Rgba([0; 4])).ok_or(CanvasError::OutOfMemory)?;
    for y in 0..height {
        pasted.row_colours(y, 0, &mut colours);
        for (sample, colour) in row.iter_mut().zip(&colours) {
            *sample = match nearest.as_mut() {
                None => Sample::Rgba(*colour),
                Some(_) if colour[3] < OPAQUE_FROM => Sample::Index(0, 0),
                Some(search) => Sample::Index(
                    search.find([colour[0], colour[1], colour[2], u8::MAX]),
                    u8::MAX,
                ),
            };
        }
        built.set_row(y, &row);
    }
    Ok(built.finish())
}

/// The part `area` of `picture`, alone, as the clipboard carries it.
///
/// # Errors
///
/// [`CanvasError`] where it cannot be held.
pub fn cut_out(picture: &Canvas, area: Bounds) -> Result<Canvas, CanvasError> {
    let area = area.intersection(&Bounds::picture(picture.width(), picture.height()));
    let (Ok(x0), Ok(y0), Ok(width), Ok(height)) = (
        u32::try_from(area.x0),
        u32::try_from(area.y0),
        u32::try_from(area.x1 - area.x0),
        u32::try_from(area.y1 - area.y0),
    ) else {
        return Err(CanvasError::BadSize);
    };
    let fill = clear_of(picture.kind());
    let fill = match (fill, picture.kind().masked()) {
        (Sample::Index(index, _), false) => Sample::Index(index, u8::MAX),
        (other, _) => other,
    };
    let mut built = CanvasBuilder::new(width, height, picture.kind().clone(), fill)?;
    let mut row = fallible::filled(width as usize, fill).ok_or(CanvasError::OutOfMemory)?;
    for y in 0..height {
        picture.row_samples(y0 + y, x0, &mut row);
        built.set_row(y, &row);
    }
    Ok(built.finish())
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
