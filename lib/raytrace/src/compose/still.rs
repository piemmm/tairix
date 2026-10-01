//! Still lifes: pieces set out on a floor, under the open sky, softboxes, the
//! dusk, or lamps at night.

use core::f64::consts::{FRAC_PI_2, PI, TAU};

use tairix_util::mathf;

use super::landscape::{self, Lawning, Vantage, GREEN};
use super::plants::{self, Character, Kind};
use super::weather::{self, Climate, Cover, Hour};
use super::{
    direction, rgb, softbox, sun, Composed, Cut, Dice, Look, Stage, View, CHECKERS, GLASS_TINTS,
    LAMPS, VIVID,
};
use crate::light::Light;
use crate::material::{Finish, Material, Relief};
use crate::pigment::Pigment;
use crate::scene::Exposure;
use crate::sky::{Dome, Glow, Gradient, Sky};
use crate::tree::Season;
use crate::vector::{Frame, Vec3};

const CLASSIC: Climate = Climate {
    hours: &[(Hour::Noon, 2), (Hour::Day, 5), (Hour::Golden, 3)],
    covers: &[(Cover::Clear, 3), (Cover::Fair, 4), (Cover::Cirrus, 2)],
    haze: (1.0, 2.2),
    base: 100.0,
    albedo: 0.2,
};

pub(super) fn classic(stage: &mut Stage, dice: &mut Dice) -> Option<Look> {
    let yaw = dice.range(0.0, TAU);
    let floor = floor(stage, dice)?;
    stage.ground(0.0, floor)?;
    match dice.count(0, 3) {
        0 => scatter(stage, dice)?,
        1 => row(stage, dice, yaw)?,
        2 => stack(stage, dice)?,
        _ => circle(stage, dice)?,
    }
    let weather = weather::outdoors(stage, dice, &CLASSIC, yaw + PI)?;
    // A cool fill from the side away from the sun, so no shadow is black.
    let away = direction(
        yaw,
        dice.sign() * dice.angle(120.0, 170.0),
        35.0_f64.to_radians(),
    ) * 6.0;
    stage.light(Light::Point {
        at: away,
        intensity: rgb(0xB8_C8_FF) * dice.range(2.0, 4.0),
    })?;
    Some(Look {
        sky: weather.sky,
        fog: None,
        exposure: weather.exposure,
        daylight: weather.daylight,
        view: View::Framed {
            yaw,
            elevation: dice.angle(10.0, 26.0),
            fov: dice.angle(38.0, 50.0),
            fill: 0.86,
            aperture: dice.range(0.0, 0.008),
        },
    })
}

/// A floor running to the horizon: a checkerboard, tiles, or flagstones.
fn floor(stage: &mut Stage, dice: &mut Dice) -> Option<usize> {
    let roughness = dice.pick(&[0.0, 0.04, 0.12, 0.22])?;
    let pigment = match dice.count(0, 4) {
        0..=2 => {
            let (a, b) = dice.pick(&CHECKERS)?;
            Pigment::Checker {
                a: rgb(a),
                b: rgb(b),
                size: dice.range(0.8, 1.3),
            }
        }
        3 => {
            let (a, b) = dice.pick(&CHECKERS)?;
            Pigment::Tiles {
                a: rgb(a),
                b: rgb(a).lerp(rgb(b), 0.25),
                grout: rgb(0x30_2C_28),
                size: dice.range(0.5, 1.0),
                gap: 0.02,
                seed: dice.seed(),
            }
        }
        _ => Pigment::Stones {
            a: rgb(0xB8_B0_A0),
            b: rgb(0x8A_82_74),
            joint: rgb(0x3A_36_30),
            size: dice.range(0.6, 1.1),
            gap: 0.03,
            seed: dice.seed(),
        },
    };
    stage.coated(pigment, roughness)
}

/// A centrepiece and a handful of pieces about it.
fn scatter(stage: &mut Stage, dice: &mut Dice) -> Option<()> {
    let hero = stage.precious(dice)?;
    let hero_radius = dice.range(0.8, 1.15);
    let spot = stage.place(dice, ((0.0, 0.0), 0.4), hero_radius)?;
    stage.ball(Vec3::new(spot.0, 0.0, spot.1), hero_radius, hero, dice)?;
    for _ in 0..dice.count(4, 7) {
        let radius = dice.range(0.25, 0.7);
        let Some((x, z)) = stage.place(dice, ((0.0, 0.0), 3.0), radius) else {
            continue;
        };
        let material = piece_material(stage, dice)?;
        piece(stage, dice, Vec3::new(x, 0.0, z), radius, material)?;
    }
    Some(())
}

/// Something to make a lesser piece of.
fn piece_material(stage: &mut Stage, dice: &mut Dice) -> Option<usize> {
    if dice.chance(0.45) {
        stage.precious(dice)
    } else if dice.chance(0.3) {
        stage.marble(dice)
    } else {
        stage.paint(dice, &VIVID)
    }
}

/// A piece about `radius` across at `base`: mostly a ball, now and then a
/// ring, a gem, or a post.
fn piece(
    stage: &mut Stage,
    dice: &mut Dice,
    base: Vec3,
    radius: f64,
    material: usize,
) -> Option<usize> {
    match dice.count(0, 11) {
        0 => {
            let major = radius * 0.72;
            let upright = Frame::turned(dice.range(0.0, TAU), FRAC_PI_2);
            stage.ring(
                base + Vec3::UP * radius,
                upright,
                (major, radius - major),
                material,
            )
        }
        1 | 2 => {
            let cut = dice.pick(&Cut::ALL)?;
            stage.gem(base, cut, radius, dice.range(0.0, TAU), material)
        }
        3 => stage.post(
            base,
            (
                radius,
                radius * dice.range(0.3, 1.0),
                dice.range(1.2, 3.0) * radius,
            ),
            material,
            dice,
        ),
        _ => stage.ball(base, radius, material, dice),
    }
}

/// Balls in a curving row, each smaller than the last.
fn row(stage: &mut Stage, dice: &mut Dice, yaw: f64) -> Option<()> {
    let materials = [
        stage.precious(dice)?,
        stage.precious(dice)?,
        piece_material(stage, dice)?,
    ];
    let count = dice.count(4, 7);
    let shrink = dice.range(0.72, 0.88);
    let bend = dice.range(-0.3, 0.3);
    let mut balls = [(0.0, 0.0, 0.0); 7];
    let (mut x, mut z, mut radius) = (0.0, 0.0, dice.range(0.7, 1.0));
    let mut heading = yaw + FRAC_PI_2;
    for (index, ball) in balls.iter_mut().take(count as usize).enumerate() {
        if index > 0 {
            let next = radius * shrink;
            let gap = radius + next + dice.range(0.08, 0.3);
            heading += bend;
            x += gap * mathf::sin(heading);
            z += gap * mathf::cos(heading);
            radius = next;
        }
        *ball = (x, z, radius);
    }
    let taken = balls.get(..count as usize)?;
    let (mut low, mut high) = (
        (f64::INFINITY, f64::INFINITY),
        (f64::NEG_INFINITY, f64::NEG_INFINITY),
    );
    for &(x, z, radius) in taken {
        low = (low.0.min(x - radius), low.1.min(z - radius));
        high = (high.0.max(x + radius), high.1.max(z + radius));
    }
    let middle = (low.0.midpoint(high.0), low.1.midpoint(high.1));
    for (index, &(x, z, radius)) in taken.iter().enumerate() {
        let at = (x - middle.0, z - middle.1);
        stage.claim(at, radius)?;
        let material = *materials.get(index % materials.len())?;
        stage.ball(Vec3::new(at.0, 0.0, at.1), radius, material, dice)?;
    }
    Some(())
}

/// A pyramid of balls, each resting in the hollow of three below, as shot
/// was stacked.
fn stack(stage: &mut Stage, dice: &mut Dice) -> Option<()> {
    let layers = dice.count(2, 3);
    let radius = dice.range(0.3, 0.5);
    let body = piece_material(stage, dice)?;
    let crown = stage.precious(dice)?;
    let turn = Frame::turned(dice.range(0.0, TAU), 0.0);
    let span = f64::from(layers - 1);
    let middle = Vec3::new(radius * span, 0.0, radius * span / mathf::sqrt(3.0));
    let rise = 2.0 * radius * mathf::sqrt(2.0 / 3.0);
    for layer in 0..layers {
        let size = layers - layer;
        let offset = Vec3::new(
            radius * f64::from(layer),
            0.0,
            radius * f64::from(layer) / mathf::sqrt(3.0),
        );
        for j in 0..size {
            for i in 0..size - j {
                let local = Vec3::new(
                    2.0 * radius * (f64::from(i) + 0.5 * f64::from(j)),
                    rise * f64::from(layer),
                    mathf::sqrt(3.0) * radius * f64::from(j),
                ) + offset
                    - middle;
                let material = if layer + 1 == layers { crown } else { body };
                stage.ball(turn.to_world(local), radius, material, dice)?;
            }
        }
    }
    stage.claim((0.0, 0.0), 2.0 * radius * f64::from(layers))?;
    for _ in 0..dice.count(1, 3) {
        let radius = dice.range(0.2, 0.5);
        let Some((x, z)) = stage.place(dice, ((0.0, 0.0), 3.0), radius) else {
            continue;
        };
        let material = piece_material(stage, dice)?;
        piece(stage, dice, Vec3::new(x, 0.0, z), radius, material)?;
    }
    Some(())
}

/// A centrepiece in a ring of small balls.
fn circle(stage: &mut Stage, dice: &mut Dice) -> Option<()> {
    let hero = stage.precious(dice)?;
    let size = dice.range(0.6, 0.95);
    stage.claim((0.0, 0.0), size)?;
    piece(stage, dice, Vec3::ZERO, size, hero)?;
    let beads = [stage.precious(dice)?, piece_material(stage, dice)?];
    let count = dice.count(6, 11);
    let reach = size + dice.range(0.9, 1.6);
    let small = dice.range(0.16, 0.3);
    let turn = dice.range(0.0, TAU);
    for bead in 0..count {
        let angle = turn + TAU * f64::from(bead) / f64::from(count);
        let at = (reach * mathf::sin(angle), reach * mathf::cos(angle));
        stage.claim(at, small)?;
        let material = *beads.get(bead as usize % beads.len())?;
        stage.ball(Vec3::new(at.0, 0.0, at.1), small, material, dice)?;
    }
    Some(())
}

pub(super) fn studio(stage: &mut Stage, dice: &mut Dice) -> Option<Look> {
    let yaw = dice.range(0.0, TAU);
    // Mostly high key, on a pale sweep in a bright room; now and then low
    // key, on black glass with the softboxes the only light.
    let high_key = dice.chance(0.65);
    let floor = if high_key {
        stage.coated(
            Pigment::Solid(rgb(dice.pick(&[0xD6_D4_D0, 0xE4_DC_D0, 0xC8_D0_D8])?)),
            0.4,
        )?
    } else {
        stage.coated(Pigment::Solid(rgb(0x2A_2A_2E)), 0.2)?
    };
    stage.ground(0.0, floor)?;
    let top = plinth(stage, dice)?;
    showpiece(stage, dice, top)?;
    for _ in 0..dice.count(2, 5) {
        let radius = dice.range(0.18, 0.42);
        let Some((x, z)) = stage.place(dice, ((0.0, 0.0), 2.2), radius) else {
            continue;
        };
        let material = if dice.chance(0.4) {
            stage.precious(dice)?
        } else {
            stage.paint(dice, &VIVID)?
        };
        let base = Vec3::new(x, 0.0, z);
        if dice.chance(0.3) {
            let height = dice.range(0.4, 1.1);
            stage.post(
                base,
                (radius, radius * dice.range(0.35, 1.0), height),
                material,
                dice,
            )?;
        } else {
            stage.ball(base, radius, material, dice)?;
        }
    }
    softboxes(stage, dice, yaw)?;
    let (sky, exposure) = if high_key {
        (room(0xB0_B0_B4, 0xDC_DC_DE, 0x9A_9A_9C), 0.82)
    } else {
        (room(0x1C_1C_22, 0x40_40_48, 0x14_1416), 1.3)
    };
    Some(Look {
        sky,
        fog: None,
        exposure: Exposure::Fixed(exposure),
        daylight: 1.0,
        view: View::Framed {
            yaw,
            elevation: dice.angle(8.0, 22.0),
            fov: dice.angle(28.0, 38.0),
            fill: 0.82,
            aperture: dice.range(0.004, 0.012),
        },
    })
}

/// The piece a studio shows off on the plinth whose top is at `top`: a
/// ring on edge, a cut gem, a ringed planet, or a ball.
fn showpiece(stage: &mut Stage, dice: &mut Dice, top: Vec3) -> Option<()> {
    let hero = stage.precious(dice)?;
    let size = dice.range(0.35, 0.5);
    match dice.count(0, 4) {
        0 => {
            let upright = Frame::turned(dice.range(0.0, TAU), FRAC_PI_2);
            stage.ring(
                top + Vec3::UP * size,
                upright,
                (size * 0.72, size * 0.28),
                hero,
            )?;
        }
        1 => {
            let cut = dice.pick(&Cut::ALL)?;
            stage.gem(top, cut, size, dice.range(0.0, TAU), hero)?;
        }
        2 => {
            // A ball with a ring about it, tilted as a planet's.
            stage.ball(top, 0.7 * size, hero, dice)?;
            let ring = stage.precious(dice)?;
            let tilt = Frame::turned(dice.range(0.0, TAU), dice.angle(15.0, 35.0));
            stage.ring(
                top + Vec3::UP * (0.7 * size),
                tilt,
                (1.35 * size, 0.06 * size),
                ring,
            )?;
        }
        _ => {
            stage.ball(top, size, hero, dice)?;
        }
    }
    Some(())
}

/// A studio's softboxes about a stage seen from `yaw`.
fn softboxes(stage: &mut Stage, dice: &mut Dice, yaw: f64) -> Option<()> {
    // A large key softbox to one side, a broader fill opposite, and a strip
    // above and behind that draws a rim down every edge.
    let hand = dice.sign();
    let key = direction(yaw, hand * dice.angle(35.0, 60.0), dice.angle(30.0, 48.0));
    softbox(
        stage,
        key * 3.8,
        (2.2, 1.6),
        rgb(0xFF_F2_E2) * dice.range(6.0, 9.0),
    )?;
    let fill = direction(yaw, -hand * dice.angle(55.0, 85.0), 22.0_f64.to_radians());
    softbox(
        stage,
        fill * 4.6,
        (3.0, 2.0),
        rgb(0xE4_EC_FF) * dice.range(1.2, 2.2),
    )?;
    let rim = direction(yaw, PI + dice.angle(-25.0, 25.0), dice.angle(40.0, 60.0));
    softbox(
        stage,
        rim * 4.2,
        (0.6, 2.8),
        rgb(0xF4_F6_FF) * dice.range(8.0, 12.0),
    )?;
    Some(())
}

/// The walls of a room, as a sky from `zenith` to `horizon` above a floor
/// seen far off as `ground`.
fn room(zenith: u32, horizon: u32, ground: u32) -> Sky {
    Sky {
        dome: Dome::Gradient(Gradient {
            zenith: rgb(zenith),
            horizon: rgb(horizon),
            ground: rgb(ground),
            glow: None,
        }),
        stars: 0.0,
        clouds: None,
        bank: None,
    }
}

/// A plinth at the middle of the stage: a block, a drum, or a flight of
/// steps; where its top is.
fn plinth(stage: &mut Stage, dice: &mut Dice) -> Option<Vec3> {
    let stone = stage.plinth_stone(dice)?;
    let yaw = dice.range(0.0, TAU);
    match dice.count(0, 2) {
        0 => {
            let half = Vec3::new(
                dice.range(0.45, 0.7),
                dice.range(0.3, 0.6),
                dice.range(0.45, 0.7),
            );
            stage.claim((0.0, 0.0), half.x.max(half.z) * 1.42)?;
            stage.block(Vec3::ZERO, half, yaw, stone)?;
            Some(Vec3::UP * (2.0 * half.y))
        }
        1 => {
            let (radius, height) = (dice.range(0.45, 0.65), dice.range(0.6, 1.1));
            stage.claim((0.0, 0.0), radius)?;
            stage.post(Vec3::ZERO, (radius, radius, height), stone, dice)?;
            Some(Vec3::UP * height)
        }
        _ => {
            let (mut half, mut level) = (dice.range(0.8, 1.0), 0.0);
            stage.claim((0.0, 0.0), half * 1.42)?;
            for _ in 0..3 {
                let rise = dice.range(0.12, 0.2);
                stage.block(
                    Vec3::UP * level,
                    Vec3::new(half, 0.5 * rise, half),
                    yaw,
                    stone,
                )?;
                level += rise;
                half *= 0.72;
            }
            Some(Vec3::UP * level)
        }
    }
}

pub(super) fn crystals(stage: &mut Stage, dice: &mut Dice) -> Option<Look> {
    let yaw = dice.range(0.0, TAU);
    let floor = match dice.count(0, 2) {
        0 | 1 => stage.coated(Pigment::Solid(rgb(0x08_08_0A)), dice.range(0.03, 0.08))?,
        _ => stage.coated(
            Pigment::Stones {
                a: rgb(0x2A_2C_30),
                b: rgb(0x1C_1C_20),
                joint: rgb(0x0A_0A_0C),
                size: dice.range(0.4, 0.8),
                gap: 0.02,
                seed: dice.seed(),
            },
            0.25,
        )?,
    };
    stage.ground(0.0, floor)?;
    if dice.chance(0.35) {
        druse(stage, dice)?;
    } else {
        clusters(stage, dice)?;
    }
    for _ in 0..dice.count(2, 4) {
        let size = dice.range(0.15, 0.32);
        let Some((x, z)) = stage.place(dice, ((0.0, 0.0), 1.6), size) else {
            continue;
        };
        let gem = if dice.chance(0.4) {
            let tint = rgb(dice.pick(&GLASS_TINTS)?);
            stage.fire(dice, tint)?
        } else {
            stage.glass(rgb(dice.pick(&GLASS_TINTS)?), 0.0)?
        };
        stage.gem(
            Vec3::new(x, 0.0, z),
            dice.pick(&Cut::ALL)?,
            size,
            dice.range(0.0, TAU),
            gem,
        )?;
    }
    // Coloured spots before the crystals; behind them the last of the day
    // glows along the horizon, and it is that glow they bend toward the eye.
    for turn in [dice.range(30.0, 80.0), -dice.range(30.0, 80.0)] {
        let at = direction(yaw, turn.to_radians(), dice.angle(40.0, 65.0)) * 5.5;
        stage.light(Light::Spot {
            at,
            axis: (Vec3::UP * 0.4 - at).normalized(),
            cos_inner: mathf::cos(16.0_f64.to_radians()),
            cos_outer: mathf::cos(28.0_f64.to_radians()),
            intensity: rgb(dice.pick(&LAMPS)?) * dice.range(25.0, 45.0),
        })?;
    }
    if dice.chance(0.6) {
        if let Some((x, z)) = stage.place(dice, ((0.0, 0.0), 2.8), 0.12) {
            let colour = rgb(dice.pick(&LAMPS)?);
            stage.orb(Vec3::new(x, dice.range(0.12, 0.9), z), 0.12, colour * 4.0)?;
        }
    }
    let dusk = direction(yaw, PI + dice.angle(-40.0, 40.0), (-4.0_f64).to_radians());
    Some(Look {
        sky: Sky {
            dome: Dome::Gradient(Gradient {
                zenith: rgb(0x0C_0C_22),
                horizon: rgb(0x4A_32_60).lerp(rgb(dice.pick(&LAMPS)?), 0.25),
                ground: rgb(0x06_06_08),
                glow: Some(Glow {
                    toward: dusk,
                    colour: rgb(0xE8_80_60),
                    horizon: 2.0,
                }),
            }),
            stars: 0.3,
            clouds: None,
            bank: None,
        },
        fog: None,
        exposure: Exposure::Fixed(1.2),
        daylight: 1.0,
        view: View::Framed {
            yaw,
            elevation: dice.angle(6.0, 16.0),
            fov: dice.angle(30.0, 40.0),
            fill: 0.86,
            aperture: dice.range(0.004, 0.012),
        },
    })
}

/// One or two clusters of crystals, each in a glass of its own tint.
fn clusters(stage: &mut Stage, dice: &mut Dice) -> Option<()> {
    for cluster in 0..dice.count(1, 2) {
        let spread = if cluster == 0 { 0.2 } else { 1.5 };
        let Some((cx, cz)) = stage.place(dice, ((0.0, 0.0), spread), 0.7) else {
            continue;
        };
        let tint = rgb(dice.pick(&GLASS_TINTS)?);
        for _ in 0..dice.count(5, 11) {
            let frame = Frame::turned(dice.range(0.0, TAU), dice.angle(0.0, 38.0));
            let base = Vec3::new(
                cx + dice.range(-0.25, 0.25),
                0.0,
                cz + dice.range(-0.25, 0.25),
            );
            let shade = tint.lerp(Vec3::splat(0.95), dice.range(0.0, 0.3));
            let glass = stage.glass(shade, if dice.chance(0.2) { 0.15 } else { 0.0 })?;
            stage.crystal(
                base,
                frame,
                (dice.range(0.08, 0.2), dice.range(0.45, 1.5)),
                glass,
            )?;
        }
    }
    Some(())
}

/// A rough stone, crystals growing out of its crown.
fn druse(stage: &mut Stage, dice: &mut Dice) -> Option<()> {
    let radius = dice.range(0.55, 0.8);
    stage.claim((0.0, 0.0), radius)?;
    let rock = stage.material(
        Material::new(
            Pigment::Speckle {
                base: rgb(0x5A_52_4C),
                flecks: [rgb(0x2A_26_24), rgb(0x8A_80_76)],
                scale: 30.0,
                seed: dice.seed(),
            },
            Finish::Coated { roughness: 0.8 },
        )
        .with_relief(Relief::Grain {
            depth: 0.35,
            scale: 7.0,
            seed: dice.seed(),
        }),
    )?;
    let (stone, middle) = stage.boulder(Vec3::ZERO, radius, rock, dice)?;
    let tint = rgb(dice.pick(&GLASS_TINTS)?);
    for _ in 0..dice.count(9, 16) {
        let out = Vec3::new(
            dice.range(-0.7, 0.7),
            dice.range(0.5, 1.0),
            dice.range(-0.7, 0.7),
        )
        .normalized();
        let (width, reach) = (dice.range(0.05, 0.12), dice.range(0.2, 0.6));
        let shade = tint.lerp(Vec3::splat(0.95), dice.range(0.0, 0.35));
        // Rooted inside the rock along its own line, however flat the rock
        // came out, and standing `reach` clear of it.
        let Some(face) = stage.exit(stone, middle, out) else {
            continue;
        };
        let root = 0.8 * face;
        let glass = stage.glass(shade, 0.0)?;
        stage.crystal(
            middle + out * root,
            Frame::WORLD.aligning(Vec3::UP, out),
            (width, face - root + reach),
            glass,
        )?;
    }
    Some(())
}

pub(super) fn nocturne(stage: &mut Stage, dice: &mut Dice) -> Option<Look> {
    let yaw = dice.range(0.0, TAU);
    let floor = stage.coated(night_floor(dice), dice.range(0.06, 0.14))?;
    stage.ground(0.0, floor)?;
    for _ in 0..dice.count(3, 5) {
        let radius = dice.range(0.3, 0.7);
        let Some((x, z)) = stage.place(dice, ((0.0, 0.0), 3.0), radius) else {
            continue;
        };
        let material = if dice.chance(0.75) {
            stage.precious(dice)?
        } else {
            stage.paint(dice, &[0xE8_E6_E0, 0x8A_86_80])?
        };
        piece(stage, dice, Vec3::new(x, 0.0, z), radius, material)?;
    }
    // Lamps bright enough to light what stands near them, and no brighter,
    // so each keeps its colour on screen: floating, on the floor, or on
    // posts.
    let posts = dice.chance(0.4);
    let iron = stage.metal(rgb(0x2A_2A_2E), 0.35)?;
    for _ in 0..dice.count(4, 6) {
        let radius = dice.range(0.12, 0.26);
        let Some((x, z)) = stage.place(dice, ((0.0, 0.0), 3.6), radius) else {
            continue;
        };
        let colour = rgb(dice.pick(&LAMPS)?);
        let height = if posts {
            let height = dice.range(1.2, 2.0);
            stage.post(Vec3::new(x, 0.0, z), (0.05, 0.03, height), iron, dice)?;
            height + radius
        } else if dice.chance(0.4) {
            radius
        } else {
            dice.range(0.5, 1.6)
        };
        stage.orb(
            Vec3::new(x, height, z),
            radius,
            colour * dice.range(3.5, 6.0),
        )?;
    }
    let spot_at = direction(
        yaw,
        dice.sign() * dice.angle(20.0, 70.0),
        62.0_f64.to_radians(),
    ) * 6.0;
    stage.light(Light::Spot {
        at: spot_at,
        axis: (Vec3::UP * 0.3 - spot_at).normalized(),
        cos_inner: mathf::cos(14.0_f64.to_radians()),
        cos_outer: mathf::cos(28.0_f64.to_radians()),
        intensity: rgb(0xFF_EE_DC) * dice.range(12.0, 20.0),
    })?;
    let moon = direction(yaw, dice.angle(120.0, 240.0), dice.angle(22.0, 48.0));
    stage.light(sun(moon, 1.1, rgb(0xB8_C8_FF), 0.25))?;
    Some(Look {
        sky: Sky {
            dome: Dome::Gradient(Gradient {
                zenith: rgb(0x05_07_14),
                horizon: rgb(0x1A_20_3C),
                ground: rgb(0x06_06_08),
                glow: Some(Glow {
                    toward: moon,
                    colour: rgb(0x30_3C_60),
                    horizon: 0.0,
                }),
            }),
            stars: dice.range(0.35, 0.6),
            clouds: None,
            bank: None,
        },
        fog: Some(crate::scene::Fog { density: 0.01 }),
        exposure: Exposure::Fixed(1.9),
        daylight: 1.0,
        view: View::Framed {
            yaw,
            elevation: dice.angle(9.0, 20.0),
            fov: dice.angle(38.0, 48.0),
            fill: 0.88,
            aperture: dice.range(0.0, 0.01),
        },
    })
}

/// A floor for a night scene: dark tiles, boards, or cobbles.
fn night_floor(dice: &mut Dice) -> Pigment {
    match dice.count(0, 2) {
        0 => Pigment::Tiles {
            a: rgb(0x2C_2C32),
            b: rgb(0x46_42_44),
            grout: rgb(0x12_12_14),
            size: dice.range(0.8, 1.3),
            gap: 0.02,
            seed: dice.seed(),
        },
        1 => Pigment::Planks {
            light: rgb(0x5A_3C_26),
            dark: rgb(0x2A_1A_10),
            width: dice.range(0.14, 0.22),
            length: dice.range(1.6, 2.6),
            seed: dice.seed(),
        },
        _ => Pigment::Stones {
            a: rgb(0x3A_3A_40),
            b: rgb(0x26_26_2C),
            joint: rgb(0x10_10_12),
            size: dice.range(0.25, 0.45),
            gap: 0.025,
            seed: dice.seed(),
        },
    }
}

const BUBBLES: Climate = Climate {
    hours: &[(Hour::Day, 5), (Hour::Golden, 4), (Hour::Noon, 1)],
    covers: &[(Cover::Clear, 2), (Cover::Fair, 5), (Cover::Cirrus, 2)],
    haze: (1.0, 2.4),
    base: 120.0,
    albedo: 0.18,
};

pub(super) fn bubbles(stage: &mut Stage, dice: &mut Dice) -> Option<Composed> {
    let heading = dice.range(0.0, TAU);
    let backdrop = landscape::backdrop(
        stage,
        dice,
        (40.0, 0.0),
        &GREEN,
        (&[Kind::Oak, Kind::Birch, Kind::Poplar], Season::Summer),
    )?;
    let terrain = &backdrop.terrain;
    // The bubbles drift ahead of the camera, which looks a little up at
    // them against the trees and the sky.
    let ahead = direction(heading, 0.0, 0.0);
    let middle = ahead * 4.0;
    for _ in 0..dice.count(6, 14) {
        let radius = dice.range(0.08, 0.4);
        let Some((x, z)) = stage.place(dice, ((middle.x, middle.z), 3.0), radius) else {
            continue;
        };
        let film = stage.bubble_film(dice)?;
        let centre = Vec3::new(x, terrain.height(x, z) + dice.range(0.6, 2.8), z);
        stage.add(
            crate::shape::Shape::Sphere { centre, radius },
            film,
            crate::vector::Pose::new(centre, super::tumble(dice)),
            true,
        )?;
    }
    let eye = Vec3::new(
        -ahead.x * 1.5,
        terrain.height(-ahead.x * 1.5, -ahead.z * 1.5) + dice.range(0.8, 1.5),
        -ahead.z * 1.5,
    );
    let season = if dice.chance(0.5) {
        Season::Spring
    } else {
        Season::Summer
    };
    let lawning = Lawning {
        eye: (eye.x, eye.z),
        grassland: plants::grassland(dice, Character::Park, season),
    };
    let weather = weather::outdoors(stage, dice, &BUBBLES, heading)?;
    let target = middle + Vec3::UP * (terrain.height(middle.x, middle.z) + dice.range(1.4, 2.2));
    let look = Look {
        sky: weather.sky,
        fog: None,
        exposure: weather.exposure,
        daylight: weather.daylight,
        view: View::Placed {
            eye,
            target,
            fov: dice.angle(45.0, 58.0),
            aperture: dice.range(0.004, 0.012),
        },
    };
    Some(backdrop.seen(look, Vantage { eye, heading }, Some(&lawning)))
}
