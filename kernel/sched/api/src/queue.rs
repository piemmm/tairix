//! The slot a run-queue heap orders its ready tasks in.
//!
//! A policy's heap orders by a key — a deadline, a virtual runtime — that is
//! made unique by an arrival count, and carries the queued task itself beside
//! it, so a pick hands the dispatcher the task rather than an id it must look
//! up in the policy's registry.

use core::cmp::Ordering;

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

#[cfg(test)]
mod tests {
    use super::Keyed;

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
}
