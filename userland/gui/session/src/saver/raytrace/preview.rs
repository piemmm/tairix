//! What a reveal shows while it is under way: the picture out of focus at
//! first, sharpening pass by pass until it is every pixel's own trace.
//!
//! The picture shown is a uniform cubic B-spline over the grid the current
//! pass traces, smooth through its second derivative, so a coarse grid
//! shows as a soft blur and no point as a peak or a cross. A pass's grid
//! starts as the coarser grid refined (Lane and Riesenfeld, 1980), which is
//! the same surface, so a pass never jumps. A point's control then moves
//! from that refinement to its own trace by the share of the points about
//! it that are traced, so detail fades in as the samples gather rather than
//! at each new point. In the last pass each pixel settles onto its trace by
//! the same share, so the finished picture is exactly the traced one.
//!
//! The grid mirrors across its first column and row; past its last, the
//! first grid repeats its edge and every later one keeps what refinement
//! carries down. Both agree with refining, so no edge jumps either.
//!
//! A step changes nothing beyond three of its pass's spacings from its
//! point. A frame repaints the cells those reaches cover, each once, and
//! marks a cover of whole tiles, which a compositor merges cheaply.

use alloc::vec::Vec;
use core::ops::{Range, RangeInclusive};

use tairix_parallel::JobRunner;
use tairix_raster::surface::MAX_SURFACE_PIXELS;
use tairix_raster::{DitherRow, Pixel, RowBand};
use tairix_raytrace::{Reveal, Step};
use tairix_util::fallible;
use tairix_wm::{Rect, Region, Surface};

use super::engine::Traced;

/// Red, green and blue levels, as the B-spline weighs them.
type Rgb = [f32; 3];

const BLACK: Rgb = [0.0; 3];

/// How many of its pass's spacings a step's change reaches from its point:
/// it settles the points beside it, whose basis reaches two spacings on.
const REACH: u32 = 3;

/// The fewest pixels worth handing another core to paint: a pixel costs a
/// few dozen multiplies, so a hand-off must carry many.
const PAINT_GRAIN: usize = 16_384;

/// What laying out a cell's controls costs, in pixels painted: a few
/// controls each, each weighing up to eighteen traced pixels. It is most of
/// a cell's cost in the finest passes.
const CELL_CONTROLS: usize = 32;

/// The most cells one stretch of the painter lays controls out for.
const CHUNK: u32 = 64;

/// Room for a stretch's controls along a row: its cells and three beyond.
const STRETCH: usize = CHUNK as usize + 3;

/// The side of the finest tile damage is marked in.
const DAMAGE_TILE: u32 = 16;

/// The most rectangles a frame's damage lists. A compositor merges each
/// against the rest, so scattered rectangles cost more than the pixels they
/// spare long before they cover the screen; past this the tiles double.
const DAMAGE_BUDGET: usize = 128;

/// One pass's grid over the picture: its spacing and its last column and row.
#[derive(Copy, Clone, Debug)]
struct Grid {
    side: u32,
    last: (u32, u32),
}

impl Grid {
    /// The grid of spacing `side` over a `width` by `height` picture.
    const fn over((width, height): (u32, u32), side: u32) -> Self {
        Self {
            side,
            last: (
                width.saturating_sub(1) / side,
                height.saturating_sub(1) / side,
            ),
        }
    }

    /// Whether point `(i, j)` is also one of the grid twice as coarse.
    const fn coarser(i: u32, j: u32) -> bool {
        i.is_multiple_of(2) && j.is_multiple_of(2)
    }
}

/// One control of the B-spline.
#[derive(Copy, Clone, Debug)]
struct Control {
    /// What it weighs in the B-spline.
    value: Rgb,
    /// The pixel traced at its point, and the share of it settled: none for a
    /// point not yet traced, all of it once every point about it is.
    trace: Rgb,
    share: f32,
}

impl Control {
    const BLACK: Self = Self::predicted(BLACK);

    /// A control standing at `value`, with nothing of its own settled.
    const fn predicted(value: Rgb) -> Self {
        Self {
            value,
            trace: value,
            share: 0.0,
        }
    }
}

/// What a grid holds past its last column and row, where nothing is traced:
/// two columns from the top to two rows past the last, and two rows below
/// the rest.
struct Border {
    grid: Grid,
    right: Vec<Rgb>,
    below: Vec<Rgb>,
}

impl Border {
    /// Room for the border of any grid over a `size` picture coarser than its
    /// pixels; `None` when the heap will not hold it.
    fn new(size: (u32, u32)) -> Option<Self> {
        let finest = Grid::over(size, 2);
        let (columns, rows) = (finest.last.0 as usize, finest.last.1 as usize);
        Some(Self {
            grid: finest,
            right: fallible::filled(2 * (rows + 3), BLACK)?,
            below: fallible::filled(2 * (columns + 1), BLACK)?,
        })
    }

    /// The control at `(i, j)`, past the grid's last column or row by at most
    /// two.
    fn get(&self, i: u32, j: u32) -> Rgb {
        let (columns, rows) = (self.grid.last.0 as usize, self.grid.last.1 as usize);
        let (i, j) = (i as usize, j as usize);
        let held = if i > columns && i <= columns + 2 && j <= rows + 2 {
            self.right.get((i - columns - 1) * (rows + 3) + j)
        } else if i <= columns && j > rows && j <= rows + 2 {
            self.below.get((j - rows - 1) * (columns + 1) + i)
        } else {
            None
        };
        held.copied().unwrap_or(BLACK)
    }

    /// Hold `grid`'s border as `control` gives each of its points.
    fn fill(&mut self, grid: Grid, control: impl Fn(u32, u32) -> Rgb) {
        self.grid = grid;
        let (columns, rows) = grid.last;
        let right = (columns + 1..=columns + 2).flat_map(|i| (0..=rows + 2).map(move |j| (i, j)));
        for (slot, (i, j)) in self.right.iter_mut().zip(right) {
            *slot = control(i, j);
        }
        let below = (rows + 1..=rows + 2).flat_map(|j| (0..=columns).map(move |i| (i, j)));
        for (slot, (i, j)) in self.below.iter_mut().zip(below) {
            *slot = control(i, j);
        }
    }
}

/// The picture as the grid now traced sees it: all a control, and so a pixel
/// shown, is drawn from.
#[derive(Copy, Clone)]
struct Field<'a> {
    samples: &'a [Pixel],
    width: u32,
    grid: Grid,
    /// Whether `grid` is the reveal's first, which no grid lies beneath.
    first: bool,
    /// What the grid twice as coarse holds past its edge.
    coarse: &'a Border,
}

impl Field<'_> {
    /// The pixel traced at point `(i, j)` of a grid of spacing `side`, if
    /// it is traced.
    fn traced(&self, side: u32, i: u32, j: u32) -> Option<Rgb> {
        let x = i as usize * side as usize;
        let y = j as usize * side as usize;
        let pixel = self.samples.get(y * self.width as usize + x)?;
        (pixel.a != 0).then(|| [pixel.r, pixel.g, pixel.b].map(f32::from))
    }

    /// Control `(i, j)` of the grid, mirrored across its first column and row.
    fn control(&self, i: i64, j: i64) -> Control {
        let (i, j) = (mirror(i), mirror(j));
        let (columns, rows) = self.grid.last;
        if i <= columns && j <= rows {
            self.point(i, j)
        } else if self.first {
            Control::predicted(self.point(i.min(columns), j.min(rows)).value)
        } else {
            Control::predicted(self.refined(i, j))
        }
    }

    /// The control at point `(i, j)` of the grid.
    fn point(&self, i: u32, j: u32) -> Control {
        let base = if self.first {
            BLACK
        } else {
            self.refined(i, j)
        };
        let Some(trace) = self.traced(self.grid.side, i, j) else {
            return Control::predicted(base);
        };
        let share = self.settled(i, j);
        Control {
            value: settle(base, trace, share),
            trace,
            share,
        }
    }

    /// The share of this pass's points about `(i, j)`, itself among them
    /// where it is one, that are traced.
    fn settled(&self, i: u32, j: u32) -> f32 {
        let (columns, rows) = self.grid.last;
        let (mut traced, mut points) = (0u16, 0u16);
        for v in j.saturating_sub(1)..=j.saturating_add(1).min(rows) {
            for u in i.saturating_sub(1)..=i.saturating_add(1).min(columns) {
                if !self.first && Grid::coarser(u, v) {
                    continue;
                }
                points += 1;
                traced += u16::from(self.traced(self.grid.side, u, v).is_some());
            }
        }
        if points == 0 {
            1.0
        } else {
            f32::from(traced) / f32::from(points)
        }
    }

    /// What refining the grid twice as coarse puts at `(i, j)`.
    fn refined(&self, i: u32, j: u32) -> Rgb {
        let (left, across) = refining(i);
        let (top, down) = refining(j);
        let mut sum = BLACK;
        for (row, down) in (top..).zip(down) {
            for (column, across) in (left..).zip(across) {
                add_scaled(&mut sum, self.coarse_control(column, row), across * down);
            }
        }
        sum
    }

    /// Control `(i, j)` of the grid twice as coarse, whose pass is over.
    fn coarse_control(&self, i: i64, j: i64) -> Rgb {
        let (i, j) = (mirror(i), mirror(j));
        let grid = self.coarse.grid;
        if i <= grid.last.0 && j <= grid.last.1 {
            self.traced(grid.side, i, j).unwrap_or(BLACK)
        } else {
            self.coarse.get(i, j)
        }
    }
}

/// The painter of one reveal on one screen.
pub(super) struct Preview {
    size: (u32, u32),
    coarsest: u32,
    /// The grid the reveal is tracing now.
    grid: Grid,
    /// Every pixel as traced, opaque, and every other transparent.
    samples: Vec<Pixel>,
    /// What the grid twice as coarse as `grid` holds past its edge, and room
    /// to keep `grid`'s own once its pass is over.
    coarse: Border,
    spare: Border,
    /// The B-spline's weights at each offset into a cell of `grid`.
    weights: Vec<[f32; 4]>,
    /// The cells of `grid` the steps taken since the last paint change.
    dirty: Cells,
    /// Which tiles of the finest size those cells touch.
    tiles: Vec<bool>,
}

impl Preview {
    /// A painter for a `size` screen; `None` for a screen with no pixels or
    /// more than a surface holds, or when the heap will not hold what it
    /// keeps.
    pub(super) fn new(size: (u32, u32)) -> Option<Self> {
        let (width, height) = size;
        let pixels = usize::try_from(u64::from(width) * u64::from(height))
            .ok()
            .filter(|count| (1..=MAX_SURFACE_PIXELS).contains(count))?;
        let tiles = usize::try_from(
            u64::from(width.div_ceil(DAMAGE_TILE)) * u64::from(height.div_ceil(DAMAGE_TILE)),
        )
        .ok()?;
        let coarsest = Reveal::coarsest(size);
        let mut preview = Self {
            size,
            coarsest,
            grid: Grid::over(size, coarsest),
            samples: fallible::filled(pixels, Pixel::TRANSPARENT)?,
            coarse: Border::new(size)?,
            spare: Border::new(size)?,
            weights: fallible::filled(coarsest as usize, [0.0; 4])?,
            dirty: Cells::new(size)?,
            tiles: fallible::filled(tiles, false)?,
        };
        preview.load_weights();
        Some(preview)
    }

    /// Forget every traced pixel: the next reveal begins over black.
    pub(super) fn reset(&mut self) {
        self.samples.fill(Pixel::TRANSPARENT);
        self.grid = Grid::over(self.size, self.coarsest);
        self.load_weights();
        self.dirty.clear();
        self.tiles.fill(false);
    }

    /// Take `steps` in and add what they change to `damage` — the whole
    /// picture when the window's buffer was not `kept`, which the next
    /// [`paint`](Self::paint) then lays afresh.
    pub(super) fn take(&mut self, steps: &[Traced], kept: bool, damage: &mut Region) {
        for traced in steps {
            self.record(traced);
        }
        if !kept {
            let (columns, rows) = self.grid.last;
            self.dirty.add(0..=columns, 0..=rows);
            damage.add(self.rect(0..=columns, 0..=rows));
            return;
        }
        for traced in steps {
            let Some((columns, rows)) = self.reach(traced.step) else {
                continue;
            };
            self.dirty.add(columns.clone(), rows.clone());
            self.mark_tiles(self.rect(columns, rows));
        }
        self.cover(damage);
    }

    /// Paint what the steps taken since the last paint change into `surface`,
    /// spreading the rows over `runner`.
    pub(super) fn paint(&mut self, surface: &mut Surface, runner: &dyn JobRunner) {
        let side = self.grid.side;
        let rows = self.dirty.rows.start.saturating_mul(side)
            ..self.dirty.rows.end.saturating_mul(side).min(self.size.1);
        if rows.is_empty() {
            return;
        }
        let cell = (side as usize)
            .saturating_mul(side as usize)
            .saturating_add(CELL_CONTROLS);
        let pieces =
            tairix_parallel::bands(runner, self.dirty.count().saturating_mul(cell), PAINT_GRAIN);
        let painter = Painter {
            field: self.field(),
            weights: self.weights.get(..side as usize).unwrap_or(&[]),
            dirty: &self.dirty,
        };
        let span = rows.end - rows.start;
        let per_band = span
            .div_ceil(u32::try_from(pieces.max(1)).unwrap_or(u32::MAX))
            .max(1);
        let paint = |band: &mut RowBand<'_>| painter.band(band);
        let mut bands: Vec<RowBand<'_>> = Vec::new();
        if pieces > 1 && fallible::reserve(&mut bands, pieces) {
            bands.extend(surface.row_bands_mut(rows, per_band));
            tairix_parallel::for_each(runner, &mut bands, &paint);
        } else {
            for mut band in surface.row_bands_mut(rows, span) {
                paint(&mut band);
            }
        }
        self.dirty.clear();
    }

    /// The picture as the grid now traced sees it.
    fn field(&self) -> Field<'_> {
        Field {
            samples: &self.samples,
            width: self.size.0,
            grid: self.grid,
            first: self.grid.side == self.coarsest,
            coarse: &self.coarse,
        }
    }

    /// Keep `traced`'s pixel, beginning the passes it is beyond. A step the
    /// reveal could not have taken from here is ignored.
    fn record(&mut self, traced: &Traced) {
        let step = traced.step;
        if !self.admits(step) {
            return;
        }
        while step.side < self.grid.side {
            self.refine();
        }
        let at = step.y as usize * self.size.0 as usize + step.x as usize;
        if let Some(sample) = self.samples.get_mut(at) {
            *sample = Pixel {
                a: u8::MAX,
                ..traced.pixel
            };
        }
    }

    /// Whether `step` is a point of the current pass's grid or a finer one,
    /// and not one a coarser pass holds.
    fn admits(&self, step: Step) -> bool {
        let Step { x, y, side } = step;
        let (width, height) = self.size;
        let on_grid = side.is_power_of_two()
            && side <= self.grid.side
            && x < width
            && y < height
            && x.is_multiple_of(side)
            && y.is_multiple_of(side);
        on_grid && (side == self.coarsest || !Grid::coarser(x / side, y / side))
    }

    /// Begin the next pass, on the grid of half the spacing, keeping what the
    /// finished grid holds past its edge for refinement to carry into it.
    fn refine(&mut self) {
        let finished = self.grid;
        let field = Field {
            samples: &self.samples,
            width: self.size.0,
            grid: finished,
            first: finished.side == self.coarsest,
            coarse: &self.coarse,
        };
        self.spare.fill(finished, |i, j| {
            field.control(i64::from(i), i64::from(j)).value
        });
        core::mem::swap(&mut self.coarse, &mut self.spare);
        self.grid = Grid::over(self.size, finished.side / 2);
        self.load_weights();
    }

    /// Weigh each offset into a cell of the current grid. A surface's pixels
    /// keep its shorter side within a `u16`'s range, and a grid's spacing
    /// within that.
    fn load_weights(&mut self) {
        let side = u16::try_from(self.grid.side).unwrap_or(u16::MAX);
        for (offset, slot) in (0..side).zip(self.weights.iter_mut()) {
            *slot = weights(f32::from(offset) / f32::from(side));
        }
    }

    /// The cells of the current grid painting `step` may change, or `None`
    /// for a step off the picture.
    fn reach(&self, step: Step) -> Option<(RangeInclusive<u32>, RangeInclusive<u32>)> {
        let Step { x, y, side } = step;
        let (width, height) = self.size;
        if side == 0 || x >= width || y >= height {
            return None;
        }
        let near = side.saturating_mul(REACH) - 1;
        let cell = self.grid.side;
        Some((
            x.saturating_sub(near) / cell..=x.saturating_add(near).min(width - 1) / cell,
            y.saturating_sub(near) / cell..=y.saturating_add(near).min(height - 1) / cell,
        ))
    }

    /// The pixels of the current grid's cells `columns` by `rows`.
    fn rect(&self, columns: RangeInclusive<u32>, rows: RangeInclusive<u32>) -> Rect {
        let (width, height) = self.size;
        let cell = self.grid.side;
        let left = columns.start().saturating_mul(cell);
        let top = rows.start().saturating_mul(cell);
        let right = columns
            .end()
            .saturating_add(1)
            .saturating_mul(cell)
            .min(width);
        let bottom = rows
            .end()
            .saturating_add(1)
            .saturating_mul(cell)
            .min(height);
        Rect::new(
            i32::try_from(left).unwrap_or(i32::MAX),
            i32::try_from(top).unwrap_or(i32::MAX),
            right.saturating_sub(left),
            bottom.saturating_sub(top),
        )
    }

    /// Mark the tiles of the finest size `rect` touches.
    fn mark_tiles(&mut self, rect: Rect) {
        if rect.is_empty() {
            return;
        }
        let across = self.size.0.div_ceil(DAMAGE_TILE) as usize;
        let tile = |edge: i32| (u32::try_from(edge).unwrap_or(0) / DAMAGE_TILE) as usize;
        let (left, right) = (tile(rect.left()), tile(rect.right() - 1));
        for row in tile(rect.top())..=tile(rect.bottom() - 1) {
            let start = row * across;
            if let Some(run) = self.tiles.get_mut(start + left..=start + right) {
                run.fill(true);
            }
        }
    }

    /// Add the marked tiles to `damage`, doubling their side until their runs
    /// fit the budget, and clear them.
    fn cover(&mut self, damage: &mut Region) {
        let (width, height) = self.size;
        let mut tile = DAMAGE_TILE;
        let mut across = width.div_ceil(tile) as usize;
        let mut down = height.div_ceil(tile) as usize;
        while runs(&self.tiles, across, down).count() > DAMAGE_BUDGET && (across > 1 || down > 1) {
            coarsen(&mut self.tiles, across, down);
            (across, down) = (across.div_ceil(2), down.div_ceil(2));
            tile = tile.saturating_mul(2);
        }
        let edge = |at: usize| u32::try_from(at).unwrap_or(u32::MAX).saturating_mul(tile);
        for (row, columns) in runs(&self.tiles, across, down) {
            let (left, top) = (edge(columns.start), edge(row));
            damage.add(Rect::new(
                i32::try_from(left).unwrap_or(i32::MAX),
                i32::try_from(top).unwrap_or(i32::MAX),
                edge(columns.end).min(width).saturating_sub(left),
                top.saturating_add(tile).min(height).saturating_sub(top),
            ));
        }
        self.tiles.fill(false);
    }
}

/// What a paint reads, shared by every band.
struct Painter<'a> {
    field: Field<'a>,
    weights: &'a [[f32; 4]],
    dirty: &'a Cells,
}

impl Painter<'_> {
    /// Paint the marked cells reaching into `band`, each run of them with the
    /// same run in the rows below, so each row of controls is laid out once.
    fn band(&self, band: &mut RowBand<'_>) {
        let side = self.field.grid.side;
        let rows = band.rows();
        if rows.is_empty() {
            return;
        }
        let last = self.field.grid.last;
        let (top, bottom) = (rows.start / side, ((rows.end - 1) / side).min(last.1));
        for row in top..=bottom {
            for run in self.dirty.runs(row, last.0) {
                // The row above painted this run with its own.
                if row > top && self.dirty.holds(row - 1, run, last.0) {
                    continue;
                }
                let mut through = row;
                while through < bottom && self.dirty.holds(through + 1, run, last.0) {
                    through += 1;
                }
                let (mut start, end) = run;
                while start <= end {
                    let stop = end.min(start.saturating_add(CHUNK - 1));
                    self.stack(band, row..=through, start..=stop, &rows);
                    start = stop.saturating_add(1);
                }
            }
        }
    }

    /// Paint cells `cells` of grid rows `stack` within lines `lines`, the
    /// controls rolling down a row of them at a time.
    fn stack(
        &self,
        band: &mut RowBand<'_>,
        stack: RangeInclusive<u32>,
        cells: RangeInclusive<u32>,
        lines: &Range<u32>,
    ) {
        let side = self.field.grid.side;
        // A pixel on a grid line takes nothing from the control two cells
        // on, and every pixel of the last pass lies on one.
        let taps: u32 = if side == 1 { 3 } else { 4 };
        let (first, last) = (*cells.start(), *cells.end());
        let span = (last - first + taps) as usize;
        let lay = |line: &mut [Control; STRETCH], j: i64| {
            for (i, slot) in (i64::from(first) - 1..).zip(line.iter_mut().take(span)) {
                *slot = self.field.control(i, j);
            }
        };
        let mut controls = [[Control::BLACK; STRETCH]; 4];
        let top = *stack.start();
        for (j, line) in (i64::from(top) - 1..).zip(controls.iter_mut().take(taps as usize)) {
            lay(line, j);
        }
        let shift = side.trailing_zeros();
        let left = first.saturating_mul(side);
        let right = last
            .saturating_add(1)
            .saturating_mul(side)
            .min(self.field.width);
        let mut column = [BLACK; STRETCH];
        for row in stack {
            if row > top {
                controls[..taps as usize].rotate_left(1);
                lay(&mut controls[taps as usize - 1], i64::from(row + taps) - 2);
            }
            let start = row * side;
            for y in start.max(lines.start)..start.saturating_add(side).min(lines.end) {
                let Some(down) = self.weights.get((y - start) as usize) else {
                    continue;
                };
                for (at, sum) in column.iter_mut().take(span).enumerate() {
                    *sum = BLACK;
                    for (line, weight) in controls.iter().zip(down).take(taps as usize) {
                        add_scaled(sum, line[at].value, *weight);
                    }
                }
                let dither = DitherRow::at(y);
                let Some((from, pixels)) = band.row_span_mut(y, left, right - left) else {
                    continue;
                };
                for (x, pixel) in (from..).zip(pixels) {
                    let cell = ((x >> shift) - first) as usize;
                    let Some(across) = self.weights.get((x & (side - 1)) as usize) else {
                        continue;
                    };
                    let mut value = BLACK;
                    for (sum, weight) in column[cell..].iter().zip(across).take(taps as usize) {
                        add_scaled(&mut value, *sum, *weight);
                    }
                    if side == 1 {
                        let own = controls[1][cell + 1];
                        value = settle(value, own.trace, own.share);
                    }
                    *pixel = encode(value, dither.bias(x));
                }
            }
        }
    }
}

/// A set of one grid's cells, a bit each, its rows at the finest grid's
/// stride.
struct Cells {
    stride: usize,
    bits: Vec<u64>,
    /// The rows holding any cell; empty when none does.
    rows: Range<u32>,
}

impl Cells {
    /// Room for every pixel of a `size` picture; `None` when the heap will
    /// not hold it.
    fn new((width, height): (u32, u32)) -> Option<Self> {
        let stride = width.div_ceil(u64::BITS) as usize;
        Some(Self {
            stride,
            bits: fallible::filled(stride.checked_mul(height as usize)?, 0)?,
            rows: 0..0,
        })
    }

    /// Add the cells in columns `columns` of rows `rows`.
    fn add(&mut self, columns: RangeInclusive<u32>, rows: RangeInclusive<u32>) {
        let (first, last) = (*columns.start() as usize, *columns.end() as usize);
        if first > last || rows.is_empty() {
            return;
        }
        for row in rows.clone() {
            let start = row as usize * self.stride;
            let Some(words) = self.bits.get_mut(start..start + self.stride) else {
                continue;
            };
            for (at, word) in words
                .iter_mut()
                .enumerate()
                .take(last / 64 + 1)
                .skip(first / 64)
            {
                let low = if at == first / 64 { first % 64 } else { 0 };
                let high = if at == last / 64 { last % 64 } else { 63 };
                *word |= (u64::MAX << low) & (u64::MAX >> (63 - high));
            }
        }
        let (top, bottom) = (*rows.start(), rows.end().saturating_add(1));
        self.rows = if self.rows.is_empty() {
            top..bottom
        } else {
            self.rows.start.min(top)..self.rows.end.max(bottom)
        };
    }

    /// The runs of cells in `row`, each as its first and last column, none
    /// past `last`.
    fn runs(&self, row: u32, last: u32) -> CellRuns<'_> {
        let start = row as usize * self.stride;
        CellRuns {
            words: self.bits.get(start..start + self.stride).unwrap_or(&[]),
            at: 0,
            end: last.saturating_add(1),
        }
    }

    /// Whether `run` is one of the runs of `row`, none past `last`.
    fn holds(&self, row: u32, run: (u32, u32), last: u32) -> bool {
        self.runs(row, last)
            .take_while(|&(first, _)| first <= run.0)
            .any(|held| held == run)
    }

    /// The words of the rows holding any cell.
    fn held(&self) -> Range<usize> {
        let word = |row: u32| (row as usize * self.stride).min(self.bits.len());
        word(self.rows.start)..word(self.rows.end)
    }

    /// How many cells the set holds.
    fn count(&self) -> usize {
        let words = self.bits.get(self.held()).unwrap_or(&[]);
        words.iter().map(|word| word.count_ones() as usize).sum()
    }

    /// Remove every cell.
    fn clear(&mut self) {
        let held = self.held();
        if let Some(words) = self.bits.get_mut(held) {
            words.fill(0);
        }
        self.rows = 0..0;
    }
}

/// The runs of one row of [`Cells`].
struct CellRuns<'a> {
    words: &'a [u64],
    at: u32,
    end: u32,
}

impl CellRuns<'_> {
    /// The first column from `from` whose cell is `held` or not, before the
    /// end.
    fn seek(&self, from: u32, held: bool) -> Option<u32> {
        let mut word = from / 64;
        let mut mask = u64::MAX << (from % 64);
        while word * 64 < self.end {
            let bits = *self.words.get(word as usize)?;
            let bits = if held { bits } else { !bits } & mask;
            if bits != 0 {
                let column = word * 64 + bits.trailing_zeros();
                return (column < self.end).then_some(column);
            }
            word += 1;
            mask = u64::MAX;
        }
        None
    }
}

impl Iterator for CellRuns<'_> {
    type Item = (u32, u32);

    fn next(&mut self) -> Option<(u32, u32)> {
        let first = self.seek(self.at, true)?;
        let end = self.seek(first, false).unwrap_or(self.end);
        self.at = end;
        Some((first, end - 1))
    }
}

/// The runs of marked tiles in an `across` by `down` grid of them, row by
/// row, each as its row and its columns.
fn runs(
    tiles: &[bool],
    across: usize,
    down: usize,
) -> impl Iterator<Item = (usize, Range<usize>)> + '_ {
    (0..down).flat_map(move |row| {
        let line = tiles.get(row * across..(row + 1) * across).unwrap_or(&[]);
        let mut at = 0;
        core::iter::from_fn(move || {
            let first = at + line.get(at..)?.iter().position(|marked| *marked)?;
            let end = line
                .get(first..)
                .and_then(|rest| rest.iter().position(|marked| !*marked))
                .map_or(line.len(), |length| first + length);
            at = end;
            Some((row, first..end))
        })
    })
}

/// Merge each two by two of an `across` by `down` grid of tiles into one, in
/// place: a merged tile's index is never past those it reads.
fn coarsen(tiles: &mut [bool], across: usize, down: usize) {
    let half = across.div_ceil(2);
    for row in 0..down.div_ceil(2) {
        for column in 0..half {
            let marked = (2 * row..(2 * row + 2).min(down)).any(|y| {
                (2 * column..(2 * column + 2).min(across))
                    .any(|x| tiles.get(y * across + x).copied().unwrap_or(false))
            });
            if let Some(tile) = tiles.get_mut(row * half + column) {
                *tile = marked;
            }
        }
    }
}

/// The first point of the grid twice as coarse that refining draws point
/// `index` of a grid from, and the weights of it and those after: a point on
/// the coarse grid keeps three quarters of its control and an eighth of each
/// neighbour's, and one between two takes half of each.
fn refining(index: u32) -> (i64, &'static [f32]) {
    let half = i64::from(index / 2);
    if index.is_multiple_of(2) {
        (half - 1, &[0.125, 0.75, 0.125])
    } else {
        (half, &[0.5, 0.5])
    }
}

/// The uniform cubic B-spline's weights at `t` of the way across a cell, for
/// the controls one before its corner, at it, one after and two after.
fn weights(t: f32) -> [f32; 4] {
    let (square, rest) = (t * t, 1.0 - t);
    let cube = square * t;
    [
        rest * rest * rest / 6.0,
        (3.0 * cube - 6.0 * square + 4.0) / 6.0,
        (3.0 * (t + square - cube) + 1.0) / 6.0,
        cube / 6.0,
    ]
}

/// Grid index `index`, mirrored across the first column or row.
fn mirror(index: i64) -> u32 {
    u32::try_from(index.unsigned_abs()).unwrap_or(u32::MAX)
}

/// `share` of the way from `base` to `trace`, and `trace` itself once all
/// of it is settled.
fn settle(base: Rgb, trace: Rgb, share: f32) -> Rgb {
    if share >= 1.0 {
        return trace;
    }
    let mut value = base;
    for (level, to) in value.iter_mut().zip(trace) {
        *level += share * (to - *level);
    }
    value
}

/// Add `weight` of `value` to `sum`.
fn add_scaled(sum: &mut Rgb, value: Rgb, weight: f32) {
    for (level, value) in sum.iter_mut().zip(value) {
        *level += weight * value;
    }
}

/// `rgb` as an opaque pixel, each level rounding up `bias` 256ths of the way
/// short of the next, so a blur's slow gradients do not band.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a float casts to an integer saturating, so a level the B-spline overshoots by \
              rounding error alone holds at the extreme, and truncating it rounds it down"
)]
fn encode(rgb: Rgb, bias: u32) -> Pixel {
    let bias = f32::from(u8::try_from(bias).unwrap_or(u8::MAX));
    let level = |value: f32| u8::try_from(((value * 256.0 + bias) as u32) >> 8).unwrap_or(u8::MAX);
    Pixel {
        r: level(rgb[0]),
        g: level(rgb[1]),
        b: level(rgb[2]),
        a: u8::MAX,
    }
}

#[cfg(test)]
#[path = "preview_tests.rs"]
mod tests;
