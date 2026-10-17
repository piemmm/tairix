//! The realm's fine ground: the realm field refined by the land generator's
//! levels (`lib/land`), the one the desktop's ray tracer lays its landscapes
//! with, at its *Maximum* detail (`plans/RAYTRACE.md` RT40.5).
//!
//! The field's samples are the generator's coarse lattice, padded beyond the
//! realm's edge by the samples at it, so every chunk's working grid lies on
//! the land. Level 0 refines it a fixed few times over across the whole
//! realm, once; level 1 refines that to the cells, a chunk's working grid at
//! a time. Droplets wear both, each piece exactly as the realm worn whole,
//! and the blocks a level-1 piece is worn in are kept in a store the caller
//! supplies, so a chunk beside one already solved pays only for the ground no
//! earlier chunk reached.
//!
//! The relief below the coarse step takes its grain from the field: ridged
//! in a mountain belt, billowed into dunes where it is dry, and fading into
//! the sea floor, so a coastline lies where the realm put it.

use alloc::vec::Vec;

use tairix_land::coarse::{wetness, Coarse, Field};
use tairix_land::level::{Keeps, Parent, Refining};
use tairix_land::plan::{Character, Grain, Grains, Lattice, Lattices, Level, LEVELS};
use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

pub use tairix_terrain::droplet::{Block, Kept, Placed, Store, Unkept};

use crate::chunk::SHORE_CELLS;
use crate::error::WorldError;
use crate::geom::{lerp, rise, signed, Elevation};
use crate::realm::RealmField;
use crate::seed::Stage;

/// The most cells level 0 lays across the realm a side: its size is the
/// field's sample count times its ratio, held to this whatever the realm.
const LEVEL_ZERO_CELLS: u32 = 2048;

/// How many times over level 0 refines the field, at most.
const LEVEL_ZERO_RATIO: u32 = 8;

/// The droplets each level runs, to each vertex they may start on: as many
/// as the tracer's meadow and coast run over their far land and their
/// middle distance.
const LEVEL_ZERO_DROPLETS: f64 = 0.05;
const CELL_DROPLETS: f64 = 0.08;

/// The longest wave of relief the levels add below the coarse step, in
/// world units: absolute, so a hillside's texture is the same size in a small
/// realm and a large one.
const DETAIL_UNITS: f64 = 380.0;

/// How rough the relief is on open ground, and at the heart of a mountain
/// belt, in world units.
const DETAIL_AMPLITUDE: f64 = 22.0;
const BELT_AMPLITUDE: f64 = 46.0;

/// The share of open ground's roughness a sea of dunes lies in where it is
/// driest: dunes some seven units high on ground twenty-two rough.
const DUNE_SHARE: f64 = 7.0 / DETAIL_AMPLITUDE;

/// The height above the sea over which the relief below the coarse step
/// fades in: a sea floor the player never sees takes none, so none pokes
/// above the water to move the coastline the realm asked for.
const SHORE_FADE_UNITS: f64 = 12.0;

/// The realm field, and its land as the levels refine it.
#[derive(Debug)]
pub struct RealmLand {
    field: RealmField,
    coarse: Coarse,
    /// Level 0 over the whole realm: its square of the level's lattice, and
    /// its heights.
    far: (Placed, Vec<f32>),
}

impl RealmLand {
    /// The land of `field`, its level 0 refined across `runner`.
    ///
    /// # Errors
    ///
    /// [`WorldError::OutOfMemory`] if the land does not fit, and
    /// [`WorldError::Mismatch`] for a field the levels cannot lie on.
    pub fn new(field: RealmField, runner: &dyn JobRunner) -> Result<Self, WorldError> {
        let coarse = Coarse::of_field(survey(&field)?, runner)?;
        let domain = coarse.lattices().domain(0)?;
        let mut refining = Refining::new(&coarse, (0, domain), Keeps::Heights, &mut Unkept)?;
        while !refining.step(&coarse, None, &mut Unkept, runner)? {}
        let piece = refining.finish()?;
        Ok(Self {
            field,
            coarse,
            far: (piece.square, piece.heights),
        })
    }

    /// The realm field it was refined from.
    #[must_use]
    pub const fn field(&self) -> &RealmField {
        &self.field
    }

    /// The ground's height at each cell of `region`, row by row, the cells
    /// worn in blocks `store` keeps where it keeps them, across `runner`.
    ///
    /// # Errors
    ///
    /// [`WorldError::OutOfRange`] for a region off the land,
    /// [`WorldError::OutOfMemory`] if its working set does not fit, and
    /// [`WorldError::Mismatch`] where the land does not hold what the region
    /// reads.
    pub fn ground(
        &self,
        region: Placed,
        store: &mut dyn Store,
        runner: &dyn JobRunner,
    ) -> Result<Vec<f32>, WorldError> {
        let domain = self.coarse.lattices().domain(CELLS)?;
        if !domain.holds(region) {
            return Err(WorldError::OutOfRange);
        }
        let parent = Parent {
            square: self.far.0,
            heights: &self.far.1,
            attributes: &[],
        };
        let mut refining = Refining::new(&self.coarse, (CELLS, region), Keeps::Heights, store)?;
        while !refining.step(&self.coarse, Some(parent), store, runner)? {}
        Ok(refining.finish()?.heights)
    }
}

/// The level whose vertices are the cells.
const CELLS: usize = 1;

/// A store that keeps the blocks it is handed until they hold its bound in
/// bytes, for a set of pieces solved together, as the digest's probes are.
/// Full, or refused room, it keeps no more, and the pieces are worn all the
/// same.
#[derive(Debug)]
pub struct Keeping {
    blocks: Vec<(Block, Kept)>,
    bytes: usize,
    bound: usize,
}

impl Keeping {
    /// A store keeping at most `bound` bytes of blocks.
    #[must_use]
    pub const fn within(bound: usize) -> Self {
        Self {
            blocks: Vec::new(),
            bytes: 0,
            bound,
        }
    }
}

impl Store for Keeping {
    fn read(&mut self, block: Block, read: &mut dyn FnMut(&Kept)) -> bool {
        let found = self
            .blocks
            .binary_search_by(|(at, _)| at.cmp(&block))
            .ok()
            .and_then(|index| self.blocks.get(index));
        if let Some((_, kept)) = found {
            read(kept);
        }
        found.is_some()
    }

    fn keep(&mut self, block: Block, kept: Kept) {
        let size = kept.bytes();
        match self.blocks.binary_search_by(|(at, _)| at.cmp(&block)) {
            Ok(index) => {
                let held = self.blocks.get(index).map_or(0, |(_, held)| held.bytes());
                let rest = self.bytes.saturating_sub(held);
                if rest.saturating_add(size) <= self.bound {
                    if let Some(slot) = self.blocks.get_mut(index) {
                        slot.1 = kept;
                        self.bytes = rest + size;
                    }
                } else {
                    // Too large to keep in its place: the block it would
                    // replace goes too, as no longer what the ground is.
                    self.blocks.remove(index);
                    self.bytes = rest;
                }
            }
            Err(index) => {
                if self.bytes.saturating_add(size) <= self.bound
                    && self.blocks.try_reserve(1).is_ok()
                {
                    self.bytes += size;
                    self.blocks.insert(index, (block, kept));
                }
            }
        }
    }
}

/// The field's samples as the land generator's coarse land: its lattice
/// padded by the samples at the realm's edge, the levels it is refined
/// through, and how it lies.
fn survey(field: &RealmField) -> Result<Field, WorldError> {
    let params = field.params();
    let side = params.coarse_samples();
    let step = params.cells_per_coarse();
    // Far enough past the realm's edge for the last sample's step and every
    // edge chunk's ring.
    let pad = 1 + SHORE_CELLS.div_ceil(step);
    let cells = side - 1 + 2 * pad;
    let first = -signed(side / 2) - signed(pad);
    let lattice = Lattice {
        step: f64::from(step),
        origin: (i64::from(first), i64::from(first)),
        cells,
    };
    let lattices = Lattices::new(lattice, levels(side, step)).map_err(WorldError::from)?;
    let across = cells + 1;
    let area = usize::try_from(across * across).map_err(|_| WorldError::Mismatch)?;
    let (mut heights, mut filled, mut wet, mut grains) = (
        reserved(area)?,
        reserved(area)?,
        reserved(area)?,
        reserved(area)?,
    );
    let cell_area = f64::from(step) * f64::from(step);
    for row in 0..across {
        for column in 0..across {
            let sample = field.sample(signed(column) - signed(pad), signed(row) - signed(pad));
            let elevation = sample.elevation.units();
            heights.push(elevation);
            filled.push(sample.water.units());
            wet.push(wetness(f64::from(sample.discharge) * cell_area));
            let ridge = mathf::smoothstep(f64::from(sample.belt) / f64::from(u8::MAX));
            let arid = 1.0 - rise(sample.precipitation.millimetres(), 250.0, 300.0);
            let fade = mathf::clamp(elevation / SHORE_FADE_UNITS, 0.0, 1.0);
            grains.push(Grain {
                roughness: lerp(DETAIL_AMPLITUDE, BELT_AMPLITUDE, ridge) * fade,
                ridges: ridge,
                dunes: DUNE_SHARE * arid * (1.0 - ridge),
            });
        }
    }
    Ok(Field {
        lattices,
        heights,
        filled,
        wetness: wet,
        grains: Grains::Sampled(grains),
        character: Character {
            longest: DETAIL_UNITS,
            growth: 1.0,
            snow_line: None,
            sea: Some(Elevation::SEA_LEVEL.units()),
            snowpack: None,
            seed: seed_of(field),
        },
    })
}

/// The levels a realm of `side` samples `step` cells apart is refined
/// through: level 0 a fixed few times over the field, as many as hold it
/// within its most cells, and level 1 the rest of the way to the cells.
fn levels(side: u32, step: u32) -> [Option<Level>; LEVELS] {
    let far = LEVEL_ZERO_RATIO
        .min(step)
        .min((LEVEL_ZERO_CELLS / side).max(1));
    let mut levels = [None; LEVELS];
    levels[0] = Some(Level {
        ratio: far,
        droplets: LEVEL_ZERO_DROPLETS,
    });
    levels[CELLS] = Some(Level {
        ratio: step / far,
        droplets: CELL_DROPLETS,
    });
    levels
}

/// The seed the realm's fine relief is drawn under.
fn seed_of(field: &RealmField) -> u32 {
    let key = field.key().lattice(Stage::Detail, 0, 0);
    u32::try_from(key & u64::from(u32::MAX)).unwrap_or(0)
}

/// An empty buffer holding `area` entries, refused rather than aborting on a
/// short heap.
fn reserved<T>(area: usize) -> Result<Vec<T>, WorldError> {
    let mut buffer = Vec::new();
    if fallible::reserve(&mut buffer, area) {
        Ok(buffer)
    } else {
        Err(WorldError::OutOfMemory)
    }
}

#[cfg(test)]
mod tests;
