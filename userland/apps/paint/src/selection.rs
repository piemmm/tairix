//! The selection: pixels lifted out of the picture or pasted in, floating
//! over it until they are put down.
//!
//! A floating selection always holds transparency of its own, whatever the
//! picture can: what it does not cover leaves the picture showing, both while
//! it floats and when it is put down. A lift writes nothing: what floats is
//! the picture as it stood seen through the selection's mask, and where it came
//! from shows what the lift leaves there, as much as the mask chose, until it
//! is put down, so lifting, moving and turning it down cost nothing that
//! grows with the selection. Putting it down is one change, the work of a
//! worker, so one undo takes the whole move back.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_image::Rgba8;
use tairix_util::fallible;

use crate::canvas::{Canvas, CanvasBuilder, CanvasError, Kind, OutOfMemory, Sample, Tile, TILE};
use crate::colour::{Ink, Nearest};
use crate::mask::{scale, Mask};
use crate::quantize::OPAQUE_FROM;
use crate::shape::Bounds;
use crate::stroke::{lay_over, Blend, Change, Coat};

/// The kind a selection floating over a picture of `kind` is held as: the
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

/// `sample` of a floating selection laid over `below` of the picture beneath.
#[must_use]
pub fn over(below: Sample, sample: Sample) -> Sample {
    match sample {
        // An opaque colour over a colour is that colour, exactly as the blend
        // would make it.
        Sample::Rgba([.., u8::MAX]) if matches!(below, Sample::Rgba(_)) => sample,
        Sample::Rgba(colour) => lay_over(
            below,
            Coat {
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
    /// What floats is drawn from: the picture as it stood when the selection was
    /// lifted from it, or the picture pasted.
    source: Canvas,
    /// The part of `source` that floats.
    area: Bounds,
    /// How much of each pixel of `area` floats, in `source`'s own places;
    /// `None` for every one of them wholly.
    chosen: Option<Mask>,
    /// Where its top left sits on the picture.
    at: (i64, i64),
    /// What the lift leaves where `area` was, for a selection lifted.
    lifted: Option<Ink>,
}

impl Floating {
    /// Lift what `chosen` selects of `picture` into a floating selection,
    /// leaving `left` where it was, as much as it chose, once it is put
    /// down. Nothing of the picture is written or copied.
    ///
    /// # Errors
    ///
    /// [`CanvasError`] where the selection covers nothing of the picture or
    /// its tiles cannot be shared.
    pub fn lift(picture: &Canvas, chosen: &Mask, left: Ink) -> Result<Self, CanvasError> {
        let area = chosen
            .bounds()
            .intersection(&Bounds::picture(picture.width(), picture.height()));
        if area.is_empty() {
            return Err(CanvasError::BadSize);
        }
        Ok(Self {
            source: picture.try_clone()?,
            area,
            chosen: (!chosen.is_rect()).then(|| chosen.clone()),
            at: (area.x0, area.y0),
            lifted: Some(left),
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
            chosen: None,
            at,
            lifted: None,
        }
    }

    /// The same floating selection, the pixels it draws from shared.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the tiles cannot be shared.
    pub fn try_clone(&self) -> Result<Self, OutOfMemory> {
        Ok(Self {
            source: self.source.try_clone()?,
            area: self.area,
            chosen: self.chosen.clone(),
            at: self.at,
            lifted: self.lifted,
        })
    }

    /// How much of source pixel `(x, y)` floats.
    fn chosen_at(&self, x: i64, y: i64) -> u8 {
        self.chosen.as_ref().map_or(u8::MAX, |mask| mask.at(x, y))
    }

    /// The selection it floats as, moved to where it now lies.
    #[must_use]
    pub fn selection(&self) -> Option<Mask> {
        let (dx, dy) = (self.at.0 - self.area.x0, self.at.1 - self.area.y0);
        match &self.chosen {
            Some(mask) => Some(mask.shifted(dx, dy)),
            None => Mask::rect(self.bounds()),
        }
    }

    /// Whether picture pixel `(x, y)` counts as inside what floats: it lies
    /// over it, chosen at least half there.
    #[must_use]
    pub fn chooses(&self, x: i64, y: i64) -> bool {
        let bounds = self.bounds();
        if !(bounds.x0..bounds.x1).contains(&x) || !(bounds.y0..bounds.y1).contains(&y) {
            return false;
        }
        let (sx, sy) = (self.area.x0 + x - self.at.0, self.area.y0 + y - self.at.1);
        self.chosen.as_ref().is_none_or(|mask| mask.chooses(sx, sy))
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
    /// put down: `None` for a picture pasted.
    #[must_use]
    pub fn lifted_from(&self) -> Option<Bounds> {
        self.lifted.map(|_| self.area)
    }

    /// What it was lifted as — where it came from, as much as it chose there
    /// — and what the lift leaves: `None` for a picture pasted.
    #[must_use]
    pub fn lifted(&self) -> Option<(Mask, Ink)> {
        let ink = self.lifted?;
        let chosen = match &self.chosen {
            Some(mask) => mask.clone(),
            None => Mask::rect(self.area)?,
        };
        Some((chosen, ink))
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
        let (sx, sy) = (self.area.x0 + x - self.at.0, self.area.y0 + y - self.at.1);
        let (Ok(px), Ok(py)) = (u32::try_from(sx), u32::try_from(sy)) else {
            return None;
        };
        let sample = self.source.sample(px, py)?;
        Some(through(sample, self.chosen_at(sx, sy)))
    }

    /// What picture pixel `(x, y)`, `below` on a picture `masked` or not,
    /// shows with the selection floating: what the lift leaves where it came
    /// from, and what floats over that.
    #[must_use]
    pub fn shows(&self, x: i64, y: i64, below: Sample, masked: bool) -> Sample {
        let area = self.area;
        let below = match self.lifted {
            Some(ink) if (area.x0..area.x1).contains(&x) && (area.y0..area.y1).contains(&y) => {
                lay_over(
                    below,
                    Coat {
                        ink,
                        blend: Blend::Over,
                    },
                    self.chosen_at(x, y),
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
        match &self.chosen {
            Some(mask) => cut_out(&self.source, mask),
            None => cut_out(
                &self.source,
                &Mask::rect(self.area).ok_or(CanvasError::BadSize)?,
            ),
        }
    }

    /// Put it down on `picture`, leaving what the lift leaves where it came
    /// from: the worker's half of putting a selection down, answering each tile
    /// written as it now stands.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when a tile cannot be copied for writing.
    pub fn put_down(&self, picture: &mut Canvas) -> Result<Vec<(usize, Arc<Tile>)>, OutOfMemory> {
        let mut change = Change::new();
        if let Some((chosen, ink)) = self.lifted() {
            clear_area(picture, &chosen, ink, &mut change)?;
        }
        let mut above = [Sample::Rgba([0; 4]); TILE as usize];
        let mut alpha = [u8::MAX; TILE as usize];
        change.repaint(picture, self.bounds(), |x, y, run| {
            // The area painted lies inside what floats, so every run is in it.
            let (sx, sy) = (self.area.x0 + x - self.at.0, self.area.y0 + y - self.at.1);
            let (Ok(px), Ok(py)) = (u32::try_from(sx), u32::try_from(sy)) else {
                return;
            };
            let above = &mut above[..run.len()];
            let alpha = &mut alpha[..run.len()];
            self.source.row_samples(py, px, above);
            self.chosen_row(sy, sx, alpha);
            for ((below, above), &chosen) in run.iter_mut().zip(above.iter()).zip(alpha.iter()) {
                *below = over(*below, through(*above, chosen));
            }
        })?;
        change.written(picture)
    }

    /// How much of source row `y` from column `x` floats, into `out`.
    fn chosen_row(&self, y: i64, x: i64, out: &mut [u8]) {
        match &self.chosen {
            Some(mask) => mask.row(y, x, out),
            None => out.fill(u8::MAX),
        }
    }

    /// Compose what floats into `row` — picture row `y` from column `first`
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
        if let Some(ink) = self.lifted {
            if let Some((from, columns)) = span(self.area) {
                let coat = Coat {
                    ink,
                    blend: Blend::Over,
                };
                for (x, below) in (from..).zip(&mut row[columns]) {
                    *below = lay_over(*below, coat, self.chosen_at(x, y), masked);
                }
            }
        }
        let Some((from, columns)) = span(self.bounds()) else {
            return;
        };
        let (sx, sy) = (
            self.area.x0 + from - self.at.0,
            self.area.y0 + y - self.at.1,
        );
        let (Ok(px), Ok(py)) = (u32::try_from(sx), u32::try_from(sy)) else {
            return;
        };
        let Some(above) = above.get_mut(..columns.len()) else {
            return;
        };
        self.source.row_samples(py, px, above);
        for (x, (below, above)) in (sx..).zip(row[columns].iter_mut().zip(above.iter())) {
            *below = over(*below, through(*above, self.chosen_at(x, sy)));
        }
    }
}

/// `sample` seen through a selection that chose `chosen` of it: wholly
/// clear where it chose none, so nothing left out is carried beneath.
fn through(sample: Sample, chosen: u8) -> Sample {
    match sample {
        Sample::Rgba([r, g, b, a]) => match scale(a, chosen) {
            0 => Sample::Rgba([0; 4]),
            a => Sample::Rgba([r, g, b, a]),
        },
        Sample::Index(index, alpha) => match scale(alpha, chosen) {
            0 => Sample::Index(0, 0),
            alpha => Sample::Index(index, alpha),
        },
    }
}

/// Clear what `chosen` selects of `picture` to `ink`, as an eraser would:
/// the worker's half of deleting a selection, answering each tile written
/// as it now stands.
///
/// # Errors
///
/// [`OutOfMemory`] when a tile cannot be copied for writing.
pub fn cleared(
    picture: &mut Canvas,
    chosen: &Mask,
    ink: Ink,
) -> Result<Vec<(usize, Arc<Tile>)>, OutOfMemory> {
    let mut change = Change::new();
    clear_area(picture, chosen, ink, &mut change)?;
    change.written(picture)
}

/// Lay `ink` over what `chosen` selects, as much as it chose of each pixel —
/// a soft edge keeps what the selection left of it — the tiles written going
/// on `change`.
fn clear_area(
    picture: &mut Canvas,
    chosen: &Mask,
    ink: Ink,
    change: &mut Change,
) -> Result<(), OutOfMemory> {
    let coat = Coat {
        ink,
        blend: Blend::Over,
    };
    let masked = picture.kind().masked();
    let mut alpha = [0u8; TILE as usize];
    change.repaint(picture, chosen.bounds(), |x, y, run| {
        let alpha = &mut alpha[..run.len()];
        chosen.row(y, x, alpha);
        for (below, &cover) in run.iter_mut().zip(alpha.iter()) {
            if cover > 0 {
                *below = lay_over(*below, coat, cover, masked);
            }
        }
    })
}

/// What a picture of `kind` shows where nothing is: clear.
fn clear_of(kind: &Kind) -> Sample {
    match kind {
        Kind::Indexed { .. } => Sample::Index(0, 0),
        Kind::Rgba => Sample::Rgba([0; 4]),
    }
}

/// `pasted`, a picture decoded from the clipboard, as a floating selection over
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

/// What `chosen` selects of `picture`, alone, as the clipboard carries it:
/// as much of each pixel as it chose, with transparency of its own where the
/// selection is soft.
///
/// # Errors
///
/// [`CanvasError`] where it cannot be held.
pub fn cut_out(picture: &Canvas, chosen: &Mask) -> Result<Canvas, CanvasError> {
    let area = chosen
        .bounds()
        .intersection(&Bounds::picture(picture.width(), picture.height()));
    let (Ok(x0), Ok(y0), Ok(width), Ok(height)) = (
        u32::try_from(area.x0),
        u32::try_from(area.y0),
        u32::try_from(area.x1 - area.x0),
        u32::try_from(area.y1 - area.y0),
    ) else {
        return Err(CanvasError::BadSize);
    };
    // A soft selection takes part of a pixel, which only pixels with
    // transparency of their own can hold.
    let kind = if chosen.is_rect() {
        picture.kind().clone()
    } else {
        floating_kind(picture.kind())
    };
    let fill = clear_of(&kind);
    let fill = match (fill, kind.masked()) {
        (Sample::Index(index, _), false) => Sample::Index(index, u8::MAX),
        (other, _) => other,
    };
    let mut built = CanvasBuilder::new(width, height, kind, fill)?;
    let mut row = fallible::filled(width as usize, fill).ok_or(CanvasError::OutOfMemory)?;
    let mut alpha = fallible::filled(width as usize, 0u8).ok_or(CanvasError::OutOfMemory)?;
    for y in 0..height {
        picture.row_samples(y0 + y, x0, &mut row);
        if !chosen.is_rect() {
            chosen.row(i64::from(y0 + y), i64::from(x0), &mut alpha);
            for (sample, &cover) in row.iter_mut().zip(alpha.iter()) {
                *sample = through(*sample, cover);
            }
        }
        built.set_row(y, &row);
    }
    Ok(built.finish())
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
