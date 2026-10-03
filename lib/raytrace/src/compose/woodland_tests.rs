extern crate std;

use alloc::vec::Vec;

use tairix_parallel::Threaded;

use super::*;
use crate::compose::{Composition, Job, Progress, Recipe, Setting, Stage};
use crate::detail::Detail;
use crate::shape::Shape;

const EYE: Vantage = Vantage {
    eye: Vec3::new(0.0, 1.7, 0.0),
    heading: 0.0,
};

fn woodland(cover: f64) -> Woodland {
    Woodland {
        cover,
        patch: 200.0,
        closure: (0.6, 1.0),
        stature: (0.8, 1.0),
        gaps: 0.3,
        most: 1000,
        open: (1.5, 0.3),
    }
}

#[test]
fn a_wood_covers_about_the_share_of_the_ground_asked_of_it() {
    for cover in [0.15, 0.4, 0.65, 0.9] {
        let (mut wooded_places, mut places) = (0.0, 0.0);
        for row in 0..160 {
            for column in 0..160 {
                let at = (f64::from(column) * 37.0, f64::from(row) * 37.0);
                wooded_places += wooded(&woodland(cover), 99, at);
                places += 1.0;
            }
        }
        let share = wooded_places / places;
        assert!((share - cover).abs() < 0.08, "{cover}: {share}");
    }
}

#[test]
fn a_canopy_opens_gaps_where_its_lattice_holds_them_and_none_where_it_is_closed() {
    let share_open = |gaps: f64| {
        let woodland = Woodland {
            gaps,
            ..woodland(0.9)
        };
        let mut open = 0.0;
        for row in 0..300 {
            for column in 0..300 {
                open += gap(
                    &woodland,
                    5,
                    (f64::from(column) * 7.3, f64::from(row) * 7.3),
                );
            }
        }
        open / (300.0 * 300.0)
    };
    assert!(share_open(0.0).abs() < 1e-12);
    let (few, many) = (share_open(0.2), share_open(0.6));
    assert!((0.01..0.06).contains(&few), "{few}");
    assert!((0.05..0.15).contains(&many), "{many}");
    // A gap's heart is open all through, its edge closing over it.
    let woodland = Woodland {
        gaps: 1.0,
        ..woodland(0.9)
    };
    let found = cells2(0.5, 0.5, 5 ^ 0x6a95, 0.8);
    let middle = (
        (0.5 + found.toward.x) * GAP_SPACING,
        (0.5 + found.toward.z) * GAP_SPACING,
    );
    assert!(gap(&woodland, 5, middle) > 0.99, "at {middle:?}");
}

#[test]
fn a_crown_keeps_its_peers_off_but_a_much_shorter_tree_may_stand_beneath_it() {
    let mut crowns = Crowns::new(((0.0, 0.0), 100.0), 30.0).expect("a grid");
    let tall = Stood {
        at: (0.0, 0.0),
        height: 20.0,
        reach: 6.0,
        apart: 0.6,
    };
    crowns.add(tall).expect("stood");
    let peer = |x: f64| Stood {
        at: (x, 0.0),
        height: 19.0,
        reach: 6.0,
        apart: 0.6,
    };
    assert!(
        crowns.crowds(&peer(7.0)),
        "peers' trunks keep more than 7.2 m apart"
    );
    assert!(!crowns.crowds(&peer(7.4)));
    let young = |x: f64| Stood {
        at: (x, 0.0),
        height: 5.0,
        reach: 1.5,
        apart: 0.6,
    };
    assert!(
        !crowns.crowds(&young(3.0)),
        "a sapling under the crown's edge"
    );
    assert!(crowns.crowds(&young(1.2)), "but never against the trunk");
    // Across a cell's wall, at x = 20, as readily as within one.
    let mut crowns = Crowns::new(((0.0, 0.0), 100.0), 30.0).expect("a grid");
    crowns.add(peer(19.0)).expect("stood");
    assert!(crowns.crowds(&peer(21.0)));
    assert!(!crowns.crowds(&peer(27.0)));
}

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
fn what_grows_beneath_a_wood_takes_to_its_gaps_and_edges() {
    let deep = thrives_beneath(0.99);
    let gap = thrives_beneath(0.5);
    let open = thrives_beneath(0.0);
    assert!(deep < 0.25, "{deep}");
    assert!(gap > 0.95, "{gap}");
    assert!(open > deep && open < 0.6 * gap, "{open}");
}

#[test]
fn places_are_sown_all_about_the_eye_near_it_and_across_the_view_beyond() {
    let sowing = Sowing {
        sown: 2.0,
        about: 40.0,
        far: 120.0,
    };
    let sown = |square: ((f64, f64), f64)| {
        let mut seedlings = Vec::new();
        for ring in 0..rings(&sowing) {
            sow_ring(ring, (&EYE, &sowing), square, 7, &mut seedlings).expect("sown");
        }
        seedlings
    };
    let seedlings = sown(((0.0, 0.0), 1000.0));
    let (mut near, mut ahead) = (0, 0);
    for seedling in &seedlings {
        let (x, z) = seedling.at;
        let distance = mathf::hypot(x, z);
        assert!(distance <= 120.0 + 1e-6);
        if distance < 40.0 {
            near += 1;
        } else {
            let turn = mathf::atan2(x, z);
            assert!(
                turn.abs() <= ACROSS + 0.06,
                "beyond the eye's round only across the view: {turn}"
            );
            ahead += 1;
        }
    }
    let expected_near = PI * 40.0 * 40.0 / 4.0;
    let expected_ahead = ACROSS * (120.0 * 120.0 - 40.0 * 40.0) / 4.0;
    assert!(
        (f64::from(near) / expected_near - 1.0).abs() < 0.05,
        "{near} of {expected_near}"
    );
    assert!(
        (f64::from(ahead) / expected_ahead - 1.0).abs() < 0.05,
        "{ahead} of {expected_ahead}"
    );
    let inland = sown(((0.0, 50.0), 20.0));
    assert!(inland
        .iter()
        .all(|seedling| seedling.at.0.abs() < 20.0 && (seedling.at.1 - 50.0).abs() < 20.0));
}

#[test]
fn a_wood_keeps_out_of_its_clearing_off_the_road_and_within_its_heights() {
    let lie = Lie {
        height: 40.0,
        upright: 0.98,
        green: 1.0,
        ..Lie::default()
    };
    let rooting = Rooting {
        above: Some((10.0, 20.0)),
        below: Some((60.0, 80.0)),
        clearing: Some(((100.0, 0.0), 15.0)),
        ..ANYWHERE
    };
    let suits = |lie: Lie, at: (f64, f64)| rooting.suits(&lie, at);
    let barred = |lie: Lie, at: (f64, f64)| suits(lie, at).abs() < 1e-12;
    assert!((suits(lie, (0.0, 0.0)) - 1.0).abs() < 1e-9);
    assert!(barred(lie, (110.0, 0.0)), "in the clearing");
    assert!(barred(Lie { road: 1.0, ..lie }, (0.0, 0.0)), "on the road");
    assert!(
        suits(Lie { path: 1.0, ..lie }, (0.0, 0.0)) < 0.1,
        "on a path"
    );
    assert!(
        barred(Lie { height: 5.0, ..lie }, (0.0, 0.0)),
        "below its heights"
    );
    assert!(
        barred(
            Lie {
                height: 90.0,
                ..lie
            },
            (0.0, 0.0)
        ),
        "above them"
    );
    assert!(
        barred(
            Lie {
                upright: 0.6,
                ..lie
            },
            (0.0, 0.0)
        ),
        "too steep"
    );
    // Under snow nothing grows green, but a wood that roots bare stands on.
    let snowed = Lie { green: 0.0, ..lie };
    assert!(barred(snowed, (0.0, 0.0)));
    assert!(
        Rooting {
            bare: 0.8,
            ..rooting
        }
        .suits(&snowed, (0.0, 0.0))
            > 0.79
    );
}

/// A composition of `setting` under `seed` for a picture `size`, its jobs run
/// until its woods stand and it is seen.
fn planted(setting: Setting, seed: u64, size: (u32, u32)) -> Composition {
    planted_at(setting, seed, size, Detail::Maximum)
}

/// `planted`, at `detail`.
fn planted_at(setting: Setting, seed: u64, size: (u32, u32), detail: Detail) -> Composition {
    let mut composition = Composition::new(setting, seed, size, detail).expect("composes");
    let runner = Threaded::new(8);
    while composition.seen.is_none() {
        let job = composition
            .jobs
            .pop_front()
            .expect("work remains until the scene is seen");
        if let Progress::Again(unfinished) = composition.run(job, &runner).expect("runs") {
            composition.jobs.push_front(unfinished);
        }
    }
    composition
}

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

/// A ranking hands its places out tallest first, those as tall in the order
/// they were added — as one sort of them all would — however its runs were
/// shared among cores, across run boundaries and with many heights alike.
#[test]
fn a_ranking_takes_its_places_tallest_first_as_one_sort_would() {
    for count in [0usize, 1, 7, RUN - 1, RUN, RUN + 1, 3 * RUN + 1234] {
        let heights: Vec<f64> = (0..count)
            .map(|index| {
                // Rounded to tenths, so most heights recur many times.
                let draw = unit(mix32(u32::try_from(index).expect("index") ^ 0x2545));
                mathf::round(300.0 * draw) / 10.0
            })
            .collect();
        let mut expected: Vec<(f32, u32)> = heights
            .iter()
            .enumerate()
            .map(|(index, &height)| (single(height), u32::try_from(index).expect("index")))
            .collect();
        expected.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        let expected: Vec<u32> = expected.into_iter().map(|(_, index)| index).collect();
        let runners: [&dyn JobRunner; 3] = [
            &tairix_parallel::SERIAL,
            &tairix_parallel::Reversed::new(3),
            &tairix_parallel::Threaded::new(4),
        ];
        for runner in runners {
            let mut ranking = Ranking::default();
            assert!(ranking.reserve(count));
            for (index, &height) in heights.iter().enumerate() {
                ranking.add(height, u32::try_from(index).expect("index"));
            }
            let mut units = 0;
            while !ranking.ranked() {
                ranking.rank(runner).expect("held");
                units += 1;
            }
            assert!(units <= count.div_ceil(RUN), "{units} units for {count}");
            let mut taken = Vec::new();
            while let Some(index) = ranking.next() {
                taken.push(index);
            }
            assert!(ranking.exhausted());
            assert_eq!(taken, expected, "{count} places");
        }
    }
}

/// The trees a composition stands: where each trunk stands.
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
type Deck = ((f64, f64), (f64, f64), f64);

#[test]
fn no_tree_stands_on_a_bridges_deck() {
    let mut crossed = 0;
    for seed in 0..6 {
        let mut composition =
            Composition::new(Setting::Valley, seed, (320, 180), Detail::Maximum).expect("composes");
        let runner = Threaded::new(8);
        let mut decks: Vec<Deck> = Vec::new();
        while composition.seen.is_none() {
            let job = composition
                .jobs
                .pop_front()
                .expect("work remains until the scene is seen");
            if let Job::Plant(planting) = &job {
                let road = planting.land.road.map_or(0.0, |road| road.width);
                decks = planting
                    .land
                    .crossings
                    .iter()
                    .map(|deck| ((deck.from.x, deck.from.z), (deck.to.x, deck.to.z), road))
                    .collect();
            }
            if let Progress::Again(unfinished) = composition.run(job, &runner).expect("runs") {
                composition.jobs.push_front(unfinished);
            }
        }
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
