//! Drainage, lakes, discharge and erosion, over the whole realm at once.
//!
//! The algorithms are `lib/terrain`'s: Priority-Flood drainage, whose pop
//! order is the downstream-first order accumulation needs and whose fill is a
//! lake's surface at its outflow; detachment-limited stream-power incision
//! (`E = K·√A·S`); and linear hillslope diffusion. That is deliberately a
//! landscape-evolution model and not a particle simulation: it cuts valleys
//! along the drainage the flood established, and the diffusion lays the flats
//! in their floors. Here they run over the coarse field with the realm's
//! outlets — its rim, and everything at or below sea level — for a bounded
//! number of passes, each reading a snapshot and writing a delta, so no pass
//! depends on the order its samples are visited in.

use tairix_terrain::drainage::Network;
use tairix_terrain::hillslope::{self, Diffusion};
use tairix_terrain::incision::{self, Explicit};
use tairix_terrain::{Grid, TerrainError};
use tairix_util::mathf;

pub use tairix_terrain::FlowDir;

use crate::error::WorldError;
use crate::geom::Elevation;
use crate::params::RealmParams;
use crate::realm::{try_filled, CoarseSample};

/// Erosion passes. Each is a full solve of the drainage, so the count is
/// what bounds the stage; four is where the incised network stops changing
/// shape and only deepens, which a further pass cannot improve.
const EROSION_PASSES: u32 = 4;

/// Stream-power incision coefficient.
const EROSION_K: f64 = 0.012;

/// Hillslope diffusion coefficient, per pass. Below a quarter, so the
/// explicit update is unconditionally stable on the five-point stencil.
const DIFFUSION: f64 = 0.16;

/// Sea level in world units: erosion cuts valleys, it does not enlarge the
/// ocean the operator asked for, so nothing is cut or crept below it.
const SEA: f64 = 0.0;

/// The specific catchment of a coarse `discharge`, in cells: the area that
/// drains in from upstream, over the width of the coarse step it drains
/// across.
///
/// The sample's own area is left out, so a ridge top drains nothing at any
/// coarse step. The same drained area reads larger at a finer step, so a
/// threshold on it is calibrated at one step: the default realm's.
#[must_use]
pub fn specific_catchment(params: RealmParams, discharge: f64) -> f64 {
    mathf::fmax(discharge - 1.0, 0.0) * f64::from(params.cells_per_coarse())
}

/// Erode `samples`, then record the final drainage into them.
///
/// # Errors
///
/// [`WorldError::OutOfMemory`] if the working vectors do not fit, and
/// [`WorldError::Mismatch`] if `samples` is not the realm's coarse grid.
pub fn solve(params: RealmParams, samples: &mut [CoarseSample]) -> Result<(), WorldError> {
    let grid = Grid::new(params.coarse_samples());
    let mut height = try_filled(samples.len(), 0.0_f64)?;
    for (slot, sample) in height.iter_mut().zip(samples.iter()) {
        *slot = sample.elevation.units();
    }

    let step_units = f64::from(params.cells_per_coarse());
    for _ in 0..EROSION_PASSES {
        let network = drain(&height, grid)?;
        let incise = Explicit {
            k: EROSION_K,
            floor: SEA,
        };
        incision::incise(&mut height, &network, grid, step_units, incise).map_err(world)?;
        let creep = Diffusion {
            rate: DIFFUSION,
            floor: SEA,
        };
        hillslope::diffuse(&mut height, grid, creep).map_err(world)?;
    }

    let network = drain(&height, grid)?;
    for (index, sample) in samples.iter_mut().enumerate() {
        let ground = Elevation::from_units(height[index]);
        sample.elevation = ground;
        // Standing water is the filled surface where the flood had to
        // raise the ground to get out — a lake — and sea level where the
        // ground is below it. Dry ground's water surface is the ground.
        sample.water = Elevation::from_units(network.filled[index]).max(if ground.is_submerged() {
            Elevation::SEA_LEVEL
        } else {
            ground
        });
        sample.flow = network.flow[index];
        sample.discharge = network.discharge[index];
    }
    Ok(())
}

/// The drainage of `height`: the realm's rim and its sea drain out of it.
fn drain(height: &[f64], grid: Grid) -> Result<Network, WorldError> {
    let outlet = |index: usize| {
        let (x, y) = grid.position(index);
        grid.is_rim(x, y) || height[index] <= SEA
    };
    Network::solve(height, grid, outlet).map_err(world)
}

/// A terrain pass's refusal as the world's.
fn world(error: TerrainError) -> WorldError {
    match error {
        TerrainError::OutOfMemory => WorldError::OutOfMemory,
        TerrainError::Shape => WorldError::Mismatch,
    }
}

#[cfg(test)]
mod tests;
