//! A farmed land's pastures as their stock left them: each pat the grazing
//! scatters ([`crate::grazing`]) laid about the eye where the land is grazed,
//! fresh and glossy, crusted, or old and crumbling into the grass.
//!
//! A pat is a low mound coiled as it was dropped, its edge ragged, built
//! once at a unit radius for each age and laid at the pat's own size.

use alloc::vec::Vec;
use core::f64::consts::{PI, TAU};

use tairix_countryside::Point;
use tairix_util::mathf;

use super::super::{rgb, Dice, Stage};
use crate::farmed::Grown;
use crate::grazing::{self, Pat};
use crate::land::Land;
use crate::material::{Finish, Material, Relief};
use crate::pigment::Pigment;
use crate::prototype::{Assembly, Building};
use crate::sample::unit;
use crate::shape::Shape;
use crate::vector::{power, share, Frame, Pose, Vec3};

/// How many rings out from its middle and pieces round a pat is built in.
const RINGS: usize = 6;
const PIECES: usize = 16;

/// The most pats laid about the eye, and the share of the stage's room left
/// they may take: what one scene may cost.
const MOST: usize = 4000;
const ROOM: usize = 4;

/// A pat at an age: how high it stands at its middle as a share of its
/// radius, how deep its coils lie, how ragged its edge, and its colour and
/// finish.
struct Aged {
    rise: f64,
    coils: f64,
    ragged: f64,
    colour: u32,
    wet: bool,
}

/// Fresh and wet, crusted, and old.
const AGES: [Aged; 3] = [
    Aged {
        rise: 0.3,
        coils: 0.14,
        ragged: 0.12,
        colour: 0x36_2C_18,
        wet: true,
    },
    Aged {
        rise: 0.22,
        coils: 0.1,
        ragged: 0.18,
        colour: 0x4E_3E_28,
        wet: false,
    },
    Aged {
        rise: 0.1,
        coils: 0.05,
        ragged: 0.3,
        colour: 0x7E_72_5C,
        wet: false,
    },
];

impl Aged {
    /// The most a pat of a unit radius stands at this age.
    fn top(&self) -> f64 {
        self.rise * (1.0 + self.coils)
    }
}

/// Which of [`AGES`] a pat `age` old is laid as.
fn aged(age: f64) -> usize {
    if age < 0.25 {
        0
    } else if age < 0.65 {
        1
    } else {
        2
    }
}

/// A pat `aged` as it is, in `material`, drawn from `dice`: a coiled mound a
/// unit across, sunk at its edge into the ground.
fn pat(aged: &Aged, material: u16, dice: &mut Dice) -> Option<Building> {
    let phases = [
        dice.range(0.0, TAU),
        dice.range(0.0, TAU),
        dice.range(0.0, TAU),
    ];
    let coil = dice.range(0.0, TAU);
    let edge = |around: f64| {
        1.0 + aged.ragged
            * (0.5 * mathf::sin(3.0 * around + phases[0])
                + 0.3 * mathf::sin(5.0 * around + phases[1])
                + 0.2 * mathf::sin(9.0 * around + phases[2]))
    };
    let height = |out: f64, around: f64| {
        let mound = aged.rise * power(1.0 - out * out, 0.7);
        // Laid down in coils, a turn of them crossing each ring.
        let coiled =
            aged.coils * aged.rise * mathf::sin(PI * 5.0 * out + around + coil) * (1.0 - out);
        mound + coiled - 0.04 * out * out
    };
    let mut points = Vec::new();
    points.try_reserve_exact(1 + RINGS * PIECES).ok()?;
    points.push(Vec3::new(0.0, height(0.0, 0.0), 0.0));
    for ring in 1..=RINGS {
        let out = share(ring, RINGS);
        for piece in 0..PIECES {
            let around = TAU * share(piece, PIECES);
            let reach = out * edge(around);
            points.push(Vec3::new(
                reach * mathf::cos(around),
                height(out, around),
                reach * mathf::sin(around),
            ));
        }
    }
    let at =
        |ring: usize, piece: usize| u32::try_from(1 + (ring - 1) * PIECES + piece % PIECES).ok();
    let mut faces = Vec::new();
    faces.try_reserve_exact(PIECES * (2 * RINGS - 1)).ok()?;
    for piece in 0..PIECES {
        faces.push(([0, at(1, piece + 1)?, at(1, piece)?], material));
        for ring in 1..RINGS {
            let (a, b, c, d) = (
                at(ring, piece)?,
                at(ring, piece + 1)?,
                at(ring + 1, piece + 1)?,
                at(ring + 1, piece)?,
            );
            faces.push(([a, b, c], material));
            faces.push(([a, c, d], material));
        }
    }
    let mut assembly = Assembly::with_room(faces.len(), points.len())?;
    assembly.mesh(&points, &faces)?;
    assembly.finish()
}

/// The pats under `seed` on `land`'s grazed ground within `reach` of `eye`
/// that no snow buries, no more than `room` of them, the nearest kept where
/// more lie there.
fn nearest(
    stage: &Stage,
    land: &Land,
    (eye, reach): (Point, f64),
    (seed, room): (u32, usize),
) -> Option<Vec<Pat>> {
    let mut near = Vec::new();
    for pat in grazing::about((eye.x, eye.y), reach, seed) {
        let ((x, z), fields) = (pat.at, &stage.fields);
        let lying = land.grids.height(fields, x, z) - land.grids.beneath_snow(fields, x, z);
        let top = AGES.get(aged(pat.age)).map_or(0.0, Aged::top) * pat.radius;
        if lying < top && Grown::of(land.grids.grows(fields, x, z).0) == Grown::Grazed {
            near.try_reserve(1).ok()?;
            near.push(pat);
        }
    }
    if near.len() > room {
        let off = |pat: &Pat| {
            let (x, z) = (pat.at.0 - eye.x, pat.at.1 - eye.y);
            x * x + z * z
        };
        near.select_nth_unstable_by(room, |a, b| off(a).total_cmp(&off(b)));
        near.truncate(room);
    }
    Some(near)
}

/// Lay the pats under `seed` lying within `reach` of the eye at `eye` on
/// `land`'s grazed ground, on the ground beneath any snow, nearest first as
/// far as the stage has room; `None` when the stage will not hold them.
pub(super) fn lay(
    stage: &mut Stage,
    land: &Land,
    (eye, reach): (Point, f64),
    seed: u32,
) -> Option<()> {
    let room = (stage.room() / ROOM).min(MOST);
    let near = nearest(stage, land, (eye, reach), (seed, room))?;
    if near.is_empty() {
        return Some(());
    }
    let mut made = [(0, 0); 3];
    for (index, (slot, aged)) in made.iter_mut().zip(&AGES).enumerate() {
        let mut dice = Dice::keyed(u64::from(seed), index);
        let roughness = if aged.wet { 0.25 } else { 0.9 };
        let material = stage.material(
            Material::new(
                Pigment::Solid(rgb(aged.colour)),
                Finish::Coated { roughness },
            )
            .with_relief(Relief::grain(0.35, 25.0, dice.seed())),
        )?;
        let building = pat(aged, u16::try_from(material).ok()?, &mut dice)?;
        *slot = (stage.assemble(building)?, material);
    }
    for Pat {
        at,
        radius,
        age,
        key,
    } in near
    {
        let (prototype, material) = *made.get(aged(age))?;
        let normal = land.grids.normal(&stage.fields, at.0, at.1);
        let frame = Frame::turned(TAU * unit(key), 0.0).aligning(Vec3::UP, normal);
        let pose = Pose::new(
            Vec3::new(
                at.0,
                land.grids.beneath_snow(&stage.fields, at.0, at.1),
                at.1,
            ),
            frame,
        );
        stage.add(
            Shape::Instance {
                prototype,
                pose,
                scale: radius,
                key,
            },
            material,
            pose,
            false,
        )?;
    }
    Some(())
}

#[cfg(test)]
#[path = "pats_tests.rs"]
mod tests;
