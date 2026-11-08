//! Tests for [`SharedSlice`], [`SharedStr`] and [`try_arc`]: sharing, every
//! element dropped once by the last holder, a clone panicking part-way, the
//! block's layout, and the saturated count.

extern crate std;

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use core::alloc::Layout;
use core::cell::Cell;
use core::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use std::panic::{catch_unwind, AssertUnwindSafe};

use tairix_fuzzseed::Counted;

use super::{try_arc, Header, SharedSlice, SharedStr, PINNED, SATURATED};
use crate::loom_compat::Ordering;
use crate::TryReserveError;

fn hashed<T: Hash + ?Sized>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[test]
fn the_empty_slice_holds_no_block() {
    let made = [
        SharedSlice::<u32>::new(),
        SharedSlice::default(),
        SharedSlice::try_from_slice(&[]).expect("nothing to hold"),
        SharedSlice::try_from_vec(vec![]).expect("nothing to hold"),
    ];
    for empty in &made {
        assert!(empty.block.is_none());
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert_eq!(empty.as_slice(), &[] as &[u32]);
        assert!(SharedSlice::ptr_eq(empty, &made[0]), "the one empty slice");
    }
    let text = SharedStr::try_from_str("").expect("nothing to hold");
    assert!(text.is_empty() && SharedStr::ptr_eq(&text, &SharedStr::new()));
}

#[test]
fn a_slice_holds_what_it_was_made_from_and_clones_by_sharing() {
    let first = SharedSlice::try_from_slice(&[3_u64, 1, 4]).expect("held");
    assert_eq!(&*first, &[3, 1, 4]);
    assert_eq!(first.len(), 3);
    let shared = first.clone();
    assert!(SharedSlice::ptr_eq(&first, &shared));
    let again = SharedSlice::try_from_vec(vec![3_u64, 1, 4]).expect("held");
    assert!(!SharedSlice::ptr_eq(&first, &again), "a block of its own");
    assert_eq!(first, again, "equal by what they hold");
    assert_eq!(first, *[3_u64, 1, 4].as_slice());
    let later = SharedSlice::try_from_slice(&[3_u64, 2]).expect("held");
    assert!(first < later && later > again);
    // SAFETY-free read of the count: two handles hold the first block.
    let holders = first
        .block
        .map(|block| unsafe { block.as_ref() }.holders.load(Ordering::Relaxed));
    assert_eq!(holders, Some(2));
}

#[test]
fn every_element_is_dropped_once_by_the_last_holder() {
    let drops = Counted::counter();
    let items = vec![
        Counted::new(&drops),
        Counted::new(&drops),
        Counted::new(&drops),
    ];
    let first = SharedSlice::try_from_vec(items).expect("held");
    let second = first.clone();
    let third = second.clone();
    drop(first);
    drop(third);
    assert_eq!(drops.get(), 0, "a holder remains");
    drop(second);
    assert_eq!(drops.get(), 3, "each element once");
}

/// A value whose clone panics once `remaining` clones have been made.
struct Fragile {
    remaining: Rc<Cell<usize>>,
    drops: Rc<Cell<usize>>,
}

impl Clone for Fragile {
    fn clone(&self) -> Self {
        let left = self.remaining.get();
        assert!(left > 0, "the clone that fails");
        self.remaining.set(left - 1);
        Self {
            remaining: Rc::clone(&self.remaining),
            drops: Rc::clone(&self.drops),
        }
    }
}

impl Drop for Fragile {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}

#[test]
fn a_clone_that_panics_drops_what_it_made_and_gives_the_block_back() {
    let remaining = Rc::new(Cell::new(2));
    let drops = Rc::new(Cell::new(0));
    let items: alloc::vec::Vec<Fragile> = (0..4)
        .map(|_| Fragile {
            remaining: Rc::clone(&remaining),
            drops: Rc::clone(&drops),
        })
        .collect();
    let made = catch_unwind(AssertUnwindSafe(|| SharedSlice::try_from_slice(&items)));
    assert!(made.is_err(), "the third clone panicked");
    assert_eq!(drops.get(), 2, "the two clones made were dropped");
    drop(items);
    assert_eq!(drops.get(), 6);
}

#[repr(align(64))]
#[derive(Clone, Copy, Debug, PartialEq)]
struct Wide(u8);

#[test]
fn elements_lie_where_the_blocks_layout_puts_them() {
    fn offset<T>() -> usize {
        Layout::new::<Header>()
            .extend(Layout::new::<T>())
            .expect("a layout")
            .1
    }
    assert_eq!(SharedSlice::<u8>::ELEMENTS, offset::<u8>());
    assert_eq!(SharedSlice::<u64>::ELEMENTS, offset::<u64>());
    assert_eq!(SharedSlice::<()>::ELEMENTS, offset::<()>());
    assert_eq!(SharedSlice::<Wide>::ELEMENTS, offset::<Wide>());
    let wide = SharedSlice::try_from_slice(&[Wide(1), Wide(2)]).expect("held");
    assert_eq!(wide.as_ptr().addr() % 64, 0, "aligned as its elements ask");
    assert_eq!(&*wide, &[Wide(1), Wide(2)]);
    let units = SharedSlice::try_from_vec(vec![(); 3]).expect("held");
    assert_eq!(units.len(), 3, "elements that take no room still count");
    assert_eq!(
        SharedSlice::<u64>::layout(usize::MAX),
        Err(TryReserveError::CapacityOverflow)
    );
}

#[test]
fn a_saturated_count_never_frees_its_block() {
    let drops = Counted::counter();
    let first = SharedSlice::try_from_vec(vec![Counted::new(&drops)]).expect("held");
    let block = first.block.expect("a block");
    // SAFETY: `first` keeps the block alive throughout.
    let holders = unsafe { &block.as_ref().holders };
    holders.store(SATURATED, Ordering::Relaxed);
    let second = first.clone();
    assert_eq!(
        holders.load(Ordering::Relaxed),
        PINNED,
        "pinned on reaching the ceiling"
    );
    drop(second);
    assert_eq!(
        holders.load(Ordering::Relaxed),
        PINNED,
        "held there as holders go"
    );
    assert_eq!(drops.get(), 0);
    // Put the count back to the one real holder, so the block is freed.
    holders.store(1, Ordering::Relaxed);
    drop(first);
    assert_eq!(drops.get(), 1);
}

#[test]
fn slices_and_text_hash_and_order_as_what_they_borrow_as() {
    let numbers = SharedSlice::try_from_slice(&[1_u16, 2]).expect("held");
    assert_eq!(hashed(&numbers), hashed([1_u16, 2].as_slice()));
    let nan = SharedSlice::try_from_slice(&[f64::NAN]).expect("held");
    assert!(nan != nan.clone(), "a NaN equals nothing, shared or not");
    let text = SharedStr::try_from_string(String::from("Café")).expect("held");
    assert_eq!(text, "Café");
    assert_eq!(text.as_str(), "Café");
    assert_eq!(text.len(), "Café".len());
    assert_eq!(hashed(&text), hashed("Café"));
    assert_eq!(alloc::format!("{text} {text:?}"), "Café \"Café\"");
    let other = SharedStr::try_from_str("Cafe").expect("held");
    assert!(other < text);
    assert!(SharedStr::ptr_eq(&text, &text.clone()));
}

#[test]
fn a_shared_slice_is_sent_and_shared_as_its_elements_may_be() {
    fn shareable<T: Send + Sync>() {}
    shareable::<SharedSlice<u64>>();
    shareable::<SharedStr>();
}

#[test]
fn a_value_is_shared_fallibly() {
    let shared = try_arc(String::from("held")).expect("held");
    assert_eq!(shared.as_str(), "held");
}
