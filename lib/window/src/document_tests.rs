//! A window's document file: saves, the saves asked for behind them,
//! closing, and file choosers.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::{Access, DocumentFile, Landed, PickFor, SaveJob, SaveStep, SavedDocument};

/// A file, as the engine sees one: something only shared.
type Handle = &'static str;

/// A document of bytes, each typed change a new generation.
struct Doc {
    text: Vec<u8>,
    generation: u64,
    saved_at: Option<u64>,
    name: String,
    access: Access,
    message: Option<String>,
}

impl Doc {
    fn new(text: &[u8], access: Access) -> Self {
        Self {
            text: text.to_vec(),
            generation: 0,
            saved_at: Some(0),
            name: String::from("notes.txt"),
            access,
            message: None,
        }
    }

    fn type_in(&mut self, text: &[u8]) {
        self.text.extend_from_slice(text);
        self.generation += 1;
    }
}

impl SavedDocument for Doc {
    type Snapshot = Vec<u8>;

    fn access(&self) -> Access {
        self.access
    }

    fn is_modified(&self) -> bool {
        self.saved_at != Some(self.generation)
    }

    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn snapshot(&mut self) -> Option<(u64, Arc<Vec<u8>>)> {
        Some((self.generation, Arc::new(self.text.clone())))
    }

    fn saved(&mut self, generation: u64) {
        // A save of an older generation leaves the document still changed.
        self.saved_at = (generation == self.generation).then_some(generation);
    }

    fn rename(&mut self, name: String) {
        self.name = name;
    }

    fn set_access(&mut self, access: Access) {
        self.access = access;
    }

    fn say(&mut self, message: String) {
        self.message = Some(message);
    }
}

type File = DocumentFile<Handle, Vec<u8>>;
type Job = SaveJob<Handle, Vec<u8>>;

fn opened(handle: Handle) -> File {
    let mut file = File::new();
    file.opened(Some(Arc::new(handle)));
    file
}

fn write(step: SaveStep<Handle, Vec<u8>>) -> Job {
    match step {
        SaveStep::Write(job) => job,
        _ => panic!("a save to write now"),
    }
}

/// Where a Save As writes: the file chosen and what it is called.
fn destination(target: Handle, name: &str) -> (Arc<Handle>, String) {
    (Arc::new(target), String::from(name))
}

/// Land `job` as written, answering what the landing left to do.
fn land(file: &mut File, doc: &mut Doc, job: Job) -> Landed<Handle, Vec<u8>> {
    file.saved(
        doc,
        job.target,
        job.generation,
        job.rename,
        Ok::<Option<&str>, &str>(None),
    )
}

#[test]
fn a_writable_document_saves_where_it_came_from() {
    let mut doc = Doc::new(b"text", Access::Writable);
    let mut file = opened("original");
    doc.type_in(b"more ");
    let job = write(file.save(&mut doc, None, false));
    assert_eq!(*job.target, "original");
    assert_eq!(job.rename, None);
    let landed = land(&mut file, &mut doc, job);
    assert!(landed.next.is_none() && !landed.close);
    assert!(!doc.is_modified());
}

#[test]
fn an_untitled_document_asks_where_and_its_save_as_is_then_its_file() {
    let mut doc = Doc::new(b"", Access::Untitled);
    let mut file = File::new();
    assert!(matches!(
        file.save(&mut doc, None, true),
        SaveStep::AskWhere { then_close: true }
    ));
    let job = write(file.save(&mut doc, Some(destination("chosen", "a.txt")), false));
    assert_eq!(job.rename.as_deref(), Some("a.txt"));
    land(&mut file, &mut doc, job);
    assert_eq!(doc.name, "a.txt");
    assert_eq!(doc.access, Access::Writable);
    assert_eq!(doc.message.as_deref(), Some("Saved"));
    doc.type_in(b"x");
    assert_eq!(*write(file.save(&mut doc, None, false)).target, "chosen");
}

#[test]
fn a_save_asked_behind_another_writes_the_document_as_it_was_asked() {
    let mut doc = Doc::new(b"", Access::Writable);
    let mut file = opened("original");
    doc.type_in(b"one");
    let first = write(file.save(&mut doc, None, false));
    doc.type_in(b" two");
    assert!(matches!(file.save(&mut doc, None, false), SaveStep::Queued));
    doc.type_in(b" three");
    let landed = land(&mut file, &mut doc, first);
    let next = write(landed.next.expect("the save behind it"));
    assert_eq!(
        next.snapshot.as_slice(),
        b"one two",
        "not what was typed after"
    );
    land(&mut file, &mut doc, next);
    assert!(doc.is_modified(), "' three' was never asked to be saved");
}

#[test]
fn nothing_is_opened_over_a_window_with_a_save_or_a_chooser_under_way() {
    let mut doc = Doc::new(b"", Access::Untitled);
    let mut file = File::new();
    assert!(file.pristine(&doc));
    let job = write(file.save(&mut doc, Some(destination("chosen", "a.txt")), false));
    assert!(!file.pristine(&doc), "a Save As in flight");
    land(&mut file, &mut doc, job);
    let mut chooser = File::new();
    let blank = Doc::new(b"", Access::Untitled);
    assert!(chooser.start_pick(PickFor::Open));
    assert!(!chooser.pristine(&blank), "a chooser open");
    assert!(!chooser.start_pick(PickFor::Open), "and only one at a time");
    assert_eq!(chooser.end_pick(), Some(PickFor::Open));
    assert!(chooser.pristine(&blank));
    assert!(
        !chooser.pristine(&Doc::new(b"x", Access::Untitled)),
        "not empty"
    );
}

#[test]
fn closing_writes_a_save_as_asked_behind_the_one_in_flight() {
    let mut doc = Doc::new(b"", Access::Writable);
    let mut file = opened("original");
    doc.type_in(b"draft");
    let _in_flight = write(file.save(&mut doc, None, false));
    assert!(matches!(
        file.save(&mut doc, Some(destination("copy", "copy.txt")), false),
        SaveStep::Queued
    ));
    let flushed = file.close(&doc);
    assert_eq!(flushed.len(), 1, "the Save As is not dropped");
    assert_eq!(*flushed[0].target, "copy");
    assert_eq!(flushed[0].snapshot.as_slice(), b"draft");
    assert!(file.close(&doc).is_empty(), "and it is handed over once");
}

#[test]
fn a_plain_save_behind_a_save_as_goes_where_the_save_as_went() {
    let mut doc = Doc::new(b"", Access::Writable);
    let mut file = opened("original");
    let _save_as = write(file.save(&mut doc, Some(destination("copy", "copy.txt")), false));
    doc.type_in(b"later");
    assert!(matches!(file.save(&mut doc, None, false), SaveStep::Queued));
    let flushed = file.close(&doc);
    assert_eq!(flushed.len(), 1);
    assert_eq!(*flushed[0].target, "copy");
    assert_eq!(
        flushed[0].rename.as_deref(),
        Some("copy.txt"),
        "and is written as that file, not the one the window still shows"
    );
}

#[test]
fn a_plain_save_is_told_the_file_it_will_go_to() {
    let mut doc = Doc::new(b"", Access::Writable);
    let mut file = opened("original");
    assert_eq!(file.plain_destination(), None, "its own file");
    assert!(file.writes_back(&doc));
    let _save_as = write(file.save(&mut doc, Some(destination("copy", "copy.txt")), false));
    assert_eq!(file.plain_destination(), Some("copy.txt"));
    assert!(matches!(
        file.save(&mut doc, Some(destination("later", "later.txt")), false),
        SaveStep::Queued
    ));
    assert_eq!(
        file.plain_destination(),
        Some("later.txt"),
        "the last ahead"
    );
    let read_only = Doc::new(b"", Access::ReadOnly);
    assert!(!opened("original").writes_back(&read_only));
}

#[test]
fn every_save_as_asked_behind_a_save_is_written_in_turn() {
    let mut doc = Doc::new(b"", Access::Writable);
    let mut file = opened("original");
    let first = write(file.save(&mut doc, None, false));
    for (target, name) in [("x", "x.txt"), ("y", "y.txt")] {
        doc.type_in(name.as_bytes());
        assert!(matches!(
            file.save(&mut doc, Some(destination(target, name)), false),
            SaveStep::Queued
        ));
    }
    let mut written = Vec::new();
    let mut next = land(&mut file, &mut doc, first).next;
    while let Some(step) = next {
        let job = write(step);
        written.push((*job.target, job.snapshot.as_ref().clone()));
        next = land(&mut file, &mut doc, job).next;
    }
    assert_eq!(
        written,
        [("x", b"x.txt".to_vec()), ("y", b"x.txty.txt".to_vec())],
        "each chosen file gets the document as it was when it was chosen"
    );
    assert_eq!(doc.name, "y.txt");
}

#[test]
fn closing_writes_every_save_as_asked_behind_the_one_in_flight() {
    let mut doc = Doc::new(b"", Access::Writable);
    let mut file = opened("original");
    let _in_flight = write(file.save(&mut doc, None, false));
    for target in ["x", "y"] {
        assert!(matches!(
            file.save(&mut doc, Some(destination(target, target)), false),
            SaveStep::Queued
        ));
    }
    doc.type_in(b"after");
    assert!(matches!(file.save(&mut doc, None, false), SaveStep::Queued));
    let targets: Vec<Handle> = file.close(&doc).iter().map(|job| *job.target).collect();
    assert_eq!(
        targets,
        ["x", "y", "y"],
        "a plain save goes where the last Save As went"
    );
}

#[test]
fn plain_saves_asked_behind_one_become_one_of_the_latest_document() {
    let mut doc = Doc::new(b"", Access::Writable);
    let mut file = opened("original");
    let first = write(file.save(&mut doc, None, false));
    for text in [&b"a"[..], b"b", b"c"] {
        doc.type_in(text);
        assert!(matches!(file.save(&mut doc, None, false), SaveStep::Queued));
    }
    let next = write(
        land(&mut file, &mut doc, first)
            .next
            .expect("one save behind it"),
    );
    assert_eq!(next.snapshot.as_slice(), b"abc");
    assert!(
        land(&mut file, &mut doc, next).next.is_none(),
        "and only one"
    );
}

#[test]
fn a_plain_save_behind_a_failed_save_as_has_nowhere_to_go_and_goes() {
    let mut doc = Doc::new(b"", Access::Untitled);
    let mut file = File::new();
    let first = write(file.save(&mut doc, Some(destination("chosen", "a.txt")), false));
    doc.type_in(b"x");
    assert!(matches!(file.save(&mut doc, None, false), SaveStep::Queued));
    let landed = file.saved(
        &mut doc,
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
    let mut doc = Doc::new(b"", Access::Writable);
    let mut file = opened("original");
    let first = write(file.save(&mut doc, Some(destination("chosen", "b.txt")), false));
    doc.type_in(b"x");
    assert!(matches!(file.save(&mut doc, None, true), SaveStep::Queued));
    let landed = file.saved(
        &mut doc,
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
fn a_save_says_what_its_format_could_not_keep() {
    let mut doc = Doc::new(b"text", Access::Writable);
    let mut file = opened("original");
    doc.type_in(b"x");
    let job = write(file.save(&mut doc, None, false));
    let _ = file.saved(
        &mut doc,
        job.target,
        job.generation,
        job.rename,
        Ok::<_, &str>(Some("Its format holds no transparency")),
    );
    assert_eq!(
        doc.message.as_deref(),
        Some("Saved. Its format holds no transparency")
    );
}

#[test]
fn a_refused_save_says_why_gives_up_its_close_and_still_writes_what_followed() {
    let mut doc = Doc::new(b"", Access::Writable);
    let mut file = opened("original");
    doc.type_in(b"a");
    let first = write(file.save(&mut doc, None, true));
    assert!(file.closing());
    assert!(matches!(
        file.save(&mut doc, Some(destination("copy", "c.txt")), false),
        SaveStep::Queued
    ));
    let landed = file.saved(
        &mut doc,
        first.target,
        first.generation,
        first.rename,
        Err("no space left on device"),
    );
    assert!(landed.close_abandoned && !landed.close);
    assert_eq!(
        doc.message.as_deref(),
        Some("Could not save: no space left on device")
    );
    assert_eq!(
        *write(landed.next.expect("the Save As behind it")).target,
        "copy"
    );
    assert!(!file.closing(), "the close went with the failed save");
}

#[test]
fn a_close_waiting_on_a_save_saves_again_what_changed_meanwhile() {
    let mut doc = Doc::new(b"", Access::Writable);
    let mut file = opened("original");
    doc.type_in(b"a");
    let first = write(file.save(&mut doc, None, true));
    doc.type_in(b"b");
    let landed = land(&mut file, &mut doc, first);
    assert!(!landed.close, "not yet: 'b' is unsaved");
    let again = write(landed.next.expect("a save of what was typed"));
    assert_eq!(again.snapshot.as_slice(), b"ab");
    assert!(land(&mut file, &mut doc, again).close);
}

#[test]
fn a_document_that_cannot_be_frozen_is_not_saved() {
    struct Starved(Doc);
    impl SavedDocument for Starved {
        type Snapshot = Vec<u8>;
        fn access(&self) -> Access {
            self.0.access()
        }
        fn is_modified(&self) -> bool {
            self.0.is_modified()
        }
        fn is_empty(&self) -> bool {
            self.0.is_empty()
        }
        fn snapshot(&mut self) -> Option<(u64, Arc<Vec<u8>>)> {
            None
        }
        fn saved(&mut self, generation: u64) {
            self.0.saved(generation);
        }
        fn rename(&mut self, name: String) {
            self.0.rename(name);
        }
        fn set_access(&mut self, access: Access) {
            self.0.set_access(access);
        }
        fn say(&mut self, message: String) {
            self.0.say(message);
        }
    }
    let mut doc = Starved(Doc::new(b"x", Access::Writable));
    let mut file = opened("original");
    assert!(matches!(
        file.save(&mut doc, None, false),
        SaveStep::NoMemory
    ));
    assert!(!file.saving(), "nothing is in flight");
}

/// A document's unreadable file is stated the one way every editor states it.
#[test]
fn a_read_failure_says_why() {
    use super::ReadFailure;
    assert_eq!(
        alloc::format!(
            "{}",
            ReadFailure::Unreadable(tairix_abi::Errno::PermissionDenied)
        ),
        alloc::format!(
            "it could not be read ({})",
            tairix_abi::Errno::PermissionDenied
        )
    );
    assert_eq!(
        alloc::format!("{}", ReadFailure::NotAFile),
        "it is not a file"
    );
}
