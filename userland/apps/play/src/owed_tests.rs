use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::ipc::IPC_MEMORY_RETRY_NS;
use tairix_abi::Errno;
use tairix_player::Control;

use super::{Owed, COMMAND_CAPACITY};
use crate::view::Key;

const NOW: u64 = 1_000;
const LOUDER: Key = Key::Control(Control::Louder);
const NEXT: Key = Key::Control(Control::Next);
const STOP: Key = Key::Control(Control::Stop);

/// The playback's mailbox: it answers each send with the next of its
/// answers, takes everything once they are spent, and records what it was
/// offered.
struct Mailbox {
    answers: Vec<Result<(), Errno>>,
    offered: Vec<Key>,
}

impl Mailbox {
    fn answering(answers: &[Result<(), Errno>]) -> Self {
        Self {
            answers: answers.iter().rev().copied().collect(),
            offered: Vec::new(),
        }
    }

    fn send(&mut self, key: Key) -> Result<(), Errno> {
        self.offered.push(key);
        self.answers.pop().unwrap_or(Ok(()))
    }
}

/// A command the mailbox refused for want of room, or of this process's own
/// budget, arms the room member and is held, the keys after it held behind
/// it; the room wake sends them in the order they were typed. Regression:
/// the refused command byte was dropped, Ctrl-Z's suspend among them.
#[test]
fn a_command_refused_for_room_or_budget_is_sent_in_order_on_the_room_wake() {
    for refused in [Errno::WouldBlock, Errno::LimitExceeded] {
        let mut mailbox = Mailbox::answering(&[Err(refused)]);
        let mut owed = Owed::new();
        owed.offer(Key::Suspend, NOW, |key| mailbox.send(key));
        owed.offer(LOUDER, NOW, |key| mailbox.send(key));
        assert_eq!(mailbox.offered, [Key::Suspend], "{refused:?}");
        assert!(owed.watches_room(), "{refused:?}");
        assert_eq!(owed.retry_in(NOW), None, "{refused:?}");
        owed.flush(NOW, |key| mailbox.send(key));
        assert_eq!(mailbox.offered, [Key::Suspend, Key::Suspend, LOUDER]);
        assert!(!owed.watches_room(), "{refused:?}");
        assert_eq!(owed.lost(), 0);
    }
}

/// A command the kernel had no memory for arms no room member, which would
/// report room at once for a send refused again, and is sent once the retry
/// falls due, not before. Regression: it was dropped.
#[test]
fn a_command_the_kernel_had_no_memory_for_is_sent_once_the_retry_falls_due() {
    let mut mailbox = Mailbox::answering(&[Err(Errno::OutOfMemory)]);
    let mut owed = Owed::new();
    owed.offer(NEXT, NOW, |key| mailbox.send(key));
    assert!(!owed.watches_room());
    assert_eq!(owed.retry_in(NOW), Some(IPC_MEMORY_RETRY_NS));
    let due = NOW + IPC_MEMORY_RETRY_NS;
    assert!(!owed.retry_due(due - 1));
    assert!(owed.retry_due(due));
    owed.flush(due, |key| mailbox.send(key));
    assert_eq!(mailbox.offered, [NEXT, NEXT]);
    assert_eq!(owed.retry_in(due), None);
}

/// What is owed waits for whatever refused it last: a flush the kernel had
/// no memory for stops watching room and is timed, and one refused for room
/// again watches room and is not.
#[test]
fn what_is_owed_waits_for_whatever_refused_it_last() {
    let mut mailbox = Mailbox::answering(&[
        Err(Errno::WouldBlock),
        Err(Errno::OutOfMemory),
        Err(Errno::WouldBlock),
    ]);
    let mut owed = Owed::new();
    owed.offer(LOUDER, NOW, |key| mailbox.send(key));
    assert!(owed.watches_room());
    owed.flush(NOW, |key| mailbox.send(key));
    assert!(!owed.watches_room());
    assert_eq!(owed.retry_in(NOW), Some(IPC_MEMORY_RETRY_NS));
    let due = NOW + IPC_MEMORY_RETRY_NS;
    owed.flush(due, |key| mailbox.send(key));
    assert!(owed.watches_room());
    assert_eq!(owed.retry_in(due), None);
}

/// A command refused for good is given up once and counted, never offered
/// again, and the commands held behind it go on. Regression: it was dropped
/// unstated.
#[test]
fn a_command_refused_for_good_is_given_up_once_and_counted() {
    let mut mailbox = Mailbox::answering(&[Err(Errno::PermissionDenied)]);
    let mut owed = Owed::new();
    owed.offer(LOUDER, NOW, |key| mailbox.send(key));
    assert_eq!(owed.lost(), 1);
    assert!(!owed.watches_room());
    assert_eq!(owed.retry_in(NOW), None);
    owed.flush(NOW, |key| mailbox.send(key));
    assert_eq!(mailbox.offered, [LOUDER], "nor offered again");

    let mut mailbox = Mailbox::answering(&[Err(Errno::WouldBlock), Err(Errno::PermissionDenied)]);
    let mut owed = Owed::new();
    owed.offer(LOUDER, NOW, |key| mailbox.send(key));
    owed.offer(NEXT, NOW, |key| mailbox.send(key));
    owed.flush(NOW, |key| mailbox.send(key));
    assert_eq!(mailbox.offered, [LOUDER, LOUDER, NEXT]);
    assert_eq!(owed.lost(), 1);
    assert!(!owed.watches_room());
}

/// Commands to a mailbox gone with the playback are dropped: nobody waits on
/// them, so nothing is held or counted.
#[test]
fn commands_to_a_playback_that_is_gone_are_dropped_uncounted() {
    let mut mailbox = Mailbox::answering(&[Err(Errno::NotFound)]);
    let mut owed = Owed::new();
    owed.offer(STOP, NOW, |key| mailbox.send(key));
    assert_eq!(owed.lost(), 0);
    assert!(!owed.watches_room());

    let mut mailbox = Mailbox::answering(&[Err(Errno::WouldBlock), Err(Errno::NotFound)]);
    let mut owed = Owed::new();
    owed.offer(LOUDER, NOW, |key| mailbox.send(key));
    owed.offer(NEXT, NOW, |key| mailbox.send(key));
    owed.flush(NOW, |key| mailbox.send(key));
    assert_eq!(mailbox.offered, [LOUDER, LOUDER], "the rest went with it");
    assert_eq!(owed.lost(), 0);
    assert!(!owed.watches_room());
}

/// Nothing is held behind a stop, which the playback ends at, and a suspend
/// is held once; every other command is held once each time it was typed.
#[test]
fn only_what_a_stop_or_a_held_suspend_answers_for_folds() {
    let mut mailbox = Mailbox::answering(&[Err(Errno::WouldBlock)]);
    let mut owed = Owed::new();
    let typed = [Key::Suspend, LOUDER, LOUDER, Key::Suspend, STOP, NEXT];
    for key in typed {
        owed.offer(key, NOW, |key| mailbox.send(key));
    }
    owed.flush(NOW, |key| mailbox.send(key));
    assert_eq!(
        mailbox.offered,
        [Key::Suspend, Key::Suspend, LOUDER, LOUDER, STOP]
    );
    assert_eq!(owed.lost(), 0, "what folds is answered for, not lost");
}

/// At most the mailbox's own depth is held: a step past it is given up and
/// counted, but a stop and a suspend are never refused for room.
#[test]
fn steps_past_the_mailbox_depth_are_given_up_but_a_stop_and_a_suspend_are_held() {
    let mut mailbox = Mailbox::answering(&[Err(Errno::WouldBlock)]);
    let mut owed = Owed::new();
    for _ in 0..=COMMAND_CAPACITY {
        owed.offer(LOUDER, NOW, |key| mailbox.send(key));
    }
    assert_eq!(owed.lost(), 1);
    owed.offer(Key::Suspend, NOW, |key| mailbox.send(key));
    owed.offer(STOP, NOW, |key| mailbox.send(key));
    assert_eq!(owed.lost(), 1);
    owed.flush(NOW, |key| mailbox.send(key));
    let mut sent = vec![LOUDER; COMMAND_CAPACITY + 1];
    sent.extend([Key::Suspend, STOP]);
    assert_eq!(mailbox.offered, sent);
}

/// What is owed while the mailbox's room cannot be watched is offered again
/// on the timer instead.
#[test]
fn a_room_that_cannot_be_watched_is_waited_for_on_the_timer() {
    let mut mailbox = Mailbox::answering(&[Err(Errno::WouldBlock)]);
    let mut owed = Owed::new();
    owed.offer(NEXT, NOW, |key| mailbox.send(key));
    owed.room_unwatched(NOW);
    assert!(!owed.watches_room());
    assert_eq!(owed.retry_in(NOW), Some(IPC_MEMORY_RETRY_NS));
}
