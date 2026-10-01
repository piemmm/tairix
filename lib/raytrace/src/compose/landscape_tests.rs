//! Host tests of where a landscape's eye stands on its land.

use tairix_parallel::Threaded;
use tairix_rng::NonCryptoRng;

use super::*;
use crate::compose::{Composed, Stage};

/// The landing `plan` composes under `seed`, its far land built and waiting
/// to be sited, and the stage it is set out on.
fn surveyed(
    plan: fn(&mut Stage, &mut Dice) -> Option<Composed>,
    seed: u64,
) -> (Stage, Landing, Dice) {
    let mut dice = Dice(NonCryptoRng::seed_from_u64(seed));
    let mut stage = Stage::new().expect("a stage");
    let Some(Composed::Landed(mut landing)) = plan(&mut stage, &mut dice) else {
        panic!("a landscape stands on a land");
    };
    let runner = Threaded::new(8);
    while !landing
        .build
        .step(&mut stage.fields, &runner)
        .expect("builds")
    {}
    assert!(
        landing.build.waiting(),
        "the far land stands, waiting to be sited"
    );
    (stage, landing, dice)
}

/// Every landscape's eye stands above the land and clear of the water, with
/// somewhere to look.
#[test]
fn a_landscapes_eye_stands_above_its_land_and_clear_of_its_water() {
    let plans: [fn(&mut Stage, &mut Dice) -> Option<Composed>; 7] =
        [meadow, forest, alpine, coast, desert, winter, canyon];
    for (index, plan) in plans.into_iter().enumerate() {
        let (stage, landing, mut dice) = surveyed(plan, 5);
        let survey = landing.build.survey(&stage.fields).expect("a survey");
        let (vantage, siting) = landing.scheme.site(&mut dice, &survey).expect("sited");
        let Vantage { eye, heading } = vantage.expect("a landscape sites its eye");
        let ground = survey.height(eye.x, eye.z);
        let water = survey.water(eye.x, eye.z).unwrap_or(f64::NEG_INFINITY);
        assert!(
            eye.y > ground + 0.5 && eye.y > water + 0.5,
            "{index}: {eye:?} over {ground}"
        );
        assert!(heading.is_finite());
        let (focus, near) = (siting.focus, survey.finest_reach());
        assert!(
            (eye.x - focus.0).abs() < near && (eye.z - focus.1).abs() < near,
            "{index}: the eye stands on its near land"
        );
    }
}

/// A frozen pond's ice lies level over the hollow it fills to where the
/// water would spill, ends within a few of its radii every way, and meets
/// the snow at its edge: a lip at its outlet, never a sheet over a drop.
#[test]
fn a_frozen_pond_lies_level_in_its_hollow() {
    for seed in 0..3 {
        let (stage, landing, _) = surveyed(winter, seed);
        let Scheme::Winter { pond } = landing.scheme else {
            panic!("a winter scheme");
        };
        let survey = landing.build.survey(&stage.fields).expect("a survey");
        let level = survey.water(0.0, 0.0).expect("ice at the pond's middle");
        for step in 0..96u32 {
            let angle = TAU * f64::from(step) / 96.0;
            let (sin, cos) = (mathf::sin(angle), mathf::cos(angle));
            let edge = (1..=300u32)
                .map(|out| 0.01 * pond * f64::from(out))
                .find(
                    |&distance| match survey.water(distance * sin, distance * cos) {
                        Some(ice) => {
                            assert!(
                                (ice - level).abs() < 1e-6,
                                "{seed}: ice at {ice:.3}, not {level:.3}"
                            );
                            false
                        }
                        None => true,
                    },
                )
                .unwrap_or_else(|| panic!("{seed}: the ice runs on toward {angle:.2}"));
            let ground = survey.height(edge * sin, edge * cos);
            assert!(
                ground > level - 1.0,
                "{seed}: the ice stands {:.2} proud toward {angle:.2}",
                level - ground
            );
        }
    }
}

/// A clearing a scene is set out in stays level however water wore the
/// land about it, out to where its wandering edge may come in.
#[test]
fn a_backdrops_clearing_stays_level_once_its_land_is_built() {
    let mut dice = Dice(NonCryptoRng::seed_from_u64(2));
    let mut stage = Stage::new().expect("a stage");
    let backdrop = backdrop(
        &mut stage,
        &mut dice,
        (25.0, 0.0),
        &GREEN,
        (&[Kind::Oak], Season::Summer),
    )
    .expect("a backdrop");
    let mut build = backdrop.build;
    let runner = Threaded::new(8);
    loop {
        let done = build.step(&mut stage.fields, &runner).expect("builds");
        if done && build.waiting() {
            build.site((0.0, 0.0), (0.0, 0.0), None).expect("sited");
        } else if done {
            break;
        }
    }
    let land = build.finish().expect("built");
    for step in 0..64u32 {
        let angle = TAU * f64::from(step) / 64.0;
        for distance in [0.0, 5.0, 12.5, 18.75] {
            let (x, z) = (distance * mathf::sin(angle), distance * mathf::cos(angle));
            let height = land.height(&stage.fields, x, z);
            assert!(height.abs() < 1e-3, "{height} at {x:.1}, {z:.1}");
        }
    }
}
