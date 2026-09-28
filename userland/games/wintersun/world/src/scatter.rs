//! Vegetation, rock and resource scatter.
//!
//! # Poisson disks without a global pass
//!
//! Dart-throwing and Bridson's algorithm both need to know what has
//! already been placed, which makes the result depend on the order chunks
//! were generated in — the one thing this crate may not do. So the
//! placement is a **jittered grid with a priority rule**: every scatter
//! cell offers exactly one candidate, at a hashed position with a hashed
//! priority, and a candidate survives only if nothing within its
//! exclusion radius outranks it.
//!
//! That gives the two properties dart-throwing is used for — a minimum
//! spacing, and no visible grid — while depending on nothing outside the
//! eight neighbouring scatter cells. Two chunks generated a week apart
//! agree on every item along the seam, because neither ever looked at the
//! other.
//!
//! The exclusion radius is bounded by the scatter step for exactly that
//! reason: a radius that reached further than one ring of neighbours
//! would make the outcome depend on candidates the query never examined.

use alloc::vec::Vec;

use tairix_util::mathf;
use tairix_wintersun_net::value::{ChunkCoord, WorldPoint};

use crate::biome::Biome;
use crate::blend::Blend;
use crate::error::WorldError;
use crate::geom::{chunk_origin, CellCoord, CELL_SUB_UNITS, CHUNK_CELLS};
use crate::seed::{SeedKey, Stage};

/// Cells along one edge of a scatter cell.
///
/// Also the ceiling on any exclusion radius, which is what keeps a
/// candidate's fate a function of its own scatter cell and the eight
/// around it.
pub const SCATTER_STEP: u32 = 4;

/// What a scattered item is.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(u8)]
pub enum ScatterKind {
    /// A tree.
    Tree = 0,
    /// A bush or low scrub.
    Shrub = 1,
    /// A boulder or outcrop.
    Boulder = 2,
    /// A gatherable resource node.
    Resource = 3,
}

impl ScatterKind {
    /// How far this kind keeps from anything that outranks it, in cells.
    ///
    /// Bounded by [`SCATTER_STEP`]: a wider exclusion would reach past the
    /// ring of neighbours the rule examines.
    #[must_use]
    pub const fn exclusion_cells(self) -> u32 {
        match self {
            Self::Tree | Self::Resource => 4,
            Self::Shrub => 2,
            Self::Boulder => 3,
        }
    }

    /// The steepest ground this kind stands on, as a rise over one cell.
    #[must_use]
    pub const fn slope_limit(self) -> f64 {
        match self {
            Self::Tree => 1.1,
            Self::Shrub => 1.6,
            Self::Boulder => 4.0,
            Self::Resource => 2.2,
        }
    }
}

/// One scattered item.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Scattered {
    /// Where it stands, to sub-unit precision.
    pub at: WorldPoint,
    /// What it is.
    pub kind: ScatterKind,
    /// The biome it grew in, which selects the artwork.
    pub host: Biome,
    /// Which of the host biome's variants, for visual variety.
    pub variant: u8,
    /// Relative size, `0` smallest through [`u8::MAX`] largest.
    pub scale: u8,
}

/// What the terrain offers one scatter candidate.
#[derive(Copy, Clone, Debug)]
pub struct Footing {
    /// The cell's biome blend.
    pub biomes: Blend<Biome>,
    /// Steepest local gradient, as a rise over one cell.
    pub slope: f64,
    /// Whether standing water covers the cell.
    pub submerged: bool,
    /// Whether a road or a settlement has cleared the cell.
    pub cleared: bool,
}

/// One scatter cell's offer, before the exclusion rule is applied.
#[derive(Copy, Clone, Debug)]
struct Candidate {
    at: CellCoord,
    sub: (i32, i32),
    kind: ScatterKind,
    host: Biome,
    variant: u8,
    scale: u8,
    priority: u64,
}

/// Every item standing in `chunk`.
///
/// `footing` answers what the terrain is at a world cell; the caller
/// supplies it because the chunk that owns the terrain is the one asking.
/// Cells in the neighbouring chunks are queried too — a candidate just
/// outside the chunk can exclude one just inside it — which is why the
/// query is a function of an absolute cell rather than an index into this
/// chunk.
///
/// # Errors
///
/// [`WorldError::OutOfMemory`] if the item list does not fit.
pub fn for_chunk(
    key: SeedKey,
    chunk: ChunkCoord,
    footing: &dyn Fn(CellCoord) -> Footing,
) -> Result<Vec<Scattered>, WorldError> {
    let origin = chunk_origin(chunk);
    #[allow(
        clippy::cast_possible_wrap,
        reason = "the scatter step is a small constant"
    )]
    let step = SCATTER_STEP as i32;
    let cells = CHUNK_CELLS.div_ceil(SCATTER_STEP);
    #[allow(
        clippy::cast_possible_wrap,
        reason = "a chunk is CHUNK_CELLS cells across"
    )]
    let span = cells as i32;

    // Scatter-cell indices are absolute, so a scatter cell has the same
    // identity whichever chunk asks about it — which is what makes the
    // seam between two chunks agree.
    let base = (origin.x.div_euclid(step), origin.y.div_euclid(step));

    // The chunk's scatter cells plus one ring, because a candidate just
    // outside can exclude one just inside. Every offer is computed once
    // here rather than re-derived by each of its neighbours' exclusion
    // tests, which is the difference between one pass over the halo and
    // nine.
    let halo = span + 2;
    let mut offers = Vec::new();
    let across = usize::try_from(halo).unwrap_or(0);
    offers
        .try_reserve_exact(across * across)
        .map_err(|_| WorldError::OutOfMemory)?;
    for row in 0..halo {
        for column in 0..halo {
            let cell = (base.0 + column - 1, base.1 + row - 1);
            offers.push(offer(key, cell, step, footing));
        }
    }

    let mut placed = Vec::new();
    placed
        .try_reserve((cells as usize) * (cells as usize))
        .map_err(|_| WorldError::OutOfMemory)?;

    for cy in 0..span {
        for cx in 0..span {
            let cell = (base.0 + cx, base.1 + cy);
            let Some(candidate) = offers[index_of(cx, cy, halo)] else {
                continue;
            };
            if outranked(&offers, cx, cy, halo, cell, candidate) {
                continue;
            }
            let Some(at) = candidate.at.centre() else {
                continue;
            };
            placed.push(Scattered {
                at: WorldPoint {
                    x: at.x + candidate.sub.0,
                    y: at.y + candidate.sub.1,
                },
                kind: candidate.kind,
                host: candidate.host,
                variant: candidate.variant,
                scale: candidate.scale,
            });
        }
    }
    Ok(placed)
}

/// The offer slot for a chunk-relative scatter cell, which the halo shifts
/// by one.
fn index_of(cx: i32, cy: i32, halo: i32) -> usize {
    // Callers pass a chunk-relative cell, so both components are in
    // `-1..=span`; a value outside that would index the halo's first slot
    // rather than wrap into another row.
    let column = usize::try_from(cx + 1).unwrap_or(0);
    let row = usize::try_from(cy + 1).unwrap_or(0);
    row * usize::try_from(halo).unwrap_or(1) + column
}

/// The candidate a scatter cell offers, if the ground admits one.
fn offer(
    key: SeedKey,
    cell: (i32, i32),
    step: i32,
    footing: &dyn Fn(CellCoord) -> Footing,
) -> Option<Candidate> {
    let mut stream = key.stream(Stage::Scatter, cell.0, cell.1);

    // Jitter across the whole scatter cell, so the grid the candidates
    // came from is not visible in the result.
    let span = f64::from(step);
    let jx = stream.unit() * span;
    let jy = stream.unit() * span;
    let ox = mathf::round_i32(mathf::floor(jx));
    let oy = mathf::round_i32(mathf::floor(jy));
    let at = CellCoord::new(cell.0 * step + ox, cell.1 * step + oy);

    let terrain = footing(at);
    if terrain.submerged || terrain.cleared {
        return None;
    }

    let host = terrain.biomes.dominant();
    let kind = kind_for(host, &mut stream)?;
    if terrain.slope > kind.slope_limit() {
        return None;
    }
    // How strongly the cell is the biome this kind grows in: a forest thins
    // toward its edge instead of stopping at a line.
    let strength = f64::from(terrain.biomes.weights()[0]) / f64::from(u8::MAX);
    if stream.unit() > strength * density_of(host, kind) {
        return None;
    }

    let sub_x =
        mathf::round_i32((jx - mathf::floor(jx)) * f64::from(CELL_SUB_UNITS)) - CELL_SUB_UNITS / 2;
    let sub_y =
        mathf::round_i32((jy - mathf::floor(jy)) * f64::from(CELL_SUB_UNITS)) - CELL_SUB_UNITS / 2;

    Some(Candidate {
        at,
        sub: (sub_x, sub_y),
        kind,
        host,
        variant: variant_of(&mut stream),
        scale: variant_of(&mut stream),
        // Drawn last so an earlier rejection cannot shift it, and taken
        // from the lattice rather than the stream so two candidates never
        // tie.
        priority: key.lattice(Stage::Scatter, cell.0, cell.1),
    })
}

/// Whether anything within the candidate's exclusion radius outranks it.
fn outranked(
    offers: &[Option<Candidate>],
    cx: i32,
    cy: i32,
    halo: i32,
    cell: (i32, i32),
    candidate: Candidate,
) -> bool {
    let reach = i64::from(candidate.kind.exclusion_cells());

    for dy in -1..=1 {
        for dx in -1..=1 {
            if dx == 0 && dy == 0 {
                continue;
            }
            let Some(other) = offers[index_of(cx + dx, cy + dy, halo)] else {
                continue;
            };
            // A strict comparison on the priority, then on the scatter
            // cell: a total order, so of any two candidates exactly one
            // yields and neither is dropped for the other's sake.
            let neighbour = (cell.0 + dx, cell.1 + dy);
            if (other.priority, neighbour) <= (candidate.priority, cell) {
                continue;
            }
            let ox = i64::from(other.at.x - candidate.at.x).abs();
            let oy = i64::from(other.at.y - candidate.at.y).abs();
            if ox.max(oy) < reach {
                return true;
            }
        }
    }
    false
}

/// How a biome is covered: the one grouping both what grows in it and how
/// densely are read from, so the two cannot disagree about a biome.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Cover {
    ClosedForest,
    OpenForest,
    Savanna,
    Heath,
    Scrub,
    Wetland,
    Barren,
    Beach,
    Bare,
}

fn cover_of(host: Biome) -> Cover {
    match host {
        Biome::BorealForest
        | Biome::TemperateConiferForest
        | Biome::TemperateBroadleafForest
        | Biome::TemperateRainforest
        | Biome::TropicalRainforest
        | Biome::SwampForest
        | Biome::Mangrove => Cover::ClosedForest,
        Biome::TropicalDryForest | Biome::MediterraneanWoodland => Cover::OpenForest,
        Biome::Savanna => Cover::Savanna,
        Biome::HeathMoor => Cover::Heath,
        Biome::Tundra | Biome::AlpineTundra | Biome::XericShrubland | Biome::TemperateGrassland => {
            Cover::Scrub
        }
        Biome::Bog | Biome::Fen | Biome::Marsh => Cover::Wetland,
        Biome::PolarDesert
        | Biome::ColdDesert
        | Biome::HotDesert
        | Biome::RockyCoast
        | Biome::VolcanicBarren
        | Biome::Badlands
        | Biome::RiftWaste => Cover::Barren,
        Biome::BeachDune => Cover::Beach,
        Biome::OpenWater | Biome::IceSheet => Cover::Bare,
    }
}

/// What grows in a biome, if anything.
fn kind_for(host: Biome, stream: &mut crate::seed::Stream) -> Option<ScatterKind> {
    let draw = stream.unit();
    let woodland = |trees: f64, shrubs: f64| {
        Some(if draw < trees {
            ScatterKind::Tree
        } else if draw < trees + shrubs {
            ScatterKind::Shrub
        } else {
            ScatterKind::Resource
        })
    };
    let scrub = |shrubs: f64, boulders: f64| {
        Some(if draw < shrubs {
            ScatterKind::Shrub
        } else if draw < shrubs + boulders {
            ScatterKind::Boulder
        } else {
            ScatterKind::Resource
        })
    };
    match cover_of(host) {
        Cover::ClosedForest => woodland(0.72, 0.22),
        Cover::OpenForest => woodland(0.5, 0.4),
        Cover::Savanna => woodland(0.22, 0.6),
        Cover::Heath | Cover::Scrub => scrub(0.58, 0.28),
        Cover::Wetland => (draw < 0.7).then_some(ScatterKind::Shrub),
        Cover::Barren => Some(if draw < 0.78 {
            ScatterKind::Boulder
        } else {
            ScatterKind::Resource
        }),
        Cover::Beach => (draw < 0.3).then_some(ScatterKind::Boulder),
        Cover::Bare => None,
    }
}

/// How densely a kind stands in its biome.
fn density_of(host: Biome, kind: ScatterKind) -> f64 {
    let base = match kind {
        ScatterKind::Tree => 0.85,
        ScatterKind::Shrub => 0.6,
        ScatterKind::Boulder => 0.35,
        ScatterKind::Resource => 0.12,
    };
    match cover_of(host) {
        Cover::ClosedForest => base,
        Cover::OpenForest | Cover::Heath | Cover::Wetland => base * 0.7,
        Cover::Savanna | Cover::Scrub | Cover::Barren | Cover::Beach | Cover::Bare => base * 0.5,
    }
}

/// A byte of variation drawn from a stream.
fn variant_of(stream: &mut crate::seed::Stream) -> u8 {
    crate::geom::quantise_u8(stream.unit() * f64::from(u8::MAX))
}

#[cfg(test)]
mod tests;
