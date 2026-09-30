//! Host tests of the flying saucer: the legs of a visit, where it roams, how
//! it tilts, and its rim lights.

use alloc::vec::Vec;

use tairix_rng::NonCryptoRng;
use tairix_util::space::Pose;
use tairix_wm::Scale;

use super::{Ease, Saucer, CROWN, MOST_TILT, SIDES};
use crate::saver::retro_games::wire::{Camera, HullId, Models, Part, Stage};
use crate::saver::retro_games::{Moment, View};

fn view() -> View {
    View::new((1920, 1080), Scale::ONE).expect("a view")
}

fn visits() -> impl Iterator<Item = Saucer> {
    let view = view();
    let shape = (
        f64::from(view.width) / f64::from(view.height),
        f64::from(view.horizon) / f64::from(view.height),
    );
    (0..48u64).map(move |seed| Saucer::new(&mut NonCryptoRng::seed_from_u64(seed), 2.0, shape))
}

/// A visit arrives, darts and hovers in turn, and leaves; each leg begins
/// where and when the last ended.
#[test]
fn a_visit_is_one_unbroken_run_of_legs() {
    for saucer in visits() {
        let legs = &saucer.legs;
        assert_eq!(legs.first().map(|leg| leg.ease), Some(Ease::Arrive));
        assert_eq!(legs.last().map(|leg| leg.ease), Some(Ease::Leave));
        assert!(legs.iter().filter(|leg| leg.ease == Ease::Dart).count() >= super::DARTS.0);
        for pair in legs.windows(2) {
            assert!((pair[0].ends - pair[1].begins).abs() < 1e-12);
            assert!((pair[0].to - pair[1].from).length() < 1e-12);
        }
        assert!((legs[0].begins - 2.0).abs() < 1e-12);
        assert!(saucer.is_over(saucer.ends()) && !saucer.is_over(saucer.ends() - 0.1));
    }
}

/// Between arriving and leaving it stays in sight, and it comes to rest at
/// the end of every dart.
#[test]
fn it_stays_in_sight_and_rests_between_darts() {
    let view = view();
    let camera = Camera::at(&view, Moment::default());
    for saucer in visits() {
        for leg in &saucer.legs {
            if matches!(leg.ease, Ease::Dart | Ease::Hover) {
                for share in [0.0, 0.5, 1.0] {
                    let time = leg.begins + (leg.ends - leg.begins) * share;
                    let (seen, _) = saucer.course(time);
                    assert!(
                        camera.sees(camera.position() + seen, 0.0),
                        "{seen:?} is in sight"
                    );
                }
            }
            if leg.ease == Ease::Dart {
                let (_, rate, _) = leg.along(1.0);
                assert!(rate.abs() < 1e-12, "at rest where a dart ends");
                assert!((leg.along(1.0).0 - 1.0).abs() < 1e-12);
            }
        }
    }
}

/// It tilts into its acceleration no further than its bound.
#[test]
fn it_tilts_within_its_bound() {
    let view = view();
    for saucer in visits() {
        let mut time = saucer.legs[0].begins;
        while time < saucer.ends() {
            let mut stage = Stage::new(&view, 32).expect("a stage");
            stage.reset(Camera::at(&view, Moment::default()));
            saucer.stage(time, &mut stage);
            let Some(Part::Solid { pose, .. }) = stage.parts().first().copied() else {
                panic!("the saucer is staged");
            };
            let tilt = pose
                .frame
                .y
                .dot(tairix_util::space::Vec3::UP)
                .clamp(-1.0, 1.0)
                .acos();
            assert!(tilt <= MOST_TILT + 1e-6, "tilt {tilt} at {time}");
            time += 0.05;
        }
    }
}

/// Seen from below, the dome is drawn before the lens hides it; from above,
/// after; and a rim light is lit only where the rim is in sight.
#[test]
fn its_dome_and_lights_are_drawn_as_the_camera_sees_them() {
    let view = view();
    let models = Models::new().expect("the solids");
    let saucer = visits().next().expect("a visit");
    let time = saucer.legs[1].begins;
    let camera = Camera::at(&view, Moment::default());
    let mut stage = Stage::new(&view, 32).expect("a stage");
    stage.reset(camera);
    saucer.stage(time, &mut stage);
    let solids: Vec<(HullId, Pose)> = stage
        .parts()
        .iter()
        .filter_map(|part| match *part {
            Part::Solid { hull, pose, .. } => Some((hull, pose)),
            _ => None,
        })
        .collect();
    assert_eq!(solids.len(), 2);
    let above = solids[0].1.point_to_local(camera.position()).y > CROWN;
    assert_eq!(solids[0].0 == HullId::SaucerLens, above);
    let lens = models.get(HullId::SaucerLens).expect("built");
    for part in stage.parts() {
        if let Part::Beacon { corner, .. } = *part {
            assert!((SIDES..2 * SIDES).contains(&corner), "on the rim");
        }
    }
    let rim_seen = (SIDES..2 * SIDES)
        .filter(|&corner| lens.shows_corner(usize::from(corner), &solids[0].1, &camera))
        .count();
    assert!(rim_seen > 0, "some of the rim is in sight");
}
