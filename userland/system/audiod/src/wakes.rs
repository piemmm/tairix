//! The wakes the service owes its clients: what a stream's mailbox refused
//! for want of room, budget or kernel memory, held until it can go.
//!
//! A client parks on its stream's mailbox, so a wake it is never sent is a
//! refill it never makes, a drain it never sees end, or a device it never
//! learns it lost. A refusal waiting lifts therefore holds the wake: a full
//! mailbox, or this service's own spent `ipc-queue-bytes` budget, until the
//! mailbox's room member — which the kernel wakes for either — reports room,
//! and kernel memory until the pause the serve loop times, since nothing
//! reports memory freeing. A mailbox already owed a wake takes the next behind
//! it, never around it.
//!
//! # Folding
//!
//! A stream owes its client at most one wake of each kind, so what a client
//! that stays behind costs is bounded by its streams, never by how long it was
//! away:
//!
//! * `SpaceAvailable` says where the service has reached in the ring; the
//!   newest supersedes the one before it.
//! * `StateChanged` is the state the client converges on and the frame it
//!   changed at; the newest supersedes the one before it, as a `State` read
//!   would.
//! * `Xrun` counts lost frames: a held one absorbs the next, the frames summing
//!   at the later position.
//!
//! A folded wake takes the place of the newest it absorbed, so a client hears
//! its stream's wakes in the order they happened. A closed stream's held wakes
//! go with it, and the store is sized when a stream opens, so holding a wake
//! never allocates on the period path.
//!
//! A wake refused for good is given up, counted and reported; one to a mailbox
//! gone with its client loses nobody anything.

use alloc::vec::Vec;

use tairix_abi::audio::AudioNotify;
use tairix_abi::ipc::{SendWait, Sent};
use tairix_abi::waitset::WaitSetOp;
use tairix_inline::ArrayVec;
use tairix_log::Doubling;
use tairix_util::fallible;

use crate::service::Notifier;

/// The wake kinds `audio-v1` defines: a stream owes at most one of each.
const KINDS: usize = 3;

/// A wake for a stream's client, as the engine's streams tell it.
pub(crate) trait Wake {
    /// Tell `mailbox` `what`, now or once it can take it.
    fn wake(&mut self, mailbox: u64, what: AudioNotify);
}

/// One mailbox's held wakes, oldest first, what they wait for, and whether
/// its room member is armed.
struct Owed {
    mailbox: u64,
    wait: SendWait,
    watched: bool,
    held: ArrayVec<AudioNotify, KINDS>,
}

/// The wakes the service owes its clients, over the transport that carries
/// them.
pub struct Wakes<N: Notifier> {
    transport: N,
    owed: Vec<Owed>,
    given_up: Doubling,
}

impl<N: Notifier> Wakes<N> {
    /// Wakes carried by `transport`, owing nothing.
    pub(crate) fn new(transport: N) -> Self {
        Self {
            transport,
            owed: Vec::new(),
            given_up: Doubling::new(),
        }
    }

    /// Make room to hold what the clients of `streams` streams may be owed,
    /// so the period path never allocates to hold a wake.
    pub(crate) fn make_room(&mut self, streams: usize) -> bool {
        let held = self.owed.len();
        fallible::reserve(&mut self.owed, streams.saturating_sub(held))
    }

    /// Offer every held wake again, mailbox by mailbox, each stopping at its
    /// first refusal a wait lifts: room came, or the pause ran.
    pub fn flush(&mut self) {
        let Self {
            transport,
            owed,
            given_up,
        } = self;
        for owing in owed.iter_mut() {
            while let Some(&what) = owing.held.first() {
                match Sent::of(transport.notify(owing.mailbox, &what.encode())) {
                    Sent::Waits(wait) => {
                        owing.wait = wait;
                        break;
                    }
                    Sent::Gone => owing.held.clear(),
                    sent => {
                        if matches!(sent, Sent::Refused(_)) {
                            given_up.note();
                        }
                        owing.held.remove(0);
                    }
                }
            }
        }
    }

    /// Keep a room member armed, through `member`, on exactly the mailboxes
    /// whose held wakes wait on room — `member` adds or deletes the one for a
    /// mailbox, answering whether it took — and let go of what is settled,
    /// answering whether every one is armed: one that cannot be has nothing to
    /// report its room, so what it is owed waits on the pause instead. Called
    /// before every wait, since room is level-triggered and a member left on a
    /// mailbox owed nothing would wake the loop for ever.
    pub fn watch(&mut self, member: &mut dyn FnMut(WaitSetOp, u64) -> bool) -> bool {
        let mut every = true;
        for owing in &mut self.owed {
            let wanted = owing.wait == SendWait::Room && !owing.held.is_empty();
            if owing.watched && !wanted {
                member(WaitSetOp::Del, owing.mailbox);
                owing.watched = false;
            } else if wanted && !owing.watched {
                owing.watched = member(WaitSetOp::Add, owing.mailbox);
                every &= owing.watched;
            }
        }
        self.owed.retain(|owing| !owing.held.is_empty());
        every
    }

    /// Whether a held wake waits on kernel memory, which nothing reports: the
    /// serve loop then owes [`Self::flush`] once a pause has run.
    #[must_use]
    pub fn starved(&self) -> bool {
        self.owed
            .iter()
            .any(|owing| owing.wait == SendWait::Memory && !owing.held.is_empty())
    }

    /// How many wakes were refused for good and given up on, once that total
    /// has doubled since it was last taken.
    pub fn given_up(&mut self) -> Option<u64> {
        self.given_up.take_due()
    }

    /// Forget what `mailbox` is owed for the stream `stream_id`, which closed.
    pub(crate) fn forget(&mut self, mailbox: u64, stream_id: u64) {
        if let Some(owing) = self.owed.iter_mut().find(|owing| owing.mailbox == mailbox) {
            owing.held.retain(|what| what.stream_id() != stream_id);
        }
    }
}

impl<N: Notifier> Wake for Wakes<N> {
    fn wake(&mut self, mailbox: u64, what: AudioNotify) {
        let at = self.owed.iter().position(|owing| owing.mailbox == mailbox);
        if let Some(owing) = at
            .and_then(|at| self.owed.get_mut(at))
            .filter(|owing| !owing.held.is_empty())
        {
            if !fold(&mut owing.held, what) {
                self.given_up.note();
            }
            return;
        }
        let wait = match Sent::of(self.transport.notify(mailbox, &what.encode())) {
            Sent::Waits(wait) => wait,
            Sent::Refused(_) => return self.given_up.note(),
            Sent::Taken | Sent::Gone => return,
        };
        let mut held = ArrayVec::new();
        // A store holds one of each kind, so an empty one takes this wake.
        let _ = held.try_push(what);
        if let Some(owing) = at.and_then(|at| self.owed.get_mut(at)) {
            owing.wait = wait;
            owing.held = held;
            return;
        }
        let owing = Owed {
            mailbox,
            wait,
            watched: false,
            held,
        };
        if !fallible::push(&mut self.owed, owing) {
            self.given_up.note();
        }
    }
}

/// Fold `what` into `held`, one mailbox's held wakes: a held wake of its
/// stream and kind is taken out — a loss's frames carried into `what` — and
/// `what` goes to the back, where the newest of what it stands for happened.
/// `false` only when the store, one of each kind, is outgrown.
fn fold(held: &mut ArrayVec<AudioNotify, KINDS>, mut what: AudioNotify) -> bool {
    let kind = held.iter().position(|old| same_kind(old, &what));
    if let Some(old) = kind.and_then(|at| held.remove(at)) {
        absorb(&mut what, &old);
    }
    held.try_push(what).is_ok()
}

/// Whether `a` and `b` are one stream's wakes of one kind.
fn same_kind(a: &AudioNotify, b: &AudioNotify) -> bool {
    a.stream_id() == b.stream_id() && core::mem::discriminant(a) == core::mem::discriminant(b)
}

/// Carry into `what` the frames `old`, the loss it supersedes, counted lost.
fn absorb(what: &mut AudioNotify, old: &AudioNotify) {
    if let (
        AudioNotify::Xrun { lost_frames, .. },
        AudioNotify::Xrun {
            lost_frames: before,
            ..
        },
    ) = (what, old)
    {
        *lost_frames = lost_frames.saturating_add(*before);
    }
}
