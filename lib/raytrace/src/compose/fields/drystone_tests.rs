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
    let mean = |low: f64, draws: &mut Dice| (0..2000).map(|_| WALL.reach(low, draws)).sum::<f64>() / 2000.0;
    let (foot, head) = (mean(0.0, &mut draws), mean(1.0, &mut draws));
    assert!(head < 0.65 * foot, "{head} near the top against {foot} at the foot");
    for _ in 0..2000 {
        let near = WALL.reach(1.1, &mut draws);
        assert!((LEAST..=0.5 * (WALL.height + PROUD - 1.1) + 1e-12).contains(&near), "{near}");
    }
}

/// The stones of the stretch `(0, 20)` laid as far as `last`, and its faces
/// as they then stand.
fn laid_to(last: f64, seed: u64) -> (Vec<Placed>, [Pile; 2]) {
    let mut placed = Vec::new();
    let faces = courses(&WALL, ((0.0, 20.0), last), seed, &mut |stone| {
        placed.push(stone);
        Some(())
    })
    .expect("room");
    (placed, faces)
}

#[test]
fn a_stretch_laid_less_far_lays_the_same_stones_as_far_as_it_goes() {
    for seed in 0..4 {
        let near: Vec<Placed> = laid_to(4.0, seed).0.into_iter().filter(|stone| stone.middle.x <= 4.0).collect();
        let far: Vec<Placed> = laid_to(13.0, seed).0.into_iter().filter(|stone| stone.middle.x <= 4.0).collect();
        assert!(near.len() > 100, "{seed}: {} stones", near.len());
        assert_eq!(near, far, "{seed}");
    }
}

#[test]
fn a_finished_stretch_is_capped_along_its_top_its_stones_within_its_faces() {
    let (placed, faces) = laid_to(20.0, 7);
    for face in &faces {
        let (_, low) = face.lowest((0.0, 20.0 / WALL.long)).expect("a face");
        assert!(low >= WALL.height - DIP, "raised to the top: {low}");
    }
    let (caps, stones): (Vec<&Placed>, Vec<&Placed>) = placed.iter().partition(|stone| stone.middle.z.abs() < 1e-12);
    let capped: f64 = caps.iter().map(|cap| 2.0 * cap.half.x).sum();
    assert!(capped > 0.7 * 20.0, "capped along {capped} of its 20 metres");
    for cap in &caps {
        assert!(cap.middle.y > WALL.height - DIP, "on the top: {cap:?}");
        assert!(cap.half.z >= WALL.half_at(WALL.height), "across it: {cap:?}");
    }
    assert!(stones.len() > 400, "{} stones", stones.len());
    for stone in &stones {
        let face = stone.middle.z.abs() + ROCK_FACE * stone.half.z;
        assert!((face - WALL.half_at(stone.middle.y)).abs() <= 0.02 + 1e-9, "set by its face on the batter: {stone:?}");
        assert!(stone.middle.z.abs() - stone.half.z >= 0.0, "short of the far face: {stone:?}");
        let settled = stone.half.y / TALL.1;
        assert!(stone.middle.y + settled <= WALL.height + PROUD + 1e-9, "proud of the top: {stone:?}");
        assert!(stone.middle.x >= 0.0 && stone.middle.x <= 20.0);
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
