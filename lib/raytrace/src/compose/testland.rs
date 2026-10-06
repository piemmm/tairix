//! A land for the composition's host tests: one grid 400 m across, its
//! height and what lies on it as a test asks, laid in a ground of its own.

use super::Stage;
use crate::ground::{Ground, Palette};
use crate::heightfield::{Attributes, Heightfield};
use crate::land::Land;
use crate::material::{Finish, Material};
use crate::pigment::Pigment;
use crate::shape::Shape;
use crate::vector::{Frame, Pose, Vec3};

/// A land of one grid 400 m across whose height `profile` gives, carrying
/// `lie` everywhere, on `stage`.
pub(super) fn land(stage: &mut Stage, profile: &dyn Fn(f64, f64) -> f64, lie: Attributes) -> Land {
    let mut field = Heightfield::new(400, (-200.0, -200.0), 1.0, false).expect("a grid");
    let side = field.side();
    assert!(field.carry_attributes());
    {
        let (heights, attributes) = field.rows_mut(0..side);
        for (index, slot) in heights.iter_mut().enumerate() {
            let (column, row) = (index % side, index / side);
            *slot = crate::vector::single(profile(
                -200.0 + crate::vector::real(column),
                -200.0 + crate::vector::real(row),
            ));
        }
        attributes.fill(lie);
    }
    field.seal();
    stage.fields.push(field);
    Land::plain(0, ((0.0, 0.0), 200.0))
}

/// The ground of the land on `stage`, as a stage holds a land's: a material
/// the land's own grid is laid in, whose rock its stones are of and whose
/// silt its mud dries to.
pub(super) fn grounded(stage: &mut Stage) {
    let palette = Palette {
        grass: Vec3::splat(0.2),
        dry: Vec3::splat(0.3),
        moss: Vec3::splat(0.2),
        earth: Vec3::splat(0.3),
        silt: Vec3::splat(0.4),
        rock: Vec3::splat(0.4),
        strata: Vec3::splat(0.3),
        lichen: Vec3::splat(0.5),
        sand: Vec3::splat(0.6),
        snow: Vec3::ONE,
    };
    let ground = Ground {
        palette,
        shore: -1e3,
        snow_line: 1e5,
        cliff: 0.6,
        bedding: 2.0,
        seed: 1,
        ways: None,
        bounds: None,
        floor: None,
    };
    let material = stage
        .material(Material::new(Pigment::Ground(ground), Finish::Matte))
        .expect("a material");
    stage
        .add(
            Shape::Land { field: 0 },
            material,
            Pose::new(Vec3::ZERO, Frame::WORLD),
            false,
        )
        .expect("the land");
}
