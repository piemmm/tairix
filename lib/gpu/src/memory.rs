//! What memory is charged to.

use alloc::sync::Arc;
use core::ops::Range;
use core::sync::atomic::{AtomicU64, Ordering};

use tairix_inline::ArrayVec;

use crate::Error;

/// How much of a pool is taken, against what it may give: by allocations,
/// and by the host memory objects and queued work keep on the device, held
/// to one limit together. Past it, a charge is refused with `refusal`.
///
/// Only the device's lock holder takes from a budget, while a charge is given
/// back wherever its memory is finally released. A give-back only lowers what
/// is taken, so a check made under the lock can never admit past the limit,
/// and the counters carry no ordering of their own.
#[derive(Debug)]
pub(crate) struct Budget {
    limit: u64,
    refusal: Error,
    allocations: AtomicU64,
    host: AtomicU64,
    /// A context's budget is host memory of its own, charged to the heap
    /// for as long as any charge names it.
    _itself: Option<Charge>,
}

/// What a charge holds memory for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Held {
    /// An allocation's memory.
    Allocation,
    /// The host memory an object or queued work keeps on the device.
    Host,
}

impl Budget {
    pub(crate) const fn new(limit: u64, refusal: Error) -> Self {
        Self::with(limit, refusal, None)
    }

    /// A context's budget, whose own memory `itself` charges to the heap.
    pub(crate) const fn of_context(limit: u64, itself: Charge) -> Self {
        Self::with(limit, Error::OverLimit, Some(itself))
    }

    const fn with(limit: u64, refusal: Error, itself: Option<Charge>) -> Self {
        Self {
            limit,
            refusal,
            allocations: AtomicU64::new(0),
            host: AtomicU64::new(0),
            _itself: itself,
        }
    }

    const fn counter(&self, held: Held) -> &AtomicU64 {
        match held {
            Held::Allocation => &self.allocations,
            Held::Host => &self.host,
        }
    }

    /// What `held` takes of the budget.
    pub(crate) fn taken_by(&self, held: Held) -> u64 {
        self.counter(held).load(Ordering::Relaxed)
    }

    pub(crate) fn taken(&self) -> u64 {
        self.taken_by(Held::Allocation)
            .saturating_add(self.taken_by(Held::Host))
    }

    /// Refused with its refusal where `bytes` more would pass its limit.
    pub(crate) fn admit(&self, bytes: u64) -> Result<(), Error> {
        let total = self.taken().checked_add(bytes);
        if total.is_some_and(|total| total <= self.limit) {
            Ok(())
        } else {
            Err(self.refusal)
        }
    }
}

/// What a context's memory limit is spent on.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MemoryUse {
    /// The bytes of its allocations, for as long as anything holds them.
    pub allocations: u64,
    /// The bytes of host memory its objects and queued work keep on the
    /// device, for as long as anything holds them.
    pub objects: u64,
}

impl MemoryUse {
    /// Both, as the limit counts them.
    #[must_use]
    pub const fn total(self) -> u64 {
        self.allocations.saturating_add(self.objects)
    }
}

/// Bytes taken from each of its budgets — a context's and its heap's, or
/// one alone — for what `held` names, given back to each when it is dropped.
#[derive(Debug)]
pub(crate) struct Charge {
    budgets: ArrayVec<Arc<Budget>, 2>,
    bytes: u64,
    held: Held,
}

impl Charge {
    /// Take `bytes` for `held` from every one of `budgets`, or from none,
    /// refused with the first refusing budget's error. The caller holds the
    /// device's lock.
    pub(crate) fn take(budgets: &[&Arc<Budget>], bytes: u64, held: Held) -> Result<Self, Error> {
        let mut kept = ArrayVec::new();
        for &budget in budgets {
            budget.admit(bytes)?;
            kept.try_push(Arc::clone(budget))
                .map_err(|_| Error::Malformed)?;
        }
        for budget in &kept {
            budget.counter(held).fetch_add(bytes, Ordering::Relaxed);
        }
        Ok(Self {
            budgets: kept,
            bytes,
            held,
        })
    }

    pub(crate) const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// `bytes` of this charge, at most all of it, moved into one of their
    /// own, given back apart.
    pub(crate) fn split(&mut self, bytes: u64) -> Self {
        let moved = bytes.min(self.bytes);
        self.bytes -= moved;
        Self {
            budgets: self.budgets.clone(),
            bytes: moved,
            held: self.held,
        }
    }

    /// Fold `other` into this charge, to be given back with it: refused, and
    /// `other` given back, unless both are of the same budgets and use.
    pub(crate) fn absorb(&mut self, mut other: Self) -> Result<(), Error> {
        let same = self.held == other.held
            && self.budgets.len() == other.budgets.len()
            && self
                .budgets
                .iter()
                .zip(&other.budgets)
                .all(|(a, b)| Arc::ptr_eq(a, b));
        if !same {
            return Err(Error::Malformed);
        }
        self.bytes = self.bytes.saturating_add(core::mem::take(&mut other.bytes));
        Ok(())
    }

    /// Give back `bytes` of the charge, at most all of it.
    pub(crate) fn release(&mut self, bytes: u64) {
        let given = bytes.min(self.bytes);
        self.bytes -= given;
        for budget in &self.budgets {
            budget
                .counter(self.held)
                .fetch_sub(given, Ordering::Relaxed);
        }
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.release(self.bytes);
    }
}

/// Whether two spans share a value: an empty one shares none.
pub(crate) const fn overlaps(a: &Range<u64>, b: &Range<u64>) -> bool {
    a.start < a.end && b.start < b.end && a.start < b.end && b.start < a.end
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
