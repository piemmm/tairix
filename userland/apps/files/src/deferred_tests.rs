use alloc::string::String;

use tairix_abi::fs::{FileId, FileStat};
use tairix_abi::time::Time64;
use tairix_abi::{Errno, NodeTimes};
use tairix_browse::{EntryKind, Properties};

use super::{FilesClients, PropertyJob, PropertyReads};

/// Every browser window lists under a consumer of its own: sharing one, two
/// windows discarded each other's answers and neither ever listed.
#[test]
fn every_browser_window_lists_under_a_consumer_of_its_own() {
    let mut clients = FilesClients::default();
    let (first, second) = (clients.mint(), clients.mint());
    assert_ne!(first, second);
    assert_ne!(clients.mint(), first, "never the same one twice");
}

/// A summary for the node at `name`, so an answer can be told from another's.
fn summary(name: &str) -> Properties {
    Properties::from_stat(
        name,
        EntryKind::File,
        &FileStat {
            kind: tairix_abi::fs::FileKind::Regular,
            nlink: 1,
            size: 0,
            allocated: 0,
            mode: 0o644,
            uid: 0,
            gid: 0,
            id: FileId::NONE,
            times: NodeTimes {
                created: Time64::UNIX_EPOCH,
                modified: Time64::UNIX_EPOCH,
                accessed: Time64::UNIX_EPOCH,
                changed: Time64::UNIX_EPOCH,
            },
            content_gen: 0,
        },
    )
}

fn job(path: &str) -> PropertyJob {
    PropertyJob {
        path: String::from(path),
        kind: EntryKind::File,
    }
}

/// Several Properties windows are open at once, so one window's read must
/// neither displace nor be answered into another's.
#[test]
fn two_properties_windows_are_answered_independently() {
    let mut desk = PropertyReads::new();
    assert!(desk.submit(1, job("/a")));
    assert!(desk.submit(2, job("/b")));
    assert!(desk.has_work());

    let (first, _) = desk.next_job().expect("a read");
    let (second, _) = desk.next_job().expect("a second read");
    assert_eq!((first, second), (1, 2), "asked order, served");
    assert!(!desk.has_work());

    // Answered out of order, each lands in its own window's slot.
    assert!(desk.deliver(second, Ok(summary("b"))));
    assert!(desk.deliver(first, Ok(summary("a"))));
    assert!(desk.take_landed());
    assert_eq!(
        desk.take(1)
            .expect("window 1's answer")
            .map(|p| p.name().into()),
        Ok(String::from("a"))
    );
    assert_eq!(
        desk.take(2)
            .expect("window 2's answer")
            .map(|p| p.name().into()),
        Ok(String::from("b"))
    );
    assert!(desk.take(1).is_none(), "an answer is collected once");
}

/// A re-read follows a write the window just made, so the older answer
/// describes a node that has since changed: showing it would undo the edit.
#[test]
fn a_re_read_supersedes_the_same_windows_outstanding_answer() {
    let mut desk = PropertyReads::new();
    assert!(desk.submit(7, job("/a")));
    let (first, _) = desk.next_job().expect("a read");
    assert!(desk.deliver(first, Ok(summary("stale"))));

    assert!(desk.submit(7, job("/a")));
    assert!(
        desk.take(7).is_none(),
        "the stale answer went with the request it superseded"
    );
    let (again, _) = desk.next_job().expect("the re-read");
    assert!(desk.deliver(again, Ok(summary("fresh"))));
    assert_eq!(
        desk.take(7)
            .expect("the fresh answer")
            .map(|p| p.name().into()),
        Ok(String::from("fresh"))
    );
    assert!(desk.next_job().is_none());
}

/// A refusal is an answer: the window states it rather than showing nothing.
#[test]
fn a_refused_read_is_delivered_as_the_reason_it_failed() {
    let mut desk = PropertyReads::new();
    assert!(desk.submit(3, job("/gone")));
    let (read, _) = desk.next_job().expect("a read");
    assert!(desk.deliver(read, Err(Errno::NotFound)));
    assert_eq!(desk.take(3), Some(Err(Errno::NotFound)));
}

/// A closed window's answer belongs to nobody — and a window id could be
/// reused, so an in-flight answer must not land in whatever took its place.
#[test]
fn a_closed_windows_read_is_forgotten_and_its_answer_dropped() {
    let mut desk = PropertyReads::new();
    assert!(desk.submit(4, job("/a")));
    let (read, _) = desk.next_job().expect("a read");
    desk.forget(4);
    assert!(
        !desk.deliver(read, Ok(summary("a"))),
        "nothing owns the slot, so no repaint is owed"
    );
    assert!(desk.take(4).is_none());
    assert!(!desk.take_landed());

    // A request not yet started is simply dropped.
    let mut desk = PropertyReads::new();
    assert!(desk.submit(5, job("/a")));
    desk.forget(5);
    assert!(!desk.has_work());
    assert!(desk.next_job().is_none());
}

/// A stopping desk records nothing and offers nothing, so a parked worker
/// leaves rather than finding fresh work on the way out.
#[test]
fn a_stopping_property_desk_hands_out_no_work() {
    let mut desk = PropertyReads::new();
    assert!(desk.submit(1, job("/a")));
    desk.stop();
    assert!(!desk.has_work());
    assert!(desk.next_job().is_none());
    assert!(!desk.submit(2, job("/b")));
    assert!(desk.take(1).is_none());
    assert!(!desk.take_landed());
}

/// With no worker to answer, a read made on the asking thread is collected
/// exactly as a worker's would be, so the window shows its answer rather than
/// reading for ever.
#[test]
fn an_answer_read_on_the_asking_thread_is_collected_like_a_workers() {
    let mut desk = PropertyReads::new();
    desk.stop();
    assert!(!desk.submit(6, job("/a")), "no worker will take it");
    desk.answer_here(6, Ok(summary("here")));
    assert!(desk.take_landed());
    assert_eq!(
        desk.take(6)
            .expect("the answer read here")
            .map(|p| p.name().into()),
        Ok(String::from("here"))
    );
    assert!(desk.take(6).is_none());
}

#[test]
fn a_client_round_trips_through_its_number() {
    let client = FilesClients::default().mint();
    assert_eq!(super::FilesClient::from_number(client.number()), client);
}
