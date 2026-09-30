//! Host tests of the cast: nothing comes on under reduced motion, each lane
//! brings its acts on now and then with rests between, and a seed plays the
//! same show every time.

use alloc::vec::Vec;

use tairix_wm::Scale;

use super::{Act, Cast, Realm};
use crate::saver::retro_games::wire::{Camera, Models};
use crate::saver::retro_games::{Moment, View};

const SPEED: f64 = 0.8;

/// What each lane played across `seconds` of a show from `seed`, a frame at a
/// time: the kind and the times it came on and went off.
fn show(seed: u64, calm: bool, seconds: f64) -> [Vec<(&'static str, f64, f64)>; 2] {
    let view = View::new((960, 540), Scale::ONE).expect("a view");
    let models = Models::new().expect("the solids");
    let mut cast = Cast::new(seed, SPEED, calm).expect("a cast");
    let mut played: [Vec<(&'static str, f64, f64)>; 2] = [Vec::new(), Vec::new()];
    let mut time = 0.0;
    while time < seconds {
        time += 1.0 / 15.0;
        let moment = Moment::at(time, SPEED);
        cast.advance(moment, &Camera::at(&view, moment), &models);
        for (lane, log) in cast.lanes.iter().zip(&mut played) {
            let kind = lane.act.as_ref().map(|act| match act {
                Act::Ship(_) => "ship",
                Act::Saucer(_) => "saucer",
                Act::Tanks(_) => "tanks",
                Act::Riders(_) => "riders",
            });
            match (kind, log.last_mut()) {
                (Some(kind), Some(last)) if last.0 == kind && last.2 >= time - 0.1 => last.2 = time,
                (Some(kind), _) => log.push((kind, time, time)),
                (None, _) => {}
            }
        }
    }
    played
}

#[test]
fn nothing_comes_on_under_reduced_motion() {
    let [sky, ground] = show(4, true, 300.0);
    assert!(sky.is_empty() && ground.is_empty());
}

/// Each lane plays its own two kinds, brings its first act on after its
/// first wait, rests between acts, and over a few minutes plays both kinds.
#[test]
fn each_lane_plays_now_and_then() {
    for seed in [1, 2, 3] {
        let played = show(seed, false, 600.0);
        for (log, realm) in played.iter().zip([Realm::Sky, Realm::Ground]) {
            let kinds: &[&str] = match realm {
                Realm::Sky => &["ship", "saucer"],
                Realm::Ground => &["tanks", "riders"],
            };
            assert!(
                log.iter().all(|(kind, _, _)| kinds.contains(kind)),
                "{realm:?}: {log:?}"
            );
            assert!(
                kinds
                    .iter()
                    .all(|kind| log.iter().any(|(played, _, _)| played == kind)),
                "{realm:?} plays both"
            );
            let (least, most) = realm.first();
            let first = log.first().expect("an act came on").1;
            assert!(
                first >= least && first <= most + 0.2,
                "{realm:?} first at {first}"
            );
            for pair in log.windows(2) {
                let rest = pair[1].1 - pair[0].2;
                assert!(rest >= realm.rest().0 - 0.2, "{realm:?} rests {rest} s");
            }
        }
    }
}

/// A lane's first act is either of its kinds alike.
#[test]
fn a_lanes_first_act_is_either_kind_alike() {
    let view = View::new((960, 540), Scale::ONE).expect("a view");
    let models = Models::new().expect("the solids");
    let moment = Moment::at(Realm::Sky.first().1.max(Realm::Ground.first().1), SPEED);
    let mut seconds = [0u32; 2];
    for seed in 0..400 {
        let mut cast = Cast::new(seed, SPEED, false).expect("a cast");
        cast.advance(moment, &Camera::at(&view, moment), &models);
        for (lane, count) in cast.lanes.iter().zip(&mut seconds) {
            assert!(lane.act.is_some(), "the first act is on");
            *count += u32::from(lane.second_last);
        }
    }
    for count in seconds {
        assert!(
            (140..=260).contains(&count),
            "{count} of 400 open on the second kind"
        );
    }
}

#[test]
fn a_seed_plays_the_same_show() {
    assert_eq!(show(9, false, 200.0), show(9, false, 200.0));
    assert_ne!(show(9, false, 200.0), show(10, false, 200.0));
}
