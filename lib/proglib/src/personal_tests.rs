//! Unit tests for the account's record of its library.

use alloc::string::String;
use alloc::vec::Vec;

use super::*;

const DAY: i64 = 24 * 60 * 60;
const HALF: i64 = 14 * DAY;

fn id(text: &str) -> EntryId {
    EntryId::new(text).expect("a valid identifier")
}

fn launched(personal: &mut Personal, entry: &str, at: i64) -> Vec<EntryId> {
    personal
        .apply(&Edit::Launched(id(entry), Some(at)))
        .expect("room")
}

fn favourite(personal: &mut Personal, entry: &str) -> Vec<EntryId> {
    personal.apply(&Edit::Favourite(id(entry))).expect("room")
}

fn round_trip(personal: &Personal) -> Personal {
    let stored: Vec<(String, String)> = personal
        .keeps()
        .filter_map(|(entry, _)| Some((key_of(entry), personal.stored(entry)?)))
        .collect();
    let (read, refused) = Personal::load(
        stored
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );
    assert!(refused.is_empty(), "a rendered record refused: {refused:?}");
    read
}

#[test]
fn a_weight_halves_every_half_life_and_never_rises() {
    let half = u64::try_from(HALF).expect("positive");
    assert_eq!(decayed(1000, 0), 1000);
    assert_eq!(decayed(1000, half), 500);
    assert_eq!(decayed(1000, 2 * half), 250);
    assert_eq!(decayed(1000, half / 2), 750, "linear within a half-life");
    assert_eq!(decayed(u32::MAX, 64 * half), 0);
    let mut last = decayed(MAX_WEIGHT, 0);
    for step in 0..400 {
        let now = decayed(MAX_WEIGHT, step * half / 10);
        assert!(now <= last, "rose at step {step}");
        last = now;
    }
}

#[test]
fn a_launch_adds_its_weight_to_what_has_decayed() {
    let mut personal = Personal::new();
    assert_eq!(launched(&mut personal, "a", 1_000), [id("a")]);
    let used = personal.keep(&id("a")).used().expect("used");
    assert_eq!(used.weight_at(Some(1_000)), LAUNCH_WEIGHT);
    launched(&mut personal, "a", 1_000 + HALF);
    let used = personal.keep(&id("a")).used().expect("used");
    assert_eq!(used.weight_at(Some(1_000 + HALF)), 1_500);
    assert_eq!(used.last(), 1_000 + HALF);
    // A clock behind the last launch decays nothing.
    assert_eq!(used.weight_at(Some(0)), 1_500);
}

#[test]
fn a_record_cannot_outgrow_its_ceiling() {
    let mut personal = Personal::new();
    for _ in 0..1_500 {
        launched(&mut personal, "a", 7);
    }
    let used = personal.keep(&id("a")).used().expect("used");
    assert_eq!(used.weight_at(Some(7)), MAX_WEIGHT);
}

#[test]
fn a_launch_the_clock_cannot_place_counts_but_starts_nothing() {
    let mut personal = Personal::new();
    assert_eq!(
        personal.apply(&Edit::Launched(id("a"), None)),
        Ok(Vec::new())
    );
    assert!(
        personal.is_empty(),
        "a record with no time would decay from the epoch"
    );
    launched(&mut personal, "a", 50);
    assert_eq!(
        personal.apply(&Edit::Launched(id("a"), None)),
        Ok(alloc::vec![id("a")])
    );
    let used = personal.keep(&id("a")).used().expect("used");
    assert_eq!(used.last(), 50);
    assert_eq!(used.weight_at(Some(50)), 2 * LAUNCH_WEIGHT);
}

#[test]
fn favourites_keep_the_order_they_were_added_and_move_one_place() {
    let mut personal = Personal::new();
    for entry in ["c", "a", "b"] {
        favourite(&mut personal, entry);
    }
    assert_eq!(personal.shelf(None).favourites, [id("c"), id("a"), id("b")]);
    assert_eq!(
        favourite(&mut personal, "a"),
        Vec::<EntryId>::new(),
        "already one"
    );

    let moved = personal.apply(&Edit::MoveEarlier(id("b"))).expect("move");
    assert_eq!(moved.len(), 2);
    assert_eq!(personal.shelf(None).favourites, [id("c"), id("b"), id("a")]);
    let moved = personal.apply(&Edit::MoveLater(id("c"))).expect("move");
    assert_eq!(moved.len(), 2);
    assert_eq!(personal.shelf(None).favourites, [id("b"), id("c"), id("a")]);
    // At either end there is nowhere to go.
    assert_eq!(personal.apply(&Edit::MoveEarlier(id("b"))), Ok(Vec::new()));
    assert_eq!(personal.apply(&Edit::MoveLater(id("a"))), Ok(Vec::new()));
    assert_eq!(personal.apply(&Edit::MoveLater(id("x"))), Ok(Vec::new()));
}

#[test]
fn an_unfavourite_leaves_a_gap_the_next_favourite_closes() {
    let mut personal = Personal::new();
    for entry in ["a", "b", "c"] {
        favourite(&mut personal, entry);
    }
    assert_eq!(
        personal.apply(&Edit::Unfavourite(id("a"))),
        Ok(alloc::vec![id("a")])
    );
    assert_eq!(personal.stored(&id("a")), None, "nothing else was kept");
    let changed = favourite(&mut personal, "d");
    assert_eq!(personal.shelf(None).favourites, [id("b"), id("c"), id("d")]);
    for entry in ["b", "c", "d"] {
        assert!(changed.contains(&id(entry)), "{entry} renumbered or added");
    }
    assert_eq!(personal.keep(&id("d")).favourite(), Some(3));
}

#[test]
fn two_favourites_of_one_rank_still_read_one_way_and_renumber_on_a_move() {
    let (mut personal, refused) =
        Personal::load([("library.b", "favourite=1"), ("library.a", "favourite=1")]);
    assert!(refused.is_empty());
    assert_eq!(personal.shelf(None).favourites, [id("a"), id("b")]);
    personal.apply(&Edit::MoveLater(id("a"))).expect("move");
    assert_eq!(personal.shelf(None).favourites, [id("b"), id("a")]);
    assert_eq!(personal.keep(&id("b")).favourite(), Some(1));
    assert_eq!(personal.keep(&id("a")).favourite(), Some(2));
}

#[test]
fn forgetting_use_keeps_a_favourite_and_drops_an_empty_record() {
    let mut personal = Personal::new();
    launched(&mut personal, "kept", 10);
    favourite(&mut personal, "kept");
    launched(&mut personal, "gone", 10);
    assert_eq!(
        personal.apply(&Edit::Forget(id("gone"))),
        Ok(alloc::vec![id("gone")])
    );
    assert_eq!(personal.stored(&id("gone")), None);
    assert_eq!(personal.apply(&Edit::Forget(id("gone"))), Ok(Vec::new()));

    launched(&mut personal, "other", 10);
    let forgotten = personal.apply(&Edit::ForgetAll).expect("forget");
    assert_eq!(forgotten, [id("kept"), id("other")]);
    assert_eq!(personal.keep(&id("kept")).favourite(), Some(1));
    assert_eq!(personal.keep(&id("kept")).used(), None);
    assert_eq!(personal.len(), 1);
}

#[test]
fn the_shelf_ranks_the_frequently_used_without_the_favourites() {
    let mut personal = Personal::new();
    launched(&mut personal, "daily", 0);
    launched(&mut personal, "daily", DAY);
    launched(&mut personal, "daily", 2 * DAY);
    launched(&mut personal, "once", 2 * DAY);
    launched(&mut personal, "faded", -(10 * HALF));
    launched(&mut personal, "pinned", 2 * DAY);
    favourite(&mut personal, "pinned");
    let shelf = personal.shelf(Some(2 * DAY));
    assert_eq!(shelf.favourites, [id("pinned")]);
    assert_eq!(shelf.frequent, [id("daily"), id("once")]);
    assert!(
        shelf.weights.contains_key(&id("pinned")),
        "weights rank every used one"
    );
    assert_eq!(shelf.weights.get(&id("faded")), Some(&0));
}

#[test]
fn ties_rank_the_latest_first_then_by_entry() {
    let mut personal = Personal::new();
    launched(&mut personal, "b", 100);
    launched(&mut personal, "a", 100);
    launched(&mut personal, "c", 200);
    let shelf = personal.shelf(Some(200));
    // All three weigh the same: `c` wins on being the latest, and `a` and `b`
    // tie on both and fall back to the entry.
    assert_eq!(shelf.frequent, [id("c"), id("a"), id("b")]);
}

#[test]
fn a_full_record_evicts_the_least_used_and_names_it() {
    let mut personal = Personal::new();
    for n in 0..MAX_RECORDS {
        let entry = alloc::format!("e{n}");
        launched(&mut personal, &entry, i64::try_from(n).expect("small"));
    }
    favourite(&mut personal, "e0");
    let changed = launched(&mut personal, "new", 10_000);
    assert_eq!(personal.len(), MAX_RECORDS);
    assert_eq!(
        changed,
        [id("e1"), id("new")],
        "the oldest non-favourite went"
    );
    assert_eq!(personal.stored(&id("e1")), None);
    assert!(personal.keep(&id("e0")).favourite().is_some());
}

#[test]
fn a_record_of_nothing_but_favourites_refuses_another_entry() {
    let mut personal = Personal::new();
    for n in 0..MAX_RECORDS {
        favourite(&mut personal, &alloc::format!("f{n}"));
    }
    let before = personal.clone();
    assert_eq!(
        personal.apply(&Edit::Launched(id("new"), Some(0))),
        Err(RecordFull)
    );
    assert_eq!(personal.apply(&Edit::Favourite(id("new"))), Err(RecordFull));
    assert_eq!(personal, before, "a refused edit changes nothing");
}

#[test]
fn every_record_renders_and_reads_back_as_itself() {
    let mut personal = Personal::new();
    launched(&mut personal, "used", -5);
    favourite(&mut personal, "fav");
    launched(&mut personal, "both", 1_760_000_000);
    favourite(&mut personal, "both");
    assert_eq!(personal.stored(&id("fav")).as_deref(), Some("favourite=1"));
    assert_eq!(
        personal.stored(&id("used")).as_deref(),
        Some("weight=1000 last=-5")
    );
    assert_eq!(
        personal.stored(&id("both")).as_deref(),
        Some("favourite=2 weight=1000 last=1760000000")
    );
    assert_eq!(round_trip(&personal), personal);
}

#[test]
fn a_record_outside_the_grammar_is_left_out_and_named() {
    let hostile = [
        "",
        " ",
        "favourite=1  weight=2 last=3",
        "favourite=0",
        "favourite=01",
        "favourite=+1",
        "favourite=65536",
        "favourite=1 favourite=2",
        "weight=5",
        "last=5",
        "weight=0 last=5",
        "weight=1000001 last=5",
        "weight=-1 last=5",
        "weight=5 last=-0",
        "colour=red",
        "favourite",
        "none favourite=1",
        "NONE",
    ];
    for value in hostile {
        let (personal, refused) =
            Personal::load([("library.a", value), ("library.b", "favourite=1")]);
        assert_eq!(refused, ["library.a"], "{value:?}");
        assert_eq!(personal.shelf(None).favourites, [id("b")], "{value:?}");
    }
}

#[test]
fn only_keys_of_the_record_are_read_and_bad_entries_are_named() {
    let (personal, refused) = Personal::load([
        ("sound.login", "chime"),
        ("libraryx.a", "favourite=1"),
        ("library", "favourite=1"),
        ("library.A", "favourite=1"),
        ("library.a.b.c.d.e.f.g.h", "favourite=1"),
        ("library.ok", "favourite=1"),
    ]);
    assert_eq!(personal.len(), 1);
    assert_eq!(refused, ["library.A", "library.a.b.c.d.e.f.g.h"]);
}

#[test]
fn a_tombstone_keeps_nothing_and_takes_no_place_in_the_bound() {
    let (personal, refused) = Personal::load([("library.a", FORGOTTEN)]);
    assert!(refused.is_empty());
    assert!(personal.is_empty());
}

#[test]
fn reading_more_records_than_the_bound_names_the_rest() {
    let stored: Vec<(String, &str)> = (0..=MAX_RECORDS)
        .map(|n| (alloc::format!("library.e{n}"), "favourite=1"))
        .collect();
    let (personal, refused) =
        Personal::load(stored.iter().map(|(key, value)| (key.as_str(), *value)));
    assert_eq!(personal.len(), MAX_RECORDS);
    assert_eq!(refused.len(), 1);
}

#[test]
fn adopting_the_stores_answer_replaces_or_removes_the_record() {
    let mut personal = Personal::new();
    favourite(&mut personal, "a");
    assert_eq!(personal.adopt(&id("a"), Some("weight=2000 last=9")), Ok(()));
    assert_eq!(personal.keep(&id("a")).favourite(), None);
    assert_eq!(personal.adopt(&id("a"), Some("junk")), Err(RecordRefused));
    assert!(
        personal.keep(&id("a")).used().is_some(),
        "a refused answer changes nothing"
    );
    assert_eq!(personal.adopt(&id("a"), Some(FORGOTTEN)), Ok(()));
    assert!(personal.is_empty());
    favourite(&mut personal, "a");
    assert_eq!(personal.adopt(&id("a"), None), Ok(()));
    assert!(personal.is_empty());
}

#[test]
fn every_entry_key_is_a_legal_configuration_key() {
    for entry in ["a", "os.tairix.terminal", "a.b.c.d.e.f.g", "0-x_y"] {
        let key = key_of(&id(entry));
        assert_eq!(tairix_appconf::validate_key(&key), Ok(()), "{key}");
        assert_eq!(entry_of_key(&key), Some(Ok(id(entry))));
    }
}
