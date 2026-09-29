//! Reading directories off an interactive loop: the request/answer policy a
//! worker and a serve loop share.
//!
//! Listing a directory is a `fs_open` + `fs_readdir` on whatever disk holds it.
//! Run on the loop's own task it stalls the compositor, the seat drain, and
//! every application blocked in a window call for as long as that disk takes —
//! which on a slow or contended device is not a frame but a visible freeze. So
//! it runs on a worker thread, and the loop learns the answer landed through
//! the wait-set it already parks in.
//!
//! [`ListingDesk`] is that arrangement's whole policy, and it holds no lock, no
//! thread, and no syscall: one request slot and one answer slot per consumer,
//! the staleness rule that drops an answer nobody wants any more, the freshness
//! rule that never answers a [`refresh`](ListingDesk::refresh) with a read that
//! began before it, and the round-robin that stops one busy consumer starving
//! another. A `Run` binary wraps it in the runtime's futex mutex, parks a worker
//! on a condition variable over it, and nudges the loop's wake pipe.
//!
//! # Consumers are named, not counted
//!
//! How many things in one program list directories is a structural fact about
//! that program — the desktop session has its icon column and its trusted file
//! picker; the file manager has its browser — not a capacity a bigger machine
//! outgrows. Each program therefore declares its consumers as a closed
//! [`ListingClient`] set and every one gets a slot, so none can lose its place
//! to another.
//!
//! # Nothing here waits
//!
//! A request is *recorded*; the answer is *collected*. Both are plain state
//! transitions. The party that blocks is the worker, on its condition variable,
//! and the party that parks is the loop, on its wait-set — never this.

use alloc::string::String;
use alloc::vec::Vec;
use core::marker::PhantomData;

use tairix_abi::Errno;

use crate::entry::Entry;
use crate::source::Listing;

/// One program's closed set of directory-listing consumers.
///
/// The slot order is [`ALL`](Self::ALL)'s own order, and a consumer's slot is
/// its position in it — derived rather than declared, so an enumeration and its
/// slot mapping cannot drift apart.
pub trait ListingClient: Copy + Eq + 'static {
    /// Every consumer of this program's desk, in slot order.
    const ALL: &'static [Self];
}

/// What one consumer has asked for and what it has been answered.
#[derive(Default)]
struct Slot {
    /// The directory this consumer is waiting on, cleared when its answer is
    /// stored. Recording a different one abandons the old request.
    wanted: Option<Vec<String>>,
    /// Whether a worker has taken [`Slot::wanted`] and not yet answered it, so
    /// the same request is never handed out twice.
    reading: bool,
    /// Advanced by every fresh request, and stamped on each job handed out, so
    /// a read that began before the latest one is recognised when it lands.
    request: u64,
    /// The answer, kept until the consumer asks for that same directory.
    done: Option<Answer>,
}

impl Slot {
    /// Make `components` the directory this consumer is waiting on.
    fn want(&mut self, components: &[String]) {
        if self.wanted.as_deref() != Some(components) {
            self.wanted = Some(components.to_vec());
        }
    }
}

/// One directory read a worker has taken off the desk, handed back to
/// [`ListingDesk::deliver`] with what the read produced.
pub struct ListingJob<C> {
    client: C,
    target: Vec<String>,
    /// The slot's request count when the read was handed out.
    request: u64,
}

impl<C: Copy> ListingJob<C> {
    /// The consumer this read is for.
    #[must_use]
    pub const fn client(&self) -> C {
        self.client
    }

    /// Root-first components of the directory to read.
    #[must_use]
    pub fn target(&self) -> &[String] {
        &self.target
    }
}

/// One completed read: which directory, and what the read produced.
struct Answer {
    target: Vec<String>,
    result: Result<Vec<Entry>, Errno>,
}

/// The listing arrangement's policy: who has asked for what, what has come
/// back, and which consumer a worker serves next.
///
/// Deliberately free of locks, threads, and syscalls, so every rule is a host
/// test rather than an argument. The embedder supplies the exclusion and the
/// blocking.
pub struct ListingDesk<C: ListingClient> {
    /// One slot per [`ListingClient::ALL`] entry, in that order.
    slots: Vec<Slot>,
    /// The consumer to consider first, rotated after each job is handed out.
    ///
    /// The fairness discipline: round-robin over the consumers, so one walking
    /// a deep tree cannot hold another's pending re-list behind it
    /// indefinitely.
    next: usize,
    /// Set once the embedder is tearing down, so a parked worker leaves instead
    /// of looking for work.
    stopping: bool,
    client: PhantomData<C>,
}

impl<C: ListingClient> Default for ListingDesk<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: ListingClient> ListingDesk<C> {
    /// A desk with nothing asked for and nothing answered, one slot per
    /// declared consumer.
    #[must_use]
    pub fn new() -> Self {
        Self {
            slots: C::ALL.iter().map(|_| Slot::default()).collect(),
            next: 0,
            stopping: false,
            client: PhantomData,
        }
    }

    /// `client`'s slot, or `None` for a value outside the declared set — which
    /// a `Copy + Eq` enumeration listing itself in `ALL` cannot produce.
    fn slot_of(&mut self, client: C) -> Option<&mut Slot> {
        let index = C::ALL.iter().position(|listed| *listed == client)?;
        self.slots.get_mut(index)
    }

    /// Answer `client`'s request for `components`, recording the request if
    /// this desk does not already hold the answer.
    ///
    /// [`Listing::Ready`] once — the answer is *consumed*, because the consumer
    /// has adopted it and a later ask means a genuinely fresh read. An answer
    /// for a different directory is dropped as stale rather than served.
    ///
    /// # Errors
    ///
    /// Whatever the read reported, once, on the same consume-it rule.
    pub fn take(&mut self, client: C, components: &[String]) -> Result<Listing, Errno> {
        let Some(slot) = self.slot_of(client) else {
            return Ok(Listing::Pending);
        };
        match slot.done.take() {
            Some(answer) if answer.target == components => {
                return answer.result.map(Listing::Ready);
            }
            // For somewhere the consumer has since navigated away from: gone.
            Some(_) | None => {}
        }
        slot.want(components);
        Ok(Listing::Pending)
    }

    /// Record `client`'s request for a *fresh* listing of `components`: one
    /// only a read beginning after this call may answer.
    ///
    /// What a consumer asks when it knows the directory may just have changed.
    /// An answer already held, or a read already under way, can describe the
    /// directory as it was before that change, so neither is served; a read
    /// still queued has not begun, so it qualifies and is not read twice.
    pub fn refresh(&mut self, client: C, components: &[String]) {
        let Some(slot) = self.slot_of(client) else {
            return;
        };
        slot.done = None;
        slot.want(components);
        slot.request = slot.request.wrapping_add(1);
    }

    /// Whether any consumer has an unanswered request no worker has taken.
    #[must_use]
    pub fn has_work(&self) -> bool {
        !self.stopping
            && self
                .slots
                .iter()
                .any(|slot| slot.wanted.is_some() && !slot.reading)
    }

    /// Take the next directory to read, or `None` when there is nothing to do.
    ///
    /// Round-robin from wherever the last hand-out left the cursor, so two
    /// consumers asking continuously each get every other read.
    pub fn next_job(&mut self) -> Option<ListingJob<C>> {
        if self.stopping {
            return None;
        }
        let count = self.slots.len();
        for step in 0..count {
            let index = (self.next + step) % count;
            let (Some(client), Some(slot)) = (C::ALL.get(index), self.slots.get_mut(index)) else {
                continue;
            };
            if slot.reading {
                continue;
            }
            if let Some(target) = slot.wanted.clone() {
                slot.reading = true;
                self.next = (index + 1) % count;
                return Some(ListingJob {
                    client: *client,
                    target,
                    request: slot.request,
                });
            }
        }
        None
    }

    /// Record what the read `job` produced.
    ///
    /// Answers `false` — and keeps nothing — when the consumer has since asked
    /// for somewhere else, or for a fresh listing after this read began. The
    /// first is a directory nobody is looking at; the second may predate
    /// whatever made the consumer look again, so its request stands and the
    /// next job reads the directory anew. The caller uses the answer to decide
    /// whether a wake is owed at all.
    ///
    /// An accepted answer **clears the request it answers**. Leaving it standing
    /// made the slot workable again the instant it was answered, so a worker
    /// handed itself the same directory forever and woke the embedder on every
    /// completion — a read loop at whatever rate the disk allowed.
    pub fn deliver(&mut self, job: ListingJob<C>, result: Result<Vec<Entry>, Errno>) -> bool {
        let Some(slot) = self.slot_of(job.client) else {
            return false;
        };
        slot.reading = false;
        if slot.wanted.as_deref() != Some(job.target.as_slice()) || slot.request != job.request {
            return false;
        }
        slot.wanted = None;
        slot.done = Some(Answer {
            target: job.target,
            result,
        });
        true
    }

    /// Stop handing out work, so a parked worker leaves its loop.
    pub fn stop(&mut self) {
        self.stopping = true;
    }

    /// Whether the embedder has asked workers to leave.
    #[must_use]
    pub const fn stopping(&self) -> bool {
        self.stopping
    }
}

#[cfg(test)]
#[path = "desk_tests.rs"]
mod tests;
