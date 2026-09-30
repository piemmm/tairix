//! Host tests of the integrator, on small scenes whose answers are known.

use alloc::vec::Vec;

use tairix_util::mathf;

use super::{power, Quality, Tracer};
use crate::camera::Camera;
use crate::light::Light;
use crate::material::{Finish, Material, COAT_F0};
use crate::pigment::Pigment;
use crate::scene::{Object, Parts, Scene};
use crate::shape::Shape;
use crate::sky::Sky;
use crate::tone::{filmic, Encoder};
use crate::vector::{Frame, Pose, Vec3};

const SIZE: (u32, u32) = (9, 9);
const MIDDLE: (u32, u32) = (4, 4);

fn uniform_sky(level: f64) -> Sky {
    Sky {
        zenith: Vec3::splat(level),
        horizon: Vec3::splat(level),
        ground: Vec3::splat(level),
        glow: None,
        stars: 0.0,
        clouds: None,
    }
}

struct Setup {
    objects: Vec<Object>,
    materials: Vec<Material>,
    lights: Vec<Light>,
    sky: Sky,
    eye: Vec3,
    target: Vec3,
}

impl Setup {
    fn new(sky: Sky) -> Self {
        Self {
            objects: Vec::new(),
            materials: Vec::new(),
            lights: Vec::new(),
            sky,
            eye: Vec3::new(0.0, 0.0, -5.0),
            target: Vec3::ZERO,
        }
    }

    fn add(&mut self, shape: Shape, material: Material, filter: Option<Vec3>) -> usize {
        self.materials.push(material);
        self.objects.push(Object {
            shape,
            material: self.materials.len() - 1,
            texture: Pose::new(Vec3::ZERO, Frame::WORLD),
            light: None,
            filter,
            in_view: true,
        });
        self.objects.len() - 1
    }

    fn scene(self) -> Scene {
        Scene::new(Parts {
            objects: self.objects,
            faces: Vec::new(),
            fields: Vec::new(),
            materials: self.materials,
            lights: self.lights,
            sky: self.sky,
            fog: None,
            camera: Camera::looking(self.eye, self.target, 0.3, 1.0, (0.0, 1.0)),
            exposure: 1.0,
            bounce: Vec3::ZERO,
            fills: Vec::new(),
        })
        .expect("a scene")
    }
}

fn ball(centre: Vec3, radius: f64) -> Shape {
    Shape::Sphere { centre, radius }
}

fn ground() -> Shape {
    Shape::Plane {
        normal: Vec3::UP,
        offset: 0.0,
    }
}

/// What pixel `at` shows as display-linear light, and how many samples it
/// took, at the finest quality.
fn shown(scene: &Scene, at: (u32, u32)) -> (Vec3, u32) {
    let encoder = Encoder::new().expect("an encoder");
    Tracer::new(scene, &encoder, SIZE, 0x5eed).light(at, Quality::Fine)
}

fn glass() -> Finish {
    Finish::Glass {
        ior: 1.5,
        absorb: Vec3::ZERO,
        glow: Vec3::ZERO,
        roughness: 0.0,
        dispersion: 0.0,
        foam: None,
    }
}

fn close(a: Vec3, b: Vec3, tolerance: f64) -> bool {
    let apart = a - b;
    apart.x.abs().max(apart.y.abs()).max(apart.z.abs()) <= tolerance
}

/// Under a sky of one radiance everywhere, a smooth coated surface shows
/// the sky through its coat and its pigment under it, in the Fresnel share
/// of each.
#[test]
fn a_coated_surface_under_an_even_sky_shows_its_pigment_and_the_sky() {
    let sky = 0.6;
    let pigment = Vec3::new(0.7, 0.4, 0.2);
    let mut setup = Setup::new(uniform_sky(sky));
    setup.add(
        ground(),
        Material::new(Pigment::Solid(pigment), Finish::Coated { roughness: 0.0 }),
        None,
    );
    setup.eye = Vec3::new(0.0, 5.0, -0.5);
    let cos = 5.0 / mathf::sqrt(25.25);
    let off = 1.0 - cos;
    let coat = COAT_F0 + (1.0 - COAT_F0) * off * off * off * off * off;
    let expected = (pigment * (1.0 - coat) + Vec3::splat(coat)) * sky;
    let (light, _) = shown(&setup.scene(), MIDDLE);
    let toned = Vec3::new(filmic(expected.x), filmic(expected.y), filmic(expected.z));
    assert!(close(light, toned, 0.01), "{light:?} against {toned:?}");
}

/// A perfect mirror under an even sky shows the sky exactly; so does clear
/// glass, which neither makes light nor loses it.
#[test]
fn a_mirror_and_clear_glass_show_an_even_sky_unchanged() {
    for finish in [Finish::Metal { roughness: 0.0 }, glass()] {
        let mut setup = Setup::new(uniform_sky(0.5));
        let clear = matches!(finish, Finish::Glass { .. }).then_some(Vec3::ONE);
        setup.add(
            ball(Vec3::ZERO, 1.0),
            Material::new(Pigment::Solid(Vec3::ONE), finish.clone()),
            clear,
        );
        let (light, _) = shown(&setup.scene(), MIDDLE);
        let expected = Vec3::splat(filmic(0.5));
        assert!(
            close(light, expected, 0.01),
            "{finish:?}: {light:?} against {expected:?}"
        );
    }
}

#[test]
fn a_lamp_seen_directly_shows_its_own_radiance() {
    let radiance = Vec3::new(0.3, 0.9, 1.5);
    let mut setup = Setup::new(uniform_sky(0.0));
    let orb = setup.add(
        ball(Vec3::ZERO, 1.0),
        Material::new(Pigment::Solid(Vec3::ZERO), Finish::Glow { radiance }),
        None,
    );
    setup.lights.push(Light::Orb {
        object: 0,
        centre: Vec3::ZERO,
        radius: 1.0,
        radiance,
    });
    setup.objects[orb].light = Some(0);
    let (light, _) = shown(&setup.scene(), MIDDLE);
    let expected = Vec3::new(filmic(radiance.x), filmic(radiance.y), filmic(radiance.z));
    assert!(close(light, expected, 1e-9), "{light:?}");
}

/// A shadow is dark behind an opaque ball and lighter behind a clear one.
#[test]
fn a_clear_ball_casts_a_lighter_shadow_than_an_opaque_one() {
    let floor = || {
        Material::new(
            Pigment::Solid(Vec3::splat(0.8)),
            Finish::Coated { roughness: 1.0 },
        )
    };
    let lit_floor = |blocker: Option<(Material, Option<Vec3>)>| {
        let mut setup = Setup::new(uniform_sky(0.0));
        setup.add(ground(), floor(), None);
        if let Some((material, filter)) = blocker {
            setup.add(ball(Vec3::new(0.0, 1.0, 0.0), 0.6), material, filter);
        }
        setup.lights.push(Light::Point {
            at: Vec3::new(0.0, 3.0, 0.0),
            intensity: Vec3::splat(9.0),
        });
        // Looking at the floor just beside the ball, where its shadow falls.
        setup.eye = Vec3::new(0.0, 0.5, -3.0);
        setup.target = Vec3::new(0.0, 0.0, -0.1);
        shown(&setup.scene(), MIDDLE).0.x
    };
    let open = lit_floor(None);
    let opaque = lit_floor(Some((floor(), None)));
    let glass = lit_floor(Some((
        Material::new(Pigment::Solid(Vec3::ONE), glass()),
        Some(Vec3::splat(0.55)),
    )));
    assert!(opaque < 0.05 * open, "{opaque} against {open}");
    assert!(
        glass > 4.0 * opaque.max(1e-6) && glass < open,
        "{glass} between {opaque} and {open}"
    );
}

#[test]
fn a_pixel_traces_the_same_every_time() {
    let mut setup = Setup::new(uniform_sky(0.3));
    setup.add(
        ball(Vec3::ZERO, 1.0),
        Material::new(
            Pigment::Solid(Vec3::new(0.2, 0.5, 0.8)),
            Finish::Coated { roughness: 0.4 },
        ),
        None,
    );
    setup.lights.push(Light::Point {
        at: Vec3::new(2.0, 3.0, -3.0),
        intensity: Vec3::splat(5.0),
    });
    let scene = setup.scene();
    for x in 0..SIZE.0 {
        assert_eq!(shown(&scene, (x, 4)), shown(&scene, (x, 4)));
    }
}

/// An even sky settles after the first round; the edge of a ball against it
/// is sampled on.
#[test]
fn flat_pixels_settle_early_and_edges_take_more_samples() {
    let first = Quality::Fine.rounds()[0];
    let empty = Setup::new(uniform_sky(0.4)).scene();
    assert_eq!(shown(&empty, MIDDLE).1, first);
    let mut setup = Setup::new(uniform_sky(0.9));
    setup.add(
        ball(Vec3::ZERO, 0.4),
        Material::new(
            Pigment::Solid(Vec3::splat(0.02)),
            Finish::Coated { roughness: 1.0 },
        ),
        None,
    );
    // The ball fills the middle of the frame; its silhouette crosses the
    // row half way out from the middle.
    let scene = setup.scene();
    let taken: Vec<u32> = (0..SIZE.0).map(|x| shown(&scene, (x, 4)).1).collect();
    assert!(taken.iter().any(|count| *count > first), "{taken:?}");
}

#[test]
fn the_power_heuristic_splits_one_between_two_strategies() {
    for (a, b) in [(1.0, 1.0), (3.0, 0.5), (1e-9, 7.0), (0.0, 2.0)] {
        assert!((power(a, b) + power(b, a) - 1.0).abs() < 1e-12);
    }
    assert!((power(2.0, 0.0) - 1.0).abs() < 1e-12);
    assert!(
        (power(0.0, 0.0) - 1.0).abs() < 1e-12,
        "no strategy to share with"
    );
    assert!((power(1.0, 1.0) - 0.5).abs() < 1e-12);
}

#[test]
fn a_quality_caps_the_samples_a_pixel_takes() {
    let mut setup = Setup::new(uniform_sky(0.9));
    setup.add(
        ball(Vec3::ZERO, 0.4),
        Material::new(
            Pigment::Solid(Vec3::splat(0.02)),
            Finish::Coated { roughness: 1.0 },
        ),
        None,
    );
    let scene = setup.scene();
    let encoder = Encoder::new().expect("an encoder");
    let tracer = Tracer::new(&scene, &encoder, SIZE, 0x5eed);
    for quality in Quality::ALL {
        let most = (0..SIZE.0).map(|x| tracer.light((x, 4), quality).1).max();
        assert!(most <= Some(quality.most()), "{quality:?}");
        assert_eq!(quality.rounds().last(), Some(&quality.most()));
    }
    let mut last = 0;
    for quality in Quality::ALL {
        assert!(quality.most() > last, "the finer, the more samples");
        last = quality.most();
    }
}

/// A pixel's encoded colour is its traced light, toned and dithered for the
/// screen, whatever core traces it.
#[test]
fn a_pixel_is_its_light_encoded() {
    let mut setup = Setup::new(uniform_sky(0.3));
    setup.add(
        ball(Vec3::ZERO, 1.0),
        Material::new(
            Pigment::Solid(Vec3::new(0.2, 0.5, 0.8)),
            Finish::Coated { roughness: 0.4 },
        ),
        None,
    );
    let scene = setup.scene();
    let encoder = Encoder::new().expect("an encoder");
    let tracer = Tracer::new(&scene, &encoder, SIZE, 7);
    for x in 0..SIZE.0 {
        let (light, taken) = tracer.light((x, 3), Quality::Good);
        assert_eq!(
            tracer.pixel((x, 3), Quality::Good),
            (encoder.pixel(light, (x, 3)), taken)
        );
    }
}

/// A pattern is looked up in its object's own frame, normal and all: a box
/// turned a quarter turn keeps its bricks on its faces, not on a world axis.
#[test]
fn a_pattern_turns_with_its_object() {
    let mut setup = Setup::new(uniform_sky(0.3));
    let turned = Pose::new(Vec3::ZERO, Frame::turned(core::f64::consts::FRAC_PI_2, 0.0));
    let index = setup.add(
        ball(Vec3::ZERO, 1.0),
        Material::new(Pigment::Solid(Vec3::ONE), Finish::Matte),
        None,
    );
    setup.objects[index].texture = turned;
    let scene = setup.scene();
    let encoder = Encoder::new().expect("an encoder");
    let tracer = Tracer::new(&scene, &encoder, SIZE, 1);
    let ray = crate::vector::Ray::new(Vec3::new(0.0, 0.0, -5.0), Vec3::new(0.0, 0.0, 1.0));
    let (_, hit) = scene
        .closest(&ray, f64::INFINITY, crate::scene::Sight::Eye)
        .expect("the ball");
    let (surface, _) = Tracer::surface(&ray, &hit, &scene.objects[index], &scene.materials[0], 0.0);
    let spot = tracer.spot(&surface, &scene.objects[index], &hit);
    // The world's -z face is the turned frame's +x or -x face.
    assert!(
        (spot.normal.x.abs() - 1.0).abs() < 1e-9,
        "{:?}",
        spot.normal
    );
    assert!((spot.normal - turned.frame.to_local(surface.normal)).length() < 1e-12);
}

/// Below the horizon, a ray that meets nothing has passed into the haze;
/// above it, the sky shows.
#[test]
fn below_the_horizon_nothing_met_is_haze() {
    let mut setup = Setup::new(Sky {
        zenith: Vec3::new(0.1, 0.2, 0.6),
        horizon: Vec3::new(0.7, 0.75, 0.8),
        ground: Vec3::splat(0.05),
        glow: None,
        stars: 0.0,
        clouds: None,
    });
    setup.add(
        ball(Vec3::new(0.0, 0.0, 50.0), 0.1),
        Material::new(Pigment::Solid(Vec3::ONE), Finish::Matte),
        None,
    );
    let mut scene = setup.scene();
    scene.fog = Some(crate::scene::Fog { density: 0.001 });
    let encoder = Encoder::new().expect("an encoder");
    let tracer = Tracer::new(&scene, &encoder, SIZE, 1);
    let mut sampler = crate::sample::Sampler::new(1, 0);
    let down = crate::vector::Ray::new(Vec3::ZERO, Vec3::new(1.0, -0.1, 0.0).normalized());
    let seen = tracer.radiance(&down, super::Path::EYE, &mut sampler);
    assert!(
        close(seen, scene.sky.haze(Vec3::new(1.0, 0.0, 0.0)), 1e-12),
        "{seen:?}"
    );
    let up = crate::vector::Ray::new(Vec3::ZERO, Vec3::new(1.0, 0.5, 0.0).normalized());
    let sky = tracer.radiance(&up, super::Path::EYE, &mut sampler);
    assert!(
        close(sky, scene.sky.radiance(Vec3::ZERO, up.dir, true), 1e-12),
        "{sky:?}"
    );
}
