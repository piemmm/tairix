//! Mud dried and cracked about the eye: on level, bare silt the water laid
//! and has since left, in tiles of plates laid square to a lattice of the
//! land's own, so a flat of it runs on unbroken from tile to tile, and in
//! patches with ground between where the silt lay thinner.

use core::f64::consts::FRAC_1_SQRT_2;

use tairix_util::mathf;

use super::landscape::ground_of;
use super::lattice::snap;
use super::{Dice, Recipe, Stage};
use crate::land::{Land, Lie};
use crate::material::{Finish, Material, Relief};
use crate::mud::{Crust, Mud};
use crate::noise::{fbm2, hash2};
use crate::pigment::Pigment;
use crate::shape::Shape;
use crate::vector::{Frame, Pose, Vec3};

/// The side of a tile of cracked mud, and how many tiles of a crust are
/// drawn to choose among.
const TILE: f64 = 3.0;
const VARIANTS: usize = 4;
/// How broad the patches mud dries in run, and the share of a dry flat they
/// cover.
const PATCHES: f64 = 14.0;
const COVER: f64 = 0.55;
/// How far a tile's corners may stand off the plane through its middle and
/// still be laid as one crust.
const LEVEL: f64 = 0.01;
/// How large a crust's plates come, and how thick it dries.
const PLATES: (f64, f64) = (0.12, 0.3);
const THICK: (f64, f64) = (0.006, 0.025);
/// How steeply a dried face's fine grain stands, and how many of its
/// grains span a metre.
const GRAIN: (f64, f64) = (0.1, 250.0);

/// How much paler than the land's silt its mud's face bleaches, and how much
/// darker than its earth the damp mud beneath stays.
const BLEACHED: f64 = 1.15;
const DAMP: f64 = 0.8;

/// How upright ground must stand for its mud to dry as a crust.
const LEVEL_GROUND: f64 = 0.99;

/// Whether ground lying as `lie` is a level wash of silt bare of growth,
/// where a flash flood's water stood and dried.
pub(super) fn wash(lie: &Lie) -> bool {
    lie.upright > LEVEL_GROUND && lie.green < 0.35 && lie.sediment > 0.05
}

/// Whether ground lying as `lie` is a level, bare bank of silt the water has
/// fallen from over a dry season, damp still beneath.
pub(super) fn bank(lie: &Lie) -> bool {
    lie.upright > LEVEL_GROUND
        && (0.3..0.85).contains(&lie.wet)
        && lie.sediment > 0.1
        && lie.green < 0.3
}

/// Lay the mud dried and cracked about `eye` on `land`, out to the detail's
/// reach, wherever `dried` says its ground is level silt the water left, in
/// its ground's own silt and earth. One draw of `dice` keys it, the same at
/// either detail. `None` when the stage will not hold it.
pub(super) fn crack(
    stage: &mut Stage,
    dice: &mut Dice,
    (land, eye): (&Land, Vec3),
    dried: &dyn Fn(&Lie) -> bool,
) -> Option<()> {
    let mut dice = Dice::keyed(dice.wide(), 0);
    let seed = dice.seed();
    let Some(ground) = ground_of(stage, land) else {
        return Some(());
    };
    let drying = (ground.palette.silt * BLEACHED, ground.palette.earth * DAMP);
    let plates = mathf::round_i32(TILE / dice.range(PLATES.0, PLATES.1));
    let crust = Crust {
        side: TILE,
        plates: u16::try_from(plates).ok()?,
        thick: dice.range(THICK.0, THICK.1),
    };
    let reach = stage.densities.mud;
    let cells = mathf::round_i32(reach / TILE) + 1;
    let (column0, row0) = (snap(eye.x, TILE), snap(eye.z, TILE));
    let mut material = None;
    let mut planned = [None; VARIANTS];
    for row in -cells..=cells {
        for column in -cells..=cells {
            let (x, z) = (
                column0 + (f64::from(column) + 0.5) * TILE,
                row0 + (f64::from(row) + 0.5) * TILE,
            );
            if mathf::hypot(x - eye.x, z - eye.z) > reach {
                continue;
            }
            let Some((base, normal)) = dry_flat((land, stage), (x, z), (seed, dried)) else {
                continue;
            };
            let material = if let Some(material) = material {
                material
            } else {
                let made = mud_material(stage, drying, seed)?;
                material = Some(made);
                made
            };
            let key = hash2(column.cast_unsigned(), row.cast_unsigned(), seed ^ 0x6d75);
            let variant = usize::try_from(key).ok()? % VARIANTS;
            let prototype = match planned.get(variant).copied().flatten() {
                Some(prototype) => prototype,
                None if stage.plannable() > 0 => {
                    let prototype = stage.plan(&Recipe::Mud {
                        crust,
                        material,
                        seed,
                        variant: u32::try_from(variant).ok()?,
                    })?;
                    *planned.get_mut(variant)? = Some(prototype);
                    prototype
                }
                None => continue,
            };
            let pose = Pose::new(base, Frame::WORLD.aligning(Vec3::UP, normal));
            stage.add(
                Shape::Instance {
                    prototype,
                    pose,
                    scale: 1.0,
                    key,
                },
                usize::from(material),
                pose,
                false,
            )?;
        }
    }
    Some(())
}

/// Where on `land` a tile at `(x, z)` lies, and the way its ground faces, if
/// a crust dried there: on ground `dried` says is silt the water left, out of
/// any standing water, in one of the patches drawn under `seed`, level
/// enough across the tile to lie as one crust, and clear of what stands on
/// `stage`.
fn dry_flat(
    (land, stage): (&Land, &Stage),
    (x, z): (f64, f64),
    (seed, dried): (u32, &dyn Fn(&Lie) -> bool),
) -> Option<(Vec3, Vec3)> {
    let fields = &stage.fields;
    let lie = land.grids.lie(fields, x, z);
    if !dried(&lie) || land.grids.wet_at(fields, x, z) {
        return None;
    }
    let patch = 0.5 + 0.5 * fbm2(x / PATCHES, z / PATCHES, seed ^ 0x9a7c, (3, 0.5, 2.0));
    if patch < 1.0 - COVER {
        return None;
    }
    let normal = land.grids.normal(fields, x, z);
    let half = 0.5 * TILE;
    for (dx, dz) in [(-half, -half), (half, -half), (half, half), (-half, half)] {
        let plane = lie.height - (normal.x * dx + normal.z * dz) / normal.y.max(1e-6);
        let ground = land.grids.height(fields, x + dx, z + dz);
        if (ground - plane).abs() > LEVEL || land.grids.wet_at(fields, x + dx, z + dz) {
            return None;
        }
    }
    stage
        .clear_of_pieces((x, z), FRAC_1_SQRT_2 * TILE)
        .then_some((Vec3::new(x, lie.height, z), normal))
}

/// The material a land's mud dries in, its face `dry` and the mud beneath
/// `damp`: matte, finely grained under `seed`, every plate its own shade.
fn mud_material(stage: &mut Stage, (dry, damp): (Vec3, Vec3), seed: u32) -> Option<u16> {
    let material = stage.material(
        Material::new(Pigment::Mud(Mud { dry, damp }), Finish::Matte)
            .with_relief(Relief::grain(GRAIN.0, GRAIN.1, seed)),
    )?;
    u16::try_from(material).ok()
}

#[cfg(test)]
#[path = "cracked_tests.rs"]
mod tests;
