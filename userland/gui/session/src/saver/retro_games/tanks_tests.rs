//! Host tests of the tank battle: that every battle ends, plays the same
//! however its frames fall, keeps clear of the flight's path, and wrecks
//! what it strikes.

use tairix_rng::NonCryptoRng;
use tairix_util::space::Vec3;
use tairix_wm::Scale;

use super::{before_turret, Battle, Leg, BATTLE_ROOM, PLACE_ACROSS};
use crate::saver::retro_games::blast::Blasts;
use crate::saver::retro_games::wire::{Camera, Models};
use crate::saver::retro_games::{Moment, View};

const SPEED: f64 = 0.8;

fn view() -> View {
    View::new((1920, 1080), Scale::ONE).expect("a view")
}

/// A battle begun at `start`, from `seed`, with the camera where the flight
/// stands then.
fn battle(seed: u64, start: f64) -> Battle {
    let camera = Camera::at(&view(), Moment::at(start, SPEED));
    Battle::new(NonCryptoRng::seed_from_u64(seed), start, &camera, SPEED)
}

/// Play `battle` on in steps of `step` seconds until it is over or
/// `within` seconds have passed, answering when it ended.
fn play(
    battle: &mut Battle,
    blasts: &mut Blasts,
    models: &Models,
    step: f64,
    within: f64,
) -> Option<f64> {
    let view = view();
    let (mut time, end) = (battle.start, battle.start + within);
    while time < end {
        time = (time + step).min(end);
        let camera = Camera::at(&view, Moment::at(time, SPEED));
        battle.advance(time, &camera, models, blasts);
        blasts.retire(time);
        if battle.is_over() {
            return Some(time);
        }
    }
    None
}

/// Every battle ends — each tank wrecked or driven off, no shell still in
/// flight — soon after its time runs out at the latest.
#[test]
fn every_battle_ends() {
    let models = Models::new().expect("the solids");
    let mut wrecks = 0;
    for seed in 0..40 {
        let mut fight = battle(seed, 4.0);
        let mut blasts = Blasts::new().expect("room");
        let limit = BATTLE_ROOM / SPEED + 12.0;
        let ended = play(&mut fight, &mut blasts, &models, 1.0 / 30.0, limit);
        assert!(ended.is_some(), "seed {seed} ends within {limit} s");
        wrecks += fight.tanks.iter().filter(|tank| !tank.alive).count();
        assert!(fight.tanks.iter().all(|tank| !tank.alive || tank.gone));
    }
    assert!(wrecks > 20, "battles are won: {wrecks} wrecks in forty");
}

/// A battle plays the same whether its frames fall often or seldom: every
/// decision falls at a time its own course fixed.
#[test]
fn a_battle_plays_the_same_however_its_frames_fall() {
    let models = Models::new().expect("the solids");
    for seed in [3, 17, 29] {
        let (mut fine, mut coarse) = (battle(seed, 1.0), battle(seed, 1.0));
        let (mut fine_blasts, mut coarse_blasts) =
            (Blasts::new().expect("room"), Blasts::new().expect("room"));
        let _ = play(&mut fine, &mut fine_blasts, &models, 1.0 / 60.0, 9.0);
        let _ = play(&mut coarse, &mut coarse_blasts, &models, 0.37, 9.0);
        let time = 1.0 + 9.0;
        assert_eq!(fine.tanks.len(), coarse.tanks.len());
        for (a, b) in fine.tanks.iter().zip(&coarse.tanks) {
            assert_eq!(
                (a.alive, a.gone, a.plan),
                (b.alive, b.gone, b.plan),
                "seed {seed}"
            );
            assert!(
                (a.leg.at(time).0 - b.leg.at(time).0).length() < 1e-9,
                "seed {seed}"
            );
            assert!(
                (a.turret.at(time) - b.turret.at(time)).abs() < 1e-9,
                "seed {seed}"
            );
        }
        assert_eq!(fine.shells.len(), coarse.shells.len(), "seed {seed}");
    }
}

/// While they fight, the tanks keep to either side of the flight's path,
/// ahead of the camera, and every one of them stands on the floor.
#[test]
fn the_tanks_keep_clear_of_the_flights_path() {
    let models = Models::new().expect("the solids");
    for seed in 0..20 {
        let mut fight = battle(seed, 0.0);
        let mut blasts = Blasts::new().expect("room");
        let view = view();
        let mut time = 0.0;
        while time < 12.0 {
            time += 0.1;
            let camera = Camera::at(&view, Moment::at(time, SPEED));
            fight.advance(time, &camera, &models, &mut blasts);
            for tank in fight.tanks.iter().filter(|tank| tank.fighting()) {
                let (at, _, _) = tank.at(time);
                assert!(at.y.abs() < 1e-12, "on the floor");
                assert!(
                    at.z > camera.position().z + 2.0,
                    "ahead: seed {seed} at {time}"
                );
                if tank.plan != super::Plan::Arriving && tank.plan != super::Plan::Leaving {
                    assert!(
                        at.x.abs() >= PLACE_ACROSS.0 - 2.1,
                        "clear of the path: {at:?}"
                    );
                }
            }
        }
    }
}

/// A shell that strikes home wrecks the tank it struck, and the blast goes
/// off where that tank stood.
#[test]
fn a_hit_wrecks_what_it_strikes() {
    let models = Models::new().expect("the solids");
    for seed in 0..40 {
        let mut fight = battle(seed, 0.0);
        let mut blasts = Blasts::new().expect("room");
        let view = view();
        let mut time = 0.0;
        while time < 30.0 {
            time += 1.0 / 30.0;
            let camera = Camera::at(&view, Moment::at(time, SPEED));
            let landing = fight
                .shells
                .iter()
                .filter(|shell| shell.span.1 <= time)
                .find_map(|shell| shell.target.map(|target| (target, shell.span.1)));
            let standing = landing.map(|(target, _)| fight.tanks[target].fighting());
            fight.advance(time, &camera, &models, &mut blasts);
            if let (Some((target, lands)), Some(true)) = (landing, standing) {
                assert!(!fight.tanks[target].alive, "seed {seed}: struck at {lands}");
                let blast = blasts
                    .bursts()
                    .find(|burst| (burst.start - lands).abs() < 1e-9);
                let stood = fight.tanks[target].leg.at(lands).0;
                let at = blast.expect("a blast went off").at;
                assert!((at - stood).length() < 1e-9);
                return;
            }
        }
    }
    panic!("no shell struck home in forty battles");
}

/// The barrel is drawn after the turret only while the camera stands before
/// the turret's front face.
#[test]
fn the_barrel_is_drawn_over_the_turret_only_from_in_front() {
    assert!(before_turret(Vec3::new(0.0, 0.5, 3.0)));
    assert!(!before_turret(Vec3::new(0.0, 0.5, -3.0)));
    assert!(
        !before_turret(Vec3::new(4.0, 0.5, 0.0)),
        "beside it, the turret hides its root"
    );
}

/// A course turns on the spot before it drives, and ends where it was sent.
#[test]
fn a_course_turns_and_then_drives() {
    let leg = Leg::still(Vec3::new(0.0, 0.0, 10.0), 0.0, 1.0).towards(
        1.0,
        Vec3::new(3.0, 0.0, 10.0),
        2.0,
    );
    let (at, yaw) = leg.at(leg.turn.1);
    assert!(
        (at - Vec3::new(0.0, 0.0, 10.0)).length() < 1e-12,
        "no drive while turning"
    );
    assert!(
        (yaw - core::f64::consts::FRAC_PI_2).abs() < 1e-12,
        "faces the way it goes"
    );
    assert!((leg.at(leg.ends()).0 - Vec3::new(3.0, 0.0, 10.0)).length() < 1e-12);
    assert!(
        (leg.ends() - leg.turn.1 - 1.5).abs() < 1e-12,
        "three cells at two a second"
    );
    let still = leg.towards(leg.ends(), Vec3::new(3.0, 0.0, 10.0), 2.0);
    assert!(
        (still.at(still.ends()).1 - yaw).abs() < 1e-12,
        "no turn to stand still"
    );
}
