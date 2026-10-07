use alloc::vec::Vec;

use super::*;

const WALL: Battered = Battered {
    height: 1.2,
    base: 0.8,
    top: 0.4,
    cap: 0.2,
    long: 1.6,
};

#[test]
fn a_battered_wall_narrows_from_its_foot_to_its_top() {
    assert!((WALL.half_at(0.0) - 0.4).abs() < 1e-12);
    assert!((WALL.half_at(1.2) - 0.2).abs() < 1e-12);
    assert!(WALL.half_at(0.6) < WALL.half_at(0.3));
    assert!((WALL.batter() - mathf::atan2(0.2, 1.2)).abs() < 1e-12);
}

#[test]
fn stones_run_smaller_toward_the_top_and_never_stand_far_proud_of_it() {
    let mut draws = Dice::keyed(5, 0);
    let mean = |low: f64, draws: &mut Dice| {
        (0..2000).map(|_| WALL.reach(low, draws)).sum::<f64>() / 2000.0
    };
    let (foot, head) = (mean(0.0, &mut draws), mean(1.0, &mut draws));
    assert!(
        head < 0.65 * foot,
        "{head} near the top against {foot} at the foot"
    );
    for _ in 0..2000 {
        let near = WALL.reach(1.1, &mut draws);
        assert!(
            (LEAST..=0.5 * (WALL.height + PROUD - 1.1) + 1e-12).contains(&near),
            "{near}"
        );
    }
}

/// The stretch `(0, 20)` of a wall standing whole on level ground at the
/// world's origin, its frame the world's, to be laid as far as `last`.
fn stretch(last: f64, seed: u64) -> Stretch {
    let bay = Bay {
        from: 0.0,
        to: 20.0,
        foot: Vec3::ZERO,
        frame: Frame::WORLD,
        stretch: 1.0,
        near: true,
    };
    Stretch::new(&WALL, (alloc::vec![bay], plan(Vec::new(), seed)), last).expect("room")
}

/// The plan of the stretch `(0, 20)`, tumbled where `tumbles` have it, its
/// stones drawn under `seed`.
fn plan(tumbles: Vec<Tumble>, seed: u64) -> Plan {
    Plan {
        span: (0.0, 20.0),
        spacing: 20.0,
        tumbles,
        seed,
    }
}

/// The units of the stretch `(0, 20)` laid as far as `last`, `stones` at a
/// time, and its faces as they then stand.
fn laid_to(last: f64, seed: u64, stones: usize) -> (Vec<Unit>, [Pile; 2]) {
    let mut stretch = stretch(last, seed);
    let mut units = Vec::new();
    while !stretch.raise(&WALL, &mut units, stones).expect("room").0 {}
    (units, stretch.faces)
}

#[test]
fn a_stretch_laid_less_far_lays_the_same_stones_as_far_as_it_goes() {
    for seed in 0..4 {
        let near: Vec<Unit> = laid_to(4.0, seed, usize::MAX)
            .0
            .into_iter()
            .filter(|unit| unit.placing.0.at.x <= 4.0)
            .collect();
        let far: Vec<Unit> = laid_to(13.0, seed, usize::MAX)
            .0
            .into_iter()
            .filter(|unit| unit.placing.0.at.x <= 4.0)
            .collect();
        assert!(near.len() > 100, "{seed}: {} stones", near.len());
        assert_eq!(near, far, "{seed}");
    }
}

#[test]
fn a_stretch_laid_a_few_stones_at_a_time_lays_what_it_lays_at_once() {
    for seed in 0..3 {
        let whole = laid_to(20.0, seed, usize::MAX).0;
        assert_eq!(laid_to(20.0, seed, 7).0, whole, "{seed}");
        assert_eq!(laid_to(20.0, seed, 1).0, whole, "{seed}");
    }
}

#[test]
fn a_finished_stretch_is_capped_along_its_top_its_stones_within_its_faces() {
    let (units, faces) = laid_to(20.0, 7, usize::MAX);
    for face in &faces {
        let (_, low) = face.lowest((0.0, 20.0 / WALL.long)).expect("a face");
        assert!(low >= WALL.height - DIP, "raised to the top: {low}");
    }
    let (caps, stones): (Vec<&Unit>, Vec<&Unit>) = units
        .iter()
        .partition(|unit| unit.placing.0.at.z.abs() < 1e-12);
    let capped: f64 = caps.iter().map(|cap| 2.0 * cap.placing.1.x).sum();
    assert!(
        capped > 0.7 * 20.0,
        "capped along {capped} of its 20 metres"
    );
    for cap in &caps {
        assert!(
            cap.placing.0.at.y > WALL.height - DIP,
            "on the top: {cap:?}"
        );
        assert!(
            cap.placing.1.z >= WALL.half_at(WALL.height),
            "across it: {cap:?}"
        );
    }
    assert!(stones.len() > 400, "{} stones", stones.len());
    for stone in &stones {
        let (middle, half) = (stone.placing.0.at, stone.placing.1);
        let face = middle.z.abs() + ROCK_FACE * half.z;
        assert!(
            (face - WALL.half_at(middle.y)).abs() <= 0.02 + 1e-9,
            "set by its face on the batter: {stone:?}"
        );
        assert!(
            middle.z.abs() - half.z >= 0.0,
            "short of the far face: {stone:?}"
        );
        let settled = half.y / TALL.1;
        assert!(
            middle.y + settled <= WALL.height + PROUD + 1e-9,
            "proud of the top: {stone:?}"
        );
        assert!(middle.x >= 0.0 && middle.x <= 20.0);
    }
}

#[test]
fn a_tumbled_stretch_stands_low_and_whole_again_past_its_breach() {
    let tumbles = [Tumble {
        from: 4.0,
        to: 6.0,
        low: 0.3,
        seed: 0,
    }];
    let at = |x: f64| standing(&tumbles, WALL.height, x);
    assert!((at(5.0) - 0.3 * WALL.height).abs() < 1e-12);
    assert!((at(6.0) - 0.3 * WALL.height).abs() < 1e-12);
    assert!((at(4.0 - BREACH - 0.01) - WALL.height).abs() < 1e-12);
    assert!(at(6.2) > at(6.0) && at(6.2) < WALL.height);
    assert!((standing(&[], WALL.height, 5.0) - WALL.height).abs() < 1e-12);
}

/// A tumbled stretch keeps nothing standing above what is left of it where
/// it fell, and its capstones run on over the rest.
#[test]
fn a_tumbled_stretch_keeps_nothing_above_its_breach_and_its_caps_elsewhere() {
    let bay = Bay {
        from: 0.0,
        to: 20.0,
        foot: Vec3::ZERO,
        frame: Frame::WORLD,
        stretch: 1.0,
        near: true,
    };
    let tumble = Tumble {
        from: 8.0,
        to: 10.0,
        low: 0.3,
        seed: 1,
    };
    let mut stretch = Stretch::new(
        &WALL,
        (alloc::vec![bay], plan(alloc::vec![tumble], 3)),
        20.0,
    )
    .expect("room");
    let mut units = Vec::new();
    while !stretch
        .raise(&WALL, &mut units, usize::MAX)
        .expect("room")
        .0
    {}
    let low = 0.3 * WALL.height;
    for unit in &units {
        let (middle, half) = (unit.placing.0.at, unit.placing.1);
        if (8.0..=10.0).contains(&middle.x) {
            assert!(
                middle.y + half.y <= low + 0.03 + 1e-9,
                "standing in the breach: {unit:?}"
            );
        }
    }
    let capped = units
        .iter()
        .filter(|unit| {
            unit.placing.0.at.z.abs() < 1e-12 && !(7.5..=10.5).contains(&unit.placing.0.at.x)
        })
        .count();
    assert!(capped > 20, "{capped} capstones beside the breach");
}
