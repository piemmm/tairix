//! What the mixer is owed: the notifications its ports refused for want of
//! room, budget or kernel memory, held until they can go.
//!
//! The mixer parks on its notify ports, so a notification it is never sent is
//! a period it never refills, a drain it never sees end, a connector it never
//! reads again, or a fault it never hears, and the endpoint raises nothing more
//! on its own. A refusal waiting lifts therefore holds the notification: a full
//! port, or this driver's own spent `ipc-queue-bytes` budget, until the port's
//! room member — which the kernel wakes for either — reports room, and kernel
//! memory until the pause the serve loop times, since nothing reports memory
//! freeing. A port already owed something takes the next notification behind
//! it, never around it.
//!
//! # Folding
//!
//! An endpoint owes at most one notification of each kind, so what a mixer that
//! falls behind costs is bounded by the protocol's own endpoint ceiling and
//! kinds, never by how far behind it fell:
//!
//! * `PeriodElapsed` carries the clock pair and asks for a refill; the newest
//!   pair supersedes the one before it.
//! * `Xrun` counts lost frames: a held one absorbs the next, the frames summing
//!   at the later position.
//! * `Drained`, `JackChanged` and `Faulted` each say where the endpoint now
//!   stands; the newest supersedes the one before it.
//!
//! A folded notification takes the place of the newest it absorbed, so what is
//! held goes in the order it happened. Each goes where its endpoint reports
//! when it is offered — a connector's change to the watch, the rest to the
//! endpoint's attachment — and one whose endpoint the mixer has let go is owed
//! to nobody.
//!
//! A notification refused for good is given up, counted and reported; one to a
//! port gone with its mixer loses nobody anything.

use tairix_abi::driver::audio_channel::AudioChannelNotify;
use tairix_abi::ipc::{SendWait, Sent};
use tairix_abi::Errno;
use tairix_inline::ArrayVec;
use tairix_log::Doubling;

use crate::server::ENDPOINT_SLOTS;

/// The notification kinds `audiochan-v1` defines: an endpoint holds one of
/// each at most.
const KINDS: usize = 5;

/// Notifications held at most: one of each kind for every endpoint the
/// channel tracks.
const HELD: usize = KINDS * ENDPOINT_SLOTS;

/// The ports a channel tells at most: every endpoint's attachment and the
/// watch.
pub(crate) const PORTS: usize = ENDPOINT_SLOTS + 1;

/// One held notification and what its port last answered.
#[derive(Copy, Clone, Debug)]
struct Pending {
    what: AudioChannelNotify,
    wait: SendWait,
}

/// The notifications the mixer is owed.
#[derive(Default)]
pub(crate) struct Owed {
    /// Oldest first; one of each kind per endpoint.
    pending: ArrayVec<Pending, HELD>,
    /// Notifications given up on.
    given_up: Doubling,
}

impl Owed {
    /// Tell `what` at the port `to` names for it: at once while that port is
    /// owed nothing, else behind what it is owed, a held notification of its
    /// endpoint and kind folded into it.
    pub(crate) fn tell<D, S>(&mut self, mut what: AudioChannelNotify, to: D, mut send: S)
    where
        D: Fn(&AudioChannelNotify) -> Option<u64>,
        S: FnMut(u64, &AudioChannelNotify) -> Result<(), Errno>,
    {
        let Some(port) = to(&what) else {
            return;
        };
        let owed = self
            .pending
            .iter()
            .find(|held| to(&held.what) == Some(port))
            .map(|held| held.wait);
        let kind = self
            .pending
            .iter()
            .position(|held| same_kind(&held.what, &what));
        if let Some(folded) = kind.and_then(|at| self.pending.remove(at)) {
            absorb(&mut what, &folded.what);
        }
        let wait = match owed {
            Some(wait) => wait,
            None => match Sent::of(send(port, &what)) {
                Sent::Waits(wait) => wait,
                Sent::Refused(_) => return self.given_up.note(),
                Sent::Taken | Sent::Gone => return,
            },
        };
        self.hold(Pending { what, wait });
    }

    /// Offer everything held again, oldest first, each port stopping at its
    /// first refusal a wait lifts: room came, or the pause ran.
    pub(crate) fn flush<D, S>(&mut self, to: D, mut send: S)
    where
        D: Fn(&AudioChannelNotify) -> Option<u64>,
        S: FnMut(u64, &AudioChannelNotify) -> Result<(), Errno>,
    {
        // What each port answered this pass: the wait it stands at, or `None`
        // once it proved gone.
        let mut answered: ArrayVec<(u64, Option<SendWait>), PORTS> = ArrayVec::new();
        let mut at = 0;
        while let Some(&held) = self.pending.get(at) {
            let Some(port) = to(&held.what) else {
                self.pending.remove(at);
                continue;
            };
            let stands =
                if let Some(&(_, stands)) = answered.iter().find(|(offered, _)| *offered == port) {
                    stands
                } else {
                    let stands = match Sent::of(send(port, &held.what)) {
                        Sent::Waits(wait) => Some(wait),
                        Sent::Gone => None,
                        settled => {
                            if matches!(settled, Sent::Refused(_)) {
                                self.given_up.note();
                            }
                            self.pending.remove(at);
                            continue;
                        }
                    };
                    if answered.try_push((port, stands)).is_err() {
                        return;
                    }
                    stands
                };
            if let Some(wait) = stands {
                if let Some(held) = self.pending.get_mut(at) {
                    held.wait = wait;
                }
                at += 1;
            } else {
                self.pending.remove(at);
            }
        }
    }

    /// Whether a held notification `to` names `port` for waits on its room.
    pub(crate) fn waits_on_room<D>(&self, port: u64, to: D) -> bool
    where
        D: Fn(&AudioChannelNotify) -> Option<u64>,
    {
        self.pending
            .iter()
            .any(|held| held.wait == SendWait::Room && to(&held.what) == Some(port))
    }

    /// The ports `to` names for the held notifications waiting on room, once
    /// for each.
    pub(crate) fn rooms<'a, D>(&'a self, to: D) -> impl Iterator<Item = u64> + 'a
    where
        D: Fn(&AudioChannelNotify) -> Option<u64> + 'a,
    {
        self.pending
            .iter()
            .filter(|held| held.wait == SendWait::Room)
            .filter_map(move |held| to(&held.what))
    }

    /// Whether a held notification waits on kernel memory.
    pub(crate) fn starved(&self) -> bool {
        self.pending
            .iter()
            .any(|held| held.wait == SendWait::Memory)
    }

    /// The running total given up on, once it has doubled since it was last
    /// taken.
    pub(crate) fn given_up(&mut self) -> Option<u64> {
        self.given_up.take_due()
    }

    /// Keep `held` behind everything held already, giving it up — counted —
    /// should a kind the protocol gains outgrow the store.
    fn hold(&mut self, held: Pending) {
        if self.pending.try_push(held).is_err() {
            self.given_up.note();
        }
    }
}

/// The endpoint `what` is about.
pub(crate) const fn endpoint(what: &AudioChannelNotify) -> u16 {
    match *what {
        AudioChannelNotify::PeriodElapsed { endpoint, .. }
        | AudioChannelNotify::Xrun { endpoint, .. }
        | AudioChannelNotify::Drained { endpoint, .. }
        | AudioChannelNotify::JackChanged { endpoint, .. }
        | AudioChannelNotify::Faulted { endpoint, .. } => endpoint,
    }
}

/// Whether `a` and `b` are one endpoint's notifications of one kind.
fn same_kind(a: &AudioChannelNotify, b: &AudioChannelNotify) -> bool {
    endpoint(a) == endpoint(b) && core::mem::discriminant(a) == core::mem::discriminant(b)
}

/// Carry into `what` the frames `folded`, the loss it supersedes, counted
/// lost.
fn absorb(what: &mut AudioChannelNotify, folded: &AudioChannelNotify) {
    if let (
        AudioChannelNotify::Xrun { lost_frames, .. },
        AudioChannelNotify::Xrun {
            lost_frames: before,
            ..
        },
    ) = (what, folded)
    {
        *lost_frames = lost_frames.saturating_add(*before);
    }
}
