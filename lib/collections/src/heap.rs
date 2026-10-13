//! [`IndexedHeap`]: a binary min-heap over a holder's own dense slot numbers,
//! each slot holding at most one priority that can be moved or withdrawn by
//! slot.
//!
//! `alloc::BinaryHeap` can neither find an entry nor move one, so a deadline
//! that moves is pushed again and its stale copy skipped when it surfaces,
//! the heap growing with every move. Here each slot's place in the heap is
//! recorded, so moving a priority either way and removing a slot cost
//! O(log n) and leave nothing behind, the least is read in O(1), and every
//! entry at or below a bound is walked in time proportional to how many there
//! are, however many are held.
//!
//! Slots are the holder's own dense indices — an arena slot, a table row — and
//! the heap keeps room for every slot below the largest it has reserved, so a
//! sparse holder pays for the gaps. [`IndexedHeap::try_reserve_slot`] makes
//! that room and it is never given back: inserting any slot below a reserved
//! one cannot fail, so a holder changing several tables at once can make every
//! allocation before changing any.

use alloc::vec::Vec;
use core::alloc::Layout;

use crate::TryReserveError;

/// A slot's place while it is not held.
const ABSENT: usize = usize::MAX;

/// One held slot and its priority, kept side by side so a sift compares
/// without leaving the heap array.
#[derive(Copy, Clone, Debug)]
struct Entry<P> {
    priority: P,
    slot: usize,
}

/// A min-heap of slots keyed by priority, re-prioritised and removed by slot.
///
/// Ties are broken arbitrarily. A slot is its holder's until withdrawn: a
/// holder hands a slot number on only after [`Self::remove`]ing it, since
/// [`Self::try_insert`] on a slot still held moves that slot rather than
/// inserting afresh, and only its return value shows the priority it
/// replaced.
#[derive(Debug)]
pub struct IndexedHeap<P> {
    heap: Vec<Entry<P>>,
    /// Per slot, its place in `heap`, or [`ABSENT`].
    places: Vec<usize>,
}

impl<P> Default for IndexedHeap<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P> IndexedHeap<P> {
    /// An empty heap, allocating nothing until a slot is inserted.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            heap: Vec::new(),
            places: Vec::new(),
        }
    }

    /// How many slots are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// Whether no slot is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// Whether `slot` is held.
    #[must_use]
    pub fn contains(&self, slot: usize) -> bool {
        self.place_of(slot).is_some()
    }

    /// Make room for `slot` and every slot below it, so no
    /// [`Self::try_insert`] of any of them can fail. Changes nothing a reader
    /// can see.
    ///
    /// # Errors
    ///
    /// [`TryReserveError::CapacityOverflow`] when `slot + 1` places or
    /// entries have no representable layout, which no retry can change, and
    /// [`TryReserveError::AllocFailed`] when the heap or its place table
    /// cannot grow. Neither changes anything a reader can see; room one table
    /// gained before the other failed is kept for the next attempt.
    pub fn try_reserve_slot(&mut self, slot: usize) -> Result<(), TryReserveError> {
        if slot < self.places.len() {
            return Ok(());
        }
        let room = slot
            .checked_add(1)
            .filter(|&room| Layout::array::<usize>(room).is_ok())
            .filter(|&room| Layout::array::<Entry<P>>(room).is_ok())
            .ok_or(TryReserveError::CapacityOverflow)?;
        // The heap never holds more entries than there are places, so storage
        // for one entry a place keeps every insertion from growing it.
        self.places
            .try_reserve(room - self.places.len())
            .map_err(|_| TryReserveError::AllocFailed)?;
        self.heap
            .try_reserve(room - self.heap.len())
            .map_err(|_| TryReserveError::AllocFailed)?;
        self.places.resize(room, ABSENT);
        Ok(())
    }

    /// Remove every slot, keeping the room made for them.
    pub fn clear(&mut self) {
        let Self { heap, places } = self;
        for entry in heap.iter() {
            places[entry.slot] = ABSENT;
        }
        heap.clear();
    }

    /// Every held slot whose priority is no greater than `bound`, with that
    /// priority, in no particular order.
    ///
    /// Costs O(k) for k entries returned: a heap's entries at or below a
    /// bound are a subtree at its root, so the walk stops at the first entry
    /// past the bound on every path and allocates nothing.
    pub fn up_to(&self, bound: P) -> UpTo<'_, P> {
        UpTo {
            heap: &self.heap,
            bound,
            next: Some(0),
        }
    }

    fn place_of(&self, slot: usize) -> Option<usize> {
        self.places.get(slot).copied().filter(|&at| at != ABSENT)
    }
}

impl<P: Copy + Ord> IndexedHeap<P> {
    /// The priority `slot` holds, or `None` if it is not held.
    #[must_use]
    pub fn get(&self, slot: usize) -> Option<P> {
        self.place_of(slot).map(|at| self.heap[at].priority)
    }

    /// The slot holding the least priority, with that priority.
    #[must_use]
    pub fn peek(&self) -> Option<(usize, P)> {
        self.heap.first().map(|entry| (entry.slot, entry.priority))
    }

    /// Hold `slot` at `priority`, returning the priority it held before, if
    /// it was already held.
    ///
    /// # Errors
    ///
    /// As [`Self::try_reserve_slot`], when `slot` was not held and no room was
    /// made for it. Nothing a reader can see changes in that case.
    pub fn try_insert(&mut self, slot: usize, priority: P) -> Result<Option<P>, TryReserveError> {
        if let Some(previous) = self.set(slot, priority) {
            return Ok(Some(previous));
        }
        self.try_reserve_slot(slot)?;
        // Free of cost while the reserved room stands; it keeps the push from
        // ever allocating infallibly if it did not.
        self.heap
            .try_reserve(1)
            .map_err(|_| TryReserveError::AllocFailed)?;
        let at = self.heap.len();
        self.heap.push(Entry { priority, slot });
        self.places[slot] = at;
        self.sift_up(at);
        Ok(None)
    }

    /// Move a held slot to `priority`, returning the one it held, or `None` —
    /// changing nothing — if `slot` is not held.
    pub fn set(&mut self, slot: usize, priority: P) -> Option<P> {
        let at = self.place_of(slot)?;
        let previous = core::mem::replace(&mut self.heap[at].priority, priority);
        match priority.cmp(&previous) {
            core::cmp::Ordering::Less => self.sift_up(at),
            core::cmp::Ordering::Greater => self.sift_down(at),
            core::cmp::Ordering::Equal => {}
        }
        Some(previous)
    }

    /// Withdraw `slot`, returning the priority it held, or `None` if it was
    /// not held.
    pub fn remove(&mut self, slot: usize) -> Option<P> {
        let at = self.place_of(slot)?;
        self.take_out(at).map(|entry| entry.priority)
    }

    /// Withdraw the slot holding the least priority, returning it with that
    /// priority.
    pub fn pop(&mut self) -> Option<(usize, P)> {
        self.take_out(0).map(|entry| (entry.slot, entry.priority))
    }

    /// Take the entry at place `at` out, filling the hole with the last entry
    /// and sifting that whichever way it has to go.
    fn take_out(&mut self, at: usize) -> Option<Entry<P>> {
        let last = self.heap.pop()?;
        let taken = if at < self.heap.len() {
            let taken = core::mem::replace(&mut self.heap[at], last);
            self.places[last.slot] = at;
            if at > 0 && last.priority < self.heap[(at - 1) / 2].priority {
                self.sift_up(at);
            } else {
                self.sift_down(at);
            }
            taken
        } else {
            last
        };
        self.places[taken.slot] = ABSENT;
        Some(taken)
    }

    fn sift_up(&mut self, mut at: usize) {
        let entry = self.heap[at];
        while at > 0 {
            let parent = (at - 1) / 2;
            let above = self.heap[parent];
            if above.priority <= entry.priority {
                break;
            }
            self.put(at, above);
            at = parent;
        }
        self.put(at, entry);
    }

    fn sift_down(&mut self, mut at: usize) {
        let entry = self.heap[at];
        let len = self.heap.len();
        loop {
            let left = 2 * at + 1;
            if left >= len {
                break;
            }
            let right = left + 1;
            let child = if right < len && self.heap[right].priority < self.heap[left].priority {
                right
            } else {
                left
            };
            let below = self.heap[child];
            if entry.priority <= below.priority {
                break;
            }
            self.put(at, below);
            at = child;
        }
        self.put(at, entry);
    }

    fn put(&mut self, at: usize, entry: Entry<P>) {
        self.heap[at] = entry;
        self.places[entry.slot] = at;
    }
}

/// The entries at or below a bound, from [`IndexedHeap::up_to`].
#[derive(Clone, Debug)]
pub struct UpTo<'h, P> {
    heap: &'h [Entry<P>],
    bound: P,
    /// The next place to consider, or `None` once the walk is over.
    next: Option<usize>,
}

impl<P: Copy + Ord> Iterator for UpTo<'_, P> {
    type Item = (usize, P);

    fn next(&mut self) -> Option<Self::Item> {
        let mut at = self.next?;
        // A pre-order walk needing no stack: past the bound, climb over every
        // right child to the next sibling, which is where a stack would resume.
        loop {
            if let Some(entry) = self
                .heap
                .get(at)
                .filter(|entry| entry.priority <= self.bound)
            {
                self.next = Some(2 * at + 1);
                return Some((entry.slot, entry.priority));
            }
            while at > 0 && at % 2 == 0 {
                at = (at - 1) / 2;
            }
            if at == 0 {
                self.next = None;
                return None;
            }
            at += 1;
        }
    }
}

impl<P: Copy + Ord> core::iter::FusedIterator for UpTo<'_, P> {}

#[cfg(test)]
#[path = "heap_tests.rs"]
mod tests;
