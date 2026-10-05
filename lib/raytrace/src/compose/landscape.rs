//! Landscapes: rolling farmland, a forest glade, mountains over a lake, a
//! coast with the sea running in, dunes and gullied badlands, snow, a lagoon
//! at sunset, and canyons between mesas.
//!
//! Each stands on a land worn by the water that drains it (`crate::land`). A
//! setting plans its land and what its ground is made of; once the far land
//! stands, its scheme sites the eye on it and says where the near land is to
//! lie; once all of it stands, the scheme sets the scene out about the eye —
//! the lawn underfoot, trees where the ground suits them, stones — and the
//! weather over it.

use alloc::vec::Vec;
use core::f64::consts::{FRAC_PI_2, PI, TAU};

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use super::architecture::Aqueduct;
use super::plants::{self, Character, Fallen, Grassland, Grove, Kind, Stand, Tier};
use super::plants::{Dead, Drift};
use super::stones::{self, Brook, Stones};
use super::waterside;
use super::weather::{self, Climate, Cover, Hour, Outdoors};
use super::woodland::{Beneath, Deadfall, Rooting, Wood, Woodland, ANYWHERE};
use super::{direction, lumens, rgb, Composed, Dice, Landing, Look, Stage, View, GOLD};

use crate::channel::{Section, Station};
use crate::course::Mark;
use crate::grass::Seen;
use crate::ground::{Ground, Palette, Road, Rock};
use crate::heightfield::Heightfield;
use crate::land::{
    self, Build, Fields, Horizon, Laid, Land, Lie, NearWater, Nest, Plan, Rivers, Roadway, Surface,
    Survey, Wear, NESTS,
};
use crate::material::{Finish, Foam, Material, Relief, Wind};
use crate::noise::smoothstep;
use crate::pigment::Pigment;
use crate::rock::Lithology;
use crate::scene::Exposure;
use crate::shade::Rect;
use crate::shape::Shape;
use crate::terrain::{Landform, Sea, Terrain};
use crate::tree::Season;
use crate::vector::{real, Frame, Pose, Vec3};

/// The colours of a region's ground, as sRGB.
#[derive(Copy, Clone, Debug)]
pub(super) struct Soil {
    grass: u32,
    dry: u32,
    moss: u32,
    earth: u32,
    silt: u32,
    rock: u32,
    strata: u32,
    lichen: u32,
    sand: u32,
    snow: u32,
}

impl Soil {
    fn palette(&self) -> Palette {
        Palette {
            grass: rgb(self.grass),
            dry: rgb(self.dry),
            moss: rgb(self.moss),
            earth: rgb(self.earth),
            silt: rgb(self.silt),
            rock: rgb(self.rock),
            strata: rgb(self.strata),
            lichen: rgb(self.lichen),
            sand: rgb(self.sand),
            snow: rgb(self.snow),
        }
    }
}

pub(super) const GREEN: Soil = Soil {
    grass: 0x4E_74_2C,
    dry: 0x8A_8C_44,
    moss: 0x3C_58_22,
    earth: 0x6A_54_3A,
    silt: 0x9A_8A_6A,
    rock: 0x7A_76_70,
    strata: 0x5A_56_52,
    lichen: 0xA4_A6_84,
    sand: 0xD8_C8_A0,
    snow: 0xF4_F6_FA,
};
pub(super) const GOLDEN: Soil = Soil {
    grass: 0x8C_8A_40,
    dry: 0xB8_A2_5C,
    moss: 0x5E_6A_2C,
    earth: 0x8A_6A_44,
    silt: 0xB0_A0_80,
    rock: 0x9A_8A_74,
    strata: 0x7A_6A_56,
    lichen: 0xC0_B0_70,
    sand: 0xE0_CC_A0,
    snow: 0xF4_F6_FA,
};
const HIGHLAND: Soil = Soil {
    grass: 0x5A_6E_34,
    dry: 0x8A_80_4C,
    moss: 0x48_5C_26,
    earth: 0x5E_4E_3C,
    silt: 0x8A_80_6C,
    rock: 0x6E_6C_6A,
    strata: 0x4A_48_48,
    lichen: 0x9E_A0_7A,
    sand: 0xC8_BC_A0,
    snow: 0xF4_F6_FA,
};
const RED_ROCK: Soil = Soil {
    grass: 0x86_7C_4C,
    dry: 0xAC_8E_5C,
    moss: 0x6A_68_40,
    earth: 0xA0_5A_34,
    silt: 0xC0_90_70,
    rock: 0xB8_64_3A,
    strata: 0x8A_42_28,
    lichen: 0x7A_4A_34,
    sand: 0xD8_A0_6C,
    snow: 0xF4_F6_FA,
};
const SCRUB: Soil = Soil {
    grass: 0x94_88_5C,
    dry: 0xC4_AC_78,
    moss: 0x74_70_48,
    earth: 0xB0_7A_4A,
    silt: 0xC8_A0_7A,
    rock: 0xB0_78_50,
    strata: 0x8A_58_38,
    lichen: 0x6E_4E_3A,
    sand: 0xD8_BC_8C,
    snow: 0xF4_F6_FA,
};
const DUNE: Soil = Soil {
    grass: 0xD8_B0_78,
    dry: 0xE0_BC_84,
    moss: 0xA8_90_60,
    earth: 0xC8_9C_65,
    silt: 0xD8_B0_80,
    rock: 0xA8_7A_50,
    strata: 0x8A_60_40,
    lichen: 0xB0_98_70,
    sand: 0xE4_C0_88,
    snow: 0xF4_F6_FA,
};
const SNOWFIELD: Soil = Soil {
    grass: 0xE8_EC_F2,
    dry: 0xF0_F2_F6,
    moss: 0xD0_D8_E0,
    earth: 0xD8_DC_E4,
    silt: 0xE0_E4_EA,
    rock: 0x5A_5C_60,
    strata: 0x3A_3C_40,
    lichen: 0x8A_8C_80,
    sand: 0xE0_E4_EA,
    snow: 0xF6_F8_FC,
};
const VOLCANIC: Soil = Soil {
    grass: 0x3E_5A_2A,
    dry: 0x5A_62_34,
    moss: 0x34_50_2A,
    earth: 0x3A_32_2E,
    silt: 0x5A_50_4A,
    rock: 0x3A_38_38,
    strata: 0x24_22_24,
    lichen: 0x7A_7A_60,
    sand: 0x4A_46_44,
    snow: 0xF4_F6_FA,
};

/// No land in these scenes lies this high, so no snow lies on it.
const NO_SNOW: f64 = 1e5;

/// The ground of a land in `soil`: sand up to `shore`, snow from
/// `snow_line`, bare rock where it stands steeper than `cliff` allows, in
/// beds `bedding` thick.
pub(super) fn ground(
    stage: &mut Stage,
    dice: &mut Dice,
    soil: &Soil,
    (shore, snow_line, cliff): (f64, f64, f64),
    bedding: f64,
) -> Option<usize> {
    let ground = Ground {
        palette: soil.palette(),
        shore,
        snow_line,
        cliff,
        bedding,
        seed: dice.seed(),
        road: None,
        floor: None,
    };
    // Grain a few centimetres across: the land's own grids carry every
    // larger shape, and a coarser grain its geometry does not share would
    // light as if it did.
    stage.material(
        Material::new(Pigment::Ground(ground), Finish::Ground).with_relief(Relief::grain(
            0.06,
            14.0,
            dice.seed(),
        )),
    )
}

/// The shortest wave a breeze raises on water: a little past where surface
/// tension takes over from gravity, at about 1.7 cm.
const CAPILLARY: f64 = 0.03;

/// Fresh water: a lake, a river, a pool, of the colour its depth takes, and
/// ruffled by a breeze that gusts over it in patches.
pub(super) fn river(stage: &mut Stage, dice: &mut Dice) -> Option<usize> {
    let (absorb, glow) = dice.pick(&[
        (Vec3::new(0.35, 0.18, 0.22), Vec3::new(0.004, 0.012, 0.01)),
        (Vec3::new(0.45, 0.12, 0.1), Vec3::new(0.002, 0.012, 0.016)),
        (Vec3::new(0.25, 0.2, 0.3), Vec3::new(0.01, 0.012, 0.006)),
    ])?;
    let ripples = Relief::waves(
        Wind {
            slope_variance: dice.range(3e-4, 8e-3),
            lengths: (dice.range(0.8, 2.4), CAPILLARY),
            spread: 0.6,
            gusts: (dice.range(0.15, 0.5), dice.range(15.0, 60.0)),
        },
        dice.seed(),
    )?;
    stage.water(absorb, glow, None, ripples)
}

/// The land of `plan` with its ground in `material` and its lakes and rivers
/// in `water`: its grids taken among the scene's and its surfaces added, all
/// to be built.
pub(super) fn lay(
    stage: &mut Stage,
    plan: Plan,
    material: usize,
    water: Option<usize>,
) -> Option<Build> {
    let (cells, step) = (plan.cells.1, plan.far_step());
    let origin = (
        plan.relief.centre.0 - plan.reach,
        plan.relief.centre.1 - plan.reach,
    );
    let far = stage.field(Heightfield::new(cells, origin, step, false)?)?;
    // Each finer grid is laid once the eye is sited; until then a grid of
    // one cell holds its place among the scene's.
    let mut nested = [None; NESTS];
    for (slot, nest) in nested.iter_mut().zip(&plan.nests) {
        if nest.is_some() {
            *slot = Some(stage.field(Heightfield::new(1, origin, 1.0, false)?)?);
        }
    }
    // The lakes' and rivers' grids, like the finer grids, are laid once the
    // land knows it has any.
    let fresh = match water {
        Some(_) => Some(stage.field(Heightfield::new(1, origin, 1.0, false)?)?),
        None => None,
    };
    let near_fresh = match (water, plan.near_water) {
        (Some(_), Some(_)) => Some(stage.field(Heightfield::new(1, origin, 1.0, false)?)?),
        _ => None,
    };
    let beyond = match (plan.horizon, plan.horizon_placing()) {
        (Some(horizon), Some((origin, step))) => {
            Some(stage.field(Heightfield::new(horizon.cells, origin, step, false)?)?)
        }
        _ => None,
    };
    let flat = Pose::new(Vec3::ZERO, Frame::WORLD);
    for field in [Some(far), beyond].into_iter().chain(nested).flatten() {
        stage.add(Shape::Land { field }, material, flat, false)?;
    }
    // Beyond the horizon grid a floor below anything the land can reach, so
    // a ray passing over its edge still meets ground.
    if beyond.is_some() {
        stage.ground(plan.relief.lowest() - 2.0 * plan.roughness - 1.0, material)?;
    }
    if let Some(water) = water {
        for field in [fresh, near_fresh].into_iter().flatten() {
            stage.add(Shape::Land { field }, water, flat, false)?;
        }
    }
    let fields = Fields {
        far,
        nests: nested,
        water: fresh,
        near_water: near_fresh,
        horizon: beyond,
    };
    Build::new(plan, fields)
}

/// The land out to the horizon about a far land `reach` either way: some
/// twenty kilometres or six reaches, whichever is further, in 256 cells.
pub(super) fn horizon(reach: f64) -> Horizon {
    Horizon {
        reach: (6.0 * reach).max(20_000.0),
        cells: 256,
    }
}

/// Finer grids about the eye, each 1024 cells a side: one `reaches.0`
/// either way over the middle distance and one `reaches.1` either way
/// underfoot, droplets running over each as `droplets` has them per vertex.
pub(super) fn nests(reaches: (f64, f64), droplets: (f64, f64)) -> [Option<Nest>; NESTS] {
    [
        Some(Nest {
            reach: reaches.0,
            cells: 1024,
            droplets: droplets.0,
        }),
        Some(Nest {
            reach: reaches.1,
            cells: 1024,
            droplets: droplets.1,
        }),
    ]
}

/// Where the eye stands, and which way it looks.
#[derive(Copy, Clone, Debug)]
pub(super) struct Vantage {
    pub(super) eye: Vec3,
    pub(super) heading: f64,
}

/// Where a scheme has the finer grids laid: each about `focus`, moved on by
/// `lead` times its own half breadth, and a footpath worn into them.
pub(super) struct Siting {
    pub(super) focus: (f64, f64),
    pub(super) lead: (f64, f64),
    pub(super) path: Option<Vec<Mark>>,
}

impl Siting {
    /// The finer grids laid ahead of `vantage`, so each holds the ground the
    /// eye sees at its own distance.
    fn ahead(vantage: &Vantage) -> Self {
        let (sin, cos) = (mathf::sin(vantage.heading), mathf::cos(vantage.heading));
        Self {
            focus: (vantage.eye.x, vantage.eye.z),
            lead: (0.55 * sin, 0.55 * cos),
            path: None,
        }
    }
}

/// How a scene is set out on its land once the land is built.
#[allow(
    clippy::large_enum_variant,
    reason = "held once, while the scene is set out"
)]
#[derive(Debug)]
pub(super) enum Scheme {
    /// A scene already set out and seen, on a land about it.
    Set(Set),
    Meadow,
    Forest {
        glade: f64,
    },
    Alpine {
        lake: f64,
    },
    Coast {
        centre: (f64, f64),
        out: f64,
    },
    Desert {
        dunes: bool,
    },
    Winter {
        pond: f64,
    },
    Canyon {
        terrace: f64,
    },
    Valley,
    /// A stream whose bed is of `lithology`, seen from its edge.
    Stream {
        lithology: Lithology,
    },
    /// A sculpture set out on the land another of these schemes sites.
    Sculpture(Grounds),
    Aqueduct(Aqueduct),
}

/// The lands a sculpture stands out in.
#[derive(Copy, Clone, Debug)]
pub(super) enum Grounds {
    Meadow,
    Dunes,
    Alpine { lake: f64 },
}

impl Grounds {
    /// The scheme whose land this is.
    const fn scheme(self) -> Scheme {
        match self {
            Self::Meadow => Scheme::Meadow,
            Self::Dunes => Scheme::Desert { dunes: true },
            Self::Alpine { lake } => Scheme::Alpine { lake },
        }
    }
}

impl Scheme {
    /// Where the eye stands on the far land `survey` shows, and where the
    /// near land is to lie; `None` when the heap will not hold a path.
    pub(super) fn site(
        &self,
        dice: &mut Dice,
        survey: &Survey<'_>,
        runner: &dyn JobRunner,
    ) -> Option<(Option<Vantage>, Siting)> {
        let vantage = match *self {
            Self::Set(ref set) => {
                let focus = set
                    .planting
                    .as_ref()
                    .map_or(survey.extent().0, |planting| planting.focus);
                let siting = Siting {
                    focus,
                    lead: (0.0, 0.0),
                    path: None,
                };
                return Some((None, siting));
            }
            Self::Meadow => {
                let rise = dice.range(1.6, 3.0);
                overlook(survey, dice, rise)
            }
            Self::Forest { glade } => {
                let heading = dice.range(0.0, TAU);
                let back = glade * dice.range(0.6, 0.9);
                let (centre, _) = survey.extent();
                let spot = (
                    centre.0 - mathf::sin(heading) * back,
                    centre.1 - mathf::cos(heading) * back,
                );
                Vantage {
                    eye: stand(survey, spot, dice.range(1.4, 2.0)),
                    heading,
                }
            }
            Self::Alpine { lake } => shore_vantage(survey, dice, lake),
            Self::Coast { centre, out } => coast_vantage(survey, dice, centre, out),
            Self::Desert { .. } => road_vantage(survey, dice).unwrap_or_else(|| {
                let (centre, _) = survey.extent();
                let spot = rise(survey, dice, centre, 200.0, 16);
                let eye = stand(survey, spot, dice.range(1.6, 3.5));
                Vantage {
                    eye,
                    heading: open_heading(&|x, z| survey.height(x, z), dice, eye, 1200.0, 6),
                }
            }),
            Self::Winter { pond } => {
                // Back from the pond's middle to its bank, and a few paces on.
                let heading = dice.range(0.0, TAU);
                let (centre, reach) = survey.extent();
                let (sin, cos) = (mathf::sin(heading), mathf::cos(heading));
                let mut back = pond;
                while back < 0.5 * reach
                    && survey.wet_at(centre.0 - sin * back, centre.1 - cos * back)
                {
                    back += 2.0;
                }
                back += dice.range(4.0, 20.0);
                let spot = (centre.0 - sin * back, centre.1 - cos * back);
                Vantage {
                    eye: stand(survey, spot, dice.range(1.6, 3.0)),
                    heading,
                }
            }
            Self::Canyon { terrace } => {
                let rise = dice.range(2.0, 12.0);
                terrace_vantage(survey, dice, terrace, rise)
            }
            Self::Valley => bridge_vantage(survey, dice).unwrap_or_else(|| {
                let rise = dice.range(1.6, 3.0);
                overlook(survey, dice, rise)
            }),
            Self::Stream { .. } => stream_vantage(survey, dice, runner).unwrap_or_else(|| {
                let rise = dice.range(1.6, 3.0);
                overlook(survey, dice, rise)
            }),
            Self::Aqueduct(ref aqueduct) => aqueduct.site(survey, dice),
            Self::Sculpture(grounds) => return grounds.scheme().site(dice, survey, runner),
        };
        let mut siting = Siting::ahead(&vantage);
        let paths = match self {
            Self::Meadow | Self::Valley => 0.4,
            Self::Forest { .. } => 0.5,
            Self::Alpine { .. } | Self::Winter { .. } => 0.25,
            _ => 0.0,
        };
        if dice.chance(paths) {
            siting.path = Some(footpath(survey, dice, &vantage)?);
        }
        Some((Some(vantage), siting))
    }

    /// The scene set out on `land`, built, about `vantage`: how it is seen.
    pub(super) fn finish(
        self,
        stage: &mut Stage,
        dice: &mut Dice,
        land: &mut Land,
        vantage: Option<Vantage>,
    ) -> Option<Look> {
        paint_road(stage, land);
        self.scene(stage, dice, land, vantage)
    }

    /// The scene set out on `land`, its road painted, about `vantage`.
    fn scene(
        self,
        stage: &mut Stage,
        dice: &mut Dice,
        land: &Land,
        vantage: Option<Vantage>,
    ) -> Option<Look> {
        match self {
            Self::Set(set) => set.plant(stage, dice),
            Self::Meadow => meadow_scene(stage, dice, land, vantage?),
            Self::Forest { glade } => forest_scene(stage, dice, land, (vantage?, glade)),
            Self::Alpine { lake } => alpine_scene(stage, dice, land, (vantage?, lake)),
            Self::Coast { centre, out } => coast_scene(stage, dice, land, (vantage?, centre, out)),
            Self::Desert { dunes } => desert_scene(stage, dice, land, (vantage?, dunes)),
            Self::Winter { pond } => winter_scene(stage, dice, land, (vantage?, pond)),
            Self::Canyon { .. } => canyon_scene(stage, dice, land, vantage?),
            Self::Valley => valley_scene(stage, dice, land, vantage?),
            Self::Stream { lithology } => stream_scene(stage, dice, land, (vantage?, lithology)),
            Self::Sculpture(grounds) => {
                let vantage = vantage?;
                sculpture_piece(stage, dice, land, &vantage)?;
                grounds.scheme().scene(stage, dice, land, Some(vantage))
            }
            Self::Aqueduct(aqueduct) => aqueduct.finish(stage, dice, land, vantage?),
        }
    }
}

/// A stone bridge carrying `land`'s road over each river it crosses: a
/// deck between abutments bedded in either bank, parapets along its edges,
/// and piers standing in the water where the span is long.
fn bridges(stage: &mut Stage, dice: &mut Dice, land: &Land) -> Option<()> {
    let Some(roadway) = land.road else {
        return Some(());
    };
    if land.crossings.is_empty() {
        return Some(());
    }
    let stone = stage.stone(dice)?;
    for crossing in &land.crossings {
        let (from, to) = (crossing.from, crossing.to);
        let (dx, dz) = (to.x - from.x, to.z - from.z);
        let length = mathf::hypot(dx, dz);
        if length < 1.0 {
            continue;
        }
        let frame = Frame::turned(mathf::atan2(dx, dz), 0.0);
        let middle = Vec3::new(
            f64::midpoint(from.x, to.x),
            crossing.deck,
            f64::midpoint(from.z, to.z),
        );
        let (half, run) = (0.5 * roadway.width + 0.45, 0.5 * length + 2.0);
        stage.slab(
            Pose::new(middle - Vec3::UP * 0.4, frame),
            Vec3::new(half, 0.4, run),
            stone,
        )?;
        let (ahead_x, ahead_z) = (frame.z.x * run, frame.z.z * run);
        stage.claim_along(
            (middle.x - ahead_x, middle.z - ahead_z),
            (middle.x + ahead_x, middle.z + ahead_z),
            half,
        )?;
        for side in [-1.0, 1.0] {
            let at = middle + frame.x * (side * (half - 0.22)) + Vec3::UP * 0.5;
            stage.slab(Pose::new(at, frame), Vec3::new(0.22, 0.5, run), stone)?;
        }
        // Each end founded below the lower of its bank and the water.
        for end in [from, to] {
            let ground = land.height(&stage.fields, end.x, end.z).min(crossing.water);
            let top = crossing.deck - 0.8;
            let depth = (top - ground + 1.5).max(1.0);
            let at = Vec3::new(end.x, top - 0.5 * depth, end.z);
            stage.slab(
                Pose::new(at, frame),
                Vec3::new(half + 0.3, 0.5 * depth, 1.4),
                stone,
            )?;
        }
        // Piers stand in the water only, a few metres apart across the river.
        let bays = u32::try_from(mathf::round_i32(mathf::floor(
            crossing.width / dice.range(9.0, 12.0),
        )))
        .unwrap_or(0)
        .min(6)
            + 1;
        let bed = crossing.water - 2.5;
        let (open, water) = (
            0.5 * (length - crossing.width) / length,
            crossing.width / length,
        );
        for bay in 1..bays {
            let t = open.max(0.0) + water * f64::from(bay) / f64::from(bays);
            let (x, z) = (from.x + dx * t, from.z + dz * t);
            let height = crossing.deck - 0.8 - bed;
            let at = Vec3::new(x, bed + 0.5 * height, z);
            stage.slab(
                Pose::new(at, frame),
                Vec3::new(0.75 * half, 0.5 * height, 0.7),
                stone,
            )?;
        }
    }
    Some(())
}

/// On the valley's side a way off from where its road bridges the river,
/// looking toward the bridge: of a few such places, the dry one on ground
/// level enough to stand on with the best prospect; `None` if the road
/// crosses no river.
fn bridge_vantage(survey: &Survey<'_>, dice: &mut Dice) -> Option<Vantage> {
    let crossing = *survey.crossings().first()?;
    let target = (
        f64::midpoint(crossing.from.x, crossing.to.x),
        f64::midpoint(crossing.from.z, crossing.to.z),
    );
    let height = |x: f64, z: f64| survey.height(x, z);
    let mut best: Option<(f64, Vantage)> = None;
    for _ in 0..32 {
        let angle = dice.range(0.0, TAU);
        let distance = dice.range(45.0, 220.0);
        let spot = (
            target.0 + mathf::sin(angle) * distance,
            target.1 + mathf::cos(angle) * distance,
        );
        let lie = survey.lie(spot.0, spot.1);
        let rise = dice.range(1.6, 3.0);
        let turn = dice.angle(-12.0, 12.0);
        if lie.upright < 0.86 || lie.road > 0.05 || survey.wet_at(spot.0, spot.1) {
            continue;
        }
        let eye = Vec3::new(spot.0, lie.height + rise, spot.1);
        let heading = mathf::atan2(target.0 - spot.0, target.1 - spot.1) + turn;
        // Standing above the bridge sees it whole and the river either side.
        let above = smoothstep(0.0, 25.0, eye.y - crossing.deck);
        let score = prospect(&height, eye, heading) + 0.05 * above;
        if best.is_none_or(|(most, _)| score > most) {
            best = Some((score, Vantage { eye, heading }));
        }
    }
    best.map(|(_, vantage)| vantage)
}

/// Paint `land`'s road onto its ground, the road's course handed to the
/// ground's pigment.
fn paint_road(stage: &mut Stage, land: &mut Land) {
    let Some(roadway) = land.road else {
        return;
    };
    if land.roads.len() == 0 {
        return;
    }
    if let Some(ground) = ground_of(stage, land) {
        ground.road = Some(Road {
            surface: roadway.surface,
            courses: core::mem::take(&mut land.roads),
        });
    }
}

/// A scene already set out and seen as its look has it, on a land about it:
/// what is planted there once the land is built.
#[derive(Debug)]
pub(super) struct Set {
    pub(super) look: Look,
    pub(super) planting: Option<Planting>,
}

/// What is planted about a scene set out in a land's clearing.
#[derive(Debug)]
pub(super) struct Planting {
    /// The clearing's middle, which the near land is laid about.
    pub(super) focus: (f64, f64),
    pub(super) vantage: Vantage,
    /// The trees of the land, planted no nearer the clearing's middle than
    /// `keep`.
    pub(super) grove: Grove,
    pub(super) keep: f64,
    pub(super) lawn: Option<Lawning>,
}

/// A sward to grow: seen from where over the land, and of what grass.
#[derive(Copy, Clone, Debug)]
pub(super) struct Lawning {
    pub(super) eye: (f64, f64),
    pub(super) grassland: Grassland,
}

impl Set {
    fn plant(self, stage: &mut Stage, dice: &mut Dice) -> Option<Look> {
        if let Some(planting) = self.planting {
            let rooting = Rooting {
                clearing: Some((planting.focus, planting.keep)),
                ..ANYWHERE
            };
            plant(
                stage,
                dice,
                (planting.grove, planting.vantage),
                planting.lawn.as_ref(),
                rooting,
            )?;
        }
        Some(self.look)
    }
}

/// A lawn as `lawning` has it, if any, and a scattering of woods of `grove`
/// over the land seen from `vantage`, on the ground `rooting` says suits
/// them.
pub(super) fn plant(
    stage: &mut Stage,
    dice: &mut Dice,
    (grove, vantage): (Grove, Vantage),
    lawning: Option<&Lawning>,
    rooting: Rooting,
) -> Option<()> {
    let woodland = Woodland {
        cover: dice.range(0.15, 0.35),
        patch: 220.0,
        closure: (0.6, 1.6),
        stature: (0.65, 0.9),
        gaps: 0.15,
        most: stage.densities.woods.backdrop,
        open: (4.0, 0.6),
    };
    stage.sow(Wood {
        grove,
        woodland,
        rooting,
        vantage,
        beneath: None,
        deadfall: None,
    })?;
    stage.sward = lawning.copied();
    Some(())
}

/// Land about a level clearing a scene is set out in, still to be built,
/// and the trees that will grow on it.
pub(super) struct Backdrop {
    /// The land's relief, level in the clearing where the scene stands.
    pub(super) terrain: Terrain,
    build: Build,
    grove: Grove,
}

impl Backdrop {
    /// The leaves fallen by `season` from the first kind of the land's trees,
    /// lying `thickness` as thick as they fall.
    pub(super) fn fallen(&self, season: Season, thickness: f64) -> Option<Fallen> {
        self.grove
            .first()
            .and_then(|grown| Fallen::from(&grown, season, thickness))
    }

    /// The scene set out on this land, seen as `look` has it from `vantage`,
    /// a lawn about it as `lawn` has it.
    pub(super) fn seen(self, look: Look, vantage: Vantage, lawn: Option<&Lawning>) -> Composed {
        let keep = self
            .terrain
            .clearing
            .map_or(0.0, |(_, radius)| 1.15 * radius);
        let planting = Planting {
            focus: self.terrain.centre,
            vantage,
            grove: self.grove,
            keep,
            lawn: lawn.copied(),
        };
        Composed::Landed(Landing {
            build: self.build,
            scheme: Scheme::Set(Set {
                look,
                planting: Some(planting),
            }),
            vantage: None,
        })
    }
}

/// Gentle wear: a few passes of drainage and creep, enough to settle rolling
/// land into the valleys water would cut in it.
const GENTLE: Wear = Wear {
    passes: 10,
    incision: 3.0e-4,
    creep: 0.1,
    repose: 0.9,
    infill: 0.3,
    strata: None,
};

/// Rolling land about a level clearing `(radius, level)` at the origin, in
/// `soil`, where `kinds` grow as they are in `season`, and a plain past its
/// rim.
pub(super) fn backdrop(
    stage: &mut Stage,
    dice: &mut Dice,
    (radius, level): (f64, f64),
    soil_colours: &Soil,
    (kinds, season): (&[Kind], Season),
) -> Option<Backdrop> {
    let form = Landform::Hills {
        scale: dice.range(250.0, 600.0),
        height: dice.range(8.0, 35.0),
        seed: dice.seed(),
    };
    let datum = form.height(0.0, 0.0);
    let reach = 1600.0;
    let terrain = Terrain {
        rim: None,
        form,
        datum,
        centre: (0.0, 0.0),
        radius: reach,
        tilt: (0.0, 0.0),
        clearing: Some((level, radius)),
    };
    let plan = Plan {
        relief: terrain.clone(),
        reach,
        sea: None,
        wear: GENTLE,
        rivers: None,
        road: None,
        roughness: 0.6,
        ridges: 0.35,
        droplets: 0.03,
        cells: (192, 512),
        nests: nests((700.0, (3.0 * radius).max(70.0)), (0.08, 0.08)),
        near_water: None,
        horizon: Some(horizon(reach)),
        snow_line: None,
        pond: None,
        growth: 1.0,
        seed: dice.seed(),
    };
    let material = ground(stage, dice, soil_colours, (-1e3, NO_SNOW, 0.7), 3.0)?;
    let build = lay(stage, plan, material, None)?;
    Some(Backdrop {
        terrain,
        build,
        grove: Grove::new(stage, dice, (kinds, season), Stand::Open)?,
    })
}

/// Stand the eye `rise` above the land at `spot`, or above the water where
/// it lies there.
pub(super) fn stand(survey: &Survey<'_>, (x, z): (f64, f64), rise: f64) -> Vec3 {
    Vec3::new(x, survey.surface(x, z) + rise, z)
}

/// The highest of `tries` spots within `spread` of `around` that is dry,
/// off any road, and level enough to stand on; `around` itself if none is.
fn rise(
    survey: &Survey<'_>,
    dice: &mut Dice,
    around: (f64, f64),
    spread: f64,
    tries: u32,
) -> (f64, f64) {
    let mut best: Option<(f64, (f64, f64))> = None;
    for _ in 0..tries {
        let spot = (
            around.0 + dice.range(-spread, spread),
            around.1 + dice.range(-spread, spread),
        );
        let lie = survey.lie(spot.0, spot.1);
        if lie.upright < 0.9 || lie.road > 0.05 || survey.wet_at(spot.0, spot.1) {
            continue;
        }
        if best.is_none_or(|(height, _)| lie.height > height) {
            best = Some((lie.height, spot));
        }
    }
    best.map_or(around, |(_, spot)| spot)
}

/// How well the view from `eye` along `heading` over the land `height`
/// gives reads: the skyline standing up beyond, and the land nearer falling
/// away before it rather than walling it off.
fn prospect(height: &dyn Fn(f64, f64) -> f64, eye: Vec3, heading: f64) -> f64 {
    let (sin, cos) = (mathf::sin(heading), mathf::cos(heading));
    let (mut near, mut far) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut distance = 25.0;
    while distance < 3500.0 {
        let angle = mathf::atan2(
            height(eye.x + sin * distance, eye.z + cos * distance) - eye.y,
            distance,
        );
        if distance < 250.0 {
            near = near.max(angle);
        } else if distance > 400.0 {
            far = far.max(angle);
        }
        distance *= 1.1;
    }
    far.min(0.12) - near.max(-0.12) - 3.0 * (near - 0.01).max(0.0)
}

/// A place on a hillside of the far land looking out across it: of a few
/// dry spots on ground level enough to stand on, and a few ways from each,
/// the one with the best prospect, the eye `rise` above the ground.
fn overlook(survey: &Survey<'_>, dice: &mut Dice, rise: f64) -> Vantage {
    let (centre, reach) = survey.extent();
    let spread = 0.4 * reach;
    let height = |x: f64, z: f64| survey.height(x, z);
    let mut best: Option<(f64, Vantage)> = None;
    for _ in 0..40 {
        let spot = (
            centre.0 + dice.range(-spread, spread),
            centre.1 + dice.range(-spread, spread),
        );
        let lie = survey.lie(spot.0, spot.1);
        if lie.upright < 0.88 || lie.road > 0.05 || survey.wet_at(spot.0, spot.1) {
            continue;
        }
        let eye = Vec3::new(spot.0, lie.height + rise, spot.1);
        for _ in 0..6 {
            let heading = dice.range(0.0, TAU);
            let score = prospect(&height, eye, heading);
            if best.is_none_or(|(most, _)| score > most) {
                best = Some((score, Vantage { eye, heading }));
            }
        }
    }
    best.map_or_else(
        || {
            let eye = stand(survey, centre, rise);
            Vantage {
                eye,
                heading: open_heading(&height, dice, eye, 1500.0, 8),
            }
        },
        |(_, vantage)| vantage,
    )
}

/// How steeply the land `height` gives rises ahead of `eye` along `heading`
/// out to `reach`, as the greatest angle above the eye's level at which any
/// of it stands.
fn ridge(height: &dyn Fn(f64, f64) -> f64, eye: Vec3, heading: f64, reach: f64) -> f64 {
    let (sin, cos) = (mathf::sin(heading), mathf::cos(heading));
    let mut steepest = f64::NEG_INFINITY;
    let mut distance = 6.0;
    while distance < reach {
        let rise = height(eye.x + sin * distance, eye.z + cos * distance) - eye.y;
        steepest = steepest.max(mathf::atan2(rise, distance));
        distance *= 1.12;
    }
    steepest
}

/// A camera at `eye` looking along `heading`, pitched so the ridge ahead
/// within `reach` sits `sky` of the way down a picture `fov` tall.
fn looking(
    height: &dyn Fn(f64, f64) -> f64,
    eye: Vec3,
    heading: f64,
    (fov, sky): (f64, f64),
    reach: f64,
) -> View {
    let ahead = ridge(height, eye, heading, reach).clamp(-0.3, 0.6);
    let pitch = ahead + fov * (sky - 0.5);
    View::Placed {
        eye,
        target: eye + direction(heading, 0.0, pitch) * 100.0,
        fov,
        aperture: 0.0,
    }
}

/// The most open of `tries` headings from `eye`: where the land rises least
/// within `reach`.
fn open_heading(
    height: &dyn Fn(f64, f64) -> f64,
    dice: &mut Dice,
    eye: Vec3,
    reach: f64,
    tries: u32,
) -> f64 {
    let mut best = (f64::INFINITY, 0.0);
    for _ in 0..tries {
        let heading = dice.range(0.0, TAU);
        let rise = ridge(height, eye, heading, reach);
        if rise < best.0 {
            best = (rise, heading);
        }
    }
    best.1
}

/// How far the view from `eye` along `heading` runs, out to `reach`, before
/// the land stands across it more steeply than `wall`.
fn openness(
    height: &dyn Fn(f64, f64) -> f64,
    eye: Vec3,
    heading: f64,
    (wall, reach): (f64, f64),
) -> f64 {
    let (sin, cos, steep) = (mathf::sin(heading), mathf::cos(heading), mathf::tan(wall));
    let mut distance = 10.0;
    while distance < reach {
        let rise = height(eye.x + sin * distance, eye.z + cos * distance) - eye.y;
        if rise > distance * steep {
            return distance;
        }
        distance *= 1.1;
    }
    reach
}

/// A footpath worn from just behind the eye out along the way it looks,
/// bending round steep and wet ground.
fn footpath(survey: &Survey<'_>, dice: &mut Dice, vantage: &Vantage) -> Option<Vec<Mark>> {
    let Vantage { eye, heading } = *vantage;
    let start = (
        eye.x - mathf::sin(heading) * 6.0,
        eye.z - mathf::cos(heading) * 6.0,
    );
    let rough = |x: f64, z: f64| {
        let lie = survey.lie(x, z);
        let wet = if survey.wet_at(x, z) { 10.0 } else { 0.0 };
        4.0 * (1.0 - lie.upright) + wet + 2.0 * lie.road
    };
    let reach = 1.8 * survey.finest_reach();
    land::footpath(
        start,
        heading + dice.range(-0.45, 0.45),
        reach,
        dice.range(0.7, 1.3),
        &rough,
        dice.seed(),
    )
}

/// How far either way of the eye a sward's finest lawn reaches, and the side
/// of the cells of its lawns, finest first: the finest two over the land's
/// finest grid, one over each coarser grid, each coarse enough that a leaf
/// merged to a pixel's width at its lawn's outer reach, a desktop's picture
/// across, still stands within its cell.
const FINE_REACH: f64 = 30.0;
const TIER_CELLS: [f64; 4] = [0.15, 0.35, 1.0, 3.0];
/// How far either way of the eye a sward reaches at most, and the share of
/// its reach at which it begins thinning into the land's own grass.
const SWARD_REACH: f64 = 1800.0;
const SWARD_FADE: f64 = 0.6;
/// How much of a grid's breadth a lawn keeps to: clear of its border, where
/// the grid about it takes over.
const GRID_INSET: f64 = 0.96;
/// The share of their reach at which weeds and fallen leaves begin fading.
const LITTER_FADE: f64 = 0.75;

/// The square `reach` either way of `centre`.
fn square(centre: (f64, f64), reach: f64) -> Rect {
    (
        (centre.0 - reach, centre.1 - reach),
        (centre.0 + reach, centre.1 + reach),
    )
}

/// What the two rectangles share.
fn within((from, to): Rect, (least, most): Rect) -> Rect {
    (
        (from.0.max(least.0), from.1.max(least.1)),
        (to.0.min(most.0), to.1.min(most.1)),
    )
}

/// How far `(x, z)` lies within `rect` from its nearest edge.
fn inset_of((x, z): (f64, f64), (from, to): Rect) -> f64 {
    (x - from.0)
        .min(to.0 - x)
        .min(z - from.1)
        .min(to.1 - z)
        .max(0.0)
}

/// A sward as `lawning` has it over `land`, to be laid: fine about the eye,
/// coarser over the rest of the finest grid, coarser again over each grid
/// about that as far as the sward reaches, thinning far off into the land's
/// own grass, which from there on is this sward's colour.
pub(super) fn sward(
    stage: &mut Stage,
    dice: &mut Dice,
    land: &Land,
    lawning: &Lawning,
) -> Option<plants::Laying> {
    let Lawning { eye, grassland } = *lawning;
    let on = |grid: Laid| square(grid.centre, GRID_INSET * grid.reach);
    let far = Laid {
        field: land.far,
        centre: land.centre,
        reach: land.reach,
    };
    // The land's grids, finest first.
    let mut grids = [None; NESTS + 1];
    for (slot, grid) in grids
        .iter_mut()
        .zip(land.nests.iter().rev().flatten().copied().chain([far]))
    {
        *slot = Some(grid);
    }
    let finest = grids[0].unwrap_or(far);
    let fine = within(square(eye, FINE_REACH), on(finest));
    let mut tiers = [None; TIER_CELLS.len()];
    tiers[0] = Some(Tier {
        field: finest.field,
        from: fine.0,
        to: fine.1,
        hole: None,
        cell: TIER_CELLS[0],
    });
    let mut inner = fine;
    let reached = square(eye, SWARD_REACH);
    for (index, grid) in grids.iter().enumerate() {
        let (Some(grid), Some(&cell), Some(slot)) =
            (grid, TIER_CELLS.get(index + 1), tiers.get_mut(index + 1))
        else {
            break;
        };
        let outer = within(on(*grid), reached);
        *slot = Some(Tier {
            field: grid.field,
            from: outer.0,
            to: outer.1,
            hole: Some(inner),
            cell,
        });
        inner = outer;
    }
    let reach = inset_of(eye, inner);
    let seen = Seen::from(eye, (SWARD_FADE * reach, reach));
    let middle = on(finest);
    let beside = inset_of(eye, middle);
    let near = Seen::from(eye, (LITTER_FADE * beside, beside));
    let tiered: Vec<Tier> = fallible::collected(tiers.len(), tiers.into_iter().flatten())?;
    // From afar, the land's grass is this sward's.
    let (green, straw) = grassland.afar();
    if let Some(soil) = ground_of(stage, land) {
        soil.palette.grass = green;
        soil.palette.dry = straw;
    }
    plants::Laying::new(
        stage,
        dice,
        &grassland,
        (tiered, seen),
        ((finest.field, middle), near),
    )
}

/// Which of the stage's materials `land`'s ground is painted in.
fn ground_material(stage: &Stage, land: &Land) -> Option<usize> {
    stage
        .objects
        .iter()
        .find(|object| matches!(object.shape, Shape::Land { field } if field == land.far))
        .map(|object| object.material)
}

/// The ground `land` is painted with, if its ground is a land's.
pub(super) fn ground_of<'a>(stage: &'a mut Stage, land: &Land) -> Option<&'a mut Ground> {
    let material = ground_material(stage, land)?;
    match &mut stage.materials.get_mut(material)?.pigment {
        Pigment::Ground(ground) => Some(ground),
        _ => None,
    }
}

/// The rock `land`'s ground is made of, as its pigment has it; `None` for
/// a land whose ground is not a land's.
fn rock_of(stage: &Stage, land: &Land) -> Option<Rock> {
    let material = ground_material(stage, land)?;
    let Pigment::Ground(ground) = &stage.materials.get(material)?.pigment else {
        return None;
    };
    Some(Rock {
        stone: ground.palette.rock,
        strata: ground.palette.strata,
        lichen: ground.palette.lichen,
        bedding: 0.25 * ground.bedding.min(4.0),
        seed: ground.seed,
    })
}

/// Stones of the land's own rock strewn on `land` ahead of `vantage`:
/// `count` of them `sizes` across, `reach` off and `spread` either side of
/// the view, the smaller far the commoner, wherever `lies` says a stone can
/// lie.
fn strew(
    stage: &mut Stage,
    dice: &mut Dice,
    (land, vantage): (&Land, &Vantage),
    (count, sizes, reach, spread): (u32, (f64, f64), (f64, f64), f64),
    lies: &dyn Fn(&Lie) -> bool,
) -> Option<()> {
    let stones = Stones::new(stage, dice, rock_of(stage, land)?)?;
    let Vantage { eye, heading } = *vantage;
    for _ in 0..count {
        let angle = heading + dice.range(-spread, spread);
        // The nearer ground holds more of them, as the eye would see them.
        let near = dice.unit();
        let distance = reach.0 + (reach.1 - reach.0) * near * mathf::sqrt(near);
        let at = (
            eye.x + mathf::sin(angle) * distance,
            eye.z + mathf::cos(angle) * distance,
        );
        let size = sizes.0 + (sizes.1 - sizes.0) * dice.unit() * dice.unit();
        let lie = land.lie(&stage.fields, at.0, at.1);
        if !lies(&lie) || land.wet_at(&stage.fields, at.0, at.1) || !stage.clear(at, 0.5 * size) {
            continue;
        }
        stage.claim(at, 0.5 * size)?;
        let normal = land.normal(&stage.fields, at.0, at.1);
        stones.lay(
            stage,
            dice,
            (Vec3::new(at.0, lie.height, at.1), normal),
            size,
        )?;
    }
    Some(())
}

/// Heather and gorse scattered over the open ground ahead of `vantage`,
/// where nothing taller grows.
fn moorland(stage: &mut Stage, dice: &mut Dice, vantage: &Vantage, season: Season) -> Option<()> {
    let shrubs = Grove::new(
        stage,
        dice,
        (&[Kind::Heather, Kind::Gorse], season),
        Stand::Open,
    )?;
    let woodland = Woodland {
        cover: 0.5,
        patch: 60.0,
        closure: (1.2, 2.6),
        stature: (0.6, 1.0),
        gaps: 0.0,
        most: 2500,
        open: (3.0, 0.4),
    };
    stage.sow(Wood {
        grove: shrubs,
        woodland,
        rooting: Rooting {
            upright: (0.7, 0.85),
            ..ANYWHERE
        },
        vantage: *vantage,
        beneath: None,
        deadfall: None,
    })
}

/// Whether a stone can lie on the ground `lie` describes: off the road, and
/// not so steep it would roll.
fn lies_still(lie: &Lie) -> bool {
    lie.road < 0.1 && lie.upright > 0.7
}

/// A large ball of something that shows light off, resting in the land
/// ahead of the eye: chrome, gold, glass.
fn marvel(stage: &mut Stage, dice: &mut Dice, land: &Land, vantage: &Vantage) -> Option<()> {
    let Vantage { eye, heading } = *vantage;
    let distance = dice.range(7.0, 16.0);
    let angle = heading + dice.range(-0.25, 0.25);
    let (x, z) = (
        eye.x + mathf::sin(angle) * distance,
        eye.z + mathf::cos(angle) * distance,
    );
    let radius = dice.range(0.8, 1.8);
    stage.claim((x, z), radius)?;
    let material = match dice.count(0, 3) {
        0 => stage.metal(Vec3::splat(0.93), 0.0)?,
        1 => stage.metal(GOLD, 0.05)?,
        2 => stage.glass(Vec3::splat(0.97), 0.0)?,
        _ => stage.precious(dice)?,
    };
    let base = Vec3::new(x, land.height(&stage.fields, x, z) - 0.15 * radius, z);
    stage.ball(base, radius, material, dice).map(|_| ())
}

/// The look of a landscape under `weather`, seen by `view`, its exposure
/// scaled by `brightness` for a land lighter or darker than most.
fn look(weather: Outdoors, view: View, brightness: f64) -> Look {
    Look {
        sky: weather.sky,
        exposure: match weather.exposure {
            Exposure::Metered { key } => Exposure::Metered {
                key: key * brightness,
            },
            fixed @ Exposure::Fixed(_) => fixed,
        },
        view,
    }
}

/// A camera at the vantage looking along it, the land ahead set `sky` of
/// the way down a picture `fov` tall.
fn view(stage: &Stage, land: &Land, vantage: &Vantage, (fov, sky): (f64, f64)) -> View {
    let height = |x: f64, z: f64| land.height(&stage.fields, x, z);
    looking(
        &height,
        vantage.eye,
        vantage.heading,
        (fov, sky),
        0.9 * land.reach,
    )
}

/// A camera at the vantage looking along it, `pitch` above the level.
fn level_view(vantage: &Vantage, fov: f64, pitch: f64) -> View {
    View::Placed {
        eye: vantage.eye,
        target: vantage.eye + direction(vantage.heading, 0.0, pitch) * 100.0,
        fov,
        aperture: 0.0,
    }
}

const MEADOW: Climate = Climate {
    hours: &[
        (Hour::Noon, 1),
        (Hour::Day, 4),
        (Hour::Golden, 4),
        (Hour::Sunset, 2),
    ],
    covers: &[
        (Cover::Clear, 2),
        (Cover::Fair, 5),
        (Cover::Broken, 2),
        (Cover::Cirrus, 2),
        (Cover::Towering, 1),
        (Cover::Mackerel, 1),
        (Cover::Overcast, 1),
    ],
    haze: (1.0, 2.4),
    base: 120.0,
    albedo: 0.18,
};

/// A regional slope falling `fall` metres per metre toward a heading drawn
/// at random, a little more or less.
fn tilt(dice: &mut Dice, fall: f64) -> (f64, f64) {
    let (heading, fall) = (dice.range(0.0, TAU), fall * dice.range(0.7, 1.3));
    (fall * mathf::sin(heading), fall * mathf::cos(heading))
}

/// A road across a land, of one of `surfaces`.
fn roadway(dice: &mut Dice, surfaces: &[Surface]) -> Option<Roadway> {
    let surface = dice.pick(surfaces)?;
    let width = match surface {
        Surface::Tarmac => dice.range(5.5, 7.5),
        Surface::Gravel => dice.range(3.5, 5.0),
        Surface::Track => dice.range(2.6, 3.2),
    };
    Some(Roadway {
        surface,
        width,
        heading: dice.range(0.0, TAU),
    })
}

/// Farmland: rolling hills, streams in their valleys, now and then a lane.
pub(super) fn meadow(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let reach = 4000.0;
    let height = dice.range(70.0, 170.0);
    let relief = Terrain {
        form: Landform::Hills {
            scale: dice.range(500.0, 950.0),
            height,
            seed: dice.seed(),
        },
        datum: 0.0,
        centre: (0.0, 0.0),
        radius: reach,
        rim: None,
        tilt: tilt(dice, height / reach),
        clearing: None,
    };
    let road = if dice.chance(0.5) {
        Some(roadway(
            dice,
            &[
                Surface::Track,
                Surface::Track,
                Surface::Gravel,
                Surface::Tarmac,
            ],
        )?)
    } else {
        None
    };
    let plan = Plan {
        relief,
        reach,
        sea: None,
        wear: Wear {
            passes: 32,
            incision: 2.0e-3,
            creep: 0.08,
            repose: 0.9,
            infill: 0.3,
            strata: None,
        },
        rivers: Some(Rivers {
            catchment: 4.0e5,
            width: 3.0,
            meander: 1.2,
            flowing: 1.0,
            ledges: 0.0,
            outcrops: 0.05,
        }),
        road,
        roughness: 1.2,
        ridges: 0.35,
        droplets: 0.05,
        cells: (256, 1024),
        nests: nests((700.0, 80.0), (0.08, 0.12)),
        near_water: None,
        horizon: Some(horizon(reach)),
        snow_line: None,
        pond: None,
        growth: 1.0,
        seed: dice.seed(),
    };
    let soil = dice.pick(&[GREEN, GREEN, GOLDEN, HIGHLAND])?;
    let material = ground(stage, dice, &soil, (-1e3, NO_SNOW, 0.72), 4.0)?;
    let water = river(stage, dice)?;
    let build = lay(stage, plan, material, Some(water))?;
    Some(Composed::Landed(Landing {
        build,
        scheme: Scheme::Meadow,
        vantage: None,
    }))
}

fn meadow_scene(stage: &mut Stage, dice: &mut Dice, land: &Land, vantage: Vantage) -> Option<Look> {
    bridges(stage, dice, land)?;
    let kinds: &[Kind] = match dice.count(0, 3) {
        0 => &[Kind::Oak, Kind::Poplar, Kind::Willow],
        1 => &[Kind::Oak, Kind::Birch, Kind::Cherry],
        2 => &[Kind::Maple, Kind::Birch, Kind::Oak],
        _ => &[Kind::Olive, Kind::Poplar],
    };
    let season = dice.pick(&[
        Season::Spring,
        Season::Summer,
        Season::Summer,
        Season::Autumn { fallen: 15 },
    ])?;
    let grove = Grove::new(stage, dice, (kinds, season), Stand::Open)?;
    let eye = vantage.eye;
    stage.keep_open((eye.x, eye.z), 4.0)?;
    let grassland = Grassland {
        fallen: grove
            .of(kinds[0])
            .and_then(|grown| Fallen::from(&grown, season, 0.5)),
        ..plants::grassland(dice, Character::Meadow, season)
    };
    // Trees gather in the land's woods, and along its streams.
    let woodland = Woodland {
        cover: dice.range(0.1, 0.4),
        patch: 260.0,
        closure: (0.6, 1.6),
        stature: (0.65, 0.9),
        gaps: 0.1,
        most: stage.densities.woods.meadow,
        open: (4.0, 0.6),
    };
    stage.sow(Wood {
        grove,
        woodland,
        rooting: Rooting {
            streams: 0.5,
            ..ANYWHERE
        },
        vantage,
        beneath: None,
        deadfall: None,
    })?;
    waterside::margins(stage, dice, ((eye.x, eye.z), season, None))?;
    stage.sward = Some(Lawning {
        eye: (eye.x, eye.z),
        grassland,
    });
    if dice.chance(0.1) {
        marvel(stage, dice, land, &vantage)?;
    }
    let weather = weather::outdoors(stage, dice, &MEADOW, vantage.heading)?;
    let fov = dice.angle(46.0, 62.0);
    let view = view(stage, land, &vantage, (fov, dice.range(0.5, 0.68)));
    Some(look(weather, view, 1.0))
}

const FOREST: Climate = Climate {
    hours: &[
        (Hour::Day, 4),
        (Hour::Golden, 5),
        (Hour::Sunset, 1),
        (Hour::Noon, 1),
    ],
    covers: &[
        (Cover::Clear, 3),
        (Cover::Fair, 4),
        (Cover::Broken, 2),
        (Cover::Overcast, 1),
    ],
    haze: (1.4, 3.2),
    base: 250.0,
    albedo: 0.14,
};

/// A glade among wooded hills, a brook somewhere below.
pub(super) fn forest(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let reach = 1400.0;
    let form = Landform::Hills {
        scale: dice.range(150.0, 300.0),
        height: dice.range(10.0, 30.0),
        seed: dice.seed(),
    };
    let glade = dice.range(12.0, 22.0);
    let level = form.height(0.0, 0.0);
    let relief = Terrain {
        form,
        datum: 0.0,
        centre: (0.0, 0.0),
        radius: reach,
        rim: None,
        tilt: (0.0, 0.0),
        clearing: Some((level, glade)),
    };
    let plan = Plan {
        relief,
        reach,
        sea: None,
        wear: GENTLE,
        rivers: Some(Rivers {
            catchment: 2.0e5,
            width: 1.8,
            meander: 1.6,
            flowing: 1.0,
            ledges: 0.0,
            outcrops: 0.1,
        }),
        road: None,
        roughness: 0.8,
        ridges: 0.3,
        droplets: 0.04,
        cells: (256, 1024),
        nests: nests((700.0, 64.0), (0.08, 0.1)),
        near_water: None,
        horizon: Some(horizon(reach)),
        snow_line: None,
        pond: None,
        growth: 1.0,
        seed: dice.seed(),
    };
    let material = ground(stage, dice, &GREEN, (-1e3, NO_SNOW, 0.7), 3.0)?;
    let water = river(stage, dice)?;
    let build = lay(stage, plan, material, Some(water))?;
    Some(Composed::Landed(Landing {
        build,
        scheme: Scheme::Forest { glade },
        vantage: None,
    }))
}

/// A kind of forest: the trees of its canopy and how they stand; the
/// shrubs beneath them and how many to a hectare where the light suits them;
/// and how many fallen trunks, stumps and standing dead to a hectare where
/// its canopy has opened.
struct Forest {
    kinds: &'static [Kind],
    woodland: Woodland,
    beneath: (&'static [Kind], f64),
    deadfall: (f64, f64, f64),
}

/// A kind of forest drawn from `dice`, its canopy standing at most `most`
/// trees.
fn forest_kind(dice: &mut Dice, most: u32) -> Forest {
    let woodland = |cover: f64, closure: (f64, f64), stature: (f64, f64), gaps: f64| Woodland {
        cover,
        patch: 260.0,
        closure,
        stature,
        gaps,
        most,
        open: (1.5, 0.3),
    };
    match dice.count(0, 5) {
        // High beech and oak: closed, dark beneath, its floor its own leaves.
        0 => Forest {
            kinds: &[Kind::Beech, Kind::Oak, Kind::Birch],
            woodland: woodland(0.97, (0.42, 0.62), (0.8, 1.0), 0.25),
            beneath: (&[Kind::Fern, Kind::Hazel, Kind::Box], 700.0),
            deadfall: (25.0, 6.0, 8.0),
        },
        // Open oak wood: glades, a grassy floor, hazel beneath.
        1 => Forest {
            kinds: &[Kind::Oak, Kind::Birch, Kind::Maple],
            woodland: woodland(0.85, (0.55, 1.1), (0.7, 0.95), 0.3),
            beneath: (&[Kind::Fern, Kind::Hazel], 1200.0),
            deadfall: (20.0, 8.0, 6.0),
        },
        // Spruce and pine, tall and close, worked for its timber.
        2 => Forest {
            kinds: &[Kind::Spruce, Kind::Pine],
            woodland: woodland(0.97, (0.4, 0.58), (0.8, 1.0), 0.15),
            beneath: (&[Kind::Fern, Kind::Box], 600.0),
            deadfall: (35.0, 25.0, 12.0),
        },
        // Birch and pine, light and airy.
        3 => Forest {
            kinds: &[Kind::Birch, Kind::Pine],
            woodland: woodland(0.92, (0.5, 0.9), (0.7, 0.95), 0.25),
            beneath: (&[Kind::Fern, Kind::Hazel], 900.0),
            deadfall: (25.0, 10.0, 10.0),
        },
        // Wildwood: its canopy broken by the trees that fell and lie where
        // they fell, a thicket of young growth in every gap.
        4 => Forest {
            kinds: &[Kind::Oak, Kind::Beech, Kind::Maple, Kind::Birch],
            woodland: woodland(0.93, (0.45, 1.3), (0.6, 1.0), 0.6),
            beneath: (&[Kind::Fern, Kind::Hazel, Kind::Box], 2500.0),
            deadfall: (70.0, 12.0, 25.0),
        },
        _ => Forest {
            kinds: &[Kind::Oak, Kind::Spruce, Kind::Birch, Kind::Pine],
            woodland: woodland(0.95, (0.45, 0.8), (0.75, 0.98), 0.3),
            beneath: (&[Kind::Fern, Kind::Hazel, Kind::Box], 900.0),
            deadfall: (30.0, 12.0, 10.0),
        },
    }
}

/// The dead of the first two of `kinds` in `season`, as `(logs, stumps,
/// snags)` of them lie and stand to a hectare where the canopy has opened;
/// `None` when the heap will not hold them.
fn deadfall(
    stage: &mut Stage,
    dice: &mut Dice,
    (kinds, season): (&[Kind], Season),
    (logs, stumps, snags): (f64, f64, f64),
) -> Option<Deadfall> {
    let mut dead = [None, None];
    for (slot, &kind) in dead.iter_mut().zip(kinds) {
        *slot = Some(Dead::new(stage, dice, (kind, season))?);
    }
    Some(Deadfall {
        dead,
        logs,
        stumps,
        snags,
    })
}

fn forest_scene(
    stage: &mut Stage,
    dice: &mut Dice,
    land: &Land,
    (vantage, glade): (Vantage, f64),
) -> Option<Look> {
    let forest = forest_kind(dice, stage.densities.woods.forest);
    let season = dice.pick(&[
        Season::Summer,
        Season::Autumn { fallen: 10 },
        Season::Autumn { fallen: 35 },
    ])?;
    let kinds = forest.kinds;
    let grove = Grove::new(stage, dice, (kinds, season), Stand::Close)?;
    let young = kinds.get(..2).unwrap_or(kinds);
    let (shrubs, plants) = forest.beneath;
    let beneath = Grove::new(stage, dice, (shrubs, season), Stand::Open)?.and(&Grove::new(
        stage,
        dice,
        (young, season),
        Stand::Young,
    )?);
    let grassland = Grassland {
        fallen: grove
            .first()
            .and_then(|grown| Fallen::from(&grown, season, 1.8)),
        ..plants::grassland(dice, Character::Meadow, season)
    };
    stage.keep_open((vantage.eye.x, vantage.eye.z), 2.0)?;
    let deadfall = deadfall(stage, dice, (young, season), forest.deadfall)?;
    stage.sow(Wood {
        grove,
        woodland: forest.woodland,
        rooting: Rooting {
            clearing: Some((land.centre, 1.1 * glade)),
            ..ANYWHERE
        },
        vantage,
        beneath: Some(Beneath {
            grove: beneath,
            plants,
        }),
        deadfall: Some(deadfall),
    })?;
    let eye = vantage.eye;
    waterside::margins(stage, dice, ((eye.x, eye.z), season, None))?;
    stage.sward = Some(Lawning {
        eye: (eye.x, eye.z),
        grassland,
    });
    let weather = weather::outdoors(stage, dice, &FOREST, vantage.heading)?;
    let fov = dice.angle(50.0, 64.0);
    let view = level_view(&vantage, fov, dice.angle(4.0, 12.0));
    Some(look(weather, view, 1.0))
}

const ALPINE: Climate = Climate {
    hours: &[
        (Hour::Day, 4),
        (Hour::Golden, 4),
        (Hour::Sunset, 2),
        (Hour::Noon, 1),
        (Hour::Dusk, 1),
    ],
    covers: &[
        (Cover::Clear, 3),
        (Cover::Fair, 4),
        (Cover::Broken, 2),
        (Cover::Cirrus, 2),
        (Cover::Towering, 1),
    ],
    haze: (0.5, 1.2),
    base: 1400.0,
    albedo: 0.2,
};

/// Mountains worn into valleys and ridges, snow on their peaks, a lake in
/// the valley floor.
pub(super) fn alpine(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let height = dice.range(900.0, 1700.0);
    let floor = dice.range(700.0, 1300.0);
    let reach = 5000.0;
    let relief = Terrain {
        form: Landform::Mountains {
            scale: dice.range(900.0, 1600.0),
            height,
            floor,
            seed: dice.seed(),
        },
        datum: 0.0,
        centre: (0.0, 0.0),
        radius: reach,
        rim: None,
        tilt: (0.0, 0.0),
        clearing: None,
    };
    // The lake fills the valley floor to about the lower half of its lie.
    let mut lies = [0.0f64; 32];
    for (index, lie) in lies.iter_mut().enumerate() {
        let angle = TAU * real(index) / 32.0;
        let distance = 0.45 * floor * dice.unit();
        *lie = relief.height(distance * mathf::sin(angle), distance * mathf::cos(angle));
    }
    lies.sort_by(f64::total_cmp);
    let lake = lies.get(12).copied()?;
    let snow_line = lake + dice.range(0.45, 0.65) * height;
    let plan = Plan {
        relief,
        reach,
        sea: Some(lake),
        wear: Wear {
            passes: 22,
            incision: 1.4e-3,
            creep: 0.012,
            repose: 1.5,
            infill: 0.3,
            strata: None,
        },
        rivers: Some(Rivers {
            catchment: 1.5e6,
            width: 6.0,
            meander: 0.9,
            flowing: 1.0,
            ledges: 0.0,
            outcrops: 0.3,
        }),
        road: None,
        roughness: 22.0,
        ridges: 0.75,
        droplets: 0.06,
        cells: (384, 1024),
        nests: nests((1000.0, 90.0), (0.08, 0.12)),
        near_water: None,
        horizon: Some(Horizon {
            cells: 512,
            ..horizon(reach)
        }),
        snow_line: Some(snow_line),
        pond: None,
        growth: 1.0,
        seed: dice.seed(),
    };
    let material = ground(stage, dice, &HIGHLAND, (lake + 1.5, snow_line, 0.68), 5.0)?;
    let water = lake_water(stage, dice)?;
    let build = lay(stage, plan, material, Some(water))?;
    let half = 0.9 * reach;
    stage.add(
        Shape::Quad {
            corner: Vec3::new(-half, lake, -half),
            edge_u: Vec3::new(0.0, 0.0, 2.0 * half),
            edge_v: Vec3::new(2.0 * half, 0.0, 0.0),
        },
        water,
        Pose::new(Vec3::ZERO, Frame::WORLD),
        false,
    )?;
    Some(Composed::Landed(Landing {
        build,
        scheme: Scheme::Alpine { lake },
        vantage: None,
    }))
}

/// On the shore of the lake at `lake`, looking back across the water.
fn shore_vantage(survey: &Survey<'_>, dice: &mut Dice, lake: f64) -> Vantage {
    let (centre, reach) = survey.extent();
    let out = dice.range(0.0, TAU);
    let (sin, cos) = (mathf::sin(out), mathf::cos(out));
    let mut distance = 20.0;
    while distance < 0.6 * reach
        && survey.height(centre.0 + distance * sin, centre.1 + distance * cos) < lake + 3.0
    {
        distance += 10.0;
    }
    distance += dice.range(15.0, 60.0);
    let spot = (centre.0 + distance * sin, centre.1 + distance * cos);
    Vantage {
        eye: stand(survey, spot, dice.range(2.0, 12.0)),
        heading: out + PI + dice.angle(-35.0, 35.0),
    }
}

/// A mountain lake: clear and cold, still but for a breath of wind that draws
/// its catspaws over the mirror the mountains stand in.
fn lake_water(stage: &mut Stage, dice: &mut Dice) -> Option<usize> {
    let ripples = Relief::waves(
        Wind {
            slope_variance: dice.range(1e-6, 3e-4),
            lengths: (dice.range(0.8, 2.4), CAPILLARY),
            spread: 0.5,
            gusts: (dice.range(0.05, 0.3), dice.range(20.0, 80.0)),
        },
        dice.seed(),
    )?;
    stage.water(
        Vec3::new(0.3, 0.1, 0.08),
        Vec3::new(0.002, 0.01, 0.014),
        None,
        ripples,
    )
}

fn alpine_scene(
    stage: &mut Stage,
    dice: &mut Dice,
    land: &Land,
    (vantage, lake): (Vantage, f64),
) -> Option<Look> {
    let (kinds, season): (&[Kind], Season) = if dice.chance(0.6) {
        (&[Kind::Spruce, Kind::Pine], Season::Summer)
    } else {
        (
            &[Kind::Spruce, Kind::Birch, Kind::Pine],
            Season::Autumn { fallen: 5 },
        )
    };
    let grove = Grove::new(stage, dice, (kinds, season), Stand::Close)?;
    stage.keep_open((vantage.eye.x, vantage.eye.z), 5.0)?;
    let count = dice.count(30, 70);
    strew(
        stage,
        dice,
        (land, &vantage),
        (count, (0.2, 2.4), (4.0, 110.0), 0.9),
        &lies_still,
    )?;
    // Forest on the lower slopes, from the lake's shore up to the tree line.
    let tree_line = lake + dice.range(250.0, 500.0);
    let woodland = Woodland {
        cover: dice.range(0.6, 0.85),
        patch: 400.0,
        closure: (0.5, 1.0),
        stature: (0.65, 0.95),
        gaps: 0.2,
        most: 60_000,
        open: (4.0, 0.6),
    };
    let deadfall = deadfall(stage, dice, (kinds, season), (15.0, 4.0, 6.0))?;
    stage.sow(Wood {
        grove,
        woodland,
        rooting: Rooting {
            above: Some((lake + 1.5, lake + 4.0)),
            below: Some((tree_line - 80.0, tree_line)),
            ..ANYWHERE
        },
        vantage,
        beneath: None,
        deadfall: Some(deadfall),
    })?;
    moorland(stage, dice, &vantage, season)?;
    let grassland = plants::grassland(dice, Character::Upland, season);
    let eye = vantage.eye;
    waterside::margins(stage, dice, ((eye.x, eye.z), season, Some(lake)))?;
    stage.sward = Some(Lawning {
        eye: (eye.x, eye.z),
        grassland,
    });
    let weather = weather::outdoors(stage, dice, &ALPINE, vantage.heading)?;
    let fov = dice.angle(44.0, 58.0);
    let view = level_view(&vantage, fov, dice.angle(2.0, 9.0));
    Some(look(weather, view, 0.95))
}

const COAST: Climate = Climate {
    hours: &[
        (Hour::Day, 4),
        (Hour::Golden, 4),
        (Hour::Sunset, 3),
        (Hour::Noon, 1),
        (Hour::Dusk, 1),
        (Hour::Night, 1),
    ],
    covers: &[
        (Cover::Clear, 2),
        (Cover::Fair, 4),
        (Cover::Broken, 3),
        (Cover::Cirrus, 2),
        (Cover::Mackerel, 1),
        (Cover::Overcast, 1),
    ],
    haze: (1.2, 2.6),
    base: 0.0,
    albedo: 0.15,
};

/// An island rising from the sea, its streams running down to the shore.
pub(super) fn coast(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let out = dice.range(0.0, TAU);
    let across = out + FRAC_PI_2 * dice.sign();
    let island = dice.range(700.0, 1500.0);
    let rise = dice.range(40.0, 160.0);
    let centre = (
        -mathf::sin(out) * 0.85 * island + mathf::sin(across) * 0.3 * island,
        -mathf::cos(out) * 0.85 * island + mathf::cos(across) * 0.3 * island,
    );
    let landform = Landform::Island {
        centre,
        radius: island,
        height: rise,
        seed: dice.seed(),
    };
    let reach = 1.4 * island;
    let relief = Terrain {
        rim: Some(landform.lowest()),
        form: landform,
        datum: 0.0,
        centre,
        radius: reach,
        tilt: (0.0, 0.0),
        clearing: None,
    };
    let plan = Plan {
        relief,
        reach,
        sea: Some(0.0),
        wear: Wear {
            passes: 16,
            incision: 6.0e-4,
            creep: 0.06,
            repose: 1.0,
            infill: 0.3,
            strata: None,
        },
        rivers: Some(Rivers {
            catchment: 2.5e5,
            width: 3.0,
            meander: 1.2,
            flowing: 1.0,
            ledges: 0.0,
            outcrops: 0.05,
        }),
        road: None,
        roughness: 1.5,
        ridges: 0.5,
        droplets: 0.05,
        cells: (256, 1024),
        nests: nests((700.0, 70.0), (0.08, 0.12)),
        near_water: None,
        horizon: None,
        snow_line: None,
        pond: None,
        growth: 1.0,
        seed: dice.seed(),
    };
    let soil = dice.pick(&[GREEN, GOLDEN, VOLCANIC, HIGHLAND])?;
    let material = ground(stage, dice, &soil, (1.8, NO_SNOW, 0.72), 3.0)?;
    let water = river(stage, dice)?;
    let build = lay(stage, plan, material, Some(water))?;
    Some(Composed::Landed(Landing {
        build,
        scheme: Scheme::Coast { centre, out },
        vantage: None,
    }))
}

/// How far out from an island's `centre` along `angle` its shore lies, the
/// land `height` gives reaching no further than `reach`.
fn shore(
    height: &dyn Fn(f64, f64) -> f64,
    (centre, reach): ((f64, f64), f64),
    angle: f64,
) -> Option<f64> {
    let (sin, cos) = (mathf::sin(angle), mathf::cos(angle));
    let mut distance = 0.1 * reach;
    while distance < reach {
        if height(centre.0 + sin * distance, centre.1 + cos * distance) < 0.4 {
            return Some(distance);
        }
        distance += 4.0;
    }
    None
}

/// Up from the sea to the island's outer shore about the way `out` looks,
/// and back from it to a beach or a cliff's top, looking along the coast: of
/// a dozen such places, the dry one standing up best over its prospect.
fn coast_vantage(survey: &Survey<'_>, dice: &mut Dice, centre: (f64, f64), out: f64) -> Vantage {
    let (_, reach) = survey.extent();
    let height = |x: f64, z: f64| survey.height(x, z);
    let mut best: Option<(f64, Vantage)> = None;
    for _ in 0..12 {
        let angle = out + dice.angle(-50.0, 50.0);
        let (sin, cos) = (mathf::sin(angle), mathf::cos(angle));
        // In from the open sea, so an estuary or an inland hollow is never
        // taken for the coast.
        let mut distance = 0.98 * reach;
        while distance > 0.1 * reach
            && height(centre.0 + sin * distance, centre.1 + cos * distance) < 1.0
        {
            distance -= 4.0;
        }
        let back = distance - dice.range(8.0, 60.0);
        let spot = (centre.0 + sin * back, centre.1 + cos * back);
        let lie = survey.lie(spot.0, spot.1);
        let rise = dice.range(1.6, 4.0);
        // Out to sea, but turned along the shore so the coast runs down one
        // side of the picture.
        let heading = angle + dice.sign() * dice.angle(35.0, 65.0);
        if survey.wet_at(spot.0, spot.1) || lie.upright < 0.8 {
            continue;
        }
        let eye = Vec3::new(spot.0, lie.height + rise, spot.1);
        let score = prospect(&height, eye, heading) + 0.004 * lie.height.min(40.0);
        if best.is_none_or(|(most, _)| score > most) {
            best = Some((score, Vantage { eye, heading }));
        }
    }
    best.map_or_else(
        || {
            let edge = shore(&height, (centre, reach), out).unwrap_or(0.7 * reach);
            let spot = (
                centre.0 + mathf::sin(out) * (edge - 20.0),
                centre.1 + mathf::cos(out) * (edge - 20.0),
            );
            Vantage {
                eye: stand(survey, spot, 2.5),
                heading: out,
            }
        },
        |(_, vantage)| vantage,
    )
}

fn coast_scene(
    stage: &mut Stage,
    dice: &mut Dice,
    land: &Land,
    (vantage, centre, out): (Vantage, (f64, f64), f64),
) -> Option<Look> {
    let eye = vantage.eye;
    stage.keep_open((eye.x, eye.z), 3.0)?;
    let weather = weather::outdoors(stage, dice, &COAST, vantage.heading)?;
    ocean(stage, dice, out)?;
    let count = dice.count(20, 50);
    let beach = |lie: &Lie| lie.upright > 0.6 && lie.height < 6.0;
    strew(
        stage,
        dice,
        (land, &vantage),
        (count, (0.3, 3.5), (6.0, 150.0), 0.8),
        &beach,
    )?;
    let grove = Grove::new(
        stage,
        dice,
        (&[Kind::Pine, Kind::Oak], Season::Summer),
        Stand::Open,
    )?;
    // Wind-bent woods inland, clear of the shore.
    let woodland = Woodland {
        cover: 0.35,
        patch: 250.0,
        closure: (0.7, 1.8),
        stature: (0.6, 0.85),
        gaps: 0.1,
        most: 15_000,
        open: (4.0, 0.6),
    };
    stage.sow(Wood {
        grove,
        woodland,
        rooting: Rooting {
            above: Some((4.0, 12.0)),
            ..ANYWHERE
        },
        vantage,
        beneath: None,
        deadfall: None,
    })?;
    moorland(stage, dice, &vantage, Season::Summer)?;
    // The rivers running down to the sea; the sea itself is salt.
    waterside::margins(stage, dice, ((eye.x, eye.z), Season::Summer, None))?;
    let grassland = plants::grassland(dice, Character::Coast, Season::Summer);
    stage.sward = Some(Lawning {
        eye: (eye.x, eye.z),
        grassland,
    });
    let angle = out + dice.sign() * dice.angle(4.0, 16.0);
    let edge = shore(
        &|x, z| land.height(&stage.fields, x, z),
        (centre, land.reach),
        angle,
    );
    if let Some(reach) = edge.filter(|_| dice.chance(0.45)) {
        let at = (
            centre.0 + mathf::sin(angle) * (reach - 25.0),
            centre.1 + mathf::cos(angle) * (reach - 25.0),
        );
        let base = land.height(&stage.fields, at.0, at.1);
        if base > 2.0 {
            let lit = matches!(weather.hour, Hour::Dusk | Hour::Night | Hour::Sunset);
            lighthouse(stage, dice, Vec3::new(at.0, base, at.1), lit)?;
        }
    }
    let fov = dice.angle(46.0, 60.0);
    let view = level_view(&vantage, fov, dice.angle(-6.0, 2.0));
    Some(look(weather, view, 1.0))
}

/// How far the open sea's grid runs before it repeats, and the cells it holds
/// across: a kilometre of a metre's cells, so its tiles stand well apart and
/// its shortest swell spans four of them.
const SEA_PERIOD: f64 = 1024.0;
const SEA_CELLS: usize = 1024;

/// The open sea off a shore facing out along `out`, its swells running in
/// toward it.
fn ocean(stage: &mut Stage, dice: &mut Dice, out: f64) -> Option<u32> {
    let swell = dice.range(0.8, 2.5);
    let shortest = 4.0 * SEA_PERIOD / real(SEA_CELLS);
    let sea = Sea::new(
        SEA_PERIOD,
        (dice.range(30.0, 70.0), shortest, swell),
        (out + PI + dice.range(-0.3, 0.3), dice.range(0.35, 0.8)),
        dice.seed(),
    );
    // Crests whiten from a little above the sea's middling height.
    let foam = Foam {
        crest: 0.35 * swell,
        spread: 0.15 * swell,
        seed: dice.seed(),
    };
    let ripples = Relief::waves(
        Wind {
            slope_variance: dice.range(0.01, 0.04),
            lengths: (dice.range(1.5, 4.0), CAPILLARY),
            spread: 0.9,
            gusts: (dice.range(0.3, 0.6), dice.range(60.0, 200.0)),
        },
        dice.seed(),
    )?;
    let water = stage.water(
        Vec3::new(0.45, 0.09, 0.055),
        Vec3::new(0.002, 0.018, 0.035),
        Some(foam),
        ripples,
    )?;
    stage.sea(sea, (SEA_PERIOD, SEA_CELLS), water)
}

/// A lighthouse standing at `base`, its lamp `lit` or not.
fn lighthouse(stage: &mut Stage, dice: &mut Dice, base: Vec3, lit: bool) -> Option<()> {
    let (a, b) = dice.pick(&[
        (0xE8_E4_DC, 0xB0_24_20),
        (0xE8_E4_DC, 0x1E_1E_22),
        (0xF0_EC_E0, 0xE0_A0_20),
    ])?;
    let paint = stage.coated(
        Pigment::Stripes {
            a: rgb(a),
            b: rgb(b),
            width: dice.range(2.5, 4.0),
        },
        0.5,
    )?;
    let (bottom, height) = (dice.range(2.6, 3.4), dice.range(18.0, 28.0));
    let top = 0.72 * bottom;
    stage.claim((base.x, base.z), bottom + 1.0)?;
    let foot = base - Vec3::UP * 1.0;
    stage.frustum(
        Pose::new(foot, Frame::WORLD),
        (bottom, top, height + 1.0),
        paint,
        true,
    )?;
    let iron = stage.metal(rgb(0x30_30_34), 0.4)?;
    let deck = base + Vec3::UP * height;
    stage.frustum(
        Pose::new(deck, Frame::WORLD),
        (top + 0.6, top + 0.6, 0.3),
        iron,
        true,
    )?;
    stage.ring(deck + Vec3::UP * 1.2, Frame::WORLD, (top + 0.5, 0.05), iron)?;
    let lantern = stage.glass(Vec3::splat(0.96), 0.0)?;
    let room = 0.6 * top;
    stage.frustum(
        Pose::new(deck + Vec3::UP * 0.3, Frame::WORLD),
        (room, room, 2.2),
        lantern,
        true,
    )?;
    stage.dome(deck + Vec3::UP * 2.5, room + 0.1, iron)?;
    let lamp = deck + Vec3::UP * 1.4;
    if lit {
        // A lighthouse lamp of a kilowatt or two, its light shed every way
        // through the lantern's glass.
        stage.orb(
            lamp,
            0.45,
            lumens(0xFF_F0_C8, dice.range(20_000.0, 40_000.0), 0.45),
        )
    } else {
        let brass = stage.metal(super::BRASS, 0.1)?;
        stage
            .ball(lamp - Vec3::UP * 0.45, 0.45, brass, dice)
            .map(|_| ())
    }
}

const DESERT: Climate = Climate {
    hours: &[
        (Hour::Noon, 2),
        (Hour::Day, 4),
        (Hour::Golden, 4),
        (Hour::Sunset, 3),
        (Hour::Night, 1),
    ],
    covers: &[
        (Cover::Clear, 6),
        (Cover::Cirrus, 3),
        (Cover::Fair, 2),
        (Cover::Towering, 1),
    ],
    haze: (1.0, 3.0),
    base: 400.0,
    albedo: 0.35,
};

/// A sea of dunes, or badlands gullied by the rare storm's runoff; now and
/// then a road across it.
pub(super) fn desert(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let dunes = dice.chance(0.55);
    desert_of(stage, dice, dunes)
}

/// A sea of dunes if `dunes`, else gullied badlands.
fn desert_of(stage: &mut Stage, dice: &mut Dice, dunes: bool) -> Option<Composed> {
    let reach = 2400.0;
    let (form, wear, droplets, roughness) = if dunes {
        let form = Landform::Dunes {
            scale: dice.range(70.0, 180.0),
            height: dice.range(8.0, 30.0),
            heading: dice.range(0.0, TAU),
            seed: dice.seed(),
        };
        let still = Wear {
            passes: 0,
            ..GENTLE
        };
        (form, still, (0.0, 0.0, 0.0), 0.3)
    } else {
        let height = dice.range(40.0, 110.0);
        let form = Landform::Hills {
            scale: dice.range(160.0, 320.0),
            height,
            seed: dice.seed(),
        };
        let gullied = Wear {
            passes: 24,
            incision: 1.6e-3,
            creep: 0.02,
            repose: 1.4,
            infill: 0.3,
            strata: Some((dice.range(4.0, 9.0), dice.range(2.5, 4.0))),
        };
        (form, gullied, (0.1, 0.1, 0.2), 2.5)
    };
    let relief = Terrain {
        rim: None,
        form,
        datum: 0.0,
        centre: (0.0, 0.0),
        radius: reach,
        tilt: (0.0, 0.0),
        clearing: None,
    };
    let road = if dice.chance(0.4) {
        Some(roadway(
            dice,
            &[Surface::Tarmac, Surface::Tarmac, Surface::Gravel],
        )?)
    } else {
        None
    };
    let plan = Plan {
        relief,
        reach,
        sea: None,
        wear,
        rivers: None,
        road,
        roughness,
        ridges: if dunes { 0.0 } else { 0.6 },
        droplets: droplets.0,
        cells: (256, 1024),
        nests: nests((700.0, 80.0), (droplets.1, droplets.2)),
        near_water: None,
        horizon: Some(horizon(reach)),
        snow_line: None,
        pond: None,
        growth: if dunes { 0.0 } else { 0.12 },
        seed: dice.seed(),
    };
    let material = if dunes {
        let ground = Ground {
            palette: DUNE.palette(),
            shore: -1e3,
            snow_line: NO_SNOW,
            cliff: 0.3,
            bedding: 2.0,
            seed: dice.seed(),
            road: None,
            floor: None,
        };
        // Wind ripples in the sand: a narrow band of lengths, steep, their
        // crests fading and sharpening across the dune in patches.
        let ripples = Relief::waves(
            Wind {
                slope_variance: dice.range(0.04, 0.09),
                lengths: (dice.range(0.14, 0.2), 0.07),
                spread: 0.35,
                gusts: (dice.range(0.4, 0.8), dice.range(3.0, 10.0)),
            },
            dice.seed(),
        )?;
        stage
            .material(Material::new(Pigment::Ground(ground), Finish::Ground).with_relief(ripples))?
    } else {
        let soil = dice.pick(&[RED_ROCK, SCRUB])?;
        ground(stage, dice, &soil, (-1e3, NO_SNOW, 0.8), 1.2)?
    };
    let build = lay(stage, plan, material, None)?;
    Some(Composed::Landed(Landing {
        build,
        scheme: Scheme::Desert { dunes },
        vantage: None,
    }))
}

/// Beside the land's road, looking down it, if it has a road the eye can
/// stand beside near the land's middle.
fn road_vantage(survey: &Survey<'_>, dice: &mut Dice) -> Option<Vantage> {
    let roads = survey.roads();
    if roads.len() == 0 || !dice.chance(0.65) {
        return None;
    }
    let course = roads.course(0);
    let middle = course.len() / 2 + usize::try_from(dice.count(0, 8)).ok()?;
    let (here, ahead) = (course.get(middle)?, course.get(middle + 12)?);
    let heading = mathf::atan2(ahead.x - here.x, ahead.z - here.z);
    let aside = dice.sign() * (0.5 * here.width + dice.range(0.6, 2.5));
    let spot = (
        here.x + mathf::cos(heading) * aside,
        here.z - mathf::sin(heading) * aside,
    );
    Some(Vantage {
        eye: stand(survey, spot, dice.range(1.5, 2.2)),
        heading: heading + dice.angle(-8.0, 8.0),
    })
}

fn desert_scene(
    stage: &mut Stage,
    dice: &mut Dice,
    land: &Land,
    (vantage, dunes): (Vantage, bool),
) -> Option<Look> {
    stage.keep_open((vantage.eye.x, vantage.eye.z), 4.0)?;
    if dunes {
        erg(stage, dice, land, &vantage)?;
    } else {
        badlands(stage, dice, land, &vantage)?;
    }
    let weather = weather::outdoors(stage, dice, &DESERT, vantage.heading)?;
    let fov = dice.angle(46.0, 60.0);
    let view = view(stage, land, &vantage, (fov, dice.range(0.55, 0.7)));
    Some(look(weather, view, 0.85))
}

/// The place `distance` ahead of the eye, turned `turn` from the way it
/// looks.
fn ahead(vantage: &Vantage, distance: f64, turn: f64) -> (f64, f64) {
    let angle = vantage.heading + turn;
    (
        vantage.eye.x + mathf::sin(angle) * distance,
        vantage.eye.z + mathf::cos(angle) * distance,
    )
}

/// What stands on a sea of dunes: pyramids far off, now and then something
/// nearer that catches the light, and the stones the wind has left bare.
fn erg(stage: &mut Stage, dice: &mut Dice, land: &Land, vantage: &Vantage) -> Option<()> {
    if dice.chance(0.45) {
        let stone = stage.material(Material::new(
            Pigment::Bricks {
                a: rgb(0xD8_BC_8C),
                b: rgb(0xB8_98_68),
                mortar: rgb(0xA0_84_5C),
                size: (1.4, 0.9),
                seed: dice.seed(),
            },
            Finish::Coated { roughness: 0.8 },
        ))?;
        for _ in 0..dice.count(1, 3) {
            let at = ahead(vantage, dice.range(400.0, 1400.0), dice.range(-0.5, 0.5));
            let half = dice.range(40.0, 140.0);
            if !stage.clear(at, 1.2 * half) {
                continue;
            }
            stage.claim(at, 1.2 * half)?;
            let base = Vec3::new(
                at.0,
                land.height(&stage.fields, at.0, at.1) - 0.1 * half,
                at.1,
            );
            stage.pyramid(
                base,
                half,
                51.8_f64.to_radians(),
                dice.range(0.0, TAU),
                stone,
            )?;
        }
    }
    if dice.chance(0.4) {
        let at = ahead(vantage, dice.range(14.0, 40.0), dice.range(-0.3, 0.3));
        stage.claim(at, 2.0)?;
        let base = Vec3::new(at.0, land.height(&stage.fields, at.0, at.1) - 1.0, at.1);
        match dice.count(0, 2) {
            0 => {
                let stone = stage.stone(dice)?;
                let height = dice.range(10.0, 20.0);
                stage.spire(
                    base,
                    (0.08 * height, 0.055 * height, height, 0.07 * height),
                    dice.range(0.0, TAU),
                    stone,
                )?;
            }
            1 => {
                let black = stage.coated(Pigment::Solid(rgb(0x06_06_08)), 0.02)?;
                stage.block(base, Vec3::new(0.5, 3.4, 1.2), dice.range(0.0, TAU), black)?;
            }
            _ => {
                let chrome = stage.metal(Vec3::splat(0.93), 0.0)?;
                let radius = dice.range(1.5, 3.0);
                stage.ball(base + Vec3::UP * 0.8, radius, chrome, dice)?;
            }
        }
    }
    let count = dice.count(25, 60);
    strew(
        stage,
        dice,
        (land, vantage),
        (count, (0.15, 1.4), (4.0, 150.0), 0.8),
        &lies_still,
    )
}

/// What grows and lies on the badlands: saguaros, boulders, and scrub.
fn badlands(stage: &mut Stage, dice: &mut Dice, land: &Land, vantage: &Vantage) -> Option<()> {
    let count = dice.count(40, 90);
    strew(
        stage,
        dice,
        (land, vantage),
        (count, (0.15, 2.6), (4.0, 130.0), 0.9),
        &lies_still,
    )?;
    // Saguaros stand far apart over the flats, scrub between them, on ground
    // too dry for anything green.
    let flat = Rooting {
        upright: (0.8, 0.92),
        bare: 0.5,
        ..ANYWHERE
    };
    let scattered = |cover: f64, closure: (f64, f64), most: u32| Woodland {
        cover,
        patch: 150.0,
        closure,
        stature: (0.6, 1.0),
        gaps: 0.0,
        most,
        open: (3.0, 0.5),
    };
    let cacti = Grove::new(stage, dice, (&[Kind::Saguaro], Season::Summer), Stand::Open)?;
    stage.sow(Wood {
        grove: cacti,
        woodland: scattered(0.7, (16.0, 34.0), stage.densities.woods.cacti),
        rooting: flat,
        vantage: *vantage,
        beneath: None,
        deadfall: None,
    })?;
    let scrub = Grove::new(stage, dice, (&[Kind::Box], Season::Summer), Stand::Open)?;
    stage.sow(Wood {
        grove: scrub,
        woodland: scattered(0.5, (7.0, 16.0), stage.densities.woods.scrub),
        rooting: flat,
        vantage: *vantage,
        beneath: None,
        deadfall: None,
    })
}

const WINTER: Climate = Climate {
    hours: &[
        (Hour::Day, 3),
        (Hour::Golden, 5),
        (Hour::Noon, 1),
        (Hour::Sunset, 2),
        (Hour::Night, 2),
    ],
    covers: &[
        (Cover::Clear, 5),
        (Cover::Fair, 3),
        (Cover::Overcast, 1),
        (Cover::Cirrus, 2),
    ],
    haze: (0.7, 1.8),
    base: 450.0,
    albedo: 0.7,
};

/// Snow over hills, a frozen pond among them.
pub(super) fn winter(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let form = Landform::Hills {
        scale: dice.range(180.0, 400.0),
        height: dice.range(20.0, 70.0),
        seed: dice.seed(),
    };
    let pond = dice.range(40.0, 90.0);
    // The pond's floor lies a few metres below the lowest land about it, so
    // water fills the hollow to where it would spill, frozen, meeting banks
    // all round in whatever shape the land gives them.
    let mut lowest = f64::INFINITY;
    for ring in [1.5 * pond, 2.5 * pond] {
        for step in 0..48u32 {
            let angle = TAU * f64::from(step) / 48.0;
            lowest = lowest.min(form.height(ring * mathf::sin(angle), ring * mathf::cos(angle)));
        }
    }
    let level = lowest - dice.range(3.0, 5.0);
    let reach = 2000.0;
    let relief = Terrain {
        rim: None,
        tilt: tilt(dice, 0.5 * form.height(0.0, 0.0).max(20.0) / reach),
        form,
        datum: 0.0,
        centre: (0.0, 0.0),
        radius: reach,
        clearing: Some((level, pond)),
    };
    let plan = Plan {
        relief,
        reach,
        sea: None,
        wear: Wear {
            incision: 1.0e-4,
            ..GENTLE
        },
        rivers: None,
        road: None,
        roughness: 0.6,
        // Snow lies smooth over whatever the ground beneath it is, and
        // softens what water cut in it.
        ridges: 0.0,
        droplets: 0.01,
        cells: (256, 1024),
        nests: nests((700.0, 72.0), (0.015, 0.0)),
        near_water: None,
        horizon: Some(horizon(reach)),
        snow_line: Some(-1e3),
        pond: Some(((0.0, 0.0), 1.3 * pond)),
        growth: 1.0,
        seed: dice.seed(),
    };
    let material = ground(stage, dice, &SNOWFIELD, (-1e3, -1e3, 0.6), 2.0)?;
    let ice = stage.material(
        Material::new(
            Pigment::Solid(Vec3::ONE),
            Finish::Glass {
                ior: 1.31,
                absorb: Vec3::new(1.4, 0.6, 0.35),
                glow: Vec3::ZERO,
                roughness: dice.range(0.08, 0.16),
                dispersion: 0.0,
                foam: None,
            },
        )
        .with_relief(Relief::grain(0.005, 0.8, dice.seed())),
    )?;
    let build = lay(stage, plan, material, Some(ice))?;
    stage.keep_open((0.0, 0.0), 1.6 * pond)?;
    Some(Composed::Landed(Landing {
        build,
        scheme: Scheme::Winter { pond },
        vantage: None,
    }))
}

fn winter_scene(
    stage: &mut Stage,
    dice: &mut Dice,
    land: &Land,
    (vantage, pond): (Vantage, f64),
) -> Option<Look> {
    let grove = Grove::new(
        stage,
        dice,
        (&[Kind::Spruce, Kind::Spruce, Kind::Birch], Season::Winter),
        Stand::Close,
    )?;
    let eye = vantage.eye;
    stage.keep_open((eye.x, eye.z), 3.0)?;
    // Trees crowd the banks, never the ice.
    let woodland = Woodland {
        cover: 0.7,
        patch: 250.0,
        closure: (0.5, 1.1),
        stature: (0.6, 0.9),
        gaps: 0.2,
        most: stage.densities.woods.winter,
        open: (3.0, 0.5),
    };
    let deadfall = deadfall(
        stage,
        dice,
        (&[Kind::Spruce, Kind::Birch], Season::Winter),
        (12.0, 3.0, 5.0),
    )?;
    stage.sow(Wood {
        grove,
        woodland,
        rooting: Rooting {
            upright: (0.72, 0.86),
            bare: 0.85,
            clearing: Some((land.centre, 1.6 * pond)),
            ..ANYWHERE
        },
        vantage,
        beneath: None,
        deadfall: Some(deadfall),
    })?;
    waterside::margins(stage, dice, ((eye.x, eye.z), Season::Winter, None))?;
    if dice.chance(0.3) {
        let at = ahead(&vantage, 5.0, 0.4);
        let base = land.height(&stage.fields, at.0, at.1);
        snowman(
            stage,
            dice,
            Vec3::new(at.0, base, at.1),
            vantage.heading + PI,
        )?;
    }
    let weather = weather::outdoors(stage, dice, &WINTER, vantage.heading)?;
    let fov = dice.angle(46.0, 60.0);
    let view = level_view(&vantage, fov, dice.angle(-4.0, 4.0));
    Some(look(weather, view, 0.72))
}

/// A snowman at `base`, looking toward `facing`.
fn snowman(stage: &mut Stage, dice: &mut Dice, base: Vec3, facing: f64) -> Option<()> {
    let snow =
        stage.material(
            Material::new(Pigment::Solid(rgb(0xF4_F6_FA)), Finish::Matte)
                .with_relief(Relief::grain(0.105, 8.0, dice.seed())),
        )?;
    stage.claim((base.x, base.z), 0.6)?;
    let mut level = base.y - 0.1;
    let mut head = Vec3::ZERO;
    for radius in [0.5, 0.36, 0.25] {
        stage.ball(Vec3::new(base.x, level, base.z), radius, snow, dice)?;
        head = Vec3::new(base.x, level + radius, base.z);
        level += 1.7 * radius;
    }
    let look = direction(facing, 0.0, 0.0);
    let side = look.cross(Vec3::UP).normalized();
    let coal = stage.coated(Pigment::Solid(rgb(0x10_10_12)), 0.4)?;
    for sign in [-1.0, 1.0] {
        let eye = head + look * 0.21 + side * (0.08 * sign) + Vec3::UP * 0.06;
        stage.add(
            Shape::Sphere {
                centre: eye,
                radius: 0.028,
            },
            coal,
            Pose::new(eye, Frame::WORLD),
            true,
        )?;
    }
    let carrot = stage.coated(Pigment::Solid(rgb(0xE0_6A_1A)), 0.5)?;
    stage.frustum(
        Pose::new(head + look * 0.2, Frame::WORLD.aligning(Vec3::UP, look)),
        (0.035, 0.0, 0.2),
        carrot,
        true,
    )?;
    Some(())
}

const LAGOON: Climate = Climate {
    hours: &[
        (Hour::Sunset, 5),
        (Hour::Golden, 3),
        (Hour::Dusk, 1),
        (Hour::Day, 1),
    ],
    covers: &[
        (Cover::Clear, 3),
        (Cover::Fair, 3),
        (Cover::Cirrus, 3),
        (Cover::Mackerel, 2),
        (Cover::Broken, 1),
    ],
    haze: (1.4, 3.0),
    base: 0.0,
    albedo: 0.12,
};

pub(super) fn lagoon(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let yaw = dice.range(0.0, TAU);
    let depth = dice.range(0.35, 0.8);
    let sand = stage.material(Material::new(
        Pigment::Solid(rgb(0xD8_C8_A0)),
        Finish::Matte,
    ))?;
    stage.ground(-depth, sand)?;
    let stone = stage.material(
        Material::new(
            Pigment::Marble {
                base: rgb(0xC0_AC_90),
                vein: rgb(0x8A_78_62),
                scale: 1.6,
                seed: dice.seed(),
            },
            Finish::Coated { roughness: 0.7 },
        )
        .with_relief(Relief::grain(0.075, 9.0, dice.seed())),
    )?;
    pillars(stage, dice, depth, stone)?;
    if dice.chance(0.6) {
        if let Some((x, z)) = stage.place(dice, ((0.0, 0.0), 3.0), 1.2) {
            let major = dice.range(0.9, 1.4);
            let arch = if dice.chance(0.5) {
                stone
            } else {
                stage.metal(GOLD, 0.15)?
            };
            let upright = Frame::turned(dice.range(0.0, TAU), FRAC_PI_2);
            stage.ring(
                Vec3::new(x, dice.range(-0.1, 0.25), z),
                upright,
                (major, major * 0.16),
                arch,
            )?;
        }
    }
    let facing = yaw + PI;
    let island = if dice.chance(0.45) {
        Some(island(stage, dice, facing)?)
    } else {
        None
    };
    let weather = weather::outdoors(stage, dice, &LAGOON, facing)?;
    let ripples = Relief::waves(
        Wind {
            slope_variance: dice.range(5e-4, 6e-3),
            lengths: (dice.range(1.4, 3.2), CAPILLARY),
            spread: 0.7,
            gusts: (dice.range(0.2, 0.5), dice.range(30.0, 90.0)),
        },
        dice.seed(),
    )?;
    let water = stage.water(
        Vec3::new(0.3, 0.06, 0.045) * dice.range(0.8, 1.5),
        Vec3::new(0.004, 0.03, 0.03),
        None,
        ripples,
    )?;
    stage.ground(0.0, water)?;
    let look = Look {
        sky: weather.sky,
        exposure: weather.exposure,
        view: View::Framed {
            yaw,
            elevation: dice.angle(4.0, 12.0),
            fov: dice.angle(42.0, 55.0),
            fill: 0.9,
            aperture: 0.0,
        },
    };
    Some(match island {
        Some(build) => Composed::Landed(Landing {
            build,
            scheme: Scheme::Set(Set {
                look,
                planting: None,
            }),
            vantage: None,
        }),
        None => Composed::Seen(look),
    })
}

/// An island off across the water, the way `facing` looks: worn by its
/// streams, and far enough off to need no near land.
fn island(stage: &mut Stage, dice: &mut Dice, facing: f64) -> Option<Build> {
    let distance = dice.range(600.0, 1500.0);
    let angle = facing + dice.range(-0.5, 0.5);
    let centre = (mathf::sin(angle) * distance, mathf::cos(angle) * distance);
    let radius = dice.range(150.0, 400.0);
    let form = Landform::Island {
        centre,
        radius,
        height: dice.range(30.0, 120.0),
        seed: dice.seed(),
    };
    let reach = 1.4 * radius;
    let plan = Plan {
        relief: Terrain {
            rim: Some(form.lowest()),
            form,
            datum: 0.0,
            centre,
            radius: reach,
            tilt: (0.0, 0.0),
            clearing: None,
        },
        reach,
        sea: Some(0.0),
        wear: Wear {
            passes: 12,
            incision: 6.0e-4,
            creep: 0.06,
            repose: 1.0,
            infill: 0.3,
            strata: None,
        },
        rivers: None,
        road: None,
        roughness: 1.0,
        ridges: 0.5,
        droplets: 0.04,
        cells: (128, 512),
        nests: [None; NESTS],
        near_water: None,
        horizon: None,
        snow_line: None,
        pond: None,
        growth: 1.0,
        seed: dice.seed(),
    };
    let soil = dice.pick(&[GREEN, VOLCANIC])?;
    let material = ground(stage, dice, &soil, (1.5, NO_SNOW, 0.72), 3.0)?;
    lay(stage, plan, material, None)
}

/// Stone pillars rising from a lagoon's floor `depth` below its surface,
/// some crowned with something precious.
fn pillars(stage: &mut Stage, dice: &mut Dice, depth: f64, stone: usize) -> Option<()> {
    for _ in 0..dice.count(3, 6) {
        let radius = dice.range(0.22, 0.45);
        let Some((x, z)) = stage.place(dice, ((0.0, 0.0), 4.0), radius + 0.2) else {
            continue;
        };
        let height = depth + dice.range(0.4, 2.4);
        let base = Vec3::new(x, -depth, z);
        stage.post(base, (radius * 1.08, radius, height), stone, dice)?;
        if dice.chance(0.65) {
            let crown = stage.precious(dice)?;
            stage.ball(
                base + Vec3::UP * height,
                radius * dice.range(0.8, 1.3),
                crown,
                dice,
            )?;
        }
    }
    Some(())
}

const CANYON: Climate = Climate {
    hours: &[
        (Hour::Day, 3),
        (Hour::Golden, 5),
        (Hour::Sunset, 2),
        (Hour::Noon, 1),
        (Hour::Night, 1),
    ],
    covers: &[
        (Cover::Clear, 4),
        (Cover::Fair, 3),
        (Cover::Cirrus, 2),
        (Cover::Towering, 1),
        (Cover::Broken, 1),
    ],
    haze: (0.8, 1.8),
    base: 1100.0,
    albedo: 0.3,
};

/// Terraced mesas of hard caps over soft beds, the canyons between them cut
/// by a river.
pub(super) fn canyon(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let height = dice.range(80.0, 260.0);
    let steps = f64::from(dice.count(3, 5));
    let reach = 3000.0;
    let relief = Terrain {
        form: Landform::Mesas {
            scale: dice.range(250.0, 600.0),
            height,
            steps,
            seed: dice.seed(),
        },
        datum: 0.0,
        centre: (0.0, 0.0),
        radius: reach,
        rim: None,
        tilt: (0.0, 0.0),
        clearing: None,
    };
    let bed = height / steps;
    let plan = Plan {
        relief,
        reach,
        sea: None,
        wear: Wear {
            passes: 20,
            incision: 1.4e-3,
            creep: 0.015,
            repose: 1.6,
            infill: 0.3,
            strata: Some((bed, 4.0)),
        },
        rivers: Some(Rivers {
            catchment: 6.0e5,
            width: 7.0,
            meander: 0.8,
            flowing: 1.0,
            ledges: 0.0,
            outcrops: 0.4,
        }),
        road: None,
        roughness: 2.5,
        ridges: 0.6,
        droplets: 0.06,
        cells: (256, 1024),
        nests: nests((700.0, 90.0), (0.08, 0.12)),
        near_water: None,
        horizon: Some(horizon(reach)),
        snow_line: None,
        pond: None,
        growth: 0.15,
        seed: dice.seed(),
    };
    let material = ground(stage, dice, &RED_ROCK, (-1e3, NO_SNOW, 0.8), bed / 2.0)?;
    let water = river(stage, dice)?;
    let build = lay(stage, plan, material, Some(water))?;
    Some(Composed::Landed(Landing {
        build,
        scheme: Scheme::Canyon { terrace: 0.5 * bed },
        vantage: None,
    }))
}

/// How many spots a vantage is chosen among.
const SPOTS: usize = 32;

/// Of a few level, dry spots on the far land, those on the lowest ground
/// among them — within `terrace` of it — and of those the one looking
/// furthest before the land stands across the view: the eye `rise` above
/// it, and the way it looks. Where no spot is level and dry, the lowest one
/// seen, looking its most open way, the eye clear of any water.
fn terrace_vantage(survey: &Survey<'_>, dice: &mut Dice, terrace: f64, rise: f64) -> Vantage {
    let (centre, reach) = survey.extent();
    let spread = 0.3 * reach;
    let height = |x: f64, z: f64| survey.height(x, z);
    let mut spots = [None; SPOTS];
    let mut lowest = (centre, f64::INFINITY);
    for spot in &mut spots {
        let at = (
            centre.0 + dice.range(-spread, spread),
            centre.1 + dice.range(-spread, spread),
        );
        let lie = survey.lie(at.0, at.1);
        if lie.height < lowest.1 {
            lowest = (at, lie.height);
        }
        if !survey.wet_at(at.0, at.1) && lie.upright >= 0.9 {
            *spot = Some((at, lie.height));
        }
    }
    let floor = spots
        .iter()
        .flatten()
        .fold(f64::INFINITY, |floor, &(_, lie)| floor.min(lie));
    let mut best: Option<(f64, (f64, f64), f64)> = None;
    for &(at, lie) in spots
        .iter()
        .flatten()
        .filter(|(_, lie)| *lie <= floor + terrace)
    {
        let eye = Vec3::new(at.0, lie + rise, at.1);
        for _ in 0..8 {
            let heading = dice.range(0.0, TAU);
            let open = openness(&height, eye, heading, (8.0_f64.to_radians(), 2500.0));
            if best.is_none_or(|(most, ..)| open > most) {
                best = Some((open, at, heading));
            }
        }
    }
    if let Some((_, at, heading)) = best {
        return Vantage {
            eye: stand(survey, at, rise),
            heading,
        };
    }
    let eye = stand(survey, lowest.0, rise);
    Vantage {
        eye,
        heading: open_heading(&height, dice, eye, 0.6 * reach, 8),
    }
}

fn canyon_scene(stage: &mut Stage, dice: &mut Dice, land: &Land, vantage: Vantage) -> Option<Look> {
    stage.keep_open((vantage.eye.x, vantage.eye.z), 4.0)?;
    let count = dice.count(40, 90);
    strew(
        stage,
        dice,
        (land, &vantage),
        (count, (0.2, 3.2), (4.0, 120.0), 0.8),
        &lies_still,
    )?;
    // Olives and scrub on the benches, and along the watercourses.
    let grove = Grove::new(
        stage,
        dice,
        (&[Kind::Olive, Kind::Box], Season::Summer),
        Stand::Open,
    )?;
    let woodland = Woodland {
        cover: 0.45,
        patch: 200.0,
        closure: (1.4, 3.5),
        stature: (0.6, 0.9),
        gaps: 0.0,
        most: stage.densities.woods.canyon,
        open: (3.0, 0.5),
    };
    stage.sow(Wood {
        grove,
        woodland,
        rooting: Rooting {
            upright: (0.85, 0.95),
            streams: 0.2,
            ..ANYWHERE
        },
        vantage,
        beneath: None,
        deadfall: None,
    })?;
    let eye = vantage.eye;
    waterside::margins(stage, dice, ((eye.x, eye.z), Season::Summer, None))?;
    let weather = weather::outdoors(stage, dice, &CANYON, vantage.heading)?;
    let fov = dice.angle(48.0, 62.0);
    let view = view(stage, land, &vantage, (fov, dice.range(0.35, 0.5)));
    Some(look(weather, view, 0.95))
}

/// A river valley running along `heading`, falling gently along its line so
/// its river runs down it, a road crossing it on a stone bridge.
pub(super) fn valley(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let heading = dice.range(0.0, TAU);
    let height = dice.range(80.0, 180.0);
    let floor = dice.range(90.0, 180.0);
    let reach = 4000.0;
    let fall = dice.range(0.003, 0.007);
    let relief = Terrain {
        form: Landform::Valley {
            heading,
            floor,
            height,
            seed: dice.seed(),
        },
        datum: 0.0,
        centre: (0.0, 0.0),
        radius: reach,
        rim: None,
        tilt: (-fall * mathf::sin(heading), -fall * mathf::cos(heading)),
        clearing: None,
    };
    let road = Roadway {
        heading: heading + FRAC_PI_2 + dice.range(-0.4, 0.4),
        ..roadway(dice, &[Surface::Tarmac, Surface::Gravel, Surface::Track])?
    };
    let plan = Plan {
        relief,
        reach,
        sea: None,
        wear: Wear {
            passes: 24,
            incision: 1.2e-3,
            creep: 0.07,
            repose: 0.9,
            infill: 0.35,
            strata: None,
        },
        rivers: Some(Rivers {
            catchment: 3.0e5,
            width: 4.5,
            meander: 1.4,
            flowing: 1.0,
            ledges: 0.0,
            outcrops: 0.15,
        }),
        road: Some(road),
        roughness: 1.2,
        ridges: 0.35,
        droplets: 0.05,
        cells: (256, 1024),
        nests: nests((700.0, 80.0), (0.08, 0.12)),
        near_water: None,
        horizon: Some(horizon(reach)),
        snow_line: None,
        pond: None,
        growth: 1.0,
        seed: dice.seed(),
    };
    let soil = dice.pick(&[GREEN, GREEN, GOLDEN, HIGHLAND])?;
    let material = ground(stage, dice, &soil, (-1e3, NO_SNOW, 0.72), 4.0)?;
    let water = river(stage, dice)?;
    let build = lay(stage, plan, material, Some(water))?;
    Some(Composed::Landed(Landing {
        build,
        scheme: Scheme::Valley,
        vantage: None,
    }))
}

fn valley_scene(stage: &mut Stage, dice: &mut Dice, land: &Land, vantage: Vantage) -> Option<Look> {
    bridges(stage, dice, land)?;
    let season = dice.pick(&[
        Season::Spring,
        Season::Summer,
        Season::Summer,
        Season::Autumn { fallen: 20 },
    ])?;
    let kinds: &[Kind] = if dice.chance(0.5) {
        &[Kind::Willow, Kind::Poplar, Kind::Oak]
    } else {
        &[Kind::Oak, Kind::Birch, Kind::Willow]
    };
    let grove = Grove::new(stage, dice, (kinds, season), Stand::Open)?;
    let eye = vantage.eye;
    stage.keep_open((eye.x, eye.z), 4.0)?;
    let grassland = Grassland {
        fallen: grove
            .first()
            .and_then(|grown| Fallen::from(&grown, season, 0.5)),
        ..plants::grassland(dice, Character::Meadow, season)
    };
    // Trees line the river and gather in the valley's woods.
    let woodland = Woodland {
        cover: dice.range(0.1, 0.35),
        patch: 300.0,
        closure: (0.6, 1.6),
        stature: (0.65, 0.9),
        gaps: 0.1,
        most: stage.densities.woods.valley,
        open: (4.0, 0.6),
    };
    stage.sow(Wood {
        grove,
        woodland,
        rooting: Rooting {
            streams: 1.4,
            ..ANYWHERE
        },
        vantage,
        beneath: None,
        deadfall: None,
    })?;
    waterside::margins(stage, dice, ((eye.x, eye.z), season, None))?;
    stage.sward = Some(Lawning {
        eye: (eye.x, eye.z),
        grassland,
    });
    let count = dice.count(8, 24);
    let riverside = |lie: &Lie| lies_still(lie) && lie.wet > 0.3;
    strew(
        stage,
        dice,
        (land, &vantage),
        (count, (0.2, 1.4), (6.0, 200.0), 1.0),
        &riverside,
    )?;
    let weather = weather::outdoors(stage, dice, &MEADOW, vantage.heading)?;
    let fov = dice.angle(44.0, 58.0);
    // Framed on its bridge, where it has one, a little below the middle of
    // the picture, so the river runs off either side of it.
    let view = match land.crossings.first() {
        Some(crossing) => {
            let bridge = Vec3::new(
                f64::midpoint(crossing.from.x, crossing.to.x),
                crossing.deck,
                f64::midpoint(crossing.from.z, crossing.to.z),
            );
            let lift = mathf::tan(0.12 * fov) * (bridge - eye).length();
            View::Placed {
                eye,
                target: bridge + Vec3::UP * lift,
                fov,
                aperture: 0.0,
            }
        }
        None => view(stage, land, &vantage, (fov, dice.range(0.5, 0.66))),
    };
    Some(look(weather, view, 1.0))
}

/// A narrow valley falling steeply enough along `heading` that the stream
/// down it runs over stones, its bed of one rock, and the water low in its
/// channel in a dry season.
pub(super) fn stream(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let lithology = dice.pick(&Lithology::ALL)?;
    let (ledges, outcrops) = bedrock(lithology);
    let heading = dice.range(0.0, TAU);
    let reach = 2600.0;
    let fall = dice.range(0.004, 0.014);
    let relief = Terrain {
        form: Landform::Valley {
            heading,
            floor: dice.range(50.0, 120.0),
            height: dice.range(60.0, 160.0),
            seed: dice.seed(),
        },
        datum: 0.0,
        centre: (0.0, 0.0),
        radius: reach,
        rim: None,
        tilt: (-fall * mathf::sin(heading), -fall * mathf::cos(heading)),
        clearing: None,
    };
    let bed = &stage.densities.bed;
    let plan = Plan {
        relief,
        reach,
        sea: None,
        wear: Wear {
            passes: 20,
            incision: 1.2e-3,
            creep: 0.07,
            repose: 0.9,
            infill: 0.3,
            strata: None,
        },
        rivers: Some(Rivers {
            catchment: 1.5e5,
            width: 2.2,
            meander: 0.9,
            flowing: dice.range(0.28, 0.5),
            ledges,
            outcrops,
        }),
        road: None,
        roughness: 1.0,
        ridges: 0.4,
        droplets: 0.05,
        cells: (256, 1024),
        nests: nests((500.0, 24.0), (0.08, 0.1)),
        near_water: Some(NearWater {
            reach: bed.water.0,
            cells: bed.water.1,
        }),
        horizon: Some(horizon(reach)),
        snow_line: None,
        pond: None,
        growth: 1.0,
        seed: dice.seed(),
    };
    let soil = dale(lithology);
    let bedding = match lithology {
        Lithology::Slate => 0.3,
        Lithology::Granite => 6.0,
        Lithology::Sandstone | Lithology::Limestone => 1.5,
    };
    let material = ground(stage, dice, &soil, (-1e3, NO_SNOW, 0.72), bedding)?;
    let water = brook_water(stage, dice)?;
    let build = lay(stage, plan, material, Some(water))?;
    Some(Composed::Landed(Landing {
        build,
        scheme: Scheme::Stream { lithology },
        vantage: None,
    }))
}

/// How often a stream over `lithology` gathers its fall at a ledge, and how
/// much of its banks the rock outcrops in: bedded rock steps and shows most,
/// granite in boulder-strewn steps and tors.
fn bedrock(lithology: Lithology) -> (f64, f64) {
    match lithology {
        Lithology::Slate | Lithology::Limestone => (0.35, 0.3),
        Lithology::Sandstone => (0.3, 0.3),
        Lithology::Granite => (0.15, 0.25),
    }
}

/// A green upland dale's ground, its rock `lithology`'s.
fn dale(lithology: Lithology) -> Soil {
    let (rock, strata, lichen) = match lithology {
        Lithology::Granite => (0x8C_88_82, 0x6E_6A_66, 0xA4_A6_84),
        Lithology::Sandstone => (0xA0_84_62, 0x84_68_4A, 0xAC_A6_80),
        Lithology::Limestone => (0xB2_AE_A4, 0x96_92_88, 0xB0_B2_92),
        Lithology::Slate => (0x5C_62_6A, 0x48_4C_54, 0x8C_92_76),
    };
    Soil {
        rock,
        strata,
        lichen,
        ..HIGHLAND
    }
}

/// A stream's water: clear, tinted as its peat or its chalk has it, the
/// breeze down its sheltered valley lightly rippling it, so its own flow
/// over its stones roughens its riffles far more than its pools.
fn brook_water(stage: &mut Stage, dice: &mut Dice) -> Option<usize> {
    let (absorb, glow) = dice.pick(&[
        (Vec3::new(0.3, 0.15, 0.2), Vec3::new(0.003, 0.01, 0.008)),
        (Vec3::new(0.5, 0.22, 0.12), Vec3::new(0.004, 0.008, 0.006)),
        (Vec3::new(0.22, 0.1, 0.14), Vec3::new(0.002, 0.01, 0.012)),
    ])?;
    let ripples = Relief::waves(
        Wind {
            slope_variance: dice.range(1e-3, 5e-3),
            lengths: (dice.range(0.3, 0.9), CAPILLARY),
            spread: 0.8,
            gusts: (dice.range(0.3, 0.6), dice.range(3.0, 10.0)),
        },
        dice.seed(),
    )?;
    stage.water(absorb, glow, None, ripples)
}

/// A place at the edge of one of `survey`'s streams, a few metres across,
/// on the margin its low water leaves bare — on the bar across from the
/// deep water most often — looking up or down along it toward a ledge or a
/// riffle where one lies ahead; `None` when the land has no such stream.
fn stream_vantage(survey: &Survey<'_>, dice: &mut Dice, runner: &dyn JobRunner) -> Option<Vantage> {
    let rivers = survey.rivers();
    let form = survey.form()?;
    let (centre, reach) = survey.extent();
    let up = dice.chance(0.6);
    let ahead = if up { -1.0 } else { 1.0 };
    // Each mark well along its course, with room to look along it, and how
    // far along the course it lies.
    let mut marks = Vec::new();
    for course in 0..rivers.len() {
        let course_marks = rivers.course(course);
        let mut along = 0.0;
        for (index, pair) in course_marks.windows(2).enumerate() {
            let (mark, next) = (pair[0], pair[1]);
            let here = along;
            along += mathf::hypot(next.x - mark.x, next.z - mark.z);
            if index >= 4 && index + 5 <= course_marks.len() {
                marks.try_reserve(1).ok()?;
                marks.push((course, index, here));
            }
        }
    }
    // Best near the land's middle where it runs as broad as a stream,
    // between dry banks rather than through a lake, and running; each
    // mark's standing weighed across the runner.
    let mut placed = fallible::collected(marks.len(), core::iter::repeat(0u32))?;
    crate::band::for_each(runner, &mut placed, (0, MARKS), &|band, placed| {
        let weighed = marks.get(band * MARKS..).unwrap_or_default();
        for (&(course, index, _), slot) in weighed.iter().zip(placed) {
            let course_marks = rivers.course(course);
            let (Some(&mark), Some(&next)) = (course_marks.get(index), course_marks.get(index + 1))
            else {
                continue;
            };
            let inner = mathf::hypot(mark.x - centre.0, mark.z - centre.1) < 0.55 * reach;
            let sized = (2.5..9.0).contains(&mark.width);
            let running = (RUNNING.0..RUNNING.1).contains(&mark.fall);
            *slot = (u32::from(inner) * 2 + u32::from(sized)) * 4
                + 2 * u32::from(banked(survey, (mark, next)))
                + u32::from(running);
        }
    });
    // Toward a ledge or a riffle ahead, which breaks a tie among the best
    // placed alone: each of those ranked one more than what lies ahead of it,
    // across the runner, and every other none.
    let most = placed.iter().copied().max()?;
    let ahead_of = |course: usize, here: f64| {
        let mut interest = 0;
        for step in 0..12 {
            let at = here + ahead * (8.0 + 2.0 * f64::from(step));
            let Some(station) = Station::on(rivers, course, at) else {
                continue;
            };
            let section = Section::new(&station, &form);
            // Nothing ahead ranks above a ledge.
            if section.ledge > 0.5 {
                return 2;
            }
            interest = interest.max(u32::from(section.pool < 0.15));
        }
        interest
    };
    crate::band::for_each(runner, &mut placed, (0, MARKS), &|band, placed| {
        let ranked = marks.get(band * MARKS..).unwrap_or_default();
        for (&(course, _, here), slot) in ranked.iter().zip(placed) {
            *slot = if *slot == most {
                1 + ahead_of(course, here)
            } else {
                0
            };
        }
    });
    let best = placed.iter().copied().max()?;
    let mut spots = Vec::new();
    for (&(course, _, here), _) in marks.iter().zip(&placed).filter(|(_, &rank)| rank == best) {
        spots.try_reserve(1).ok()?;
        spots.push((course, here));
    }
    let (course, along) = dice.pick(&spots)?;
    let section = Section::new(&Station::on(rivers, course, along)?, &form);
    let mark = rivers.at(course, along)?;
    let (downstream, left) = rivers.way(course, along)?;
    // The bar rises across from where the deep water swings.
    let bar = if section.thalweg >= 0.0 { -1.0 } else { 1.0 };
    let side = if dice.chance(0.75) { bar } else { -bar };
    let edge = side * section.edge(side);
    let back = edge + dice.range(0.15, 0.6) * (section.half - edge).max(0.2);
    let spot = (mark.x + side * left.0 * back, mark.z + side * left.1 * back);
    // Looking along the stream, turned a little out over the water.
    let turn = dice.range(0.05, 0.3);
    let (cos, sin) = (mathf::cos(turn), mathf::sin(turn));
    let look = (
        ahead * downstream.0 * cos - side * left.0 * sin,
        ahead * downstream.1 * cos - side * left.1 * sin,
    );
    Some(Vantage {
        eye: stand(survey, spot, dice.range(1.2, 1.7)),
        heading: mathf::atan2(look.0, look.1),
    })
}

/// The falls between which a stream's eye looks over running water: not a
/// level reach where it pools still, nor a steep one where it steps down.
const RUNNING: (f64, f64) = (0.003, 0.03);

/// The marks of a land's streams a core weighs at a time for a stream's eye.
const MARKS: usize = 512;

/// Whether the stream running from `mark` to `next` on `survey`'s land runs
/// between dry banks, no lake or marsh standing beside it.
fn banked(survey: &Survey<'_>, (mark, next): (Mark, Mark)) -> bool {
    let (dx, dz) = (next.x - mark.x, next.z - mark.z);
    let length = mathf::hypot(dx, dz).max(1e-6);
    let beyond = 0.5 * mark.width + 2.5;
    [-1.0, 1.0].iter().all(|&side| {
        let (x, z) = (
            mark.x - side * dz / length * beyond,
            mark.z + side * dx / length * beyond,
        );
        survey.water(x, z).is_none()
    })
}

fn stream_scene(
    stage: &mut Stage,
    dice: &mut Dice,
    land: &Land,
    (vantage, lithology): (Vantage, Lithology),
) -> Option<Look> {
    let eye = vantage.eye;
    let brook = land
        .rivers
        .nearest(eye.x, eye.z)
        .zip(land.form)
        .map(|(near, form)| Brook {
            course: near.course,
            station: Station::of(&near),
            form,
            run: near.run,
            lithology,
        });
    let season = dice.pick(&[
        Season::Spring,
        Season::Summer,
        Season::Summer,
        Season::Autumn { fallen: 30 },
    ])?;
    let kinds: &[Kind] = if dice.chance(0.5) {
        &[Kind::Willow, Kind::Birch, Kind::Hazel]
    } else {
        &[Kind::Oak, Kind::Willow, Kind::Hazel]
    };
    // What the floods broke from the trees along it lies lodged in its bed;
    // an eye that found no stream looks over its dale without one.
    if let Some(brook) = brook {
        let drift = Drift::new(stage, dice, (*kinds.first()?, season))?;
        stones::bed(
            stage,
            dice,
            (brook, (eye.x, eye.z), vantage.heading),
            Some(drift),
        )?;
    }
    let grove = Grove::new(stage, dice, (kinds, season), Stand::Open)?;
    stage.keep_open((eye.x, eye.z), 3.0)?;
    let grassland = Grassland {
        fallen: grove
            .first()
            .and_then(|grown| Fallen::from(&grown, season, 0.6)),
        ..plants::grassland(dice, Character::Meadow, season)
    };
    let woodland = Woodland {
        cover: dice.range(0.2, 0.45),
        patch: 160.0,
        closure: (0.7, 1.6),
        stature: (0.6, 0.9),
        gaps: 0.1,
        most: stage.densities.woods.valley,
        open: (3.0, 0.6),
    };
    stage.sow(Wood {
        grove,
        woodland,
        rooting: Rooting {
            streams: 1.6,
            ..ANYWHERE
        },
        vantage,
        beneath: None,
        deadfall: None,
    })?;
    // Its banks are grass down to the margin its floods scour bare; reeds
    // and pondweed take only its slack water and silt, crowfoot its runs.
    waterside::margins(stage, dice, ((eye.x, eye.z), season, None))?;
    stage.sward = Some(Lawning {
        eye: (eye.x, eye.z),
        grassland,
    });
    let weather = weather::outdoors(stage, dice, &MEADOW, vantage.heading)?;
    let fov = dice.angle(50.0, 62.0);
    let pitch = -dice.range(0.16, 0.34);
    Some(look(weather, level_view(&vantage, fov, pitch), 1.0))
}

/// A monumental abstract sculpture standing out in a meadow, a sea of dunes
/// or an alpine valley.
pub(super) fn sculpture(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let composed = match dice.count(0, 2) {
        0 => meadow(stage, dice)?,
        1 => desert_of(stage, dice, true)?,
        _ => alpine(stage, dice)?,
    };
    let Composed::Landed(landing) = composed else {
        return Some(composed);
    };
    let grounds = match landing.scheme {
        Scheme::Desert { .. } => Grounds::Dunes,
        Scheme::Alpine { lake } => Grounds::Alpine { lake },
        _ => Grounds::Meadow,
    };
    Some(Composed::Landed(Landing {
        scheme: Scheme::Sculpture(grounds),
        ..landing
    }))
}

/// The pieces a sculpture is made of.
#[derive(Copy, Clone, Debug)]
enum Piece {
    /// Great rings standing interlocked on edge.
    Rings,
    /// A ring of polished monoliths.
    Monoliths,
    /// A cluster of orbs hovering over the ground.
    Orbs,
}

/// A sculpture set in the middle distance ahead of `vantage`, claiming its
/// ground so nothing grows over it.
fn sculpture_piece(
    stage: &mut Stage,
    dice: &mut Dice,
    land: &Land,
    vantage: &Vantage,
) -> Option<()> {
    let piece = dice.pick(&[Piece::Rings, Piece::Monoliths, Piece::Orbs])?;
    let distance = dice.range(28.0, 60.0);
    let (x, z) = ahead(vantage, distance, dice.range(-0.22, 0.22));
    let ground = land.height(&stage.fields, x, z);
    let facing = vantage.heading + PI;
    match piece {
        Piece::Rings => {
            let chrome = stage.metal(Vec3::splat(0.93), 0.0)?;
            let gold = stage.metal(GOLD, dice.range(0.0, 0.12))?;
            let count = dice.count(2, 4);
            let major = dice.range(3.5, 7.0);
            stage.claim((x, z), 1.2 * major)?;
            for index in 0..count {
                let turn =
                    facing + f64::from(index) * PI / f64::from(count) + dice.range(-0.15, 0.15);
                let lean = dice.range(-0.25, 0.25);
                let material = if index % 2 == 0 { chrome } else { gold };
                let centre = Vec3::new(x, ground + major * dice.range(0.82, 0.95), z);
                let frame = Frame::turned(turn, FRAC_PI_2 + lean);
                stage.ring(
                    centre,
                    frame,
                    (major, major * dice.range(0.07, 0.12)),
                    material,
                )?;
            }
        }
        Piece::Monoliths => {
            let polished = if dice.chance(0.5) {
                stage.coated(Pigment::Solid(rgb(0x05_05_06)), 0.015)?
            } else {
                stage.metal(Vec3::splat(0.92), 0.02)?
            };
            let count = dice.count(5, 9);
            let radius = dice.range(6.0, 11.0);
            stage.claim((x, z), radius + 2.0)?;
            let height = dice.range(4.0, 7.0);
            for index in 0..count {
                let angle = TAU * f64::from(index) / f64::from(count) + dice.range(-0.05, 0.05);
                let at = (
                    x + radius * mathf::sin(angle),
                    z + radius * mathf::cos(angle),
                );
                let base = land.height(&stage.fields, at.0, at.1) - 0.6;
                let tall = height * dice.range(0.8, 1.15);
                let half = Vec3::new(
                    dice.range(0.35, 0.6),
                    0.5 * tall + 0.3,
                    dice.range(0.9, 1.5),
                );
                stage.block(
                    Vec3::new(at.0, base, at.1),
                    half,
                    angle + FRAC_PI_2,
                    polished,
                )?;
            }
        }
        Piece::Orbs => {
            stage.claim((x, z), 6.0)?;
            for _ in 0..dice.count(5, 11) {
                let radius = dice.range(0.4, 2.2);
                let material = match dice.count(0, 3) {
                    0 => stage.metal(Vec3::splat(0.93), 0.0)?,
                    1 => stage.glass(Vec3::splat(0.97), 0.0)?,
                    2 => stage.metal(GOLD, 0.03)?,
                    _ => stage.precious(dice)?,
                };
                let centre = Vec3::new(
                    x + dice.range(-5.0, 5.0),
                    ground + dice.range(1.5, 9.0),
                    z + dice.range(-5.0, 5.0),
                );
                stage.add(
                    Shape::Sphere { centre, radius },
                    material,
                    Pose::new(centre, Frame::WORLD),
                    true,
                )?;
            }
        }
    }
    Some(())
}

#[cfg(test)]
#[path = "landscape_tests.rs"]
mod tests;
