//! What a reveal shows while it is under way: each traced point refines a
//! bilinear picture over its pass's grid, so a coarse picture is soft rather
//! than blocky, and every pixel shows its own trace once the reveal is whole.
//!
//! A step changes only the cells of its grid it is a corner of, and inside
//! those only a cell's top-left corner can already be traced — which the
//! interpolation gives back exactly — so a finer pass never alters a traced
//! pixel (`tairix_raytrace::Reveal`). The corners are read from the picture as
//! it stands, the grid's untraced points standing there at the value the grid
//! twice as coarse gives them, so the window's buffer is all that holds the
//! picture. The first pass alone has no coarser grid beneath it: which of its
//! points are traced is kept, and the rest count as black.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_parallel::JobRunner;
use tairix_raster::{Pixel, RowBand};
use tairix_raytrace::{Reveal, Step};
use tairix_util::fallible;
use tairix_wm::{Rect, Region, Surface};

use super::engine::Traced;

/// The fewest pixels worth handing another core to repaint: a cell's pixel
/// costs a few multiplies, so a hand-off must carry many.
const PAINT_GRAIN: usize = 16_384;

/// The most cells a frame's damage lists one by one, rather than as the box
/// they span: a few are repainted alone, while thousands, scattered as a pass
/// is, span the screen anyway, and merging each into a region would cost more
/// than it saves.
const DAMAGE_BUDGET: usize = 256;

/// The colour an untraced point of the first pass stands at.
const BLACK: Pixel = Pixel {
    r: 0,
    g: 0,
    b: 0,
    a: u8::MAX,
};

/// One cell of a pass's grid to repaint: its top-left corner, its side, and
/// its corners' colours once they are read.
#[derive(Copy, Clone, Debug)]
struct Cell {
    x: u32,
    y: u32,
    side: u32,
    /// Top-left, top-right, bottom-left, bottom-right.
    corners: [Pixel; 4],
}

/// The painter of one reveal on one screen.
pub(super) struct Preview {
    size: (u32, u32),
    /// The first pass's grid spacing, its points across, and which of them
    /// are traced, row by row.
    coarsest: u32,
    columns: u32,
    traced: Vec<bool>,
    /// The cells a frame repaints, pass by pass, and where each pass's run
    /// of steps and of cells lies; kept from frame to frame for their room.
    cells: Vec<Cell>,
    runs: Vec<(Range<usize>, Range<usize>)>,
    /// Whether this frame's cells could not be laid out, so its steps are
    /// painted one at a time instead.
    unplanned: bool,
}

impl Preview {
    /// A painter for a `size` screen; `None` when the heap will not hold the
    /// first pass's record.
    pub(super) fn new(size: (u32, u32)) -> Option<Self> {
        let coarsest = Reveal::coarsest(size);
        let columns = size.0.div_ceil(coarsest);
        let points =
            usize::try_from(u64::from(columns) * u64::from(size.1.div_ceil(coarsest))).ok()?;
        Some(Self {
            size,
            coarsest,
            columns,
            traced: fallible::filled(points, false)?,
            cells: Vec::new(),
            runs: Vec::new(),
            unplanned: false,
        })
    }

    /// Forget every traced point: the reveal begins again over black.
    pub(super) fn reset(&mut self) {
        self.traced.fill(false);
    }

    /// Lay out the cells `steps` change, pass by pass, each once, and add
    /// them to `damage` — or the box they span once they are many.
    pub(super) fn plan(&mut self, steps: &[Traced], damage: &mut Region) {
        self.cells.clear();
        self.runs.clear();
        self.unplanned = !self.lay_out(steps);
        if self.unplanned {
            let span = steps
                .iter()
                .filter_map(|traced| self.reach(traced.step))
                .fold(Rect::EMPTY, |span, rect| span.union(&rect));
            damage.add(span);
            return;
        }
        if self.cells.len() <= DAMAGE_BUDGET {
            for cell in &self.cells {
                damage.add(self.rect(cell.x, cell.y, cell.side));
            }
        } else {
            let span = self.cells.iter().fold(Rect::EMPTY, |span, cell| {
                span.union(&self.rect(cell.x, cell.y, cell.side))
            });
            damage.add(span);
        }
    }

    /// Paint `steps`, which [`plan`](Self::plan) laid out, into `surface`,
    /// spreading the cells over `runner`.
    pub(super) fn paint(
        &mut self,
        surface: &mut Surface,
        steps: &[Traced],
        runner: &dyn JobRunner,
    ) {
        if self.unplanned {
            for traced in steps {
                self.paint_one(surface, traced);
            }
            return;
        }
        let runs = core::mem::take(&mut self.runs);
        for (run, cells) in &runs {
            for traced in steps.get(run.clone()).unwrap_or(&[]) {
                self.write(surface, traced);
            }
            let Some(cells) = self.cells.get_mut(cells.clone()) else {
                continue;
            };
            for cell in cells.iter_mut() {
                cell.corners = corners(
                    surface,
                    self.size,
                    &self.traced,
                    self.coarsest,
                    self.columns,
                    cell,
                );
            }
            fill_cells(surface, cells, self.size, runner);
        }
        self.runs = runs;
    }

    /// Lay out each pass's cells in `steps`, sorted and each once; `false`
    /// when the heap would not hold them.
    fn lay_out(&mut self, steps: &[Traced]) -> bool {
        let mut start = 0;
        while start < steps.len() {
            let side = steps.get(start).map_or(0, |traced| traced.step.side);
            let end = steps
                .get(start..)
                .and_then(|rest| rest.iter().position(|traced| traced.step.side != side))
                .map_or(steps.len(), |offset| start + offset);
            let first = self.cells.len();
            if side > 0 {
                // A step is a corner of at most four cells.
                if !fallible::reserve(&mut self.cells, 4 * (end - start)) {
                    return false;
                }
                for traced in steps.get(start..end).unwrap_or(&[]) {
                    for (x, y) in self.cells_of(traced.step) {
                        self.cells.push(Cell {
                            x,
                            y,
                            side,
                            corners: [BLACK; 4],
                        });
                    }
                }
                if let Some(run) = self.cells.get_mut(first..) {
                    run.sort_unstable_by_key(|cell| (cell.y, cell.x));
                }
                let mut kept = first;
                for at in first..self.cells.len() {
                    let fresh = kept == first
                        || self.cells.get(kept - 1).map(|cell| (cell.x, cell.y))
                            != self.cells.get(at).map(|cell| (cell.x, cell.y));
                    if fresh {
                        self.cells.swap(kept, at);
                        kept += 1;
                    }
                }
                self.cells.truncate(kept);
            }
            if !fallible::reserve(&mut self.runs, 1) {
                return false;
            }
            self.runs.push((start..end, first..self.cells.len()));
            start = end;
        }
        true
    }

    /// The top-left corners of the cells of `step`'s grid it is a corner of,
    /// within the picture.
    fn cells_of(&self, step: Step) -> impl Iterator<Item = (u32, u32)> {
        let side = step.side.max(1);
        let xs = [step.x.checked_sub(side), Some(step.x)];
        let ys = [step.y.checked_sub(side), Some(step.y)];
        let (width, height) = self.size;
        ys.into_iter()
            .flatten()
            .filter(move |y| *y < height)
            .flat_map(move |y| {
                xs.into_iter()
                    .flatten()
                    .filter(move |x| *x < width)
                    .map(move |x| (x, y))
            })
    }

    /// Every pixel painting `step` may change, or `None` for a step of no
    /// grid.
    fn reach(&self, step: Step) -> Option<Rect> {
        (step.side > 0).then(|| {
            self.cells_of(step)
                .map(|(x, y)| self.rect(x, y, step.side))
                .fold(Rect::EMPTY, |span, rect| span.union(&rect))
        })
    }

    /// The cell at `(x, y)` of side `side`, clipped to the picture.
    fn rect(&self, x: u32, y: u32, side: u32) -> Rect {
        let width = side.min(self.size.0.saturating_sub(x));
        let height = side.min(self.size.1.saturating_sub(y));
        Rect::new(
            i32::try_from(x).unwrap_or(i32::MAX),
            i32::try_from(y).unwrap_or(i32::MAX),
            width,
            height,
        )
    }

    /// Write `traced`'s own pixel, and record it if it is the first pass's.
    fn write(&mut self, surface: &mut Surface, traced: &Traced) {
        let step = traced.step;
        if step.side == 0 {
            return;
        }
        surface.set(step.x, step.y, traced.pixel);
        if step.side == self.coarsest {
            let at = (step.y / self.coarsest) as usize * self.columns as usize
                + (step.x / self.coarsest) as usize;
            if let Some(traced) = self.traced.get_mut(at) {
                *traced = true;
            }
        }
    }

    /// Paint `traced` alone: its pixel, then the cells it is a corner of.
    fn paint_one(&mut self, surface: &mut Surface, traced: &Traced) {
        if traced.step.side == 0 {
            return;
        }
        self.write(surface, traced);
        let mut cells = [Cell {
            x: 0,
            y: 0,
            side: traced.step.side,
            corners: [BLACK; 4],
        }; 4];
        let mut count = 0;
        for ((x, y), cell) in self.cells_of(traced.step).zip(&mut cells) {
            cell.x = x;
            cell.y = y;
            count += 1;
        }
        let cells = &mut cells[..count];
        for cell in cells.iter_mut() {
            cell.corners = corners(
                surface,
                self.size,
                &self.traced,
                self.coarsest,
                self.columns,
                cell,
            );
        }
        fill_cells(surface, cells, self.size, &tairix_parallel::SERIAL);
    }
}

/// The colours at `cell`'s corners as `surface` holds them, a corner past the
/// picture's edge taking the one beside it within, and an untraced point of
/// the first pass black.
fn corners(
    surface: &Surface,
    (width, height): (u32, u32),
    traced: &[bool],
    coarsest: u32,
    columns: u32,
    cell: &Cell,
) -> [Pixel; 4] {
    let side = cell.side;
    let last = |extent: u32| (extent.saturating_sub(1) / side) * side;
    let right = cell.x.saturating_add(side).min(last(width));
    let below = cell.y.saturating_add(side).min(last(height));
    let at = |x: u32, y: u32| {
        let untraced = side == coarsest
            && !traced
                .get((y / coarsest) as usize * columns as usize + (x / coarsest) as usize)
                .copied()
                .unwrap_or(false);
        if untraced {
            BLACK
        } else {
            surface.get(x, y).unwrap_or(BLACK)
        }
    };
    [
        at(cell.x, cell.y),
        at(right, cell.y),
        at(cell.x, below),
        at(right, below),
    ]
}

/// Fill `cells` — sorted by row, then column — from their corners into
/// `surface`, the picture's rows split in bands across `runner`.
fn fill_cells(
    surface: &mut Surface,
    cells: &[Cell],
    (width, height): (u32, u32),
    runner: &dyn JobRunner,
) {
    let Some(rows) = cells
        .first()
        .zip(cells.last())
        .map(|(first, last)| first.y..last.y.saturating_add(last.side).min(height))
    else {
        return;
    };
    let pixels = cells.len().saturating_mul(cells.first().map_or(1, |cell| {
        (cell.side as usize).saturating_mul(cell.side as usize)
    }));
    let pieces = tairix_parallel::bands(runner, pixels, PAINT_GRAIN);
    let span = rows.end.saturating_sub(rows.start);
    let per_band = span
        .div_ceil(u32::try_from(pieces.max(1)).unwrap_or(u32::MAX))
        .max(1);
    let paint = |band: &mut RowBand<'_>| {
        let rows = band.rows();
        // Cells are sorted by row, so those reaching into the band are a run.
        let first = cells.partition_point(|cell| cell.y.saturating_add(cell.side) <= rows.start);
        for cell in cells.get(first..).unwrap_or(&[]) {
            if cell.y >= rows.end {
                break;
            }
            fill_cell(band, cell, width, rows.clone());
        }
    };
    let mut bands: Vec<RowBand<'_>> = Vec::new();
    if pieces > 1 && fallible::reserve(&mut bands, pieces) {
        bands.extend(surface.row_bands_mut(rows, per_band));
        tairix_parallel::for_each(runner, &mut bands, &paint);
    } else {
        for mut band in surface.row_bands_mut(rows, span.max(1)) {
            paint(&mut band);
        }
    }
}

/// Fill the rows `within` of `cell` in `band` by bilinear interpolation of its
/// corners, in 8-bit fixed point: a pixel at a corner is that corner's colour
/// exactly.
fn fill_cell(band: &mut RowBand<'_>, cell: &Cell, width: u32, within: Range<u32>) {
    let shift = cell.side.trailing_zeros();
    let across_cell = cell.side.min(width.saturating_sub(cell.x));
    let rows = cell.y.max(within.start)..cell.y.saturating_add(cell.side).min(within.end);
    let [top_left, top_right, bottom_left, bottom_right] = cell.corners.map(channels);
    for y in rows {
        let down = ((y - cell.y) << 8) >> shift;
        let up = 256 - down;
        let left = [0, 1, 2, 3].map(|at| top_left[at] * up + bottom_left[at] * down);
        let right = [0, 1, 2, 3].map(|at| top_right[at] * up + bottom_right[at] * down);
        let Some((first, span)) = band.row_span_mut(y, cell.x, across_cell) else {
            continue;
        };
        for (x, pixel) in (first..).zip(span.iter_mut()) {
            let across = ((x - cell.x) << 8) >> shift;
            let back = 256 - across;
            let mix = |at: usize| {
                let level = (left[at] * back + right[at] * across + (1 << 15)) >> 16;
                u8::try_from(level).unwrap_or(u8::MAX)
            };
            *pixel = Pixel {
                r: mix(0),
                g: mix(1),
                b: mix(2),
                a: mix(3),
            };
        }
    }
}

/// A pixel's channels, widened for the interpolation.
fn channels(pixel: Pixel) -> [u32; 4] {
    [
        u32::from(pixel.r),
        u32::from(pixel.g),
        u32::from(pixel.b),
        u32::from(pixel.a),
    ]
}

#[cfg(test)]
#[path = "preview_tests.rs"]
mod tests;
