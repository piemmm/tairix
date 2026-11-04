//! Host tests for [`Notifier`](super::Notifier): what a refused notify
//! leaves owed, what folds into it, and when it is offered again.

extern crate alloc;

use alloc::vec::Vec;

use super::Notifier;
use tairix_abi::driver::net_channel::NetChannelNotify;
use tairix_abi::ipc::IPC_MEMORY_RETRY_NS;
use tairix_abi::Errno;

/// The attached stack's notify port.
const PORT: u64 = 0x4E45_5453;

fn notify(link_changed: bool, back_pressure: bool, filtered: u64) -> NetChannelNotify {
    NetChannelNotify {
        link_changed,
        back_pressure,
        filtered,
    }
}

/// A notify refused for room or for the driver's spent budget is owed, its
/// port watched for room, and posted on the room wake with the device's
/// latest count. Regression: the interrupt path dropped a refused
/// notify, and with it a masked device's only request for release.
#[test]
fn a_notify_refused_for_room_or_budget_is_posted_on_the_room_wake() {
    for refused in [Errno::WouldBlock, Errno::LimitExceeded] {
        let mut notifier = Notifier::default();
        notifier.ring(PORT, notify(false, true, 3), |_, _| Err(refused));
        assert_eq!((notifier.room(), notifier.starved()), (Some(PORT), false));
        let mut posted = Vec::new();
        notifier.retry_room(9, |port, sent| {
            posted.push((port, *sent));
            Ok(())
        });
        assert_eq!(posted, [(PORT, notify(false, true, 9))]);
        assert_eq!(notifier.room(), None, "{refused:?}");
    }
}

/// A notify the kernel had no memory for arms no room member and is offered
/// once the pause begun at the loop's next turn has run, the loop parking no
/// longer than that. Regression: it was dropped.
#[test]
fn a_notify_refused_for_memory_is_posted_once_its_retry_falls_due() {
    let mut notifier = Notifier::default();
    let held = notify(false, false, 3);
    notifier.ring(PORT, held, |_, _| Err(Errno::OutOfMemory));
    assert_eq!((notifier.room(), notifier.starved()), (None, true));
    notifier.retry_room(3, |_, _| panic!("not owed for room"));
    let begun = notifier.retry_starved(100, 3, |_, _| panic!("not yet due"));
    assert_eq!(begun, Some(IPC_MEMORY_RETRY_NS), "the pause begins");
    let left = notifier.retry_starved(101, 3, |_, _| panic!("not yet due"));
    assert_eq!(left, Some(IPC_MEMORY_RETRY_NS - 1));
    let mut posted = Vec::new();
    let due = 100 + IPC_MEMORY_RETRY_NS;
    let after = notifier.retry_starved(due, 4, |port, sent| {
        posted.push((port, *sent));
        Ok(())
    });
    assert_eq!(after, None, "nothing waits on memory");
    assert_eq!(posted, [(PORT, notify(false, false, 4))]);
}

/// A notify refused for good is given up on and never offered again, each
/// one counted and the count reported each time it doubles.
#[test]
fn a_notify_refused_for_good_is_counted_and_never_offered_again() {
    let mut notifier = Notifier::default();
    let mut reported = Vec::new();
    for _ in 0..4 {
        let held = notify(false, true, 0);
        notifier.ring(PORT, held, |_, _| Err(Errno::PermissionDenied));
        assert_eq!((notifier.room(), notifier.starved()), (None, false));
        notifier.retry_room(0, |_, _| panic!("nothing is owed"));
        let pause = notifier.retry_starved(0, 0, |_, _| panic!("nothing is owed"));
        assert_eq!(pause, None);
        reported.extend(notifier.lost_to_report());
    }
    assert_eq!(reported, [1, 2, 4]);
}

/// A notify to a port that is gone is dropped and not counted: a restarted
/// stack attaches afresh.
#[test]
fn a_notify_to_a_port_that_is_gone_is_dropped_unreported() {
    let mut notifier = Notifier::default();
    let held = notify(false, true, 0);
    notifier.ring(PORT, held, |_, _| Err(Errno::NotFound));
    assert_eq!((notifier.room(), notifier.starved()), (None, false));
    assert_eq!(notifier.lost_to_report(), None);
}

/// Notifies rung while one is owed join it unsent, keeping any link-change or
/// back-pressure claim, so the stack is told once everything since.
#[test]
fn notifies_rung_while_one_is_owed_fold_into_it() {
    let mut notifier = Notifier::default();
    let first = notify(false, true, 3);
    notifier.ring(PORT, first, |_, _| Err(Errno::WouldBlock));
    let later = notify(true, false, 5);
    notifier.ring(PORT, later, |_, _| panic!("held behind the notify owed"));
    let mut posted = Vec::new();
    notifier.retry_room(6, |port, sent| {
        posted.push((port, *sent));
        Ok(())
    });
    assert_eq!(posted, [(PORT, notify(true, true, 6))]);
}

/// A notify offered again waits for whatever refused it this time, and one
/// whose room member could not be armed waits on the timed retry.
#[test]
fn a_retried_notify_waits_for_whatever_refused_it_this_time() {
    let mut notifier = Notifier::default();
    let held = notify(false, false, 0);
    notifier.ring(PORT, held, |_, _| Err(Errno::WouldBlock));
    notifier.retry_room(0, |_, _| Err(Errno::OutOfMemory));
    assert_eq!((notifier.room(), notifier.starved()), (None, true));
    let begun = notifier.retry_starved(0, 0, |_, _| panic!("not yet due"));
    assert_eq!(begun, Some(IPC_MEMORY_RETRY_NS));
    let pause = notifier.retry_starved(IPC_MEMORY_RETRY_NS, 0, |_, _| Err(Errno::LimitExceeded));
    assert_eq!((pause, notifier.room()), (None, Some(PORT)));
    notifier.watch_refused();
    assert_eq!((notifier.room(), notifier.starved()), (None, true));
}

/// An attach or a detach forgets the notify owed, and the pause timed for
/// it: the channel it reported on has gone.
#[test]
fn forgetting_drops_the_notify_owed_and_its_pause() {
    let mut notifier = Notifier::default();
    let held = notify(false, false, 0);
    notifier.ring(PORT, held, |_, _| Err(Errno::OutOfMemory));
    let begun = notifier.retry_starved(0, 0, |_, _| panic!("not yet due"));
    assert!(begun.is_some());
    notifier.forget();
    assert_eq!((notifier.room(), notifier.starved()), (None, false));
    let due = IPC_MEMORY_RETRY_NS;
    let pause = notifier.retry_starved(due, 0, |_, _| panic!("nothing is owed"));
    assert_eq!(pause, None);
}

/// A notify owed to a port since replaced is dropped for the one rung to the
/// port attached now.
#[test]
fn a_notify_owed_to_a_port_since_replaced_is_dropped_for_the_new_one() {
    let mut notifier = Notifier::default();
    let old = notify(false, true, 0);
    notifier.ring(PORT, old, |_, _| Err(Errno::WouldBlock));
    let mut posted = Vec::new();
    let new = notify(false, false, 1);
    notifier.ring(PORT + 1, new, |port, sent| {
        posted.push((port, *sent));
        Ok(())
    });
    assert_eq!(posted, [(PORT + 1, new)]);
    assert_eq!(notifier.room(), None);
}
