//! Flood fill: the pixels joined to one pixel through pixels like it, side
//! to side and top to bottom.
//!
//! The flood judges each pixel as it reaches it, and keeps one bitmap of the
//! pixels it has taken.

use alloc::vec::Vec;

use tairix_util::fallible;

use crate::canvas::{Canvas, OutOfMemory, Planes, Sample};
use crate::shape::Bounds;
use crate::stroke::{Layer, Stroke};

/// The pixels a flood reached.
#[derive(Debug)]
pub struct Region {
    width: u32,
    bits: Vec<u64>,
    bounds: Bounds,
}

impl Region {
    /// The pixels the region lies within.
    #[must_use]
    pub const fn bounds(&self) -> Bounds {
        self.bounds
    }

    /// Whether pixel `(x, y)` is in the region.
    #[must_use]
    pub fn contains(&self, x: u32, y: u32) -> bool {
        x < self.width && bit(&self.bits, y as usize * self.width as usize + x as usize)
    }
}

fn bit(bits: &[u64], at: usize) -> bool {
    bits.get(at / 64)
        .is_some_and(|word| word & (1 << (at % 64)) != 0)
}

fn set(bits: &mut [u64], at: usize) {
    if let Some(word) = bits.get_mut(at / 64) {
        *word |= 1 << (at % 64);
    }
}

/// Whether `sample` is like `seed`: the same entry of a palette picture, and
/// within `tolerance` in every channel of a colour one. Clear pixels are all
/// alike whatever colour they hide.
fn alike(seed: Sample, sample: Sample, tolerance: u8, planes: Planes) -> bool {
    match (seed, sample) {
        (Sample::Index(want, want_alpha), Sample::Index(index, alpha)) => match planes {
            Planes::Indexed { masked: true } if want_alpha < 128 || alpha < 128 => {
                want_alpha < 128 && alpha < 128
            }
            _ => want == index,
        },
        (Sample::Rgba(want), Sample::Rgba(colour)) => {
            (want[3] == 0 && colour[3] == 0)
                || want
                    .iter()
                    .zip(colour)
                    .all(|(a, b)| a.abs_diff(b) <= tolerance)
        }
        _ => false,
    }
}

/// The region of `canvas` joined to pixel `(x, y)` through pixels like it,
/// or `None` for a pixel off the canvas. A pixel is judged only once the
/// flood reaches it, so a small region of a large picture costs its own size.
///
/// # Errors
///
/// [`OutOfMemory`] when the bitmap cannot be had.
pub fn region(
    canvas: &Canvas,
    x: u32,
    y: u32,
    tolerance: u8,
) -> Result<Option<Region>, OutOfMemory> {
    let Some(seed) = canvas.sample(x, y) else {
        return Ok(None);
    };
    let (width, height) = (canvas.width() as usize, canvas.height() as usize);
    let mut reached = fallible::filled((width * height).div_ceil(64), 0u64).ok_or(OutOfMemory)?;
    let planes = canvas.kind().planes();
    // Whether pixel `(x, y)`, not yet reached, joins the region.
    let open = |reached: &[u64], x: usize, y: usize| {
        !bit(reached, y * width + x)
            && u32::try_from(x)
                .ok()
                .zip(u32::try_from(y).ok())
                .and_then(|(x, y)| canvas.sample(x, y))
                .is_some_and(|sample| alike(seed, sample, tolerance, planes))
    };
    let mut bounds = Bounds {
        x0: i64::from(x),
        y0: i64::from(y),
        x1: i64::from(x) + 1,
        y1: i64::from(y) + 1,
    };
    let mut pending: Vec<(usize, usize)> = Vec::new();
    push(&mut pending, (x as usize, y as usize))?;
    while let Some((px, py)) = pending.pop() {
        if !open(&reached, px, py) {
            continue;
        }
        let mut left = px;
        while left > 0 && open(&reached, left - 1, py) {
            left -= 1;
        }
        let mut right = px;
        while right + 1 < width && open(&reached, right + 1, py) {
            right += 1;
        }
        let row = py * width;
        for column in left..=right {
            set(&mut reached, row + column);
        }
        bounds = bounds.union(&Bounds {
            x0: i64::try_from(left).unwrap_or(0),
            y0: i64::try_from(py).unwrap_or(0),
            x1: i64::try_from(right + 1).unwrap_or(0),
            y1: i64::try_from(py + 1).unwrap_or(0),
        });
        for next in [
            py.checked_sub(1),
            Some(py + 1).filter(|&below| below < height),
        ]
        .into_iter()
        .flatten()
        {
            let mut column = left;
            while column <= right {
                if open(&reached, column, next) {
                    push(&mut pending, (column, next))?;
                    while column <= right && open(&reached, column, next) {
                        column += 1;
                    }
                } else {
                    column += 1;
                }
            }
        }
    }
    Ok(Some(Region {
        width: canvas.width(),
        bits: reached,
        bounds,
    }))
}

fn push(pending: &mut Vec<(usize, usize)>, at: (usize, usize)) -> Result<(), OutOfMemory> {
    if pending.len() == pending.capacity() {
        pending
            .try_reserve(pending.len().max(64))
            .map_err(|_| OutOfMemory)?;
    }
    pending.push(at);
    Ok(())
}

/// Lay `layer` over every pixel of `region` of `canvas`.
///
/// # Errors
///
/// [`OutOfMemory`] when a tile cannot be copied for writing; the stroke so
/// far is handed back undone.
pub fn fill(canvas: &mut Canvas, region: &Region, layer: Layer) -> Result<Stroke, OutOfMemory> {
    let mut stroke = Stroke::new(layer, None);
    let outcome = stroke.cover_rows(canvas, 0, region.bounds, |y, x, out| {
        let (Ok(y), Ok(x)) = (u32::try_from(y), u32::try_from(x)) else {
            return;
        };
        for (column, cover) in (x..).zip(out.iter_mut()) {
            if region.contains(column, y) {
                *cover = 255;
            }
        }
    });
    match outcome {
        Ok(()) => Ok(stroke),
        Err(err) => {
            let _ = stroke.revert(canvas);
            Err(err)
        }
    }
}

#[cfg(test)]
#[path = "fill_tests.rs"]
mod tests;
