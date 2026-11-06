//! The session reaper: the kernel task that ends a session once its anchor
//! has died (`docs/src/architecture/sessions.md`).
//!
//! Ending a session kills every member, and a member that is not running is
//! torn down by whoever kills it. Walked where the anchor's death landed, a
//! large session's teardowns would run back to back on that CPU with nothing
//! else able to run there. So a death path hands the session over instead:
//! the reaper walks it on its own task and offers the CPU back after every
//! member. Until the reaper is serving, and for a session it cannot queue,
//! the walk runs where the session ended.
//!
//! What outlives a session's members and ends with it — an image volume its
//! member attached — is told on a notifier task of its own, so an observer
//! that parks, flushing a slow device, holds up no session's end.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, Ordering};

use tairix_abi::ProcId;
use tairix_kernel_sched_api::TaskId;
use tairix_kernel_sec::CapTable;
use tairix_log::Sink;
use tairix_sync::once::OnceCell;
use tairix_sync::{RwLock, SpinLock};

use crate::kthread::{KernelServiceBody, YieldHandle};
use crate::waitq::{wait_arch, WaitQueue, WaitQueueArch, NO_DEADLINE};

/// The sessions waiting to be ended, and the table they are ended against.
struct SessionReaper {
    caps: &'static RwLock<CapTable>,
    audit: &'static (dyn Sink + Sync),
    /// Anchors whose sessions are ending, oldest first.
    ending: SpinLock<VecDeque<ProcId>>,
    /// Set once the reaper has shown it can park and be woken; before that
    /// nothing is handed to it.
    serving: AtomicBool,
}

static REAPER: OnceCell<&'static SessionReaper> = OnceCell::new();

/// What outlives a session's members and ends with the session: told on a
/// task that may park, once or more after sessions end.
pub trait SessionObserver: Sync {
    /// Sessions have ended; `closed` answers whether a given one has. Asked
    /// of every session the observer holds, so word of several ends arrives
    /// as one telling, and one that ended where no task could be told is
    /// caught at the next.
    fn session_ended(&self, closed: &dyn Fn(ProcId) -> bool);

    /// Do one piece of the work the observer owes — work the telling, or a
    /// task that could not park, left it — answering whether it did any.
    /// Several tasks may ask at once, so one that takes a piece while more is
    /// owed calls [`owe_work`] for the next. May park.
    fn settle(&self) -> bool;

    /// Whether [`settle`](Self::settle) has work it could do now.
    fn owes(&self) -> bool;
}

static OBSERVER: OnceCell<&'static dyn SessionObserver> = OnceCell::new();

/// Have `observer` told of every session that ends. First wins.
pub fn observe(observer: &'static dyn SessionObserver) {
    let _ = OBSERVER.set(observer);
}

/// How many tasks the observer is told on and settles its work on, each
/// settling one piece at a time. It hands each piece to one task, so a second
/// keeps the rest moving while one piece is slow; a third and fourth spread
/// several slow ones.
pub const NOTIFIER_TASKS: usize = 4;

/// The tasks the observer is told on.
struct Notifier {
    caps: &'static RwLock<CapTable>,
    /// A session has ended since the observer was last told.
    owed: SpinLock<bool>,
    /// Set once it has shown it can park and be woken; before that a session's
    /// end is told where it lands, on a caller that may park.
    serving: AtomicBool,
    /// Where its tasks park while it owes the observer nothing.
    parked: WaitQueue,
}

static NOTIFIER: OnceCell<&'static Notifier> = OnceCell::new();

/// Where the reaper parks while no session is ending.
static REAPER_WAITQ: WaitQueue = WaitQueue::new();

/// Why [`start`] did not start the reaper.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ReaperNotStarted {
    /// A reaper is already running for this boot.
    AlreadyStarted,
    /// The boot could not admit its task.
    NotAdmitted,
}

/// Start the reaper, and the notifier its observer is told on, for the
/// kernel's one capability table, admitting their tasks through `admit`, the
/// boot's own way of starting a kernel service. A notifier it cannot admit
/// leaves the reaper to tell the observer on its own task; a reaper it cannot
/// admit leaves every session to end where its anchor's death lands, still
/// told on the notifier.
///
/// # Errors
///
/// [`ReaperNotStarted`]. With neither admitted, the observer is told only by
/// a caller that may park, so what it owes waits for its own owner.
pub fn start(
    caps: &'static RwLock<CapTable>,
    audit: &'static (dyn Sink + Sync),
    mut admit: impl FnMut(KernelServiceBody) -> Option<TaskId>,
) -> Result<(), ReaperNotStarted> {
    if matches!(REAPER.get(), Ok(Some(_))) {
        return Err(ReaperNotStarted::AlreadyStarted);
    }
    let notifier: &'static Notifier = crate::forever(Box::new(Notifier::over(caps)));
    let mut admitted = false;
    for _ in 0..NOTIFIER_TASKS {
        admitted |= admit(Box::new(move |yielder: &mut dyn YieldHandle| {
            notifier.serve(yielder);
        }))
        .is_some();
    }
    if admitted {
        let _ = NOTIFIER.set(notifier);
    }
    let reaper: &'static SessionReaper = crate::forever(Box::new(SessionReaper {
        caps,
        audit,
        ending: SpinLock::new(VecDeque::new()),
        serving: AtomicBool::new(false),
    }));
    admit(Box::new(move |yielder: &mut dyn YieldHandle| {
        reaper.serve(yielder);
    }))
    .ok_or(ReaperNotStarted::NotAdmitted)?;
    REAPER
        .set(reaper)
        .map_err(|_| ReaperNotStarted::AlreadyStarted)
}

/// Have the observer told that a session over `caps` has ended: by the
/// notifier where it serves, else at once on a caller that `may_park`, else
/// with the next session that ends on one.
pub(crate) fn session_over(caps: &RwLock<CapTable>, may_park: bool) {
    if let (Ok(Some(notifier)), Some(arch)) = (NOTIFIER.get(), wait_arch()) {
        if core::ptr::eq(caps, notifier.caps) && notifier.tell_ended(arch) {
            return;
        }
    }
    if may_park {
        tell(caps);
        while settle() {}
    }
}

/// Wake a notifier task to settle work the observer has come to owe on a task
/// that could not do it; a task that takes a piece wakes the next only while
/// more is owed. Where no notifier serves, the work waits for the next
/// session's end on a caller that may park.
pub fn owe_work() {
    if let (Ok(Some(notifier)), Some(arch)) = (NOTIFIER.get(), wait_arch()) {
        notifier.wake(arch);
    }
}

/// Tell the observer which sessions over `caps` have ended, each judged under
/// a hold of the table of its own: a closed session stays closed, and the
/// observer's own work runs with the table free. May park.
fn tell(caps: &RwLock<CapTable>) {
    if let Ok(Some(observer)) = OBSERVER.get() {
        observer.session_ended(&|session| caps.read().sessions().closed(session));
    }
}

/// Have the observer do one piece of what it owes, answering whether it did.
/// May park.
fn settle() -> bool {
    matches!(OBSERVER.get(), Ok(Some(observer)) if observer.settle())
}

/// Whether the observer owes work a [`settle`] would do now.
fn owes() -> bool {
    matches!(OBSERVER.get(), Ok(Some(observer)) if observer.owes())
}

/// The calling task, if the scheduler can park and wake it.
fn parkable_task() -> Option<u64> {
    wait_arch().and_then(|arch| {
        arch.current_cpu()
            .and_then(|cpu| WaitQueueArch::current_task(arch, cpu))
    })
}

/// Hand the session anchored at `anchor`, over `caps`, to the reaper,
/// answering whether it took it. It does not while it is not yet serving, for
/// a table other than its own, or when its queue cannot grow.
pub(crate) fn hand_over(caps: &RwLock<CapTable>, anchor: ProcId) -> bool {
    let (Ok(Some(reaper)), Some(arch)) = (REAPER.get(), wait_arch()) else {
        return false;
    };
    if !reaper.queue(caps, anchor) {
        return false;
    }
    REAPER_WAITQ.wake_all(arch);
    true
}

impl SessionReaper {
    fn queue(&self, caps: &RwLock<CapTable>, anchor: ProcId) -> bool {
        if !self.serving.load(Ordering::Acquire) || !core::ptr::eq(caps, self.caps) {
            return false;
        }
        let mut ending = self.ending.lock();
        if ending.try_reserve(1).is_err() {
            return false;
        }
        ending.push_back(anchor);
        true
    }

    fn next(&self) -> Option<ProcId> {
        self.ending.lock().pop_front()
    }

    /// End every session handed over, having the observer told of each.
    fn end_queued(&self) {
        while let Some(anchor) = self.next() {
            crate::procsignal::end_session_now(self.caps, self.audit, anchor, &mut || {
                let _ = crate::preempt::yield_if_owed();
            });
            session_over(self.caps, true);
        }
    }

    /// End every session handed over, then park until the next one is.
    ///
    /// Registered on its queue before its last look at the work, so a session
    /// queued in between wakes the park rather than waiting behind it.
    fn serve(&self, yielder: &mut dyn YieldHandle) {
        let Some(task) = parkable_task() else {
            // It could never be woken, so it takes nothing and every session
            // keeps ending where its anchor's death lands.
            return;
        };
        // Filed once for the service's life, so no later park asks the heap;
        // refused, sessions keep ending where their anchors' deaths land.
        if REAPER_WAITQ.register(task, NO_DEADLINE).is_err() {
            return;
        }
        self.serving.store(true, Ordering::Release);
        loop {
            self.end_queued();
            if self.ending.lock().is_empty() {
                yielder.park();
            }
        }
    }
}

impl Notifier {
    fn over(caps: &'static RwLock<CapTable>) -> Self {
        Self {
            caps,
            owed: SpinLock::new(false),
            serving: AtomicBool::new(false),
            parked: WaitQueue::new(),
        }
    }

    /// Owe the observer word of an ended session and wake one task to give
    /// it, answering whether one will: it wakes the next for what the telling
    /// leaves owed.
    fn tell_ended(&self, arch: &dyn WaitQueueArch) -> bool {
        if !self.owe() {
            return false;
        }
        self.parked.wake_one(arch);
        true
    }

    /// Wake one task, while it serves, to settle work owed.
    fn wake(&self, arch: &dyn WaitQueueArch) {
        if self.serving.load(Ordering::Acquire) {
            self.parked.wake_one(arch);
        }
    }

    /// Owe the observer word of an ended session, answering whether this
    /// task will give it.
    fn owe(&self) -> bool {
        if !self.serving.load(Ordering::Acquire) {
            return false;
        }
        *self.owed.lock() = true;
        true
    }

    /// Whether word is owed, settling it.
    fn take_owed(&self) -> bool {
        core::mem::take(&mut *self.owed.lock())
    }

    /// Tell the observer of the sessions ended since it was last told, if
    /// any have, then settle everything it owes.
    fn round(&self) {
        if self.take_owed() {
            tell(self.caps);
        }
        while settle() {}
    }

    /// Tell the observer of each run of ended sessions and settle what it
    /// owes, then park until another ends or more work is owed; registered
    /// before its last look, as the reaper is.
    fn serve(&self, yielder: &mut dyn YieldHandle) {
        let Some(task) = parkable_task() else {
            return;
        };
        // Filed once for the service's life, as the reaper's is.
        if self.parked.register(task, NO_DEADLINE).is_err() {
            return;
        }
        self.serving.store(true, Ordering::Release);
        loop {
            self.round();
            if !*self.owed.lock() && !owes() {
                yielder.park();
            }
        }
    }
}

#[cfg(test)]
#[path = "session_reaper_tests.rs"]
mod tests;
