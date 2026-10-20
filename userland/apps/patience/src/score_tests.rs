//! Scoring tests.

use super::*;
use crate::table::{Action, Applied};
use crate::variant::{PileId, Variant};

fn applied(action: Action, turned: bool) -> Applied {
    let mut applied = Applied::of(action);
    applied.turned = turned;
    applied
}

fn moving(from: usize, to: usize) -> Action {
    Action::Move {
        from: PileId::new(from).expect("a pile"),
        to: PileId::new(to).expect("a pile"),
        count: 1,
    }
}

#[test]
fn standard_points_follow_the_classic_table() {
    let shape = Variant::Klondike(Draw::One).shape();
    // Klondike: stock 0, waste 1, foundations 2..6, columns 6..13.
    assert_eq!(
        standard_points(shape, Draw::One, &applied(moving(1, 6), false)),
        5
    );
    assert_eq!(
        standard_points(shape, Draw::One, &applied(moving(1, 2), false)),
        10
    );
    assert_eq!(
        standard_points(shape, Draw::One, &applied(moving(6, 2), true)),
        15
    );
    assert_eq!(
        standard_points(shape, Draw::One, &applied(moving(2, 6), false)),
        -15
    );
    assert_eq!(
        standard_points(shape, Draw::One, &applied(moving(6, 7), true)),
        5
    );
    assert_eq!(
        standard_points(shape, Draw::One, &applied(Action::Recycle, false)),
        -100
    );
    assert_eq!(
        standard_points(shape, Draw::Three, &applied(Action::Recycle, false)),
        -20
    );
}

#[test]
fn time_costs_two_points_a_ten_seconds_and_a_quick_win_earns_a_bonus() {
    assert_eq!(time_penalty(9), 0);
    assert_eq!(time_penalty(10), 2);
    assert_eq!(time_penalty(125), 24);
    assert_eq!(time_bonus(29), 0);
    assert_eq!(time_bonus(100), 7_000);
}

#[test]
fn vegas_limits_the_waste_and_standard_does_not() {
    assert_eq!(
        Scoring::Vegas.rules(Variant::Klondike(Draw::One)).recycles,
        Recycles::AtMost(0)
    );
    assert_eq!(
        Scoring::Cumulative
            .rules(Variant::Klondike(Draw::Three))
            .recycles,
        Recycles::AtMost(2)
    );
    assert_eq!(
        Scoring::Standard
            .rules(Variant::Klondike(Draw::Three))
            .recycles,
        Recycles::Unlimited
    );
    assert_eq!(
        Scoring::Vegas.rules(Variant::FreeCell).recycles,
        Recycles::Unlimited
    );
}

#[test]
fn every_scoring_is_spelled_once() {
    for scoring in Scoring::ALL {
        assert_eq!(Scoring::named(scoring.name()), Some(scoring));
    }
    assert_eq!(Scoring::named("golf"), None);
}
