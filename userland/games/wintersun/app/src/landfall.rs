//! Where a session's first body stands.
//!
//! The realm is centred on the origin, but the ground there may be sea, a
//! lake, a river bed, or a pocket walled in by banks too steep to climb. A
//! body placed in any of them wades from its first frame or never gets
//! anywhere. So a session starts on dry ground inside a stretch of walkable
//! land with room to move, at the point of it nearest the realm's centre.
//!
//! "Walkable" is the rules' own: two neighbouring cells join a stretch only
//! when a body can step each way between them, so a start is never at the
//! bottom of a drop it cannot climb back out of.

use alloc::collections::BinaryHeap;
use alloc::vec::Vec;

use tairix_wintersun_net::value::{ChunkCoord, WorldPoint};
use tairix_wintersun_rules::motion::{footprint, footprint_clear};
use tairix_wintersun_rules::terrain::{ChunkTerrain, Terrain};
use tairix_wintersun_world::chunk::{Chunk, ChunkBuild};
use tairix_wintersun_world::geom::{chunk_origin, signed, CellCoord, CHUNK_AREA, CHUNK_CELLS};
use tairix_wintersun_world::realm::{try_filled, RealmField};

use crate::error::ClientError;

/// Chunks solved before a realm is reported as having no ground near its
/// centre. Each costs a chunk solve, so this bounds how long a session can
/// take to find its feet.
///
/// Enough to walk in from a coast: a realm centred on open sea offers its
/// nearest shore first, and a shore is often a strip of beach under cliffs.
pub const TRIES: usize = 64;

/// The fewest cells a start's walkable stretch must hold inside its chunk.
///
/// A quarter of the chunk. The traps this rules out — a river bed between
/// its banks, an islet, a hollow among cliffs — are bands and pockets a few
/// cells across, far under it, while open ground holds most of its chunk.
pub const ROOM_CELLS: usize = CHUNK_AREA / 4;

/// Where a session starts, and the chunk solved to find it.
#[derive(Debug)]
pub struct Landfall {
    /// Where the body stands: the centre of a dry cell.
    pub at: WorldPoint,
    /// The chunk it stands in, so the caller holds it rather than solving it
    /// again.
    pub chunk: Chunk,
    /// How many cells of that chunk the body can walk to and back from.
    pub room: usize,
}

/// The dry ground nearest the realm's centre, inside a walkable stretch of
/// at least [`ROOM_CELLS`], that a body of `radius` stands on.
///
/// Chunks are tried nearest the centre first. Where none of the [`TRIES`]
/// holds that much room, the start is the one with the most room found, so a
/// realm of broken ground still starts rather than refusing to.
///
/// # Errors
///
/// [`ClientError::NoGround`] when no chunk tried holds a dry place a body
/// stands on, [`ClientError::World`] when a chunk cannot be solved, and
/// [`ClientError::OutOfMemory`] when the search's working set does not fit.
pub fn landfall(field: &RealmField, radius: u16) -> Result<Landfall, ClientError> {
    let mut roomiest: Option<Landfall> = None;
    for coord in nearest_land(field)? {
        let chunk = ChunkBuild::new(coord)
            .and_then(|build| build.finish(field))
            .map_err(|_| ClientError::World)?;
        let Some((at, room)) = start_in(&chunk, radius)? else {
            continue;
        };
        if room >= ROOM_CELLS {
            return Ok(Landfall { at, chunk, room });
        }
        if roomiest.as_ref().is_none_or(|held| room > held.room) {
            roomiest = Some(Landfall { at, chunk, room });
        }
    }
    roomiest.ok_or(ClientError::NoGround)
}

/// The chunks holding the dry coarse samples nearest the realm's centre,
/// nearest first and each once, at most [`TRIES`] of them.
fn nearest_land(field: &RealmField) -> Result<Vec<ChunkCoord>, ClientError> {
    let params = field.params();
    let side = signed(field.side());
    let first = params.sample_cell(0, 0);
    let step = signed(params.cells_per_coarse());
    let centre = ((-first.x).div_euclid(step), (-first.y).div_euclid(step));

    // Where the sample step is finer than a chunk, several samples share one,
    // so enough are kept to name TRIES different chunks.
    let per_chunk = usize::try_from((signed(CHUNK_CELLS) / step).max(1)).unwrap_or(1);
    let samples = field.samples().len();
    let keep = TRIES
        .saturating_mul(per_chunk.saturating_mul(per_chunk))
        .min(samples);

    // A max-heap of the nearest seen so far, so the scan holds `keep` entries
    // rather than every dry sample in the realm.
    let mut nearest = BinaryHeap::new();
    nearest
        .try_reserve_exact(keep + 1)
        .map_err(|_| ClientError::OutOfMemory)?;
    for (index, sample) in field.samples().iter().enumerate() {
        if sample.is_water() {
            continue;
        }
        let index = i32::try_from(index).map_err(|_| ClientError::World)?;
        let (sx, sy) = (index % side, index / side);
        let (dx, dy) = (i64::from(sx - centre.0), i64::from(sy - centre.1));
        nearest.push((dx * dx + dy * dy, sy, sx));
        if nearest.len() > keep {
            nearest.pop();
        }
    }

    let mut chunks: Vec<ChunkCoord> = Vec::new();
    chunks
        .try_reserve_exact(TRIES)
        .map_err(|_| ClientError::OutOfMemory)?;
    for (_, sy, sx) in nearest.into_sorted_vec() {
        let chunk = params.sample_cell(sx, sy).chunk();
        if !chunks.contains(&chunk) {
            chunks.push(chunk);
            if chunks.len() == TRIES {
                break;
            }
        }
    }
    Ok(chunks)
}

/// No stretch claims this cell: a body cannot stand on it.
const UNREACHED: u16 = u16::MAX;

/// Cells along a chunk's edge, as the index arithmetic counts them.
const EDGE: usize = CHUNK_CELLS as usize;

/// Where in `chunk` a body of `radius` starts, and how many cells it can walk
/// among from there, or `None` when no dry cell of it holds a body.
fn start_in(chunk: &Chunk, radius: u16) -> Result<Option<(WorldPoint, usize)>, ClientError> {
    let window = [chunk];
    let terrain = ChunkTerrain::new(&window).map_err(|_| ClientError::World)?;
    start_on(&terrain, chunk_origin(chunk.coord()), radius)
}

/// [`start_in`] over the chunk-sized patch of `terrain` whose north-west cell
/// is `origin`.
///
/// The cells a body stands on are joined into walkable stretches; the start
/// is in the stretch of at least [`ROOM_CELLS`] whose nearest dry cell is
/// nearest the realm's centre, or failing any that large, the largest, at
/// its dry cell nearest the centre.
fn start_on(
    terrain: &impl Terrain,
    origin: CellCoord,
    radius: u16,
) -> Result<Option<(WorldPoint, usize)>, ClientError> {
    let grid = Grid { origin };
    let stretches = Stretches::label(terrain, grid, radius)?;

    // Each stretch's dry cell nearest the centre, ranked (distance, row,
    // column) so the choice is total.
    let mut nearest: Vec<Option<(i64, i32, i32)>> =
        try_filled(stretches.sizes.len(), None).map_err(out_of_memory)?;
    for (index, &label) in stretches.labels.iter().enumerate() {
        let Some(slot) = nearest.get_mut(usize::from(label)) else {
            continue;
        };
        let cell = grid.cell(index);
        let Some(at) = cell.centre() else {
            continue;
        };
        if !dry(terrain, at, radius) {
            continue;
        }
        let rank = (distance_sq(at), cell.y, cell.x);
        if slot.is_none_or(|held| rank < held) {
            *slot = Some(rank);
        }
    }

    let roomy = stretches
        .sizes
        .iter()
        .zip(&nearest)
        .filter_map(|(&size, rank)| rank.map(|rank| (size, rank)))
        .filter(|&(size, _)| size >= ROOM_CELLS)
        .min_by_key(|&(_, rank)| rank);
    // Failing a roomy stretch, the largest, ties to the nearer.
    let chosen = roomy.or_else(|| {
        stretches
            .sizes
            .iter()
            .zip(&nearest)
            .filter_map(|(&size, rank)| rank.map(|rank| (size, rank)))
            .min_by_key(|&(size, rank)| (core::cmp::Reverse(size), rank))
    });
    Ok(chosen.and_then(|(size, (_, y, x))| CellCoord::new(x, y).centre().map(|at| (at, size))))
}

/// A chunk's cells, addressed by their row-major index.
#[derive(Copy, Clone)]
struct Grid {
    origin: CellCoord,
}

impl Grid {
    /// The cell at row-major `index`.
    fn cell(self, index: usize) -> CellCoord {
        CellCoord::new(
            self.origin.x + signed_index(index % EDGE),
            self.origin.y + signed_index(index / EDGE),
        )
    }

    /// The row-major index of `cell`, or `None` outside the chunk.
    fn index(self, cell: CellCoord) -> Option<usize> {
        let x = usize::try_from(cell.x - self.origin.x).ok()?;
        let y = usize::try_from(cell.y - self.origin.y).ok()?;
        (x < EDGE && y < EDGE).then_some(y * EDGE + x)
    }
}

/// A chunk's cells joined into walkable stretches.
struct Stretches {
    /// Each cell's stretch, or [`UNREACHED`] where no body stands.
    labels: Vec<u16>,
    /// How many cells each stretch holds.
    sizes: Vec<usize>,
}

impl Stretches {
    /// Label every cell of `grid`'s chunk a body of `radius` stands on with the
    /// stretch it belongs to, joining four-neighbours a body can step between
    /// both ways.
    fn label(terrain: &impl Terrain, grid: Grid, radius: u16) -> Result<Self, ClientError> {
        let mut labels = try_filled(CHUNK_AREA, UNREACHED).map_err(out_of_memory)?;
        let mut standing = try_filled(CHUNK_AREA, false).map_err(out_of_memory)?;
        for (index, stands) in standing.iter_mut().enumerate() {
            let cell = grid.cell(index);
            *stands = cell
                .centre()
                .is_some_and(|at| footprint_clear(terrain, cell, at, radius));
        }

        let mut sizes = Vec::new();
        let mut frontier: Vec<u16> = Vec::new();
        frontier
            .try_reserve_exact(CHUNK_AREA)
            .map_err(out_of_memory)?;
        for seed in 0..CHUNK_AREA {
            if !standing[seed] || labels[seed] != UNREACHED {
                continue;
            }
            let label = u16::try_from(sizes.len()).map_err(|_| ClientError::World)?;
            labels[seed] = label;
            frontier.clear();
            frontier.push(index_u16(seed));
            let mut size = 0;
            while let Some(here) = frontier.pop() {
                size += 1;
                let from = grid.cell(usize::from(here));
                for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                    let to = CellCoord::new(from.x + dx, from.y + dy);
                    let Some(there) = grid.index(to) else {
                        continue;
                    };
                    if standing[there]
                        && labels[there] == UNREACHED
                        && both_ways(terrain, from, to, radius)
                    {
                        labels[there] = label;
                        frontier.push(index_u16(there));
                    }
                }
            }
            sizes.try_reserve(1).map_err(out_of_memory)?;
            sizes.push(size);
        }
        Ok(Self { labels, sizes })
    }
}

/// Whether a body of `radius` can step from `a`'s centre to `b`'s and back.
///
/// Both ways, because a drop is legal and the climb back is not: a stretch
/// joined through one would hold ground the body could reach and never leave.
fn both_ways(terrain: &impl Terrain, a: CellCoord, b: CellCoord, radius: u16) -> bool {
    let (Some(at_a), Some(at_b)) = (a.centre(), b.centre()) else {
        return false;
    };
    footprint_clear(terrain, a, at_b, radius) && footprint_clear(terrain, b, at_a, radius)
}

/// Whether no water stands anywhere under a body of `radius` at `at`.
fn dry(terrain: &impl Terrain, at: WorldPoint, radius: u16) -> bool {
    footprint(at, radius).all(|cell| terrain.cell(cell).is_some_and(|cell| cell.depth() == 0))
}

/// How far `at` is from the realm's centre, squared.
fn distance_sq(at: WorldPoint) -> i64 {
    let (x, y) = (i64::from(at.x), i64::from(at.y));
    x * x + y * y
}

/// Every allocation this search makes is its working set, so a refusal of
/// any is the one answer.
fn out_of_memory<E>(_: E) -> ClientError {
    ClientError::OutOfMemory
}

/// A chunk cell index, which [`CHUNK_AREA`] keeps inside `u16`.
fn index_u16(index: usize) -> u16 {
    u16::try_from(index).unwrap_or(UNREACHED)
}

/// A chunk cell offset, which [`CHUNK_CELLS`] keeps inside `i32`.
fn signed_index(offset: usize) -> i32 {
    i32::try_from(offset).unwrap_or(i32::MAX)
}

#[cfg(test)]
#[path = "landfall_tests.rs"]
mod tests;
