//! Where a session's first body stands.
//!
//! The realm is centred on the origin, but the origin may be sea, a lake or a
//! river bed, and a body placed where it cannot stand never moves. So a
//! session starts on the ground nearest the centre that the zone's own spawn
//! rule admits.

use alloc::collections::BinaryHeap;
use alloc::vec::Vec;

use tairix_wintersun_net::value::WorldPoint;
use tairix_wintersun_rules::motion::footprint_clear;
use tairix_wintersun_rules::terrain::ChunkTerrain;
use tairix_wintersun_world::chunk::{Chunk, ChunkBuild};
use tairix_wintersun_world::geom::{chunk_origin, signed, CellCoord, CHUNK_CELLS};
use tairix_wintersun_world::realm::RealmField;

use crate::error::ClientError;

/// Dry coarse samples tried before a realm is reported as having no ground
/// near its centre. Each try solves a chunk, so this bounds how long a
/// session can take to find its feet.
pub const TRIES: usize = 16;

/// Where a session starts, and the chunk solved to find it.
#[derive(Debug)]
pub struct Landfall {
    /// Where the body stands: the centre of a cell.
    pub at: WorldPoint,
    /// The chunk it stands in, so the caller holds it rather than solving it
    /// again.
    pub chunk: Chunk,
}

/// The ground nearest the realm's centre a body of `radius` can stand on.
///
/// Dry coarse samples are tried nearest the centre first, each solved to its
/// chunk; within one, cells are searched ring by ring out from the sample's
/// own cell for a footprint [`footprint_clear`] admits.
///
/// # Errors
///
/// [`ClientError::NoGround`] when none of the [`TRIES`] nearest dry samples
/// holds a body, and [`ClientError::World`] when a chunk cannot be solved.
pub fn landfall(field: &RealmField, radius: u16) -> Result<Landfall, ClientError> {
    let params = field.params();
    for (sx, sy) in nearest_land(field)? {
        let target = params.sample_cell(sx, sy);
        let chunk = ChunkBuild::new(target.chunk())
            .and_then(|build| build.finish(field))
            .map_err(|_| ClientError::World)?;
        if let Some(at) = standing(&chunk, target, radius)? {
            return Ok(Landfall { at, chunk });
        }
    }
    Err(ClientError::NoGround)
}

/// The [`TRIES`] dry coarse samples nearest the one at the origin, nearest
/// first, ties broken by row then column.
fn nearest_land(field: &RealmField) -> Result<Vec<(i32, i32)>, ClientError> {
    let side = signed(field.side());
    let first = field.params().sample_cell(0, 0);
    let step = signed(field.params().cells_per_coarse());
    let centre = ((-first.x).div_euclid(step), (-first.y).div_euclid(step));

    // A max-heap of the nearest seen so far, so the scan holds TRIES entries
    // rather than every dry sample in the realm.
    let mut nearest = BinaryHeap::new();
    nearest
        .try_reserve_exact(TRIES + 1)
        .map_err(|_| ClientError::OutOfMemory)?;
    for (index, sample) in field.samples().iter().enumerate() {
        if sample.is_water() {
            continue;
        }
        let index = i32::try_from(index).map_err(|_| ClientError::World)?;
        let (sx, sy) = (index % side, index / side);
        let (dx, dy) = (i64::from(sx - centre.0), i64::from(sy - centre.1));
        nearest.push((dx * dx + dy * dy, sy, sx));
        if nearest.len() > TRIES {
            nearest.pop();
        }
    }

    let mut ordered = Vec::new();
    ordered
        .try_reserve_exact(nearest.len())
        .map_err(|_| ClientError::OutOfMemory)?;
    ordered.extend(
        nearest
            .into_sorted_vec()
            .into_iter()
            .map(|(_, sy, sx)| (sx, sy)),
    );
    Ok(ordered)
}

/// The first cell of `chunk`, ring by ring out from `target`, where a body
/// of `radius` stands clear.
fn standing(
    chunk: &Chunk,
    target: CellCoord,
    radius: u16,
) -> Result<Option<WorldPoint>, ClientError> {
    let window = [chunk];
    let terrain = ChunkTerrain::new(&window).map_err(|_| ClientError::World)?;
    let origin = chunk_origin(chunk.coord());
    let edge = signed(CHUNK_CELLS);
    let (tx, ty) = (target.x - origin.x, target.y - origin.y);

    for ring in 0..edge {
        for dy in -ring..=ring {
            // Every cell of the ring's top and bottom rows, only the two ends
            // of the rows between.
            let stride = if dy.abs() == ring { 1 } else { 2 * ring };
            let mut dx = -ring;
            while dx <= ring {
                let (x, y) = (tx + dx, ty + dy);
                if (0..edge).contains(&x) && (0..edge).contains(&y) {
                    let cell = CellCoord::new(origin.x + x, origin.y + y);
                    if let Some(at) = cell.centre() {
                        if footprint_clear(&terrain, cell, at, radius) {
                            return Ok(Some(at));
                        }
                    }
                }
                dx += stride;
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
#[path = "landfall_tests.rs"]
mod tests;
