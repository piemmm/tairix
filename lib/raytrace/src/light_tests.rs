//! Host tests of how each light is sampled.

use core::f64::consts::PI;

use tairix_util::mathf;

use super::Light;
use crate::shape::{Geometry, Shape};
use crate::vector::{Ray, Vec3};

/// A stratified grid of `side × side` pairs over the unit square.
fn grid(side: u32) -> impl Iterator<Item = (f64, f64)> {
    (0..side * side).map(move |at| {
        (
            (f64::from(at % side) + 0.5) / f64::from(side),
            (f64::from(at / side) + 0.5) / f64::from(side),
        )
    })
}

#[test]
fn a_point_falls_off_as_the_square_of_its_distance() {
    let lamp = Light::Point {
        at: Vec3::new(0.0, 4.0, 0.0),
        intensity: Vec3::splat(8.0),
    };
    let near = lamp
        .sample(Vec3::new(0.0, 2.0, 0.0), (0.5, 0.5))
        .expect("lit");
    let far = lamp.sample(Vec3::ZERO, (0.5, 0.5)).expect("lit");
    assert!((near.light.x - 2.0).abs() < 1e-12 && (far.light.x - 0.5).abs() < 1e-12);
    assert!((near.dir - Vec3::UP).length() < 1e-12 && (near.distance - 2.0).abs() < 1e-12);
    assert!(near.density <= 0.0, "nothing else finds a point");
    assert!(lamp.density(Vec3::ZERO, Vec3::UP, 4.0) <= 0.0);
}

#[test]
fn a_spot_lights_its_cone_alone_and_softens_at_the_edge() {
    let spot = Light::Spot {
        at: Vec3::new(0.0, 5.0, 0.0),
        axis: -Vec3::UP,
        cos_inner: mathf::cos(0.2),
        cos_outer: mathf::cos(0.4),
        intensity: Vec3::splat(25.0),
    };
    let centre = spot.sample(Vec3::ZERO, (0.5, 0.5)).expect("in the cone");
    assert!((centre.light.x - 1.0).abs() < 1e-12);
    let edge = spot
        .sample(Vec3::new(5.0 * mathf::tan(0.3), 0.0, 0.0), (0.5, 0.5))
        .expect("penumbra");
    assert!(edge.light.x > 0.0 && edge.light.x < centre.light.x);
    assert!(spot.sample(Vec3::new(5.0, 0.0, 0.0), (0.5, 0.5)).is_none());
}

#[test]
fn the_sun_is_sampled_within_its_disc_at_its_own_density() {
    let toward = Vec3::new(0.3, 0.8, -0.2).normalized();
    let cos_radius = mathf::cos(0.02);
    let sun = Light::Sun {
        toward,
        cos_radius,
        radiance: Vec3::splat(1000.0),
    };
    for pair in grid(12) {
        let incidence = sun.sample(Vec3::ZERO, pair).expect("the sun always shines");
        assert!(incidence.dir.dot(toward) >= cos_radius - 1e-12);
        assert!(incidence.distance.is_infinite());
        // The estimate the sample makes of the sun's light is its radiance
        // over its solid angle, whichever sample it is.
        let solid = 2.0 * PI * (1.0 - cos_radius);
        assert!((incidence.light.x * incidence.density - 1000.0).abs() < 1e-6);
        assert!((incidence.density * solid - 1.0).abs() < 1e-9);
        assert!(
            (sun.density(Vec3::ZERO, incidence.dir, f64::INFINITY) - incidence.density).abs()
                < 1e-9
        );
    }
    assert!(sun.density(Vec3::ZERO, -toward, f64::INFINITY) <= 0.0);
}

#[test]
fn an_orb_is_sampled_on_its_near_side_and_lights_as_its_solid_angle_says() {
    let (centre, radius) = (Vec3::new(0.0, 3.0, 0.0), 0.5);
    let orb = Light::Orb {
        object: 0,
        centre,
        radius,
        radiance: Vec3::splat(10.0),
    };
    let ball = Shape::Sphere { centre, radius };
    let p = Vec3::ZERO;
    let mut irradiance = 0.0;
    let side = 48u32;
    for pair in grid(side) {
        let incidence = orb.sample(p, pair).expect("in sight");
        let hit = ball
            .intersect(
                &Ray::new(p, incidence.dir),
                1e-9,
                f64::INFINITY,
                Geometry {
                    faces: &[],
                    fields: &[],
                },
            )
            .expect("every sampled direction meets the orb");
        assert!(
            (hit.t - incidence.distance).abs() < 1e-6,
            "{} against {}",
            hit.t,
            incidence.distance
        );
        assert!((orb.density(p, incidence.dir, hit.t) - incidence.density).abs() < 1e-9);
        irradiance += incidence.light.x * incidence.dir.dot(Vec3::UP);
    }
    irradiance /= f64::from(side * side);
    // A sphere of uniform radiance L seen overhead lights a level surface
    // with π L sin²θ, θ its angular radius.
    let expected = PI * 10.0 * (radius * radius) / (3.0 * 3.0);
    assert!(
        (irradiance - expected).abs() < expected * 0.01,
        "{irradiance} against {expected}"
    );
    assert!(
        orb.sample(centre, (0.5, 0.5)).is_none(),
        "nothing is lit from inside a lamp"
    );
}

#[test]
fn a_panel_lights_its_face_alone() {
    let panel = Light::Panel {
        object: 0,
        corner: Vec3::new(-1.0, 4.0, -1.0),
        edge_u: Vec3::new(2.0, 0.0, 0.0),
        edge_v: Vec3::new(0.0, 0.0, 2.0),
        radiance: Vec3::splat(5.0),
    };
    // Facing `edge_u × edge_v`, which points down onto the floor.
    for pair in grid(8) {
        let incidence = panel.sample(Vec3::ZERO, pair).expect("beneath its face");
        assert!(incidence.dir.y > 0.0);
        let dir = incidence.dir;
        assert!(
            (panel.density(Vec3::ZERO, dir, incidence.distance) - incidence.density).abs() < 1e-9
        );
    }
    assert!(
        panel.sample(Vec3::new(0.0, 8.0, 0.0), (0.5, 0.5)).is_none(),
        "its back is dark"
    );
    assert!((panel.seen(Vec3::UP) - Vec3::splat(5.0)).length() < 1e-12);
    assert!(panel.seen(-Vec3::UP).max_element() <= 0.0);
    assert_eq!(panel.object(), Some(0));
}
