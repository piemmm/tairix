//! `IndexedHeap` under real allocation failure.
//!
//! This binary's allocator refuses, on the calling thread only, every request
//! of at least a chosen size, so a reservation can be made to fail at either of
//! its two tables while every other test thread allocates normally.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::Cell;
use std::alloc::System;

use tairix_collections::{IndexedHeap, TryReserveError};

std::thread_local! {
    /// The smallest request this thread's allocator refuses.
    static REFUSED_FROM: Cell<usize> = const { Cell::new(usize::MAX) };
}

/// Whether this thread refuses a request of `size` bytes.
fn refused(size: usize) -> bool {
    REFUSED_FROM
        .try_with(|from| size >= from.get())
        .unwrap_or(false)
}

struct RefusingAlloc;

// SAFETY: every call forwards unchanged to the system allocator, except that a
// refused request answers null, which `GlobalAlloc` permits for any request.
unsafe impl GlobalAlloc for RefusingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if refused(layout.size()) {
            return core::ptr::null_mut();
        }
        // SAFETY: forwarded under the caller's own contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if refused(layout.size()) {
            return core::ptr::null_mut();
        }
        // SAFETY: forwarded under the caller's own contract.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: every block this allocator hands out came from `System`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if refused(new_size) {
            return core::ptr::null_mut();
        }
        // SAFETY: forwarded under the caller's own contract; `ptr` came from
        // `System`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: RefusingAlloc = RefusingAlloc;

/// Run `f` with this thread refusing every request of `from` bytes or more.
fn refusing<R>(from: usize, f: impl FnOnce() -> R) -> R {
    let previous = REFUSED_FROM.with(|refused| refused.replace(from));
    let out = f();
    REFUSED_FROM.with(|refused| refused.set(previous));
    out
}

#[test]
fn a_refused_reservation_changes_nothing_and_the_retry_succeeds() {
    let mut heap: IndexedHeap<u64> = IndexedHeap::new();
    assert_eq!(
        refusing(0, || heap.try_insert(5, 50)),
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
        refusing(places + 1, || heap.try_reserve_slot(SLOT)),
        Err(TryReserveError::AllocFailed)
    );
    assert_eq!(heap.len(), 1);
    assert_eq!(heap.peek(), Some((2, 20)));
    assert!(!heap.contains(SLOT));
    assert_eq!(heap.try_reserve_slot(SLOT), Ok(()));
    let all_held = refusing(0, || {
        (0..=SLOT).all(|slot| {
            heap.try_insert(slot, u64::try_from(SLOT - slot).unwrap())
                .is_ok()
        })
    });
    assert!(all_held, "an insertion below a reserved slot allocated");
    assert_eq!(heap.len(), SLOT + 1);
    assert_eq!(heap.peek(), Some((SLOT, 0)));
}
