//! Text typed into a booted machine: read back as typed, a character begun
//! only once the guest has read the last, and what is stopped counted.

use super::{Overfull, Typist};
use crate::bios::{self, run_until};
use crate::chips::keyboard::{Scan, SHIFT};
use crate::dos::harness::{bare_on, boot_image, idle, prompted, settle, SECOND};
use crate::link::TYPE_WINDOW;
use crate::machine::{Machine, Template};
use crate::time::{Nanos, NANOS_PER_MILLISECOND as MS};
use alloc::vec::Vec;

/// Runs `m` for `span`, feeding `typist` between slices as the worker does.
fn type_for(m: &mut Machine, typist: &mut Typist, span: Nanos) {
    let end = m.now() + span;
    while m.now() < end {
        typist.feed(m, true);
        let now = m.now();
        run_until(m, (now + 4 * MS).min(end));
    }
}

/// The rows of the screen, each without its trailing blanks.
fn rows(m: &Machine) -> Vec<Vec<u8>> {
    let shown = m.board().shown_text(0).unwrap();
    shown
        .codes
        .chunks(usize::from(shown.columns))
        .map(|row| {
            let kept = row
                .iter()
                .rposition(|&code| code != b' ')
                .map_or(0, |at| at + 1);
            row[..kept].to_vec()
        })
        .collect()
}

/// A machine booted from a diskette that never reads its keys.
fn deaf() -> Machine {
    let mut m = bare_on(&Template::IbmPc.config(), boot_image(idle));
    run_until(&mut m, 3 * SECOND);
    m
}

#[test]
fn text_typed_at_the_shell_is_read_as_it_was_typed() {
    let mut m = prompted(&[]);
    let mut typist = Typist::new().unwrap();
    typist.take(b"ECHO Typed \x82\xDB!\r").unwrap();
    type_for(&mut m, &mut typist, 3 * SECOND);
    settle(&mut m);
    assert_eq!(typist.take_taken(), Some(15));
    let shown = rows(&m);
    assert!(
        shown
            .iter()
            .any(|row| row.ends_with(b">ECHO Typed \x82\xDB!")),
        "{shown:02X?}"
    );
    assert!(
        shown.iter().any(|row| row == b"Typed \x82\xDB!"),
        "{shown:02X?}"
    );
}

#[test]
fn caps_lock_on_leaves_each_letters_case_as_given() {
    let mut m = prompted(&[]);
    let caps = Scan::plain(0x3A);
    m.board_mut().key(caps, true);
    m.board_mut().key(caps, false);
    settle(&mut m);
    assert!(bios::caps_lock(&m));
    let mut typist = Typist::new().unwrap();
    typist.take(b"ECHO Ab\r").unwrap();
    type_for(&mut m, &mut typist, 2 * SECOND);
    settle(&mut m);
    let shown = rows(&m);
    assert!(shown.iter().any(|row| row == b"Ab"), "{shown:02X?}");
}

#[test]
fn a_guest_that_reads_no_keys_is_given_one_and_no_more() {
    let mut m = deaf();
    let mut typist = Typist::new().unwrap();
    typist.take(b"abcdef").unwrap();
    type_for(&mut m, &mut typist, SECOND);
    assert_eq!(typist.take_taken(), Some(1));
    assert_eq!(bios::keys_waiting(&m), Some(1));
    assert!(m.board().keyboard_idle() && !m.board().keys_held());
    typist.stop();
    assert_eq!(typist.take_taken(), Some(5), "what waited is left untyped");
    assert_eq!(typist.take_taken(), None);
}

#[test]
fn nothing_is_begun_while_a_key_is_held_or_beginning_is_not_allowed() {
    let mut m = deaf();
    let mut typist = Typist::new().unwrap();
    typist.take(b"a").unwrap();
    typist.feed(&mut m, false);
    assert_eq!(typist.take_taken(), None, "not allowed");
    m.board_mut().key(SHIFT, true);
    type_for(&mut m, &mut typist, SECOND / 10);
    assert_eq!(typist.take_taken(), None, "Shift held");
    m.board_mut().key(SHIFT, false);
    type_for(&mut m, &mut typist, SECOND / 10);
    assert_eq!(typist.take_taken(), Some(1));
}

#[test]
fn more_than_the_window_is_refused_whole() {
    let mut typist = Typist::new().unwrap();
    typist.take(&[b'a'; TYPE_WINDOW - 1]).unwrap();
    assert_eq!(typist.take(b"bc"), Err(Overfull));
    typist.take(b"b").unwrap();
    typist.clear();
    assert_eq!(
        typist.take_taken(),
        Some(u32::try_from(TYPE_WINDOW).unwrap())
    );
}
