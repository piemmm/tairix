//! Host tests of where a landscape's eye stands and where its ice lies.

use core::f64::consts::TAU;

use tairix_rng::NonCryptoRng;
use tairix_util::mathf;

use super::vantage;
use crate::compose::{compose, Dice, Setting};
use crate::material::Finish;
use crate::scene::Form;
use crate::shape::Shape;
use crate::terrain::{Landform, Terrain};

/// Level land at `height`, reaching far past where an eye is looked for.
fn level(height: f64) -> Terrain {
    Terrain {
        form: Landform::Hills {
            scale: 100.0,
            height: 0.0,
            seed: 1,
        },
        datum: -height,
        centre: (0.0, 0.0),
        radius: 5000.0,
        rim: height,
        clearing: None,
    }
}

/// With nowhere dry and level to stand, the eye still stands clear of the
/// water rather than beneath it.
#[test]
fn an_eye_with_nowhere_dry_to_stand_stays_above_the_water() {
    for seed in 0..16 {
        let mut dice = Dice(NonCryptoRng::seed_from_u64(seed));
        let (eye, heading) = vantage(&level(-8.0), &mut dice, (2.0, 30.0), 3.0);
        assert!((eye.y - 5.0).abs() < 1e-9, "{seed}: {}", eye.y);
        assert!(heading.is_finite());
    }
}

/// On dry ground the eye stands `rise` above it.
#[test]
fn an_eye_stands_its_rise_above_the_ground() {
    for (ground, seed) in [(10.0, 1), (40.0, 2)] {
        let mut dice = Dice(NonCryptoRng::seed_from_u64(seed));
        let (eye, _) = vantage(&level(ground), &mut dice, (2.0, 30.0), 3.0);
        assert!((eye.y - ground - 3.0).abs() < 1e-9, "{ground}: {}", eye.y);
    }
}

/// Of the level ground about it, the eye stands on the lowest: a floor
/// levelled among hills, rather than their gentle upper slopes.
#[test]
fn an_eye_stands_on_the_lowest_ground_in_sight() {
    let floor = 5.0;
    let terrain = Terrain {
        form: Landform::Hills {
            scale: 400.0,
            height: 200.0,
            seed: 3,
        },
        datum: 0.0,
        centre: (0.0, 0.0),
        radius: 5000.0,
        rim: 100.0,
        clearing: Some((floor, 600.0)),
    };
    for seed in 0..16 {
        let mut dice = Dice(NonCryptoRng::seed_from_u64(seed));
        let (eye, _) = vantage(&terrain, &mut dice, (2.0, 10.0), 3.0);
        let ground = eye.y - 3.0;
        assert!(
            (floor..=floor + 10.0).contains(&ground),
            "{seed}: on {ground}"
        );
    }
}

/// A frozen pond's ice reaches under its banks all round, so no edge of it
/// stands proud of the snow.
#[test]
fn a_frozen_pond_meets_its_banks_all_round() {
    for seed in 0..24 {
        let parts = compose(Setting::Winter, seed, 16.0 / 9.0).expect("a scene");
        let terrain = parts
            .fills
            .iter()
            .find_map(|fill| match &fill.form {
                Form::Land(terrain) => Some(terrain),
                _ => None,
            })
            .expect("land");
        let (centre, reach, depth) = parts
            .objects
            .iter()
            .find_map(|object| match object.shape {
                Shape::Frustum {
                    pose, top, height, ..
                } if matches!(
                    parts.materials[object.material].finish,
                    Finish::Glass { .. }
                ) =>
                {
                    Some((pose.at, top, height))
                }
                _ => None,
            })
            .expect("ice");
        for step in 0..96u32 {
            let angle = TAU * f64::from(step) / 96.0;
            let (x, z) = (
                centre.x + reach * mathf::sin(angle),
                centre.z + reach * mathf::cos(angle),
            );
            let bank = terrain.height(x, z);
            assert!(
                bank > centre.y + depth,
                "{seed}: the bank at {angle:.2} is {bank:.2}, the ice {:.2}",
                centre.y + depth
            );
        }
    }
}
