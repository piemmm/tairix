//! Host tests of the spaceship: where its passes begin and end, how it faces
//! and banks, and its plumes.

use alloc::vec::Vec;

use tairix_rng::NonCryptoRng;
use tairix_util::space::Vec3;
use tairix_wm::Scale;

use super::{Ship, COURSES, MOST_BANK, NOZZLES, STERN};
use crate::saver::retro_games::wire::{Camera, Part, Stage};
use crate::saver::retro_games::{Moment, View};

const SPEED: f64 = 0.8;

/// A pass of each course, from each side, with each draw of its variation.
fn passes() -> impl Iterator<Item = Ship> {
    (0..64u64).map(|seed| Ship::new(&mut NonCryptoRng::seed_from_u64(seed), 3.0, 16.0 / 9.0))
}

/// Every pass begins and ends where the ship cannot be seen: out of sight, or
/// faded into the haze.
#[test]
fn every_pass_begins_and_ends_unseen() {
    let view = View::new((1920, 1080), Scale::ONE).expect("a view");
    let camera = Camera::at(&view, Moment::default());
    for ship in passes() {
        for time in [ship.start, ship.start + ship.duration] {
            let (seen, _, alpha) = ship.pose(time, SPEED);
            let unseen = alpha < 1e-9 || !camera.sees(camera.position() + seen, 400.0);
            assert!(
                unseen,
                "{:?} at {time}: {seen:?}, alpha {alpha}",
                ship.course
            );
        }
        assert!(ship.is_over(ship.start + ship.duration));
        assert!(!ship.is_over(ship.start + ship.duration * 0.5));
    }
}

/// The ship faces the way it moves, stays well above the floor, and banks no
/// further than its bound.
#[test]
fn the_ship_faces_its_course_and_banks_within_bounds() {
    for ship in passes() {
        for step in 0..40 {
            let time = ship.start + ship.duration * f64::from(step) / 40.0;
            let (seen, frame, _) = ship.pose(time, SPEED);
            let ahead = ship.pose(time + 0.01, SPEED).0 - seen + Vec3::new(0.0, 0.0, SPEED * 0.01);
            assert!(
                frame.z.dot(ahead.normalized()) > 0.98,
                "it faces the way it moves"
            );
            assert!((frame.z.length() - 1.0).abs() < 1e-9 && frame.x.dot(frame.z).abs() < 1e-9);
            assert!(seen.y > 0.3, "above the camera's height: {seen:?}");
            let level = Vec3::UP.cross(frame.z).normalized();
            let bank = frame.x.dot(level).clamp(-1.0, 1.0).acos();
            assert!(bank <= MOST_BANK + 1e-6, "bank {bank}");
        }
    }
    assert!(!COURSES.is_empty());
}

/// Each engine trails its plume straight back from its nozzle on the stern.
#[test]
fn the_plumes_stream_back_from_the_stern() {
    let view = View::new((1920, 1080), Scale::ONE).expect("a view");
    let ship = Ship::new(&mut NonCryptoRng::seed_from_u64(9), 0.0, 16.0 / 9.0);
    let time = ship.duration / 2.0;
    let (_, frame, _) = ship.pose(time, SPEED);
    let mut stage = Stage::new(&view, 32).expect("a stage");
    stage.reset(Camera::at(&view, Moment::default()));
    ship.stage(time, SPEED, &mut stage);
    let plumes: Vec<(Vec3, Vec3)> = stage
        .parts()
        .iter()
        .filter_map(|part| match *part {
            Part::Glow { base, tip, .. } if (tip - base).length() > 0.1 => Some((base, tip)),
            _ => None,
        })
        .collect();
    assert_eq!(
        plumes.len(),
        2 * NOZZLES.len(),
        "a flame and its core for each"
    );
    for (base, tip) in plumes {
        assert!(
            (tip - base).normalized().dot(-frame.z) > 0.999,
            "straight back"
        );
    }
    assert!(NOZZLES
        .iter()
        .all(|nozzle| (nozzle.z - STERN).abs() < 1e-12));
}
