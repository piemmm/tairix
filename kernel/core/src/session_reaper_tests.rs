//! The reaper's queue: what it takes, and in what order it ends it.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tairix_abi::ProcId;
use tairix_kernel_sec::CapTable;
use tairix_sync::{RwLock, SpinLock};

use super::{Notifier, SessionObserver, SessionReaper};
use crate::test_boot::{claim_peer_task, claim_scheduler, take_unparked};
use crate::test_sink::TestSink;
use crate::waitq::NO_DEADLINE;

fn table() -> &'static RwLock<CapTable> {
    crate::forever(Box::new(RwLock::new(CapTable::new())))
}

fn reaper(caps: &'static RwLock<CapTable>) -> SessionReaper {
    SessionReaper {
        caps,
        audit: crate::forever(Box::new(TestSink::new())),
        ending: SpinLock::new(VecDeque::new()),
        serving: AtomicBool::new(false),
    }
}

fn anchor(byte: u8) -> ProcId {
    ProcId::from_raw([byte; 16])
}

#[test]
fn nothing_is_handed_to_a_reaper_that_cannot_yet_be_woken() {
    let caps = table();
    let reaper = reaper(caps);
    assert!(
        !reaper.queue(caps, anchor(1)),
        "the walk stays where it ended"
    );
    assert_eq!(reaper.next(), None);
}

#[test]
fn a_serving_reaper_ends_its_own_tables_sessions_oldest_first() {
    let caps = table();
    let reaper = reaper(caps);
    reaper.serving.store(true, Ordering::Release);
    assert!(reaper.queue(caps, anchor(1)));
    assert!(reaper.queue(caps, anchor(2)));
    assert_eq!(reaper.next(), Some(anchor(1)));
    assert_eq!(reaper.next(), Some(anchor(2)));
    assert_eq!(reaper.next(), None);
}

#[test]
fn a_session_over_another_table_is_left_where_it_ended() {
    let reaper = reaper(table());
    reaper.serving.store(true, Ordering::Release);
    assert!(!reaper.queue(table(), anchor(1)));
    assert_eq!(reaper.next(), None);
}

#[test]
fn a_notifier_is_started_though_the_reaper_cannot_be() {
    let mut admitted = 0u64;
    let started = super::start(
        table(),
        crate::forever(Box::new(TestSink::new())),
        |_body| {
            admitted += 1;
            (admitted <= super::NOTIFIER_TASKS as u64).then_some(admitted)
        },
    );
    assert_eq!(started, Err(super::ReaperNotStarted::NotAdmitted));
    assert!(
        matches!(super::NOTIFIER.get(), Ok(Some(_))),
        "what ends with a session is still told on the notifier"
    );
}

#[test]
fn a_notifier_not_yet_serving_takes_no_word() {
    let notifier = Notifier::over(table());
    assert!(!notifier.owe(), "the reaper tells the observer itself");
    assert!(!notifier.take_owed());
}

/// An observer counting its tellings and settling `owed` pieces of work,
/// noting whether `table` could be written while it was told.
struct Counting {
    told: AtomicUsize,
    owed: AtomicUsize,
    settled: AtomicUsize,
    table: SpinLock<Option<&'static RwLock<CapTable>>>,
    writable_while_told: AtomicBool,
}

impl SessionObserver for Counting {
    fn session_ended(&self, closed: &dyn Fn(ProcId) -> bool) {
        let _ = closed(anchor(0x77));
        if let Some(table) = *self.table.lock() {
            self.writable_while_told
                .store(table.try_write().is_some(), Ordering::Release);
        }
        self.told.fetch_add(1, Ordering::AcqRel);
    }

    fn settle(&self) -> bool {
        let took = self
            .owed
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |owed| {
                owed.checked_sub(1)
            })
            .is_ok();
        if took {
            self.settled.fetch_add(1, Ordering::AcqRel);
        }
        took
    }

    fn owes(&self) -> bool {
        self.owed.load(Ordering::Acquire) > 0
    }
}

static COUNTING: Counting = Counting {
    told: AtomicUsize::new(0),
    owed: AtomicUsize::new(0),
    settled: AtomicUsize::new(0),
    table: SpinLock::new(None),
    writable_while_told: AtomicBool::new(false),
};

/// One test, since the observer is the process's own: every path that ends
/// a session tells it, and settles all it owes, exactly where it may park.
#[test]
fn every_session_end_tells_the_observer_and_settles_what_it_owes() {
    let told = || COUNTING.told.load(Ordering::Acquire);
    let settled = || COUNTING.settled.load(Ordering::Acquire);
    super::observe(&COUNTING);
    COUNTING.owed.store(3, Ordering::Release);
    super::session_over(table(), false);
    assert_eq!(told(), 0, "a caller that may not park tells nothing");
    super::owe_work();
    assert_eq!(
        settled(),
        0,
        "with no notifier, work owed waits for a session's end"
    );
    let first = table();
    *COUNTING.table.lock() = Some(first);
    super::session_over(first, true);
    assert_eq!((told(), settled()), (1, 3), "told, and every piece settled");
    assert!(!COUNTING.owes());
    assert!(
        COUNTING.writable_while_told.load(Ordering::Acquire),
        "no telling holds the table while the observer works"
    );

    let caps = table();
    let reaper = reaper(caps);
    reaper.serving.store(true, Ordering::Release);
    assert!(reaper.queue(caps, anchor(1)) && reaper.queue(caps, anchor(2)));
    COUNTING.owed.store(1, Ordering::Release);
    reaper.end_queued();
    assert_eq!(
        (told(), settled()),
        (3, 4),
        "the reaper has it told of each session it ends"
    );
    assert_eq!(reaper.next(), None);

    let notifier = Notifier::over(caps);
    notifier.serving.store(true, Ordering::Release);
    assert!(notifier.owe() && notifier.owe());
    COUNTING.owed.store(2, Ordering::Release);
    notifier.round();
    assert_eq!(
        (told(), settled()),
        (4, 6),
        "the notifier tells once for both ends and settles all"
    );
    notifier.round();
    assert_eq!(told(), 4, "nothing ended since");
}

#[test]
fn a_session_end_wakes_one_notifier_task_not_every_one() {
    let _ = claim_scheduler();
    let arch = crate::waitq::wait_arch().expect("the claimed hook");
    let notifier = Notifier::over(table());
    let parked: [_; super::NOTIFIER_TASKS] = core::array::from_fn(|_| claim_peer_task());
    for task in parked {
        notifier.parked.register(task, NO_DEADLINE).expect("filed");
    }
    let _ = take_unparked();
    assert!(!notifier.tell_ended(arch), "not serving yet");
    notifier.wake(arch);
    assert!(
        take_unparked().is_empty(),
        "no task is woken before it serves"
    );
    notifier.serving.store(true, Ordering::Release);
    assert!(notifier.tell_ended(arch));
    assert_eq!(take_unparked().len(), 1, "one task takes the telling");
    notifier.wake(arch);
    assert_eq!(take_unparked().len(), 1, "and one the work owed");
}

#[test]
fn word_of_several_ended_sessions_is_one_telling() {
    let notifier = Notifier::over(table());
    notifier.serving.store(true, Ordering::Release);
    assert!(notifier.owe());
    assert!(notifier.owe());
    assert!(notifier.take_owed(), "owed once");
    assert!(!notifier.take_owed(), "and settled");
    assert!(notifier.owe(), "a later end is owed afresh");
    assert!(notifier.take_owed());
}
