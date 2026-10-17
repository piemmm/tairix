//! A growable list whose elements never move.

use alloc::vec::Vec;

use crate::TryReserveError;

/// A growable list held in chunks of `CHUNK` elements, `CHUNK` a power of
/// two.
///
/// An element never moves once pushed, so a push costs one write, or one
/// chunk's allocation, however many the list holds: where a `Vec` doubling
/// copies all it holds in one push, this copies nothing. Indexing costs one
/// load more than a slice's. Every chunk is allocated whole, so it has no
/// `Clone`: a clone's chunks would hold only what they held, and the next push
/// would grow one infallibly.
#[derive(Debug)]
pub struct ChunkVec<T, const CHUNK: usize> {
    chunks: Vec<Vec<T>>,
    len: usize,
}

impl<T, const CHUNK: usize> ChunkVec<T, CHUNK> {
    const SHIFT: u32 = {
        assert!(
            CHUNK.is_power_of_two(),
            "a chunk holds a power of two of elements"
        );
        CHUNK.trailing_zeros()
    };
    const MASK: usize = CHUNK - 1;

    /// An empty list, holding no chunk.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            chunks: Vec::new(),
            len: 0,
        }
    }

    /// How many elements it holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether it holds none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Push `value`, answering its index; `AllocFailed` when the heap will not
    /// hold another chunk, the list left as it was.
    pub fn try_push(&mut self, value: T) -> Result<usize, TryReserveError> {
        let index = self.len;
        if let Some(chunk) = self.chunks.last_mut().filter(|chunk| chunk.len() < CHUNK) {
            chunk.push(value);
        } else {
            let mut chunk = Vec::new();
            chunk
                .try_reserve_exact(CHUNK)
                .map_err(|_| TryReserveError::AllocFailed)?;
            self.chunks
                .try_reserve(1)
                .map_err(|_| TryReserveError::AllocFailed)?;
            chunk.push(value);
            self.chunks.push(chunk);
        }
        self.len += 1;
        Ok(index)
    }

    /// The element at `index`, if it holds one there.
    #[must_use]
    #[inline]
    pub fn get(&self, index: usize) -> Option<&T> {
        self.chunks
            .get(index >> Self::SHIFT)?
            .get(index & Self::MASK)
    }

    /// The element at `index`, if it holds one there, to change.
    #[must_use]
    #[inline]
    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        self.chunks
            .get_mut(index >> Self::SHIFT)?
            .get_mut(index & Self::MASK)
    }

    /// Its elements in the order they were pushed.
    pub fn iter(&self) -> impl Iterator<Item = &T> + '_ {
        self.chunks.iter().flatten()
    }

    /// Its elements from the one at `start` on, in the order they were
    /// pushed; none for a `start` past its end.
    pub fn iter_from(&self, start: usize) -> impl Iterator<Item = &T> + '_ {
        let (first, within) = (start >> Self::SHIFT, start & Self::MASK);
        self.chunks
            .iter()
            .skip(first)
            .enumerate()
            .flat_map(move |(index, chunk)| {
                let skip = if index == 0 { within } else { 0 };
                chunk.get(skip..).unwrap_or_default()
            })
    }

    /// Its elements in the order they were pushed, to change.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> + '_ {
        self.chunks.iter_mut().flatten()
    }
}

impl<'list, T, const CHUNK: usize> IntoIterator for &'list ChunkVec<T, CHUNK> {
    type Item = &'list T;
    type IntoIter = core::iter::Flatten<core::slice::Iter<'list, Vec<T>>>;

    fn into_iter(self) -> Self::IntoIter {
        self.chunks.iter().flatten()
    }
}

impl<'list, T, const CHUNK: usize> IntoIterator for &'list mut ChunkVec<T, CHUNK> {
    type Item = &'list mut T;
    type IntoIter = core::iter::Flatten<core::slice::IterMut<'list, Vec<T>>>;

    fn into_iter(self) -> Self::IntoIter {
        self.chunks.iter_mut().flatten()
    }
}

impl<T, const CHUNK: usize> Default for ChunkVec<T, CHUNK> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "chunkvec_tests.rs"]
mod tests;
