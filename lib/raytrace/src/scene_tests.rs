//! Host tests of a scene's two queries: the nearest object along a ray, and
//! the light that gets through along one.

use alloc::vec;
use alloc::vec::Vec;

use core::f64::consts::{FRAC_PI_2, TAU};

use tairix_util::mathf;

use super::{daylight, Draft, Exposure, Object, Parts, Scene, Sight};
use crate::atmosphere::Air;
use crate::body::{self, sunlight};
use crate::camera::Camera;
use crate::compose::Setting;
use crate::detail::Detail;
use crate::light::Light;
use crate::material::{Finish, Material};
use crate::pigment::Pigment;
use crate::sample::{mix32, unit};
use crate::shape::Shape;
use crate::sky::{Dome, Gradient, Seeing, Sky};
use crate::vector::{Frame, Pose, Ray, Vec3, PACKET};

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
            }),
            stars: None,
            low: None,
            high: None,
        },
        shades: None,
        camera: Camera::looking(Vec3::new(0.0, 1.0, -5.0), Vec3::ZERO, 0.8, 1.0, (0.0, 1.0)),
        exposure: Exposure::Fixed(1.0),
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
    let mut building =
        Scene::building(parts(crowd()), &tairix_parallel::Threaded::new(3)).expect("a building");
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

/// The slab test is given finite boxes only: an object whose box is not
/// finite is tested by every ray instead, never culled by its box.
#[test]
fn an_object_whose_box_is_not_finite_is_tested_by_every_ray() {
    let scene = scene(vec![
        object(ball(-5.0, 1.0), 0, None),
        object(ball(0.0, f64::INFINITY), 0, None),
        object(ball(-9.0, 1.0), 0, None),
    ]);
    assert_eq!(scene.unbounded, vec![1]);
    let ray = Ray::new(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0));
    let (index, hit) = scene
        .closest(&ray, f64::INFINITY, Sight::Eye)
        .expect("the near ball");
    assert_eq!((index, hit.t.to_bits()), (0, 4.0f64.to_bits()));
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
        let seeing = crate::sky::Seeing {
            fine: true,
            spread: Some(1e-3),
            jitter: 0.5,
            air: 0.5,
        };
        let light = scene.sky.radiance(Vec3::ZERO, dir, seeing);
        marks.extend([light.x.to_bits(), light.y.to_bits(), light.z.to_bits()]);
    }
    marks.push(scene.exposure.to_bits());
    marks.push(scene.prototypes.len() as u64);
    if let Some(adaptation) = &scene.adaptation {
        for step in 0..60u32 {
            let film = (unit(mix32(step)), unit(mix32(step ^ 0x77)));
            let luminance = 0.01 * f64::from(1 + step % 12) * f64::from(1 + step);
            marks.push(adaptation.factor(film, luminance).to_bits());
        }
    }
    marks
}

/// A packet of eye rays through one part of the picture finds, ray for ray,
/// what each finds alone: each walks the hierarchy in its own order, and the
/// lawns they come to are crossed together.
#[test]
fn a_packet_of_eye_rays_finds_what_each_finds_alone() {
    let scene = Draft::new(Setting::Meadow, 3, SIZE, Detail::Maximum)
        .expect("a draft")
        .finish()
        .expect("a scene");
    let mut on_lawns = 0;
    for pixel in 0..300u32 {
        let draw = |salt: u32| unit(mix32(mix32(pixel) ^ salt));
        let centre = (2.0 * draw(1) - 1.0, 2.0 * draw(2) - 1.0);
        // A pixel's breadth, or every eighth packet a fifth of the picture,
        // so the rays part and come to lawns at different times.
        let spread = if pixel % 8 == 0 {
            0.4
        } else {
            2.0 / f64::from(SIZE.1)
        };
        let rays: [Ray; PACKET] = core::array::from_fn(|lane| {
            let lane = u32::try_from(lane).expect("a packet is small");
            let jitter = |salt: u32| spread * (unit(mix32(mix32(pixel ^ lane << 20) ^ salt)) - 0.5);
            scene
                .camera
                .ray((centre.0 + jitter(3), centre.1 + jitter(4)), (0.0, 0.0))
        });
        let mut together = [None; PACKET];
        scene.closest_of(&rays, f64::INFINITY, Sight::Eye, &mut together);
        for (lane, ray) in rays.iter().enumerate() {
            let alone = scene.closest(ray, f64::INFINITY, Sight::Eye);
            assert_eq!(
                alloc::format!("{:?}", together[lane]),
                alloc::format!("{alone:?}"),
                "pixel {pixel}, ray {lane}"
            );
            if let Some((index, _)) = alone {
                on_lawns += usize::from(matches!(scene.objects[index].shape, Shape::Lawn { .. }));
            }
        }
    }
    assert!(on_lawns > 500, "only {on_lawns} rays met a lawn");
}

/// A draft does its work a unit at a time, however soon its caller's time is
/// spent, and the scene it finishes is the one a draft prepared at once
/// finishes.
#[test]
fn a_draft_prepared_a_unit_at_a_time_finishes_the_scene_prepared_at_once() {
    let mut stepped = Draft::new(Setting::Meadow, 3, SIZE, Detail::Maximum).expect("a draft");
    let mut calls = 0;
    while !stepped
        .prepare(&tairix_parallel::SERIAL, &mut || true)
        .expect("prepared")
    {
        calls += 1;
    }
    assert!(calls > 20, "a meadow's work takes {calls} units");
    let stepped = stepped.finish().expect("a scene");
    let at_once = Draft::new(Setting::Meadow, 3, SIZE, Detail::Maximum)
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
    let mut spread = Draft::new(Setting::Coast, 11, SIZE, Detail::Maximum).expect("a draft");
    while !spread.prepare(&pool, &mut || false).expect("prepared") {}
    let spread = spread.finish().expect("a scene");
    let alone = Draft::new(Setting::Coast, 11, SIZE, Detail::Maximum)
        .expect("a draft")
        .finish()
        .expect("a scene");
    assert!(
        alone.adaptation.is_some(),
        "a coast's sky lies past what one exposure holds"
    );
    assert_eq!(fingerprint(&spread), fingerprint(&alone));
}

/// A draft's progress never falls back, climbs through the work rather than
/// leaping to its end, and reaches its whole only once the scene is ready:
/// on a land, and for a still life with no land at all, at either detail.
#[test]
fn a_drafts_progress_climbs_steadily_to_its_whole_once_ready() {
    for (setting, detail) in [Setting::Meadow, Setting::Studio]
        .into_iter()
        .flat_map(|setting| Detail::ALL.map(|detail| (setting, detail)))
    {
        let mut draft = Draft::new(setting, 7, SIZE, detail).expect("a draft");
        assert_eq!(draft.progress(), 0);
        let mut seen = vec![0u16];
        while !draft
            .prepare(&tairix_parallel::SERIAL, &mut || true)
            .expect("prepared")
        {
            let progress = draft.progress();
            assert!(
                progress >= *seen.last().expect("one"),
                "{setting:?} {detail:?}: fell back"
            );
            assert!(
                progress < 1000,
                "{setting:?} {detail:?}: whole before it is ready"
            );
            if progress != *seen.last().expect("one") {
                seen.push(progress);
            }
        }
        assert_eq!(draft.progress(), 1000);
        let largest = seen
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .chain([1000 - seen.last().copied().unwrap_or(0)])
            .max()
            .unwrap_or(1000);
        assert!(seen.len() > 10, "{setting:?} {detail:?}: {seen:?}");
        assert!(
            largest <= 300,
            "{setting:?} {detail:?} leapt {largest}: {seen:?}"
        );
    }
}

/// A picture of no pixels has no scene.
#[test]
fn a_picture_with_no_pixels_has_no_draft() {
    assert!(Draft::new(Setting::Meadow, 1, (0, 10), Detail::Maximum).is_none());
    assert!(Draft::new(Setting::Meadow, 1, (10, 0), Detail::Maximum).is_none());
}

/// Where a draft's water has beams to lay, laying them takes its share of the
/// readout; where it has none, gathering takes that share too, so the readout
/// steps over no stage that does no work.
#[test]
fn a_draft_with_no_beams_to_lay_gives_their_share_to_gathering() {
    for (setting, watered) in [(Setting::Studio, false), (Setting::Lagoon, true)] {
        let mut draft = Draft::new(setting, 7, (64, 36), Detail::Simple).expect("a draft");
        let shares = super::ends(draft.landed, draft.detail);
        let mut gathering_from = None;
        while !draft
            .prepare(&tairix_parallel::SERIAL, &mut || true)
            .expect("prepared")
        {
            if gathering_from.is_none() && matches!(draft.state, super::State::Gathering(..)) {
                gathering_from = Some(f64::from(draft.progress()) / 1000.0);
            }
        }
        let from = gathering_from.expect("a gathering stage");
        assert_eq!(draft.watered, watered, "{setting:?}");
        let expected = if watered {
            shares.focused
        } else {
            shares.built
        };
        assert!(
            (from - expected).abs() <= 0.002,
            "{setting:?} gathers from {from} against {expected}"
        );
    }
}

/// Where the daylight tests' eye stands, and the level beneath it.
const EYE: Vec3 = Vec3::new(0.0, 2.0, 0.0);
const LEVEL: Vec3 = Vec3::new(0.0, 0.0, 0.0);

/// An open sky over sea level lit by `light`, which stands toward `toward`.
fn open_under(toward: Vec3, light: &Light) -> Sky {
    let air = Air {
        sun: toward,
        solar: light.irradiance(),
        base: 0.0,
        haze: 1.0,
        albedo: Vec3::splat(0.2),
        eye: EYE,
    };
    crate::sky::tests::open(air, None)
}

/// The unit direction `elevation` degrees above the horizon.
fn raised(elevation: f64) -> Vec3 {
    let radians = elevation.to_radians();
    Vec3::new(0.0, mathf::sin(radians), mathf::cos(radians))
}

/// The sun `elevation` degrees up, and the open sky it lights.
fn sunlit(elevation: f64) -> (Sky, Light) {
    let sun = body::sun(raised(elevation));
    (open_under(raised(elevation), &sun), sun)
}

/// `light` over the sunlight above the air, channel by channel.
fn of_sunlight(light: Vec3) -> Vec3 {
    let above = sunlight();
    Vec3::new(light.x / above.x, light.y / above.y, light.z / above.z)
}

/// The light falling on the level is the sun's beam as the air keeps and
/// spreads it, and the sky's light over the whole sky above, which a fine
/// grid over the sky's rings and spokes gathers as well.
#[test]
fn the_daylight_on_the_level_is_the_suns_kept_beam_and_the_whole_skys_light() {
    let (sky, sun) = sunlit(60.0);
    let (whole, skylight) = (daylight(&sky, &[sun], EYE), daylight(&sky, &[], EYE));
    let arriving = sky.arriving(LEVEL, raised(60.0)).expect("the sun is up");
    let beam = arriving.kept * (arriving.dir.y * arriving.stretch);
    for channel in 0..3 {
        let (sampled, expected) = (
            whole.along(channel) - skylight.along(channel),
            beam.along(channel),
        );
        assert!(
            (sampled / expected - 1.0).abs() < 1e-2,
            "{channel}: {sampled} against {expected}"
        );
    }
    let seeing = Seeing {
        fine: false,
        spread: None,
        jitter: 0.5,
        air: 0.5,
    };
    let (rings, spokes) = (180u32, 360u32);
    let mut gathered = Vec3::ZERO;
    for ring in 0..rings {
        let edge = |ring: u32| mathf::sin(f64::from(ring) / f64::from(rings) * FRAC_PI_2);
        let (low, high) = (edge(ring), edge(ring + 1));
        // Each ring's share of the level's cosine-weighted sky.
        let weight = 0.5 * (high * high - low * low) * TAU / f64::from(spokes);
        let rise = mathf::sin((f64::from(ring) + 0.5) / f64::from(rings) * FRAC_PI_2);
        let level = mathf::sqrt(1.0 - rise * rise);
        for spoke in 0..spokes {
            let heading = (f64::from(spoke) + 0.5) / f64::from(spokes) * TAU;
            let dir = Vec3::new(
                level * mathf::sin(heading),
                rise,
                level * mathf::cos(heading),
            );
            gathered += sky.radiance(LEVEL, dir, seeing) * weight;
        }
    }
    let expected = of_sunlight(gathered);
    for channel in 0..3 {
        let (sampled, expected) = (skylight.along(channel), expected.along(channel));
        assert!(
            (sampled / expected - 1.0).abs() < 1e-2,
            "{channel}: {sampled} against {expected}"
        );
    }
    assert!(
        skylight.z > skylight.y && skylight.y > skylight.x && skylight.y > 0.02,
        "the clear sky adds blue light most: {skylight:?}"
    );
}

/// Ever less light falls as the sun sinks, and once it has set only the
/// twilight sky's.
#[test]
fn ever_less_light_falls_as_the_sun_sinks_and_once_set_only_the_twilights() {
    let falling = [60.0, 20.0, 5.0, -4.0].map(|elevation| {
        let (sky, sun) = sunlit(elevation);
        daylight(&sky, &[sun], EYE)
    });
    for pair in falling.windows(2) {
        assert!(pair[1].y < pair[0].y, "{falling:?}");
    }
    let (sky, sun) = sunlit(-4.0);
    assert_eq!(daylight(&sky, &[sun], EYE), daylight(&sky, &[], EYE));
    assert!(
        falling[3].y > 0.0 && falling[3].y < 1e-2,
        "{:?}",
        falling[3]
    );
}

/// By the full moon the level takes the moon's share of what the sun as high
/// would give it, which is what darkens a deep pool's glow at night.
#[test]
fn by_the_full_moon_the_level_takes_the_moons_share_of_daylight() {
    let moon = body::full_moon(raised(60.0));
    let sky = open_under(raised(60.0), &moon);
    let share = of_sunlight(moon.irradiance());
    let moonlit = daylight(&sky, &[moon], EYE);
    let (sky, sun) = sunlit(60.0);
    let sunlit = daylight(&sky, &[sun], EYE);
    assert!(share.y < 1e-5, "{share:?}");
    for channel in 0..3 {
        let (by_moon, by_sun) = (
            moonlit.along(channel) / share.along(channel),
            sunlit.along(channel),
        );
        assert!(
            (by_moon / by_sun - 1.0).abs() < 1e-2,
            "{channel}: {by_moon} against {by_sun}"
        );
    }
}

/// Under a room's walls the whole light falls, there being no water to dim.
#[test]
fn under_a_rooms_walls_the_whole_daylight_falls() {
    let sky = parts(Vec::new()).sky;
    assert_eq!(daylight(&sky, &[body::sun(raised(60.0))], EYE), Vec3::ONE);
}
