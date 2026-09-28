//! Rock provinces, and the soils that weather out of them.
//!
//! A continent is not one rock. Its old, buoyant interior is a planed-down
//! crystalline shield; the belts where plates collided are metamorphic cores
//! intruded by granite; rifts and ocean floors are basalt; and the platforms
//! between are sediments — limestone, sandstone, shale and chalk. What the
//! rock is decides what a cliff looks like, what a beach is made of, where a
//! bog can form, where rain can cut badlands, and what a soil is.
//!
//! # Provinces
//!
//! Rock comes in provinces: a jittered-grid Voronoi partition several times
//! finer than the plates', each province taking its rock from the tectonic
//! setting at its own site, read through the relief's continental warp so a
//! metamorphic core sits under the mountain belt the relief raised. A
//! province is identified by its grid cell, and the partition is looked up
//! as a pure function of position with a noise-perturbed query point, so a
//! boundary wanders rather than running straight and is the same boundary
//! from either side of any seam.
//!
//! The table is built once per realm and its size follows the plate count,
//! never the realm's extent.

use alloc::vec::Vec;

use tairix_util::mathf;

use crate::error::WorldError;
use crate::geom::{rise, signed, smoothstep};
use crate::noise;
use crate::relief::continental_warp;
use crate::seed::{SeedKey, Stage};
use crate::uplift::Plates;
use crate::voronoi::{self, wrap};

/// A rock class.
///
/// The discriminants are frozen: a new class takes a new number and never
/// reuses a retired one.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(u8)]
pub enum Rock {
    /// The planed-down crystalline core of an old continent.
    Shield = 0,
    /// Coarse intrusive granite.
    Granite = 1,
    /// Basalt: ocean floor, rift and flood lava.
    Basalt = 2,
    /// Limestone.
    Limestone = 3,
    /// Sandstone.
    Sandstone = 4,
    /// Shale and mudstone.
    Shale = 5,
    /// Chalk.
    Chalk = 6,
    /// Schist and gneiss: the core of a collision belt.
    Metamorphic = 7,
}

impl Rock {
    /// Every class, in discriminant order.
    pub const ALL: [Self; 8] = [
        Self::Shield,
        Self::Granite,
        Self::Basalt,
        Self::Limestone,
        Self::Sandstone,
        Self::Shale,
        Self::Chalk,
        Self::Metamorphic,
    ];

    /// Whether it weathers to acid, base-poor ground: where a podzol and a
    /// bog form, and a fen does not.
    #[must_use]
    pub const fn is_acidic(self) -> bool {
        matches!(
            self,
            Self::Shield | Self::Granite | Self::Sandstone | Self::Metamorphic
        )
    }

    /// Whether it is lime-rich: where a fen forms rather than a bog, and a
    /// beach is pale.
    #[must_use]
    pub const fn is_calcareous(self) -> bool {
        matches!(self, Self::Limestone | Self::Chalk)
    }

    /// Whether a shore of it resists the sea, so the sea cuts cliffs into it
    /// rather than laying a beach against it.
    #[must_use]
    pub const fn is_hard(self) -> bool {
        matches!(
            self,
            Self::Shield | Self::Granite | Self::Basalt | Self::Metamorphic
        )
    }

    /// How readily rain cuts it, `0.0` barely through `1.0` into a maze of
    /// gullies: what makes dry clay country badlands.
    #[must_use]
    pub const fn softness(self) -> f64 {
        match self {
            Self::Shale => 1.0,
            Self::Sandstone => 0.6,
            Self::Chalk => 0.45,
            Self::Limestone => 0.15,
            Self::Shield | Self::Granite | Self::Basalt | Self::Metamorphic => 0.05,
        }
    }
}

/// The rock at a place, and how freshly volcanic it is.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Lithology {
    /// The rock class.
    pub rock: Rock,
    /// `0.0` long quiet through `1.0` fresh lava: basalt laid down where
    /// plates are pulling apart.
    pub volcanism: f64,
}

/// Rock provinces along one plate-grid cell's edge.
const PROVINCE_SPLIT: u32 = 4;

/// How far a province boundary wanders, in province cells.
const BOUNDARY_WANDER: f64 = 0.28;

/// Cycles of that wander across one province cell.
const WANDER_CYCLES: f64 = 2.2;

/// Buoyancy below which a plate is ocean floor.
const OCEANIC_BUOYANCY: f64 = 0.3;

/// Distance to a pulling-apart seam, in plate-grid units, within which a
/// continent floods with basalt.
///
/// Flooding is its own quantity, not the rift `uplift::Tectonics::rift`
/// measures: any opening lets the ground subside, but only a fast one near
/// the seam erupts, so the basalt reaches less far than the rift valley.
const FLOOD_BASALT_REACH: f64 = 0.3;

/// How strongly two plates must pull apart before the rift between them
/// erupts.
const FLOOD_BASALT_OPENING: f64 = 0.15;

/// One province.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Province {
    /// The site, in province-grid units.
    site: (f64, f64),
    rock: Rock,
    volcanism: f64,
}

/// A realm's rock provinces.
#[derive(Debug)]
pub struct Geology {
    key: SeedKey,
    /// Provinces along one edge of the realm.
    grid: u32,
    /// Row-major, `grid` by `grid`.
    provinces: Vec<Province>,
}

impl Geology {
    /// Settle every province of the realm the plates describe.
    ///
    /// # Errors
    ///
    /// [`WorldError::OutOfMemory`] if the table does not fit.
    pub fn new(key: SeedKey, plates: Plates) -> Result<Self, WorldError> {
        let grid = plates.grid() * PROVINCE_SPLIT;
        let area = (grid as usize) * (grid as usize);
        let mut provinces = Vec::new();
        provinces
            .try_reserve_exact(area)
            .map_err(|_| WorldError::OutOfMemory)?;
        for cy in 0..signed(grid) {
            for cx in 0..signed(grid) {
                provinces.push(settle(key, plates, grid, cx, cy));
            }
        }
        Ok(Self {
            key,
            grid,
            provinces,
        })
    }

    /// The rock at the realm-fraction position `(u, v)`.
    ///
    /// A pure function of position: the query point is perturbed by noise
    /// keyed on the realm, then the nearest province site wins.
    #[must_use]
    pub fn at(&self, u: f64, v: f64) -> Lithology {
        let grid = f64::from(self.grid);
        let query = noise::warp(
            self.key,
            Stage::Wander,
            u * grid,
            v * grid,
            WANDER_CYCLES,
            BOUNDARY_WANDER,
        );
        let province = self.nearest(query);
        Lithology {
            rock: province.rock,
            volcanism: province.volcanism,
        }
    }

    /// The province whose site is nearest `point`, in province-grid units.
    fn nearest(&self, point: (f64, f64)) -> Province {
        let [(_, (x, y))] = voronoi::nearest::<1>(point, |x, y| {
            let site = self.province(x, y).site;
            // A wrapped neighbour's site is shifted back beside the query
            // rather than measured across the realm.
            (
                site.0 + f64::from(x - wrap(x, self.grid)),
                site.1 + f64::from(y - wrap(y, self.grid)),
            )
        });
        self.province(x, y)
    }

    fn province(&self, cx: i32, cy: i32) -> Province {
        let (x, y) = (wrap(cx, self.grid), wrap(cy, self.grid));
        let index = usize::try_from(y).unwrap_or(0) * (self.grid as usize)
            + usize::try_from(x).unwrap_or(0);
        self.provinces[index]
    }
}

/// The province grid cell `(cx, cy)` holds: its site, and the rock the
/// tectonic setting there lays down.
fn settle(key: SeedKey, plates: Plates, grid: u32, cx: i32, cy: i32) -> Province {
    let mut stream = key.stream(Stage::Province, cx, cy);
    let site = voronoi::site(cx, cy, (stream.signed(), stream.signed()));
    let roll = stream.unit();

    let span = f64::from(grid);
    let (wu, wv) = continental_warp(key, site.0 / span, site.1 / span);
    let plate_grid = f64::from(plates.grid());
    let (x, y) = (wu * plate_grid, wv * plate_grid);
    let meeting = plates.meeting(x, y);
    let (near, boundary) = (meeting.near, meeting.boundary);
    let belt = meeting.tectonics().belt;

    let opening = -boundary.convergence;
    let flooding = if opening > FLOOD_BASALT_OPENING {
        mathf::clamp(
            (opening - FLOOD_BASALT_OPENING) / (1.0 - FLOOD_BASALT_OPENING),
            0.0,
            1.0,
        ) * (1.0 - smoothstep(boundary.distance / FLOOD_BASALT_REACH))
    } else {
        0.0
    };

    let rock = if near.buoyancy < OCEANIC_BUOYANCY || flooding > 0.0 {
        Rock::Basalt
    } else if belt > 0.5 {
        if roll < 0.35 {
            Rock::Granite
        } else {
            Rock::Metamorphic
        }
    } else if belt > 0.15 {
        pick(
            roll,
            &[
                (0.2, Rock::Granite),
                (0.55, Rock::Sandstone),
                (0.8, Rock::Shale),
            ],
            Rock::Limestone,
        )
    } else if near.buoyancy > 0.72 && near.age > 0.55 {
        if roll < 0.8 {
            Rock::Shield
        } else {
            Rock::Granite
        }
    } else {
        pick(
            roll,
            &[
                (0.32, Rock::Limestone),
                (0.56, Rock::Sandstone),
                (0.8, Rock::Shale),
                (0.9, Rock::Chalk),
            ],
            Rock::Granite,
        )
    };
    Province {
        site,
        rock,
        volcanism: flooding,
    }
}

/// The first rock whose cumulative share `roll` falls under, else `rest`.
fn pick(roll: f64, shares: &[(f64, Rock)], rest: Rock) -> Rock {
    shares
        .iter()
        .find(|&&(under, _)| roll < under)
        .map_or(rest, |&(_, rock)| rock)
}

/// What weathers out of a rock under a climate, as a share of each soil.
///
/// A soft partition rather than one label, so a soil boundary is a gradient
/// like a biome's: shares are non-negative and sum to one.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Soils {
    /// River-laid silt and sand along a floodplain.
    pub alluvium: f64,
    /// Wind-blown silt on the dry margins of the temperate belt.
    pub loess: f64,
    /// Iron-red, deeply weathered tropical earth.
    pub laterite: f64,
    /// Ashen, acid soil under cold forest and heath.
    pub podzol: f64,
    /// Deep black grassland earth.
    pub chernozem: f64,
    /// The brown forest earth of the humid temperate belt.
    pub brown_earth: f64,
    /// Crusted, salted desert ground.
    pub desert_crust: f64,
}

/// What a soil forms from.
#[derive(Copy, Clone, Debug)]
pub struct SoilSite {
    /// The parent rock.
    pub rock: Rock,
    /// Mean annual temperature, in degrees Celsius.
    pub celsius: f64,
    /// Effective moisture: precipitation over what the warmth evaporates.
    pub moisture: f64,
    /// `0.0` away from any river through `1.0` on a floodplain.
    pub alluvial: f64,
}

/// The soils a site weathers into.
#[must_use]
pub fn soils(site: SoilSite) -> Soils {
    let arid = 1.0 - rise(site.moisture, 0.35, 0.3);
    let tropical = rise(site.celsius, 20.0, 6.0);
    let cool = 1.0 - rise(site.celsius, 7.0, 6.0);
    let leaching = if site.rock.is_acidic() { 1.0 } else { 0.35 };
    // Grassland earth wants the semi-humid band; wetter or drier it is
    // forest earth or loess.
    let prairie = rise(site.moisture, 0.6, 0.3) * (1.0 - rise(site.moisture, 1.25, 0.4));

    let alluvium = mathf::clamp(site.alluvial, 0.0, 1.0);
    let rest = 1.0 - alluvium;
    let desert_crust = rest * arid;
    let humid = rest * (1.0 - arid);
    let laterite = humid * tropical * rise(site.moisture, 0.8, 0.4);
    let temperate = humid - laterite;
    let podzol = temperate * cool * leaching;
    let grass = (temperate - podzol) * prairie * (1.0 - tropical);
    let chernozem = grass * rise(site.moisture, 0.85, 0.3);
    let loess = grass - chernozem;
    let brown_earth = temperate - podzol - grass;
    Soils {
        alluvium,
        loess,
        laterite,
        podzol,
        chernozem,
        brown_earth,
        desert_crust,
    }
}

#[cfg(test)]
mod tests;
