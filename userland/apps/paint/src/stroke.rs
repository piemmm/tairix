//! Changes to a canvas, gathered into one step of history.
//!
//! A change keeps each tile as it stood before its first write, which is the
//! whole of what undoing it needs. A stroke is a change that lays down
//! coverage: each pixel keeps the most any pass has covered it and is
//! recomposed from its first state, so going back over a pixel within one
//! stroke does not darken it twice.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_image::Rgba8;
use tairix_util::fallible;

use crate::canvas::{write_sample, Canvas, OutOfMemory, Planes, Sample, Tile, TILE};
use crate::colour::Ink;
use crate::shape::{Bounds, Shape};

/// What is kept for each tile a change writes, by tile index: in order, as a
/// change writes few of a canvas's tiles and asks after each often.
#[derive(Debug)]
struct Kept<V>(Vec<(usize, V)>);

impl<V> Kept<V> {
    const fn new() -> Self {
        Self(Vec::new())
    }

    fn find(&self, index: usize) -> Result<usize, usize> {
        self.0.binary_search_by_key(&index, |&(held, _)| held)
    }

    fn get(&self, index: usize) -> Option<&V> {
        self.find(index).ok().map(|at| &self.0[at].1)
    }

    /// What is kept for tile `index`, made by `make` when nothing is yet.
    fn get_or_try_insert(
        &mut self,
        index: usize,
        make: impl FnOnce() -> Result<V, OutOfMemory>,
    ) -> Result<&mut V, OutOfMemory> {
        let at = match self.find(index) {
            Ok(at) => at,
            Err(at) => {
                self.0.try_reserve(1).map_err(|_| OutOfMemory)?;
                self.0.insert(at, (index, make()?));
                at
            }
        };
        Ok(&mut self.0[at].1)
    }
}

/// Tiles written, each with the state it had before.
#[derive(Debug)]
pub struct Change {
    before: Kept<Arc<Tile>>,
    damage: Option<Bounds>,
}

impl Default for Change {
    fn default() -> Self {
        Self::new()
    }
}

impl Change {
    /// A change that has written nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            before: Kept::new(),
            damage: None,
        }
    }

    /// Tile `index` of `canvas`, to write, its state before this change kept.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the tile cannot be copied for writing, or its
    /// state kept.
    pub fn touch<'a>(
        &mut self,
        canvas: &'a mut Canvas,
        index: usize,
    ) -> Result<&'a mut Tile, OutOfMemory> {
        self.before
            .get_or_try_insert(index, || Ok(Arc::clone(canvas.tile(index))))?;
        canvas.tile_mut(index)
    }

    /// Note that `bounds` of the picture changed.
    pub fn mark(&mut self, bounds: Bounds) {
        if !bounds.is_empty() {
            self.damage = Some(self.damage.map_or(bounds, |held| held.union(&bounds)));
        }
    }

    /// What changed since this was last asked.
    pub fn take_damage(&mut self) -> Option<Bounds> {
        self.damage.take()
    }

    /// Every tile written, with the state it had before: what undoing this
    /// change puts back.
    #[must_use]
    pub fn finish(self) -> Vec<(usize, Arc<Tile>)> {
        self.before.0
    }

    /// Put every tile back as it was, reporting what that changed.
    pub fn revert(self, canvas: &mut Canvas) -> Option<Bounds> {
        let mut damage: Option<Bounds> = None;
        for (index, tile) in self.before.0 {
            let bounds = canvas.tile_rect(index).bounds();
            damage = Some(damage.map_or(bounds, |held| held.union(&bounds)));
            canvas.replace_tile(index, tile);
        }
        damage
    }
}

/// How an ink meets the pixel beneath it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Blend {
    /// The ink takes the pixel's place where it covers at least half of it.
    Replace,
    /// The ink is laid over the pixel, in proportion to how much of it the
    /// ink covers and how opaque the ink is. A palette picture, whose pixels
    /// cannot be part one entry and part another, replaces instead.
    Over,
}

/// One ink a stroke lays down.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Layer {
    /// What is put down.
    pub ink: Ink,
    /// How it meets what is beneath.
    pub blend: Blend,
}

/// Most inks one stroke lays down: a shape's fill and its outline.
pub const MAX_LAYERS: usize = 2;

/// A change laying down coverage.
#[derive(Debug)]
pub struct Stroke {
    layers: [Option<Layer>; MAX_LAYERS],
    change: Change,
    /// For each tile written, the coverage of each layer it lays down, a
    /// layer's pixels after the one before.
    coverage: Kept<Vec<u8>>,
    scratch: Vec<u8>,
}

impl Stroke {
    /// A stroke laying down `fill` and, over it, `over`.
    #[must_use]
    pub const fn new(fill: Layer, over: Option<Layer>) -> Self {
        Self {
            layers: [Some(fill), over],
            change: Change::new(),
            coverage: Kept::new(),
            scratch: Vec::new(),
        }
    }

    /// What changed since this was last asked.
    pub fn take_damage(&mut self) -> Option<Bounds> {
        self.change.take_damage()
    }

    /// Lay layer `layer` down where `shape` covers, anti-aliased when `aa`.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when a tile could not be copied for writing; what was
    /// already laid down stays.
    pub fn cover(
        &mut self,
        canvas: &mut Canvas,
        layer: usize,
        shape: &Shape,
        aa: bool,
    ) -> Result<(), OutOfMemory> {
        let bounds = shape.bounds();
        self.cover_rows(canvas, layer, bounds, |y, x, out| shape.row(y, x, aa, out))
    }

    /// Lay layer `layer` down wholly over pixel `(x, y)`.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`], as [`cover`](Self::cover).
    pub fn cover_pixel(
        &mut self,
        canvas: &mut Canvas,
        layer: usize,
        x: i64,
        y: i64,
    ) -> Result<(), OutOfMemory> {
        let bounds = Bounds {
            x0: x,
            y0: y,
            x1: x + 1,
            y1: y + 1,
        };
        self.cover_rows(canvas, layer, bounds, |_, _, out| out.fill(255))
    }

    /// Lay layer `layer` down over `bounds` as `row` says: it is handed each
    /// row's index, its first column and a coverage a pixel to fill in.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`], as [`cover`](Self::cover).
    pub fn cover_rows(
        &mut self,
        canvas: &mut Canvas,
        layer: usize,
        bounds: Bounds,
        mut row: impl FnMut(i64, i64, &mut [u8]),
    ) -> Result<(), OutOfMemory> {
        let bounds = bounds.intersection(&Bounds::picture(canvas.width(), canvas.height()));
        if bounds.is_empty() || layer >= MAX_LAYERS || self.layers[layer].is_none() {
            return Ok(());
        }
        let span = usize::try_from(bounds.x1 - bounds.x0).map_err(|_| OutOfMemory)?;
        if !fallible::grow_to(&mut self.scratch, span, 0) {
            return Err(OutOfMemory);
        }
        let planes = canvas.kind().planes();
        let mut scratch = core::mem::take(&mut self.scratch);
        let mut outcome = Ok(());
        'rows: for y in bounds.y0..bounds.y1 {
            let cover = &mut scratch[..span];
            cover.fill(0);
            row(y, bounds.x0, cover);
            let mut x = bounds.x0;
            while x < bounds.x1 {
                let Some(start) = cover[offset(x, bounds.x0)..]
                    .iter()
                    .position(|&c| c != 0)
                    .map(|skip| x + i64::try_from(skip).unwrap_or(0))
                else {
                    break;
                };
                // A run stops at the tile's right edge, so each run writes
                // within one tile.
                let tile_end = (start / i64::from(TILE) + 1) * i64::from(TILE);
                let end = tile_end.min(bounds.x1);
                if let Err(err) = self.lay(
                    canvas,
                    planes,
                    layer,
                    y,
                    start,
                    &cover[offset(start, bounds.x0)..offset(end, bounds.x0)],
                ) {
                    outcome = Err(err);
                    break 'rows;
                }
                x = end;
            }
        }
        self.scratch = scratch;
        self.change.mark(bounds);
        outcome
    }

    /// Lay `covers` down along row `y` from column `x`, all within one tile.
    fn lay(
        &mut self,
        canvas: &mut Canvas,
        planes: Planes,
        layer: usize,
        y: i64,
        x: i64,
        covers: &[u8],
    ) -> Result<(), OutOfMemory> {
        let (Ok(px), Ok(py)) = (u32::try_from(x), u32::try_from(y)) else {
            return Ok(());
        };
        let index = canvas.tile_index(px, py);
        let rect = canvas.tile_rect(index);
        let pixels = (rect.width * rect.height) as usize;
        let layers = self.layers.iter().flatten().count();
        let coverage = self.coverage.get_or_try_insert(index, || {
            fallible::filled(pixels * layers, 0u8).ok_or(OutOfMemory)
        })?;
        let tile = self.change.touch(canvas, index)?;
        let Some(before) = self.change.before.get(index) else {
            return Ok(());
        };
        let masked = planes == Planes::Indexed { masked: true };
        let row_start = ((py - rect.y) * rect.width + (px - rect.x)) as usize;
        let (samples, mask) = tile.planes_mut();
        for (step, &cover) in covers.iter().enumerate() {
            let at = row_start + step;
            let held = &mut coverage[layer * pixels + at];
            if cover <= *held {
                continue;
            }
            *held = cover;
            let first = before.sample(planes, at);
            let stack = [
                coverage[at],
                coverage.get(pixels + at).copied().unwrap_or(0),
            ];
            write_sample(
                samples,
                mask,
                at,
                compose(first, &self.layers, stack, masked),
            );
        }
        Ok(())
    }

    /// Every tile written, with the state it had before.
    #[must_use]
    pub fn finish(self) -> Vec<(usize, Arc<Tile>)> {
        self.change.finish()
    }

    /// Take the stroke back off the canvas, reporting what that changed.
    pub fn revert(self, canvas: &mut Canvas) -> Option<Bounds> {
        self.change.revert(canvas)
    }
}

fn offset(x: i64, from: i64) -> usize {
    usize::try_from(x - from).unwrap_or(0)
}

/// `first` with each layer laid over it in turn.
fn compose(
    first: Sample,
    layers: &[Option<Layer>; MAX_LAYERS],
    covers: [u8; MAX_LAYERS],
    masked: bool,
) -> Sample {
    let mut sample = first;
    for (layer, cover) in layers.iter().zip(covers) {
        if let (Some(layer), 1..) = (layer, cover) {
            sample = lay_over(sample, *layer, cover, masked);
        }
    }
    sample
}

/// `sample` with `layer` laid over it covering `cover` of it.
#[must_use]
pub fn lay_over(sample: Sample, layer: Layer, cover: u8, masked: bool) -> Sample {
    let most = cover >= 128;
    match (sample, layer.ink, layer.blend) {
        (Sample::Rgba(_), Ink::Colour(ink), Blend::Replace) if most => Sample::Rgba(ink),
        (Sample::Rgba(below), Ink::Colour(ink), Blend::Over) => {
            Sample::Rgba(over(ink, cover, below))
        }
        (Sample::Rgba(_), Ink::Clear, Blend::Replace) if most => Sample::Rgba([0; 4]),
        (Sample::Rgba(below), Ink::Clear, Blend::Over) => Sample::Rgba(erase(below, cover)),
        (Sample::Index(..), Ink::Index(index), _) if most => Sample::Index(index, u8::MAX),
        (Sample::Index(index, _), Ink::Clear, _) if most && masked => Sample::Index(index, 0),
        _ => sample,
    }
}

/// `value * weight / 255`, rounded.
fn scaled(value: u8, weight: u8) -> u32 {
    (u32::from(value) * u32::from(weight) + 127) / 255
}

/// `ink`, covering `cover` of the pixel, composited over `below`.
fn over(ink: Rgba8, cover: u8, below: Rgba8) -> Rgba8 {
    let alpha = u8::try_from(scaled(ink[3], cover)).unwrap_or(u8::MAX);
    tairix_image::over(below, [ink[0], ink[1], ink[2], alpha])
}

/// `below` with `cover` of its opacity taken away.
fn erase(below: Rgba8, cover: u8) -> Rgba8 {
    let alpha = scaled(below[3], 255 - cover);
    if alpha == 0 {
        [0; 4]
    } else {
        [
            below[0],
            below[1],
            below[2],
            u8::try_from(alpha).unwrap_or(u8::MAX),
        ]
    }
}

#[cfg(test)]
#[path = "stroke_tests.rs"]
mod tests;
