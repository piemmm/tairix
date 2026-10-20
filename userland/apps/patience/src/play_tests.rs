//! Line-of-play tests: grouped undo, redo, the score, and the saved line.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_rng::{NonCryptoRng, RandU64};

use super::*;
use crate::card::SpiderSuits;
use crate::variant::Draw;

fn dealt(variant: Variant, number: u32) -> Play {
    let number = DealNumber::new(variant, number).expect("a deal");
    Play::deal(Scoring::Standard.rules(variant), Scoring::Standard, number).0
}

/// A random game of `steps` moves, each followed by every safe move home as
/// the game itself makes them.
fn played(variant: Variant, number: u32, steps: usize, seed: u64) -> Play {
    let mut play = dealt(variant, number);
    let mut rng = NonCryptoRng::seed_from_u64(seed);
    let mut actions = Vec::new();
    for _ in 0..steps {
        play.table().actions(&mut actions);
        if actions.is_empty() {
            break;
        }
        let pick = usize::try_from(rng.next_below(actions.len() as u64)).expect("index");
        play.act(actions[pick]).expect("an offered action");
        while let Some(home) = play.table().safe_home(true) {
            play.follow(home).expect("a safe move");
        }
    }
    play
}

#[test]
fn an_undo_takes_back_the_players_move_with_everything_that_followed_it() {
    let mut play = dealt(Variant::FreeCell, 1);
    let start = play.table().clone();
    let mut actions = Vec::new();
    play.table().actions(&mut actions);
    play.act(actions[0]).expect("a move");
    let home = Action::Draw;
    assert_eq!(play.follow(home), None, "FreeCell has no stock");
    let after = play.table().clone();
    assert_eq!(play.moves(), 1);
    let undone = play.undo();
    assert_eq!(undone.len(), 1);
    assert_eq!(play.table(), &start);
    let redone = play.redo();
    assert_eq!(redone.len(), 1);
    assert_eq!(play.table(), &after);
}

#[test]
fn a_group_of_automatic_moves_comes_back_with_its_move() {
    let mut play = played(Variant::Klondike(Draw::One), 3, 200, 11);
    let groups = play.line().iter().filter(|step| !step.automatic).count();
    let before = play.moves();
    let mut undone = 0;
    while play.can_undo() {
        let taken = play.undo();
        assert!(!taken.is_empty());
        undone += 1;
    }
    assert_eq!(undone, groups, "one undo a player's move");
    assert_eq!(play.moves(), 0);
    let mut redone = 0;
    while play.can_redo() {
        redone += play.redo().len();
    }
    assert_eq!(redone, before, "every step made again");
}

#[test]
fn a_new_move_forgets_what_was_taken_back() {
    let mut play = played(Variant::FreeCell, 9, 10, 2);
    let _ = play.undo();
    assert!(play.can_redo());
    let mut actions = Vec::new();
    play.table().actions(&mut actions);
    play.act(actions[0]).expect("a move");
    assert!(!play.can_redo());
}

#[test]
fn a_saved_line_replays_to_the_same_game() {
    for (variant, seed) in [
        (Variant::Klondike(Draw::Three), 1),
        (Variant::FreeCell, 2),
        (Variant::Spider(SpiderSuits::Two), 3),
    ] {
        let play = played(variant, 41, 150, seed);
        let mut line = String::new();
        play.spell_line(&mut line);
        assert!(line
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".,:".contains(&b)));
        let again = Play::replay(play.rules(), play.scoring(), play.number(), &line)
            .expect("the line replays");
        assert_eq!(again.table(), play.table(), "{variant:?}");
        assert_eq!(again.moves(), play.moves());
        assert_eq!(again.score(60, 0), play.score(60, 0));
    }
}

#[test]
fn a_line_that_is_not_a_legal_game_is_refused_whole() {
    let variant = Variant::FreeCell;
    let number = DealNumber::new(variant, 1).expect("a deal");
    let rules = Scoring::Standard.rules(variant);
    for line in ["zzz", "8", "8,", "88", "::.", "8f1x", "."] {
        assert!(
            Play::replay(rules, Scoring::Standard, number, line).is_none(),
            "{line}"
        );
    }
    assert!(Play::replay(rules, Scoring::Standard, number, "").is_some());
}

#[test]
fn klondike_standard_scoring_counts_the_line_less_time_and_never_goes_negative() {
    let mut play = dealt(Variant::Klondike(Draw::One), 7);
    assert_eq!(play.score(0, 0), Some(Score::Points(0)));
    let draw = play.act(Action::Draw).expect("a draw");
    assert_eq!(standard_points(play.table().shape(), Draw::One, &draw), 0);
    assert_eq!(
        play.score(600, 0),
        Some(Score::Points(0)),
        "time never takes it below zero"
    );
}

#[test]
fn vegas_stakes_a_dollar_a_card_and_pays_five_a_card_home() {
    let variant = Variant::Klondike(Draw::Three);
    let number = DealNumber::new(variant, 7).expect("a deal");
    let (play, _) = Play::deal(Scoring::Vegas.rules(variant), Scoring::Vegas, number);
    assert_eq!(play.score(0, 0), Some(Score::Dollars(-52)));
    let (cumulative, _) = Play::deal(
        Scoring::Cumulative.rules(variant),
        Scoring::Cumulative,
        number,
    );
    assert_eq!(cumulative.score(0, 100), Some(Score::Dollars(48)));
}

#[test]
fn spider_starts_at_five_hundred_and_loses_a_point_a_move() {
    let play = played(Variant::Spider(SpiderSuits::One), 5, 12, 4);
    let runs = i64::try_from(play.table().home() / 13).expect("runs");
    let moves = i64::try_from(play.moves()).expect("moves");
    assert_eq!(
        play.score(0, 0),
        Some(Score::Points(500 + 100 * runs - moves))
    );
    assert_eq!(
        dealt(Variant::FreeCell, 1).score(10, 0),
        None,
        "FreeCell keeps no score"
    );
}
