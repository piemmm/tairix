//! What memory is charged to, and the software device's memory itself.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use tairix_inline::ArrayVec;

use crate::fair::{Fair, FairGuard};
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

/// One allocation's memory on the software device: little-endian words,
/// zeroed when made, and the charge they hold until the last holder lets
/// them go.
///
/// The words have their own lock, so the CPU's reads and writes and the work
/// running over them never touch them at once, and neither takes the
/// device's lock to do it. Lockers have the words in turn, in the order they
/// asked, so neither a CPU thread locking again at once nor the work can keep
/// the other out. Work holds the lock for a whole command, and shares the
/// words between the cores it runs on as atomic views.
pub(crate) struct Memory {
    words: Fair<Vec<u32>>,
    charge: Charge,
    _overhead: Charge,
}

/// A memory's words, held in its locker's turn.
pub(crate) type Words<'m> = FairGuard<'m, Vec<u32>>;

/// Freeing the words takes as long as they are large, so no fair lock is held
/// as they go.
#[cfg(test)]
impl Drop for Memory {
    fn drop(&mut self) {
        crate::fair::held::note_drop();
    }
}

impl fmt::Debug for Memory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Memory")
            .field("bytes", &self.charge.bytes())
            .finish_non_exhaustive()
    }
}

impl Memory {
    /// The host bytes an allocation of `len` keeps besides those it was
    /// asked for: its words rounded up whole, and the memory itself as its
    /// holders share it.
    pub(crate) const fn overhead(len: u64) -> u64 {
        let rounding = (4 - len % 4) % 4;
        rounding + crate::model::shared_bytes::<Self>()
    }

    /// `len` zeroed bytes under `charge`, held as whole words, and what they
    /// keep besides under `overhead`.
    pub(crate) fn zeroed(len: u64, charge: Charge, overhead: Charge) -> Result<Self, Error> {
        let words = usize::try_from(len.div_ceil(4)).map_err(|_| Error::OutOfMemory)?;
        let words = tairix_util::fallible::filled(words, 0u32).ok_or(Error::OutOfMemory)?;
        Ok(Self {
            words: Fair::new(words),
            charge,
            _overhead: overhead,
        })
    }

    /// The words, once every locker that asked before has had them.
    pub(crate) fn lock(&self) -> Words<'_> {
        self.words.lock()
    }

    /// Lock `a` and `b`, which are different memories, in one order every
    /// caller shares, so two lockers of the same pair never wait on each
    /// other.
    pub(crate) fn lock_pair<'m>(a: &'m Self, b: &'m Self) -> (Words<'m>, Words<'m>) {
        if core::ptr::from_ref(a) < core::ptr::from_ref(b) {
            let first = a.lock();
            (first, b.lock())
        } else {
            let second = b.lock();
            (a.lock(), second)
        }
    }
}

/// Copy `out.len()` bytes from byte `offset` of `words` into `out`; `None`
/// past the words.
pub(crate) fn read_bytes(words: &[u32], offset: usize, out: &mut [u8]) -> Option<()> {
    let end = offset.checked_add(out.len())?;
    if end > words.len().checked_mul(4)? {
        return None;
    }
    let lead = (4 - offset % 4) % 4;
    let (head, rest) = out.split_at_mut(lead.min(out.len()));
    if let Some(word) = words.get(offset / 4) {
        let within = offset % 4;
        head.copy_from_slice(word.to_le_bytes().get(within..within + head.len())?);
    }
    let first = (offset + head.len()) / 4;
    let (middle, tail) = rest.as_chunks_mut::<4>();
    let whole = words.get(first..first + middle.len())?;
    for (bytes, word) in middle.iter_mut().zip(whole) {
        *bytes = word.to_le_bytes();
    }
    if let Some(word) = words.get(first + middle.len()) {
        tail.copy_from_slice(word.to_le_bytes().get(..tail.len())?);
    }
    Some(())
}

/// Copy `data` into `words` from byte `offset`; `None`, changing nothing,
/// past the words.
pub(crate) fn write_bytes(words: &mut [u32], offset: usize, data: &[u8]) -> Option<()> {
    let end = offset.checked_add(data.len())?;
    if end > words.len().checked_mul(4)? {
        return None;
    }
    let lead = (4 - offset % 4) % 4;
    let (head, rest) = data.split_at(lead.min(data.len()));
    patch(words, offset, head)?;
    let first = (offset + head.len()) / 4;
    let (middle, tail) = rest.as_chunks::<4>();
    let whole = words.get_mut(first..first + middle.len())?;
    for (word, bytes) in whole.iter_mut().zip(middle) {
        *word = u32::from_le_bytes(*bytes);
    }
    patch(words, (first + middle.len()) * 4, tail)
}

/// Write `bytes`, which lie within one word, at byte `offset` of `words`.
fn patch(words: &mut [u32], offset: usize, bytes: &[u8]) -> Option<()> {
    if bytes.is_empty() {
        return Some(());
    }
    let word = words.get_mut(offset / 4)?;
    let within = offset % 4;
    let mut current = word.to_le_bytes();
    current
        .get_mut(within..within + bytes.len())?
        .copy_from_slice(bytes);
    *word = u32::from_le_bytes(current);
    Some(())
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
