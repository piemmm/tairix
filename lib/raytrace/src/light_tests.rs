//! Host tests of how each light is sampled: a spot within its cone, the sun
//! within its disc as its limb has it and where the air bends it, an orb and
//! a panel as their solid angles say.

use alloc::vec::Vec;
use core::f64::consts::PI;

use tairix_util::mathf;

use super::{Light, Limb};
use crate::atmosphere::Air;
use crate::shape::{Geometry, Shape};
use crate::sky::{Dome, Gradient, Sky};
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

/// A room's walls, which bend no light.
fn room() -> Sky {
    Sky {
        dome: Dome::Gradient(Gradient {
            zenith: Vec3::ZERO,
            horizon: Vec3::ZERO,
            ground: Vec3::ZERO,
        }),
        stars: None,
        low: None,
        high: None,
    }
}

/// The open air at the sea, its sun `elevation` degrees up toward +z.
fn open(elevation: f64) -> Sky {
    let radians = elevation.to_radians();
    let air = Air {
        sun: Vec3::new(0.0, mathf::sin(radians), mathf::cos(radians)),
        solar: Vec3::splat(20.0),
        base: 0.0,
        haze: 1.0,
        albedo: Vec3::splat(0.2),
        eye: Vec3::new(0.0, 2.0, 0.0),
    };
    crate::sky::tests::open(air, None)
}

/// The sun's own disc toward `toward`, half a degree across, as `limb` has
/// it.
fn sun(toward: Vec3, limb: Limb) -> Light {
    Light::Sun {
        toward,
        cos_radius: mathf::cos(0.004_65),
        radiance: Vec3::splat(1000.0),
        limb,
    }
}

const DARKENING: Limb = Limb::Darkening(Vec3::new(0.40, 0.51, 0.66));

#[test]
fn a_spot_lights_its_cone_alone_and_softens_at_the_edge() {
    let sky = room();
    let spot = Light::Spot {
        at: Vec3::new(0.0, 5.0, 0.0),
        axis: -Vec3::UP,
        cos_inner: mathf::cos(0.2),
        cos_outer: mathf::cos(0.4),
        intensity: Vec3::splat(25.0),
    };
    let centre = spot
        .sample(Vec3::ZERO, (0.5, 0.5), &sky)
        .expect("in the cone");
    assert!((centre.light.x - 1.0).abs() < 1e-12);
    let edge = spot
        .sample(Vec3::new(5.0 * mathf::tan(0.3), 0.0, 0.0), (0.5, 0.5), &sky)
        .expect("penumbra");
    assert!(edge.light.x > 0.0 && edge.light.x < centre.light.x);
    assert!(spot
        .sample(Vec3::new(5.0, 0.0, 0.0), (0.5, 0.5), &sky)
        .is_none());
    assert!(spot.density(Vec3::ZERO, Vec3::UP, 5.0, &sky) <= 0.0);
}

#[test]
fn the_sun_is_sampled_within_its_disc_at_its_own_density() {
    let sky = room();
    let toward = Vec3::new(0.3, 0.8, -0.2).normalized();
    let cos_radius = mathf::cos(0.02);
    let sun = Light::Sun {
        toward,
        cos_radius,
        radiance: Vec3::splat(1000.0),
        limb: Limb::Even,
    };
    for pair in grid(12) {
        let incidence = sun
            .sample(Vec3::ZERO, pair, &sky)
            .expect("the sun always shines");
        assert!(incidence.dir.dot(toward) >= cos_radius - 1e-12);
        assert!(incidence.distance.is_infinite());
        // The estimate the sample makes of the sun's light is its radiance
        // over its solid angle, whichever sample it is.
        let solid = 2.0 * PI * (1.0 - cos_radius);
        assert!((incidence.light.x * incidence.density - 1000.0).abs() < 1e-6);
        assert!((incidence.density * solid - 1.0).abs() < 1e-9);
        assert!(
            (sun.density(Vec3::ZERO, incidence.dir, f64::INFINITY, &sky) - incidence.density).abs()
                < 1e-9
        );
    }
    assert!(sun.density(Vec3::ZERO, -toward, f64::INFINITY, &sky) <= 0.0);
    assert!((sun.irradiance().x / (1000.0 * 2.0 * PI * (1.0 - cos_radius)) - 1.0).abs() < 1e-12);
}

#[test]
fn a_darkening_limb_spreads_the_discs_light_and_keeps_all_of_it() {
    // Over the disc's area, out to its edge, the profile's mean is one.
    let steps = 100_000u32;
    let mut held = Vec3::ZERO;
    for step in 0..steps {
        let fraction = (f64::from(step) + 0.5) / f64::from(steps);
        held += DARKENING.across([fraction; 3]) * (2.0 * fraction / f64::from(steps));
    }
    assert!((held - Vec3::ONE).max_element().abs() < 1e-4, "{held:?}");
    let (middle, rim) = (DARKENING.across([0.0; 3]), DARKENING.across([0.95; 3]));
    assert!(
        middle.x.min(middle.y).min(middle.z) > 1.0,
        "the middle outshines the mean"
    );
    assert!(rim.z / middle.z < rim.x / middle.x, "blue darkens most");
    assert_eq!(Limb::Even.across([0.7; 3]), Vec3::ONE);
    let apart = DARKENING.across([0.2, 0.5, 0.9]);
    assert!(
        (apart.x - DARKENING.across([0.2; 3]).x).abs() < 1e-15,
        "each its own"
    );
    assert!(
        (apart.z - DARKENING.across([0.9; 3]).z).abs() < 1e-15,
        "each its own"
    );
    // So the light the disc's samples bring is its irradiance still.
    let sky = room();
    let light = sun(Vec3::UP, DARKENING);
    let side = 64u32;
    let mut brought = Vec3::ZERO;
    for pair in grid(side) {
        brought += light.sample(Vec3::ZERO, pair, &sky).expect("up").light;
    }
    brought = brought * (1.0 / f64::from(side * side));
    let whole = light.irradiance();
    assert!(
        (brought.x / whole.x - 1.0).abs() < 2e-3 && (brought.z / whole.z - 1.0).abs() < 2e-3,
        "{brought:?} against {whole:?}"
    );
}

#[test]
fn a_low_sun_is_lifted_and_squashed_by_the_air_and_rimmed_in_blue_above() {
    let elevation = 1.0_f64;
    let sky = open(elevation);
    let toward = Vec3::new(
        0.0,
        mathf::sin(elevation.to_radians()),
        mathf::cos(elevation.to_radians()),
    );
    let light = sun(toward, DARKENING);
    let eye = Vec3::new(0.0, 2.0, 0.0);
    // Every ray its samples arrive along stands higher than the disc does.
    for pair in grid(8) {
        let incidence = light.sample(eye, pair, &sky).expect("above the horizon");
        assert!(incidence.dir.y > toward.y - 0.004_65, "{:?}", incidence.dir);
        let density = light.density(eye, incidence.dir, f64::INFINITY, &sky);
        assert!(
            (density / incidence.density - 1.0).abs() < 0.02,
            "{density} against {}",
            incidence.density
        );
    }
    // Scan the disc where it is seen: up its middle and across it.
    let seen = |rise: f64, across: f64| {
        let dir = Vec3::new(mathf::sin(across), mathf::sin(rise), mathf::cos(rise)).normalized();
        light.disc(eye, dir, &sky)
    };
    // Up from a degree below the disc's true middle to a degree above it,
    // where green light meets it, in steps of ten microradians.
    let step = 1e-5;
    let offsets = || (0..3500u32).map(|index| (f64::from(index) - 1750.0) * step);
    let base = elevation.to_radians();
    let lit: Vec<f64> = offsets()
        .filter(|&offset| seen(base + offset, 0.0).y > 0.0)
        .collect();
    let (bottom, top) = (
        lit.first().copied().expect("the disc is seen"),
        lit.last().copied().expect("the disc is seen"),
    );
    let middle = base + f64::midpoint(bottom, top);
    assert!(middle > base + 0.3_f64.to_radians(), "lifted: {middle}");
    let tall = top - bottom;
    let wide = step
        * f64::from(
            u32::try_from(
                offsets()
                    .filter(|&offset| seen(middle, offset).y > 0.0)
                    .count(),
            )
            .unwrap_or(0),
        );
    assert!(wide > 0.0092 && wide < 0.0094, "{wide} across");
    let squash = tall / wide;
    assert!((0.8..0.95).contains(&squash), "{squash}");
    let highest = |channel: usize| {
        offsets()
            .rev()
            .find(|&offset| seen(base + offset, 0.0).along(channel) > 0.0)
            .unwrap_or(0.0)
    };
    assert!(
        highest(2) > highest(0),
        "blue lifted above red: {} {}",
        highest(2),
        highest(0)
    );
}

/// The air squashes a low sun's disc into a smaller solid angle, so less of
/// its light falls than the air keeps: the same share on a surface, sampled
/// across the disc, as on the air and the cloud it lights.
#[test]
fn a_low_sun_lights_a_surface_as_it_lights_the_air_spread_as_the_air_squashes_it() {
    let elevation = 3.0_f64;
    let sky = open(elevation);
    let toward = Vec3::new(
        0.0,
        mathf::sin(elevation.to_radians()),
        mathf::cos(elevation.to_radians()),
    );
    let light = sun(toward, Limb::Even);
    let eye = Vec3::new(0.0, 2.0, 0.0);
    let count = 256;
    let sampled = light
        .samples(eye, count, &sky)
        .fold(Vec3::ZERO, |sum, incidence| {
            sum + incidence.light * incidence.kept
        })
        * (1.0 / (f64::from(count) * light.irradiance().y));
    let Dome::Air(atmosphere) = &sky.dome else {
        unreachable!("an open sky");
    };
    let air = atmosphere.sunlight(eye.y, toward.y);
    let kept = atmosphere.arriving(eye.y, toward).expect("up").kept;
    assert!(air.y < 0.97 * kept.y, "squashed: {air:?} of {kept:?}");
    let apart = (sampled - air)
        .max_element()
        .abs()
        .max((air - sampled).max_element().abs());
    assert!(
        apart < 0.01 * air.max_element(),
        "{sampled:?} against {air:?}"
    );
}

/// What a sample of a low sun brings, each channel from where its own bent
/// light truly comes, is what the disc shows along the sample's way.
#[test]
fn a_sample_of_a_low_sun_brings_each_channel_as_its_disc_shows_it_there() {
    let elevation = 0.6_f64;
    let sky = open(elevation);
    let toward = Vec3::new(
        0.0,
        mathf::sin(elevation.to_radians()),
        mathf::cos(elevation.to_radians()),
    );
    let light = sun(toward, DARKENING);
    let eye = Vec3::new(0.0, 2.0, 0.0);
    let mut compared = 0;
    for pair in grid(24) {
        let incidence = light.sample(eye, pair, &sky).expect("above the horizon");
        let brought = incidence.light * incidence.density;
        let (sources, kept) = sky.beyond(eye, incidence.dir).expect("above the horizon");
        // The air's light kept, read along the ray's way out against along
        // the light's way in, agrees as the tables' resolution allows.
        assert!((kept.y / incidence.kept.y - 1.0).abs() < 1e-2, "{kept:?}");
        let shown = light.disc(eye, incidence.dir, &sky);
        for (channel, source) in sources.iter().enumerate() {
            let (brought, shown) = (
                brought.along(channel),
                shown.along(channel) / kept.along(channel),
            );
            // Taken where the green light comes from, a channel near the rim
            // would stand a hundredth of the radius or two off where its own
            // does, and darken a tenth more or less. In the last hundredth
            // the limb steepens past what the tables' bends resolve.
            let cos = source.dot(toward);
            let fraction = mathf::sqrt((1.0 - cos) * (1.0 + cos)) / mathf::sin(0.004_65);
            if brought > 0.0 && shown > 0.0 && fraction < 0.99 {
                compared += 1;
                assert!(
                    (brought / shown - 1.0).abs() < 5e-3,
                    "{channel} at {fraction}: {brought} against {shown}"
                );
            }
        }
    }
    assert!(compared > 1500, "{compared}");
}

#[test]
fn an_orb_is_sampled_on_its_near_side_and_lights_as_its_solid_angle_says() {
    let sky = room();
    let (centre, radius) = (Vec3::new(0.0, 3.0, 0.0), 0.5);
    let orb = Light::Orb {
        centre,
        radius,
        radiance: Vec3::splat(10.0),
    };
    let ball = Shape::Sphere { centre, radius };
    let p = Vec3::ZERO;
    let mut irradiance = 0.0;
    let side = 48u32;
    for pair in grid(side) {
        let incidence = orb.sample(p, pair, &sky).expect("in sight");
        let hit = ball
            .intersect(
                &Ray::new(p, incidence.dir),
                1e-9,
                f64::INFINITY,
                Geometry {
                    faces: &[],
                    fields: &[],
                    prototypes: &[],
                    lawns: &[],
                },
            )
            .expect("every sampled direction meets the orb");
        assert!(
            (hit.t - incidence.distance).abs() < 1e-6,
            "{} against {}",
            hit.t,
            incidence.distance
        );
        assert!((orb.density(p, incidence.dir, hit.t, &sky) - incidence.density).abs() < 1e-9);
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
        orb.sample(centre, (0.5, 0.5), &sky).is_none(),
        "nothing is lit from inside a lamp"
    );
}

#[test]
fn a_panel_lights_its_face_alone() {
    let sky = room();
    let panel = Light::Panel {
        corner: Vec3::new(-1.0, 4.0, -1.0),
        edge_u: Vec3::new(2.0, 0.0, 0.0),
        edge_v: Vec3::new(0.0, 0.0, 2.0),
        radiance: Vec3::splat(5.0),
    };
    // Facing `edge_u × edge_v`, which points down onto the floor.
    for pair in grid(8) {
        let incidence = panel
            .sample(Vec3::ZERO, pair, &sky)
            .expect("beneath its face");
        assert!(incidence.dir.y > 0.0);
        let dir = incidence.dir;
        assert!(
            (panel.density(Vec3::ZERO, dir, incidence.distance, &sky) - incidence.density).abs()
                < 1e-9
        );
    }
    assert!(
        panel
            .sample(Vec3::new(0.0, 8.0, 0.0), (0.5, 0.5), &sky)
            .is_none(),
        "its back is dark"
    );
    assert!((panel.seen(Vec3::UP) - Vec3::splat(5.0)).length() < 1e-12);
    assert!(panel.seen(-Vec3::UP).max_element() <= 0.0);
}
