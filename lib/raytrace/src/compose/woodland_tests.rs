extern crate std;

use alloc::vec::Vec;

use tairix_parallel::Threaded;

use super::*;
use crate::compose::{Composition, Progress, Recipe, Setting};
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
    let mut composition = Composition::new(setting, seed, size).expect("composes");
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
