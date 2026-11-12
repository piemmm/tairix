//! The menus: their key letters, moves past rules and greyed entries, and
//! the commands chosen.

use super::*;

const ALL: Able = Able {
    selection: true,
    clipboard: true,
    searched: true,
};

#[test]
fn a_menu_is_found_by_its_letter_and_an_entry_by_its_own() {
    assert_eq!(
        (
            menu_for(b'f'),
            menu_for(b'S'),
            menu_for(b'h'),
            menu_for(b'z')
        ),
        (Some(0), Some(2), Some(4), None)
    );
    assert_eq!(
        key(0, 0, MenuKey::Letter(b'x'), ALL),
        Outcome::Chosen(Command::Exit)
    );
    assert_eq!(
        key(0, 0, MenuKey::Letter(b'A'), ALL),
        Outcome::Chosen(Command::SaveAs)
    );
    assert_eq!(
        key(1, 0, MenuKey::Letter(b't'), Able::default()),
        Outcome::Ignored,
        "no selection to cut"
    );
}

#[test]
fn moves_pass_rules_and_greyed_entries_and_wrap() {
    assert_eq!(
        key(0, 3, MenuKey::Down, ALL),
        Outcome::At(0, 5),
        "past the rule"
    );
    assert_eq!(
        key(0, 0, MenuKey::Up, ALL),
        Outcome::At(0, 7),
        "round to Exit"
    );
    let pasting = Able {
        clipboard: true,
        ..Able::default()
    };
    assert_eq!(first(1, pasting), 2, "Paste, Cut and Copy greyed");
    assert_eq!(
        key(1, 2, MenuKey::Down, pasting),
        Outcome::At(1, 2),
        "nothing else to reach"
    );
    assert_eq!(
        key(0, 0, MenuKey::Left, ALL),
        Outcome::At(4, 0),
        "round to Help"
    );
    assert_eq!(key(4, 3, MenuKey::Right, ALL), Outcome::At(0, 0));
    assert_eq!(
        key(1, 0, MenuKey::Enter, Able::default()),
        Outcome::Ignored,
        "a greyed entry is not chosen"
    );
    assert_eq!(
        key(2, 1, MenuKey::Enter, ALL),
        Outcome::Chosen(Command::Repeat)
    );
    assert_eq!(key(2, 1, MenuKey::Escape, ALL), Outcome::Closed);
}

#[test]
fn a_menu_drops_below_its_entry_and_fits_the_screen() {
    let (top, left, bottom, right) = rect(0, 80);
    assert_eq!((top, left, bottom), (1, 1, 10), "eight entries in a frame");
    assert!(right > left + 12);
    let (_, help_left, _, help_right) = rect(4, 80);
    assert!(
        help_right < 78 && help_left > 50,
        "the Help menu kept on the screen with its shadow: {help_left}..{help_right}"
    );
}
