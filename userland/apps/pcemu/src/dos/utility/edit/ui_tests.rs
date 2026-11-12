//! The editor whole: keys typed, moved with, chosen from the menus and the
//! dialogs, and the mouse; its state read back from its record after each.

use super::super::fixture::{holds, Run, ALT, CTRL, SHIFT};
use super::*;

const ENTER: u16 = 0x1C0D;
const ESC: u16 = 0x011B;
const ALT_DOWN: u16 = u16::from_le_bytes([0, own::ALT_DOWN]);
const ALT_UP: u16 = u16::from_le_bytes([0, own::ALT_UP]);
const DOWN: u16 = 0x5000;
const UP: u16 = 0x4800;
const SHIFT_RIGHT: u16 = 0x4D36;

fn run_with(text: &[u8]) -> Run {
    let mut run = Run::new(4096);
    run.typed(text);
    run.key(0x7700, CTRL);
    run
}

#[test]
fn typing_moving_and_the_status_line_follow_the_cursor() {
    let mut run = Run::new(4096);
    run.typed(b"hello");
    run.key(ENTER, 0);
    run.typed(b"world");
    assert_eq!(run.document(), b"hello\r\nworld");
    assert_eq!(
        (run.ui.editor.cursor.line, run.ui.editor.cursor.column),
        (1, 5)
    );
    assert_eq!(
        run.ui.cursor(),
        Some((3, 6)),
        "the hardware cursor over the window's cell"
    );
    assert!(run.row(24).ends_with(b"00002:006 "), "{:?}", run.row(24));
    run.key(UP, 0);
    run.key(0x4F00, 0);
    assert_eq!(run.ui.editor.cursor.column, 5, "End");
    run.key(0x5200, 0);
    assert!(run.ui.overtype(), "Ins types over");
    run.typed(b"!?");
    assert_eq!(
        run.document(),
        b"hello!?\r\nworld",
        "past the end, overtyping adds"
    );
}

#[test]
fn alt_alone_takes_the_menu_bar_and_alt_with_a_letter_opens_a_menu() {
    let mut run = Run::new(4096);
    run.key(ALT_DOWN, ALT);
    assert!(
        run.row(0).starts_with(b"  File"),
        "the bar while Alt is down"
    );
    run.key(ALT_UP, 0);
    assert_eq!(
        (run.ui.focus, run.ui.menu),
        (Focus::Bar, 0),
        "pressed and let go alone"
    );
    run.key(0x4D00, 0);
    run.key(DOWN, 0);
    assert_eq!(
        (run.ui.focus, run.ui.menu, run.ui.item),
        (Focus::Menu, 1, 3),
        "Edit, every entry greyed, the last shown"
    );
    run.key(ESC, 0);
    assert_eq!(run.ui.focus, Focus::Text);
    run.key(ALT_DOWN, ALT);
    run.key(0x2100, ALT);
    run.key(ALT_UP, 0);
    assert_eq!(
        (run.ui.focus, run.ui.menu),
        (Focus::Menu, 0),
        "Alt+F, Alt then let go not taken as alone"
    );
    assert_eq!(
        run.key(0x2D78, 0),
        Action::Exit,
        "E&xit, the document unchanged"
    );
}

#[test]
fn leaving_a_changed_document_asks_first() {
    let mut run = Run::new(4096);
    run.typed(b"draft");
    run.key(0x2100, ALT);
    run.key(0x3177, 0);
    assert_eq!(run.ui.dialog.kind, Kind::None, "w is no File entry");
    run.key(0x316E, 0);
    assert_eq!(
        (run.ui.focus, run.ui.dialog.kind),
        (Focus::Dialog, Kind::Unsaved),
        "New asks"
    );
    assert!(holds(
        &run.row(10),
        b"Loaded file is not saved. Save it now?"
    ));
    run.key(0x3100, ALT);
    assert_eq!(
        (run.document(), run.ui.focus, run.ui.editor.modified),
        (Vec::new(), Focus::Text, false),
        "No: a new document"
    );
    run.typed(b"more");
    run.key(0x2100, ALT);
    run.key(0x2D78, 0);
    assert_eq!(run.key(ESC, 0), Action::Wait, "Cancel");
    assert_eq!(
        (run.ui.focus, run.ui.then, run.document()),
        (Focus::Text, Then::Nothing, b"more".to_vec())
    );
}

#[test]
fn shift_and_the_keypad_select_and_the_clipboard_lives_past_the_document() {
    let mut run = run_with(b"abcdef");
    let whole = run.capacity();
    for _ in 0..3 {
        run.key(SHIFT_RIGHT, SHIFT);
    }
    run.key(0x532E, SHIFT);
    assert_eq!((run.document(), run.ui.clip), (b"def".to_vec(), 3), "cut");
    assert_eq!(
        run.capacity(),
        whole - 3,
        "the clipboard's bytes taken from the document's room"
    );
    run.key(0x4F00, 0);
    run.key(0x5230, SHIFT);
    assert_eq!(run.document(), b"defabc", "pasted");
    run.key(0x4700, 0);
    run.key(SHIFT_RIGHT, SHIFT);
    run.key(u16::from_le_bytes([0, own::COPY]), CTRL);
    assert_eq!(
        (run.ui.clip, run.capacity()),
        (1, whole - 1),
        "Ctrl+Ins copies, the clipboard the smaller"
    );
    run.key(0x1519, CTRL);
    assert_eq!(
        (run.document(), run.ui.clip),
        (Vec::new(), 6),
        "Ctrl+Y cuts the line to the clipboard"
    );
    let mut empty = run_with(b"keep");
    empty.key(SHIFT_RIGHT, SHIFT);
    empty.key(0x5230, SHIFT);
    assert_eq!(
        empty.document(),
        b"keep",
        "nothing to paste leaves the selection be"
    );
}

#[test]
fn wordstar_keys_move_mark_and_quote() {
    let mut run = run_with(b"one two\r\nthree");
    run.key(0x2106, CTRL);
    assert_eq!(run.ui.editor.cursor.column, 4, "Ctrl+F, a word on");
    run.key(0x250B, CTRL);
    run.key(0x0231, 0);
    run.key(0x2D18, CTRL);
    run.key(0x1011, CTRL);
    run.key(0x2064, 0);
    assert_eq!(
        (run.ui.editor.cursor.line, run.ui.editor.cursor.column),
        (1, 5),
        "Ctrl+X down, Ctrl+Q D to the line's end"
    );
    run.key(0x1011, CTRL);
    run.key(0x0231, 0);
    assert_eq!(
        (run.ui.editor.cursor.line, run.ui.editor.cursor.column),
        (0, 4),
        "back to bookmark 1"
    );
    run.key(0x1910, CTRL);
    run.key(0x260C, CTRL);
    assert_eq!(
        run.document(),
        b"one \x0Ctwo\r\nthree",
        "Ctrl+P Ctrl+L puts a form feed in"
    );
    run.key(0x1011, CTRL);
    run.key(0x2E63, 0);
    assert_eq!(run.ui.editor.cursor.line, 1, "Ctrl+Q C to the end");
}

#[test]
fn find_selects_what_it_finds_and_f3_finds_it_again() {
    let mut run = run_with(b"red green\r\nblue green");
    run.key(0x1011, CTRL);
    run.key(0x2166, 0);
    assert_eq!(run.ui.dialog.kind, Kind::Find);
    assert_eq!(
        run.ui.dialog.fields[0].text(),
        b"red",
        "the word at the cursor offered"
    );
    run.typed(b"green");
    assert_eq!(run.key(ENTER, 0), Action::Wait);
    assert_eq!(
        (
            run.ui.focus,
            run.ui.editor.anchor.map(|spot| spot.column),
            run.ui.editor.cursor.column
        ),
        (Focus::Text, Some(4), 9)
    );
    run.key(0x3D00, 0);
    assert_eq!(
        (run.ui.editor.cursor.line, run.ui.editor.cursor.column),
        (1, 10),
        "F3: the next"
    );
    run.key(0x3D00, 0);
    assert_eq!(run.ui.editor.cursor.line, 0, "round to the first");
    run.key(0x1011, CTRL);
    run.key(0x2166, 0);
    run.typed(b"purple");
    run.key(ENTER, 0);
    assert_eq!(
        (run.ui.dialog.kind, run.ui.dialog.note.text()),
        (Kind::Message, &b"Match not found"[..])
    );
    run.key(ENTER, 0);
    assert_eq!(run.ui.focus, Focus::Text);
}

#[test]
fn change_verifies_each_match_or_changes_them_all() {
    let mut run = run_with(b"a b a b a");
    run.key(0x1E01, CTRL);
    run.key(0x1011, CTRL);
    run.key(0x1E61, 0);
    assert_eq!(run.ui.dialog.kind, Kind::Change, "Ctrl+Q A");
    run.typed(b"a");
    run.key(0x0F09, 0);
    run.typed(b"xy");
    run.key(0x1E00, ALT);
    assert_eq!(run.document(), b"xy b xy b xy");
    assert_eq!(run.ui.dialog.note.text(), b"Change complete");
    run.key(ENTER, 0);
    run.key(0x7700, CTRL);
    run.key(0x1011, CTRL);
    run.key(0x1E61, 0);
    run.typed(b"b");
    run.key(0x0F09, 0);
    run.typed(b"B");
    run.key(ENTER, 0);
    assert_eq!(
        run.ui.dialog.kind,
        Kind::Verify,
        "Find and Verify, the default"
    );
    run.key(0x1F00, ALT);
    assert_eq!(run.ui.dialog.kind, Kind::Verify, "skipped, the next shown");
    run.key(0x2E00, ALT);
    assert_eq!(run.document(), b"xy b xy B xy");
    assert_eq!(run.ui.dialog.note.text(), b"Change complete", "none left");
}

#[test]
fn display_sets_colours_bars_and_tab_stops_and_keeps_the_cursor_on_its_byte() {
    let mut run = run_with(b"\tx");
    run.key(0x4F00, 0);
    assert_eq!(run.ui.editor.cursor.column, 9);
    run.key(0x1800, ALT);
    run.key(ENTER, 0);
    assert_eq!(run.ui.dialog.kind, Kind::Display);
    assert_eq!(run.ui.dialog.picks, [7, 1], "white on blue");
    run.key(DOWN, 0);
    run.key(0x1400, ALT);
    run.typed(b"4");
    run.key(0x1F00, ALT);
    assert_eq!(run.key(ENTER, 0), Action::Settings);
    assert_eq!(
        run.ui.settings,
        Settings {
            colour: 0x18,
            bars: false,
            tab: 4
        }
    );
    assert_eq!(run.ui.editor.cursor.column, 5, "still after the x");
    assert_eq!(run.ui.palette().text, 0x18);
    run.key(0x1800, ALT);
    run.key(ENTER, 0);
    run.key(0x1400, ALT);
    run.typed(b"0");
    run.key(ENTER, 0);
    assert_eq!(run.ui.dialog.note.text(), b"Tab stops must be from 1 to 99");
}

#[test]
fn the_open_dialog_lists_what_the_program_found_sorted() {
    let mut run = Run::new(4096);
    run.key(0x2100, ALT);
    assert_eq!(
        run.key(0x186F, 0),
        Action::List,
        "Open, nothing to save first"
    );
    run.with(|ui, text| {
        let room = ui.begin_listing(text);
        assert_eq!(room, 4096 / 13);
        for (n, (kind, name)) in (0..).zip([
            (entry::FILE, &b"ZEBRA.TXT"[..]),
            (entry::DRIVE, b"C"),
            (entry::DIRECTORY, b"DOCS"),
            (entry::FILE, b"APPLE.TXT"),
            (entry::DIRECTORY, b".."),
            (entry::DRIVE, b"A"),
        ]) {
            super::super::commands::put_entry(text, ui.clip, n, kind, name);
        }
        ui.end_listing(text, 6);
        ui.path.set(b"C:\\TEXTS");
    });
    run.done(Done::Listed);
    assert_eq!((run.ui.files, run.ui.others), (2, 4));
    let screen: Vec<Vec<u8>> = (0..25).map(|n| run.row(n)).collect();
    assert!(holds(&screen[4], b"File Name: [*.TXT"), "{:?}", screen[4]);
    assert!(holds(&screen[6], b"C:\\TEXTS"));
    assert!(
        holds(&screen[10], b"APPLE.TXT") && holds(&screen[10], b".."),
        "{:?}",
        screen[10]
    );
    assert!(holds(&screen[11], b"ZEBRA.TXT") && holds(&screen[11], b"DOCS"));
    assert!(holds(&screen[12], b"[-A-]") && holds(&screen[13], b"[-C-]"));
    run.key(0x0F09, 0);
    run.key(DOWN, 0);
    assert_eq!(
        run.ui.dialog.fields[0].text(),
        b"ZEBRA.TXT",
        "the file chosen named in the field"
    );
    assert_eq!(run.key(ENTER, 0), Action::Open);
    run.key(ESC, 0);
    assert_eq!(
        (run.ui.focus, run.ui.files, run.capacity()),
        (Focus::Text, 0, 4096),
        "the listing given back"
    );
}

#[test]
fn saving_an_untitled_document_names_it_and_carries_on() {
    let mut run = Run::new(4096);
    run.typed(b"notes");
    run.key(0x2100, ALT);
    run.key(0x2D78, 0);
    assert_eq!(
        run.key(0x1500, ALT),
        Action::List,
        "Yes, to save: untitled, so Save As"
    );
    run.with(|ui, text| {
        ui.begin_listing(text);
        ui.end_listing(text, 0);
    });
    run.done(Done::Listed);
    run.typed(b"NOTES.TXT");
    assert_eq!(run.key(ENTER, 0), Action::SaveAs);
    run.ui.target.set(b"C:\\NOTES.TXT");
    run.done(Done::Exists);
    assert_eq!(run.ui.dialog.kind, Kind::Overwrite);
    assert_eq!(run.key(0x1500, ALT), Action::Save, "Yes, replace it");
    run.ui.name = run.ui.target;
    assert_eq!(
        run.done(Done::Saved),
        Action::Exit,
        "saved, the exit carried on"
    );
    assert_eq!(
        (run.ui.editor.modified, run.ui.title()),
        (false, b"NOTES.TXT".to_vec())
    );
}

#[test]
fn a_failed_save_cancels_what_waited_on_it() {
    let mut run = Run::new(4096);
    run.typed(b"x");
    run.ui.name.set(b"A:\\X.TXT");
    run.key(0x2100, ALT);
    run.key(0x2D78, 0);
    assert_eq!(run.key(ENTER, 0), Action::Save, "Yes, to its own file");
    run.done(Done::Failed(0x15));
    assert_eq!(
        (run.ui.dialog.note.text(), run.ui.then),
        (&b"Drive not ready"[..], Then::Nothing)
    );
    run.key(ENTER, 0);
    assert!(run.ui.editor.modified, "still to be saved");
}

#[test]
fn the_mouse_places_the_cursor_selects_and_reaches_the_menus() {
    let mut run = run_with(b"alpha beta\r\ngamma");
    run.click((3, 3), 100);
    assert_eq!(
        (run.ui.editor.cursor.line, run.ui.editor.cursor.column),
        (1, 2)
    );
    run.pointer(Pointer {
        presses: 1,
        pressed_at: (2, 2),
        releases: 0,
        held: true,
        at: (2, 2),
        flags: 0,
        now: 200,
    });
    run.pointer(Pointer {
        presses: 0,
        pressed_at: (2, 2),
        releases: 1,
        held: false,
        at: (2, 6),
        flags: 0,
        now: 202,
    });
    run.pointer(Pointer {
        presses: 0,
        pressed_at: (2, 2),
        releases: 0,
        held: true,
        at: (2, 6),
        flags: 0,
        now: 202,
    });
    assert_eq!(
        run.ui.editor.anchor, None,
        "a drag only while the button is held"
    );
    run.pointer(Pointer {
        presses: 1,
        pressed_at: (2, 2),
        releases: 0,
        held: true,
        at: (2, 2),
        flags: 0,
        now: 300,
    });
    run.pointer(Pointer {
        presses: 0,
        pressed_at: (2, 2),
        releases: 0,
        held: true,
        at: (2, 6),
        flags: 0,
        now: 301,
    });
    let selection = run.with(|ui, text| ui.edit(text).selection());
    assert_eq!(selection, Some(1..5), "dragged from the l to the a");
    run.click((2, 8), 400);
    run.click((2, 8), 403);
    let selection = run.with(|ui, text| ui.edit(text).selection());
    assert_eq!(selection, Some(6..10), "a double click selects the word");
    run.click((0, 3), 500);
    assert_eq!((run.ui.focus, run.ui.menu), (Focus::Menu, 0), "File opened");
    run.click((0, 3), 520);
    assert_eq!(run.ui.focus, Focus::Text, "and closed");
    run.click((0, 8), 540);
    assert_eq!(run.ui.menu, 1);
    run.click((3, 10), 560);
    assert_eq!(
        (run.ui.focus, run.ui.clip),
        (Focus::Text, 4),
        "Edit's Copy chosen"
    );
}

#[test]
fn help_opens_over_the_text_or_a_dialog_and_esc_goes_back() {
    let mut run = Run::new(4096);
    run.key(0x3B00, 0);
    assert_eq!(
        (run.ui.focus, run.ui.help.topic),
        (Focus::Help, Topic::Started)
    );
    assert!(holds(&run.row(1), b"HELP: Getting Started"));
    run.key(ESC, 0);
    assert_eq!(run.ui.focus, Focus::Text);
    run.key(0x1F00, ALT);
    run.key(0x2166, 0);
    run.key(0x2300, ALT);
    assert_eq!(
        (run.ui.focus, run.ui.help.topic),
        (Focus::Help, Topic::Find),
        "the dialog's own topic"
    );
    run.key(ESC, 0);
    assert_eq!(
        (run.ui.focus, run.ui.dialog.kind),
        (Focus::Dialog, Kind::Find),
        "the dialog waiting"
    );
    assert_eq!(run.ui.cursor(), Some((10, 25)), "in its field");
}

/// Keys that edit: characters typed and given as Alt codes, line ends,
/// the deletions, and the moves with and without Shift.
const EDITING: &[(u16, u8)] = &[
    (0x1E61, 0),
    (0x3920, 0),
    (0x0231, 0),
    (0x2D78, 0),
    (0x000D, 0),
    (0x000A, 0),
    (0x00FF, 0),
    (0x0009, 0),
    (ENTER, 0),
    (ENTER, 0),
    (0x1C0A, CTRL),
    (0x0E08, 0),
    (0x0E08, 0),
    (0x5300, 0),
    (0x0F09, 0),
    (0x0F00, SHIFT),
    (UP, 0),
    (DOWN, 0),
    (0x4B00, 0),
    (0x4D00, 0),
    (0x4B34, SHIFT),
    (SHIFT_RIGHT, SHIFT),
    (0x4838, SHIFT),
    (0x5032, SHIFT),
    (0x4700, 0),
    (0x4F00, 0),
    (0x4900, 0),
    (0x5100, 0),
    (0x4737, SHIFT),
    (0x4F31, SHIFT),
    (0x7700, CTRL),
    (0x7500, CTRL),
    (0x7300, CTRL),
    (0x7400, CTRL),
    (0x8400, CTRL),
    (0x7600, CTRL),
    (0x5200, 0),
    (0x532E, SHIFT),
    (0x5230, SHIFT),
    (0xF300, CTRL),
    (0x1519, CTRL),
    (0x310E, CTRL),
    (0x1117, CTRL),
    (0x2C1A, CTRL),
    (0x2207, CTRL),
    (0x1F13, CTRL),
    (0x2004, CTRL),
    (0x1205, CTRL),
];

/// The rest: the menus, the dialogs, the function keys, Alt and the
/// prefixes and what follows them.
const OTHERS: &[(u16, u8)] = &[
    (0x316E, 0),
    (0x1579, 0),
    (0x186F, 0),
    (0x1F73, 0),
    (0x2064, 0),
    (0x2166, 0),
    (0x2E63, 0),
    (0x1372, 0),
    (0x1265, 0),
    (0x3B00, 0),
    (0x3D00, 0),
    (0x4400, 0),
    (0x2100, ALT),
    (0x1200, ALT),
    (0x1F00, ALT),
    (0x1800, ALT),
    (0x2300, ALT),
    (ALT_DOWN, ALT),
    (ALT_UP, 0),
    (0x1011, CTRL),
    (0x250B, CTRL),
    (0x1910, CTRL),
    (0x260C, CTRL),
    (0x2F16, CTRL),
    (0x0B30, 0),
    (0x0433, 0),
    (0x0000, 0),
    (ESC, 0),
    (ESC, 0),
    (ESC, 0),
];

/// What the program would answer `action` with, made up: a listing of a
/// few entries, or an outcome of the kind the action can have.
fn answer(run: &mut Run, rng: &mut tairix_fuzzseed::Prng, action: Action) {
    let done = match action {
        Action::Wait | Action::Exit => return,
        Action::List => {
            let entries = rng.below(30);
            run.with(|ui, text| {
                let room = ui.begin_listing(text);
                let count = u16::try_from(entries).unwrap().min(room);
                for n in 0..count {
                    super::super::commands::put_entry(
                        text,
                        ui.clip,
                        n,
                        u8::try_from(n % 3).unwrap(),
                        b"NAME.TXT",
                    );
                }
                ui.end_listing(text, count);
            });
            Done::Listed
        }
        Action::Open => {
            if rng.below(2) == 0 {
                Done::Failed(0x02)
            } else {
                run.with(|ui, text| {
                    ui.release_listing(text);
                    let len = text.len();
                    text.delete(0..len);
                    let _ = text.insert(0, b"loaded\r\nfile\n");
                });
                Done::Loaded
            }
        }
        Action::SaveAs => *rng.pick(&[Done::Exists, Done::Saved, Done::Failed(0x05)]),
        Action::Save => *rng.pick(&[
            Done::Saved,
            Done::Failed(failure::DISK_FULL),
            Done::Failed(failure::KEPT),
        ]),
        Action::Print { .. } => *rng.pick(&[Done::Printed, Done::Failed(0x53)]),
        Action::Settings => *rng.pick(&[Done::Stored, Done::Failed(0x13)]),
    };
    let next = run.done(done);
    answer(run, rng, next);
}

/// The editor's places and counts held to the document's own.
fn agrees(run: &mut Run) {
    run.with(|ui, text| {
        let len = text.len();
        assert_eq!(
            ui.editor.lines,
            text.breaks_in(0..len) + 1,
            "the lines counted"
        );
        let mut spots = alloc::vec![ui.editor.cursor, ui.editor.top];
        spots.extend(ui.editor.anchor);
        for spot in spots {
            assert_eq!(
                text.line_start(spot.start),
                spot.start,
                "{spot:?} at a line's start"
            );
            assert_eq!(
                text.breaks_in(0..spot.start),
                spot.line,
                "{spot:?} on its line"
            );
        }
        let listing = (u32::from(ui.files) + u32::from(ui.others)) * 13;
        assert_eq!(
            text.capacity() + ui.clip + listing,
            text.store_len(),
            "the store's parts its whole"
        );
        assert!(
            ui.editor.top.line <= ui.editor.cursor.line || ui.focus != Focus::Text,
            "the window over the cursor"
        );
    });
    run.screen();
}

#[test]
fn random_keys_and_clicks_keep_the_editor_and_its_document_agreeing() {
    for seed in 0..24 {
        let mut rng = tairix_fuzzseed::Prng::new(seed);
        let mut run = Run::new(600 + rng.below(1500));
        for _ in 0..300 {
            let action = if rng.below(12) == 0 {
                let at = (
                    u8::try_from(rng.below(25)).unwrap(),
                    u8::try_from(rng.below(80)).unwrap(),
                );
                let presses = u16::try_from(rng.below(3)).unwrap();
                let held = rng.below(2) == 0;
                let now = u16::try_from(rng.below(40)).unwrap();
                run.pointer(Pointer {
                    presses,
                    pressed_at: at,
                    releases: u16::from(!held),
                    held,
                    at,
                    flags: 0,
                    now,
                })
            } else {
                let keys = if rng.below(5) == 0 { OTHERS } else { EDITING };
                let &(code, flags) = rng.pick(keys);
                run.key(code, flags)
            };
            answer(&mut run, &mut rng, action);
            agrees(&mut run);
        }
    }
}
