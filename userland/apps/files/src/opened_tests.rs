use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::Errno;

use super::{OpenedVolumes, FIRST_RETRY_NS, LONGEST_RETRY_NS};

const GAME: [u8; 16] = [7; 16];
/// The instant every sweep but the retry test's is made at.
const NOW: u64 = 1_000;
const DISC: [u8; 16] = [9; 16];

/// Whether a window shows a place within `root`, given the places windows
/// show.
fn showing<'a>(places: &'a [&'a [&'a str]]) -> impl Fn(&[String]) -> bool + 'a {
    move |root| {
        places.iter().any(|place| {
            place.len() >= root.len() && place.iter().zip(root).all(|(a, b)| *a == b.as_str())
        })
    }
}

fn none_lost(_: &[u8; 16]) -> bool {
    false
}

/// The name of the volume `sweep` asks to unmount, if any.
fn asked(
    opened: &mut OpenedVolumes,
    on_show: impl Fn(&[String]) -> bool,
    lost: impl Fn(&[u8; 16]) -> bool,
) -> Option<String> {
    opened
        .sweep((NOW, true), on_show, lost)
        .map(|finished| String::from(finished.name))
}

fn path(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| String::from(*part)).collect()
}

#[test]
fn a_volume_is_unmounted_once_no_window_shows_it() {
    let mut opened = OpenedVolumes::new();
    opened.opened(GAME, "game").expect("noted");
    let inside: &[&[&str]] = &[&["Storage", "game", "Levels"]];
    assert_eq!(asked(&mut opened, showing(inside), none_lost), None);
    let finished = opened
        .sweep(
            (NOW, true),
            showing(&[&["Users", "ann", "Home"]]),
            none_lost,
        )
        .expect("owed");
    assert_eq!((finished.volume_id, finished.name), (GAME, "game"));
    assert_eq!(
        asked(&mut opened, showing(&[]), none_lost),
        None,
        "asked once, not again while its answer is on its way"
    );
    opened.landed(GAME, Ok(()), NOW);
    opened.released();
    assert_eq!(
        asked(&mut opened, showing(&[]), none_lost),
        None,
        "unmounted, it is forgotten"
    );
}

#[test]
fn a_sibling_of_a_similar_name_does_not_show_it() {
    let mut opened = OpenedVolumes::new();
    opened.opened(GAME, "game").expect("noted");
    assert_eq!(
        asked(&mut opened, showing(&[&["Storage", "game"]]), none_lost),
        None
    );
    assert_eq!(
        asked(&mut opened, showing(&[&["Storage", "games"]]), none_lost).as_deref(),
        Some("game")
    );
}

/// A volume found in use is asked about again at once when something this
/// file manager started ends, and otherwise only after a wait that doubles
/// each time it is still in use, up to the longest: never every turn.
#[test]
fn a_volume_in_use_is_asked_again_ever_more_rarely() {
    let mut opened = OpenedVolumes::new();
    opened.opened(GAME, "game").expect("noted");
    let away = showing(&[]);
    let at = |opened: &mut OpenedVolumes, now: u64| {
        opened
            .sweep((now, true), &away, none_lost)
            .map(|finished| finished.volume_id)
    };
    assert_eq!(at(&mut opened, 0), Some(GAME));
    opened.landed(GAME, Err(Errno::Busy), 0);
    assert_eq!(opened.retry_at(), Some(FIRST_RETRY_NS));
    for now in [1, FIRST_RETRY_NS - 1] {
        assert_eq!(
            at(&mut opened, now),
            None,
            "nothing is asked before its wait"
        );
    }
    assert_eq!(at(&mut opened, FIRST_RETRY_NS), Some(GAME));
    opened.landed(GAME, Err(Errno::Busy), FIRST_RETRY_NS);
    assert_eq!(
        opened.retry_at(),
        Some(FIRST_RETRY_NS * 3),
        "the second wait is twice the first"
    );
    opened.released();
    assert_eq!(
        at(&mut opened, FIRST_RETRY_NS + 1),
        Some(GAME),
        "something it started ending asks again at once"
    );
    let mut now = FIRST_RETRY_NS + 1;
    let mut longest = 0;
    for _ in 0..32 {
        opened.landed(GAME, Err(Errno::Busy), now);
        let retry = opened.retry_at().expect("waiting");
        longest = longest.max(retry - now);
        now = retry;
        assert_eq!(at(&mut opened, now), Some(GAME));
    }
    assert_eq!(longest, LONGEST_RETRY_NS, "the wait stops growing");
}

#[test]
fn a_volume_shown_again_and_left_again_is_asked_again() {
    let mut opened = OpenedVolumes::new();
    opened.opened(GAME, "game").expect("noted");
    let inside: &[&[&str]] = &[&["Storage", "game"]];
    assert_eq!(
        asked(&mut opened, showing(&[]), none_lost).as_deref(),
        Some("game")
    );
    opened.landed(GAME, Err(Errno::Busy), NOW);
    assert_eq!(asked(&mut opened, showing(inside), none_lost), None);
    assert_eq!(
        asked(&mut opened, showing(&[]), none_lost).as_deref(),
        Some("game"),
        "leaving it again is a new chance it is free"
    );
}

/// A volume found shown through a place other than its catalog entry — a
/// binding of one of its folders — is not taken for one in use, which nothing
/// would ever ask about again: that place is learned, and the volume is asked
/// about once no window shows it there either.
#[test]
fn a_volume_found_shown_elsewhere_is_asked_again_once_that_window_leaves() {
    let mut opened = OpenedVolumes::new();
    opened.opened(GAME, "game").expect("noted");
    assert_eq!(
        asked(&mut opened, showing(&[]), none_lost).as_deref(),
        Some("game")
    );
    let bound = path(&["Users", "ann", "Home", "levels"]);
    let catalog = path(&["Storage", "game"]);
    opened.shown_at(GAME, &[catalog.as_slice(), bound.as_slice()]);
    let there: &[&[&str]] = &[&["Users", "ann", "Home", "levels", "one"]];
    assert_eq!(
        asked(&mut opened, showing(there), none_lost),
        None,
        "still shown where it was found"
    );
    assert_eq!(
        asked(&mut opened, showing(&[]), none_lost).as_deref(),
        Some("game"),
        "that window gone, it is asked about again"
    );
}

/// The unmounts the user did not ask for take one place at a time, so they
/// never crowd out an eject the user does ask for.
#[test]
fn one_unmount_is_asked_for_at_a_time() {
    let mut opened = OpenedVolumes::new();
    opened.opened(GAME, "game").expect("noted");
    opened.opened(DISC, "disc").expect("noted");
    assert_eq!(
        asked(&mut opened, showing(&[]), none_lost).as_deref(),
        Some("game")
    );
    assert_eq!(
        asked(&mut opened, showing(&[]), none_lost),
        None,
        "the second waits for the first's answer"
    );
    opened.landed(GAME, Ok(()), NOW);
    assert_eq!(
        asked(&mut opened, showing(&[]), none_lost).as_deref(),
        Some("disc")
    );
}

/// An unmount is on its way from the turn it is asked for until its answer
/// lands, or it is found shown again or could not be sent.
#[test]
fn an_unmount_is_on_its_way_until_it_is_settled() {
    let mut opened = OpenedVolumes::new();
    opened.opened(GAME, "game").expect("noted");
    assert!(!opened.unmounting());
    assert!(opened.sweep((NOW, true), showing(&[]), none_lost).is_some());
    assert!(opened.unmounting());
    opened.deferred(GAME);
    assert!(!opened.unmounting(), "not sent");
    assert!(opened.sweep((NOW, true), showing(&[]), none_lost).is_some());
    opened.shown_at(GAME, &[]);
    assert!(!opened.unmounting(), "shown again");
    assert!(opened.sweep((NOW, true), showing(&[]), none_lost).is_some());
    opened.landed(GAME, Err(Errno::Busy), NOW);
    assert!(!opened.unmounting(), "answered");
}

#[test]
fn a_refusal_the_user_was_told_of_is_not_asked_about_again() {
    let mut opened = OpenedVolumes::new();
    opened.opened(GAME, "game").expect("noted");
    assert_eq!(
        asked(&mut opened, showing(&[]), none_lost).as_deref(),
        Some("game")
    );
    opened.landed(GAME, Err(Errno::DeviceFault), NOW);
    opened.released();
    assert_eq!(asked(&mut opened, showing(&[]), none_lost), None);
}

#[test]
fn nothing_is_unmounted_while_the_setting_is_off_or_the_volume_is_lost() {
    let mut opened = OpenedVolumes::new();
    opened.opened(GAME, "game").expect("noted");
    opened.opened(DISC, "disc").expect("noted");
    let inside: &[&[&str]] = &[&["Storage", "game"], &["Storage", "disc"]];
    assert_eq!(asked(&mut opened, showing(inside), none_lost), None);
    assert!(
        opened
            .sweep((NOW, false), showing(&[]), none_lost)
            .is_none(),
        "the setting off unmounts nothing"
    );
    let lost = |id: &[u8; 16]| *id == DISC;
    assert_eq!(
        asked(&mut opened, showing(&[]), lost).as_deref(),
        Some("game"),
        "turned on, it unmounts what no window shows, but a lost volume is \
         the user's alone to let go"
    );
    opened.landed(GAME, Ok(()), NOW);
    assert_eq!(
        asked(&mut opened, showing(&[]), none_lost).as_deref(),
        Some("disc"),
        "found again, it is unmounted as any other"
    );
}

#[test]
fn a_volume_unmounted_some_other_way_is_forgotten_once_it_was_listed() {
    let mut fresh = OpenedVolumes::new();
    fresh.opened(GAME, "game").expect("noted");
    fresh.mounted(|_| false);
    assert_eq!(
        asked(&mut fresh, showing(&[]), none_lost).as_deref(),
        Some("game"),
        "a fresh attach the mount table has not listed yet is kept"
    );

    let mut opened = OpenedVolumes::new();
    opened.opened(GAME, "game").expect("noted");
    opened.mounted(|id| *id == GAME);
    opened.mounted(|_| false);
    assert_eq!(asked(&mut opened, showing(&[]), none_lost), None);
}

#[test]
fn an_unmount_that_could_not_be_sent_is_asked_for_again() {
    let mut opened = OpenedVolumes::new();
    opened.opened(GAME, "game").expect("noted");
    assert_eq!(
        asked(&mut opened, showing(&[]), none_lost).as_deref(),
        Some("game")
    );
    assert_eq!(asked(&mut opened, showing(&[]), none_lost), None);
    opened.deferred(GAME);
    assert_eq!(
        asked(&mut opened, showing(&[]), none_lost).as_deref(),
        Some("game")
    );
}

#[test]
fn opening_a_volume_twice_notes_it_once() {
    let mut opened = OpenedVolumes::new();
    opened.opened(GAME, "game").expect("noted");
    opened.opened(GAME, "game").expect("noted");
    assert_eq!(
        asked(&mut opened, showing(&[]), none_lost).as_deref(),
        Some("game")
    );
    opened.landed(GAME, Ok(()), NOW);
    assert_eq!(asked(&mut opened, showing(&[]), none_lost), None);
}
