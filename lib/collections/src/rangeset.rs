//! [`RangeSet`] — disjoint ranges kept maximally coalesced.

use core::iter::FusedIterator;
use core::ops::Range;

use crate::range::{self, RangeKey, RangeMap};
use crate::TryReserveError;

/// A set of half-open `[start, end)` ranges, held disjoint **and never
/// adjacent**.
///
/// Insertion absorbs every range it overlaps or touches and removal splits the
/// ranges it cuts, so the representation is canonical: two sets holding the
/// same elements hold the same ranges whatever order they were built in. That
/// makes the range count a property of what the set covers rather than of the
/// order a caller happened to build it in — a set of block runs costs one
/// entry per contiguous extent, not one per block.
///
/// It is [`RangeMap`] with a zero-sized value and coalescing insertion, so it
/// inherits the disjointness invariant and the neighbour probes rather than
/// repeating them. Where a range is an *identity* whose neighbour must stay a
/// separate entry — a reservation, a mapping — the map is the right container.
pub struct RangeSet<K> {
    spans: RangeMap<K, ()>,
    /// Elements held across every range, kept in step with `spans` so a
    /// caller's accounting reads it rather than summing.
    covered: u64,
}

impl<K> Default for RangeSet<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K> RangeSet<K> {
    /// An empty set. Allocates nothing until the first insertion.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            spans: RangeMap::new(),
            covered: 0,
        }
    }

    /// Ranges held; [`Self::covered`] is what they account for, and
    /// [`Self::allocated_bytes`] what they cost.
    #[must_use]
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    /// `true` when the set holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Elements held, across every range.
    #[must_use]
    pub const fn covered(&self) -> u64 {
        self.covered
    }

    /// Drop every range.
    pub fn clear(&mut self) {
        self.spans.clear();
        self.covered = 0;
    }

    /// Make room for `additional` further ranges, after which that many
    /// insertions and splitting removals cannot be refused for want of
    /// memory.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the room cannot be had; the set is as it was.
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.spans.try_reserve(additional)
    }

    /// Bytes the set's storage holds, with the room reserved and the records
    /// removals freed: its memory cost, of which [`Self::len`] counts only
    /// the ranges.
    #[must_use]
    pub fn allocated_bytes(&self) -> usize {
        self.spans.allocated_bytes()
    }

    /// Give back the room the set holds beyond its ranges.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the fitted storage cannot be had; every range
    /// is where it was, and the set keeps the room it had.
    pub fn try_shrink_to_fit(&mut self) -> Result<(), TryReserveError> {
        self.spans.try_shrink_to_fit()
    }
}

impl<K: RangeKey> RangeSet<K> {
    /// Add `range`, absorbing every held range it overlaps or touches.
    ///
    /// A range covering nothing adds nothing. One that touches a held range
    /// reuses that range's record — re-filed under the union's start, or
    /// stretched to its end — so it allocates nothing.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when `range` touches nothing held and its own
    /// record cannot be allocated; the set is as it was.
    pub fn insert(&mut self, range: Range<K>) -> Result<(), TryReserveError> {
        if range.start >= range.end {
            return Ok(());
        }
        let mut union = range;
        // Elements the union already holds, which it does not add again.
        let mut absorbed = 0u64;
        // The record the union is filed under: the held range reaching its
        // start from below, else the first it absorbs above.
        let mut filed = None;
        // A range touching or overlapping the union's start extends it
        // downward. One probe suffices, and that rests on the set's own
        // invariant rather than on disjointness alone: were two held ranges
        // ever adjacent, absorbing the lower one would expose a second
        // touching predecessor this never looks for.
        if let Some(below) = self
            .spans
            .entry_at_or_below(union.start)
            .map(|(held, ())| held)
            .filter(|held| held.end >= union.start)
        {
            union.start = below.start;
            union.end = union.end.max(below.end);
            absorbed = below.end.distance_from(below.start);
            filed = Some(below.start);
        }
        // Absorb upward: a range that overlaps the union, or begins exactly
        // where it ends, may in turn bring the next within reach. Each pass
        // files or removes a range, so this ends.
        loop {
            let above = self
                .spans
                .overlapping(union.clone())
                .map(|(held, ())| held)
                .find(|held| held.start > union.start)
                .or_else(|| self.spans.get(union.end).map(|(held, ())| held));
            let Some(above) = above else { break };
            union.end = union.end.max(above.end);
            absorbed = absorbed.saturating_add(above.end.distance_from(above.start));
            if filed.is_none() && self.spans.refile(above.start, union.start) {
                filed = Some(union.start);
            } else {
                self.spans.remove(above.start);
            }
        }
        match filed {
            Some(start) => self.spans.set_end(start, union.end),
            None => self.spans.record(union.clone(), ())?,
        }
        self.covered = self
            .covered
            .saturating_sub(absorbed)
            .saturating_add(union.end.distance_from(union.start));
        Ok(())
    }

    /// Drop `range`, splitting the held ranges it cuts.
    ///
    /// A range covering nothing drops nothing.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when `range` lies strictly inside one held range,
    /// whose tail then needs a record of its own, and it cannot be allocated;
    /// the set is as it was.
    pub fn remove(&mut self, range: Range<K>) -> Result<(), TryReserveError> {
        let dropped = self.spans.remove_range(range)?;
        self.covered = self.covered.saturating_sub(dropped);
        Ok(())
    }

    /// Whether `point` lies in a held range.
    #[must_use]
    pub fn contains(&self, point: K) -> bool {
        self.covering(point).is_some()
    }

    /// The held range containing `point`.
    #[must_use]
    pub fn covering(&self, point: K) -> Option<Range<K>> {
        self.spans.covering(point).map(|(held, ())| held)
    }

    /// The lowest part of `query` the set holds.
    ///
    /// Walking a query by alternating this with [`Self::first_gap`] is how a
    /// caller splits one range into the parts two different sets own, without
    /// visiting an element.
    #[must_use]
    pub fn first_overlap(&self, query: Range<K>) -> Option<Range<K>> {
        let held = self.spans.overlapping(query.clone()).next()?.0;
        Some(held.start.max(query.start)..held.end.min(query.end))
    }

    /// The lowest part of `query` the set does **not** hold.
    #[must_use]
    pub fn first_gap(&self, query: Range<K>) -> Option<Range<K>> {
        self.spans.first_gap(query)
    }

    /// The highest held range that *ends* at or below `point`.
    #[must_use]
    pub fn ending_at_or_below(&self, point: K) -> Option<Range<K>> {
        self.spans.ending_at_or_below(point)
    }

    /// Remove and return the lowest held range.
    pub fn pop_first(&mut self) -> Option<Range<K>> {
        let first = self.spans.iter().next()?.0;
        self.take(first.start)
    }

    /// Every held range, in ascending order.
    #[must_use]
    pub fn iter(&self) -> Ranges<'_, K> {
        Ranges {
            inner: self.spans.iter(),
        }
    }

    /// Remove the range beginning at `start`, discharging what it covered.
    fn take(&mut self, start: K) -> Option<Range<K>> {
        let (range, ()) = self.spans.remove(start)?;
        self.covered = self
            .covered
            .saturating_sub(range.end.distance_from(range.start));
        Some(range)
    }
}

/// Ascending iterator over a [`RangeSet`]'s ranges.
pub struct Ranges<'a, K> {
    inner: range::Iter<'a, K, ()>,
}

impl<K: RangeKey> Iterator for Ranges<'_, K> {
    type Item = Range<K>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(held, ())| held)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<K: RangeKey> DoubleEndedIterator for Ranges<'_, K> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(held, ())| held)
    }
}

impl<K: RangeKey> ExactSizeIterator for Ranges<'_, K> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<K: RangeKey> FusedIterator for Ranges<'_, K> {}

impl<'a, K: RangeKey> IntoIterator for &'a RangeSet<K> {
    type Item = Range<K>;
    type IntoIter = Ranges<'a, K>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
#[path = "rangeset_tests.rs"]
mod tests;
