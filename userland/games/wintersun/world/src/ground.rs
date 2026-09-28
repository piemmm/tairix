//! Grounds: the surfaces the splat draws, and what each biome grows on.
//!
//! A biome says what lives in a place; a ground says what the eye sees
//! underfoot — needle litter, dry grass, dune sand, bare granite. Every
//! biome is a weighted palette of grounds, modulated by moisture, drainage,
//! soil, rock and a patch field, so a forest floor is litter with moss in
//! its damp hollows and a desert is dune sand in one place and gravel plain
//! in the next. The ground blend a cell stores is its biomes' palettes
//! weighed by the biome blend, with steep ground given over to its bare rock
//! and scree whatever grows around it.
//!
//! Rock shows through as the ground of its own class, so a limestone
//! escarpment and a granite tor read differently, and a beach's sand is the
//! colour of the rock it was ground from.

use tairix_util::mathf;

use crate::biome::Biome;
use crate::blend::{Blend, Kind};
use crate::geology::{Rock, Soils};
use crate::geom::rise;

/// A surface the splat draws.
///
/// The discriminants are the identifiers a stored world edit carries, so
/// they are frozen: a new ground takes a new number and never reuses a
/// retired one.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(u8)]
pub enum Ground {
    /// Open water.
    Water = 0,
    /// Glacier and sheet ice.
    Ice = 1,
    /// Lying snow.
    Snow = 2,
    /// Lichen over stones.
    Lichen = 3,
    /// Moss.
    Moss = 4,
    /// Conifer needles.
    NeedleLitter = 5,
    /// Fallen broadleaves.
    LeafLitter = 6,
    /// Dark forest earth.
    ForestLoam = 7,
    /// Rainforest floor.
    RainforestFloor = 8,
    /// Short, grazed grass.
    ShortGrass = 9,
    /// Lush, well-watered grass.
    LushGrass = 10,
    /// Dry, bleached grass.
    DryGrass = 11,
    /// Tall grass and reed.
    TallGrass = 12,
    /// Flowering meadow.
    Meadow = 13,
    /// Heather and ling.
    Heath = 14,
    /// Peat.
    Peat = 15,
    /// Mud.
    Mud = 16,
    /// Pale sand ground from shell and lime.
    WhiteSand = 17,
    /// Golden quartz sand.
    GoldenSand = 18,
    /// Iron-red sand.
    RedSand = 19,
    /// Black volcanic sand.
    BlackSand = 20,
    /// Wind-heaped dune sand.
    DuneSand = 21,
    /// Gravel.
    Gravel = 22,
    /// Rounded beach shingle.
    Shingle = 23,
    /// Scree under a crag.
    Scree = 24,
    /// Cracked clay crust.
    ClayCrust = 25,
    /// A dried salt pan.
    SaltPan = 26,
    /// Red laterite earth.
    Laterite = 27,
    /// Volcanic ash.
    Ash = 28,
    /// Fresh, cooled lava.
    CooledLava = 29,
    /// Crystalline shield rock.
    ShieldRock = 30,
    /// Granite.
    Granite = 31,
    /// Basalt.
    Basalt = 32,
    /// Limestone.
    Limestone = 33,
    /// Sandstone.
    Sandstone = 34,
    /// Shale.
    Shale = 35,
    /// Chalk.
    Chalk = 36,
    /// Schist and gneiss.
    Schist = 37,
    /// Ground the world was torn through.
    RiftGround = 38,
}

/// How many grounds there are.
pub const GROUND_COUNT: usize = 39;

impl Kind for Ground {
    const ALL: &'static [Self] = &[
        Self::Water,
        Self::Ice,
        Self::Snow,
        Self::Lichen,
        Self::Moss,
        Self::NeedleLitter,
        Self::LeafLitter,
        Self::ForestLoam,
        Self::RainforestFloor,
        Self::ShortGrass,
        Self::LushGrass,
        Self::DryGrass,
        Self::TallGrass,
        Self::Meadow,
        Self::Heath,
        Self::Peat,
        Self::Mud,
        Self::WhiteSand,
        Self::GoldenSand,
        Self::RedSand,
        Self::BlackSand,
        Self::DuneSand,
        Self::Gravel,
        Self::Shingle,
        Self::Scree,
        Self::ClayCrust,
        Self::SaltPan,
        Self::Laterite,
        Self::Ash,
        Self::CooledLava,
        Self::ShieldRock,
        Self::Granite,
        Self::Basalt,
        Self::Limestone,
        Self::Sandstone,
        Self::Shale,
        Self::Chalk,
        Self::Schist,
        Self::RiftGround,
    ];

    fn id(self) -> u8 {
        self as u8
    }
}

const _: () = assert!(Ground::ALL.len() == GROUND_COUNT);

impl Ground {
    /// The bare face of a rock class.
    #[must_use]
    pub const fn of_rock(rock: Rock) -> Self {
        match rock {
            Rock::Shield => Self::ShieldRock,
            Rock::Granite => Self::Granite,
            Rock::Basalt => Self::Basalt,
            Rock::Limestone => Self::Limestone,
            Rock::Sandstone => Self::Sandstone,
            Rock::Shale => Self::Shale,
            Rock::Chalk => Self::Chalk,
            Rock::Metamorphic => Self::Schist,
        }
    }
}

/// Everything the palette reads about one cell of dry ground besides its
/// biomes.
#[derive(Copy, Clone, Debug)]
pub struct GroundSite {
    /// Mean annual temperature, in degrees Celsius.
    pub celsius: f64,
    /// Warm-season temperature.
    pub warm: f64,
    /// Effective moisture, as the classification reads it.
    pub moisture: f64,
    /// `0.0` well drained through `1.0` where water gathers.
    pub wetness: f64,
    /// Steepest local gradient, as a rise over one cell.
    pub slope: f64,
    /// The rock beneath.
    pub rock: Rock,
    /// What weathered out of it.
    pub soils: Soils,
    /// `0.0..=1.0`: a low-frequency patch field, so one biome's grounds
    /// cluster rather than speckle.
    pub patch: f64,
}

/// Slope, as a rise over one cell, at which bare rock wholly replaces
/// whatever would otherwise grow.
const ROCK_SLOPE: f64 = 2.6;

/// Most grounds any one biome's palette weighs.
const PALETTE_SLOTS: usize = 6;

/// The ground blend of a dry cell whose biomes are `biomes`.
#[must_use]
pub fn cover(biomes: &Blend<Biome>, site: &GroundSite) -> Blend<Ground> {
    let mut raw = [0.0_f64; GROUND_COUNT];
    for (biome, weight) in biomes.slots() {
        let palette = palette(biome, site);
        let total: f64 = palette.iter().map(|&(_, share)| share).sum();
        if total <= 0.0 {
            continue;
        }
        let scale = f64::from(weight) / total;
        for &(ground, share) in &palette {
            raw[ground as usize] += share * scale;
        }
    }

    // A face is rock before it is anything that grows, and scree gathers
    // below it.
    let steep = mathf::clamp(site.slope / ROCK_SLOPE, 0.0, 1.0);
    let bare = steep * steep;
    let scree = 0.5 * steep * (1.0 - steep);
    let keep = 1.0 - bare - scree;
    for weight in &mut raw {
        *weight *= keep;
    }
    let total = f64::from(crate::blend::WEIGHT_TOTAL);
    raw[Ground::of_rock(site.rock) as usize] += bare * total;
    raw[Ground::Scree as usize] += scree * total;

    Blend::normalise(&raw, Ground::of_rock(site.rock))
}

/// The sand a shore or a desert of this rock is made of.
fn sand(site: &GroundSite) -> Ground {
    match site.rock {
        Rock::Limestone | Rock::Chalk => Ground::WhiteSand,
        Rock::Basalt => Ground::BlackSand,
        Rock::Sandstone if site.celsius > 18.0 => Ground::RedSand,
        _ => Ground::GoldenSand,
    }
}

/// A biome's grounds and their relative shares at `site`.
///
/// Shares need not sum to anything: [`cover`] normalises each palette
/// before weighing it by its biome.
#[allow(
    clippy::too_many_lines,
    reason = "one palette per biome: a table, which splitting would scatter"
)]
fn palette(biome: Biome, site: &GroundSite) -> [(Ground, f64); PALETTE_SLOTS] {
    let rock = Ground::of_rock(site.rock);
    let (p, w) = (site.patch, site.wetness);
    let dry = 1.0 - rise(site.moisture, 1.0, 0.8);
    let soils = site.soils;
    let river = soils.alluvium;
    let nothing = (Ground::Water, 0.0);
    match biome {
        Biome::OpenWater => [
            (Ground::Water, 1.0),
            nothing,
            nothing,
            nothing,
            nothing,
            nothing,
        ],
        Biome::IceSheet => [
            (Ground::Ice, 0.6 + 0.3 * (1.0 - p)),
            (Ground::Snow, 0.25 + 0.3 * p),
            nothing,
            nothing,
            nothing,
            nothing,
        ],
        Biome::PolarDesert => [
            (Ground::Gravel, 0.45),
            (Ground::Lichen, 0.15 + 0.2 * p),
            (rock, 0.2),
            (Ground::Snow, 0.35 * (1.0 - rise(site.warm, 2.0, 4.0))),
            nothing,
            nothing,
        ],
        Biome::Tundra => [
            (Ground::Lichen, 0.25 + 0.25 * (1.0 - w)),
            (Ground::Moss, 0.15 + 0.4 * w),
            (Ground::ShortGrass, 0.2 + 0.2 * p),
            (Ground::Peat, 0.35 * w * w),
            (Ground::Gravel, 0.25 * dry),
            nothing,
        ],
        Biome::AlpineTundra => [
            (Ground::Meadow, 0.5 * rise(site.warm, 5.0, 4.0)),
            (Ground::ShortGrass, 0.25),
            (
                Ground::Lichen,
                0.15 + 0.25 * (1.0 - rise(site.warm, 5.0, 4.0)),
            ),
            (Ground::Scree, 0.2),
            (Ground::Snow, 0.6 * (1.0 - rise(site.warm, 2.0, 3.0))),
            nothing,
        ],
        Biome::BorealForest => [
            (Ground::NeedleLitter, 0.55),
            (Ground::Moss, 0.15 + 0.3 * w),
            (Ground::Lichen, 0.05 + 0.35 * soils.podzol * (1.0 - w)),
            (Ground::Peat, 0.25 * w * w),
            (Ground::LushGrass, 0.3 * river),
            nothing,
        ],
        Biome::TemperateConiferForest => [
            (Ground::NeedleLitter, 0.6),
            (Ground::ForestLoam, 0.1 + 0.3 * soils.brown_earth),
            (Ground::Moss, 0.1 + 0.25 * w),
            (Ground::LushGrass, 0.3 * river),
            nothing,
            nothing,
        ],
        Biome::TemperateBroadleafForest => [
            (Ground::LeafLitter, 0.55),
            (Ground::ForestLoam, 0.15 + 0.25 * soils.brown_earth),
            (Ground::LushGrass, 0.08 + 0.2 * p + 0.3 * river),
            (Ground::Moss, 0.15 * w),
            nothing,
            nothing,
        ],
        Biome::TemperateRainforest => [
            (Ground::Moss, 0.45),
            (Ground::ForestLoam, 0.2),
            (Ground::LeafLitter, 0.15 + 0.1 * p),
            (Ground::NeedleLitter, 0.15),
            nothing,
            nothing,
        ],
        Biome::MediterraneanWoodland => [
            (Ground::DryGrass, 0.35),
            (Ground::ShortGrass, 0.12 + 0.15 * p),
            (Ground::LeafLitter, 0.2),
            (rock, 0.15),
            (Ground::ClayCrust, 0.2 * soils.desert_crust),
            (Ground::LushGrass, 0.3 * river),
        ],
        Biome::TemperateGrassland => [
            (
                Ground::TallGrass,
                0.15 + 0.6 * soils.chernozem + 0.2 * soils.brown_earth,
            ),
            (Ground::ShortGrass, 0.3),
            (
                Ground::DryGrass,
                0.1 + f64::midpoint(soils.loess, soils.desert_crust),
            ),
            (Ground::Meadow, 0.15 * p),
            (Ground::LushGrass, 0.3 * river),
            nothing,
        ],
        Biome::ColdDesert => [
            (Ground::Gravel, 0.3),
            (Ground::DryGrass, 0.15 + 0.1 * p),
            (Ground::ClayCrust, 0.1 + 0.2 * soils.desert_crust),
            (sand(site), 0.1 + 0.25 * p),
            (rock, 0.15),
            (Ground::SaltPan, 0.4 * w * w),
        ],
        Biome::HotDesert => [
            (sand(site), 0.3),
            (Ground::DuneSand, 0.45 * p * p),
            (Ground::Gravel, 0.25 * (1.0 - p)),
            (Ground::ClayCrust, 0.1),
            (Ground::SaltPan, 0.45 * w * w),
            (rock, 0.08),
        ],
        Biome::XericShrubland => [
            (Ground::DryGrass, 0.35),
            (sand(site), 0.12 + 0.1 * p),
            (Ground::ClayCrust, 0.2),
            (Ground::Gravel, 0.15),
            (Ground::Laterite, 0.35 * soils.laterite),
            nothing,
        ],
        Biome::Savanna => [
            (Ground::DryGrass, 0.45),
            (Ground::TallGrass, 0.12 + 0.25 * (1.0 - dry)),
            (Ground::Laterite, 0.12 + 0.3 * soils.laterite),
            (Ground::LushGrass, 0.3 * river),
            nothing,
            nothing,
        ],
        Biome::TropicalDryForest => [
            (Ground::LeafLitter, 0.45),
            (Ground::DryGrass, 0.2),
            (Ground::Laterite, 0.12 + 0.3 * soils.laterite),
            (Ground::LushGrass, 0.3 * river),
            nothing,
            nothing,
        ],
        Biome::TropicalRainforest => [
            (Ground::RainforestFloor, 0.6),
            (Ground::LeafLitter, 0.2 + 0.1 * p),
            (Ground::Moss, 0.08 + 0.15 * w),
            (Ground::Mud, 0.25 * river),
            nothing,
            nothing,
        ],
        Biome::Mangrove => [
            (Ground::Mud, 0.6),
            (Ground::LeafLitter, 0.25),
            (Ground::Peat, 0.15),
            nothing,
            nothing,
            nothing,
        ],
        Biome::SwampForest => [
            (Ground::Mud, 0.35),
            (Ground::LeafLitter, 0.3),
            (Ground::Moss, 0.2),
            (Ground::Peat, 0.15),
            nothing,
            nothing,
        ],
        Biome::Marsh => [
            (Ground::TallGrass, 0.35),
            (Ground::Mud, 0.25 + 0.25 * w),
            (Ground::LushGrass, 0.3),
            nothing,
            nothing,
            nothing,
        ],
        Biome::Bog => [
            (Ground::Peat, 0.4),
            (Ground::Moss, 0.35),
            (Ground::Heath, 0.25),
            nothing,
            nothing,
            nothing,
        ],
        Biome::Fen => [
            (Ground::Peat, 0.3),
            (Ground::LushGrass, 0.3),
            (Ground::Meadow, 0.2),
            (Ground::Mud, 0.2),
            nothing,
            nothing,
        ],
        Biome::HeathMoor => [
            (Ground::Heath, 0.5),
            (Ground::Peat, 0.1 + 0.4 * w),
            (Ground::ShortGrass, 0.2),
            (rock, 0.1),
            nothing,
            nothing,
        ],
        Biome::BeachDune => {
            // A cold or a hard-rock shore is shingle; a warm soft one sand.
            let shingle = (1.0 - rise(site.celsius, 6.0, 8.0)).max(if site.rock.is_hard() {
                0.45
            } else {
                0.0
            });
            [
                (sand(site), 0.55 * (1.0 - shingle)),
                (Ground::DuneSand, 0.35 * (1.0 - shingle) * p),
                (Ground::Shingle, 0.1 + 0.7 * shingle),
                nothing,
                nothing,
                nothing,
            ]
        }
        Biome::RockyCoast => [
            (rock, 0.55),
            (Ground::Shingle, 0.25),
            (Ground::Gravel, 0.2),
            nothing,
            nothing,
            nothing,
        ],
        Biome::VolcanicBarren => [
            (Ground::CooledLava, 0.45),
            (Ground::Ash, 0.25),
            (Ground::Basalt, 0.15),
            (Ground::BlackSand, 0.15),
            nothing,
            nothing,
        ],
        Biome::Badlands => [
            (rock, 0.45),
            (Ground::ClayCrust, 0.35),
            (Ground::DryGrass, 0.2),
            nothing,
            nothing,
            nothing,
        ],
        Biome::RiftWaste => [
            (Ground::RiftGround, 0.7),
            (rock, 0.3),
            nothing,
            nothing,
            nothing,
            nothing,
        ],
    }
}

#[cfg(test)]
mod tests;
