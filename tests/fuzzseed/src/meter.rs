//! The system allocator, metered per thread: what a call allocated, the
//! largest request it made and their sum, the most bytes it held, and the
//! refusal of every request past a size or past a count — what a host test
//! holds an allocation bound or a graceful refusal to.
//!
//! A test binary installs it as its global allocator,
//! `#[global_allocator] static ALLOC: Metered = Metered;`, and measures with
//! [`metered`], [`refusing_above`] and [`refusing_after`]. Every figure is the calling thread's,
//! so neither the harness's own threads nor tests running beside a
//! measurement charge it. A block is charged to the thread that frees it, so a
//! measured call that frees what another thread allocated reads as holding
//! that much less.

use core::alloc::{GlobalAlloc, Layout};
use std::alloc::System;
use std::cell::Cell;

std::thread_local! {
    // Destructor-free and const-initialised, so reading them never allocates
    // and cannot recurse into the allocator.
    static REFUSE_ABOVE: Cell<usize> = const { Cell::new(usize::MAX) };
    /// The requests still granted inside a [`refusing_after`] window; none
    /// outside one, where no request spends a grant.
    static GRANTS_LEFT: Cell<Option<usize>> = const { Cell::new(None) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static FREES: Cell<usize> = const { Cell::new(0) };
    static LARGEST: Cell<usize> = const { Cell::new(0) };
    static REQUESTED: Cell<usize> = const { Cell::new(0) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

/// What a [`metered`] call did on its thread.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Metering {
    /// Requests made: `alloc`, `alloc_zeroed` and `realloc` count one each.
    pub allocations: usize,
    /// Blocks released: each `dealloc` counts one.
    pub frees: usize,
    /// The largest single request, in bytes.
    pub largest: usize,
    /// Every request's bytes, summed.
    pub requested: usize,
    /// The most bytes held at once beyond what the thread held before.
    pub peak: usize,
    /// The bytes still held when `f` returned beyond what the thread held
    /// before: what a call kept.
    pub retained: usize,
}

/// Run `f`, measuring what it allocates on the calling thread.
pub fn metered<R>(f: impl FnOnce() -> R) -> (R, Metering) {
    let before = LIVE.with(Cell::get);
    ALLOCATIONS.with(|count| count.set(0));
    FREES.with(|count| count.set(0));
    LARGEST.with(|largest| largest.set(0));
    REQUESTED.with(|requested| requested.set(0));
    PEAK.with(|peak| peak.set(before));
    let out = f();
    let metering = Metering {
        allocations: ALLOCATIONS.with(Cell::get),
        frees: FREES.with(Cell::get),
        largest: LARGEST.with(Cell::get),
        requested: REQUESTED.with(Cell::get),
        peak: PEAK.with(Cell::get).saturating_sub(before),
        retained: LIVE.with(Cell::get).saturating_sub(before),
    };
    (out, metering)
}

/// Run `f` with every request larger than `bytes` refused on the calling
/// thread, as an exhausted heap refuses it.
pub fn refusing_above<R>(bytes: usize, f: impl FnOnce() -> R) -> R {
    let previous = REFUSE_ABOVE.with(|limit| limit.replace(bytes));
    let out = f();
    REFUSE_ABOVE.with(|limit| limit.set(previous));
    out
}

/// Run `f` with its first `granted` requests on the calling thread let
/// through and every later one refused, as a heap that runs out partway
/// refuses it. A request refused for its size spends no grant.
pub fn refusing_after<R>(granted: usize, f: impl FnOnce() -> R) -> R {
    let previous = GRANTS_LEFT.with(|left| left.replace(Some(granted)));
    let out = f();
    GRANTS_LEFT.with(|left| left.set(previous));
    out
}

/// The metered system allocator. Install it as a test binary's
/// `#[global_allocator]`.
pub struct Metered;

/// Whether this thread refuses a request of `size` bytes. `try_with` keeps a
/// request made while the thread's locals are torn down from panicking.
fn refused(size: usize) -> bool {
    if REFUSE_ABOVE
        .try_with(|limit| size > limit.get())
        .unwrap_or(false)
    {
        return true;
    }
    GRANTS_LEFT
        .try_with(|left| match left.get() {
            None => false,
            Some(0) => true,
            Some(granted) => {
                left.set(Some(granted - 1));
                false
            }
        })
        .unwrap_or(false)
}

/// Charge a request of `size` bytes.
fn requested(size: usize) {
    let _ = ALLOCATIONS.try_with(|count| count.set(count.get().saturating_add(1)));
    let _ = LARGEST.try_with(|largest| largest.set(largest.get().max(size)));
    let _ = REQUESTED.try_with(|requested| requested.set(requested.get().saturating_add(size)));
}

/// Move the bytes this thread holds by `grown` and `released`.
fn held(grown: usize, released: usize) {
    let _ = LIVE.try_with(|live| {
        let now = live.get().saturating_sub(released).saturating_add(grown);
        live.set(now);
        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(now)));
    });
}

// SAFETY: every request is either passed to the system allocator with the
// caller's arguments unchanged or refused with a null pointer, which the
// `GlobalAlloc` contract permits for any request; the metering touches only
// this thread's destructor-free counters.
unsafe impl GlobalAlloc for Metered {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        requested(layout.size());
        if refused(layout.size()) {
            return core::ptr::null_mut();
        }
        // SAFETY: the caller's obligations for `layout` are passed on as given.
        let block = unsafe { System.alloc(layout) };
        if !block.is_null() {
            held(layout.size(), 0);
        }
        block
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        requested(layout.size());
        if refused(layout.size()) {
            return core::ptr::null_mut();
        }
        // SAFETY: as `alloc`.
        let block = unsafe { System.alloc_zeroed(layout) };
        if !block.is_null() {
            held(layout.size(), 0);
        }
        block
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        requested(new_size);
        if refused(new_size) {
            return core::ptr::null_mut();
        }
        // SAFETY: `ptr` and `layout` come from this allocator, which only ever
        // hands out the system allocator's blocks.
        let block = unsafe { System.realloc(ptr, layout, new_size) };
        if !block.is_null() {
            // A moved block holds both until the copy is done.
            held(new_size, 0);
            held(0, layout.size());
        }
        block
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let _ = FREES.try_with(|count| count.set(count.get().saturating_add(1)));
        held(0, layout.size());
        // SAFETY: as `realloc`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[cfg(test)]
mod tests {
    use super::{metered, refusing_above, refusing_after, Metered};

    #[global_allocator]
    static ALLOC: Metered = Metered;

    #[test]
    fn a_metered_call_counts_its_requests_its_largest_and_what_it_held() {
        let ((), metering) = metered(|| {
            let small = std::vec![0u8; 16];
            let large = std::vec![0u8; 4096];
            drop(small);
            drop(large);
        });
        assert_eq!(metering.allocations, 2);
        assert_eq!(metering.frees, 2);
        assert_eq!(metering.largest, 4096);
        assert_eq!(metering.requested, 4096 + 16);
        assert!(metering.peak >= 4096 + 16);
    }

    #[test]
    fn a_request_past_the_limit_is_refused_and_the_limit_then_lifts() {
        let refused = refusing_above(1024, || std::vec::Vec::<u8>::new().try_reserve_exact(4096));
        assert!(refused.is_err());
        assert!(std::vec::Vec::<u8>::new().try_reserve_exact(4096).is_ok());
    }

    /// A refused growth leaves the block it would have moved whole, and the
    /// growth is charged once it is let through.
    #[test]
    fn a_refused_growth_keeps_the_block_it_would_have_moved() {
        let mut grown = std::vec![7u8; 64];
        let refused = refusing_above(1024, || grown.try_reserve_exact(4096));
        assert!(refused.is_err());
        assert_eq!(grown, [7u8; 64]);
        let ((), metering) = metered(|| {
            grown.try_reserve_exact(4096).ok();
        });
        assert_eq!(metering.allocations, 1);
        assert!(metering.largest >= 4096);
        assert_eq!(&grown[..], &[7u8; 64]);
    }

    /// Requests past the granted count are refused, the count lifts with the
    /// window, and requests made outside a window spend nothing of it.
    #[test]
    fn requests_past_the_granted_count_are_refused_and_the_count_then_lifts() {
        drop(std::vec![0u8; 16]);
        let answered = refusing_after(2, || {
            [(); 3].map(|()| std::vec::Vec::<u8>::new().try_reserve_exact(16).is_ok())
        });
        assert_eq!(answered, [true, true, false]);
        assert!(std::vec::Vec::<u8>::new().try_reserve_exact(16).is_ok());
        let sized_out = refusing_after(1, || {
            let large = refusing_above(8, || std::vec::Vec::<u8>::new().try_reserve_exact(64));
            let small = std::vec::Vec::<u8>::new().try_reserve_exact(4);
            (large.is_ok(), small.is_ok())
        });
        assert_eq!(
            sized_out,
            (false, true),
            "a request refused for its size spends no grant"
        );
    }

    #[test]
    fn what_another_thread_allocates_is_not_charged() {
        let ((), metering) = metered(|| {
            std::thread::spawn(|| drop(std::vec![0u8; 1 << 20]))
                .join()
                .ok();
        });
        assert!(metering.largest < 1 << 20);
    }
}
