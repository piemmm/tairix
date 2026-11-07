//! Numbering what a drawing shares: each distinct definition once, in the
//! order the drawing first refers to it, as a writer names its gradients and
//! clips and the sandboxed read numbers the styles it sends.

use alloc::vec::Vec;

use tairix_util::fallible;

use crate::DrawingError;

/// Distinct definitions in the order they were first referred to, and the
/// index finding a definition's number by its key, so numbering a reference
/// costs a search however many definitions there are.
#[derive(Clone, Debug)]
pub struct Numbered<K, T> {
    items: Vec<T>,
    /// `(key, number)`, sorted by key.
    index: Vec<(K, usize)>,
}

impl<K: Ord + Copy, T> Numbered<K, T> {
    /// Number each distinct key `found` holds — `(key, item)` in the order
    /// they are referred to — once, in the order of its first sighting,
    /// counting from zero.
    ///
    /// # Errors
    ///
    /// [`DrawingError::OutOfMemory`] when the index cannot be held.
    pub fn of(found: Vec<(K, T)>) -> Result<Self, DrawingError> {
        let sightings = found
            .iter()
            .enumerate()
            .map(|(seen, (key, _))| (*key, seen));
        let mut firsts =
            fallible::collected(found.len(), sightings).ok_or(DrawingError::OutOfMemory)?;
        // Sorted by key then sighting, each key's first sighting leads its run.
        firsts.sort_unstable();
        firsts.dedup_by_key(|(key, _)| *key);
        firsts.sort_unstable_by_key(|&(_, seen)| seen);
        let numbers = firsts
            .iter()
            .enumerate()
            .map(|(number, &(key, _))| (key, number));
        let mut index =
            fallible::collected(firsts.len(), numbers).ok_or(DrawingError::OutOfMemory)?;
        index.sort_unstable();
        let mut items = Vec::new();
        if !fallible::reserve(&mut items, firsts.len()) {
            return Err(DrawingError::OutOfMemory);
        }
        let mut wanted = firsts.iter().map(|&(_, seen)| seen).peekable();
        for (seen, (_, item)) in found.into_iter().enumerate() {
            if wanted.next_if_eq(&seen).is_some() {
                items.push(item);
            }
        }
        Ok(Self { items, index })
    }

    /// The number of the definition `key` stands for.
    #[must_use]
    pub fn number(&self, key: K) -> Option<usize> {
        self.index
            .binary_search_by_key(&key, |&(known, _)| known)
            .ok()
            .map(|at| self.index[at].1)
    }

    /// Every definition, in number order.
    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::Numbered;

    #[test]
    fn each_definition_is_numbered_once_by_its_first_sighting() {
        let numbered = Numbered::of(vec![(30, 'c'), (10, 'a'), (30, 'x'), (20, 'b'), (10, 'y')])
            .expect("numbered");
        assert_eq!(numbered.items(), &['c', 'a', 'b']);
        assert_eq!(numbered.number(30), Some(0));
        assert_eq!(numbered.number(10), Some(1));
        assert_eq!(numbered.number(20), Some(2));
        assert_eq!(numbered.number(40), None);
    }
}
