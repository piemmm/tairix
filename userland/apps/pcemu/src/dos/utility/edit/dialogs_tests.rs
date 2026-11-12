//! The dialogs: focus moved among their controls, fields edited, lists
//! chosen from by key and by mouse, the boxes drawn and kept in memory.

use super::*;
use alloc::vec;

/// Lists of fixed entries.
struct Fixed {
    files: Vec<&'static [u8]>,
    directories: Vec<&'static [u8]>,
}

impl Lists for Fixed {
    fn count(&mut self, source: Source) -> u16 {
        let len = match source {
            Source::Files => self.files.len(),
            Source::Directories => self.directories.len(),
            Source::Foreground => COLOURS.len(),
            Source::Background => 8,
        };
        u16::try_from(len).unwrap()
    }

    fn entry(&mut self, source: Source, n: u16) -> Vec<u8> {
        let n = usize::from(n);
        match source {
            Source::Files => self.files[n].to_vec(),
            Source::Directories => self.directories[n].to_vec(),
            Source::Foreground | Source::Background => COLOURS[n].to_vec(),
        }
    }
}

fn lists() -> Fixed {
    Fixed {
        files: vec![
            b"ALPHA.TXT",
            b"BETA.TXT",
            b"BRAVO.TXT",
            b"CHARLIE.TXT",
            b"DELTA.TXT",
            b"ECHO.TXT",
            b"FOX.TXT",
            b"GOLF.TXT",
            b"HOTEL.TXT",
            b"INDIA.TXT",
        ],
        directories: vec![b"..", b"DOCS", b"[-A-]", b"[-C-]"],
    }
}

fn typed(dialog: &mut Dialog, text: &[u8]) {
    for &byte in text {
        dialog.key(DialogKey::Char(byte), &mut lists());
    }
}

#[test]
fn tab_moves_among_the_controls_and_buttons_that_take_the_focus() {
    let mut dialog = Dialog::open(Kind::Open);
    assert_eq!(dialog.control(), Some(NAME_FIELD), "the name first");
    dialog.key(DialogKey::Tab, &mut lists());
    assert!(
        matches!(
            dialog.control(),
            Some(Control::List {
                source: Source::Files,
                ..
            })
        ),
        "the path line passed over"
    );
    dialog.key(DialogKey::Tab, &mut lists());
    dialog.key(DialogKey::Tab, &mut lists());
    assert_eq!(dialog.button(), Some(Button::Ok));
    for _ in 0..3 {
        dialog.key(DialogKey::Tab, &mut lists());
    }
    assert_eq!(dialog.control(), Some(NAME_FIELD), "round to the first");
    dialog.key(DialogKey::BackTab, &mut lists());
    assert_eq!(dialog.button(), Some(Button::Help));
    assert_eq!(
        dialog.key(DialogKey::Enter, &mut lists()),
        Outcome::Pressed(Button::Help),
        "the button focused"
    );
    let mut print = Dialog::open(Kind::Print);
    print.choose(1);
    assert_eq!(
        (print.choice, print.focus),
        (1, 1),
        "the whole document, its option the only one focused"
    );
    print.key(DialogKey::Up, &mut lists());
    assert_eq!(
        (print.choice, print.focus),
        (0, 0),
        "the arrows choose and the focus follows"
    );
    print.key(DialogKey::Tab, &mut lists());
    assert_eq!(
        print.button(),
        Some(Button::Ok),
        "the option not chosen passed over"
    );
    assert_eq!(
        print.key(DialogKey::Escape, &mut lists()),
        Outcome::Pressed(Button::Cancel)
    );
}

#[test]
fn alt_and_a_letter_reach_a_control_or_press_a_button() {
    let mut find = Dialog::open(Kind::Find);
    assert_eq!(find.key(DialogKey::Alt(b'u'), &mut lists()), Outcome::Kept);
    assert_eq!(
        find.checks,
        check::MATCH_CASE,
        "the check box set as it is reached"
    );
    find.key(DialogKey::Alt(b'w'), &mut lists());
    assert_eq!(find.checks, check::MATCH_CASE | check::WHOLE_WORD);
    find.key(DialogKey::Char(b' '), &mut lists());
    assert_eq!(find.checks, check::MATCH_CASE, "Space clears it again");
    assert_eq!(
        find.key(DialogKey::Alt(b'h'), &mut lists()),
        Outcome::Pressed(Button::Help)
    );
    let mut unsaved = Dialog::open(Kind::Unsaved);
    assert_eq!(
        unsaved.key(DialogKey::Char(b'n'), &mut lists()),
        Outcome::Pressed(Button::No),
        "a letter alone, no field focused"
    );
    find.key(DialogKey::Alt(b'f'), &mut lists());
    find.key(DialogKey::Char(b'h'), &mut lists());
    assert_eq!(find.fields[0].text(), b"h", "typed into the field focused");
    let mut change = Dialog::open(Kind::Change);
    assert_eq!(
        change.key(DialogKey::Alt(b'a'), &mut lists()),
        Outcome::Pressed(Button::ChangeAll)
    );
    assert_eq!(
        change.key(DialogKey::Enter, &mut lists()),
        Outcome::Pressed(Button::Verify),
        "the first button by default"
    );
}

#[test]
fn a_field_reached_is_replaced_by_typing_and_scrolls_to_its_cursor() {
    let mut find = Dialog::open(Kind::Find);
    find.fields[0].set(b"old");
    find.fields[0].fresh = true;
    typed(&mut find, b"new");
    assert_eq!(find.fields[0].text(), b"new", "replaced, not added to");
    find.key(DialogKey::Home, &mut lists());
    typed(&mut find, b"a ");
    find.key(DialogKey::End, &mut lists());
    find.key(DialogKey::Backspace, &mut lists());
    find.key(DialogKey::Home, &mut lists());
    find.key(DialogKey::Delete, &mut lists());
    assert_eq!(find.fields[0].text(), b" ne");
    let long = [b'x'; 60];
    typed(&mut find, &long);
    assert_eq!(
        (find.fields[0].cursor, find.fields[0].left),
        (60, 21),
        "forty columns show the cursor"
    );
    typed(&mut find, &[b'y'; 30]);
    assert_eq!(
        usize::from(find.fields[0].len),
        FIELD_LEN,
        "no more than it holds"
    );
}

#[test]
fn a_list_is_chosen_from_by_arrow_page_and_letter() {
    let mut open = Dialog::open(Kind::Open);
    open.key(DialogKey::Tab, &mut lists());
    assert_eq!(
        open.key(DialogKey::Down, &mut lists()),
        Outcome::Picked(Source::Files)
    );
    assert_eq!(open.picks[0], 1);
    open.key(DialogKey::PageDown, &mut lists());
    assert_eq!(
        (open.picks[0], open.tops[0]),
        (9, 2),
        "a page on, the window following"
    );
    open.key(DialogKey::Home, &mut lists());
    assert_eq!((open.picks[0], open.tops[0]), (0, 0));
    open.key(DialogKey::Char(b'b'), &mut lists());
    open.key(DialogKey::Char(b'b'), &mut lists());
    assert_eq!(open.picks[0], 2, "the next starting with the letter");
    open.key(DialogKey::Char(b'b'), &mut lists());
    assert_eq!(open.picks[0], 1, "round again");
    assert_eq!(
        open.key(DialogKey::Enter, &mut lists()),
        Outcome::Taken(Source::Files)
    );
    open.key(DialogKey::Tab, &mut lists());
    open.key(DialogKey::Char(b'c'), &mut lists());
    assert_eq!(open.picks[1], 3, "a drive by its letter");
}

#[test]
fn the_find_dialog_is_drawn_with_its_controls_and_buttons() {
    let mut cells = Cells::new(25, 80);
    let mut find = Dialog::open(Kind::Find);
    find.fields[0].set(b"needle");
    find.checks = check::WHOLE_WORD;
    find.draw(&mut Canvas {
        cells: &mut cells,
        palette: &Palette::COLOUR,
        path: b"",
        lists: &mut lists(),
    });
    let rect = find.rect(25, 80);
    assert_eq!(
        (rect.top, rect.left, rect.bottom, rect.right),
        (8, 10, 16, 69)
    );
    let row = |n: u8| -> Vec<u8> {
        cells.grid[usize::from(n) * 80..usize::from(n) * 80 + 80]
            .iter()
            .map(|cell| cell.to_le_bytes()[0])
            .collect()
    };
    assert!(
        row(8).windows(6).any(|window| window == b" Find "),
        "the title"
    );
    assert_eq!(&row(10)[13..39], b"Find What: [needle        ");
    assert_eq!(&row(12)[13..17], b"[ ] ");
    assert_eq!(&row(12)[45..59], b"[X] Whole Word");
    let buttons = row(15);
    let text: Vec<u8> = buttons
        .iter()
        .copied()
        .filter(|&byte| byte != b' ')
        .collect();
    assert!(text.ends_with(b"<OK><Cancel><Help>\xB3"), "{text:?}");
    assert_eq!(
        find.cursor(25, 80),
        Some((10, 25 + 6)),
        "after the text, the field reached"
    );
    let note = Dialog::message(b"A long message that needs a wide box to hold it");
    assert_eq!(
        note.rect(25, 80).right - note.rect(25, 80).left + 1,
        55,
        "a message's box fits its note"
    );
}

#[test]
fn the_mouse_presses_buttons_sets_boxes_and_chooses_entries() {
    let mut find = Dialog::open(Kind::Find);
    let screen = (25, 80);
    assert_eq!(
        find.click(screen, (12, 14), false, &mut lists()),
        Outcome::Kept
    );
    assert_eq!(
        (find.checks, find.focus),
        (check::MATCH_CASE, 1),
        "set and focused"
    );
    let rect = find.rect(25, 80);
    let ok = find.button_columns(rect)[0];
    assert_eq!(
        find.click(screen, (rect.bottom - 1, ok + 1), false, &mut lists()),
        Outcome::Pressed(Button::Ok)
    );
    assert_eq!(
        find.click(screen, (rect.bottom - 1, ok + 7), false, &mut lists()),
        Outcome::Kept,
        "between buttons"
    );
    find.fields[0].set(b"abcdef");
    find.click(screen, (10, 27), false, &mut lists());
    assert_eq!(
        (find.focus, find.fields[0].cursor),
        (0, 2),
        "the field's cursor where it was pressed"
    );
    let mut open = Dialog::open(Kind::Open);
    let rect = open.rect(25, 80);
    let (top, left) = rect.at(7, 2);
    assert_eq!(
        open.click(screen, (top + 3, left + 4), false, &mut lists()),
        Outcome::Picked(Source::Files)
    );
    assert_eq!(open.picks[0], 2);
    assert_eq!(
        open.click(screen, (top + 3, left + 4), true, &mut lists()),
        Outcome::Taken(Source::Files)
    );
    assert_eq!(
        open.click(screen, (top + 9, left + 4), false, &mut lists()),
        Outcome::Picked(Source::Files),
        "the frame's foot"
    );
    assert_eq!((open.picks[0], open.tops[0]), (8, 1), "a page on");
}

#[test]
fn a_dialog_kept_in_memory_comes_back_the_same() {
    use crate::frames::Field as _;
    let mut change = Dialog::open(Kind::Change);
    change.fields[0].set(b"find");
    change.fields[1].set(b"replace");
    change.note.set(b"note");
    change.checks = check::MATCH_CASE;
    change.tops = [1, 2];
    change.picks = [3, 4];
    change.focus = 5;
    let mut bytes = vec![0u8; <Dialog as crate::frames::Field>::SIZE];
    change.put(&mut bytes, &mut 0);
    assert_eq!(Dialog::take(&bytes, &mut 0), change);
    bytes[0] = 99;
    bytes[1] = 200;
    let mut damaged = Dialog::take(&bytes, &mut 0);
    assert_eq!(
        damaged.kind,
        Kind::None,
        "a number no dialog has opens none"
    );
    assert_eq!(
        damaged.key(DialogKey::Tab, &mut lists()),
        Outcome::Kept,
        "and keys meet nothing"
    );
    assert_eq!(
        damaged.key(DialogKey::Enter, &mut lists()),
        Outcome::Pressed(Button::Cancel)
    );
}
