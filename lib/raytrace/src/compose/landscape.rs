//! Landscapes: rolling meadows, a forest glade, mountains over a lake, a
//! coast with the sea running in, dunes and rocky desert, snow, a lagoon at
//! sunset, and canyons between mesas.

use core::f64::consts::{FRAC_PI_2, PI, TAU};

use tairix_util::mathf;

use super::plants::{
    self, Grown, Growth, Species, Sward, BIRCH, BOX, CHERRY, FIR, GOLDEN_BIRCH, HAY, HEATHER,
    MAPLE, MOOR, OAK, OLIVE, PINE, POPLAR, SNOWY_FIR, SPRING, SUMMER,
};
use super::weather::{self, Climate, Cover, Hour, Outdoors};
use super::{direction, rgb, Dice, Look, Stage, View, GOLD};
use crate::material::{Finish, Foam, Material, Relief};
use crate::pigment::{Land, Pigment};
use crate::shape::Shape;
use crate::terrain::{Landform, Sea, Terrain};
use crate::vector::{Frame, Pose, Vec3};

/// The colours of a region's land.
#[derive(Copy, Clone, Debug)]
pub(super) struct Soil {
    grass: u32,
    dry: u32,
    earth: u32,
    rock: u32,
    strata: u32,
    sand: u32,
    snow: u32,
}

pub(super) const GREEN: Soil = Soil {
    grass: 0x4E_74_2C,
    dry: 0x8A_8C_44,
    earth: 0x6A_54_3A,
    rock: 0x7A_76_70,
    strata: 0x5A_56_52,
    sand: 0xD8_C8_A0,
    snow: 0xF4_F6_FA,
};
pub(super) const GOLDEN: Soil = Soil {
    grass: 0x8C_8A_40,
    dry: 0xB8_A2_5C,
    earth: 0x8A_6A_44,
    rock: 0x9A_8A_74,
    strata: 0x7A_6A_56,
    sand: 0xE0_CC_A0,
    snow: 0xF4_F6_FA,
};
const HIGHLAND: Soil = Soil {
    grass: 0x5A_6E_34,
    dry: 0x8A_80_4C,
    earth: 0x5E_4E_3C,
    rock: 0x6E_6C_6A,
    strata: 0x4A_48_48,
    sand: 0xC8_BC_A0,
    snow: 0xF4_F6_FA,
};
const RED_ROCK: Soil = Soil {
    grass: 0x7A_7A_3C,
    dry: 0xA8_8C_50,
    earth: 0xA0_5A_34,
    rock: 0xB8_64_3A,
    strata: 0x8A_42_28,
    sand: 0xD8_A0_6C,
    snow: 0xF4_F6_FA,
};
const SCRUB: Soil = Soil {
    grass: 0xA8_94_60,
    dry: 0xC4_AC_78,
    earth: 0xB0_7A_4A,
    rock: 0xB0_78_50,
    strata: 0x8A_58_38,
    sand: 0xD8_BC_8C,
    snow: 0xF4_F6_FA,
};
const DUNE: Soil = Soil {
    grass: 0xD8_B0_78,
    dry: 0xE0_BC_84,
    earth: 0xC8_9C64,
    rock: 0xA8_7A_50,
    strata: 0x8A_60_40,
    sand: 0xE4_C0_88,
    snow: 0xF4_F6_FA,
};
const SNOWFIELD: Soil = Soil {
    grass: 0xE8_EC_F2,
    dry: 0xF0_F2_F6,
    earth: 0xD8_DC_E4,
    rock: 0x5A_5C_60,
    strata: 0x3A_3C_40,
    sand: 0xE0_E4_EA,
    snow: 0xF6_F8_FC,
};
const VOLCANIC: Soil = Soil {
    grass: 0x3E_5A_2A,
    dry: 0x5A_62_34,
    earth: 0x3A_32_2E,
    rock: 0x3A_38_38,
    strata: 0x24_22_24,
    sand: 0x4A_46_44,
    snow: 0xF4_F6_FA,
};

/// No land in these scenes lies this high, so no snow lies on it.
const NO_SNOW: f64 = 1e5;

/// The land's material in `soil`: sand up to `shore`, snow from
/// `snow_line`, rock where the ground stands steeper than `cliff`, its
/// patches the size a land `scale` across wants.
pub(super) fn soil(
    stage: &mut Stage,
    dice: &mut Dice,
    soil: &Soil,
    levels: (f64, f64, f64),
    scale: f64,
) -> Option<usize> {
    let pigment = land_pigment(soil, levels, scale, dice.seed());
    stage.material(
        Material::new(pigment, Finish::Matte).with_relief(Relief::Grain {
            depth: 0.2,
            scale: 0.8,
            seed: dice.seed(),
        }),
    )
}

/// The land pigment of `soil`, sand up to `shore`, snow from `snow_line`,
/// rock where the ground stands steeper than `cliff`.
fn land_pigment(
    soil: &Soil,
    (shore, snow_line, cliff): (f64, f64, f64),
    scale: f64,
    seed: u32,
) -> Pigment {
    Pigment::Land(Land {
        grass: rgb(soil.grass),
        dry: rgb(soil.dry),
        earth: rgb(soil.earth),
        rock: rgb(soil.rock),
        strata: rgb(soil.strata),
        sand: rgb(soil.sand),
        snow: rgb(soil.snow),
        shore,
        snow_line,
        cliff,
        scale,
        seed,
    })
}

/// Land a scene stands on, and the trees that grow on it.
pub(super) struct Backdrop {
    pub(super) terrain: Terrain,
    pub(super) field: u32,
    species: [Option<Grown>; 4],
}

impl Backdrop {
    /// The land of `terrain` on the scene's grid `field`, where `species`
    /// grow.
    pub(super) fn new(
        stage: &mut Stage,
        dice: &mut Dice,
        terrain: Terrain,
        field: u32,
        species: &[Species],
    ) -> Option<Self> {
        let mut grown = [None; 4];
        for (slot, kind) in grown.iter_mut().zip(species) {
            *slot = Some(kind.grow(stage, dice)?);
        }
        Some(Self {
            terrain,
            field,
            species: grown,
        })
    }

    /// One of the trees that grow here, drawn at random.
    fn species(&self, dice: &mut Dice) -> Option<Grown> {
        let kinds = self.species.iter().flatten().count();
        let pick = dice.count(0, u32::try_from(kinds).ok()?.checked_sub(1)?);
        self.species.iter().flatten().nth(pick as usize).copied()
    }

    /// The land's grass, as `sward` grows it, over the square `reach` either
    /// way of `(x, z)`.
    pub(super) fn lawn(
        &self,
        stage: &mut Stage,
        dice: &mut Dice,
        ((x, z), reach): ((f64, f64), f64),
        sward: &Sward,
        growth: Growth,
    ) -> Option<usize> {
        let terrain = &self.terrain;
        plants::lawn(
            stage,
            dice,
            (self.field, &|x, z| terrain.height(x, z)),
            ((x - reach, z - reach), (x + reach, z + reach)),
            sward,
            growth,
        )
    }
}

/// Rolling land about a level clearing `(radius, level)` at the origin, in
/// `soil`, where `species` grow, and a plain past its rim.
pub(super) fn backdrop(
    stage: &mut Stage,
    dice: &mut Dice,
    (radius, level): (f64, f64),
    soil_colours: &Soil,
    species: &[Species],
) -> Option<Backdrop> {
    let form = Landform::Hills {
        scale: dice.range(250.0, 600.0),
        height: dice.range(8.0, 35.0),
        seed: dice.seed(),
    };
    let datum = form.height(0.0, 0.0);
    let terrain = Terrain {
        rim: form.lowest() - datum,
        form,
        datum,
        centre: (0.0, 0.0),
        radius: 1600.0,
        clearing: Some((level, radius)),
    };
    let material = soil(stage, dice, soil_colours, (-1e3, NO_SNOW, 0.7), 3200.0)?;
    let field = stage.land(&terrain, 512, material)?;
    stage.ground(terrain.lowest() - 0.05, material)?;
    Backdrop::new(stage, dice, terrain, field, species)
}

/// Trees of the land standing about the view from `eye` along `facing`,
/// clear of any clearing, wherever `fits` says one can grow.
pub(super) fn grove(
    stage: &mut Stage,
    dice: &mut Dice,
    land: &Backdrop,
    (eye, facing): (Vec3, f64),
    fits: &dyn Fn(f64, f64) -> bool,
) -> Option<()> {
    let keep = land
        .terrain
        .clearing
        .map_or(0.0, |(_, radius)| 1.15 * radius);
    let open = |x: f64, z: f64| mathf::sqrt(x * x + z * z) >= keep && fits(x, z);
    let count = dice.count(6, 18);
    woods(
        stage,
        dice,
        land,
        (eye, facing, 1.0),
        (count, (7.0, 16.0), (12.0, 280.0)),
        &open,
    )
}

/// Fresh water: a lake, a river, a pool, of the colour its depth takes, and
/// rippled by the wind.
pub(super) fn river(stage: &mut Stage, dice: &mut Dice) -> Option<usize> {
    let (absorb, glow) = dice.pick(&[
        (Vec3::new(0.35, 0.18, 0.22), Vec3::new(0.004, 0.012, 0.01)),
        (Vec3::new(0.45, 0.12, 0.1), Vec3::new(0.002, 0.012, 0.016)),
        (Vec3::new(0.25, 0.2, 0.3), Vec3::new(0.01, 0.012, 0.006)),
    ])?;
    let ripples = Relief::ripples(
        dice.range(0.004, 0.01),
        dice.range(0.8, 2.4),
        0.6,
        dice.seed(),
    );
    stage.water(absorb, glow, None, ripples)
}

/// How steeply the land ahead of `eye` along `heading` rises out to
/// `reach`, as the greatest angle above the eye's level at which any of it
/// stands.
fn ridge(terrain: &Terrain, eye: Vec3, heading: f64, reach: f64) -> f64 {
    let (sin, cos) = (mathf::sin(heading), mathf::cos(heading));
    let mut steepest = f64::NEG_INFINITY;
    let mut distance = 6.0;
    while distance < reach {
        let rise = terrain.height(eye.x + sin * distance, eye.z + cos * distance) - eye.y;
        steepest = steepest.max(mathf::atan2(rise, distance));
        distance *= 1.12;
    }
    steepest
}

/// A camera at `eye` looking along `heading`, pitched so the ridge ahead
/// sits `sky` of the way down a picture `fov` tall.
fn looking(terrain: &Terrain, eye: Vec3, heading: f64, (fov, sky): (f64, f64)) -> View {
    let ahead = ridge(terrain, eye, heading, 0.9 * terrain.radius).clamp(-0.3, 0.6);
    let pitch = ahead + fov * (sky - 0.5);
    View::Placed {
        eye,
        target: eye + direction(heading, 0.0, pitch) * 100.0,
        fov,
        aperture: 0.0,
    }
}

/// How many spots a vantage is chosen among.
const SPOTS: usize = 32;

/// Of a few level spots on `terrain` above `water`, those on the lowest
/// ground among them — within `terrace` of it — and of those the one looking
/// furthest before the land stands across the view: the eye `rise` above
/// it, and the way it looks. Where no spot is level and dry, the lowest one
/// seen, looking its most open way, the eye clear of the water.
fn vantage(
    terrain: &Terrain,
    dice: &mut Dice,
    (water, terrace): (f64, f64),
    rise: f64,
) -> (Vec3, f64) {
    let mut spots = [None; SPOTS];
    let mut lowest = ((0.0, 0.0), f64::INFINITY);
    for spot in &mut spots {
        let at = (dice.range(-900.0, 900.0), dice.range(-900.0, 900.0));
        let lie = terrain.height(at.0, at.1);
        if lie < lowest.1 {
            lowest = (at, lie);
        }
        if lie >= water + 1.0 && upright(terrain, at) >= 0.9 {
            *spot = Some((at, lie));
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
            let open = openness(terrain, eye, heading, (8.0_f64.to_radians(), 2500.0));
            if best.is_none_or(|(most, ..)| open > most) {
                best = Some((open, at, heading));
            }
        }
    }
    let over = |(x, z): (f64, f64)| Vec3::new(x, terrain.height(x, z).max(water) + rise, z);
    if let Some((_, at, heading)) = best {
        return (over(at), heading);
    }
    let eye = over(lowest.0);
    (eye, open_heading(terrain, dice, eye, 8))
}

/// How far the view from `eye` along `heading` runs, out to `reach`, before
/// the land stands across it more steeply than `wall`.
fn openness(terrain: &Terrain, eye: Vec3, heading: f64, (wall, reach): (f64, f64)) -> f64 {
    let (sin, cos, steep) = (mathf::sin(heading), mathf::cos(heading), mathf::tan(wall));
    let mut distance = 10.0;
    while distance < reach {
        let rise = terrain.height(eye.x + sin * distance, eye.z + cos * distance) - eye.y;
        if rise > distance * steep {
            return distance;
        }
        distance *= 1.1;
    }
    reach
}

/// The most open of a few headings from `eye`: where the land ahead rises
/// least.
fn open_heading(terrain: &Terrain, dice: &mut Dice, eye: Vec3, tries: u32) -> f64 {
    let mut best = (f64::INFINITY, 0.0);
    for _ in 0..tries {
        let heading = dice.range(0.0, TAU);
        let rise = ridge(terrain, eye, heading, 0.6 * terrain.radius);
        if rise < best.0 {
            best = (rise, heading);
        }
    }
    best.1
}

/// The highest of a few spots within `spread` of `(x, z)`.
fn rise_near(terrain: &Terrain, dice: &mut Dice, (x, z): (f64, f64), spread: f64) -> (f64, f64) {
    let mut best = (f64::NEG_INFINITY, (x, z));
    for _ in 0..16 {
        let spot = (
            x + dice.range(-spread, spread),
            z + dice.range(-spread, spread),
        );
        let height = terrain.height(spot.0, spot.1);
        if height > best.0 {
            best = (height, spot);
        }
    }
    best.1
}

/// How steep the land is at `(x, z)`: the upward part of its normal.
fn upright(terrain: &Terrain, (x, z): (f64, f64)) -> f64 {
    let step = 1.5;
    let dx = (terrain.height(x + step, z) - terrain.height(x - step, z)) / (2.0 * step);
    let dz = (terrain.height(x, z + step) - terrain.height(x, z - step)) / (2.0 * step);
    1.0 / mathf::sqrt(1.0 + dx * dx + dz * dz)
}

/// Trees of `land` scattered ahead of `eye` along `facing`, `count` of them
/// wherever `fits` says one can grow, `heights` tall.
fn woods(
    stage: &mut Stage,
    dice: &mut Dice,
    land: &Backdrop,
    (eye, facing, spread): (Vec3, f64, f64),
    (count, heights, reach): (u32, (f64, f64), (f64, f64)),
    fits: &dyn Fn(f64, f64) -> bool,
) -> Option<()> {
    let mut planted = 0;
    for _ in 0..6 * count {
        if planted >= count {
            break;
        }
        let turn = dice.range(-spread, spread);
        let distance = reach.0 + (reach.1 - reach.0) * dice.unit() * dice.unit();
        let (x, z) = (
            eye.x + mathf::sin(facing + turn) * distance,
            eye.z + mathf::cos(facing + turn) * distance,
        );
        let height = dice.range(heights.0, heights.1);
        let room = 0.2 * height;
        // Near the eye a tree keeps to the sides, so none walls off the view.
        let blocking = distance < 4.0 * height && turn.abs() < 0.35;
        if blocking || !fits(x, z) || !stage.clear((x, z), room) {
            continue;
        }
        stage.claim((x, z), room)?;
        let grown = land.species(dice)?;
        plants::tree(
            stage,
            dice,
            Vec3::new(x, land.terrain.height(x, z), z),
            height,
            grown,
        )?;
        planted += 1;
    }
    Some(())
}

/// A large ball of something that shows light off, resting in the land ahead
/// of `eye`: chrome, gold, glass.
fn marvel(
    stage: &mut Stage,
    dice: &mut Dice,
    land: &Backdrop,
    (eye, facing): (Vec3, f64),
) -> Option<()> {
    let distance = dice.range(7.0, 16.0);
    let angle = facing + dice.range(-0.25, 0.25);
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
    let base = Vec3::new(x, land.terrain.height(x, z) - 0.15 * radius, z);
    stage.ball(base, radius, material, dice).map(|_| ())
}

/// The look of a landscape under `weather`, seen by `view`, its exposure
/// scaled by `brightness` for a land lighter or darker than most.
fn look(weather: Outdoors, view: View, brightness: f64) -> Look {
    Look {
        sky: weather.sky,
        fog: weather.fog,
        exposure: weather.exposure * brightness,
        bounce: weather.bounce,
        view,
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
        (Cover::Overcast, 1),
    ],
    haze: (0.0008, 0.0025),
};

pub(super) fn meadow(stage: &mut Stage, dice: &mut Dice) -> Option<Look> {
    let terrain = Terrain {
        form: Landform::Hills {
            scale: dice.range(200.0, 450.0),
            height: dice.range(25.0, 80.0),
            seed: dice.seed(),
        },
        datum: 0.0,
        centre: (0.0, 0.0),
        radius: 2400.0,
        rim: 0.0,
        clearing: None,
    };
    let colours = dice.pick(&[GREEN, GREEN, GOLDEN, HIGHLAND])?;
    let material = soil(stage, dice, &colours, (-1e3, NO_SNOW, 0.72), 4800.0)?;
    let field = stage.land(&terrain, 1024, material)?;
    stage.ground(terrain.lowest() - 0.05, material)?;
    let kinds: &[Species] = match dice.count(0, 3) {
        0 => &[OAK, POPLAR],
        1 => &[OAK, BIRCH, CHERRY],
        2 => &[MAPLE, GOLDEN_BIRCH, OAK],
        _ => &[OLIVE, POPLAR],
    };
    let land = Backdrop::new(stage, dice, terrain, field, kinds)?;
    let spot = rise_near(&land.terrain, dice, (0.0, 0.0), 300.0);
    let eye = Vec3::new(
        spot.0,
        land.terrain.height(spot.0, spot.1) + dice.range(1.5, 3.0),
        spot.1,
    );
    stage.claim(spot, 4.0)?;
    let heading = open_heading(&land.terrain, dice, eye, 4);
    let sward = dice.pick(&[SPRING, SUMMER, HAY])?;
    let growth = Growth {
        height: (0.12, dice.range(0.3, 0.6)),
        lean: dice.range(0.25, 0.45),
        flowers: dice.range(0.0, 0.06),
    };
    land.lawn(stage, dice, (spot, 26.0), &sward, growth)?;
    let count = dice.count(5, 16);
    woods(
        stage,
        dice,
        &land,
        (eye, heading, 1.1),
        (count, (7.0, 16.0), (25.0, 450.0)),
        &|_, _| true,
    )?;
    if dice.chance(0.3) {
        marvel(stage, dice, &land, (eye, heading))?;
    }
    let weather = weather::outdoors(stage, dice, &MEADOW, heading)?;
    let fov = dice.angle(46.0, 62.0);
    Some(look(
        weather,
        looking(&land.terrain, eye, heading, (fov, dice.range(0.5, 0.68))),
        1.0,
    ))
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
    haze: (0.003, 0.008),
};

pub(super) fn forest(stage: &mut Stage, dice: &mut Dice) -> Option<Look> {
    let form = Landform::Hills {
        scale: dice.range(120.0, 250.0),
        height: dice.range(8.0, 25.0),
        seed: dice.seed(),
    };
    let glade = dice.range(12.0, 22.0);
    let level = form.height(0.0, 0.0);
    let terrain = Terrain {
        form,
        datum: 0.0,
        centre: (0.0, 0.0),
        radius: 1200.0,
        rim: 0.0,
        clearing: Some((level, glade)),
    };
    let material = soil(stage, dice, &GREEN, (-1e3, NO_SNOW, 0.7), 2400.0)?;
    let field = stage.land(&terrain, 512, material)?;
    stage.ground(terrain.lowest() - 0.05, material)?;
    let kinds: &[Species] = match dice.count(0, 3) {
        0 => &[OAK, BIRCH, OAK],
        1 => &[MAPLE, GOLDEN_BIRCH, OAK],
        2 => &[FIR, PINE, FIR],
        _ => &[OAK, FIR, BIRCH, PINE],
    };
    let land = Backdrop::new(stage, dice, terrain, field, kinds)?;
    // From the glade's edge, across it to the trees beyond.
    let heading = dice.range(0.0, TAU);
    let back = glade * dice.range(0.6, 0.9);
    let spot = (-mathf::sin(heading) * back, -mathf::cos(heading) * back);
    let eye = Vec3::new(
        spot.0,
        land.terrain.height(spot.0, spot.1) + dice.range(1.4, 2.0),
        spot.1,
    );
    stage.claim(spot, 2.0)?;
    let ring = |x: f64, z: f64| mathf::sqrt(x * x + z * z) > 1.1 * glade;
    let count = dice.count(40, 80);
    woods(
        stage,
        dice,
        &land,
        (eye, heading, 1.3),
        (count, (9.0, 20.0), (6.0, 120.0)),
        &ring,
    )?;
    let bush = BOX.grow(stage, dice)?;
    for _ in 0..dice.count(3, 8) {
        let angle = dice.range(0.0, TAU);
        let at = (glade * mathf::sin(angle), glade * mathf::cos(angle));
        if stage.clear(at, 1.0) {
            let height = dice.range(1.0, 2.2);
            plants::tree(
                stage,
                dice,
                Vec3::new(at.0, land.terrain.height(at.0, at.1), at.1),
                height,
                bush,
            )?;
        }
    }
    let sward = dice.pick(&[SPRING, SUMMER])?;
    let growth = Growth {
        height: (0.1, dice.range(0.25, 0.45)),
        lean: 0.35,
        flowers: dice.range(0.01, 0.08),
    };
    land.lawn(stage, dice, ((0.0, 0.0), 1.2 * glade), &sward, growth)?;
    let weather = weather::outdoors(stage, dice, &FOREST, heading)?;
    let fov = dice.angle(50.0, 64.0);
    let view = View::Placed {
        eye,
        target: eye + direction(heading, 0.0, dice.angle(4.0, 12.0)) * 100.0,
        fov,
        aperture: 0.0,
    };
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
    ],
    haze: (0.00004, 0.00015),
};

pub(super) fn alpine(stage: &mut Stage, dice: &mut Dice) -> Option<Look> {
    let height = dice.range(700.0, 1500.0);
    let floor = dice.range(700.0, 1300.0);
    let terrain = Terrain {
        form: Landform::Mountains {
            scale: dice.range(900.0, 1600.0),
            height,
            floor,
            seed: dice.seed(),
        },
        datum: 0.0,
        centre: (0.0, 0.0),
        radius: 6000.0,
        rim: 0.35 * height,
        clearing: None,
    };
    // The lake fills the valley floor to about the lower half of its lie.
    let mut lies = [0.0f64; 32];
    for (index, lie) in lies.iter_mut().enumerate() {
        let angle = TAU * f64::from(u32::try_from(index).ok()?) / 32.0;
        let distance = 0.45 * floor * dice.unit();
        *lie = terrain.height(distance * mathf::sin(angle), distance * mathf::cos(angle));
    }
    lies.sort_by(f64::total_cmp);
    let lake = lies.get(12).copied()?;
    let snow_line = lake + dice.range(0.45, 0.65) * height;
    let material = soil(
        stage,
        dice,
        &HIGHLAND,
        (lake + 1.5, snow_line, 0.68),
        12_000.0,
    )?;
    let field = stage.land(&terrain, 1024, material)?;
    let kinds: &[Species] = if dice.chance(0.6) {
        &[FIR, PINE]
    } else {
        &[FIR, GOLDEN_BIRCH, PINE]
    };
    let land = Backdrop::new(stage, dice, terrain, field, kinds)?;
    // On the shore, looking back across the water to the far side.
    let out = dice.range(0.0, TAU);
    let mut distance = 20.0;
    while distance < 1.5 * floor
        && land
            .terrain
            .height(distance * mathf::sin(out), distance * mathf::cos(out))
            < lake + 3.0
    {
        distance += 10.0;
    }
    distance += dice.range(15.0, 60.0);
    let spot = (distance * mathf::sin(out), distance * mathf::cos(out));
    let eye = Vec3::new(
        spot.0,
        land.terrain.height(spot.0, spot.1) + dice.range(2.0, 12.0),
        spot.1,
    );
    stage.claim(spot, 5.0)?;
    let heading = out + PI + dice.angle(-35.0, 35.0);
    let water = alpine_water(stage, dice)?;
    let half = 1.7 * floor;
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
    let terrain = &land.terrain;
    let tree_line = lake + dice.range(0.15, 0.3) * height;
    let fits = |x: f64, z: f64| {
        let at = terrain.height(x, z);
        at > lake + 2.0 && at < tree_line && upright(terrain, (x, z)) > 0.8
    };
    let count = dice.count(20, 45);
    woods(
        stage,
        dice,
        &land,
        (eye, heading, 1.2),
        (count, (8.0, 22.0), (12.0, 600.0)),
        &fits,
    )?;
    let sward = dice.pick(&[MOOR, SPRING])?;
    let growth = Growth {
        height: (0.1, dice.range(0.2, 0.4)),
        lean: 0.3,
        flowers: dice.range(0.0, 0.05),
    };
    land.lawn(stage, dice, (spot, 22.0), &sward, growth)?;
    let weather = weather::outdoors(stage, dice, &ALPINE, heading)?;
    let fov = dice.angle(44.0, 58.0);
    let view = View::Placed {
        eye,
        target: eye + direction(heading, 0.0, dice.angle(2.0, 9.0)) * 100.0,
        fov,
        aperture: 0.0,
    };
    Some(look(weather, view, 0.95))
}

/// A mountain lake: clear and cold, still but for a breath of wind.
fn alpine_water(stage: &mut Stage, dice: &mut Dice) -> Option<usize> {
    let ripples = Relief::ripples(
        dice.range(0.001, 0.006),
        dice.range(0.6, 2.0),
        0.5,
        dice.seed(),
    );
    stage.water(
        Vec3::new(0.3, 0.1, 0.08),
        Vec3::new(0.002, 0.01, 0.014),
        None,
        ripples,
    )
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
        (Cover::Overcast, 1),
    ],
    haze: (0.0004, 0.0012),
};

pub(super) fn coast(stage: &mut Stage, dice: &mut Dice) -> Option<Look> {
    let heading = dice.range(0.0, TAU);
    let across = heading + FRAC_PI_2 * dice.sign();
    let island = dice.range(700.0, 1500.0);
    let rise = dice.range(40.0, 160.0);
    let centre = (
        -mathf::sin(heading) * 0.85 * island + mathf::sin(across) * 0.3 * island,
        -mathf::cos(heading) * 0.85 * island + mathf::cos(across) * 0.3 * island,
    );
    let landform = Landform::Island {
        centre,
        radius: island,
        height: rise,
        seed: dice.seed(),
    };
    let terrain = Terrain {
        rim: landform.lowest(),
        form: landform,
        datum: 0.0,
        centre,
        radius: 1.4 * island,
        clearing: None,
    };
    let colours = dice.pick(&[GREEN, GOLDEN, VOLCANIC, HIGHLAND])?;
    let material = soil(stage, dice, &colours, (1.8, NO_SNOW, 0.72), 2.8 * island)?;
    let field = stage.land(&terrain, 1024, material)?;
    let land = Backdrop::new(stage, dice, terrain, field, &[PINE, OAK])?;
    // Out from the island's middle to where it meets the sea, then back up
    // the shore to a beach, or the top of a cliff.
    let shore = |angle: f64| {
        let mut distance = 0.2 * island;
        while distance < 1.4 * island {
            let at = (
                centre.0 + mathf::sin(angle) * distance,
                centre.1 + mathf::cos(angle) * distance,
            );
            if land.terrain.height(at.0, at.1) < 0.4 {
                return Some(distance);
            }
            distance += 4.0;
        }
        None
    };
    let edge = shore(heading).unwrap_or(island);
    let setback = dice.range(6.0, 70.0);
    let spot = (
        centre.0 + mathf::sin(heading) * (edge - setback),
        centre.1 + mathf::cos(heading) * (edge - setback),
    );
    let ground = land.terrain.height(spot.0, spot.1).max(0.5);
    let eye = Vec3::new(spot.0, ground + dice.range(1.6, 5.0), spot.1);
    stage.claim(spot, 3.0)?;
    // Out to sea, but turned along the shore so the coast runs down one
    // side of the picture.
    let hand = dice.sign();
    let facing = heading + hand * dice.angle(35.0, 65.0);
    let weather = weather::outdoors(stage, dice, &COAST, facing)?;
    ocean(stage, dice, heading, weather.daylight)?;
    strand(stage, dice, &land, (centre, heading, across, edge))?;
    if ground > 4.0 {
        let sward = dice.pick(&[MOOR, SUMMER, HAY])?;
        let growth = Growth {
            height: (0.1, dice.range(0.2, 0.45)),
            lean: dice.range(0.35, 0.55),
            flowers: dice.range(0.0, 0.03),
        };
        land.lawn(stage, dice, (spot, 16.0), &sward, growth)?;
    }
    // Along the shore the camera looks down, a little way off.
    let angle = heading + hand * dice.angle(4.0, 16.0);
    if let Some(reach) = shore(angle).filter(|_| dice.chance(0.45)) {
        let at = (
            centre.0 + mathf::sin(angle) * (reach - 25.0),
            centre.1 + mathf::cos(angle) * (reach - 25.0),
        );
        if land.terrain.height(at.0, at.1) > 2.0 {
            let lit = matches!(weather.hour, Hour::Dusk | Hour::Night | Hour::Sunset);
            lighthouse(
                stage,
                dice,
                Vec3::new(at.0, land.terrain.height(at.0, at.1), at.1),
                lit,
            )?;
        }
    }
    let fov = dice.angle(46.0, 60.0);
    let view = View::Placed {
        eye,
        target: eye + direction(facing, 0.0, dice.angle(-6.0, 2.0)) * 100.0,
        fov,
        aperture: 0.0,
    };
    Some(look(weather, view, 1.0))
}

/// Boulders strewn along the shore, `edge` out from the island's `centre`
/// along `heading`, and a way either side along `across`.
fn strand(
    stage: &mut Stage,
    dice: &mut Dice,
    land: &Backdrop,
    (centre, heading, across, edge): ((f64, f64), f64, f64, f64),
) -> Option<()> {
    let rock = stage.stone(dice)?;
    for _ in 0..dice.count(4, 10) {
        let along = dice.range(-60.0, 60.0);
        let out = dice.range(-10.0, 25.0);
        let at = (
            centre.0 + mathf::sin(heading) * (edge + out) + mathf::sin(across) * along,
            centre.1 + mathf::cos(heading) * (edge + out) + mathf::cos(across) * along,
        );
        let size = dice.range(0.6, 3.5);
        if !stage.clear(at, size) {
            continue;
        }
        stage.claim(at, size)?;
        let base = Vec3::new(at.0, land.terrain.height(at.0, at.1), at.1);
        stage.boulder(base, size, rock, dice)?;
    }
    Some(())
}

/// The open sea off a shore facing out along `heading`, its swells running
/// in toward it, its depths lit as `daylight` has them.
fn ocean(stage: &mut Stage, dice: &mut Dice, heading: f64, daylight: f64) -> Option<u32> {
    let swell = dice.range(0.8, 2.5);
    let sea = Sea::new(
        384.0,
        (dice.range(30.0, 70.0), swell),
        (heading + PI + dice.range(-0.3, 0.3), dice.range(0.35, 0.8)),
        dice.seed(),
    );
    // Crests whiten from a little above the sea's middling height.
    let foam = Foam {
        crest: 0.35 * swell,
        spread: 0.15 * swell,
        seed: dice.seed(),
    };
    let ripples = Relief::ripples(
        dice.range(0.01, 0.03),
        dice.range(1.5, 4.0),
        0.9,
        dice.seed(),
    );
    let water = stage.water(
        Vec3::new(0.45, 0.09, 0.055),
        Vec3::new(0.002, 0.018, 0.035) * daylight,
        Some(foam),
        ripples,
    )?;
    stage.sea(sea, (384.0, 512), water)
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
        stage.orb(lamp, 0.45, rgb(0xFF_F0_C8) * dice.range(60.0, 90.0))
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
    covers: &[(Cover::Clear, 6), (Cover::Cirrus, 3), (Cover::Fair, 2)],
    haze: (0.0003, 0.001),
};

pub(super) fn desert(stage: &mut Stage, dice: &mut Dice) -> Option<Look> {
    let dunes = dice.chance(0.6);
    let form = if dunes {
        Landform::Dunes {
            scale: dice.range(70.0, 180.0),
            height: dice.range(8.0, 30.0),
            heading: dice.range(0.0, TAU),
            seed: dice.seed(),
        }
    } else {
        Landform::Hills {
            scale: dice.range(150.0, 300.0),
            height: dice.range(20.0, 60.0),
            seed: dice.seed(),
        }
    };
    let terrain = Terrain {
        rim: form.lowest(),
        form,
        datum: 0.0,
        centre: (0.0, 0.0),
        radius: 2000.0,
        clearing: None,
    };
    let colours = if dunes {
        DUNE
    } else {
        dice.pick(&[RED_ROCK, SCRUB])?
    };
    let material = if dunes {
        let sand = Material::new(
            land_pigment(&colours, (-1e3, NO_SNOW, 0.3), 4000.0, dice.seed()),
            Finish::Matte,
        )
        .with_relief(Relief::ripples(
            0.012,
            dice.range(0.1, 0.18),
            0.35,
            dice.seed(),
        ));
        stage.material(sand)?
    } else {
        soil(stage, dice, &colours, (-1e3, NO_SNOW, 0.8), 4000.0)?
    };
    let field = stage.land(&terrain, 1024, material)?;
    stage.ground(terrain.lowest() - 0.05, material)?;
    let land = Backdrop::new(stage, dice, terrain, field, &[OLIVE])?;
    let spot = rise_near(&land.terrain, dice, (0.0, 0.0), 200.0);
    let eye = Vec3::new(
        spot.0,
        land.terrain.height(spot.0, spot.1) + dice.range(1.6, 3.5),
        spot.1,
    );
    stage.claim(spot, 4.0)?;
    let heading = open_heading(&land.terrain, dice, eye, 4);
    let ahead = |distance: f64, turn: f64| {
        let angle = heading + turn;
        (
            eye.x + mathf::sin(angle) * distance,
            eye.z + mathf::cos(angle) * distance,
        )
    };
    if dunes {
        erg(stage, dice, &land, &ahead)?;
    } else {
        badlands(stage, dice, &land, &ahead)?;
    }
    let weather = weather::outdoors(stage, dice, &DESERT, heading)?;
    let fov = dice.angle(46.0, 60.0);
    Some(look(
        weather,
        looking(&land.terrain, eye, heading, (fov, dice.range(0.55, 0.7))),
        0.85,
    ))
}

/// What stands on a sea of dunes, `ahead` placing it a distance and a turn
/// from the eye's heading: pyramids far off, now and then something nearer
/// that catches the light, and the stones the wind has left bare.
fn erg(
    stage: &mut Stage,
    dice: &mut Dice,
    land: &Backdrop,
    ahead: &dyn Fn(f64, f64) -> (f64, f64),
) -> Option<()> {
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
            let at = ahead(dice.range(400.0, 1400.0), dice.range(-0.5, 0.5));
            let half = dice.range(40.0, 140.0);
            if !stage.clear(at, 1.2 * half) {
                continue;
            }
            stage.claim(at, 1.2 * half)?;
            let base = Vec3::new(at.0, land.terrain.height(at.0, at.1) - 0.1 * half, at.1);
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
        let at = ahead(dice.range(14.0, 40.0), dice.range(-0.3, 0.3));
        stage.claim(at, 2.0)?;
        let base = Vec3::new(at.0, land.terrain.height(at.0, at.1) - 1.0, at.1);
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
    // Stones the wind has left bare, so the sand is never empty.
    let rock = stage.stone(dice)?;
    for _ in 0..dice.count(3, 10) {
        let at = ahead(dice.range(6.0, 150.0), dice.range(-0.8, 0.8));
        let size = dice.range(0.2, 1.2);
        if !stage.clear(at, size) {
            continue;
        }
        stage.claim(at, size)?;
        stage.boulder(
            Vec3::new(at.0, land.terrain.height(at.0, at.1), at.1),
            size,
            rock,
            dice,
        )?;
    }
    Some(())
}

/// What grows and lies on rocky desert, placed as `ahead` has it: saguaros,
/// boulders, and scrub.
fn badlands(
    stage: &mut Stage,
    dice: &mut Dice,
    land: &Backdrop,
    ahead: &dyn Fn(f64, f64) -> (f64, f64),
) -> Option<()> {
    let cactus = stage.material(Material::new(
        Pigment::Bark {
            light: rgb(0x5A_7A_42),
            dark: rgb(0x2E_44_28),
            scale: 3.0,
            seed: dice.seed(),
        },
        Finish::Coated { roughness: 0.6 },
    ))?;
    for _ in 0..dice.count(6, 20) {
        let at = ahead(dice.range(8.0, 200.0), dice.range(-0.9, 0.9));
        if !stage.clear(at, 1.2) {
            continue;
        }
        stage.claim(at, 1.2)?;
        saguaro(
            stage,
            dice,
            Vec3::new(at.0, land.terrain.height(at.0, at.1), at.1),
            cactus,
        )?;
    }
    let rock = stage.stone(dice)?;
    for _ in 0..dice.count(5, 14) {
        let at = ahead(dice.range(6.0, 120.0), dice.range(-0.9, 0.9));
        let size = dice.range(0.4, 2.5);
        if !stage.clear(at, size) {
            continue;
        }
        stage.claim(at, size)?;
        stage.boulder(
            Vec3::new(at.0, land.terrain.height(at.0, at.1), at.1),
            size,
            rock,
            dice,
        )?;
    }
    let bush = HEATHER.grow(stage, dice)?;
    for _ in 0..dice.count(3, 10) {
        let at = ahead(dice.range(5.0, 80.0), dice.range(-0.9, 0.9));
        let height = dice.range(0.6, 1.4);
        if stage.clear(at, 0.8) {
            plants::tree(
                stage,
                dice,
                Vec3::new(at.0, land.terrain.height(at.0, at.1), at.1),
                height,
                bush,
            )?;
        }
    }
    Some(())
}

/// A saguaro at `base`: a ribbed trunk with an arm or two turned up.
fn saguaro(stage: &mut Stage, dice: &mut Dice, base: Vec3, flesh: usize) -> Option<()> {
    let girth = dice.range(0.22, 0.38);
    let height = dice.range(2.5, 7.0);
    stage.limb(
        base - Vec3::UP * 0.3,
        base + Vec3::UP * height,
        (girth, 0.9 * girth),
        flesh,
    )?;
    stage.ball(
        base + Vec3::UP * (height - 0.9 * girth),
        0.9 * girth,
        flesh,
        dice,
    )?;
    for _ in 0..dice.count(0, 3) {
        let heading = dice.range(0.0, TAU);
        let joint = base + Vec3::UP * (height * dice.range(0.35, 0.6));
        let out = direction(heading, 0.0, 0.0) * dice.range(0.5, 0.9);
        let arm = 0.7 * girth;
        stage.limb(joint, joint + out, (arm, arm), flesh)?;
        let elbow = joint + out;
        stage.ball(elbow - Vec3::UP * arm, arm, flesh, dice)?;
        let up = dice.range(0.8, 2.2);
        stage.limb(elbow, elbow + Vec3::UP * up, (arm, 0.92 * arm), flesh)?;
        stage.ball(elbow + Vec3::UP * (up - 0.9 * arm), 0.9 * arm, flesh, dice)?;
    }
    Some(())
}

const WINTER: Climate = Climate {
    hours: &[
        (Hour::Day, 3),
        (Hour::Golden, 3),
        (Hour::Noon, 1),
        (Hour::Sunset, 2),
        (Hour::Night, 3),
    ],
    covers: &[
        (Cover::Clear, 4),
        (Cover::Fair, 2),
        (Cover::Overcast, 2),
        (Cover::Cirrus, 2),
    ],
    haze: (0.0003, 0.001),
};

pub(super) fn winter(stage: &mut Stage, dice: &mut Dice) -> Option<Look> {
    let form = Landform::Hills {
        scale: dice.range(180.0, 400.0),
        height: dice.range(20.0, 70.0),
        seed: dice.seed(),
    };
    let pond = dice.range(40.0, 90.0);
    let edge = 1.5 * pond;
    // The pond lies a few metres below the lowest land at the ice's edge and
    // beyond, so its ice meets banks all round.
    let mut lowest = f64::INFINITY;
    for ring in [edge, 2.5 * pond] {
        for step in 0..48u32 {
            let angle = TAU * f64::from(step) / 48.0;
            lowest = lowest.min(form.height(ring * mathf::sin(angle), ring * mathf::cos(angle)));
        }
    }
    let level = lowest - dice.range(3.0, 5.0);
    let terrain = Terrain {
        rim: form.lowest().min(level),
        form,
        datum: 0.0,
        centre: (0.0, 0.0),
        radius: 2000.0,
        clearing: Some((level, pond)),
    };
    let material = soil(stage, dice, &SNOWFIELD, (-1e3, -1e3, 0.6), 4000.0)?;
    let field = stage.land(&terrain, 1024, material)?;
    stage.ground(terrain.lowest() - 0.05, material)?;
    let land = Backdrop::new(stage, dice, terrain, field, &[SNOWY_FIR, SNOWY_FIR, FIR])?;
    let ice = stage.material(
        Material::new(
            Pigment::Solid(Vec3::ONE),
            Finish::Glass {
                ior: 1.31,
                absorb: Vec3::new(1.4, 0.6, 0.35),
                glow: Vec3::ZERO,
                roughness: dice.range(0.03, 0.08),
                dispersion: 0.0,
                foam: None,
            },
        )
        .with_relief(Relief::Grain {
            depth: 0.05,
            scale: 1.5,
            seed: dice.seed(),
        }),
    )?;
    stage.frustum(
        Pose::new(Vec3::UP * (level - 0.2), Frame::WORLD),
        (edge, edge, 0.7),
        ice,
        false,
    )?;
    let heading = dice.range(0.0, TAU);
    let back = pond * dice.range(1.8, 2.4);
    let spot = (-mathf::sin(heading) * back, -mathf::cos(heading) * back);
    let eye = Vec3::new(
        spot.0,
        land.terrain.height(spot.0, spot.1) + dice.range(1.6, 3.0),
        spot.1,
    );
    stage.claim(spot, 3.0)?;
    stage.claim((0.0, 0.0), 1.6 * pond)?;
    // Trees crowd the banks, never the ice.
    let banks = |x: f64, z: f64| mathf::sqrt(x * x + z * z) > 1.6 * pond;
    let count = dice.count(25, 50);
    woods(
        stage,
        dice,
        &land,
        (eye, heading, 1.2),
        (count, (6.0, 16.0), (8.0, 300.0)),
        &banks,
    )?;
    if dice.chance(0.3) {
        let at = (
            spot.0 + mathf::sin(heading + 0.4) * 5.0,
            spot.1 + mathf::cos(heading + 0.4) * 5.0,
        );
        snowman(
            stage,
            dice,
            Vec3::new(at.0, land.terrain.height(at.0, at.1), at.1),
            heading + PI,
        )?;
    }
    let weather = weather::outdoors(stage, dice, &WINTER, heading)?;
    let fov = dice.angle(46.0, 60.0);
    let view = View::Placed {
        eye,
        target: eye + direction(heading, 0.0, dice.angle(-4.0, 4.0)) * 100.0,
        fov,
        aperture: 0.0,
    };
    Some(look(weather, view, 0.72))
}

/// A snowman at `base`, looking toward `facing`.
fn snowman(stage: &mut Stage, dice: &mut Dice, base: Vec3, facing: f64) -> Option<()> {
    let snow = stage.material(
        Material::new(Pigment::Solid(rgb(0xF4_F6_FA)), Finish::Matte).with_relief(Relief::Grain {
            depth: 0.25,
            scale: 8.0,
            seed: dice.seed(),
        }),
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
        (Cover::Broken, 1),
    ],
    haze: (0.002, 0.006),
};

pub(super) fn lagoon(stage: &mut Stage, dice: &mut Dice) -> Option<Look> {
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
        .with_relief(Relief::Grain {
            depth: 0.18,
            scale: 9.0,
            seed: dice.seed(),
        }),
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
    if dice.chance(0.45) {
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
        let terrain = Terrain {
            rim: form.lowest(),
            form,
            datum: 0.0,
            centre,
            radius: 1.4 * radius,
            clearing: None,
        };
        let colours = dice.pick(&[GREEN, VOLCANIC])?;
        let material = soil(stage, dice, &colours, (1.5, NO_SNOW, 0.72), 2.8 * radius)?;
        stage.land(&terrain, 256, material)?;
    }
    let weather = weather::outdoors(stage, dice, &LAGOON, facing)?;
    let ripples = Relief::ripples(
        dice.range(0.004, 0.012),
        dice.range(1.4, 3.2),
        0.7,
        dice.seed(),
    );
    let water = stage.water(
        Vec3::new(0.3, 0.06, 0.045) * dice.range(0.8, 1.5),
        Vec3::new(0.004, 0.03, 0.03) * weather.daylight,
        None,
        ripples,
    )?;
    stage.ground(0.0, water)?;
    Some(Look {
        sky: weather.sky,
        fog: weather.fog,
        exposure: weather.exposure,
        bounce: weather.bounce,
        view: View::Framed {
            yaw,
            elevation: dice.angle(4.0, 12.0),
            fov: dice.angle(42.0, 55.0),
            fill: 0.9,
            aperture: 0.0,
        },
    })
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
        (Cover::Broken, 1),
    ],
    haze: (0.0004, 0.0012),
};

pub(super) fn canyon(stage: &mut Stage, dice: &mut Dice) -> Option<Look> {
    let height = dice.range(80.0, 260.0);
    let steps = f64::from(dice.count(3, 5));
    let terrain = Terrain {
        form: Landform::Mesas {
            scale: dice.range(250.0, 600.0),
            height,
            steps,
            seed: dice.seed(),
        },
        datum: 0.0,
        centre: (0.0, 0.0),
        radius: 3000.0,
        rim: height,
        clearing: None,
    };
    let material = soil(stage, dice, &RED_ROCK, (-1e3, NO_SNOW, 0.8), 6000.0)?;
    let field = stage.land(&terrain, 1024, material)?;
    let land = Backdrop::new(stage, dice, terrain, field, &[OLIVE, HEATHER])?;
    let water_level = 0.015 * height;
    let rise = dice.range(2.0, 12.0);
    // On the lowest terrace in sight, the mesas standing about it.
    let terrace = 0.5 * height / steps;
    let (eye, heading) = vantage(&land.terrain, dice, (water_level, terrace), rise);
    stage.claim((eye.x, eye.z), 4.0)?;
    let river = river(stage, dice)?;
    let half = 0.9 * land.terrain.radius;
    stage.add(
        Shape::Quad {
            corner: Vec3::new(-half, water_level, -half),
            edge_u: Vec3::new(0.0, 0.0, 2.0 * half),
            edge_v: Vec3::new(2.0 * half, 0.0, 0.0),
        },
        river,
        Pose::new(Vec3::ZERO, Frame::WORLD),
        false,
    )?;
    let terrain = &land.terrain;
    let fits = |x: f64, z: f64| {
        terrain.height(x, z) > water_level + 0.5 && upright(terrain, (x, z)) > 0.85
    };
    let count = dice.count(8, 24);
    woods(
        stage,
        dice,
        &land,
        (eye, heading, 1.1),
        (count, (1.2, 4.0), (8.0, 300.0)),
        &fits,
    )?;
    let rock = stage.stone(dice)?;
    for _ in 0..dice.count(4, 12) {
        let angle = heading + dice.range(-0.8, 0.8);
        let distance = dice.range(6.0, 90.0);
        let at = (
            eye.x + mathf::sin(angle) * distance,
            eye.z + mathf::cos(angle) * distance,
        );
        let size = dice.range(0.5, 3.0);
        if terrain.height(at.0, at.1) < water_level || !stage.clear(at, size) {
            continue;
        }
        stage.claim(at, size)?;
        stage.boulder(
            Vec3::new(at.0, terrain.height(at.0, at.1), at.1),
            size,
            rock,
            dice,
        )?;
    }
    let weather = weather::outdoors(stage, dice, &CANYON, heading)?;
    let fov = dice.angle(48.0, 62.0);
    Some(look(
        weather,
        looking(&land.terrain, eye, heading, (fov, dice.range(0.35, 0.5))),
        0.95,
    ))
}

#[cfg(test)]
#[path = "landscape_tests.rs"]
mod tests;
