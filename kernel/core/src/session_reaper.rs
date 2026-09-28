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

/// Start the reaper for the kernel's one capability table, admitting its task
/// through `admit`, the boot's own way of starting a kernel service.
///
/// # Errors
///
/// [`ReaperNotStarted`]; every session then ends where its anchor's death
/// lands, as before the reaper existed.
pub fn start(
    caps: &'static RwLock<CapTable>,
    audit: &'static (dyn Sink + Sync),
    admit: impl FnOnce(KernelServiceBody) -> Option<TaskId>,
) -> Result<(), ReaperNotStarted> {
    if matches!(REAPER.get(), Ok(Some(_))) {
        return Err(ReaperNotStarted::AlreadyStarted);
    }
    let reaper: &'static SessionReaper = Box::leak(Box::new(SessionReaper {
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

    /// End every session handed over, then park until the next one is.
    ///
    /// Registered on its queue before its last look at the work, so a session
    /// queued in between wakes the park rather than waiting behind it.
    fn serve(&self, yielder: &mut dyn YieldHandle) {
        let Some(task) = wait_arch().and_then(|arch| {
            arch.current_cpu()
                .and_then(|cpu| WaitQueueArch::current_task(arch, cpu))
        }) else {
            // It could never be woken, so it takes nothing and every session
            // keeps ending where its anchor's death lands.
            return;
        };
        self.serving.store(true, Ordering::Release);
        loop {
            while let Some(anchor) = self.next() {
                crate::procsignal::end_session_now(self.caps, self.audit, anchor, &mut || {
                    let _ = crate::preempt::yield_if_owed();
                });
            }
            REAPER_WAITQ.register(task, NO_DEADLINE);
            if !self.ending.lock().is_empty() {
                REAPER_WAITQ.deregister(task);
                continue;
            }
            yielder.park();
            REAPER_WAITQ.deregister(task);
        }
    }
}

#[cfg(test)]
#[path = "session_reaper_tests.rs"]
mod tests;
