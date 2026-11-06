//! A scheduler-blocking mutual-exclusion lock (a *sleeping* mutex).
//!
//! Every lock in `lib/sync` ([`SpinLock`],
//! [`RwLock`](tairix_sync::RwLock), [`McsLock`](tairix_sync::McsLock), …)
//! *spins* on contention. That is correct only for a short critical section
//! whose holder never gives up the CPU. A `SleepLock` is the opposite: its
//! critical section may **park** — most importantly it may be held across a
//! block-device completion-IRQ wait (`Block::read_blocks` parks the calling
//! task on the controller interrupt). A spin lock held across such a park is
//! a defect: a second contender on the same CPU deadlocks, and on another CPU
//! it busy-spins on a holder that is asleep (forbidden busy-waiting). A
//! `SleepLock` instead **parks the contender off the run queue** and wakes it
//! when the holder releases — no spinning while a holder sleeps.
//!
//! This is the per-mount serialisation primitive the userland filesystem
//! path needs: each `fs_*` operation runs in the calling task's own context
//! and takes the mount's `SleepLock` for the duration of one operation
//! (including the device park), so operations on *different* mounts proceed
//! fully in parallel while operations on one mount are serialised without a
//! single global server task.
//!
//! # Why this lives in `kernel/core`, not `lib/sync`
//!
//! Parking and waking a task is the scheduler's job, and the layering forbids
//! a `lib/*` crate from depending on the kernel. So, unlike the spinning
//! primitives, a sleeping lock cannot live in `lib/sync`: it reaches the
//! scheduler through the installed [`WaitQueueArch`]
//! hook (for the current CPU, the current task, and `unpark`) and the one
//! park of the calling task ([`park_calling_task`]), exactly as the
//! console-read and process-wait blocking backings do.
//!
//! # No lost wake-ups
//!
//! The acquire path closes the release/park race with the same discipline
//! the other kernel waiters use: the contender **stands in the lock's line
//! before it re-tests** the lock, so a release in the window between its
//! failed fast-path attempt and its park cannot be missed — the releaser's
//! wake finds the task in line, and the scheduler's wake-pending token turns
//! an `unpark` that races a not-yet-committed park into a re-ready rather
//! than a lost wake-up. Each woken contender re-tests and either acquires or
//! parks again, so a wake meant for another contender is a harmless spurious
//! wake.
//!
//! # Waiting needs no heap
//!
//! An acquire cannot fail, and a lock is often contended exactly where memory
//! is short — a filesystem writing back to make room. So a contender stands in
//! line through the [`LineRow`] its thread was made with, and the line threads
//! its rows through their own links: joining it allocates nothing. A line
//! holds a share of every row in it, so a thread that dies waiting leaves its
//! row there until a release reaps it.
//!
//! # No livelock
//!
//! A contender stays registered from before its first look until it owns
//! the lock, and each look either takes the lock or publishes contention in
//! one read-modify-write of the word. A releaser that finds a registration
//! can therefore always hand the lock over, and no look finds the lock free
//! without taking it. A contender that withdrew on seeing the lock free, and
//! a releaser that retook it on seeing the registration, once chased each
//! other on two cores until an interrupt broke the rhythm
//! (`plans/OPEN-DEFECTS.md` D968).
//!
//! # Fairness
//!
//! Waiters retain FIFO registration order. Release hands ownership directly
//! to the oldest task while keeping the lock closed to fresh contenders, then
//! wakes only that task. This avoids both a thundering herd and barging: a
//! long-waiting disk operation cannot be perpetually displaced by newer work.
//!
//! # The uncontended path is two atomics
//!
//! Acquire and release are one compare-exchange each when nobody is waiting,
//! and the line is not touched at all. That matters because this lock
//! serialises *every* block-device operation on a shared disk
//! (`crate::shared_block`), so a filesystem read walking a file pays one
//! acquire/release per device operation — and a device operation served from
//! the block cache above the disk is a memcpy, not a park.
//!
//! What makes it possible is that contention lives **in the lock word**: a
//! contender sets a `CONTENDED` bit there before it parks, so the releaser's
//! single `LOCKED -> 0` compare-exchange fails precisely when a wake is
//! owed. Flag and lock bit share one location, so their modification order is
//! total and no store/load fence is needed: a contender that publishes before
//! the release makes that release take the wake path, and one that looks
//! after it takes the lock and never parks. Keeping the
//! "is anyone waiting?" answer in a separate structure would have needed the
//! line's lock on every release to learn that nobody was.
//!
//! That holds for the fast path, which is itself a read-modify-write of the
//! word and so cannot miss a bit set before it. It does **not** extend to the
//! slow path's "the queue was empty, so drop the word" tail: a contender
//! registering after that scan set `CONTENDED` in the word the release then
//! wiped, having already read `LOCKED` as set and committed to park, and no
//! later release consulted the queue again — a silent boot hang. The slow
//! path therefore releases *before* it decides nobody is waiting, and reads
//! the queue once more afterwards (`release_and_recheck`).

use alloc::sync::{Arc, Weak};
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::panic::Location;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use tairix_kernel_sched_api::TaskId;
use tairix_log::{Field, FieldValue, Level, Sink};
use tairix_sync::SpinLock;

use crate::audit::{emit, AuditEvent};
use crate::waitq::{park_calling_task, wait_arch, WaitQueueArch, LATE_WAKE_NS};

/// Lock-word bit: the lock is held.
const LOCKED: u32 = 1 << 0;

/// Lock-word bit: a contender registered on the wait queue while the lock
/// was held, so the release owes it a wake.
///
/// Set by the contender *before* it parks and cleared only by a release
/// that has already dropped [`LOCKED`] and then found the queue empty. It
/// may therefore linger over a handoff, or over a contender that took a lock
/// a release left free after it published — each costs one release that
/// consults the wait queue and finds nothing, and clears the bit. Clearing it
/// while [`LOCKED`] is still set would erase a contender's publication and
/// lose its wake.
const CONTENDED: u32 = 1 << 1;

/// A mutual-exclusion lock whose contenders **park** off the run queue
/// instead of spinning, so its critical section may be held across a task
/// park (e.g. a block-device completion-IRQ wait).
///
/// Construct one with [`SleepLock::new`] and acquire it with
/// [`lock`](SleepLock::lock) (blocking) or [`try_lock`](SleepLock::try_lock)
/// (non-blocking). The returned [`SleepGuard`] dereferences to the protected
/// value and releases the lock — waking a parked contender — when dropped.
///
/// Acquiring this lock may park the caller, so it must be taken only from a
/// context that can be rescheduled (a task / kthread), never from an
/// interrupt handler.
pub struct SleepLock<T: ?Sized> {
    /// [`LOCKED`] while held, plus [`CONTENDED`] once a contender has
    /// registered to park. The single point of mutual exclusion; every
    /// acquire is a `compare_exchange` against it.
    state: AtomicU32,
    /// FIFO ownership handed directly to one parked task. Zero means no
    /// handoff is outstanding; scheduler task ids never use zero.
    handoff: AtomicU64,
    /// When the outstanding handoff was published and by which holder, so a
    /// claimant that ran late can say so. Written before the
    /// [`Self::handoff`] publication and touched only on the contended path.
    handed: SpinLock<Handed>,
    /// Contenders parked waiting for the holder to release, in arrival order.
    waiters: Line,
    /// The protected value. Access is guarded by [`LOCKED`]: a live
    /// [`SleepGuard`] is proof of exclusive ownership.
    data: UnsafeCell<T>,
}

// SAFETY: `SleepLock` is a mutual-exclusion boundary: `lock`/`try_lock` hand
// out a `SleepGuard` only after a successful `compare_exchange` that sets
// `LOCKED`, so at most one thread ever holds `&mut`-equivalent access to
// `data` at a time, and ownership is transferred (not shared) on release. It
// is therefore safe to send the lock (and the value it guards) between threads
// when `T: Send`, and to share `&SleepLock` across threads when `T: Send`
// (sharing the reference only ever yields serialised, exclusive access to
// `T`). `T` need not be `Sync` because the guard never hands out concurrent
// `&T`.
unsafe impl<T: ?Sized + Send> Send for SleepLock<T> {}
// SAFETY: as for `Send` above — `&SleepLock` only ever yields exclusive
// access to `T` through the `LOCKED` gate, never concurrent shared access.
unsafe impl<T: ?Sized + Send> Sync for SleepLock<T> {}

/// Formats without taking the lock or showing what it guards, so a holder
/// is never blocked by a debug print and the value never reaches one.
impl<T: ?Sized> core::fmt::Debug for SleepLock<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SleepLock").finish_non_exhaustive()
    }
}

impl<T> SleepLock<T> {
    /// A new unlocked `SleepLock` guarding `value`.
    ///
    /// `const` so a lock may be placed in a `static` or built in a `const`
    /// context, like the spinning primitives.
    #[must_use]
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicU32::new(0),
            handoff: AtomicU64::new(0),
            handed: SpinLock::new(Handed {
                at_ns: 0,
                from: None,
            }),
            waiters: Line::new(),
            data: UnsafeCell::new(value),
        }
    }

    /// Consume the lock and return the guarded value.
    ///
    /// Takes `self` by value, so no other reference can exist and no locking
    /// is required.
    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

impl<T: ?Sized> SleepLock<T> {
    /// Try to acquire the lock without blocking.
    ///
    /// Returns the [`SleepGuard`] on success, or [`None`] if the lock is
    /// currently held — never parks, so it is safe from any context.
    ///
    /// The contention bit is carried through an acquire rather than cleared:
    /// waiters may still be queued, and only a release that has looked at
    /// the queue may say otherwise.
    #[must_use]
    #[track_caller]
    pub fn try_lock(&self) -> Option<SleepGuard<'_, T>> {
        self.try_lock_at(Location::caller())
    }

    /// [`Self::try_lock`] on behalf of a caller at `site`.
    fn try_lock_at(&self, site: &'static Location<'static>) -> Option<SleepGuard<'_, T>> {
        let mut observed = self.state.load(Ordering::Relaxed);
        loop {
            if observed & LOCKED != 0 {
                return None;
            }
            match self.state.compare_exchange_weak(
                observed,
                observed | LOCKED,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(SleepGuard::new(self, site)),
                Err(current) => observed = current,
            }
        }
    }

    /// Acquire the lock, parking the caller off the run queue while it is
    /// held by someone else.
    ///
    /// Blocks until the lock is acquired; the returned [`SleepGuard`]
    /// releases it on drop. Must be called from a reschedulable context (a
    /// task / kthread), never an interrupt handler.
    ///
    /// A contended acquire that waited a second or more, ran a quarter of a
    /// second or more after being handed the lock, or waited a quarter of a
    /// second unable to park, is recorded ([`AuditEvent::LockAcquiredLate`])
    /// under the site that called this, with how its wait went.
    #[track_caller]
    pub fn lock(&self) -> SleepGuard<'_, T> {
        let site = Location::caller();
        if let Some(guard) = self.try_lock_at(site) {
            return guard;
        }
        let asked_ns = crate::waitq::wait_now_ns();
        let mut wait = Wait::default();
        let (guard, handed) = match current_waiter() {
            Some((task, row)) => {
                self.wait_in_line(site, task, &row, &mut park_calling_task, &mut wait)
            }
            None => (self.spin_unscheduled(site, &mut wait), None),
        };
        if let (Some(asked_ns), Some(now_ns)) = (asked_ns, crate::waitq::wait_now_ns()) {
            report_if_late(
                crate::audit::kernel_sink(),
                site,
                Acquired {
                    asked_ns,
                    now_ns,
                    handed,
                    wait,
                },
            );
        }
        guard
    }

    /// Take the ownership the prior holder handed `task`, for a caller at
    /// `site`, with the publication it was handed by; [`None`] when nothing
    /// was handed to it (a spurious wake).
    fn take_handoff(
        &self,
        task: u64,
        site: &'static Location<'static>,
    ) -> Option<(SleepGuard<'_, T>, Option<Handed>)> {
        if !self.claim_handoff(task) {
            return None;
        }
        // The claim's `Acquire` orders this after the publication.
        let handed = *self.handed.lock();
        Some((SleepGuard::new(self, site), Some(handed)))
    }

    /// Claim direct FIFO ownership granted to `task` by the prior holder.
    fn claim_handoff(&self, task: u64) -> bool {
        self.handoff
            .compare_exchange(task, 0, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    /// Wait as `task`, standing in line with `row`, for the lock, a caller at
    /// `site`, until it owns it: `park` gives the CPU up between looks and
    /// reports whether it parked.
    ///
    /// The task stands in line from before its first look until it owns the
    /// lock, so a release that finds it there can always hand the lock to it,
    /// and a spurious wake keeps its place. Joining before the look is what
    /// makes the release/park race lossless.
    fn wait_in_line(
        &self,
        site: &'static Location<'static>,
        task: TaskId,
        row: &Arc<LineRow>,
        park: &mut dyn FnMut() -> bool,
        wait: &mut Wait,
    ) -> (SleepGuard<'_, T>, Option<Handed>) {
        wait.task = Some(task);
        self.waiters.join(row, task);
        let owned = loop {
            if self.take_or_contend() {
                break (SleepGuard::new(self, site), None);
            }
            // Claim only after the park: a handoff claimed before its wake
            // lands leaves that wake pending, to cut short this task's next,
            // unrelated park.
            let parked = park();
            if parked {
                wait.parks = wait.parks.saturating_add(1);
            } else {
                wait.unparkable = wait.unparkable.saturating_add(1);
                core::hint::spin_loop();
            }
            if let Some(taken) = self.take_handoff(task, site) {
                break taken;
            }
            if parked {
                wait.unhanded = wait.unhanded.saturating_add(1);
            }
        };
        self.waiters.leave(row);
        owned
    }

    /// One look by a registered contender: take the lock if it is free, else
    /// publish [`CONTENDED`] on it while it is held. Returns whether it took
    /// the lock.
    ///
    /// One read-modify-write of the word, so no look can find the lock free
    /// and then fail to take it (the module docs' "No livelock").
    fn take_or_contend(&self) -> bool {
        let mut observed = self.state.load(Ordering::Relaxed);
        loop {
            let take = observed & LOCKED == 0;
            let next = observed | if take { LOCKED } else { CONTENDED };
            match self.state.compare_exchange_weak(
                observed,
                next,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return take,
                Err(current) => observed = current,
            }
        }
    }

    /// Take the lock for a caller no scheduler can park: before the boot path
    /// installs one, or outside any task. It stands in no line, so no release
    /// waits on it.
    fn spin_unscheduled(
        &self,
        site: &'static Location<'static>,
        wait: &mut Wait,
    ) -> SleepGuard<'_, T> {
        loop {
            wait.unparkable = wait.unparkable.saturating_add(1);
            core::hint::spin_loop();
            if let Some(guard) = self.try_lock_at(site) {
                return guard;
            }
        }
    }

    /// Release the lock and wake the oldest parked contender.
    ///
    /// Called only by [`SleepGuard`]'s `Drop`. Uncontended — no contender has
    /// published [`CONTENDED`] — this is one compare-exchange and the wait
    /// queue is never consulted. Otherwise ownership is published directly to
    /// the oldest task and [`LOCKED`] remains set, so a fresh contender cannot
    /// barge before the wake runs; the designated waiter's Acquire claim
    /// observes the prior holder's critical-section writes. `from` is the
    /// site the releasing holder took the lock at.
    ///
    /// A contended release that took [`LATE_WAKE_NS`] or more is recorded
    /// ([`AuditEvent::LockReleasedLate`]).
    fn release(&self, from: &'static Location<'static>) {
        if self
            .state
            .compare_exchange(LOCKED, 0, Ordering::Release, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }
        let hook = wait_arch();
        let releasing_ns = hook.map(WaitQueueArch::now_ns);
        self.release_contended(hook, from);
        if let (Some(hook), Some(releasing_ns)) = (hook, releasing_ns) {
            report_if_released_late(
                crate::audit::kernel_sink(),
                from,
                hook.now_ns().saturating_sub(releasing_ns),
                || hook.current_cpu().and_then(|cpu| hook.current_task(cpu)),
            );
        }
    }

    /// The release path a published [`CONTENDED`] flag selects, factored so
    /// host tests can drive the direct handoff state machine without
    /// installing the process-global boot hook.
    ///
    /// Hand off to the oldest waiter if there is one; otherwise release the
    /// word and look again, retaking the lock when a contender turned up in
    /// that window ([`Self::release_and_recheck`]).
    fn release_contended(
        &self,
        hook: Option<&dyn crate::waitq::WaitQueueArch>,
        from: &'static Location<'static>,
    ) {
        let Some(hook) = hook else {
            // No scheduler hook: nothing can be parked, so no wake is owed
            // and nobody can arrive to be stranded.
            self.state.store(0, Ordering::Release);
            return;
        };
        while !self.hand_off_oldest(hook, from) {
            if !self.release_and_recheck() {
                return;
            }
        }
    }

    /// Publish ownership to the oldest live waiter and wake it, returning
    /// whether one took it. [`LOCKED`] is left **set** on success: ownership
    /// is in flight, so a fresh contender cannot barge ahead of the FIFO
    /// waiter, and the waiter's Acquire claim observes the prior holder's
    /// critical-section writes.
    ///
    /// A waiter that cannot take the handoff is passed over for the
    /// next-oldest, never taken as licence to unlock with the queue still
    /// occupied. "Cannot take it" is the *wake landing*, not a row existing:
    /// a task the scheduler retired while it was still registered would
    /// otherwise be handed ownership it can never claim, and [`LOCKED`] would
    /// stay set on a lock nobody holds — every later acquirer parking for
    /// ever on a free mount.
    ///
    /// **Nothing is removed here.** The designation is a
    /// [`Registration`], and the queue reaps a
    /// row only when it has proved that row's own task can never run again.
    /// Deleting by task id instead deletes whatever is registered *now*,
    /// which — for a waiter that resumed, failed its claim, and parked again
    /// inside this very window — is a live row; the lock is then released
    /// with [`CONTENDED`] clear, so no later release consults the queue and
    /// that waiter sleeps for ever on a free lock.
    ///
    /// Retracting the publication is a compare-exchange, not a store: the
    /// designated waiter may have been resumed by an unrelated wake (a
    /// deferred termination unparking it) and claimed the handoff on its way
    /// past, in which case it is already the owner and a second successor
    /// must not be named.
    ///
    /// The scan terminates. Every round that does not return either reaps the
    /// head or observes it replaced, and a fresh registration takes a
    /// strictly larger arrival sequence — so the head's sequence rises
    /// monotonically and no round can revisit one.
    fn hand_off_oldest(
        &self,
        hook: &dyn crate::waitq::WaitQueueArch,
        from: &'static Location<'static>,
    ) -> bool {
        while let Some(reg) = self.waiters.oldest_registration() {
            if self.offer_to(hook, &reg, from) {
                return true;
            }
        }
        false
    }

    /// One round of the scan: publish ownership to `reg` and wake it,
    /// reporting whether it took the lock.
    ///
    /// On a wake that did not land the publication is withdrawn again and
    /// **nothing is removed** — the queue reaps a row only when it has proved
    /// that row's own task can never run, which it does under `reg`'s own
    /// identity. Removing by task id instead removes whatever is registered
    /// *now*, and for a waiter that resumed, failed its claim and parked
    /// again inside this very round that is a live row.
    fn offer_to(
        &self,
        hook: &dyn crate::waitq::WaitQueueArch,
        reg: &Registration,
        from: &'static Location<'static>,
    ) -> bool {
        *self.handed.lock() = Handed {
            at_ns: hook.now_ns(),
            from: Some(from),
        };
        self.handoff.store(reg.task(), Ordering::Release);
        if self.waiters.wake_registration(hook, reg) {
            return true;
        }
        // Nothing left to withdraw means the waiter claimed on its way past,
        // so it is already the owner.
        !self.retract_handoff(reg.task())
    }

    /// Withdraw a published handoff to `task` whose wake did not land,
    /// reporting whether it was still there to withdraw.
    ///
    /// A compare-exchange, not a store: an unrelated wake — a deferred
    /// termination unparking the waiter — can resume the designated task
    /// between the publication and the wake, and it then claims on its way
    /// past. `false` is that case, and it means ownership transferred after
    /// all; overwriting the claimed slot would let the scan name a second
    /// successor and hand two tasks the same lock.
    fn retract_handoff(&self, task: u64) -> bool {
        self.handoff
            .compare_exchange(task, 0, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    /// Release a lock no waiter wanted, then look at the queue once more
    /// ([`Self::recheck_after_release`]). Returns whether the lock was
    /// retaken because a contender appeared in the window, so the caller owes
    /// another handoff.
    fn release_and_recheck(&self) -> bool {
        self.state.fetch_and(!LOCKED, Ordering::AcqRel);
        self.recheck_after_release()
    }

    /// The second look of a release that has dropped [`LOCKED`]: retake the
    /// lock for a contender registered since the first, or clear the whole
    /// word when there is none. Returns whether it retook the lock.
    ///
    /// Clearing the word without this look would erase the [`CONTENDED`] of
    /// a contender that registered after the first and is about to park, and
    /// every later release would take the fast path past it. Looking after
    /// the release mirrors the contender's register-then-look: whichever of
    /// the two read-modify-writes on the word runs second observes the first.
    fn recheck_after_release(&self) -> bool {
        if self.waiters.is_empty() {
            // Genuinely nobody, so the next release is one compare-exchange
            // again. A contender arriving after this look finds the lock bit
            // clear and takes the lock in its own. The bit carries no data,
            // hence the relaxed ordering.
            let _ = self
                .state
                .compare_exchange(CONTENDED, 0, Ordering::Relaxed, Ordering::Relaxed);
            return false;
        }
        // One appeared. Retake the lock so the handoff keeps its FIFO order.
        // If an acquirer took it first, that one owns the release obligation
        // — the contention bit is still set, so its own release consults the
        // queue — and nothing more is owed here.
        self.state
            .compare_exchange(
                CONTENDED,
                CONTENDED | LOCKED,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_ok()
    }
}

/// An RAII proof of exclusive ownership of a [`SleepLock`]'s value.
///
/// Dereferences to the guarded `T` and releases the lock — waking a parked
/// contender — when dropped.
pub struct SleepGuard<'a, T: ?Sized> {
    lock: &'a SleepLock<T>,
    /// Where the lock was taken, which a successor handed it from names.
    site: &'static Location<'static>,
}

impl<'a, T: ?Sized> SleepGuard<'a, T> {
    fn new(lock: &'a SleepLock<T>, site: &'static Location<'static>) -> Self {
        Self { lock, site }
    }
}

/// A handoff's publication: when it was made, and the site of the holder
/// that made it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Handed {
    at_ns: u64,
    from: Option<&'static Location<'static>>,
}

/// The task a scheduler has switched in on the calling CPU and the row it
/// stands in line with, or [`None`] before the boot path installs one, or
/// outside any task.
fn current_waiter() -> Option<(TaskId, Arc<LineRow>)> {
    let hook = wait_arch()?;
    let cpu = hook.current_cpu()?;
    Some((hook.current_task(cpu)?, hook.line_row(cpu)?))
}

/// The room one thread's wait on a sleeping lock takes: its place in the
/// lock's line, made with the thread so a contended acquire never needs the
/// heap.
///
/// A thread waits on one lock at a time, so one row serves every lock it
/// contends on. A line holds a share of each row standing in it, so the row
/// of a thread that died waiting stays until the line reaps it.
pub struct LineRow {
    place: SpinLock<Place>,
}

impl LineRow {
    /// A row standing in no line.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            place: SpinLock::new(Place {
                task: None,
                joins: 0,
                prev: Weak::new(),
                next: None,
            }),
        }
    }
}

impl Default for LineRow {
    fn default() -> Self {
        Self::new()
    }
}

/// Where a row stands: in no line, or in one between its neighbours.
struct Place {
    /// The task waiting, while the row stands in a line.
    task: Option<TaskId>,
    /// How often the row has joined a line, so a designation names one stand
    /// and never a later one.
    joins: u64,
    prev: Weak<LineRow>,
    next: Option<Arc<LineRow>>,
}

/// A sleeping lock's waiters in arrival order, threaded through their rows.
///
/// Every link changes under the line's lock, and a row's own lock is taken
/// only inside it, one row at a time, by the one line the row stands in.
struct Line {
    ends: SpinLock<Ends>,
}

/// A line's first and last rows.
struct Ends {
    first: Option<Arc<LineRow>>,
    last: Weak<LineRow>,
}

/// One stand of a row in a line, named well enough that a decision taken
/// about it cannot be applied to a later one: a waiter that left and joined
/// again inside the window a release acts in is a new stand.
struct Registration {
    row: Arc<LineRow>,
    task: TaskId,
    joins: u64,
}

impl Registration {
    /// The waiting task, which the handoff publishes ownership to.
    const fn task(&self) -> TaskId {
        self.task
    }

    /// Whether this is still the row's present stand.
    fn stands(&self) -> bool {
        let place = self.row.place.lock();
        place.task.is_some() && place.joins == self.joins
    }
}

impl Line {
    const fn new() -> Self {
        Self {
            ends: SpinLock::new(Ends {
                first: None,
                last: Weak::new(),
            }),
        }
    }

    /// Stand `row` at the back as `task`'s; a row standing already stays
    /// where it is.
    fn join(&self, row: &Arc<LineRow>, task: TaskId) {
        let mut ends = self.ends.lock();
        {
            let mut place = row.place.lock();
            if place.task.is_some() {
                return;
            }
            place.task = Some(task);
            place.joins = place.joins.wrapping_add(1);
            place.prev.clone_from(&ends.last);
        }
        match ends.last.upgrade() {
            Some(last) => last.place.lock().next = Some(Arc::clone(row)),
            None => ends.first = Some(Arc::clone(row)),
        }
        ends.last = Arc::downgrade(row);
    }

    /// Take `row` out if it stands here. Idempotent.
    fn leave(&self, row: &Arc<LineRow>) {
        Self::unlink(&mut self.ends.lock(), row);
    }

    /// Splice `row` out of the line `ends` bounds, unless it stands in none.
    fn unlink(ends: &mut Ends, row: &LineRow) {
        let (prev, next) = {
            let mut place = row.place.lock();
            if place.task.take().is_none() {
                return;
            }
            (core::mem::take(&mut place.prev), place.next.take())
        };
        match prev.upgrade() {
            Some(before) => before.place.lock().next.clone_from(&next),
            None => ends.first.clone_from(&next),
        }
        match &next {
            Some(after) => after.place.lock().prev = prev,
            None => ends.last = prev,
        }
    }

    /// The front row's stand, without taking it out. O(1).
    fn oldest_registration(&self) -> Option<Registration> {
        let ends = self.ends.lock();
        let first = ends.first.as_ref()?;
        let place = first.place.lock();
        Some(Registration {
            row: Arc::clone(first),
            task: place.task?,
            joins: place.joins,
        })
    }

    /// Wake exactly the stand `reg`, reporting whether the wake landed.
    ///
    /// `false` covers three outcomes a caller transferring ownership need not
    /// tell apart, because none of them is a successor: the stand is over, a
    /// *newer* one of the same row stands in its place (its waiter went round
    /// its own acquire loop), or the row still stands and the scheduler can
    /// never run that task again. Only the last is reaped, and only under the
    /// same identity check — removing a stand this designation was not taken
    /// about is what strands a live waiter on a free lock. The `unpark` runs
    /// with no lock held.
    fn wake_registration(&self, arch: &dyn WaitQueueArch, reg: &Registration) -> bool {
        if !reg.stands() {
            return false;
        }
        if arch.unpark(reg.task()) {
            return true;
        }
        let mut ends = self.ends.lock();
        if reg.stands() {
            Self::unlink(&mut ends, &reg.row);
        }
        false
    }

    /// Whether no row stands here.
    fn is_empty(&self) -> bool {
        self.ends.lock().first.is_none()
    }
}

impl Drop for Ends {
    /// Splice the rows out one at a time: each holds the next, so dropping
    /// the first would drop a long line of them recursively.
    fn drop(&mut self) {
        let mut next = self.first.take();
        while let Some(row) = next {
            let mut place = row.place.lock();
            place.task = None;
            place.prev = Weak::new();
            next = place.next.take();
        }
    }
}

/// How a contended acquire spent its wait: the task it waited as, how often
/// it parked, how many of those wakes brought no handoff, and how often it
/// could not park at all.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Wait {
    task: Option<u64>,
    parks: u32,
    unhanded: u32,
    unparkable: u32,
}

/// How a contended acquire went: when it asked, when it held the lock, the
/// publication it was handed the lock by ([`None`] when it found the lock
/// free), and how it waited.
#[derive(Clone, Copy, Debug)]
struct Acquired {
    asked_ns: u64,
    now_ns: u64,
    handed: Option<Handed>,
    wait: Wait,
}

/// How long a contended acquire may wait before it is recorded: a sleeping
/// lock serialises single operations, which take milliseconds, and a wait
/// may span several holders queued ahead.
const LOCK_WAIT_NS: u64 = 1_000_000_000;

/// Record through `sink` a contended release of the lock taken at `site`
/// that took `release_ns`, [`LATE_WAKE_NS`] or more, with the releasing
/// task `holder` names. A release hands over to one waiter, which is
/// microseconds of work.
fn report_if_released_late(
    sink: Option<&dyn Sink>,
    site: &'static Location<'static>,
    release_ns: u64,
    holder: impl FnOnce() -> Option<u64>,
) {
    if release_ns < LATE_WAKE_NS {
        return;
    }
    let Some(sink) = sink else {
        return;
    };
    emit(
        sink,
        Level::Warn,
        AuditEvent::LockReleasedLate,
        &[
            Field {
                key: "site",
                value: FieldValue::Str(site.file()),
            },
            Field {
                key: "line",
                value: FieldValue::UnsignedInt(u64::from(site.line())),
            },
            Field {
                key: "release_ms",
                value: FieldValue::UnsignedInt(release_ns / 1_000_000),
            },
            Field {
                key: "task",
                value: holder().map_or(FieldValue::Null, FieldValue::UnsignedInt),
            },
        ],
    );
}

/// Record the contended acquire at `site` through `sink` when it waited
/// [`LOCK_WAIT_NS`], ran [`LATE_WAKE_NS`] after being handed the lock, or
/// waited [`LATE_WAKE_NS`] while unable to park.
fn report_if_late(sink: Option<&dyn Sink>, site: &'static Location<'static>, acquired: Acquired) {
    let waited_ns = acquired.now_ns.saturating_sub(acquired.asked_ns);
    let late_ns = acquired
        .handed
        .map(|handed| acquired.now_ns.saturating_sub(handed.at_ns));
    // A waiter that cannot park holds its CPU, and with it anything queued
    // there, the holder included.
    let spun = acquired.wait.unparkable > 0 && waited_ns >= LATE_WAKE_NS;
    if waited_ns < LOCK_WAIT_NS && !spun && late_ns.is_none_or(|ns| ns < LATE_WAKE_NS) {
        return;
    }
    let Some(sink) = sink else {
        return;
    };
    let from = acquired.handed.and_then(|handed| handed.from);
    emit(
        sink,
        Level::Warn,
        AuditEvent::LockAcquiredLate,
        &[
            Field {
                key: "site",
                value: FieldValue::Str(site.file()),
            },
            Field {
                key: "line",
                value: FieldValue::UnsignedInt(u64::from(site.line())),
            },
            Field {
                key: "waited_ms",
                value: FieldValue::UnsignedInt(waited_ns / 1_000_000),
            },
            Field {
                key: "handoff_ms",
                value: late_ns.map_or(FieldValue::Null, |ns| {
                    FieldValue::UnsignedInt(ns / 1_000_000)
                }),
            },
            Field {
                key: "from",
                value: from.map_or(FieldValue::Null, |from| FieldValue::Str(from.file())),
            },
            Field {
                key: "from_line",
                value: from.map_or(FieldValue::Null, |from| {
                    FieldValue::UnsignedInt(u64::from(from.line()))
                }),
            },
            Field {
                key: "task",
                value: acquired
                    .wait
                    .task
                    .map_or(FieldValue::Null, FieldValue::UnsignedInt),
            },
            Field {
                key: "parks",
                value: FieldValue::UnsignedInt(u64::from(acquired.wait.parks)),
            },
            Field {
                key: "unhanded",
                value: FieldValue::UnsignedInt(u64::from(acquired.wait.unhanded)),
            },
            Field {
                key: "unparkable",
                value: FieldValue::UnsignedInt(u64::from(acquired.wait.unparkable)),
            },
        ],
    );
}

impl<T: ?Sized> Deref for SleepGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: a live guard is proof the lock is held, so this is the only
        // reference to `data`; no other guard can exist concurrently.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> DerefMut for SleepGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: a live guard is proof of *exclusive* ownership of the lock,
        // so this `&mut` is unique — no other guard or reference exists.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for SleepGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.release(self.site);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloc::vec::Vec;

    /// The site every test release hands the lock over from.
    fn releaser() -> &'static Location<'static> {
        Location::caller()
    }

    /// A row of a thread a test plays, standing in no line.
    fn row() -> Arc<LineRow> {
        Arc::new(LineRow::new())
    }

    struct RecordingWake {
        tasks: SpinLock<Vec<TaskId>>,
        /// What the scheduler answers: `false` models a waiter it can no
        /// longer run (retired while still registered).
        wakeable: core::sync::atomic::AtomicBool,
        /// The wait clock's reading.
        now: AtomicU64,
    }

    impl RecordingWake {
        const fn new() -> Self {
            Self {
                tasks: SpinLock::new(Vec::new()),
                wakeable: core::sync::atomic::AtomicBool::new(true),
                now: AtomicU64::new(0),
            }
        }

        /// Model a scheduler that refuses every wake from here on.
        fn refuse_wakes(&self) {
            self.wakeable.store(false, Ordering::Relaxed);
        }
    }

    impl crate::waitq::WaitQueueArch for RecordingWake {
        fn unpark(&self, id: TaskId) -> bool {
            self.tasks.lock().push(id);
            self.wakeable.load(Ordering::Relaxed)
        }

        fn now_ns(&self) -> u64 {
            self.now.load(Ordering::Relaxed)
        }

        fn set_wakeup(&self, _deadline_ns: Option<u64>) {}
    }

    /// A successor learns when ownership was published and by which holder,
    /// and holds the lock under its own site.
    #[test]
    fn a_handoff_carries_its_publication_to_the_successor() {
        let lock = SleepLock::new(());
        let wake = RecordingWake::new();
        wake.now.store(1_000, Ordering::Relaxed);
        lock.state.store(LOCKED | CONTENDED, Ordering::Relaxed);
        let waiter = row();
        lock.waiters.join(&waiter, 7);

        lock.release_contended(Some(&wake), releaser());
        lock.waiters.leave(&waiter);
        let site = Location::caller();
        assert!(lock.take_handoff(8, site).is_none(), "not 8's");
        let (guard, handed) = lock.take_handoff(7, site).expect("handed to 7");
        assert_eq!(
            handed,
            Some(Handed {
                at_ns: 1_000,
                from: Some(releaser()),
            })
        );
        assert_eq!(guard.site, site);
        // Released through this test's hook rather than the guard's drop,
        // which would reach the process-wide one.
        core::mem::forget(guard);
        lock.release_contended(Some(&wake), releaser());
        assert!(lock.try_lock().is_some(), "free at last");
    }

    /// A wait past the bar with a prompt handoff, a late handoff after a
    /// short wait, a long wait that found the lock free, and a wait unable
    /// to park at its shorter bar are each recorded with how they waited; a
    /// quick acquire is not.
    #[test]
    fn a_long_wait_or_a_late_handoff_is_recorded_and_a_quick_acquire_is_not() {
        use crate::test_sink::{with_log_level, TestSink};

        let sink = TestSink::new();
        let site = Location::caller();
        let handed = |at_ns| {
            Some(Handed {
                at_ns,
                from: Some(releaser()),
            })
        };
        let parked = Wait {
            task: Some(7),
            parks: 1,
            unhanded: 0,
            unparkable: 0,
        };
        let woken_unhanded = Wait {
            parks: 2,
            unhanded: 2,
            unparkable: 3,
            ..parked
        };
        let acquired = |now_ns, handed, wait| Acquired {
            asked_ns: 0,
            now_ns,
            handed,
            wait,
        };
        with_log_level(Level::Trace, || {
            let quick = LOCK_WAIT_NS - 1;
            report_if_late(Some(&sink), site, acquired(quick, handed(quick), parked));
            report_if_late(
                Some(&sink),
                site,
                acquired(LOCK_WAIT_NS, handed(LOCK_WAIT_NS), parked),
            );
            report_if_late(Some(&sink), site, acquired(LATE_WAKE_NS, handed(0), parked));
            report_if_late(
                Some(&sink),
                site,
                acquired(LOCK_WAIT_NS, None, woken_unhanded),
            );
            report_if_late(
                Some(&sink),
                site,
                acquired(LOCK_WAIT_NS, None, Wait::default()),
            );
            let spinning = Wait {
                unparkable: 1,
                ..Wait::default()
            };
            report_if_late(
                Some(&sink),
                site,
                acquired(LATE_WAKE_NS - 1, None, spinning),
            );
            report_if_late(Some(&sink), site, acquired(LATE_WAKE_NS, None, spinning));
        });
        let events = sink.snapshot();
        assert_eq!(events.len(), 5, "{events:?}");
        let field = |at: usize, key: &str| {
            events[at]
                .fields
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        let line = alloc::format!("{}", site.line());
        let from_line = alloc::format!("{}", releaser().line());
        assert_eq!(events[0].id, AuditEvent::LockAcquiredLate.id());
        assert_eq!(field(0, "site").as_deref(), Some(site.file()));
        assert_eq!(field(0, "line"), Some(line));
        assert_eq!(field(0, "waited_ms").as_deref(), Some("1000"));
        assert_eq!(field(0, "handoff_ms").as_deref(), Some("0"));
        assert_eq!(field(0, "from").as_deref(), Some(releaser().file()));
        assert_eq!(field(0, "from_line"), Some(from_line));
        assert_eq!(field(0, "task").as_deref(), Some("7"));
        assert_eq!(field(0, "parks").as_deref(), Some("1"));
        assert_eq!(field(0, "unhanded").as_deref(), Some("0"));
        assert_eq!(field(1, "waited_ms").as_deref(), Some("250"));
        assert_eq!(field(1, "handoff_ms").as_deref(), Some("250"));
        assert_eq!(field(2, "handoff_ms").as_deref(), Some("null"));
        assert_eq!(field(2, "from").as_deref(), Some("null"));
        assert_eq!(field(2, "parks").as_deref(), Some("2"));
        assert_eq!(field(2, "unhanded").as_deref(), Some("2"));
        assert_eq!(field(2, "unparkable").as_deref(), Some("3"));
        assert_eq!(field(3, "task").as_deref(), Some("null"), "never parked");
        assert_eq!(field(3, "parks").as_deref(), Some("0"));
        assert_eq!(field(4, "waited_ms").as_deref(), Some("250"));
        assert_eq!(field(4, "unparkable").as_deref(), Some("1"));
    }

    /// A contended release at the bar is recorded with its holder, which is
    /// looked up only then; a quicker one is not.
    #[test]
    fn a_release_at_the_bar_is_recorded_with_its_holder() {
        use crate::test_sink::{with_log_level, TestSink};

        let sink = TestSink::new();
        let site = Location::caller();
        with_log_level(Level::Trace, || {
            report_if_released_late(Some(&sink), site, LATE_WAKE_NS - 1, || {
                unreachable!("a quick release names no holder")
            });
            report_if_released_late(Some(&sink), site, LATE_WAKE_NS, || Some(9));
        });
        let events = sink.snapshot();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].id, AuditEvent::LockReleasedLate.id());
        let field = |key: &str| {
            events[0]
                .fields
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(field("site").as_deref(), Some(site.file()));
        assert_eq!(field("release_ms").as_deref(), Some("250"));
        assert_eq!(field("task").as_deref(), Some("9"));
    }

    #[test]
    fn an_uncontended_lock_grants_and_releases() {
        let lock = SleepLock::new(0u32);
        {
            let mut guard = lock.lock();
            *guard = 7;
        }
        // The release made the value visible and the lock re-acquirable.
        assert_eq!(*lock.lock(), 7);
    }

    #[test]
    fn try_lock_succeeds_when_free_and_fails_while_held() {
        let lock = SleepLock::new(());
        let held = lock.try_lock().expect("free lock is acquirable");
        // A second attempt fails closed while the first guard is alive.
        assert!(lock.try_lock().is_none(), "a held lock refuses try_lock");
        drop(held);
        // Once released it is acquirable again.
        assert!(lock.try_lock().is_some(), "a released lock is acquirable");
    }

    #[test]
    fn the_guard_mutates_the_protected_value_in_place() {
        let lock = SleepLock::new(Vec::<u8>::new());
        lock.lock().push(1);
        lock.lock().push(2);
        let guard = lock.lock();
        assert_eq!(&*guard, &[1, 2]);
    }

    #[test]
    fn into_inner_returns_the_value() {
        let lock = SleepLock::new(99u64);
        assert_eq!(lock.into_inner(), 99);
    }

    #[test]
    fn a_released_lock_leaves_no_parked_waiter() {
        // With no scheduler hook installed nothing ever parks, so the
        // wait-queue stays empty across uncontended acquire/release — the
        // release path's wake is a safe no-op.
        let lock = SleepLock::new(0u8);
        {
            let _guard = lock.lock();
        }
        assert!(lock.waiters.is_empty(), "no contender was ever registered");
        assert_eq!(lock.state.load(Ordering::Acquire), 0);
    }

    #[test]
    fn an_uncontended_release_never_consults_the_wait_queue() {
        // The regression: this lock serialises every block-device operation
        // on a shared disk, and release took the wait-queue spin lock (and a
        // `BTreeMap` lookup) on *every* one just to learn that nobody was
        // waiting. The fast path is selected by the lock word alone, so a
        // registration nothing published `CONTENDED` for is not looked at —
        // which is sound because a real contender always sets that flag
        // before it parks (`take_or_contend`).
        let lock = SleepLock::new(());
        let wake = RecordingWake::new();
        lock.waiters.join(&row(), 44);
        {
            let _guard = lock.try_lock().expect("free lock is acquirable");
            assert_eq!(lock.state.load(Ordering::Acquire), LOCKED);
        }
        assert_eq!(
            lock.state.load(Ordering::Acquire),
            0,
            "the fast path unlocked"
        );
        assert_eq!(
            lock.handoff.load(Ordering::Acquire),
            0,
            "nothing handed off"
        );
        assert!(
            wake.tasks.lock().is_empty(),
            "an unpublished registration is never woken"
        );

        // Publishing the flag is what selects the wake path, and the same
        // registration is then handed the lock.
        let _guard = lock.try_lock().expect("free lock is acquirable");
        assert_eq!(lock.state.fetch_or(CONTENDED, Ordering::AcqRel), LOCKED);
        lock.release_contended(Some(&wake), releaser());
        assert_eq!(wake.tasks.lock().as_slice(), &[44]);
        assert_eq!(lock.handoff.load(Ordering::Acquire), 44);
    }

    #[test]
    fn a_stale_contention_flag_clears_itself_on_the_next_release() {
        // A contender that published the flag and then took a lock a release
        // left free leaves it set with an empty queue. That costs one release
        // which consults the queue, finds nothing, and clears the word — never
        // a permanently slow lock and never a lost wake.
        let lock = SleepLock::new(());
        let wake = RecordingWake::new();
        let guard = lock.try_lock().expect("free lock is acquirable");
        lock.state.fetch_or(CONTENDED, Ordering::AcqRel);
        drop(guard);
        assert_eq!(lock.state.load(Ordering::Acquire), 0, "the flag is cleared");
        assert!(wake.tasks.lock().is_empty());
        // And the lock is acquirable again through the plain fast path.
        assert!(lock.try_lock().is_some());
    }

    #[test]
    fn a_vanished_waiter_is_passed_over_rather_than_stranding_the_rest() {
        // A wake that finds its target already gone must not unlock with the
        // queue still occupied: the word is cleared with it, so no later
        // release would owe the remaining contenders a wake and they would
        // park for good.
        let lock = SleepLock::new(());
        let wake = RecordingWake::new();
        lock.state.store(LOCKED | CONTENDED, Ordering::Relaxed);
        let first = row();
        lock.waiters.join(&first, 51);
        lock.waiters.join(&row(), 52);
        // 51 leaves the line after joining it, exactly the window the
        // pass-over exists for.
        lock.waiters.leave(&first);

        lock.release_contended(Some(&wake), releaser());

        assert_eq!(wake.tasks.lock().as_slice(), &[52], "the next-oldest woke");
        assert_eq!(lock.handoff.load(Ordering::Acquire), 52);
        assert_ne!(
            lock.state.load(Ordering::Acquire) & LOCKED,
            0,
            "ownership is in flight, so the lock stays closed"
        );
    }

    #[test]
    fn a_waiter_the_scheduler_cannot_wake_never_wedges_the_lock() {
        // The `stress-qemu-aarch64` wedge (`plans/OPEN-DEFECTS.md` D112): a
        // loading child was retired while parked on the mount lock, so its
        // registration outlived it. The
        // handoff read "a row exists" as "the successor took it", left
        // `LOCKED` set for a task that could never claim, and every later
        // filesystem call on that mount parked for ever on a lock nobody
        // held. A wake that does not land is no successor.
        let lock = SleepLock::new(());
        let wake = RecordingWake::new();
        lock.state.store(LOCKED | CONTENDED, Ordering::Relaxed);
        lock.waiters.join(&row(), 61);
        wake.refuse_wakes();

        lock.release_contended(Some(&wake), releaser());

        assert_eq!(
            lock.state.load(Ordering::Acquire),
            0,
            "no successor took it, so the lock is free"
        );
        assert_eq!(lock.handoff.load(Ordering::Acquire), 0, "nothing in flight");
        assert!(
            lock.waiters.is_empty(),
            "the dead registration is dropped, not left at the head of the queue"
        );
        assert!(lock.try_lock().is_some(), "the lock is acquirable again");
    }

    #[test]
    fn a_dead_head_waiter_is_passed_over_for_a_live_successor() {
        // The same defect with a live contender behind the dead one: passing
        // over must remove the dead row, or the scan re-reads it for ever.
        let lock = SleepLock::new(());
        let dead = RecordingWake::new();
        dead.refuse_wakes();
        let live = RecordingWake::new();
        lock.state.store(LOCKED | CONTENDED, Ordering::Relaxed);
        let successor = row();
        lock.waiters.join(&row(), 71);
        lock.waiters.join(&successor, 72);

        // 71 is unwakeable; the scan must drop it and hand off to 72.
        assert!(!lock.hand_off_oldest(&dead, releaser()), "no wake landed");
        assert!(lock.waiters.is_empty(), "both rows were passed over");

        lock.waiters.join(&successor, 72);
        assert!(lock.hand_off_oldest(&live, releaser()));
        assert_eq!(lock.handoff.load(Ordering::Acquire), 72);
        assert_eq!(live.tasks.lock().as_slice(), &[72]);
    }

    #[test]
    fn a_designation_the_waiter_outran_deletes_nothing() {
        // The SMP boot hang (`plans/OPEN-DEFECTS.md` D129). The releaser
        // designates a waiter by reading the queue, which drops the queue
        // lock, and the waiter's row may be replaced inside that window — a
        // waiter resumed by an unrelated wake once left the queue and
        // registered again there. Deleting by task id deletes the new row,
        // the queue then reads empty, and the lock is released with
        // `CONTENDED` clear — so no later release ever consults the queue and
        // the waiter sleeps for ever on a free lock.
        //
        // The halves are driven directly because nothing else can place a
        // re-park between them deterministically.
        let lock = SleepLock::new(());
        let wake = RecordingWake::new();
        lock.state.store(LOCKED | CONTENDED, Ordering::Relaxed);
        let waiter = row();
        lock.waiters.join(&waiter, 81);

        let designated = lock
            .waiters
            .oldest_registration()
            .expect("81 is the FIFO head");

        // 81 resumes, leaves the line, and stands in it again: a new stand.
        lock.waiters.leave(&waiter);
        lock.waiters.join(&waiter, 81);

        assert!(
            !lock.offer_to(&wake, &designated, releaser()),
            "the designated park is over, so this is no successor"
        );
        assert!(
            wake.tasks.lock().is_empty(),
            "and a registration that is gone is not unparked"
        );
        assert_eq!(
            lock.handoff.load(Ordering::Acquire),
            0,
            "the publication is withdrawn, so the scan may name a successor"
        );
        assert!(
            !lock.waiters.is_empty(),
            "the live re-park survives: deleting it is the strand"
        );

        // The rescan finds that new row and hands the lock over properly.
        assert!(
            lock.hand_off_oldest(&wake, releaser()),
            "the re-parked waiter is next"
        );
        assert_eq!(lock.handoff.load(Ordering::Acquire), 81);
        assert_eq!(wake.tasks.lock().as_slice(), &[81]);
        assert_ne!(
            lock.state.load(Ordering::Acquire) & LOCKED,
            0,
            "ownership is in flight, so the lock stays closed"
        );
    }

    #[test]
    fn a_release_never_clears_the_word_over_a_live_waiter() {
        // The invariant the strand broke, asserted over the whole release:
        // either ownership is in flight to a successor, or no live row is
        // left behind. `CONTENDED` clear with a row still parked is the state
        // no later release can recover from.
        let lock = SleepLock::new(());
        let wake = RecordingWake::new();
        lock.state.store(LOCKED | CONTENDED, Ordering::Relaxed);
        lock.waiters.join(&row(), 82);
        lock.waiters.join(&row(), 83);

        lock.release_contended(Some(&wake), releaser());

        let state = lock.state.load(Ordering::Acquire);
        assert_ne!(state & LOCKED, 0, "a successor took it");
        assert_eq!(lock.handoff.load(Ordering::Acquire), 82);
        assert!(!lock.waiters.is_empty(), "83 is still owed a wake");
    }

    #[test]
    fn a_reap_only_removes_the_registration_it_was_taken_about() {
        // Defence in depth for the other half: were a wake ever to report
        // "did not land" for a task that is in fact live — which is what the
        // scheduler's `unpark` used to do when its `Parked -> Ready` claim
        // lost to a concurrent waker — the reap must still not remove a park
        // that began after the designation was read.
        /// Re-parks `task` while the wake is in flight, then refuses it —
        /// the live-task-reported-unwakeable case. Legal because the wake is
        /// issued with the line's lock released.
        struct ReparkThenRefuse<'q> {
            line: &'q Line,
            row: Arc<LineRow>,
            task: TaskId,
        }

        impl crate::waitq::WaitQueueArch for ReparkThenRefuse<'_> {
            fn unpark(&self, _id: TaskId) -> bool {
                self.line.leave(&self.row);
                self.line.join(&self.row, self.task);
                false
            }

            fn now_ns(&self) -> u64 {
                0
            }

            fn set_wakeup(&self, _deadline_ns: Option<u64>) {}
        }

        let lock = SleepLock::new(());
        let waiter = row();
        lock.waiters.join(&waiter, 84);
        let designated = lock
            .waiters
            .oldest_registration()
            .expect("84 is the FIFO head");
        let wake = ReparkThenRefuse {
            line: &lock.waiters,
            row: waiter,
            task: 84,
        };

        assert!(!lock.waiters.wake_registration(&wake, &designated));
        assert!(
            !lock.waiters.is_empty(),
            "the row present is a later park, so the reap left it alone"
        );
    }

    #[test]
    fn a_successor_that_claimed_on_a_foreign_wake_is_not_superseded() {
        // A waiter resumed by an unrelated wake — a deferred termination
        // unparking it — deregisters and claims the handoff on its way past,
        // so the releaser's own wake then finds no row. Withdrawing the
        // publication with a plain store would name a *second* successor and
        // hand two tasks the same lock.
        let lock = SleepLock::new(());
        lock.state.store(LOCKED | CONTENDED, Ordering::Relaxed);
        lock.handoff.store(91, Ordering::Release);
        assert!(lock.claim_handoff(91), "91 took ownership on its way past");

        assert!(
            !lock.retract_handoff(91),
            "nothing left to withdraw, so ownership transferred"
        );

        // The converse: an unclaimed publication is withdrawn, so the scan
        // may go on to name a real successor.
        lock.handoff.store(92, Ordering::Release);
        assert!(lock.retract_handoff(92));
        assert_eq!(lock.handoff.load(Ordering::Acquire), 0);
    }

    #[test]
    fn release_hands_ownership_to_the_oldest_waiter_without_barging() {
        let lock = SleepLock::new(());
        let wake = RecordingWake::new();
        lock.state.store(LOCKED | CONTENDED, Ordering::Relaxed);
        lock.waiters.join(&row(), 11);
        lock.waiters.join(&row(), 22);

        lock.release_contended(Some(&wake), releaser());

        assert_ne!(lock.state.load(Ordering::Acquire) & LOCKED, 0);
        assert_eq!(lock.handoff.load(Ordering::Acquire), 11);
        assert_eq!(wake.tasks.lock().as_slice(), &[11]);
        assert!(
            lock.try_lock().is_none(),
            "a fresh contender cannot barge ahead of the FIFO handoff"
        );
        assert!(!lock.claim_handoff(22));
        assert!(lock.claim_handoff(11));
    }

    #[test]
    fn repeated_release_handoffs_follow_fifo_order() {
        let lock = SleepLock::new(());
        let wake = RecordingWake::new();
        lock.state.store(LOCKED | CONTENDED, Ordering::Relaxed);
        let rows = [31, 32, 33].map(|task| (task, row()));
        for (task, waiter) in &rows {
            lock.waiters.join(waiter, *task);
        }

        for (task, waiter) in &rows {
            lock.release_contended(Some(&wake), releaser());
            assert_eq!(lock.handoff.load(Ordering::Acquire), *task);
            lock.waiters.leave(waiter);
            assert!(lock.claim_handoff(*task));
        }
        assert_eq!(wake.tasks.lock().as_slice(), &[31, 32, 33]);

        lock.release_contended(Some(&wake), releaser());
        assert_eq!(lock.state.load(Ordering::Acquire), 0);
        assert!(lock.try_lock().is_some());
    }

    #[test]
    fn a_contender_publishing_after_the_queue_scan_is_still_woken() {
        // A release that clears the word after scanning the queue erases the
        // `CONTENDED` a contender published in that window, and every later
        // release then matches the fast path and never looks at the queue
        // again — the contender sleeps for ever on a free lock (the silent
        // boot hang). The two halves of the release are driven directly
        // because nothing else can place a contender in the window between
        // them deterministically.
        let lock = SleepLock::new(());
        let wake = RecordingWake::new();
        lock.state.store(LOCKED | CONTENDED, Ordering::Relaxed);

        // The holder looks for a successor and finds none.
        assert!(
            !lock.hand_off_oldest(&wake, releaser()),
            "the queue is empty"
        );

        // A contender arrives now: it registers, finds `LOCKED` still set,
        // publishes `CONTENDED`, and parks.
        lock.waiters.join(&row(), 77);
        assert!(
            !lock.take_or_contend(),
            "the contender observed a held lock, so it parks"
        );

        // The holder finishes its release. It must see the contender.
        assert!(
            lock.release_and_recheck(),
            "a contender appeared, so the lock is retaken to hand it over"
        );
        assert!(
            lock.hand_off_oldest(&wake, releaser()),
            "the contender is the successor"
        );
        assert_eq!(
            wake.tasks.lock().as_slice(),
            &[77],
            "the contender that published after the scan must be woken"
        );
        assert_eq!(lock.handoff.load(Ordering::Acquire), 77);
    }

    #[test]
    fn a_release_with_no_contender_clears_the_whole_word() {
        // The other side of the recheck: when nobody did turn up, the
        // contention bit goes too, so the next release is one
        // compare-exchange again rather than a wait-queue lookup for ever.
        let lock = SleepLock::new(());
        lock.state.store(LOCKED | CONTENDED, Ordering::Relaxed);
        assert!(!lock.release_and_recheck(), "nobody appeared");
        assert_eq!(lock.state.load(Ordering::Acquire), 0);
    }

    /// The boot-disk stall (`plans/OPEN-DEFECTS.md` D968), contender first. A
    /// holder whose handoff scan found nobody drops `LOCKED` to look again,
    /// and a contender registers in that window. It used to see the free
    /// word, withdraw, and lose the lock to the holder's retake, which then
    /// found nobody to hand it to — and round they went. Its look now takes
    /// the free lock outright, so the holder's second look finds it owned.
    #[test]
    fn a_look_inside_a_release_takes_the_free_lock_outright() {
        let lock = SleepLock::new(());
        let wake = RecordingWake::new();
        lock.state.store(LOCKED | CONTENDED, Ordering::Relaxed);
        assert!(!lock.hand_off_oldest(&wake, releaser()), "nobody in line");

        lock.waiters.join(&row(), 95);
        lock.state.fetch_and(!LOCKED, Ordering::AcqRel);
        assert!(lock.take_or_contend(), "the look takes the free lock");

        assert!(
            !lock.recheck_after_release(),
            "the holder's second look finds the lock owned and is done"
        );
        assert_eq!(lock.state.load(Ordering::Acquire), LOCKED | CONTENDED);
        assert!(wake.tasks.lock().is_empty(), "nobody was handed anything");
    }

    /// The same window, holder first: its second look retakes the lock for
    /// the registration it finds, the contender's look then publishes
    /// contention and stays in line, and the holder's next scan hands the
    /// lock to it.
    #[test]
    fn a_retake_inside_a_release_hands_the_lock_to_the_contender_in_line() {
        let lock = SleepLock::new(());
        let wake = RecordingWake::new();
        lock.state.store(LOCKED | CONTENDED, Ordering::Relaxed);
        assert!(!lock.hand_off_oldest(&wake, releaser()), "nobody in line");

        lock.waiters.join(&row(), 96);
        assert!(lock.release_and_recheck(), "the registration is seen");
        assert!(!lock.take_or_contend(), "the retaken lock is held");

        assert!(lock.hand_off_oldest(&wake, releaser()));
        assert_eq!(wake.tasks.lock().as_slice(), &[96]);
        assert!(lock.claim_handoff(96));
    }

    /// A wake that brings nothing leaves the waiter where it stood in line:
    /// the older waiter ahead of it is handed the lock first, then it.
    #[test]
    fn a_waiter_keeps_its_place_in_line_through_a_spurious_wake() {
        let lock = SleepLock::new(());
        let wake = RecordingWake::new();
        lock.state.store(LOCKED, Ordering::Relaxed);
        let older = row();
        lock.waiters.join(&older, 40);

        let mut parks = 0;
        let mut park = || {
            parks += 1;
            if parks == 2 {
                lock.release_contended(Some(&wake), releaser());
                lock.waiters.leave(&older);
                assert!(lock.claim_handoff(40), "the older waiter is first");
                lock.release_contended(Some(&wake), releaser());
            }
            true
        };
        let mut wait = Wait::default();
        let (guard, handed) =
            lock.wait_in_line(Location::caller(), 41, &row(), &mut park, &mut wait);

        assert!(handed.is_some(), "it was handed the lock in its turn");
        assert_eq!(wake.tasks.lock().as_slice(), &[40, 41]);
        assert_eq!((wait.parks, wait.unhanded), (2, 1));
        assert!(lock.waiters.is_empty(), "it left the queue once it owned");
        drop(guard);
    }

    /// A waiter no scheduler can park spins rather than parks, and still
    /// claims the ownership a release hands it.
    #[test]
    fn a_waiter_that_cannot_park_still_claims_its_handoff() {
        let lock = SleepLock::new(());
        let wake = RecordingWake::new();
        lock.state.store(LOCKED, Ordering::Relaxed);

        let mut park = || {
            lock.release_contended(Some(&wake), releaser());
            false
        };
        let mut wait = Wait::default();
        let (_guard, handed) =
            lock.wait_in_line(Location::caller(), 42, &row(), &mut park, &mut wait);

        assert!(handed.is_some());
        assert_eq!((wait.parks, wait.unparkable), (0, 1));
        assert!(lock.waiters.is_empty());
    }

    /// A waiter that finds the lock free on its first look takes it there,
    /// without parking.
    #[test]
    fn a_waiter_that_finds_the_lock_free_takes_it_without_parking() {
        let lock = SleepLock::new(());
        lock.state.store(CONTENDED, Ordering::Relaxed);

        let mut park = || unreachable!("a free lock is taken in the look");
        let mut wait = Wait::default();
        let (_guard, handed) =
            lock.wait_in_line(Location::caller(), 43, &row(), &mut park, &mut wait);

        assert!(handed.is_none());
        assert_eq!(lock.state.load(Ordering::Acquire), LOCKED | CONTENDED);
        assert!(lock.waiters.is_empty());
    }

    /// A contender on an exhausted heap still stands in line, parks, and is
    /// handed the lock: its row was made with its thread.
    ///
    /// Regression: standing in line filed the contender in a wait queue's
    /// B-tree indices, and a refused node aborted the kernel
    /// (`plans/OPEN-DEFECTS.md` D1682).
    #[test]
    fn a_contended_acquire_on_an_exhausted_heap_parks_and_is_handed_the_lock() {
        let lock = SleepLock::new(());
        let wake = RecordingWake::new();
        lock.state.store(LOCKED, Ordering::Relaxed);
        let _ = crate::test_boot::claim_scheduler();
        let cpu = crate::test_boot::claim_cpu();
        crate::test_boot::run_claim_on(cpu);
        crate::test_boot::with_park_probe(
            cpu,
            |_| {
                // The holder's release is another party's work, and its own
                // record of the wake may use the heap.
                crate::test_alloc::refuse_allocations_on_current_thread(false);
                lock.release_contended(Some(&wake), releaser());
                crate::test_alloc::refuse_allocations_on_current_thread(true);
            },
            |probe| {
                crate::test_alloc::refuse_allocations_on_current_thread(true);
                let guard = lock.lock();
                crate::test_alloc::refuse_allocations_on_current_thread(false);
                assert_eq!(probe.parks(), 1, "it parked rather than spun");
                assert!(lock.waiters.is_empty(), "and left the line once it owned");
                drop(guard);
            },
        );
    }

    /// Joins and leaves racing a release that reaps the stand it read leave
    /// the line whole: it is empty once the row has left, and takes it again.
    #[test]
    fn a_line_stays_whole_under_a_reap_racing_a_rejoin() {
        // The interpreter preempts at random points in every round, so it
        // needs far fewer rounds than hardware to reach the racing orders.
        const ROUNDS: usize = if cfg!(miri) { 200 } else { 20_000 };
        let lock = SleepLock::new(());
        let refuse = RecordingWake::new();
        refuse.refuse_wakes();
        let waiter = row();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..ROUNDS {
                    lock.waiters.join(&waiter, 5);
                    lock.waiters.leave(&waiter);
                }
            });
            for _ in 0..ROUNDS {
                if let Some(reg) = lock.waiters.oldest_registration() {
                    let _ = lock.waiters.wake_registration(&refuse, &reg);
                }
            }
        });
        assert!(lock.waiters.is_empty(), "the row left the line");
        lock.waiters.join(&waiter, 5);
        assert!(
            !lock.waiters.is_empty(),
            "and stands in it again on joining"
        );
        lock.waiters.leave(&waiter);
        assert!(lock.waiters.is_empty());
    }

    /// A waiter woken from one park resumes on another core and parks there
    /// next time. Parking again through the core it left suspended whatever
    /// task ran there instead, putting two cores on one stack.
    #[test]
    fn a_waiter_woken_on_another_core_parks_there_next_time() {
        let lock = SleepLock::new(());
        let wake = RecordingWake::new();
        lock.state.store(LOCKED, Ordering::Relaxed);
        let _ = crate::test_boot::claim_scheduler();
        let (left, joined) = (crate::test_boot::claim_cpu(), crate::test_boot::claim_cpu());
        crate::test_boot::run_claim_on(left);
        crate::test_boot::with_park_probe(
            joined,
            |_| lock.release_contended(Some(&wake), releaser()),
            |on_joined| {
                crate::test_boot::with_park_probe(
                    left,
                    |parks| {
                        assert_eq!(parks, 1, "parked again through the core it left");
                        crate::test_boot::run_claim_on(joined);
                    },
                    |on_left| {
                        drop(lock.lock());
                        assert_eq!((on_left.parks(), on_joined.parks()), (1, 1));
                    },
                );
            },
        );
    }

    /// Contenders on several cores, each registered with the scheduler, all
    /// finish: half park through the scheduler, half cannot park. A waiter
    /// that withdrew on finding the lock free, or that spun without claiming
    /// the ownership a release handed it, wedges here, which the test reports
    /// once no contender has finished a round for a minute.
    #[test]
    fn contended_acquires_on_several_cores_all_complete() {
        use std::sync::{mpsc, Arc};
        use std::time::Duration;

        const CORES: u64 = 4;
        // The interpreter preempts at random points in every round, so it
        // needs far fewer rounds than hardware to reach the racing orders.
        const ROUNDS: u64 = if cfg!(miri) { 24 } else { 20_000 };
        const QUIET_LIMIT: u32 = 60;

        fn contend(lock: &SleepLock<u64>, rounds: &AtomicU64) {
            for _ in 0..ROUNDS {
                let mut count = lock.lock();
                *count += 1;
                for _ in 0..16 {
                    core::hint::spin_loop();
                }
                drop(count);
                rounds.fetch_add(1, Ordering::Relaxed);
            }
        }

        let lock = Arc::new(SleepLock::new(0u64));
        let rounds = Arc::new(AtomicU64::new(0));
        let (done, finished) = mpsc::channel();
        for core in 0..CORES {
            let (lock, rounds, done) = (Arc::clone(&lock), Arc::clone(&rounds), done.clone());
            std::thread::spawn(move || {
                crate::test_boot::claim_scheduler();
                if core % 2 == 0 {
                    let cpu = crate::test_boot::claim_cpu();
                    crate::test_boot::run_claim_on(cpu);
                    crate::test_boot::with_park_probe(
                        cpu,
                        |_| std::thread::yield_now(),
                        |_| contend(&lock, &rounds),
                    );
                } else {
                    contend(&lock, &rounds);
                }
                let _ = done.send(());
            });
        }
        let (mut finished_cores, mut seen, mut quiet) = (0, 0, 0);
        while finished_cores < CORES {
            if finished.recv_timeout(Duration::from_secs(1)).is_ok() {
                finished_cores += 1;
                continue;
            }
            let now = rounds.load(Ordering::Relaxed);
            quiet = if now == seen { quiet + 1 } else { 0 };
            seen = now;
            assert!(
                quiet < QUIET_LIMIT,
                "the contenders stopped at {now} rounds"
            );
        }
        assert_eq!(*lock.lock(), CORES * ROUNDS);
    }
}
