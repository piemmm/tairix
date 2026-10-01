//! Land and sea as height grids: a height at every vertex of a square grid,
//! and between four vertices the bilinear patch they span.
//!
//! A ray finds the few cells it can meet by walking down a pyramid of maxima
//! (Tevs, Ihrke and Seidel, "Maximum Mipmaps for Fast, Accurate, and Scalable
//! Dynamic Height Field Rendering", 2008) and meets a cell's patch where a
//! quadratic along it says, so the surface it finds is exactly the one the
//! grid describes. A grid that wraps repeats endlessly across the plane, as
//! the open sea does, the ray walking tile to tile.
//!
//! Its heights are filled a band of rows at a time, so a caller spreads the
//! work over as many frames and cores as it has; then [`Heightfield::seal`]
//! builds the pyramid. A grid may leave out a rectangle of its cells, which
//! a finer grid over the same ground covers instead, and any cell with a
//! corner marked absent: water lies only where there is water. A land's grid
//! carries what the land is like at each vertex beside its height — how wet,
//! what the water laid down or wore away, whether a road or a path runs
//! there, how much grows — read back blended as its heights are.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_util::{fallible, mathf};

use crate::scene::{Grid, Layout};
use crate::shape::{quadratic, reciprocal, Aabb, Hit};
use crate::vector::{real, Ray, Vec3};

/// How far a ray is followed across a wrapping grid before it is taken to
/// have passed over it: beyond this, the haze has taken the horizon.
const WRAP_REACH: f64 = 6_000.0;

/// A square grid of heights.
#[derive(Debug)]
pub(crate) struct Heightfield {
    /// Vertices along each side: a power of two, plus one.
    side: usize,
    /// The world x and z of vertex (0, 0).
    origin: (f64, f64),
    /// The distance between neighbouring vertices.
    step: f64,
    /// Whether the grid repeats endlessly across the plane; its last row and
    /// column are then its first again.
    wrap: bool,
    heights: Vec<f32>,
    /// For each level, the highest corner of each block of cells: the first
    /// level one cell a block, each after it half as many a side.
    maxima: Vec<f32>,
    /// Where each level starts in `maxima`, and how many blocks it has a
    /// side.
    levels: Vec<(usize, usize)>,
    low: f64,
    high: f64,
    /// The columns and rows of cells a finer grid covers instead.
    absent: Option<(Range<usize>, Range<usize>)>,
    /// What the land is like at each vertex, four bytes of it; empty for a
    /// grid that says nothing but its heights.
    attributes: Vec<[u8; 4]>,
}

/// The height a vertex is given where the grid has no surface.
pub(crate) const ABSENT: f32 = f32::NEG_INFINITY;

/// What a grid carrying no attributes is like everywhere: dry, neither worn
/// nor built up, on no road or path, and green enough for anything to grow.
pub(crate) const PLAIN: [f64; 4] = [0.0, 0.5, 0.0, 1.0];

impl Grid for Heightfield {
    fn rows(&self) -> usize {
        self.side
    }

    fn layout(&self) -> Layout {
        Layout {
            origin: self.origin,
            step: self.step,
        }
    }

    fn bands(
        &mut self,
        range: Range<usize>,
        rows: usize,
    ) -> impl Iterator<Item = (usize, &mut [f32])> {
        banded(&mut self.heights, self.side, range, rows)
    }
}

/// `values`, rows of `side`, over `range`, as disjoint bands `rows` rows
/// high, each with the row it starts at.
pub(crate) fn banded(
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
    /// A flat grid of `cells` cells a side (a power of two), `step` apart,
    /// its first vertex at `origin`; `None` for a size that is not a power
    /// of two, or when the heap will not hold it.
    pub(crate) fn new(cells: usize, origin: (f64, f64), step: f64, wrap: bool) -> Option<Self> {
        if !cells.is_power_of_two() || step.is_nan() || step <= 0.0 {
            return None;
        }
        let side = cells + 1;
        let heights = fallible::filled(side.checked_mul(side)?, 0.0f32)?;
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
            blocks /= 2;
        }
        Some(Self {
            side,
            origin,
            step,
            wrap,
            heights,
            maxima: fallible::filled(total, 0.0f32)?,
            levels,
            low: 0.0,
            high: 0.0,
            absent: None,
            attributes: Vec::new(),
        })
    }

    /// Leave out the cells of `columns` and `rows`, which a finer grid covers,
    /// before the grid is sealed.
    pub(crate) fn leave_out(&mut self, columns: Range<usize>, rows: Range<usize>) {
        self.absent = Some((columns, rows));
    }

    /// Carry four bytes of attributes at every vertex; `false` when the heap
    /// will not hold them.
    pub(crate) fn carry_attributes(&mut self) -> bool {
        match fallible::filled(self.heights.len(), [0u8; 4]) {
            Some(attributes) => {
                self.attributes = attributes;
                true
            }
            None => false,
        }
    }

    /// The heights, row by row, to shape directly.
    pub(crate) fn heights_mut(&mut self) -> &mut [f32] {
        &mut self.heights
    }

    /// The heights, row by row.
    pub(crate) fn heights(&self) -> &[f32] {
        &self.heights
    }

    /// The attributes, row by row, to set; empty for a grid that carries none.
    pub(crate) fn attributes_mut(&mut self) -> &mut [[u8; 4]] {
        &mut self.attributes
    }

    /// The heights and the attributes both, row by row, to set together.
    pub(crate) fn surfaces_mut(&mut self) -> (&mut [f32], &mut [[u8; 4]]) {
        (&mut self.heights, &mut self.attributes)
    }

    /// The attributes vertex `(column, row)` carries, as set; nought for a
    /// grid that carries none or a vertex beyond it.
    pub(crate) fn attributes_of(&self, column: usize, row: usize) -> [u8; 4] {
        if column >= self.side {
            return [0; 4];
        }
        self.attributes
            .get(row * self.side + column)
            .copied()
            .unwrap_or([0; 4])
    }

    /// Vertices along each side.
    pub(crate) const fn side(&self) -> usize {
        self.side
    }

    /// The world x and z of vertex `(0, 0)`, and the distance between
    /// neighbours.
    pub(crate) const fn placing(&self) -> ((f64, f64), f64) {
        (self.origin, self.step)
    }

    /// The attributes at world `(x, z)`, blended from the vertices about it,
    /// each `0.0..=1.0`; [`PLAIN`] for a grid that carries none.
    pub(crate) fn attributes_at(&self, x: f64, z: f64) -> [f64; 4] {
        if self.attributes.is_empty() {
            return PLAIN;
        }
        let (column, across) = self.split((x - self.origin.0) / self.step);
        let (row, down) = self.split((z - self.origin.1) / self.step);
        let at = |c: usize, r: usize| {
            self.attributes
                .get(r.min(self.side - 1) * self.side + c.min(self.side - 1))
                .copied()
                .unwrap_or([0; 4])
        };
        let corners = [
            at(column, row),
            at(column + 1, row),
            at(column, row + 1),
            at(column + 1, row + 1),
        ];
        let mut out = [0.0; 4];
        for (channel, slot) in out.iter_mut().enumerate() {
            let value = |corner: [u8; 4]| f64::from(corner[channel]) / 255.0;
            *slot = bilinear(corners.map(value), (across, down));
        }
        out
    }

    /// Whether cell `(column, row)` has a surface.
    fn present(&self, column: usize, row: usize) -> bool {
        if let Some((columns, rows)) = &self.absent {
            if columns.contains(&column) && rows.contains(&row) {
                return false;
            }
        }
        true
    }

    /// Settle the grid once every row is filled: its extremes, and the
    /// pyramid a ray walks.
    pub(crate) fn seal(&mut self) {
        let side = self.side;
        let (mut low, mut high) = (f64::INFINITY, f64::NEG_INFINITY);
        for &height in self.heights.iter().filter(|height| height.is_finite()) {
            low = low.min(f64::from(height));
            high = high.max(f64::from(height));
        }
        (self.low, self.high) = if low <= high { (low, high) } else { (0.0, 0.0) };
        let cells = cells_of(side);
        for row in 0..cells {
            for column in 0..cells {
                let corner =
                    |c: usize, r: usize| self.heights.get(r * side + c).copied().unwrap_or(ABSENT);
                let corners = [
                    corner(column, row),
                    corner(column + 1, row),
                    corner(column, row + 1),
                    corner(column + 1, row + 1),
                ];
                // A cell with a corner absent, or left to a finer grid, has no
                // surface, and so no height a ray could reach.
                let whole = corners.iter().all(|height| height.is_finite());
                let peak = if whole && self.present(column, row) {
                    corners[0].max(corners[1]).max(corners[2]).max(corners[3])
                } else {
                    ABSENT
                };
                if let Some(slot) = self.maxima.get_mut(row * cells + column) {
                    *slot = peak;
                }
            }
        }
        for level in 1..self.levels.len() {
            let ((below, below_side), (at, blocks)) = (self.levels[level - 1], self.levels[level]);
            for row in 0..blocks {
                for column in 0..blocks {
                    let child = |c: usize, r: usize| {
                        self.maxima
                            .get(below + (2 * row + r) * below_side + 2 * column + c)
                            .copied()
                            .unwrap_or(f32::MIN)
                    };
                    let peak = child(0, 0)
                        .max(child(1, 0))
                        .max(child(0, 1))
                        .max(child(1, 1));
                    if let Some(slot) = self.maxima.get_mut(at + row * blocks + column) {
                        *slot = peak;
                    }
                }
            }
        }
    }

    /// The span one tile of the grid covers, each way.
    fn span(&self) -> f64 {
        self.step * real(cells_of(self.side))
    }

    /// The grid's height at vertex `(column, row)`, wrapping when it wraps.
    fn at(&self, column: usize, row: usize) -> f64 {
        let cells = cells_of(self.side);
        let (column, row) = if self.wrap {
            (column % cells, row % cells)
        } else {
            (column.min(cells), row.min(cells))
        };
        f64::from(
            self.heights
                .get(row * self.side + column)
                .copied()
                .unwrap_or(0.0),
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
            count(whole).min(cells.saturating_sub(1)),
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
            return None;
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
        None
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
    pub(crate) fn highest_over(&self, (x0, z0): (f64, f64), (x1, z1): (f64, f64)) -> f64 {
        let cells = cells_of(self.side).max(1);
        let column = |x: f64| self.split((x - self.origin.0) / self.step).0;
        let row = |z: f64| self.split((z - self.origin.1) / self.step).0;
        // Counted round the far side for a grid that wraps.
        let span = |first: usize, last: usize| (last + cells - first) % cells + 1;
        let (first_column, first_row) = (column(x0), row(z0));
        let mut peak = f64::NEG_INFINITY;
        for down in 0..span(first_row, row(z1)) {
            for across in 0..span(first_column, column(x1)) {
                let at = ((first_row + down) % cells) * cells + (first_column + across) % cells;
                peak = peak.max(f64::from(self.maxima.get(at).copied().unwrap_or(f32::MAX)));
            }
        }
        peak
    }

    /// The lowest the surface lies over the rectangle `(x0, z0)`–`(x1, z1)`:
    /// the lowest of the corners of the cells it covers, since a cell's patch
    /// lies nowhere below its lowest corner.
    pub(crate) fn lowest_over(&self, (x0, z0): (f64, f64), (x1, z1): (f64, f64)) -> f64 {
        let cells = cells_of(self.side).max(1);
        let column = |x: f64| self.split((x - self.origin.0) / self.step).0;
        let row = |z: f64| self.split((z - self.origin.1) / self.step).0;
        let span = |first: usize, last: usize| (last + cells - first) % cells + 2;
        let (first_column, first_row) = (column(x0), row(z0));
        let mut least = f64::INFINITY;
        for down in 0..span(first_row, row(z1)) {
            for across in 0..span(first_column, column(x1)) {
                least = least.min(self.at(first_column + across, first_row + down));
            }
        }
        least
    }

    /// Walk the pyramid of the tile offset by `offset`, the nearer of each
    /// block's children first, through the blocks the ray passes beneath
    /// within `(from, to)` once each is raised by `lift`: each cell reached
    /// is handed to `reached` with its column and row, its first corner, and
    /// where the ray is over it, and a reach it answers cuts the walk short.
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
        let mut stack = [(0usize, 0usize, 0usize); 64];
        let mut pending = 1usize;
        stack[0] = (top, 0, 0);
        let mut reach = to;
        while pending > 0 {
            pending -= 1;
            let (level, column, row) = stack[pending];
            let (at, blocks) = self.levels[level];
            let cells = 1usize << level;
            let peak = f64::from(
                self.maxima
                    .get(at + row * blocks + column)
                    .copied()
                    .unwrap_or(f32::MIN),
            );
            // Absent, or past the maxima: nothing there to meet.
            if peak.is_nan() || peak < self.low {
                continue;
            }
            let (x0, z0) = (
                self.origin.0 + offset.0 + self.step * real(column * cells),
                self.origin.1 + offset.1 + self.step * real(row * cells),
            );
            let size = self.step * real(cells);
            let block = Aabb {
                min: Vec3::new(x0, self.low, z0),
                max: Vec3::new(x0 + size, peak + lift, z0 + size),
            };
            let Some((enter, leave)) = block.padded().span(ray, inverse, reach) else {
                continue;
            };
            if enter.max(from) >= leave {
                continue;
            }
            if level == 0 {
                if let Some(cut) = reached((column, row), (x0, z0), (enter.max(from), leave)) {
                    reach = reach.min(cut);
                }
                continue;
            }
            // The four children, the one the ray reaches first pushed last so
            // it is walked first.
            let first = (usize::from(ray.dir.x < 0.0), usize::from(ray.dir.z < 0.0));
            for (a, b) in [
                (1 - first.0, 1 - first.1),
                (first.0, 1 - first.1),
                (1 - first.0, first.1),
                first,
            ] {
                if let Some(slot) = stack.get_mut(pending) {
                    *slot = (level - 1, 2 * column + a, 2 * row + b);
                    pending += 1;
                }
            }
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

/// The least root of `a t² + b t + c` in `(0, reach]`.
fn smallest_root(a: f64, b: f64, c: f64, reach: f64) -> Option<f64> {
    let (near, far) = quadratic(a, 0.5 * b, c)?;
    [near, far].into_iter().find(|t| *t > 0.0 && *t <= reach)
}

/// A non-negative whole float as a grid index.
fn count(whole: f64) -> usize {
    usize::try_from(mathf::round_i32(whole.max(0.0))).unwrap_or(0)
}

#[cfg(test)]
#[path = "heightfield_tests.rs"]
mod tests;
