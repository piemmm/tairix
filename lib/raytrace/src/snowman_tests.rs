//! Host tests of a snowman's pieces: a rolled ball is never round, narrower
//! along its roll axis than round it and ridged by its sheets; it stands on
//! a flat foot and carries a flat seat, closed and met from outside; a seed
//! rolls the same ball every time; a carrot tapers to its tip and a stick
//! forks; and snow rolled up is streaked round its drum, settling to its
//! mean far off.

use alloc::vec::Vec;

use super::*;
use crate::heightfield::CHANNELS;
use crate::vector::Ray;

const ROLLED: Making = Making {
    rolled: true,
    pressed: 0.14,
    seat: Some(0.06),
};

/// Directions spread evenly over the sphere.
fn directions(count: u32) -> impl Iterator<Item = Vec3> {
    (0..count).map(move |step| {
        let turn = f64::from(step) * 2.399_963;
        let rise = 1.0 - 2.0 * (f64::from(step) + 0.5) / f64::from(count);
        let level = mathf::sqrt(1.0 - rise * rise);
        Vec3::new(level * mathf::cos(turn), rise, level * mathf::sin(turn))
    })
}

#[test]
fn a_rolled_ball_is_never_round() {
    for seed in 0..8u64 {
        let ball = Ball::made(0.4, ROLLED, seed);
        let reaches: Vec<f64> = directions(400)
            .filter(|d| d.y.abs() < 0.6)
            .map(|d| ball.surface(d).length())
            .collect();
        let most = reaches.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let least = reaches.iter().copied().fold(f64::INFINITY, f64::min);
        assert!(most - least > 0.06 * 0.4, "{seed}: {least}..{most}");
        // A drum: narrower along the axis it rolled about than round it.
        let along =
            ball.surface(Vec3::new(1.0, 0.0, 0.0)).x - ball.surface(Vec3::new(-1.0, 0.0, 0.0)).x;
        let round =
            ball.surface(Vec3::new(0.0, 0.0, 1.0)).z - ball.surface(Vec3::new(0.0, 0.0, -1.0)).z;
        assert!(
            along < round,
            "{seed}: {along} along its axis, {round} round it"
        );
    }
}

#[test]
fn the_sheets_a_ball_took_up_ridge_its_drum_and_a_packed_ball_has_none() {
    let rolled = Ball::made(0.4, ROLLED, 3);
    let packed = Ball::made(
        0.4,
        Making {
            rolled: false,
            ..ROLLED
        },
        3,
    );
    // Round the drum, a sheet's lip is a step up then a long thinning: over
    // a fiftieth of a turn it rises far more steeply than it ever falls.
    let lips = |ball: &Ball| {
        let around: Vec<f64> = (0..400u32)
            .map(|step| {
                let angle = core::f64::consts::TAU * f64::from(step) / 400.0;
                ball.sheet(Vec3::new(0.0, mathf::cos(angle), mathf::sin(angle)))
            })
            .collect();
        let (mut rise, mut fall) = (0.0f64, 0.0f64);
        for step in 0..400 {
            let change = around[(step + 8) % 400] - around[step];
            rise = rise.max(change);
            fall = fall.max(-change);
        }
        (rise, fall)
    };
    let (rise, fall) = lips(&rolled);
    assert!(rise > 0.003 && rise > 3.0 * fall, "{rise} up, {fall} down");
    assert_eq!(lips(&packed), (0.0, 0.0));
}

#[test]
fn a_ball_stands_on_a_flat_foot_and_carries_a_flat_seat() {
    let ball = rolled(&Ball::made(0.4, ROLLED, 5)).expect("a ball").whole();
    let (foot, seat) = (
        Ball::made(0.4, ROLLED, 5).foot(),
        Ball::made(0.4, ROLLED, 5).seat().expect("a seat"),
    );
    for (x, z) in [(0.0, 0.0), (0.05, -0.03), (-0.06, 0.04), (0.02, 0.07)] {
        let up = Ray::new(Vec3::new(x, -5.0, z), Vec3::UP);
        let met = ball
            .intersect(&up, (0.0, 10.0), None)
            .expect("met from below");
        assert!(
            (-5.0 + met.t - foot).abs() < 0.004,
            "foot at ({x}, {z}): {} against {foot}",
            -5.0 + met.t
        );
        let down = Ray::new(Vec3::new(x, 5.0, z), -Vec3::UP);
        let met = ball
            .intersect(&down, (0.0, 10.0), None)
            .expect("met from above");
        assert!(
            (5.0 - met.t - seat).abs() < 0.004,
            "seat at ({x}, {z}): {} against {seat}",
            5.0 - met.t
        );
    }
}

#[test]
fn a_ball_is_met_from_outside_whichever_way_it_is_looked_at() {
    let ball = rolled(&Ball::made(0.3, ROLLED, 9)).expect("a ball").whole();
    for toward in directions(300) {
        let ray = Ray::new(toward * 3.0, -toward);
        let hit = ball
            .intersect(&ray, (0.0, f64::INFINITY), None)
            .expect("a closed ball is met");
        assert!(
            hit.normal.dot(ray.dir) < 0.0,
            "met from outside at {toward:?}"
        );
        assert_eq!(hit.material, None, "made in what it is placed in");
    }
}

#[test]
fn a_seed_rolls_the_same_ball_every_time_and_another_seed_another() {
    let surface = |seed: u64| {
        let ball = Ball::made(0.35, ROLLED, seed);
        directions(64).map(|d| ball.surface(d)).collect::<Vec<_>>()
    };
    assert_eq!(surface(4), surface(4));
    assert_ne!(surface(4), surface(5));
}

#[test]
fn a_carrot_tapers_to_its_tip_and_a_stick_forks() {
    let carrot = carrot(0.14, 0.015, 0, 3).expect("a carrot").whole();
    let parts = carrot.parts();
    let first = match parts.first() {
        Some(Part::Tube(tube)) => tube.radii[0],
        _ => panic!("a carrot is tubes"),
    };
    let last = match parts.last() {
        Some(Part::Tube(tube)) => tube.radii[1],
        _ => panic!("a carrot is tubes"),
    };
    assert!(
        last < 0.15 * first,
        "{first} at its crown, {last} at its tip"
    );
    assert!(carrot.bounds().max.z > 0.12, "{:?}", carrot.bounds());
    for seed in 0..6u64 {
        let stick = stick(0.6, 0.015, 0, seed).expect("a stick").whole();
        assert!(
            stick.parts().len() >= STICK_SEGMENTS as usize + TWIG_SEGMENTS as usize,
            "{seed}: {} parts",
            stick.parts().len()
        );
    }
}

/// A spot on a rolled ball at `p`, seen a pixel `width` wide.
fn spot(p: Vec3, width: f64) -> Spot {
    Spot {
        p,
        normal: p.normalized(),
        height: p.y,
        width,
        mark: 0,
        along: 0.0,
        uv: (0.0, 0.0),
        girth: 0.0,
        instance: 7,
        front: true,
        ground: [0.0; CHANNELS],
        thatch: 0.0,
    }
}

#[test]
fn snow_rolled_up_is_streaked_round_its_drum_and_settles_to_its_mean_far_off() {
    let rolled = Rolled {
        snow: Vec3::splat(0.9),
        taken: [Vec3::new(0.2, 0.15, 0.1); 3],
        streaked: 0.6,
        seed: 11,
    };
    let drum: Vec<f64> = (0..400u32)
        .map(|step| {
            let angle = core::f64::consts::TAU * f64::from(step) / 400.0;
            let across = 0.3 * (f64::from(step % 7) / 7.0 - 0.5);
            let p = Vec3::new(across, mathf::cos(angle), mathf::sin(angle)) * 0.4;
            rolled.colour(&spot(p, 1e-4)).luminance()
        })
        .collect();
    let darkest = drum.iter().copied().fold(f64::INFINITY, f64::min);
    assert!(darkest < 0.75, "streaked: {darkest}");
    // Its ends, which never touched the ground, stay clean.
    let end = rolled
        .colour(&spot(Vec3::new(0.4, 0.02, 0.0), 1e-4))
        .luminance();
    assert!(end > 0.85, "{end}");
    // Far off, its streaks settle to one shade.
    let far: Vec<f64> = (0..50u32)
        .map(|step| {
            let angle = core::f64::consts::TAU * f64::from(step) / 50.0;
            let p = Vec3::new(0.0, mathf::cos(angle), mathf::sin(angle)) * 0.4;
            rolled.colour(&spot(p, 0.2)).luminance()
        })
        .collect();
    let spread = far.iter().copied().fold(f64::NEG_INFINITY, f64::max)
        - far.iter().copied().fold(f64::INFINITY, f64::min);
    assert!(spread < 0.02, "{spread}");
}

#[test]
fn a_carrot_and_a_stick_end_in_fine_tips_never_balls() {
    let tubes = |prototype: &Prototype| -> Vec<Tube> {
        prototype
            .parts()
            .iter()
            .filter_map(|part| match part {
                Part::Tube(tube) => Some(*tube),
                _ => None,
            })
            .collect()
    };
    let farthest = |tubes: &[Tube]| {
        tubes
            .iter()
            .copied()
            .fold(None::<Tube>, |best, tube| match best {
                Some(held)
                    if crate::prototype::point(held.b).z >= crate::prototype::point(tube.b).z =>
                {
                    Some(held)
                }
                _ => Some(tube),
            })
            .expect("a limb")
    };
    let radius = 0.014;
    let nose = carrot(0.13, radius, 0, 5).expect("a carrot").whole();
    let tip = farthest(&tubes(&nose));
    assert!(
        f64::from(tip.radii[1]) < 0.02 * radius,
        "a carrot's tip: {}",
        tip.radii[1]
    );
    for seed in 0..6 {
        let arm = stick(0.5, 0.009, 0, seed).expect("a stick").whole();
        let end = farthest(&tubes(&arm));
        assert!(
            f64::from(end.radii[1]) < 0.15 * 0.009,
            "{seed}: a stick's end: {}",
            end.radii[1]
        );
    }
}
