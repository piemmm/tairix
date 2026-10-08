//! Unit tests for the probe desk's policy.

use super::*;

use alloc::vec;

use tairix_icon::{FolderSample, IconKind, SampleCard};

use crate::entry::Entry;

fn path(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| String::from(*name)).collect()
}

/// What a probe finds in a folder holding text.
fn text() -> Probe {
    Probe::Holds(FolderSample::new([SampleCard::Kind(IconKind::Text)]))
}

/// A probe's answer for an empty folder.
const EMPTY: Result<Probe, Errno> = Ok(Probe::Empty);

/// Answer the batch in flight with `answer` for every folder in it, and sweep
/// as the embedder's loop does once it has drawn what landed.
fn answer_all(probes: &mut Probes, answer: &Result<Probe, Errno>) -> bool {
    let batch = probes.next_batch().expect("a batch");
    let fresh = probes.deliver(batch.into_iter().map(|f| (f, answer.clone())).collect());
    probes.sweep();
    fresh
}

/// A refused probe is an answer like any other: served, the folder is recorded
/// as one that may not be read and is never asked again, where dropping it left
/// it asked about on every repaint.
#[test]
fn a_refused_probe_is_answered_rather_than_dropped() {
    let mut probes = Probes::new();
    let folder = path(&["Locked"]);
    assert_eq!(probes.ask(&folder), (Ok(Probe::Pending), true));
    assert!(answer_all(&mut probes, &Err(Errno::PermissionDenied)));
    assert_eq!(probes.ask(&folder), (Err(Errno::PermissionDenied), false));
}

/// The rule that lets a paint resolve occupancy: an ask performs no I/O, it
/// records one and answers "not yet".
#[test]
fn a_first_ask_records_a_probe_and_answers_pending() {
    let mut probes = Probes::new();
    let folder = path(&["Users", "notes"]);
    assert_eq!(probes.ask(&folder), (Ok(Probe::Pending), true));
    assert!(probes.has_work());
    assert_eq!(probes.next_batch(), Some(vec![folder]));
}

/// A paint asks on every frame, so a re-ask queues no second probe of the
/// same folder — nor while one is in flight.
#[test]
fn asking_again_records_no_second_probe() {
    let mut probes = Probes::new();
    let folder = path(&["Users"]);
    assert_eq!(probes.ask(&folder), (Ok(Probe::Pending), true));
    for _ in 0..4 {
        assert_eq!(probes.ask(&folder), (Ok(Probe::Pending), false));
    }
    assert_eq!(probes.next_batch(), Some(vec![folder.clone()]));
    for _ in 0..5 {
        assert_eq!(probes.ask(&folder), (Ok(Probe::Pending), false));
    }
    assert!(!probes.has_work());
    assert_eq!(probes.next_batch(), None);
}

/// A screenful of folders is one batch and one repaint, not one of each per
/// folder.
#[test]
fn every_outstanding_probe_is_taken_as_one_batch() {
    let mut probes = Probes::new();
    for name in ["a", "b", "c"] {
        assert_eq!(probes.ask(&path(&[name])), (Ok(Probe::Pending), true));
    }
    let batch = probes.next_batch().expect("a batch");
    assert_eq!(batch.len(), 3);
    assert!(probes.deliver(batch.into_iter().map(|f| (f, Ok(text()))).collect()));
    for name in ["a", "b", "c"] {
        assert_eq!(probes.ask(&path(&[name])), (Ok(text()), false));
    }
}

/// A second worker finds nothing while a batch is in flight: two batches in
/// flight would each clear the other's record of what is being read.
#[test]
fn one_batch_is_in_flight_at_a_time() {
    let mut probes = Probes::new();
    let _ = probes.ask(&path(&["a"]));
    let first = probes.next_batch().expect("a batch");
    assert_eq!(probes.ask(&path(&["b"])), (Ok(Probe::Pending), true));
    assert!(
        !probes.has_work(),
        "the second ask waits for the first batch"
    );
    assert_eq!(probes.next_batch(), None);
    assert_eq!(
        probes.ask(&path(&["a"])),
        (Ok(Probe::Pending), false),
        "the batch in flight still answers its own folder"
    );
    assert!(probes.deliver(first.into_iter().map(|f| (f, Ok(text()))).collect()));
    assert_eq!(
        probes.next_batch(),
        None,
        "a landed batch waits for the sweep"
    );
    assert!(probes.sweep(), "b was asked since, so it stays wanted");
    assert_eq!(probes.next_batch(), Some(vec![path(&["b"])]));
}

/// An answer is served once: the entry latches it, so a later ask means the
/// listing was replaced and the question is fresh.
#[test]
fn an_answer_is_served_once_and_then_asked_again() {
    let mut probes = Probes::new();
    let folder = path(&["Empty"]);
    let _ = probes.ask(&folder);
    assert!(answer_all(&mut probes, &EMPTY));
    assert_eq!(probes.ask(&folder), (EMPTY, false));
    assert_eq!(probes.ask(&folder), (Ok(Probe::Pending), true));
    assert!(probes.has_work());
}

/// A batch that answered nothing owes no repaint, so a wake costs no frame.
#[test]
fn an_empty_delivery_owes_no_repaint() {
    let mut probes = Probes::new();
    let _ = probes.ask(&path(&["a"]));
    let _ = probes.next_batch();
    assert!(!probes.deliver(Vec::new()));
    assert!(!probes.take_landed());
}

/// Regression: a delivered batch has to tell the loop it landed.
///
/// The worker probed and woke the loop, but the desk recorded only the
/// answers, so the wake found nothing to adopt and every folder kept its plain
/// picture until an unrelated gesture repainted.
#[test]
fn a_delivered_batch_owes_the_loop_one_adoption() {
    let mut probes = Probes::new();
    let folder = path(&["Users"]);
    let _ = probes.ask(&folder);
    assert!(answer_all(&mut probes, &Ok(text())));
    assert!(probes.take_landed());
    assert_eq!(
        probes.ask(&folder),
        (Ok(text()), false),
        "the resolve it owes draws the cue"
    );
    assert!(!probes.take_landed(), "and it owes exactly one");
}

/// A desk asked to stop owes no repaint: there is nothing left to draw it.
#[test]
fn stopping_drops_an_unconsumed_adoption() {
    let mut probes = Probes::new();
    let _ = probes.ask(&path(&["a"]));
    assert!(answer_all(&mut probes, &Ok(text())));
    probes.stop();
    assert!(!probes.take_landed());
}

/// A folder re-asked while its probe is in flight is answered by that probe,
/// not left needing a second one.
#[test]
fn a_probe_in_flight_answers_the_re_asks_it_absorbed() {
    let mut probes = Probes::new();
    let folder = path(&["Users"]);
    let _ = probes.ask(&folder);
    let batch = probes.next_batch().expect("a batch");
    let _ = probes.ask(&folder);
    assert!(probes.deliver(batch.into_iter().map(|f| (f, Ok(text()))).collect()));
    assert_eq!(probes.ask(&folder), (Ok(text()), false));
}

/// The held answers are bounded by the screen, not by every folder the user
/// ever scrolled past: one a whole resolve pass was offered and did not take
/// is dropped before the next.
#[test]
fn an_answer_a_whole_pass_did_not_take_is_dropped() {
    let mut probes = Probes::new();
    let _ = probes.ask(&path(&["scrolled-away"]));
    assert!(answer_all(&mut probes, &Ok(text())));
    assert!(probes.take_landed());
    // The pass that followed never asked: the folder had left the view.
    assert!(!probes.take_landed());
    assert_eq!(
        probes.ask(&path(&["scrolled-away"])),
        (Ok(Probe::Pending), true),
        "the answer nothing drew was dropped, not hoarded"
    );
}

/// Regression: a batch landing before the loop has resolved the last one keeps
/// the last one's answers.
///
/// Each delivery replaced what was held, so a fast worker landing a second
/// window's batch threw the first window's answers away before that window
/// drew them, and every folder in it was probed a second time.
#[test]
fn a_batch_landing_before_the_loop_resolved_keeps_the_last_ones_answers() {
    let mut probes = Probes::new();
    let (first, second) = (path(&["one", "a"]), path(&["two", "b"]));
    let _ = probes.ask(&first);
    assert!(answer_all(&mut probes, &Ok(text())));
    let _ = probes.ask(&second);
    assert!(answer_all(&mut probes, &EMPTY));
    assert!(probes.take_landed());
    assert_eq!(probes.ask(&first), (Ok(text()), false));
    assert_eq!(probes.ask(&second), (EMPTY, false));
}

/// A stopped desk records nothing and offers nothing, and answers as a source
/// that does not probe, so a folder asked after it is drawn plain and never
/// asked again rather than re-asked on every paint.
#[test]
fn a_stopped_desk_answers_that_it_does_not_probe() {
    let mut probes = Probes::new();
    let _ = probes.ask(&path(&["a"]));
    probes.stop();
    assert!(probes.stopping());
    assert!(!probes.has_work());
    assert_eq!(probes.next_batch(), None);
    assert_eq!(
        probes.ask(&path(&["b"])),
        (Err(Errno::NotImplemented), false)
    );
    assert!(!probes.has_work());
}

#[test]
fn a_folder_that_changed_mid_probe_is_asked_afresh() {
    let dir = path(&["Users"]);
    let folder = path(&["Users", "src"]);
    let changed = [EntryChange::Upsert(Entry::directory("src"))];
    let mut probes = Probes::new();
    let _ = probes.ask(&folder);
    let batch = probes.next_batch().expect("a batch");
    assert!(probes.invalidate(&dir, &changed));
    assert!(
        probes.ask(&folder).1,
        "asked again while the stale probe is still under way"
    );
    assert!(
        !probes.deliver(vec![(batch[0].clone(), EMPTY)]),
        "the stale answer is dropped"
    );
    assert_eq!(
        probes.next_batch(),
        Some(vec![folder.clone()]),
        "and the fresh probe is the next batch"
    );
    assert!(probes.deliver(vec![(folder.clone(), Ok(text()))]));
    probes.sweep();
    assert!(probes.invalidate(&dir, &[EntryChange::Upsert(Entry::directory("docs"))]));
    assert!(probes.invalidate(&folder, &changed));
    assert_eq!(
        probes.ask(&folder).0,
        Ok(text()),
        "another folder's change keeps it"
    );
    let _ = probes.ask(&folder);
    assert!(answer_all(&mut probes, &Ok(text())));
    assert!(probes.invalidate(&dir, &changed));
    assert_eq!(
        probes.ask(&folder).0,
        Ok(Probe::Pending),
        "a held answer goes too"
    );
}

/// Only a change to a folder owes the cues anything: a file has no cue, and a
/// removed folder is never asked about again.
#[test]
fn a_change_to_no_folder_invalidates_nothing() {
    let dir = path(&["Users"]);
    let folder = path(&["Users", "src"]);
    let mut probes = Probes::new();
    let _ = probes.ask(&folder);
    assert!(answer_all(&mut probes, &Ok(text())));
    let changes = [
        EntryChange::Upsert(Entry::file("src")),
        EntryChange::Remove(String::from("src")),
    ];
    assert!(!probes.invalidate(&dir, &changes));
    assert_eq!(probes.ask(&folder).0, Ok(text()));
}

/// A directory read afresh is one whose every cue may have moved: nothing
/// held or in flight for a folder in it survives, and nothing elsewhere is
/// touched.
#[test]
fn a_relisted_directory_forgets_its_folders_answers() {
    let dir = path(&["Users"]);
    let (held, other) = (path(&["Users", "src"]), path(&["Apps", "src"]));
    let mut probes = Probes::new();
    let _ = probes.ask(&held);
    let _ = probes.ask(&other);
    assert!(answer_all(&mut probes, &Ok(text())));
    let in_flight = path(&["Users", "docs"]);
    let _ = probes.ask(&in_flight);
    let batch = probes.next_batch().expect("a batch");
    probes.invalidate_listing(&dir);
    assert_eq!(probes.ask(&held), (Ok(Probe::Pending), true));
    assert_eq!(probes.ask(&other).0, Ok(text()));
    assert!(
        probes.ask(&in_flight).1,
        "the read under way describes the folder as it was"
    );
    assert!(!probes.deliver(batch.into_iter().map(|f| (f, EMPTY)).collect()));
}

/// A sweep drops every wanted folder no pass asked about since the last one,
/// so a folder scrolled out of view while a batch was read is never read.
#[test]
fn a_sweep_drops_the_folders_no_pass_asked_about() {
    let mut probes = Probes::new();
    let _ = probes.ask(&path(&["first"]));
    let batch = probes.next_batch().expect("a batch");
    let _ = probes.ask(&path(&["shown"]));
    let _ = probes.ask(&path(&["scrolled"]));
    assert!(probes.deliver(batch.into_iter().map(|f| (f, Ok(text()))).collect()));
    assert!(probes.sweep(), "both were asked since the last sweep");
    let _ = probes.ask(&path(&["shown"]));
    assert!(probes.sweep());
    assert_eq!(probes.next_batch(), Some(vec![path(&["shown"])]));
}

/// A delivery that drew nothing holds nothing: no loop is woken to sweep.
#[test]
fn a_delivery_that_landed_nothing_holds_no_batch() {
    let mut probes = Probes::new();
    let _ = probes.ask(&path(&["a"]));
    let _ = probes.next_batch();
    let _ = probes.ask(&path(&["b"]));
    assert!(!probes.deliver(Vec::new()));
    assert_eq!(probes.next_batch(), Some(vec![path(&["b"])]));
}

/// Past the bound a new folder is left unrecorded, to be asked again.
#[test]
fn the_folders_waiting_are_bounded() {
    let mut probes = Probes::new();
    for n in 0..MAX_WANTED_PROBES {
        assert!(probes.ask(&path(&[&alloc::format!("{n}")])).1);
    }
    assert_eq!(probes.ask(&path(&["over"])), (Ok(Probe::Pending), false));
    assert_eq!(
        probes.next_batch().map(|batch| batch.len()),
        Some(MAX_WANTED_PROBES)
    );
}
