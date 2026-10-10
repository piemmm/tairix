use alloc::vec;
use alloc::vec::Vec;

use tairix_player::{Advance, EntryId, Programme};

use super::{Edit, Playlist, Repeat};

fn ids(raw: &[u64]) -> Vec<EntryId> {
    raw.iter().copied().map(EntryId::new).collect()
}

fn listed(raw: &[u64]) -> Playlist<u64> {
    let mut playlist = Playlist::default();
    playlist.add(raw.iter().map(|&id| (EntryId::new(id), id * 10)).collect());
    playlist
}

#[test]
fn entries_play_in_the_order_they_were_added_and_carry_their_payload() {
    let playlist = listed(&[1, 2, 3]);
    assert_eq!(playlist.arranged(), ids(&[1, 2, 3]));
    assert_eq!(playlist.playing(), ids(&[1, 2, 3]));
    assert_eq!(playlist.item(EntryId::new(2)), Some(&20));
    assert_eq!(playlist.first_of_pass(0), Some(EntryId::new(1)));
    assert_eq!(
        playlist.next(EntryId::new(1), Advance::Played),
        Some(EntryId::new(2))
    );
    assert_eq!(playlist.next(EntryId::new(3), Advance::Played), None);
    assert_eq!(playlist.previous(EntryId::new(2)), Some(EntryId::new(1)));
    assert_eq!(playlist.previous(EntryId::new(1)), None);
}

#[test]
fn a_name_given_twice_keeps_its_first_payload() {
    let mut playlist = listed(&[1]);
    playlist.add(vec![(EntryId::new(1), 99)]);
    assert_eq!(playlist.len(), 1);
    assert_eq!(playlist.get(EntryId::new(1)), Some(&10));
}

#[test]
fn repeat_decides_whether_another_pass_begins_and_one_holds_the_entry() {
    let mut playlist = listed(&[1, 2]);
    assert_eq!(
        playlist.first_of_pass(1),
        None,
        "off ends with the last entry"
    );
    playlist.apply(Edit::Repeat(Repeat::All));
    assert_eq!(playlist.first_of_pass(7), Some(EntryId::new(1)));
    playlist.apply(Edit::Repeat(Repeat::One));
    let one = EntryId::new(1);
    assert_eq!(playlist.next(one, Advance::Played), Some(one));
    assert_eq!(
        playlist.next(one, Advance::Skipped),
        Some(EntryId::new(2)),
        "the listener may still move on"
    );
    assert_eq!(Repeat::Off.next(), Repeat::All);
    assert_eq!(Repeat::One.next(), Repeat::Off);
    for mode in [Repeat::Off, Repeat::All, Repeat::One] {
        assert_eq!(Repeat::from_word(mode.word()), Some(mode));
    }
    assert_eq!(Repeat::from_word("sometimes"), None);
}

#[test]
fn a_moved_entry_plays_where_it_was_put() {
    let mut playlist = listed(&[1, 2, 3, 4]);
    playlist.apply(Edit::Move {
        entry: EntryId::new(4),
        to: 1,
    });
    assert_eq!(playlist.arranged(), ids(&[1, 4, 2, 3]));
    assert_eq!(playlist.playing(), ids(&[1, 4, 2, 3]));
    playlist.apply(Edit::Move {
        entry: EntryId::new(1),
        to: usize::MAX,
    });
    assert_eq!(
        playlist.arranged(),
        ids(&[4, 2, 3, 1]),
        "clamped to the end"
    );
    playlist.apply(Edit::Move {
        entry: EntryId::new(9),
        to: 0,
    });
    assert_eq!(
        playlist.arranged(),
        ids(&[4, 2, 3, 1]),
        "a stranger moves nothing"
    );
}

#[test]
fn a_shuffle_is_a_stable_permutation_its_seed_decides() {
    let raw: Vec<u64> = (1..=40).collect();
    let mut playlist = listed(&raw);
    playlist.apply(Edit::Shuffle(Some(7)));
    let shuffled = playlist.playing().to_vec();
    assert_ne!(shuffled, ids(&raw), "forty entries in their own order");
    let mut sorted = shuffled.clone();
    sorted.sort();
    assert_eq!(sorted, ids(&raw), "a permutation");
    assert_eq!(
        playlist.arranged(),
        ids(&raw),
        "the arrangement is the listener's"
    );

    let mut same = listed(&raw);
    same.apply(Edit::Shuffle(Some(7)));
    assert_eq!(same.playing(), shuffled, "two copies agree");

    playlist.add(vec![(EntryId::new(41), 410)]);
    playlist.apply(Edit::Remove(ids(&[shuffled[5].get()])));
    let survivors: Vec<EntryId> = playlist
        .playing()
        .iter()
        .copied()
        .filter(|&entry| entry != EntryId::new(41))
        .collect();
    let expected: Vec<EntryId> = shuffled
        .iter()
        .copied()
        .filter(|&entry| entry != shuffled[5])
        .collect();
    assert_eq!(survivors, expected, "nothing else moved");

    playlist.apply(Edit::Shuffle(None));
    assert_eq!(playlist.playing(), playlist.arranged());
}

#[test]
fn a_removed_entry_answers_what_followed_it_until_the_engine_settles() {
    let mut playlist = listed(&[1, 2, 3, 4]);
    playlist.apply(Edit::Remove(ids(&[2, 3])));
    assert_eq!(playlist.arranged(), ids(&[1, 4]));
    assert_eq!(playlist.item(EntryId::new(2)), None);
    assert_eq!(
        playlist.next(EntryId::new(2), Advance::Played),
        Some(EntryId::new(4)),
        "past every entry removed with it"
    );
    assert_eq!(
        playlist.next(EntryId::new(3), Advance::Played),
        Some(EntryId::new(4))
    );
    playlist.settle();
    assert_eq!(playlist.next(EntryId::new(2), Advance::Played), None);

    playlist.apply(Edit::Clear);
    assert!(playlist.is_empty());
    assert_eq!(playlist.next(EntryId::new(4), Advance::Played), None);
    assert_eq!(playlist.first_of_pass(0), None);
}
