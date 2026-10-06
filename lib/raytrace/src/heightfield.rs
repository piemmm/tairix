//! Land and sea as height grids: a height at every vertex of a square grid,
//! and between four vertices the bilinear patch they span.
//!
//! A ray finds the few cells it can meet by walking down a pyramid of maxima
//! (Tevs, Ihrke and Seidel, "Maximum Mipmaps for Fast, Accurate, and Scalable
//! Dynamic Height Field Rendering", 2008), crossing each block's four children
//! together, and meets a cell's patch where a quadratic along it says, so the
//! surface it finds is exactly the one the grid describes. A grid that wraps repeats endlessly across the plane, as
//! the open sea does, the ray walking tile to tile.
//!
//! Its heights are filled a band of rows at a time, so a caller spreads the
//! work over as many frames and cores as it has; then a [`Sealing`] builds
//! the pyramid the same way. A grid may leave out a rectangle of its cells, which
//! a finer grid over the same ground covers instead, and any cell with a
//! corner marked absent: water lies only where there is water. A land's grid
//! carries what the land is like at each vertex beside its height — how wet,
//! what the water laid down or wore away, whether a road or a path runs
//! there, how much grows, how deep snow lies — read back blended as its
//! heights are.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_parallel::JobRunner;
use tairix_util::fallible;
use tairix_util::mathf::{self, fmax, fmin};

use crate::band;
use crate::lanes::{Corners, Lanes};
use crate::shape::{quadratic, reciprocal, Aabb, Hit};
use crate::vector::{cell_of, real, share, Ray, Vec3};

/// How far a ray is followed over a wrapping grid's tiles: beyond this a swell
/// is finer than a pixel, and the grid lies at its mean level out to the
/// horizon.
const WRAP_REACH: f64 = 6_000.0;

/// A square grid of heights.
#[derive(Debug)]
pub(crate) struct Heightfield {
    /// Vertices along each side: its cells, plus one.
    side: usize,
    /// The world x and z of vertex (0, 0).
    origin: (f64, f64),
    /// The distance between neighbouring vertices.
    step: f64,
    /// Whether the grid repeats endlessly across the plane; its last row and
    /// column are then its first again.
    wrap: bool,
    /// The rows written so far. Each buffer is reserved whole when the grid
    /// is made and extended as its rows are written, so neither the making
    /// nor any one unit of the filling zeroes the whole grid, and extending
    /// never allocates.
    heights: Vec<f32>,
    /// For each level sealed so far, the highest corner of each block of
    /// cells: the first level one cell a block, each after it half as many a
    /// side, the last of an odd count holding the one cell or block left over.
    maxima: Vec<f32>,
    /// Where each level starts in `maxima`, and how many blocks it has a
    /// side.
    levels: Vec<(usize, usize)>,
    low: f64,
    high: f64,
    /// The mean of its heights, which a wrapping grid lies at past its reach.
    mean: f64,
    /// The columns and rows of cells a finer grid covers instead.
    absent: Option<(Range<usize>, Range<usize>)>,
    /// What the land is like at each vertex written so far, if the grid
    /// carries it.
    attributes: Vec<Attributes>,
    carries: bool,
}

/// A block of the pyramid a walk has still to come to, and where the ray
/// enters and leaves it, however far.
#[derive(Copy, Clone, Debug, Default)]
struct Waiting {
    level: usize,
    column: usize,
    row: usize,
    enter: f64,
    leave: f64,
}

/// A row of a grid being written: its number, its heights, and the
/// attributes there, empty for a grid that carries none.
pub(crate) type Row<'a> = (usize, &'a mut [f32], &'a mut [Attributes]);

/// The channels of what a land is like that a grid carries at each vertex:
/// how wet, what the water laid down or wore away, the road or path there,
/// how much grows, and how deep snow lies.
pub(crate) const CHANNELS: usize = 5;

/// What a land is like at one vertex, a byte to a channel.
pub(crate) type Attributes = [u8; CHANNELS];

/// The grid `written` of `fields` to write, and the grid `read` it is
/// written from; `None` unless both are there and apart.
pub(crate) fn apart(
    fields: &mut [Heightfield],
    written: usize,
    read: usize,
) -> Option<(&mut Heightfield, &Heightfield)> {
    if written == read {
        return None;
    }
    let (low, high) = fields.split_at_mut_checked(written.max(read))?;
    let (first, second) = (low.get_mut(written.min(read))?, high.first_mut()?);
    Some(if written < read {
        (first, &*second)
    } else {
        (second, &*first)
    })
}

/// The height a vertex is given where the grid has no surface.
pub(crate) const ABSENT: f32 = f32::NEG_INFINITY;

/// What a grid carrying no attributes is like everywhere: dry, neither worn
/// nor built up, on no road or path, green enough for anything to grow, and
/// bare of snow.
pub(crate) const PLAIN: [f64; CHANNELS] = [0.0, 0.5, 0.0, 1.0, 0.0];

impl Heightfield {
    /// Its rows `range`, as disjoint bands `rows` rows high, each with the
    /// row it starts at.
    pub(crate) fn bands(
        &mut self,
        range: Range<usize>,
        rows: usize,
    ) -> impl Iterator<Item = (usize, &mut [f32])> {
        self.reach(range.end);
        banded(&mut self.heights, self.side, range, rows)
    }
}

/// `values`, rows of `side`, over `range`, as disjoint bands `rows` rows
/// high, each with the row it starts at.
fn banded(
    values: &mut [f32],
    side: usize,
    range: Range<usize>,
    rows: usize,
) -> impl Iterator<Item = (usize, &mut [f32])> {
    let rows = rows.max(1);
    let start = range.start.min(side);
    let end = range.end.clamp(start, side);
    values
        .get_mut(start * side..end * side)
        .unwrap_or_default()
        .chunks_mut((rows * side).max(1))
        .enumerate()
        .map(move |(band, values)| (start + band * rows, values))
}

/// The blend of a cell's corners `[h00, h10, h01, h11]` at `(across, down)`
/// within it.
pub(crate) fn bilinear([h00, h10, h01, h11]: [f64; 4], (across, down): (f64, f64)) -> f64 {
    let top = h00 + (h10 - h00) * across;
    let bottom = h01 + (h11 - h01) * across;
    top + (bottom - top) * down
}

/// The cells each side of a grid whose vertices are `side` a side.
fn cells_of(side: usize) -> usize {
    side.saturating_sub(1)
}

impl Heightfield {
    /// A flat grid of `cells` cells a side, `step` apart, its first vertex at
    /// `origin`; `None` for a grid of no cells, or when the heap will not
    /// hold it.
    pub(crate) fn new(cells: usize, origin: (f64, f64), step: f64, wrap: bool) -> Option<Self> {
        if cells == 0 || step.is_nan() || step <= 0.0 {
            return None;
        }
        let side = cells.checked_add(1)?;
        let mut heights = Vec::new();
        if !fallible::reserve(&mut heights, side.checked_mul(side)?) {
            return None;
        }
        let mut levels = Vec::new();
        let mut total = 0usize;
        let mut blocks = cells;
        loop {
            if !fallible::reserve(&mut levels, 1) {
                return None;
            }
            levels.push((total, blocks));
            total = total.checked_add(blocks.checked_mul(blocks)?)?;
            if blocks == 1 {
                break;
            }
            blocks = blocks.div_ceil(2);
        }
        let mut maxima = Vec::new();
        if !fallible::reserve(&mut maxima, total) {
            return None;
        }
        Some(Self {
            side,
            origin,
            step,
            wrap,
            heights,
            maxima,
            levels,
            low: 0.0,
            high: 0.0,
            mean: 0.0,
            absent: None,
            attributes: Vec::new(),
            carries: false,
        })
    }

    /// Leave out the cells of `columns` and `rows`, which a finer grid covers,
    /// before the grid is sealed.
    pub(crate) fn leave_out(&mut self, columns: Range<usize>, rows: Range<usize>) {
        self.absent = Some((columns, rows));
    }

    /// Carry what the land is like at every vertex, as the rows already
    /// written do; `false` when the heap will not hold it.
    pub(crate) fn carry_attributes(&mut self) -> bool {
        if self.carries {
            return true;
        }
        if !fallible::reserve(&mut self.attributes, self.side * self.side) {
            return false;
        }
        self.carries = true;
        self.reach(self.heights.len() / self.side);
        true
    }

    /// Extend the heights, and the attributes the grid carries, over its
    /// first `rows` rows, zeroed where nothing was written: within what was
    /// reserved, so it never allocates.
    fn reach(&mut self, rows: usize) {
        let length = rows.min(self.side) * self.side;
        if self.heights.len() < length {
            self.heights.resize(length, 0.0);
        }
        if self.carries && self.attributes.len() < length {
            self.attributes.resize(length, [0; CHANNELS]);
        }
    }

    /// The heights, every row, to shape directly.
    pub(crate) fn heights_mut(&mut self) -> &mut [f32] {
        self.reach(self.side);
        &mut self.heights
    }

    /// The heights, row by row, of the rows written so far.
    pub(crate) fn heights(&self) -> &[f32] {
        &self.heights
    }

    /// The heights and the attributes both of `rows`, to set together; the
    /// attributes empty for a grid that carries none.
    pub(crate) fn rows_mut(&mut self, rows: Range<usize>) -> (&mut [f32], &mut [Attributes]) {
        let end = rows.end.min(self.side);
        self.reach(end);
        let span = rows.start.min(end) * self.side..end * self.side;
        let attributes = if self.carries {
            self.attributes.get_mut(span.clone()).unwrap_or_default()
        } else {
            &mut []
        };
        (self.heights.get_mut(span).unwrap_or_default(), attributes)
    }

    /// Visit rows `rows` with `visit`, a row a piece across `runner`.
    pub(crate) fn each_row(
        &mut self,
        rows: Range<usize>,
        runner: &dyn JobRunner,
        visit: &(dyn Fn(&mut Row<'_>) + Sync),
    ) {
        let side = self.side;
        let first = rows.start;
        let (heights, attributes) = self.rows_mut(rows);
        let mut kept = attributes.chunks_mut(side.max(1));
        let lines = (first..)
            .zip(heights.chunks_mut(side.max(1)))
            .map(move |(row, line)| (row, line, kept.next().unwrap_or_default()));
        tairix_parallel::for_each_drawn(runner, lines, &|mut row| visit(&mut row));
    }

    /// The attributes vertex `(column, row)` carries, as set; nought for a
    /// grid that carries none or a vertex beyond it.
    pub(crate) fn attributes_of(&self, column: usize, row: usize) -> Attributes {
        if column >= self.side {
            return [0; CHANNELS];
        }
        self.attributes
            .get(row * self.side + column)
            .copied()
            .unwrap_or([0; CHANNELS])
    }

    /// Vertices along each side.
    pub(crate) const fn side(&self) -> usize {
        self.side
    }

    /// The highest the sealed surface stands.
    pub(crate) const fn highest(&self) -> f64 {
        self.high
    }

    /// The world x and z of vertex `(0, 0)`, and the distance between
    /// neighbours.
    pub(crate) const fn placing(&self) -> ((f64, f64), f64) {
        (self.origin, self.step)
    }

    /// The attributes at world `(x, z)`, blended from the vertices about it,
    /// each `0.0..=1.0`; [`PLAIN`] for a grid that carries none.
    pub(crate) fn attributes_at(&self, x: f64, z: f64) -> [f64; CHANNELS] {
        if !self.carries {
            return PLAIN;
        }
        let (column, across) = self.split((x - self.origin.0) / self.step);
        let (row, down) = self.split((z - self.origin.1) / self.step);
        let at = |c: usize, r: usize| {
            self.attributes
                .get(r.min(self.side - 1) * self.side + c.min(self.side - 1))
                .copied()
                .unwrap_or([0; CHANNELS])
        };
        let corners = [
            at(column, row),
            at(column + 1, row),
            at(column, row + 1),
            at(column + 1, row + 1),
        ];
        let mut out = [0.0; CHANNELS];
        for (channel, slot) in out.iter_mut().enumerate() {
            let value = |corner: Attributes| f64::from(corner[channel]) / 255.0;
            *slot = bilinear(corners.map(value), (across, down));
        }
        out
    }

    /// Whether cell `(column, row)` has a surface.
    fn present(&self, column: usize, row: usize) -> bool {
        present(self.absent.as_ref(), column, row)
    }

    /// Seal the grid whole on the calling thread.
    #[cfg(test)]
    pub(crate) fn seal(&mut self) {
        let mut sealing = Sealing::BEGUN;
        while !sealing.step(self, &tairix_parallel::SERIAL) {}
    }

    /// The peaks of cell rows `rows` into the pyramid's first level, in bands
    /// of `per` rows across `runner`, and `before`, the extremes of the rows
    /// sealed before them, joined with those of these rows of vertices — and
    /// of the last row too, once they reach it.
    fn seal_cells(
        &mut self,
        rows: Range<usize>,
        per: usize,
        (runner, before): (&dyn JobRunner, Extremes),
    ) -> Extremes {
        let (side, cells) = (self.side, cells_of(self.side));
        let Self {
            heights,
            maxima,
            absent,
            ..
        } = self;
        maxima.resize(rows.end * cells, ABSENT);
        let peaks = maxima
            .get_mut(rows.start * cells..rows.end * cells)
            .unwrap_or_default();
        let heights: &[f32] = heights;
        let absent = absent.as_ref();
        let band = |number: usize, peaks: &mut [f32]| {
            let first = number * per;
            for (row, out) in (first..).zip(peaks.chunks_mut(cells)) {
                for (column, slot) in out.iter_mut().enumerate() {
                    let corner =
                        |c: usize, r: usize| heights.get(r * side + c).copied().unwrap_or(ABSENT);
                    let corners = [
                        corner(column, row),
                        corner(column + 1, row),
                        corner(column, row + 1),
                        corner(column + 1, row + 1),
                    ];
                    // A cell with a corner absent, or left to a finer grid, has
                    // no surface, and so no height a ray could reach.
                    let whole = corners.iter().all(|height| height.is_finite());
                    *slot = if whole && present(absent, column, row) {
                        corners[0].max(corners[1]).max(corners[2]).max(corners[3])
                    } else {
                        ABSENT
                    };
                }
            }
            let end = first + peaks.len() / cells.max(1);
            let last = if end >= cells { side } else { end };
            Extremes::of(heights.get(first * side..last * side).unwrap_or_default())
        };
        // Joined onto those before band by band, so the mean sums its bands in
        // one order however many a unit holds.
        band::fold(
            runner,
            peaks,
            (rows.start / per, per * cells),
            before,
            &band,
            &Extremes::join,
        )
    }

    /// The peaks of rows `rows` of pyramid level `level`, each the highest of
    /// the four blocks beneath it that the level below has, in bands of `per`
    /// rows across `runner`.
    fn seal_blocks(
        &mut self,
        level: usize,
        rows: Range<usize>,
        per: usize,
        runner: &dyn JobRunner,
    ) {
        let (Some(&(below_at, below)), Some(&(at, blocks))) = (
            level
                .checked_sub(1)
                .and_then(|lower| self.levels.get(lower)),
            self.levels.get(level),
        ) else {
            return;
        };
        self.maxima.resize(at + rows.end * blocks, ABSENT);
        let (lower, upper) = self.maxima.split_at_mut(at);
        let children: &[f32] = lower.get(below_at..).unwrap_or_default();
        let peaks = upper
            .get_mut(rows.start * blocks..rows.end * blocks)
            .unwrap_or_default();
        band::for_each(
            runner,
            peaks,
            (rows.start / per, per * blocks),
            &|number, peaks| {
                for (row, out) in (number * per..).zip(peaks.chunks_mut(blocks)) {
                    for (column, slot) in out.iter_mut().enumerate() {
                        let child = |c: usize, r: usize| {
                            let (column, row) = (2 * column + c, 2 * row + r);
                            if column < below && row < below {
                                children
                                    .get(row * below + column)
                                    .copied()
                                    .unwrap_or(ABSENT)
                            } else {
                                ABSENT
                            }
                        };
                        *slot = child(0, 0)
                            .max(child(1, 0))
                            .max(child(0, 1))
                            .max(child(1, 1));
                    }
                }
            },
        );
    }

    /// Take `found` as the grid's extremes and mean.
    fn settle(&mut self, found: Extremes) {
        (self.low, self.high) = if found.low <= found.high {
            (found.low, found.high)
        } else {
            (0.0, 0.0)
        };
        self.mean = if found.count > 0 {
            found.sum / real(found.count)
        } else {
            0.0
        };
    }

    /// The span one tile of the grid covers, each way.
    fn span(&self) -> f64 {
        self.step * real(cells_of(self.side))
    }

    /// The grid's height at vertex `(column, row)`, wrapping when it wraps.
    fn at(&self, column: usize, row: usize) -> f64 {
        f64::from(
            self.heights
                .get(self.vertex(column, row))
                .copied()
                .unwrap_or(0.0),
        )
    }

    /// Where vertex `(column, row)` is kept among the grid's: round the far
    /// side for a grid that wraps, its edge's for one that does not.
    fn vertex(&self, column: usize, row: usize) -> usize {
        let cells = cells_of(self.side);
        let (column, row) = if self.wrap {
            (column % cells.max(1), row % cells.max(1))
        } else {
            (column.min(cells), row.min(cells))
        };
        row * self.side + column
    }

    /// The first column and row of the cells the rectangle from `(x0, z0)`
    /// to `(x1, z1)` lies over, and how many of each, counted round the far
    /// side for a grid that wraps.
    fn spanned(
        &self,
        (x0, z0): (f64, f64),
        (x1, z1): (f64, f64),
    ) -> ((usize, usize), (usize, usize)) {
        let cells = cells_of(self.side).max(1);
        let column = |x: f64| self.split((x - self.origin.0) / self.step).0;
        let row = |z: f64| self.split((z - self.origin.1) / self.step).0;
        let span = |first: usize, last: usize| (last + cells - first) % cells + 1;
        let (first_column, first_row) = (column(x0), row(z0));
        (
            (first_column, span(first_column, column(x1))),
            (first_row, span(first_row, row(z1))),
        )
    }

    /// The vertices either side of `at` along a side, and how many steps
    /// apart they are: round to the far side of a grid that wraps, and no
    /// further than the edge of one that does not.
    fn around(&self, at: usize) -> (usize, usize, f64) {
        let cells = cells_of(self.side);
        if self.wrap {
            let before = at.checked_sub(1).unwrap_or(cells.saturating_sub(1));
            return (before, at + 1, 2.0);
        }
        let (before, after) = (at.saturating_sub(1), (at + 1).min(cells));
        (before, after, real(after - before))
    }

    /// The box the grid lies in, or `None` for one without end.
    pub(crate) fn bounds(&self) -> Option<Aabb> {
        (!self.wrap).then(|| Aabb {
            min: Vec3::new(self.origin.0, self.low, self.origin.1),
            max: Vec3::new(
                self.origin.0 + self.span(),
                self.high,
                self.origin.1 + self.span(),
            ),
        })
    }

    /// The surface's height at world `(x, z)`, the patch beneath it read
    /// there; the nearest edge's for a point beyond a grid that does not
    /// wrap.
    pub(crate) fn height_at(&self, x: f64, z: f64) -> f64 {
        let (u, v) = (
            (x - self.origin.0) / self.step,
            (z - self.origin.1) / self.step,
        );
        let (column, across) = self.split(u);
        let (row, down) = self.split(v);
        let corners = [
            self.at(column, row),
            self.at(column + 1, row),
            self.at(column, row + 1),
            self.at(column + 1, row + 1),
        ];
        bilinear(corners, (across, down))
    }

    /// The cell a grid coordinate falls in, and how far across it.
    fn split(&self, at: f64) -> (usize, f64) {
        let cells = cells_of(self.side);
        let whole = mathf::floor(at);
        let fraction = at - whole;
        let cells_f = real(cells);
        let whole = if self.wrap {
            whole - mathf::floor(whole / cells_f) * cells_f
        } else if whole < 0.0 {
            return (0, 0.0);
        } else if whole >= cells_f {
            return (cells.saturating_sub(1), 1.0);
        } else {
            whole
        };
        (
            cell_of(whole).0.min(cells.saturating_sub(1)),
            fraction.clamp(0.0, 1.0),
        )
    }

    /// The nearest place in `(near, far)` along `ray` where it meets the
    /// surface.
    pub(crate) fn intersect(&self, ray: &Ray, near: f64, far: f64) -> Option<Hit> {
        if self.wrap {
            self.intersect_tiles(ray, near, far)
        } else {
            let bounds = self.bounds()?;
            let entry = bounds
                .padded()
                .entry(ray, reciprocal(ray.dir), far)?
                .max(near);
            self.walk(ray, (0.0, 0.0), entry, far)
        }
    }

    /// Across a grid that wraps: the slab its heights lie in, then each tile
    /// the ray crosses within it, nearest first.
    fn intersect_tiles(&self, ray: &Ray, near: f64, far: f64) -> Option<Hit> {
        let (low, high) = (self.low - 1e-6, self.high + 1e-6);
        let (mut start, mut end) = (near, far.min(WRAP_REACH));
        if ray.dir.y.abs() < 1e-12 {
            if ray.origin.y < low || ray.origin.y > high {
                return None;
            }
        } else {
            let (a, b) = (
                (low - ray.origin.y) / ray.dir.y,
                (high - ray.origin.y) / ray.dir.y,
            );
            start = start.max(a.min(b));
            end = end.min(a.max(b));
        }
        if start >= end {
            return self.beyond_reach(ray, far);
        }
        let span = self.span();
        let entry = ray.at(start);
        let tile = |at: f64, origin: f64| mathf::floor((at - origin) / span);
        let (mut tx, mut tz) = (tile(entry.x, self.origin.0), tile(entry.z, self.origin.1));
        let steps = |dir: f64| if dir > 0.0 { 1.0 } else { -1.0 };
        let (sx, sz) = (steps(ray.dir.x), steps(ray.dir.z));
        // Where the ray crosses into the next tile along each axis.
        let crossing = |origin: f64, t: f64, o: f64, d: f64, s: f64| {
            if d.abs() < 1e-12 {
                f64::INFINITY
            } else {
                (origin + (t + if s > 0.0 { 1.0 } else { 0.0 }) * span - o) / d
            }
        };
        let mut t = start;
        while t < end {
            let (next_x, next_z) = (
                crossing(self.origin.0, tx, ray.origin.x, ray.dir.x, sx),
                crossing(self.origin.1, tz, ray.origin.z, ray.dir.z, sz),
            );
            let leave = next_x.min(next_z).min(end);
            let offset = (tx * span, tz * span);
            if let Some(hit) = self.walk(ray, offset, t, leave) {
                return Some(hit);
            }
            if next_x < next_z {
                tx += sx;
            } else {
                tz += sz;
            }
            t = leave;
        }
        self.beyond_reach(ray, far)
    }

    /// Where `ray`, having crossed a wrapping grid's tiles to their reach
    /// without meeting it, meets the grid's mean level beyond them, nearer
    /// than `far`: so the sea runs on to the horizon rather than stopping
    /// short of it. A ray already below that level there meets it at the
    /// reach itself.
    fn beyond_reach(&self, ray: &Ray, far: f64) -> Option<Hit> {
        if ray.dir.y >= 0.0 || ray.origin.y <= self.mean || far <= WRAP_REACH {
            return None;
        }
        let t = ((self.mean - ray.origin.y) / ray.dir.y).max(WRAP_REACH);
        (t < far).then(|| Hit::plain(t, Vec3::UP))
    }

    /// Walk the pyramid of the tile offset by `offset` for the ray's nearest
    /// meeting with it within `(from, to)`.
    fn walk(&self, ray: &Ray, offset: (f64, f64), from: f64, to: f64) -> Option<Hit> {
        let mut best: Option<Hit> = None;
        self.descend(ray, offset, 0.0, (from, to), |cell, corner, span| {
            // A cell's patch is met only while the ray is over the cell: past
            // it, the patch's own surface runs on where the grid has none.
            let hit = self.patch(ray, cell, corner, span)?;
            best = Some(hit);
            Some(hit.t)
        });
        best
    }

    /// Where `ray` first comes within `lift` above the surface in
    /// `(from, to)`: its entry into the nearest cell whose highest corner,
    /// raised by `lift`, it passes beneath; `None` when it passes above them
    /// all. A grid that wraps answers `from`, having no pyramid of its own
    /// for the tiles beyond the first.
    pub(crate) fn approach(&self, ray: &Ray, lift: f64, (from, to): (f64, f64)) -> Option<f64> {
        if self.wrap {
            return Some(from);
        }
        let mut first: Option<f64> = None;
        self.descend(ray, (0.0, 0.0), lift, (from, to), |_, _, (enter, _)| {
            first = Some(first.map_or(enter, |held| held.min(enter)));
            Some(enter)
        });
        first
    }

    /// The highest the surface stands over the rectangle from `(x0, z0)` to
    /// `(x1, z1)`: the highest corner of every cell it lies over.
    pub(crate) fn highest_over(&self, from: (f64, f64), to: (f64, f64)) -> f64 {
        let cells = cells_of(self.side).max(1);
        let ((first_column, columns), (first_row, rows)) = self.spanned(from, to);
        let mut peak = f64::NEG_INFINITY;
        for down in 0..rows {
            for across in 0..columns {
                let at = ((first_row + down) % cells) * cells + (first_column + across) % cells;
                peak = fmax(
                    peak,
                    f64::from(self.maxima.get(at).copied().unwrap_or(f32::MAX)),
                );
            }
        }
        peak
    }

    /// The most channel `channel` of what the land is like stands anywhere
    /// over the rectangle `(x0, z0)`–`(x1, z1)`: the most at any corner of
    /// the cells it covers, which every place within is blended from; its
    /// [`PLAIN`] value for a grid that carries none.
    pub(crate) fn most_of(&self, channel: usize, from: (f64, f64), to: (f64, f64)) -> f64 {
        if !self.carries {
            return PLAIN.get(channel).copied().unwrap_or(0.0);
        }
        let ((first_column, columns), (first_row, rows)) = self.spanned(from, to);
        let mut most = 0u8;
        for down in 0..=rows {
            for across in 0..=columns {
                let value = self
                    .attributes
                    .get(self.vertex(first_column + across, first_row + down))
                    .and_then(|attributes| attributes.get(channel))
                    .copied()
                    .unwrap_or(0);
                most = most.max(value);
            }
        }
        f64::from(most) / 255.0
    }

    /// The lowest the surface lies over the rectangle `(x0, z0)`–`(x1, z1)`:
    /// the lowest of the corners of the cells it covers, since a cell's patch
    /// lies nowhere below its lowest corner.
    pub(crate) fn lowest_over(&self, from: (f64, f64), to: (f64, f64)) -> f64 {
        let ((first_column, columns), (first_row, rows)) = self.spanned(from, to);
        let mut least = f64::INFINITY;
        for down in 0..=rows {
            for across in 0..=columns {
                least = fmin(least, self.at(first_column + across, first_row + down));
            }
        }
        least
    }

    /// Walk the pyramid of the tile offset by `offset`, the nearer of each
    /// block's children first, through the blocks the ray passes beneath
    /// within `(from, to)` once each is raised by `lift`: each cell reached
    /// is handed to `reached` with its column and row, its first corner, and
    /// where the ray is over it, and a reach it answers cuts the walk short.
    ///
    /// A block's four children are crossed together, a lane each, and each
    /// is still judged against the reach as it stands when the walk comes to
    /// it, so the cells reached and what each is told are as one block at a
    /// time would find them.
    fn descend(
        &self,
        ray: &Ray,
        offset: (f64, f64),
        lift: f64,
        (from, to): (f64, f64),
        mut reached: impl FnMut((usize, usize), (f64, f64), (f64, f64)) -> Option<f64>,
    ) {
        let inverse = reciprocal(ray.dir);
        let Some(top) = self.levels.len().checked_sub(1) else {
            return;
        };
        let x_of = |column: usize, cells: usize| {
            self.origin.0 + offset.0 + self.step * real(column * cells)
        };
        let z_of =
            |row: usize, cells: usize| self.origin.1 + offset.1 + self.step * real(row * cells);
        let top_peak = self.peak(top, 0, 0);
        // No surface, so nothing to reach; and its box, topped at minus
        // infinity, is one the slab test cannot be given.
        if top_peak == ABSENT {
            return;
        }
        let top_cells = 1usize << top;
        let (x0, z0, size) = (
            x_of(0, top_cells),
            z_of(0, top_cells),
            self.step * real(top_cells),
        );
        let (enter, leave) = Aabb {
            min: Vec3::new(x0, self.low, z0),
            max: Vec3::new(x0 + size, f64::from(top_peak) + lift, z0 + size),
        }
        .padded()
        .crossing(ray, inverse);
        let mut stack = [Waiting::default(); 64];
        stack[0] = Waiting {
            level: top,
            column: 0,
            row: 0,
            enter,
            leave,
        };
        let mut pending = 1usize;
        // A block's children lie in lanes as column + 2 * row. Over the ground
        // the ray passes from the child on the side it comes from to the one
        // it heads for, crossing at most one of the other two: nearest first.
        let near = usize::from(ray.dir.x < 0.0) + 2 * usize::from(ray.dir.z < 0.0);
        let mut reach = to;
        while pending > 0 {
            pending -= 1;
            let block = stack[pending];
            let (enter, leave) = (fmax(block.enter, from), fmin(block.leave, reach));
            if enter >= leave {
                continue;
            }
            if block.level == 0 {
                let corner = (x_of(block.column, 1), z_of(block.row, 1));
                if let Some(cut) = reached((block.column, block.row), corner, (enter, leave)) {
                    reach = fmin(reach, cut);
                }
                continue;
            }
            let level = block.level - 1;
            let cells = 1usize << level;
            let columns = [2 * block.column, 2 * block.column + 1];
            let rows = [2 * block.row, 2 * block.row + 1];
            let peaks =
                [0, 1, 2, 3].map(|lane| self.peak(level, columns[lane & 1], rows[lane >> 1]));
            let size = self.step * real(cells);
            let (xs, zs) = (
                columns.map(|column| x_of(column, cells)),
                rows.map(|row| z_of(row, cells)),
            );
            let children = Corners {
                min: [
                    Lanes([0, 1, 2, 3].map(|lane| xs[lane & 1])),
                    Lanes([self.low; 4]),
                    Lanes([0, 1, 2, 3].map(|lane| zs[lane >> 1])),
                ],
                max: [
                    Lanes([0, 1, 2, 3].map(|lane| xs[lane & 1] + size)),
                    Lanes(peaks.map(|peak| f64::from(peak) + lift)),
                    Lanes([0, 1, 2, 3].map(|lane| zs[lane >> 1] + size)),
                ],
            };
            let (Lanes(enters), Lanes(leaves)) = children.padded().crossing(ray.origin, inverse);
            // The nearest pushed last, so walked first. A child out of reach
            // now is out of reach when the walk would come to it.
            for lane in [near ^ 3, near ^ 2, near ^ 1, near] {
                if peaks[lane] == ABSENT || fmax(enters[lane], from) >= fmin(leaves[lane], reach) {
                    continue;
                }
                if let Some(slot) = stack.get_mut(pending) {
                    *slot = Waiting {
                        level,
                        column: columns[lane & 1],
                        row: rows[lane >> 1],
                        enter: enters[lane],
                        leave: leaves[lane],
                    };
                    pending += 1;
                }
            }
        }
    }

    /// The highest any cell of block `(column, row)` of `level` stands:
    /// [`ABSENT`] where none has a surface.
    fn peak(&self, level: usize, column: usize, row: usize) -> f32 {
        match self.levels.get(level) {
            Some(&(at, blocks)) if column < blocks && row < blocks => self
                .maxima
                .get(at + row * blocks + column)
                .copied()
                .unwrap_or(ABSENT),
            _ => ABSENT,
        }
    }

    /// Where the ray meets the patch of cell `(column, row)`, whose first
    /// corner is at `(x0, z0)`, within `(from, to)`.
    fn patch(
        &self,
        ray: &Ray,
        (column, row): (usize, usize),
        (x0, z0): (f64, f64),
        (from, to): (f64, f64),
    ) -> Option<Hit> {
        let (h00, h10) = (self.at(column, row), self.at(column + 1, row));
        let (h01, h11) = (self.at(column, row + 1), self.at(column + 1, row + 1));
        let whole = h00.is_finite() && h10.is_finite() && h01.is_finite() && h11.is_finite();
        if !(whole && self.present(column, row)) {
            return None;
        }
        // In the cell's own coordinates, from where the ray enters it.
        let start = ray.at(from);
        let (au, av) = ((start.x - x0) / self.step, (start.z - z0) / self.step);
        let (bu, bv) = (ray.dir.x / self.step, ray.dir.z / self.step);
        let (rise_u, rise_v, twist) = (h10 - h00, h01 - h00, h00 - h10 - h01 + h11);
        let flat = h00 + rise_u * au + rise_v * av + twist * au * av;
        let slope = rise_u * bu + rise_v * bv + twist * (au * bv + av * bu);
        let curve = twist * bu * bv;
        // y(t) - h(t) = 0, in t from the entry.
        let (qa, qb, qc) = (-curve, ray.dir.y - slope, start.y - flat);
        let span = to - from;
        let root = smallest_root(qa, qb, qc, span)?;
        let t = from + root;
        let (u, v) = (
            (au + bu * root).clamp(0.0, 1.0),
            (av + bv * root).clamp(0.0, 1.0),
        );
        let du = (rise_u + twist * v) / self.step;
        let dv = (rise_v + twist * u) / self.step;
        Some(Hit {
            t,
            normal: Vec3::new(-du, 1.0, -dv).normalized(),
            shading: self.smooth_normal((column, row), (u, v)),
            mark: 0,
            along: 0.0,
            uv: (0.0, 0.0),
            girth: 0.0,
            material: None,
            tangent: Vec3::ZERO,
            relieved: false,
            member: None,
        })
    }

    /// The normal of the smooth surface through the grid at `(u, v)` of cell
    /// `(column, row)`: its vertices' own normals, blended.
    fn smooth_normal(&self, (column, row): (usize, usize), (u, v): (f64, f64)) -> Vec3 {
        // A neighbour with no height stands at the vertex's own.
        let gradient = |c: usize, r: usize| {
            let ((left, right, across), (back, front, down)) = (self.around(c), self.around(r));
            let here = self.at(c, r);
            let read = |c: usize, r: usize| {
                let height = self.at(c, r);
                if height.is_finite() {
                    height
                } else {
                    here
                }
            };
            (
                (read(right, r) - read(left, r)) / (across * self.step),
                (read(c, front) - read(c, back)) / (down * self.step),
            )
        };
        let blend =
            |a: (f64, f64), b: (f64, f64), t: f64| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        let top = blend(gradient(column, row), gradient(column + 1, row), u);
        let bottom = blend(gradient(column, row + 1), gradient(column + 1, row + 1), u);
        let (gx, gz) = blend(top, bottom, v);
        Vec3::new(-gx, 1.0, -gz).normalized()
    }
}

/// Whether cell `(column, row)` has a surface, `absent` the rectangle of
/// cells a finer grid covers instead.
fn present(absent: Option<&(Range<usize>, Range<usize>)>, column: usize, row: usize) -> bool {
    absent.is_none_or(|(columns, rows)| !(columns.contains(&column) && rows.contains(&row)))
}

/// About how many cells a band of a grid's sealing holds, in whole rows: a
/// count of its own rather than the runner's, so the mean sums the same
/// however many cores share the bands.
const SEAL_CELLS: usize = 8192;

/// A grid being sealed a band of rows at a time: its extremes and its mean,
/// then each level of its pyramid from the one below.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Sealing {
    /// The pyramid level being built, and its next row.
    level: usize,
    row: usize,
    extremes: Extremes,
}

impl Sealing {
    /// A sealing not yet begun.
    pub(crate) const BEGUN: Self = Self {
        level: 0,
        row: 0,
        extremes: Extremes::NONE,
    };

    /// Seal the next unit of `field` across `runner`: whether it is sealed.
    pub(crate) fn step(&mut self, field: &mut Heightfield, runner: &dyn JobRunner) -> bool {
        field.reach(field.side);
        let mut budget = SEAL_CELLS.saturating_mul(runner.width().max(1));
        while let Some(&(_, blocks)) = field.levels.get(self.level) {
            if budget == 0 {
                return false;
            }
            let per = (SEAL_CELLS / blocks).max(1);
            let bands = (budget / (per * blocks)).max(1);
            let rows = self.row..(self.row + bands * per).min(blocks);
            if self.level == 0 {
                self.extremes = field.seal_cells(rows.clone(), per, (runner, self.extremes));
            } else {
                field.seal_blocks(self.level, rows.clone(), per, runner);
            }
            budget = budget.saturating_sub(rows.len() * blocks);
            self.row = rows.end;
            if self.row < blocks {
                return false;
            }
            if self.level == 0 {
                field.settle(self.extremes);
            }
            self.level += 1;
            self.row = 0;
        }
        true
    }

    /// How far the sealing of `field` has come, as a share of its pyramid.
    pub(crate) fn done(&self, field: &Heightfield) -> f64 {
        let area = |&(_, blocks): &(usize, usize)| blocks * blocks;
        let sealed: usize = field.levels.iter().take(self.level).map(area).sum();
        let within = field
            .levels
            .get(self.level)
            .map_or(0, |&(_, blocks)| self.row * blocks);
        share(sealed + within, field.levels.iter().map(area).sum())
    }
}

/// The least, the greatest and the sum of a run of heights with a surface,
/// and how many there were.
#[derive(Copy, Clone, Debug)]
struct Extremes {
    low: f64,
    high: f64,
    sum: f64,
    count: usize,
}

impl Extremes {
    /// Of no heights at all.
    const NONE: Self = Self {
        low: f64::INFINITY,
        high: f64::NEG_INFINITY,
        sum: 0.0,
        count: 0,
    };

    fn of(heights: &[f32]) -> Self {
        heights
            .iter()
            .filter(|height| height.is_finite())
            .fold(Self::NONE, |found, &height| {
                let height = f64::from(height);
                Self {
                    low: fmin(found.low, height),
                    high: fmax(found.high, height),
                    sum: found.sum + height,
                    count: found.count + 1,
                }
            })
    }

    /// These and the heights after them.
    fn join(self, after: Self) -> Self {
        Self {
            low: fmin(self.low, after.low),
            high: fmax(self.high, after.high),
            sum: self.sum + after.sum,
            count: self.count + after.count,
        }
    }
}

/// The least root of `a t² + b t + c` in `(0, reach]`.
fn smallest_root(a: f64, b: f64, c: f64, reach: f64) -> Option<f64> {
    let (near, far) = quadratic(a, 0.5 * b, c)?;
    [near, far].into_iter().find(|t| *t > 0.0 && *t <= reach)
}

#[cfg(test)]
#[path = "heightfield_tests.rs"]
mod tests;
