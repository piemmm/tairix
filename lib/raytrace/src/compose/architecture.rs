//! Buildings: colonnades, arcades and aqueducts, rotundas, and ruins, each on
//! a plaza or open ground and seen under the day's weather.

use core::f64::consts::{FRAC_PI_2, TAU};

use tairix_util::mathf;

use super::landscape::{self, Backdrop, Lawning, Scheme, Vantage, GOLDEN, GREEN};
use super::plants::{self, Character, Grassland, Grove, Kind, Stand};
use super::weather::{self, Climate, Cover, Hour};
use super::woodland::ANYWHERE;
use super::{direction, rgb, Composed, Dice, Landing, Look, Stage, View, COPPER, GOLD};
use crate::land::{Land, Plan, Rivers, Survey, Wear};
use crate::material::{Finish, Material, Relief};
use crate::pigment::Pigment;
use crate::terrain::{Landform, Terrain};
use crate::tree::Season;
use crate::vector::{Frame, Pose, Vec3};

const MONUMENT: Climate = Climate {
    hours: &[
        (Hour::Noon, 1),
        (Hour::Day, 4),
        (Hour::Golden, 4),
        (Hour::Sunset, 2),
    ],
    covers: &[
        (Cover::Clear, 3),
        (Cover::Fair, 4),
        (Cover::Cirrus, 2),
        (Cover::Broken, 1),
    ],
    haze: (1.2, 2.4),
    base: 150.0,
    albedo: 0.2,
};

/// How far below the floor a building's surroundings lie, on the plaza it
/// stands on.
const PLINTH: f64 = 0.6;

/// A building stone: marble, sandstone, limestone, or granite.
fn masonry(stage: &mut Stage, dice: &mut Dice) -> Option<usize> {
    let (base, dark, light) = match dice.count(0, 4) {
        0 | 1 => return stage.marble(dice),
        2 => (0xD0_B4_88, 0xA8_8C64, 0xE8_D8_B8),
        3 => (0xE0_D8_C4, 0xB8_AE_98, 0xF0_EA_DC),
        _ => return stage.stone(dice),
    };
    stage.material(
        Material::new(
            Pigment::Speckle {
                base: rgb(base),
                flecks: [rgb(dark), rgb(light)],
                scale: dice.range(20.0, 36.0),
                seed: dice.seed(),
            },
            Finish::Coated { roughness: 0.75 },
        )
        .with_relief(Relief::Grain {
            depth: 0.12,
            scale: 6.0,
            seed: dice.seed(),
        }),
    )
}

/// Paving for a floor: tiles in two stones, or flagstones.
fn paving(stage: &mut Stage, dice: &mut Dice) -> Option<usize> {
    let pigment = if dice.chance(0.55) {
        let (a, b) = dice.pick(&[
            (0xE8_E4_DC, 0xB0_AA_A0),
            (0xE8_DC_C8, 0x9A_76_58),
            (0xD8_D4_CC, 0x5A_5A_5E),
            (0xE0_C8_B0, 0xB8_5A_44),
        ])?;
        Pigment::Tiles {
            a: rgb(a),
            b: rgb(b),
            grout: rgb(0x70_6C_66),
            size: dice.range(0.8, 1.5),
            gap: 0.025,
            seed: dice.seed(),
        }
    } else {
        Pigment::Stones {
            a: rgb(0xC8_BC_A8),
            b: rgb(0x9A_90_80),
            joint: rgb(0x5A_54_4A),
            size: dice.range(0.7, 1.3),
            gap: 0.03,
            seed: dice.seed(),
        }
    };
    stage.coated(pigment, dice.pick(&[0.12, 0.3, 0.5])?)
}

/// What a building stands on.
#[allow(
    clippy::large_enum_variant,
    reason = "held once while a building is set out, and a box could not fail gracefully"
)]
enum Footing {
    /// Tiles to the horizon.
    Endless,
    /// A paved plaza on open land, with trees about it.
    Plaza(Backdrop),
}

/// The ground a building `half_x` by `half_z` stands on.
fn setting(stage: &mut Stage, dice: &mut Dice, (half_x, half_z): (f64, f64)) -> Option<Footing> {
    let paved = paving(stage, dice)?;
    if dice.chance(0.3) {
        stage.ground(0.0, paved)?;
        return Some(Footing::Endless);
    }
    let reach = half_x.max(half_z);
    let soil = if dice.chance(0.6) { GREEN } else { GOLDEN };
    let backdrop = landscape::backdrop(
        stage,
        dice,
        (reach + 12.0, -PLINTH),
        &soil,
        (
            &[Kind::Oak, Kind::Olive, Kind::Poplar, Kind::Cherry],
            Season::Spring,
        ),
    )?;
    stage.block(
        Vec3::UP * -(PLINTH + 0.4),
        Vec3::new(half_x, 0.5 * PLINTH + 0.2, half_z),
        0.0,
        paved,
    )?;
    Some(Footing::Plaza(backdrop))
}

/// The orders a column is built in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Order {
    /// Straight from the floor to a cushion and a square slab.
    Doric,
    /// On a moulded base, under a scrolled capital.
    Ionic,
    /// On a plain plinth, under a plain block.
    Tuscan,
}

impl Order {
    const ALL: [Self; 3] = [Self::Doric, Self::Ionic, Self::Tuscan];
}

/// A column of `order` standing on `base`, its shaft `height` tall and
/// `radius` thick, turned `yaw` so that its capital's long side lies along
/// the beam it carries: the height of its top.
fn column(
    stage: &mut Stage,
    dice: &mut Dice,
    base: Vec3,
    (radius, height): (f64, f64),
    order: Order,
    yaw: f64,
    stone: usize,
) -> Option<f64> {
    stage.claim((base.x, base.z), 1.5 * radius)?;
    let mut level = base.y;
    match order {
        Order::Doric => {}
        Order::Ionic => {
            stage.block(
                base,
                Vec3::new(1.4 * radius, 0.35 * radius, 1.4 * radius),
                yaw,
                stone,
            )?;
            level += 0.7 * radius;
            let torus = Vec3::new(base.x, level + 0.16 * radius, base.z);
            stage.ring(torus, Frame::WORLD, (1.02 * radius, 0.16 * radius), stone)?;
        }
        Order::Tuscan => {
            stage.block(
                base,
                Vec3::new(1.3 * radius, 0.25 * radius, 1.3 * radius),
                yaw,
                stone,
            )?;
            level += 0.5 * radius;
        }
    }
    let taper = if order == Order::Doric { 0.78 } else { 0.86 };
    stage.post(
        Vec3::new(base.x, level, base.z),
        (radius, taper * radius, height),
        stone,
        dice,
    )?;
    level += height;
    let neck = taper * radius;
    match order {
        Order::Doric => {
            stage.ring(
                Vec3::new(base.x, level + 0.12 * radius, base.z),
                Frame::WORLD,
                (neck, 0.24 * radius),
                stone,
            )?;
            stage.block(
                Vec3::new(base.x, level + 0.2 * radius, base.z),
                Vec3::new(1.25 * radius, 0.16 * radius, 1.25 * radius),
                yaw,
                stone,
            )?;
            Some(level + 0.52 * radius)
        }
        Order::Ionic => {
            volutes(stage, Vec3::new(base.x, level, base.z), radius, yaw, stone)?;
            stage.block(
                Vec3::new(base.x, level, base.z),
                Vec3::new(1.3 * radius, 0.22 * radius, radius),
                yaw,
                stone,
            )?;
            Some(level + 0.44 * radius)
        }
        Order::Tuscan => {
            stage.block(
                Vec3::new(base.x, level, base.z),
                Vec3::new(1.2 * radius, 0.2 * radius, 1.2 * radius),
                yaw,
                stone,
            )?;
            Some(level + 0.4 * radius)
        }
    }
}

/// An Ionic capital's scrolls, under an abacus standing on `neck` and
/// turned `yaw`: one at each end of both broad faces, hanging beside the
/// shaft, a bolster running between each pair.
fn volutes(stage: &mut Stage, neck: Vec3, radius: f64, yaw: f64, stone: usize) -> Option<()> {
    let (across, facing) = (Frame::turned(yaw, 0.0), Frame::turned(yaw, FRAC_PI_2));
    for end in [-1.0, 1.0] {
        let eye = neck + Vec3::UP * (0.04 * radius) + across.x * (end * 1.05 * radius);
        let (front, back) = (eye + across.z * radius, eye - across.z * radius);
        stage.limb(back, front, (0.2 * radius, 0.2 * radius), stone)?;
        for face in [front, back] {
            stage.ring(face, facing, (0.2 * radius, 0.07 * radius), stone)?;
        }
    }
    Some(())
}

/// Which way a capital in row `j` of a grid of columns `along` rows deep is
/// turned: along the beam over it, a corner's with the front's.
fn grid_yaw(j: u32, along: u32) -> f64 {
    if j == 0 || j + 1 == along {
        0.0
    } else {
        FRAC_PI_2
    }
}

pub(super) fn colonnade(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    match dice.count(0, 2) {
        0 => avenue(stage, dice),
        1 => peristyle(stage, dice),
        _ => stoa(stage, dice),
    }
}

/// Two rows of columns, each carrying its beam, seen down their length.
fn avenue(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let columns = dice.count(4, 7);
    let spacing = dice.range(2.6, 3.4);
    let half_width = dice.range(2.6, 3.4);
    let shaft = dice.range(3.6, 4.6);
    let radius = dice.range(0.3, 0.38);
    let length = spacing * f64::from(columns - 1);
    let land = setting(stage, dice, (half_width + 3.0, 0.5 * length + 4.0))?;
    let stone = masonry(stage, dice)?;
    let order = dice.pick(&Order::ALL)?;
    for row in [-1.0, 1.0] {
        let x = row * half_width;
        let mut top = 0.0;
        for index in 0..columns {
            let z = -0.5 * length + spacing * f64::from(index);
            top = column(
                stage,
                dice,
                Vec3::new(x, 0.0, z),
                (radius, shaft),
                order,
                FRAC_PI_2,
                stone,
            )?;
        }
        stage.block(
            Vec3::new(x, top, 0.0),
            Vec3::new(1.3 * radius, 0.28, 0.5 * length + 0.6),
            0.0,
            stone,
        )?;
    }
    let centre = Vec3::new(0.0, 0.0, dice.range(-0.2, 0.3) * length);
    centrepiece(stage, dice, centre)?;
    // Down the colonnade from one end, the sun across it.
    let end = -0.5 * length - dice.range(3.5, 5.5);
    let eye = Vec3::new(dice.range(-0.6, 0.6), dice.range(1.4, 2.2), end);
    let target = Vec3::new(0.0, dice.range(1.0, 1.6), 0.25 * length);
    let fov = dice.angle(46.0, 58.0);
    finish(stage, dice, land, (eye, target), fov)
}

/// Columns all round a court, carrying one beam about it, a statue in its
/// middle: seen from outside a corner.
fn peristyle(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let (along, across) = (dice.count(4, 6), dice.count(3, 4));
    let spacing = dice.range(2.4, 3.0);
    let (half_x, half_z) = (
        0.5 * spacing * f64::from(across - 1),
        0.5 * spacing * f64::from(along - 1),
    );
    let shaft = dice.range(3.2, 4.2);
    let radius = dice.range(0.26, 0.34);
    let land = setting(stage, dice, (half_x + 3.0, half_z + 3.0))?;
    let stone = masonry(stage, dice)?;
    let order = dice.pick(&Order::ALL)?;
    let mut top = 0.0;
    for i in 0..across {
        for j in 0..along {
            if i != 0 && i + 1 != across && j != 0 && j + 1 != along {
                continue;
            }
            let x = -half_x + spacing * f64::from(i);
            let z = -half_z + spacing * f64::from(j);
            top = column(
                stage,
                dice,
                Vec3::new(x, 0.0, z),
                (radius, shaft),
                order,
                grid_yaw(j, along),
                stone,
            )?;
        }
    }
    let beam = 1.3 * radius;
    for side in [-1.0, 1.0] {
        stage.block(
            Vec3::new(side * half_x, top, 0.0),
            Vec3::new(beam, 0.3, half_z + beam),
            0.0,
            stone,
        )?;
        stage.block(
            Vec3::new(0.0, top, side * half_z),
            Vec3::new(half_x - beam, 0.3, beam),
            0.0,
            stone,
        )?;
    }
    centrepiece(stage, dice, Vec3::ZERO)?;
    let heading = dice.range(0.0, TAU);
    let distance = (half_x.max(half_z) + 4.0) * dice.range(1.5, 2.2);
    let eye = Vec3::new(
        -mathf::sin(heading) * distance,
        dice.range(1.6, 3.5),
        -mathf::cos(heading) * distance,
    );
    let target = Vec3::UP * (0.45 * shaft);
    let fov = dice.angle(40.0, 52.0);
    finish(stage, dice, land, (eye, target), fov)
}

/// A row of columns before a wall, under a roof: a porch seen from along it.
fn stoa(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let columns = dice.count(5, 8);
    let spacing = dice.range(2.4, 3.0);
    let length = spacing * f64::from(columns - 1);
    let depth = dice.range(3.0, 4.0);
    let shaft = dice.range(3.4, 4.2);
    let radius = dice.range(0.26, 0.32);
    let land = setting(stage, dice, (0.5 * length + 3.0, depth + 3.0))?;
    let stone = masonry(stage, dice)?;
    let order = dice.pick(&Order::ALL)?;
    let mut top = 0.0;
    for index in 0..columns {
        let x = -0.5 * length + spacing * f64::from(index);
        top = column(
            stage,
            dice,
            Vec3::new(x, 0.0, 0.0),
            (radius, shaft),
            order,
            0.0,
            stone,
        )?;
    }
    let wall = stage.material(Material::new(
        Pigment::Bricks {
            a: rgb(dice.pick(&[0xB0_5A_3C, 0xC8_A0_78, 0x9A_8A_7A])?),
            b: rgb(0x8A_4A_30),
            mortar: rgb(0xC8_C0_B0),
            size: (0.45, 0.18),
            seed: dice.seed(),
        },
        Finish::Coated { roughness: 0.8 },
    ))?;
    stage.claim((0.0, -depth), 0.5 * length)?;
    stage.block(
        Vec3::new(0.0, 0.0, -depth),
        Vec3::new(0.5 * length + 0.6, 0.5 * top, 0.3),
        0.0,
        wall,
    )?;
    stage.block(
        Vec3::new(0.0, top, -0.5 * depth),
        Vec3::new(0.5 * length + 0.8, 0.25, 0.5 * depth + 0.8),
        0.0,
        stone,
    )?;
    if dice.chance(0.6) {
        let hero = stage.precious(dice)?;
        let spot = Vec3::new(dice.range(-0.3, 0.3) * length, 0.0, -0.5 * depth);
        stage.post(spot, (0.35, 0.3, 0.9), stone, dice)?;
        stage.ball(spot + Vec3::UP * 0.9, 0.35, hero, dice)?;
    }
    let side = dice.sign();
    let eye = Vec3::new(
        side * (0.5 * length + dice.range(3.0, 6.0)),
        dice.range(1.5, 2.2),
        dice.range(3.0, 6.0),
    );
    let target = Vec3::new(-side * 0.15 * length, 0.4 * shaft, -0.4 * depth);
    let fov = dice.angle(46.0, 58.0);
    finish(stage, dice, land, (eye, target), fov)
}

/// A statue of something precious on a pedestal at `centre`.
fn centrepiece(stage: &mut Stage, dice: &mut Dice, centre: Vec3) -> Option<()> {
    let plinth = stage.plinth_stone(dice)?;
    stage.claim((centre.x, centre.z), 0.9)?;
    let height = dice.range(0.8, 1.2);
    stage.post(centre, (0.55, 0.42, height), plinth, dice)?;
    let hero = stage.precious(dice)?;
    let top = centre + Vec3::UP * height;
    if dice.chance(0.25) {
        stage.gem(
            top,
            dice.pick(&super::Cut::ALL)?,
            0.5,
            dice.range(0.0, TAU),
            hero,
        )?;
    } else {
        stage.ball(top, dice.range(0.4, 0.55), hero, dice)?;
    }
    Some(())
}

/// The weather, and a camera at `eye` looking at `target`, over the
/// building on its `footing`.
fn finish(
    stage: &mut Stage,
    dice: &mut Dice,
    footing: Footing,
    (eye, target): (Vec3, Vec3),
    fov: f64,
) -> Option<Composed> {
    let facing = mathf::atan2(target.x - eye.x, target.z - eye.z);
    let weather = weather::outdoors(stage, dice, &MONUMENT, facing)?;
    let look = Look {
        sky: weather.sky,
        exposure: weather.exposure,
        view: View::Placed {
            eye,
            target,
            fov,
            aperture: 0.0,
        },
    };
    Some(match footing {
        Footing::Endless => Composed::Seen(look),
        Footing::Plaza(land) => land.seen(
            look,
            Vantage {
                eye,
                heading: facing,
            },
            None,
        ),
    })
}

pub(super) fn arcade(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    if dice.chance(0.45) {
        aqueduct(stage, dice)
    } else {
        loggia(stage, dice)
    }
}

/// Arches on piers along a plaza, and a cornice over them.
fn loggia(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let bays = dice.count(4, 7);
    let span = dice.range(2.4, 3.6);
    let length = span * f64::from(bays);
    let pier = dice.range(0.28, 0.4);
    let rise = dice.range(2.2, 3.4);
    let rows = if dice.chance(0.4) { 2 } else { 1 };
    let gap = dice.range(3.0, 4.2);
    let land = setting(stage, dice, (0.5 * length + 3.0, gap + 3.0))?;
    let stone = masonry(stage, dice)?;
    let mut crown = 0.0;
    for row in 0..rows {
        let z = -gap * f64::from(row);
        crown = arches(
            stage,
            (Vec3::new(-0.5 * length, 0.0, z), 0.0),
            (bays, span),
            (pier, rise),
            stone,
        )?;
        stage.block(
            Vec3::new(0.0, crown, z),
            Vec3::new(0.5 * length + pier, 0.3, pier),
            0.0,
            stone,
        )?;
    }
    if rows == 2 {
        let roof = Vec3::new(0.0, crown + 0.6, -0.5 * gap);
        stage.block(
            roof,
            Vec3::new(0.5 * length + pier, 0.15, 0.5 * gap + pier),
            0.0,
            stone,
        )?;
    }
    if dice.chance(0.5) {
        let spot = Vec3::new(dice.range(-0.3, 0.3) * length, 0.0, 3.0);
        centrepiece(stage, dice, spot)?;
    }
    let side = dice.sign();
    let eye = Vec3::new(
        side * dice.range(0.3, 0.7) * length,
        dice.range(1.5, 2.4),
        dice.range(7.0, 11.0),
    );
    let target = Vec3::new(
        -side * 0.1 * length,
        0.55 * rise,
        -0.3 * gap * f64::from(rows - 1),
    );
    let fov = dice.angle(46.0, 58.0);
    finish(stage, dice, land, (eye, target), fov)
}

/// A row of `bays` arches `span` apart, on piers `pier` thick and `rise`
/// tall standing at `from`'s level, running from `from` at right angles to
/// `heading`, so each arch opens along it: the height of their crowns.
fn arches(
    stage: &mut Stage,
    (from, heading): (Vec3, f64),
    (bays, span): (u32, f64),
    (pier, rise): (f64, f64),
    stone: usize,
) -> Option<f64> {
    let along = direction(heading, FRAC_PI_2, 0.0);
    let springing = from.y + rise;
    let end = from + along * (span * f64::from(bays));
    stage.claim_along((from.x, from.z), (end.x, end.z), pier)?;
    for index in 0..=bays {
        let at = from + along * (span * f64::from(index));
        stage.claim((at.x, at.z), pier)?;
        stage.block(
            Vec3::new(at.x, from.y, at.z),
            Vec3::new(pier, 0.5 * rise, pier),
            heading,
            stone,
        )?;
        if index < bays {
            let middle = at + along * (0.5 * span);
            stage.arch(
                Vec3::new(middle.x, springing, middle.z),
                heading,
                (span, 1.8 * pier),
                stone,
            )?;
        }
    }
    Some(springing + 0.5 * span + 0.9 * pier)
}

const VALLEY: Climate = Climate {
    hours: &[
        (Hour::Day, 4),
        (Hour::Golden, 4),
        (Hour::Sunset, 2),
        (Hour::Noon, 1),
    ],
    covers: &[
        (Cover::Clear, 2),
        (Cover::Fair, 5),
        (Cover::Broken, 2),
        (Cover::Cirrus, 2),
    ],
    haze: (1.0, 2.2),
    base: 300.0,
    albedo: 0.18,
};

/// An aqueduct striding across a river valley on two tiers of arches: the
/// valley's line and breadth, and how high its hills stand.
#[derive(Copy, Clone, Debug)]
pub(super) struct Aqueduct {
    heading: f64,
    floor: f64,
    height: f64,
}

fn aqueduct(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let heading = dice.range(0.0, TAU);
    let height = dice.range(40.0, 70.0);
    let floor = dice.range(40.0, 70.0);
    let reach = 2600.0;
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
        tilt: (0.0, 0.0),
        clearing: None,
    };
    let plan = Plan {
        relief,
        reach,
        sea: None,
        wear: Wear {
            passes: 12,
            incision: 4.0e-4,
            creep: 0.08,
            repose: 0.9,
            infill: 0.3,
            strata: None,
        },
        rivers: Some(Rivers {
            catchment: 5.0e5,
            width: 5.0,
            meander: 1.1,
        }),
        road: None,
        roughness: 1.0,
        ridges: 0.35,
        droplets: 0.05,
        cells: (256, 1024),
        nests: landscape::nests((700.0, 80.0), (0.08, 0.12)),
        horizon: Some(landscape::horizon(reach)),
        snow_line: None,
        pond: None,
        growth: 1.0,
        seed: dice.seed(),
    };
    let material = landscape::ground(stage, dice, &GREEN, (-1e3, 0.55 * height + 400.0, 0.7), 3.0)?;
    let water = landscape::river(stage, dice)?;
    let build = landscape::lay(stage, plan, material, Some(water))?;
    Some(Composed::Landed(Landing {
        build,
        scheme: Scheme::Aqueduct(Aqueduct {
            heading,
            floor,
            height,
        }),
        vantage: None,
    }))
}

impl Aqueduct {
    /// From down the valley, off to one side, the crossing across the view.
    pub(super) fn site(&self, survey: &Survey<'_>, dice: &mut Dice) -> Vantage {
        let across = self.heading + FRAC_PI_2;
        let back = dice.range(120.0, 220.0);
        let aside = dice.sign() * dice.range(40.0, 90.0);
        let (x, z) = (
            -back * mathf::sin(self.heading) + aside * mathf::sin(across),
            -back * mathf::cos(self.heading) + aside * mathf::cos(across),
        );
        Vantage {
            eye: landscape::stand(survey, (x, z), dice.range(2.0, 8.0)),
            heading: mathf::atan2(-x, -z),
        }
    }

    /// The aqueduct built across the valley of `land`, the scene about it,
    /// and the view of it from `vantage`.
    pub(super) fn finish(
        self,
        stage: &mut Stage,
        dice: &mut Dice,
        land: &Land,
        vantage: Vantage,
    ) -> Option<Look> {
        let deck = dice.range(0.45, 0.62) * self.height;
        let (left, right, lowest) = self.crossing(&|x, z| land.height(&stage.fields, x, z), deck);
        let (middle, top) = bridge(stage, dice, (left, right, lowest), (self.heading, deck))?;
        let eye = vantage.eye;
        let target = Vec3::new(middle.x, 0.45 * top, middle.z);
        let character = if dice.chance(0.5) {
            Character::Meadow
        } else {
            Character::Upland
        };
        let lawning = Lawning {
            eye: (eye.x, eye.z),
            grassland: plants::grassland(dice, character, Season::Summer),
        };
        let grove = Grove::new(
            stage,
            dice,
            (&[Kind::Oak, Kind::Poplar, Kind::Olive], Season::Summer),
            Stand::Open,
        )?;
        let facing = mathf::atan2(target.x - eye.x, target.z - eye.z);
        let seen = Vantage {
            eye,
            heading: facing,
        };
        landscape::plant(stage, dice, (grove, seen), Some(&lawning), ANYWHERE)?;
        let weather = weather::outdoors(stage, dice, &VALLEY, facing)?;
        Some(Look {
            sky: weather.sky,
            exposure: weather.exposure,
            view: View::Placed {
                eye,
                target,
                fov: dice.angle(44.0, 56.0),
                aperture: 0.0,
            },
        })
    }

    /// Where the valley's sides rise to `deck` either side of its middle,
    /// across it, on the land `height` gives; and the lowest the land lies
    /// between them.
    fn crossing(&self, height: &dyn Fn(f64, f64) -> f64, deck: f64) -> (f64, f64, f64) {
        let across = self.heading + FRAC_PI_2;
        let mut lowest = f64::INFINITY;
        let mut reach = |sign: f64| {
            let mut distance = 0.0;
            while distance < 6.0 * self.floor {
                let (x, z) = (
                    sign * distance * mathf::sin(across),
                    sign * distance * mathf::cos(across),
                );
                let ground = height(x, z);
                // Every sample counts, so a middle already above the deck still
                // founds the piers on finite ground.
                lowest = lowest.min(ground);
                if ground > deck {
                    return distance;
                }
                distance += 2.0;
            }
            distance
        };
        let (left, right) = (reach(-1.0), reach(1.0));
        (left, right, lowest)
    }
}

/// Two tiers of arches carrying a channel at `deck` across a valley running
/// along `heading`, from `left` of its middle to `right` of it, their piers
/// founded below `lowest`: the middle of the crossing, and the height of its
/// top.
fn bridge(
    stage: &mut Stage,
    dice: &mut Dice,
    (left, right, lowest): (f64, f64, f64),
    (heading, deck): (f64, f64),
) -> Option<(Vec3, f64)> {
    let across = heading + FRAC_PI_2;
    let span = dice.range(14.0, 22.0);
    let bays = u32::try_from(mathf::round_i32(((left + right) / span).max(3.0)))
        .ok()?
        .min(24);
    let length = span * f64::from(bays);
    let line = direction(across, 0.0, 0.0);
    let middle = line * (0.5 * (right - left));
    let start = middle - line * (0.5 * length);
    let stone = masonry(stage, dice)?;
    let pier = 0.09 * span;
    let bed = lowest - 1.5;
    let springing = deck - 0.5 * span - 0.9 * pier;
    let lower = Vec3::new(start.x, bed, start.z);
    let crown = arches(
        stage,
        (lower, heading),
        (bays, span),
        (pier, (springing - bed).max(2.0)),
        stone,
    )?;
    let along = Frame::turned(across, 0.0);
    let channel = Vec3::new(1.2 * pier, 0.4, 0.5 * length + pier);
    stage.slab(
        Pose::new(Vec3::new(middle.x, crown + 0.4, middle.z), along),
        channel,
        stone,
    )?;
    let upper = Vec3::new(start.x, crown + 0.8, start.z);
    let top = arches(
        stage,
        (upper, heading),
        (bays * 2, 0.5 * span),
        (0.6 * pier, (0.5 * span).max(6.0)),
        stone,
    )?;
    let conduit = Vec3::new(0.9 * pier, 0.8, 0.5 * length + pier);
    stage.slab(
        Pose::new(Vec3::new(middle.x, top + 0.8, middle.z), along),
        conduit,
        stone,
    )?;
    Some((middle, top))
}

pub(super) fn rotunda(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let radius = dice.range(3.0, 4.6);
    let columns = dice.count(8, 14);
    let shaft = dice.range(3.4, 4.6);
    let thick = dice.range(0.24, 0.32);
    let steps = dice.count(2, 4);
    let tread = 0.45;
    let base = radius + 1.0 + tread * f64::from(steps);
    let land = setting(stage, dice, (base + 2.0, base + 2.0))?;
    let stone = masonry(stage, dice)?;
    for step in 0..steps {
        let reach = base - tread * f64::from(step);
        let level = 0.2 * f64::from(step);
        stage.post(Vec3::UP * level, (reach, reach, 0.2), stone, dice)?;
    }
    let floor = 0.2 * f64::from(steps);
    let order = dice.pick(&Order::ALL)?;
    let turn = dice.range(0.0, TAU);
    let mut top = floor;
    for index in 0..columns {
        let angle = turn + TAU * f64::from(index) / f64::from(columns);
        let at = Vec3::new(
            radius * mathf::sin(angle),
            floor,
            radius * mathf::cos(angle),
        );
        top = column(stage, dice, at, (thick, shaft), order, angle, stone)?;
    }
    stage.ring(Vec3::UP * (top + 0.3), Frame::WORLD, (radius, 0.42), stone)?;
    let roof = match dice.count(0, 3) {
        0 => stage.material(
            Material::new(
                Pigment::Solid(rgb(0x5E_9E_8A)),
                Finish::Coated { roughness: 0.55 },
            )
            .with_relief(Relief::Grain {
                depth: 0.15,
                scale: 3.0,
                seed: dice.seed(),
            }),
        )?,
        1 => stage.metal(GOLD, 0.25)?,
        2 => stage.metal(COPPER, 0.3)?,
        _ => stone,
    };
    stage.dome(Vec3::UP * (top + 0.5), radius + 0.35, roof)?;
    let crest = top + 0.5 + radius + 0.35;
    stage.post(Vec3::UP * (crest - 0.05), (0.18, 0.12, 0.6), roof, dice)?;
    stage.ball(Vec3::UP * (crest + 0.5), 0.2, roof, dice)?;
    centrepiece(stage, dice, Vec3::UP * floor)?;
    let heading = dice.range(0.0, TAU);
    let distance = base * dice.range(2.6, 3.6);
    let eye = Vec3::new(
        -mathf::sin(heading) * distance,
        dice.range(1.6, 4.0),
        -mathf::cos(heading) * distance,
    );
    let target = Vec3::UP * (0.5 * crest);
    let fov = dice.angle(40.0, 52.0);
    finish(stage, dice, land, (eye, target), fov)
}

const WILD: Climate = Climate {
    hours: &[
        (Hour::Day, 3),
        (Hour::Golden, 5),
        (Hour::Sunset, 2),
        (Hour::Noon, 1),
    ],
    covers: &[
        (Cover::Clear, 2),
        (Cover::Fair, 4),
        (Cover::Broken, 3),
        (Cover::Cirrus, 2),
        (Cover::Overcast, 1),
    ],
    haze: (1.2, 2.6),
    base: 200.0,
    albedo: 0.18,
};

/// The ruin of a temple: some columns standing, some broken, some fallen,
/// stones strewn about and the grass grown up among them.
pub(super) fn ruins(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let (along, across) = (dice.count(4, 6), dice.count(2, 4));
    let spacing = dice.range(2.6, 3.4);
    let (half_x, half_z) = (
        0.5 * spacing * f64::from(across - 1),
        0.5 * spacing * f64::from(along - 1),
    );
    let soil = if dice.chance(0.5) { GREEN } else { GOLDEN };
    let land = landscape::backdrop(
        stage,
        dice,
        (half_x.max(half_z) + 14.0, 0.0),
        &soil,
        (
            &[Kind::Oak, Kind::Olive, Kind::Poplar],
            Season::Autumn { fallen: 10 },
        ),
    )?;
    let stone = masonry(stage, dice)?;
    let shaft = dice.range(3.6, 4.6);
    temple_ruin(stage, dice, ((along, across), spacing), shaft, stone)?;
    for _ in 0..dice.count(4, 10) {
        let size = dice.range(0.25, 0.7);
        let Some((x, z)) = stage.place(dice, ((0.0, 0.0), half_x.max(half_z) + 5.0), size) else {
            continue;
        };
        let pose = Pose::new(
            Vec3::new(x, land.terrain.height(x, z) + 0.3 * size, z),
            Frame::turned(dice.range(0.0, TAU), dice.range(-0.35, 0.35)),
        );
        stage.slab(
            pose,
            Vec3::new(
                size,
                dice.range(0.4, 0.7) * size,
                dice.range(0.6, 1.2) * size,
            ),
            stone,
        )?;
    }
    let hedges = Grove::new(stage, dice, (&[Kind::Box], Season::Summer), Stand::Open)?;
    let bush = hedges.of(Kind::Box)?;
    for _ in 0..dice.count(2, 5) {
        let Some((x, z)) = stage.place(dice, ((0.0, 0.0), half_x.max(half_z) + 6.0), 0.8) else {
            continue;
        };
        let height = dice.range(0.8, 1.6);
        plants::plant(
            stage,
            dice,
            &bush,
            Vec3::new(x, land.terrain.height(x, z), z),
            height,
        )?;
    }
    let reach = half_x.max(half_z) + 10.0;
    let heading = dice.range(0.0, TAU);
    let distance = reach * dice.range(0.9, 1.3);
    let (x, z) = (
        -mathf::sin(heading) * distance,
        -mathf::cos(heading) * distance,
    );
    let eye = Vec3::new(x, land.terrain.height(x, z) + dice.range(1.3, 2.4), z);
    let character = if dice.chance(0.5) {
        Character::Meadow
    } else {
        Character::Upland
    };
    let lawning = Lawning {
        eye: (eye.x, eye.z),
        grassland: Grassland {
            fallen: land.fallen(Season::Autumn { fallen: 10 }, 0.8),
            ..plants::grassland(dice, character, Season::Summer)
        },
    };
    let target = Vec3::UP * (0.3 * shaft);
    let facing = mathf::atan2(-x, -z);
    let weather = weather::outdoors(stage, dice, &WILD, facing)?;
    let look = Look {
        sky: weather.sky,
        exposure: weather.exposure,
        view: View::Placed {
            eye,
            target,
            fov: dice.angle(46.0, 58.0),
            aperture: 0.0,
        },
    };
    Some(land.seen(
        look,
        Vantage {
            eye,
            heading: facing,
        },
        Some(&lawning),
    ))
}

/// What is left of a temple of `along` by `across` columns `spacing` apart,
/// their shafts `shaft` tall: a few worn slabs of its platform, some columns
/// standing, some broken with their drums fallen beside them, some fallen
/// whole, and here and there a lintel still across two that stand.
fn temple_ruin(
    stage: &mut Stage,
    dice: &mut Dice,
    ((along, across), spacing): ((u32, u32), f64),
    shaft: f64,
    stone: usize,
) -> Option<()> {
    let (half_x, half_z) = (
        0.5 * spacing * f64::from(across - 1),
        0.5 * spacing * f64::from(along - 1),
    );
    let order = dice.pick(&Order::ALL)?;
    let radius = dice.range(0.3, 0.38);
    for _ in 0..dice.count(2, 4) {
        let at = Vec3::new(
            dice.range(-half_x, half_x),
            -0.35,
            dice.range(-half_z, half_z),
        );
        let half = Vec3::new(
            dice.range(1.0, 2.2),
            dice.range(0.25, 0.45),
            dice.range(1.0, 2.2),
        );
        stage.block(at, half, dice.range(-0.1, 0.1), stone)?;
    }
    let mut standing: [Option<(Vec3, f64)>; 4] = [None; 4];
    for i in 0..across {
        for j in 0..along {
            let at = Vec3::new(
                -half_x + spacing * f64::from(i),
                0.0,
                -half_z + spacing * f64::from(j),
            );
            match dice.count(0, 9) {
                0..=2 => {
                    let yaw = grid_yaw(j, along);
                    let top = column(stage, dice, at, (radius, shaft), order, yaw, stone)?;
                    if let Some(slot) = standing.iter_mut().find(|slot| slot.is_none()) {
                        *slot = Some((at, top));
                    }
                }
                3..=5 => {
                    stage.claim((at.x, at.z), 1.4 * radius)?;
                    let broken = shaft * dice.range(0.15, 0.7);
                    stage.post(
                        at,
                        (radius, radius * (1.0 - 0.2 * broken / shaft), broken),
                        stone,
                        dice,
                    )?;
                    let fell = dice.range(0.0, TAU);
                    let drum = at + direction(fell, 0.0, 0.0) * dice.range(1.4, 2.6);
                    let length = dice.range(0.6, 1.2);
                    drum_lying(
                        stage,
                        dice,
                        drum,
                        (0.85 * radius, length),
                        fell + FRAC_PI_2,
                        stone,
                    )?;
                }
                6 | 7 => {
                    let fell = dice.range(0.0, TAU);
                    let length = dice.range(1.5, 3.2);
                    drum_lying(stage, dice, at, (radius, length), fell, stone)?;
                }
                _ => {}
            }
        }
    }
    // A lintel still spanning two neighbours that stand.
    if let [Some((a, top_a)), Some((b, _)), ..] = standing {
        if (a - b).length() < 1.2 * spacing {
            let middle = (a + b) * 0.5;
            let heading = mathf::atan2(b.x - a.x, b.z - a.z);
            let half = Vec3::new(1.2 * radius, 0.3, 0.5 * (a - b).length() + radius);
            stage.slab(
                Pose::new(
                    Vec3::new(middle.x, top_a + 0.3, middle.z),
                    Frame::turned(heading, 0.0),
                ),
                half,
                stone,
            )?;
        }
    }
    Some(())
}

/// A column's drum lying on its side at `at`, `radius` thick and `length`
/// long, along `heading`.
fn drum_lying(
    stage: &mut Stage,
    dice: &mut Dice,
    at: Vec3,
    (radius, length): (f64, f64),
    heading: f64,
    stone: usize,
) -> Option<()> {
    if !stage.clear((at.x, at.z), 0.5 * length) {
        return Some(());
    }
    stage.claim((at.x, at.z), 0.5 * length)?;
    let lying = Frame::turned(heading, FRAC_PI_2 + dice.range(-0.05, 0.05));
    let start = at - lying.y * (0.5 * length) + Vec3::UP * (0.92 * radius);
    stage.frustum(
        Pose::new(start, lying),
        (radius, radius, length),
        stone,
        true,
    )?;
    Some(())
}

#[cfg(test)]
#[path = "architecture_tests.rs"]
mod tests;
