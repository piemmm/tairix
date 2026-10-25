//! Claims on the bits of a word array: the lowest clear bit, taken by one
//! compare-and-swap, and given back by one atomic clear.
//!
//! An allocator of vectors or slots keeps its map here rather than behind a
//! lock. A claim is `AcqRel` and a release `Release`, so what the last holder
//! of a bit wrote happens before the next holder's claim returns.

use crate::loom_compat::{AtomicU64, Ordering};

/// Claim the lowest clear bit below `bits`, returning its index, or [`None`]
/// when every one is set. Bits past the end of `words` count as set.
#[must_use]
pub fn claim_lowest(words: &[AtomicU64], bits: usize) -> Option<usize> {
    for (index, word) in words.iter().enumerate() {
        let first = index.checked_mul(64)?;
        if first >= bits {
            return None;
        }
        let free = u64::MAX >> (64 - (bits - first).min(64));
        let mut current = word.load(Ordering::Acquire);
        while !current & free != 0 {
            let bit = (!current & free).trailing_zeros();
            match word.compare_exchange_weak(
                current,
                current | (1u64 << bit),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(first + bit as usize),
                Err(seen) => current = seen,
            }
        }
    }
    None
}

/// Clear bit `index`, so a later claim may take it. An index past the end of
/// `words` changes nothing.
pub fn release(words: &[AtomicU64], index: usize) {
    if let Some(word) = words.get(index / 64) {
        word.fetch_and(!(1u64 << (index % 64)), Ordering::Release);
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::{claim_lowest, release};
    use core::sync::atomic::AtomicU64;
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::thread;
    use std::vec::Vec;

    fn words<const N: usize>() -> [AtomicU64; N] {
        [const { AtomicU64::new(0) }; N]
    }

    #[test]
    fn every_bit_is_claimed_once_lowest_first() {
        let map = words::<3>();
        for expected in 0..130 {
            assert_eq!(claim_lowest(&map, 130), Some(expected));
        }
        assert_eq!(claim_lowest(&map, 130), None);
    }

    #[test]
    fn a_released_bit_is_claimed_again() {
        let map = words::<2>();
        while claim_lowest(&map, 100).is_some() {}
        release(&map, 70);
        release(&map, 3);
        assert_eq!(claim_lowest(&map, 100), Some(3));
        assert_eq!(claim_lowest(&map, 100), Some(70));
        assert_eq!(claim_lowest(&map, 100), None);
    }

    #[test]
    fn bits_past_the_words_count_as_set() {
        let map = words::<1>();
        for expected in 0..64 {
            assert_eq!(claim_lowest(&map, 200), Some(expected));
        }
        assert_eq!(claim_lowest(&map, 200), None);
        assert_eq!(claim_lowest(&[], 8), None);
        assert_eq!(claim_lowest(&words::<1>(), 0), None);
    }

    #[test]
    fn a_release_past_the_words_changes_nothing() {
        let map = words::<1>();
        while claim_lowest(&map, 64).is_some() {}
        release(&map, 64);
        release(&map, usize::MAX);
        assert_eq!(claim_lowest(&map, 64), None);
    }

    #[test]
    fn racing_claims_take_distinct_bits() {
        let map = Arc::new(words::<4>());
        let claimers: Vec<_> = (0..4)
            .map(|_| {
                let map = Arc::clone(&map);
                thread::spawn(move || {
                    let mut mine = Vec::new();
                    while let Some(bit) = claim_lowest(&*map, 256) {
                        mine.push(bit);
                    }
                    mine
                })
            })
            .collect();
        let mut seen = BTreeSet::new();
        for claimer in claimers {
            for bit in claimer.join().unwrap() {
                assert!(seen.insert(bit), "bit {bit} was claimed twice");
            }
        }
        assert_eq!(seen.len(), 256);
    }
}
