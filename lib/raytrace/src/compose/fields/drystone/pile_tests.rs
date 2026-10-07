use alloc::vec::Vec;

use super::*;
use crate::sample::{mix32, unit};

/// Let a stone reaching `r` fall at `x` and set it where it rests.
fn drop(pile: &mut Pile, r: f64, x: f64) -> Stone {
    let (x, y) = pile.rest(r, x).expect("it comes to rest");
    let stone = Stone { x, y, r };
    pile.set(stone).expect("room");
    stone
}

/// A face from `start` to `end` filled to within a few centimetres of
/// `height`, stones of every size let fall where it stands lowest.
fn heaped((start, end): (f64, f64), height: f64, seed: u32) -> Pile {
    let mut pile = Pile::new((start, end)).expect("room");
    let mut count = 0u32;
    while let Some((at, _)) = pile
        .lowest((start, end))
        .filter(|&(_, low)| low < height - 0.05)
    {
        count += 1;
        assert!(count < 20_000, "it fills");
        let draw = |salt: u32| unit(mix32(seed ^ mix32(count ^ salt)));
        let r = 0.03 + 0.15 * draw(1) * draw(1);
        let x = pile.x_of(at) + r * (1.2 * draw(2) - 0.6);
        match pile.rest(r, x).filter(|&(_, y)| y + r <= height + 0.04) {
            Some((x, y)) => pile.set(Stone { x, y, r }).expect("room"),
            None => pile.bridge(at, 0.02),
        }
    }
    pile
}

#[test]
fn a_stone_let_fall_into_a_nook_rests_touching_both_its_sides() {
    let mut pile = Pile::new((0.0, 2.0)).expect("room");
    let a = drop(&mut pile, 0.1, 0.5);
    assert_eq!(
        (a.x, a.y),
        (0.5, 0.1),
        "on the bare foundation where it fell"
    );
    let b = drop(&mut pile, 0.1, 0.78);
    assert_eq!((b.x, b.y), (0.78, 0.1));
    // A small one falls through the gap to the foundation between them.
    let d = drop(&mut pile, 0.03, 0.64);
    assert_eq!((d.x, d.y), (0.64, 0.03));
    // Too broad to fall between them, it lands on one and rolls into the
    // nook they make, over the small one.
    let c = drop(&mut pile, 0.1, 0.62);
    assert!((c.x - 0.64).abs() < 1e-9, "midway: {c:?}");
    assert!(
        (c.y - (0.1 + mathf::sqrt(0.04 - 0.14 * 0.14))).abs() < 1e-9,
        "{c:?}"
    );
}

#[test]
fn a_stone_rolls_off_one_it_lands_on_and_falls_to_the_foundation_beside_it() {
    let mut pile = Pile::new((0.0, 2.0)).expect("room");
    drop(&mut pile, 0.15, 1.0);
    let rolled = drop(&mut pile, 0.05, 1.04);
    assert!(
        (rolled.x - 1.2).abs() < 1e-9 && (rolled.y - 0.05).abs() < 1e-9,
        "{rolled:?}"
    );
    let other = drop(&mut pile, 0.05, 0.97);
    assert!(
        (other.x - 0.8).abs() < 1e-9 && (other.y - 0.05).abs() < 1e-9,
        "the other way: {other:?}"
    );
}

#[test]
fn a_stone_comes_to_rest_against_the_walls_head() {
    let mut pile = Pile::new((0.0, 1.0)).expect("room");
    drop(&mut pile, 0.1, 0.84);
    let headed = drop(&mut pile, 0.1, 0.97);
    assert!(
        (headed.x - 0.9).abs() < 1e-9,
        "against the head: {headed:?}"
    );
    assert!(headed.y > 0.1, "on the one beside it: {headed:?}");
}

#[test]
fn a_heap_holds_no_stone_in_another_and_each_rests_on_what_lies_below_it() {
    for seed in 0..4 {
        let pile = heaped((0.0, 4.0), 1.1, seed);
        let stones = pile.stones();
        assert!(stones.len() > 80, "{seed}: {} stones", stones.len());
        for (index, a) in stones.iter().enumerate() {
            assert!(
                a.x - a.r >= -1e-9 && a.x + a.r <= 4.0 + 1e-9,
                "{seed}: through the head: {a:?}"
            );
            assert!(
                a.y + a.r <= 1.1 + 0.04 + 1e-9,
                "{seed}: standing proud: {a:?}"
            );
            for b in stones.iter().skip(index + 1) {
                let apart = mathf::hypot(a.x - b.x, a.y - b.y);
                assert!(
                    apart >= a.r + b.r - 1e-5,
                    "{seed}: {a:?} and {b:?} stand in each other"
                );
            }
            let rests = (a.y - a.r).abs() < 1e-6
                || stones.iter().take(index).any(|below| {
                    below.y < a.y
                        && (mathf::hypot(a.x - below.x, a.y - below.y) - (a.r + below.r)).abs()
                            < 1e-5
                });
            assert!(rests, "{seed}: {a:?} hangs in the air");
        }
    }
}

/// Coursed, a wall has a joint running its length at every course's top;
/// laid haphazard, every level through it crosses stone along most of it.
#[test]
fn a_heap_is_haphazard_no_joint_running_along_it() {
    for seed in 0..3 {
        let pile = heaped((0.0, 6.0), 1.1, seed);
        let stones: Vec<Stone> = pile.stones().to_vec();
        let mut thinnest = f64::INFINITY;
        for level in 10..=95u32 {
            let y = f64::from(level) / 100.0;
            let crossed = (0..600u32)
                .filter(|&step| {
                    let x = f64::from(step) / 100.0 + 0.005;
                    stones
                        .iter()
                        .any(|c| (x - c.x) * (x - c.x) + (y - c.y) * (y - c.y) < c.r * c.r)
                })
                .count();
            thinnest = thinnest.min(f64::from(u32::try_from(crossed).unwrap_or(0)) / 600.0);
        }
        assert!(
            thinnest > 0.45,
            "{seed}: a level crosses stone along only {thinnest} of it"
        );
    }
}
