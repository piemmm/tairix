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
//! session and the file manager each park workers on it behind the runtime's
//! futex mutex. When to wake the loop is part of the policy too —
//! [`deliver`](ArtworkDesk::deliver) answers it — so neither embedder keeps
//! its own count of what it still owes.
//!
//! # Shares
//!
//! An embedder whose workers each serve their own share of the decodes — the
//! file manager's and the desktop session's, one per volume — names each
//! decode's share when a worker first looks for work, once per decode, and is
//! handed and answered for that share alone
//! ([`next_job_where`](ArtworkDesk::next_job_where)), so a worker held up on
//! one device holds up no other share's decodes or wake.
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
//!
//! # Listed bundles
//!
//! A bundle a listing names is pictured from two files of its own, its
//! manifest and the icon the manifest names, which a copy may still be
//! writing when the bundle first shows. The desk remembers, per bundle and
//! version listed, the files its producer last read it from and keys its
//! picture by them ([`ArtworkDesk::version_of`]); until they are read, by what
//! was listed, whose decode reads them ([`ArtworkDesk::deliver_listed`]). An
//! embedder that follows those files' changes reports each
//! ([`ArtworkDesk::moved`]) and the bundle is read again, so one still being
//! written when it was first read is pictured from its finished files, and one
//! whose files stand is not decoded twice. A bundle let go while off screen is
//! followed again once it is shown again, its files read anew. Every producer
//! makes a decode the one way ([`ArtworkJob::produce`]), and the desk records
//! what it made ([`ArtworkDesk::deliver_produced`]).
//!
//! The listed bundles are remembered by directory, the directory asked about
//! least recently forgotten first with every version of it, in a map whose
//! recency touch costs constant time. A user chooses those paths, so the map
//! is keyed under the embedder's key ([`ArtworkDesk::new`]).
//!
//! # An exhausted heap
//!
//! Nothing an ask, a collect or a delivery records grows infallibly: what the
//! heap refuses goes unrecorded and is asked for again by the next paint, and
//! a landing it has no room to name is reported as everything having landed
//! ([`Landed::resolves`]), so no item waits on it unrepainted.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::fs::DocumentStamp;
use tairix_collections::{LruMap, OrderedMap, OrderedSet};
use tairix_hash::BuildSipHash13;
use tairix_reclaim::CachedBytes;
use tairix_util::fallible;

use crate::artwork::{
    is_at_or_under, render_artwork, ArtworkKey, ArtworkRasteriser, ArtworkReader, ArtworkResolver,
    BundleFiles, BundleVersion, IconRequest, ListedBundle, Resolved,
};
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

impl ArtworkJob {
    /// This job copied, `None` where the heap has no room for its key's text.
    fn try_clone(&self) -> Option<Self> {
        Some(Self {
            key: self.key.try_clone()?,
            side: self.side,
        })
    }

    /// Produce this decode with no lock held, as every producer does: a listed
    /// bundle's files read through `files` first, and its picture decoded only
    /// where they differ from `last`, the files it was last read from; any
    /// other decode made outright. The decode is [`render_artwork`]'s, so
    /// which producer ran it cannot change what it made.
    #[must_use]
    pub fn produce<R, D>(
        &self,
        (reader, rasteriser): (&mut R, &mut D),
        files: impl FnOnce(&ListedBundle, &mut R) -> BundleFiles,
        last: impl FnOnce(&ListedBundle) -> Option<BundleFiles>,
    ) -> Produced
    where
        R: ArtworkReader + ?Sized,
        D: ArtworkRasteriser + ?Sized,
    {
        let Some(bundle) = self.key.listed_bundle() else {
            return Produced::Decoded(render_artwork(reader, rasteriser, &self.key, self.side));
        };
        let read = files(&bundle, &mut *reader);
        if last(&bundle) == Some(read) {
            return Produced::Unchanged;
        }
        let decoded = render_artwork(reader, rasteriser, &self.key, self.side);
        Produced::Listed(read, decoded)
    }
}

/// What a producer made of one decode ([`ArtworkJob::produce`]), for its desk
/// to record ([`ArtworkDesk::deliver_produced`]).
#[derive(Debug)]
pub enum Produced {
    /// The decode of anything but a listed bundle's own picture, `None` where
    /// it was refused.
    Decoded(Option<Artwork>),
    /// A listed bundle's files, found as they were when its picture was last
    /// read from them: nothing was decoded.
    Unchanged,
    /// A listed bundle's files as read, and the decode they made, `None` where
    /// it was refused.
    Listed(BundleFiles, Option<Artwork>),
}

/// The most thumbnails waiting for a producer at once.
///
/// A containment bound, not a capacity: a sweep withdraws every thumbnail no
/// surface asked for since the last, so this bounds only what scrolling can
/// queue between two sweeps. Past it the oldest is withdrawn, and asked for
/// again by the next paint that still shows it.
pub const MAX_WANTED_THUMBNAILS: usize = 1024;

/// The most bundle directories a desk remembers the files of.
///
/// A containment bound, not a capacity: one past it forgets the directory
/// asked about least recently, with every version of it, whose files are read
/// again when it is next shown.
pub const MAX_LISTED_BUNDLE_DIRS: usize = 1024;

/// The most versions of one bundle directory a desk remembers.
///
/// A containment bound, not a capacity: a directory is listed at more than
/// one version only while one surface's listing lags another's or a copy
/// rewrites it, so past this the version asked about least recently goes.
pub const MAX_LISTED_VERSIONS: usize = 4;

/// The most listed bundles a desk asks to have followed at once.
///
/// A containment bound on the change waits a folder of untrusted bundles can
/// make a surface hold: one past it is read again only when its listing moves,
/// until a sweep makes room and it is next asked about.
pub const MAX_FOLLOWED_BUNDLES: usize = 64;

/// The key a test desk indexes its listed bundles under.
#[cfg(test)]
pub(crate) const TEST_KEYS: BuildSipHash13 = BuildSipHash13::with_seed(
    tairix_hash::HashSeed::from_bytes([7; tairix_hash::HashSeed::LEN]),
);

/// What a desk remembers of one listed bundle.
struct Record {
    /// The version its listing named.
    listed: DocumentStamp,
    /// The files its picture was last read from.
    files: Option<BundleFiles>,
    /// The side it was last asked about at, which a read again is made at.
    side: u32,
    /// Whether a surface asked about it since the last sweep.
    asked: bool,
    /// Whether its files are followed.
    follow: Follow,
    /// Its files moved while a read of them was under way.
    moved: bool,
}

/// Whether a listed bundle's files are followed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Follow {
    /// Not followed, and followed again once asked about with room.
    No,
    /// To be followed from its next read.
    Wanted,
    /// Followed.
    Yes,
    /// Never to be followed: a wait on its files was refused, so a change to
    /// them shows once its listing moves.
    Refused,
}

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
    /// Collected, and the cache could not keep its `bytes`: no room the
    /// current pressure band allows. Held rather than forgotten, because
    /// decoding it again would only be refused again.
    Declined(usize),
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
/// So does a tree whose writing ended ([`ArtworkDesk::release_under`]): what
/// was drawn from inside it while it was held draws the tier below, and asks
/// again now. A request's stand-in is asked about with its tiers
/// ([`IconRequest::standing_in`]): its landing changes what the request draws
/// while its own is produced. A landing the heap had no room to name is
/// reported as everything having landed, so every item repaints and asks
/// again rather than one waiting on it unrepainted.
#[derive(Debug)]
pub struct Landed {
    jobs: OrderedSet<ArtworkJob>,
    released: Vec<String>,
    everything: bool,
}

impl Landed {
    const fn new() -> Self {
        Self {
            jobs: OrderedSet::new(),
            released: Vec::new(),
            everything: false,
        }
    }

    /// Name `job`; with no room to name it, everything landed.
    fn name(&mut self, job: Option<ArtworkJob>) {
        let named = job.is_some_and(|job| self.jobs.try_insert(job).is_ok());
        self.everything |= !named;
    }

    /// Name `tree` released; with no room to name it, everything landed.
    fn release(&mut self, tree: String) {
        self.everything |= !fallible::push(&mut self.released, tree);
    }

    /// Whether nothing landed, so a wake that delivered nothing costs no
    /// frame.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.everything && self.jobs.is_empty() && self.released.is_empty()
    }

    /// Whether `request` at `side` draws through one of these decodes, or
    /// from inside a released tree.
    ///
    /// **Every** tier is asked, not just the one that answers today: a
    /// request whose own icon is still being produced draws its class
    /// artwork, so the class artwork landing is what moved its pixels. The
    /// converse — a lower tier landing while a higher one already serves —
    /// can only cost one item a repaint that changes nothing, which is the
    /// direction a missed latch is not. A landing the heap had no room to name
    /// resolves every request.
    #[must_use]
    pub fn resolves(&self, request: IconRequest<'_>, side: u32) -> bool {
        if self.everything {
            return true;
        }
        request.tiers().chain(request.stand_in_tier()).any(|tier| {
            self.jobs
                .iter()
                .any(|job| job.side == side && tier.names(&job.key))
                || self.released.iter().any(|tree| tier.named_under(tree))
        })
    }
}

/// What has been asked for, what is being produced, and what has come back,
/// the decodes waiting kept apart by the share of workers `S` that takes them.
///
/// The embedder supplies the exclusion and the blocking; nothing here waits.
pub struct ArtworkDesk<S = ()> {
    /// Every job this desk knows about, indexed for an O(log n) collect —
    /// a paint asks once per icon it draws, so the lookup is on the frame path.
    slots: OrderedMap<ArtworkJob, Slot>,
    /// The order [`State::Wanted`] jobs are handed out in: first asked, first
    /// decoded, so a busy surface cannot indefinitely displace a quiet one's
    /// single icon.
    queue: Order<S>,
    /// Thumbnails, in the same order but handed out apart: a picture of a file
    /// costs a whole file's read, and its class picture already stands in for
    /// it, so no icon waits behind one.
    thumbnails: Order<S>,
    /// What has been delivered and the trees released since the embedder
    /// last asked. Bounded by the decodes in flight and the trees held, and
    /// drained on the wake each batch owes.
    landed: Landed,
    /// Whether a delivery still owes the embedder's loop a wake. Distinct
    /// from `landed`, which the loop itself consumes: this is the producer's
    /// debt, and it survives a delivery made while more work was queued.
    wake_owed: bool,
    /// The trees still being written, under which nothing is produced.
    held: Vec<String>,
    /// The listed bundles asked about, by directory, each at every version
    /// listed, least recently asked about first both across directories and
    /// within one.
    bundles: LruMap<String, Vec<Record>, BuildSipHash13>,
    /// Those followed or wanted followed, at most [`MAX_FOLLOWED_BUNDLES`]:
    /// all a sweep walks.
    following: Vec<ListedBundle>,
    /// The followed bundles forgotten since the embedder last asked.
    unfollowed: Vec<ListedBundle>,
    /// Whether the embedder follows the files of the bundles asked about.
    follows: bool,
    /// Set once the embedder is tearing down, so a parked producer leaves
    /// instead of looking for work and no further decode is recorded.
    stopping: bool,
}

impl<S> ArtworkDesk<S> {
    /// A desk with nothing asked for and nothing answered, whose listed
    /// bundles are indexed under `keys`: their directories are paths a user
    /// chooses.
    #[must_use]
    pub const fn new(keys: BuildSipHash13) -> Self {
        Self {
            slots: OrderedMap::new(),
            queue: Order::new(),
            thumbnails: Order::new(),
            landed: Landed::new(),
            wake_owed: false,
            held: Vec::new(),
            bundles: LruMap::with_hasher(keys),
            following: Vec::new(),
            unfollowed: Vec::new(),
            follows: false,
            stopping: false,
        }
    }
}

impl<S: Clone + Eq> ArtworkDesk<S> {
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
    /// answer it. Answers too whether this call recorded the decode, the one
    /// ask a worker is owed a wake for. A key the heap has no room to copy is
    /// answered pending, recording nothing.
    pub fn collect(&mut self, key: &ArtworkKey, side: u32) -> (Resolved, bool) {
        let Some(key) = key.try_clone() else {
            return (Resolved::Pending, false);
        };
        let job = ArtworkJob { key, side };
        match self.slots.get_mut(&job) {
            Some(Slot {
                state: State::Done(artwork),
                ..
            }) => {
                let artwork = artwork.take();
                self.slots.remove(&job);
                (Resolved::Done(artwork), false)
            }
            Some(slot) => {
                slot.asked = true;
                (Resolved::Pending, false)
            }
            None => (Resolved::Pending, !self.stopping && self.enqueue(job)),
        }
    }

    /// Record `job` as wanted, behind the others of its kind, answering
    /// whether it was — not where what it pictures is still being written,
    /// whose release is what makes the item ask again, nor where the heap has
    /// no room for it, which the next paint asks again.
    fn enqueue(&mut self, job: ArtworkJob) -> bool {
        if self.held.iter().any(|tree| job.key.is_named_under(tree)) {
            return false;
        }
        let Some(queued) = job.try_clone() else {
            return false;
        };
        let thumbnail = is_thumbnail(&job);
        if thumbnail && self.thumbnails.len() >= MAX_WANTED_THUMBNAILS {
            if let Some(oldest) = self.thumbnails.pop_oldest(&self.slots) {
                self.slots.remove(&oldest);
            }
        }
        let wanted = Slot {
            state: State::Wanted,
            asked: true,
        };
        let order = if thumbnail {
            &mut self.thumbnails
        } else {
            &mut self.queue
        };
        if !order.reserve_one() || self.slots.try_insert(job, wanted).is_err() {
            return false;
        }
        order.push(queued);
        true
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
        self.thumbnails.retain(|job| is_wanted(slots, job));
    }

    /// Record `key` at `side` as wanted, without collecting anything.
    ///
    /// The prefetch half of [`collect`](Self::collect): a surface that knows
    /// what it is *about* to draw asks now, so the decode finishes before the
    /// frame that needs it rather than a round trip per icon after it.
    ///
    /// A key this desk already knows is left exactly as it is, so a prefetch
    /// can never consume an answer a draw is about to collect, nor queue a
    /// second decode for one already in flight. Answers whether it recorded
    /// the decode.
    pub fn want(&mut self, key: &ArtworkKey, side: u32) -> bool {
        if self.stopping {
            return false;
        }
        let Some(key) = key.try_clone() else {
            return false;
        };
        let job = ArtworkJob { key, side };
        match self.slots.get_mut(&job) {
            Some(slot) => {
                slot.asked = true;
                false
            }
            None => self.enqueue(job),
        }
    }

    /// Note that a surface still shows what `key` at `side` is for, so the
    /// next [`sweep`](Self::sweep_thumbnails) keeps it — recording nothing
    /// this desk does not already hold.
    ///
    /// What a surface that repainted only part of itself asks about the rest
    /// before a sweep. [`want`](Self::want) would serve that too, but it also
    /// queues a decode for a picture the cache has since evicted, though the
    /// surface already shows it; asking for every shown picture that way lets
    /// a working set larger than the cache's budget decode itself in a loop.
    pub fn keep(&mut self, key: &ArtworkKey, side: u32) {
        let Some(key) = key.try_clone() else {
            return;
        };
        let job = ArtworkJob { key, side };
        if let Some(slot) = self.slots.get_mut(&job) {
            slot.asked = true;
        }
    }

    /// Whether any recorded decode is waiting for a producer to take it.
    #[must_use]
    pub fn has_work(&self) -> bool {
        !self.stopping && (!self.queue.is_empty() || !self.thumbnails.is_empty())
    }

    /// Take the oldest icon decode of a share `mine` admits, with its share:
    /// what a producer takes when producers each serve their own share. A
    /// decode recorded since the last take or delivery is first given its
    /// share by `route`.
    pub fn next_job_where(
        &mut self,
        route: impl FnMut(&ArtworkJob) -> S,
        mine: impl FnMut(&S) -> bool,
    ) -> Option<(S, ArtworkJob)> {
        if self.stopping {
            return None;
        }
        Self::take_wanted(&mut self.queue, &mut self.slots, route, mine)
    }

    /// Take the oldest thumbnail of a share `mine` admits, as
    /// [`next_job_where`](Self::next_job_where) takes an icon.
    ///
    /// Apart from icons so the producer decides what comes between: every
    /// icon first, and in a program whose workers also read folders, their
    /// cues too.
    pub fn next_thumbnail_where(
        &mut self,
        route: impl FnMut(&ArtworkJob) -> S,
        mine: impl FnMut(&S) -> bool,
    ) -> Option<(S, ArtworkJob)> {
        if self.stopping {
            return None;
        }
        Self::take_wanted(&mut self.thumbnails, &mut self.slots, route, mine)
    }

    /// The oldest job in `order` still wanted of a share `mine` admits,
    /// marked running.
    fn take_wanted(
        order: &mut Order<S>,
        slots: &mut OrderedMap<ArtworkJob, Slot>,
        route: impl FnMut(&ArtworkJob) -> S,
        mine: impl FnMut(&S) -> bool,
    ) -> Option<(S, ArtworkJob)> {
        let (share, job) = order.take(slots, route, mine)?;
        if let Some(slot) = slots.get_mut(&job) {
            slot.state = State::Running;
        }
        Some((share, job))
    }

    /// Give every decode waiting its share afresh at the next take or
    /// delivery: the embedder's routing changed.
    pub fn reroute(&mut self) {
        self.queue.reroute();
        self.thumbnails.reroute();
    }

    /// Record what decoding `job` produced for a producer serving `share`.
    ///
    /// A wake is owed once something has been delivered *and* no further icon
    /// of `share` is waiting to be handed out, so a share whose producer is
    /// held up holds no other share's wake back. Waking on the drained batch
    /// rather than on each icon costs a folder of fifty bundles one repaint
    /// instead of fifty, and a lone icon empties the queue at once so it still
    /// lands the moment it is ready. A thumbnail is shown as it lands instead:
    /// it costs a whole file's read and decode, far more than the repaint that
    /// shows it. The debt outlives the delivery that incurred it, so a batch
    /// drained without a wake cannot be stranded by a final job the desk no
    /// longer wants. A decode recorded since the last take or delivery is
    /// first given its share by `route`.
    pub fn deliver_where(
        &mut self,
        job: &ArtworkJob,
        artwork: Option<Artwork>,
        route: impl FnMut(&ArtworkJob) -> S,
        share: &S,
    ) -> Delivered {
        let mut kept = false;
        if let Some(Slot {
            state: state @ State::Running,
            ..
        }) = self.slots.get_mut(job)
        {
            *state = State::Done(artwork);
            kept = true;
        }
        if kept {
            self.land(job.try_clone());
        }
        self.delivered(job, kept, route, share)
    }

    /// Record what decoding `job` — a read of a listed bundle's files and of
    /// its picture — produced from them as `files`, for a producer serving
    /// `share`: the picture is kept under the version those files are, which
    /// every later ask about the bundle is keyed by, and a bundle whose files
    /// moved while they were read is read again. As
    /// [`deliver_where`](Self::deliver_where) otherwise.
    pub fn deliver_listed(
        &mut self,
        job: &ArtworkJob,
        artwork: Option<Artwork>,
        files: BundleFiles,
        route: impl FnMut(&ArtworkJob) -> S,
        share: &S,
    ) -> Delivered {
        let running = matches!(
            self.slots.get(job),
            Some(Slot {
                state: State::Running,
                ..
            })
        );
        if running {
            self.slots.remove(job);
        }
        let listed = job.key.listed_dir().filter(|_| running);
        let kept = listed.is_some();
        match listed {
            Some((dir, listed)) => {
                self.file_answer((dir, listed), files, job.side, artwork);
                self.again(dir, listed);
            }
            None => wipe(artwork),
        }
        self.delivered(job, kept, route, share)
    }

    /// `job`, a read of a listed bundle's files, found them as they were last
    /// read, for a producer serving `share`: nothing is decoded and nothing
    /// lands, and a bundle whose files moved meanwhile is read again.
    pub fn unchanged(
        &mut self,
        job: &ArtworkJob,
        route: impl FnMut(&ArtworkJob) -> S,
        share: &S,
    ) -> Delivered {
        if matches!(
            self.slots.get(job),
            Some(Slot {
                state: State::Running,
                ..
            })
        ) {
            self.slots.remove(job);
            if let Some((dir, listed)) = job.key.listed_dir() {
                self.again(dir, listed);
            }
        }
        self.delivered(job, false, route, share)
    }

    /// Record what producing `job` made ([`ArtworkJob::produce`]), for a
    /// producer serving `share`: a listed bundle's picture under the files it
    /// was read from ([`deliver_listed`](Self::deliver_listed)), files found as
    /// they were as nothing ([`unchanged`](Self::unchanged)), and any other
    /// decode as [`deliver_where`](Self::deliver_where) records it.
    pub fn deliver_produced(
        &mut self,
        job: &ArtworkJob,
        produced: Produced,
        route: impl FnMut(&ArtworkJob) -> S,
        share: &S,
    ) -> Delivered {
        match produced {
            Produced::Decoded(artwork) => self.deliver_where(job, artwork, route, share),
            Produced::Unchanged => self.unchanged(job, route, share),
            Produced::Listed(files, artwork) => {
                self.deliver_listed(job, artwork, files, route, share)
            }
        }
    }

    /// What answering `job`, keeping its answer where `kept`, owes the
    /// embedder's loop for a producer serving `share`: a wake once no further
    /// icon of that share waits, and at once for a thumbnail.
    fn delivered(
        &mut self,
        job: &ArtworkJob,
        kept: bool,
        route: impl FnMut(&ArtworkJob) -> S,
        share: &S,
    ) -> Delivered {
        let drained = self.stopping || !self.queue.holds(&self.slots, route, share);
        let wake = self.wake_owed && (drained || is_thumbnail(job));
        self.wake_owed &= !wake;
        Delivered { kept, wake }
    }

    /// Name `job` in the next [`Landed`], owing the loop its wake. A name the
    /// heap had no room for lands as everything, so nothing drawn through it
    /// is left unrepainted.
    fn land(&mut self, job: Option<ArtworkJob>) {
        self.wake_owed = true;
        self.landed.name(job);
    }

    /// Keep `artwork`, what the files of the bundle at `dir`, listed as
    /// `listed`, read as `files` produced at `side`, under the version they
    /// are, and remember them as its files.
    fn file_answer(
        &mut self,
        (dir, listed): (&str, DocumentStamp),
        files: BundleFiles,
        side: u32,
        artwork: Option<Artwork>,
    ) {
        if let Some(record) = self.record_mut(dir, listed) {
            record.files = Some(files);
        }
        // An answer the heap will not name lands as everything: the item asks
        // again, and the version its files are is decoded then.
        let Some(dir) = fallible::concatenated(&[dir]) else {
            wipe(artwork);
            self.wake_owed = true;
            self.landed.name(None);
            return;
        };
        let read = ArtworkJob {
            key: ArtworkKey::Bundle {
                dir,
                version: Some(BundleVersion::Files(files)),
            },
            side,
        };
        match self.slots.get_mut(&read) {
            Some(slot) if matches!(slot.state, State::Wanted) => {
                slot.state = State::Done(artwork);
            }
            Some(_) => wipe(artwork),
            None => {
                let room = self.slots.try_reserve(1).is_ok();
                match read.try_clone().filter(|_| room) {
                    Some(key) => {
                        let done = Slot {
                            state: State::Done(artwork),
                            asked: true,
                        };
                        let _ = self.slots.try_insert(key, done);
                    }
                    None => wipe(artwork),
                }
            }
        }
        self.land(Some(read));
    }

    /// Read the files of the bundle at `dir`, listed as `listed`, again where
    /// they moved while the read just answered was under way.
    fn again(&mut self, dir: &str, listed: DocumentStamp) {
        let Some(record) = self.record_mut(dir, listed) else {
            return;
        };
        if !core::mem::take(&mut record.moved) {
            return;
        }
        let side = record.side;
        let _ = self.read_anew(dir, listed, side);
    }

    /// Record a read of the files of the bundle at `dir`, listed as `listed`,
    /// at `side` — or, while one is under way, another once it is answered —
    /// unless one is already recorded, answering whether this recorded one.
    fn read_anew(&mut self, dir: &str, listed: DocumentStamp, side: u32) -> bool {
        let Some(job) = listed_read(dir, listed, side) else {
            return false;
        };
        let running = self
            .slots
            .get(&job)
            .map(|slot| matches!(slot.state, State::Running));
        match running {
            Some(true) => {
                if let Some(record) = self.record_mut(dir, listed) {
                    record.moved = true;
                }
                false
            }
            Some(false) => false,
            None => self.enqueue(job),
        }
    }

    /// What has been delivered since this was last asked, clearing the record.
    ///
    /// The embedder repaints the items these answers changed, so the surfaces
    /// that drew a glyph for want of pixels draw the pixels — and a wake that
    /// delivered nothing costs no frame.
    pub fn take_landed(&mut self) -> Landed {
        core::mem::replace(&mut self.landed, Landed::new())
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
    /// the refusal will be repeated on the collect that answers it. One the
    /// heap has no room to record goes unrecorded, and is offered again.
    pub fn decline(&mut self, key: &ArtworkKey, side: u32, bytes: usize) {
        if self.stopping {
            return;
        }
        let Some(key) = key.try_clone() else {
            return;
        };
        let job = ArtworkJob { key, side };
        match self.slots.get_mut(&job) {
            Some(Slot {
                state: State::Running,
                ..
            }) => {}
            Some(slot) => slot.state = State::Declined(bytes),
            None => {
                let declined = Slot {
                    state: State::Declined(bytes),
                    asked: true,
                };
                let _ = self.slots.try_insert(job, declined);
            }
        }
    }

    /// Offer again every declined key whose size the cache now `holds`, the
    /// pressure band having moved, answering whether there was one.
    ///
    /// Each is named in the next [`Landed`] as though it had come back: an item
    /// drawing the tier below for want of it is repainted, and asks again —
    /// nothing else would, since what it shows is already on screen. A key the
    /// cache would still refuse stays held: offering it would cost a read and a
    /// decode only to be refused again.
    pub fn retry_declined(&mut self, holds: impl Fn(usize) -> bool) -> bool {
        let landed = &mut self.landed;
        let mut retried = false;
        self.slots.retain(|job, slot| {
            let State::Declined(bytes) = slot.state else {
                return true;
            };
            if !holds(bytes) {
                return true;
            }
            landed.name(job.try_clone());
            retried = true;
            false
        });
        retried
    }

    /// The version the bundle a listing names at `dir`, as `listed`, is
    /// pictured at when asked for at `side`: the files its producer last read
    /// it from, else what was listed, which the decode the picture then asks
    /// for reads them for ([`deliver_listed`](Self::deliver_listed)).
    ///
    /// Notes the ask, so the next sweep keeps the bundle followed
    /// ([`sweep_follows`](Self::sweep_follows)). A bundle not yet remembered
    /// is remembered, followed where the embedder follows bundles
    /// ([`follow_bundles`](Self::follow_bundles)) and the bound leaves room;
    /// one remembered unfollowed is followed again on the same terms, its
    /// files read anew so that read arms the follow, which
    /// [`recorded`](Self::recorded) tells the embedder of. Asking makes its
    /// directory, and this version of it, the most recent.
    pub fn version_of(&mut self, dir: &str, listed: DocumentStamp, side: u32) -> BundleVersion {
        if self.stopping {
            return BundleVersion::Listed(listed);
        }
        let Some(record) = self.touch(dir, listed) else {
            self.remember(dir, listed, side);
            return BundleVersion::Listed(listed);
        };
        record.asked = true;
        record.side = side;
        let unfollowed = record.follow == Follow::No;
        let version = record
            .files
            .map_or(BundleVersion::Listed(listed), BundleVersion::Files);
        if unfollowed && self.may_follow_more() {
            self.follow_again(dir, listed, side);
        }
        version
    }

    /// Whether a bundle asked about now may be followed: the embedder follows
    /// bundles and the bound leaves room.
    fn may_follow_more(&self) -> bool {
        self.follows && self.following.len() < MAX_FOLLOWED_BUNDLES
    }

    /// Follow the remembered bundle at `dir`, listed as `listed`, again: its
    /// files are read anew at `side`, so that read arms the follow. One the
    /// heap will not name stays unfollowed.
    fn follow_again(&mut self, dir: &str, listed: DocumentStamp, side: u32) {
        let Some(name) = fallible::concatenated(&[dir]) else {
            return;
        };
        if !fallible::push(&mut self.following, ListedBundle { dir: name, listed }) {
            return;
        }
        if let Some(record) = self.record_mut(dir, listed) {
            record.follow = Follow::Wanted;
        }
        let _ = self.read_anew(dir, listed, side);
    }

    /// Follow the files of each listed bundle remembered from now on, as far
    /// as [`MAX_FOLLOWED_BUNDLES`] allows: the embedder holds what reports
    /// their changes, each through [`moved`](Self::moved).
    pub fn follow_bundles(&mut self) {
        self.follows = true;
    }

    /// Whether the read of `bundle`'s files a producer is about to make should
    /// arm their follow first, so no change after it goes unreported: the
    /// bundle is followed, or is to be from this read on.
    pub fn follows(&mut self, bundle: &ListedBundle) -> bool {
        let Some(record) = self.record_mut(&bundle.dir, bundle.listed) else {
            return false;
        };
        if record.follow == Follow::Wanted {
            record.follow = Follow::Yes;
        }
        record.follow == Follow::Yes
    }

    /// `bundle`'s files cannot be followed — their volume reports no change,
    /// or a wait on them was refused — so a change to them shows once its
    /// listing moves, and it is not followed again until then.
    pub fn unfollowable(&mut self, bundle: &ListedBundle) {
        let was = self
            .record_mut(&bundle.dir, bundle.listed)
            .map(|record| core::mem::replace(&mut record.follow, Follow::Refused));
        if matches!(was, Some(Follow::Wanted | Follow::Yes)) {
            self.stop_following(bundle);
        }
    }

    /// The files `bundle`'s picture was last read from.
    #[must_use]
    pub fn files_of(&self, bundle: &ListedBundle) -> Option<BundleFiles> {
        self.bundles
            .peek(bundle.dir.as_str())?
            .iter()
            .find(|record| record.listed == bundle.listed)?
            .files
    }

    /// A followed `bundle`'s files changed: they are read again at the side
    /// it was last asked about at, after the read of them under way where
    /// there is one. Answers whether a read was recorded, the one change a
    /// producer is owed a wake for.
    pub fn moved(&mut self, bundle: &ListedBundle) -> bool {
        if self.stopping {
            return false;
        }
        let side = self
            .record_mut(&bundle.dir, bundle.listed)
            .map(|record| record.side);
        side.is_some_and(|side| self.read_anew(&bundle.dir, bundle.listed, side))
    }

    /// Stop following each listed bundle no surface asked about since the
    /// last sweep, walking those followed alone, and answer those the
    /// embedder followed — with every one forgotten since
    /// ([`take_unfollowed`](Self::take_unfollowed)) — for it to let their
    /// waits go. Where the heap has no room for that answer every follow stays
    /// until a later sweep.
    ///
    /// Run only after a pass over every surface whose bundles are followed, as
    /// [`sweep_thumbnails`](Self::sweep_thumbnails) is: a bundle a pass skipped
    /// would lose its follow while still shown.
    pub fn sweep_follows(&mut self) -> Vec<ListedBundle> {
        let mut gone = core::mem::take(&mut self.unfollowed);
        if !fallible::reserve(&mut gone, self.following.len()) {
            return gone;
        }
        let mut at = 0;
        while let Some(followed) = self.following.get(at) {
            let mut was = Follow::No;
            if let Some(record) = record_in(&mut self.bundles, &followed.dir, followed.listed) {
                if core::mem::replace(&mut record.asked, false) {
                    at += 1;
                    continue;
                }
                was = core::mem::replace(&mut record.follow, Follow::No);
            }
            let followed = self.following.swap_remove(at);
            if was == Follow::Yes {
                gone.push(followed);
            }
        }
        gone
    }

    /// The followed bundles forgotten since this was last asked, for the
    /// embedder to let their waits go: each was asked about least recently
    /// when the bound made room for another. Drained in place, so the room
    /// they were given stays for the next.
    pub fn take_unfollowed(&mut self) -> alloc::vec::Drain<'_, ListedBundle> {
        self.unfollowed.drain(..)
    }

    /// How many decodes this desk has recorded: what an embedder compares
    /// across a call to learn whether it recorded one, as
    /// [`version_of`](Self::version_of) records a read of the files of a
    /// bundle it follows again.
    #[must_use]
    pub fn recorded(&self) -> u64 {
        self.queue.asked.wrapping_add(self.thumbnails.asked)
    }

    /// Stop following each listed bundle whose directory `within` admits —
    /// those on a volume about to be ejected — so a read of one under way
    /// arms no follow that stands. Each is followed again once asked about.
    pub fn stop_following_where(&mut self, mut within: impl FnMut(&str) -> bool) {
        let mut at = 0;
        while let Some(followed) = self.following.get(at) {
            if !within(&followed.dir) {
                at += 1;
                continue;
            }
            let followed = self.following.swap_remove(at);
            if let Some(record) = record_in(&mut self.bundles, &followed.dir, followed.listed) {
                record.follow = Follow::No;
            }
        }
    }

    /// Take `bundle` out of those followed.
    fn stop_following(&mut self, bundle: &ListedBundle) {
        if let Some(at) = self
            .following
            .iter()
            .position(|followed| followed == bundle)
        {
            drop(self.following.swap_remove(at));
        }
    }

    /// What the desk remembers of the bundle at `dir` listed as `listed`.
    fn record_mut(&mut self, dir: &str, listed: DocumentStamp) -> Option<&mut Record> {
        record_in(&mut self.bundles, dir, listed)
    }

    /// What the desk remembers of the bundle at `dir` listed as `listed`, as
    /// just asked about: its directory the most recent, and this version the
    /// most recent of its own.
    fn touch(&mut self, dir: &str, listed: DocumentStamp) -> Option<&mut Record> {
        let records = self.bundles.get_mut(dir)?;
        let at = records.iter().position(|record| record.listed == listed)?;
        records.get_mut(at..)?.rotate_left(1);
        records.last_mut()
    }

    /// Remember the bundle at `dir`, listed as `listed`, as just asked about
    /// at `side`: past the bound the version of it asked about least recently
    /// is forgotten first, or, for a directory not yet remembered, the
    /// directory asked about least recently. One the heap refuses is not
    /// remembered, and is asked for at the version listed.
    fn remember(&mut self, dir: &str, listed: DocumentStamp, side: u32) {
        let versions = self.bundles.peek(dir).map(Vec::len);
        let crowded = match versions {
            Some(held) => held >= MAX_LISTED_VERSIONS,
            None => self.bundles.len() >= MAX_LISTED_BUNDLE_DIRS,
        };
        if crowded {
            // Room first for the follows forgetting may hand back, so the
            // forgetting itself needs nothing the heap could refuse.
            let room = match versions {
                Some(_) => 1,
                None => MAX_LISTED_VERSIONS,
            };
            if !fallible::reserve(&mut self.unfollowed, room) {
                return;
            }
            match versions {
                Some(_) => self.forget_least_recent_version(dir),
                None => self.forget_least_recent(),
            }
        }
        let followed = if self.may_follow_more() && fallible::reserve(&mut self.following, 1) {
            fallible::concatenated(&[dir])
        } else {
            None
        };
        let record = Record {
            listed,
            files: None,
            side,
            asked: true,
            follow: if followed.is_some() {
                Follow::Wanted
            } else {
                Follow::No
            },
            moved: false,
        };
        if !self.file_record(dir, record) {
            return;
        }
        if let Some(dir) = followed {
            self.following.push(ListedBundle { dir, listed });
        }
    }

    /// File `record` among the versions of the bundle at `dir` the desk
    /// remembers, answering whether the heap had room.
    fn file_record(&mut self, dir: &str, record: Record) -> bool {
        if let Some(records) = self.bundles.peek_mut(dir) {
            return fallible::push(records, record);
        }
        let Some(key) = fallible::concatenated(&[dir]) else {
            return false;
        };
        let mut records = Vec::new();
        fallible::push(&mut records, record) && self.bundles.try_insert(key, records).is_ok()
    }

    /// Forget the directory asked about least recently, with every version of
    /// it, their follows let go into the room the caller kept, so forgetting
    /// allocates nothing.
    fn forget_least_recent(&mut self) {
        if let Some((dir, records)) = self.bundles.pop_lru() {
            for record in &records {
                self.let_go(&dir, record);
            }
        }
    }

    /// Forget the version of the bundle at `dir` asked about least recently,
    /// its follow let go into the room the caller kept.
    fn forget_least_recent_version(&mut self, dir: &str) {
        let Some(records) = self.bundles.peek_mut(dir) else {
            return;
        };
        if records.is_empty() {
            return;
        }
        let record = records.remove(0);
        self.let_go(dir, &record);
    }

    /// Stop following `record`, a version of the bundle at `dir` just
    /// forgotten, handing a follow its embedder armed back for it to let go.
    fn let_go(&mut self, dir: &str, record: &Record) {
        if !matches!(record.follow, Follow::Wanted | Follow::Yes) {
            return;
        }
        let at = self
            .following
            .iter()
            .position(|followed| followed.dir == dir && followed.listed == record.listed);
        if let Some(at) = at {
            let followed = self.following.swap_remove(at);
            if record.follow == Follow::Yes {
                self.unfollowed.push(followed);
            }
        }
    }

    /// Each remembered bundle at or under `tree` whose files have been read,
    /// with the side it was last asked about at.
    fn read_under(&self, tree: &str) -> Vec<(ListedBundle, u32)> {
        let mut read = Vec::new();
        for (dir, records) in self.bundles.iter_lru() {
            if !is_at_or_under(dir, tree) {
                continue;
            }
            for record in records.iter().filter(|record| record.files.is_some()) {
                let Some(dir) = fallible::concatenated(&[dir.as_str()]) else {
                    continue;
                };
                let bundle = ListedBundle {
                    dir,
                    listed: record.listed,
                };
                if !fallible::push(&mut read, (bundle, record.side)) {
                    return read;
                }
            }
        }
        read
    }

    /// Produce nothing pictured from at or under `tree` until
    /// [`release_under`](Self::release_under): it is being written — a copy
    /// filling a bundle — so an answer read now could picture the half-written
    /// state, and a refusal read now would be retained as the answer.
    ///
    /// What is asked for meanwhile records nothing and draws the tier below, so
    /// however much of a tree is shown while it is written, holding it costs
    /// one path. Called before the first write, so no ask precedes it. A tree
    /// the heap refuses to record is not held, which costs only a decode of the
    /// unfinished state.
    pub fn hold_under(&mut self, tree: &str) {
        if self.stopping {
            return;
        }
        if let Some(tree) = fallible::concatenated(&[tree]) {
            let _ = fallible::push(&mut self.held, tree);
        }
    }

    /// `tree` has been written: it is named in the next [`Landed`], so each
    /// item drawn from inside it while it was held asks again and the decode
    /// reads the finished tree, and each bundle under it whose files were read
    /// before is read again. Answers whether it was held, so the embedder
    /// wakes its loop and a producer.
    pub fn release_under(&mut self, tree: &str) -> bool {
        let Some(at) = self.held.iter().position(|held| held == tree) else {
            return false;
        };
        let tree = self.held.swap_remove(at);
        for (bundle, side) in self.read_under(&tree) {
            let _ = self.read_anew(&bundle.dir, bundle.listed, side);
        }
        self.landed.release(tree);
        true
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
        self.landed = Landed::new();
        self.held.clear();
        self.bundles.clear();
        self.following.clear();
        self.unfollowed.clear();
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

impl ArtworkDesk {
    /// Take the next icon decode to run, or `None` when there is none.
    pub fn next_job(&mut self) -> Option<ArtworkJob> {
        self.next_job_where(|_| (), |()| true).map(|((), job)| job)
    }

    /// Take the next thumbnail to run, or `None` when there is none.
    pub fn next_thumbnail(&mut self) -> Option<ArtworkJob> {
        self.next_thumbnail_where(|_| (), |()| true)
            .map(|((), job)| job)
    }

    /// Record what decoding `job` produced, as
    /// [`deliver_where`](Self::deliver_where) records it for the one share.
    pub fn deliver(&mut self, job: &ArtworkJob, artwork: Option<Artwork>) -> Delivered {
        self.deliver_where(job, artwork, |_| (), &())
    }
}

/// Wanted decodes in the order they are handed out, kept apart by the share
/// of producers that takes them, so a take or a delivery looks at its own
/// share's decodes and no other's.
///
/// Each decode is routed to its share once, by the first take or delivery
/// after it was recorded. The order is only the hand-out *order*; the slots
/// are the authority on whether a decode is still wanted, and one no longer
/// wanted is dropped as it is reached, so a paint never pays for the
/// producers' bookkeeping.
struct Order<S> {
    /// Stamped on each decode as it is recorded: the order it was asked in.
    asked: u64,
    /// Recorded and not yet routed, oldest first: every one asked after every
    /// routed decode.
    unrouted: VecDeque<Queued>,
    /// One queue per share holding decodes, each oldest first.
    shares: Vec<(S, VecDeque<Queued>)>,
}

/// A decode waiting to be handed out, and when it was asked for.
struct Queued {
    asked: u64,
    job: ArtworkJob,
}

impl<S> Order<S> {
    const fn new() -> Self {
        Self {
            asked: 0,
            unrouted: VecDeque::new(),
            shares: Vec::new(),
        }
    }
}

impl<S: Clone + Eq> Order<S> {
    /// Whether the heap has room to record one more decode, so a
    /// [`push`](Self::push) after it allocates nothing.
    fn reserve_one(&mut self) -> bool {
        self.unrouted.try_reserve(1).is_ok()
    }

    /// Record `job` as asked for now.
    fn push(&mut self, job: ArtworkJob) {
        self.unrouted.push_back(Queued {
            asked: self.asked,
            job,
        });
        self.asked = self.asked.wrapping_add(1);
    }

    fn len(&self) -> usize {
        self.unrouted.len()
            + self
                .shares
                .iter()
                .map(|(_, queue)| queue.len())
                .sum::<usize>()
    }

    fn is_empty(&self) -> bool {
        self.unrouted.is_empty() && self.shares.iter().all(|(_, queue)| queue.is_empty())
    }

    /// Give each decode recorded since the last call its share, as far as the
    /// heap has room: the first it has none for waits, first, for the next.
    fn route(&mut self, mut route: impl FnMut(&ArtworkJob) -> S) {
        while let Some(queued) = self.unrouted.pop_front() {
            let share = route(&queued.job);
            let held = self.shares.iter().position(|(each, _)| *each == share);
            let at = if let Some(at) = held {
                at
            } else {
                if !fallible::push(&mut self.shares, (share, VecDeque::new())) {
                    self.unrouted.push_front(queued);
                    return;
                }
                self.shares.len() - 1
            };
            let Some((_, queue)) = self.shares.get_mut(at) else {
                self.unrouted.push_front(queued);
                return;
            };
            if queue.try_reserve(1).is_err() {
                self.unrouted.push_front(queued);
                return;
            }
            queue.push_back(queued);
        }
    }

    /// The oldest wanted decode of the shares `mine` admits, taken out with
    /// its share.
    fn take(
        &mut self,
        slots: &OrderedMap<ArtworkJob, Slot>,
        route: impl FnMut(&ArtworkJob) -> S,
        mut mine: impl FnMut(&S) -> bool,
    ) -> Option<(S, ArtworkJob)> {
        self.route(route);
        let mut oldest: Option<(usize, u64)> = None;
        for (at, (share, queue)) in self.shares.iter_mut().enumerate() {
            if !mine(share) {
                continue;
            }
            drop_unwanted(queue, slots);
            if let Some(front) = queue.front() {
                if oldest.is_none_or(|(_, asked)| front.asked < asked) {
                    oldest = Some((at, front.asked));
                }
            }
        }
        let taken = oldest.and_then(|(at, _)| {
            let (share, queue) = self.shares.get_mut(at)?;
            queue.pop_front().map(|queued| (share.clone(), queued.job))
        });
        self.shares.retain(|(_, queue)| !queue.is_empty());
        taken
    }

    /// Whether `share` holds a wanted decode.
    fn holds(
        &mut self,
        slots: &OrderedMap<ArtworkJob, Slot>,
        route: impl FnMut(&ArtworkJob) -> S,
        share: &S,
    ) -> bool {
        self.route(route);
        let Some(at) = self.shares.iter().position(|(each, _)| each == share) else {
            return false;
        };
        let Some((_, queue)) = self.shares.get_mut(at) else {
            return false;
        };
        drop_unwanted(queue, slots);
        if queue.is_empty() {
            self.shares.swap_remove(at);
            return false;
        }
        true
    }

    /// Take out the decode asked for longest ago that is still wanted.
    fn pop_oldest(&mut self, slots: &OrderedMap<ArtworkJob, Slot>) -> Option<ArtworkJob> {
        for (_, queue) in &mut self.shares {
            drop_unwanted(queue, slots);
        }
        self.shares.retain(|(_, queue)| !queue.is_empty());
        let routed = self
            .shares
            .iter_mut()
            .filter_map(|(_, queue)| Some((queue.front()?.asked, queue)))
            .min_by_key(|(asked, _)| *asked);
        if let Some((_, queue)) = routed {
            return queue.pop_front().map(|queued| queued.job);
        }
        drop_unwanted(&mut self.unrouted, slots);
        self.unrouted.pop_front().map(|queued| queued.job)
    }

    /// Keep only the decodes `keep` answers for.
    fn retain(&mut self, mut keep: impl FnMut(&ArtworkJob) -> bool) {
        self.unrouted.retain(|queued| keep(&queued.job));
        for (_, queue) in &mut self.shares {
            queue.retain(|queued| keep(&queued.job));
        }
        self.shares.retain(|(_, queue)| !queue.is_empty());
    }

    /// Route every decode afresh at the next take or delivery, in the order
    /// they were asked for.
    fn reroute(&mut self) {
        let mut waiting: Vec<Queued> = self.shares.drain(..).flat_map(|(_, queue)| queue).collect();
        waiting.extend(self.unrouted.drain(..));
        waiting.sort_unstable_by_key(|queued| queued.asked);
        self.unrouted = VecDeque::from(waiting);
    }

    fn clear(&mut self) {
        self.unrouted.clear();
        self.shares.clear();
    }
}

/// Drop the decodes at the front of `queue` no longer wanted.
fn drop_unwanted(queue: &mut VecDeque<Queued>, slots: &OrderedMap<ArtworkJob, Slot>) {
    while queue
        .front()
        .is_some_and(|queued| !is_wanted(slots, &queued.job))
    {
        queue.pop_front();
    }
}

/// The job that reads the files of the bundle at `dir`, listed as `listed`,
/// and decodes its picture at `side`.
fn listed_read(dir: &str, listed: DocumentStamp, side: u32) -> Option<ArtworkJob> {
    Some(ArtworkJob {
        key: ArtworkKey::Bundle {
            dir: fallible::concatenated(&[dir])?,
            version: Some(BundleVersion::Listed(listed)),
        },
        side,
    })
}

/// What `bundles` remembers of the bundle at `dir` listed as `listed`.
fn record_in<'b>(
    bundles: &'b mut LruMap<String, Vec<Record>, BuildSipHash13>,
    dir: &str,
    listed: DocumentStamp,
) -> Option<&'b mut Record> {
    bundles
        .peek_mut(dir)?
        .iter_mut()
        .find(|record| record.listed == listed)
}

/// Overwrite `artwork`, an answer nobody will collect, before it is dropped.
fn wipe(artwork: Option<Artwork>) {
    if let Some(mut artwork) = artwork {
        artwork.wipe();
    }
}

/// Whether `job` reads picture files' own content rather than an icon.
fn is_thumbnail(job: &ArtworkJob) -> bool {
    job.key.is_thumbnail_class()
}

/// Whether `job` is still waiting for a producer.
fn is_wanted(slots: &OrderedMap<ArtworkJob, Slot>, job: &ArtworkJob) -> bool {
    matches!(
        slots.get(job),
        Some(Slot {
            state: State::Wanted,
            ..
        })
    )
}

/// The desk *is* the deferring resolver: it answers what a producer has
/// already delivered and records everything else.
///
/// An embedder that owns the desk outright hands the cache a plain `&mut` to
/// it and needs no wrapper. One that shares the desk with a worker thread
/// implements the trait over its own mutex instead, so the notify happens
/// inside the same critical section as the state change.
impl<S: Clone + Eq> ArtworkResolver for ArtworkDesk<S> {
    fn resolve(&mut self, key: &ArtworkKey, side: u32) -> Resolved {
        self.collect(key, side).0
    }

    fn prefetch(&mut self, key: &ArtworkKey, side: u32) {
        self.want(key, side);
    }

    fn declined(&mut self, key: &ArtworkKey, side: u32, bytes: usize) {
        self.decline(key, side, bytes);
    }

    fn still_shown(&mut self, key: &ArtworkKey, side: u32) {
        self.keep(key, side);
    }

    fn bundle_version(&mut self, dir: &str, listed: DocumentStamp, side: u32) -> BundleVersion {
        self.version_of(dir, listed, side)
    }
}

#[cfg(test)]
#[path = "desk_tests.rs"]
mod tests;
