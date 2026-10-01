//! Hillslope processes: soil creeping downhill, and ground too steep to
//! stand slumping to its angle of repose.
//!
//! Both read a snapshot of the grid and write the next, so no sample's
//! answer depends on the order the samples are visited in, and a pass split
//! into bands of rows gives exactly what one whole pass would.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_util::mathf;

use crate::grid::{Grid, NEIGHBOURS};
use crate::{filled, fits, TerrainError};

/// Linear diffusion: how far each sample moves toward the mean of its four
/// straight neighbours, and the base level it never leaves or falls below.
#[derive(Copy, Clone, Debug)]
pub struct Diffusion {
    /// The share of the gap to the mean closed per pass: at most one, each
    /// pass is a weighted average and cannot overshoot.
    pub rate: f64,
    /// Samples at or below this are left alone, and none is lowered past it.
    pub floor: f64,
}

/// One pass of `law` over `height`.
pub fn diffuse(height: &mut [f64], grid: Grid, law: Diffusion) -> Result<(), TerrainError> {
    let before = snapshot(height)?;
    diffuse_rows(&before, height, grid, law, 0..grid.side())
}

/// One pass of `law` over rows `rows` of `height`, read from `before`, the
/// grid as the pass began.
pub fn diffuse_rows(
    before: &[f64],
    height: &mut [f64],
    grid: Grid,
    law: Diffusion,
    rows: Range<u32>,
) -> Result<(), TerrainError> {
    fits(before, grid)?;
    fits(height, grid)?;
    for y in rows.start..rows.end.min(grid.side()) {
        for x in 0..grid.side() {
            let index = grid.index(x, y);
            if before[index] <= law.floor {
                continue;
            }
            let mut total = 0.0;
            let mut count = 0.0;
            for (_, dx, dy, distance) in NEIGHBOURS {
                if distance > 1.0 {
                    continue;
                }
                let Some(next) = grid.neighbour(x, y, dx, dy) else {
                    continue;
                };
                total += before[next];
                count += 1.0;
            }
            if count == 0.0 {
                continue;
            }
            let mean = total / count;
            height[index] =
                mathf::fmax(before[index] + law.rate * (mean - before[index]), law.floor);
        }
    }
    Ok(())
}

/// A copy of `height`, the snapshot a pass reads.
pub fn snapshot(height: &[f64]) -> Result<Vec<f64>, TerrainError> {
    let mut copy = filled(height.len(), 0.0_f64)?;
    copy.copy_from_slice(height);
    Ok(copy)
}

/// Talus: the steepest ground stands, and how much of what stands steeper
/// slumps each pass.
#[derive(Copy, Clone, Debug)]
pub struct Talus {
    /// The most a sample may stand above a straight neighbour and stay put:
    /// the tangent of the angle of repose times the samples' spacing.
    pub drop: f64,
    /// The share of a sample's steepest excess that moves per pass, split
    /// among its lower neighbours by their own excess; at most a half, so no
    /// sample is lowered past a neighbour it sheds onto.
    pub rate: f64,
}

/// What one sample sheds in a talus pass: how much, and over how much excess
/// it is shared out.
#[derive(Copy, Clone, Debug, Default)]
pub struct Shed {
    amount: f64,
    excess: f64,
}

/// How far `before[next]` lies below `before[index]` past what `law` lets
/// stand, a diagonal step allowed its longer run.
fn excess(before: &[f64], (index, next): (usize, usize), distance: f64, law: Talus) -> f64 {
    before[index] - before[next] - law.drop * distance
}

/// The first half of a talus pass over rows `rows`: what each sample of
/// `before` sheds, into `sheds`.
pub fn slump_measure(
    before: &[f64],
    sheds: &mut [Shed],
    grid: Grid,
    law: Talus,
    rows: Range<u32>,
) -> Result<(), TerrainError> {
    fits(before, grid)?;
    fits(sheds, grid)?;
    let rate = law.rate.clamp(0.0, 0.5);
    for y in rows.start..rows.end.min(grid.side()) {
        for x in 0..grid.side() {
            let index = grid.index(x, y);
            let (mut total, mut steepest) = (0.0, 0.0_f64);
            for (_, dx, dy, distance) in NEIGHBOURS {
                let Some(next) = grid.neighbour(x, y, dx, dy) else {
                    continue;
                };
                let over = excess(before, (index, next), distance, law);
                if over > 0.0 {
                    total += over;
                    steepest = steepest.max(over);
                }
            }
            sheds[index] = Shed {
                amount: rate * steepest,
                excess: total,
            };
        }
    }
    Ok(())
}

/// The second half of a talus pass over rows `rows`: each sample of `height`
/// loses what it sheds and gains its share of what its higher neighbours do.
pub fn slump_settle(
    before: &[f64],
    sheds: &[Shed],
    height: &mut [f64],
    grid: Grid,
    law: Talus,
    rows: Range<u32>,
) -> Result<(), TerrainError> {
    fits(before, grid)?;
    fits(sheds, grid)?;
    fits(height, grid)?;
    for y in rows.start..rows.end.min(grid.side()) {
        for x in 0..grid.side() {
            let index = grid.index(x, y);
            let mut settled = before[index] - sheds[index].amount;
            for (_, dx, dy, distance) in NEIGHBOURS {
                let Some(next) = grid.neighbour(x, y, dx, dy) else {
                    continue;
                };
                let over = excess(before, (next, index), distance, law);
                let from = sheds[next];
                if over > 0.0 && from.excess > 0.0 {
                    settled += from.amount * over / from.excess;
                }
            }
            height[index] = settled;
        }
    }
    Ok(())
}

/// One whole talus pass of `law` over `height`.
pub fn slump(height: &mut [f64], grid: Grid, law: Talus) -> Result<(), TerrainError> {
    let before = snapshot(height)?;
    let mut sheds = filled(height.len(), Shed::default())?;
    slump_measure(&before, &mut sheds, grid, law, 0..grid.side())?;
    slump_settle(&before, &sheds, height, grid, law, 0..grid.side())
}

#[cfg(test)]
#[path = "hillslope_tests.rs"]
mod tests;
