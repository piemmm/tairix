//! The realm's ground beyond the chunks a viewer has solved: level 0's land,
//! read for its water and for what grows on it.
//!
//! A chunk is a large part of a second of work, so a view across a valley
//! cannot wait for every chunk it reaches. Level 0 is already refined over
//! the whole realm, and every condition the classifier reads but the fine
//! relief has a coarse answer, so a far patch is that land at level 0's own
//! step, classified by the chunk's classifier and coloured by its palette.
//! It is coarser than a chunk, not a different world: a solved chunk
//! replaces it where it lies.
//!
//! Every vertex is a pure function of its cell and its four neighbours, which
//! any patch's one-vertex halo holds, so two patches agree along their seam.

use alloc::vec::Vec;

use tairix_util::mathf;
use tairix_wintersun_net::value::ChunkCoord;

use crate::biome::{Conditions, Water};
use crate::blend::Blend;
use crate::chunk::{
    biomes_of, channels, distance_to_segment, dry_ground, ground_site, wetness, Channel, Reading,
};
use crate::climate::LAPSE_RATE;
use crate::error::WorldError;
use crate::geom::{chunk_origin, lerp, signed, CellCoord, Elevation, CHUNK_CELLS};
use crate::ground::{self, Ground};
use crate::hydrology;
use crate::land::RealmLand;
use crate::realm::{try_filled, RealmField};

/// One chunk's ground at level 0's step: each vertex's surface — water where
/// water stands, the ground elsewhere — and the grounds it shows.
#[derive(Clone, Debug)]
pub struct FarPatch {
    coord: ChunkCoord,
    step: u32,
    side: u32,
    /// The surface over the patch and its one-vertex halo.
    surface: Vec<Elevation>,
    ground: Vec<Blend<Ground>>,
}

impl FarPatch {
    /// The chunk it lies over.
    #[must_use]
    pub const fn coord(&self) -> ChunkCoord {
        self.coord
    }

    /// Cells between its vertices.
    #[must_use]
    pub const fn step(&self) -> u32 {
        self.step
    }

    /// Vertices along each edge: the chunk's corners are both vertices.
    #[must_use]
    pub const fn side(&self) -> u32 {
        self.side
    }

    /// The surface at vertex `(i, j)`, `step` cells apart from the chunk's
    /// first cell, or `None` past the patch.
    #[must_use]
    pub fn surface(&self, i: u32, j: u32) -> Option<Elevation> {
        self.index(i, j)?;
        self.surface_beside(i64::from(i), i64::from(j))
    }

    /// The surface at vertex `(i, j)` within one vertex of the patch: inside
    /// it, or what the neighbouring patch holds there, so a slope read at
    /// the patch's edge is the slope read from either side of it.
    #[must_use]
    pub fn surface_beside(&self, i: i64, j: i64) -> Option<Elevation> {
        let grid = i64::from(self.side) + 2;
        let inside = |at: i64| (-1..=i64::from(self.side)).contains(&at);
        if !inside(i) || !inside(j) {
            return None;
        }
        let index = usize::try_from((j + 1) * grid + (i + 1)).ok()?;
        self.surface.get(index).copied()
    }

    /// The grounds vertex `(i, j)` shows.
    #[must_use]
    pub fn ground(&self, i: u32, j: u32) -> Option<Blend<Ground>> {
        self.ground.get(self.index(i, j)?).copied()
    }

    /// Heap bytes the patch retains.
    #[must_use]
    pub fn payload_bytes(&self) -> usize {
        core::mem::size_of_val(self.surface.as_slice())
            + core::mem::size_of_val(self.ground.as_slice())
    }

    fn index(&self, i: u32, j: u32) -> Option<usize> {
        (i < self.side && j < self.side).then(|| (j as usize) * (self.side as usize) + (i as usize))
    }
}

/// What one vertex of a far patch's grid is, its halo's included.
#[derive(Copy, Clone, Debug)]
struct Vertex {
    cell: CellCoord,
    ground: f64,
    /// The water standing there, and which water it is, or `None` dry.
    water: Option<(f64, Water)>,
}

impl Vertex {
    fn surface(&self) -> f64 {
        self.water.map_or(self.ground, |(level, _)| level)
    }
}

impl RealmLand {
    /// Cells between a far patch's vertices: level 0's own step, where it
    /// divides a chunk, so every vertex is one of its vertices; a chunk's
    /// width where level 0 is coarser still.
    #[must_use]
    pub fn far_step(&self) -> u32 {
        let step = self.level_zero_step();
        if step > 0 && step <= CHUNK_CELLS && CHUNK_CELLS.is_multiple_of(step) {
            step
        } else {
            CHUNK_CELLS
        }
    }

    /// The far patch over chunk `coord`.
    ///
    /// # Errors
    ///
    /// [`WorldError::OutOfMemory`] if the patch does not fit, and
    /// [`WorldError::OutOfRange`] for a chunk off the realm.
    pub fn far(&self, coord: ChunkCoord) -> Result<FarPatch, WorldError> {
        let field = self.field();
        if !field.params().holds_chunk(coord.x, coord.y) {
            return Err(WorldError::OutOfRange);
        }
        let step = self.far_step();
        let side = CHUNK_CELLS / step + 1;
        // One vertex of halo on every side, for the slope, the gradient and
        // the shore a vertex reads from its neighbours.
        let grid = side + 2;
        let origin = chunk_origin(coord);
        let reach = f64::from(step) * 2.0;
        let courses = channels(field, coord, reach)?;
        let mut vertices = Vec::new();
        vertices
            .try_reserve((grid as usize) * (grid as usize))
            .map_err(|_| WorldError::OutOfMemory)?;
        for gj in 0..grid {
            for gi in 0..grid {
                let cell = CellCoord::new(
                    origin.x + (signed(gi) - 1) * signed(step),
                    origin.y + (signed(gj) - 1) * signed(step),
                );
                vertices.push(self.vertex(field, cell, &courses));
            }
        }

        let area = (side as usize) * (side as usize);
        let mut surface = try_filled((grid as usize) * (grid as usize), Elevation::SEA_LEVEL)?;
        for (held, vertex) in surface.iter_mut().zip(&vertices) {
            *held = Elevation::from_units(vertex.surface());
        }
        let mut ground = try_filled(area, Blend::solid(Ground::Water))?;
        let at = |gi: u32, gj: u32| vertices[(gj as usize) * (grid as usize) + (gi as usize)];
        for j in 0..side {
            for i in 0..side {
                let (gi, gj) = (i + 1, j + 1);
                let here = at(gi, gj);
                let index = (j as usize) * (side as usize) + (i as usize);
                let neighbours = [
                    at(gi + 1, gj),
                    at(gi - 1, gj),
                    at(gi, gj + 1),
                    at(gi, gj - 1),
                ];
                ground[index] = if here.water.is_some() {
                    Blend::solid(Ground::Water)
                } else {
                    let reading = far_reading(field, here, &neighbours, step);
                    let biomes = biomes_of(Some(&reading));
                    ground::cover(&biomes, &ground_site(field, &reading))
                };
            }
        }
        Ok(FarPatch {
            coord,
            step,
            side,
            surface,
            ground,
        })
    }

    /// Level 0's ground at `cell` and the water standing over it.
    fn vertex(&self, field: &RealmField, cell: CellCoord, courses: &[Channel]) -> Vertex {
        let ground = self.level_zero_height(cell);
        let (gx, gy) = field.grid_position(cell);
        let coarse = field.coarse_at(gx, gy);
        let coarse_water = coarse.water_units();
        let holds = coarse_water > coarse.elevation_units();
        let sea = holds && coarse.sea_share() >= 0.5;
        let still = if sea { 0.0 } else { coarse_water };
        let standing = (holds && still > ground)
            .then_some((still, if sea { Water::Sea } else { Water::Lake }));
        let point = (f64::from(cell.x), f64::from(cell.y));
        let running = courses
            .iter()
            .filter_map(|course| {
                let (distance, along) = distance_to_segment(course.from, course.to, point);
                (distance < course.half_width)
                    .then(|| lerp(course.surface.0, course.surface.1, along))
            })
            .fold(None, |highest: Option<f64>, level| {
                Some(highest.map_or(level, |h| mathf::fmax(h, level)))
            });
        let water = standing.or_else(|| running.map(|level| (level, Water::Running)));
        Vertex {
            cell,
            ground: if water.is_some() {
                ground
            } else {
                dry_ground(ground)
            },
            water,
        }
    }
}

/// What the classifier reads at a dry far vertex `here`, its four
/// neighbours `neighbours` east, west, south and north of it, `step`
/// cells away.
fn far_reading(field: &RealmField, here: Vertex, neighbours: &[Vertex; 4], step: u32) -> Reading {
    let (gx, gy) = field.grid_position(here.cell);
    let coarse = field.coarse_at(gx, gy);
    let catchment = hydrology::specific_catchment(field.params(), coarse.discharge());
    let run = f64::from(step);
    let gradient = mathf::hypot(
        (neighbours[0].surface() - neighbours[1].surface()) / (2.0 * run),
        (neighbours[2].surface() - neighbours[3].surface()) / (2.0 * run),
    );
    let slope = neighbours.iter().fold(0.0, |steepest, neighbour| {
        mathf::fmax(
            steepest,
            mathf::fabs(neighbour.surface() - here.ground) / run,
        )
    });
    let shore = neighbours
        .iter()
        .filter_map(|neighbour| neighbour.water.map(|(_, water)| water))
        .max()
        .map_or((u16::MAX, Water::Running), |water| {
            (u16::try_from(step).unwrap_or(u16::MAX), water)
        });
    let celsius = coarse.celsius() - (here.ground - coarse.elevation_units()) * LAPSE_RATE;
    Reading {
        cell: here.cell,
        catchment,
        conditions: Conditions {
            celsius,
            range: coarse.range_celsius(),
            continentality: coarse.continentality(),
            precipitation: coarse.precipitation(),
            rain_season: coarse.rain_season(),
            wetness: wetness(catchment, gradient),
            elevation_units: here.ground,
            slope,
            rift: coarse.rift(),
            lithology: field.lithology_at(gx, gy),
            shore,
        },
    }
}

#[cfg(test)]
mod tests;
