//! The half-resolution blur a window's frost is drawn with.
//!
//! The backdrop is averaged two by two into cells on a grid anchored at the
//! blurred rectangle's top-left, the cells are box-blurred by half the radius,
//! and each pixel is spread back from the two cells nearest its centre along
//! each axis, weighted three to one. A quarter of the area is blurred, for a
//! frost close to the full-resolution one; samples past the rectangle's edges
//! replicate them at every stage.

use core::ops::Range;

use tairix_parallel::JobRunner;
use tairix_util::fallible;

use super::{
    clamp, confined, covered_run, lay_unblurred, mix_covered, room, BlurScratch, Blurring, Lay,
    Offset, MIN_PARALLEL_FROST_PX,
};
use crate::color::Pixel;
use crate::surface::{band_rows, RowBand, Surface};

/// A cell's channels taken between two rows of cells, in quarters of a level,
/// laid twice: once for each pixel of the cell. Kept as plain lanes rather
/// than as pixels, so a row of them is spread in vector lanes.
type Cell = [u16; 8];

/// The most pixels spread at once, on the stack, before they are laid.
const SPREAD_RUN: usize = 64;

/// How far past the pixels it writes a half-resolution blur of `radius` reads
/// its backdrop, and so how far into it a change to the backdrop reaches.
#[must_use]
pub const fn halved_reach(radius: u32) -> u32 {
    if radius == 0 {
        0
    } else {
        (radius / 2).saturating_mul(2).saturating_add(2)
    }
}

impl BlurScratch {
    /// Reserve what a [`Surface::blur_halved_from`] or
    /// [`Surface::frost_halved_from`] of up to `bands` bands of a rectangle no
    /// wider than `width` and no taller than `height` needs when `runner`
    /// spreads it, reporting whether it could.
    ///
    /// A caller that frosts every frame reserves once, for its output, so no
    /// frost it runs grows the scratch on its frame path or finds it refused.
    /// A refusal keeps what was reserved before: lines and pieces short of the
    /// runner cost parallelism, cells short of the rectangle a growth the frost
    /// may itself be refused.
    pub fn reserve_halved(
        &mut self,
        width: u32,
        height: u32,
        bands: usize,
        runner: &dyn JobRunner,
    ) -> bool {
        let (across, down) = (width.div_ceil(2), height.div_ceil(2));
        let lines = spread_line(across).checked_mul(runner.width().max(1));
        let reduced = self.hold_reduced(across, down);
        let blur = self.reserve(across, down, bands, runner);
        let spread = lines.is_some_and(|len| fallible::grow_to(&mut self.spread, len, 0));
        reduced && blur && spread && room(&mut self.cells, bands.max(1))
    }

    /// Hold a plane of at least `across` × `down` cells, keeping the one held
    /// where it is large enough or no larger one can be had.
    fn hold_reduced(&mut self, across: u32, down: u32) -> bool {
        if self
            .reduced
            .as_ref()
            .is_some_and(|held| held.width() >= across && held.height() >= down)
        {
            return true;
        }
        let Some(grown) = Surface::new(across.max(1), down.max(1)) else {
            return false;
        };
        self.reduced = Some(grown);
        true
    }
}

impl Surface {
    /// Write the half-resolution blur `blurring` describes, unmixed, into this
    /// surface, whose top-left lies at `at` of `plane` — past its top or left
    /// edge or not — reading the backdrop from `plane`, and report whether it
    /// was written.
    ///
    /// The bands and the rectangle are `plane`'s coordinates, and the cells
    /// are anchored at the rectangle's top-left, so the blur at a pixel is the
    /// same whichever bands it is written in and wherever this surface lies.
    /// The backdrop is read within [`halved_reach`] of the bands. What of a
    /// band this surface does not reach is left out. `plane` is working memory
    /// past the backdrop it is handed: what it holds afterwards is unspecified.
    ///
    /// A radius of `0` copies the backdrop. `false` is a blur the scratch could
    /// not be grown for, and leaves this surface untouched.
    pub fn blur_halved_from(
        &mut self,
        at: (i32, i32),
        plane: &mut Self,
        blurring: &Blurring<'_>,
        scratch: &mut BlurScratch,
        runner: &dyn JobRunner,
    ) -> bool {
        let at = Offset { x: at.0, y: at.1 };
        let reach = at.reach(self);
        let lay = Lay::<fn(u32, u32) -> u8>::Field(at);
        halve(self, &lay, &reach, plane, blurring, scratch, runner)
    }

    /// Frost this surface with the half-resolution blur `blurring` describes:
    /// the blur of the backdrop `plane` holds, addressed in this surface's
    /// coordinates, mixed over this surface's own pixels at each pixel's
    /// `coverage` — `255` takes the blur, `0` keeps the pixel — rounded at the
    /// surface's ordered dither. Reports whether it was written.
    ///
    /// The blur is [`blur_halved_from`](Self::blur_halved_from)'s, so a blur
    /// retained there and mixed back by [`mix_from`](Self::mix_from) is this
    /// frost to the bit. The bands must not overlap, and this surface must hold
    /// the backdrop wherever a partial coverage mixes over it. `plane` is
    /// working memory as there.
    ///
    /// `false` is a frost the scratch could not be grown for, and leaves this
    /// surface untouched; bands covering nothing frost nothing and answer
    /// `true`.
    pub fn frost_halved_from(
        &mut self,
        plane: &mut Self,
        blurring: &Blurring<'_>,
        scratch: &mut BlurScratch,
        runner: &dyn JobRunner,
        coverage: impl Fn(u32, u32) -> u8 + Sync,
    ) -> bool {
        let (x, y, w, h) = blurring.rect;
        let Some(reach) = self.admitted(x, y, w, h) else {
            return true;
        };
        halve(
            self,
            &Lay::Mix(&coverage),
            &reach,
            plane,
            blurring,
            scratch,
            runner,
        )
    }
}

/// The cells a half-resolution blur is taken on: the columns and rows of the
/// backdrop rectangle, each cell two by two of them from its top-left.
struct Grid {
    columns: Range<u32>,
    rows: Range<u32>,
}

impl Grid {
    fn width(&self) -> u32 {
        self.columns.end.saturating_sub(self.columns.start)
    }

    fn height(&self) -> u32 {
        self.rows.end.saturating_sub(self.rows.start)
    }

    /// Cells across and down.
    fn cells(&self) -> (u32, u32) {
        (self.width().div_ceil(2), self.height().div_ceil(2))
    }

    /// The cells spreading the band `cols` × `rows`, in the plane's
    /// coordinates, reads.
    fn spread_from(&self, (cols, rows): &(Range<u32>, Range<u32>)) -> (Range<u32>, Range<u32>) {
        (
            spread_cells(&local(cols, self.columns.start), self.width()),
            spread_cells(&local(rows, self.rows.start), self.height()),
        )
    }
}

/// Blur `blurring` at half resolution from `plane` and lay it into `target` as
/// `lay` says, over the part of each band inside `reach`, reporting whether the
/// scratch could hold the blur.
///
/// Every growth the blur needs is taken before `target` is written, so a
/// refusal leaves it as it was.
fn halve<C: Fn(u32, u32) -> u8 + Sync>(
    target: &mut Surface,
    lay: &Lay<'_, C>,
    reach: &(Range<u32>, Range<u32>),
    plane: &mut Surface,
    blurring: &Blurring<'_>,
    scratch: &mut BlurScratch,
    runner: &dyn JobRunner,
) -> bool {
    let Blurring {
        rect: (x, y, w, h),
        bands,
        radius,
    } = *blurring;
    let Some((columns, rows)) = plane.admitted(x, y, w, h) else {
        return true;
    };
    let within = (clamp(&reach.0, &columns), clamp(&reach.1, &rows));
    let grid = Grid { columns, rows };
    let written = confined(bands, &within);
    if radius == 0 {
        lay_unblurred(target, lay, plane, written);
        return true;
    }
    let (across, down) = grid.cells();
    if !scratch.hold_reduced(across, down)
        || !fallible::grow_to(&mut scratch.spread, spread_line(across), 0)
    {
        return false;
    }
    let Some(mut reduced) = scratch.reduced.take() else {
        return false;
    };
    let mut cells = core::mem::take(&mut scratch.cells);
    cells.clear();
    let mut gathered = true;
    for band in written.clone() {
        if !fallible::push(&mut cells, grid.spread_from(&band)) {
            gathered = false;
            break;
        }
    }
    let half = radius / 2;
    let blurred = gathered
        && match cell_origin(&grid) {
            Some(at) => {
                reduce(plane, &grid, &mut reduced, &cells, half, runner);
                let blurring = Blurring {
                    rect: (0, 0, across, down),
                    bands: &cells,
                    radius: half,
                };
                plane.blur_from(at, &mut reduced, &blurring, scratch, runner)
            }
            None => false,
        };
    if blurred {
        spread(
            target,
            lay,
            plane,
            &grid,
            &written,
            &mut scratch.spread,
            runner,
        );
    }
    scratch.reduced = Some(reduced);
    scratch.cells = cells;
    blurred
}

/// Where the plane's own top-left lies in the coordinates of the cells: the
/// blurred cells are written back over the plane from the rectangle's
/// top-left, once every pixel of the backdrop they reduce has been read.
fn cell_origin(grid: &Grid) -> Option<(i32, i32)> {
    let back = |at: u32| i32::try_from(at).ok().map(i32::wrapping_neg);
    Some((back(grid.columns.start)?, back(grid.rows.start)?))
}

/// The lanes one row of a spread takes: a cell for every one across, and a
/// copy of each edge cell past it.
fn spread_line(across: u32) -> usize {
    index(across)
        .saturating_add(2)
        .saturating_mul(core::mem::size_of::<Cell>() / core::mem::size_of::<u16>())
}

/// `range`, counted from `start`.
fn local(range: &Range<u32>, start: u32) -> Range<u32> {
    range.start.saturating_sub(start)..range.end.saturating_sub(start)
}

/// The cells spreading the pixels `pixels` of an axis `extent` pixels long
/// reads, both counted from the axis's start.
fn spread_cells(pixels: &Range<u32>, extent: u32) -> Range<u32> {
    let last = extent.div_ceil(2).saturating_sub(1);
    let lower = |at: u32| {
        if at.is_multiple_of(2) {
            (at / 2).saturating_sub(1)
        } else {
            at / 2
        }
    };
    let upper = |at: u32| {
        if at.is_multiple_of(2) {
            at / 2
        } else {
            (at / 2).saturating_add(1).min(last)
        }
    };
    lower(pixels.start)..upper(pixels.end.saturating_sub(1)).saturating_add(1)
}

/// The two cells pixel `at` of an axis of `cells` cells is spread from, each
/// with its weight in quarters: the nearer three, the farther one.
fn taps(at: u32, cells: u32) -> [(u32, u16); 2] {
    let near = at / 2;
    if at.is_multiple_of(2) {
        [(near.saturating_sub(1), 1), (near, 3)]
    } else {
        [
            (near, 3),
            (near.saturating_add(1).min(cells.saturating_sub(1)), 1),
        ]
    }
}

/// `range` widened by `by` on both sides, within `0..limit`.
fn widened(range: &Range<u32>, by: u32, limit: u32) -> Range<u32> {
    range.start.saturating_sub(by)..range.end.saturating_add(by).min(limit)
}

/// A coordinate as the index it is.
fn index(at: u32) -> usize {
    usize::try_from(at).unwrap_or(usize::MAX)
}

/// An index as the coordinate it is.
fn coordinate(at: usize) -> u32 {
    u32::try_from(at).unwrap_or(u32::MAX)
}

/// `range` as the indices it holds.
fn indices(range: &Range<u32>) -> Range<usize> {
    index(range.start)..index(range.end)
}

/// Average the backdrop `plane` holds two by two into every cell of `reduced`
/// that blurring `cells` by `radius` reads, each cell once however many bands
/// read it.
fn reduce(
    plane: &Surface,
    grid: &Grid,
    reduced: &mut Surface,
    cells: &[(Range<u32>, Range<u32>)],
    radius: u32,
    runner: &dyn JobRunner,
) {
    let (across, down) = grid.cells();
    let read = move |(cols, rows): &(Range<u32>, Range<u32>)| {
        (widened(cols, radius, across), widened(rows, radius, down))
    };
    let rows_read = cells.iter().map(|band| indices(&read(band).1));
    // A cell reads four pixels.
    let grain = MIN_PARALLEL_FROST_PX.div_ceil(index(across).saturating_mul(4).max(1));
    let mut start = 0;
    while let Some(run) = covered_run(&rows_read, start) {
        let count = tairix_parallel::bands(runner, run.len(), grain);
        let bands = reduced.row_bands_mut(
            coordinate(run.start)..coordinate(run.end),
            band_rows(run.len(), count),
        );
        tairix_parallel::for_each_drawn(runner, bands, &|mut band| {
            for row in band.rows() {
                let spans = cells
                    .iter()
                    .map(read)
                    .filter(|(_, rows)| rows.contains(&row))
                    .map(|(cols, _)| indices(&cols));
                let mut from = 0;
                while let Some(run) = covered_run(&spans, from) {
                    let cells = coordinate(run.start)..coordinate(run.end);
                    reduce_run(plane, grid, &mut band, row, &cells);
                    from = run.end;
                }
            }
        });
        start = run.end;
    }
}

/// Average the cells `cells` of row `row` into `band` from the backdrop
/// `plane` holds: a cell past an odd edge takes its one pixel twice.
fn reduce_run(plane: &Surface, grid: &Grid, band: &mut RowBand<'_>, row: u32, cells: &Range<u32>) {
    let pair = |cell: u32, extent: u32| {
        let first = cell.saturating_mul(2);
        (first, first.saturating_add(1).min(extent.saturating_sub(1)))
    };
    let (upper, lower) = pair(row, grid.height());
    let first = cells.start.saturating_mul(2);
    let pixels = cells
        .end
        .saturating_mul(2)
        .min(grid.width())
        .saturating_sub(first);
    let column = grid.columns.start.saturating_add(first);
    let read = |row: u32| {
        plane
            .row_span(grid.rows.start.saturating_add(row), column, pixels)
            .filter(|(at, span)| *at == column && span.len() == index(pixels))
            .map(|(_, span)| span)
    };
    let (Some(top), Some(bottom)) = (read(upper), read(lower)) else {
        return;
    };
    let wide = cells.end.saturating_sub(cells.start);
    let Some((_, out)) = band
        .row_span_mut(row, cells.start, wide)
        .filter(|(at, out)| *at == cells.start && out.len() == index(wide))
    else {
        return;
    };
    let ((top_pairs, top_rest), (bottom_pairs, bottom_rest)) =
        (top.as_chunks::<2>(), bottom.as_chunks::<2>());
    let (whole, tail) = out.split_at_mut(top_pairs.len().min(out.len()));
    for ((cell, &[a, b]), &[c, d]) in whole.iter_mut().zip(top_pairs).zip(bottom_pairs) {
        *cell = quad(a, b, c, d);
    }
    if let ([cell], [a], [c]) = (tail, top_rest, bottom_rest) {
        *cell = quad(*a, *a, *c, *c);
    }
}

/// The rounded mean of four premultiplied pixels, channel by channel: the
/// `colour <= alpha` invariant holds of the mean wherever it holds of each.
fn quad(a: Pixel, b: Pixel, c: Pixel, d: Pixel) -> Pixel {
    // Four levels and the rounding stay below 1024, so the sum cannot wrap.
    let mean = |w: u8, x: u8, y: u8, z: u8| {
        let sum = u16::from(w)
            .wrapping_add(u16::from(x))
            .wrapping_add(u16::from(y))
            .wrapping_add(u16::from(z))
            .wrapping_add(2);
        u8::try_from(sum >> 2).unwrap_or(u8::MAX)
    };
    Pixel {
        r: mean(a.r, b.r, c.r, d.r),
        g: mean(a.g, b.g, c.g, d.g),
        b: mean(a.b, b.b, c.b, d.b),
        a: mean(a.a, b.a, c.a, d.a),
    }
}

/// Spread the blurred cells the plane holds over every band of `written` into
/// `target` as `lay` says, a run of the rows they cover at a time split into
/// bands of whole target rows across `runner`, each with a line of its own.
fn spread<C: Fn(u32, u32) -> u8 + Sync>(
    target: &mut Surface,
    lay: &Lay<'_, C>,
    plane: &Surface,
    grid: &Grid,
    written: &(impl Iterator<Item = (Range<u32>, Range<u32>)> + Clone + Sync),
    lines: &mut [u16],
    runner: &dyn JobRunner,
) {
    let line = spread_line(grid.cells().0).max(1);
    let at = lay.at();
    let rows_written = written.clone().map(|(_, rows)| indices(&rows));
    let grain = MIN_PARALLEL_FROST_PX.div_ceil(index(grid.width()).max(1));
    let target_row = |row: usize| at.row_in(coordinate(row)).unwrap_or(0);
    let mut start = 0;
    while let Some(run) = covered_run(&rows_written, start) {
        let count =
            tairix_parallel::bands(runner, run.len(), grain).clamp(1, (lines.len() / line).max(1));
        let work = target
            .row_bands_mut(
                target_row(run.start)..target_row(run.end),
                band_rows(run.len(), count),
            )
            .zip(lines.chunks_exact_mut(line));
        tairix_parallel::for_each_drawn(runner, work, &|(mut band, slots)| {
            for row in band.rows() {
                let Some(backdrop_row) = at.row_of(row) else {
                    continue;
                };
                let crossing = written
                    .clone()
                    .filter(|(_, rows)| rows.contains(&backdrop_row));
                for (cols, _) in crossing {
                    spread_span(&mut band, lay, plane, grid, slots, (&cols, backdrop_row));
                }
            }
        });
        start = run.end;
    }
}

/// Spread the columns `cols` of backdrop row `row` into the target row `band`
/// holds there, from the two rows of blurred cells around it taken into
/// `lanes`.
///
/// Each cell is laid into a slot for each pixel it covers, so that every pixel
/// is the same three-slot stencil — a quarter of the slot before, half its own
/// and a quarter of the slot after — which is the bilinear spread from the two
/// cells nearest its centre, weighted three to one.
fn spread_span<C: Fn(u32, u32) -> u8>(
    band: &mut RowBand<'_>,
    lay: &Lay<'_, C>,
    plane: &Surface,
    grid: &Grid,
    lanes: &mut [u16],
    (cols, row): (&Range<u32>, u32),
) {
    let (across, down) = grid.cells();
    let pixels = local(cols, grid.columns.start);
    let held = spread_cells(&pixels, grid.width());
    // A pixel at either edge reads a cell past it, a copy of the edge's own.
    let lead = u32::from(pixels.start == 0);
    let trail = u32::from(pixels.end >= across.saturating_mul(2));
    let wide = held.end.saturating_sub(held.start);
    let [(upper, upper_weight), (lower, lower_weight)] =
        taps(row.saturating_sub(grid.rows.start), down);
    let column = grid.columns.start.saturating_add(held.start);
    let read = |cell_row: u32| {
        plane
            .row_span(grid.rows.start.saturating_add(cell_row), column, wide)
            .filter(|(at, span)| *at == column && span.len() == index(wide))
            .map(|(_, span)| span)
    };
    let (cells, _) = lanes.as_chunks_mut::<8>();
    let count = index(wide.saturating_add(lead).saturating_add(trail));
    let (Some(top), Some(bottom), Some(cells)) = (read(upper), read(lower), cells.get_mut(..count))
    else {
        return;
    };
    let Some(inner) = cells.get_mut(index(lead)..) else {
        return;
    };
    for ((cell, &a), &b) in inner.iter_mut().zip(top).zip(bottom) {
        *cell = between(a, b, (upper_weight, lower_weight));
    }
    if let (1, [first, second, ..]) = (lead, &mut *cells) {
        *first = *second;
    }
    if let (1, [.., before, last]) = (trail, &mut *cells) {
        *last = *before;
    }
    let at = lay.at();
    let (Some(target_row), Some(asked)) = (at.row_in(row), at.column_in(cols.start)) else {
        return;
    };
    let Some((start, dst)) =
        band.row_span_mut(target_row, asked, cols.end.saturating_sub(cols.start))
    else {
        return;
    };
    let Some(backdrop_column) = at.column_of(start) else {
        return;
    };
    // Pixel `u`'s stencil begins at the slot of pixel `u - 1`, and the slots
    // begin at the left pixel of the first cell they hold, four lanes apiece.
    let first = i64::from(held.start) - i64::from(lead);
    let before = i64::from(backdrop_column.saturating_sub(grid.columns.start)) - 1;
    let slot = usize::try_from(before - first * 2).unwrap_or(usize::MAX);
    let Some(stencil) = lanes.get(slot.saturating_mul(4)..) else {
        return;
    };
    lay.spread(dst, stencil, (backdrop_column, row));
}

impl<C: Fn(u32, u32) -> u8> Lay<'_, C> {
    /// Spread `lanes`, which begin at the first pixel's stencil, into `dst`,
    /// which lies at `(x, y)` of the backdrop's coordinates: a run at a time
    /// into bytes on the stack, which a vector unit fills far faster than it
    /// assembles pixels.
    fn spread(&self, dst: &mut [Pixel], lanes: &[u16], (x, y): (u32, u32)) {
        let mut bytes = [0u8; SPREAD_RUN * 4];
        let mut run = [Pixel::TRANSPARENT; SPREAD_RUN];
        for (part, offset) in dst.chunks_mut(SPREAD_RUN).zip((0..).step_by(SPREAD_RUN)) {
            let (Some(line), Some(lanes)) = (
                bytes.get_mut(..part.len().saturating_mul(4)),
                lanes.get(offset * 4..),
            ) else {
                return;
            };
            spread_row(lanes, line);
            let (quads, _) = line.as_chunks::<4>();
            match self {
                Self::Field(_) => lay_bytes(part, quads),
                Self::Mix(coverage) => {
                    let Some(spread) = run.get_mut(..part.len()) else {
                        return;
                    };
                    lay_bytes(spread, quads);
                    let column = x.saturating_add(coordinate(offset));
                    mix_covered(part, spread, (column, y), coverage);
                }
            }
        }
    }
}

/// Spread `lanes` into the channel bytes `out`: each takes a quarter of its
/// lane in the slot before, half its own and a quarter of the one after,
/// rounded.
///
/// No lane passes 1020, so the stencil and its rounding stay below 4096: the
/// arithmetic cannot wrap, and runs in vector lanes rather than branching on
/// an overflow it cannot meet.
fn spread_row(lanes: &[u16], out: &mut [u8]) {
    let (Some(after), Some(centre)) = (lanes.get(8..), lanes.get(4..)) else {
        return;
    };
    for (((level, &a), &b), &c) in out.iter_mut().zip(lanes).zip(centre).zip(after) {
        let sum = a
            .wrapping_add(b.wrapping_mul(2))
            .wrapping_add(c)
            .wrapping_add(8);
        *level = u8::try_from(sum >> 4).unwrap_or(u8::MAX);
    }
}

/// Lay the channel bytes `quads` into `pixels`, a pixel apiece.
fn lay_bytes(pixels: &mut [Pixel], quads: &[[u8; 4]]) {
    for (pixel, &[r, g, b, a]) in pixels.iter_mut().zip(quads) {
        *pixel = Pixel { r, g, b, a };
    }
}

/// A cell's channels taken between the cells `upper` and `lower` of two rows,
/// weighed by the quarters `weights`, once for each pixel of the cell.
///
/// Levels are at most 255 and the weights four quarters between them, so a
/// lane never passes 1020: the arithmetic cannot wrap.
fn between(upper: Pixel, lower: Pixel, (over, under): (u16, u16)) -> Cell {
    let weigh = |p: u8, q: u8| {
        over.wrapping_mul(u16::from(p))
            .wrapping_add(under.wrapping_mul(u16::from(q)))
    };
    let (r, g, b, a) = (
        weigh(upper.r, lower.r),
        weigh(upper.g, lower.g),
        weigh(upper.b, lower.b),
        weigh(upper.a, lower.a),
    );
    [r, g, b, a, r, g, b, a]
}

/// The half-resolution blur of a whole `width` × `height` plane of `pixels`,
/// spelled as its definition: the reference every faster path is held to.
///
/// Each cell is the rounded mean of its two by two pixels, edges replicated;
/// the cells are box-blurred by half `radius`, edges replicated; and each
/// pixel is the bilinear sample of the cells a quarter of a cell up and left
/// of its centre, weighted in sixteenths and rounded.
#[cfg(any(test, feature = "test-util"))]
#[must_use]
pub fn halved_reference(
    pixels: &[Pixel],
    width: usize,
    height: usize,
    radius: u32,
) -> alloc::vec::Vec<Pixel> {
    use alloc::vec::Vec;

    if radius == 0 || width == 0 || height == 0 {
        return pixels.to_vec();
    }
    let at = |field: &[Pixel], across: usize, x: usize, y: usize| {
        field
            .get(y * across + x)
            .copied()
            .unwrap_or(Pixel::TRANSPARENT)
    };
    let (across, down) = (width.div_ceil(2), height.div_ceil(2));
    let mut cells = Vec::with_capacity(across * down);
    for j in 0..down {
        for i in 0..across {
            let sample = |dx: usize, dy: usize| {
                let (x, y) = ((2 * i + dx).min(width - 1), (2 * j + dy).min(height - 1));
                at(pixels, width, x, y)
            };
            cells.push(quad(sample(0, 0), sample(1, 0), sample(0, 1), sample(1, 1)));
        }
    }
    let reach = index(radius / 2);
    let count = coordinate(2 * reach + 1);
    let pass = |field: &[Pixel], horizontal: bool| -> Vec<Pixel> {
        let mut out = Vec::with_capacity(field.len());
        for y in 0..down {
            for x in 0..across {
                let mut sums = [0u32; 4];
                for step in 0..=2 * reach {
                    let (sx, sy) = if horizontal {
                        ((x + step).saturating_sub(reach).min(across - 1), y)
                    } else {
                        (x, (y + step).saturating_sub(reach).min(down - 1))
                    };
                    let p = at(field, across, sx, sy);
                    for (sum, level) in sums.iter_mut().zip([p.r, p.g, p.b, p.a]) {
                        *sum += u32::from(level);
                    }
                }
                let [r, g, b, a] =
                    sums.map(|sum| u8::try_from((sum + count / 2) / count).unwrap_or(u8::MAX));
                out.push(Pixel { r, g, b, a });
            }
        }
        out
    };
    let blurred = pass(&pass(&cells, true), false);
    // In quarters of a cell, the point a quarter cell up and left of pixel
    // `u`'s centre lies at `2u - 1`: its two cells and its weight toward the
    // second are read straight from that.
    let bilinear = |u: usize, extent: usize| {
        let quarter = i64::try_from(u).unwrap_or(i64::MAX) * 2 - 1;
        let low = quarter.div_euclid(4);
        let toward = u32::try_from(quarter.rem_euclid(4)).unwrap_or(0);
        let last = i64::try_from(extent - 1).unwrap_or(0);
        let cell = |at: i64| usize::try_from(at.clamp(0, last)).unwrap_or(0);
        [(cell(low), 4 - toward), (cell(low + 1), toward)]
    };
    let mut out = Vec::with_capacity(pixels.len());
    for y in 0..height {
        for x in 0..width {
            let mut sums = [0u32; 4];
            for (cy, wy) in bilinear(y, down) {
                for (cx, wx) in bilinear(x, across) {
                    let p = at(&blurred, across, cx, cy);
                    for (sum, level) in sums.iter_mut().zip([p.r, p.g, p.b, p.a]) {
                        *sum += wx * wy * u32::from(level);
                    }
                }
            }
            let [r, g, b, a] = sums.map(|sum| u8::try_from((sum + 8) / 16).unwrap_or(u8::MAX));
            out.push(Pixel { r, g, b, a });
        }
    }
    out
}

#[cfg(test)]
#[path = "halved_tests.rs"]
mod tests;
