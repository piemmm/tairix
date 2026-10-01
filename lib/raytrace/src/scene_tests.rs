//! Host tests of a scene's two queries: the nearest object along a ray, and
//! the light that gets through along one.

use alloc::vec;
use alloc::vec::Vec;

use super::{Draft, Exposure, Object, Parts, Scene, Sight};
use crate::camera::Camera;
use crate::compose::Setting;
use crate::material::{Finish, Material};
use crate::pigment::Pigment;
use crate::shape::Shape;
use crate::sky::{Dome, Gradient, Sky};
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
    Scene::new(parts(objects)).expect("a scene")
}

fn parts(objects: Vec<Object>) -> Parts {
    Parts {
        objects,
        faces: Vec::new(),
        fields: Vec::new(),
        prototypes: Vec::new(),
        lawns: Vec::new(),
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
            dome: Dome::Gradient(Gradient {
                zenith: Vec3::ONE,
                horizon: Vec3::ONE,
                ground: Vec3::ONE,
                glow: None,
            }),
            stars: 0.0,
            clouds: None,
            bank: None,
        },
        fog: None,
        shades: None,
        camera: Camera::looking(Vec3::new(0.0, 1.0, -5.0), Vec3::ZERO, 0.8, 1.0, (0.0, 1.0)),
        exposure: Exposure::Fixed(1.0),
        daylight: 1.0,
    }
}

/// A forest's worth of small spheres over a square a kilometre across.
fn crowd() -> Vec<Object> {
    (0..40_000u32)
        .map(|index| {
            let (x, z) = (
                f64::from(index % 200) * 5.0 - 500.0,
                f64::from(index / 200) * 5.0 - 500.0,
            );
            object(
                Shape::Sphere {
                    centre: Vec3::new(x, 1.0, z),
                    radius: 1.0,
                },
                0,
                None,
            )
        })
        .collect()
}

/// A scene of tens of thousands of objects builds its hierarchy a bounded
/// share a step, and meets every ray as one built at once does.
#[test]
fn a_large_scenes_hierarchy_builds_a_step_at_a_time_and_meets_rays_as_one_built_whole() {
    let mut building = Scene::building(parts(crowd())).expect("a building");
    let mut steps = 1;
    while !building.step() {
        steps += 1;
    }
    assert!(steps > 2, "built in {steps} steps");
    let stepped = building.finish();
    let whole = scene(crowd());
    for index in 0..500u32 {
        let t = f64::from(index);
        let ray = Ray::new(
            Vec3::new(
                -520.0,
                1.0 + 0.3 * tairix_util::mathf::sin(t * 0.37),
                -500.0 + 2.0 * t,
            ),
            Vec3::new(1.0, -0.0005 * t, 0.2).normalized(),
        );
        let (a, b) = (
            stepped.closest(&ray, f64::INFINITY, Sight::Eye),
            whole.closest(&ray, f64::INFINITY, Sight::Eye),
        );
        assert_eq!(
            a.map(|(object, hit)| (object, hit.t.to_bits())),
            b.map(|(object, hit)| (object, hit.t.to_bits())),
            "ray {index}"
        );
    }
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

/// The picture a draft is prepared for.
const SIZE: (u32, u32) = (96, 54);

/// What a finished scene shows, sampled: its grids' heights, a ray over its
/// land, its sky, and its exposure.
fn fingerprint(scene: &Scene) -> Vec<u64> {
    let mut marks = Vec::new();
    for field in &scene.fields {
        for step in 0..200 {
            let (x, z) = (
                f64::from(step) * 7.3 - 700.0,
                f64::from(step) * -5.1 + 500.0,
            );
            marks.push(field.height_at(x, z).to_bits());
        }
    }
    let ray = Ray::new(
        Vec3::new(0.0, 400.0, 0.0),
        Vec3::new(0.3, -0.5, 0.2).normalized(),
    );
    if let Some((index, hit)) = scene.closest(&ray, f64::INFINITY, Sight::Eye) {
        marks.push(index as u64);
        marks.push(hit.t.to_bits());
    }
    for step in 0..24u32 {
        let dir = Vec3::new(f64::from(step) * 0.1 - 1.0, 0.3, 0.9).normalized();
        let light = scene.sky.radiance(Vec3::ZERO, dir, true, 0.5);
        marks.extend([light.x.to_bits(), light.y.to_bits(), light.z.to_bits()]);
    }
    marks.push(scene.exposure.to_bits());
    marks.push(scene.prototypes.len() as u64);
    marks
}

/// A draft does its work a unit at a time, however soon its caller's time is
/// spent, and the scene it finishes is the one a draft prepared at once
/// finishes.
#[test]
fn a_draft_prepared_a_unit_at_a_time_finishes_the_scene_prepared_at_once() {
    let mut stepped = Draft::new(Setting::Meadow, 3, SIZE).expect("a draft");
    let mut calls = 0;
    while !stepped
        .prepare(&tairix_parallel::SERIAL, &mut || true)
        .expect("prepared")
    {
        calls += 1;
    }
    assert!(calls > 20, "a meadow's work takes {calls} units");
    let stepped = stepped.finish().expect("a scene");
    let at_once = Draft::new(Setting::Meadow, 3, SIZE)
        .expect("a draft")
        .finish()
        .expect("a scene");
    assert_eq!(fingerprint(&stepped), fingerprint(&at_once));
}

/// Prepared across a pool of workers, a draft's scene comes out as it does
/// prepared on the one thread.
#[test]
fn a_draft_prepared_across_workers_matches_one_prepared_alone() {
    let pool = tairix_parallel::Threaded::new(3);
    let mut spread = Draft::new(Setting::Coast, 11, SIZE).expect("a draft");
    while !spread.prepare(&pool, &mut || false).expect("prepared") {}
    let spread = spread.finish().expect("a scene");
    let alone = Draft::new(Setting::Coast, 11, SIZE)
        .expect("a draft")
        .finish()
        .expect("a scene");
    assert_eq!(fingerprint(&spread), fingerprint(&alone));
}

/// A picture of no pixels has no scene.
#[test]
fn a_picture_with_no_pixels_has_no_draft() {
    assert!(Draft::new(Setting::Meadow, 1, (0, 10)).is_none());
    assert!(Draft::new(Setting::Meadow, 1, (10, 0)).is_none());
}
