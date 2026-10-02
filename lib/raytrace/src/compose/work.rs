//! The work a scene takes once it is set out, done a bounded unit at a time:
//! each unit small enough that a caller answering a frame can stop after it.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_parallel::JobRunner;
use tairix_util::fallible;

use crate::grass::Lawn;
use crate::heightfield::{apart, Heightfield, Sealing};
use crate::scene::Grid;
use crate::sky::Clouds;
use crate::terrain::{Cloudscape, Sea};
use crate::vector::share;

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
    Clouds(Cloudscape),
    /// How high a sward's shoots stand over the ground it grows on, a vertex
    /// to each block of `block` by `block` of its cells.
    Canopy {
        lawn: Lawn,
        block: u32,
    },
}

/// Which grid a fill fills.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Target {
    /// The scene's height grid of this index.
    Field(usize),
    /// The sky's cloud cover.
    Clouds,
}

/// A grid still being filled.
#[derive(Debug)]
pub(crate) struct Fill {
    pub(crate) target: Target,
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

/// The grids a fill can reach.
pub(crate) struct Grids<'a> {
    pub(crate) fields: &'a mut [Heightfield],
    pub(crate) clouds: Option<&'a mut Clouds>,
}

impl Fill {
    /// A fill of `target` from `form`, from its first row.
    pub(crate) fn new(target: Target, form: Form) -> Self {
        Self {
            target,
            form,
            row: 0,
            sealing: Sealing::BEGUN,
        }
    }

    /// How far the fill has come, `rows` the rows of the grid it fills and
    /// `field` that grid if it is a height grid.
    pub(crate) fn done(&self, rows: usize, field: Option<&Heightfield>) -> f64 {
        let filled = share(self.row, rows);
        match field {
            Some(field) => {
                (1.0 - SEALING_SHARE) * filled + SEALING_SHARE * self.sealing.done(field)
            }
            None => filled,
        }
    }

    /// Fill the next unit of rows, spread over `runner`, then seal a height
    /// grid a unit at a time once its last row is filled: whether all of it
    /// is done, or `None` when the grids it names are not there to fill and
    /// read.
    pub(crate) fn step(&mut self, grids: Grids<'_>, runner: &dyn JobRunner) -> Option<bool> {
        let Self {
            target,
            form,
            row,
            sealing,
        } = self;
        let Grids { fields, clouds } = grids;
        if let Target::Field(index) = *target {
            let field = fields.get_mut(index)?;
            if *row >= field.side() {
                return Some(sealing.step(field, runner));
            }
        }
        match (*target, &*form) {
            (Target::Field(index), Form::Canopy { lawn, block }) => {
                let (field, ground) = apart(fields, index, lawn.field as usize)?;
                canopy(field, row, runner, &|x, z| {
                    lawn.canopy_at(ground, (x, z), *block)
                });
            }
            (Target::Field(index), Form::Sea(sea)) => {
                advance(fields.get_mut(index)?, row, runner, &|x, z| {
                    sea.height(x, z)
                });
            }
            // A sky with no cloud layer has none to fill.
            (Target::Clouds, Form::Clouds(cloudscape)) => {
                return Some(clouds.is_none_or(|clouds| {
                    advance(clouds, row, runner, &|x, z| cloudscape.density(x, z))
                }));
            }
            _ => return None,
        }
        Some(false)
    }
}

/// Fill the unit of `grid`'s rows from `row`; whether it is full.
fn advance<G: Grid>(
    grid: &mut G,
    row: &mut usize,
    runner: &dyn JobRunner,
    value: &(dyn Fn(f64, f64) -> f64 + Sync),
) -> bool {
    let side = grid.rows();
    let unit = (UNIT_VERTICES / side.max(1)).max(1) * runner.width().max(1);
    let rows = *row..(*row + unit).min(side);
    *row = rows.end;
    fill_grid(grid, rows, runner, value);
    *row >= side
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
        let z = origin_z + step * crate::vector::real(*at);
        for (column, height) in heights.iter_mut().enumerate() {
            let (top, packed) = value(origin_x + step * crate::vector::real(column), z);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "the grids hold single precision: heights need no more"
            )]
            let narrowed = top as f32;
            *height = narrowed;
            if let Some(slot) = kept.get_mut(column) {
                *slot = packed;
            }
        }
    });
}

/// Fill `rows` of `grid` with `value` at each vertex, the rows spread over
/// `runner` in bands; on the calling thread alone when the heap will not
/// hold the list of bands.
fn fill_grid<G: Grid>(
    grid: &mut G,
    rows: Range<usize>,
    runner: &dyn JobRunner,
    value: &(dyn Fn(f64, f64) -> f64 + Sync),
) {
    let side = grid.rows().max(1);
    let layout = grid.layout();
    let pieces = tairix_parallel::bands(runner, rows.len(), FILL_GRAIN.div_ceil(side));
    let per = rows.len().div_ceil(pieces.max(1)).max(1);
    let fill_band = |(start, cells): &mut (usize, &mut [f32])| {
        for (offset, row) in cells.chunks_mut(side).enumerate() {
            for (column, cell) in row.iter_mut().enumerate() {
                let (x, z) = layout.vertex(column, *start + offset);
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "the grids hold single precision: heights and densities need no more"
                )]
                let narrowed = value(x, z) as f32;
                *cell = narrowed;
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
