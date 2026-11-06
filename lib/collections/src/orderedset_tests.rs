//! [`OrderedSet`] unit tests: the set's own surface over the ordered map.

use alloc::format;
use alloc::vec::Vec;
use core::ops::Bound;

use super::*;

#[test]
fn a_member_is_held_once_and_found_in_order() {
    let mut set = OrderedSet::new();
    for member in [30u32, 10, 20, 10] {
        set.try_insert(member).unwrap();
    }
    assert_eq!(set.len(), 3);
    assert!(set.contains(&20) && !set.contains(&25));
    assert_eq!(set.get(&10), Some(&10));
    assert!(set.iter().copied().eq([10, 20, 30]));
    assert!(set.iter().rev().copied().eq([30, 20, 10]));
    assert_eq!(set.iter().len(), 3);
    assert_eq!((set.first(), set.last()), (Some(&10), Some(&30)));
    assert!((&set).into_iter().eq(set.iter()));
}

#[test]
fn an_insertion_and_a_removal_report_membership() {
    let mut set: OrderedSet<u32> = OrderedSet::new();
    assert_eq!(set.try_insert(4), Ok(true));
    assert_eq!(set.try_insert(4), Ok(false), "a member already held");
    assert!(set.remove(&4));
    assert!(!set.remove(&4));
    assert_eq!(set.try_insert(5), Ok(true));
    assert_eq!(set.take(&5), Some(5));
    assert_eq!(set.take(&5), None);
    assert!(set.is_empty() && set.height() == 0);
}

#[test]
fn a_range_of_members_walks_from_either_end() {
    let mut set: OrderedSet<u32> = OrderedSet::new();
    for member in (0..200).rev() {
        set.try_insert(member * 3).unwrap();
    }
    assert!(set
        .range(30..60)
        .copied()
        .eq((10..20).map(|member| member * 3)));
    let admitted = set.range((Bound::Excluded(30), Bound::Included(60)));
    assert!(admitted
        .rev()
        .copied()
        .eq((11..=20).rev().map(|member| member * 3)));
    let crossed = (Bound::Included(60), Bound::Excluded(30));
    assert_eq!(
        set.range(crossed).next(),
        None,
        "a range past its end admits nothing"
    );
    assert_eq!((set.pop_first(), set.pop_last()), (Some(0), Some(597)));
}

#[test]
fn retain_a_copy_and_the_owning_walk() {
    let mut set: OrderedSet<u32> = OrderedSet::try_with_capacity(64).unwrap();
    for member in 0..64 {
        set.try_insert(member).unwrap();
    }
    set.retain(|member| member % 4 == 0);
    assert!(set.iter().copied().eq((0..16).map(|member| member * 4)));
    let copy = set.try_clone().unwrap();
    assert_eq!(copy, set);
    assert_eq!(
        format!("{:?}", copy.range(..9).collect::<Vec<_>>()),
        "[0, 4, 8]"
    );
    let owned: Vec<u32> = copy.into_iter().rev().collect();
    assert!(owned.into_iter().eq((0..16).rev().map(|member| member * 4)));
    let mut small: OrderedSet<u32> = OrderedSet::new();
    small.try_insert(2).unwrap();
    small.try_insert(1).unwrap();
    assert_eq!(format!("{small:?}"), "{1, 2}");
    set.clear();
    assert!(set.is_empty() && set.allocated_bytes() == 0);
    set.try_reserve(10).unwrap();
    set.try_shrink_to_fit().unwrap();
    assert_eq!(set.allocated_bytes(), 0);
}
