use alloc::vec::Vec;
use core::num::NonZeroU32;

use super::{fresh_room, Key, SlotKey, SlotMap, NONE};
use crate::TryReserveError;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Named(SlotKey);

impl Key for Named {
    fn from_slot(slot: SlotKey) -> Self {
        Self(slot)
    }

    fn slot(self) -> SlotKey {
        self.0
    }
}

fn map() -> SlotMap<SlotKey, u32> {
    SlotMap::new()
}

#[test]
fn a_filed_value_is_found_under_its_key_until_it_is_removed() {
    let mut map = map();
    let a = map.try_insert(10).expect("room");
    let b = map.try_insert(20).expect("room");
    assert_eq!(map.len(), 2);
    assert_eq!(map.get(a), Some(&10));
    assert_eq!(map.get(b), Some(&20));
    if let Some(value) = map.get_mut(a) {
        *value += 1;
    }
    assert_eq!(map.remove(a), Some(11));
    assert_eq!(map.get(a), None);
    assert!(!map.contains_key(a));
    assert_eq!(map.remove(a), None);
    assert_eq!(map.len(), 1);
    assert_eq!(map.get(b), Some(&20));
}

#[test]
fn a_key_that_outlived_its_value_is_refused_after_its_slot_is_reused() {
    let mut map = map();
    let old = map.try_insert(1).expect("room");
    assert_eq!(map.remove(old), Some(1));
    let new = map.try_insert(2).expect("room");
    assert_eq!(old.index(), new.index());
    assert_ne!(old.generation(), new.generation());
    assert_eq!(map.get(old), None);
    assert_eq!(map.get_mut(old), None);
    assert_eq!(map.remove(old), None);
    assert_eq!(map.get(new), Some(&2));
}

#[test]
fn a_slot_whose_generation_is_spent_is_retired_and_never_handed_out_again() {
    let mut map = map();
    let key = map.try_insert(1).expect("room");
    let last = NonZeroU32::MAX;
    map.slots[key.index()].generation = last;
    let last_key = SlotKey {
        index: key.index,
        generation: last,
    };
    assert_eq!(map.remove(last_key), Some(1));
    assert_eq!(map.vacant.len, 0);
    let next = map.try_insert(2).expect("room");
    assert_ne!(next.index(), key.index());
    assert_eq!(map.get(last_key), None);
    assert_eq!(map.remove(last_key), None);
    assert_eq!(map.len(), 1);
}

#[test]
fn keys_past_the_end_and_of_no_generation_ever_issued_are_refused() {
    let mut map = map();
    let key = map.try_insert(7).expect("room");
    let past = SlotKey {
        index: 5,
        generation: NonZeroU32::MIN,
    };
    let forged = SlotKey {
        index: key.index,
        generation: NonZeroU32::new(9).expect("non-zero"),
    };
    assert_eq!(map.get(past), None);
    assert_eq!(map.get(forged), None);
    assert_eq!(map.remove(forged), None);
    assert_eq!(map.get(key), Some(&7));
}

#[test]
fn a_reservation_makes_that_many_insertions_allocate_nothing() {
    let mut map = map();
    let first = map.try_insert(0).expect("room");
    let _ = map.remove(first);
    map.try_reserve(9).expect("room");
    assert!(map.capacity() >= 9);
    let storage = map.slots.as_ptr();
    let reserved = map.slots.capacity();
    for value in 0..9 {
        let _ = map.try_insert(value).expect("room");
    }
    assert_eq!(map.slots.as_ptr(), storage);
    assert_eq!(map.slots.capacity(), reserved);
}

#[test]
fn the_last_index_a_key_can_carry_bounds_the_slots() {
    assert_eq!(fresh_room(0), NONE as usize);
    assert_eq!(fresh_room(NONE as usize - 1), 1);
    assert_eq!(fresh_room(NONE as usize), 0);
    let mut map = map();
    assert_eq!(
        map.try_reserve(NONE as usize + 1),
        Err(TryReserveError::CapacityOverflow)
    );
    assert_eq!(map.capacity(), 0);
}

#[test]
fn retain_and_clear_advance_every_removed_slot_s_generation() {
    let mut map = map();
    let keys: Vec<SlotKey> = (0..6).map(|v| map.try_insert(v).expect("room")).collect();
    map.retain(|_, value| *value % 2 == 0);
    assert_eq!(map.len(), 3);
    for (value, key) in (0..).zip(&keys) {
        assert_eq!(map.get(*key).is_some(), value % 2 == 0);
    }
    map.clear();
    assert!(map.is_empty());
    for key in &keys {
        assert_eq!(map.get(*key), None);
    }
    let reused: Vec<SlotKey> = (0..6).map(|v| map.try_insert(v).expect("room")).collect();
    for key in &keys {
        assert_eq!(map.get(*key), None);
    }
    assert_eq!(map.slots.len(), 6);
    assert!(reused.iter().all(|key| map.contains_key(*key)));
}

#[test]
fn iteration_visits_filed_values_in_slot_order() {
    let mut map = map();
    let keys: Vec<SlotKey> = (0..5)
        .map(|v| map.try_insert(v * 10).expect("room"))
        .collect();
    let _ = map.remove(keys[1]);
    let _ = map.remove(keys[3]);
    let seen: Vec<(SlotKey, u32)> = map.iter().map(|(key, value)| (key, *value)).collect();
    assert_eq!(seen, [(keys[0], 0), (keys[2], 20), (keys[4], 40)]);
    for value in map.values_mut() {
        *value += 1;
    }
    assert!(map.values().copied().eq([1, 21, 41]));
    assert!(map.keys().eq([keys[0], keys[2], keys[4]]));
    for (key, value) in map.iter_mut() {
        *value = u32::try_from(key.index()).expect("small");
    }
    assert!(map.values().copied().eq([0, 2, 4]));
}

#[test]
fn a_value_is_built_from_the_key_it_is_filed_under() {
    let mut map: SlotMap<Named, Named> = SlotMap::new();
    let key = map.try_insert_with_key(|key| key).expect("room");
    assert_eq!(map.get(key), Some(&key));
}

#[test]
fn vacated_slots_are_reused_most_recently_freed_first() {
    let mut map = map();
    let keys: Vec<SlotKey> = (0..4).map(|v| map.try_insert(v).expect("room")).collect();
    let _ = map.remove(keys[0]);
    let _ = map.remove(keys[2]);
    assert_eq!(map.try_insert(9).expect("room").index(), keys[2].index());
    assert_eq!(map.try_insert(9).expect("room").index(), keys[0].index());
    assert_eq!(map.try_insert(9).expect("room").index(), 4);
}
