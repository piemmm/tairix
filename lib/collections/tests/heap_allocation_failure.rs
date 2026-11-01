//! `IndexedHeap` under real allocation failure.
//!
//! The shared metered allocator refuses, on the calling thread only, every
//! request larger than a chosen size, so a reservation can be made to fail at
//! either of its two tables while every other test thread allocates normally.

use core::alloc::Layout;

use tairix_collections::{IndexedHeap, TryReserveError};
use tairix_fuzzseed::meter::refusing_above;

#[global_allocator]
static ALLOCATOR: tairix_fuzzseed::meter::Metered = tairix_fuzzseed::meter::Metered;

#[test]
fn a_refused_reservation_changes_nothing_and_the_retry_succeeds() {
    let mut heap: IndexedHeap<u64> = IndexedHeap::new();
    assert_eq!(
        refusing_above(0, || heap.try_insert(5, 50)),
        Err(TryReserveError::AllocFailed)
    );
    assert!(heap.is_empty());
    assert!(!heap.contains(5));
    assert_eq!(heap.try_insert(5, 50), Ok(None));
    assert_eq!(heap.peek(), Some((5, 50)));
}

/// An entry is wider than a place, so a refusal between the two arrays'
/// sizes lets the place table grow and refuses the entries.
#[test]
fn a_refusal_between_the_two_tables_changes_nothing_a_reader_sees() {
    const SLOT: usize = 1023;
    let places = Layout::array::<usize>(SLOT + 1).unwrap().size();
    let mut heap: IndexedHeap<u64> = IndexedHeap::new();
    assert_eq!(heap.try_insert(2, 20), Ok(None));
    assert_eq!(
        refusing_above(places, || heap.try_reserve_slot(SLOT)),
        Err(TryReserveError::AllocFailed)
    );
    assert_eq!(heap.len(), 1);
    assert_eq!(heap.peek(), Some((2, 20)));
    assert!(!heap.contains(SLOT));
    assert_eq!(heap.try_reserve_slot(SLOT), Ok(()));
    let all_held = refusing_above(0, || {
        (0..=SLOT).all(|slot| {
            heap.try_insert(slot, u64::try_from(SLOT - slot).unwrap())
                .is_ok()
        })
    });
    assert!(all_held, "an insertion below a reserved slot allocated");
    assert_eq!(heap.len(), SLOT + 1);
    assert_eq!(heap.peek(), Some((SLOT, 0)));
}
