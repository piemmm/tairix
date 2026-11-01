use alloc::string::String;

use tairix_abi::fs::{FileId, FileStat};
use tairix_abi::time::Time64;
use tairix_abi::{Errno, NodeTimes};
use tairix_browse::{EntryKind, Properties};

use super::{
    first_in_order, EjectDesk, FilesClients, PropertyJob, PropertyReads, ReadKind, WindowReads,
    READ_ORDER,
};

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

/// A write the user committed before an eject is carried out before it, and
/// the eject before a document opened after it.
#[test]
fn a_committed_write_goes_before_an_eject_and_an_eject_before_an_open() {
    let at = |kind| {
        READ_ORDER
            .iter()
            .position(|ordered| *ordered == kind)
            .expect("ordered")
    };
    assert!(at(ReadKind::Write) < at(ReadKind::Locate));
    assert!(at(ReadKind::Locate) < at(ReadKind::Settle));
    assert!(at(ReadKind::Settle) < at(ReadKind::Open));
    for (index, kind) in READ_ORDER.iter().enumerate() {
        assert_eq!(at(*kind), index, "{kind:?} is ordered once");
    }
}

/// An eject keeps its room from its admission until its outcome lands, while
/// it is being located or carried out too, so one let go of its windows is
/// never refused afterwards.
#[test]
fn an_eject_keeps_its_room_until_its_outcome_lands() {
    let mut desk = EjectDesk::<u32, (), &str>::new(2);
    assert_eq!(desk.admit(1), Ok(()));
    assert_eq!(desk.admit(2), Ok(()));
    assert_eq!(desk.admit(3), Err(3), "two are held");
    let locating = desk.next_locate().expect("the first");
    assert_eq!(desk.admit(4), Err(4), "one being located still counts");
    desk.located(locating, ());
    let (located, ()) = desk.take_located().expect("located");
    desk.settle(located);
    let settling = desk.next_settle_where(|_| true).expect("to carry out");
    assert_eq!(settling, 1);
    assert_eq!(desk.admit(5), Err(5), "one being carried out still counts");
    desk.landed("ejected");
    assert_eq!(desk.take_landed(), Some("ejected"));
    assert_eq!(
        desk.admit(6),
        Ok(()),
        "its outcome landed, so its room is free"
    );
    assert_eq!(desk.take_landed(), None);
}

/// An eject is carried out by the worker of its own volume's lane: one
/// passed over keeps its place, behind none of the others.
#[test]
fn an_eject_is_carried_out_by_its_own_lanes_worker() {
    let mut desk = EjectDesk::<u32, (), &str>::new(4);
    for act in [1, 2, 3] {
        assert_eq!(desk.admit(act), Ok(()));
        let located = desk.next_locate().expect("located");
        desk.settle(located);
    }
    assert_eq!(desk.next_settle_where(|act| *act == 2), Some(2));
    assert_eq!(desk.next_settle_where(|act| *act == 2), None);
    assert_eq!(
        desk.next_settle_where(|_| true),
        Some(1),
        "the order settled holds"
    );
    assert_eq!(desk.next_settle_where(|_| true), Some(3));
}

/// A worker serving its own share of the windows' reads takes only what it
/// admits, in the order asked, and leaves the rest waiting.
#[test]
fn a_worker_takes_only_the_window_reads_it_admits() {
    let mut reads = WindowReads::<&str, ()>::new();
    assert!(reads.submit(1, "stick"));
    assert!(reads.submit(2, "home"));
    assert!(reads.submit(3, "stick"));
    let stick = |job: &&str| *job == "stick";
    assert_eq!(reads.next_job_where(stick), Some((1, "stick")));
    assert_eq!(reads.next_job_where(stick), Some((3, "stick")));
    assert_eq!(reads.next_job_where(stick), None);
    assert!(reads.has_work());
    assert_eq!(reads.next_job(), Some((2, "home")));
    assert!(
        reads.deliver(1, ()),
        "a read taken by share is answered as any is"
    );
}

/// Of each kind a worker takes its last lane's work first, but a kind the
/// order puts first is taken on another lane before a later kind on its own:
/// a listing on a free volume never waits behind the decodes queued where the
/// worker last read.
#[test]
fn a_kind_the_order_puts_first_is_taken_on_another_lane_before_a_later_on_the_last() {
    let queued = [
        (ReadKind::Thumbnail, 'a'),
        (ReadKind::List, 'b'),
        (ReadKind::Thumbnail, 'b'),
    ];
    let mut asked = Vec::new();
    let found = first_in_order(Some('a'), |kind, only| {
        asked.push((kind, only));
        queued
            .iter()
            .find(|(each, lane)| *each == kind && only.is_none_or(|only| only == *lane))
            .copied()
    });
    assert_eq!(found, Some((ReadKind::List, 'b')));
    assert_eq!(
        first_in_order(Some('a'), |kind, only| queued
            .iter()
            .find(|(each, lane)| *each == kind
                && *each != ReadKind::List
                && only.is_none_or(|only| only == *lane))
            .copied()),
        Some((ReadKind::Thumbnail, 'a')),
        "of one kind, the last lane's goes first"
    );
    assert!(
        asked.starts_with(&[(ReadKind::Write, Some('a')), (ReadKind::Write, None)]),
        "each kind is asked of the last lane, then of any"
    );
}
