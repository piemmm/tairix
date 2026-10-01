//! The work a scene takes once it is set out, done a bounded unit at a time:
//! each unit small enough that a caller answering a frame can stop after it.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_parallel::JobRunner;
use tairix_util::fallible;

use crate::grass::Lawn;
use crate::heightfield::Heightfield;
use crate::scene::Grid;
use crate::sky::Clouds;
use crate::terrain::{Cloudscape, Sea};

/// What fills a grid.
///
/// A sea's swells make it far the largest; a scene holds a handful of fills
/// at most, and boxing it would take an allocation that cannot fail
/// gracefully.
#[allow(
    clippy::large_enum_variant,
    reason = "a scene holds a handful of fills, and a box could not fail gracefully"
)]
#[derive(Clone, Debug)]
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
#[derive(Clone, Debug)]
pub(crate) struct Fill {
    pub(crate) target: Target,
    pub(crate) form: Form,
    /// The first row not yet filled.
    pub(crate) row: usize,
}

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
    /// Fill the next unit of rows, spread over `runner`, sealing the grid
    /// once its last row is filled; whether it is.
    pub(crate) fn step(&mut self, grids: Grids<'_>, runner: &dyn JobRunner) -> bool {
        let Self { target, form, row } = self;
        let Grids { fields, clouds } = grids;
        let filled = match (*target, &*form) {
            (Target::Field(index), Form::Canopy { lawn, block }) => {
                let Some((field, ground)) = apart(fields, index, lawn.field as usize) else {
                    return true;
                };
                canopy(field, row, runner, &|x, z| {
                    lawn.canopy_at(ground, (x, z), *block)
                })
            }
            (Target::Field(index), Form::Sea(sea)) => match fields.get_mut(index) {
                Some(field) => advance(field, row, runner, &|x, z| sea.height(x, z)),
                None => return true,
            },
            (Target::Clouds, Form::Clouds(cloudscape)) => {
                return clouds.is_none_or(|clouds| {
                    advance(clouds, row, runner, &|x, z| cloudscape.density(x, z))
                });
            }
            _ => return true,
        };
        if filled {
            if let Target::Field(index) = *target {
                if let Some(field) = fields.get_mut(index) {
                    field.seal();
                }
            }
        }
        filled
    }
}

/// The grid `filled` to fill, and the grid `read` it is filled from; `None`
/// unless both are there and apart.
fn apart(
    fields: &mut [Heightfield],
    filled: usize,
    read: usize,
) -> Option<(&mut Heightfield, &Heightfield)> {
    if filled == read {
        return None;
    }
    let (low, high) = fields.split_at_mut(filled.max(read));
    let (first, second) = (low.get_mut(filled.min(read))?, high.first_mut()?);
    Some(if filled < read {
        (first, &*second)
    } else {
        (second, &*first)
    })
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

/// A row of the canopy grid: its number, its heights, and what its vertices
/// keep.
type Row<'a> = (usize, &'a mut [f32], &'a mut [[u8; 4]]);

/// Fill the unit of the canopy grid `tops`'s rows from `row` with `value`
/// at each vertex, its height and what the vertex keeps; whether it is full.
fn canopy(
    tops: &mut Heightfield,
    row: &mut usize,
    runner: &dyn JobRunner,
    value: &(dyn Fn(f64, f64) -> (f64, [u8; 4]) + Sync),
) -> bool {
    let side = tops.side().max(1);
    let unit = (UNIT_VERTICES / side).max(1) * runner.width().max(1);
    let rows = *row..(*row + unit).min(side);
    *row = rows.end;
    let ((origin_x, origin_z), step) = tops.placing();
    let (heights, kept) = tops.surfaces_mut();
    let heights = heights
        .get_mut(rows.start * side..rows.end * side)
        .unwrap_or_default();
    let kept = kept
        .get_mut(rows.start * side..rows.end * side)
        .unwrap_or_default();
    let fill_row = |(at, heights, kept): &mut Row<'_>| {
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
    };
    let mut bands: Vec<Row<'_>> = Vec::new();
    if fallible::reserve(&mut bands, rows.len()) {
        bands.extend(
            (rows.start..)
                .zip(heights.chunks_mut(side))
                .zip(kept.chunks_mut(side))
                .map(|((at, heights), kept)| (at, heights, kept)),
        );
        tairix_parallel::for_each(runner, &mut bands, &fill_row);
    } else {
        for band in (rows.start..)
            .zip(heights.chunks_mut(side))
            .zip(kept.chunks_mut(side))
        {
            let ((at, heights), kept) = band;
            fill_row(&mut (at, heights, kept));
        }
    }
    *row >= side
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
