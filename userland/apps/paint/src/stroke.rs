//! Changes to a canvas, gathered into one step of history.
//!
//! A change keeps each tile as it stood before its first write, which is the
//! whole of what undoing it needs. A stroke is a change that lays down
//! coverage, each pixel recomposed from its first state: a line or a shape
//! keeps the most any pass covered a pixel, so going back over it does not
//! darken it twice, while a brush's dabs build up to its opacity. A selection
//! held while painting clips the stroke: each pixel is laid no more than the
//! selection chooses it.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_image::Rgba8;
use tairix_util::fallible;

use crate::canvas::{read_sample, write_sample, Canvas, OutOfMemory, Planes, Sample, Tile, TILE};
use crate::colour::Ink;
use crate::mask::{scale, Mask};
use crate::shape::{polygon_rows, Bounds, Point, Shape, ShapeScratch};

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

    /// Every tile written, as it now stands on `picture`: what a worker
    /// answers for the window to adopt.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the list cannot be held.
    pub fn written(self, picture: &Canvas) -> Result<Vec<(usize, Arc<Tile>)>, OutOfMemory> {
        let before = self.finish();
        fallible::collected(
            before.len(),
            before
                .into_iter()
                .map(|(index, _)| (index, Arc::clone(picture.tile(index)))),
        )
        .ok_or(OutOfMemory)
    }

    /// Repaint `area` of `picture` through `paint`, handed each run of a
    /// tile's row — the picture pixels from `(x, y)` rightwards — to rewrite
    /// in place, each tile's first state kept.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when a tile cannot be copied for writing.
    pub fn repaint(
        &mut self,
        picture: &mut Canvas,
        area: Bounds,
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
                let written = self.touch(picture, index)?;
                let (samples, mask) = written.planes_mut();
                let mut run = [Sample::Rgba([0; 4]); TILE as usize];
                let run = &mut run[..usize::try_from(span.x1 - span.x0).unwrap_or(0)];
                for y in span.y0..span.y1 {
                    let row =
                        usize::try_from(y - i64::from(rect.y)).unwrap_or(0) * rect.width as usize;
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
        self.mark(area);
        Ok(())
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
pub struct Coat {
    /// What is put down.
    pub ink: Ink,
    /// How it meets what is beneath.
    pub blend: Blend,
}

/// Most coats one stroke lays down: a shape's fill and its outline.
pub const MAX_COATS: usize = 2;

/// How passes over one pixel within a stroke add up.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Build {
    /// A pixel keeps the most any pass covered it, so a line crossing itself
    /// does not darken: a pencil, a line, a shape, a fill.
    Most,
    /// Each pass lays its share over what the stroke has laid, and the whole
    /// is laid at `opacity`: a brush's dabs building up.
    Up {
        /// The most of a pixel the whole stroke lays.
        opacity: u8,
    },
}

impl Build {
    /// What the stroke lays of a pixel it has built up `held` of.
    fn laid(self, held: u8) -> u8 {
        match self {
            Self::Most => held,
            Self::Up { opacity } => scale(held, opacity),
        }
    }

    /// What `held` comes to once a pass covers `cover` of the pixel.
    fn after(self, held: u8, cover: u8) -> u8 {
        match self {
            Self::Most => held.max(cover),
            Self::Up { .. } => held.saturating_add(scale(cover, u8::MAX - held)),
        }
    }
}

/// A change laying down coverage.
#[derive(Debug)]
pub struct Stroke {
    coats: [Option<Coat>; MAX_COATS],
    change: Change,
    /// For each tile written, the coverage of each coat it lays down, a
    /// coat's pixels after the one before.
    coverage: Kept<Vec<u8>>,
    build: Build,
    /// How far a clone stroke's source lies from each pixel it lays, in
    /// pixels: what it lays is the picture there as it stood when the
    /// stroke began.
    source: Option<(i64, i64)>,
    /// The selection it is held to, if any.
    clip: Option<Mask>,
    /// A row's coverage, and the selection's share of the same row.
    scratch: Vec<u8>,
    shapes: ShapeScratch,
}

impl Stroke {
    /// A stroke laying down `fill` and, over it, `over`, within `clip` where
    /// a selection is held.
    #[must_use]
    pub fn new(fill: Coat, over: Option<Coat>, clip: Option<Mask>) -> Self {
        Self {
            coats: [Some(fill), over],
            change: Change::new(),
            coverage: Kept::new(),
            build: Build::Most,
            source: None,
            clip,
            scratch: Vec::new(),
            shapes: ShapeScratch::default(),
        }
    }

    /// A stroke whose passes of `coat` build up, the whole laid at
    /// `opacity`, within `clip` where a selection is held: a brush's.
    #[must_use]
    pub fn building(coat: Coat, opacity: u8, clip: Option<Mask>) -> Self {
        Self {
            build: Build::Up { opacity },
            ..Self::new(coat, None, clip)
        }
    }

    /// A building stroke laying, at each pixel, the picture `offset` pixels
    /// away as it stood when the stroke began, met as `blend` says: the
    /// clone tool's.
    #[must_use]
    pub fn cloning(offset: (i64, i64), blend: Blend, opacity: u8, clip: Option<Mask>) -> Self {
        let coat = Coat {
            ink: Ink::Clear,
            blend,
        };
        Self {
            source: Some(offset),
            ..Self::building(coat, opacity, clip)
        }
    }

    /// Pixel `(x, y)` of `canvas` as it stood when the stroke began; `None`
    /// off the picture.
    fn first_sample(&self, canvas: &Canvas, x: i64, y: i64) -> Option<Sample> {
        let (px, py) = (u32::try_from(x).ok()?, u32::try_from(y).ok()?);
        if px >= canvas.width() || py >= canvas.height() {
            return None;
        }
        let index = canvas.tile_index(px, py);
        let Some(before) = self.change.before.get(index) else {
            return canvas.sample(px, py);
        };
        let rect = canvas.tile_rect(index);
        let at = ((py - rect.y) * rect.width + (px - rect.x)) as usize;
        Some(before.sample(canvas.kind().planes(), at))
    }

    /// What changed since this was last asked.
    pub fn take_damage(&mut self) -> Option<Bounds> {
        self.change.take_damage()
    }

    /// Lay coat `coat` down where `shape` covers, anti-aliased when `aa`.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when a tile could not be copied for writing; what was
    /// already laid down stays.
    pub fn cover(
        &mut self,
        canvas: &mut Canvas,
        coat: usize,
        shape: &Shape,
        aa: bool,
    ) -> Result<(), OutOfMemory> {
        self.cover_shaded(canvas, coat, shape, aa, |_, _, _| {})
    }

    /// Lay coat `coat` down where `shape` covers, anti-aliased when `aa`,
    /// each row's coverage then handed to `weigh` — its row, its first column
    /// and the coverage — to scale in place: a brush tip's falloff and flow.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`], as [`cover`](Self::cover).
    pub fn cover_shaded(
        &mut self,
        canvas: &mut Canvas,
        coat: usize,
        shape: &Shape,
        aa: bool,
        mut weigh: impl FnMut(i64, i64, &mut [u8]),
    ) -> Result<(), OutOfMemory> {
        let mut shapes = core::mem::take(&mut self.shapes);
        let outcome = match shape.rows(aa, &mut shapes) {
            Ok(Some(mut rows)) => self.cover_rows(canvas, coat, shape.bounds(), |y, x, out| {
                // Rows and columns reach here held to the picture's own.
                let (Ok(row), Ok(column)) = (u32::try_from(y), u32::try_from(x)) else {
                    return;
                };
                rows.row(row, column, out);
                weigh(y, x, out);
            }),
            Ok(None) => Ok(()),
            Err(err) => Err(err),
        };
        self.shapes = shapes;
        outcome
    }

    /// Lay coat `coat` down over what the closed outline through `points`
    /// encloses, anti-aliased when `aa`: a filled polygon.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`], as [`cover`](Self::cover).
    pub fn cover_enclosed(
        &mut self,
        canvas: &mut Canvas,
        coat: usize,
        points: &[Point],
        aa: bool,
    ) -> Result<(), OutOfMemory> {
        let mut shapes = core::mem::take(&mut self.shapes);
        let outcome = match polygon_rows(points, aa, &mut shapes) {
            Ok(Some((mut rows, bounds))) => self.cover_rows(canvas, coat, bounds, |y, x, out| {
                let (Ok(row), Ok(column)) = (u32::try_from(y), u32::try_from(x)) else {
                    return;
                };
                rows.row(row, column, out);
            }),
            Ok(None) => Ok(()),
            Err(err) => Err(err),
        };
        self.shapes = shapes;
        outcome
    }

    /// Lay coat `coat` down wholly over pixel `(x, y)`.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`], as [`cover`](Self::cover).
    pub fn cover_pixel(
        &mut self,
        canvas: &mut Canvas,
        coat: usize,
        x: i64,
        y: i64,
    ) -> Result<(), OutOfMemory> {
        let bounds = Bounds {
            x0: x,
            y0: y,
            x1: x + 1,
            y1: y + 1,
        };
        self.cover_rows(canvas, coat, bounds, |_, _, out| out.fill(255))
    }

    /// Lay coat `coat` down over `bounds` as `row` says: it is handed each
    /// row's index, its first column and a coverage a pixel to fill in.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`], as [`cover`](Self::cover).
    pub fn cover_rows(
        &mut self,
        canvas: &mut Canvas,
        coat: usize,
        bounds: Bounds,
        mut row: impl FnMut(i64, i64, &mut [u8]),
    ) -> Result<(), OutOfMemory> {
        let mut bounds = bounds.intersection(&Bounds::picture(canvas.width(), canvas.height()));
        if let Some(clip) = &self.clip {
            bounds = bounds.intersection(&clip.bounds());
        }
        if bounds.is_empty() || coat >= MAX_COATS || self.coats[coat].is_none() {
            return Ok(());
        }
        let span = usize::try_from(bounds.x1 - bounds.x0).map_err(|_| OutOfMemory)?;
        if !fallible::grow_to(&mut self.scratch, span * 2, 0) {
            return Err(OutOfMemory);
        }
        let planes = canvas.kind().planes();
        let mut scratch = core::mem::take(&mut self.scratch);
        let mut outcome = Ok(());
        'rows: for y in bounds.y0..bounds.y1 {
            let (cover, chosen) = scratch[..span * 2].split_at_mut(span);
            cover.fill(0);
            row(y, bounds.x0, cover);
            let chosen = self.clip.as_ref().map(|clip| {
                clip.row(y, bounds.x0, chosen);
                &*chosen
            });
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
                let run = offset(start, bounds.x0)..offset(end, bounds.x0);
                let chosen = chosen.map(|chosen| &chosen[run.clone()]);
                let mut copied = [None; TILE as usize];
                let sources = self.source.map(|(dx, dy)| {
                    let copied = &mut copied[..run.len()];
                    for (column, slot) in (start..).zip(copied.iter_mut()) {
                        *slot = self.first_sample(canvas, column + dx, y + dy);
                    }
                    &*copied
                });
                let picks = (chosen, sources);
                if let Err(err) = self.lay(canvas, (planes, coat), (y, start), &cover[run], picks) {
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

    /// Lay `covers` of `coat` down along row `y` from column `x`, all
    /// within one tile, each pixel laid no more than `chosen` of it where a
    /// selection is held, and a clone stroke's each pixel taken from
    /// `sources` — none where its source lies off the picture.
    fn lay(
        &mut self,
        canvas: &mut Canvas,
        (planes, coat): (Planes, usize),
        (y, x): (i64, i64),
        covers: &[u8],
        (chosen, sources): (Option<&[u8]>, Option<&[Option<Sample>]>),
    ) -> Result<(), OutOfMemory> {
        let (Ok(px), Ok(py)) = (u32::try_from(x), u32::try_from(y)) else {
            return Ok(());
        };
        let index = canvas.tile_index(px, py);
        let rect = canvas.tile_rect(index);
        let pixels = (rect.width * rect.height) as usize;
        let coats = self.coats.iter().flatten().count();
        let coverage = self.coverage.get_or_try_insert(index, || {
            fallible::filled(pixels * coats, 0u8).ok_or(OutOfMemory)
        })?;
        let tile = self.change.touch(canvas, index)?;
        let Some(before) = self.change.before.get(index) else {
            return Ok(());
        };
        let masked = planes == Planes::Indexed { masked: true };
        let row_start = ((py - rect.y) * rect.width + (px - rect.x)) as usize;
        let (samples, mask) = tile.planes_mut();
        let build = self.build;
        for (step, &cover) in covers.iter().enumerate() {
            let at = row_start + step;
            let chosen = chosen.map_or(u8::MAX, |chosen| chosen[step]);
            let held = &mut coverage[coat * pixels + at];
            let next = build.after(*held, cover);
            if chosen == 0 || next == *held {
                continue;
            }
            *held = next;
            let first = before.sample(planes, at);
            let laid = |held: u8| scale(build.laid(held), chosen);
            let stack = [
                laid(coverage[at]),
                laid(coverage.get(pixels + at).copied().unwrap_or(0)),
            ];
            let composed = match sources {
                None => compose(first, &self.coats, stack, masked),
                Some(sources) => match (sources.get(step).copied().flatten(), self.coats[0]) {
                    (Some(source), Some(coat)) => {
                        let ink = match source {
                            Sample::Rgba(colour) => Ink::Colour(colour),
                            Sample::Index(_, 0) if masked => Ink::Clear,
                            Sample::Index(index, _) => Ink::Index(index),
                        };
                        lay_over(first, Coat { ink, ..coat }, stack[0], masked)
                    }
                    _ => first,
                },
            };
            write_sample(samples, mask, at, composed);
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

/// `first` with each coat laid over it in turn.
fn compose(
    first: Sample,
    coats: &[Option<Coat>; MAX_COATS],
    covers: [u8; MAX_COATS],
    masked: bool,
) -> Sample {
    let mut sample = first;
    for (coat, cover) in coats.iter().zip(covers) {
        if let (Some(coat), 1..) = (coat, cover) {
            sample = lay_over(sample, *coat, cover, masked);
        }
    }
    sample
}

/// `sample` with `coat` laid over it covering `cover` of it.
#[must_use]
pub fn lay_over(sample: Sample, coat: Coat, cover: u8, masked: bool) -> Sample {
    let most = cover >= 128;
    match (sample, coat.ink, coat.blend) {
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
