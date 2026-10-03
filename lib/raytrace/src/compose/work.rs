//! The work a scene takes once it is set out, done a bounded unit at a time:
//! each unit small enough that a caller answering a frame can stop after it.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_parallel::JobRunner;
use tairix_util::fallible;

use crate::grass::Lawn;
use crate::heightfield::{apart, Heightfield, Sealing};
use crate::terrain::Sea;
use crate::vector::{real, share, single};

/// What fills a grid.
///
/// A sea's swells make it far the largest; a scene holds a handful of fills
/// at most, and boxing it would take an allocation that cannot fail
/// gracefully.
#[allow(
    clippy::large_enum_variant,
    reason = "a scene holds a handful of fills, and a box could not fail gracefully"
)]
#[derive(Debug)]
pub(crate) enum Form {
    Sea(Sea),
    /// How high a sward's shoots stand over the ground it grows on, a vertex
    /// to each block of `block` by `block` of its cells.
    Canopy {
        lawn: Lawn,
        block: u32,
    },
}

/// A height grid still being filled.
#[derive(Debug)]
pub(crate) struct Fill {
    /// The index of the scene's height grid it fills.
    pub(crate) field: usize,
    pub(crate) form: Form,
    /// The first row not yet filled.
    pub(crate) row: usize,
    /// A height grid's sealing, once every row is filled.
    pub(crate) sealing: Sealing,
}

/// How much of a height grid's fill its sealing is, as a share: a seal's
/// pass over a cell is a few comparisons where a fill's is noise or waves.
const SEALING_SHARE: f64 = 0.05;

/// About how many vertices one core fills in a unit of work: well under a
/// millisecond of noise on a desktop core.
const UNIT_VERTICES: usize = 8192;

/// How few vertices a band of a grid should hold to be worth another core.
const FILL_GRAIN: usize = 2048;

impl Fill {
    /// A fill of the scene's height grid `field` from `form`, from its first
    /// row.
    pub(crate) fn new(field: usize, form: Form) -> Self {
        Self {
            field,
            form,
            row: 0,
            sealing: Sealing::BEGUN,
        }
    }

    /// How far the fill of `field`, the grid it fills, has come.
    pub(crate) fn done(&self, field: &Heightfield) -> f64 {
        let filled = share(self.row, field.side());
        (1.0 - SEALING_SHARE) * filled + SEALING_SHARE * self.sealing.done(field)
    }

    /// Fill the next unit of rows of its grid among `fields`, spread over
    /// `runner`, then seal it a unit at a time once its last row is filled:
    /// whether all of it is done, or `None` when the grids it names are not
    /// there to fill and read.
    pub(crate) fn step(
        &mut self,
        fields: &mut [Heightfield],
        runner: &dyn JobRunner,
    ) -> Option<bool> {
        let Self {
            field: index,
            form,
            row,
            sealing,
        } = self;
        let field = fields.get_mut(*index)?;
        if *row >= field.side() {
            return Some(sealing.step(field, runner));
        }
        match &*form {
            Form::Canopy { lawn, block } => {
                let (field, ground) = apart(fields, *index, lawn.field as usize)?;
                canopy(field, row, runner, &|x, z| {
                    lawn.canopy_at(ground, (x, z), *block)
                });
            }
            Form::Sea(sea) => {
                advance(field, row, runner, &|x, z| sea.height(x, z));
            }
        }
        Some(false)
    }
}

/// Fill the unit of `grid`'s rows from `row`.
fn advance(
    grid: &mut Heightfield,
    row: &mut usize,
    runner: &dyn JobRunner,
    value: &(dyn Fn(f64, f64) -> f64 + Sync),
) {
    let side = grid.side();
    let unit = (UNIT_VERTICES / side.max(1)).max(1) * runner.width().max(1);
    let rows = *row..(*row + unit).min(side);
    *row = rows.end;
    fill_grid(grid, rows, runner, value);
}

/// Fill the unit of the canopy grid `tops`'s rows from `row` with `value`
/// at each vertex, its height and what the vertex keeps.
fn canopy(
    tops: &mut Heightfield,
    row: &mut usize,
    runner: &dyn JobRunner,
    value: &(dyn Fn(f64, f64) -> (f64, [u8; 4]) + Sync),
) {
    let side = tops.side().max(1);
    let unit = (UNIT_VERTICES / side).max(1) * runner.width().max(1);
    let rows = *row..(*row + unit).min(side);
    *row = rows.end;
    let ((origin_x, origin_z), step) = tops.placing();
    tops.each_row(rows, runner, &|(at, heights, kept)| {
        let z = origin_z + step * real(*at);
        for (column, height) in heights.iter_mut().enumerate() {
            let (top, packed) = value(origin_x + step * real(column), z);
            *height = single(top);
            if let Some(slot) = kept.get_mut(column) {
                *slot = packed;
            }
        }
    });
}

/// Fill `rows` of `grid` with `value` at each vertex, the rows spread over
/// `runner` in bands; on the calling thread alone when the heap will not
/// hold the list of bands.
fn fill_grid(
    grid: &mut Heightfield,
    rows: Range<usize>,
    runner: &dyn JobRunner,
    value: &(dyn Fn(f64, f64) -> f64 + Sync),
) {
    let side = grid.side().max(1);
    let ((origin_x, origin_z), step) = grid.placing();
    let pieces = tairix_parallel::bands(runner, rows.len(), FILL_GRAIN.div_ceil(side));
    let per = rows.len().div_ceil(pieces.max(1)).max(1);
    let fill_band = |(start, cells): &mut (usize, &mut [f32])| {
        for (offset, row) in cells.chunks_mut(side).enumerate() {
            let z = origin_z + step * real(*start + offset);
            for (column, cell) in row.iter_mut().enumerate() {
                *cell = single(value(origin_x + step * real(column), z));
            }
        }
    };
    let mut bands = Vec::new();
    if pieces > 1 && fallible::reserve(&mut bands, pieces) {
        bands.extend(grid.bands(rows, per));
        tairix_parallel::for_each(runner, &mut bands, &fill_band);
    } else {
        for mut band in grid.bands(rows, per) {
            fill_band(&mut band);
        }
    }
}

#[cfg(test)]
#[path = "work_tests.rs"]
mod tests;
