//! Buildings: colonnades, arcades and aqueducts, rotundas, and ruins, each on
//! a plaza or open ground and seen under the day's weather.

use alloc::vec::Vec;
use core::f64::consts::{FRAC_PI_2, TAU};

use tairix_util::mathf;

use super::courses::{
    brick_courses, Annulus, Bond, Clay, Column, Dressing, Mason, Opening, Order, Quarry, Ring,
    Stonework, Wall, Weathering,
};
use super::landscape::{self, Backdrop, Lawning, Scheme, Vantage, GOLDEN, GREEN};
use super::plants::{self, Character, Grassland, Grove, Kind, Stand};
use super::weather::{self, Climate, Cover, Hour};
use super::{direction, rgb, Composed, Dice, Landing, Look, Stage, View, COPPER, GOLD};
use crate::course::Mark;
use crate::land::{Land, Plan, Rivers, Survey, Wear, CUT_SLOPE, DEEPEST_CUT};
use crate::material::{Finish, Material, Relief};
use crate::pigment::Pigment;
use crate::solid::Form;
use crate::terrain::{Landform, Terrain};
use crate::tree::Season;
use crate::vector::{Frame, Pose, Vec3};
use crate::wood::ANYWHERE;

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

/// How weathered a monument stands, from kept to long neglected, and a
/// ruin.
const KEPT: (f64, f64) = (0.2, 0.65);
const RUINED: (f64, f64) = (0.8, 1.0);

/// The stone a monument is built in, `age` weathered where it stands, its
/// floor `foot` high.
fn building_stone(stage: &mut Stage, dice: &mut Dice, age: f64, foot: f64) -> Option<Stonework> {
    let quarry = dice.pick(&[
        Quarry::Marble,
        Quarry::Marble,
        Quarry::Limestone,
        Quarry::Limestone,
        Quarry::Sandstone,
        Quarry::RedSandstone,
        Quarry::Granite,
    ])?;
    let exposure = Weathering {
        damp: dice.range(0.15, 0.5),
        drought: dice.range(0.2, 0.6),
        foot,
    };
    stage.stonework(dice, quarry, (age, exposure))
}

/// Tiles to the horizon: two stones in squares, or flagstones.
fn endless_paving(stage: &mut Stage, dice: &mut Dice) -> Option<usize> {
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

/// The ground a building `half_x` by `half_z` stands on, a building `age`
/// weathered: tiles to the horizon, or a plaza on open land, its flags laid
/// over a bed behind a kerb.
fn setting(
    stage: &mut Stage,
    dice: &mut Dice,
    (half_x, half_z): (f64, f64),
    age: f64,
) -> Option<Footing> {
    if dice.chance(0.3) {
        let paved = endless_paving(stage, dice)?;
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
    let quarry = dice.pick(&[Quarry::Limestone, Quarry::Granite, Quarry::Sandstone])?;
    let exposure = Weathering {
        damp: dice.range(0.2, 0.5),
        drought: dice.range(0.2, 0.6),
        foot: 0.25,
    };
    let work = stage.stonework(dice, quarry, (age, exposure))?;
    let mut mason = Mason::new(work, dice.seed())?;
    plaza(&mut mason, dice, (half_x, half_z))?;
    stage.raise(mason, Pose::new(Vec3::ZERO, Frame::WORLD), dice.seed())?;
    Some(Footing::Plaza(backdrop))
}

/// A plaza `half_x` by `half_z` its floor at nought: a kerb of two courses
/// round it from below the ground about it, and flags laid in rows within,
/// each settled a little its own way.
fn plaza(mason: &mut Mason, dice: &mut Dice, (half_x, half_z): (f64, f64)) -> Option<()> {
    let (depth, kerb) = (PLINTH + 0.4, 0.4);
    for (centre, length, heading) in [
        (
            Vec3::new(0.0, -depth, half_z - 0.5 * kerb),
            2.0 * half_x,
            FRAC_PI_2,
        ),
        (
            Vec3::new(0.0, -depth, -half_z + 0.5 * kerb),
            2.0 * half_x,
            -FRAC_PI_2,
        ),
        (
            Vec3::new(half_x - 0.5 * kerb, -depth, 0.0),
            2.0 * half_z - 2.0 * kerb,
            0.0,
        ),
        (
            Vec3::new(-half_x + 0.5 * kerb, -depth, 0.0),
            2.0 * half_z - 2.0 * kerb,
            core::f64::consts::PI,
        ),
    ] {
        mason.wall(&Wall {
            pose: Pose::new(centre, Frame::turned(heading - FRAC_PI_2, 0.0)),
            length,
            height: depth,
            thickness: kerb,
            dressing: Dressing::Ashlar,
            rise: (0.3, 0.36),
            long: (2.2, 3.6),
            joint: 0.006,
            openings: &[],
            back: false,
        })?;
    }
    let (inner_x, inner_z) = (half_x - kerb, half_z - kerb);
    let thick = 0.12;
    let mut z = -inner_z;
    let mut row = 0u32;
    while z < inner_z - 0.05 {
        let deep = dice.range(0.45, 0.9).min(inner_z - z);
        let mut x = -inner_x - dice.range(0.0, 0.5);
        while x < inner_x - 0.05 {
            let long = dice.range(0.5, 1.2);
            let to = (x + long).min(inner_x);
            let from = x.max(-inner_x);
            if to - from > 0.08 {
                let settle = Frame::turned(dice.range(-0.004, 0.004), dice.range(-0.008, 0.008));
                let at = Vec3::new(
                    f64::midpoint(from, to),
                    -0.5 * thick + dice.range(-0.004, 0.002),
                    z + 0.5 * deep,
                );
                let half = Vec3::new(0.5 * (to - from) - 0.005, 0.5 * thick, 0.5 * deep - 0.005);
                mason.unit(
                    (Pose::new(at, settle), half),
                    Form::Block { fan: 0 },
                    Dressing::Flag,
                )?;
            }
            x = to;
        }
        z += deep;
        row += 1;
    }
    let bed = Vec3::new(inner_x, 0.5 * (depth - thick) - 0.003, inner_z);
    mason.unit(
        (
            Pose::new(Vec3::UP * -(thick + 0.5 * (depth - thick)), Frame::WORLD),
            bed,
        ),
        Form::Block { fan: 0 },
        Dressing::Mortar,
    )?;
    (row > 0).then_some(())
}

/// The heading a column's capital in row `j` of a grid of columns `along`
/// rows deep faces: along the beam over it, a corner's with the front's.
fn grid_yaw(j: u32, along: u32) -> f64 {
    if j == 0 || j + 1 == along {
        0.0
    } else {
        FRAC_PI_2
    }
}

/// A beam of blocks laid on `level` from `from` to `to`, `half_depth` either
/// side of its line and `height` tall, jointed into `blocks` blocks: an
/// architrave over columns, its joints over each one.
fn beam(
    mason: &mut Mason,
    (from, to): (Vec3, Vec3),
    level: f64,
    (half_depth, height): (f64, f64),
    blocks: u32,
) -> Option<()> {
    let run = Vec3::new(to.x - from.x, 0.0, to.z - from.z);
    let length = run.length();
    if length <= 0.0 {
        return Some(());
    }
    let heading = mathf::atan2(run.x, run.z);
    let frame = Frame::turned(heading - FRAC_PI_2, 0.0);
    let each = length / f64::from(blocks.max(1));
    for index in 0..blocks.max(1) {
        let middle = from + run * ((f64::from(index) + 0.5) / f64::from(blocks.max(1)));
        let half = Vec3::new(0.5 * each - 0.003, 0.5 * height - 0.002, half_depth);
        let at = Vec3::new(middle.x, level + 0.5 * height, middle.z);
        mason.unit(
            (Pose::new(at, frame), half),
            Form::Block { fan: 0 },
            Dressing::Ashlar,
        )?;
    }
    Some(())
}

/// An architrave over a row of columns from `from` to `to`, `bays` between
/// them, and a cornice projecting over it, its joints breaking with the
/// architrave's: the height of its top.
fn entablature(
    mason: &mut Mason,
    (from, to): (Vec3, Vec3),
    level: f64,
    (half_depth, bays): (f64, u32),
) -> Option<f64> {
    let run = (to - from) * (1.0 / f64::from(bays.max(1)));
    let (start, end) = (from - run * 0.18, to + run * 0.18);
    beam(
        mason,
        (from - run * 0.18, to + run * 0.18),
        level,
        (half_depth, 0.56),
        bays.max(1),
    )?;
    let cornice = level + 0.56;
    let (start, end) = (start - run * 0.04, end + run * 0.04);
    beam(
        mason,
        (start, end),
        cornice,
        (half_depth + 0.14, 0.22),
        bays.max(1) + 1,
    )?;
    Some(cornice + 0.22)
}

pub(super) fn colonnade(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    match dice.count(0, 2) {
        0 => avenue(stage, dice),
        1 => peristyle(stage, dice),
        _ => stoa(stage, dice),
    }
}

/// Lay a column of `order` at `foot`, its shaft `height` tall and `radius`
/// thick, its capital turned `yaw`, claiming the ground it stands on: the
/// height of its top.
fn column(
    stage: &mut Stage,
    mason: &mut Mason,
    foot: Vec3,
    (radius, height): (f64, f64),
    (order, yaw): (Order, f64),
) -> Option<f64> {
    stage.claim((foot.x, foot.z), 1.5 * radius)?;
    mason.column(&Column {
        foot,
        radius,
        height,
        order,
        yaw,
        broken: None,
    })
}

/// Two rows of columns, each carrying its beam, seen down their length.
fn avenue(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let columns = dice.count(4, 7);
    let spacing = dice.range(2.6, 3.4);
    let half_width = dice.range(2.6, 3.4);
    let shaft = dice.range(3.6, 4.6);
    let radius = dice.range(0.3, 0.38);
    let length = spacing * f64::from(columns - 1);
    let age = dice.range(KEPT.0, KEPT.1);
    let land = setting(stage, dice, (half_width + 3.0, 0.5 * length + 4.0), age)?;
    let work = building_stone(stage, dice, age, 0.3)?;
    let mut mason = Mason::new(work, dice.seed())?;
    let order = dice.pick(&Order::ALL)?;
    for row in [-1.0, 1.0] {
        let x = row * half_width;
        let mut top = 0.0;
        for index in 0..columns {
            let z = -0.5 * length + spacing * f64::from(index);
            top = column(
                stage,
                &mut mason,
                Vec3::new(x, 0.0, z),
                (radius, shaft),
                (order, FRAC_PI_2),
            )?;
        }
        let ends = (
            Vec3::new(x, 0.0, -0.5 * length),
            Vec3::new(x, 0.0, 0.5 * length),
        );
        entablature(&mut mason, ends, top, (1.2 * radius, columns - 1))?;
    }
    let centre = Vec3::new(0.0, 0.0, dice.range(-0.2, 0.3) * length);
    centrepiece(stage, dice, &mut mason, centre)?;
    stage.raise(mason, Pose::new(Vec3::ZERO, Frame::WORLD), dice.seed())?;
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
    let age = dice.range(KEPT.0, KEPT.1);
    let land = setting(stage, dice, (half_x + 3.0, half_z + 3.0), age)?;
    let work = building_stone(stage, dice, age, 0.3)?;
    let mut mason = Mason::new(work, dice.seed())?;
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
                &mut mason,
                Vec3::new(x, 0.0, z),
                (radius, shaft),
                (order, grid_yaw(j, along)),
            )?;
        }
    }
    let corners = [
        Vec3::new(-half_x, 0.0, -half_z),
        Vec3::new(half_x, 0.0, -half_z),
        Vec3::new(half_x, 0.0, half_z),
        Vec3::new(-half_x, 0.0, half_z),
    ];
    for (index, &from) in corners.iter().enumerate() {
        let to = *corners.get((index + 1) % corners.len())?;
        let bays = if index % 2 == 0 {
            across - 1
        } else {
            along - 1
        };
        entablature(&mut mason, (from, to), top, (1.2 * radius, bays))?;
    }
    centrepiece(stage, dice, &mut mason, Vec3::ZERO)?;
    stage.raise(mason, Pose::new(Vec3::ZERO, Frame::WORLD), dice.seed())?;
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

/// A row of columns before a wall of brick or stone, under a roof of stone
/// slabs: a porch seen from along it.
fn stoa(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let columns = dice.count(5, 8);
    let spacing = dice.range(2.4, 3.0);
    let length = spacing * f64::from(columns - 1);
    let depth = dice.range(3.0, 4.0);
    let shaft = dice.range(3.4, 4.2);
    let radius = dice.range(0.26, 0.32);
    let age = dice.range(KEPT.0, KEPT.1);
    let land = setting(stage, dice, (0.5 * length + 3.0, depth + 3.0), age)?;
    let work = building_stone(stage, dice, age, 0.3)?;
    let mut mason = Mason::new(work, dice.seed())?;
    let order = dice.pick(&Order::ALL)?;
    let mut top = 0.0;
    for index in 0..columns {
        let x = -0.5 * length + spacing * f64::from(index);
        top = column(
            stage,
            &mut mason,
            Vec3::new(x, 0.0, 0.0),
            (radius, shaft),
            (order, 0.0),
        )?;
    }
    let front = (
        Vec3::new(-0.5 * length, 0.0, 0.0),
        Vec3::new(0.5 * length, 0.0, 0.0),
    );
    let eaves = entablature(&mut mason, front, top, (1.2 * radius, columns - 1))?;
    // The back wall, as high as the beam the roof rests on.
    stage.claim((0.0, -depth), 0.5 * length)?;
    let wall = Wall {
        pose: Pose::new(Vec3::new(0.0, 0.0, -depth), Frame::WORLD),
        length: length + 1.2,
        height: eaves,
        thickness: 0.45,
        dressing: Dressing::Ashlar,
        rise: (0.32, 0.42),
        long: (1.6, 3.0),
        joint: 0.006,
        openings: &[],
        back: true,
    };
    if dice.chance(0.5) {
        let exposure = Weathering {
            damp: dice.range(0.2, 0.5),
            drought: dice.range(0.2, 0.6),
            foot: 0.3,
        };
        let clay = dice.pick(&Clay::ALL)?;
        let bricks = stage.brickwork(dice, clay, (age, exposure))?;
        let mut bricklayer = Mason::new(bricks, dice.seed())?;
        bricklayer.bricks(&wall, dice.pick(&Bond::ALL)?)?;
        stage.raise(bricklayer, Pose::new(Vec3::ZERO, Frame::WORLD), dice.seed())?;
    } else {
        mason.wall(&wall)?;
    }
    // Slabs spanning from the beam to the wall, side by side.
    let slabs = u32::try_from(mathf::round_i32(length / 1.1).max(2)).ok()?;
    let run = (length + 1.6) / f64::from(slabs);
    for index in 0..slabs {
        let x = -0.5 * (length + 1.6) + run * (f64::from(index) + 0.5);
        let half = Vec3::new(0.5 * run - 0.004, 0.13, 0.5 * depth + 0.6);
        let at = Vec3::new(x, eaves + 0.13, -0.5 * depth);
        mason.unit(
            (Pose::new(at, Frame::WORLD), half),
            Form::Block { fan: 0 },
            Dressing::Ashlar,
        )?;
    }
    if dice.chance(0.6) {
        let spot = Vec3::new(dice.range(-0.3, 0.3) * length, 0.0, -0.5 * depth);
        centrepiece(stage, dice, &mut mason, spot)?;
    }
    stage.raise(mason, Pose::new(Vec3::ZERO, Frame::WORLD), dice.seed())?;
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

/// Something precious on a pedestal at `centre`: a plinth, a dado and a cap
/// moulding laid by `mason`, and on it a ball or a gem.
fn centrepiece(stage: &mut Stage, dice: &mut Dice, mason: &mut Mason, centre: Vec3) -> Option<()> {
    stage.claim((centre.x, centre.z), 0.9)?;
    let height = dice.range(0.8, 1.2);
    let frame = Frame::turned(dice.range(0.0, TAU), 0.0);
    let plinth = Vec3::new(0.55, 0.09, 0.55);
    mason.unit(
        (Pose::new(centre + Vec3::UP * plinth.y, frame), plinth),
        Form::Block { fan: 0 },
        Dressing::Ashlar,
    )?;
    let dado = 0.5 * (height - 0.3);
    let at = centre + Vec3::UP * (2.0 * plinth.y + dado);
    if dice.chance(0.5) {
        let half = Vec3::new(0.42, dado, 0.42);
        mason.unit(
            (Pose::new(at, frame), half),
            Form::Drum {
                taper: 20,
                swell: 0,
                flutes: 0,
            },
            Dressing::Ashlar,
        )?;
    } else {
        mason.unit(
            (Pose::new(at, frame), Vec3::new(0.4, dado, 0.4)),
            Form::Block { fan: 0 },
            Dressing::Ashlar,
        )?;
    }
    let cap = Vec3::new(0.42, 0.06, 0.5);
    let level = centre.y + 2.0 * plinth.y + 2.0 * dado;
    mason.unit(
        (
            Pose::new(Vec3::new(centre.x, level + cap.y, centre.z), frame),
            cap,
        ),
        Form::Turned { bow: 2, bulge: 0 },
        Dressing::Ashlar,
    )?;
    let hero = stage.precious(dice)?;
    let top = Vec3::new(centre.x, level + 2.0 * cap.y, centre.z);
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

/// Arches on piers along a plaza, a cornice over them, in stone or brick.
fn loggia(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let bays = dice.count(4, 7);
    let span = dice.range(2.4, 3.6);
    let length = span * f64::from(bays);
    let pier = dice.range(0.28, 0.4);
    let rise = dice.range(2.2, 3.4);
    let rows = if dice.chance(0.4) { 2 } else { 1 };
    let gap = dice.range(3.0, 4.2);
    let age = dice.range(KEPT.0, KEPT.1);
    let land = setting(stage, dice, (0.5 * length + 3.0, gap + 3.0), age)?;
    // A brick arcade is built by a bricklayer, its cornice and its pedestal
    // still of stone.
    let brick = dice.chance(0.35);
    let mut mason = Mason::new(building_stone(stage, dice, age, 0.3)?, dice.seed())?;
    let mut bricklayer = if brick {
        let exposure = Weathering {
            damp: dice.range(0.2, 0.5),
            drought: dice.range(0.2, 0.6),
            foot: 0.3,
        };
        let clay = dice.pick(&Clay::ALL)?;
        let work = stage.brickwork(dice, clay, (age, exposure))?;
        Some((Mason::new(work, dice.seed())?, dice.pick(&Bond::ALL)?))
    } else {
        None
    };
    let mut crown = 0.0;
    for row in 0..rows {
        let z = -gap * f64::from(row);
        let (builder, bond) = match bricklayer.as_mut() {
            Some((bricklayer, bond)) => (bricklayer, Some(*bond)),
            None => (&mut mason, None),
        };
        crown = arcade_wall(
            stage,
            builder,
            (Vec3::new(0.0, 0.0, z), 0.0),
            (bays, span),
            (pier, rise),
            bond,
        )?;
        let ends = (
            Vec3::new(-0.5 * length - pier, 0.0, z),
            Vec3::new(0.5 * length + pier, 0.0, z),
        );
        beam(&mut mason, ends, crown, (pier + 0.12, 0.26), bays + 1)?;
    }
    if rows == 2 {
        let slabs = u32::try_from(mathf::round_i32(length / 1.2).max(2)).ok()?;
        let run = (length + 2.0 * pier) / f64::from(slabs);
        for index in 0..slabs {
            let x = -0.5 * (length + 2.0 * pier) + run * (f64::from(index) + 0.5);
            let half = Vec3::new(0.5 * run - 0.004, 0.12, 0.5 * gap + pier + 0.1);
            let at = Vec3::new(x, crown + 0.26 + 0.12, -0.5 * gap);
            mason.unit(
                (Pose::new(at, Frame::WORLD), half),
                Form::Block { fan: 0 },
                Dressing::Ashlar,
            )?;
        }
    }
    if dice.chance(0.5) {
        let spot = Vec3::new(dice.range(-0.3, 0.3) * length, 0.0, 3.0);
        centrepiece(stage, dice, &mut mason, spot)?;
    }
    stage.raise(mason, Pose::new(Vec3::ZERO, Frame::WORLD), dice.seed())?;
    if let Some((bricklayer, _)) = bricklayer {
        stage.raise(bricklayer, Pose::new(Vec3::ZERO, Frame::WORLD), dice.seed())?;
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

/// A wall of `bays` arches `span` apart on piers `pier` thick, standing
/// `rise` tall to where the arches spring, centred on `middle` and running
/// across `heading`, so each arch opens along it; laid in stone, or in
/// bricks of `bond`. It claims the strip it stands on; the height of its top.
fn arcade_wall(
    stage: &mut Stage,
    mason: &mut Mason,
    (middle, heading): (Vec3, f64),
    (bays, span): (u32, f64),
    (pier, rise): (f64, f64),
    bond: Option<Bond>,
) -> Option<f64> {
    let length = span * f64::from(bays) + 2.0 * pier;
    let along = direction(heading, FRAC_PI_2, 0.0);
    let (from, to) = (
        middle - along * (0.5 * length),
        middle + along * (0.5 * length),
    );
    stage.claim_along((from.x, from.z), (to.x, to.z), pier)?;
    let clear = span - 2.0 * pier;
    let ring = (0.12 * span).clamp(0.25, 0.9);
    // A brick arcade's arches spring from a course's top.
    let rise = match bond {
        Some(_) => brick_courses(rise),
        None => rise,
    };
    let mut openings = [Opening {
        at: 0.0,
        span: clear,
        rise: 0.5 * clear,
        springing: rise,
        ring,
    }; 32];
    let count = usize::try_from(bays).ok()?.min(openings.len());
    for (index, opening) in (0u32..).zip(openings.iter_mut().take(count)) {
        opening.at = -0.5 * span * f64::from(bays) + span * (f64::from(index) + 0.5);
    }
    let openings = openings.get(..count)?;
    let height = rise + 0.5 * clear + ring + 0.35;
    let frame = Frame::turned(heading, 0.0);
    let wall = Wall {
        pose: Pose::new(middle, frame),
        length,
        height,
        thickness: 2.0 * pier,
        dressing: Dressing::Ashlar,
        rise: (0.28, 0.4),
        long: (1.2, 2.6),
        joint: 0.006,
        openings,
        back: true,
    };
    match bond {
        Some(bond) => mason.bricks(&wall, bond)?,
        None => mason.wall(&wall)?,
    }
    for opening in openings {
        let voussoirs = u32::try_from(mathf::round_i32(
            core::f64::consts::PI * (0.5 * clear + 0.5 * ring) / 0.32,
        ))
        .ok()?;
        let arch = Ring {
            centre: Vec3::new(opening.at, rise, 0.0),
            wall: wall.pose,
            span: clear,
            rise: 0.5 * clear,
            depth: ring,
            through: (-pier, pier),
            count: voussoirs.max(9),
            dressing: Dressing::Ashlar,
            joint: 0.006,
        };
        match bond {
            Some(_) => mason.rowlocks(&arch)?,
            None => mason.arch(&arch)?,
        }
    }
    Some(middle.y + height)
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

/// An aqueduct striding across a river valley on its arches: the valley's
/// line and breadth, how high its hills stand, the level its channel holds
/// across it, and the span of its great arches.
#[derive(Copy, Clone, Debug)]
pub(super) struct Aqueduct {
    heading: f64,
    floor: f64,
    height: f64,
    channel: f64,
    span: f64,
}

/// The longest a cutting carries an aqueduct's channel on into a hillside
/// before the hill stands high enough over it to tunnel.
const LONGEST_CUT: f64 = 160.0;

/// How thick the slabs roofing an aqueduct's conduit are, and the least
/// depth of the base it runs on beyond its arches, below its channel.
const SLAB: f64 = 0.32;
const BASE: f64 = 0.4;

/// How far below an aqueduct's channel its cuttings' floors are dug, so its
/// base shows above them.
const CUT_FLOOR: f64 = 0.15;

/// The least depth of hill over an aqueduct's conduit a portal retains: a
/// shallower hill is no tunnel, and the conduit runs on into it buried.
const COVER: f64 = 0.8;

/// How far behind a cutting's end the hill it stops against is read: past
/// the step the land's grid softens its square end across.
const PAST_CUT: f64 = 3.0;

/// The longest a stretch of a portal's headwall runs before its top steps
/// with the hill behind it, and of the base an aqueduct's conduit runs on
/// beyond its arches before it deepens with the ground.
const HEADWALL_STEP: f64 = 2.2;
const BASE_STRETCH: f64 = 8.0;

/// How far about the valley's middle an aqueduct's finer land reaches: over
/// its whole crossing and its cuttings, which only the finer land is cut by.
const AQUEDUCT_NEST: f64 = 1100.0;

fn aqueduct(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let heading = dice.range(0.0, TAU);
    let height = dice.range(60.0, 120.0);
    let floor = dice.range(30.0, 60.0);
    let channel = dice.range(0.45, 0.62) * height;
    let span = dice.range(14.0, 22.0);
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
            flowing: 1.0,
            ledges: 0.0,
            outcrops: 0.1,
        }),
        road: None,
        farming: None,
        roughness: 1.0,
        ridges: 0.35,
        droplets: 0.05,
        cells: (256, 1024),
        nests: landscape::nests((AQUEDUCT_NEST, 80.0), (0.08, 0.12)),
        near_water: None,
        horizon: Some(landscape::horizon(reach)),
        snow_line: None,
        snowpack: None,
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
            channel,
            span,
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

    /// Which way from `vantage`'s eye, and how far as a share of their
    /// reach, its finer grids are laid: the coarser over the valley's middle,
    /// where the works stand, so the finer lies at the eye's feet.
    pub(super) fn lead(vantage: &Vantage) -> (f64, f64) {
        (
            -vantage.eye.x / AQUEDUCT_NEST,
            -vantage.eye.z / AQUEDUCT_NEST,
        )
    }

    /// The way its arches run, across the valley.
    fn line(&self) -> Vec3 {
        direction(self.heading + FRAC_PI_2, 0.0, 0.0)
    }

    /// How wide its channel's covered conduit stands, and its walls' height.
    fn conduit(&self) -> (f64, f64) {
        (0.135 * self.span, 1.6)
    }

    /// How broad the floor of a cutting its conduit runs in is dug.
    fn cutting(&self) -> f64 {
        self.conduit().0 + 1.6
    }

    /// Its channel's level beds cut into either hillside on `ground` beyond
    /// the arcade's ends, each as far as a cutting is dug before the
    /// channel tunnels on into the hill.
    pub(super) fn cuttings(&self, ground: &dyn Fn(f64, f64) -> f64) -> Option<Vec<Vec<Mark>>> {
        let (left, right, _) = self.crossing(ground);
        let line = self.line();
        let width = self.cutting();
        let mut cuttings = Vec::new();
        cuttings.try_reserve_exact(2).ok()?;
        for (end, sign) in [(left, -1.0), (right, 1.0)] {
            let length = self.cut(ground, (end, sign));
            let mut marks = Vec::new();
            let steps = mathf::ceil((length + 4.0) / 3.0).max(2.0);
            marks
                .try_reserve_exact(usize::try_from(mathf::round_i32(steps)).ok()? + 1)
                .ok()?;
            for step in 0..=u32::try_from(mathf::round_i32(steps)).ok()? {
                // From a little within the arcade's end, so the cutting meets
                // its abutment, on to where the channel tunnels.
                let s = sign * (end - 4.0 + (length + 4.0) * f64::from(step) / steps);
                let at = line * s;
                marks.push(Mark {
                    x: at.x,
                    z: at.z,
                    level: self.channel - CUT_FLOOR,
                    width,
                    ..Mark::default()
                });
            }
            cuttings.push(marks);
        }
        Some(cuttings)
    }

    /// How far beyond its arcade's `end` on the `sign` side the channel runs
    /// in a cutting on `ground`: until the hill stands as high over it as a
    /// cutting is dug.
    fn cut(&self, ground: &dyn Fn(f64, f64) -> f64, (end, sign): (f64, f64)) -> f64 {
        let line = self.line();
        let mut length = 0.0;
        while length < LONGEST_CUT {
            let at = line * (sign * (end + length));
            if ground(at.x, at.z) - self.channel > DEEPEST_CUT {
                break;
            }
            length += 0.5;
        }
        length
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
        let groundwork = {
            let fields = &stage.fields;
            let far = fields.get(usize::try_from(land.grids.far).ok()?)?;
            let built = |x: f64, z: f64| land.grids.height(fields, x, z);
            let (line, reach) = (self.line(), self.reach());
            let sited = Section::along(line, reach, &|x, z| far.height_at(x, z))?;
            let (left, right, _) = self.crossing(&|x, z| sited.at(x * line.x + z * line.z));
            let into = [
                self.entering(&sited, &built, (left, -1.0))?,
                self.entering(&sited, &built, (right, 1.0))?,
            ];
            Groundwork {
                built: Section::along(line, reach, &built)?,
                sited,
                ends: (left, right),
                into,
            }
        };
        let top = self.works(stage, dice, &groundwork)?;
        let eye = vantage.eye;
        let target = Vec3::new(0.0, 0.45 * top, 0.0);
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

    /// How far either side of the valley's middle its works may run: as far
    /// as its sides may lie, and a cutting beyond.
    fn reach(&self) -> f64 {
        self.widest() + LONGEST_CUT + 8.0
    }

    /// The farthest either side of the valley's middle its sides may rise to
    /// the channel.
    fn widest(&self) -> f64 {
        6.0 * self.floor + self.height / 0.6
    }

    /// Where the valley's sides rise to the channel either side of its
    /// middle, along its line, on the land `ground` gives; and the lowest the
    /// land lies between them.
    fn crossing(&self, ground: &dyn Fn(f64, f64) -> f64) -> (f64, f64, f64) {
        let line = self.line();
        let mut lowest = f64::INFINITY;
        let mut reach = |sign: f64| {
            let mut distance = 0.0;
            while distance < self.widest() {
                let at = line * (sign * distance);
                let level = ground(at.x, at.z);
                // Every sample counts, so a middle already above the channel
                // still founds the piers on finite ground.
                lowest = lowest.min(level);
                if level > self.channel {
                    return distance;
                }
                distance += 2.0;
            }
            distance
        };
        let (left, right) = (reach(-1.0), reach(1.0));
        (left, right, lowest)
    }

    /// How the channel goes on into the hill beyond the arcade's `end` on
    /// its `sign` side: in a cutting as far as the land `sited` was dug, and
    /// on the land `built` through a portal at the cutting's end where the
    /// hill there covers the conduit, or else run on until the hill does.
    fn entering(
        &self,
        sited: &Section,
        built: &dyn Fn(f64, f64) -> f64,
        (end, sign): (f64, f64),
    ) -> Option<Entering> {
        let line = self.line();
        let cut = self.cut(&|x, z| sited.at(x * line.x + z * line.z), (end, sign));
        let roof = self.channel + self.conduit().1 + SLAB;
        let along = |s: f64| {
            let at = line * (sign * s);
            built(at.x, at.z)
        };
        if cut < LONGEST_CUT && along(end + cut + PAST_CUT) >= roof + COVER {
            let steps = self.headwall(built, (end + cut, sign))?;
            return Some(Entering {
                cut,
                run: cut + 2.0,
                portal: Some(steps),
            });
        }
        let mut run = cut;
        while run < LONGEST_CUT && along(end + run) < roof + COVER {
            run += 0.5;
        }
        Some(Entering {
            cut,
            run: run + 2.0,
            portal: None,
        })
    }

    /// A portal's frame on the valley's `sign` side: its face toward the
    /// valley, its length across the cutting.
    fn facing(&self, sign: f64) -> Frame {
        Frame::turned(self.heading - sign * FRAC_PI_2, 0.0)
    }

    /// How the conduit's arch through a portal spans, and its ring's depth.
    fn arch(&self) -> (f64, f64) {
        (self.conduit().0 + 0.5, 0.45)
    }

    /// The stretches of the headwall across the cutting's end `at` from the
    /// valley's middle on its `sign` side, each standing from the cutting's
    /// floor or sides beneath it and its top stepping with the land `built`
    /// behind it: one over the conduit's arch, and stretches either side out
    /// as far as the cutting's end has a face to retain.
    fn headwall(
        &self,
        built: &dyn Fn(f64, f64) -> f64,
        (at, sign): (f64, f64),
    ) -> Option<Vec<Step>> {
        let across = self.facing(sign).x;
        let behind = self.line() * (sign * (at + PAST_CUT));
        let hill = |(from, to): (f64, f64)| {
            let count = mathf::ceil((to - from) / 0.5).max(1.0);
            (0..=u32::try_from(mathf::round_i32(count)).unwrap_or(1))
                .map(|index| {
                    let place = behind + across * (from + (to - from) * f64::from(index) / count);
                    built(place.x, place.z)
                })
                .fold(f64::NEG_INFINITY, f64::max)
        };
        let half = 0.5 * self.cutting();
        let side = |offset: f64| self.channel - CUT_FLOOR + (offset - half).max(0.0) * CUT_SLOPE;
        let (span, ring) = self.arch();
        let crown = self.channel + self.conduit().1 + SLAB + 0.25 * span + ring + 0.4;
        let middle = 0.5 * span + 1.2;
        let farthest = half + (DEEPEST_CUT + 3.0) / CUT_SLOPE;
        // Each side's stretches as alike in length as fit, at most a step.
        let stretches = mathf::ceil((farthest - middle) / HEADWALL_STEP).max(1.0);
        let each = (farthest - middle) / stretches;
        let count = u32::try_from(mathf::round_i32(stretches)).ok()?;
        let mut steps = Vec::new();
        steps
            .try_reserve_exact(1 + 2 * usize::try_from(count).ok()?)
            .ok()?;
        steps.push(Step {
            from: -middle,
            to: middle,
            foot: self.channel - BASE,
            top: (hill((-middle, middle)) + 0.5).max(crown),
        });
        for way in [-1.0, 1.0] {
            for index in 0..count {
                let near = middle + each * f64::from(index);
                let far = near + each;
                let (from, to) = if way < 0.0 {
                    (-far, -near)
                } else {
                    (near, far)
                };
                let (low, top) = (side(near), hill((from, to)) + 0.5);
                // Beyond where the hill stands over the cutting's side, its
                // end has no face left to hold back.
                if top < low + 0.8 {
                    break;
                }
                steps.push(Step {
                    from,
                    to,
                    foot: low - 0.4,
                    top,
                });
            }
        }
        Some(steps)
    }

    /// Build the aqueduct on its `footing`: its arcade across the valley,
    /// its channel's conduit along its top and on into either hill, in a
    /// cutting and through a portal or buried; the height of its top.
    fn works(&self, stage: &mut Stage, dice: &mut Dice, groundwork: &Groundwork) -> Option<f64> {
        let line = self.line();
        let (left, right) = groundwork.ends;
        let age = dice.range(0.6, 0.95);
        let weathering = Weathering {
            damp: dice.range(0.3, 0.6),
            drought: dice.range(0.2, 0.6),
            foot: 0.5,
        };
        let quarry = dice.pick(&[Quarry::Limestone, Quarry::Sandstone, Quarry::Granite])?;
        let work = stage.stonework(dice, quarry, (age, weathering))?;
        let mut mason = Mason::new(work, dice.seed())?;
        let founded = |s: f64| groundwork.founded(s);
        Arcade::new(self, (left, right)).lay(stage, &mut mason, &founded)?;
        let [into_left, into_right] = &groundwork.into;
        self.channel(
            &mut mason,
            (left + into_left.run, right + into_right.run),
            &founded,
        )?;
        for (end, sign, into) in [(left, -1.0, into_left), (right, 1.0, into_right)] {
            if let Some(steps) = &into.portal {
                self.portal(&mut mason, (end + into.cut, sign), steps)?;
            }
            // Nothing takes root on the cutting's floor, the portal's face, or
            // over the conduit where it runs on buried.
            let (from, to) = (line * (sign * end), line * (sign * (end + into.cut + 1.0)));
            stage.claim_along((from.x, from.z), (to.x, to.z), 0.5 * self.cutting() + 1.0)?;
            if into.portal.is_none() {
                let beyond = line * (sign * (end + into.run));
                stage.claim_along(
                    (to.x, to.z),
                    (beyond.x, beyond.z),
                    0.5 * self.conduit().0 + 1.0,
                )?;
            }
        }
        stage.raise(mason, Pose::new(Vec3::ZERO, Frame::WORLD), dice.seed())?;
        Some(self.channel + self.conduit().1 + SLAB)
    }

    /// The channel's covered conduit from `left` of the valley's middle to
    /// `right` of it at its level: two walls and the slabs that roof them,
    /// on the arcade's top across the valley and on a base laid in the
    /// cutting beyond, a little way past each portal into the hill.
    fn channel(
        &self,
        mason: &mut Mason,
        (left, right): (f64, f64),
        profile: &dyn Fn(f64) -> f64,
    ) -> Option<()> {
        let (wide, walls) = self.conduit();
        let line = self.line();
        let frame = Frame::turned(self.heading, 0.0);
        let (from, to) = (-left - 2.0, right + 2.0);
        let middle = line * f64::midpoint(from, to);
        let length = to - from;
        let thick = 0.3 * wide;
        for side in [-1.0, 1.0] {
            let at =
                middle + frame.z * (side * (0.5 * wide - 0.5 * thick)) + Vec3::UP * self.channel;
            mason.wall(&Wall {
                pose: Pose::new(at, frame),
                length,
                height: walls,
                thickness: thick,
                dressing: Dressing::Squared,
                rise: (0.35, 0.5),
                long: (1.2, 2.4),
                joint: 0.012,
                openings: &[],
                back: true,
            })?;
        }
        let slabs = u32::try_from(mathf::round_i32(length / 1.4).max(2)).ok()?;
        let run = length / f64::from(slabs);
        for index in 0..slabs {
            let along = from + run * (f64::from(index) + 0.5);
            let at = line * along + Vec3::UP * (self.channel + walls + 0.5 * SLAB);
            let half = Vec3::new(0.5 * run - 0.01, 0.5 * SLAB, 0.5 * wide + 0.12);
            mason.unit(
                (Pose::new(at, frame), half),
                Form::Block { fan: 0 },
                Dressing::Squared,
            )?;
        }
        // Beyond the arcade, where the channel runs on the ground, its base,
        // laid in stretches each as deep as the ground beneath it falls.
        for (start, end) in [(from, -left), (right, to)] {
            let stretches = mathf::ceil((end - start) / BASE_STRETCH).max(1.0);
            let each = (end - start) / stretches;
            for index in 0..u32::try_from(mathf::round_i32(stretches)).ok()? {
                let a = start + each * f64::from(index);
                let lowest = (0..=4)
                    .map(|step| profile(a + each * f64::from(step) / 4.0))
                    .fold(f64::INFINITY, f64::min);
                let bed = (self.channel - lowest).max(0.0) + BASE;
                let at = line * (a + 0.5 * each) + Vec3::UP * (self.channel - bed);
                mason.wall(&Wall {
                    pose: Pose::new(at, frame),
                    length: each,
                    height: bed,
                    thickness: wide + 0.3,
                    dressing: Dressing::Squared,
                    rise: (0.35, 0.5),
                    long: (1.2, 2.4),
                    joint: 0.012,
                    openings: &[],
                    back: true,
                })?;
            }
        }
        Some(())
    }

    /// The portal the channel tunnels into the hill through, `at` from the
    /// valley's middle on its `sign` side: a headwall across the cutting's
    /// end facing the valley in `steps`, retaining the hill above, the
    /// conduit passing in under an arch whose tympanum is walled up over it.
    fn portal(&self, mason: &mut Mason, (at, sign): (f64, f64), steps: &[Step]) -> Option<()> {
        let face = self.facing(sign);
        let end = self.line() * (sign * at);
        let (span, ring) = self.arch();
        let thickness = 0.9;
        for step in steps {
            let foot = Pose::new(
                end + face.x * f64::midpoint(step.from, step.to) + Vec3::UP * step.foot,
                face,
            );
            let over = step.from < 0.0 && step.to > 0.0;
            let roof = self.channel + self.conduit().1 + SLAB - step.foot;
            let opening = Opening {
                at: 0.0,
                span,
                rise: 0.5 * span,
                springing: roof - 0.25 * span,
                ring,
            };
            let openings = [opening];
            let wall = Wall {
                pose: foot,
                length: step.to - step.from,
                height: step.top - step.foot,
                thickness,
                dressing: Dressing::Squared,
                rise: (0.3, 0.45),
                long: (1.2, 2.4),
                joint: 0.012,
                openings: if over { &openings } else { &[] },
                back: false,
            };
            mason.wall(&wall)?;
            if over {
                mason.arch(&Ring {
                    centre: Vec3::new(0.0, opening.springing, 0.0),
                    wall: foot,
                    span,
                    rise: opening.rise,
                    depth: ring,
                    through: (-0.5 * thickness, 0.5 * thickness),
                    count: 13,
                    dressing: Dressing::Ashlar,
                    joint: 0.01,
                })?;
                // The tympanum, walled up over the conduit's roof.
                mason.infill(&wall, &opening, (roof, 0.12))?;
            }
        }
        Some(())
    }
}

/// What an aqueduct is laid out over and founded on: the land along its
/// line as it was sited, before its cuttings were dug, and as it was built,
/// which its masonry stands on wherever that lies lower; how far either side
/// of the valley's middle its arcade runs; and how its channel goes on into
/// either hill.
struct Groundwork {
    sited: Section,
    built: Section,
    ends: (f64, f64),
    into: [Entering; 2],
}

impl Groundwork {
    /// The ground `s` along the line masonry there is founded on.
    fn founded(&self, s: f64) -> f64 {
        self.sited.at(s).min(self.built.at(s))
    }
}

/// How an aqueduct's channel goes on into a hill beyond its arcade's end:
/// how far its cutting runs, and its conduit; and the stretches of the
/// headwall of the portal it tunnels through, where the hill stands high
/// enough over the cutting's end to retain.
#[derive(Debug)]
struct Entering {
    cut: f64,
    run: f64,
    portal: Option<Vec<Step>>,
}

/// A stretch of a portal's headwall: from and to across the cutting, in the
/// headwall's own frame, the height it stands from and its top's.
#[derive(Copy, Clone, Debug)]
struct Step {
    from: f64,
    to: f64,
    foot: f64,
    top: f64,
}

/// The land's height along a line through the valley's middle, sampled a
/// metre apart: what an aqueduct is laid out over.
struct Section {
    from: f64,
    heights: Vec<f64>,
}

impl Section {
    /// The section along `line`, `reach` either side of the middle, of the
    /// land `ground` gives; `None` when the heap will not hold it.
    fn along(line: Vec3, reach: f64, ground: &dyn Fn(f64, f64) -> f64) -> Option<Self> {
        let count = usize::try_from(mathf::round_i32(mathf::ceil(2.0 * reach))).ok()? + 1;
        let heights = tairix_util::fallible::collected(
            count,
            (0..count).map(|index| {
                let s = -reach + crate::vector::real(index);
                let at = line * s;
                ground(at.x, at.z)
            }),
        )?;
        Some(Self {
            from: -reach,
            heights,
        })
    }

    /// The height `s` along the line from the middle, between its samples.
    fn at(&self, s: f64) -> f64 {
        let (index, within) = crate::vector::cell_of(s - self.from);
        let here = self
            .heights
            .get(index)
            .or(self.heights.last())
            .copied()
            .unwrap_or(0.0);
        let next = self.heights.get(index + 1).copied().unwrap_or(here);
        here + (next - here) * within
    }
}

/// An aqueduct's arcade across its valley: an upper tier of arches from
/// side to side carrying the channel, and beneath it, where the valley lies
/// deep enough, a tier of great arches each spanning two of the upper; every
/// pier founded on the ground where it stands, and a bay too shallow to
/// arch walled solid.
struct Arcade {
    line: Vec3,
    /// How far either side of the valley's middle it runs.
    ends: (f64, f64),
    /// The channel's level, the span of its great arches and their piers'
    /// half breadth.
    channel: f64,
    span: f64,
    pier: f64,
}

impl Arcade {
    fn new(aqueduct: &Aqueduct, ends: (f64, f64)) -> Self {
        Self {
            line: aqueduct.line(),
            ends,
            channel: aqueduct.channel,
            span: aqueduct.span,
            pier: 0.09 * aqueduct.span,
        }
    }

    /// The upper tier's span, its piers' half breadth, how high they stand
    /// to their springing, and its arches' ring.
    fn upper(&self) -> (f64, f64, f64, f64) {
        let span = 0.5 * self.span;
        (
            span,
            0.6 * self.pier,
            (0.3 * self.span).max(4.0),
            (0.08 * span).clamp(0.45, 1.0),
        )
    }

    /// The great arches' ring.
    fn ring(&self) -> f64 {
        (0.08 * self.span).clamp(0.5, 1.6)
    }

    /// How tall a tier's band of spandrels over its springing stands: its
    /// arches' half clear span, their ring and the course over them.
    fn band((span, pier, ring): (f64, f64, f64)) -> f64 {
        0.5 * (span - 2.0 * pier) + ring + 0.6
    }

    /// Lay it over the land `profile` gives along its line.
    fn lay(
        &self,
        stage: &mut Stage,
        mason: &mut Mason,
        profile: &dyn Fn(f64) -> f64,
    ) -> Option<()> {
        let (span_u, pier_u, rise_u, ring_u) = self.upper();
        let springing_u = self.channel - Self::band((span_u, pier_u, ring_u));
        let base_u = springing_u - rise_u;
        let (from, to) = self.great(mason, profile, base_u)?;
        let resting = |s: f64| s >= from && s <= to;
        let arched = self.upper_tier(mason, (&resting, profile), (springing_u, base_u))?;
        self.walled(mason, (&resting, profile), &arched, springing_u)?;
        let (left, right) = self.ends;
        let (from, to) = (self.line * -left, self.line * right);
        stage.claim_along((from.x, from.z), (to.x, to.z), self.pier)
    }

    /// The great arches, each spanning two of the upper tier's bays, where
    /// the valley lies deep below them, on the land `profile` gives, laid up
    /// to the upper tier's `base`: how far along the line they reach, from
    /// and to, or from past to where none stands.
    fn great(
        &self,
        mason: &mut Mason,
        profile: &dyn Fn(f64) -> f64,
        base: f64,
    ) -> Option<(f64, f64)> {
        let ring = self.ring();
        let springing = base - Self::band((self.span, self.pier, ring));
        let (left, right) = self.ends;
        let (first, last) = (
            mathf::round_i32(mathf::ceil(-left / self.span)),
            mathf::round_i32(mathf::floor(right / self.span)),
        );
        let mut great = Vec::new();
        great
            .try_reserve(usize::try_from(last - first + 1).unwrap_or(0))
            .ok()?;
        for k in first..=last {
            let s = f64::from(k) * self.span;
            if profile(s) < springing - 2.0 {
                great.push(s);
            }
        }
        let mut lower = Vec::new();
        lower.try_reserve(great.len()).ok()?;
        for pair in great.windows(2) {
            if let [a, b] = *pair {
                if (b - a - self.span).abs() < 1e-6 {
                    lower.push((a, b));
                }
            }
        }
        let Some((from, to)) = lower
            .first()
            .zip(lower.last())
            .map(|(first, last)| (first.0 - self.pier, last.1 + self.pier))
        else {
            return Some((f64::INFINITY, f64::NEG_INFINITY));
        };
        self.tier(
            mason,
            (from, to),
            (springing, base),
            (self.span, self.pier, ring),
            &lower,
        )?;
        // Each great pier once, whether it ends a run of arches or stands
        // between two.
        let ends = |s: f64| {
            lower
                .iter()
                .any(|&(a, b)| (a - s).abs() < 1e-6 || (b - s).abs() < 1e-6)
        };
        for &s in great.iter().filter(|&&s| ends(s)) {
            self.pier(mason, s, (profile(s) - 1.0, springing), self.pier)?;
        }
        Some((from, to))
    }

    /// The upper tier from side to side, standing on the great arches where
    /// `resting` has it and else on the land `profile` gives: a bay arched
    /// where both its piers stand clear of the ground below its `springing`,
    /// the tier's piers founded from its `base` or the ground; the bays
    /// arched.
    fn upper_tier(
        &self,
        mason: &mut Mason,
        (resting, profile): (&dyn Fn(f64) -> bool, &dyn Fn(f64) -> f64),
        (springing, base): (f64, f64),
    ) -> Option<Vec<(f64, f64)>> {
        let (span, pier, _, ring) = self.upper();
        let (left, right) = self.ends;
        let (first, last) = (
            mathf::round_i32(mathf::ceil(-left / span)),
            mathf::round_i32(mathf::floor(right / span)),
        );
        let foot = |s: f64| if resting(s) { base } else { profile(s) - 1.0 };
        let mut arched = Vec::new();
        arched
            .try_reserve(usize::try_from(last - first).unwrap_or(0))
            .ok()?;
        for j in first..last {
            let a = f64::from(j) * span;
            let b = a + span;
            if foot(a) < springing - 1.5 && foot(b) < springing - 1.5 {
                arched.push((a, b));
            }
        }
        self.tier(
            mason,
            (-left, right),
            (springing, self.channel),
            (span, pier, ring),
            &arched,
        )?;
        for j in first..=last {
            let s = f64::from(j) * span;
            if foot(s) < springing {
                self.pier(mason, s, (foot(s), springing), pier)?;
            }
        }
        Some(arched)
    }

    /// Wall up from the land `profile` gives to the upper tier's `springing`
    /// every bay of it not `arched` and not `resting` on the great arches,
    /// and the stretches from its last piers to where the hills rise to the
    /// channel.
    fn walled(
        &self,
        mason: &mut Mason,
        (resting, profile): (&dyn Fn(f64) -> bool, &dyn Fn(f64) -> f64),
        arched: &[(f64, f64)],
        springing: f64,
    ) -> Option<()> {
        let (span, pier, ..) = self.upper();
        let (left, right) = self.ends;
        let (first, last) = (
            mathf::round_i32(mathf::ceil(-left / span)),
            mathf::round_i32(mathf::floor(right / span)),
        );
        let mut solid = Vec::new();
        solid
            .try_reserve(usize::try_from(last - first).unwrap_or(0) + 2)
            .ok()?;
        for j in first..last {
            let a = f64::from(j) * span;
            if !arched.iter().any(|&(from, _)| (from - a).abs() < 1e-6) {
                solid.push((a, a + span));
            }
        }
        solid.push((-left, f64::from(first) * span));
        solid.push((f64::from(last) * span, right));
        for (a, b) in solid {
            if b - a < 0.05 || resting(f64::midpoint(a, b)) {
                continue;
            }
            let lowest = (0..=6)
                .map(|step| profile(a + (b - a) * f64::from(step) / 6.0))
                .fold(f64::INFINITY, f64::min);
            if lowest < springing {
                let middle = self.line * f64::midpoint(a, b) + Vec3::UP * (lowest - 1.0);
                mason.wall(&Wall {
                    pose: Pose::new(middle, self.frame_along()),
                    length: b - a,
                    height: springing - lowest + 1.0,
                    thickness: 2.0 * pier,
                    dressing: Dressing::Squared,
                    rise: (0.55, 0.9),
                    long: (1.4, 2.8),
                    joint: 0.008,
                    openings: &[],
                    back: true,
                })?;
            }
        }
        Some(())
    }

    /// A wall's frame along the line, its face across the valley.
    fn frame_along(&self) -> Frame {
        Frame {
            x: self.line,
            y: Vec3::UP,
            z: self.line.cross(Vec3::UP),
        }
    }

    /// A tier's band of spandrels from `from` to `to` along the line, from
    /// its `springing` to its `top`, its arches of `span` on piers `pier`
    /// broad with rings `ring` deep opening over each of `bays`.
    fn tier(
        &self,
        mason: &mut Mason,
        (from, to): (f64, f64),
        (springing, top): (f64, f64),
        (span, pier, ring): (f64, f64, f64),
        bays: &[(f64, f64)],
    ) -> Option<()> {
        let middle = f64::midpoint(from, to);
        let clear = span - 2.0 * pier;
        let mut openings = Vec::new();
        openings.try_reserve_exact(bays.len()).ok()?;
        for &(a, b) in bays {
            openings.push(Opening {
                at: f64::midpoint(a, b) - middle,
                span: clear,
                rise: 0.5 * clear,
                springing: 0.0,
                ring,
            });
        }
        let foot = Pose::new(
            self.line * middle + Vec3::UP * springing,
            self.frame_along(),
        );
        mason.wall(&Wall {
            pose: foot,
            length: to - from,
            height: top - springing,
            thickness: 2.0 * pier.max(0.6 * self.pier),
            dressing: Dressing::Squared,
            rise: (0.55, 0.9),
            long: (1.4, 2.8),
            joint: 0.008,
            openings: &openings,
            back: true,
        })?;
        for opening in &openings {
            let voussoirs = u32::try_from(mathf::round_i32(
                core::f64::consts::PI * (0.5 * clear + 0.5 * ring) / 0.7,
            ))
            .ok()?;
            mason.arch(&Ring {
                centre: Vec3::new(opening.at, 0.0, 0.0),
                wall: foot,
                span: clear,
                rise: 0.5 * clear,
                depth: ring,
                through: (-pier.max(0.6 * self.pier), pier.max(0.6 * self.pier)),
                count: voussoirs.max(11),
                dressing: Dressing::Squared,
                joint: 0.008,
            })?;
        }
        Some(())
    }

    /// A pier `half` its breadth either side of `at` along the line, laid in
    /// courses from `foot` to `top`.
    fn pier(&self, mason: &mut Mason, at: f64, (foot, top): (f64, f64), half: f64) -> Option<()> {
        if top - foot < 0.1 {
            return Some(());
        }
        let pose = Pose::new(self.line * at + Vec3::UP * foot, self.frame_along());
        mason.wall(&Wall {
            pose,
            length: 2.0 * half,
            height: top - foot,
            thickness: 2.0 * half.max(0.6 * self.pier),
            dressing: Dressing::Squared,
            rise: (0.55, 0.9),
            long: (1.4, 2.8),
            joint: 0.008,
            openings: &[],
            back: true,
        })
    }
}

pub(super) fn rotunda(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let radius = dice.range(3.0, 4.6);
    let columns = dice.count(8, 14);
    let shaft = dice.range(3.4, 4.6);
    let thick = dice.range(0.24, 0.32);
    let steps = dice.count(2, 4);
    let tread = 0.45;
    let base = radius + 1.0 + tread * f64::from(steps);
    let age = dice.range(KEPT.0, KEPT.1);
    let land = setting(stage, dice, (base + 2.0, base + 2.0), age)?;
    let work = building_stone(stage, dice, age, 0.3)?;
    let mut mason = Mason::new(work, dice.seed())?;
    let rise = 0.2;
    for step in 0..steps {
        let outer = base - tread * f64::from(step);
        let level = rise * f64::from(step);
        let stones = u32::try_from(mathf::round_i32(TAU * outer / 1.1)).ok()?;
        mason.annulus(&Annulus {
            centre: Vec3::UP * level,
            inner: outer - tread,
            outer,
            height: rise,
            stones,
            turn: dice.range(0.0, TAU),
            dressing: Dressing::Ashlar,
            joint: 0.006,
        })?;
    }
    let floor = rise * f64::from(steps);
    // The floor within the top step: rings of flags about a middle stone.
    let mut outer = base - tread * f64::from(steps);
    while outer > 0.6 {
        let inner = (outer - dice.range(0.55, 0.8)).max(0.35);
        let stones = u32::try_from(mathf::round_i32(TAU * outer / 0.9)).ok()?;
        mason.annulus(&Annulus {
            centre: Vec3::UP * (floor - 0.12),
            inner,
            outer,
            height: 0.12,
            stones,
            turn: dice.range(0.0, TAU),
            dressing: Dressing::Flag,
            joint: 0.006,
        })?;
        outer = inner;
    }
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
        top = column(stage, &mut mason, at, (thick, shaft), (order, angle))?;
    }
    // The architrave and cornice round the columns' heads.
    for (reach, height, stones) in [
        (1.2 * thick, 0.56, 2 * columns),
        (1.2 * thick + 0.14, 0.24, 3 * columns),
    ] {
        mason.annulus(&Annulus {
            centre: Vec3::UP * top,
            inner: radius - reach,
            outer: radius + reach,
            height,
            stones,
            turn,
            dressing: Dressing::Ashlar,
            joint: 0.005,
        })?;
        top += height;
    }
    let roof = match dice.count(0, 3) {
        0 => stage.material(
            Material::new(
                Pigment::Solid(rgb(0x5E_9E_8A)),
                Finish::Coated { roughness: 0.55 },
            )
            .with_relief(Relief::grain(0.063, 3.0, dice.seed())),
        )?,
        1 => stage.metal(GOLD, 0.25)?,
        2 => stage.metal(COPPER, 0.3)?,
        _ => usize::from(work.stone),
    };
    stage.dome(Vec3::UP * top, radius + 0.35, roof)?;
    let crest = top + radius + 0.35;
    stage.post(Vec3::UP * (crest - 0.05), (0.18, 0.12, 0.6), roof, dice)?;
    stage.ball(Vec3::UP * (crest + 0.5), 0.2, roof, dice)?;
    centrepiece(stage, dice, &mut mason, Vec3::UP * floor)?;
    stage.raise(mason, Pose::new(Vec3::ZERO, Frame::WORLD), dice.seed())?;
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
    let age = dice.range(RUINED.0, RUINED.1);
    let quarry = dice.pick(&Quarry::ALL)?;
    let exposure = Weathering {
        damp: dice.range(0.45, 0.85),
        drought: dice.range(0.2, 0.5),
        foot: 0.3,
    };
    let work = stage.stonework(dice, quarry, (age, exposure))?;
    let mut mason = Mason::new(work, dice.seed())?;
    let shaft = dice.range(3.6, 4.6);
    temple_ruin(stage, dice, &mut mason, ((along, across), spacing), shaft)?;
    tumbled(stage, dice, &mut mason, (&land, half_x.max(half_z) + 5.0))?;
    stage.raise(mason, Pose::new(Vec3::ZERO, Frame::WORLD), dice.seed())?;
    box_bushes(stage, dice, (&land, half_x.max(half_z) + 6.0))?;
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

/// Blocks tumbled from a ruin and strewn over `land` within `reach` of its
/// middle, each lying where it came to rest.
fn tumbled(
    stage: &mut Stage,
    dice: &mut Dice,
    mason: &mut Mason,
    (land, reach): (&Backdrop, f64),
) -> Option<()> {
    for _ in 0..dice.count(4, 10) {
        let size = dice.range(0.25, 0.7);
        let Some((x, z)) = stage.place(dice, ((0.0, 0.0), reach), size) else {
            continue;
        };
        let half = Vec3::new(
            size,
            dice.range(0.4, 0.7) * size,
            dice.range(0.6, 1.2) * size,
        );
        let tilt = Frame::turned(dice.range(0.0, TAU), dice.range(-0.35, 0.35));
        let at = Vec3::new(x, land.terrain.height(x, z) + 0.6 * half.y, z);
        mason.unit(
            (Pose::new(at, tilt), half),
            Form::Block { fan: 0 },
            Dressing::Squared,
        )?;
    }
    Some(())
}

/// Box bushes grown up about a ruin on `land` within `reach` of its middle.
fn box_bushes(stage: &mut Stage, dice: &mut Dice, (land, reach): (&Backdrop, f64)) -> Option<()> {
    let hedges = Grove::new(stage, dice, (&[Kind::Box], Season::Summer), Stand::Open)?;
    let bush = hedges.of(Kind::Box)?;
    for _ in 0..dice.count(2, 5) {
        let Some((x, z)) = stage.place(dice, ((0.0, 0.0), reach), 0.8) else {
            continue;
        };
        let height = dice.range(0.8, 1.6);
        plants::plant(
            stage,
            dice,
            &bush.habit,
            Vec3::new(x, land.terrain.height(x, z), z),
            height,
        )?;
    }
    Some(())
}

/// What is left of a temple of `along` by `across` columns `spacing` apart,
/// their shafts `shaft` tall: the blocks of its platform that stayed, some
/// heaved or sunk; some columns standing, some broken with their drums
/// fallen beside them, some fallen whole, their drums lying apart along the
/// way they fell; and here and there a lintel still across two that stand.
fn temple_ruin(
    stage: &mut Stage,
    dice: &mut Dice,
    mason: &mut Mason,
    ((along, across), spacing): ((u32, u32), f64),
    shaft: f64,
) -> Option<()> {
    let (half_x, half_z) = (
        0.5 * spacing * f64::from(across - 1),
        0.5 * spacing * f64::from(along - 1),
    );
    let order = dice.pick(&Order::ALL)?;
    let radius = dice.range(0.3, 0.38);
    // The stylobate's blocks, most long gone.
    let (rows, run) = (2 * along + 2, 2 * across + 2);
    for i in 0..run {
        for j in 0..rows {
            if !dice.chance(0.35) {
                continue;
            }
            let x = -half_x - 0.7 + (2.0 * half_x + 1.4) * (f64::from(i) + 0.5) / f64::from(run);
            let z = -half_z - 0.7 + (2.0 * half_z + 1.4) * (f64::from(j) + 0.5) / f64::from(rows);
            let half = Vec3::new(
                f64::midpoint(2.0 * half_x, 1.4) / f64::from(run) - 0.01,
                dice.range(0.16, 0.22),
                f64::midpoint(2.0 * half_z, 1.4) / f64::from(rows) - 0.01,
            );
            let heaved = Frame::turned(dice.range(-0.04, 0.04), dice.range(-0.06, 0.06));
            let at = Vec3::new(x, -half.y + dice.range(-0.12, 0.05), z);
            mason.unit(
                (Pose::new(at, heaved), half),
                Form::Block { fan: 0 },
                Dressing::Ashlar,
            )?;
        }
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
                    let top = column(
                        stage,
                        mason,
                        at,
                        (radius, shaft),
                        (order, grid_yaw(j, along)),
                    )?;
                    if let Some(slot) = standing.iter_mut().find(|slot| slot.is_none()) {
                        *slot = Some((at, top));
                    }
                }
                3..=5 => {
                    stage.claim((at.x, at.z), 1.4 * radius)?;
                    let share = dice.range(0.15, 0.7);
                    mason.column(&Column {
                        foot: at,
                        radius,
                        height: shaft,
                        order,
                        yaw: grid_yaw(j, along),
                        broken: Some(share),
                    })?;
                    let fell = dice.range(0.0, TAU);
                    let lying = (shaft * (1.0 - share)).min(dice.range(0.6, 1.6));
                    let from = at + direction(fell, 0.0, 0.0) * dice.range(1.2, 2.2);
                    fallen(stage, dice, mason, (from, fell), (0.85 * radius, lying))?;
                }
                6 | 7 => {
                    let fell = dice.range(0.0, TAU);
                    let lying = dice.range(1.5, 3.2);
                    fallen(stage, dice, mason, (at, fell), (radius, lying))?;
                }
                _ => {}
            }
        }
    }
    // A lintel still spanning two neighbours that stand.
    if let [Some((a, top_a)), Some((b, _)), ..] = standing {
        if (a - b).length() < 1.2 * spacing {
            let half = Vec3::new(0.5 * (a - b).length() + radius, 0.3, 1.2 * radius);
            let heading = mathf::atan2(b.x - a.x, b.z - a.z);
            let at = Vec3::new(
                f64::midpoint(a.x, b.x),
                top_a + 0.3,
                f64::midpoint(a.z, b.z),
            );
            mason.unit(
                (Pose::new(at, Frame::turned(heading - FRAC_PI_2, 0.0)), half),
                Form::Block { fan: 0 },
                Dressing::Ashlar,
            )?;
        }
    }
    Some(())
}

/// A column's drums lying where they fell from `from` toward `heading`,
/// `radius` thick and `length` of shaft in all: drums parted along their
/// joints as the column struck the ground, each settled into it a little
/// askew.
fn fallen(
    stage: &mut Stage,
    dice: &mut Dice,
    mason: &mut Mason,
    (from, heading): (Vec3, f64),
    (radius, length): (f64, f64),
) -> Option<()> {
    let middle = from + direction(heading, 0.0, 0.0) * (0.5 * length);
    if !stage.clear((middle.x, middle.z), 0.5 * length) {
        return Some(());
    }
    stage.claim((middle.x, middle.z), 0.5 * length)?;
    let drums = u32::try_from(mathf::round_i32(length / (1.6 * radius)).max(1)).ok()?;
    let tall = length / f64::from(drums);
    let mut reach = 0.0;
    for _ in 0..drums {
        let skew = heading + dice.range(-0.12, 0.12);
        let lying = Frame::turned(skew, FRAC_PI_2 + dice.range(-0.04, 0.04));
        let centre =
            from + direction(heading, 0.0, 0.0) * (reach + 0.5 * tall) + Vec3::UP * (0.88 * radius);
        let half = Vec3::new(radius, 0.5 * tall - 0.002, radius);
        mason.unit(
            (Pose::new(centre, lying), half),
            Form::Drum {
                taper: 6,
                swell: 0,
                flutes: 0,
            },
            Dressing::Ashlar,
        )?;
        reach += tall + dice.range(0.04, 0.3);
    }
    Some(())
}

#[cfg(test)]
#[path = "architecture_tests.rs"]
mod tests;
