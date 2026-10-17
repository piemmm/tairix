extern crate std;

use alloc::vec::Vec;

use super::*;
use crate::compose::{planted_at, Composition, Job, Progress, Setting, Stage};
use crate::detail::Detail;
use crate::wood::{Affinity, Habit, KINDS};
#[cfg(feature = "scene-tests")]
use crate::{compose::Recipe, shape::Shape};

const EYE: Vantage = Vantage {
    eye: Vec3::new(0.0, 1.7, 0.0),
    heading: 0.0,
};

#[test]
fn a_tree_just_ahead_of_the_eye_walls_the_view_off_and_one_aside_does_not() {
    let open = (1.5, 0.3);
    assert!(walls_off(open, &EYE, (0.5, 20.0), 18.0));
    assert!(
        !walls_off(open, &EYE, (0.5, 30.0), 18.0),
        "far enough ahead"
    );
    assert!(!walls_off(open, &EYE, (12.0, 12.0), 18.0), "aside");
    assert!(!walls_off(open, &EYE, (0.5, -10.0), 18.0), "behind");
    let turned = Vantage {
        heading: 3.0 * core::f64::consts::FRAC_PI_2,
        ..EYE
    };
    assert!(
        walls_off(open, &turned, (-10.0, 0.5), 18.0),
        "whichever way the eye looks"
    );
}

#[test]
fn a_wood_is_sown_as_closely_throughout_reaching_only_as_far_as_its_places_fit() {
    let fine = Sowing {
        sown: 4.0,
        about: 40.0,
        far: 1200.0,
    };
    let whole = fine.places();
    let kept = fine.fitted(2.0 * whole);
    assert!((kept.far - fine.far).abs() < 1e-9, "fits as it is");
    // Kept to a third of them, it stops short, its places as close as ever
    // and as many as fit.
    let budget = whole / 3.0;
    let kept = fine.fitted(budget);
    assert!(kept.far < fine.far && (kept.sown - fine.sown).abs() < 1e-12);
    assert!((kept.places() / budget - 1.0).abs() < 1e-9);
    // An eye's round alone too big to hold is cut short too.
    let round = fine.fitted(100.0);
    assert!(round.far < fine.about && (round.about - round.far).abs() < 1e-12);
    assert!((round.places() / 100.0 - 1.0).abs() < 1e-9);
}

/// A region all about an eye out to 40 m and across its view to 120 m stood
/// one by one, then on to 300 m carried on, over a land 2 km across.
const REGION: Region = Region {
    eye: (3.0, -5.0),
    heading: 0.4,
    across: ACROSS,
    about: 40.0,
    near: 120.0,
    far: 300.0,
    traced: ((0.0, 0.0), 1000.0),
};

#[test]
fn a_region_spans_every_place_about_the_eye_and_across_the_view_and_little_more() {
    let sown = Sown { side: 2.0, seed: 7 };
    let spans = REGION.spans(&sown).expect("spans");
    assert!(spans.windows(2).all(|pair| pair[0].row < pair[1].row));
    let spanned = |(column, row): Cell| {
        spans
            .iter()
            .any(|span| span.row == row && (span.columns.0..span.columns.1).contains(&column))
    };
    let (mut near, mut far, mut held) = (0.0, 0.0, 0.0);
    for row in -200..200 {
        for column in -200..200 {
            let cell = (column, row);
            let (at, _) = sown.place(cell).expect("a place");
            held += f64::from(u8::from(spanned(cell)));
            let (dx, dz) = (at.0 - REGION.eye.0, at.1 - REGION.eye.1);
            let distance = mathf::hypot(dx, dz);
            let across = wrapped(mathf::atan2(dx, dz) - REGION.heading).abs() <= ACROSS;
            match REGION.part(at) {
                Some(Part::Near {
                    distance: within,
                    across: facing,
                }) => {
                    assert!(spanned(cell), "{cell:?}");
                    assert!((within - distance).abs() < 1e-9 && facing == across);
                    assert!(distance < 40.0 || (across && distance < 120.0));
                    near += 1.0;
                }
                Some(Part::Far) => {
                    assert!(spanned(cell), "{cell:?}");
                    assert!(across && (120.0..=300.0).contains(&distance));
                    far += 1.0;
                }
                None => assert!(distance >= 40.0 && (!across || distance > 300.0)),
            }
        }
    }
    let sowing = |far: f64| Sowing {
        sown: 2.0,
        about: 40.0,
        far,
    };
    let (expected_near, expected_all) = (sowing(120.0).places(), sowing(300.0).places());
    assert!(
        (near / expected_near - 1.0).abs() < 0.01,
        "{near} of {expected_near}"
    );
    let expected_far = expected_all - expected_near;
    assert!(
        (far / expected_far - 1.0).abs() < 0.01,
        "{far} of {expected_far}"
    );
    assert!(
        held < 1.12 * (near + far),
        "{held} spanned for {}",
        near + far
    );
    // Traced only so far, it spans only the land's own cells.
    let edged = Region {
        traced: ((0.0, 0.0), 50.0),
        ..REGION
    };
    for span in edged.spans(&sown).expect("spans") {
        assert!((-26..=26).contains(&span.row), "{span:?}");
        assert!(span.columns.0 >= -26 && span.columns.1 <= 27, "{span:?}");
    }
}

/// One kind of trees a wood far off and near alike grows, ten metres tall,
/// covering most of the ground.
fn ten_metre_wood() -> Reader {
    let habit = Habit {
        affinity: Affinity::EVEN,
        prototypes: [0; VARIANTS],
        heights: [10.0; VARIANTS],
        bark: 0,
        crown: 0.2,
        scaled: (0.75, 1.35),
    };
    let mut kinds = [None; KINDS];
    kinds[0] = Some(habit);
    Reader {
        woodland: Woodland {
            cover: 0.9,
            patch: 260.0,
            closure: (0.45, 0.8),
            stature: (0.8, 1.0),
            gaps: 0.2,
            most: 100_000,
            open: (1.5, 0.3),
        },
        rooting: crate::wood::ANYWHERE,
        kinds,
        seeds: (3, 4),
        layer: Layer::Canopy,
    }
}

/// The lie of level, green ground.
const GREEN: tairix_land::attributes::Attributes = [0, 128, 0, 0, 255, 0, 0, 0];

/// What a wood stands on a level, green land seen from `vantage`, sown on
/// `sown` over `region` and carried on far off where `beyond` asks: the
/// places stood one by one, the wood far off, and the land's grids.
fn grown(
    vantage: &Vantage,
    (sown, region): (Sown, Region),
    beyond: bool,
) -> (Vec<Offer>, Option<FarWood>, Stage) {
    let mut stage = Stage::new(Detail::Maximum.densities()).expect("a stage");
    let land = crate::compose::testland::land(&mut stage, &|_, _| 0.0, GREEN);
    let reader = ten_metre_wood();
    let far = beyond.then(|| {
        let stretch = Stretch {
            eye: region.eye,
            heading: region.heading,
            across: ACROSS,
            to: region.far,
        };
        FarWood::new((reader, sown), (land.grids, &stage.fields), stretch).expect("a wood")
    });
    let most = usize::try_from(reader.woodland.most).expect("a count");
    let sheltering = Some(reader.widest_room());
    let mut standing = Standing::new(
        &stage,
        (&reader, most),
        (sown, region),
        (far, None, sheltering),
    )
    .expect("standing");
    let runner = tairix_parallel::Threaded::new(4);
    let mut steps = 0;
    while !standing
        .step(&mut stage, (&land, &runner), (vantage, None))
        .expect("stood")
    {
        steps += 1;
        assert!(steps < 100_000);
    }
    (stood(&standing), standing.beyond.take(), stage)
}

/// The trees `standing` stood, as the canopy they make holds them.
fn stood(standing: &Standing) -> Vec<Offer> {
    standing
        .thinned
        .as_ref()
        .and_then(|thinned| thinned.canopy.as_ref())
        .map(|canopy| canopy.trees.clone())
        .expect("a standing that shelters what grows beneath it")
}

/// Every two of `trees` that crowd each other.
fn crowded(trees: &[Offer]) -> Vec<(Offer, Offer)> {
    let mut sorted = trees.to_vec();
    sorted.sort_by(|a, b| a.at.0.total_cmp(&b.at.0));
    let widest = ten_metre_wood().widest_room();
    let mut pairs = Vec::new();
    for (index, tree) in sorted.iter().enumerate() {
        for other in sorted[index + 1..]
            .iter()
            .take_while(|other| other.at.0 - tree.at.0 < widest)
        {
            if crowd(tree, other) {
                pairs.push((*tree, *other));
            }
        }
    }
    pairs
}

#[test]
fn a_wood_stands_the_same_trees_at_a_place_from_wherever_it_is_seen() {
    let sown = lattice(&ten_metre_wood(), 1.4);
    let seen_from = |eye: (f64, f64), heading: f64| {
        let vantage = Vantage {
            eye: Vec3::new(eye.0, 1.7, eye.1),
            heading,
        };
        let region = Region {
            eye,
            heading,
            across: ACROSS,
            about: 60.0,
            near: 70.0,
            far: 70.0,
            traced: ((0.0, 0.0), 200.0),
        };
        grown(&vantage, (sown, region), false).0
    };
    // Well clear of either eye and the view each keeps open before it.
    let window = |trees: Vec<Offer>| -> Vec<Offer> {
        let mut within: Vec<Offer> = trees
            .into_iter()
            .filter(|tree| tree.at.0.abs() < 10.0 && tree.at.1.abs() < 20.0)
            .collect();
        within.sort_by(|a, b| a.at.0.total_cmp(&b.at.0).then(a.at.1.total_cmp(&b.at.1)));
        within
    };
    let west = seen_from((-30.0, 0.0), 0.0);
    let east = seen_from((30.0, 0.0), core::f64::consts::PI);
    assert!(crowded(&west).is_empty() && crowded(&east).is_empty());
    let (west, east) = (window(west), window(east));
    assert!(west.len() > 10, "{}", west.len());
    assert_eq!(west, east);
}

#[test]
fn a_wood_far_off_carries_on_the_one_stood_near_as_one_wood() {
    let sown = lattice(&ten_metre_wood(), 1.4);
    let vantage = Vantage {
        eye: Vec3::new(0.0, 1.7, -120.0),
        heading: 0.0,
    };
    let region = |near: f64| Region {
        eye: (0.0, -120.0),
        heading: 0.0,
        across: ACROSS,
        about: 20.0,
        near,
        far: 190.0,
        traced: ((0.0, 0.0), 200.0),
    };
    let (stood, far, stage) = grown(&vantage, (sown, region(80.0)), true);
    let far = far.expect("carried on");
    let carried: Vec<Offer> = far.kept(&stage.fields).collect();
    assert!(
        stood.len() > 300 && carried.len() > 300,
        "{} and {}",
        stood.len(),
        carried.len()
    );
    let distance = |tree: &Offer| mathf::hypot(tree.at.0, tree.at.1 + 120.0);
    assert!(stood.iter().all(|tree| distance(tree) < 80.0));
    assert!(carried
        .iter()
        .all(|tree| (80.0..=190.0).contains(&distance(tree)) && far.across(tree.at)));
    // Together they are the wood the land grows there stood whole: no trunk
    // in another's room where they meet, and none missing.
    let (whole, _, _) = grown(&vantage, (sown, region(190.0)), false);
    let order = |trees: &mut Vec<Offer>| {
        trees.sort_by(|a, b| a.at.0.total_cmp(&b.at.0).then(a.at.1.total_cmp(&b.at.1)));
    };
    let mut joined: Vec<Offer> = stood.iter().chain(&carried).copied().collect();
    let mut whole = whole;
    order(&mut joined);
    order(&mut whole);
    assert_eq!(joined.len(), whole.len());
    assert!(joined == whole, "the wood far off is the wood stood near");
    assert!(crowded(&whole).is_empty());
}

/// However many cores a runner claims, a step stands no more of a wood's
/// trees than one core sets on the stage, and every tree kept stands.
#[test]
fn a_step_stands_no_more_trees_than_one_core_sets_out() {
    let mut stage = Stage::new(Detail::Maximum.densities()).expect("a stage");
    let land = crate::compose::testland::land(&mut stage, &|_, _| 0.0, GREEN);
    let reader = ten_metre_wood();
    let vantage = Vantage {
        eye: Vec3::new(0.0, 1.7, -60.0),
        heading: 0.0,
    };
    let region = Region {
        eye: (0.0, -60.0),
        heading: 0.0,
        across: ACROSS,
        about: 120.0,
        near: 190.0,
        far: 190.0,
        traced: ((0.0, 0.0), 200.0),
    };
    let most = usize::try_from(reader.woodland.most).expect("a count");
    let sown = lattice(&reader, 1.4);
    let mut standing = Standing::new(&stage, (&reader, most), (sown, region), (None, None, None))
        .expect("a standing");
    let wide = tairix_parallel::Reversed::new(64);
    while standing.thinned.is_none() {
        let stood = standing.step(&mut stage, (&land, &wide), (&vantage, None));
        assert_eq!(stood, Some(false));
    }
    let places = standing
        .thinned
        .as_ref()
        .map_or(0, |thinned| thinned.places);
    assert!(places > STAND_MOST, "{places}");
    loop {
        let before = standing.stood;
        let whole = standing
            .step(&mut stage, (&land, &wide), (&vantage, None))
            .expect("stood");
        assert!(
            standing.stood - before <= STAND_MOST,
            "{}",
            standing.stood - before
        );
        if whole {
            break;
        }
    }
    assert_eq!(standing.stood, places);
}

/// A standing left no room stands nothing, so a wood whose canopy took all
/// its room leaves its subcanopy out rather than thinning it for nothing.
#[test]
fn a_standing_left_no_room_stands_nothing() {
    let mut stage = Stage::new(Detail::Maximum.densities()).expect("a stage");
    let land = crate::compose::testland::land(&mut stage, &|_, _| 0.0, GREEN);
    let reader = ten_metre_wood();
    let vantage = Vantage {
        eye: Vec3::new(0.0, 1.7, 0.0),
        heading: 0.0,
    };
    let region = Region {
        eye: (0.0, 0.0),
        heading: 0.0,
        across: ACROSS,
        about: 40.0,
        near: 40.0,
        far: 40.0,
        traced: ((0.0, 0.0), 200.0),
    };
    let sown = lattice(&reader, 1.4);
    let mut standing = Standing::new(&stage, (&reader, 0), (sown, region), (None, None, None))
        .expect("a standing");
    assert_eq!(standing.most, 0);
    let objects = stage.objects.len();
    let runner = tairix_parallel::Threaded::new(4);
    while !standing
        .step(&mut stage, (&land, &runner), (&vantage, None))
        .expect("stood")
    {}
    assert_eq!((standing.stood, stage.objects.len()), (0, objects));
}

#[test]
fn a_subcanopy_stands_beneath_the_canopy_and_never_in_a_canopy_trees_room() {
    let mut stage = Stage::new(Detail::Maximum.densities()).expect("a stage");
    let land = crate::compose::testland::land(&mut stage, &|_, _| 0.0, GREEN);
    let canopy = ten_metre_wood();
    let vantage = Vantage {
        eye: Vec3::new(0.0, 1.7, 0.0),
        heading: 0.0,
    };
    let region = Region {
        eye: (0.0, 0.0),
        heading: 0.0,
        across: ACROSS,
        about: 70.0,
        near: 70.0,
        far: 70.0,
        traced: ((0.0, 0.0), 200.0),
    };
    let runner = tairix_parallel::Threaded::new(4);
    let stand = |standing: &mut Standing, stage: &mut Stage| {
        while !standing
            .step(stage, (&land, &runner), (&vantage, None))
            .expect("stood")
        {}
        stood(standing)
    };
    let most = usize::try_from(canopy.woodland.most).expect("a count");
    let room = canopy.widest_room();
    let mut above = Standing::new(
        &stage,
        (&canopy, most),
        (lattice(&canopy, 2.8), region),
        (None, None, Some(room)),
    )
    .expect("a canopy");
    let tall = stand(&mut above, &mut stage);
    let (oracle, canopied) = above.sheltered().expect("a canopy to grow beneath");
    assert!(canopied > 40.0, "{canopied}");
    let beneath = Reader {
        layer: Layer::Subcanopy,
        ..canopy
    };
    let region = Region {
        about: canopied,
        near: canopied,
        far: canopied,
        ..region
    };
    let mut below = Standing::new(
        &stage,
        (&beneath, most),
        (lattice(&beneath, 1.7), region),
        (None, Some(oracle), Some(beneath.widest_room())),
    )
    .expect("a subcanopy");
    let short = stand(&mut below, &mut stage);
    assert!(
        tall.len() > 100 && short.len() > 100,
        "{} over {}",
        tall.len(),
        short.len()
    );
    for tree in &short {
        assert!(!tall.iter().any(|over| crowd(tree, over)), "{tree:?}");
        assert!(mathf::hypot(tree.at.0, tree.at.1) < canopied);
    }
    assert!(crowded(&short).is_empty());
    // Drawn from its own lattice and the lower shares of its stands.
    let highest = |trees: &[Offer]| trees.iter().map(|tree| tree.height).fold(0.0, f32::max);
    assert!(
        highest(&short) < highest(&tall),
        "{} under {}",
        highest(&short),
        highest(&tall)
    );
}

/// A composition of `setting` under `seed` for a picture `size`, its jobs run
/// until its woods stand and it is seen.
#[cfg(feature = "scene-tests")]
fn planted(setting: Setting, seed: u64, size: (u32, u32)) -> Composition {
    planted_at(setting, seed, size, Detail::Maximum)
}

#[cfg(feature = "scene-tests")]
#[test]
fn a_forest_stands_thousands_of_trees_none_in_the_way_of_another() {
    let composition = planted(Setting::Forest, 1, (640, 360));
    let stage = &composition.stage;
    let mut trunks: Vec<(f64, f64, f64)> = stage
        .objects
        .iter()
        .filter_map(|object| match object.shape {
            Shape::Instance {
                prototype, pose, ..
            } if matches!(
                stage.recipes.get(prototype as usize),
                Some(Recipe::Tree { .. })
            ) =>
            {
                Some((pose.at.x, pose.at.z, pose.at.y))
            }
            _ => None,
        })
        .collect();
    assert!(
        trunks.len() > 5000,
        "a forest, not a copse: {}",
        trunks.len()
    );
    let eye = composition
        .seen
        .as_ref()
        .map(|(_, camera)| camera.eye())
        .expect("seen");
    trunks.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (index, &(x, z, _)) in trunks.iter().enumerate() {
        assert!(mathf::hypot(x - eye.x, z - eye.z) > 2.0, "none on the eye");
        for &(other_x, other_z, _) in trunks
            .iter()
            .skip(index + 1)
            .take_while(|other| other.0 - x < 0.6)
        {
            assert!(
                mathf::hypot(x - other_x, z - other_z) >= 0.58,
                "two trunks at ({x}, {z}) and ({other_x}, {other_z})"
            );
        }
    }
    // The wood's dead lie about the eye, none at its feet.
    let fallen: Vec<(f64, f64)> = stage
        .objects
        .iter()
        .filter_map(|object| match object.shape {
            Shape::Instance {
                prototype, pose, ..
            } if matches!(
                stage.recipes.get(prototype as usize),
                Some(Recipe::Log { .. })
            ) =>
            {
                Some((pose.at.x, pose.at.z))
            }
            _ => None,
        })
        .collect();
    assert!(
        fallen.len() > 20,
        "a forest's floor holds its fallen: {}",
        fallen.len()
    );
    for &(x, z) in &fallen {
        let distance = mathf::hypot(x - eye.x, z - eye.z);
        assert!(
            distance > DEADFALL_CLEAR && distance <= DEADFALL_REACH + 1.0,
            "a fallen trunk at {distance}"
        );
    }
}

/// The trees a composition stands: where each trunk stands.
#[cfg(feature = "scene-tests")]
fn trunks(composition: &Composition) -> Vec<Vec3> {
    let stage = &composition.stage;
    stage
        .objects
        .iter()
        .filter_map(|object| match object.shape {
            Shape::Instance {
                prototype, pose, ..
            } if matches!(
                stage.recipes.get(prototype as usize),
                Some(Recipe::Tree { .. })
            ) =>
            {
                Some(pose.at)
            }
            _ => None,
        })
        .collect()
}

#[cfg(feature = "scene-tests")]
#[test]
fn a_seed_shows_the_same_place_at_either_detail() {
    let mut laid = 0;
    for (setting, seed) in [
        (Setting::Meadow, 2),
        (Setting::Forest, 1),
        (Setting::Winter, 3),
        (Setting::Valley, 1),
    ] {
        let [simple, maximum] =
            Detail::ALL.map(|detail| planted_at(setting, seed, (320, 180), detail));
        // The land, every grid of it the ground or its water lies on.
        let lands = |composition: &Composition| -> Vec<u32> {
            composition
                .stage
                .objects
                .iter()
                .filter_map(|object| match object.shape {
                    Shape::Land { field } => Some(field),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(lands(&simple), lands(&maximum), "{setting:?}");
        for field in lands(&simple) {
            let at = |composition: &Composition| {
                composition.stage.fields[field as usize].heights().to_vec()
            };
            assert!(at(&simple) == at(&maximum), "{setting:?}: grid {field}");
        }
        // The eye, and the hour and weather it is seen under.
        let ([simple_look, maximum_look], [simple_camera, maximum_camera]) = (
            [&simple, &maximum].map(|composition| &composition.seen.as_ref().expect("seen").0),
            [&simple, &maximum].map(|composition| &composition.seen.as_ref().expect("seen").1),
        );
        for film in [(0.0, 0.0), (-0.9, 0.7), (0.6, -0.8)] {
            let (a, b) = (
                simple_camera.ray(film, (0.0, 0.0)),
                maximum_camera.ray(film, (0.0, 0.0)),
            );
            assert_eq!((a.origin, a.dir), (b.origin, b.dir), "{setting:?}");
        }
        let suns = |stage: &Stage| -> Vec<Vec3> {
            stage
                .lights
                .iter()
                .filter_map(|light| match *light {
                    crate::light::Light::Sun { toward, .. } => Some(toward),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(suns(&simple.stage), suns(&maximum.stage), "{setting:?}");
        assert_eq!(simple_look.exposure, maximum_look.exposure, "{setting:?}");
        assert_eq!(
            (
                simple_look.sky.low.is_some(),
                simple_look.sky.high.is_some(),
                simple_look.sky.stars.is_some()
            ),
            (
                maximum_look.sky.low.is_some(),
                maximum_look.sky.high.is_some(),
                maximum_look.sky.stars.is_some()
            ),
            "{setting:?}"
        );
        // The sward, laid in patches and keyed apart from whatever the woods
        // above it drew.
        let swards = |composition: &Composition| -> Vec<(u32, u32)> {
            composition
                .stage
                .lawns
                .iter()
                .map(|lawn| (lawn.sward, lawn.seed))
                .collect()
        };
        assert_eq!(swards(&simple), swards(&maximum), "{setting:?}");
        laid += swards(&simple).len();
    }
    assert!(laid > 0, "a sward was laid to compare");
}

#[cfg(feature = "scene-tests")]
#[test]
fn a_simple_wood_stands_fewer_trees_nearer_the_eye_and_keeps_to_its_room() {
    let [simple, maximum] =
        Detail::ALL.map(|detail| planted_at(Setting::Meadow, 2, (640, 360), detail));
    let reach = |composition: &Composition| {
        let eye = composition.seen.as_ref().expect("seen").1.eye();
        trunks(composition)
            .iter()
            .map(|at| mathf::hypot(at.x - eye.x, at.z - eye.z))
            .fold(0.0, f64::max)
    };
    let (few, many) = (trunks(&simple).len(), trunks(&maximum).len());
    assert!(few > 2000, "still a wood: {few}");
    assert!(few <= 20_000, "within its cap: {few}");
    assert!(many > 2 * few, "{many} against {few}");
    assert!(
        reach(&simple) < reach(&maximum),
        "{} against {}",
        reach(&simple),
        reach(&maximum)
    );
    assert!(simple.stage.objects.len() <= Detail::Simple.densities().objects);
    // Only Maximum carries a wood on past the trees it stands one by one.
    assert_eq!(simple.carried_on(), 0);
    assert!(maximum.carried_on() > 0);
}

#[test]
fn a_strip_claimed_along_a_line_leaves_no_gap_at_its_edges() {
    let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
    stage
        .claim_along((-10.0, 3.0), (14.0, -4.0), 2.5)
        .expect("claimed");
    let (along, across) = ((24.0, -7.0), (7.0, 24.0));
    let unit = |(x, z): (f64, f64)| {
        let length = mathf::hypot(x, z);
        (x / length, z / length)
    };
    let (along, across) = (unit(along), unit(across));
    let length = mathf::hypot(24.0, -7.0);
    for step in 0..=200u32 {
        let t = f64::from(step) / 200.0 * length;
        for side in [-2.45, -1.2, 0.0, 1.2, 2.45] {
            let at = (
                -10.0 + along.0 * t + across.0 * side,
                3.0 + along.1 * t + across.1 * side,
            );
            assert!(!stage.clear(at, 0.0), "uncovered at {at:?}");
        }
    }
    assert!(stage.clear((-10.0 + across.0 * 6.0, 3.0 + across.1 * 6.0), 0.0));
}

/// A bridge's deck: its ends, and the road's width across it.
#[cfg(feature = "scene-tests")]
type Deck = ((f64, f64), (f64, f64), f64);

#[cfg(feature = "scene-tests")]
#[test]
fn no_tree_stands_on_a_bridges_deck() {
    let mut crossed = 0;
    for seed in 0..6 {
        let mut composition =
            Composition::new(Setting::Valley, seed, (320, 180), Detail::Maximum).expect("composes");
        let land = composition
            .run_until_seen()
            .expect("a valley stands on a land");
        let decks: Vec<Deck> = land
            .crossings
            .iter()
            .map(|deck| {
                (
                    (deck.from.x, deck.from.z),
                    (deck.to.x, deck.to.z),
                    deck.from.width,
                )
            })
            .collect();
        crossed += decks.len();
        for trunk in trunks(&composition) {
            for &((x0, z0), (x1, z1), width) in &decks {
                let (dx, dz) = (x1 - x0, z1 - z0);
                let t = (((trunk.x - x0) * dx + (trunk.z - z0) * dz) / (dx * dx + dz * dz))
                    .clamp(0.0, 1.0);
                let apart = mathf::hypot(trunk.x - (x0 + dx * t), trunk.z - (z0 + dz * t));
                assert!(
                    apart > 0.5 * width,
                    "seed {seed}: a trunk {apart} off a deck's road"
                );
            }
        }
    }
    assert!(crossed > 0, "some valley's road crosses its river");
}

#[test]
fn a_wood_with_nothing_growing_beneath_it_still_roofs_the_air_and_strews_its_floor() {
    // A colonnade stands on paving among woods with no sward under them.
    let composition = planted_at(Setting::Colonnade, 0, (160, 90), Detail::Simple);
    let stage = &composition.stage;
    assert!(
        stage.sward.is_none() && stage.lawns.is_empty(),
        "no sward to lay"
    );
    assert!(!stage.canopies.is_empty(), "woods stand");
    let shades = stage.shades.as_ref().expect("the woods' shade is cast");
    let roofed = stage
        .canopies
        .iter()
        .filter(|&&((x, z), _)| shades.at(x, z).1 > 0.0)
        .count();
    assert!(roofed > 0, "the ground beneath the crowns lies roofed");
    let floored = stage.materials.iter().any(|material| {
        matches!(&material.pigment, crate::pigment::Pigment::Ground(ground) if ground.floor.is_some())
    });
    assert!(floored, "and strewn with what the crowns shed");
}

/// A stream's growing reports its work rising steadily through its woods,
/// its bed and the flow over it, and its water's edge, never falling back.
#[test]
fn a_streams_growing_never_reports_less_done() {
    let mut composition =
        Composition::new(Setting::Stream, 1, (64, 36), Detail::Simple).expect("composes");
    let runner = tairix_parallel::Threaded::new(8);
    let mut planting = loop {
        let job = composition.jobs.pop_front().expect("work remains");
        if let Job::Plant(planting) = job {
            break planting;
        }
        if let Progress::Again(unfinished) = composition.run(job, &runner).expect("runs") {
            composition.jobs.push_front(unfinished);
        }
    };
    let mut last = planting.growing.done();
    let mut steps = 0;
    while !planting
        .growing
        .step(&mut composition.stage, (&planting.land, &runner))
        .expect("grows")
    {
        let now = planting.growing.done();
        assert!(
            now >= last - 1e-12,
            "step {steps}: {now} fell back from {last}"
        );
        last = now;
        steps += 1;
    }
    assert!(steps > 10, "grown in {steps} steps");
}

#[cfg(feature = "scene-tests")]
#[test]
fn a_forest_carried_on_far_off_is_one_wood_with_the_trees_stood_before_it() {
    let mut composition = planted(Setting::Forest, 1, (640, 360));
    let carried = composition.carried_on();
    assert!(carried > 0, "a forest carried on at Maximum");
    // Surveyed once the prototypes its trees are measured by grow.
    let runner = tairix_parallel::Threaded::new(8);
    while !composition
        .advance(&runner, &mut || false)
        .expect("composes")
    {}
    assert_eq!(composition.carried_on(), 0);
    let stage = &composition.stage;
    assert_eq!(stage.far_woods.len(), carried);
    let far = stage.far_woods.first().expect("a forest carried on");
    let eye = composition
        .seen
        .as_ref()
        .map(|(_, camera)| camera.eye())
        .expect("seen");
    let distance = |at: (f64, f64)| mathf::hypot(at.0 - eye.x, at.1 - eye.z);
    // The canopy's trees stood one by one — each where its own cell's place
    // is, as the land offered it; the subcanopy's stand elsewhere — and those
    // carried on beyond.
    let stood: Vec<Offer> = trunks(&composition)
        .into_iter()
        .filter_map(|at| {
            far.offered_at((at.x, at.z), &stage.fields)
                .filter(|tree| tree.at == (at.x, at.z))
        })
        .filter(|tree| far.across(tree.at))
        .collect();
    let carried: Vec<Offer> = far.kept(&stage.fields).collect();
    let meets = carried
        .iter()
        .map(|tree| distance(tree.at))
        .fold(f64::INFINITY, f64::min);
    assert!(meets.is_finite() && stood.len() > 1000);
    assert!(stood.iter().all(|tree| distance(tree.at) <= meets + 1e-6));
    // No trunk in another's room where the two meet.
    let meeting = |tree: &&Offer| (0.95 * meets..1.05 * meets).contains(&distance(tree.at));
    let mut near: Vec<Offer> = stood.iter().filter(meeting).copied().collect();
    near.extend(carried.iter().filter(meeting));
    near.sort_by(|a, b| a.at.0.total_cmp(&b.at.0));
    for (index, tree) in near.iter().enumerate() {
        for other in near[index + 1..]
            .iter()
            .take_while(|other| other.at.0 - tree.at.0 < 40.0)
        {
            assert!(!crowd(tree, other), "{tree:?} crowds {other:?}");
        }
    }
    // As thick either side.
    let thickness = |trees: &[Offer], (from, to): (f64, f64)| {
        let count = trees
            .iter()
            .filter(|tree| (from..to).contains(&distance(tree.at)))
            .count();
        crate::vector::real(count) / (ACROSS * (to * to - from * from))
    };
    let (before, after) = (
        thickness(&stood, (0.85 * meets, meets)),
        thickness(&carried, (meets, 1.15 * meets)),
    );
    assert!(
        before > 0.0 && (after / before - 1.0).abs() < 0.15,
        "{after} against {before}"
    );
    // Each is traced as the tiles a crown stands over.
    let mut tiles = 0;
    for object in &stage.objects {
        if let Shape::FarWood { wood, tile } = object.shape {
            assert!(stage.far_woods.get(wood as usize).is_some(), "its wood");
            assert!(tile.crowns > tile.ground.0 && tile.bounds().max.y > tile.crowns);
            tiles += 1;
        }
    }
    assert!(
        tiles > 0 && tiles < crate::far_wood::TILES * crate::far_wood::TILES,
        "{tiles}"
    );
}
