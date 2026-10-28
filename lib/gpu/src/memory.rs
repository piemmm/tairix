//! What memory is charged to, and the software device's memory itself.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use tairix_rt::sync::{Mutex, MutexGuard};

use crate::Error;

/// How much of a pool is taken, against what it may give.
///
/// Only the device's lock holder takes from a budget, while a charge is given
/// back wherever its memory is finally released. A give-back only lowers what
/// is taken, so a check made under the lock can never admit past the limit,
/// and the counter carries no ordering of its own.
#[derive(Debug)]
pub(crate) struct Budget {
    limit: u64,
    taken: AtomicU64,
}

impl Budget {
    pub(crate) const fn new(limit: u64) -> Self {
        Self {
            limit,
            taken: AtomicU64::new(0),
        }
    }

    pub(crate) fn taken(&self) -> u64 {
        self.taken.load(Ordering::Relaxed)
    }

    fn admits(&self, bytes: u64) -> bool {
        self.taken()
            .checked_add(bytes)
            .is_some_and(|total| total <= self.limit)
    }
}

/// Bytes taken from a context's budget and its heap's, given back to both
/// when it is dropped.
#[derive(Debug)]
pub(crate) struct Charge {
    context: Arc<Budget>,
    heap: Arc<Budget>,
    bytes: u64,
}

impl Charge {
    /// Take `bytes` from both budgets, or from neither. The caller holds the
    /// device's lock.
    pub(crate) fn take(
        context: &Arc<Budget>,
        heap: &Arc<Budget>,
        bytes: u64,
    ) -> Result<Self, Error> {
        if !context.admits(bytes) {
            return Err(Error::OverLimit);
        }
        if !heap.admits(bytes) {
            return Err(Error::HeapFull);
        }
        context.taken.fetch_add(bytes, Ordering::Relaxed);
        heap.taken.fetch_add(bytes, Ordering::Relaxed);
        Ok(Self {
            context: Arc::clone(context),
            heap: Arc::clone(heap),
            bytes,
        })
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.context.taken.fetch_sub(self.bytes, Ordering::Relaxed);
        self.heap.taken.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

/// One allocation's bytes on the software device, zeroed when made, and the
/// charge they hold until the last holder lets them go.
///
/// The bytes have their own lock, so the CPU's reads and writes and the
/// work running over them never touch them at once, and neither takes the
/// device's lock to do it.
pub(crate) struct Memory {
    bytes: Mutex<Vec<u8>>,
    charge: Charge,
}

impl fmt::Debug for Memory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Memory")
            .field("bytes", &self.charge.bytes)
            .finish_non_exhaustive()
    }
}

impl Memory {
    /// `len` zeroed bytes under `charge`.
    pub(crate) fn zeroed(len: u64, charge: Charge) -> Result<Self, Error> {
        let len = usize::try_from(len).map_err(|_| Error::OutOfMemory)?;
        let bytes = tairix_util::fallible::filled(len, 0u8).ok_or(Error::OutOfMemory)?;
        Ok(Self {
            bytes: Mutex::new(bytes),
            charge,
        })
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Vec<u8>> {
        self.bytes.lock()
    }

    /// Lock `a` and `b`, which are different memories, in one order every
    /// caller shares, so two lockers of the same pair never wait on each
    /// other.
    pub(crate) fn lock_pair<'m>(
        a: &'m Self,
        b: &'m Self,
    ) -> (MutexGuard<'m, Vec<u8>>, MutexGuard<'m, Vec<u8>>) {
        if core::ptr::from_ref(a) < core::ptr::from_ref(b) {
            let first = a.lock();
            (first, b.lock())
        } else {
            let second = b.lock();
            (a.lock(), second)
        }
    }
}
