//! [`OrderedSet`] — an ordered set, the ordered map over a zero-sized value.

use core::borrow::Borrow;
use core::fmt;
use core::iter::FusedIterator;
use core::ops::RangeBounds;

use crate::ordered::{self, OrderedMap};
use crate::TryReserveError;

/// An ordered set, with [`OrderedMap`]'s guarantees.
///
/// It is that map with a zero-sized value, so it inherits the layout, the
/// fill bounds, and the fallible-allocation rules rather than repeating them.
pub struct OrderedSet<T> {
    map: OrderedMap<T, ()>,
}

impl<T> Default for OrderedSet<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> OrderedSet<T> {
    /// An empty set, which allocates nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            map: OrderedMap::new(),
        }
    }

    /// An empty set with room for `capacity` members inserted in any order.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the room cannot be had.
    pub fn try_with_capacity(capacity: usize) -> Result<Self, TryReserveError> {
        Ok(Self {
            map: OrderedMap::try_with_capacity(capacity)?,
        })
    }

    /// Members held.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether it holds no member.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Levels every lookup descends; zero while empty.
    #[must_use]
    pub const fn height(&self) -> usize {
        self.map.height()
    }

    /// Bytes the set holds, freed nodes and spare room included.
    #[must_use]
    pub fn allocated_bytes(&self) -> usize {
        self.map.allocated_bytes()
    }

    /// Make the next `additional` insertions unable to fail; removals in
    /// between only add to the room.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the room cannot be had; the set is as it was.
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.map.try_reserve(additional)
    }

    /// Drop every member and give back every node.
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Give back every freed node and any spare room.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the fitted arenas cannot be had; the set is as
    /// it was but holds the room it had.
    pub fn try_shrink_to_fit(&mut self) -> Result<(), TryReserveError> {
        self.map.try_shrink_to_fit()
    }

    /// The least member.
    #[must_use]
    pub fn first(&self) -> Option<&T> {
        self.map.first_key_value().map(|(member, ())| member)
    }

    /// The greatest member.
    #[must_use]
    pub fn last(&self) -> Option<&T> {
        self.map.last_key_value().map(|(member, ())| member)
    }

    /// Take out the least member.
    pub fn pop_first(&mut self) -> Option<T> {
        self.map.pop_first().map(|(member, ())| member)
    }

    /// Take out the greatest member.
    pub fn pop_last(&mut self) -> Option<T> {
        self.map.pop_last().map(|(member, ())| member)
    }

    /// Every member, in order.
    #[must_use]
    pub fn iter(&self) -> Iter<'_, T> {
        Iter {
            inner: self.map.keys(),
        }
    }
}

impl<T: Ord> OrderedSet<T> {
    /// Whether `value` is a member.
    #[must_use]
    pub fn contains<Q>(&self, value: &Q) -> bool
    where
        T: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.map.contains_key(value)
    }

    /// The stored member equal to `value`.
    #[must_use]
    pub fn get<Q>(&self, value: &Q) -> Option<&T>
    where
        T: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.map.get_key_value(value).map(|(member, ())| member)
    }

    /// The members `range` admits, in order.
    #[must_use]
    pub fn range<Q, R>(&self, range: R) -> Range<'_, T>
    where
        T: Borrow<Q>,
        Q: Ord + ?Sized,
        R: RangeBounds<Q>,
    {
        Range {
            inner: self.map.range(range),
        }
    }

    /// Insert `value`, answering whether it was not yet a member; a member
    /// already held is kept, and `value` dropped.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when a node its splits need cannot be had; the set
    /// is as it was.
    pub fn try_insert(&mut self, value: T) -> Result<bool, TryReserveError> {
        Ok(self.map.try_insert(value, ())?.is_none())
    }

    /// Take `value` out, answering whether it was a member.
    pub fn remove<Q>(&mut self, value: &Q) -> bool
    where
        T: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.map.remove(value).is_some()
    }

    /// Take out the stored member equal to `value`.
    pub fn take<Q>(&mut self, value: &Q) -> Option<T>
    where
        T: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.map.remove_entry(value).map(|(member, ())| member)
    }

    /// Keep only the members `keep` accepts, visited in order.
    pub fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        self.map.retain(|member, ()| keep(member));
    }
}

impl<T: Clone> OrderedSet<T> {
    /// A copy of the set, node for node.
    ///
    /// # Errors
    ///
    /// [`TryReserveError`] when the copy's arenas cannot be had.
    pub fn try_clone(&self) -> Result<Self, TryReserveError> {
        Ok(Self {
            map: self.map.try_clone()?,
        })
    }
}

impl<T: fmt::Debug> fmt::Debug for OrderedSet<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl<T: PartialEq> PartialEq for OrderedSet<T> {
    fn eq(&self, other: &Self) -> bool {
        self.map == other.map
    }
}

impl<T: Eq> Eq for OrderedSet<T> {}

/// The members of an [`OrderedSet`], in order.
pub struct Iter<'a, T> {
    inner: ordered::Keys<'a, T, ()>,
}

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        self.inner.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<T> DoubleEndedIterator for Iter<'_, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back()
    }
}

impl<T> ExactSizeIterator for Iter<'_, T> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<T> FusedIterator for Iter<'_, T> {}

/// The members of an [`OrderedSet`] a range admits, in order.
pub struct Range<'a, T> {
    inner: ordered::Range<'a, T, ()>,
}

impl<'a, T> Iterator for Range<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        self.inner.next().map(|(member, ())| member)
    }
}

impl<T> DoubleEndedIterator for Range<'_, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(member, ())| member)
    }
}

impl<T> FusedIterator for Range<'_, T> {}

/// The members of an [`OrderedSet`], taken out in order.
pub struct IntoIter<T> {
    inner: ordered::IntoIter<T, ()>,
}

impl<T> Iterator for IntoIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.inner.next().map(|(member, ())| member)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<T> DoubleEndedIterator for IntoIter<T> {
    fn next_back(&mut self) -> Option<T> {
        self.inner.next_back().map(|(member, ())| member)
    }
}

impl<T> ExactSizeIterator for IntoIter<T> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<T> FusedIterator for IntoIter<T> {}

impl<T> IntoIterator for OrderedSet<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> IntoIter<T> {
        IntoIter {
            inner: self.map.into_iter(),
        }
    }
}

impl<'a, T> IntoIterator for &'a OrderedSet<T> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Iter<'a, T> {
        self.iter()
    }
}

#[cfg(test)]
#[path = "orderedset_tests.rs"]
mod tests;
