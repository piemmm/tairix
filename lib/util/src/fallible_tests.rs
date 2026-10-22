//! Tests for the shared fallible allocation helpers.

use super::{boxed_array, collected, concatenated, filled, grow_to, push, reserve};
use alloc::vec::Vec;

/// A count whose `T`-sized layout cannot be reserved on any target: the
/// byte size overflows, so the refusal is the allocator's answer rather
/// than a property of the host's free memory.
const UNRESERVABLE: usize = usize::MAX / 2;

#[test]
fn a_refused_reservation_is_answered_not_aborted() {
    assert_eq!(filled::<u32>(UNRESERVABLE, 0), None);
    assert_eq!(collected(UNRESERVABLE, core::iter::repeat(0_u32)), None);
    let mut scratch: Vec<u32> = Vec::new();
    assert!(!grow_to(&mut scratch, UNRESERVABLE, 0));
    assert!(scratch.is_empty(), "a refused growth leaves the scratch");
    let mut buffer: Vec<u32> = Vec::new();
    assert!(!reserve(&mut buffer, UNRESERVABLE));
}

#[test]
fn a_list_pushed_an_item_at_a_time_grows_by_doubling() {
    let mut list: Vec<u32> = Vec::new();
    let mut grown = 0;
    for item in 0..4096 {
        let before = list.capacity();
        assert!(push(&mut list, item));
        grown += usize::from(list.capacity() != before);
    }
    assert_eq!(list.len(), 4096);
    assert_eq!(list[4095], 4095);
    assert!(grown <= 13, "{grown} reallocations for 4096 pushes");
}

#[test]
fn a_boxed_array_holds_its_copies() {
    assert_eq!(boxed_array::<u8, 4>(5).as_deref(), Some(&[5; 4]));
}

#[test]
fn a_granted_reservation_holds_exactly_what_was_asked_for() {
    assert_eq!(filled(3, 7_u8), Some(alloc::vec![7, 7, 7]));
    assert_eq!(
        collected(2, [1_u8, 2, 3].into_iter()),
        Some(alloc::vec![1, 2])
    );
    let mut scratch = alloc::vec![1_u8, 2];
    assert!(grow_to(&mut scratch, 4, 9));
    assert_eq!(scratch, alloc::vec![1, 2, 9, 9]);
    assert!(
        grow_to(&mut scratch, 1, 9),
        "a scratch already long enough is left alone"
    );
    assert_eq!(scratch.len(), 4);
}

#[test]
fn parts_are_joined_into_a_string_of_exactly_their_length() {
    let joined = concatenated(&["/System", "/", "icon", ".png"]).expect("room");
    assert_eq!(joined, "/System/icon.png");
    assert_eq!(joined.capacity(), joined.len());
    assert_eq!(concatenated(&[]).as_deref(), Some(""));
}
