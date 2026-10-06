//! Host tests of where a landscape's eye stands on its land.

use tairix_parallel::Threaded;
use tairix_rng::NonCryptoRng;

use super::*;
use crate::compose::{Composed, Composition, Job, Progress, Setting, Stage};
use crate::detail::Detail;
use crate::heightfield::Heightfield;
use crate::land::{Build, Fields, Plan, Wear};
use crate::shape::{Aabb, Shape};
use crate::terrain::{Landform, Terrain};

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

/// A land of `form`, `datum` taken as nought, about a clearing if it has one
/// and a sea at `sea` if it has one, unworn and smooth, its far land built
/// and waiting to be sited: its grids and its build.
fn unworn(
    form: Landform,
    datum: f64,
    clearing: Option<(f64, f64)>,
    sea: Option<f64>,
) -> (Vec<Heightfield>, Build) {
    let reach = 1200.0;
    let plan = Plan {
        relief: Terrain {
            form,
            datum,
            centre: (0.0, 0.0),
            radius: reach,
            rim: None,
            tilt: (0.0, 0.0),
            clearing,
        },
        reach,
        sea,
        wear: Wear {
            passes: 1,
            incision: 0.0,
            creep: 0.0,
            repose: 0.9,
            infill: 0.0,
            strata: None,
        },
        rivers: None,
        road: None,
        farming: None,
        roughness: 0.0,
        ridges: 0.0,
        droplets: 0.0,
        cells: (32, 128),
        nests: [None, None],
        near_water: None,
        horizon: None,
        snow_line: None,
        snowpack: None,
        pond: None,
        growth: 1.0,
        seed: 3,
    };
    let origin = (-reach, -reach);
    let mut fields = alloc::vec![
        Heightfield::new(plan.cells.1, origin, plan.far_step(), false).expect("a grid"),
        Heightfield::new(1, origin, 1.0, false).expect("a grid"),
    ];
    let grids = Fields {
        far: 0,
        nests: [None, None],
        water: Some(1),
        near_water: None,
        horizon: None,
    };
    let mut build = Build::new(plan, grids).expect("a build");
    let runner = Threaded::new(4);
    while !build.step(&mut fields, &runner).expect("builds") {}
    assert!(build.waiting(), "the far land stands, waiting to be sited");
    (fields, build)
}

/// Level land `ground` high.
fn level(ground: f64) -> (Vec<Heightfield>, Build) {
    let flat = Landform::Hills {
        scale: 100.0,
        height: 0.0,
        seed: 1,
    };
    unworn(flat, -ground, None, None)
}

/// On dry ground the eye stands its rise above it, exactly.
#[test]
fn an_eye_stands_its_rise_above_the_ground() {
    for (ground, seed) in [(10.0, 1), (40.0, 2), (-3.0, 3)] {
        let (fields, build) = level(ground);
        let survey = build.survey(&fields).expect("a survey");
        let mut dice = Dice(NonCryptoRng::seed_from_u64(seed));
        let eye = terrace_vantage(&survey, &mut dice, 5.0, 3.0).eye;
        assert!(
            (eye.y - survey.height(eye.x, eye.z) - 3.0).abs() < 1e-9,
            "{ground}: the eye at {}",
            eye.y
        );
        assert!((survey.height(eye.x, eye.z) - ground).abs() < 1e-6);
    }
}

/// With nowhere dry to stand, the eye still stands its rise above the water
/// it is over — the sea's surface, not the bed beneath it.
#[test]
fn an_eye_with_nowhere_dry_to_stand_stays_above_the_water() {
    let flat = Landform::Hills {
        scale: 100.0,
        height: 0.0,
        seed: 1,
    };
    let (fields, build) = unworn(flat, 8.0, None, Some(0.0));
    let survey = build.survey(&fields).expect("a survey");
    for seed in 0..16 {
        let mut dice = Dice(NonCryptoRng::seed_from_u64(seed));
        let Vantage { eye, heading } = terrace_vantage(&survey, &mut dice, 5.0, 3.0);
        assert!(
            survey.wet_at(eye.x, eye.z),
            "{seed}: somewhere dry after all"
        );
        assert!((eye.y - 3.0).abs() < 1e-9, "{seed}: the eye at {}", eye.y);
        assert!(heading.is_finite());
        assert_eq!(survey.water(eye.x, eye.z), Some(0.0));
        assert!((stand(&survey, (eye.x, eye.z), 1.5).y - 1.5).abs() < 1e-9);
    }
}

/// Of the level ground about it, a canyon's eye stands on the lowest — the
/// canyon's floor, not a mesa's top. Its terraces stand twice its `terrace`
/// apart and it looks over a few spots, so it stands less than a terrace and
/// a half above the lowest dry, level ground anywhere it looks.
#[test]
fn an_eye_stands_on_the_lowest_ground_in_sight() {
    for seed in 0..8 {
        let (stage, landing, mut dice) = surveyed(canyon, seed);
        let Scheme::Canyon { terrace } = landing.scheme else {
            panic!("a canyon's scheme");
        };
        let survey = landing.build.survey(&stage.fields).expect("a survey");
        let rise = 3.0;
        let eye = terrace_vantage(&survey, &mut dice, terrace, rise).eye;
        let ground = eye.y - rise;
        let (centre, reach) = survey.extent();
        let spread = 0.3 * reach;
        let lowest = (0..=64u32)
            .flat_map(|row| (0..=64u32).map(move |column| (row, column)))
            .map(|(row, column)| {
                (
                    centre.0 - spread + 2.0 * spread * f64::from(column) / 64.0,
                    centre.1 - spread + 2.0 * spread * f64::from(row) / 64.0,
                )
            })
            .filter(|&(x, z)| !survey.wet_at(x, z) && survey.lie(x, z).upright >= 0.9)
            .map(|(x, z)| survey.height(x, z))
            .fold(f64::INFINITY, f64::min);
        assert!(
            ground < lowest + 1.5 * terrace,
            "{seed}: on {ground:.1}, the lowest level ground {lowest:.1}, a terrace {terrace:.1}"
        );
    }
}

/// A sculpture stands on dry ground, in the frame, and nothing set out after
/// it — trees, shrubs, stones, the grounds' own pieces — stands within the
/// ground it claims.
#[test]
fn a_sculpture_stands_on_dry_ground_clear_of_every_other_piece() {
    let runner = Threaded::new(8);
    for seed in 0..24 {
        let mut composition =
            Composition::new(Setting::Sculpture, seed, (96, 54), Detail::Simple).expect("composes");
        let mut sculpture = None;
        let mut land = None;
        while composition.seen.is_none() {
            let job = composition.jobs.pop_front().expect("work remains");
            let laying = matches!(job, Job::Land(_));
            let before = (
                composition.stage.objects.len(),
                composition.stage.footprints.len(),
            );
            match composition
                .run(job, &runner)
                .unwrap_or_else(|| panic!("{seed}: a sculpture's scene refused"))
            {
                Progress::Again(Job::Plant(planting)) if laying => {
                    sculpture = Some(before);
                    land = Some(planting.land.clone());
                    composition.jobs.push_front(Job::Plant(planting));
                }
                Progress::Again(unfinished) => composition.jobs.push_front(unfinished),
                // A land with nothing to grow on it, the dunes', is let go
                // as soon as its scene is set out; it holds no water.
                Progress::Done if laying => sculpture = Some(before),
                Progress::Done => {}
            }
        }
        let (from, claimed) = sculpture.expect("set out with its land");
        let stage = &composition.stage;
        let geometry = stage.geometry();
        let (centre, reach) = stage
            .footprints
            .circle(claimed)
            .expect("the sculpture claims its ground first");
        let kind = |shape: &Shape| match shape {
            Shape::Torus { .. } => Some(0),
            Shape::Hull { .. } => Some(1),
            Shape::Sphere { .. } => Some(2),
            _ => None,
        };
        let first = kind(&stage.objects[from].shape).expect("a sculpture first");
        let pieces = stage.objects[from..]
            .iter()
            .take_while(|object| kind(&object.shape) == Some(first))
            .count();
        let mut footprint = Aabb::EMPTY;
        for object in &stage.objects[from..from + pieces] {
            let bounds = object.shape.bounds(geometry).expect("bounded");
            footprint = footprint.union(bounds);
            let at = bounds.centre();
            assert!(
                land.as_ref()
                    .is_none_or(|land| !land.grids.wet_at(&stage.fields, at.x, at.z)),
                "{seed}: a piece of the sculpture stands in water at {at:?}"
            );
        }
        assert!(
            land.as_ref()
                .is_none_or(|land| !land.grids.wet_at(&stage.fields, centre.0, centre.1)),
            "{seed}: the sculpture's ground is under water"
        );
        let camera = &composition.seen.as_ref().expect("seen").1;
        assert!(
            camera
                .project(footprint.centre())
                .is_some_and(|(x, y)| x.abs() <= 1.0 && y.abs() <= 1.0),
            "{seed}: the sculpture out of the frame, the eye at {:?}",
            camera.eye()
        );
        for (index, object) in stage.objects.iter().enumerate() {
            if (from..from + pieces).contains(&index) {
                continue;
            }
            let at = match object.shape {
                Shape::Instance { pose, .. } => pose.at,
                Shape::Land { .. } | Shape::Lawn { .. } | Shape::Quad { .. } => continue,
                _ => match object.shape.bounds(geometry) {
                    Some(bounds) => bounds.centre(),
                    None => continue,
                },
            };
            let apart = mathf::hypot(at.x - centre.0, at.z - centre.1);
            assert!(
                apart >= reach,
                "{seed}: object {index} stands {apart:.2} from the sculpture's middle, within its {reach:.2}"
            );
        }
    }
}

/// A valley's eye, however it is sited — up its side by its bridge, or
/// overlooking it where it has none — stands on dry ground off its road, no
/// steeper than one keeps one's footing on a valley's side, its rise above
/// it.
#[test]
fn a_valleys_eye_stands_on_dry_level_ground_off_its_road() {
    for seed in 0..6 {
        let (stage, landing, mut dice) = surveyed(valley, seed);
        let survey = landing.build.survey(&stage.fields).expect("a survey");
        let (vantage, siting) = landing
            .scheme
            .site(&mut dice, &survey, &tairix_parallel::SERIAL)
            .expect("sited");
        let Vantage { eye, heading } = vantage.expect("a valley sites its eye");
        let lie = survey.lie(eye.x, eye.z);
        assert!(
            !survey.wet_at(eye.x, eye.z),
            "{seed}: the eye stands in water"
        );
        assert!(lie.road <= 0.05, "{seed}: the eye stands on the road");
        assert!(
            lie.upright >= 0.8,
            "{seed}: on ground as steep as {}",
            lie.upright
        );
        let rise = eye.y - survey.surface(eye.x, eye.z);
        assert!(
            (1.6..=3.0).contains(&rise),
            "{seed}: {rise} above the ground"
        );
        assert!(heading.is_finite());
        assert_eq!(siting.focus, (eye.x, eye.z));
    }
}

/// A sea of dunes from `seed`, built and sited as a composition builds it
/// but kept once its scene would be set out: its stage, its land, where its
/// eye stands, and the draws that would set its scene out.
fn dunes(seed: u64) -> Option<(Stage, Land, Vantage, Dice)> {
    let mut stage = Stage::new(Detail::Simple.densities())?;
    let mut dice = Dice(NonCryptoRng::seed_from_u64(seed));
    let Composed::Landed(mut landing) = desert_of(&mut stage, &mut dice, true)? else {
        return None;
    };
    let runner = Threaded::new(8);
    let mut vantage = None;
    loop {
        while !landing.build.step(&mut stage.fields, &runner)? {}
        if !landing.build.waiting() {
            break;
        }
        let survey = landing.build.survey(&stage.fields)?;
        let (sited, siting) = landing.scheme.site(&mut dice, &survey, &runner)?;
        vantage = sited;
        landing.build.site(siting.focus, siting.lead, (siting.path.as_deref(), &siting.cuttings))?;
    }
    let land = landing.build.finish()?;
    Some((stage, land, vantage?, dice))
}

/// A pyramid out on the dunes stands clear of the desert's road, as every
/// piece stands off a way.
#[test]
fn a_deserts_pyramids_stand_clear_of_its_road() {
    let mut roads = 0;
    for seed in 0..24 {
        let (mut stage, land, vantage, mut dice) = dunes(seed).expect("a sea of dunes");
        // Whether its road runs through the ground a pyramid is drawn on.
        let crossed = (8..28)
            .flat_map(|step| (-5..=5).map(move |turn| (50.0 * f64::from(step), 0.1 * f64::from(turn))))
            .map(|(distance, turn)| ahead(&vantage, distance, turn))
            .any(|(x, z)| land.grids.lie(&stage.fields, x, z).road > 0.05);
        roads += usize::from(crossed);
        let before = stage.footprints.len();
        erg(&mut stage, &mut dice, &land, &vantage).expect("its scene");
        for index in before..stage.footprints.len() {
            let Some((centre, reach)) = stage.footprints.circle(index) else {
                continue;
            };
            // Nothing but a pyramid claims ground this broad.
            if reach < 40.0 {
                continue;
            }
            for (x, z) in rim_and_middle(centre, reach) {
                let lie = land.grids.lie(&stage.fields, x, z);
                assert!(lie.road <= 0.05 && lie.path <= 0.3, "{seed}: a pyramid stands on a way at {x}, {z}");
            }
        }
    }
    assert!(roads > 0, "no desert's road ran where its pyramids are drawn");
}
