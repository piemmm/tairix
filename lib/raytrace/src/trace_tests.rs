//! Host tests of the integrator, on small scenes whose answers are known.

use alloc::vec::Vec;

use tairix_util::mathf;

use super::{power, Quality, Tracer};
use crate::camera::Camera;
use crate::light::Light;
use crate::material::{Finish, Material, Relief, Wind, COAT_F0};
use crate::pigment::Pigment;
use crate::sample::Sampler;
use crate::scene::{Exposure, Object, Parts, Scene};
use crate::shape::Shape;
use crate::sky::{Dome, Gradient, Sky};
use crate::tone::{filmic, Encoder};
use crate::vector::{Frame, Pose, Ray, Vec3};

const SIZE: (u32, u32) = (9, 9);
const MIDDLE: (u32, u32) = (4, 4);

/// A gradient sky from `zenith` to `horizon` over `ground`.
fn gradient(zenith: Vec3, horizon: Vec3, ground: Vec3) -> Sky {
    Sky {
        dome: Dome::Gradient(Gradient {
            zenith,
            horizon,
            ground,
            glow: None,
        }),
        stars: 0.0,
        clouds: None,
        bank: None,
    }
}

fn uniform_sky(level: f64) -> Sky {
    gradient(Vec3::splat(level), Vec3::splat(level), Vec3::splat(level))
}

struct Setup {
    objects: Vec<Object>,
    materials: Vec<Material>,
    lights: Vec<Light>,
    sky: Sky,
    shades: Option<crate::shade::Shades>,
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
            shades: None,
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
            prototypes: Vec::new(),
            lawns: Vec::new(),
            materials: self.materials,
            lights: self.lights,
            sky: self.sky,
            fog: None,
            shades: self.shades,
            camera: Camera::looking(self.eye, self.target, 0.3, 1.0, (0.0, 1.0)),
            exposure: Exposure::Fixed(1.0),
            daylight: 1.0,
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

/// Water ruffled steeply and seen low, clear and lossless, still shows an
/// even sky unchanged: a facet whose reflection dips under the surface turns
/// it back up rather than losing it, so low water keeps the brightness its
/// reflectance owes.
#[test]
fn rippled_water_seen_low_loses_none_of_an_even_sky() {
    let waves = Relief::waves(
        Wind {
            slope_variance: 0.05,
            lengths: (1.0, 0.03),
            spread: 0.8,
            gusts: (1.0, 50.0),
        },
        3,
    )
    .expect("waves");
    let mut setup = Setup::new(uniform_sky(0.5));
    setup.eye = Vec3::new(0.0, 0.3, -5.0);
    setup.target = Vec3::new(0.0, 0.0, 40.0);
    let water = Finish::Glass {
        ior: 1.333,
        absorb: Vec3::ZERO,
        glow: Vec3::ZERO,
        roughness: 0.0,
        dispersion: 0.0,
        foam: None,
    };
    setup.add(
        ground(),
        Material::new(Pigment::Solid(Vec3::ONE), water).with_relief(waves),
        Some(Vec3::ONE),
    );
    let scene = setup.scene();
    let expected = Vec3::splat(filmic(0.5));
    for row in 5..SIZE.1 {
        for column in 0..SIZE.0 {
            let (light, _) = shown(&scene, (column, row));
            assert!(
                close(light, expected, 0.005),
                "({column}, {row}): {light:?} against {expected:?}"
            );
        }
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
    let (surface, _) = tracer.surface(
        &ray,
        &hit,
        (&scene.objects[index], &scene.materials[0]),
        0.0,
    );
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
    let mut setup = Setup::new(gradient(
        Vec3::new(0.1, 0.2, 0.6),
        Vec3::new(0.7, 0.75, 0.8),
        Vec3::splat(0.05),
    ));
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
        close(
            sky,
            scene.sky.radiance(Vec3::ZERO, up.dir, true, 0.5),
            1e-12
        ),
        "{sky:?}"
    );
}

/// A room of a floor and a roof over it, the sun low enough to light the floor
/// beyond the roof but not beneath it, the sky black: the roof's underside is
/// lit by the floor alone, and the more the floor gives back, the more.
#[test]
fn the_light_a_floor_gives_back_lights_the_roof_above_it() {
    let underside = |floor: f64| {
        let mut setup = Setup::new(uniform_sky(0.0));
        setup.add(
            ground(),
            Material::new(Pigment::Solid(Vec3::splat(floor)), Finish::Matte),
            None,
        );
        setup.add(
            Shape::Quad {
                corner: Vec3::new(-1.0, 1.0, -1.0),
                edge_u: Vec3::new(0.0, 0.0, 2.0),
                edge_v: Vec3::new(2.0, 0.0, 0.0),
            },
            Material::new(Pigment::Solid(Vec3::splat(0.7)), Finish::Matte),
            None,
        );
        setup.lights.push(Light::Sun {
            toward: Vec3::new(0.8, 0.6, 0.0),
            cos_radius: 0.9999,
            radiance: Vec3::splat(5000.0),
        });
        setup.eye = Vec3::new(0.0, 0.3, 0.0);
        setup.target = Vec3::new(0.0, 1.0, 0.1);
        let scene = setup.scene();
        (0..SIZE.0)
            .map(|x| shown(&scene, (x, MIDDLE.1)).0.x)
            .sum::<f64>()
            / f64::from(SIZE.0)
    };
    let (none, some, more) = (underside(0.0), underside(0.3), underside(0.6));
    assert!(none < 1e-9, "a black floor gives back nothing: {none}");
    assert!(some > 0.01, "{some}");
    assert!(more > 1.3 * some, "{more} against {some}");
}

/// A lamp is gathered directly, so a ray scattered off the floor that finds
/// it adds nothing: the floor under a glowing ball, in a black sky, shows
/// what the ball's light alone gives it, and no more.
#[test]
fn a_lamp_is_counted_once_on_the_surface_it_lights() {
    let (radius, height, albedo) = (0.5, 2.0, 0.5);
    let radiance = Vec3::splat(3.0);
    let mut setup = Setup::new(uniform_sky(0.0));
    setup.add(
        ground(),
        Material::new(Pigment::Solid(Vec3::splat(albedo)), Finish::Matte),
        None,
    );
    let orb = setup.add(
        ball(Vec3::new(0.0, height, 0.0), radius),
        Material::new(Pigment::Solid(Vec3::ZERO), Finish::Glow { radiance }),
        None,
    );
    setup.lights.push(Light::Orb {
        object: u32::try_from(orb).expect("few objects"),
        centre: Vec3::new(0.0, height, 0.0),
        radius,
        radiance,
    });
    setup.objects[orb].light = Some(0);
    setup.eye = Vec3::new(0.0, 0.5, -1.0);
    setup.target = Vec3::ZERO;
    let scene = setup.scene();
    // Under a sphere of radiance L seen at sine s from straight below, a
    // floor of albedo a shows a L s².
    let sine = radius / height;
    let expected = filmic(albedo * radiance.x * sine * sine);
    let (light, _) = shown(&scene, MIDDLE);
    assert!(
        (light.x - expected).abs() < 0.03 * expected,
        "{light:?} against {expected}"
    );
}

/// Something that glows with no lamp standing for it lights what is about it
/// through the light scattered to it.
#[test]
fn a_glow_no_lamp_stands_for_lights_the_floor_beneath_it() {
    let mut setup = Setup::new(uniform_sky(0.0));
    setup.add(
        ground(),
        Material::new(Pigment::Solid(Vec3::splat(0.5)), Finish::Matte),
        None,
    );
    setup.add(
        ball(Vec3::new(0.0, 1.0, 0.0), 0.5),
        Material::new(
            Pigment::Solid(Vec3::ZERO),
            Finish::Glow {
                radiance: Vec3::splat(2.0),
            },
        ),
        None,
    );
    setup.eye = Vec3::new(0.0, 0.5, -1.5);
    setup.target = Vec3::new(0.0, 0.0, 0.3);
    let (light, _) = shown(&setup.scene(), MIDDLE);
    assert!(light.x > 0.02, "{light:?}");
}

/// Air under a closed wood's crowns is lit only by what gets through them,
/// the air above them and out in the open by the whole sky.
#[test]
fn the_air_beneath_a_wood_is_roofed_by_its_crowns() {
    let mut crowns = Vec::new();
    for row in -40..=40 {
        for column in -40..=40 {
            crowns.push(((f64::from(column) * 5.0, f64::from(row) * 5.0), 4.5));
        }
    }
    let shades = crate::shade::Shades::of(&crowns, ((0.0, 0.0), 400.0), (0.0, 0.0)).expect("held");
    let scene = Setup {
        shades: Some(shades),
        ..Setup::new(uniform_sky(1.0))
    }
    .scene();
    let encoder = Encoder::new().expect("an encoder");
    let tracer = Tracer::new(&scene, &encoder, SIZE, 0x5eed);
    let level = Ray::new(Vec3::new(0.0, 1.7, 0.0), Vec3::new(1.0, 0.0, 0.0));
    let under = tracer.lit_air(&level, 120.0);
    assert!(under < 0.25, "{under}");
    let skyward = Ray::new(Vec3::new(0.0, 1.7, 0.0), Vec3::new(0.0, 1.0, 0.0));
    assert!(
        tracer.lit_air(&skyward, 200.0) > 0.7,
        "most of the air up there is above the crowns"
    );
    let beyond = Ray::new(Vec3::new(600.0, 1.7, 0.0), Vec3::new(1.0, 0.0, 0.0));
    assert!(
        (tracer.lit_air(&beyond, 120.0) - 1.0).abs() < 1e-9,
        "out in the open"
    );
    assert!(
        (tracer.lit_air(&level, 5.0) - 1.0).abs() < 1e-9,
        "a few metres of air is not worth reading"
    );
}

/// Toward a low sun behind a wall, the air before the wall is in its shadow
/// and takes none of the sun's light; the air beside it takes it all, and
/// air running across the wall's shadow takes it along what lies outside.
#[test]
fn the_sun_lights_only_the_air_it_reaches() {
    let mut setup = Setup::new(uniform_sky(1.0));
    setup.add(
        Shape::Quad {
            corner: Vec3::new(60.0, -1.0, -10.0),
            edge_u: Vec3::new(0.0, 0.0, 20.0),
            edge_v: Vec3::new(0.0, 40.0, 0.0),
        },
        Material::new(Pigment::Solid(Vec3::splat(0.3)), Finish::Matte),
        None,
    );
    setup.lights.push(Light::Sun {
        toward: Vec3::new(1.0, 0.12, 0.0).normalized(),
        cos_radius: 0.9999,
        radiance: Vec3::splat(5000.0),
    });
    let scene = setup.scene();
    let encoder = Encoder::new().expect("an encoder");
    let tracer = Tracer::new(&scene, &encoder, SIZE, 0x5eed);
    let lit = |from: Vec3, dir: Vec3, reach: f64| {
        let draws = 256u32;
        (0..draws)
            .map(|index| {
                let mut sampler = Sampler::new(0x5eed, index);
                tracer
                    .sunlit_air(&Ray::new(from, dir), reach, &mut sampler)
                    .max_element()
            })
            .sum::<f64>()
            / f64::from(draws)
    };
    let east = Vec3::new(1.0, 0.0, 0.0);
    assert!(
        lit(Vec3::new(0.0, 1.7, 0.0), east, 50.0) < 1e-9,
        "in the wall's shadow"
    );
    assert!(
        (lit(Vec3::new(0.0, 1.7, 30.0), east, 50.0) - 1.0).abs() < 1e-9,
        "beside it"
    );
    let across = lit(Vec3::new(50.0, 1.7, -30.0), Vec3::new(0.0, 0.0, 1.0), 60.0);
    assert!(
        (across - 2.0 / 3.0).abs() < 0.06,
        "a third of it in the shadow: {across}"
    );
}

/// An evenly lit picture is exposed to its key; one a third of which is a
/// sky far brighter than the land is pulled down just so far that the sky
/// sits below white, and no more than two stops however bright the sky; and
/// a sun's sliver moves nothing.
#[test]
fn the_meter_holds_a_bright_sky_below_white_but_lets_the_sun_blow_out() {
    let key = 0.18;
    let even = [mathf::ln(0.25); 600];
    assert!((super::exposure_of(&even, key) - key / 0.25).abs() < 1e-9);
    let skyward = |sky: f64| {
        let mut logs: Vec<f64> = (0..600)
            .map(|index| mathf::ln(if index < 200 { sky } else { 0.4 }))
            .collect();
        logs.sort_unstable_by(f64::total_cmp);
        let trim = logs.len() / super::METER_TRIM;
        let kept = &logs[trim..logs.len() - trim];
        let metered = key / mathf::exp(kept.iter().sum::<f64>() / crate::vector::real(kept.len()));
        (super::exposure_of(&logs, key), metered)
    };
    let (held, metered) = skyward(60.0);
    assert!(
        metered * 60.0 > super::NEAR_WHITE + 0.5,
        "left alone the sky would blow out: {}",
        metered * 60.0
    );
    assert!(
        (held * 60.0 - super::NEAR_WHITE).abs() < 1e-9,
        "held below white: {}",
        held * 60.0
    );
    let (held, metered) = skyward(1e4);
    assert!(
        (held - metered / 4.0).abs() < 1e-12,
        "two stops at most: {held} against {metered}"
    );
    let mut sunlit = even.to_vec();
    for slot in sunlit.iter_mut().take(10) {
        *slot = mathf::ln(1e5);
    }
    sunlit.sort_unstable_by(f64::total_cmp);
    assert!(
        (super::exposure_of(&sunlit, key) - key / 0.25).abs() < 1e-9,
        "the sun alone"
    );
}
