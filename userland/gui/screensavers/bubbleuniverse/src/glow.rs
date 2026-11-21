//! The glow the light casts about itself: a soft halo close in, and a wide
//! haze around the bubble.
//!
//! Every point's light is gathered at a coarse grid of cells a few pixels
//! across, softened so a crowd glows more than a lone point but never as all
//! of its points would, and spread by the near-Gaussian of box blurs
//! `tairix_raster::soften16` makes: over the cells themselves for the halo,
//! and over cells [`HAZE_CELLS`] times wider for the haze. Their sum is read
//! back at each pixel between the cells about it and added to its light
//! before it is toned, so the glow costs a small fraction of the pixels it
//! covers.
//!
//! The grids hold opaque sixteen-bit pixels whose channels count light:
//! [`RANGE`] light at the most an opaque channel holds.

use alloc::vec::Vec;

use tairix_parallel::{for_each_drawn, piece_len, JobRunner};
use tairix_raster::{bilinear_weights, soften16, Pixel16, SOFTEN_PASSES};
use tairix_util::fallible;

use crate::light::{pixel_centre, Shed};
use crate::tone::LIGHT_ONE;

/// The light a full channel of a glow grid counts.
const RANGE: u64 = 4;

/// The halo's box radius, in cells.
const HALO_RADIUS: usize = 1;

/// How many cells each way one cell of the haze gathers, and its box radius
/// in its own cells.
const HAZE_CELLS: usize = 4;
const HAZE_RADIUS: usize = 2;

/// How much of the gathered light the halo and the haze give back, in 256ths.
const HALO_GAIN: u64 = 128;
const HAZE_GAIN: u64 = 192;

/// The light a cell gathers, in grid channels, past which more glows ever
/// less: a curve that settles on one point stacks all its points there, and
/// would otherwise glow as two hundred.
const SOFTEN: u32 = 1500;

/// How far past the light it gathers the glow reaches, in cells: three of the
/// haze's deviations, beyond which a hundredth of it is left. Its passes of
/// boxes `HAZE_RADIUS` each way spread as a Gaussian of variance
/// `passes · r(r + 1) / 3` haze cells squared.
pub(crate) const GLOW_CELLS: usize = {
    let passes = SOFTEN_PASSES as usize;
    let reach = 3 * passes * HAZE_RADIUS * (HAZE_RADIUS + 1) * HAZE_CELLS * HAZE_CELLS;
    let root = reach.isqrt();
    if root * root == reach {
        root
    } else {
        root + 1
    }
};

/// Where a position lies between the centres of two neighbouring cells: the
/// nearer one's index, and the further's weight in 256ths.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Between {
    cell: usize,
    weight: u32,
}

impl Between {
    /// The nearer of the two cells this names.
    pub(crate) const fn cell(self) -> usize {
        self.cell
    }

    /// The further cell's weight, in 256ths.
    pub(crate) const fn weight(self) -> u32 {
        self.weight
    }

    /// The light between the two cells of `cells` this names.
    #[inline]
    fn sample(self, cells: &[[u16; 3]]) -> [u16; 3] {
        match (cells.get(self.cell), cells.get(self.cell + 1)) {
            (Some(near), Some(far)) => core::array::from_fn(|c| {
                let light = lerp(u32::from(near[c]), u32::from(far[c]), self.weight);
                u16::try_from(light).unwrap_or(u16::MAX)
            }),
            _ => [0; 3],
        }
    }
}

/// The cells of one axis of a grid: how many, and the reciprocal of their
/// width.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Axis {
    cells: usize,
    /// `2³² / width` rounded up: a position times this, shifted down 32, is
    /// its place in cells, exactly for any position a screen holds.
    reciprocal: u64,
}

impl Axis {
    fn new(pixels: u32, width: u32) -> Option<Self> {
        Some(Self {
            cells: usize::try_from(pixels.div_ceil(width)).ok()?.max(2),
            reciprocal: (1u64 << 32).div_ceil(u64::from(width)),
        })
    }

    /// Where `at`, in 256ths of a pixel from the axis's start, lies between
    /// its cells.
    #[inline]
    fn between(self, at: u32) -> Between {
        let in_cells = (u64::from(at) * self.reciprocal) >> 32;
        clamped(u32::try_from(in_cells).unwrap_or(u32::MAX), self.cells)
    }
}

/// The rows of cells beyond a band of the halo its blur reads: as far as the
/// passes together carry a cell, so a band blurred alone is the halo blurred
/// whole.
const APRON: usize = HALO_RADIUS * SOFTEN_PASSES as usize;

/// The fewest rows of cells worth blurring as a band of their own.
const BAND_ROWS: usize = 4 * APRON;

/// Where one band of the halo is blurred: its rows and their aprons.
struct Block {
    cells: Vec<Pixel16>,
    aux: Vec<Pixel16>,
}

/// The glow over one frame.
pub(crate) struct Glow {
    across: Axis,
    down: Axis,
    /// What the spots left, before it is softened and spread.
    gathered: Vec<Pixel16>,
    /// The gathered light spread into the halo.
    halo: Vec<Pixel16>,
    /// The rows of cells each band of the halo holds, and the blocks they
    /// are blurred in, one for each band.
    band_rows: usize,
    blocks: Vec<Block>,
    haze_size: (usize, usize),
    haze: Vec<Pixel16>,
    haze_aux: Vec<Pixel16>,
    /// What one light of a spot leaves across the four cells about it, in
    /// grid channels with sixteen fraction bits: its dot's energy over a
    /// cell's area.
    deposit: u64,
    /// The bits a spot's light is held to past light's own.
    shift: u32,
    /// For each pixel column of the frame, the cells it lies between.
    columns: Vec<Between>,
    /// For each column of cells, the haze's cells it lies between.
    haze_columns: Vec<Between>,
}

impl Glow {
    /// The glow over a `size` frame whose cells span `cell` pixels, for spots
    /// each shedding `energy` times its light in all, held as `shed` says,
    /// spread in as many bands as `runner` takes; `None` when the heap will
    /// not hold its grids.
    pub(crate) fn new(
        size: (u32, u32),
        cell: u32,
        (energy, shed): (u64, Shed),
        runner: &dyn JobRunner,
    ) -> Option<Self> {
        let cell = cell.max(1);
        let (across, down) = (Axis::new(size.0, cell)?, Axis::new(size.1, cell)?);
        let haze_size = (
            across.cells.div_ceil(HAZE_CELLS).max(2),
            down.cells.div_ceil(HAZE_CELLS).max(2),
        );
        let cells = across.cells.checked_mul(down.cells)?;
        let haze_cells = haze_size.0.checked_mul(haze_size.1)?;
        let columns = fallible::collected(
            usize::try_from(size.0).ok()?,
            (0..size.0).map(|x| across.between(pixel_centre(x))),
        )?;
        let haze_columns = fallible::collected(
            across.cells,
            (0..across.cells).map(|column| clamped(centre_of(column, HAZE_CELLS), haze_size.0)),
        )?;
        let bands = tairix_parallel::bands(runner, down.cells, BAND_ROWS).max(1);
        let band_rows = piece_len(down.cells, bands);
        let block = across.cells.checked_mul(band_rows + 2 * APRON)?;
        let mut blocks = Vec::new();
        if !fallible::reserve(&mut blocks, bands) {
            return None;
        }
        for _ in 0..bands {
            blocks.push(Block {
                cells: fallible::filled(block, Pixel16::BLACK)?,
                aux: fallible::filled(block, Pixel16::BLACK)?,
            });
        }
        let area = u64::from(cell) * u64::from(cell) * RANGE * u64::from(LIGHT_ONE);
        Some(Self {
            across,
            down,
            gathered: fallible::filled(cells, Pixel16::BLACK)?,
            halo: fallible::filled(cells, Pixel16::BLACK)?,
            band_rows,
            blocks,
            haze_size,
            haze: fallible::filled(haze_cells, Pixel16::BLACK)?,
            haze_aux: fallible::filled(haze_cells, Pixel16::BLACK)?,
            deposit: ((energy * u64::from(Pixel16::FULL)) << 16) / area,
            shift: shed.shift(),
            columns,
            haze_columns,
        })
    }

    /// Clear what the last frame gathered.
    pub(crate) fn clear(&mut self) {
        self.gathered.fill(Pixel16::BLACK);
    }

    /// Gather `light` at `x` and `y`, 256ths of a pixel inside the frame.
    #[inline]
    pub(crate) fn gather(&mut self, (x, y): (u32, u32), light: [u16; 3]) {
        let (left, top) = (self.across.between(x), self.down.between(y));
        // Brought back once from the bits the spot's light is held to past
        // light's own, rather than once a cell.
        let carried = light.map(|light| (u64::from(light) * self.deposit) >> self.shift);
        let quarters = [(0, 0), (1, 0), (0, 1), (1, 1)];
        for ((dx, dy), weight) in quarters
            .into_iter()
            .zip(bilinear_weights(left.weight, top.weight))
        {
            let index = (top.cell + dy) * self.across.cells + left.cell + dx;
            if let Some(cell) = self.gathered.get_mut(index) {
                let add = |channel: u16, carried: u64| {
                    let share = (carried * u64::from(weight)) >> 32;
                    u16::try_from((u64::from(channel) + share).min(u64::from(Pixel16::FULL)))
                        .unwrap_or(Pixel16::FULL)
                };
                cell.r = add(cell.r, carried[0]);
                cell.g = add(cell.g, carried[1]);
                cell.b = add(cell.b, carried[2]);
            }
        }
    }

    /// Soften what was gathered and spread it into the halo, a band of rows
    /// at a time across `runner`, and the halo into the haze.
    pub(crate) fn spread(&mut self, runner: &dyn JobRunner) {
        let (across, down) = (self.across.cells, self.down.cells);
        let (haze_across, haze_down) = self.haze_size;
        let (gathered, band_rows) = (&self.gathered, self.band_rows);
        let bands = self
            .halo
            .chunks_mut((band_rows * across).max(1))
            .zip(self.blocks.iter_mut())
            .enumerate()
            .map(|(band, (halo, block))| (band * band_rows, halo, block));
        for_each_drawn(runner, bands, &|(first, halo, block)| {
            let rows = halo.len() / across.max(1);
            let (from, to) = (
                first.saturating_sub(APRON),
                (first + rows + APRON).min(down),
            );
            let held = (to - from) * across;
            let (Some(cells), Some(aux), Some(source)) = (
                block.cells.get_mut(..held),
                block.aux.get_mut(..held),
                gathered.get(from * across..to * across),
            ) else {
                return;
            };
            // Softened as each band takes its rows, so it runs on every core;
            // an apron's cells are softened again by the band they belong to.
            for (cell, &gathered) in cells.iter_mut().zip(source) {
                *cell = softened(gathered);
            }
            soften16(cells, across, to - from, HALO_RADIUS, aux);
            let own = (first - from) * across;
            if let Some(blurred) = cells.get(own..own + halo.len()) {
                halo.copy_from_slice(blurred);
            }
        });
        // Gathered from the halo once it is spread, so no block of the haze
        // takes a light alone that its neighbour would have shared.
        gather_haze(&self.halo, across, &mut self.haze, haze_across);
        soften16(
            &mut self.haze,
            haze_across,
            haze_down,
            HAZE_RADIUS,
            &mut self.haze_aux,
        );
    }

    /// Where pixel row `row` of the frame lies between the rows of cells.
    pub(crate) fn rows_about(&self, row: u32) -> Between {
        self.down.between(pixel_centre(row))
    }

    /// Row `row` of cells, the halo and the haze summed as light, read out
    /// into `out` at each pixel column of the frame from `first` on, by way
    /// of `cells`, which holds [`Self::across`] of them.
    pub(crate) fn row(
        &self,
        row: usize,
        first: usize,
        cells: &mut [[u16; 3]],
        out: &mut [[u16; 3]],
    ) {
        let across = self.across.cells;
        let (haze_across, haze_down) = self.haze_size;
        let haze_row = clamped(centre_of(row, HAZE_CELLS), haze_down);
        let Some(halo) = self.halo.get(row * across..(row + 1) * across) else {
            out.fill([0; 3]);
            return;
        };
        for ((light, halo), &haze_column) in cells.iter_mut().zip(halo).zip(&self.haze_columns) {
            let haze = haze_at(&self.haze, haze_across, haze_column, haze_row);
            let glow = |halo: u16, haze: u32| {
                let sum = HALO_GAIN * u64::from(halo) + HAZE_GAIN * u64::from(haze);
                let light = sum * RANGE * u64::from(LIGHT_ONE) / (256 * u64::from(Pixel16::FULL));
                u16::try_from(light).unwrap_or(u16::MAX)
            };
            *light = [
                glow(halo.r, haze[0]),
                glow(halo.g, haze[1]),
                glow(halo.b, haze[2]),
            ];
        }
        let columns = self.columns.get(first..).unwrap_or(&[]);
        for (out, column) in out.iter_mut().zip(columns) {
            *out = column.sample(cells);
        }
    }

    /// The cells across a row.
    pub(crate) const fn across(&self) -> usize {
        self.across.cells
    }
}

/// Average each block of [`HAZE_CELLS`] cells each way of `halo`, `across`
/// cells wide, into one cell of `haze`, `haze_across` wide: a block the
/// halo's edge cuts short is the mean of the cells it holds.
fn gather_haze(halo: &[Pixel16], across: usize, haze: &mut [Pixel16], haze_across: usize) {
    for (haze_row, haze) in haze.chunks_exact_mut(haze_across).enumerate() {
        for (haze_column, out) in haze.iter_mut().enumerate() {
            let (mut sum, mut count) = ([0u32; 3], 0u32);
            let rows = halo
                .chunks_exact(across)
                .skip(haze_row * HAZE_CELLS)
                .take(HAZE_CELLS);
            for row in rows {
                for cell in row.iter().skip(haze_column * HAZE_CELLS).take(HAZE_CELLS) {
                    sum[0] += u32::from(cell.r);
                    sum[1] += u32::from(cell.g);
                    sum[2] += u32::from(cell.b);
                    count += 1;
                }
            }
            let mean = |sum: u32| u16::try_from(sum / count.max(1)).unwrap_or(Pixel16::FULL);
            *out = Pixel16 {
                r: mean(sum[0]),
                g: mean(sum[1]),
                b: mean(sum[2]),
                a: Pixel16::FULL,
            };
        }
    }
}

/// The haze `across` cells wide between the cells `column` and `row` name.
fn haze_at(haze: &[Pixel16], across: usize, column: Between, row: Between) -> [u32; 3] {
    let cell = |x: usize, y: usize| haze.get(y * across + x).copied().unwrap_or(Pixel16::BLACK);
    let (a, b) = (cell(column.cell, row.cell), cell(column.cell + 1, row.cell));
    let (c, d) = (
        cell(column.cell, row.cell + 1),
        cell(column.cell + 1, row.cell + 1),
    );
    let mix = |a: u16, b: u16, c: u16, d: u16| {
        let upper = lerp(u32::from(a), u32::from(b), column.weight);
        let lower = lerp(u32::from(c), u32::from(d), column.weight);
        lerp(upper, lower, row.weight)
    };
    [
        mix(a.r, b.r, c.r, d.r),
        mix(a.g, b.g, c.g, d.g),
        mix(a.b, b.b, c.b, d.b),
    ]
}

/// `light` gathered, as it glows: as itself while faint, never past
/// [`SOFTEN`] however bright.
fn soften(light: u16) -> u16 {
    let light = u32::from(light);
    u16::try_from(light * SOFTEN / (light + SOFTEN)).unwrap_or(Pixel16::FULL)
}

/// `cell` gathered, each channel as it glows.
fn softened(cell: Pixel16) -> Pixel16 {
    Pixel16 {
        r: soften(cell.r),
        g: soften(cell.g),
        b: soften(cell.b),
        a: cell.a,
    }
}

/// The centre of cell `index`, in 256ths of a cell `per` times as wide.
fn centre_of(index: usize, per: usize) -> u32 {
    let (index, per) = (
        u32::try_from(index).unwrap_or(u32::MAX),
        u32::try_from(per).unwrap_or(1).max(1),
    );
    index.saturating_mul(256).saturating_add(128) / per
}

/// The two cells of an axis of `cells` either side of `at`, in 256ths of a
/// cell from the axis's start; a position before the first centre or past the
/// last is held at it.
fn clamped(at: u32, cells: usize) -> Between {
    let centred = at.saturating_sub(128);
    let last = cells.saturating_sub(2);
    match usize::try_from(centred >> 8) {
        Ok(cell) if cell <= last => Between {
            cell,
            weight: centred & 255,
        },
        _ => Between {
            cell: last,
            weight: 256,
        },
    }
}

/// `from` carried `weight` 256ths of the way to `to`.
#[inline]
pub(crate) fn lerp(from: u32, to: u32, weight: u32) -> u32 {
    (from * (256 - weight) + to * weight + 128) >> 8
}

#[cfg(test)]
#[path = "glow_tests.rs"]
mod tests;
