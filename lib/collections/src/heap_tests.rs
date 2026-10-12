extern crate std;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::cell::Cell;
use core::cmp::Ordering;

use tairix_fuzzseed::Prng;

use super::*;

/// A priority that counts every comparison made of it, so a cost bound is
/// held to a count rather than to a clock.
#[derive(Copy, Clone, Debug)]
struct Counting<'c> {
    value: u32,
    compared: &'c Cell<usize>,
}

impl PartialEq for Counting<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Counting<'_> {}

impl PartialOrd for Counting<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Counting<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.compared.set(self.compared.get() + 1);
        self.value.cmp(&other.value)
    }
}

/// Every slot `heap` holds, drained least first.
fn drain(heap: &mut IndexedHeap<u32>) -> Vec<(usize, u32)> {
    core::iter::from_fn(|| heap.pop()).collect()
}

#[test]
fn an_empty_heap_holds_nothing() {
    let mut heap: IndexedHeap<u32> = IndexedHeap::new();
    assert!(heap.is_empty());
    assert_eq!(heap.peek(), None);
    assert_eq!(heap.pop(), None);
    assert_eq!(heap.remove(3), None);
    assert_eq!(heap.set(3, 1), None);
    assert!(!heap.contains(3));
    assert_eq!(heap.up_to(u32::MAX).next(), None);
}

#[test]
fn slots_come_out_least_priority_first() {
    let mut heap = IndexedHeap::new();
    for (slot, priority) in [(4, 40), (0, 10), (9, 30), (2, 20), (7, 50)] {
        assert_eq!(heap.try_insert(slot, priority), Ok(None));
    }
    assert_eq!(heap.len(), 5);
    assert_eq!(heap.peek(), Some((0, 10)));
    assert_eq!(
        drain(&mut heap),
        [(0, 10), (2, 20), (9, 30), (4, 40), (7, 50)]
    );
    assert!(heap.is_empty());
}

#[test]
fn a_priority_moves_either_way_and_the_order_follows() {
    let mut heap = IndexedHeap::new();
    for slot in 0..8 {
        assert_eq!(
            heap.try_insert(slot, 10 * u32::try_from(slot).unwrap()),
            Ok(None)
        );
    }
    assert_eq!(heap.set(6, 5), Some(60));
    assert_eq!(heap.set(0, 75), Some(0));
    assert_eq!(
        heap.try_insert(3, 35),
        Ok(Some(30)),
        "an insert of a held slot moves it"
    );
    assert_eq!(heap.get(3), Some(35));
    assert_eq!(heap.len(), 8);
    assert_eq!(
        drain(&mut heap),
        [
            (6, 5),
            (1, 10),
            (2, 20),
            (3, 35),
            (4, 40),
            (5, 50),
            (7, 70),
            (0, 75)
        ]
    );
}

#[test]
fn a_slot_removed_from_the_middle_leaves_the_rest_in_order() {
    let mut heap = IndexedHeap::new();
    for slot in 0..16 {
        assert_eq!(
            heap.try_insert(slot, (u32::try_from(slot).unwrap() * 7) % 16),
            Ok(None)
        );
    }
    assert_eq!(heap.remove(5), Some(3));
    assert_eq!(heap.remove(5), None, "a slot is withdrawn once");
    assert!(!heap.contains(5));
    let drained = drain(&mut heap);
    assert_eq!(drained.len(), 15);
    assert!(drained.windows(2).all(|pair| pair[0].1 <= pair[1].1));
    assert!(drained.iter().all(|&(slot, _)| slot != 5));
}

#[test]
fn a_slot_past_the_place_table_is_refused_and_changes_nothing() {
    let mut heap = IndexedHeap::new();
    assert_eq!(heap.try_insert(1, 1), Ok(None));
    assert_eq!(
        heap.try_insert(usize::MAX, 2),
        Err(TryReserveError::CapacityOverflow)
    );
    assert_eq!(
        heap.try_insert(usize::MAX - 1, 2),
        Err(TryReserveError::AllocFailed),
        "a place table that large has no layout"
    );
    assert_eq!(heap.len(), 1);
    assert_eq!(heap.peek(), Some((1, 1)));
}

#[test]
fn room_made_for_a_slot_covers_every_slot_below_it_and_is_kept() {
    let mut heap = IndexedHeap::new();
    assert_eq!(heap.try_reserve_slot(40), Ok(()));
    assert!(heap.is_empty(), "a reservation holds nothing");
    let room = (heap.heap.capacity(), heap.places.capacity());
    assert!(room.0 >= 41 && room.1 >= 41);
    for slot in (0..=40).rev() {
        assert_eq!(heap.try_insert(slot, 40 - slot), Ok(None));
    }
    for _ in 0..20 {
        assert!(heap.pop().is_some());
    }
    heap.clear();
    assert!(heap.is_empty());
    assert!(!heap.contains(40));
    for slot in 0..=40 {
        assert_eq!(heap.try_insert(slot, slot), Ok(None));
    }
    assert_eq!(
        (heap.heap.capacity(), heap.places.capacity()),
        room,
        "nothing below a reserved slot ever grows the heap"
    );
}

#[test]
fn up_to_walks_exactly_the_entries_at_or_below_the_bound() {
    let mut rng = Prng::new(0x1DE8_ED00_C0DE_0001);
    let mut heap = IndexedHeap::new();
    for slot in 0..300 {
        let priority = u32::try_from(rng.below(1000)).unwrap();
        assert_eq!(heap.try_insert(slot, priority), Ok(None));
    }
    for bound in [0, 1, 17, 250, 500, 999, 1000] {
        let mut walked: Vec<(usize, u32)> = heap.up_to(bound).collect();
        walked.sort_unstable();
        let mut expected: Vec<(usize, u32)> = (0..300)
            .filter_map(|slot| heap.get(slot).map(|priority| (slot, priority)))
            .filter(|&(_, priority)| priority <= bound)
            .collect();
        expected.sort_unstable();
        assert_eq!(walked, expected, "bound {bound}");
    }
}

#[test]
fn every_operation_costs_the_height_and_a_walk_costs_what_it_returns() {
    const HELD: usize = 4096;
    // A sift down compares twice a level, so twice the height bounds any one.
    const SIFT: usize = 2 * (HELD.ilog2() as usize + 1);
    let compared = Cell::new(0);
    let at = |value| Counting {
        value,
        compared: &compared,
    };
    let mut heap = IndexedHeap::new();
    for slot in 0..HELD {
        let value = u32::try_from(slot).unwrap() * 2 + 10;
        assert!(heap.try_insert(slot, at(value)).is_ok());
    }
    compared.set(0);

    assert!(heap.set(HELD - 1, at(0)).is_some());
    assert!(compared.replace(0) <= SIFT, "raised to the top");
    assert!(heap.set(HELD - 1, at(u32::MAX)).is_some());
    assert!(compared.replace(0) <= SIFT, "sunk to the bottom");
    assert!(heap.remove(HELD / 3).is_some());
    assert!(compared.replace(0) <= SIFT, "removed from the middle");
    assert_eq!(heap.pop().map(|(slot, _)| slot), Some(0));
    assert!(compared.replace(0) <= SIFT, "popped");
    assert!(heap.try_insert(HELD / 3, at(1)).is_ok());
    assert!(compared.replace(0) <= SIFT, "inserted");

    // At or below 14 are now slot HELD / 3 at 1 and slots 1 and 2: the walk
    // compares each of them and each child that stopped it, no more.
    assert_eq!(heap.up_to(at(14)).count(), 3);
    let walked = compared.replace(0);
    assert!(walked <= 2 * 3 + 1, "walked {walked} for three entries");
}

#[test]
fn a_shuffled_history_agrees_with_a_model_at_every_step() {
    let mut rng = Prng::new(0x1DE8_ED00_C0DE_0002);
    let mut heap = IndexedHeap::new();
    let mut model: BTreeMap<usize, u32> = BTreeMap::new();
    for _ in 0..20_000 {
        let slot = rng.below(64);
        let priority = u32::try_from(rng.below(128)).unwrap();
        match rng.below(5) {
            0 | 1 => assert_eq!(
                heap.try_insert(slot, priority),
                Ok(model.insert(slot, priority))
            ),
            2 => {
                let moved = model
                    .get_mut(&slot)
                    .map(|held| core::mem::replace(held, priority));
                assert_eq!(heap.set(slot, priority), moved);
            }
            3 => assert_eq!(heap.remove(slot), model.remove(&slot)),
            _ => {
                let popped = heap.pop();
                let least = model.values().min().copied();
                assert_eq!(popped.map(|(_, priority)| priority), least);
                if let Some((slot, priority)) = popped {
                    assert_eq!(model.remove(&slot), Some(priority));
                }
            }
        }
        assert_eq!(heap.len(), model.len());
        assert_eq!(
            heap.peek().map(|(_, priority)| priority),
            model.values().min().copied()
        );
        if let Some((slot, priority)) = heap.peek() {
            assert_eq!(model.get(&slot), Some(&priority));
        }
    }
    for (&slot, &priority) in &model {
        assert_eq!(heap.get(slot), Some(priority));
    }
}
