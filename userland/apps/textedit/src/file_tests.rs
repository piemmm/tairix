//! Host tests for a window's file: saves, the saves asked for behind them,
//! closing, and file choosers.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::ControlFlow;

use tairix_abi::time::Duration64;
use tairix_syntax::Format;

use super::{FileState, PickFor, SaveJob, SaveStep};
use crate::document::{Document, Snapshot};
use crate::editor::Editor;
use crate::view::{Access, View};

/// A file, as the engine sees one: something only shared.
type Handle = &'static str;

fn view(text: &[u8], access: Access) -> View {
    let document = Document::from_chunks(alloc::vec![text.to_vec()]).expect("loads");
    View::new(
        Editor::new(document, Format::PlainText),
        String::from("notes.txt"),
        access,
        Duration64::from_millis(500),
    )
}

fn opened(handle: Handle) -> FileState<Handle> {
    let mut file = FileState::new();
    file.opened(Some(Arc::new(handle)));
    file
}

fn bytes(snapshot: &Snapshot) -> Vec<u8> {
    let mut out = Vec::new();
    snapshot.walk(0, |slice| {
        out.extend_from_slice(slice);
        ControlFlow::Continue(())
    });
    out
}

fn write(step: SaveStep<Handle>) -> SaveJob<Handle> {
    match step {
        SaveStep::Write(job) => job,
        _ => panic!("a save to write now"),
    }
}

fn type_in(view: &mut View, text: &[u8]) {
    let _ = view.editor_mut().replace_selection(text);
}

/// Land `job` as written, answering what the landing left to do.
fn land(
    file: &mut FileState<Handle>,
    view: &mut View,
    job: SaveJob<Handle>,
) -> super::Landed<Handle> {
    file.saved(
        view,
        job.target,
        job.generation,
        job.rename,
        Ok::<(), &str>(()),
    )
}

#[test]
fn a_writable_document_saves_where_it_came_from() {
    let mut view = view(b"text", Access::Writable);
    let mut file = opened("original");
    type_in(&mut view, b"more ");
    let job = write(file.save(&mut view, None, false));
    assert_eq!(*job.target, "original");
    assert_eq!(job.rename, None);
    let landed = land(&mut file, &mut view, job);
    assert!(landed.next.is_none() && !landed.close);
    assert!(!view.editor().is_modified());
}

#[test]
fn an_untitled_document_asks_where_and_its_save_as_is_then_its_file() {
    let mut view = view(b"", Access::Untitled);
    let mut file = FileState::new();
    assert!(matches!(
        file.save(&mut view, None, true),
        SaveStep::AskWhere { then_close: true }
    ));
    let job = write(file.save(
        &mut view,
        Some((Arc::new("chosen"), String::from("a.txt"))),
        false,
    ));
    assert_eq!(job.rename.as_deref(), Some("a.txt"));
    land(&mut file, &mut view, job);
    assert_eq!(view.name(), "a.txt");
    assert_eq!(view.access(), Access::Writable);
    type_in(&mut view, b"x");
    assert_eq!(*write(file.save(&mut view, None, false)).target, "chosen");
}

#[test]
fn a_save_asked_behind_another_writes_the_document_as_it_was_asked() {
    let mut view = view(b"", Access::Writable);
    let mut file = opened("original");
    type_in(&mut view, b"one");
    let first = write(file.save(&mut view, None, false));
    type_in(&mut view, b" two");
    assert!(matches!(
        file.save(&mut view, None, false),
        SaveStep::Queued
    ));
    type_in(&mut view, b" three");
    let landed = land(&mut file, &mut view, first);
    let next = write(landed.next.expect("the save behind it"));
    assert_eq!(
        bytes(&next.snapshot),
        b"one two",
        "not what was typed after"
    );
    land(&mut file, &mut view, next);
    assert!(
        view.editor().is_modified(),
        "' three' was never asked to be saved"
    );
}

#[test]
fn nothing_is_opened_over_a_window_with_a_save_or_a_chooser_under_way() {
    let mut view = view(b"", Access::Untitled);
    let mut file: FileState<Handle> = FileState::new();
    assert!(file.pristine(&view));
    let job = write(file.save(
        &mut view,
        Some((Arc::new("chosen"), String::from("a.txt"))),
        false,
    ));
    assert!(!file.pristine(&view), "a Save As in flight");
    land(&mut file, &mut view, job);
    let mut chooser: FileState<Handle> = FileState::new();
    let blank = self::view(b"", Access::Untitled);
    assert!(chooser.start_pick(PickFor::Open));
    assert!(!chooser.pristine(&blank), "a chooser open");
    assert!(!chooser.start_pick(PickFor::Open), "and only one at a time");
    assert_eq!(chooser.end_pick(), Some(PickFor::Open));
    assert!(chooser.pristine(&blank));
}

#[test]
fn closing_writes_a_save_as_asked_behind_the_one_in_flight() {
    let mut view = view(b"", Access::Writable);
    let mut file = opened("original");
    type_in(&mut view, b"draft");
    let _in_flight = write(file.save(&mut view, None, false));
    assert!(matches!(
        file.save(
            &mut view,
            Some((Arc::new("copy"), String::from("copy.txt"))),
            false
        ),
        SaveStep::Queued
    ));
    let flushed = file.close(&view);
    assert_eq!(flushed.len(), 1, "the Save As is not dropped");
    assert_eq!(*flushed[0].target, "copy");
    assert_eq!(bytes(&flushed[0].snapshot), b"draft");
    assert!(file.close(&view).is_empty(), "and it is handed over once");
}

#[test]
fn a_plain_save_behind_a_save_as_goes_where_the_save_as_went() {
    let mut view = view(b"", Access::Writable);
    let mut file = opened("original");
    let _save_as = write(file.save(
        &mut view,
        Some((Arc::new("copy"), String::from("copy.txt"))),
        false,
    ));
    type_in(&mut view, b"later");
    assert!(matches!(
        file.save(&mut view, None, false),
        SaveStep::Queued
    ));
    let flushed = file.close(&view);
    assert_eq!(flushed.len(), 1);
    assert_eq!(*flushed[0].target, "copy");
}

#[test]
fn every_save_as_asked_behind_a_save_is_written_in_turn() {
    let mut view = view(b"", Access::Writable);
    let mut file = opened("original");
    let first = write(file.save(&mut view, None, false));
    for (target, name) in [("x", "x.txt"), ("y", "y.txt")] {
        type_in(&mut view, name.as_bytes());
        let save_as = Some((Arc::new(target), String::from(name)));
        assert!(matches!(
            file.save(&mut view, save_as, false),
            SaveStep::Queued
        ));
    }
    let mut written = Vec::new();
    let mut next = land(&mut file, &mut view, first).next;
    while let Some(step) = next {
        let job = write(step);
        written.push((*job.target, bytes(&job.snapshot)));
        next = land(&mut file, &mut view, job).next;
    }
    assert_eq!(
        written,
        [("x", b"x.txt".to_vec()), ("y", b"x.txty.txt".to_vec())],
        "each chosen file gets the document as it was when it was chosen"
    );
    assert_eq!(view.name(), "y.txt");
}

#[test]
fn closing_writes_every_save_as_asked_behind_the_one_in_flight() {
    let mut view = view(b"", Access::Writable);
    let mut file = opened("original");
    let _in_flight = write(file.save(&mut view, None, false));
    for target in ["x", "y"] {
        let save_as = Some((Arc::new(target), String::from(target)));
        assert!(matches!(
            file.save(&mut view, save_as, false),
            SaveStep::Queued
        ));
    }
    type_in(&mut view, b"after");
    assert!(matches!(
        file.save(&mut view, None, false),
        SaveStep::Queued
    ));
    let targets: Vec<Handle> = file.close(&view).iter().map(|job| *job.target).collect();
    assert_eq!(
        targets,
        ["x", "y", "y"],
        "a plain save goes where the last Save As went"
    );
}

#[test]
fn plain_saves_asked_behind_one_become_one_of_the_latest_document() {
    let mut view = view(b"", Access::Writable);
    let mut file = opened("original");
    let first = write(file.save(&mut view, None, false));
    for text in [&b"a"[..], b"b", b"c"] {
        type_in(&mut view, text);
        assert!(matches!(
            file.save(&mut view, None, false),
            SaveStep::Queued
        ));
    }
    let next = write(
        land(&mut file, &mut view, first)
            .next
            .expect("one save behind it"),
    );
    assert_eq!(bytes(&next.snapshot), b"abc");
    assert!(
        land(&mut file, &mut view, next).next.is_none(),
        "and only one"
    );
}

#[test]
fn a_plain_save_behind_a_failed_save_as_has_nowhere_to_go_and_goes() {
    let mut view = view(b"", Access::Untitled);
    let mut file: FileState<Handle> = FileState::new();
    let save_as = Some((Arc::new("chosen"), String::from("a.txt")));
    let first = write(file.save(&mut view, save_as, false));
    type_in(&mut view, b"x");
    assert!(matches!(
        file.save(&mut view, None, false),
        SaveStep::Queued
    ));
    let landed = file.saved(
        &mut view,
        first.target,
        first.generation,
        first.rename,
        Err("refused"),
    );
    assert!(
        landed.next.is_none(),
        "an untitled document still has no file"
    );
    assert!(!file.saving());
}

#[test]
fn a_plain_save_behind_a_failed_save_as_never_overwrites_the_original() {
    let mut view = view(b"", Access::Writable);
    let mut file = opened("original");
    let save_as = Some((Arc::new("chosen"), String::from("b.txt")));
    let first = write(file.save(&mut view, save_as, false));
    type_in(&mut view, b"x");
    assert!(matches!(file.save(&mut view, None, true), SaveStep::Queued));
    let landed = file.saved(
        &mut view,
        first.target,
        first.generation,
        first.rename,
        Err("refused"),
    );
    assert!(landed.next.is_none(), "the original is left as it was");
    assert!(
        landed.close_abandoned,
        "and the close waiting on it with it"
    );
    assert!(!file.saving());
}

#[test]
fn a_refused_save_says_why_gives_up_its_close_and_still_writes_what_followed() {
    let mut view = view(b"", Access::Writable);
    let mut file = opened("original");
    type_in(&mut view, b"a");
    let first = write(file.save(&mut view, None, true));
    assert!(file.closing());
    assert!(matches!(
        file.save(
            &mut view,
            Some((Arc::new("copy"), String::from("c.txt"))),
            false
        ),
        SaveStep::Queued
    ));
    let landed = file.saved(
        &mut view,
        first.target,
        first.generation,
        first.rename,
        Err("no space left on device"),
    );
    assert!(landed.close_abandoned && !landed.close);
    assert_eq!(
        view.message(),
        Some("Could not save: no space left on device")
    );
    assert_eq!(
        *write(landed.next.expect("the Save As behind it")).target,
        "copy"
    );
    assert!(!file.closing(), "the close went with the failed save");
}

#[test]
fn a_close_waiting_on_a_save_saves_again_what_was_typed_meanwhile() {
    let mut view = view(b"", Access::Writable);
    let mut file = opened("original");
    type_in(&mut view, b"a");
    let first = write(file.save(&mut view, None, true));
    type_in(&mut view, b"b");
    let landed = land(&mut file, &mut view, first);
    assert!(!landed.close, "not yet: 'b' is unsaved");
    let again = write(landed.next.expect("a save of what was typed"));
    assert_eq!(bytes(&again.snapshot), b"ab");
    assert!(land(&mut file, &mut view, again).close);
}
