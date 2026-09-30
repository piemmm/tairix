//! Host tests of a scene's two queries: the nearest object along a ray, and
//! the light that gets through along one.

use alloc::vec;
use alloc::vec::Vec;

use super::{Draft, Grid, Object, Parts, Scene, Sight, Target};
use crate::camera::Camera;
use crate::compose::Setting;
use crate::material::{Finish, Material};
use crate::pigment::Pigment;
use crate::shape::Shape;
use crate::sky::Sky;
use crate::vector::{Frame, Pose, Ray, Vec3};

fn object(shape: Shape, material: usize, filter: Option<Vec3>) -> Object {
    Object {
        shape,
        material,
        texture: Pose::new(Vec3::ZERO, Frame::WORLD),
        light: None,
        filter,
        in_view: true,
    }
}

fn scene(objects: Vec<Object>) -> Scene {
    Scene::new(Parts {
        objects,
        faces: Vec::new(),
        fields: Vec::new(),
        materials: vec![
            Material::new(
                Pigment::Solid(Vec3::splat(0.5)),
                Finish::Coated { roughness: 0.5 },
            ),
            Material::new(
                Pigment::Solid(Vec3::ONE),
                Finish::Glass {
                    ior: 1.5,
                    absorb: Vec3::new(0.5, 0.2, 0.1),
                    glow: Vec3::ZERO,
                    roughness: 0.0,
                    dispersion: 0.0,
                    foam: None,
                },
            ),
        ],
        lights: Vec::new(),
        sky: Sky {
            zenith: Vec3::ONE,
            horizon: Vec3::ONE,
            ground: Vec3::ONE,
            glow: None,
            stars: 0.0,
            clouds: None,
        },
        fog: None,
        camera: Camera::looking(Vec3::new(0.0, 1.0, -5.0), Vec3::ZERO, 0.8, 1.0, (0.0, 1.0)),
        exposure: 1.0,
        bounce: Vec3::ZERO,
        fills: Vec::new(),
    })
    .expect("a scene")
}

fn ball(z: f64, radius: f64) -> Shape {
    Shape::Sphere {
        centre: Vec3::new(0.0, 0.0, z),
        radius,
    }
}

#[test]
fn the_nearest_object_is_found_among_bounded_and_endless_ones() {
    let ground = Shape::Plane {
        normal: Vec3::new(0.0, 0.0, -1.0),
        offset: -8.0,
    };
    let scene = scene(vec![
        object(ball(6.0, 1.0), 0, None),
        object(ground, 0, None),
        object(ball(3.0, 0.5), 0, None),
    ]);
    let ray = Ray::new(Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0));
    let (index, hit) = scene
        .closest(&ray, f64::INFINITY, Sight::Bounce)
        .expect("a hit");
    assert_eq!(index, 2);
    assert!((hit.t - 2.5).abs() < 1e-9);
    assert!(
        scene.closest(&ray, 2.0, Sight::Bounce).is_none(),
        "nothing nearer than the reach"
    );
    let aside = Ray::new(Vec3::new(5.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0));
    let (index, hit) = scene
        .closest(&aside, f64::INFINITY, Sight::Bounce)
        .expect("the wall");
    assert_eq!(index, 1);
    assert!((hit.t - 8.0).abs() < 1e-9);
}

#[test]
fn light_is_stopped_by_an_opaque_object_and_tinted_by_a_clear_one() {
    let tint = Vec3::new(0.8, 0.6, 0.4);
    let scene = scene(vec![
        object(ball(3.0, 0.5), 1, Some(tint)),
        object(ball(6.0, 0.5), 0, None),
    ]);
    let ray = Ray::new(Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0));
    let through_glass = scene.transmittance(&ray, 5.0, None);
    assert!((through_glass - tint).length() < 1e-12);
    assert!(scene.transmittance(&ray, 10.0, None).max_element() <= 0.0);
    assert!((scene.transmittance(&ray, 2.0, None) - Vec3::ONE).length() < 1e-12);
}

/// A lamp kept out of the picture is passed through by the eye alone: a
/// reflection, and the light it casts, still find it.
#[test]
fn the_eye_looks_through_a_lamp_kept_out_of_the_picture() {
    let mut hidden = object(ball(3.0, 0.5), 0, None);
    hidden.in_view = false;
    let scene = scene(vec![hidden, object(ball(6.0, 1.0), 0, None)]);
    let ray = Ray::new(Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0));
    let (seen, _) = scene
        .closest(&ray, f64::INFINITY, Sight::Eye)
        .expect("the ball behind");
    assert_eq!(seen, 1);
    let (bounced, _) = scene
        .closest(&ray, f64::INFINITY, Sight::Bounce)
        .expect("the lamp");
    assert_eq!(bounced, 0);
    assert!(
        scene.transmittance(&ray, 4.0, None).max_element() <= 0.0,
        "it still casts its shadow"
    );
}

/// Starting inside a clear medium, light is absorbed until it leaves it.
#[test]
fn light_starting_in_a_medium_is_absorbed_until_it_leaves() {
    let sea = Shape::Plane {
        normal: Vec3::UP,
        offset: 0.0,
    };
    let filter = Vec3::splat(0.9);
    let scene = scene(vec![object(sea, 1, Some(filter))]);
    let absorb = Vec3::new(0.5, 0.2, 0.1);
    let up = Ray::new(Vec3::new(0.0, -2.0, 0.0), Vec3::UP);
    let kept = scene.transmittance(&up, f64::INFINITY, Some(absorb));
    let expected = filter * (absorb * -2.0).exp();
    assert!((kept - expected).length() < 1e-12, "{kept:?}");
}

/// A draft fills its grids a band of rows at a time, never more than it is
/// asked for, and a finished scene is the same however the rows were taken.
#[test]
fn a_draft_fills_its_grids_in_bands_and_finishes_the_same_scene_either_way() {
    let aspect = 16.0 / 9.0;
    let mut stepped = Draft::new(Setting::Meadow, 3, aspect).expect("a draft");
    let total = stepped.remaining();
    assert!(
        total > 1_000_000,
        "a meadow's land and cloud take {total} vertices"
    );
    assert!(
        stepped
            .parts
            .fills
            .iter()
            .any(|fill| fill.target == Target::Clouds)
            || stepped.parts.sky.clouds.is_none()
    );
    // Whole rows, so a call may run past its count by less than one of the
    // widest grid's rows.
    let widest = stepped
        .parts
        .fields
        .iter()
        .map(Grid::rows)
        .max()
        .unwrap_or(0);
    let mut left = total;
    while left > 0 {
        let next = stepped.prepare(&tairix_parallel::SERIAL, 5000);
        let done = (left - next) as usize;
        assert!(next < left && done < 5000 + widest, "{left} then {next}");
        left = next;
    }
    assert_eq!(
        stepped.prepare(&tairix_parallel::SERIAL, 50),
        0,
        "nothing left to fill"
    );
    let at_once = Draft::new(Setting::Meadow, 3, aspect)
        .expect("a draft")
        .finish()
        .expect("a scene");
    let stepped = stepped.finish().expect("a scene");
    assert_eq!(stepped.fields.len(), at_once.fields.len());
    let ray = Ray::new(
        Vec3::new(0.0, 400.0, 0.0),
        Vec3::new(0.3, -0.5, 0.2).normalized(),
    );
    for (a, b) in stepped.fields.iter().zip(&at_once.fields) {
        for step in 0..200 {
            let (x, z) = (
                f64::from(step) * 7.3 - 700.0,
                f64::from(step) * -5.1 + 500.0,
            );
            assert_eq!(a.height_at(x, z).to_bits(), b.height_at(x, z).to_bits());
        }
        assert_eq!(
            a.intersect(&ray, 1e-9, f64::INFINITY).map(|hit| hit.t),
            b.intersect(&ray, 1e-9, f64::INFINITY).map(|hit| hit.t)
        );
    }
}

/// Filled across a pool of workers, a draft's grids come out as they do
/// filled on the one thread.
#[test]
fn a_draft_filled_across_workers_matches_one_filled_alone() {
    let pool = tairix_parallel::Threaded::new(3);
    let aspect = 4.0 / 3.0;
    let mut spread = Draft::new(Setting::Coast, 11, aspect).expect("a draft");
    while spread.prepare(&pool, 40_000) > 0 {}
    let spread = spread.finish().expect("a scene");
    let alone = Draft::new(Setting::Coast, 11, aspect)
        .expect("a draft")
        .finish()
        .expect("a scene");
    for (a, b) in spread.fields.iter().zip(&alone.fields) {
        for step in 0..300 {
            let (x, z) = (f64::from(step) * 3.7 - 500.0, f64::from(step) * 2.9 - 400.0);
            assert_eq!(a.height_at(x, z).to_bits(), b.height_at(x, z).to_bits());
        }
    }
    let dir = Vec3::new(0.2, 0.3, 0.9).normalized();
    assert_eq!(
        spread.sky.radiance(Vec3::ZERO, dir, true),
        alone.sky.radiance(Vec3::ZERO, dir, true),
        "the cloud cover too"
    );
}
