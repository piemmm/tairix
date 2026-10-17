//! Particle hydraulic erosion: droplets that run down the slope they read,
//! pick up sediment while they speed up and have room to carry it, and drop
//! it where they slow, fill a pit, or evaporate (Beyer, "Implementation of a
//! method for hydraulic erosion", 2015).
//!
//! It carves what a drainage network at a coarser scale cannot: rills down
//! every slope, gullies where they gather, and the fans and valley fills the
//! sediment builds.
//!
//! A run lies on a lattice of samples fixed to the land, not to whatever grid
//! holds the ground it wears. Its droplets start tile by tile, each tile of the
//! lattice drawing them from a stream keyed by its place, over [`PASSES`]
//! tilings each staggered an equal share of a tile from the last. Within a
//! pass every tile's droplets run on a copy of the ground as the pass found
//! it, so all of a pass's tiles run at once across a runner, and their changes
//! are summed in the tiles' order; the next pass reads the sum. The ground a
//! run leaves at a sample is therefore a pure function of the ground within
//! [`Tiles::halo`] of it, and a region worn in pieces, each held with that
//! much ground about it, matches the region worn whole. Heights are stored in
//! single precision, as a renderer's grids hold them; the arithmetic is
//! double.

use alloc::vec::Vec;

use tairix_hash::FastHash;
use tairix_parallel::JobRunner;
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::{fallible, mathf};

use crate::TerrainError;

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
    /// How many samples around it a droplet wears at once; at least one.
    pub radius: u32,
}

impl Droplets {
    /// How far from where it starts a droplet can read or change the ground,
    /// in samples: a step a sample for its lifetime, its brush, and the far
    /// corner of the cell it stands in.
    fn reach(&self) -> Result<u32, TerrainError> {
        self.lifetime
            .checked_add(self.radius.max(1))
            .and_then(|reach| reach.checked_add(1))
            .ok_or(TerrainError::Shape)
    }
}

/// The tilings a run passes over.
pub const PASSES: u32 = 2;

/// The side of a run's tiles for droplets reaching `reach` samples: a third
/// again as broad. Measured, two passes over tiles this broad wear the ground
/// within a few per cent of one run in order, and broader tiles buy little
/// for the halo they add.
fn tile_side(reach: u32) -> Result<u32, TerrainError> {
    reach
        .checked_add(reach.div_ceil(3))
        .ok_or(TerrainError::Shape)
}

/// A rectangle of samples on a run's lattice.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Placed {
    /// Where its first sample lies on the lattice.
    pub origin: (i64, i64),
    /// Its samples across and down.
    pub size: (u32, u32),
}

impl Placed {
    /// The square of `side` samples a side from `origin`.
    #[must_use]
    pub const fn square(origin: (i64, i64), side: u32) -> Self {
        Self {
            origin,
            size: (side, side),
        }
    }

    /// Its samples, row by row; `Shape` for more than a `usize` counts.
    pub fn area(self) -> Result<usize, TerrainError> {
        let across = usize::try_from(self.size.0).map_err(|_| TerrainError::Shape)?;
        let down = usize::try_from(self.size.1).map_err(|_| TerrainError::Shape)?;
        across.checked_mul(down).ok_or(TerrainError::Shape)
    }

    /// The lattice place just past its last sample on either axis.
    pub fn end(self) -> Result<(i64, i64), TerrainError> {
        Ok((
            self.origin
                .0
                .checked_add(i64::from(self.size.0))
                .ok_or(TerrainError::Shape)?,
            self.origin
                .1
                .checked_add(i64::from(self.size.1))
                .ok_or(TerrainError::Shape)?,
        ))
    }

    /// Whether `other` lies wholly within it.
    #[must_use]
    pub fn holds(self, other: Self) -> bool {
        match (self.end(), other.end()) {
            (Ok((x1, y1)), Ok((ox1, oy1))) => {
                other.origin.0 >= self.origin.0
                    && other.origin.1 >= self.origin.1
                    && ox1 <= x1
                    && oy1 <= y1
            }
            _ => false,
        }
    }

    /// The least rectangle holding it and `other`; `Shape` for one past the
    /// lattice.
    pub fn cover(self, other: Self) -> Result<Self, TerrainError> {
        let ((x1, y1), (ox1, oy1)) = (self.end()?, other.end()?);
        let origin = (
            self.origin.0.min(other.origin.0),
            self.origin.1.min(other.origin.1),
        );
        let size = |from: i64, to: i64| u32::try_from(to - from).map_err(|_| TerrainError::Shape);
        Ok(Self {
            origin,
            size: (size(origin.0, x1.max(ox1))?, size(origin.1, y1.max(oy1))?),
        })
    }

    /// The part of it `other` overlaps; `None` where they do not.
    #[must_use]
    pub fn within(self, other: Self) -> Option<Self> {
        let (x1, y1) = self.end().ok()?;
        let (ox1, oy1) = other.end().ok()?;
        let (left, top) = (
            self.origin.0.max(other.origin.0),
            self.origin.1.max(other.origin.1),
        );
        let (right, bottom) = (x1.min(ox1), y1.min(oy1));
        Some(Self {
            origin: (left, top),
            size: (
                u32::try_from(right - left)
                    .ok()
                    .filter(|&across| across > 0)?,
                u32::try_from(bottom - top).ok().filter(|&down| down > 0)?,
            ),
        })
    }

    /// Its samples' index, row by row, of lattice place `(x, y)`; `None` off
    /// it.
    #[must_use]
    pub fn index(self, (x, y): (i64, i64)) -> Option<usize> {
        let (column, row) = (x.checked_sub(self.origin.0)?, y.checked_sub(self.origin.1)?);
        let (across, down) = (i64::from(self.size.0), i64::from(self.size.1));
        if !(0..across).contains(&column) || !(0..down).contains(&row) {
            return None;
        }
        let index = u64::try_from(row)
            .ok()?
            .checked_mul(u64::from(self.size.0))?
            .checked_add(u64::try_from(column).ok()?)?;
        usize::try_from(index).ok()
    }
}

/// Where on each axis a droplet may start and stand: its domain less a
/// brush's breadth at each edge, so its whole brush stays within it; as
/// lattice places and, for the droplets' every step, as reals.
#[derive(Copy, Clone, Debug)]
struct Bounds {
    low: (i64, i64),
    high: (i64, i64),
    from: (f64, f64),
    to: (f64, f64),
}

impl Bounds {
    /// The bounds of a domain worn by a brush `radius` samples about.
    fn within(domain: Placed, radius: u32) -> Result<Self, TerrainError> {
        let margin = i64::from(radius) + 1;
        let end = domain.end()?;
        let low = |origin: i64| origin.checked_add(margin).ok_or(TerrainError::Shape);
        let high = |end: i64| end.checked_sub(margin + 1).ok_or(TerrainError::Shape);
        let (low, high) = (
            (low(domain.origin.0)?, low(domain.origin.1)?),
            (high(end.0)?, high(end.1)?),
        );
        // Exactly representable, so a droplet's bounds read alike everywhere.
        let exact = |value: i64| value.unsigned_abs() <= EXACT;
        if [low.0, low.1, high.0, high.1].into_iter().all(exact) {
            Ok(Self {
                low,
                high,
                from: reals(low),
                to: reals(high),
            })
        } else {
            Err(TerrainError::Shape)
        }
    }

    /// Whether `(x, y)` lies within them.
    fn hold(self, (x, y): (f64, f64)) -> bool {
        (self.from.0..self.to.0).contains(&x) && (self.from.1..self.to.1).contains(&y)
    }
}

/// A lattice place, or a breadth along it, as reals.
#[allow(
    clippy::cast_precision_loss,
    reason = "a run's places are refused unless an f64 holds them exactly"
)]
const fn reals((x, y): (i64, i64)) -> (f64, f64) {
    (x as f64, y as f64)
}

/// The greatest lattice coordinate an `f64` holds exactly, and so the
/// greatest a run reaches.
const EXACT: u64 = 1 << 53;

/// One pass's tiles that reach a region and can start a droplet: the first
/// such tile's column and row, and how many columns and rows of them there
/// are.
#[derive(Copy, Clone, Debug, Default)]
struct Tiling {
    first: (i64, i64),
    columns: u64,
    rows: u64,
}

impl Tiling {
    fn count(self) -> u64 {
        self.columns.saturating_mul(self.rows)
    }

    /// Its `index`th tile, row by row.
    fn nth(self, index: u64) -> Option<(i64, i64)> {
        if index >= self.count() {
            return None;
        }
        let (row, column) = (index / self.columns, index % self.columns);
        Some((
            self.first.0.checked_add(i64::try_from(column).ok()?)?,
            self.first.1.checked_add(i64::try_from(row).ok()?)?,
        ))
    }
}

/// The side of the blocks a run keeps its ground in between pieces: squares
/// fixed to the lattice from its origin, each the ground over one after so
/// many passes.
pub const BLOCK: u32 = 64;

/// A run of droplets over a domain, tile by tile: what a tile's droplets do
/// is a pure function of its pass, its place, and the ground its window holds
/// as its pass found it.
#[derive(Clone)]
pub struct Tiles {
    law: Droplets,
    /// The samples a droplet wears, as offsets from its cell's first corner
    /// and their shares, and how far the farthest lies either way.
    brush: Vec<(isize, isize, f64)>,
    spread: usize,
    key: u64,
    rate: f64,
    reach: i64,
    tile: i64,
    domain: Placed,
    bounds: Bounds,
}

impl core::fmt::Debug for Tiles {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Tiles")
            .field("law", &self.law)
            .field("rate", &self.rate)
            .field("domain", &self.domain)
            .finish_non_exhaustive()
    }
}

/// One of a run's tiles: its pass, and its row and column on that pass's
/// tiling. Tiles order as a run sums their changes: by pass, then row by row.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct Tile {
    /// The pass it belongs to.
    pub pass: u32,
    /// Its row on the pass's tiling.
    pub row: i64,
    /// Its column.
    pub column: i64,
}

impl Tiles {
    /// Droplets behaving as `law`, `rate` of them to each sample of `domain`
    /// they may start on, drawn under `key`. `Shape` for a rate below nought
    /// or not finite, a law too long to count, or a domain whose places an
    /// `f64` cannot hold exactly.
    pub fn new(
        law: Droplets,
        (rate, key): (f64, u64),
        domain: Placed,
    ) -> Result<Self, TerrainError> {
        if !rate.is_finite() || rate < 0.0 {
            return Err(TerrainError::Shape);
        }
        let radius = law.radius.max(1);
        let reach = law.reach()?;
        let tile = tile_side(reach)?;
        domain.end()?;
        Ok(Self {
            law: Droplets { radius, ..law },
            brush: brush(radius)?,
            spread: usize::try_from(radius - 1).map_err(|_| TerrainError::Shape)?,
            key,
            rate,
            reach: i64::from(reach),
            tile: i64::from(tile),
            domain,
            bounds: Bounds::within(domain, radius)?,
        })
    }

    /// The land its droplets run on.
    #[must_use]
    pub const fn domain(&self) -> Placed {
        self.domain
    }

    /// How far beyond a piece the ground its wearing reads can lie, at most:
    /// each pass's tiles and their reach, and the blocks it is kept in.
    pub fn halo(&self) -> Result<u32, TerrainError> {
        let block = i64::from(BLOCK) - 1;
        let pass = self.tile + 2 * self.reach - 1;
        u32::try_from(3 * block + i64::from(PASSES) * pass).map_err(|_| TerrainError::Shape)
    }

    /// The tiles of pass `pass` that start a droplet and whose window reaches
    /// `region`, row by row as a run sums them, into `into`.
    pub fn reaching(
        &self,
        pass: u32,
        region: Placed,
        into: &mut Vec<Tile>,
    ) -> Result<(), TerrainError> {
        into.clear();
        if pass >= PASSES {
            return Err(TerrainError::Shape);
        }
        let tiling = self.tiling(pass, region)?;
        let count = usize::try_from(tiling.count()).map_err(|_| TerrainError::Shape)?;
        if !fallible::reserve(into, count) {
            return Err(TerrainError::OutOfMemory);
        }
        for index in 0..tiling.count() {
            let (column, row) = tiling.nth(index).ok_or(TerrainError::Shape)?;
            let tile = Tile { pass, row, column };
            if self.span(tile)?.1 > 0 {
                into.push(tile);
            }
        }
        Ok(())
    }

    /// The samples `tile`'s droplets may read and change: the tile and its
    /// reach either side, within the land.
    pub fn window(&self, tile: Tile) -> Result<Placed, TerrainError> {
        let offset = self.offset(tile.pass);
        let along = |at: i64| {
            let first = at
                .checked_mul(self.tile)
                .and_then(|place| place.checked_add(offset))
                .ok_or(TerrainError::Shape)?;
            let from = first.checked_sub(self.reach).ok_or(TerrainError::Shape)?;
            let to = first
                .checked_add(self.tile + self.reach)
                .ok_or(TerrainError::Shape)?;
            u32::try_from(to - from)
                .map(|size| (from, size))
                .map_err(|_| TerrainError::Shape)
        };
        let ((x, across), (y, down)) = (along(tile.column)?, along(tile.row)?);
        Placed {
            origin: (x, y),
            size: (across, down),
        }
        .within(self.domain)
        .ok_or(TerrainError::Shape)
    }

    /// How many droplets `tile` starts.
    pub fn droplets(&self, tile: Tile) -> Result<u32, TerrainError> {
        Ok(self.span(tile)?.1)
    }

    /// The run of `tile`'s droplets begun, none run yet.
    pub fn begin(&self, tile: Tile) -> Result<Running, TerrainError> {
        let (_, left) = self.span(tile)?;
        Ok(Running {
            tile,
            dice: self.dice(tile),
            touched: Touched::NOTHING,
            left,
        })
    }

    /// Run up to `most` more of a tile's droplets on from where `running` has
    /// come, over `ground`, the ground the tile's window holds as its pass
    /// found it, row by row, adding the water each carries past a sample into
    /// `track` where given: whether every droplet has run. `Shape` for buffers
    /// that do not fit the window, the run left where it was.
    pub fn run_on(
        &self,
        running: &mut Running,
        (ground, track): (&mut [f32], Option<&mut [f32]>),
        most: u32,
    ) -> Result<bool, TerrainError> {
        let tile = running.tile;
        let window = self.window(tile)?;
        let area = window.area()?;
        if ground.len() != area || track.as_deref().is_some_and(|track| track.len() != area) {
            return Err(TerrainError::Shape);
        }
        let (span, _) = self.span(tile)?;
        let whole = |size: u32| usize::try_from(size).map_err(|_| TerrainError::Shape);
        let mut reached = Reached {
            origin: window.origin,
            columns: whole(window.size.0)?,
            rows: whole(window.size.1)?,
            height: ground,
            track,
            touched: running.touched,
        };
        let brush = Brush {
            samples: &self.brush,
            spread: self.spread,
        };
        let count = running.left.min(most);
        for _ in 0..count {
            let from = (
                span.from.0 + span.breadth.0 * running.dice.next_f64(),
                span.from.1 + span.breadth.1 * running.dice.next_f64(),
            );
            droplet((self.law, brush), &mut reached, from, self.bounds);
        }
        running.left -= count;
        running.touched = reached.touched;
        Ok(running.left == 0)
    }

    /// The least box of a tile's window its droplets have changed or carried
    /// water across as far as `running` has come, `None` where they touched
    /// nothing.
    pub fn touched(&self, running: &Running) -> Result<Option<Placed>, TerrainError> {
        running.touched.placed(self.window(running.tile)?.origin)
    }

    /// Where pass `pass`'s tiling starts on the lattice: each pass's an equal
    /// share of a tile from the last.
    fn offset(&self, pass: u32) -> i64 {
        self.tile * i64::from(pass) / i64::from(PASSES)
    }

    /// The tiles of pass `pass` whose reach covers a sample of `region` and
    /// on which a droplet may start.
    fn tiling(&self, pass: u32, region: Placed) -> Result<Tiling, TerrainError> {
        let offset = self.offset(pass);
        let (start, end) = (region.origin, region.end()?);
        let (tile, reach) = (self.tile, self.reach);
        // Tile `i` starts droplets from `offset + i·tile` up to the next tile's
        // first sample, and they reach `reach` past that either way.
        let along = |(low, high): (i64, i64), (from, past): (i64, i64)| {
            let reaches_from = from
                .checked_sub(offset)
                .and_then(|value| value.checked_sub(tile - 1 + reach))
                .map(|value| ceiling(value, tile));
            let reaches_to = past
                .checked_sub(offset)
                .and_then(|value| value.checked_add(reach - 1))
                .map(|value| value.div_euclid(tile));
            let starts_from = low.checked_sub(offset).map(|value| value.div_euclid(tile));
            let starts_to = high
                .checked_sub(offset + 1)
                .map(|value| value.div_euclid(tile));
            let (Some(reaches_from), Some(reaches_to), Some(starts_from), Some(starts_to)) =
                (reaches_from, reaches_to, starts_from, starts_to)
            else {
                return Err(TerrainError::Shape);
            };
            let (first, last) = (reaches_from.max(starts_from), reaches_to.min(starts_to));
            let count = if high > low && last >= first {
                u64::try_from(last - first + 1).map_err(|_| TerrainError::Shape)?
            } else {
                0
            };
            Ok((first, count))
        };
        let (first_column, columns) =
            along((self.bounds.low.0, self.bounds.high.0), (start.0, end.0))?;
        let (first_row, rows) = along((self.bounds.low.1, self.bounds.high.1), (start.1, end.1))?;
        Ok(Tiling {
            first: (first_column, first_row),
            columns,
            rows,
        })
    }

    /// Where `tile`'s droplets may start — its first corner and breadth on
    /// either axis — and how many it starts there.
    fn span(&self, tile: Tile) -> Result<(Span, u32), TerrainError> {
        let offset = self.offset(tile.pass);
        let along = |at: i64, (low, high): (i64, i64)| {
            let first = at
                .checked_mul(self.tile)
                .and_then(|place| place.checked_add(offset))
                .ok_or(TerrainError::Shape)?;
            let from = first.max(low);
            let to = first.saturating_add(self.tile).min(high);
            Ok::<_, TerrainError>((from, (to - from).max(0)))
        };
        let (x, width) = along(tile.column, (self.bounds.low.0, self.bounds.high.0))?;
        let (y, depth) = along(tile.row, (self.bounds.low.1, self.bounds.high.1))?;
        let span = Span {
            from: reals((x, y)),
            breadth: reals((width, depth)),
        };
        let wanted = mathf::round(self.rate * span.breadth.0 * span.breadth.1 / f64::from(PASSES));
        if !(0.0..=f64::from(u32::MAX)).contains(&wanted) {
            return Err(TerrainError::Shape);
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "whole, at least nought and at most a u32's greatest, so exact"
        )]
        let count = wanted as u32;
        Ok((span, count))
    }

    /// The stream `tile` draws its droplets from.
    fn dice(&self, tile: Tile) -> NonCryptoRng {
        let mut message = [0u8; 20];
        message[0..4].copy_from_slice(&tile.pass.to_le_bytes());
        message[4..12].copy_from_slice(&tile.column.to_le_bytes());
        message[12..20].copy_from_slice(&tile.row.to_le_bytes());
        NonCryptoRng::seed_from_u64(FastHash::hash_bytes(self.key, &message))
    }
}

/// A block of a run's ground: how many passes have worn it, and its row and
/// column among the lattice's blocks.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct Block {
    /// The passes of the run that have worn it: nought for the ground before
    /// any droplet ran.
    pub worn: u32,
    /// Its row among the blocks.
    pub row: i64,
    /// Its column.
    pub column: i64,
}

/// A block's ground over its samples within the land, row by row, and the
/// water the run's droplets have carried past each where the run keeps it:
/// none before any pass, or where the run keeps none.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Kept {
    /// The ground.
    pub ground: Vec<f32>,
    /// The water carried past each sample, or nothing.
    pub track: Vec<f32>,
}

impl Kept {
    /// The bytes it holds.
    #[must_use]
    pub fn bytes(&self) -> usize {
        (self.ground.len() + self.track.len()).saturating_mul(core::mem::size_of::<f32>())
    }
}

/// Where a run's blocks are kept between the pieces worn from it, to be read
/// again rather than made again. Each block is a pure function of the land and
/// its place, so a store may drop any at any time, and one made again is the
/// same to the bit.
pub trait Store {
    /// Hand `read` block `block`, where it is kept: whether it was.
    fn read(&mut self, block: Block, read: &mut dyn FnMut(&Kept)) -> bool;
    /// Keep block `block`.
    fn keep(&mut self, block: Block, kept: Kept);
}

/// A store that keeps nothing, so each piece is worn from the land alone.
#[derive(Copy, Clone, Debug, Default)]
pub struct Unkept;

impl Store for Unkept {
    fn read(&mut self, _: Block, _: &mut dyn FnMut(&Kept)) -> bool {
        false
    }

    fn keep(&mut self, _: Block, _: Kept) {}
}

/// The ground a run wears before any droplet runs: a pure function of place.
pub trait Source: Sync {
    /// The ground over `region`, row by row, into `into`, which holds its
    /// samples.
    fn fill(&self, region: Placed, into: &mut [f32]) -> Result<(), TerrainError>;
}

/// How much of a wearing's work there is, or has been done: the samples of
/// ground it fills, and the droplets it runs.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Work {
    /// Samples of ground filled from the land.
    pub filled: u64,
    /// Droplets run.
    pub droplets: u64,
}

/// One stage of a wearing's ground: a rectangle of blocks worn by so many
/// passes, row by row, each held once read or made.
#[derive(Debug, Default)]
struct Stage {
    first: (i64, i64),
    across: usize,
    blocks: Vec<Held>,
}

/// A block a wearing holds: the samples it covers within the land, its ground
/// once read or made, and whether it was made here, so the store is offered it.
#[derive(Debug)]
struct Held {
    region: Placed,
    kept: Option<Kept>,
    made: bool,
}

/// A tile's run part way through: the tile, the draws its droplets start
/// from, the box they have changed so far, and how many are still to run.
pub struct Running {
    tile: Tile,
    dice: NonCryptoRng,
    touched: Touched,
    left: u32,
}

impl core::fmt::Debug for Running {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Running")
            .field("tile", &self.tile)
            .field("touched", &self.touched)
            .field("left", &self.left)
            .finish_non_exhaustive()
    }
}

/// A tile's turn: its window, the window's ground as its droplets run, their
/// water's tracks, their run, the box they touched, and whether the run
/// refused it.
#[derive(Debug, Default)]
struct Turn {
    window: Option<Placed>,
    ground: Vec<f32>,
    track: Vec<f32>,
    running: Option<Running>,
    touched: Option<Placed>,
    refused: bool,
}

/// Droplets a core runs of its tile a unit, some 5 ms at the tracer's law: a
/// tile's run goes on over as many units as its droplets need.
const DROPLET_UNIT: u32 = 1 << 9;

/// Rows of a block a core fills a unit, its samples each read from the land
/// at whatever that costs where it lies: a quarter of a block, so a unit is
/// the same work a core whatever a block's land costs to read.
const FILL_ROWS: u32 = BLOCK / 4;

/// What a wearing does next.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Phase {
    /// Filling the ground before the run from row `row` of the stage's block
    /// `block` on.
    Fill {
        block: usize,
        row: u32,
    },
    /// Running pass `pass`'s tiles from its `next` on.
    Pass {
        pass: u32,
        next: usize,
    },
    Done,
}

/// A piece of a land worn by a run, a bounded unit at a time: its ground after
/// every pass, read from the blocks a store keeps where it keeps them; the
/// rest filled from the land and worn by just the tiles they need, the tiles'
/// changes summed in their order, so it is the land worn whole to the bit.
pub struct Wearing {
    piece: Placed,
    tracked: bool,
    stages: [Stage; PASSES as usize + 1],
    /// The ground of the block the fill has come to, its rows before the
    /// fill's filled.
    filling: Option<Vec<f32>>,
    tiles: [Vec<Tile>; PASSES as usize],
    turns: Vec<Turn>,
    /// The turns begun of the batch at the pass's next tile, none between
    /// batches.
    begun: usize,
    work: Work,
    done: Work,
    phase: Phase,
}

impl core::fmt::Debug for Wearing {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Wearing")
            .field("piece", &self.piece)
            .field("tracked", &self.tracked)
            .field("work", &self.work)
            .field("done", &self.done)
            .field("phase", &self.phase)
            .finish_non_exhaustive()
    }
}

impl Wearing {
    /// The wearing of `piece` by `tiles`, keeping the water's tracks if
    /// `tracked`, reading what `store` keeps: `Shape` for a piece off the land,
    /// `OutOfMemory` when the heap will not hold its plan.
    pub fn new(
        tiles: &Tiles,
        (piece, tracked): (Placed, bool),
        store: &mut dyn Store,
    ) -> Result<Self, TerrainError> {
        if !tiles.domain.holds(piece) {
            return Err(TerrainError::Shape);
        }
        let mut wearing = Self {
            piece,
            tracked,
            stages: Default::default(),
            filling: None,
            tiles: Default::default(),
            turns: Vec::new(),
            begun: 0,
            work: Work::default(),
            done: Work::default(),
            phase: Phase::Done,
        };
        let last = PASSES as usize;
        // The last stage over the piece, then back through the passes: a
        // stage's missing blocks want the tiles reaching them and, under their
        // windows and themselves, the stage before.
        let mut wanted = Some(piece);
        for worn in (0..=last).rev() {
            let Some(region) = wanted else {
                break;
            };
            let stage = Stage::over(region, tiles.domain)?;
            let stage = wearing.read(stage, worn, store)?;
            wanted = None;
            if let (Some(pass), Some(missing)) = (worn.checked_sub(1), stage.missing()) {
                let mut reaching = Vec::new();
                let numbered = u32::try_from(pass).map_err(|_| TerrainError::Shape)?;
                tiles.reaching(numbered, missing, &mut reaching)?;
                let mut under = missing;
                for &tile in &reaching {
                    under = under.cover(tiles.window(tile)?)?;
                    wearing.work.droplets += u64::from(tiles.droplets(tile)?);
                }
                wanted = Some(under);
                wearing.tiles[pass] = reaching;
            }
            wearing.stages[worn] = stage;
        }
        for held in wearing.stages[0]
            .blocks
            .iter()
            .filter(|held| held.kept.is_none())
        {
            wearing.work.filled += count(held.region.area()?)?;
        }
        wearing.phase = wearing.after_fill();
        Ok(wearing)
    }

    /// The least rectangle about the ground it fills from the land, where it
    /// fills any.
    #[must_use]
    pub fn fills(&self) -> Option<Placed> {
        self.stages[0].missing()
    }

    /// What the wearing does, in all, and how much of it it has done.
    #[must_use]
    pub const fn work(&self) -> (Work, Work) {
        (self.work, self.done)
    }

    /// `stage`'s blocks, each read from `store` where it keeps it whole;
    /// one kept with too few or too many samples is made again.
    fn read(
        &self,
        mut stage: Stage,
        worn: usize,
        store: &mut dyn Store,
    ) -> Result<Stage, TerrainError> {
        let tracked = self.tracked && worn > 0;
        let worn = u32::try_from(worn).map_err(|_| TerrainError::Shape)?;
        let (first, across) = (stage.first, stage.across);
        for (index, held) in stage.blocks.iter_mut().enumerate() {
            let block = stage_block(first, across, index, worn);
            let area = held.region.area()?;
            let mut found = Ok(None);
            store.read(block, &mut |kept| {
                let tracks = if tracked { area } else { 0 };
                if kept.ground.len() == area && kept.track.len() == tracks {
                    found = copied(&kept.ground)
                        .and_then(|ground| {
                            Ok(Kept {
                                ground,
                                track: copied(&kept.track)?,
                            })
                        })
                        .map(Some);
                }
            });
            held.kept = found?;
        }
        Ok(stage)
    }

    /// The phase after the ground is filled: the first pass with blocks to
    /// make, or done.
    fn after_fill(&self) -> Phase {
        if self.stages[0].blocks.iter().any(|held| held.kept.is_none()) {
            Phase::Fill { block: 0, row: 0 }
        } else {
            self.passes_from(0)
        }
    }

    /// The first pass from `pass` with blocks to make, or done.
    fn passes_from(&self, pass: u32) -> Phase {
        (pass..PASSES)
            .find(|&pass| {
                self.stages
                    .get(pass as usize + 1)
                    .is_some_and(|stage| stage.blocks.iter().any(|held| held.kept.is_none()))
            })
            .map_or(Phase::Done, |pass| Phase::Pass { pass, next: 0 })
    }

    /// Do the next unit of the wearing across `runner`, filling from `source`
    /// and offering what it makes to `store`: whether it is done. A refused
    /// unit leaves it where it was.
    pub fn step(
        &mut self,
        (tiles, source): (&Tiles, &dyn Source),
        store: &mut dyn Store,
        runner: &dyn JobRunner,
    ) -> Result<bool, TerrainError> {
        self.phase = match self.phase {
            Phase::Fill { block, row } => self.fill(source, (block, row), runner)?,
            Phase::Pass { pass, next } => self.pass(tiles, (pass, next), store, runner)?,
            Phase::Done => Phase::Done,
        };
        Ok(self.phase == Phase::Done)
    }

    /// A unit of the ground before the run filled from row `row` of block
    /// `block` on, `FILL_ROWS` rows of a block a core. A refused unit leaves
    /// it where it was.
    fn fill(
        &mut self,
        source: &dyn Source,
        (block, row): (usize, u32),
        runner: &dyn JobRunner,
    ) -> Result<Phase, TerrainError> {
        let blocks = &self.stages[0].blocks;
        let (bands, (at, from)) = bands_from(blocks, (block, row), runner.width().max(1))?;
        // The block the last unit left part filled is carried on; every other
        // block a band reaches is begun at its first row.
        let carried = (row > 0).then_some(block);
        let mut begun: Vec<(usize, Vec<f32>)> = Vec::new();
        if !fallible::reserve(&mut begun, bands.len()) {
            return Err(TerrainError::OutOfMemory);
        }
        for &(index, ..) in &bands {
            if Some(index) == carried || begun.last().is_some_and(|(held, _)| *held == index) {
                continue;
            }
            let area = blocks
                .get(index)
                .ok_or(TerrainError::Shape)?
                .region
                .area()?;
            let ground = fallible::filled(area, 0.0).ok_or(TerrainError::OutOfMemory)?;
            begun.push((index, ground));
        }
        let mut jobs: Vec<(Placed, &mut [f32], bool)> = Vec::new();
        if !fallible::reserve(&mut jobs, bands.len()) {
            return Err(TerrainError::OutOfMemory);
        }
        let in_hand = match carried {
            Some(index) => Some((index, self.filling.as_mut().ok_or(TerrainError::Shape)?)),
            None => None,
        };
        let band_rows = usize::try_from(FILL_ROWS).map_err(|_| TerrainError::Shape)?;
        for (index, ground) in in_hand
            .into_iter()
            .chain(begun.iter_mut().map(|(index, ground)| (*index, ground)))
        {
            let region = blocks.get(index).ok_or(TerrainError::Shape)?.region;
            let across = usize::try_from(region.size.0).map_err(|_| TerrainError::Shape)?;
            for (first, rows) in (0..)
                .step_by(band_rows)
                .zip(ground.chunks_mut((band_rows * across).max(1)))
            {
                let Some(&(.., to)) = bands.iter().find(|&&(held, start, _)| {
                    held == index && usize::try_from(start).ok() == Some(first)
                }) else {
                    continue;
                };
                let start = u32::try_from(first).map_err(|_| TerrainError::Shape)?;
                let placed = Placed {
                    origin: (region.origin.0, region.origin.1 + i64::from(start)),
                    size: (region.size.0, to - start),
                };
                jobs.push((placed, rows, false));
            }
        }
        tairix_parallel::for_each(runner, &mut jobs, &|(region, ground, refused)| {
            *refused = source.fill(*region, ground).is_err();
        });
        if jobs.iter().any(|(.., refused)| *refused) {
            return Err(TerrainError::Shape);
        }
        let filled: usize = jobs.iter().map(|(_, ground, _)| ground.len()).sum();
        drop(jobs);
        self.done.filled += count(filled)?;
        // A block the bands end part way through is carried to the next unit.
        let blocks = &self.stages[0].blocks;
        let part = bands
            .last()
            .filter(|&&(index, _, to)| {
                blocks
                    .get(index)
                    .is_some_and(|held| to < held.region.size.1)
            })
            .map(|&(index, ..)| index);
        self.keep_filled((carried, begun), part);
        Ok(if at >= self.stages[0].blocks.len() {
            self.passes_from(0)
        } else {
            Phase::Fill {
                block: at,
                row: from,
            }
        })
    }

    /// Keep as filled the block in hand `carried` and each block `begun`, but
    /// `part`, the one the fill ended part way through, which is carried on.
    fn keep_filled(
        &mut self,
        (carried, begun): (Option<usize>, Vec<(usize, Vec<f32>)>),
        part: Option<usize>,
    ) {
        let completed = carried
            .filter(|&index| part != Some(index))
            .and_then(|index| Some((index, self.filling.take()?)));
        for (index, ground) in completed.into_iter().chain(begun) {
            if Some(index) == part {
                self.filling = Some(ground);
            } else if let Some(held) = self.stages[0].blocks.get_mut(index) {
                held.kept = Some(Kept {
                    ground,
                    track: Vec::new(),
                });
                held.made = true;
            }
        }
    }

    /// A unit of pass `pass`'s tiles from its `next` on, a batch as many as
    /// `runner` is wide when it begins: their windows of the stage before
    /// gathered, then each run over its own `DROPLET_UNIT` droplets a unit,
    /// their changes summed into the blocks the pass makes in the tiles' order
    /// once every one has run. Once all have, the stage before is let go,
    /// what it made offered to `store`. A refused unit begins its batch again,
    /// and nothing after the sum can refuse, so no batch is summed twice.
    fn pass(
        &mut self,
        tiles: &Tiles,
        (pass, next): (u32, usize),
        store: &mut dyn Store,
        runner: &dyn JobRunner,
    ) -> Result<Phase, TerrainError> {
        let at = pass as usize;
        if self.begun == 0 {
            if next == 0 {
                self.begin(at, runner)?;
            }
            let batch = self.tiles[at].get(next..).unwrap_or_default();
            let batch = batch
                .get(..batch.len().min(runner.width().max(1)))
                .unwrap_or_default();
            let gathering = (&mut self.turns, self.tracked);
            gather_turns(gathering, (tiles, &self.stages[at]), (batch, runner))?;
            self.begun = batch.len();
            if self.begun > 0 {
                return Ok(Phase::Pass { pass, next });
            }
        }
        let turns = self.turns.get_mut(..self.begun).unwrap_or_default();
        tairix_parallel::for_each(runner, turns, &|turn| {
            let Turn {
                ground,
                track,
                running: Some(running),
                refused,
                ..
            } = turn
            else {
                return;
            };
            let track = (!track.is_empty()).then_some(track.as_mut_slice());
            *refused = tiles
                .run_on(running, (ground, track), DROPLET_UNIT)
                .is_err();
        });
        let turns = self.turns.get_mut(..self.begun).unwrap_or_default();
        let unfinished = |turn: &Turn| {
            turn.running
                .as_ref()
                .is_some_and(|running| running.left > 0)
        };
        if !turns.iter().any(|turn| turn.refused) && turns.iter().any(unfinished) {
            return Ok(Phase::Pass { pass, next });
        }
        let droplets = settle(turns, tiles).inspect_err(|_| self.begun = 0)?;
        let (before, made) = self.stages.split_at_mut(at + 1);
        let (before, made) = (&before[at], &mut made[0]);
        let turns = self.turns.get(..self.begun).unwrap_or_default();
        let (first, across) = (made.first, made.across);
        let targets = made
            .blocks
            .iter_mut()
            .enumerate()
            .filter(|(_, held)| held.made);
        tairix_parallel::for_each_drawn(runner, targets, &|(index, target)| {
            if let Some(base) = before.at(block_place(first, across, index)) {
                for turn in turns {
                    add(target, base, turn);
                }
            }
        });
        self.done.droplets += droplets;
        let next = next + self.begun;
        self.begun = 0;
        Ok(if next < self.tiles[at].len() {
            Phase::Pass { pass, next }
        } else {
            offer(&mut self.stages[at], pass, store);
            self.stages[at] = Stage::default();
            self.passes_from(pass + 1)
        })
    }

    /// The blocks pass `at` makes, each begun across `runner` as the stage
    /// before holds it, with no water carried yet if it keeps the tracks and
    /// none were before. A refusal leaves those begun so far begun.
    fn begin(&mut self, at: usize, runner: &dyn JobRunner) -> Result<(), TerrainError> {
        let tracked = self.tracked;
        let (before, made) = self.stages.split_at_mut(at + 1);
        let (before, made) = (&before[at], &mut made[0]);
        let (first, across) = (made.first, made.across);
        let unbegun = made
            .blocks
            .iter_mut()
            .enumerate()
            .filter(|(_, held)| held.kept.is_none());
        tairix_parallel::fold_drawn(
            runner,
            unbegun,
            Ok(()),
            &|(index, held)| begun(held, before.at(block_place(first, across, index)), tracked),
            &|joined: Result<(), TerrainError>, next| joined.and(next),
        )
    }

    /// The piece's ground after every pass, row by row, and the water carried
    /// past each of its samples where the wearing keeps it, once it is worn;
    /// what it made offered to `store`. `Shape` before it is worn; a refusal
    /// of room for them leaves it as it was, to finish again.
    pub fn finish(&mut self, store: &mut dyn Store) -> Result<(Vec<f32>, Vec<f32>), TerrainError> {
        if self.phase != Phase::Done {
            return Err(TerrainError::Shape);
        }
        let area = self.piece.area()?;
        let mut ground = fallible::filled(area, 0.0).ok_or(TerrainError::OutOfMemory)?;
        let mut track = if self.tracked {
            fallible::filled(area, 0.0).ok_or(TerrainError::OutOfMemory)?
        } else {
            Vec::new()
        };
        let last = PASSES as usize;
        let tracks = self.tracked.then_some(track.as_mut_slice());
        gather(&self.stages[last], self.piece, (&mut ground, tracks))?;
        for (worn, stage) in (0..).zip(self.stages.iter_mut()) {
            offer(stage, worn, store);
        }
        Ok((ground, track))
    }
}

impl Stage {
    /// The blocks over `region`, row by row, each covering its samples within
    /// `domain`, none held yet.
    fn over(region: Placed, domain: Placed) -> Result<Self, TerrainError> {
        let block = i64::from(BLOCK);
        let end = region.end()?;
        let first = (
            region.origin.0.div_euclid(block),
            region.origin.1.div_euclid(block),
        );
        let last = ((end.0 - 1).div_euclid(block), (end.1 - 1).div_euclid(block));
        let across = usize::try_from(last.0 - first.0 + 1).map_err(|_| TerrainError::Shape)?;
        let down = usize::try_from(last.1 - first.1 + 1).map_err(|_| TerrainError::Shape)?;
        let mut blocks = Vec::new();
        if !fallible::reserve(
            &mut blocks,
            across.checked_mul(down).ok_or(TerrainError::Shape)?,
        ) {
            return Err(TerrainError::OutOfMemory);
        }
        for row in first.1..=last.1 {
            for column in first.0..=last.0 {
                let square = Placed::square((column * block, row * block), BLOCK);
                blocks.push(Held {
                    region: square.within(domain).ok_or(TerrainError::Shape)?,
                    kept: None,
                    made: false,
                });
            }
        }
        Ok(Self {
            first,
            across,
            blocks,
        })
    }

    /// Its block at `place` among the lattice's blocks, if it holds one there.
    fn at(&self, (column, row): (i64, i64)) -> Option<&Held> {
        let column = usize::try_from(column.checked_sub(self.first.0)?)
            .ok()
            .filter(|&column| column < self.across)?;
        let row = usize::try_from(row.checked_sub(self.first.1)?).ok()?;
        self.blocks
            .get(row.checked_mul(self.across)?.checked_add(column)?)
    }

    /// The least rectangle about the blocks no store kept, if any.
    fn missing(&self) -> Option<Placed> {
        self.blocks
            .iter()
            .filter(|held| held.kept.is_none())
            .map(|held| held.region)
            .try_fold(None, |about: Option<Placed>, region| {
                Some(Some(match about {
                    Some(about) => about.cover(region).ok()?,
                    None => region,
                }))
            })
            .flatten()
    }
}

/// The block `index` of the stage whose first block is `first`, `across` to
/// a row, worn by `worn` passes.
fn stage_block(first: (i64, i64), across: usize, index: usize, worn: u32) -> Block {
    let (column, row) = block_place(first, across, index);
    Block { worn, row, column }
}

/// Where block `index` of a stage whose first block is `first`, `across` to a
/// row, lies among the lattice's blocks.
fn block_place(first: (i64, i64), across: usize, index: usize) -> (i64, i64) {
    let across = across.max(1);
    (
        first.0 + to_i64(index % across),
        first.1 + to_i64(index / across),
    )
}

/// `held` begun as `base` holds it, with no water carried yet if `tracked`
/// and none was before.
fn begun(held: &mut Held, base: Option<&Held>, tracked: bool) -> Result<(), TerrainError> {
    let base = base
        .and_then(|base| base.kept.as_ref())
        .ok_or(TerrainError::Shape)?;
    let ground = copied(&base.ground)?;
    let track = if !tracked {
        Vec::new()
    } else if base.track.is_empty() {
        fallible::filled(held.region.area()?, 0.0).ok_or(TerrainError::OutOfMemory)?
    } else {
        copied(&base.track)?
    };
    held.kept = Some(Kept { ground, track });
    held.made = true;
    Ok(())
}

/// Offer `stage`'s blocks made here, worn by `worn` passes, to `store`.
fn offer(stage: &mut Stage, worn: u32, store: &mut dyn Store) {
    let (first, across) = (stage.first, stage.across);
    for (index, held) in stage.blocks.iter_mut().enumerate() {
        if held.made {
            if let Some(kept) = held.kept.take() {
                store.keep(stage_block(first, across, index, worn), kept);
            }
            held.made = false;
        }
    }
}

/// The turns of `batch`, tiles of the pass after `before`, keeping the
/// water's tracks if `tracked`: each tile's run begun and its window gathered
/// from `before` across `runner`.
fn gather_turns(
    (turns, tracked): (&mut Vec<Turn>, bool),
    (tiles, before): (&Tiles, &Stage),
    (batch, runner): (&[Tile], &dyn JobRunner),
) -> Result<(), TerrainError> {
    if let Some(more) = batch
        .len()
        .checked_sub(turns.len())
        .filter(|&more| more > 0)
    {
        if !fallible::reserve(turns, more) {
            return Err(TerrainError::OutOfMemory);
        }
        turns.resize_with(batch.len(), Turn::default);
    }
    for (turn, &tile) in turns.iter_mut().zip(batch) {
        let window = tiles.window(tile)?;
        let area = window.area()?;
        sized(&mut turn.ground, area)?;
        if tracked {
            sized(&mut turn.track, area)?;
        } else {
            turn.track.clear();
        }
        (turn.window, turn.touched) = (Some(window), None);
        turn.running = Some(tiles.begin(tile)?);
    }
    let turns = turns.get_mut(..batch.len()).unwrap_or_default();
    tairix_parallel::for_each(runner, turns, &|turn| {
        turn.refused = turn
            .window
            .is_none_or(|window| gather(before, window, (&mut turn.ground, None)).is_err());
        turn.track.fill(0.0);
    });
    if turns.iter().any(|turn| turn.refused) {
        Err(TerrainError::Shape)
    } else {
        Ok(())
    }
}

/// Every turn's run ended: the box each touched noted, and how many
/// droplets they ran. `Shape` for a turn refused, never begun, or not yet
/// run to its end.
fn settle(turns: &mut [Turn], tiles: &Tiles) -> Result<u64, TerrainError> {
    let mut droplets = 0;
    for turn in turns {
        let Some(running) = turn
            .running
            .as_ref()
            .filter(|running| !turn.refused && running.left == 0)
        else {
            return Err(TerrainError::Shape);
        };
        turn.touched = tiles.touched(running)?;
        droplets += u64::from(tiles.droplets(running.tile)?);
    }
    Ok(droplets)
}

/// A band of a block's rows a core fills: the block, and its rows from and
/// to.
type Band = (usize, u32, u32);

/// The bands of `blocks`' rows a unit `width` cores wide fills from row
/// `row` of block `block` on, `FILL_ROWS` rows a core, past every block held
/// already: each band's block and rows, and where the unit after starts.
fn bands_from(
    blocks: &[Held],
    (block, row): (usize, u32),
    width: usize,
) -> Result<(Vec<Band>, (usize, u32)), TerrainError> {
    let mut bands = Vec::new();
    if !fallible::reserve(&mut bands, width) {
        return Err(TerrainError::OutOfMemory);
    }
    let (mut at, mut from) = (block, row);
    while let Some(held) = blocks.get(at).filter(|_| bands.len() < width) {
        let rows = held.region.size.1;
        if held.kept.is_some() || from >= rows {
            (at, from) = (at + 1, 0);
            continue;
        }
        let to = from.saturating_add(FILL_ROWS).min(rows);
        bands.push((at, from, to));
        (at, from) = if to >= rows { (at + 1, 0) } else { (at, to) };
    }
    Ok((bands, (at, from)))
}

/// Copy `stage`'s ground over `region` into `ground`, and its water's tracks
/// into `track` where given, each row by row: `Shape` where a block over it
/// is not held.
fn gather(
    stage: &Stage,
    region: Placed,
    (ground, mut track): (&mut [f32], Option<&mut [f32]>),
) -> Result<(), TerrainError> {
    let area = region.area()?;
    if ground.len() != area || track.as_deref().is_some_and(|track| track.len() != area) {
        return Err(TerrainError::Shape);
    }
    if area == 0 {
        return Ok(());
    }
    let (block, end) = (i64::from(BLOCK), region.end()?);
    let places =
        (region.origin.1.div_euclid(block)..=(end.1 - 1).div_euclid(block)).flat_map(|row| {
            (region.origin.0.div_euclid(block)..=(end.0 - 1).div_euclid(block))
                .map(move |column| (column, row))
        });
    let mut covered = 0_usize;
    for place in places {
        let held = stage.at(place).ok_or(TerrainError::Shape)?;
        let Some(overlap) = held.region.within(region) else {
            continue;
        };
        let kept = held.kept.as_ref().ok_or(TerrainError::Shape)?;
        let width = usize::try_from(overlap.size.0).map_err(|_| TerrainError::Shape)?;
        for y in overlap.origin.1..overlap.origin.1 + i64::from(overlap.size.1) {
            let from = held
                .region
                .index((overlap.origin.0, y))
                .ok_or(TerrainError::Shape)?;
            let to = region
                .index((overlap.origin.0, y))
                .ok_or(TerrainError::Shape)?;
            let (source, target) = (
                kept.ground
                    .get(from..from + width)
                    .ok_or(TerrainError::Shape)?,
                ground.get_mut(to..to + width).ok_or(TerrainError::Shape)?,
            );
            target.copy_from_slice(source);
            if let Some(track) = track.as_deref_mut() {
                let target = track.get_mut(to..to + width).ok_or(TerrainError::Shape)?;
                match kept.track.get(from..from + width) {
                    Some(source) => target.copy_from_slice(source),
                    None => target.fill(0.0),
                }
            }
        }
        covered += overlap.area()?;
    }
    if covered == area {
        Ok(())
    } else {
        Err(TerrainError::Shape)
    }
}

/// Add `turn`'s change over the box its droplets touched to `target`, whose
/// ground before the pass `base` holds: as a run sums a pass's changes, so
/// turns added in their tiles' order wear it as the land worn whole.
fn add(target: &mut Held, base: &Held, turn: &Turn) {
    let (Some(window), Some(touched)) = (turn.window, turn.touched) else {
        return;
    };
    let (Some(overlap), Some(kept), Some(was)) = (
        touched.within(target.region),
        target.kept.as_mut(),
        base.kept.as_ref(),
    ) else {
        return;
    };
    let Ok(width) = usize::try_from(overlap.size.0) else {
        return;
    };
    for y in overlap.origin.1..overlap.origin.1 + i64::from(overlap.size.1) {
        let (Some(here), Some(there)) = (
            target.region.index((overlap.origin.0, y)),
            window.index((overlap.origin.0, y)),
        ) else {
            continue;
        };
        let (Some(slots), Some(now), Some(before)) = (
            kept.ground.get_mut(here..here + width),
            turn.ground.get(there..there + width),
            was.ground.get(here..here + width),
        ) else {
            continue;
        };
        for ((slot, &now), &before) in slots.iter_mut().zip(now).zip(before) {
            *slot += now - before;
        }
        if let (Some(slots), Some(carried)) = (
            kept.track.get_mut(here..here + width),
            turn.track.get(there..there + width),
        ) {
            for (slot, &carried) in slots.iter_mut().zip(carried) {
                *slot += carried;
            }
        }
    }
}

/// `buffer` holding `area` samples, its allocation kept from turn to turn.
fn sized(buffer: &mut Vec<f32>, area: usize) -> Result<(), TerrainError> {
    if buffer.len() < area && !fallible::reserve(buffer, area - buffer.len()) {
        return Err(TerrainError::OutOfMemory);
    }
    buffer.resize(area, 0.0);
    Ok(())
}

/// A copy of `values`, refused rather than aborting on a short heap.
fn copied(values: &[f32]) -> Result<Vec<f32>, TerrainError> {
    fallible::collected(values.len(), values.iter().copied()).ok_or(TerrainError::OutOfMemory)
}

/// A count of samples, as a run's work counts them.
fn count(samples: usize) -> Result<u64, TerrainError> {
    u64::try_from(samples).map_err(|_| TerrainError::Shape)
}

/// An index into a stage's blocks, as a lattice offset.
fn to_i64(index: usize) -> i64 {
    i64::try_from(index).unwrap_or(i64::MAX)
}

/// Where a tile's droplets may start: the first corner, and the breadth on
/// either axis.
#[derive(Copy, Clone, Debug)]
struct Span {
    from: (f64, f64),
    breadth: (f64, f64),
}

/// The samples of a tile's copy its droplets changed, as the least box about
/// them: columns and rows, each from the first to past the last.
#[derive(Copy, Clone, Debug)]
struct Touched {
    columns: (usize, usize),
    rows: (usize, usize),
}

impl Touched {
    const NOTHING: Self = Self {
        columns: (usize::MAX, 0),
        rows: (usize::MAX, 0),
    };

    /// Grown to hold the samples from `(left, top)` to `(right, bottom)`,
    /// both included.
    fn include(&mut self, (left, top): (usize, usize), (right, bottom): (usize, usize)) {
        self.columns = (self.columns.0.min(left), self.columns.1.max(right + 1));
        self.rows = (self.rows.0.min(top), self.rows.1.max(bottom + 1));
    }

    /// Whether it holds nothing.
    const fn is_empty(self) -> bool {
        self.columns.0 >= self.columns.1
    }

    /// The box in the lattice's places, the copy's first sample lying at
    /// `origin`; `None` where it holds nothing.
    fn placed(self, origin: (i64, i64)) -> Result<Option<Placed>, TerrainError> {
        if self.is_empty() {
            return Ok(None);
        }
        let place = |at: usize| i64::try_from(at).map_err(|_| TerrainError::Shape);
        let size =
            |(from, to): (usize, usize)| u32::try_from(to - from).map_err(|_| TerrainError::Shape);
        Ok(Some(Placed {
            origin: (
                origin.0 + place(self.columns.0)?,
                origin.1 + place(self.rows.0)?,
            ),
            size: (size(self.columns)?, size(self.rows)?),
        }))
    }
}

/// The samples a droplet wears about its cell's first corner, as offsets and
/// shares, and how far the farthest of them lies either way.
#[derive(Copy, Clone, Debug)]
struct Brush<'a> {
    samples: &'a [(isize, isize, f64)],
    spread: usize,
}

/// The brush a droplet `radius` samples about wears with: each sample within
/// it, and its share, nearer samples taking more.
fn brush(radius: u32) -> Result<Vec<(isize, isize, f64)>, TerrainError> {
    let reach = i32::try_from(radius).map_err(|_| TerrainError::Shape)?;
    let across = usize::try_from(radius)
        .ok()
        .and_then(|radius| radius.checked_mul(2))
        .and_then(|across| across.checked_add(1))
        .ok_or(TerrainError::Shape)?;
    let mut brush = Vec::new();
    if !fallible::reserve(
        &mut brush,
        across.checked_mul(across).ok_or(TerrainError::Shape)?,
    ) {
        return Err(TerrainError::OutOfMemory);
    }
    let mut total = 0.0;
    for dy in -reach..=reach {
        for dx in -reach..=reach {
            let weight = f64::from(radius) - mathf::sqrt(f64::from(dx * dx + dy * dy));
            if weight > 0.0 {
                total += weight;
                let offset = |at: i32| isize::try_from(at).map_err(|_| TerrainError::Shape);
                brush.push((offset(dx)?, offset(dy)?, weight));
            }
        }
    }
    for (_, _, weight) in &mut brush {
        *weight /= total;
    }
    Ok(brush)
}

/// A tile's copy of the ground it reaches and of the water's tracks there,
/// its first sample at `origin` on the lattice, and what its droplets have
/// changed. Where the region held cuts it short, a read past it stops a
/// droplet and a wear past it is lost: why a piece is exact only within the
/// region less the halo.
struct Reached<'a> {
    origin: (i64, i64),
    columns: usize,
    rows: usize,
    height: &'a mut [f32],
    track: Option<&'a mut [f32]>,
    touched: Touched,
}

impl Reached<'_> {
    /// The copy's column and row of the cell whose first corner is the
    /// lattice place `(x, y)`, where all four of its corners lie in the copy.
    fn cell(&self, (x, y): (i64, i64)) -> Option<(usize, usize)> {
        let column = usize::try_from(x.checked_sub(self.origin.0)?).ok()?;
        let row = usize::try_from(y.checked_sub(self.origin.1)?).ok()?;
        (column + 1 < self.columns && row + 1 < self.rows).then_some((column, row))
    }

    fn height(&self, (column, row): (usize, usize)) -> Option<f64> {
        self.height
            .get(row * self.columns + column)
            .map(|&value| f64::from(value))
    }

    fn add(&mut self, (column, row): (usize, usize), amount: f64) {
        if let Some(slot) = self.height.get_mut(row * self.columns + column) {
            *slot += mathf::single(amount);
        }
    }

    fn carry(&mut self, (column, row): (usize, usize), water: f64) {
        let index = row * self.columns + column;
        if let Some(slot) = self
            .track
            .as_deref_mut()
            .and_then(|track| track.get_mut(index))
        {
            *slot += mathf::single(water);
            self.touched.include((column, row), (column, row));
        }
    }
}

/// One droplet from `start`, kept within `bounds`.
fn droplet(
    (law, brush): (Droplets, Brush<'_>),
    reached: &mut Reached<'_>,
    (mut x, mut y): (f64, f64),
    bounds: Bounds,
) {
    let (mut dx, mut dy) = (0.0_f64, 0.0_f64);
    let (mut speed, mut water, mut sediment) = (1.0_f64, 1.0_f64, 0.0_f64);
    for _ in 0..law.lifetime {
        let Some(here) = sample(reached, (x, y)) else {
            return;
        };
        dx = dx * law.inertia - here.gradient.0 * (1.0 - law.inertia);
        dy = dy * law.inertia - here.gradient.1 * (1.0 - law.inertia);
        let length = mathf::sqrt(dx * dx + dy * dy);
        if length < 1e-12 {
            // On the dead level it has nowhere to run.
            deposit(reached, &here, sediment);
            return;
        }
        let (nx, ny) = (x + dx / length, y + dy / length);
        if !bounds.hold((nx, ny)) {
            return;
        }
        let Some(next) = sample(reached, (nx, ny)) else {
            return;
        };
        let fall = next.height - here.height;
        let capacity = mathf::fmax(-fall, law.min_slope) * speed * water * law.capacity;
        if sediment > capacity || fall > 0.0 {
            let dropped = if fall > 0.0 {
                mathf::fmin(fall, sediment)
            } else {
                (sediment - capacity) * law.deposition
            };
            sediment -= dropped;
            deposit(reached, &here, dropped);
        } else {
            // Never deeper than the ground it is running to, or it would
            // dig the pit it then has to fill.
            let taken = mathf::fmin((capacity - sediment) * law.erosion, -fall);
            wear(reached, brush, &here, taken);
            sediment += taken;
        }
        reached.carry(here.corner, water);
        speed = mathf::sqrt(speed * speed - fall * law.gravity) * (1.0 - law.friction);
        water *= 1.0 - law.evaporation;
        (x, y) = (nx, ny);
    }
    // Run dry, it lays down whatever it still carries where it stands.
    if let Some(here) = sample(reached, (x, y)) {
        deposit(reached, &here, sediment);
    }
}

/// Where a droplet stands.
struct Ground {
    /// The copy's column and row of its cell's first corner.
    corner: (usize, usize),
    across: f64,
    down: f64,
    height: f64,
    gradient: (f64, f64),
}

/// The ground under `(x, y)`: its cell, its height and its slope; `None`
/// where the copy does not reach.
fn sample(reached: &Reached<'_>, (x, y): (f64, f64)) -> Option<Ground> {
    let (column, row) = (mathf::floor(x), mathf::floor(y));
    let (across, down) = (x - column, y - row);
    let corner = reached.cell((whole(column)?, whole(row)?))?;
    let (left, top) = corner;
    let (h00, h10) = (reached.height(corner)?, reached.height((left + 1, top))?);
    let (h01, h11) = (
        reached.height((left, top + 1))?,
        reached.height((left + 1, top + 1))?,
    );
    Some(Ground {
        corner,
        across,
        down,
        height: mathf::bilinear([h00, h10, h01, h11], (across, down)),
        gradient: (
            (h10 - h00) * (1.0 - down) + (h11 - h01) * down,
            (h01 - h00) * (1.0 - across) + (h11 - h10) * across,
        ),
    })
}

/// Lay `amount` on the four samples of `at`'s cell, each by its nearness.
fn deposit(reached: &mut Reached<'_>, at: &Ground, amount: f64) {
    if amount <= 0.0 {
        return;
    }
    let ((left, top), across, down) = (at.corner, at.across, at.down);
    reached.add((left, top), amount * (1.0 - across) * (1.0 - down));
    reached.add((left + 1, top), amount * across * (1.0 - down));
    reached.add((left, top + 1), amount * (1.0 - across) * down);
    reached.add((left + 1, top + 1), amount * across * down);
    reached.touched.include((left, top), (left + 1, top + 1));
}

/// Wear `amount` from the brush's samples about `at`'s cell; those past the
/// copy are lost.
fn wear(reached: &mut Reached<'_>, brush: Brush<'_>, at: &Ground, amount: f64) {
    let Brush { samples, spread } = brush;
    if amount <= 0.0 {
        return;
    }
    let (column, row) = at.corner;
    let within = column >= spread
        && row >= spread
        && column + spread < reached.columns
        && row + spread < reached.rows;
    for &(dx, dy, share) in samples {
        let at = if within {
            Some((column.wrapping_add_signed(dx), row.wrapping_add_signed(dy)))
        } else {
            column
                .checked_add_signed(dx)
                .zip(row.checked_add_signed(dy))
                .filter(|&(x, y)| x < reached.columns && y < reached.rows)
        };
        if let Some(at) = at {
            reached.add(at, -amount * share);
        }
    }
    let low = (column.saturating_sub(spread), row.saturating_sub(spread));
    let high = (
        (column + spread).min(reached.columns - 1),
        (row + spread).min(reached.rows - 1),
    );
    reached.touched.include(low, high);
}

/// A whole float within the lattice a run reaches, as an `i64`.
fn whole(value: f64) -> Option<i64> {
    const LIMIT: f64 = 9_007_199_254_740_992.0;
    if !(-LIMIT..=LIMIT).contains(&value) {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "range-checked above, and whole, so the conversion is exact"
    )]
    Some(value as i64)
}

/// `value` divided by `by`, a positive divisor, rounded up.
const fn ceiling(value: i64, by: i64) -> i64 {
    let floor = value.div_euclid(by);
    if floor * by == value {
        floor
    } else {
        floor + 1
    }
}

#[cfg(test)]
#[path = "droplet_tests.rs"]
mod tests;
