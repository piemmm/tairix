//! Stones a scene strews: a few rocks grown from seeds of their own in the
//! region's rock, each placed as often as the ground takes it — sized,
//! turned, tilted to the ground it lies on and bedded into it.

use core::f64::consts::TAU;

use super::{Dice, Recipe, Stage};
use crate::ground::Rock;
use crate::material::{Finish, Material, Relief};
use crate::pigment::Pigment;
use crate::rock::Habit;
use crate::shape::Shape;
use crate::vector::{Frame, Pose, Vec3};

/// How many rocks a scene grows to choose among.
const KINDS: usize = 4;

/// A scene's rocks, planned.
#[derive(Copy, Clone, Debug)]
pub(super) struct Stones {
    kinds: [(u32, f64); KINDS],
    material: usize,
}

impl Stones {
    /// Rocks of `rock`, planned to be grown before the scene is traced;
    /// `None` when the heap will not hold them.
    pub(super) fn new(stage: &mut Stage, dice: &mut Dice, rock: Rock) -> Option<Self> {
        let material = stage.material(
            Material::new(
                Pigment::Rock(Rock {
                    seed: dice.seed(),
                    ..rock
                }),
                Finish::Coated { roughness: 0.82 },
            )
            .with_relief(Relief::Grain {
                depth: 0.12,
                scale: 14.0,
                seed: dice.seed(),
            }),
        )?;
        let stock = u16::try_from(material).ok()?;
        let mut kinds = [(0, 0.0); KINDS];
        for kind in &mut kinds {
            let habit = Habit {
                squash: dice.range(0.45, 0.85),
                fractures: dice.count(1, 6),
            };
            let recipe = Recipe::Rock {
                habit,
                stock,
                seed: dice.wide(),
            };
            *kind = (stage.plan(&recipe)?, habit.squash);
        }
        Some(Self { kinds, material })
    }

    /// A stone `size` across lying on the ground at `base`, which faces
    /// `normal` there: tilted part way to it, turned any way, and a third of
    /// its height bedded in.
    pub(super) fn lay(
        &self,
        stage: &mut Stage,
        dice: &mut Dice,
        (base, normal): (Vec3, Vec3),
        size: f64,
    ) -> Option<()> {
        let (prototype, squash) = dice.pick(&self.kinds)?;
        let scale = 0.5 * size;
        let upright = Vec3::UP.lerp(normal, 0.6).normalized();
        let frame = Frame::turned(dice.range(0.0, TAU), dice.range(-0.15, 0.15))
            .aligning(Vec3::UP, upright);
        let middle = base + upright * (0.35 * squash * scale);
        let pose = Pose::new(middle, frame);
        stage.add(
            Shape::Instance {
                prototype,
                pose,
                scale,
                key: dice.seed(),
            },
            self.material,
            pose,
            false,
        )?;
        Some(())
    }
}
