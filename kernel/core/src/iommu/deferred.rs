//! Frees batched behind one invalidation (`plans/IOMMU.md` IOM20.1).
//!
//! A carve its live owner frees leaves its domain's tables at once, and its
//! frames wait, beside the carves freed near it, for the one invalidation of
//! each domain that confirms them all gone; only then are they scrubbed and
//! freed. A batch is confirmed once it holds [`BATCH_CARVES`] carves or its
//! first has waited [`BATCH_WINDOW_NS`], whichever comes first. A carve the
//! batch has no room for, by count or by the memory a batch may hold back, is
//! confirmed gone alone.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_kernel_iommu_api::IommuError;
use tairix_kernel_mem::{Chunks, DmaError, FrameBlock};
use tairix_kernel_sched_api::TaskId;
use tairix_log::{Field, FieldValue, Level, Sink};

use super::{Owner, OwnerState, Translation};
use crate::audit::{emit, AuditEvent};
use crate::kthread::{KernelServiceBody, YieldHandle};
use crate::waitq::{wait_now_ns, Parker, DEFERRED_FREE_WAITQ};

/// Carves a batch holds before it is confirmed whatever its window.
pub const BATCH_CARVES: usize = 64;

/// How long the first carve of a batch waits for others to join it.
pub const BATCH_WINDOW_NS: u64 = 10_000_000;

/// The share of memory, as a divisor of it, a batch may hold back.
const HELD_SHARE: u64 = 256;

/// One carve waiting: whose domain it left, and its frames.
struct Waiting {
    owner: Arc<Owner>,
    blocks: Chunks,
}

/// The carves waiting for their invalidation.
pub(super) struct Batch {
    /// A flusher was admitted: there is never a second.
    admitted: bool,
    /// The flusher has proved its park: until it has, nothing waits.
    serving: bool,
    /// Both buffers hold a full batch from admission on, so a carve that was
    /// given room never allocates.
    waiting: Vec<Waiting>,
    /// The emptied buffer of the batch last confirmed.
    spare: Vec<Waiting>,
    /// Carves given room that are still leaving their tables, and their bytes.
    reserved: usize,
    reserved_bytes: u64,
    /// The bytes of frames waiting.
    bytes: u64,
    budget: u64,
    /// When the oldest waiting carve's window closes.
    due: Option<u64>,
}

impl Batch {
    pub(super) const fn new() -> Self {
        Self {
            admitted: false,
            serving: false,
            waiting: Vec::new(),
            spare: Vec::new(),
            reserved: 0,
            reserved_bytes: 0,
            bytes: 0,
            budget: 0,
            due: None,
        }
    }

    /// Give a carve of `bytes` room, answering whether there was any.
    fn reserve(&mut self, bytes: u64) -> bool {
        let room = self.serving
            && self.waiting.len() + self.reserved < BATCH_CARVES
            && self
                .bytes
                .saturating_add(self.reserved_bytes)
                .saturating_add(bytes)
                <= self.budget;
        if room {
            self.reserved += 1;
            self.reserved_bytes += bytes;
        }
        room
    }

    fn unreserve(&mut self, bytes: u64) {
        self.reserved = self.reserved.saturating_sub(1);
        self.reserved_bytes = self.reserved_bytes.saturating_sub(bytes);
    }

    /// Move a carve given room into the batch at `now`, answering when the
    /// flusher must now wake by, where this carve brought that sooner.
    fn commit(&mut self, waiting: Waiting, bytes: u64, now: u64) -> Option<u64> {
        self.unreserve(bytes);
        self.waiting.push(waiting);
        self.bytes += bytes;
        if self.waiting.len() >= BATCH_CARVES {
            return Some(now);
        }
        if self.due.is_some() {
            return None;
        }
        let due = now.saturating_add(BATCH_WINDOW_NS);
        self.due = Some(due);
        Some(due)
    }

    fn is_due(&self, now: u64) -> bool {
        self.waiting.len() >= BATCH_CARVES || self.due.is_some_and(|due| due <= now)
    }

    /// The waiting carves, the spare buffer left in their place.
    fn take(&mut self) -> Vec<Waiting> {
        self.bytes = 0;
        self.due = None;
        let spare = core::mem::take(&mut self.spare);
        core::mem::replace(&mut self.waiting, spare)
    }

    /// Admit the one flusher, batches holding at most `budget` bytes; [`false`]
    /// for a second, or no room for a full batch.
    fn admit(&mut self, budget: u64) -> bool {
        if self.admitted
            || self.waiting.try_reserve_exact(BATCH_CARVES).is_err()
            || self.spare.try_reserve_exact(BATCH_CARVES).is_err()
        {
            return false;
        }
        self.admitted = true;
        self.budget = budget;
        true
    }
}

/// Scrubs and frees blocks an invalidation confirmed gone, keeping for good
/// any it cannot scrub.
pub type Release<'a> = dyn Fn(&[FrameBlock]) + Sync + 'a;

impl Translation {
    /// Take the carve at `iova` out of `owner`'s domain and its `blocks` into
    /// the batch, answering whether it did; a carve it declined is still
    /// mapped, for the caller to confirm gone itself.
    pub(super) fn defer(
        &self,
        node: u32,
        owner: &Arc<Owner>,
        iova: u64,
        blocks: &mut Chunks,
    ) -> Result<bool, DmaError> {
        let Some(now) = wait_now_ns() else {
            return Ok(false);
        };
        let bytes = blocks.iter().map(|block| block.len() as u64).sum::<u64>();
        // Room is held before the carve leaves its tables: once out it is
        // confirmed with the batch, and its IOVA may be another carve's
        // before the caller could unmap it.
        if !self.batch.lock().reserve(bytes) {
            return Ok(false);
        }
        let removed = match &mut *owner.state.lock() {
            OwnerState::Live(domain) => domain.remove(iova),
            // Its end confirmed every carve gone, or kept them all.
            _ => Err(IommuError::NotMapped),
        };
        let mut batch = self.batch.lock();
        if let Err(err) = removed {
            batch.unreserve(bytes);
            drop(batch);
            return match err {
                IommuError::NotMapped | IommuError::Exhausted => Ok(false),
                _ => self.note(node, owner, Err(DmaError::Unconfirmed)),
            };
        }
        let carve = Waiting {
            owner: Arc::clone(owner),
            blocks: core::mem::take(blocks),
        };
        let wake = batch.commit(carve, bytes, now);
        drop(batch);
        if let Some(wake) = wake {
            DEFERRED_FREE_WAITQ.wake_by(wake);
        }
        Ok(true)
    }

    /// Confirm the batch if it is due at `now`, releasing the frames of every
    /// carve its unit confirmed gone, and answer when the next batch is due.
    pub(crate) fn flush_due(&self, now: u64, release: &Release<'_>) -> Option<u64> {
        let mut taken = {
            let mut batch = self.batch.lock();
            if !batch.is_due(now) {
                return batch.due;
            }
            batch.take()
        };
        // One owner's carves together, so its domain is invalidated once.
        taken.sort_unstable_by_key(|waiting| Arc::as_ptr(&waiting.owner).addr());
        for carves in taken.chunk_by(|a, b| Arc::ptr_eq(&a.owner, &b.owner)) {
            let Some(Waiting { owner, .. }) = carves.first() else {
                continue;
            };
            let confirmed = match &mut *owner.state.lock() {
                OwnerState::Live(domain) => domain.confirm_removed().is_ok(),
                OwnerState::Revoked { confirmed } => *confirmed,
                OwnerState::Adopting | OwnerState::Unadopted(_) => false,
            };
            if confirmed {
                for waiting in carves {
                    release(&waiting.blocks);
                }
            } else {
                // Frames no invalidation confirmed gone are kept for good.
                let _ = self.note(owner.node, owner, Err::<(), _>(DmaError::Unconfirmed));
            }
        }
        taken.clear();
        let mut batch = self.batch.lock();
        batch.spare = taken;
        batch.due
    }

    /// Admit through `admit` the one task that confirms batches, each holding
    /// at most `budget` bytes of frames freed through `release`. A flusher
    /// that is refused, or cannot park, is audited, and every free is then
    /// confirmed alone.
    pub fn serve_frees(
        &'static self,
        budget: u64,
        release: &'static Release<'static>,
        admit: impl FnOnce(KernelServiceBody) -> Option<TaskId>,
    ) {
        if !self.batch.lock().admit(budget) {
            audit_unbatched(self.audit, "no_room");
            return;
        }
        let body = move |yielder: &mut dyn YieldHandle| self.confirm_batches(yielder, release);
        if admit(Box::new(body)).is_none() {
            audit_unbatched(self.audit, "not_admitted");
        }
    }

    /// Park until a batch is due, confirm it, and repeat for good; nothing
    /// waits until the park is proven.
    fn confirm_batches(&self, yielder: &mut dyn YieldHandle, release: &Release<'_>) {
        let Some(parker) = Parker::current() else {
            audit_unbatched(self.audit, "cannot_park");
            return;
        };
        parker.register(&DEFERRED_FREE_WAITQ, None);
        self.batch.lock().serving = true;
        loop {
            yielder.park();
            parker.rearm(&DEFERRED_FREE_WAITQ, |now| self.flush_due(now, release));
        }
    }

    /// The share of memory of `total` bytes a batch may hold back.
    #[must_use]
    pub const fn batch_budget(total: u64) -> u64 {
        total / HELD_SHARE
    }
}

/// Record that no flusher confirms batches, naming why: every free then waits
/// for an invalidation of its own.
fn audit_unbatched(audit: &(dyn Sink + Sync), cause: &'static str) {
    emit(
        audit,
        Level::Warn,
        AuditEvent::DmaFreesUnbatched,
        &[Field {
            key: "cause",
            value: FieldValue::Str(cause),
        }],
    );
}

#[cfg(test)]
impl Translation {
    /// Open the batch as a flusher that has parked does, for a test that
    /// confirms it with [`Self::flush_due`] itself.
    pub(super) fn open_batch(&self, budget: u64) {
        let mut batch = self.batch.lock();
        assert!(batch.admit(budget));
        batch.serving = true;
    }
}
