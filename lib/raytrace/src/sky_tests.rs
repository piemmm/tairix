//! Host tests of the sky: a room's walls, which bend no light; the open
//! sky's stars, through its air and never below its horizon; and its banks
//! of cloud, the nearer before the farther, both shading the ground.

use tairix_util::mathf;

use super::{Dome, Gradient, Seeing, Sky};
use crate::atmosphere::{Air, Atmosphere};
use crate::body;
use crate::cloud::{Cloudbank, Deck, Lighting, Matter, SUNLIGHT_LEVELS, SUN_COSINES};
use crate::stars::Starfield;
use crate::vector::Vec3;

const ZENITH: Vec3 = Vec3::new(0.1, 0.2, 0.6);
const HORIZON: Vec3 = Vec3::new(0.7, 0.75, 0.8);
const GROUND: Vec3 = Vec3::splat(0.2);

/// A ray seen from the eye, a pixel of a milliradian across.
const CLOSE: Seeing = Seeing {
    fine: true,
    spread: Some(1e-3),
    jitter: 0.5,
    air: 0.5,
};

fn room() -> Sky {
    Sky {
        dome: Dome::Gradient(Gradient {
            zenith: ZENITH,
            horizon: HORIZON,
            ground: GROUND,
        }),
        stars: None,
        low: None,
        high: None,
    }
}

#[test]
fn a_rooms_walls_run_from_the_horizon_up_to_the_zenith_and_down_to_the_floor() {
    let sky = room();
    let seen = |dir: Vec3| sky.radiance(Vec3::ZERO, dir, CLOSE);
    assert!((seen(Vec3::UP) - ZENITH).length() < 1e-12);
    assert!((seen(Vec3::new(1.0, 0.0, 0.0)) - HORIZON).length() < 1e-12);
    assert!((seen(-Vec3::UP) - GROUND).length() < 1e-12);
    // Blue deepens steadily with height.
    let mut last = seen(Vec3::new(1.0, 0.0, 0.0)).x;
    for step in 1..=20u32 {
        let up = f64::from(step) / 20.0;
        let red = seen(Vec3::new(mathf::sqrt(1.0 - up * up), up, 0.0)).x;
        assert!(red <= last + 1e-12);
        last = red;
    }
}

#[test]
fn a_rooms_walls_bend_no_light_and_dim_none() {
    let sky = room();
    let (point, dir) = (
        Vec3::new(1.0, 2.0, 3.0),
        Vec3::new(0.2, 0.1, 0.9).normalized(),
    );
    let arriving = sky.arriving(point, dir).expect("a room hides nothing");
    assert_eq!(arriving.dir, dir);
    assert!((arriving.stretch - 1.0).abs() < 1e-15 && arriving.kept == Vec3::ONE);
    assert_eq!(sky.leaving(point, dir), Some((dir, 1.0)));
    assert_eq!(sky.beyond(point, dir), Some(([dir; 3], Vec3::ONE)));
    assert_eq!(sky.transmitted(point, dir), Vec3::ONE);
    assert_eq!(sky.reach(0.9).to_bits(), 0.9_f64.to_bits());
}

/// An open sky of `air` and `stars`, built, and no cloud.
pub(crate) fn open(air: Air, stars: Option<Starfield>) -> Sky {
    let mut sky = Sky {
        dome: Dome::Air(Atmosphere::new(air).expect("tables fit")),
        stars,
        low: None,
        high: None,
    };
    while !sky.build(&tairix_parallel::SERIAL).expect("fits") {}
    sky
}

/// An open sky by night: the full moon well up, the stars, and no cloud.
fn night() -> (Sky, Vec3) {
    let toward = Vec3::new(0.0, mathf::sin(0.6), mathf::cos(0.6));
    let eye = Vec3::new(0.0, 2.0, 0.0);
    let air = Air {
        sun: toward,
        solar: body::full_moon(toward).irradiance(),
        base: 100.0,
        haze: 1.0,
        albedo: Vec3::splat(0.15),
        eye,
    };
    (open(air, Some(Starfield::new())), eye)
}

#[test]
fn by_night_the_stars_are_points_over_the_moonlit_air_and_none_below_the_horizon() {
    let (sky, eye) = night();
    let Dome::Air(atmosphere) = &sky.dome else {
        unreachable!("an open sky");
    };
    let (mut points, mut total) = (0u32, 0u32);
    for i in 0..300u32 {
        for j in 0..300u32 {
            let (a, b) = (f64::from(i) / 300.0, f64::from(j) / 300.0);
            let dir = Vec3::new(0.4 * a - 0.2, 0.8, 0.4 * b - 0.2).normalized();
            let (seen, air) = (sky.radiance(eye, dir, CLOSE), atmosphere.sky(dir));
            assert!(seen.y >= air.y, "starlight only adds");
            total += 1;
            points += u32::from(seen.y > 3.0 * air.y);
        }
    }
    // Some dozen stars outshine the moonlit air here: points, not a haze.
    assert!(points > 0 && points < total / 50, "{points} of {total}");
    for step in 0..60u32 {
        let around = f64::from(step) * 0.1;
        let below = Vec3::new(mathf::cos(around), -0.3, mathf::sin(around)).normalized();
        assert_eq!(sky.radiance(eye, below, CLOSE), atmosphere.sky(below));
    }
}

/// A bank of one deck of `form`, stood about the eye at the scene's middle,
/// built, and lit evenly from the sun above.
fn bank(deck: Deck) -> Cloudbank {
    let sun = Vec3::new(0.2, 0.9, 0.3).normalized();
    let mut bank = Cloudbank::new([Some(deck), None], 12_000.0, sun, LEVEL).expect("fits");
    bank.stand(Vec3::ZERO).expect("levels fit");
    while !bank.step(&tairix_parallel::SERIAL, None).expect("held") {}
    bank.light_by(Lighting {
        sunlight: alloc::vec![Vec3::splat(20.0); SUNLIGHT_LEVELS * SUN_COSINES],
        cosines: (-1.0, 1.0),
        toward: sun,
        above: Vec3::new(0.6, 0.8, 1.2),
        below: Vec3::splat(0.3),
    });
    bank
}

/// The scene's level's distance from the Earth's centre, at the sea.
const LEVEL: f64 = crate::atmosphere::GROUND * 1000.0;

/// A day's air with the sun `elevation` degrees up, seen from an eye two
/// metres above the sea.
fn day(elevation: f64) -> Air {
    let radians = elevation.to_radians();
    Air {
        sun: Vec3::new(0.0, mathf::sin(radians), mathf::cos(radians)),
        solar: body::sun(Vec3::UP).irradiance(),
        base: 0.0,
        haze: 1.0,
        albedo: Vec3::splat(0.2),
        eye: Vec3::new(0.0, 2.0, 0.0),
    }
}

/// An open sky of `air` under a bank of `deck`, stood about the eye, built
/// and lit by the air.
fn under(air: Air, deck: Deck) -> Sky {
    let mut low =
        Cloudbank::new([Some(deck), None], 28_000.0, air.sun, air.ground()).expect("fits");
    low.stand(air.eye).expect("levels fit");
    let mut sky = Sky {
        dome: Dome::Air(Atmosphere::new(air).expect("tables fit")),
        stars: None,
        low: Some(low),
        high: None,
    };
    while !sky.build(&tairix_parallel::SERIAL).expect("fits") {}
    sky
}

/// An overcast runs on to the horizon, where the old bank stopped short of
/// it in a band of clear sky; and the air beneath it, which its cloud
/// shades, is grey rather than lit as a clear day's haze is.
#[test]
fn an_overcast_runs_on_to_the_horizon_and_greys_the_air_beneath_it() {
    let air = day(35.0);
    let (overcast, clear) = (under(air, CEILING), open(air, None));
    let Some(bank) = overcast.low.as_ref() else {
        unreachable!("built with a bank");
    };
    for step in 0..48u32 {
        let around = f64::from(step) * 0.52;
        let elevation = (0.05 + 0.25 * f64::from(step % 12)).to_radians();
        let dir = Vec3::new(
            mathf::cos(elevation) * mathf::cos(around),
            mathf::sin(elevation),
            mathf::cos(elevation) * mathf::sin(around),
        );
        let seen = bank
            .seen(air.eye, dir, true, 0.5)
            .and_then(|seen| seen.cloud);
        let (_, kept, depth) = seen.unwrap_or_else(|| panic!("{dir:?} meets the overcast"));
        assert!(kept < 0.05, "{dir:?}: {kept} shows through");
        let (under, open) = (
            overcast.radiance(air.eye, dir, CLOSE),
            clear.radiance(air.eye, dir, CLOSE),
        );
        assert!(
            under.y < 0.75 * open.y,
            "{dir:?} at {depth}: {under:?} under the overcast, {open:?} clear"
        );
    }
    // Low down the overcast lies further off than the old bank ever reached.
    let level = Vec3::new(1.0, 0.0005, 0.0).normalized();
    let (_, _, depth) = bank
        .seen(air.eye, level, true, 0.5)
        .and_then(|seen| seen.cloud)
        .expect("meets it at the horizon");
    assert!(depth > 60_000.0, "{depth}");
}

/// A grey ceiling nothing is seen through.
const CEILING: Deck = Deck {
    base: 800.0,
    base_spread: 50.0,
    depth: (900.0, 1000.0),
    cover: 1.0,
    heap: 0.3,
    scale: 4000.0,
    stretch: 1.2,
    heading: 0.0,
    thickness: 0.05,
    billow: 600.0,
    fibre: 1.0,
    matter: Matter::Water,
    seed: 3,
};
/// Cirrus high above it.
const STREAKS: Deck = Deck {
    base: 8000.0,
    base_spread: 200.0,
    depth: (600.0, 1200.0),
    cover: 0.5,
    heap: 0.1,
    scale: 2000.0,
    stretch: 6.0,
    heading: 0.5,
    thickness: 1e-3,
    billow: 250.0,
    fibre: 8.0,
    matter: Matter::Ice,
    seed: 9,
};

#[test]
fn the_nearer_bank_hides_the_farther_and_both_shade_the_ground() {
    let (ceiling, cirrus) = (bank(CEILING), bank(STREAKS));
    let overcast = Sky {
        low: Some(ceiling.clone()),
        ..room()
    };
    let both = Sky {
        low: Some(ceiling),
        high: Some(cirrus.clone()),
        ..room()
    };
    let high = Sky {
        high: Some(cirrus),
        ..room()
    };
    let up = Vec3::new(0.1, 0.9, 0.2).normalized();
    let seen = |sky: &Sky| sky.radiance(Vec3::ZERO, up, CLOSE);
    let (under, under_both) = (seen(&overcast), seen(&both));
    assert!(
        (under - under_both).max_element().abs() < 1e-3 * under.max_element(),
        "the ceiling hides the cirrus above it: {under:?} {under_both:?}"
    );
    assert!(
        (seen(&high) - seen(&room())).length() > 1e-6,
        "cirrus alone is seen"
    );
    for step in 0..50u32 {
        let at = Vec3::new(f64::from(step) * 150.0 - 3750.0, 0.0, 400.0);
        let (one, other, together) = (
            overcast.clouded(at, up),
            high.clouded(at, up),
            both.clouded(at, up),
        );
        assert!(
            (together - one * other).abs() < 1e-12,
            "{together} {one} {other}"
        );
    }
}
