//! Biomes: the living zones, and the classification that blends them.
//!
//! A biome is what lives in a place — boreal forest, savanna, bog — and not
//! what the ground under it looks like, which is [`crate::ground`]'s. A cell
//! has a normalised blend of biomes rather than one, so a boundary is a
//! gradient: flora and decoration read the blend, and the ground each biome
//! grows on is weighed from it.
//!
//! # A classification that is total by construction
//!
//! The climate classification is a soft decision tree over warm-season
//! temperature, cold-season temperature, effective moisture and rain
//! season. Every split is a partition of unity — a smooth threshold and its
//! complement — so the weights reaching the leaves sum to exactly one at
//! every point of the domain, and there is no climate for which nothing
//! grows. Terrain overrides — a rift, fresh lava, soft rock gullied into
//! badlands, a poorly drained flat, a shore — then each take a share of that
//! partition for their own biomes, which preserves the sum.
//!
//! The thresholds are the climatologists' where one exists: the 0 °C and
//! 10 °C warm-season isotherms of the snowline and the treeline, and Köppen
//! and Geiger's aridity threshold, `20 · (T + 7 + 7s)` millimetres for a
//! mean temperature `T` and a rain season `s`, against which a place's
//! rain is read.

use crate::blend::{Blend, Kind};
use crate::climate::LAPSE_RATE;
use crate::geology::Lithology;
use crate::geom::rise;

/// A living zone.
///
/// The discriminants are the identifiers a digest and a stored edit carry,
/// so they are frozen: a new biome takes a new number and never reuses a
/// retired one.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(u8)]
pub enum Biome {
    /// Sea, lake and river.
    OpenWater = 0,
    /// Permanent ice: an ice sheet, or a glacier above the snowline.
    IceSheet = 1,
    /// Frost-shattered ground too cold and too dry for more than lichen.
    PolarDesert = 2,
    /// Treeless ground beyond the polar treeline.
    Tundra = 3,
    /// Alpine tundra and meadow: treeless ground above a mountain's treeline.
    AlpineTundra = 4,
    /// Spruce, pine and larch under long winters.
    BorealForest = 5,
    /// Temperate pine and fir.
    TemperateConiferForest = 6,
    /// Temperate oak, beech and maple.
    TemperateBroadleafForest = 7,
    /// Mossy temperate rainforest on a mild, wet coast.
    TemperateRainforest = 8,
    /// Mediterranean woodland and scrub, green in the wet winter.
    MediterraneanWoodland = 9,
    /// Prairie and steppe.
    TemperateGrassland = 10,
    /// Desert under cold winters.
    ColdDesert = 11,
    /// Desert under hot sun.
    HotDesert = 12,
    /// Scrub on the desert margins.
    XericShrubland = 13,
    /// Tropical grassland under scattered trees, dry half the year.
    Savanna = 14,
    /// Tropical forest that sheds its leaves in the dry season.
    TropicalDryForest = 15,
    /// Evergreen tropical rainforest.
    TropicalRainforest = 16,
    /// Mangrove on a tropical tidal shore.
    Mangrove = 17,
    /// Forest standing in fresh water.
    SwampForest = 18,
    /// Reed and sedge over wet mineral ground, fresh or salt.
    Marsh = 19,
    /// Rain-fed acid peat.
    Bog = 20,
    /// Groundwater-fed peat.
    Fen = 21,
    /// Heath and moor on poor, wet, windswept ground.
    HeathMoor = 22,
    /// Beach and dune.
    BeachDune = 23,
    /// A shore of rock, too steep or too hard for a beach.
    RockyCoast = 24,
    /// Fresh lava nothing has colonised yet.
    VolcanicBarren = 25,
    /// Soft rock rain has gullied into a maze.
    Badlands = 26,
    /// Ground the world was torn through, which the realm's story turns on.
    RiftWaste = 27,
}

/// How many biomes there are.
pub const BIOME_COUNT: usize = 28;

impl Kind for Biome {
    const ALL: &'static [Self] = &[
        Self::OpenWater,
        Self::IceSheet,
        Self::PolarDesert,
        Self::Tundra,
        Self::AlpineTundra,
        Self::BorealForest,
        Self::TemperateConiferForest,
        Self::TemperateBroadleafForest,
        Self::TemperateRainforest,
        Self::MediterraneanWoodland,
        Self::TemperateGrassland,
        Self::ColdDesert,
        Self::HotDesert,
        Self::XericShrubland,
        Self::Savanna,
        Self::TropicalDryForest,
        Self::TropicalRainforest,
        Self::Mangrove,
        Self::SwampForest,
        Self::Marsh,
        Self::Bog,
        Self::Fen,
        Self::HeathMoor,
        Self::BeachDune,
        Self::RockyCoast,
        Self::VolcanicBarren,
        Self::Badlands,
        Self::RiftWaste,
    ];

    fn id(self) -> u8 {
        self as u8
    }
}

const _: () = assert!(Biome::ALL.len() == BIOME_COUNT);

/// Which water a shore faces, ordered so that where two waters are equally
/// near, the later one is the shore's.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Water {
    /// A river or stream, whose bank is no coast.
    Running,
    /// A lake.
    Lake,
    /// The sea.
    Sea,
}

/// Everything the classification reads about one cell of dry ground.
#[derive(Copy, Clone, Debug)]
pub struct Conditions {
    /// Mean annual air temperature, in degrees Celsius.
    pub celsius: f64,
    /// Warm season minus cold season, in degrees.
    pub range: f64,
    /// `0.0` on a coast through `1.0` deep in a continent's interior.
    pub continentality: f64,
    /// Annual precipitation, in millimetres.
    pub precipitation: f64,
    /// `-1.0` winter-wet through `1.0` summer-wet.
    pub rain_season: f64,
    /// `0.0` well drained through `1.0` where water gathers and stays.
    pub wetness: f64,
    /// Height above sea level, in world units.
    pub elevation_units: f64,
    /// Steepest local gradient, as a rise over one cell.
    pub slope: f64,
    /// How strongly the cell sits in a rift where plates are pulling apart,
    /// `0.0..1.0`.
    pub rift: f64,
    /// The rock beneath.
    pub lithology: Lithology,
    /// Cells to the nearest water, and which water that is. Past
    /// [`SHORE_REACH`] a shore makes no difference, so a caller need resolve
    /// no further.
    pub shore: (u16, Water),
}

impl Conditions {
    /// Warm-season temperature.
    #[must_use]
    pub fn warm(&self) -> f64 {
        self.celsius + self.range / 2.0
    }

    /// Cold-season temperature.
    #[must_use]
    pub fn cold(&self) -> f64 {
        self.celsius - self.range / 2.0
    }

    /// Effective moisture: annual precipitation over the Köppen–Geiger
    /// aridity threshold for this temperature and rain season. Below about
    /// 0.45 is desert, from there to one steppe, and above one the land is
    /// humid.
    #[must_use]
    pub fn moisture(&self) -> f64 {
        let threshold = 20.0 * (self.celsius + 7.0 + 7.0 * self.rain_season);
        self.precipitation / threshold.max(ARIDITY_FLOOR_MILLIMETRES)
    }
}

/// The least the aridity threshold is ever taken to be, in millimetres, so
/// a polar place is not humid on no rain at all merely because its cold
/// evaporates nothing.
const ARIDITY_FLOOR_MILLIMETRES: f64 = 120.0;

/// Warm-season temperature below which snow outlasts the summer.
pub const SNOWLINE_CELSIUS: f64 = 0.0;

/// Warm-season temperature below which no tree grows.
pub const TREELINE_CELSIUS: f64 = 10.0;

/// Cells from a lake or the sea within which a shore is a coast. The scatter
/// halo resolves shore distance exactly this far beyond a chunk, which is
/// what keeps a coast the same from either side of a seam.
pub const SHORE_REACH: u16 = 4;

/// Precipitation below which ice cannot build, in millimetres a year.
const ICE_ACCUMULATION: f64 = 120.0;

/// Precipitation below which ground beyond the treeline is polar desert.
const POLAR_DRYNESS: f64 = 180.0;

/// Classify one cell of dry ground.
#[must_use]
pub fn classify(site: &Conditions) -> Blend<Biome> {
    let mut raw = [0.0_f64; BIOME_COUNT];
    climate(site, &mut raw);
    terrain(site, &mut raw);
    Blend::normalise(&raw, Biome::PolarDesert)
}

/// The climate partition: weights that sum to one over the land biomes.
fn climate(site: &Conditions, raw: &mut [f64; BIOME_COUNT]) {
    let warm = site.warm();
    let above_snowline = rise(warm, SNOWLINE_CELSIUS, 3.0);
    let above_treeline = rise(warm, TREELINE_CELSIUS, 3.0);

    let frost = 1.0 - above_snowline;
    let snowy = rise(site.precipitation, ICE_ACCUMULATION, 80.0);
    add(raw, Biome::IceSheet, frost * snowy);
    add(raw, Biome::PolarDesert, frost * (1.0 - snowy));

    let treeless = above_snowline - above_treeline;
    let barren = 1.0 - rise(site.precipitation, POLAR_DRYNESS, 100.0);
    // Beyond an oceanic treeline, where summers still run mild, the ground
    // is heath and moor rather than tundra.
    let moor = oceanic(site) * rise(warm, 5.0, 4.0);
    // The cold is the altitude's where the same place at sea level would
    // grow trees.
    let sea_level_warm = warm + LAPSE_RATE * site.elevation_units.max(0.0);
    let alpine = rise(sea_level_warm, TREELINE_CELSIUS, 3.0);
    let open = treeless * (1.0 - barren);
    add(raw, Biome::PolarDesert, treeless * barren);
    add(raw, Biome::HeathMoor, open * moor);
    add(raw, Biome::AlpineTundra, open * (1.0 - moor) * alpine);
    add(raw, Biome::Tundra, open * (1.0 - moor) * (1.0 - alpine));

    wooded(site, above_treeline, raw);
}

/// Where trees could grow: the boreal, temperate and tropical belts.
fn wooded(site: &Conditions, share: f64, raw: &mut [f64; BIOME_COUNT]) {
    let (warm, cold) = (site.warm(), site.cold());
    let severe_winter = 1.0 - rise(cold, -6.0, 8.0);
    let short_summer = 1.0 - rise(warm, 19.0, 4.0);
    let boreal = share * severe_winter * short_summer;
    let tropical = share * rise(cold, 15.0, 6.0);
    let bands = Bands::of(site);

    add(raw, Biome::ColdDesert, boreal * bands.arid);
    add(raw, Biome::TemperateGrassland, boreal * bands.semiarid);
    add(
        raw,
        Biome::BorealForest,
        boreal * (bands.humid + bands.perhumid),
    );
    temperate_belt(site, &bands, share - boreal - tropical, raw);
    tropical_belt(&bands, tropical, raw);
}

/// A climate's moisture bands and rain seasons, each set a partition.
struct Bands {
    moisture: f64,
    arid: f64,
    semiarid: f64,
    humid: f64,
    perhumid: f64,
    winter_wet: f64,
    even: f64,
}

impl Bands {
    fn of(site: &Conditions) -> Self {
        let moisture = site.moisture();
        let semi = rise(moisture, 0.45, 0.2);
        let humid_edge = rise(moisture, 1.0, 0.3);
        let perhumid = rise(moisture, 3.4, 1.0);
        let winter_wet = 1.0 - rise(site.rain_season, -0.3, 0.3);
        let summer_wet = rise(site.rain_season, 0.3, 0.3);
        Self {
            moisture,
            arid: 1.0 - semi,
            semiarid: semi - humid_edge,
            humid: humid_edge - perhumid,
            perhumid,
            winter_wet,
            even: 1.0 - winter_wet - summer_wet,
        }
    }
}

/// The temperate belt's `share`: deserts, Mediterranean woodland, steppe,
/// prairie, heath, the broadleaf and conifer forests and the temperate
/// rainforest.
fn temperate_belt(site: &Conditions, bands: &Bands, share: f64, raw: &mut [f64; BIOME_COUNT]) {
    let mean = site.celsius;
    let oceanic = oceanic(site);
    let hot = rise(mean, 17.0, 4.0);

    add(raw, Biome::HotDesert, share * bands.arid * hot);
    add(raw, Biome::ColdDesert, share * bands.arid * (1.0 - hot));

    let dry_mediterranean = bands.winter_wet * rise(mean, 9.0, 4.0);
    add(
        raw,
        Biome::MediterraneanWoodland,
        share * bands.semiarid * dry_mediterranean,
    );
    let steppe = share * bands.semiarid * (1.0 - dry_mediterranean);
    add(raw, Biome::XericShrubland, steppe * hot);
    add(raw, Biome::TemperateGrassland, steppe * (1.0 - hot));

    // Heath and moor take the cool end of the oceanic belt, where summers
    // barely clear the treeline — windswept, leached ground — most readily
    // where the rock weathers poor.
    let exposure = if site.lithology.rock.is_acidic() {
        0.85
    } else {
        0.5
    };
    let heath = oceanic * (1.0 - rise(site.warm(), 13.5, 3.0)) * exposure;
    // Broadleaves take the warmer, richer ground; conifers the rest.
    let broadleaf = rise(mean, 8.0, 4.0)
        * if site.lithology.rock.is_acidic() {
            0.6
        } else {
            1.0
        };

    let wet_mediterranean = bands.winter_wet * rise(mean, 12.0, 4.0);
    add(
        raw,
        Biome::MediterraneanWoodland,
        share * bands.humid * wet_mediterranean,
    );
    let woods = share * bands.humid * (1.0 - wet_mediterranean);
    add(raw, Biome::HeathMoor, woods * heath);
    let forest = woods * (1.0 - heath);
    // Tall-grass prairie holds the drier continental edge of the humid belt,
    // which Köppen's threshold calls humid but trees do not.
    let prairie = (1.0 - oceanic) * (1.0 - rise(bands.moisture, 1.6, 0.5));
    add(raw, Biome::TemperateGrassland, forest * prairie);
    let trees = forest * (1.0 - prairie);
    add(raw, Biome::TemperateBroadleafForest, trees * broadleaf);
    add(
        raw,
        Biome::TemperateConiferForest,
        trees * (1.0 - broadleaf),
    );

    let sodden = share * bands.perhumid;
    add(raw, Biome::HeathMoor, sodden * heath);
    let mild = sodden * (1.0 - heath);
    let rainforest = oceanic * rise(mean, 6.0, 4.0) * (1.0 - rise(mean, 16.0, 4.0));
    add(raw, Biome::TemperateRainforest, mild * rainforest);
    let wet_trees = mild * (1.0 - rainforest);
    add(raw, Biome::TemperateBroadleafForest, wet_trees * broadleaf);
    add(
        raw,
        Biome::TemperateConiferForest,
        wet_trees * (1.0 - broadleaf),
    );
}

/// The tropical belt's `share`: hot desert, shrubland, savanna, and the dry
/// and wet forests the length of the dry season divides.
fn tropical_belt(bands: &Bands, share: f64, raw: &mut [f64; BIOME_COUNT]) {
    add(raw, Biome::HotDesert, share * bands.arid);
    let grassy = rise(bands.moisture, 0.75, 0.25);
    add(
        raw,
        Biome::XericShrubland,
        share * bands.semiarid * (1.0 - grassy),
    );
    add(raw, Biome::Savanna, share * bands.semiarid * grassy);
    let dry_season = 1.0 - bands.even;
    let wetter = rise(bands.moisture, 1.6, 0.4);
    add(
        raw,
        Biome::Savanna,
        share * bands.humid * dry_season * (1.0 - wetter),
    );
    add(
        raw,
        Biome::TropicalDryForest,
        share * bands.humid * (dry_season * wetter + bands.even * (1.0 - wetter)),
    );
    add(
        raw,
        Biome::TropicalRainforest,
        share * bands.humid * bands.even * wetter,
    );
    add(
        raw,
        Biome::TropicalDryForest,
        share * bands.perhumid * dry_season * 0.5,
    );
    add(
        raw,
        Biome::TropicalRainforest,
        share * bands.perhumid * (1.0 - dry_season * 0.5),
    );
}

/// The terrain overrides, each taking a share of the partition it is given
/// for its own biomes, so the sum is kept.
fn terrain(site: &Conditions, raw: &mut [f64; BIOME_COUNT]) {
    let thawed = rise(site.warm(), SNOWLINE_CELSIUS, 3.0);

    // A rift's floor is the sea's; the low land along its margins is where
    // the world was torn through.
    let torn = rise(site.rift, 0.15, 0.04) * (1.0 - rise(site.elevation_units, 80.0, 60.0));
    take(raw, 0.85 * torn, &[(Biome::RiftWaste, 1.0)]);

    take(
        raw,
        0.85 * site.lithology.volcanism,
        &[(Biome::VolcanicBarren, 1.0)],
    );

    let moisture = site.moisture();
    let gullied = rise(site.slope, 0.6, 0.4) * (1.0 - rise(site.slope, 2.6, 1.0));
    let badlands =
        (1.0 - rise(moisture, 0.7, 0.3)) * site.lithology.rock.softness() * gullied * thawed;
    take(raw, 0.9 * badlands, &[(Biome::Badlands, 1.0)]);

    let celsius = site.celsius;
    let sodden = rise(site.wetness, 0.62, 0.25) * rise(moisture, 0.6, 0.4) * thawed;
    let swamp = rise(celsius, 16.0, 4.0);
    let peat = 1.0 - rise(celsius, 8.0, 4.0);
    let marsh = 1.0 - swamp - peat;
    let base_fen = if site.lithology.rock.is_calcareous() {
        0.8
    } else if site.lithology.rock.is_acidic() {
        0.2
    } else {
        0.5
    };
    // A flat deep in a catchment is fed from below; one high in it only by
    // the rain.
    let fen = (base_fen + 0.4 * rise(site.wetness, 0.88, 0.15)).min(1.0);
    take(
        raw,
        0.9 * sodden,
        &[
            (Biome::SwampForest, swamp),
            (Biome::Marsh, marsh),
            (Biome::Bog, peat * (1.0 - fen)),
            (Biome::Fen, peat * fen),
        ],
    );

    let (distance, water) = site.shore;
    let near = if water == Water::Running {
        0.0
    } else {
        1.0 - rise(f64::from(distance), 2.5, 2.0)
    };
    let coast = near * thawed;
    // A shore too steep to walk up from the water is a cliff; hard rock
    // tilts a gentle one toward rock without making it one.
    let steep = rise(site.slope, 1.4, 1.0);
    let rocky = 1.0
        - (1.0 - steep)
            * if site.lithology.rock.is_hard() {
                0.75
            } else {
                1.0
            };
    let flat = 1.0 - rocky;
    let mangrove = if water == Water::Sea {
        rise(site.cold(), 16.0, 4.0)
    } else {
        0.0
    };
    let tidal = (1.0 - mangrove) * rise(site.wetness, 0.5, 0.3);
    take(
        raw,
        0.9 * coast,
        &[
            (Biome::RockyCoast, rocky),
            (Biome::Mangrove, flat * mangrove),
            (Biome::Marsh, flat * tidal),
            (Biome::BeachDune, flat * (1.0 - mangrove - tidal)),
        ],
    );
}

/// How oceanic a climate is: `1.0` on a coast the sea tempers, `0.0` deep
/// enough inland that the continent's own seasons rule.
fn oceanic(site: &Conditions) -> f64 {
    1.0 - rise(site.continentality, 0.25, 0.2)
}

fn add(raw: &mut [f64; BIOME_COUNT], biome: Biome, weight: f64) {
    raw[biome as usize] += weight;
}

/// Hand `share` of everything to `targets`, whose weights sum to one.
fn take(raw: &mut [f64; BIOME_COUNT], share: f64, targets: &[(Biome, f64)]) {
    let share = share.clamp(0.0, 1.0);
    if share <= 0.0 {
        return;
    }
    let keep = 1.0 - share;
    for weight in raw.iter_mut() {
        *weight *= keep;
    }
    for &(biome, weight) in targets {
        raw[biome as usize] += share * weight;
    }
}

#[cfg(test)]
mod tests;
