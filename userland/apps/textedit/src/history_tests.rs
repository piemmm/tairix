//! Unit tests for undo and redo, against the document they are applied to.

use alloc::vec::Vec;

use super::{History, Kind};
use crate::document::Document;
use crate::selection::Selection;

/// Replace `range` with `text` and record it.
fn edit(
    doc: &mut Document,
    history: &mut History,
    range: core::ops::Range<usize>,
    text: &[u8],
    kind: Kind,
) {
    let before = Selection::caret(range.start);
    let change = doc.replace(range.clone(), text).expect("room");
    history.record(
        change,
        before,
        Selection::caret(range.start + text.len()),
        kind,
    );
}

fn undo(doc: &mut Document, history: &mut History) -> Option<Selection> {
    if let Some(changes) = history.next_undo() {
        doc.room_to_revert(changes).expect("room");
    }
    history
        .undo(|change| doc.revert(change))
        .expect("room")
        .map(|restored| restored.selection)
}

fn redo(doc: &mut Document, history: &mut History) -> Option<Selection> {
    if let Some(changes) = history.next_redo() {
        doc.room_to_reapply(changes).expect("room");
    }
    history
        .redo(|change| doc.reapply(change))
        .expect("room")
        .map(|restored| restored.selection)
}

fn text(doc: &Document) -> Vec<u8> {
    doc.to_vec().expect("room")
}

#[test]
fn a_typing_run_undoes_as_one_step_and_restores_the_caret() {
    let mut doc = Document::new();
    let mut history = History::new();
    for (at, byte) in b"hello".iter().enumerate() {
        edit(&mut doc, &mut history, at..at, &[*byte], Kind::Typing);
    }
    assert_eq!(history.depth(), 1);
    assert_eq!(undo(&mut doc, &mut history), Some(Selection::caret(0)));
    assert!(doc.is_empty());
    assert_eq!(redo(&mut doc, &mut history), Some(Selection::caret(5)));
    assert_eq!(text(&doc), b"hello");
}

#[test]
fn a_moved_caret_or_another_kind_of_edit_ends_the_run() {
    let mut doc = Document::new();
    let mut history = History::new();
    edit(&mut doc, &mut history, 0..0, b"a", Kind::Typing);
    edit(&mut doc, &mut history, 1..1, b"b", Kind::Typing);
    history.close_run();
    edit(&mut doc, &mut history, 2..2, b"c", Kind::Typing);
    edit(&mut doc, &mut history, 2..3, b"", Kind::Other);
    edit(&mut doc, &mut history, 2..2, b"d", Kind::Typing);
    assert_eq!(history.depth(), 4);
    assert_eq!(text(&doc), b"abd");
}

#[test]
fn overwriting_in_place_continues_the_run() {
    let mut doc = Document::new();
    let mut history = History::new();
    edit(&mut doc, &mut history, 0..0, b"\x00\x00\x00", Kind::Other);
    // Two nibbles of one byte, then the next byte: hex typing.
    edit(&mut doc, &mut history, 0..1, b"\xa0", Kind::Typing);
    edit(&mut doc, &mut history, 0..1, b"\xab", Kind::Typing);
    edit(&mut doc, &mut history, 1..2, b"\xc0", Kind::Typing);
    assert_eq!(history.depth(), 2);
    undo(&mut doc, &mut history);
    assert_eq!(text(&doc), b"\x00\x00\x00");
}

#[test]
fn the_saved_state_is_known_through_undo_and_redo() {
    let mut doc = Document::new();
    let mut history = History::new();
    assert!(!history.is_modified());
    edit(&mut doc, &mut history, 0..0, b"a", Kind::Typing);
    assert!(history.is_modified());
    history.mark_saved();
    assert!(!history.is_modified());
    // Typing on after a save starts a new step, so undo returns to the
    // saved text exactly.
    edit(&mut doc, &mut history, 1..1, b"b", Kind::Typing);
    assert!(history.is_modified());
    undo(&mut doc, &mut history);
    assert!(!history.is_modified());
    assert_eq!(text(&doc), b"a");
    undo(&mut doc, &mut history);
    assert!(history.is_modified());
    redo(&mut doc, &mut history);
    assert!(!history.is_modified());
    // A new edit after undoing past the save makes the saved state
    // unreachable.
    undo(&mut doc, &mut history);
    edit(&mut doc, &mut history, 0..0, b"z", Kind::Other);
    assert!(history.is_modified());
    assert!(!history.can_redo());
    undo(&mut doc, &mut history);
    assert!(history.is_modified(), "the saved state is gone for good");
}

#[test]
fn a_group_undoes_all_its_changes_newest_first() {
    let mut doc = Document::new();
    let mut history = History::new();
    edit(&mut doc, &mut history, 0..0, b"abcd", Kind::Other);
    // Overwriting byte after byte is one run of changes of their own.
    for (at, byte) in [(1, b'X'), (2, b'Y')] {
        let change = doc.replace(at..at + 1, &[byte]).expect("room");
        let caret = Selection::caret(at + 1);
        history.record(change, Selection::caret(at), caret, Kind::Typing);
    }
    assert_eq!(text(&doc), b"aXYd");
    assert_eq!(history.next_undo().map(<[_]>::len), Some(2), "one group");
    assert_eq!(undo(&mut doc, &mut history), Some(Selection::caret(1)));
    assert_eq!(text(&doc), b"abcd");
    redo(&mut doc, &mut history);
    assert_eq!(text(&doc), b"aXYd");
}

#[test]
fn trimming_keeps_the_newest_steps_and_forgets_redo() {
    let mut doc = Document::new();
    let mut history = History::new();
    for at in 0..5 {
        edit(&mut doc, &mut history, at..at, b"x", Kind::Other);
    }
    undo(&mut doc, &mut history);
    history.trim(2);
    assert_eq!(history.depth(), 2);
    assert!(!history.can_redo());
    assert!(
        history.is_modified(),
        "the saved empty state was trimmed away"
    );
}

#[test]
fn a_forgotten_save_leaves_the_document_modified() {
    let mut doc = Document::new();
    let mut history = History::new();
    edit(&mut doc, &mut history, 0..0, b"a", Kind::Other);
    history.forget_saved();
    assert!(history.is_modified());
    undo(&mut doc, &mut history);
    assert!(history.is_modified());
}
