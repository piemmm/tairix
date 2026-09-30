//! One chunk of ground, and the resumable machine that builds it.
//!
//! # The halo, and why it is exactly this wide
//!
//! Everything here reads the realm field — which is global, so identical
//! for every query — plus pure functions of absolute position, plus a
//! **fixed ring of cells around the chunk**. The ring exists for the
//! quantities a cell's neighbours can change: the shore distance, a
//! distance transform, and the hillslope gradient wetness is read from. The
//! transform is run over the chunk *plus* the ring, so a cell inside the
//! chunk gets the same answer it would get if the whole world had been
//! transformed at once. That is what makes "a chunk generated alone equals
//! the same chunk generated as part of its neighbourhood" a theorem rather
//! than a hope.
//!
//! Scatter reads a scatter step into the ring as well, because a candidate
//! just outside the chunk can exclude one just inside it. Every quantity a
//! candidate's footing reads is therefore resolved exactly that far out: a
//! shore reaches [`SHORE_REACH`] cells, the wetness stencil
//! [`WETNESS_SPAN`], and the structure stamp covers the whole ring, so a
//! neighbour's candidate reads the same from either side of the seam.
//!
//! # Why it is resumable
//!
//! A client must never block a frame on generation. The build is a
//! sequence of bounded phases, each advanced by one [`ChunkBuild::step`],
//! and the partially-built chunk is readable throughout — so a chunk that
//! is not yet finished draws as the coarse relief the earlier phase
//! already answered rather than as nothing. Stopping between any two
//! phases and resuming later produces the same chunk as running them
//! back to back, because no phase reads anything a later one writes.

use alloc::vec::Vec;
use core::ops::RangeInclusive;

use tairix_util::mathf;
use tairix_util::secret::wipe_with;
use tairix_wintersun_net::value::{ChunkCoord, WorldPoint};

use crate::biome::{self, Biome, Conditions, Water, SHORE_REACH};
use crate::blend::Blend;
use crate::climate::LAPSE_RATE;
use crate::error::WorldError;
use crate::geology::{soils, SoilSite};
use crate::geom::{
    chunk_origin, lerp, rise, signed, CellCoord, Elevation, Precipitation, Temperature,
    CELL_SUB_UNITS, CHUNK_AREA, CHUNK_CELLS,
};
use crate::ground::{self, Ground, GroundSite};
use crate::hydrology::{self, FlowDir};
use crate::noise;
use crate::realm::{Coarse, RealmField};
use crate::scatter::{self, Footing, ScatterKind, Scattered, SCATTER_STEP};
use crate::seed::{SeedKey, Stage};

/// Cells the halo reaches beyond the chunk on every side.
pub const SHORE_CELLS: u32 = 8;

/// Cells either side of a cell the hillslope gradient is measured across.
pub const WETNESS_SPAN: u32 = 3;

// A scatter candidate a step outside the chunk must still see the whole
// shore band and the whole wetness stencil inside the halo.
const _: () = assert!(SCATTER_STEP + SHORE_REACH as u32 <= SHORE_CELLS);
const _: () = assert!(SCATTER_STEP + WETNESS_SPAN <= SHORE_CELLS);

/// Cells along one edge of the working grid: the chunk plus its halo.
const WORK_CELLS: u32 = CHUNK_CELLS + 2 * SHORE_CELLS;

/// Cells in the working grid.
const WORK_AREA: usize = (WORK_CELLS as usize) * (WORK_CELLS as usize);

/// World units between cycles of the fine detail field.
///
/// Absolute, not realm-relative: a hillside's texture is the same size in
/// a small realm and a large one, because it is texture and not structure.
const DETAIL_UNITS: f64 = 380.0;

/// Peak fine detail, in world units, on open ground.
const DETAIL_AMPLITUDE: f64 = 22.0;

/// Extra detail amplitude at the heart of a mountain belt.
const BELT_AMPLITUDE: f64 = 46.0;

/// World units between cycles of the dune field.
const DUNE_UNITS: f64 = 90.0;

/// Peak dune amplitude, in world units.
const DUNE_AMPLITUDE: f64 = 7.0;

/// Widest a channel's bed gets, in cells.
const CHANNEL_MAX_HALF_WIDTH: f64 = 7.0;

/// Deepest a channel cuts below its water surface, in world units.
const CHANNEL_MAX_DEPTH: f64 = 9.0;

/// Coarse discharge at which a channel reaches its widest.
const CHANNEL_FULL_DISCHARGE: f64 = 2600.0;

/// Cells either side of a road's centreline that are levelled.
const ROAD_HALF_WIDTH: f64 = 1.6;

/// Levelling strength past which a cell counts as cleared by what levelled
/// it.
const CLEARED_STRENGTH: f64 = 0.35;

/// How many cells of specific catchment a unit gradient sheds: where the
/// catchment is this times the gradient, a cell gathers as much water as it
/// sheds.
const WETNESS_SCALE: f64 = 5000.0;

/// The specific catchment, in cells, at which a river lays a floodplain, and
/// the spread of that threshold.
const FLOODPLAIN_CATCHMENT: f64 = 9600.0;
const FLOODPLAIN_SPREAD: f64 = 16_000.0;

/// The largest chunk coordinate, of either sign, whose every working cell has
/// a centre a `WorldPoint` can name.
///
/// No position reaches past it, and within it every coordinate the stages
/// derive from a cell stays far inside an `i32`.
const MAX_CHUNK_COORD: u32 = {
    let last_cell = ((i32::MAX - CELL_SUB_UNITS / 2) / CELL_SUB_UNITS).unsigned_abs();
    (last_cell - WORK_CELLS) / CHUNK_CELLS
};

/// World cells between cycles of the patch field that clusters a biome's
/// grounds.
const PATCH_CELLS: f64 = 40.0;

/// What has been built on a cell.
///
/// A byte rather than five booleans: there is one of these per cell of
/// every cached chunk.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Surface(u8);

impl Surface {
    /// A road runs over the cell.
    const ROAD: u8 = 1 << 0;
    /// A settlement has levelled the cell.
    const SETTLEMENT: u8 = 1 << 1;
    /// A river or stream bed.
    const CHANNEL: u8 = 1 << 2;
    /// Standing fresh water.
    const LAKE: u8 = 1 << 3;
    /// Sea.
    const SEA: u8 = 1 << 4;

    /// Whether a road runs over the cell.
    #[must_use]
    pub const fn is_road(self) -> bool {
        self.0 & Self::ROAD != 0
    }

    /// Whether a settlement has levelled the cell.
    #[must_use]
    pub const fn is_settlement(self) -> bool {
        self.0 & Self::SETTLEMENT != 0
    }

    /// Whether the cell is a river or stream bed.
    #[must_use]
    pub const fn is_channel(self) -> bool {
        self.0 & Self::CHANNEL != 0
    }

    /// Whether standing fresh water covers the cell.
    #[must_use]
    pub const fn is_lake(self) -> bool {
        self.0 & Self::LAKE != 0
    }

    /// Whether sea covers the cell.
    #[must_use]
    pub const fn is_sea(self) -> bool {
        self.0 & Self::SEA != 0
    }

    /// Whether anything has cleared the cell of growth.
    #[must_use]
    pub const fn is_cleared(self) -> bool {
        self.0 & (Self::ROAD | Self::SETTLEMENT) != 0
    }

    /// Whether any standing or running water covers the cell.
    #[must_use]
    pub const fn is_water(self) -> bool {
        self.0 & (Self::CHANNEL | Self::LAKE | Self::SEA) != 0
    }

    /// The packed flags, for a consumer folding a chunk into a digest.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// This surface with `flag` set.
    const fn with(self, flag: u8) -> Self {
        Self(self.0 | flag)
    }
}

/// A generated chunk.
#[derive(Clone, Debug)]
pub struct Chunk {
    coord: ChunkCoord,
    elevation: Vec<Elevation>,
    water: Vec<Elevation>,
    temperature: Vec<Temperature>,
    precipitation: Vec<Precipitation>,
    biome: Vec<Blend<Biome>>,
    ground: Vec<Blend<Ground>>,
    surface: Vec<Surface>,
    scatter: Vec<Scattered>,
}

impl Chunk {
    /// Which chunk this is.
    #[must_use]
    pub const fn coord(&self) -> ChunkCoord {
        self.coord
    }

    /// Ground height at an in-chunk cell.
    #[must_use]
    pub fn elevation(&self, cx: u32, cy: u32) -> Elevation {
        self.elevation[cell_index(cx, cy)]
    }

    /// Water-surface height at an in-chunk cell, equal to the ground
    /// where it is dry.
    #[must_use]
    pub fn water(&self, cx: u32, cy: u32) -> Elevation {
        self.water[cell_index(cx, cy)]
    }

    /// Mean annual air temperature at an in-chunk cell.
    #[must_use]
    pub fn temperature(&self, cx: u32, cy: u32) -> Temperature {
        self.temperature[cell_index(cx, cy)]
    }

    /// Annual precipitation at an in-chunk cell.
    #[must_use]
    pub fn precipitation(&self, cx: u32, cy: u32) -> Precipitation {
        self.precipitation[cell_index(cx, cy)]
    }

    /// The biomes living at an in-chunk cell, which flora and decoration
    /// read.
    #[must_use]
    pub fn biome(&self, cx: u32, cy: u32) -> Blend<Biome> {
        self.biome[cell_index(cx, cy)]
    }

    /// The grounds an in-chunk cell shows, which the splat draws.
    #[must_use]
    pub fn ground(&self, cx: u32, cy: u32) -> Blend<Ground> {
        self.ground[cell_index(cx, cy)]
    }

    /// What has been built on an in-chunk cell.
    #[must_use]
    pub fn surface(&self, cx: u32, cy: u32) -> Surface {
        self.surface[cell_index(cx, cy)]
    }

    /// Everything standing in the chunk.
    #[must_use]
    pub fn scatter(&self) -> &[Scattered] {
        &self.scatter
    }

    /// Overwrite every retained byte.
    ///
    /// Called by the cache before the allocation is freed.
    pub(crate) fn scrub(&mut self) {
        wipe_with(&mut self.elevation, Elevation::SEA_LEVEL);
        wipe_with(&mut self.water, Elevation::SEA_LEVEL);
        wipe_with(&mut self.temperature, Temperature::default());
        wipe_with(&mut self.precipitation, Precipitation::default());
        wipe_with(&mut self.biome, Blend::solid(Biome::OpenWater));
        wipe_with(&mut self.ground, Blend::solid(Ground::Water));
        wipe_with(&mut self.surface, Surface::default());
        let blank = Scattered {
            at: WorldPoint::default(),
            kind: ScatterKind::Tree,
            host: Biome::OpenWater,
            variant: 0,
            scale: 0,
        };
        wipe_with(&mut self.scatter, blank);
        self.scatter.clear();
    }

    /// Heap bytes this chunk retains.
    #[must_use]
    pub fn payload_bytes(&self) -> usize {
        core::mem::size_of_val(self.elevation.as_slice())
            + core::mem::size_of_val(self.water.as_slice())
            + core::mem::size_of_val(self.temperature.as_slice())
            + core::mem::size_of_val(self.precipitation.as_slice())
            + core::mem::size_of_val(self.biome.as_slice())
            + core::mem::size_of_val(self.ground.as_slice())
            + core::mem::size_of_val(self.surface.as_slice())
            + self.scatter.capacity() * core::mem::size_of::<Scattered>()
    }
}

/// The row-major index of an in-chunk cell, wrapped onto the chunk.
const fn cell_index(cx: u32, cy: u32) -> usize {
    let mask = CHUNK_CELLS - 1;
    (((cy & mask) * CHUNK_CELLS) + (cx & mask)) as usize
}

/// A sorted, borrowed set of resident chunks, searched by cell.
///
/// Both the simulation and a client hold a window of the chunks around
/// what they are working on and ask it which chunk a cell belongs to. The
/// *fetching* is a cache with a memory budget and belongs to the process
/// holding it; this is only the lookup, so there is one of it rather than
/// one per consumer.
#[derive(Copy, Clone, Debug)]
pub struct ChunkWindow<'a> {
    window: &'a [&'a Chunk],
}

impl<'a> ChunkWindow<'a> {
    /// Wrap a window of chunks sorted strictly by coordinate.
    ///
    /// Sorted because the lookup binary-searches it: a window is as large
    /// as the region its holder works over, and a linear scan of it per
    /// cell would put that area on the hot path.
    ///
    /// # Errors
    ///
    /// [`WorldError::UnsortedWindow`] when the slice is not strictly
    /// increasing. Strictly, so a duplicate coordinate is refused too:
    /// two chunks claiming one coordinate would make an answer depend on
    /// which the search landed on.
    pub fn new(window: &'a [&'a Chunk]) -> Result<Self, WorldError> {
        if !window.is_sorted_by(|a, b| a.coord() < b.coord()) {
            return Err(WorldError::UnsortedWindow);
        }
        Ok(Self { window })
    }

    /// The chunk holding `cell`, or `None` when it is not resident.
    #[must_use]
    pub fn chunk(&self, cell: CellCoord) -> Option<&'a Chunk> {
        let coord = cell.chunk();
        let index = self
            .window
            .binary_search_by_key(&coord, |chunk| chunk.coord())
            .ok()?;
        self.window.get(index).copied()
    }

    /// How many chunks are resident.
    #[must_use]
    pub fn len(&self) -> usize {
        self.window.len()
    }

    /// Whether no chunk is resident.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.window.is_empty()
    }
}

/// How far a build has got.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Phase {
    /// Fine relief over the working grid. Nothing is readable before it.
    Relief,
    /// Channels carved, lakes and sea filled, shore distance measured.
    Water,
    /// Settlements levelled and roads laid.
    Structures,
    /// Temperature and precipitation.
    Climate,
    /// Biomes classified and their grounds weighed.
    Biome,
    /// Vegetation, rock and resource nodes placed.
    Scatter,
    /// Nothing left to do.
    Done,
}

impl Phase {
    /// The phase after this one.
    const fn next(self) -> Self {
        match self {
            Self::Relief => Self::Water,
            Self::Water => Self::Structures,
            Self::Structures => Self::Climate,
            Self::Climate => Self::Biome,
            Self::Biome => Self::Scatter,
            Self::Scatter | Self::Done => Self::Done,
        }
    }
}

/// One coarse drainage link, as a line the fine carve follows.
#[derive(Copy, Clone, Debug)]
struct Channel {
    /// Upstream end, in cells.
    from: (f64, f64),
    /// Downstream end, in cells.
    to: (f64, f64),
    /// Water surface at each end, in world units.
    surface: (f64, f64),
    /// Half-width of the bed, in cells.
    half_width: f64,
    /// Depth below the water surface, in world units.
    depth: f64,
}

/// A chunk under construction.
#[derive(Debug)]
pub struct ChunkBuild {
    coord: ChunkCoord,
    phase: Phase,
    chunk: Chunk,
    /// Fine ground height over the chunk *and* its halo, in world units.
    work_ground: Vec<f64>,
    /// The surface a working cell presents to the air: its water where it
    /// is wet, its ground where it is dry. What a slope is measured over, so
    /// a shore is as steep as the land above the water and no steeper.
    work_level: Vec<f64>,
    /// Whether water covers each working cell.
    work_wet: Vec<bool>,
    /// Which water is nearest each working cell: a wet cell's own, a dry
    /// cell's nearest, where the shore distance is finite.
    work_water: Vec<Water>,
    /// Cells to the nearest water by the four-neighbour metric,
    /// saturating at [`u16::MAX`].
    work_shore: Vec<u16>,
    /// Whether a road or a settlement cleared each working cell.
    work_cleared: Vec<bool>,
}

impl ChunkBuild {
    /// Start building `coord`.
    ///
    /// # Errors
    ///
    /// [`WorldError::OutOfRange`] for a coordinate so far out that its
    /// working grid's cells would not fit a cell coordinate, and
    /// [`WorldError::OutOfMemory`] if the chunk's arrays do not fit.
    pub fn new(coord: ChunkCoord) -> Result<Self, WorldError> {
        use crate::realm::try_filled;
        if coord.x.unsigned_abs().max(coord.y.unsigned_abs()) > MAX_CHUNK_COORD {
            return Err(WorldError::OutOfRange);
        }
        Ok(Self {
            coord,
            phase: Phase::Relief,
            chunk: Chunk {
                coord,
                elevation: try_filled(CHUNK_AREA, Elevation::SEA_LEVEL)?,
                water: try_filled(CHUNK_AREA, Elevation::SEA_LEVEL)?,
                temperature: try_filled(CHUNK_AREA, Temperature::default())?,
                precipitation: try_filled(CHUNK_AREA, Precipitation::default())?,
                biome: try_filled(CHUNK_AREA, Blend::solid(Biome::PolarDesert))?,
                ground: try_filled(CHUNK_AREA, Blend::solid(Ground::Gravel))?,
                surface: try_filled(CHUNK_AREA, Surface::default())?,
                scatter: Vec::new(),
            },
            work_ground: try_filled(WORK_AREA, 0.0)?,
            work_level: try_filled(WORK_AREA, 0.0)?,
            work_wet: try_filled(WORK_AREA, false)?,
            work_water: try_filled(WORK_AREA, Water::Running)?,
            work_shore: try_filled(WORK_AREA, u16::MAX)?,
            work_cleared: try_filled(WORK_AREA, false)?,
        })
    }

    /// How far the build has got.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// What has been built so far.
    ///
    /// Readable at every phase, so a client draws the relief it already has
    /// rather than waiting or drawing nothing; until the biome phase, every
    /// cell reads as bare ground.
    #[must_use]
    pub const fn partial(&self) -> &Chunk {
        &self.chunk
    }

    /// Advance one phase, returning the phase now reached.
    ///
    /// # Errors
    ///
    /// [`WorldError::OutOfMemory`] if a phase's working memory does not
    /// fit. The build keeps whatever it had, so a caller may retry when
    /// the machine has recovered.
    pub fn step(&mut self, field: &RealmField) -> Result<Phase, WorldError> {
        match self.phase {
            Phase::Relief => self.relief(field),
            Phase::Water => self.water(field)?,
            Phase::Structures => self.structures(field),
            Phase::Climate => self.climate(field),
            Phase::Biome => self.biome(field),
            Phase::Scatter => self.scatter(field)?,
            Phase::Done => return Ok(Phase::Done),
        }
        self.phase = self.phase.next();
        Ok(self.phase)
    }

    /// Run every remaining phase.
    ///
    /// # Errors
    ///
    /// Whatever [`Self::step`] refuses.
    pub fn finish(mut self, field: &RealmField) -> Result<Chunk, WorldError> {
        while self.phase != Phase::Done {
            self.step(field)?;
        }
        Ok(self.chunk)
    }

    /// The working-grid cell at a chunk-relative offset, which the halo
    /// shifts.
    fn work_index(wx: u32, wy: u32) -> usize {
        (wy as usize) * (WORK_CELLS as usize) + (wx as usize)
    }

    /// The absolute cell a working-grid position refers to.
    fn work_cell(&self, wx: u32, wy: u32) -> CellCoord {
        let origin = chunk_origin(self.coord);
        CellCoord::new(
            origin.x + signed(wx) - signed(SHORE_CELLS),
            origin.y + signed(wy) - signed(SHORE_CELLS),
        )
    }

    /// The working-grid position of an absolute cell, or `None` outside it.
    fn work_position(&self, cell: CellCoord) -> Option<(u32, u32)> {
        let origin = chunk_origin(self.coord);
        let halo = signed(SHORE_CELLS);
        let wx = u32::try_from(cell.x - origin.x + halo).ok()?;
        let wy = u32::try_from(cell.y - origin.y + halo).ok()?;
        (wx < WORK_CELLS && wy < WORK_CELLS).then_some((wx, wy))
    }

    /// Fine relief over the working grid.
    fn relief(&mut self, field: &RealmField) {
        let key = field.key();
        for wy in 0..WORK_CELLS {
            for wx in 0..WORK_CELLS {
                let cell = self.work_cell(wx, wy);
                let (gx, gy) = field.grid_position(cell);
                let coarse = field.coarse_at(gx, gy);
                let broad = coarse.elevation_units();

                // Detail fades out below the waterline: a sea floor the
                // player never sees does not need texture, and letting it
                // poke above sea level would move the coastline the realm
                // asked for.
                let submerged_fade = mathf::clamp(broad / 12.0, 0.0, 1.0);
                let height = if submerged_fade > 0.0 {
                    broad
                        + detail(key, cell, coarse.belt(), coarse.precipitation()) * submerged_fade
                } else {
                    broad
                };

                let index = Self::work_index(wx, wy);
                self.work_ground[index] = height;
                self.work_level[index] = height;
                if let Some((cx, cy)) = Self::in_chunk(wx, wy) {
                    let slot = cell_index(cx, cy);
                    self.chunk.elevation[slot] = Elevation::from_units(height);
                    self.chunk.water[slot] = self.chunk.elevation[slot];
                }
            }
        }
    }

    /// Carve channels, fill lakes and sea, and measure the shore.
    fn water(&mut self, field: &RealmField) -> Result<(), WorldError> {
        self.work_level.fill(f64::MIN);
        self.work_wet.fill(false);
        for channel in &self.channels(field)? {
            self.carve(channel);
        }

        for wy in 0..WORK_CELLS {
            for wx in 0..WORK_CELLS {
                let index = Self::work_index(wx, wy);
                let cell = self.work_cell(wx, wy);
                let (gx, gy) = field.grid_position(cell);

                let ground = self.work_ground[index];
                let mut surface = self.work_level[index];
                let in_channel = self.work_wet[index];

                // Only where the coarse field holds a lake or the sea: detail
                // relief is texture, and its hollows are basins no drainage
                // ever filled. The sea stands at sea level; only a lake's
                // surface is the coarse one.
                let coarse = field.coarse_at(gx, gy);
                let coarse_water = coarse.water_units();
                let holds = coarse_water > coarse.elevation_units();
                let sea = holds && coarse.sea_share() >= 0.5;
                let still = if sea { 0.0 } else { coarse_water };
                let standing = holds && still > ground;
                if standing {
                    surface = mathf::fmax(surface, still);
                }

                let wet = in_channel || standing;
                self.work_ground[index] = ground;
                self.work_level[index] = if wet { surface } else { ground };
                self.work_wet[index] = wet;
                self.work_water[index] = match (standing, sea) {
                    (true, true) => Water::Sea,
                    (true, false) => Water::Lake,
                    (false, _) => Water::Running,
                };
                self.work_shore[index] = if wet { 0 } else { u16::MAX };

                if let Some((cx, cy)) = Self::in_chunk(wx, wy) {
                    let slot = cell_index(cx, cy);
                    self.chunk.elevation[slot] = Elevation::from_units(ground);
                    self.chunk.water[slot] =
                        Elevation::from_units(if wet { surface } else { ground });
                    if in_channel {
                        self.chunk.surface[slot] = self.chunk.surface[slot].with(Surface::CHANNEL);
                    }
                    if standing {
                        let flag = if sea { Surface::SEA } else { Surface::LAKE };
                        self.chunk.surface[slot] = self.chunk.surface[slot].with(flag);
                    }
                }
            }
        }

        self.measure_shore();
        Ok(())
    }

    /// Lay `channel`'s bed into every working cell it reaches: the deepest bed
    /// into `work_ground`, the highest surface into `work_level`, and the
    /// reach into `work_wet`, which the cell pass then reads as its water.
    ///
    /// Only the cells of the bed's own bounding box are measured, so the cost
    /// is the channel's area rather than the grid's. A cell sees its channels
    /// in the order they were listed whichever way the loops run, so its
    /// minimum and maximum are the same values bit for bit.
    fn carve(&mut self, channel: &Channel) {
        let Some((xs, ys)) = self.reach(channel) else {
            return;
        };
        for wy in ys {
            for wx in xs.clone() {
                let cell = self.work_cell(wx, wy);
                let point = (f64::from(cell.x), f64::from(cell.y));
                let (distance, along) = distance_to_segment(channel.from, channel.to, point);
                if distance >= channel.half_width {
                    continue;
                }
                let level = lerp(channel.surface.0, channel.surface.1, along);
                // A parabolic bed, so a bank rises out of the water rather
                // than stepping out of it.
                let across = distance / channel.half_width;
                let bed = level - channel.depth * (1.0 - across * across);
                let index = Self::work_index(wx, wy);
                self.work_ground[index] = mathf::fmin(self.work_ground[index], bed);
                self.work_level[index] = mathf::fmax(self.work_level[index], level);
                self.work_wet[index] = true;
            }
        }
    }

    /// The working-grid columns and rows `channel`'s bed can reach, or `None`
    /// where it misses the grid.
    ///
    /// Whole cells either side of the bed's extent: a cell centre a whole cell
    /// or more beyond it lies further than the half-width from the segment, so
    /// no cell the bed reaches is left out.
    fn reach(&self, channel: &Channel) -> Option<(RangeInclusive<u32>, RangeInclusive<u32>)> {
        let origin = chunk_origin(self.coord);
        let halo = signed(SHORE_CELLS);
        let span = |a: f64, b: f64, first: i32| {
            let low = mathf::round_i32(mathf::floor(mathf::fmin(a, b) - channel.half_width));
            let high = mathf::round_i32(mathf::ceil(mathf::fmax(a, b) + channel.half_width));
            let to_work = |cell: i32| i64::from(cell) - i64::from(first) + i64::from(halo);
            let low = u32::try_from(to_work(low).max(0)).ok()?;
            let high = u32::try_from(to_work(high).min(i64::from(WORK_CELLS) - 1)).ok()?;
            (low <= high).then_some(low..=high)
        };
        Some((
            span(channel.from.0, channel.to.0, origin.x)?,
            span(channel.from.1, channel.to.1, origin.y)?,
        ))
    }

    /// The coarse drainage links that can reach this chunk.
    fn channels(&self, field: &RealmField) -> Result<Vec<Channel>, WorldError> {
        let params = field.params();
        let step = f64::from(params.cells_per_coarse());
        // Past the ring by the widest a bed gets, and a link further, so a
        // channel whose centreline is outside the working grid still carves
        // the bank that reaches into it.
        let reach = CHANNEL_MAX_HALF_WIDTH + f64::from(SHORE_CELLS);
        let margin = mathf::round_i32(mathf::ceil(reach / step)) + 1;

        let (gx, gy) = field.chunk_grid_position(self.coord);
        let lo = (
            mathf::round_i32(mathf::floor(gx)) - margin,
            mathf::round_i32(mathf::floor(gy)) - margin,
        );
        let span = signed(CHUNK_CELLS) / signed(params.cells_per_coarse()).max(1) + 2 * margin + 2;

        let mut channels = Vec::new();
        let across = usize::try_from(span).unwrap_or(0) + 1;
        channels
            .try_reserve(across * across)
            .map_err(|_| WorldError::OutOfMemory)?;

        for dy in 0..=span {
            for dx in 0..=span {
                let (sx, sy) = (lo.0 + dx, lo.1 + dy);
                let sample = field.sample(sx, sy);
                if sample.flow == FlowDir::Sink || sample.elevation.is_submerged() {
                    continue;
                }
                let Some((ox, oy)) = sample.flow.offset() else {
                    continue;
                };
                let downstream = field.sample(sx + ox, sy + oy);
                let discharge = f64::from(sample.discharge) / CHANNEL_FULL_DISCHARGE;
                let fullness =
                    mathf::clamp(mathf::sqrt(mathf::clamp(discharge, 0.0, 1.0)), 0.0, 1.0);
                let half_width = 0.9 + fullness * (CHANNEL_MAX_HALF_WIDTH - 0.9);
                if sample.discharge < 8 {
                    continue;
                }
                channels.push(Channel {
                    from: grid_to_cells(field, sx, sy),
                    to: grid_to_cells(field, sx + ox, sy + oy),
                    surface: (sample.water.units(), downstream.water.units()),
                    half_width,
                    depth: 1.2 + fullness * (CHANNEL_MAX_DEPTH - 1.2),
                });
            }
        }
        Ok(channels)
    }

    /// Two chamfer passes over the working grid, carrying which water is
    /// nearest along with how near it is. Exact inside the halo's reach,
    /// because the grid extends [`SHORE_CELLS`] beyond the chunk; a tie
    /// counts standing water over a river and the sea over a lake, so the
    /// answer is the same whichever way it was reached.
    fn measure_shore(&mut self) {
        let width = WORK_CELLS as usize;
        let relax = |shore: &mut [u16], water: &mut [Water], here: usize, from: usize| {
            let offered = shore[from].saturating_add(1);
            if offered < shore[here] {
                shore[here] = offered;
                water[here] = water[from];
            } else if offered == shore[here] && offered != u16::MAX {
                water[here] = water[here].max(water[from]);
            }
        };
        for wy in 0..WORK_CELLS {
            for wx in 0..WORK_CELLS {
                let index = Self::work_index(wx, wy);
                if wx > 0 {
                    relax(&mut self.work_shore, &mut self.work_water, index, index - 1);
                }
                if wy > 0 {
                    relax(
                        &mut self.work_shore,
                        &mut self.work_water,
                        index,
                        index - width,
                    );
                }
            }
        }
        for wy in (0..WORK_CELLS).rev() {
            for wx in (0..WORK_CELLS).rev() {
                let index = Self::work_index(wx, wy);
                if wx + 1 < WORK_CELLS {
                    relax(&mut self.work_shore, &mut self.work_water, index, index + 1);
                }
                if wy + 1 < WORK_CELLS {
                    relax(
                        &mut self.work_shore,
                        &mut self.work_water,
                        index,
                        index + width,
                    );
                }
            }
        }
    }

    /// The working grid's extent, in absolute cells: `(min, max)` corners,
    /// inclusive.
    fn work_bounds(&self) -> ((f64, f64), (f64, f64)) {
        let low = self.work_cell(0, 0);
        let high = self.work_cell(WORK_CELLS - 1, WORK_CELLS - 1);
        (
            (f64::from(low.x), f64::from(low.y)),
            (f64::from(high.x), f64::from(high.y)),
        )
    }

    /// Level settlements and lay roads.
    fn structures(&mut self, field: &RealmField) {
        let bounds = self.work_bounds();

        for site in field.sites() {
            let radius = f64::from(site.radius_cells);
            let centre = (f64::from(site.at.x), f64::from(site.at.y));
            if !segment_reaches(centre, centre, radius, bounds) {
                continue;
            }
            let (gx, gy) = field.grid_position(site.at);
            let level = field.coarse_at(gx, gy).elevation_units();
            self.level_around(|point| {
                let distance = mathf::hypot(point.0 - centre.0, point.1 - centre.1);
                (distance < radius).then(|| {
                    (
                        level,
                        1.0 - mathf::smoothstep(distance / radius),
                        Surface::SETTLEMENT,
                    )
                })
            });
        }

        for road in field.roads() {
            for pair in road.path.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                let from = (f64::from(a.x), f64::from(a.y));
                let to = (f64::from(b.x), f64::from(b.y));
                if !segment_reaches(from, to, ROAD_HALF_WIDTH, bounds) {
                    continue;
                }
                let ga = field.grid_position(a);
                let gb = field.grid_position(b);
                let height_a = field.coarse_at(ga.0, ga.1).elevation_units();
                let height_b = field.coarse_at(gb.0, gb.1).elevation_units();
                self.level_around(|point| {
                    let (distance, along) = distance_to_segment(from, to, point);
                    (distance < ROAD_HALF_WIDTH).then(|| {
                        (
                            lerp(height_a, height_b, along),
                            1.0 - mathf::smoothstep(distance / ROAD_HALF_WIDTH),
                            Surface::ROAD,
                        )
                    })
                });
            }
        }
    }

    /// Pull the ground toward a target height wherever `shape` claims a
    /// cell, and record the flag it claims it with.
    fn level_around(&mut self, shape: impl Fn((f64, f64)) -> Option<(f64, f64, u8)>) {
        for wy in 0..WORK_CELLS {
            for wx in 0..WORK_CELLS {
                let index = Self::work_index(wx, wy);
                if self.work_wet[index] {
                    continue;
                }
                let cell = self.work_cell(wx, wy);
                let point = (f64::from(cell.x), f64::from(cell.y));
                let Some((target, strength, flag)) = shape(point) else {
                    continue;
                };
                let levelled = lerp(
                    self.work_ground[index],
                    target,
                    mathf::clamp(strength, 0.0, 1.0),
                );
                self.work_ground[index] = levelled;
                self.work_level[index] = levelled;
                let cleared = strength > CLEARED_STRENGTH;
                // Halo cells are recorded too: a scatter candidate there is
                // a neighbour's, and it must read as cleared exactly as the
                // neighbour's own build reads it.
                self.work_cleared[index] |= cleared;
                if let Some((cx, cy)) = Self::in_chunk(wx, wy) {
                    let slot = cell_index(cx, cy);
                    self.chunk.elevation[slot] = Elevation::from_units(levelled);
                    self.chunk.water[slot] = self.chunk.elevation[slot];
                    if cleared {
                        self.chunk.surface[slot] = self.chunk.surface[slot].with(flag);
                    }
                }
            }
        }
    }

    /// Interpolate the coarse climate and correct it for the fine relief.
    fn climate(&mut self, field: &RealmField) {
        for cy in 0..CHUNK_CELLS {
            for cx in 0..CHUNK_CELLS {
                let (wx, wy) = (cx + SHORE_CELLS, cy + SHORE_CELLS);
                let (gx, gy) = field.grid_position(self.work_cell(wx, wy));
                let (temperature, precipitation) =
                    self.climate_at(&field.coarse_at(gx, gy), Self::work_index(wx, wy));
                let slot = cell_index(cx, cy);
                self.chunk.temperature[slot] = temperature;
                self.chunk.precipitation[slot] = precipitation;
            }
        }
    }

    /// The temperature and precipitation of the working cell at `index`,
    /// whose coarse field is `coarse`, as stored.
    fn climate_at(&self, coarse: &Coarse, index: usize) -> (Temperature, Precipitation) {
        let lift = self.work_ground[index] - coarse.elevation_units();
        (
            Temperature::from_celsius(coarse.celsius() - lift * LAPSE_RATE),
            Precipitation::from_millimetres(coarse.precipitation()),
        )
    }

    /// Classify every cell and weigh its grounds.
    fn biome(&mut self, field: &RealmField) {
        for cy in 0..CHUNK_CELLS {
            for cx in 0..CHUNK_CELLS {
                let slot = cell_index(cx, cy);
                let reading = self.reading(field, cx + SHORE_CELLS, cy + SHORE_CELLS);
                let biomes = biomes_of(reading.as_ref());
                self.chunk.ground[slot] = reading.map_or(Blend::solid(Ground::Water), |reading| {
                    ground::cover(&biomes, &ground_site(field, &reading))
                });
                self.chunk.biome[slot] = biomes;
            }
        }
    }

    /// What the classification reads about a working cell, or `None` where
    /// water covers it — in the chunk or its halo alike, so a halo cell reads
    /// exactly as the chunk that owns it does.
    fn reading(&self, field: &RealmField, wx: u32, wy: u32) -> Option<Reading> {
        let index = Self::work_index(wx, wy);
        if self.work_wet[index] {
            return None;
        }
        let cell = self.work_cell(wx, wy);
        let (gx, gy) = field.grid_position(cell);
        let coarse = field.coarse_at(gx, gy);
        let (temperature, precipitation) = self.climate_at(&coarse, index);
        let catchment = hydrology::specific_catchment(field.params(), coarse.discharge());
        Some(Reading {
            cell,
            catchment,
            conditions: Conditions {
                celsius: temperature.celsius(),
                range: coarse.range_celsius(),
                continentality: coarse.continentality(),
                precipitation: precipitation.millimetres(),
                rain_season: coarse.rain_season(),
                wetness: wetness(catchment, self.gradient(wx, wy)),
                elevation_units: self.work_ground[index],
                slope: self.slope(wx, wy),
                rift: coarse.rift(),
                lithology: field.lithology_at(gx, gy),
                shore: (self.work_shore[index], self.work_water[index]),
            },
        })
    }

    /// The steepest rise to a four-neighbour's surface, in world units.
    fn slope(&self, wx: u32, wy: u32) -> f64 {
        let here = self.work_level[Self::work_index(wx, wy)];
        let mut steepest = 0.0;
        for (dx, dy) in [(1_i32, 0_i32), (0, 1), (-1, 0), (0, -1)] {
            let (Some(nx), Some(ny)) = (wx.checked_add_signed(dx), wy.checked_add_signed(dy))
            else {
                continue;
            };
            if nx >= WORK_CELLS || ny >= WORK_CELLS {
                continue;
            }
            let there = self.work_level[Self::work_index(nx, ny)];
            steepest = mathf::fmax(steepest, mathf::fabs(here - there));
        }
        steepest
    }

    /// The hillslope gradient at a working cell: the central difference of
    /// its surface — the water's where water stands, the ground's elsewhere
    /// — across [`WETNESS_SPAN`] cells either side, which is the slope water
    /// runs down rather than the roughness it runs over.
    fn gradient(&self, wx: u32, wy: u32) -> f64 {
        let last = WORK_CELLS - 1;
        let at = |x: u32, y: u32| self.work_level[Self::work_index(x.min(last), y.min(last))];
        let (west, east) = (wx.saturating_sub(WETNESS_SPAN), wx + WETNESS_SPAN);
        let (north, south) = (wy.saturating_sub(WETNESS_SPAN), wy + WETNESS_SPAN);
        let run = f64::from(2 * WETNESS_SPAN);
        mathf::hypot(
            (at(east, wy) - at(west, wy)) / run,
            (at(wx, south) - at(wx, north)) / run,
        )
    }

    /// Place the scatter.
    fn scatter(&mut self, field: &RealmField) -> Result<(), WorldError> {
        let placed =
            scatter::for_chunk(field.key(), self.coord, &|cell| self.footing(field, cell))?;
        self.chunk.scatter = placed;
        Ok(())
    }

    /// What a scatter candidate at `cell` stands on, read the same way for a
    /// cell of this chunk and for one of the halo.
    fn footing(&self, field: &RealmField, cell: CellCoord) -> Footing {
        let Some((wx, wy)) = self.work_position(cell) else {
            // Outside the working grid a candidate cannot reach a cell of
            // this chunk, so refusing it costs nothing and keeps the query
            // total.
            return Footing {
                biomes: Blend::solid(Biome::OpenWater),
                slope: f64::MAX,
                submerged: true,
                cleared: true,
            };
        };
        let index = Self::work_index(wx, wy);
        Footing {
            biomes: Self::in_chunk(wx, wy).map_or_else(
                || biomes_of(self.reading(field, wx, wy).as_ref()),
                |(cx, cy)| self.chunk.biome[cell_index(cx, cy)],
            ),
            slope: self.slope(wx, wy),
            submerged: self.work_wet[index],
            cleared: self.work_cleared[index],
        }
    }

    /// The in-chunk cell a working-grid position refers to, or `None` for
    /// a halo cell.
    fn in_chunk(wx: u32, wy: u32) -> Option<(u32, u32)> {
        let cx = wx.checked_sub(SHORE_CELLS)?;
        let cy = wy.checked_sub(SHORE_CELLS)?;
        (cx < CHUNK_CELLS && cy < CHUNK_CELLS).then_some((cx, cy))
    }
}

/// What the classification and the ground palette read about a dry working
/// cell.
struct Reading {
    cell: CellCoord,
    conditions: Conditions,
    /// The specific catchment, in cells.
    catchment: f64,
}

/// A working cell's biomes: open water where water covers it, else what its
/// reading classifies as.
fn biomes_of(reading: Option<&Reading>) -> Blend<Biome> {
    reading.map_or(Blend::solid(Biome::OpenWater), |reading| {
        biome::classify(&reading.conditions)
    })
}

/// What a dry working cell's ground palette reads.
fn ground_site(field: &RealmField, reading: &Reading) -> GroundSite {
    let conditions = &reading.conditions;
    let moisture = conditions.moisture();
    let (shore, water) = conditions.shore;
    // A floodplain is fine sediment beside a large river, not a sea strand
    // and not a gorge.
    let alluvial = if water == Water::Sea {
        0.0
    } else {
        rise(reading.catchment, FLOODPLAIN_CATCHMENT, FLOODPLAIN_SPREAD)
            * (1.0 - rise(f64::from(shore), 5.0, 4.0))
            * (1.0 - rise(conditions.slope, 1.2, 0.8))
    };
    let rock = conditions.lithology.rock;
    GroundSite {
        celsius: conditions.celsius,
        warm: conditions.warm(),
        moisture,
        wetness: conditions.wetness,
        slope: conditions.slope,
        rock,
        soils: soils(SoilSite {
            rock,
            celsius: conditions.celsius,
            moisture,
            alluvial,
        }),
        patch: patch(field, reading.cell),
    }
}

/// How poorly drained a cell is: its specific catchment, in cells, against
/// the gradient that sheds it, as `a / (a + k·tan β)` — the topographic
/// wetness index's ratio mapped into `0.0..1.0`, which orders cells the way
/// its logarithm does.
fn wetness(catchment: f64, gradient: f64) -> f64 {
    let catchment = catchment.max(0.0);
    let shed = WETNESS_SCALE * gradient.max(1.0e-3);
    catchment / (catchment + shed)
}

/// The patch field at a cell, `0.0..=1.0`.
fn patch(field: &RealmField, cell: CellCoord) -> f64 {
    let value = noise::fbm(
        field.key(),
        Stage::Biome,
        f64::from(cell.x) / PATCH_CELLS,
        f64::from(cell.y) / PATCH_CELLS,
    );
    mathf::clamp(0.5 + 0.8 * value, 0.0, 1.0)
}

/// The relief below the coarse step at `cell`, in world units, where the
/// mountain-belt strength is `belt` and the annual rain `rain` millimetres.
///
/// Ridged inside a belt, ordinary fractal outside it, mixed across the
/// transition — which is what makes a range read as ridges and a plain as
/// ground. Dunes only where it is dry: a billow on a wet plain would read as
/// noise, not as sand.
fn detail(key: SeedKey, cell: CellCoord, belt: f64, rain: f64) -> f64 {
    let (nx, ny) = (
        f64::from(cell.x) / DETAIL_UNITS,
        f64::from(cell.y) / DETAIL_UNITS,
    );
    let ridge = mathf::smoothstep(belt);
    let fractal = noise::fbm(key, Stage::Detail, nx, ny);
    let rough = if ridge > 0.0 {
        lerp(
            fractal,
            noise::ridged(key, Stage::Ridge, nx, ny) * 2.0 - 1.0,
            ridge,
        )
    } else {
        fractal
    };
    let arid = 1.0 - rise(rain, 250.0, 300.0);
    let dune = if arid > 0.0 {
        noise::billow(
            key,
            Stage::Dune,
            f64::from(cell.x) / DUNE_UNITS,
            f64::from(cell.y) / DUNE_UNITS,
        ) * DUNE_AMPLITUDE
            * arid
    } else {
        0.0
    };
    rough * lerp(DETAIL_AMPLITUDE, BELT_AMPLITUDE, ridge) + dune
}

/// Whether anything within `reach` of the segment `a`–`b` falls inside the
/// box `bounds`: the segment's own box, grown by `reach`, overlaps it.
///
/// Tested against the segment's whole extent, never its endpoints alone: a
/// realm's coarse step can be far wider than a chunk, so a road can cross a
/// chunk with both its ends a long way outside.
fn segment_reaches(
    a: (f64, f64),
    b: (f64, f64),
    reach: f64,
    bounds: ((f64, f64), (f64, f64)),
) -> bool {
    let ((low_x, low_y), (high_x, high_y)) = bounds;
    mathf::fmin(a.0, b.0) - reach <= high_x
        && mathf::fmax(a.0, b.0) + reach >= low_x
        && mathf::fmin(a.1, b.1) - reach <= high_y
        && mathf::fmax(a.1, b.1) + reach >= low_y
}

/// The cell position of a coarse grid sample.
fn grid_to_cells(field: &RealmField, sx: i32, sy: i32) -> (f64, f64) {
    let cell = field.params().sample_cell(sx, sy);
    (f64::from(cell.x), f64::from(cell.y))
}

/// Distance from `point` to the segment `a`–`b`, and the parameter of the
/// nearest point on it.
fn distance_to_segment(a: (f64, f64), b: (f64, f64), point: (f64, f64)) -> (f64, f64) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length2 = dx * dx + dy * dy;
    if length2 <= f64::EPSILON {
        return (mathf::hypot(point.0 - a.0, point.1 - a.1), 0.0);
    }
    let raw = ((point.0 - a.0) * dx + (point.1 - a.1) * dy) / length2;
    let along = mathf::clamp(raw, 0.0, 1.0);
    let nearest = (a.0 + dx * along, a.1 + dy * along);
    (
        mathf::hypot(point.0 - nearest.0, point.1 - nearest.1),
        along,
    )
}

#[cfg(test)]
mod tests;
