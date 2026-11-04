//! What the interface owes the playback: the commands its mailbox refused,
//! held in the order they were typed and sent when the refusal lifts.
//!
//! The mailbox refuses a command while the interface's lane in it is full or
//! this process's IPC budget is spent, both of which the mailbox's room member
//! reports lifting, and while the kernel has no memory for it, which nothing
//! reports, so that wait is timed. A refusal no wait lifts gives the command
//! up, counted; a mailbox gone with the playback takes what was owed with it.
//!
//! # Folding
//!
//! Each command is an occurrence the playback obeys once — a toggle, a step,
//! a move through the list — so two of a kind are two, with two exceptions:
//!
//! * Nothing is held behind a stop: the playback ignores every control once
//!   it has stopped.
//! * A suspend is held once: a second, typed before the first is even sent,
//!   asks for the stop the first will make.
//!
//! # Bound
//!
//! At most [`COMMAND_CAPACITY`] commands are held, the depth of the
//! interface's own lane in the mailbox. A step or move typed past it is given
//! up and counted; a stop or a suspend never is, and as each is held once at
//! most, the queue outgrows the bound by two at most.

use alloc::collections::VecDeque;

use tairix_abi::ipc::{Reoffer, Sent};
use tairix_abi::Errno;
use tairix_player::Control;

use crate::view::Key;

/// How many commands the playback's mailbox holds from the interface before
/// the playback drains them, and so how many the interface holds behind it.
pub const COMMAND_CAPACITY: usize = 32;

/// The command the playback ends at.
const STOP: Key = Key::Control(Control::Stop);

/// The commands the playback's mailbox refused, in the order they were typed.
#[derive(Debug, Default)]
pub struct Owed {
    /// Oldest first.
    held: VecDeque<Key>,
    /// When they are offered again.
    reoffer: Option<Reoffer>,
    /// Commands that never reached the playback.
    lost: u64,
}

impl Owed {
    /// Owing nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            held: VecDeque::new(),
            reoffer: None,
            lost: 0,
        }
    }

    /// Send `key` through `send`, or hold it when the mailbox refuses it for a
    /// wait, `now_ns` timing a wait on kernel memory. A key typed while
    /// anything is owed is held behind it unsent, so the playback obeys the
    /// keys in the order they were typed.
    pub fn offer<F>(&mut self, key: Key, now_ns: u64, mut send: F)
    where
        F: FnMut(Key) -> Result<(), Errno>,
    {
        if !self.held.is_empty() {
            self.hold(key);
            return;
        }
        match Sent::of(send(key)) {
            Sent::Waits(wait) => {
                self.reoffer = Some(Reoffer::of(wait, now_ns));
                self.hold(key);
            }
            Sent::Refused(_) => self.lost = self.lost.saturating_add(1),
            Sent::Taken | Sent::Gone => {}
        }
    }

    /// Offer what is owed again, in order, until all of it is taken or a send
    /// is refused for a wait. One refused for good is given up and the rest go
    /// on, so nothing is offered again for ever; a mailbox gone with the
    /// playback takes everything owed with it.
    pub fn flush<F>(&mut self, now_ns: u64, mut send: F)
    where
        F: FnMut(Key) -> Result<(), Errno>,
    {
        while let Some(&key) = self.held.front() {
            match Sent::of(send(key)) {
                Sent::Waits(wait) => {
                    self.reoffer = Some(Reoffer::of(wait, now_ns));
                    return;
                }
                Sent::Gone => {
                    self.held.clear();
                    break;
                }
                Sent::Refused(_) => self.lost = self.lost.saturating_add(1),
                Sent::Taken => {}
            }
            self.held.pop_front();
        }
        self.reoffer = None;
    }

    /// Whether the mailbox's room member must be armed: what is owed waits on
    /// room it reports. Room is level-triggered, so it is armed only then.
    #[must_use]
    pub fn watches_room(&self) -> bool {
        !self.held.is_empty() && self.reoffer == Some(Reoffer::OnRoom)
    }

    /// How long after `now_ns` what is owed is offered again on the timer, if
    /// it waits on one.
    #[must_use]
    pub fn retry_in(&self, now_ns: u64) -> Option<u64> {
        if self.held.is_empty() {
            return None;
        }
        let at_ns = self.reoffer?.deadline()?;
        Some(at_ns.saturating_sub(now_ns))
    }

    /// Whether what is owed is due to be offered again on the timer.
    #[must_use]
    pub fn retry_due(&self, now_ns: u64) -> bool {
        self.retry_in(now_ns) == Some(0)
    }

    /// Offer what is owed again on the timer from `now_ns` instead: the room
    /// member that would report the mailbox draining could not be armed.
    pub fn room_unwatched(&mut self, now_ns: u64) {
        if self.watches_room() {
            self.reoffer = Some(Reoffer::pause(now_ns));
        }
    }

    /// How many commands never reached the playback: refused for good, or
    /// typed past the bound.
    #[must_use]
    pub const fn lost(&self) -> u64 {
        self.lost
    }

    /// Hold `key` behind what is owed, unless what is owed answers for it.
    fn hold(&mut self, key: Key) {
        let folded = self.held.back() == Some(&STOP)
            || (key == Key::Suspend && self.held.contains(&Key::Suspend));
        if folded {
            return;
        }
        let once = key == STOP || key == Key::Suspend;
        if (!once && self.held.len() >= COMMAND_CAPACITY) || self.held.try_reserve(1).is_err() {
            self.lost = self.lost.saturating_add(1);
            return;
        }
        self.held.push_back(key);
    }
}

#[cfg(test)]
#[path = "owed_tests.rs"]
mod tests;
