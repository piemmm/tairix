//! The receive notify the interrupt path sends the stack, held when the
//! stack's port refuses it.
//!
//! A notify is the stack's only report of a receive, of a link change and,
//! while a completion source is masked, its only request to release one, so a
//! notify the port refuses for a reason waiting lifts is held rather than
//! dropped. One held notify is all the stack is ever owed: a later one folds
//! into it as the stack folds a run of them, keeping the latest pre-filter
//! count and any link-change or back-pressure claim, so the stack is told
//! once everything since it was last told. It goes out with the device's
//! newest count.

use tairix_abi::driver::net_channel::NetChannelNotify;
use tairix_abi::ipc::{MemoryPause, SendWait, Sent};
use tairix_abi::Errno;
use tairix_log::Doubling;

/// The notify owed to the stack's port `endpoint`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Owed {
    endpoint: u64,
    notify: NetChannelNotify,
    wait: SendWait,
}

/// The receive notify: at most one owed to the stack, and a count of those
/// given up on.
#[derive(Debug, Default)]
pub struct Notifier {
    owed: Option<Owed>,
    /// The pause after which a notify owed for kernel memory is offered
    /// again, once one has begun.
    pause: MemoryPause,
    /// Notifies refused for good.
    lost: Doubling,
}

impl Notifier {
    /// Ring `endpoint`, the attached stack's port, with `notify` through
    /// `send`. A notify owed there takes this one in unsent rather than being
    /// overtaken by it; one owed to a port no longer attached is dropped with
    /// its channel.
    pub fn ring<F>(&mut self, endpoint: u64, notify: NetChannelNotify, send: F)
    where
        F: FnOnce(u64, &NetChannelNotify) -> Result<(), Errno>,
    {
        if let Some(owed) = self.owed.as_mut().filter(|owed| owed.endpoint == endpoint) {
            owed.notify = fold(owed.notify, notify);
            return;
        }
        self.offer(endpoint, notify, send);
    }

    /// Offer a notify owed for room again through `send`, carrying the
    /// device's latest pre-filter count `filtered`: the room wake's work.
    pub fn retry_room<F>(&mut self, filtered: u64, send: F)
    where
        F: FnOnce(u64, &NetChannelNotify) -> Result<(), Errno>,
    {
        if self.owed.is_some_and(|owed| owed.wait == SendWait::Room) {
            self.offer_owed(filtered, send);
        }
    }

    /// The memory retry at `now`: once the pause begun for a notify the
    /// kernel had no memory for has run, offer it again as
    /// [`Self::retry_room`] does. Returns how long the loop may park before
    /// the next such retry, beginning a pause where none runs, while the
    /// notify owed still waits on memory.
    #[must_use]
    pub fn retry_starved<F>(&mut self, now: u64, filtered: u64, send: F) -> Option<u64>
    where
        F: FnOnce(u64, &NetChannelNotify) -> Result<(), Errno>,
    {
        if self.pause.ran_out(|| now) {
            self.offer_owed(filtered, send);
        }
        self.pause.keep(self.starved(), || now);
        self.pause.ends().map(|at| at.saturating_sub(now))
    }

    /// The room member could not be armed: the notify owed for room waits on
    /// the timed retry instead, which nothing else would then lift.
    pub fn watch_refused(&mut self) {
        if let Some(owed) = self.owed.as_mut() {
            owed.wait = SendWait::Memory;
        }
    }

    /// Whether the notify owed waits on kernel memory, so the loop must time
    /// its retry.
    #[must_use]
    pub fn starved(&self) -> bool {
        self.owed.is_some_and(|owed| owed.wait == SendWait::Memory)
    }

    /// The port whose room member the loop must arm: the stack's, while the
    /// notify owed there waits for room. Room is level-triggered, so a member
    /// left armed with nothing owed would wake the loop for ever.
    #[must_use]
    pub fn room(&self) -> Option<u64> {
        self.owed
            .filter(|owed| owed.wait == SendWait::Room)
            .map(|owed| owed.endpoint)
    }

    /// Drop the notify owed: the channel it reported on has detached, or is
    /// attached afresh.
    pub fn forget(&mut self) {
        self.owed = None;
        self.pause.stop();
    }

    /// How many notifies have been given up on, once the count has doubled
    /// since it was last reported.
    pub fn lost_to_report(&mut self) -> Option<u64> {
        self.lost.take_due()
    }

    /// Offer the notify owed again, with the device's latest `filtered` in
    /// place of the count it was rung with.
    fn offer_owed<F>(&mut self, filtered: u64, send: F)
    where
        F: FnOnce(u64, &NetChannelNotify) -> Result<(), Errno>,
    {
        if let Some(owed) = self.owed {
            let notify = NetChannelNotify {
                filtered,
                ..owed.notify
            };
            self.offer(owed.endpoint, notify, send);
        }
    }

    /// Post `notify` to `endpoint` through `send`, holding it for what
    /// refused it, or counting it given up on when no wait lifts the refusal.
    fn offer<F>(&mut self, endpoint: u64, notify: NetChannelNotify, send: F)
    where
        F: FnOnce(u64, &NetChannelNotify) -> Result<(), Errno>,
    {
        self.owed = None;
        self.pause.stop();
        let wait = match Sent::of(send(endpoint, &notify)) {
            Sent::Waits(wait) => wait,
            Sent::Refused(_) => return self.lost.note(),
            // A stack gone with its port loses nothing: a restarted one
            // attaches afresh.
            Sent::Taken | Sent::Gone => return,
        };
        self.owed = Some(Owed {
            endpoint,
            notify,
            wait,
        });
    }
}

/// `later` folded into `held` as the stack folds a run of notifies: the
/// latest pre-filter count, and any link-change or back-pressure claim, which
/// a later notify making none must not erase.
fn fold(held: NetChannelNotify, later: NetChannelNotify) -> NetChannelNotify {
    NetChannelNotify {
        link_changed: held.link_changed || later.link_changed,
        back_pressure: held.back_pressure || later.back_pressure,
        filtered: later.filtered,
    }
}

#[cfg(test)]
mod tests;
