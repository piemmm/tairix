//! The deferred-decode desk: what a draw site has asked for, what a producer
//! is running, and what has come back (`plans/FIX-DESKTOP.md` DESK-8).
//!
//! Resolving one icon costs a bounded read plus a round trip to the parser
//! sandbox that decodes it. Performed inside a paint, that stalls whatever the
//! paint is on — the desktop's compositor and seat drain, or a file manager's
//! own input — for as long as the disk and the worker take, once per icon. So
//! the decode is *recorded* here, the draw takes the built-in glyph for the
//! frame it is not ready in, and the pixels are collected when they land.
//!
//! [`ArtworkDesk`] is the whole policy and holds no lock, thread, or syscall,
//! so every rule below is a host test rather than an argument. The desktop
//! session and the file manager each park a worker thread on it behind the
//! runtime's futex mutex. When to wake the loop is part of the policy too —
//! [`deliver`](ArtworkDesk::deliver) answers it — so neither embedder keeps
//! its own count of what it still owes.
//!
//! # What the desk remembers, and for how long
//!
//! An answer handed over is *forgotten*: the cache that collected it owns it,
//! and if the cache later drops it the next paint's miss is a genuine one that
//! must be produced again. Remembering "already answered" instead would leave
//! an evicted icon drawing its glyph until unrelated input arrived, because
//! nothing else would ever ask for it.
//!
//! The decode cache is budgeted, though, so it can be asked to hold more than
//! it will, and a decode it *refuses* must not be offered again — the repaint
//! its landing drove would ask, the answer would be refused again, and every
//! icon on screen would be read and decoded every frame, precisely when the
//! machine is short of the memory that would have held them. The cache says so
//! ([`ArtworkResolver::declined`]) and the key is then held back until
//! [`ArtworkDesk::retry_declined`] offers it again, on the wake of the
//! pressure band that refused it.

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};

use tairix_reclaim::CachedBytes;

use crate::artwork::{ArtworkKey, ArtworkResolver, IconRequest, Resolved};
use crate::picture::Artwork;

/// One decode: what to resolve, and the pixel side to resolve it at.
///
/// The pair is the cache's own key, so a producer yields exactly the slot the
/// draw site missed on — a scale change asks for a different side and is a
/// different job, never a resized copy of this one.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ArtworkJob {
    /// The asset or bundle to resolve.
    pub key: ArtworkKey,
    /// The pixel side to rasterise it at.
    pub side: u32,
}

/// The most thumbnails waiting for a producer at once.
///
/// A containment bound, not a capacity: a sweep withdraws every thumbnail no
/// surface asked for since the last, so this bounds only what scrolling can
/// queue between two sweeps. Past it the oldest is withdrawn, and asked for
/// again by the next paint that still shows it.
pub const MAX_WANTED_THUMBNAILS: usize = 1024;

/// One job's slot: where it has got to, and whether a surface asked for it
/// since the last [`sweep`](ArtworkDesk::sweep_thumbnails).
struct Slot {
    state: State,
    asked: bool,
}

/// Where one job has got to.
enum State {
    /// Recorded, and no producer has taken it.
    Wanted,
    /// A producer is running it.
    Running,
    /// Produced and waiting to be collected. `None` is a refusal — an absent,
    /// over-long, or undecodable asset — which the cache retains just as it
    /// retains artwork.
    Done(Option<Artwork>),
    /// Collected, and the cache could not keep it: no room the current
    /// pressure band allows. Held rather than forgotten, because decoding it
    /// again would only be refused again.
    Declined,
}

/// What recording one decode did: whether its answer was kept, and whether the
/// embedder's loop is owed a wake now.
///
/// Two answers rather than one, because they are independent. A kept delivery
/// owes no wake while the rest of its batch is still queued — that is the whole
/// point of batching them. And a wake can fall due on a delivery that was *not*
/// kept: a job the desk had already answered, handed back after the batch it
/// belonged to drained, still leaves those earlier answers unshown. A single
/// flag cannot say both, and a producer that read "kept" as "wake now" would
/// repaint per icon while one that read "wake now" as "kept" would think a
/// stopped desk had accepted its work.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Delivered {
    kept: bool,
    wake: bool,
}

impl Delivered {
    /// Whether the desk kept this answer, so something new is there to draw.
    #[must_use]
    pub const fn kept(self) -> bool {
        self.kept
    }

    /// Whether the embedder's loop is owed a wake now.
    #[must_use]
    pub const fn wake(self) -> bool {
        self.wake
    }
}

/// Which decodes have come back since the embedder last asked.
///
/// Naming them rather than answering a bare "something landed" is what lets a
/// surface repaint the items the batch actually changed: the alternative is
/// every surface that draws any artwork repainting whole for a batch that
/// moved one slot, which on the measured desktop was a full-width icon bar
/// and a full launcher popup per delivered batch.
///
/// A refusal lands like any other answer: the decode was run, so an item that
/// resolves through it is offered the tier below rather than left as it was.
#[derive(Debug)]
pub struct Landed {
    jobs: BTreeSet<ArtworkJob>,
}

impl Landed {
    /// Whether nothing landed, so a wake that delivered nothing costs no
    /// frame.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Whether `request` at `side` draws through one of these decodes.
    ///
    /// **Every** tier is asked, not just the one that answers today: a
    /// request whose own icon is still being produced draws its class
    /// artwork, so the class artwork landing is what moved its pixels. The
    /// converse — a lower tier landing while a higher one already serves —
    /// can only cost one item a repaint that changes nothing, which is the
    /// direction a missed latch is not.
    #[must_use]
    pub fn resolves(&self, request: IconRequest<'_>, side: u32) -> bool {
        request.tiers().any(|tier| {
            self.jobs.contains(&ArtworkJob {
                key: tier.cache_key(),
                side,
            })
        })
    }
}

/// What has been asked for, what is being produced, and what has come back.
///
/// The embedder supplies the exclusion and the blocking; nothing here waits.
pub struct ArtworkDesk {
    /// Every job this desk knows about, indexed for an O(log n) collect —
    /// a paint asks once per icon it draws, so the lookup is on the frame path.
    slots: BTreeMap<ArtworkJob, Slot>,
    /// The order [`State::Wanted`] jobs are handed out in: first asked, first
    /// decoded, so a busy surface cannot indefinitely displace a quiet one's
    /// single icon.
    queue: VecDeque<ArtworkJob>,
    /// Thumbnails, in the same order but handed out apart: a picture of a file
    /// costs a whole file's read, and its class picture already stands in for
    /// it, so no icon waits behind one.
    thumbnails: VecDeque<ArtworkJob>,
    /// What has been delivered since the embedder last asked. Bounded by the
    /// decodes in flight, and drained on the wake each batch owes.
    landed: BTreeSet<ArtworkJob>,
    /// Whether a delivery still owes the embedder's loop a wake. Distinct
    /// from `landed`, which the loop itself consumes: this is the producer's
    /// debt, and it survives a delivery made while more work was queued.
    wake_owed: bool,
    /// Set once the embedder is tearing down, so a parked producer leaves
    /// instead of looking for work and no further decode is recorded.
    stopping: bool,
}

impl ArtworkDesk {
    /// A desk with nothing asked for and nothing answered.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: BTreeMap::new(),
            queue: VecDeque::new(),
            thumbnails: VecDeque::new(),
            landed: BTreeSet::new(),
            wake_owed: false,
            stopping: false,
        }
    }

    /// Answer a draw site's miss on `key` at `side`, recording the decode if
    /// this desk has neither run it nor been asked for it already.
    ///
    /// [`Resolved::Done`] hands the answer *over* — the caller is the cache
    /// that will retain it — and the slot goes with it, so a later miss on the
    /// same key is a genuine one and is produced again. Every other state is
    /// [`Resolved::Pending`]: the draw takes the tier below it, which for the
    /// last tier is the built-in glyph.
    ///
    /// A desk that is stopping records nothing: there is no producer left to
    /// answer it.
    pub fn collect(&mut self, key: &ArtworkKey, side: u32) -> Resolved {
        let job = ArtworkJob {
            key: key.clone(),
            side,
        };
        match self.slots.get_mut(&job) {
            Some(Slot {
                state: State::Done(artwork),
                ..
            }) => {
                let artwork = artwork.take();
                self.slots.remove(&job);
                Resolved::Done(artwork)
            }
            Some(slot) => {
                slot.asked = true;
                Resolved::Pending
            }
            None => {
                if !self.stopping {
                    self.enqueue(job);
                }
                Resolved::Pending
            }
        }
    }

    /// Record `job` as wanted, behind the others of its kind.
    fn enqueue(&mut self, job: ArtworkJob) {
        if is_thumbnail(&job) {
            if self.thumbnails.len() >= MAX_WANTED_THUMBNAILS {
                self.withdraw_oldest_thumbnail();
            }
            self.thumbnails.push_back(job.clone());
        } else {
            self.queue.push_back(job.clone());
        }
        self.slots.insert(
            job,
            Slot {
                state: State::Wanted,
                asked: true,
            },
        );
    }

    /// Forget the longest-waiting thumbnail still wanted.
    fn withdraw_oldest_thumbnail(&mut self) {
        while let Some(job) = self.thumbnails.pop_front() {
            if matches!(
                self.slots.get(&job),
                Some(Slot {
                    state: State::Wanted,
                    ..
                })
            ) {
                self.slots.remove(&job);
                return;
            }
        }
    }

    /// Withdraw every thumbnail no surface asked for since the last sweep —
    /// still wanted, or produced and never collected — wiping what was drawn.
    ///
    /// Run only after a pass over **every** surface that draws thumbnails: one
    /// a pass skipped would lose its still-visible pictures with nothing to ask
    /// for them again. A thumbnail in production is left to land and goes at
    /// the next sweep if nothing collects it.
    pub fn sweep_thumbnails(&mut self) {
        self.slots.retain(|job, slot| {
            let unasked = !core::mem::replace(&mut slot.asked, false);
            let withdrawn = unasked
                && is_thumbnail(job)
                && matches!(slot.state, State::Wanted | State::Done(_));
            if withdrawn {
                if let State::Done(Some(artwork)) = &mut slot.state {
                    artwork.wipe();
                }
            }
            !withdrawn
        });
        let slots = &self.slots;
        self.thumbnails.retain(|job| {
            matches!(
                slots.get(job),
                Some(Slot {
                    state: State::Wanted,
                    ..
                })
            )
        });
    }

    /// Record `key` at `side` as wanted, without collecting anything.
    ///
    /// The prefetch half of [`collect`](Self::collect): a surface that knows
    /// what it is *about* to draw asks now, so the decode finishes before the
    /// frame that needs it rather than a round trip per icon after it.
    ///
    /// A key this desk already knows is left exactly as it is, so a prefetch
    /// can never consume an answer a draw is about to collect, nor queue a
    /// second decode for one already in flight.
    pub fn want(&mut self, key: &ArtworkKey, side: u32) {
        if self.stopping {
            return;
        }
        let job = ArtworkJob {
            key: key.clone(),
            side,
        };
        match self.slots.get_mut(&job) {
            Some(slot) => slot.asked = true,
            None => self.enqueue(job),
        }
    }

    /// Whether any recorded decode is waiting for a producer to take it.
    #[must_use]
    pub fn has_work(&self) -> bool {
        !self.stopping && (!self.queue.is_empty() || !self.thumbnails.is_empty())
    }

    /// Take the next icon decode to run, or `None` when there is none.
    pub fn next_job(&mut self) -> Option<ArtworkJob> {
        if self.stopping {
            return None;
        }
        Self::take_wanted(&mut self.queue, &mut self.slots)
    }

    /// Take the next thumbnail to run, or `None` when there is none.
    ///
    /// Apart from [`next_job`](Self::next_job) so the producer decides what
    /// comes between: every icon first, and in a program whose one worker
    /// also reads folders, their cues too.
    pub fn next_thumbnail(&mut self) -> Option<ArtworkJob> {
        if self.stopping {
            return None;
        }
        Self::take_wanted(&mut self.thumbnails, &mut self.slots)
    }

    /// The first job in `queue` still wanted, marked running.
    ///
    /// The queue is only the hand-out *order*; the slots are the authority on
    /// whether a job is still wanted. Deciding that here rather than scanning
    /// the queue whenever a slot changes keeps a paint from paying for the
    /// producer's bookkeeping, and taking the next entry rather than giving up
    /// means a job that somehow lost its slot costs one decode not started,
    /// never a producer that stops taking work.
    fn take_wanted(
        queue: &mut VecDeque<ArtworkJob>,
        slots: &mut BTreeMap<ArtworkJob, Slot>,
    ) -> Option<ArtworkJob> {
        while let Some(job) = queue.pop_front() {
            if let Some(Slot {
                state: state @ State::Wanted,
                ..
            }) = slots.get_mut(&job)
            {
                *state = State::Running;
                return Some(job);
            }
        }
        None
    }

    /// Record what decoding `job` produced.
    ///
    /// A wake is owed once something has been delivered *and* no further icon
    /// is waiting to be handed out. Waking on the drained batch rather than on
    /// each icon costs a folder of fifty bundles one repaint instead of fifty,
    /// and a lone icon empties the queue at once so it still lands the moment
    /// it is ready. A thumbnail is shown as it lands instead: it costs a whole
    /// file's read and decode, far more than the repaint that shows it. The
    /// debt outlives the delivery that incurred it, so a batch drained without
    /// a wake cannot be stranded by a final job the desk no longer wants.
    pub fn deliver(&mut self, job: &ArtworkJob, artwork: Option<Artwork>) -> Delivered {
        let mut kept = false;
        if let Some(Slot {
            state: state @ State::Running,
            ..
        }) = self.slots.get_mut(job)
        {
            *state = State::Done(artwork);
            self.landed.insert(job.clone());
            self.wake_owed = true;
            kept = true;
        }
        let drained = self.stopping || self.queue.is_empty();
        let wake = self.wake_owed && (drained || is_thumbnail(job));
        self.wake_owed &= !wake;
        Delivered { kept, wake }
    }

    /// What has been delivered since this was last asked, clearing the record.
    ///
    /// The embedder repaints the items these answers changed, so the surfaces
    /// that drew a glyph for want of pixels draw the pixels — and a wake that
    /// delivered nothing costs no frame.
    pub fn take_landed(&mut self) -> Landed {
        Landed {
            jobs: core::mem::take(&mut self.landed),
        }
    }

    /// Note that the cache could not keep what `job` produced, so this desk
    /// stops offering it until the band that refused it moves.
    ///
    /// Without this the refusal is silent and self-renewing: the repaint the
    /// landing drove asks again, the answer is refused again, and every icon on
    /// screen is read and decoded on every repaint — precisely when the machine
    /// is short of the memory that would have held the answer.
    ///
    /// The [`collect`](Self::collect) that handed the answer over has already
    /// forgotten the key, so this is normally an insert. A key a producer is
    /// midway through is left alone: that decode has yet to be answered, and
    /// the refusal will be repeated on the collect that answers it.
    pub fn decline(&mut self, key: &ArtworkKey, side: u32) {
        if self.stopping {
            return;
        }
        let job = ArtworkJob {
            key: key.clone(),
            side,
        };
        match self.slots.get_mut(&job) {
            Some(Slot {
                state: State::Running,
                ..
            }) => {}
            Some(slot) => slot.state = State::Declined,
            None => {
                self.slots.insert(
                    job,
                    Slot {
                        state: State::Declined,
                        asked: true,
                    },
                );
            }
        }
    }

    /// Offer every declined key again, because the pressure band moved and the
    /// answer may now be retainable.
    pub fn retry_declined(&mut self) {
        self.slots
            .retain(|_, slot| !matches!(slot.state, State::Declined));
    }

    /// Stop handing out work, so a parked producer leaves its loop.
    ///
    /// Every decode still held is overwritten before it is dropped, on the same
    /// terms the artwork cache wipes its own: one user's rendered pixels do not
    /// outlive their session in reusable heap.
    pub fn stop(&mut self) {
        self.stopping = true;
        // Nothing is left to repaint, so a producer's outstanding wake debt —
        // and the batch it would have shown — dies with the answers.
        self.wake_owed = false;
        self.landed.clear();
        for slot in self.slots.values_mut() {
            if let State::Done(Some(artwork)) = &mut slot.state {
                artwork.wipe();
            }
        }
        self.slots.clear();
        self.queue.clear();
        self.thumbnails.clear();
    }

    /// Whether the embedder has asked producers to leave.
    #[must_use]
    pub const fn stopping(&self) -> bool {
        self.stopping
    }
}

/// Whether `job` reads picture files' own content rather than an icon.
fn is_thumbnail(job: &ArtworkJob) -> bool {
    job.key.is_thumbnail_class()
}

impl Default for ArtworkDesk {
    fn default() -> Self {
        Self::new()
    }
}

/// The desk *is* the deferring resolver: it answers what a producer has
/// already delivered and records everything else.
///
/// An embedder that owns the desk outright hands the cache a plain `&mut` to
/// it and needs no wrapper. One that shares the desk with a worker thread
/// implements the trait over its own mutex instead, so the notify happens
/// inside the same critical section as the state change.
impl ArtworkResolver for ArtworkDesk {
    fn resolve(&mut self, key: &ArtworkKey, side: u32) -> Resolved {
        self.collect(key, side)
    }

    fn prefetch(&mut self, key: &ArtworkKey, side: u32) {
        self.want(key, side);
    }

    fn declined(&mut self, key: &ArtworkKey, side: u32) {
        self.decline(key, side);
    }
}

#[cfg(test)]
#[path = "desk_tests.rs"]
mod tests;
