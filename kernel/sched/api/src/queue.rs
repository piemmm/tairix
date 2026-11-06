//! What a run queue is built from: the slot its heap orders ready tasks in,
//! and the overflow list an entry the queue refused waits in.
//!
//! A policy's heap orders by a key — a deadline, a virtual runtime — that is
//! made unique by an arrival count, and carries the queued task itself beside
//! it, so a pick hands the dispatcher the task rather than an id it must look
//! up in the policy's registry.

use alloc::collections::VecDeque;
use core::cmp::Ordering;

use tairix_sync::SpinLock;

use crate::{SchedError, SchedResult};

/// A heap entry ordered by `key` alone, carrying the `task` it queues.
///
/// Two slots compare equal exactly when their keys do: a policy whose keys
/// end in an arrival count never has two such slots, and the task is never
/// compared, so it need not be ordered at all.
#[derive(Debug)]
pub struct Keyed<K, T> {
    /// What the heap orders by.
    pub key: K,
    /// The task queued under it.
    pub task: T,
}

impl<K: Ord, T> PartialEq for Keyed<K, T> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl<K: Ord, T> Eq for Keyed<K, T> {}

impl<K: Ord, T> PartialOrd for Keyed<K, T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<K: Ord, T> Ord for Keyed<K, T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key.cmp(&other.key)
    }
}

/// Where a ready task's entry waits when its CPU's queue refuses it — full at
/// its bound, or unable to grow — until a step drains it, oldest first.
///
/// A task holds at most one queue entry and every entry belongs to a record,
/// so room for one entry per record, taken as each record is admitted
/// ([`Self::reserve`]), means a refused entry never needs memory: a wake on an
/// exhausted heap still lands.
pub struct Overflow<T> {
    waiting: SpinLock<VecDeque<T>>,
}

impl<T> Default for Overflow<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Overflow<T> {
    /// An empty list, which allocates nothing.
    #[must_use]
    pub fn new() -> Self {
        Self {
            waiting: SpinLock::new(VecDeque::new()),
        }
    }

    /// Make room for `records` entries in all: one for every record the
    /// policy will hold.
    ///
    /// # Errors
    /// [`SchedError::OutOfMemory`] when the room cannot be had; the list is
    /// as it was.
    pub fn reserve(&self, records: usize) -> SchedResult<()> {
        let mut waiting = self.waiting.lock();
        let short = records.saturating_sub(waiting.len());
        waiting
            .try_reserve(short)
            .map_err(|_| SchedError::OutOfMemory)
    }

    /// Hold an entry its queue refused, in the room its record's admission
    /// took.
    pub fn push(&self, task: T) {
        let mut waiting = self.waiting.lock();
        debug_assert!(
            waiting.len() < waiting.capacity(),
            "an overflow entry outran the room its record took"
        );
        waiting.push_back(task);
    }

    /// Take the oldest entry.
    pub fn pop(&self) -> Option<T> {
        self.waiting.lock().pop_front()
    }

    /// Entries held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.waiting.lock().len()
    }

    /// Whether no entry is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.waiting.lock().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{Keyed, Overflow};

    use alloc::collections::BinaryHeap;
    use core::cmp::Reverse;

    /// The heap orders by key whatever the task: a task that is not even
    /// comparable comes out in key order.
    #[test]
    fn a_heap_of_slots_pops_in_key_order_carrying_each_task() {
        struct Task(&'static str);
        let mut heap = BinaryHeap::new();
        for (key, name) in [(3, "c"), (1, "a"), (2, "b")] {
            heap.push(Keyed {
                key: Reverse(key),
                task: Task(name),
            });
        }
        let order: alloc::vec::Vec<&str> =
            core::iter::from_fn(|| heap.pop().map(|slot| slot.task.0)).collect();
        assert_eq!(order, ["a", "b", "c"]);
    }

    /// Entries wait oldest first, in the room their records reserved.
    #[test]
    fn the_overflow_hands_entries_back_oldest_first() {
        let overflow = Overflow::new();
        assert_eq!(overflow.reserve(3), Ok(()));
        for task in 1..=3_u32 {
            overflow.push(task);
        }
        assert_eq!(overflow.len(), 3);
        let order: alloc::vec::Vec<u32> = core::iter::from_fn(|| overflow.pop()).collect();
        assert_eq!(order, [1, 2, 3]);
        assert!(overflow.is_empty());
    }
}
