//! Unit tests for the listing desk's policy.
//!
//! Every rule a worker and a serve loop depend on is exercised here with no
//! thread and no lock: the request/answer handshake, the staleness rule, the
//! deduplication that stops one directory being read twice, the round-robin
//! that keeps one consumer from starving another, and a slot per consumer.

use super::*;

use alloc::vec;

/// A two-consumer program, standing in for the desktop session's icon column
/// and trusted file picker.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Consumer {
    Pinboard,
    Picker,
}

/// A one-consumer program, standing in for one file-manager window.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Sole {
    Browser,
}

fn path(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| String::from(*name)).collect()
}

fn entries(names: &[&str]) -> Vec<Entry> {
    names.iter().map(|name| Entry::file(*name)).collect()
}

/// The next job's consumer and directory, for comparing a hand-out whole.
fn handed_out<C: Copy + Ord>(desk: &mut ListingDesk<C>) -> Option<(C, Vec<String>)> {
    desk.next_job()
        .map(|job| (job.client(), job.target().to_vec()))
}

#[test]
fn a_first_ask_records_the_request_and_answers_pending() {
    let mut desk = ListingDesk::new();
    let home = path(&["Users", "root"]);
    assert_eq!(desk.take(Consumer::Picker, &home), Ok(Listing::Pending));
    assert!(desk.has_work());
    assert_eq!(
        handed_out(&mut desk),
        Some((Consumer::Picker, home)),
        "the recorded request is the job"
    );
}

#[test]
fn asking_again_for_the_same_directory_starts_no_second_read() {
    let mut desk = ListingDesk::new();
    let home = path(&["Users"]);
    let _ = desk.take(Consumer::Picker, &home);
    let _ = desk.take(Consumer::Picker, &home);
    assert!(desk.next_job().is_some());
    assert!(
        desk.next_job().is_none(),
        "a read already in progress was handed out twice"
    );
    assert!(!desk.has_work());
}

#[test]
fn a_delivered_answer_is_served_once_and_then_a_fresh_read_is_asked_for() {
    let mut desk = ListingDesk::new();
    let home = path(&["Users"]);
    let _ = desk.take(Consumer::Pinboard, &home);
    let job = desk.next_job().expect("a job");
    assert!(desk.deliver(job, Ok(entries(&["a", "b"]))));

    assert_eq!(
        desk.take(Consumer::Pinboard, &home),
        Ok(Listing::Ready(entries(&["a", "b"])))
    );
    // Consumed: the consumer has adopted those entries, so asking again means
    // it wants to know what is there *now*.
    assert_eq!(desk.take(Consumer::Pinboard, &home), Ok(Listing::Pending));
    assert!(desk.has_work());
}

#[test]
fn a_refusal_is_delivered_and_served_exactly_like_a_listing() {
    let mut desk = ListingDesk::new();
    let home = path(&["Locked"]);
    let _ = desk.take(Consumer::Picker, &home);
    let job = desk.next_job().expect("a job");
    assert!(desk.deliver(job, Err(Errno::PermissionDenied)));
    assert_eq!(
        desk.take(Consumer::Picker, &home),
        Err(Errno::PermissionDenied)
    );
}

#[test]
fn an_answer_for_somewhere_the_consumer_left_is_never_served() {
    let mut desk = ListingDesk::new();
    let first = path(&["Users"]);
    let second = path(&["Apps"]);
    let _ = desk.take(Consumer::Picker, &first);
    let job = desk.next_job().expect("a job");
    // The user clicks elsewhere while the first read is in flight.
    let _ = desk.take(Consumer::Picker, &second);
    assert!(
        !desk.deliver(job, Ok(entries(&["stale"]))),
        "an abandoned read must report that nobody wants it"
    );
    assert_eq!(
        desk.take(Consumer::Picker, &second),
        Ok(Listing::Pending),
        "the stale answer leaked into the new request"
    );
    assert_eq!(
        handed_out(&mut desk),
        Some((Consumer::Picker, second)),
        "the new target was not queued"
    );
}

#[test]
fn one_consumers_answer_is_not_the_others() {
    let mut desk = ListingDesk::new();
    let home = path(&["Users"]);
    let _ = desk.take(Consumer::Pinboard, &home);
    let job = desk.next_job().expect("a job");
    assert_eq!(job.client(), Consumer::Pinboard);
    assert!(desk.deliver(job, Ok(entries(&["mine"]))));
    assert_eq!(
        desk.take(Consumer::Picker, &home),
        Ok(Listing::Pending),
        "the picker was served the icon column's answer"
    );
}

#[test]
fn two_busy_consumers_are_served_in_turn() {
    let mut desk = ListingDesk::new();
    let mut served = vec![];
    for _ in 0..4 {
        let _ = desk.take(Consumer::Pinboard, &path(&["Desktop"]));
        let _ = desk.take(Consumer::Picker, &path(&["Users"]));
        let job = desk.next_job().expect("a job");
        let client = job.client();
        served.push(client);
        assert!(desk.deliver(job, Ok(entries(&["x"]))));
        // Adopt it, so the consumer asks again on the next round.
        let _ = desk.take(client, &[]);
    }
    assert_eq!(
        served,
        vec![
            Consumer::Pinboard,
            Consumer::Picker,
            Consumer::Pinboard,
            Consumer::Picker,
        ],
        "one consumer starved the other"
    );
}

#[test]
fn stopping_hands_out_no_more_work() {
    let mut desk = ListingDesk::new();
    let _ = desk.take(Consumer::Picker, &path(&["Users"]));
    assert!(desk.has_work());
    desk.stop();
    assert!(desk.stopping());
    assert!(!desk.has_work(), "a stopped desk still offered work");
    assert!(desk.next_job().is_none());
}

/// The whole handshake, driven end to end on one thread: the shape the worker
/// and the session run, with the lock and the wake left out.
#[test]
fn a_request_completes_when_a_reader_serves_it() {
    let mut desk = ListingDesk::new();
    let home = path(&["Users", "root", "Desktop"]);

    // The session asks and gets nothing yet.
    assert_eq!(desk.take(Consumer::Pinboard, &home), Ok(Listing::Pending));
    // The worker wakes, takes the job, reads, and delivers.
    let job = desk.next_job().expect("a job");
    assert_eq!(job.target(), home.as_slice());
    assert!(desk.deliver(job, Ok(entries(&["notes.txt"]))));
    // The session wakes on the pipe byte and asks again.
    assert_eq!(
        desk.take(Consumer::Pinboard, &home),
        Ok(Listing::Ready(entries(&["notes.txt"])))
    );
    assert!(!desk.has_work(), "nothing is left outstanding");
}

/// A program with one consumer needs no fairness, and the round-robin degrades
/// to serving it every time rather than to serving it every other time.
#[test]
fn a_sole_consumer_is_served_on_every_turn() {
    let mut desk = ListingDesk::new();
    for _ in 0..3 {
        let home = path(&["Users"]);
        assert_eq!(desk.take(Sole::Browser, &home), Ok(Listing::Pending));
        let job = desk.next_job().expect("a job");
        assert_eq!(job.client(), Sole::Browser);
        assert!(desk.deliver(job, Ok(entries(&["a"]))));
        assert_eq!(
            desk.take(Sole::Browser, &home),
            Ok(Listing::Ready(entries(&["a"])))
        );
    }
}

/// The defect this desk's whole point is to avoid: a worker that hands itself
/// the same read for ever.
///
/// A hand-out clones the target rather than taking it, so the request outlived
/// its own answer — the slot became workable again the instant it was answered,
/// and the serve loop went straight round to read the same directory, waking
/// the embedder on every completion. Measured on the desktop as ~150 reads a
/// second of one folder, with nothing else on that thread.
#[test]
fn an_answered_read_is_never_handed_out_again() {
    let mut desk = ListingDesk::new();
    let home = path(&["Users", "someone", "Desktop"]);
    assert_eq!(desk.take(Sole::Browser, &home), Ok(Listing::Pending));

    let job = desk.next_job().expect("a job");
    assert!(desk.deliver(job, Ok(entries(&["notes.txt"]))));

    assert!(
        !desk.has_work(),
        "the answered read must not make the slot workable again"
    );
    assert!(
        desk.next_job().is_none(),
        "a worker looking for work after answering must find none and park"
    );

    // And the answer is still there to be collected.
    assert_eq!(
        desk.take(Sole::Browser, &home),
        Ok(Listing::Ready(entries(&["notes.txt"])))
    );
}

/// A read the consumer has navigated away from leaves its *newer* request
/// standing, so the abandoned answer costs one wasted read and not a stall.
#[test]
fn a_stale_answer_does_not_clear_the_newer_request() {
    let mut desk = ListingDesk::new();
    let first = path(&["Users", "someone", "Desktop"]);
    let second = path(&["Users", "someone", "Documents"]);
    assert_eq!(desk.take(Sole::Browser, &first), Ok(Listing::Pending));
    let job = desk.next_job().expect("a job");

    // The consumer moves on while the read is in flight.
    assert_eq!(desk.take(Sole::Browser, &second), Ok(Listing::Pending));
    assert!(
        !desk.deliver(job, Ok(entries(&["notes.txt"]))),
        "an abandoned read owes no wake"
    );

    assert!(desk.has_work(), "the newer request is still owed a read");
    let job = desk.next_job().expect("the newer job");
    assert_eq!(job.target(), second.as_slice());
}

/// A re-list asked for because the directory may have changed must not be
/// answered by a read that began before it: that read can describe the folder
/// from before the change, so its answer is dropped and the folder read anew.
#[test]
fn a_refresh_is_never_answered_by_a_read_already_under_way() {
    let mut desk = ListingDesk::new();
    let home = path(&["Users", "someone", "Desktop"]);
    assert_eq!(desk.take(Sole::Browser, &home), Ok(Listing::Pending));
    let early = desk.next_job().expect("a job");

    desk.refresh(Sole::Browser, &home);
    assert!(
        !desk.deliver(early, Ok(entries(&["before"]))),
        "a read that began before the refresh owes no wake"
    );
    assert_eq!(
        desk.take(Sole::Browser, &home),
        Ok(Listing::Pending),
        "the pre-refresh answer was served"
    );

    let fresh = desk.next_job().expect("the refresh is read anew");
    assert!(desk.deliver(fresh, Ok(entries(&["before", "after"]))));
    assert_eq!(
        desk.take(Sole::Browser, &home),
        Ok(Listing::Ready(entries(&["before", "after"])))
    );
}

#[test]
fn a_refresh_drops_an_answer_it_has_not_collected() {
    let mut desk = ListingDesk::new();
    let home = path(&["Users"]);
    let _ = desk.take(Sole::Browser, &home);
    let job = desk.next_job().expect("a job");
    assert!(desk.deliver(job, Ok(entries(&["held"]))));

    desk.refresh(Sole::Browser, &home);
    assert_eq!(desk.take(Sole::Browser, &home), Ok(Listing::Pending));
    assert!(desk.has_work(), "the refresh is owed a read");
}

/// A read still queued has not begun, so it already answers a refresh: the
/// folder is read once, not twice.
#[test]
fn a_refresh_of_a_queued_read_reads_it_once() {
    let mut desk = ListingDesk::new();
    let home = path(&["Users"]);
    let _ = desk.take(Sole::Browser, &home);
    desk.refresh(Sole::Browser, &home);

    let job = desk.next_job().expect("a job");
    assert!(desk.next_job().is_none(), "one directory read twice");
    assert!(desk.deliver(job, Ok(entries(&["a"]))));
    assert_eq!(
        desk.take(Sole::Browser, &home),
        Ok(Listing::Ready(entries(&["a"])))
    );
}

/// Collecting is not asking: asking again while a read is under way neither
/// starts a second read nor makes the one under way stale.
#[test]
fn asking_while_a_read_is_under_way_leaves_it_answering() {
    let mut desk = ListingDesk::new();
    let home = path(&["Users"]);
    let _ = desk.take(Sole::Browser, &home);
    let job = desk.next_job().expect("a job");
    for _ in 0..3 {
        assert_eq!(desk.take(Sole::Browser, &home), Ok(Listing::Pending));
    }
    assert!(desk.next_job().is_none());
    assert!(desk.deliver(job, Ok(entries(&["a"]))));
    assert_eq!(
        desk.take(Sole::Browser, &home),
        Ok(Listing::Ready(entries(&["a"])))
    );
}

/// Two windows of the file manager shared its one slot: each threw the other's
/// answer away as stale and asked again for its own, so neither ever listed
/// and the worker read the disk without end.
#[test]
fn two_consumers_listing_different_places_both_settle() {
    let mut desk = ListingDesk::new();
    let (a, b) = (path(&["Users", "ann"]), path(&["Apps"]));
    assert_eq!(desk.take(1_u64, &a), Ok(Listing::Pending));
    assert_eq!(desk.take(2_u64, &b), Ok(Listing::Pending));
    for _ in 0..2 {
        let job = desk.next_job().expect("each place is read");
        let listed = if job.target() == a.as_slice() {
            entries(&["notes"])
        } else {
            entries(&["Tool.app"])
        };
        assert!(desk.deliver(job, Ok(listed)));
    }
    assert!(!desk.has_work(), "each read once, nothing re-asked");
    assert_eq!(desk.take(1, &a), Ok(Listing::Ready(entries(&["notes"]))));
    assert_eq!(desk.take(2, &b), Ok(Listing::Ready(entries(&["Tool.app"]))));
}

/// The one consumer a worker served last is served again when it alone asks.
#[test]
fn a_lone_consumer_is_served_again_after_the_round_passes_it() {
    let mut desk = ListingDesk::new();
    let home = path(&["Users", "root"]);
    let _ = desk.take(Sole::Browser, &home);
    let job = desk.next_job().expect("a job");
    assert!(desk.deliver(job, Ok(entries(&["a"]))));
    let _ = desk.take(Sole::Browser, &home);
    let _ = desk.take(Sole::Browser, &path(&["Apps"]));
    assert_eq!(
        handed_out(&mut desk),
        Some((Sole::Browser, path(&["Apps"]))),
        "the round wrapped back to it"
    );
}

/// A forgotten consumer's slot goes with it: its read under way is answered to
/// nobody, and the desk holds nothing for it.
#[test]
fn a_forgotten_consumer_is_answered_to_nobody() {
    let mut desk = ListingDesk::new();
    let home = path(&["Users", "root"]);
    let _ = desk.take(7_u64, &home);
    let job = desk.next_job().expect("a job");
    desk.forget(7);
    assert!(
        !desk.deliver(job, Ok(entries(&["a"]))),
        "no one waits on it"
    );
    assert!(!desk.has_work());
    assert_eq!(desk.take(7, &home), Ok(Listing::Pending), "a fresh slot");
}
