//! `EDIT` in a machine: started on a file, typed into, saved and left, the
//! screen it found put back; a file named that is not there made by its
//! first save; the menus reached with Alt alone and the copy key its
//! filter gives; the Open dialog's listing; a save refused by a
//! write-protected disk leaving the file as it was; and the mouse.

use super::*;
use crate::chips::keyboard::Scan;
use crate::dos::harness::{
    diskette, diskette_bytes, press, protected_floppy, screen, settle, type_ahead, typed,
};
use crate::dos::utility::tests::{run, with_utilities, with_utilities_in};
use crate::machine::{Machine, Template};
use alloc::format;
use alloc::string::String;

const ALT: Scan = Scan::plain(0x38);
const CTRL: Scan = Scan::plain(0x1D);
const SHIFT: Scan = Scan::plain(0x2A);
const ESC: Scan = Scan::plain(0x01);
const ENTER: Scan = Scan::plain(0x1C);
const INS: Scan = Scan::plain(0x52);
const KEYPAD_6: Scan = Scan::plain(0x4D);
const END: Scan = Scan::plain(0x4F);

/// The key of scan code `code`, by its letter.
const fn letter(code: u8) -> Scan {
    Scan::plain(code)
}
const F: Scan = letter(0x21);
const S: Scan = letter(0x1F);
const X: Scan = letter(0x2D);
const O: Scan = letter(0x18);
const N: Scan = letter(0x31);

/// `scan` pressed and let go, then the machine run until it waits again.
fn tap(m: &mut Machine, scan: Scan) {
    press(m, &[(scan, true), (scan, false)]);
    settle(m);
}

/// `scan` pressed and let go with `held` down.
fn with(m: &mut Machine, held: Scan, scan: Scan) {
    press(
        m,
        &[(held, true), (scan, true), (scan, false), (held, false)],
    );
    settle(m);
}

fn keyed(m: &mut Machine, text: &str) {
    type_ahead(m, text);
    settle(m);
}

/// The frame's upright, as the screen's text shows it.
const UPRIGHT: char = '\u{B3}';

fn row(m: &Machine, n: usize) -> String {
    screen(m).lines().nth(n).unwrap_or_default().into()
}

#[test]
fn the_command_line_names_a_file_and_the_switches() {
    let all = request(b" /b NOTES.TXT /h/nohi /G").unwrap();
    assert_eq!(
        (all.asked, all.file),
        (
            Asked {
                mono: true,
                most: true,
                plain: true
            },
            Some(&b"NOTES.TXT"[..])
        )
    );
    assert_eq!(
        request(b"a.txt/B").unwrap().file,
        Some(&b"a.txt"[..]),
        "a switch straight after the name"
    );
    assert_eq!(request(b" one two"), Err(Refusal::TooMany(b"two")));
    assert_eq!(request(b" /q"), Err(Refusal::Switch(b"/q")));
    assert_eq!(request(b" x/b/zz"), Err(Refusal::Switch(b"/zz")));
    assert_eq!(request(b""), Ok(Request::default()));
}

#[test]
fn a_file_is_edited_saved_and_left_the_screen_put_back() {
    let mut m = with_utilities(&[("NOTES.TXT", b"first line\r\n")]);
    typed(&mut m, "cls");
    typed(&mut m, "edit notes.txt");
    assert!(
        row(&m, 0).starts_with("  File  Edit  Search  Options"),
        "{}",
        screen(&m)
    );
    assert!(row(&m, 1).contains(" NOTES.TXT "), "{}", screen(&m));
    assert!(
        row(&m, 2).starts_with(&format!("{UPRIGHT}first line")),
        "{}",
        screen(&m)
    );
    assert!(row(&m, 24).ends_with("00001:001"), "{}", screen(&m));
    keyed(&mut m, "new ");
    with(&mut m, ALT, F);
    tap(&mut m, S);
    with(&mut m, ALT, F);
    tap(&mut m, X);
    let text = screen(&m);
    assert!(
        text.contains("A:\\>edit notes.txt\n") && !text.contains("File  Edit"),
        "the screen as it was found:\n{text}"
    );
    let shown = run(&mut m, "type notes.txt");
    assert!(shown.contains("\nnew first line\n"), "{shown}");
}

/// The files `DIR` counts in the current directory.
fn files_listed(m: &mut Machine) -> u32 {
    let listing = run(m, "dir");
    let line = listing
        .lines()
        .find(|line| line.contains("file(s)"))
        .unwrap_or_else(|| panic!("{listing}"));
    line.split_whitespace()
        .next()
        .and_then(|count| count.parse().ok())
        .unwrap_or_else(|| panic!("{line}"))
}

#[test]
fn a_file_named_that_is_not_there_is_made_by_its_first_save() {
    let mut m = with_utilities(&[]);
    let before = files_listed(&mut m);
    typed(&mut m, "edit fresh.txt");
    assert!(row(&m, 1).contains(" FRESH.TXT "), "{}", screen(&m));
    keyed(&mut m, "hello");
    with(&mut m, ALT, F);
    tap(&mut m, X);
    assert!(
        screen(&m).contains("Loaded file is not saved. Save it now?"),
        "{}",
        screen(&m)
    );
    tap(&mut m, Scan::plain(0x15));
    let shown = run(&mut m, "type fresh.txt");
    assert!(shown.contains("\nhello\n"), "{shown}");
    assert_eq!(
        files_listed(&mut m),
        before + 1,
        "the file, and no other left beside it"
    );
}

#[test]
fn alt_alone_takes_the_menu_bar_and_a_dialog_is_reached_through_it() {
    let mut m = with_utilities(&[]);
    typed(&mut m, "edit");
    assert!(row(&m, 1).contains(" Untitled "), "{}", screen(&m));
    tap(&mut m, ALT);
    tap(&mut m, O);
    tap(&mut m, ENTER);
    assert!(
        screen(&m).contains(" Display "),
        "Options, Display:\n{}",
        screen(&m)
    );
    assert!(screen(&m).contains("Colors for normal text:"));
    tap(&mut m, ESC);
    assert!(!screen(&m).contains("Colors for normal text:"));
    with(&mut m, ALT, F);
    tap(&mut m, X);
    assert!(
        screen(&m).contains("A:\\>edit"),
        "left, nothing to save:\n{}",
        screen(&m)
    );
}

#[test]
fn ctrl_ins_copies_what_shift_and_the_keypad_selected() {
    let mut m = with_utilities(&[("COPY.TXT", b"abc")]);
    typed(&mut m, "edit copy.txt");
    press(
        &mut m,
        &[
            (SHIFT, true),
            (KEYPAD_6, true),
            (KEYPAD_6, false),
            (KEYPAD_6, true),
            (KEYPAD_6, false),
            (SHIFT, false),
        ],
    );
    settle(&mut m);
    with(&mut m, CTRL, INS);
    tap(&mut m, END);
    with(&mut m, SHIFT, INS);
    with(&mut m, ALT, F);
    tap(&mut m, S);
    with(&mut m, ALT, F);
    tap(&mut m, X);
    let shown = run(&mut m, "type copy.txt");
    assert!(shown.contains("\nabcab\n"), "{shown}");
}

#[test]
fn open_lists_the_directorys_files_and_reads_one_in() {
    let mut m = with_utilities(&[
        ("ONE.TXT", b"the first\r\n"),
        ("TWO.TXT", b"the second\r\n"),
    ]);
    typed(&mut m, "edit");
    with(&mut m, ALT, F);
    tap(&mut m, O);
    let text = screen(&m);
    assert!(text.contains("File Name: [*.TXT"), "{text}");
    assert!(text.contains("A:\\"), "{text}");
    assert!(
        text.contains("ONE.TXT") && text.contains("TWO.TXT") && text.contains("[-A-]"),
        "{text}"
    );
    assert!(
        !text.contains("EDIT.COM"),
        "only the pattern's files:\n{text}"
    );
    keyed(&mut m, "two.txt\r");
    assert!(row(&m, 1).contains(" TWO.TXT "), "{}", screen(&m));
    assert!(row(&m, 2).contains("the second"), "{}", screen(&m));
    with(&mut m, ALT, F);
    tap(&mut m, X);
}

#[test]
fn a_save_the_disk_refuses_says_why_and_leaves_the_file_whole() {
    let mut m = with_utilities(&[("KEEP.TXT", b"original\r\n")]);
    typed(&mut m, "edit keep.txt");
    let mut disk = m.board_mut().eject_floppy(0).unwrap();
    let image = diskette_bytes(&mut disk);
    m.board_mut()
        .insert_floppy(0, protected_floppy(image))
        .unwrap();
    keyed(&mut m, "changed ");
    with(&mut m, ALT, F);
    tap(&mut m, S);
    assert!(
        screen(&m).contains("Disk is write-protected"),
        "{}",
        screen(&m)
    );
    tap(&mut m, ENTER);
    with(&mut m, ALT, F);
    tap(&mut m, X);
    tap(&mut m, N);
    let shown = run(&mut m, "type keep.txt");
    assert!(shown.contains("\noriginal\n"), "{shown}");
}

/// The mouse moved by `dx`, `dy` with `buttons` held, and the machine run
/// until the editor waits again.
fn mouse(m: &mut Machine, dx: i32, dy: i32, buttons: u8) {
    assert!(m.board_mut().mouse(dx, dy, buttons), "COM1 has the mouse");
    settle(m);
}

/// The pointer moved `columns` cells across and `rows` down, a mickey a
/// report, slowly enough that the driver never doubles its speed; the
/// driver counts eight mickeys a cell across and sixteen down.
fn point(m: &mut Machine, columns: i32, rows: i32) {
    for _ in 0..(columns * 8).abs() {
        mouse(m, columns.signum(), 0, 0);
    }
    for _ in 0..(rows * 16).abs() {
        mouse(m, 0, rows.signum(), 0);
    }
}

fn click(m: &mut Machine) {
    mouse(m, 0, 0, 1);
    mouse(m, 0, 0, 0);
}

#[test]
fn the_mouse_places_the_cursor_and_opens_a_menu() {
    let driver = crate::dos::mouse_com().unwrap();
    let files: [(&str, &[u8]); 3] = [
        ("CONFIG.SYS", b"INSTALL=A:\\MOUSE.COM\r\n"),
        ("MOUSE.COM", &driver),
        ("TEXT.TXT", b"one\r\ntwo two\r\n"),
    ];
    let mut m = with_utilities_in(
        crate::dos::harness::machine_of(&Template::IbmPc.config(), [None, None]),
        &diskette(),
        &files,
    );
    typed(&mut m, "edit text.txt");
    for _ in 0..8 {
        mouse(&mut m, -127, -127, 0);
    }
    point(&mut m, 5, 3);
    click(&mut m);
    assert!(
        row(&m, 24).ends_with("00002:005"),
        "the cursor where it was clicked:\n{}",
        screen(&m)
    );
    point(&mut m, -2, -3);
    click(&mut m);
    assert!(
        screen(&m).contains("Save As..."),
        "the File menu opened:\n{}",
        screen(&m)
    );
    tap(&mut m, ESC);
    with(&mut m, ALT, F);
    tap(&mut m, X);
}
