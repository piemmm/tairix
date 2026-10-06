//! Host tests of a snowman set out: its balls stack foot to seat, each
//! smaller than the one beneath, the lowest pressed into the snow it stands
//! on and leaning by no more than hands set it; its face lies on its head;
//! and none is built where its prototypes would not fit.

use alloc::vec::Vec;

use super::*;
use crate::detail::Detail;

/// Level ground at nought, holding no grids.
fn level() -> Land {
    Land::plain(0, ((0.0, 0.0), 500.0))
}

#[test]
fn a_snowmans_balls_stack_foot_to_seat_each_smaller_than_the_last() {
    let land = level();
    for seed in 0..12u64 {
        let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
        let mut dice = Dice::keyed(seed, 0);
        let balls = stacked(&mut dice);
        let stuff = stuff(&mut stage, &mut dice).expect("its stuff");
        let placed = stack(
            &mut stage,
            &mut dice,
            &land,
            ((2.0, 3.0), &balls),
            stuff.snow,
        )
        .expect("stacked");
        let placed: Vec<Placed> = placed.iter().flatten().copied().collect();
        assert!((2..=3).contains(&placed.len()), "{seed}");
        let foot = placed[0]
            .pose
            .point_to_world(Vec3::new(0.0, placed[0].ball.foot(), 0.0));
        assert!(
            (-0.03 - 0.4..0.0).contains(&foot.y),
            "{seed}: the lowest pressed into the snow, its foot at {}",
            foot.y
        );
        for pair in placed.windows(2) {
            let (under, over) = (pair[0], pair[1]);
            assert!(over.ball.radius < under.ball.radius, "{seed}");
            let seat =
                under
                    .pose
                    .point_to_world(Vec3::new(0.0, under.ball.seat().expect("a seat"), 0.0));
            let rests = over
                .pose
                .point_to_world(Vec3::new(0.0, over.ball.foot(), 0.0));
            assert!(
                (rests.y - seat.y).abs() < 0.01 * under.ball.radius,
                "{seed}: rests at {} on a seat at {}",
                rests.y,
                seat.y
            );
            assert!(
                mathf::hypot(rests.x - seat.x, rests.z - seat.z) < 0.3 * under.ball.radius,
                "{seed}: set off its seat by hand, not beside it"
            );
            assert!(
                over.pose.frame.y.y > mathf::cos(8f64.to_radians()),
                "{seed}: crooked, not toppling"
            );
        }
    }
}

#[test]
fn a_snowmans_face_lies_on_its_head() {
    let land = level();
    let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
    let mut dice = Dice::keyed(4, 0);
    snowman(&mut stage, &mut dice, &land, ((0.0, 0.0), 0.0)).expect("a snowman");
    let placed: Vec<Pose> = stage
        .objects
        .iter()
        .filter_map(|object| match object.shape {
            Shape::Instance { pose, scale, .. } if scale < 0.05 => Some(pose),
            _ => None,
        })
        .collect();
    assert!(placed.len() >= 2, "two eyes at least");
    let highest = stage
        .objects
        .iter()
        .filter_map(|object| match object.shape {
            Shape::Instance { pose, scale, .. } if scale >= 1.0 => Some(pose.at.y),
            _ => None,
        })
        .fold(f64::NEG_INFINITY, f64::max);
    let eyes = placed
        .iter()
        .filter(|pose| pose.at.y > highest - 0.05)
        .count();
    assert!(eyes >= 2, "its eyes stand about its head's middle");
}

#[test]
fn no_snowman_is_built_where_its_prototypes_would_not_fit() {
    let land = level();
    let mut stage = Stage::new(Detail::Simple.densities()).expect("a stage");
    while stage.plannable() > PROTOTYPES - 1 {
        stage
            .plan(&Recipe::Carrot {
                length: 0.1,
                radius: 0.01,
                skin: 0,
                seed: 1,
            })
            .expect("planned");
    }
    let mut dice = Dice::keyed(4, 0);
    let objects = stage.objects.len();
    snowman(&mut stage, &mut dice, &land, ((0.0, 0.0), 0.0)).expect("no refusal");
    assert_eq!(stage.objects.len(), objects);
}
