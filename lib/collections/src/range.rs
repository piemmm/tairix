//! [`RangeMap`] — disjoint half-open ranges, each carrying a value.

use core::fmt;
use core::iter::FusedIterator;
use core::ops::Range;

use crate::ordered::{self, OrderedMap};
use crate::TryReserveError;

/// An endpoint the disjoint-range containers can be keyed on.
///
/// Ordering alone decides overlap, adjacency, and splitting, so the
/// arithmetic here is only what *measuring* a range needs: turning a
/// `(base, count)` pair into a range, and reporting how much a range holds.
pub trait RangeKey: Copy + Ord {
    /// The endpoint `count` elements above `self`, or `None` when that leaves
    /// the key's representable range.
    fn advance(self, count: u64) -> Option<Self>;

    /// Elements in `[lower, self)`, saturating at zero.
    fn distance_from(self, lower: Self) -> u64;

    /// The half-open range of `count` elements from `self`, or `None` for a
    /// zero count or one that runs past the key's range.
    ///
    /// The one place a `(base, count)` pair becomes a range, so a caller that
    /// counts pages, blocks, or slots does not repeat the checked arithmetic.
    fn span(self, count: u64) -> Option<Range<Self>> {
        if count == 0 {
            return None;
        }
        Some(self..self.advance(count)?)
    }
}

impl RangeKey for u64 {
    fn advance(self, count: u64) -> Option<Self> {
        self.checked_add(count)
    }

    fn distance_from(self, lower: Self) -> u64 {
        self.saturating_sub(lower)
    }
}

impl RangeKey for usize {
    fn advance(self, count: u64) -> Option<Self> {
        self.checked_add(usize::try_from(count).ok()?)
    }

    fn distance_from(self, lower: Self) -> u64 {
        u64::try_from(self.saturating_sub(lower)).unwrap_or(u64::MAX)
    }
}

/// Why a [`RangeMap`] refused an insertion.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RangeError {
    /// The range covers nothing (`start >= end`).
    Empty,
    /// The range intersects one the map already holds. An entry here is an
    /// identity — a reservation, a mapping — so an overlapping insertion is
    /// refused rather than replacing or splitting the holder.
    Overlap,
    /// The range's record could not be allocated.
    Alloc(TryReserveError),
}

impl fmt::Display for RangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "the range covers nothing",
            Self::Overlap => "the range overlaps one already held",
            Self::Alloc(_) => "no memory to record the range",
        })
    }
}

/// One held range's exclusive end and its value. The start is the key.
struct Entry<K, V> {
    end: K,
    value: V,
}

/// A map from disjoint half-open `[start, end)` ranges to values.
///
/// Every held range is non-empty and intersects no other, so a point lies in
/// at most one — which is what makes [`Self::covering`] an answer rather than
/// a choice. **Adjacent ranges stay distinct**: a value-carrying range is an
/// identity, and a reservation that happens to abut its neighbour is still its
/// own. The set that canonicalises instead — absorbing everything it touches —
/// is [`RangeSet`](crate::RangeSet).
///
/// Lookup, iteration, gap search, and removing a whole range allocate
/// nothing. Recording a range can be refused for want of memory, and then
/// changes nothing; [`Self::try_reserve`] makes room for a run of them ahead.
pub struct RangeMap<K, V> {
    entries: OrderedMap<K, Entry<K, V>>,
}

impl<K, V> Default for RangeMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> RangeMap<K, V> {
    /// An empty map. Allocates nothing until the first insertion.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: OrderedMap::new(),
        }
    }

    /// Ranges held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when the map holds no range.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop every range.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Make room for `additional` further ranges, after which that many
    /// insertions, placements, and splitting removals cannot be refused for
    /// want of memory.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the room cannot be had; the map is as it was.
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.entries.try_reserve(additional)
    }

    /// Bytes the map's storage holds, with the room reserved and the records
    /// removals freed: what it costs, which [`Self::len`] does not say.
    #[must_use]
    pub fn allocated_bytes(&self) -> usize {
        self.entries.allocated_bytes()
    }

    /// Give back the room the map holds beyond its ranges.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the fitted storage cannot be had; every range
    /// is where it was, and the map keeps the room it had.
    pub fn try_shrink_to_fit(&mut self) -> Result<(), TryReserveError> {
        self.entries.try_shrink_to_fit()
    }
}

impl<K: RangeKey, V> RangeMap<K, V> {
    /// Record `range` carrying `value`.
    ///
    /// # Errors
    ///
    /// [`RangeError::Empty`] for a range covering nothing,
    /// [`RangeError::Overlap`] when it intersects one already held, or
    /// [`RangeError::Alloc`] when its record cannot be allocated — in each
    /// case the map is untouched and `value` is dropped.
    pub fn insert(&mut self, range: Range<K>, value: V) -> Result<(), RangeError> {
        if range.start >= range.end {
            return Err(RangeError::Empty);
        }
        if self.overlapping(range.clone()).next().is_some() {
            return Err(RangeError::Overlap);
        }
        self.record(range, value).map_err(RangeError::Alloc)
    }

    /// File `range`, which the caller has proved non-empty and disjoint from
    /// every held range.
    pub(crate) fn record(&mut self, range: Range<K>, value: V) -> Result<(), TryReserveError> {
        let entry = Entry {
            end: range.end,
            value,
        };
        self.entries.try_insert(range.start, entry)?;
        Ok(())
    }

    /// Remove the range beginning exactly at `start`, returning it and its
    /// value.
    pub fn remove(&mut self, start: K) -> Option<(Range<K>, V)> {
        let entry = self.entries.remove(&start)?;
        Some((start..entry.end, entry.value))
    }

    /// The range beginning exactly at `start`, and its value.
    #[must_use]
    pub fn get(&self, start: K) -> Option<(Range<K>, &V)> {
        let entry = self.entries.get(&start)?;
        Some((start..entry.end, &entry.value))
    }

    /// The range containing `point`, and its value.
    #[must_use]
    pub fn covering(&self, point: K) -> Option<(Range<K>, &V)> {
        self.entry_at_or_below(point)
            .filter(|(range, _)| point < range.end)
    }

    /// The highest held range that *ends* at or below `point`.
    ///
    /// Disjointness bounds the search to two entries: the highest range
    /// starting at or below `point` either ends there too, or covers `point`,
    /// in which case its predecessor cannot reach past its start.
    #[must_use]
    pub fn ending_at_or_below(&self, point: K) -> Option<Range<K>> {
        self.entries
            .range(..=point)
            .rev()
            .map(|(&start, entry)| start..entry.end)
            .find(|range| range.end <= point)
    }

    /// The lowest part of `query` the map does **not** hold.
    ///
    /// `None` means `query` is wholly held, which is how a caller tests
    /// containment across ranges it did not record one at a time.
    #[must_use]
    pub fn first_gap(&self, query: Range<K>) -> Option<Range<K>> {
        self.gaps(query).next()
    }

    /// The parts of `query` the map does not hold, in ascending order.
    ///
    /// The gaps between what a map has handed out *are* its free space, so
    /// this is the one walk both the containment test and the first-fit
    /// placement search read. Abutting ranges are stepped over rather than
    /// reported as an empty gap between them, so the answer describes what the
    /// map covers rather than how its entries were recorded.
    fn gaps(&self, query: Range<K>) -> impl Iterator<Item = Range<K>> + '_ {
        let mut cursor = query.start;
        let end = query.end;
        let mut held = self.overlapping(query);
        core::iter::from_fn(move || {
            if cursor >= end {
                return None;
            }
            for (range, _) in held.by_ref() {
                let gap = cursor..range.start;
                cursor = cursor.max(range.end);
                if gap.start < gap.end {
                    return Some(gap);
                }
                if cursor >= end {
                    return None;
                }
            }
            let tail = cursor..end;
            cursor = end;
            Some(tail)
        })
    }

    /// Every held range intersecting `query`, in ascending order, each with
    /// its value. An empty `query` intersects nothing.
    pub fn overlapping(&self, query: Range<K>) -> impl Iterator<Item = (Range<K>, &V)> + '_ {
        let live = query.start < query.end;
        // At most one held range can start below the query and still reach
        // into it; the rest begin inside it.
        let straddling = live
            .then(|| {
                self.entry_at_or_below(query.start)
                    .filter(|(range, _)| range.start < query.start && range.end > query.start)
            })
            .flatten();
        let inside = live
            .then(|| self.entries.range(query.start..query.end))
            .into_iter()
            .flatten()
            .map(entry_pair);
        straddling.into_iter().chain(inside)
    }

    /// Place `count` elements first-fit inside `within`, carrying `value`,
    /// and return the range placed — or `None` when no gap in `within` holds
    /// that many.
    ///
    /// This is the allocation half of the container: a window hands out
    /// ranges and takes them back, and the gaps between what it has handed
    /// out *are* its free space, so it keeps no second free-list to drift out
    /// of step with the first. A refusal of either kind leaves the map
    /// untouched and drops `value`, as a refused [`insert`](Self::insert)
    /// does.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the placed range's record cannot be
    /// allocated.
    pub fn place(
        &mut self,
        within: Range<K>,
        count: u64,
        value: V,
    ) -> Result<Option<Range<K>>, TryReserveError> {
        let Some(start) = self.first_free(within, count) else {
            return Ok(None);
        };
        // The gap search proved the range non-empty, inside the key's own
        // span, and disjoint from every held one, so the entry goes in
        // without a second overlap probe that could only agree.
        let Some(end) = start.advance(count) else {
            return Ok(None);
        };
        self.record(start..end, value)?;
        Ok(Some(start..end))
    }

    /// The lowest start inside `within` where `count` free elements fit,
    /// first-fit over the gaps between held ranges: where [`Self::place`]
    /// would put them, asked without placing them.
    ///
    /// Costs one pass over the ranges intersecting `within`, so a window's
    /// placement scales with what it has handed out rather than with how
    /// large the window is.
    #[must_use]
    pub fn first_free(&self, within: Range<K>, count: u64) -> Option<K> {
        if count == 0 {
            return None;
        }
        self.gaps(within)
            .find(|gap| gap.end.distance_from(gap.start) >= count)
            .map(|gap| gap.start)
    }

    /// Every held range and its value, in ascending order.
    #[must_use]
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter {
            inner: self.entries.iter(),
        }
    }

    /// The highest held range starting at or below `point`, and its value.
    /// It may or may not cover `point`; [`Self::covering`] is the filtered
    /// form, and the coalescing set needs the unfiltered one.
    pub(crate) fn entry_at_or_below(&self, point: K) -> Option<(Range<K>, &V)> {
        self.entries.range(..=point).next_back().map(entry_pair)
    }
}

impl<K: RangeKey> RangeMap<K, ()> {
    /// Drop `range`, splitting the held ranges it cuts, and report the
    /// elements dropped.
    ///
    /// Only for a valueless map: splitting a held range has to give each
    /// surviving part a value, and a value that describes the range's own
    /// position (a file offset, a device base) would be wrong in both. Where
    /// the value is nothing there is nothing to be wrong, so the split is the
    /// map's own business — which is what lets a set of pages, and a window of
    /// pages handed out, share this one definition.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when `range` lies strictly inside one held range,
    /// whose tail then needs a record of its own, and it cannot be allocated;
    /// the map is as it was. A cut that trims or takes whole ranges never
    /// allocates.
    pub fn remove_range(&mut self, range: Range<K>) -> Result<u64, TryReserveError> {
        if range.start >= range.end {
            return Ok(0);
        }
        let mut dropped = 0u64;
        let straddling = self
            .entry_at_or_below(range.start)
            .map(|(held, ())| held)
            .filter(|held| held.start < range.start && held.end > range.start);
        if let Some(held) = straddling {
            if held.end > range.end {
                // The cut lies inside it. Its tail is recorded before its head
                // is trimmed, so a refusal changes nothing.
                self.record(range.end..held.end, ())?;
                self.set_end(held.start, range.start);
                return Ok(range.end.distance_from(range.start));
            }
            self.set_end(held.start, range.start);
            dropped = held.end.distance_from(range.start);
        }
        // Each range starting inside the cut is taken whole, but the last, if
        // it runs past the cut, keeps its tail under the cut's end.
        loop {
            let held = self
                .entries
                .range(range.start..range.end)
                .next()
                .map(|(&start, entry)| start..entry.end);
            let Some(held) = held else { break };
            if held.end <= range.end {
                self.entries.remove(&held.start);
                dropped = dropped.saturating_add(held.end.distance_from(held.start));
            } else {
                if self.refile(held.start, range.end) {
                    dropped = dropped.saturating_add(range.end.distance_from(held.start));
                }
                break;
            }
        }
        Ok(dropped)
    }

    /// Move the end of the range starting at `start`; the caller keeps it
    /// disjoint from its neighbours.
    pub(crate) fn set_end(&mut self, start: K, end: K) {
        if let Some(entry) = self.entries.get_mut(&start) {
            entry.end = end;
        }
    }

    /// Move the start of the range starting at `from` to `to`, which must
    /// keep it after the range before it and before the range after it.
    /// Answers whether it moved.
    pub(crate) fn refile(&mut self, from: K, to: K) -> bool {
        self.entries.rekey(&from, to)
    }
}

fn entry_pair<'a, K: RangeKey, V>((start, entry): (&K, &'a Entry<K, V>)) -> (Range<K>, &'a V) {
    (*start..entry.end, &entry.value)
}

/// Ascending iterator over a [`RangeMap`]'s ranges and values.
pub struct Iter<'a, K, V> {
    inner: ordered::Iter<'a, K, Entry<K, V>>,
}

impl<'a, K: RangeKey, V> Iterator for Iter<'a, K, V> {
    type Item = (Range<K>, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(entry_pair)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<K: RangeKey, V> DoubleEndedIterator for Iter<'_, K, V> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(entry_pair)
    }
}

impl<K: RangeKey, V> ExactSizeIterator for Iter<'_, K, V> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<K: RangeKey, V> FusedIterator for Iter<'_, K, V> {}

impl<'a, K: RangeKey, V> IntoIterator for &'a RangeMap<K, V> {
    type Item = (Range<K>, &'a V);
    type IntoIter = Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
#[path = "range_tests.rs"]
mod tests;
