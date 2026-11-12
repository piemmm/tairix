//! The commands' own rules: names split and told patterns, the listing's
//! order, and a clipboard that does not fit.

use super::super::fixture::Run;
use super::*;

#[test]
fn a_name_is_split_at_its_directory_and_told_a_pattern() {
    assert_eq!(
        split_name(b"C:\\DOCS\\*.TXT"),
        (&b"C:\\DOCS\\"[..], &b"*.TXT"[..])
    );
    assert_eq!(split_name(b"A:NOTE"), (&b"A:"[..], &b"NOTE"[..]));
    assert_eq!(split_name(b"README"), (&b""[..], &b"README"[..]));
    assert!(is_pattern(b"*.*") && is_pattern(b"A?.TXT") && !is_pattern(b"PLAIN.TXT"));
}

#[test]
fn a_clipboard_too_large_for_the_room_is_refused_and_the_old_one_kept() {
    let mut run = Run::new(64);
    run.typed(&[b'x'; 40]);
    run.with(|ui, text| {
        assert_eq!(ui.keep(b"0123456789", text), Action::Wait);
        assert_eq!(ui.clip, 10);
        ui.keep(&[b'y'; 30], text);
        assert_eq!(
            (ui.clip, ui.dialog.note.text()),
            (10, &b"Out of memory"[..]),
            "the old clipboard kept"
        );
    });
    assert_eq!(run.capacity(), 54);
}

#[test]
fn a_save_as_name_with_a_wildcard_is_refused() {
    let mut run = Run::new(1024);
    run.key(0x2100, crate::bios::shift::ALT);
    run.key(0x1E61, 0);
    assert_eq!(run.ui.dialog.kind, Kind::SaveAs);
    run.with(|ui, text| {
        ui.begin_listing(text);
        ui.end_listing(text, 0);
    });
    run.done(Done::Listed);
    run.typed(b"*.BAK");
    run.key(0x1C0D, 0);
    assert_eq!(run.ui.dialog.note.text(), b"Bad file name");
}

#[test]
fn a_listing_too_large_for_the_room_takes_what_there_is() {
    let mut run = Run::new(100);
    run.typed(b"hello");
    let room = run.with(|ui, text| {
        let room = ui.begin_listing(text);
        for n in 0..room {
            super::super::commands::put_entry(text, ui.clip, n, entry::FILE, b"F");
        }
        ui.end_listing(text, room);
        assert_eq!(text.len(), 5, "the document kept whole");
        room
    });
    assert_eq!(room, (100 - 5) / 13);
    assert_eq!(run.capacity(), 100 - u32::from(room) * 13);
    assert_eq!(run.document(), b"hello");
}
