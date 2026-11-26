//! A catalogue's names, case-blind as RISC OS compares them, indexed to the
//! numbers their objects are kept under.

use alloc::vec::Vec;

use tairix_collections::HashMap;
use tairix_hash::{BuildSipHash13, HashSeed};
use tairix_util::fallible;

/// Names of at most `MOST` bytes, each to the number its object is kept
/// under. The guest chooses the names, so they hash under the session's key.
pub(crate) struct NameIndex<const MOST: usize> {
    numbers: HashMap<Vec<u8>, u32, BuildSipHash13>,
}

impl<const MOST: usize> NameIndex<MOST> {
    pub(crate) const fn new(key: HashSeed) -> Self {
        Self {
            numbers: HashMap::with_hasher(BuildSipHash13::with_seed(key)),
        }
    }

    /// The number `name` is kept under.
    pub(crate) fn get(&self, name: &[u8]) -> Option<u32> {
        let (key, length) = folded::<MOST>(name)?;
        self.numbers.get(&key[..length]).copied()
    }

    /// `name` kept under `number`, in place of any it had; false where there
    /// is no room, or the name is longer than the index holds.
    pub(crate) fn insert(&mut self, name: &[u8], number: u32) -> bool {
        let Some((key, length)) = folded::<MOST>(name) else {
            return false;
        };
        fallible::copied(&key[..length])
            .is_some_and(|key| self.numbers.try_insert(key, number).is_ok())
    }

    pub(crate) fn remove(&mut self, name: &[u8]) {
        if let Some((key, length)) = folded::<MOST>(name) {
            self.numbers.remove(&key[..length]);
        }
    }
}

/// `name` in lower case, and its length; none for one past `MOST` bytes.
fn folded<const MOST: usize>(name: &[u8]) -> Option<([u8; MOST], usize)> {
    if name.len() > MOST {
        return None;
    }
    let mut key = [0; MOST];
    for (folded, byte) in key.iter_mut().zip(name) {
        *folded = byte.to_ascii_lowercase();
    }
    Some((key, name.len()))
}

#[cfg(test)]
mod tests;
