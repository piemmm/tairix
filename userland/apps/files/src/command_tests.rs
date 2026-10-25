//! Host tests for the file manager's command line.
//!
//! Every decision the running program makes about its argument vector is a
//! pure function of that vector, so all of it is exercised here without a
//! kernel: which spellings open a folder, which are turned down and with what
//! reason, and which are a usage error refused outright.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_browse::BrowseError;

use super::{
    listing_refusal, parse, unlistable_reason, Command, Ladder, Role, Start, UsageError, USAGE,
};

/// The [`Start`] a command line produced, or the test's own failure text when
/// it was a usage error or the short help.
fn start_of(args: &[&str]) -> Start {
    match parse(args) {
        Ok(Command::Open(start)) => start,
        other => panic!("expected an open command, got {other:?}"),
    }
}

/// The accepted starting location of a command line, as owned components.
fn location_of(args: &[&str]) -> Option<Vec<String>> {
    start_of(args).location
}

/// The refusal a command line stated, if any.
fn refusal_of(args: &[&str]) -> Option<String> {
    start_of(args).refused
}

#[test]
fn an_absolute_directory_is_accepted_as_the_starting_location() {
    assert_eq!(
        location_of(&["/Users/ada/Documents"]),
        Some(vec![
            "Users".to_string(),
            "ada".to_string(),
            "Documents".to_string(),
        ])
    );
    assert_eq!(refusal_of(&["/Users/ada/Documents"]), None);
}

#[test]
fn redundant_separators_and_a_bare_root_still_name_a_real_place() {
    // The shared path grammar collapses leading, trailing, and repeated
    // separators, so the same directory spelled loosely opens the same place.
    assert_eq!(
        location_of(&["//Users//ada//"]),
        Some(vec!["Users".to_string(), "ada".to_string()])
    );
    // The bare root view is a location like any other: no components, and no
    // refusal — it is not confused with "no argument".
    let root = start_of(&["/"]);
    assert_eq!(root.location, Some(Vec::new()));
    assert_eq!(root.refused, None);
}

#[test]
fn no_argument_opens_the_home_directory_without_complaint() {
    let start = start_of(&[]);
    assert_eq!(start.location, None);
    assert_eq!(start.refused, None);
    assert_eq!(start, Start::default());
}

#[test]
fn a_traversal_argument_is_refused_and_falls_back_to_home() {
    let start = start_of(&["/Users/ada/../../System/Security"]);
    assert_eq!(start.location, None);
    let refused = start.refused.expect("a traversal states its reason");
    assert!(
        refused.contains("\"..\""),
        "names the offending segment: {refused}"
    );
    assert!(
        refused.contains(super::FALLBACK),
        "states the fallback: {refused}"
    );
    // A lone `.` is refused by the same rule, so a spelling can never mean a
    // different directory than it reads as.
    assert_eq!(location_of(&["/Users/./ada"]), None);
}

#[test]
fn an_over_long_argument_is_refused_before_it_is_parsed() {
    // One byte past the kernel's own path bound. The refusal never echoes the
    // spelling back: a hostile argument is not replayed at the error stream.
    let mut spelling = String::from("/");
    while spelling.len() <= tairix_abi::fs::FS_PATH_MAX {
        spelling.push('a');
    }
    let start = start_of(&[spelling.as_str()]);
    assert_eq!(start.location, None);
    let refused = start
        .refused
        .expect("an over-long location states its reason");
    assert!(
        refused.contains("longer than"),
        "states the bound: {refused}"
    );
    assert!(
        !refused.contains(&spelling),
        "does not echo the argument back"
    );
}

#[test]
fn an_over_long_single_component_is_refused() {
    // Within the whole-path bound, but one name past the per-name bound: the
    // shared filename rule, not a second copy of it here, is what refuses it.
    let mut spelling = String::from("/");
    for _ in 0..=tairix_path::MAX_COMPONENT_LEN {
        spelling.push('a');
    }
    let start = start_of(&[spelling.as_str()]);
    assert_eq!(start.location, None);
    assert!(start.refused.is_some());
}

#[test]
fn a_relative_or_alias_rooted_argument_is_refused() {
    for spelling in ["Users/ada", "", "-", "Home:/Documents"] {
        let start = start_of(&[spelling]);
        assert_eq!(start.location, None, "{spelling:?} must not be accepted");
        assert!(
            start
                .refused
                .as_deref()
                .is_some_and(|refused| refused.contains(super::FALLBACK)),
            "{spelling:?} must state the fallback"
        );
    }
    // A relative spelling says exactly why it was turned down.
    let refused = refusal_of(&["Users/ada"]).expect("a relative path states its reason");
    assert!(refused.contains("not an absolute path"), "{refused}");
}

#[test]
fn a_control_character_in_the_argument_is_refused_and_escaped_when_stated() {
    let refused = refusal_of(&["/Users/a\u{1b}[2Jb"]).expect("a control character is refused");
    assert!(
        !refused.contains('\u{1b}'),
        "the escape must not be replayed at the error stream: {refused}"
    );
    assert!(refused.contains("\\u{1b}"), "{refused}");
}

#[test]
fn a_second_operand_is_refused_rather_than_ignored() {
    assert_eq!(
        parse(&["/Users/ada", "/System"]),
        Err(UsageError::ExtraOperand("/System".to_string()))
    );
    assert_eq!(
        UsageError::ExtraOperand("/System".to_string()).to_string(),
        "extra operand '/System'"
    );
}

/// GNU's words for each refusal.
fn refused(args: &[&str]) -> String {
    parse(args).expect_err("refused").to_string()
}

#[test]
fn a_refused_option_is_worded_as_getopt_words_it() {
    assert_eq!(refused(&["--grid"]), "unrecognized option '--grid'");
    assert_eq!(refused(&["-x", "/Users/ada"]), "invalid option -- 'x'");
    assert_eq!(
        refused(&["--desktop=yes"]),
        "option '--desktop' doesn't allow an argument"
    );
    assert_eq!(
        UsageError::NotUtf8.to_string(),
        "argument vector is not valid UTF-8"
    );
}

#[test]
fn a_long_option_may_be_abbreviated() {
    assert_eq!(start_of(&["--desk"]).role, Role::Desktop);
    assert_eq!(parse(&["--he"]), Ok(Command::Help));
}

#[test]
fn the_reserved_short_help_switches_win_wherever_they_appear() {
    for args in [
        &["-h"][..],
        &["-?"][..],
        &["--help"][..],
        &["/Users/ada", "-h"][..],
        &["-h", "--not-an-option"][..],
    ] {
        assert_eq!(parse(args), Ok(Command::Help), "{args:?}");
    }
    // The banner names the operand and the switches, so a usage refusal tells
    // the caller the whole grammar.
    assert!(USAGE.contains("[path]"));
    assert!(USAGE.contains(tairix_window::DESKTOP_ROLE_SWITCH));
    assert!(USAGE.contains("-h"));
}

#[test]
fn the_ordinary_role_is_the_default_and_the_switch_is_the_only_way_into_the_other() {
    // Nothing a caller can spell reaches the component role except the one
    // shared switch, which is what keeps the desktop's own instance unique.
    assert_eq!(start_of(&[]).role, Role::Window);
    assert_eq!(start_of(&["/Users/ada"]).role, Role::Window);
    assert_eq!(
        start_of(&[tairix_window::DESKTOP_ROLE_SWITCH]).role,
        Role::Desktop
    );
    // The switch is an option, so `--` puts it out of reach: past the end of
    // options it is an operand, and one that is not a path at that.
    let after_end = start_of(&["--", tairix_window::DESKTOP_ROLE_SWITCH]);
    assert_eq!(after_end.role, Role::Window);
    assert!(after_end.refused.is_some());
}

#[test]
fn a_component_takes_no_starting_location() {
    // It opens no window until one is asked for, so a location names nothing.
    // Refused outright rather than dropped, in either order.
    assert_eq!(
        parse(&[tairix_window::DESKTOP_ROLE_SWITCH, "/Users/ada"]),
        Err(UsageError::LocationInDesktopRole("/Users/ada".to_string()))
    );
    assert_eq!(
        parse(&["/Users/ada", tairix_window::DESKTOP_ROLE_SWITCH]),
        Err(UsageError::LocationInDesktopRole("/Users/ada".to_string()))
    );
    let refusal = UsageError::LocationInDesktopRole("/Users/ada".to_string()).to_string();
    assert!(refusal.contains("/Users/ada"));
    assert!(refusal.contains(tairix_window::DESKTOP_ROLE_SWITCH));
    // And it starts nowhere: no location, and nothing refused to report.
    let component = start_of(&[tairix_window::DESKTOP_ROLE_SWITCH]);
    assert_eq!(component.location, None);
    assert_eq!(component.refused, None);
}

#[test]
fn end_of_options_lets_a_dash_leading_directory_through() {
    // After `--` the next argument is an operand, even one shaped like an
    // option — the short-help switch no longer wins — and it is still
    // validated as a path, so this one is refused rather than opened.
    let after_end = start_of(&["--", "-h"]);
    assert_eq!(after_end.location, None);
    assert!(after_end.refused.is_some());
    assert_eq!(
        location_of(&["--", "/Users/-h"]),
        Some(vec!["Users".to_string(), "-h".to_string()])
    );
    // `--` does not become an operand itself, so a second one is not an extra
    // operand — but a real second operand after it still is.
    assert_eq!(
        parse(&["--", "/a", "/b"]),
        Err(UsageError::ExtraOperand("/b".to_string()))
    );
}

#[test]
fn an_unlistable_location_states_the_place_why_and_the_fallback() {
    let home = ["Users".to_string(), "ada".to_string()];
    let files = ["Users", "ada", "Home"].map(String::from);
    assert_eq!(
        unlistable_reason(&files, "it does not exist", &home),
        "could not list /Users/ada/Home (it does not exist); opening /Users/ada instead"
    );
    // The root view spells as `/` rather than as nothing.
    assert_eq!(
        unlistable_reason(&home, "you may not read it", &[]),
        "could not list /Users/ada (you may not read it); opening / instead"
    );
}

#[test]
fn a_refused_listing_says_why_in_the_users_terms() {
    assert_eq!(
        listing_refusal(BrowseError::Source(Errno::PermissionDenied)),
        "you may not read it"
    );
    assert_eq!(
        listing_refusal(BrowseError::Source(Errno::NotFound)),
        listing_refusal(BrowseError::NoSuchEntry)
    );
    assert_eq!(
        listing_refusal(BrowseError::Source(Errno::NotADirectory)),
        listing_refusal(BrowseError::NotADirectory)
    );
    assert_eq!(
        listing_refusal(BrowseError::Source(Errno::DeviceOffline)),
        "its device was removed"
    );
    assert_eq!(
        listing_refusal(BrowseError::Source(Errno::OutOfRange)),
        "it could not be read"
    );
}

/// Owned components of an absolute spelling, for the ladder tests.
fn place(path: &[&str]) -> Vec<String> {
    path.iter()
        .map(|component| (*component).to_string())
        .collect()
}

/// Every place `ladder` tries, refusing each in turn.
fn tried(mut ladder: Ladder) -> Vec<Vec<String>> {
    let mut tried = vec![ladder.place().expect("a ladder has a place").to_vec()];
    while ladder.step("it does not exist").is_some() {
        tried.push(ladder.place().expect("a step lands on a place").to_vec());
    }
    assert_eq!(ladder.place(), None, "the last refusal leaves no place");
    tried
}

#[test]
fn a_named_place_is_tried_first_then_the_bare_open_places_then_the_root() {
    let home = place(&["Users", "ada"]);
    assert_eq!(
        tried(Ladder::new(
            Some(place(&["Storage", "Backup"])),
            Some(&home),
            None
        )),
        vec![
            place(&["Storage", "Backup"]),
            place(&["Users", "ada", "Home"]),
            home.clone(),
            Vec::new(),
        ]
    );
}

/// A window asked for no folder opens where the settings say, and falls back
/// down the same ladder when that place is refused; one asked for a folder
/// falls back to it after the folder.
#[test]
fn the_start_place_is_tried_before_the_bare_open_places() {
    let home = place(&["Users", "ada"]);
    let desktop = place(&["Users", "ada", "Desktop"]);
    assert_eq!(
        tried(Ladder::new(None, Some(&home), Some(desktop.clone()))),
        vec![
            desktop.clone(),
            place(&["Users", "ada", "Home"]),
            home.clone(),
            Vec::new(),
        ]
    );
    assert_eq!(
        tried(Ladder::new(
            Some(place(&["System"])),
            Some(&home),
            Some(desktop.clone())
        )),
        vec![
            place(&["System"]),
            desktop,
            place(&["Users", "ada", "Home"]),
            home,
            Vec::new(),
        ]
    );
}

/// A place the ladder reaches twice is tried once, wherever the repeat falls:
/// naming home (which the bare-open places reach second) or the root (which
/// the ladder always ends on) must not try the refused place a second time.
#[test]
fn a_place_reached_twice_is_tried_once() {
    let home = place(&["Users", "ada"]);
    let files = place(&["Users", "ada", "Home"]);
    assert_eq!(
        tried(Ladder::new(Some(home.clone()), Some(&home), None)),
        vec![home.clone(), files.clone(), Vec::new()]
    );
    assert_eq!(
        tried(Ladder::new(Some(Vec::new()), Some(&home), None)),
        vec![Vec::new(), files.clone(), home.clone()]
    );
    assert_eq!(
        tried(Ladder::new(Some(files.clone()), Some(&home), None)),
        vec![files, home, Vec::new()]
    );
}

/// With no home to fall back to, the root view is all a bare open has.
#[test]
fn without_a_home_the_ladder_ends_at_the_root() {
    assert_eq!(
        tried(Ladder::new(None, None, None)),
        vec![Vec::<String>::new()]
    );
    assert_eq!(
        tried(Ladder::new(Some(place(&["System"])), None, None)),
        vec![place(&["System"]), Vec::new()]
    );
}

/// Each step states its own hop, while what the window says is the whole
/// move: from where it was asked to open to where it opens.
#[test]
fn a_step_states_its_hop_and_the_window_states_the_whole_move() {
    let home = place(&["Users", "ada"]);
    let mut ladder = Ladder::new(Some(place(&["Storage", "Backup"])), Some(&home), None);
    assert_eq!(ladder.moved(), None, "still where it was asked to open");

    assert_eq!(
        ladder.step("its device was removed").as_deref(),
        Some(
            "could not list /Storage/Backup (its device was removed); \
             opening /Users/ada/Home instead"
        )
    );
    assert_eq!(
        ladder.step("you may not read it").as_deref(),
        Some("could not list /Users/ada/Home (you may not read it); opening /Users/ada instead")
    );
    assert_eq!(
        ladder.moved().as_deref(),
        Some("could not list /Storage/Backup (its device was removed); opening /Users/ada instead"),
        "why the asked place was refused, not the last hop"
    );
}

/// A file named where a folder was expected is shown in the folder holding it,
/// selected, and that is no move to state; the folder is tried once, and only
/// in place of the file itself.
#[test]
fn a_file_asked_for_is_shown_selected_in_its_own_folder() {
    let home = place(&["Users", "ada"]);
    let folder = place(&["Users", "ada", "Home"]);
    let note = place(&["Users", "ada", "Home", "notes.txt"]);
    let mut ladder = Ladder::new(Some(note), Some(&home), None);
    assert!(
        !ladder.show_instead(BrowseError::Source(Errno::PermissionDenied)),
        "a refusal is no file"
    );
    assert!(ladder.show_instead(BrowseError::Source(Errno::NotADirectory)));
    assert_eq!(ladder.place(), Some(folder.as_slice()));
    assert_eq!(ladder.shows_in(&folder), Some("notes.txt"));
    assert_eq!(
        ladder.moved(),
        None,
        "showing a file in its folder is no move"
    );
    assert!(
        !ladder.show_instead(BrowseError::Source(Errno::NotADirectory)),
        "the folder itself is never taken for a file"
    );
    assert_eq!(
        ladder.step("you may not read it").as_deref(),
        Some("could not list /Users/ada/Home (you may not read it); opening /Users/ada instead"),
        "the folder already on the ladder is not tried twice"
    );
    assert_eq!(
        ladder.shows_in(&home),
        None,
        "the file is not in the fallback"
    );
    assert_eq!(
        ladder.moved().as_deref(),
        Some("could not list /Users/ada/Home (you may not read it); opening /Users/ada instead"),
        "the move names the folder the file was to be shown in"
    );

    let mut fallen = Ladder::new(Some(place(&["Storage", "Backup"])), Some(&home), None);
    assert!(fallen.step("its device was removed").is_some());
    assert!(
        !fallen.show_instead(BrowseError::Source(Errno::NotADirectory)),
        "a fallback place is never taken for a file"
    );
}
