//! Statistics tests.

use tairix_appdata::fake::FakeService;
use tairix_appdata::Settings;

use super::*;
use crate::card::SpiderSuits;
use crate::variant::Draw;

const KLONDIKE: Variant = Variant::Klondike(Draw::One);

fn outcome(won: bool, secs: u32, moves: u32) -> Outcome {
    Outcome {
        variant: KLONDIKE,
        won,
        secs,
        moves,
        score: Some(i64::from(secs)),
        banked: None,
    }
}

#[test]
fn a_win_counts_its_game_extends_the_run_and_keeps_the_best_of_each() {
    let mut stats = Stats::default();
    let _ = stats.apply(Tally::Add(outcome(true, 300, 120)));
    let _ = stats.apply(Tally::Add(outcome(true, 200, 150)));
    let record = stats.record(KLONDIKE);
    assert_eq!((record.played, record.won), (2, 2));
    assert_eq!((record.streak, record.best_streak), (2, 2));
    assert_eq!(record.fastest, Some(200));
    assert_eq!(
        record.fewest,
        Some(120),
        "the fewest moves came from the slower game"
    );
    assert_eq!(record.best_score, Some(300));
    assert_eq!(record.percent(), 100);
}

#[test]
fn a_loss_ends_a_run_of_wins_and_counts_one_of_losses() {
    let mut stats = Stats::default();
    let _ = stats.apply(Tally::Add(outcome(true, 100, 100)));
    let _ = stats.apply(Tally::Add(outcome(false, 50, 30)));
    let _ = stats.apply(Tally::Add(outcome(false, 50, 30)));
    let record = stats.record(KLONDIKE);
    assert_eq!((record.played, record.won, record.streak), (3, 1, -2));
    assert_eq!(record.best_streak, 1);
    assert_eq!(record.percent(), 33);
    let _ = stats.apply(Tally::Add(outcome(true, 100, 100)));
    assert_eq!(stats.record(KLONDIKE).streak, 1);
}

#[test]
fn cumulative_vegas_carries_its_winnings_and_a_reset_forgets_them() {
    let mut stats = Stats::default();
    let banked = Outcome {
        banked: Some(-27),
        ..outcome(false, 10, 10)
    };
    let _ = stats.apply(Tally::Add(banked));
    let _ = stats.apply(Tally::Add(banked));
    assert_eq!(stats.bank(), -54);
    let changed = stats.apply(Tally::Reset(KLONDIKE));
    assert!(changed.contains(Stat::Bank));
    assert_eq!(stats.bank(), 0);
    assert_eq!(stats.record(KLONDIKE), Record::default());
}

#[test]
fn a_reset_forgets_one_game_and_spares_the_others() {
    let mut stats = Stats::default();
    let spider = Variant::Spider(SpiderSuits::One);
    let _ = stats.apply(Tally::Add(outcome(true, 1, 1)));
    let _ = stats.apply(Tally::Add(Outcome {
        variant: spider,
        ..outcome(true, 1, 1)
    }));
    let _ = stats.apply(Tally::Reset(spider));
    assert_eq!(stats.record(spider), Record::default());
    assert_eq!(stats.record(KLONDIKE).won, 1);
}

#[test]
fn every_statistic_survives_a_round_trip_and_an_empty_record_writes_nothing() {
    let mut host = FakeService::for_word("patience");
    let mut stats = Stats::default();
    let mut changed = Keys::EMPTY;
    for tally in [
        Tally::Add(outcome(true, 61, 99)),
        Tally::Add(outcome(false, 5, 5)),
        Tally::Add(Outcome {
            banked: Some(13),
            ..outcome(false, 5, 5)
        }),
    ] {
        changed = changed.union(stats.apply(tally));
    }
    let mut store = Settings::open_without_defaults(&mut host);
    tairix_appdata::save(&stats, changed, &mut store).expect("commits");
    drop(store);
    let (loaded, refused) = Stats::load(&Settings::open_without_defaults(&mut host));
    assert!(refused.is_empty());
    assert_eq!(loaded, stats);
    assert_eq!(host.committed().get("stats.freecell.played"), None);
}

#[test]
fn a_stored_value_out_of_bounds_is_refused_and_a_contradiction_is_held_in_bounds() {
    let mut host = FakeService::for_word("patience").with_store(
        "stats.klondike-1.played = 3\nstats.klondike-1.won = 9\n\
         stats.klondike-1.streak = 7\nstats.klondike-1.fastest = 0\n\
         vegas.bank = 99999999999\n",
    );
    let (loaded, refused) = Stats::load(&Settings::open_without_defaults(&mut host));
    assert_eq!(
        refused,
        [Stat::Of(0, Field::Fastest), Stat::Bank],
        "a zero-second game and an absurd bank"
    );
    let record = loaded.record(KLONDIKE);
    assert_eq!(record.won, 3, "no more wins than games");
    assert_eq!(record.streak, 3, "no run longer than the wins");
}

#[test]
fn every_key_is_a_distinct_name() {
    let mut names: alloc::vec::Vec<&str> =
        Stats::KEYS.iter().map(|&key| Stats::name(key)).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), Stats::KEYS.len());
    assert!(names.iter().all(|name| !name.contains("unknown")));
}
