//! Laying a picture's layers together: what the window shows of them, what a
//! merge makes of two, and what a format of one picture is written as.
//!
//! Each visible layer is laid over those beneath it source-over, its opacity
//! weighing its alpha. Source-over is associative, so layers laid together
//! onto nothing and then over the rest show as they did apart, to within the
//! rounding of a level, which is what lets a merge keep the picture's look.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_image::{over, Rgba8};
use tairix_parallel::JobRunner;
use tairix_raster::div255;
use tairix_util::fallible;

use crate::canvas::{Canvas, Kind, OutOfMemory, Sample, Tile, TILE};
use crate::document::Layer;
use crate::mask::scale;
use crate::op::Halted;

/// `above`, a layer's colour shown at `opacity`, laid over `below`.
#[must_use]
pub fn laid(below: Rgba8, above: Rgba8, opacity: u8) -> Rgba8 {
    over(
        below,
        [above[0], above[1], above[2], scale(above[3], opacity)],
    )
}

/// The colour `share` 255ths of the way from `from` to `to`, each colour
/// weighed by its alpha, so one fading to clear keeps its hue rather than
/// taking the clear one's meaningless colour.
#[must_use]
pub fn between(from: Rgba8, to: Rgba8, share: u8) -> Rgba8 {
    let (near, far) = (u32::from(u8::MAX - share), u32::from(share));
    let weight = u32::from(from[3]) * near + u32::from(to[3]) * far;
    if weight == 0 {
        return [0; 4];
    }
    let channel = |at: usize| {
        let sum = u32::from(from[at]) * u32::from(from[3]) * near
            + u32::from(to[at]) * u32::from(to[3]) * far;
        u8::try_from((sum + weight / 2) / weight).unwrap_or(u8::MAX)
    };
    [channel(0), channel(1), channel(2), div255(weight)]
}

/// What `layers` show together across a run of pixels, into `out`: each
/// visible layer's colours there, read through `read` into `scratch` — save
/// the layer `active` names, whose colours are given as it is shown — laid
/// over those beneath.
pub fn compose_run(
    layers: &[Layer],
    active: Option<(usize, &[Rgba8])>,
    out: &mut [Rgba8],
    scratch: &mut [Rgba8],
    read: impl FnMut(&Canvas, &mut [Rgba8]),
) {
    out.fill([0; 4]);
    lay_run(layers, active, out, scratch, read);
}

/// [`compose_run`] laid over what `out` holds rather than over nothing: given
/// the layers beneath `layers` laid together there, it shows exactly what
/// laying every layer does, as source-over is laid in the same order.
pub fn lay_run(
    layers: &[Layer],
    active: Option<(usize, &[Rgba8])>,
    out: &mut [Rgba8],
    scratch: &mut [Rgba8],
    mut read: impl FnMut(&Canvas, &mut [Rgba8]),
) {
    for (index, layer) in layers.iter().enumerate() {
        if !layer.shows() {
            continue;
        }
        let colours: &[Rgba8] = match active {
            Some((at, shown)) if at == index => shown,
            _ => {
                read(&layer.canvas, scratch);
                scratch
            }
        };
        for (colour, &above) in out.iter_mut().zip(colours) {
            *colour = laid(*colour, above, layer.opacity);
        }
    }
}

/// `layers` laid together onto nothing, as one canvas, a tile at a time across
/// `runner`: the one layer that shows, its pixels shared, where it shows them
/// alone and wholly.
///
/// # Errors
///
/// [`OutOfMemory`] when the canvas or a tile of it cannot be had.
pub fn flatten(layers: &[Layer], runner: &dyn JobRunner) -> Result<Canvas, OutOfMemory> {
    let mut shown = layers.iter().filter(|layer| layer.shows());
    if let (Some(only), None) = (shown.next(), shown.next()) {
        if only.opacity == u8::MAX {
            return only.canvas.try_clone();
        }
    }
    // Wanted throughout, the laying halts only for want of memory.
    laid_together(layers, runner, &|| true).map_err(|_| OutOfMemory)
}

/// A tile's place, and the tile once it is laid: none where the laying was
/// no longer wanted when it came to it.
type Laid = (usize, Option<Result<Arc<Tile>, OutOfMemory>>);

/// `layers` laid together onto nothing, every pixel exactly as
/// [`compose_run`] makes it, so laying further layers over it with
/// [`lay_run`] shows what laying them all does: a tile at a time across
/// `runner`, `wanted` asked before each, a tile nothing shows in left the
/// canvas's one clear tile. [`flatten`]'s shared pixels may differ from it in
/// the colour a clear pixel holds.
///
/// # Errors
///
/// [`Halted::Abandoned`] once `wanted` says no, and [`Halted::OutOfMemory`]
/// when the canvas or a tile of it cannot be had, or there are no layers.
pub fn laid_together(
    layers: &[Layer],
    runner: &dyn JobRunner,
    wanted: &(dyn Fn() -> bool + Sync),
) -> Result<Canvas, Halted> {
    let Some(first) = layers.first() else {
        return Err(Halted::OutOfMemory);
    };
    let (width, height) = (first.canvas.width(), first.canvas.height());
    let mut laid = Canvas::new(width, height, Kind::Rgba, Sample::Rgba([0; 4]))
        .map_err(|_| Halted::OutOfMemory)?;
    let count = laid.tile_count();
    let mut tiles: Vec<Laid> = fallible::collected(count, (0..count).map(|index| (index, None)))
        .ok_or(Halted::OutOfMemory)?;
    let clear = &laid;
    tairix_parallel::for_each(runner, &mut tiles, &|(index, made): &mut Laid| {
        if wanted() {
            *made = Some(laid_tile(layers, clear, *index));
        }
    });
    for (index, made) in tiles {
        let tile = made.ok_or(Halted::Abandoned)??;
        laid.replace_tile(index, tile);
    }
    Ok(laid)
}

/// Tile `index` of `clear`, a clear canvas the size of `layers`, as they show
/// together there: `clear`'s own tile where nothing does.
fn laid_tile(layers: &[Layer], clear: &Canvas, index: usize) -> Result<Arc<Tile>, OutOfMemory> {
    let rect = clear.tile_rect(index);
    let width = rect.width as usize;
    let (mut out, mut scratch) = ([[0u8; 4]; TILE as usize], [[0u8; 4]; TILE as usize]);
    let (shown, spare) = (&mut out[..width], &mut scratch[..width]);
    let mut written: Option<Tile> = None;
    for (row, y) in (rect.y..rect.y + rect.height).enumerate() {
        compose_run(layers, None, shown, spare, |canvas, into| {
            canvas.row_colours(y, rect.x, into);
        });
        if written.is_none() && shown.iter().all(|&colour| colour == [0; 4]) {
            continue;
        }
        if written.is_none() {
            written = Some(clear.tile(index).copy()?);
        }
        if let Some(tile) = written.as_mut() {
            let (samples, _) = tile.planes_mut();
            if let Some(into) = samples.get_mut(row * width * 4..(row + 1) * width * 4) {
                into.copy_from_slice(shown.as_flattened());
            }
        }
    }
    Ok(written.map_or_else(|| Arc::clone(clear.tile(index)), Arc::new))
}

#[cfg(test)]
#[path = "compose_tests.rs"]
mod tests;
