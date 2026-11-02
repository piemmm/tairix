//! A sprite's thumbnail in its file window (`plans/PAINT.md` PT32): the
//! picture as its layers show together, at its pixel shape, fitted inside a
//! square — enlarged by whole pixels so a sprite's pixels stay sharp, reduced
//! by averaging where it is larger than the square.

use alloc::vec::Vec;

use tairix_image::Rgba8;
use tairix_raster::{resample, Rgba8Image};
use tairix_util::fallible;

use crate::document::Picture;

/// Most source pixels a thumbnail is averaged from: a picture larger than
/// this is first sampled down to it, so a thumbnail's cost is bounded by the
/// square rather than the picture.
const MOST_SOURCE_PIXELS: u64 = 1 << 22;

/// Where in its square a thumbnail's picture lies: left, top, width, height.
pub type Placed = (u32, u32, u32, u32);

/// `picture` drawn inside a `side`-pixel square of straight-alpha RGBA8,
/// centred, with where in the square it lies; `None` where memory is refused
/// or `side` is zero.
#[must_use]
pub fn thumbnail(picture: &Picture, side: u32) -> Option<(Vec<u8>, Placed)> {
    if side == 0 {
        return None;
    }
    let canvas = picture.flattened().ok()?;
    let (width, height) = (canvas.width(), canvas.height());
    let (across, down) = picture.pixel_aspect();
    let (shown_w, shown_h) = (
        u64::from(width) * u64::from(across),
        u64::from(height) * u64::from(down),
    );
    let side64 = u64::from(side);
    // Whole pixels where the picture fits: the largest whole growth that
    // keeps it inside the square.
    let growth = (side64 / shown_w.max(1)).min(side64 / shown_h.max(1));
    let (fit_w, fit_h) = if growth >= 1 {
        (shown_w * growth, shown_h * growth)
    } else {
        let longest = shown_w.max(shown_h).max(1);
        (
            (shown_w * side64 / longest).max(1),
            (shown_h * side64 / longest).max(1),
        )
    };
    let (fit_w, fit_h) = (
        u32::try_from(fit_w.min(side64)).ok()?,
        u32::try_from(fit_h.min(side64)).ok()?,
    );
    let drawn = if growth >= 1 {
        grown(&canvas_rows(&canvas, 1)?, (width, height), (fit_w, fit_h))?
    } else {
        let step = decimation(width, height);
        let (source_w, source_h) = (width.div_ceil(step), height.div_ceil(step));
        let source = canvas_rows(&canvas, step)?;
        let image = Rgba8Image::new(source_w, source_h, &source).ok()?;
        resample(&image, image.whole(), fit_w, fit_h).ok()?
    };
    let placed = ((side - fit_w) / 2, (side - fit_h) / 2, fit_w, fit_h);
    Some((in_square(&drawn, placed, side)?, placed))
}

/// The step every `step`th pixel of each `step`th row is sampled at, so the
/// samples stay within [`MOST_SOURCE_PIXELS`].
fn decimation(width: u32, height: u32) -> u32 {
    let pixels = u64::from(width) * u64::from(height);
    let mut step = 1u32;
    while pixels / (u64::from(step) * u64::from(step)) > MOST_SOURCE_PIXELS {
        step += 1;
    }
    step
}

/// Every `step`th pixel of every `step`th row of `canvas`, as RGBA8.
fn canvas_rows(canvas: &crate::canvas::Canvas, step: u32) -> Option<Vec<u8>> {
    let (width, height) = (canvas.width(), canvas.height());
    let across = width.div_ceil(step) as usize;
    let mut row: Vec<Rgba8> = fallible::filled(width as usize, [0; 4])?;
    let mut out = fallible::filled(across * height.div_ceil(step) as usize * 4, 0u8)?;
    for (y, line) in (0..height)
        .step_by(step as usize)
        .zip(out.chunks_exact_mut(across * 4))
    {
        canvas.row_colours(y, 0, &mut row);
        for (pixel, at) in row
            .iter()
            .step_by(step as usize)
            .zip(line.as_chunks_mut::<4>().0)
        {
            *at = *pixel;
        }
    }
    Some(out)
}

/// `source`, `width`×`height`, enlarged by whole pixels to `fit`: each
/// source pixel a block, so its edges stay sharp.
fn grown(source: &[u8], (width, height): (u32, u32), fit: (u32, u32)) -> Option<Vec<u8>> {
    let (fit_w, fit_h) = (fit.0 as usize, fit.1 as usize);
    let mut out = fallible::filled(fit_w * fit_h * 4, 0u8)?;
    for (y, line) in out.chunks_exact_mut(fit_w * 4).enumerate() {
        let from = y * height as usize / fit_h;
        let row = &source[from * width as usize * 4..][..width as usize * 4];
        for (x, pixel) in line.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let at = x * width as usize / fit_w;
            pixel.copy_from_slice(&row[at * 4..at * 4 + 4]);
        }
    }
    Some(out)
}

/// `drawn` laid at `placed` in a clear `side`-pixel square.
fn in_square(drawn: &[u8], (left, top, width, height): Placed, side: u32) -> Option<Vec<u8>> {
    let side = side as usize;
    let mut square = fallible::filled(side * side * 4, 0u8)?;
    let (left, top, width) = (left as usize, top as usize, width as usize);
    for (y, line) in drawn
        .chunks_exact(width * 4)
        .enumerate()
        .take(height as usize)
    {
        let at = ((top + y) * side + left) * 4;
        square.get_mut(at..at + line.len())?.copy_from_slice(line);
    }
    Some(square)
}

#[cfg(test)]
#[path = "thumb_tests.rs"]
mod tests;
