//! Particle hydraulic erosion: droplets that run down the slope they read,
//! pick up sediment while they speed up and have room to carry it, and drop
//! it where they slow, fill a pit, or evaporate (Beyer, "Implementation of a
//! method for hydraulic erosion", 2015).
//!
//! It carves what a drainage network at a coarser scale cannot: rills down
//! every slope, gullies where they gather, and the fans and valley fills the
//! sediment builds.
//!
//! Droplets start tile by tile, each tile drawing them from a stream of its
//! own. A droplet reaches no further from its tile than its lifetime and its
//! brush allow, and the tiles take turns in [`PHASES`] phases chosen so that
//! no two tiles of one phase can reach the same sample: a phase's tiles run
//! at once, each over the rows of the grid it can reach and no others. Each
//! turn runs a fixed batch of a tile's droplets, so the ground left is the
//! same however many calls and cores share the run. Heights are stored in
//! single precision, as a renderer's grids hold them; the arithmetic is
//! double.

use alloc::vec::Vec;

use tairix_parallel::JobRunner;
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::{fallible, mathf};

use crate::grid::Grid;
use crate::{fits, TerrainError};

/// How droplets behave. Heights and slopes are in the grid's height units
/// per sample: a caller eroding a finer grid scales them to its spacing.
#[derive(Copy, Clone, Debug)]
pub struct Droplets {
    /// The most steps a droplet takes before it has run dry.
    pub lifetime: u32,
    /// How much of its heading a droplet keeps each step, against turning
    /// down the slope: `0.0` follows the slope exactly.
    pub inertia: f64,
    /// Sediment carried per unit of fall, speed and water.
    pub capacity: f64,
    /// The least fall a step reads, so a droplet on the level still carries.
    pub min_slope: f64,
    /// The share of its spare capacity a droplet takes up each step.
    pub erosion: f64,
    /// The share of what it carries past its capacity it drops each step.
    pub deposition: f64,
    /// The share of its water it loses each step.
    pub evaporation: f64,
    /// Speed gained per unit of height fallen.
    pub gravity: f64,
    /// The share of its speed friction takes each step, so a droplet slows
    /// where the ground levels out and drops what it carried there.
    pub friction: f64,
    /// How many samples around it a droplet wears at once.
    pub radius: u32,
}

/// Tiles a side of a block of the phases: a tile and the tile its phase
/// runs next to it lie two tiles apart, so a tile need only be half as broad
/// as a droplet reaches.
const STRIDE: u32 = 3;

/// The phases the tiles take turns in, one for each tile of a block.
pub const PHASES: u32 = STRIDE * STRIDE;

/// Droplets a tile runs on its phase's turn.
const TURN: u32 = 256;

/// A run of droplets over one grid.
pub struct Erosion {
    grid: Grid,
    law: Droplets,
    /// The samples a droplet wears, as offsets from its cell's first corner
    /// and their shares: a droplet runs no nearer the edge than its brush
    /// reaches, so every offset lands on the grid.
    brush: Vec<(i32, i32, f64)>,
    /// How far from where it starts a droplet can change the ground, the
    /// side of a tile in cells, and how many tiles a side there are.
    reach: u32,
    tile: u32,
    tiles: u32,
    /// Each tile's stream and its droplets left, row by row of tiles.
    streams: Vec<Stream>,
    /// The phase taking its turn, and how many of its tiles have run.
    phase: u32,
    taken: u32,
    total: u32,
}

/// A tile's droplets: where they start, the stream they are drawn from, and
/// how many are left.
struct Stream {
    from: (f64, f64),
    span: (f64, f64),
    dice: NonCryptoRng,
    left: u32,
}

impl core::fmt::Debug for Erosion {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Erosion")
            .field("grid", &self.grid)
            .field("law", &self.law)
            .field("total", &self.total)
            .finish_non_exhaustive()
    }
}

impl Erosion {
    /// Droplets behaving as `law` over `grid`, `rate` of them to each cell
    /// they may start in, drawn under `seed`; `Shape` for a run of more
    /// droplets than a `u32` counts.
    pub fn new(grid: Grid, law: Droplets, rate: f64, seed: u64) -> Result<Self, TerrainError> {
        let radius = law.radius.max(1);
        let reach_i = i32::try_from(radius).map_err(|_| TerrainError::Shape)?;
        let across = usize::try_from(2 * radius + 1).map_err(|_| TerrainError::Shape)?;
        let mut brush = Vec::new();
        if !fallible::reserve(&mut brush, across * across) {
            return Err(TerrainError::OutOfMemory);
        }
        let mut total = 0.0;
        for oy in -reach_i..=reach_i {
            for ox in -reach_i..=reach_i {
                let weight = f64::from(radius) - mathf::sqrt(f64::from(ox * ox + oy * oy));
                if weight > 0.0 {
                    total += weight;
                    brush.push((ox, oy, weight));
                }
            }
        }
        for (_, _, weight) in &mut brush {
            *weight /= total;
        }
        let reach = law
            .lifetime
            .checked_add(radius + 1)
            .ok_or(TerrainError::Shape)?;
        // Two tiles of a phase lie `STRIDE - 1` tiles apart, which must hold
        // both their reaches.
        let tile = (2 * reach + 1).div_ceil(STRIDE - 1);
        let cells = grid.side().saturating_sub(1);
        let tiles = cells.div_ceil(tile).max(1);
        let mut erosion = Self {
            grid,
            law: Droplets { radius, ..law },
            brush,
            reach,
            tile,
            tiles,
            streams: Vec::new(),
            phase: 0,
            taken: 0,
            total: 0,
        };
        erosion.sow(rate, seed)?;
        Ok(erosion)
    }

    /// Lay each tile's droplets out, `rate` to a cell they may start in.
    fn sow(&mut self, rate: f64, seed: u64) -> Result<(), TerrainError> {
        let count = usize::try_from(self.tiles).map_err(|_| TerrainError::Shape)?;
        if !fallible::reserve(&mut self.streams, count * count) {
            return Err(TerrainError::OutOfMemory);
        }
        // A droplet keeps its whole brush on the grid.
        let margin = f64::from(self.law.radius + 1);
        let span = f64::from(self.grid.side()) - 2.0 * margin - 1.0;
        let (low, high) = (margin, margin + span.max(0.0));
        let mut total = 0u32;
        for row in 0..self.tiles {
            for column in 0..self.tiles {
                let along = |at: u32| {
                    let start = f64::from(at * self.tile).clamp(low, high);
                    let end = f64::from((at + 1) * self.tile).clamp(low, high);
                    (start, end - start)
                };
                let ((x, width), (z, depth)) = (along(column), along(row));
                let wanted = mathf::round(rate.max(0.0) * width * depth);
                if wanted > f64::from(u32::MAX) {
                    return Err(TerrainError::Shape);
                }
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "whole, at least nought and at most a u32's greatest, so exact"
                )]
                let left = wanted as u32;
                let index = u64::from(row) * u64::from(self.tiles) + u64::from(column);
                self.streams.push(Stream {
                    from: (x, z),
                    span: (width, depth),
                    dice: NonCryptoRng::seed_from_u64(
                        seed ^ index.wrapping_mul(0x9e37_79b9_7f4a_7c15),
                    ),
                    left,
                });
                total = total.checked_add(left).ok_or(TerrainError::Shape)?;
            }
        }
        self.total = total;
        Ok(())
    }

    /// How many droplets the run holds in all.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.total
    }

    /// How many droplets are left to run.
    #[must_use]
    pub fn left(&self) -> u32 {
        self.streams.iter().map(|stream| stream.left).sum()
    }

    /// The samples the tile `(column, row)` can change: its cells and a
    /// droplet's reach about them, on the grid.
    fn window(&self, (column, row): (u32, u32)) -> Window {
        let side = self.grid.side();
        let span = |at: u32| {
            let start = (at * self.tile).saturating_sub(self.reach);
            let end = ((at + 1) * self.tile + self.reach + 1).min(side);
            (start, end.max(start))
        };
        Window {
            columns: span(column),
            rows: span(row),
        }
    }

    /// Run the next turn's droplets over `height`, as many of the turning
    /// phase's tiles at once as `runner` runs, adding the water each carries
    /// past a sample into that sample of `flux` if one is given: whether
    /// every droplet has run.
    pub fn run(
        &mut self,
        height: &mut [f32],
        flux: Option<&mut [f32]>,
        runner: &dyn JobRunner,
    ) -> Result<bool, TerrainError> {
        fits(height, self.grid)?;
        if let Some(flux) = flux.as_deref() {
            fits(flux, self.grid)?;
        }
        let width = runner.width().max(1);
        let mut picked: Vec<(u32, u32)> = Vec::new();
        if !fallible::reserve(&mut picked, width) {
            return Err(TerrainError::OutOfMemory);
        }
        // A phase none of whose tiles has droplets left passes its turn.
        for _ in 0..=PHASES {
            if self.left() == 0 {
                return Ok(true);
            }
            let skipped = usize::try_from(self.taken).map_err(|_| TerrainError::Shape)?;
            for tile in phase_tiles(self.tiles, self.phase).skip(skipped) {
                if picked.len() >= width {
                    break;
                }
                self.taken += 1;
                if self.stream(tile).is_some_and(|stream| stream.left > 0) {
                    picked.push(tile);
                }
            }
            if phase_tiles(self.tiles, self.phase)
                .nth(self.taken as usize)
                .is_none()
            {
                self.phase = (self.phase + 1) % PHASES;
                self.taken = 0;
            }
            if !picked.is_empty() {
                break;
            }
        }
        let windows: Vec<Window> =
            fallible::collected(picked.len(), picked.iter().map(|&tile| self.window(tile)))
                .ok_or(TerrainError::OutOfMemory)?;
        let side = self.grid.side() as usize;
        let mut heights = split(height, side, &windows)?.into_iter();
        let mut fluxes = match flux {
            Some(flux) => Some(split(flux, side, &windows)?.into_iter()),
            None => None,
        };
        let (law, brush) = (self.law, self.brush.as_slice());
        let bounds = self.bounds();
        let mut work: Vec<(&mut Stream, View<'_>, Option<View<'_>>)> = Vec::new();
        if !fallible::reserve(&mut work, picked.len()) {
            return Err(TerrainError::OutOfMemory);
        }
        let tiles = self.tiles as usize;
        let mut next = picked.iter().zip(&windows).peekable();
        for (index, stream) in self.streams.iter_mut().enumerate() {
            let Some(&(&(column, row), window)) = next.peek() else {
                break;
            };
            if index != row as usize * tiles + column as usize {
                continue;
            }
            next.next();
            let origin = (window.columns.0, window.rows.0);
            let rows = heights.next().ok_or(TerrainError::Shape)?;
            let flux = match fluxes.as_mut() {
                Some(fluxes) => Some(View {
                    rows: fluxes.next().ok_or(TerrainError::Shape)?,
                    origin,
                }),
                None => None,
            };
            work.push((stream, View { rows, origin }, flux));
        }
        tairix_parallel::for_each(runner, &mut work, &|(stream, height, flux)| {
            for _ in 0..TURN.min(stream.left) {
                let start = (
                    stream.from.0 + stream.span.0 * stream.dice.next_f64(),
                    stream.from.1 + stream.span.1 * stream.dice.next_f64(),
                );
                droplet((law, brush), (height, flux.as_mut()), start, bounds);
                stream.left -= 1;
            }
        });
        Ok(self.left() == 0)
    }

    /// The droplets of the tile `(column, row)`.
    fn stream(&self, (column, row): (u32, u32)) -> Option<&Stream> {
        self.streams
            .get(row as usize * self.tiles as usize + column as usize)
    }

    /// The bounds a droplet runs within on either axis, its whole brush
    /// kept on the grid.
    fn bounds(&self) -> (f64, f64) {
        let margin = f64::from(self.law.radius + 1);
        let span = f64::from(self.grid.side()) - 2.0 * margin - 1.0;
        (margin, margin + span.max(0.0))
    }
}

/// The tiles of phase `phase` of a grid `tiles` tiles a side, as their
/// columns and rows, row by row.
fn phase_tiles(tiles: u32, phase: u32) -> impl Iterator<Item = (u32, u32)> {
    let (column, row) = (phase % STRIDE, phase / STRIDE);
    (row..tiles)
        .step_by(STRIDE as usize)
        .flat_map(move |tile_row| {
            (column..tiles)
                .step_by(STRIDE as usize)
                .map(move |tile_column| (tile_column, tile_row))
        })
}

/// A rectangle of a grid's samples: its columns and its rows, each from the
/// first to past the last.
#[derive(Copy, Clone, Debug)]
struct Window {
    columns: (u32, u32),
    rows: (u32, u32),
}

/// The rows of `values`, a grid `side` samples wide, that each of `windows`
/// spans, handed to it apart; `Shape` should two of them overlap.
fn split<'a, T>(
    values: &'a mut [T],
    side: usize,
    windows: &[Window],
) -> Result<Vec<Vec<&'a mut [T]>>, TerrainError> {
    let mut order: Vec<usize> =
        fallible::collected(windows.len(), 0..windows.len()).ok_or(TerrainError::OutOfMemory)?;
    order.sort_unstable_by_key(|&index| windows.get(index).map_or(0, |window| window.columns.0));
    let mut parts: Vec<Vec<&'a mut [T]>> = Vec::new();
    if !fallible::reserve(&mut parts, windows.len()) {
        return Err(TerrainError::OutOfMemory);
    }
    for window in windows {
        let mut rows = Vec::new();
        if !fallible::reserve(&mut rows, (window.rows.1 - window.rows.0) as usize) {
            return Err(TerrainError::OutOfMemory);
        }
        parts.push(rows);
    }
    for (row, line) in (0u32..).zip(values.chunks_mut(side.max(1))) {
        let (mut rest, mut at) = (line, 0usize);
        for &index in &order {
            let Some(window) = windows.get(index) else {
                continue;
            };
            if !(window.rows.0..window.rows.1).contains(&row) {
                continue;
            }
            let (first, last) = (window.columns.0 as usize, window.columns.1 as usize);
            let skip = first.checked_sub(at).ok_or(TerrainError::Shape)?;
            let (_, tail) = rest.split_at_mut_checked(skip).ok_or(TerrainError::Shape)?;
            let (part, tail) = tail
                .split_at_mut_checked(last - first)
                .ok_or(TerrainError::Shape)?;
            parts.get_mut(index).ok_or(TerrainError::Shape)?.push(part);
            (rest, at) = (tail, last);
        }
    }
    Ok(parts)
}

/// The rows of a grid a tile can reach, and where the first begins.
struct View<'a> {
    rows: Vec<&'a mut [f32]>,
    origin: (u32, u32),
}

impl View<'_> {
    fn get(&self, (x, y): (u32, u32)) -> Option<f64> {
        let row = self.rows.get(y.checked_sub(self.origin.1)? as usize)?;
        row.get(x.checked_sub(self.origin.0)? as usize)
            .map(|&value| f64::from(value))
    }

    fn add(&mut self, (x, y): (u32, u32), amount: f64) {
        let (Some(row), Some(column)) =
            (y.checked_sub(self.origin.1), x.checked_sub(self.origin.0))
        else {
            return;
        };
        if let Some(slot) = self
            .rows
            .get_mut(row as usize)
            .and_then(|row| row.get_mut(column as usize))
        {
            *slot += single(amount);
        }
    }
}

/// One droplet from `start`, kept within `bounds` on either axis.
fn droplet(
    (law, brush): (Droplets, &[(i32, i32, f64)]),
    (height, mut flux): (&mut View<'_>, Option<&mut View<'_>>),
    (mut x, mut y): (f64, f64),
    (low, high): (f64, f64),
) {
    let (mut dx, mut dy) = (0.0_f64, 0.0_f64);
    let (mut speed, mut water, mut sediment) = (1.0_f64, 1.0_f64, 0.0_f64);
    for _ in 0..law.lifetime {
        let Some(here) = sample(height, (x, y)) else {
            return;
        };
        dx = dx * law.inertia - here.gradient.0 * (1.0 - law.inertia);
        dy = dy * law.inertia - here.gradient.1 * (1.0 - law.inertia);
        let length = mathf::sqrt(dx * dx + dy * dy);
        if length < 1e-12 {
            // On the dead level it has nowhere to run.
            deposit(height, &here, sediment);
            return;
        }
        let (nx, ny) = (x + dx / length, y + dy / length);
        if !(low..high).contains(&nx) || !(low..high).contains(&ny) {
            return;
        }
        let Some(next) = sample(height, (nx, ny)) else {
            return;
        };
        let fall = next.height - here.height;
        let capacity = (-fall).max(law.min_slope) * speed * water * law.capacity;
        if sediment > capacity || fall > 0.0 {
            let dropped = if fall > 0.0 {
                fall.min(sediment)
            } else {
                (sediment - capacity) * law.deposition
            };
            sediment -= dropped;
            deposit(height, &here, dropped);
        } else {
            // Never deeper than the ground it is running to, or it would
            // dig the pit it then has to fill.
            let taken = ((capacity - sediment) * law.erosion).min(-fall);
            wear(height, brush, &here, taken);
            sediment += taken;
        }
        if let Some(flux) = flux.as_deref_mut() {
            flux.add(here.corner, water);
        }
        speed = mathf::sqrt((speed * speed - fall * law.gravity).max(0.0)) * (1.0 - law.friction);
        water *= 1.0 - law.evaporation;
        (x, y) = (nx, ny);
    }
    // Run dry, it lays down whatever it still carries where it stands.
    if let Some(here) = sample(height, (x, y)) {
        deposit(height, &here, sediment);
    }
}

/// Where a droplet stands.
struct Ground {
    /// The sample at the corner of its cell nearest the grid's origin.
    corner: (u32, u32),
    across: f64,
    down: f64,
    height: f64,
    gradient: (f64, f64),
}

/// The ground under `(x, y)`: its cell, its height and its slope; `None` off
/// the rows `height` holds.
fn sample(height: &View<'_>, (x, y): (f64, f64)) -> Option<Ground> {
    let (column, row) = (mathf::floor(x), mathf::floor(y));
    let (across, down) = (x - column, y - row);
    let (column, row) = (whole(column)?, whole(row)?);
    let (h00, h10) = (height.get((column, row))?, height.get((column + 1, row))?);
    let (h01, h11) = (
        height.get((column, row + 1))?,
        height.get((column + 1, row + 1))?,
    );
    Some(Ground {
        corner: (column, row),
        across,
        down,
        height: h00 * (1.0 - across) * (1.0 - down)
            + h10 * across * (1.0 - down)
            + h01 * (1.0 - across) * down
            + h11 * across * down,
        gradient: (
            (h10 - h00) * (1.0 - down) + (h11 - h01) * down,
            (h01 - h00) * (1.0 - across) + (h11 - h10) * across,
        ),
    })
}

/// Lay `amount` on the four samples of `at`'s cell, each by its nearness.
fn deposit(height: &mut View<'_>, at: &Ground, amount: f64) {
    if amount <= 0.0 {
        return;
    }
    let ((column, row), across, down) = (at.corner, at.across, at.down);
    height.add((column, row), amount * (1.0 - across) * (1.0 - down));
    height.add((column + 1, row), amount * across * (1.0 - down));
    height.add((column, row + 1), amount * (1.0 - across) * down);
    height.add((column + 1, row + 1), amount * across * down);
}

/// Wear `amount` from the brush's samples about `at`'s cell.
fn wear(height: &mut View<'_>, brush: &[(i32, i32, f64)], at: &Ground, amount: f64) {
    if amount <= 0.0 {
        return;
    }
    let (column, row) = at.corner;
    for &(dx, dy, share) in brush {
        if let (Some(x), Some(y)) = (column.checked_add_signed(dx), row.checked_add_signed(dy)) {
            height.add((x, y), -amount * share);
        }
    }
}

/// A non-negative whole float below `u32::MAX` as a `u32`.
fn whole(value: f64) -> Option<u32> {
    if !(0.0..4_294_967_295.0).contains(&value) {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "range-checked above, and whole, so the conversion is exact"
    )]
    Some(value as u32)
}

/// A value in the grid's single precision.
fn single(value: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "heights and water are stored in single precision"
    )]
    {
        value as f32
    }
}

#[cfg(test)]
#[path = "droplet_tests.rs"]
mod tests;
