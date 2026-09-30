//! Host tests of the riders' duel: the board of crossings and the room left
//! on it, the duel's rules, that every duel is decided and plays the same
//! however its frames fall, and how a bike is put together.

use alloc::vec::Vec;

use tairix_rng::NonCryptoRng;
use tairix_util::space::Pose;
use tairix_wm::Scale;

use super::{
    bit, place, room, Duel, Fate, Way, ARM_END, BODY, COLUMNS, EVERY, FORK_TOP, FRONT_CLEAR, HUBS,
    REAR_CLEAR, RIDER, ROWS, SEAT, WHEEL,
};
use crate::saver::retro_games::blast::Blasts;
use crate::saver::retro_games::wire::{Camera, HullId, Models, Part, Stage};
use crate::saver::retro_games::{Moment, View};

const SPEED: f64 = 0.8;

fn view() -> View {
    View::new((1920, 1080), Scale::ONE).expect("a view")
}

fn duel(seed: u64, start: f64) -> Duel {
    let camera = Camera::at(&view(), Moment::at(start, SPEED));
    Duel::new(NonCryptoRng::seed_from_u64(seed), start, &camera, SPEED)
}

/// Play `duel` on in steps of `step` seconds for `within` seconds or until it
/// is over, calling `each` after every step; answer when it ended.
fn play(
    duel: &mut Duel,
    blasts: &mut Blasts,
    step: f64,
    within: f64,
    mut each: impl FnMut(&Duel, f64),
) -> Option<f64> {
    let models = Models::new().expect("the solids");
    let view = view();
    let (mut time, end) = (duel.start, duel.start + within);
    while time < end {
        time = (time + step).min(end);
        let camera = Camera::at(&view, Moment::at(time, SPEED));
        duel.advance(time, &camera, &models, blasts);
        blasts.retire(time);
        each(duel, time);
        if duel.is_over(time) {
            return Some(time);
        }
    }
    None
}

/// The room left from a crossing is every free crossing a rider could reach
/// from it along the grid, and never one reached by running off one side of
/// the board onto the other.
#[test]
fn room_is_what_can_be_reached_along_the_grid() {
    let every = u32::try_from(COLUMNS * ROWS).expect("small");
    let corner = bit((0, 0)).expect("on the board");
    assert_eq!(room(corner, EVERY), every);
    // A wall down the middle column leaves each half its own room.
    let wall = (0..ROWS)
        .filter_map(|row| bit((COLUMNS / 2, row)))
        .fold(0, |wall, claim| wall | claim);
    let free = EVERY & !wall;
    let left = room(corner, free);
    let right = room(bit((COLUMNS - 1, 0)).expect("on the board"), free);
    assert_eq!(left + right + u32::try_from(ROWS).expect("small"), every);
    assert_eq!(left, u32::try_from(ROWS * (COLUMNS / 2)).expect("small"));
    // The last crossing of a row does not lead to the first of the next.
    let row_end = bit((COLUMNS - 1, 0)).expect("on the board");
    let next_row = bit((0, 1)).expect("on the board");
    assert_eq!(room(row_end, row_end | next_row), 1);
    assert!(bit((-1, 0)).is_none() && bit((0, ROWS)).is_none() && bit((COLUMNS, 3)).is_none());
}

/// Every duel is decided and its fences sink away, whoever is left riding
/// having ridden off out of sight.
#[test]
fn every_duel_is_decided() {
    let mut survivors = 0;
    for seed in 0..40 {
        let mut game = duel(seed, 2.0);
        let mut blasts = Blasts::new().expect("room");
        let ended = play(&mut game, &mut blasts, 1.0 / 30.0, 40.0, |_, _| {});
        assert!(ended.is_some(), "seed {seed} is decided");
        assert!(game.decided.is_some());
        survivors += game
            .riders
            .iter()
            .filter(|rider| rider.fate == Fate::Gone)
            .count();
        let wrecks = game
            .riders
            .iter()
            .filter(|rider| matches!(rider.fate, Fate::Wrecked(_)))
            .count();
        assert!(
            wrecks >= game.riders.len() - 1,
            "seed {seed}: no more than one rides off"
        );
    }
    assert!(
        survivors > 20,
        "most duels have a victor: {survivors} in forty"
    );
}

/// No rider rides on across a claimed crossing or off the arena: a rider
/// still riding while racing has only ever reached crossings nobody held.
#[test]
fn no_rider_rides_through_a_fence() {
    for seed in 0..30 {
        let mut game = duel(seed, 0.0);
        let mut blasts = Blasts::new().expect("room");
        let _ = play(&mut game, &mut blasts, 1.0 / 30.0, 40.0, |game, _| {
            if game.decided.is_some() {
                return;
            }
            for rider in game.riders.iter().filter(|rider| rider.laying) {
                if rider.fate == Fate::Riding {
                    assert!(bit(rider.node).is_some(), "seed {seed}: on the arena");
                    let visits = game
                        .riders
                        .iter()
                        .flat_map(|other| other.fence.iter())
                        .filter(|post| super::node(**post) == rider.node)
                        .count();
                    assert_eq!(visits, 1, "seed {seed}: {:?} claimed once", rider.node);
                }
            }
        });
    }
}

/// A duel plays the same whether its frames fall often or seldom.
#[test]
fn a_duel_plays_the_same_however_its_frames_fall() {
    for seed in [5, 13, 21] {
        let (mut fine, mut coarse) = (duel(seed, 1.0), duel(seed, 1.0));
        let (mut a, mut b) = (Blasts::new().expect("room"), Blasts::new().expect("room"));
        let _ = play(&mut fine, &mut a, 1.0 / 60.0, 7.0, |_, _| {});
        let _ = play(&mut coarse, &mut b, 0.29, 7.0, |_, _| {});
        assert_eq!(fine.board, coarse.board, "seed {seed}");
        for (x, y) in fine.riders.iter().zip(&coarse.riders) {
            assert_eq!(
                (x.node, x.way, x.fate),
                (y.node, y.way, y.fate),
                "seed {seed}"
            );
            assert_eq!(x.fence.as_slice(), y.fence.as_slice(), "seed {seed}");
        }
    }
}

/// The fences stand on the grid's own lines: every crossing lies where a
/// line towards the horizon meets one across the floor.
#[test]
fn the_arena_lies_on_the_grids_lines() {
    let game = duel(3, 0.0);
    for node in [(0, 0), (COLUMNS - 1, ROWS - 1), (4, 7)] {
        let (x, z) = place(game.origin, node);
        assert!(
            ((x - 0.5) - (x - 0.5).round()).abs() < 1e-12,
            "across on a line: {x}"
        );
        assert!((z - z.round()).abs() < 1e-12, "ahead on a line: {z}");
    }
    let (left, _) = place(game.origin, (0, 0));
    let (right, _) = place(game.origin, (COLUMNS - 1, 0));
    assert!((left + right).abs() < 1e-12, "centred on the flight's path");
}

/// A turn is a quarter turn either way; four of them come back round.
#[test]
fn ways_turn_by_quarters() {
    for way in [Way::Away, Way::Right, Way::Toward, Way::Left] {
        assert_eq!(way.right().left(), way);
        assert_eq!(way.right().right().right().right(), way);
        assert_ne!(way.right(), way.left());
        let (dx, dy) = way.step();
        assert_eq!(dx.abs() + dy.abs(), 1);
        let yaw = way.yaw();
        assert!(
            (yaw.sin() - f64::from(dx)).abs() < 1e-12 && (yaw.cos() - f64::from(dy)).abs() < 1e-12
        );
    }
}

/// The bike is built so that the planes it is drawn in order across really do
/// part its pieces: each wheel, with its fork or swingarm, beyond the body,
/// and the rider above it.
#[test]
fn a_bikes_pieces_stand_clear_of_each_other() {
    let body_top = BODY.iter().map(|&(_, y)| y).fold(f64::MIN, f64::max);
    assert!(BODY
        .iter()
        .all(|&(z, _)| (REAR_CLEAR..=FRONT_CLEAR).contains(&z)));
    assert!(RIDER
        .iter()
        .all(|&(z, y)| y >= body_top - 1e-12 && (REAR_CLEAR..=FRONT_CLEAR).contains(&z)));
    assert!(HUBS[0] - WHEEL >= FRONT_CLEAR && FORK_TOP.0 >= FRONT_CLEAR);
    assert!(HUBS[1] + WHEEL <= REAR_CLEAR && ARM_END.0 <= REAR_CLEAR);
    assert!(
        (body_top - SEAT).abs() < 1e-12,
        "the rider sits on the seat"
    );
}

/// Of a bike's body and its rider, the one on the camera's own side of the
/// seat's plane is drawn last: the rider, seen from above; the body, once a
/// bike leaning away through a turn has tipped the plane above the camera.
#[test]
fn the_body_and_the_rider_are_drawn_as_the_camera_sees_them() {
    let view = view();
    let camera = Camera::at(&view, Moment::default());
    let game = duel(3, 0.0);
    let mut below = 0;
    for came in [Way::Away, Way::Right, Way::Toward, Way::Left] {
        for way in [came, came.left(), came.right()] {
            for step in 0..16 {
                let mut rider = game.riders[0].clone();
                (rider.node, rider.came, rider.way) = ((COLUMNS / 2, 0), came, way);
                let time = game.clock + Duel::step() * f64::from(step) / 16.0;
                let mut stage = Stage::new(&view, 64).expect("a stage");
                stage.reset(camera);
                game.bike(&rider, time, &mut stage);
                let solids: Vec<(HullId, Pose)> = stage
                    .parts()
                    .iter()
                    .filter_map(|part| match *part {
                        Part::Solid { hull, pose, .. } => Some((hull, pose)),
                        _ => None,
                    })
                    .collect();
                let &[(first, pose), (second, _)] = solids.as_slice() else {
                    panic!("a body and a rider: {solids:?}");
                };
                let above = pose.point_to_local(camera.position()).y > SEAT;
                below += usize::from(!above);
                let expected = if above {
                    (HullId::BikeBody, HullId::BikeRider)
                } else {
                    (HullId::BikeRider, HullId::BikeBody)
                };
                assert_eq!(
                    (first, second),
                    expected,
                    "{came:?} to {way:?}, step {step}"
                );
            }
        }
    }
    assert!(below > 0, "leaning away tips the seat above the camera");
}
