//! Working an operation out a block of tiles at a time: the blocks a run is
//! cut into, the margin an effect reads about each, and the order and the
//! cores they run on.
//!
//! A block reads its effect's whole reach about it wherever the picture
//! extends, so every pixel it writes is the one a single window over the whole
//! picture would make of it: a run is bit-identical however it is cut and
//! whichever block runs first. An adjustment reads nothing but the pixel it
//! maps. A run asks whether it is still wanted before each block, so a stale
//! preview stops between blocks rather than finishing, and stops at the round
//! a block halts in rather than working out the rest.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_image::Rgba8;
use tairix_inline::ArrayVec;
use tairix_parallel::JobRunner;
use tairix_util::fallible;

use crate::canvas::{write_sample, Canvas, Kind, OutOfMemory, Planes, Sample, Tile, TILE};
use crate::compose::between;
use crate::effect::Window;
use crate::mask::Mask;
use crate::op::{Halted, Op, Work};
use crate::shape::Bounds;

/// The narrowest block, in tiles a side: what an adjustment, or an effect
/// reaching no further than half a tile, is worked out in.
const NARROW_BLOCK: u32 = 2;

/// The widest block, in tiles a side, which bounds the window one block reads
/// however far its effect reaches.
const WIDE_BLOCK: u32 = 8;

/// How many times its reach a block's side is at the least, so the margin it
/// reads about what it writes is at most half its side.
const SIDE_PER_REACH: u32 = 4;

/// The most tiles a block holds.
const BLOCK_TILES: usize = (WIDE_BLOCK * WIDE_BLOCK) as usize;

/// Blocks handed to the runner at once for each participant: enough that a
/// straggler is absorbed, few enough that a stale run stops soon after.
const ROUND: usize = 4;

/// No pixel at all: where a fold of tiles' bounds starts.
const NOWHERE: Bounds = Bounds {
    x0: 0,
    y0: 0,
    x1: 0,
    y1: 0,
};

/// The tiles an operation over `canvas` held to `clip` writes: every tile the
/// area it is held to touches, in reading order.
///
/// # Errors
///
/// [`OutOfMemory`] where the list cannot be held.
pub fn tiles_for(canvas: &Canvas, clip: Option<&Mask>) -> Result<Vec<usize>, OutOfMemory> {
    let area = area_of(canvas, clip);
    if area.is_empty() {
        return Ok(Vec::new());
    }
    let tile = i64::from(TILE);
    let columns = area.x0.div_euclid(tile)..=(area.x1 - 1).div_euclid(tile);
    let rows = area.y0.div_euclid(tile)..=(area.y1 - 1).div_euclid(tile);
    let across = columns.end() - columns.start() + 1;
    let down = rows.end() - rows.start() + 1;
    let count = usize::try_from(across * down).map_err(|_| OutOfMemory)?;
    let mut tiles = Vec::new();
    if !fallible::reserve(&mut tiles, count) {
        return Err(OutOfMemory);
    }
    for ty in rows {
        for tx in columns.clone() {
            if let (Ok(x), Ok(y)) = (u32::try_from(tx * tile), u32::try_from(ty * tile)) {
                tiles.push(canvas.tile_index(x, y));
            }
        }
    }
    Ok(tiles)
}

/// Tiles `tiles` of colour picture `source` as `op` leaves them, held to
/// `clip`: a block of them at a time across `runner`, the blocks of the tiles
/// listed first first, `wanted` asked before each, and no later round run once
/// a block has halted. A tile `op` leaves as it was is answered as the very
/// tile it was, sharing its pixels.
///
/// # Errors
///
/// [`Halted::NeedsColour`] for a palette picture, [`Halted::Abandoned`] once
/// `wanted` says no, and [`Halted::OutOfMemory`] where a block's room cannot
/// be had.
pub fn run(
    source: &Canvas,
    op: &Op,
    clip: Option<&Mask>,
    tiles: &[usize],
    runner: &dyn JobRunner,
    wanted: &(dyn Fn() -> bool + Sync),
) -> Result<Vec<(usize, Arc<Tile>)>, Halted> {
    if !matches!(source.kind(), Kind::Rgba) {
        return Err(Halted::NeedsColour);
    }
    let work = op.work();
    let reach = match &work {
        Work::Tone(_) => 0,
        Work::Effect(effect) => effect.reach(),
    };
    let mut blocks = blocks_of(source, tiles, block_side(reach))?;
    let pass = Pass {
        source,
        work: &work,
        clip,
        area: area_of(source, clip),
        reach: i64::from(reach),
    };
    let round = runner.width().max(1).saturating_mul(ROUND);
    for blocks in blocks.chunks_mut(round) {
        if !wanted() {
            return Err(Halted::Abandoned);
        }
        tairix_parallel::for_each(runner, blocks, &|block: &mut Block| {
            block.work(&pass, wanted);
        });
        if let Some(halted) = blocks.iter().find_map(|block| block.halted) {
            return Err(halted);
        }
    }
    gather(blocks, tiles.len())
}

/// Every tile `op` writes over `source` held to `clip`: those of `done`,
/// worked out before over the very same layer, kept as they are, and the rest
/// worked out across `runner` — what Apply lands when the preview it adopts is
/// incomplete.
///
/// # Errors
///
/// [`Halted`] as [`run`] answers it; never [`Halted::Abandoned`], as nothing
/// abandons it.
pub fn complete(
    source: &Canvas,
    op: &Op,
    clip: Option<&Mask>,
    done: Vec<(usize, Arc<Tile>)>,
    runner: &dyn JobRunner,
) -> Result<Vec<(usize, Arc<Tile>)>, Halted> {
    let wanted = tiles_for(source, clip)?;
    let mut held = fallible::filled(source.tile_count(), false).ok_or(OutOfMemory)?;
    for (index, _) in &done {
        if let Some(slot) = held.get_mut(*index) {
            *slot = true;
        }
    }
    let missing = wanted
        .iter()
        .copied()
        .filter(|&index| !held.get(index).copied().unwrap_or(false));
    let missing = fallible::collected(wanted.len(), missing).ok_or(OutOfMemory)?;
    let made = run(source, op, clip, &missing, runner, &|| true)?;
    let mut tiles = done;
    if !fallible::reserve(&mut tiles, made.len()) {
        return Err(Halted::OutOfMemory);
    }
    tiles.extend(made);
    Ok(tiles)
}

/// The pixels an operation over `canvas` held to `clip` may change.
fn area_of(canvas: &Canvas, clip: Option<&Mask>) -> Bounds {
    let whole = Bounds::picture(canvas.width(), canvas.height());
    clip.map_or(whole, |clip| clip.bounds().intersection(&whole))
}

/// A block's side, in tiles, for an operation reaching `reach` pixels about
/// what it writes: the fewest tiles at least four times the reach, from the
/// narrowest block to the widest, so what a block reads is at most two and a
/// half times what it writes for any reach an effect has.
const fn block_side(reach: u32) -> u32 {
    let side = reach.saturating_mul(SIDE_PER_REACH).div_ceil(TILE);
    if side < NARROW_BLOCK {
        NARROW_BLOCK
    } else if side > WIDE_BLOCK {
        WIDE_BLOCK
    } else {
        side
    }
}

/// One block's tiles, each with what it was worked out to, and why the block
/// stopped short where it did.
struct Block {
    tiles: ArrayVec<(usize, Option<Arc<Tile>>), BLOCK_TILES>,
    halted: Option<Halted>,
}

impl Block {
    const fn new() -> Self {
        Self {
            tiles: ArrayVec::new(),
            halted: None,
        }
    }

    /// Work the block out over `pass`, once `wanted` says it still is.
    fn work(&mut self, pass: &Pass<'_>, wanted: &(dyn Fn() -> bool + Sync)) {
        self.halted = if wanted() {
            pass.block(&mut self.tiles).err()
        } else {
            Some(Halted::Abandoned)
        };
    }
}

/// `tiles` gathered into blocks `side` tiles square on the grid, each block
/// placed where its first tile falls among `tiles`, so the tiles wanted first
/// are worked out first; a tile past the canvas, or listed again, is passed
/// over.
fn blocks_of(source: &Canvas, tiles: &[usize], side: u32) -> Result<Vec<Block>, OutOfMemory> {
    let across = source.width().div_ceil(TILE).div_ceil(side);
    let down = source.height().div_ceil(TILE).div_ceil(side);
    let cells = usize::try_from(u64::from(across) * u64::from(down)).map_err(|_| OutOfMemory)?;
    let mut placed: Vec<Option<usize>> = fallible::filled(cells, None).ok_or(OutOfMemory)?;
    let mut listed = fallible::filled(source.tile_count(), false).ok_or(OutOfMemory)?;
    // At most one block a cell, each from a tile listed: no push outgrows this.
    let mut blocks = Vec::new();
    if !fallible::reserve(&mut blocks, cells.min(tiles.len())) {
        return Err(OutOfMemory);
    }
    for &index in tiles {
        let Some(seen) = listed.get_mut(index) else {
            continue;
        };
        if core::mem::replace(seen, true) {
            continue;
        }
        let rect = source.tile_rect(index);
        let cell = (rect.y / TILE / side) * across + rect.x / TILE / side;
        let Some(slot) = usize::try_from(cell)
            .ok()
            .and_then(|cell| placed.get_mut(cell))
        else {
            continue;
        };
        let at = *slot.get_or_insert_with(|| {
            blocks.push(Block::new());
            blocks.len() - 1
        });
        if let Some(block) = blocks.get_mut(at) {
            // A block holds at most `side²` tiles, each once.
            let _ = block.tiles.try_push((index, None));
        }
    }
    Ok(blocks)
}

/// What every block worked out, in the order the blocks were listed, none of
/// them halted.
fn gather(blocks: Vec<Block>, count: usize) -> Result<Vec<(usize, Arc<Tile>)>, Halted> {
    let mut gathered = Vec::new();
    if !fallible::reserve(&mut gathered, count) {
        return Err(Halted::OutOfMemory);
    }
    for block in blocks {
        for (index, tile) in block.tiles {
            gathered.push((index, tile.ok_or(Halted::Abandoned)?));
        }
    }
    Ok(gathered)
}

/// What a run reads and how it writes: shared by every block.
struct Pass<'a> {
    source: &'a Canvas,
    work: &'a Work,
    clip: Option<&'a Mask>,
    /// The pixels the run may change.
    area: Bounds,
    /// How far about a pixel it writes the run reads.
    reach: i64,
}

impl Pass<'_> {
    /// Work out `tiles`, one block's, each as it now stands.
    fn block(&self, tiles: &mut [(usize, Option<Arc<Tile>>)]) -> Result<(), Halted> {
        let held = |index: usize| self.source.tile_rect(index).bounds();
        let inner = tiles
            .iter()
            .fold(NOWHERE, |bounds, &(index, _)| bounds.union(&held(index)))
            .intersection(&self.area);
        let made = match self.work {
            Work::Effect(effect) if !inner.is_empty() => {
                let whole = Bounds::picture(self.source.width(), self.source.height());
                let read = Bounds {
                    x0: inner.x0 - self.reach,
                    y0: inner.y0 - self.reach,
                    x1: inner.x1 + self.reach,
                    y1: inner.y1 + self.reach,
                }
                .intersection(&whole);
                let window = Window::read(self.source, read)?;
                Some((inner, effect.run(&window, inner)?))
            }
            Work::Tone(_) | Work::Effect(_) => None,
        };
        for (index, slot) in tiles.iter_mut() {
            *slot = Some(self.tile(*index, made.as_ref())?);
        }
        Ok(())
    }

    /// Tile `index` with what the run made laid over the pixels of it the area
    /// holds, as much of each as the clip chooses: the very tile where nothing
    /// changed.
    fn tile(
        &self,
        index: usize,
        made: Option<&(Bounds, Vec<Rgba8>)>,
    ) -> Result<Arc<Tile>, OutOfMemory> {
        let held = self.source.tile(index);
        let rect = self.source.tile_rect(index);
        let span = rect.bounds().intersection(&self.area);
        let along = usize::try_from(span.x1 - span.x0).unwrap_or(0);
        let mut shares = [u8::MAX; TILE as usize];
        let mut written: Option<Tile> = None;
        for y in span.y0..span.y1 {
            let shares = &mut shares[..along];
            if let Some(clip) = self.clip {
                clip.row(y, span.x0, shares);
            }
            let row = usize::try_from(y - i64::from(rect.y)).unwrap_or(0) * rect.width as usize;
            let first = row + usize::try_from(span.x0 - i64::from(rect.x)).unwrap_or(0);
            for ((x, at), &share) in (span.x0..).zip(first..).zip(shares.iter()) {
                if share == 0 {
                    continue;
                }
                let Sample::Rgba(colour) = held.sample(Planes::Rgba, at) else {
                    continue;
                };
                let changed = self.changed(made, (x, y), colour);
                let laid = if share == u8::MAX {
                    changed
                } else {
                    between(colour, changed, share)
                };
                if laid == colour {
                    continue;
                }
                if written.is_none() {
                    written = Some(held.copy()?);
                }
                if let Some(tile) = written.as_mut() {
                    let (samples, mask) = tile.planes_mut();
                    write_sample(samples, mask, at, Sample::Rgba(laid));
                }
            }
        }
        Ok(written.map_or_else(|| Arc::clone(held), Arc::new))
    }

    /// What the run makes of `colour` at `(x, y)`.
    fn changed(
        &self,
        made: Option<&(Bounds, Vec<Rgba8>)>,
        (x, y): (i64, i64),
        colour: Rgba8,
    ) -> Rgba8 {
        match (self.work, made) {
            (Work::Tone(map), _) => map.map(colour),
            (Work::Effect(_), Some((inner, pixels))) => {
                pixel_of(pixels, *inner, x, y).unwrap_or(colour)
            }
            (Work::Effect(_), None) => colour,
        }
    }
}

/// The pixel at `(x, y)` of `pixels`, laid out row by row across `inner`.
fn pixel_of(pixels: &[Rgba8], inner: Bounds, x: i64, y: i64) -> Option<Rgba8> {
    let at = usize::try_from((y - inner.y0) * (inner.x1 - inner.x0) + (x - inner.x0)).ok()?;
    pixels.get(at).copied()
}

#[cfg(test)]
#[path = "tiled_tests.rs"]
mod tests;
