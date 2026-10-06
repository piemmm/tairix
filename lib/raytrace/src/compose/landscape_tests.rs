//! Host tests of where a landscape's eye stands on its land.

use tairix_parallel::Threaded;
use tairix_rng::NonCryptoRng;

use super::*;
use crate::compose::{Composed, Stage};
use crate::detail::Detail;

/// The landing `plan` composes under `seed`, its far land built and waiting
/// to be sited, and the stage it is set out on.
fn surveyed(
    plan: fn(&mut Stage, &mut Dice) -> Option<Composed>,
    seed: u64,
) -> (Stage, Landing, Dice) {
    let mut dice = Dice(NonCryptoRng::seed_from_u64(seed));
    let mut stage = Stage::new(Detail::Maximum.densities()).expect("a stage");
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
        let (vantage, siting) = landing
            .scheme
            .site(&mut dice, &survey, &tairix_parallel::SERIAL)
            .expect("sited");
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
    let mut stage = Stage::new(Detail::Maximum.densities()).expect("a stage");
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
            build
                .site((0.0, 0.0), (0.0, 0.0), (None, &[]))
                .expect("sited");
        } else if done {
            break;
        }
    }
    let land = build.finish().expect("built");
    for step in 0..64u32 {
        let angle = TAU * f64::from(step) / 64.0;
        for distance in [0.0, 5.0, 12.5, 18.75] {
            let (x, z) = (distance * mathf::sin(angle), distance * mathf::cos(angle));
            let height = land.grids.height(&stage.fields, x, z);
            assert!(height.abs() < 1e-3, "{height} at {x:.1}, {z:.1}");
        }
    }
}

/// A stream's land always has somewhere by its stream for the eye to stand,
/// the same however its marks are weighed out.
#[test]
fn a_stream_always_has_somewhere_to_be_looked_at_from() {
    for seed in 0..4 {
        let (stage, landing, mut dice) = surveyed(stream, seed);
        let survey = landing.build.survey(&stage.fields).expect("a survey");
        let vantage =
            stream_vantage(&survey, &mut dice, &Threaded::new(3)).expect("a spot by the stream");
        let bits = |runner: &dyn tairix_parallel::JobRunner| {
            stream_vantage(&survey, &mut Dice::keyed(seed, 1), runner).map(|vantage| {
                let Vantage { eye, heading } = vantage;
                [eye.x, eye.y, eye.z, heading].map(f64::to_bits)
            })
        };
        assert_eq!(
            bits(&tairix_parallel::SERIAL),
            bits(&Threaded::new(5)),
            "{seed}"
        );
        let near = survey.rivers().nearest(vantage.eye.x, vantage.eye.z);
        assert!(near.is_some(), "{seed}: the eye stands by its stream");
    }
}

/// An eye that stands away from every stream on a stream's land still
/// composes a scene, its dale without a brook, rather than refusing one.
#[test]
fn a_stream_scene_away_from_its_stream_still_composes() {
    let (mut stage, mut landing, mut dice) = surveyed(stream, 2);
    let survey = landing.build.survey(&stage.fields).expect("a survey");
    let (centre, reach) = survey.extent();
    let dry = (0..64)
        .map(|step| {
            let angle = core::f64::consts::TAU * f64::from(step) / 64.0;
            let at = 0.3 * reach;
            (
                centre.0 + at * mathf::sin(angle),
                centre.1 + at * mathf::cos(angle),
            )
        })
        .find(|&(x, z)| survey.rivers().nearest(x, z).is_none())
        .expect("somewhere away from the streams");
    let eye = Vec3::new(dry.0, survey.height(dry.0, dry.1) + 1.7, dry.1);
    let vantage = Vantage { eye, heading: 0.0 };
    let siting = Siting::ahead(&vantage);
    landing
        .build
        .site(siting.focus, siting.lead, (None, &[]))
        .expect("sited");
    let runner = Threaded::new(8);
    while !landing
        .build
        .step(&mut stage.fields, &runner)
        .expect("builds")
    {}
    let land = landing.build.finish().expect("a land");
    assert!(land.rivers.nearest(eye.x, eye.z).is_none());
    let look = stream_scene(&mut stage, &mut dice, &land, (vantage, Lithology::Granite));
    assert!(look.is_some(), "the scene composes");
    assert!(stage.bed.is_none(), "with no brook to lay");
}

/// A valley's view is taken from partway up its side with its main bridge —
/// the road's over the river lowest in it — in sight, not hidden behind the
/// ground between.
#[test]
fn a_valleys_bridge_is_seen_from_up_its_side() {
    let mut seen = 0;
    for seed in 0..4 {
        let (stage, landing, mut dice) = surveyed(valley, seed);
        let survey = landing.build.survey(&stage.fields).expect("a survey");
        let Some(main) = main_crossing(survey.crossings()).copied() else {
            continue;
        };
        assert!(
            survey
                .crossings()
                .iter()
                .all(|crossing| crossing.water >= main.water),
            "{seed}: the main crossing is not the lowest"
        );
        let Some(vantage) = bridge_vantage(&survey, &mut dice) else {
            continue;
        };
        let middle = Vec3::new(
            f64::midpoint(main.from.x, main.to.x),
            main.deck,
            f64::midpoint(main.from.z, main.to.z),
        );
        assert!(
            in_sight(&|x, z| survey.height(x, z), vantage.eye, middle),
            "{seed}: the bridge hidden"
        );
        assert!(
            vantage.eye.y > main.deck,
            "{seed}: looking up at the bridge from {:?}",
            vantage.eye
        );
        seen += 1;
    }
    assert!(seen >= 2, "only {seen} valleys looked at their bridge");
}

/// Ground rising into the line between an eye and what it looks at hides
/// it; open ground does not.
#[test]
fn a_ridge_hides_what_lies_behind_it() {
    let eye = Vec3::new(0.0, 10.0, 0.0);
    let target = Vec3::new(200.0, 10.0, 0.0);
    let open = |_: f64, _: f64| 0.0;
    let ridged = |x: f64, _: f64| {
        if (90.0..110.0).contains(&x) {
            15.0
        } else {
            0.0
        }
    };
    assert!(in_sight(&open, eye, target));
    assert!(!in_sight(&ridged, eye, target));
}
