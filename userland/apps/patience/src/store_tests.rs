//! Store tests: one job, one commit, nothing it never read written over, and
//! the ledger's one-write-in-flight protocol.

use alloc::vec::Vec;

use tairix_appdata::fake::FakeService;

use super::*;
use crate::prefs::{Back, Pref};
use crate::score::Scoring;
use crate::stats::Outcome;
use crate::variant::{Draw, Variant};

const KLONDIKE: Variant = Variant::Klondike(Draw::One);

fn win(secs: u32) -> Tally {
    Tally::Add(Outcome {
        variant: KLONDIKE,
        won: true,
        secs,
        moves: 100,
        score: None,
        banked: None,
    })
}

fn crimson() -> PublishJob<Preferences> {
    PublishJob::Save {
        record: Preferences {
            back: Back::Crimson,
            ..Preferences::default()
        },
        keys: Keys::of(Pref::Back),
    }
}

#[test]
fn one_job_writes_preferences_statistics_and_the_kept_game_together() {
    let mut host = FakeService::for_word("patience");
    let job = Job {
        prefs: Some(crimson()),
        tallies: alloc::vec![win(90)],
        saved: Some(Saved::default()),
    };
    let answer = carry_out(&job, &mut Settings::open_without_defaults(&mut host));
    assert_eq!(answer.saved, Ok(()));
    let stats = answer.stats.expect("written");
    assert_eq!(stats.record(KLONDIKE).won, 1);
    let prefs = answer.prefs.expect("answered").expect("written");
    assert_eq!(prefs.record.back, Back::Crimson);
    assert_eq!(host.committed().get("cards.back"), Some("crimson"));
    assert_eq!(host.committed().get("stats.klondike-1.won"), Some("1"));
}

/// The store could not be read when the game started, so its record began
/// at nothing: the games played since are added to the player's record, not
/// written over it.
#[test]
fn games_are_added_to_what_the_store_holds_when_written() {
    let mut host = FakeService::for_word("patience").with_store(
        "stats.klondike-1.played = 40\nstats.klondike-1.won = 10\n\
         stats.klondike-1.fastest = 75\ncards.back = emerald\n",
    );
    let job = Job {
        tallies: alloc::vec![win(90)],
        ..Job::default()
    };
    let answer = carry_out(&job, &mut Settings::open_without_defaults(&mut host));
    let record = answer.stats.expect("written").record(KLONDIKE);
    assert_eq!((record.played, record.won), (41, 11));
    assert_eq!(record.fastest, Some(75), "the slower win is no record");
    assert_eq!(host.committed().get("cards.back"), Some("emerald"));
}

#[test]
fn a_refused_commit_writes_nothing_and_says_so_everywhere() {
    let mut host = FakeService::for_word("patience").with_store("cards.back = emerald\n");
    host.refusal().set(Some(Errno::NoSpace));
    let job = Job {
        prefs: Some(crimson()),
        tallies: alloc::vec![win(90)],
        saved: Some(Saved::default()),
    };
    let answer = carry_out(&job, &mut Settings::open_without_defaults(&mut host));
    assert_eq!(answer.stats, Err(Errno::NoSpace));
    assert_eq!(answer.saved, Err(Errno::NoSpace));
    assert_eq!(answer.prefs, Some(Err(Errno::NoSpace)));
    host.refusal().set(None);
    assert_eq!(host.committed().get("cards.back"), Some("emerald"));
}

fn ledger() -> Ledger {
    Ledger::new(Preferences::default(), Stats::default())
}

fn answered(job: &Job, host: &mut FakeService) -> Answer {
    carry_out(job, &mut Settings::open_without_defaults(host))
}

#[test]
fn nothing_owed_is_nothing_written() {
    assert_eq!(ledger().job(), None);
}

#[test]
fn one_write_is_in_flight_and_everything_asked_meanwhile_follows_it() {
    let mut host = FakeService::for_word("patience");
    let mut ledger = ledger();
    ledger.tally(win(100));
    let first = ledger.job().expect("owed");
    assert!(ledger.writing());
    ledger.tally(win(80));
    ledger.choose(Preferences {
        back: Back::Graphite,
        ..*ledger.prefs()
    });
    assert_eq!(ledger.job(), None, "one at a time");
    assert_eq!(
        ledger.stats().record(KLONDIKE).played,
        2,
        "shown before it is written"
    );
    let mut refusals = Vec::new();
    let second = ledger
        .landed(answered(&first, &mut host), &mut refusals)
        .expect("what was owed meanwhile");
    assert_eq!(second.tallies, [win(80)]);
    assert!(second.prefs.is_some());
    assert_eq!(
        ledger.landed(answered(&second, &mut host), &mut refusals),
        None
    );
    assert!(refusals.is_empty());
    let record = ledger.stats().record(KLONDIKE);
    assert_eq!((record.played, record.fastest), (2, Some(80)));
    assert_eq!(host.committed().get("cards.back"), Some("graphite"));
}

#[test]
fn a_failed_write_is_owed_again_but_waits_for_new_work_rather_than_spinning() {
    let mut host = FakeService::for_word("patience");
    let mut ledger = ledger();
    ledger.tally(win(100));
    let job = ledger.job().expect("owed");
    host.refusal().set(Some(Errno::NoSpace));
    let mut refusals = Vec::new();
    assert_eq!(
        ledger.landed(answered(&job, &mut host), &mut refusals),
        None,
        "no retry at once"
    );
    assert_eq!(refusals, [Refusal::NotSaved(Errno::NoSpace)]);
    assert_eq!(ledger.job(), None, "still waiting for new work");
    host.refusal().set(None);
    ledger.tally(win(90));
    let retry = ledger.job().expect("new work brings the old with it");
    assert_eq!(
        retry.tallies,
        [win(100), win(90)],
        "in the order they were played"
    );
    let _ = ledger.landed(answered(&retry, &mut host), &mut refusals);
    assert_eq!(ledger.stats().record(KLONDIKE).played, 2);
}

#[test]
fn the_last_write_is_tried_once_more_as_the_game_ends() {
    let mut host = FakeService::for_word("patience");
    let mut ledger = ledger();
    ledger.keep(Saved::default());
    let job = ledger.job().expect("owed");
    host.refusal().set(Some(Errno::NoSpace));
    let mut refusals = Vec::new();
    let _ = ledger.landed(answered(&job, &mut host), &mut refusals);
    assert_eq!(ledger.job(), None);
    assert!(
        ledger.final_job().is_some(),
        "an exit tries the owed write again"
    );
}

#[test]
fn a_newer_kept_game_replaces_an_older_one_not_yet_written() {
    let mut ledger = ledger();
    let variant = Variant::FreeCell;
    let number = crate::deal::DealNumber::new(variant, 1).expect("a deal");
    let (play, _) =
        crate::play::Play::deal(Scoring::Standard.rules(variant), Scoring::Standard, number);
    ledger.keep(Saved::of(&play, 1).expect("kept"));
    ledger.keep(Saved::default());
    let job = ledger.job().expect("owed");
    assert_eq!(job.saved, Some(Saved::default()));
}
