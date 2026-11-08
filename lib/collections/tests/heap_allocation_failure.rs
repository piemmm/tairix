//! The fallible containers under real allocation failure.
//!
//! The shared metered allocator refuses, on the calling thread only, every
//! request larger than a chosen size or past a chosen count, and meters what a
//! call allocates, so a container can be made to fail at any request — the
//! indexed heap at either of its two tables, the ordered map at either of its
//! two arenas — while every other test thread allocates normally.

use core::alloc::Layout;

use tairix_collections::{
    try_arc, BitVec, IndexedHeap, OrderedMap, OrderedSet, RadixTree, RangeError, RangeMap,
    RangeSet, SharedSlice, SharedStr, TryReserveError,
};
use tairix_fuzzseed::meter::{metered, refusing_above, refusing_after};
use tairix_fuzzseed::Prng;

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

/// Entries the ordered map tests fill a map with: fewer under Miri, which
/// interprets every operation.
const ENTRIES: u64 = if cfg!(miri) { 150 } else { 2_000 };

/// What a reader of an ordered map sees, and the room it holds.
fn seen(map: &OrderedMap<u64, u64>) -> (Vec<(u64, u64)>, usize, usize) {
    let entries = map.iter().map(|(&key, &value)| (key, value)).collect();
    (entries, map.height(), map.allocated_bytes())
}

/// `count` even keys in an arena fitted to them, so the next node it needs is
/// an allocation.
fn packed(count: u64) -> OrderedMap<u64, u64> {
    let mut map = OrderedMap::new();
    for key in 0..count {
        assert_eq!(map.try_insert(key * 2, key), Ok(None));
    }
    map.try_shrink_to_fit().unwrap();
    map
}

/// More requests than any operation here makes: a copy of a map takes a
/// chunk at a time of each of its two arenas.
const MOST_GRANTS: usize = 64;

/// Run `operation` with no request granted, then one more each time until it
/// goes through, holding every refusal to changing nothing a reader sees:
/// answers the grants it took.
fn grants_until_done(
    map: &mut OrderedMap<u64, u64>,
    operation: fn(&mut OrderedMap<u64, u64>) -> Result<(), TryReserveError>,
) -> usize {
    for granted in 0..MOST_GRANTS {
        let before = seen(map);
        match refusing_after(granted, || operation(map)) {
            Ok(()) => return granted,
            Err(error) => {
                assert_eq!(error, TryReserveError::AllocFailed);
                assert_eq!(seen(map), before, "refused after {granted} grants");
            }
        }
    }
    panic!("the operation never went through");
}

#[test]
fn every_allocating_ordered_operation_refused_part_way_changes_nothing() {
    let mut map = packed(ENTRIES);
    assert!(grants_until_done(&mut map, |map| map.try_reserve(500)) > 0);
    // The reservation left room for the shrink to give back.
    assert!(grants_until_done(&mut map, OrderedMap::try_shrink_to_fit) > 0);
    assert!(grants_until_done(&mut map, |map| map.try_clone().map(drop)) > 0);
    let refused = refusing_after(0, || {
        OrderedMap::<u64, u64>::try_with_capacity(100).map(drop)
    });
    assert_eq!(refused, Err(TryReserveError::AllocFailed));
}

#[test]
fn a_refused_insertion_changes_nothing_and_the_retry_succeeds() {
    let mut map = packed(ENTRIES);
    let mut refused = 0;
    // The fitted arena's last chunk may hold spare nodes; insert past them.
    for key in ENTRIES * 2..ENTRIES * 2 + 20_000 {
        if refused == 3 {
            break;
        }
        let before = seen(&map);
        match refusing_above(0, || map.try_insert(key, key)) {
            Ok(replaced) => assert_eq!(replaced, None),
            Err(error) => {
                assert_eq!(error, TryReserveError::AllocFailed);
                assert_eq!(
                    seen(&map),
                    before,
                    "a refused insertion of {key} changed the map"
                );
                assert_eq!(map.try_insert(key, key), Ok(None), "the retry");
                refused += 1;
            }
        }
    }
    assert!(refused > 0, "no insertion needed the allocator");
}

#[test]
fn a_reserved_run_of_insertions_needs_no_allocator() {
    let mut map = packed(ENTRIES);
    let (mut above, mut between) = (ENTRIES * 2, 1);
    for batch in [1, 2, 11, 60, 300] {
        map.try_reserve(batch).unwrap();
        let held = refusing_above(0, || {
            (0..batch).all(|at| {
                // Above everything held, or in a gap between two even keys:
                // the shapes that split the most.
                let key = if at.is_multiple_of(2) {
                    above += 1;
                    above
                } else {
                    between += 2;
                    between
                };
                map.try_insert(key, key).is_ok()
            })
        });
        assert!(
            held,
            "an insertion in a reserved batch of {batch} needed the allocator"
        );
        let victims: Vec<u64> = map.keys().copied().step_by(7).collect();
        for key in victims {
            map.remove(&key);
        }
    }
}

/// Key bits between neighbours in [`ascending`], so a run of keys fits in any
/// gap.
const GAP: u32 = 20;

/// The `at`th key [`ascending`] files.
const fn spaced(at: u64) -> u64 {
    (at + 1) << GAP
}

/// A map of `count` spaced keys filed in ascending order: every leaf but the
/// last, and every interior node off the last path, at the fill floor, and
/// each arena holding only the chunks its nodes took.
fn ascending(count: u64) -> OrderedMap<u64, u64> {
    let mut map = OrderedMap::new();
    for at in 0..count {
        assert_eq!(map.try_insert(spaced(at), at), Ok(None));
    }
    map
}

/// Every allocation the next ascending key's insertion makes, refused in
/// turn, leaves the map as it was, whatever it splits: a leaf root (the
/// interior arena makes its core, and each arena grows its list of chunks and
/// a chunk), a leaf, an interior root above a leaf, and a leaf with the
/// interior node above it.
#[test]
fn an_insertion_refused_at_any_allocation_of_its_splits_changes_nothing() {
    // Entries, the grants the next insertion takes, and the height after it.
    for (entries, grants, height) in [(11, 5, 2), (17, 2, 2), (77, 3, 3), (149, 2, 3)] {
        let mut map = ascending(entries);
        let taken = grants_until_done(&mut map, |map| {
            let at = u64::try_from(map.len()).unwrap();
            map.try_insert(spaced(at), at).map(drop)
        });
        assert_eq!(
            (taken, map.height()),
            (grants, height),
            "{entries} entries filed"
        );
        assert!(map.keys().copied().eq((0..=entries).map(spaced)));
    }
}

/// `try_reserve(n)` makes the next `n` insertions allocate nothing in either
/// arena, whatever their keys: past the greatest, below the least, crowded
/// into one gap, or scattered, into maps whose next insertion splits a leaf
/// root, a leaf, an interior root or an interior node.
#[test]
fn a_reservation_covers_any_run_of_insertions_in_both_arenas() {
    const BATCHES: &[u64] = if cfg!(miri) {
        &[1, 12, 71]
    } else {
        &[1, 2, 6, 11, 12, 71, 300]
    };
    let mut rng = Prng::new(0x5ca7_7e2e);
    for entries in [0, 11, 17, 77, 149] {
        for &batch in BATCHES {
            for shape in 0..4 {
                let keys: Vec<u64> = (0..batch)
                    .map(|at| match shape {
                        0 => spaced(entries + at),
                        1 => spaced(0) - 1 - at,
                        2 => spaced(entries / 2) + 1 + at,
                        _ => rng.next_u64() >> 8,
                    })
                    .collect();
                let mut map = ascending(entries);
                map.try_reserve(usize::try_from(batch).unwrap()).unwrap();
                let insert_all = || keys.iter().all(|&key| map.try_insert(key, key).is_ok());
                let (held, metering) = metered(|| refusing_above(0, insert_all));
                assert!(
                    held && metering.allocations == 0,
                    "{batch} insertions of shape {shape} into {entries} entries allocated"
                );
            }
        }
    }
}

/// The bytes `keys` filed in a fresh set and in a fresh map hold, each as the
/// allocator meters it, which is what the container reports too.
fn footprints(keys: &[u64]) -> (usize, usize) {
    let (set, metering) = metered(|| {
        let mut set = OrderedSet::new();
        for &key in keys {
            assert_eq!(set.try_insert(key), Ok(true));
        }
        set
    });
    let set_bytes = set.allocated_bytes();
    assert_eq!(metering.retained, set_bytes, "the set's own count");
    let (map, metering) = metered(|| {
        let mut map = OrderedMap::new();
        for &key in keys {
            assert_eq!(map.try_insert(key, key), Ok(None));
        }
        map
    });
    let map_bytes = map.allocated_bytes();
    assert_eq!(metering.retained, map_bytes, "the map's own count");
    (set_bytes, map_bytes)
}

/// What ten thousand `u64` insertions hold. Ascending keys leave every node
/// off the last path at the fill floor, the most nodes so many entries can
/// take — 1 666 leaves and 331 interior nodes — so their figures bound every
/// order. A set's leaf is 112 bytes and its interior node 168, a map's 200
/// and 256; with the arenas' chunks, lists of chunks and cores that is
/// 266 416 bytes for the set and 445 680 for the map, where one 168- or
/// 256-byte node shape for both held 345 504 and 525 728.
#[test]
fn ten_thousand_insertions_hold_what_the_node_layout_allows() {
    const BOUNDS: (usize, usize) = (266_416, 445_680);
    let sorted: Vec<u64> = (0..10_000).collect();
    assert_eq!(footprints(&sorted), BOUNDS, "ascending keys meet them");
    let mut shuffled = sorted;
    let mut rng = Prng::new(0x00f0_07b1);
    for at in (1..shuffled.len()).rev() {
        shuffled.swap(at, rng.below(at + 1));
    }
    let (set, map) = footprints(&shuffled);
    assert!(
        set <= BOUNDS.0 && map <= BOUNDS.1,
        "{set} and {map} bytes for shuffled keys"
    );
}

#[test]
fn ordered_reads_walks_and_removals_allocate_nothing() {
    let mut map = packed(ENTRIES);
    let (sum, metering) = metered(|| {
        let mut sum = 0u64;
        for key in 0..ENTRIES * 2 {
            sum += map.get(&key).copied().unwrap_or(0);
        }
        sum += map
            .range(10..ENTRIES)
            .rev()
            .map(|(_, &value)| value)
            .sum::<u64>();
        sum += map.iter().map(|(&key, _)| key).sum::<u64>();
        for (_, value) in &mut map {
            *value += 1;
        }
        for (_, value) in map.range_mut(..=ENTRIES).rev() {
            *value += 1;
        }
        for value in map.values_mut() {
            *value += 1;
        }
        assert!(map.first_key_value() < map.last_key_value());
        map.remove(&10);
        map.pop_first();
        map.pop_last();
        map.retain(|key, _| key % 3 != 0);
        sum
    });
    assert!(sum > 0);
    assert_eq!(metering.allocations, 0, "{metering:?}");
}

#[test]
fn a_range_set_is_refused_only_a_record_of_its_own_and_changes_nothing() {
    let mut set: RangeSet<u64> = RangeSet::new();
    assert_eq!(
        refusing_above(0, || set.insert(10..20)),
        Err(TryReserveError::AllocFailed)
    );
    assert!(set.is_empty() && set.covered() == 0);
    set.insert(10..20).unwrap();
    // A range touching or overlapping what is held reuses its record.
    let touching = refusing_above(0, || {
        set.insert(20..30)?;
        set.insert(5..10)?;
        set.insert(12..25)?;
        set.remove(5..7)
    });
    assert_eq!(touching, Ok(()));
    assert_eq!((set.len(), set.covering(7)), (1, Some(7..30)));
    let mut set: RangeSet<u64> = RangeSet::new();
    set.insert(0..1_000_000).unwrap();
    // Each cut strictly inside the run splits it, until one needs a node the
    // arena must grow for.
    let mut refused = false;
    for cut in (2..400).step_by(2) {
        let before: (Vec<_>, u64) = (set.iter().collect(), set.covered());
        if refusing_above(0, || set.remove(cut..cut + 1)).is_err() {
            let after: (Vec<_>, u64) = (set.iter().collect(), set.covered());
            assert_eq!(after, before, "a refused cut at {cut} changed the set");
            refused = true;
            break;
        }
    }
    assert!(refused, "no cut needed the allocator");
    set.try_reserve(50).unwrap();
    let cut = |at: u64| 1_000 + 2 * at..1_001 + 2 * at;
    let held = refusing_above(0, || (0..50).all(|at| set.remove(cut(at)).is_ok()));
    assert!(held, "a reserved cut needed the allocator");
}

#[test]
fn a_range_map_refused_its_record_changes_nothing() {
    let mut map: RangeMap<u64, u32> = RangeMap::new();
    let placed = refusing_above(0, || map.place(0..100, 10, 1));
    assert_eq!(placed, Err(TryReserveError::AllocFailed));
    let inserted = refusing_above(0, || map.insert(0..10, 1));
    assert_eq!(
        inserted,
        Err(RangeError::Alloc(TryReserveError::AllocFailed))
    );
    assert!(map.is_empty());
    assert_eq!(map.place(0..100, 10, 1), Ok(Some(0..10)));
    assert_eq!(map.insert(10..20, 2), Ok(()));
    assert_eq!(map.len(), 2);
}

/// A shrink refused its fitted list keeps the nodes it freed, chained for
/// reuse: the insertions they cover then need nothing from the allocator.
#[test]
fn a_refused_shrink_keeps_its_freed_nodes_for_reuse() {
    let mut map = packed(ENTRIES);
    let removed: Vec<u64> = (0..ENTRIES).filter(|key| key % 3 != 0).collect();
    for key in &removed {
        map.remove(&(key * 2));
    }
    assert_eq!(
        refusing_after(0, || map.try_shrink_to_fit()),
        Err(TryReserveError::AllocFailed)
    );
    let back = refusing_after(0, || {
        removed
            .iter()
            .try_for_each(|&key| map.try_insert(key * 2, key).map(drop))
    });
    assert_eq!(back, Ok(()), "the freed nodes served every insertion");
    assert_eq!(map.len(), usize::try_from(ENTRIES).unwrap());
    assert!(map.iter().all(|(&key, &value)| key == value * 2));
}

/// A radix insertion refused part-way — its interior nodes had and its leaf
/// refused — leaves the tree holding the room it had, where the interior room
/// once stayed taken, held under pressure for nothing.
#[test]
fn a_refused_radix_insertion_holds_the_room_it_had() {
    for granted in 0..MOST_GRANTS {
        let mut tree: RadixTree<u64> = RadixTree::new();
        tree.try_insert(1, 1).unwrap();
        tree.try_shrink_to_fit().unwrap();
        let before = tree.allocated_bytes();
        match refusing_after(granted, || tree.try_insert(u64::MAX, 2)) {
            Ok(_) => return,
            Err(error) => {
                assert_eq!(error, TryReserveError::AllocFailed);
                assert_eq!(
                    tree.allocated_bytes(),
                    before,
                    "refused after {granted} grants"
                );
                assert_eq!(tree.get(u64::MAX), None);
                assert_eq!(tree.get(1), Some(&1));
            }
        }
    }
    panic!("the insertion never went through");
}

/// A radix tree whose interior arena is refused its fitted list still fits,
/// or chains, its leaf arena: no leaf is later handed out while it holds keys.
#[test]
fn a_refused_radix_shrink_leaves_every_key_where_it_was() {
    let mut tree: RadixTree<u64> = RadixTree::new();
    let keys: Vec<u64> = (0..ENTRIES * 4)
        .map(|key| key.wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .collect();
    for &key in &keys {
        tree.try_insert(key, key).unwrap();
    }
    for &key in keys.iter().skip(1).step_by(2) {
        tree.remove(key);
    }
    assert_eq!(
        refusing_after(0, || tree.try_shrink_to_fit()),
        Err(TryReserveError::AllocFailed)
    );
    for &key in keys.iter().skip(1).step_by(2) {
        tree.try_insert(key, !key).unwrap();
    }
    for (index, &key) in keys.iter().enumerate() {
        let expected = if index % 2 == 0 { key } else { !key };
        assert_eq!(tree.get(key), Some(&expected), "key {index}");
    }
}

/// A bitmap's growth refused at any of its three reservations — the words and
/// either summary — leaves it as it was.
#[test]
fn a_refused_bitmap_growth_leaves_it_as_it_was() {
    for granted in 0..3 {
        let mut bits = BitVec::try_with_len(100, false).unwrap();
        bits.set(7, true);
        assert_eq!(
            refusing_after(granted, || bits.try_resize(1 << 16, true)),
            Err(TryReserveError::AllocFailed),
            "{granted}"
        );
        assert_eq!(bits.len(), 100, "{granted}");
        assert_eq!(bits.count_ones(), 1, "{granted}");
        assert_eq!(bits.iter_ones().collect::<Vec<_>>(), [7], "{granted}");
        assert_eq!(bits.next_zero(7), Some(8), "{granted}");
        assert_eq!(bits.try_resize(1 << 16, true), Ok(()), "{granted}");
        assert_eq!(bits.count_ones(), (1 << 16) - 99, "{granted}");
    }
}

/// Once reserved for, a bitmap grows, is written and is searched without
/// asking the allocator for anything.
#[test]
fn a_reserved_bitmap_grows_writes_and_searches_without_allocating() {
    let mut bits = BitVec::new();
    bits.try_reserve(1 << 16).unwrap();
    let (found, metering) = metered(|| {
        refusing_above(0, || {
            bits.try_resize(1 << 16, false).unwrap();
            bits.set(9_000, true);
            bits.set_range(100..200, true);
            bits.fill(false);
            bits.set((1 << 16) - 1, true);
            (
                bits.next_one(0),
                bits.prev_zero(1 << 16),
                bits.iter_ones().count(),
            )
        })
    });
    assert_eq!(found, (Some((1 << 16) - 1), Some((1 << 16) - 2), 1));
    assert_eq!(metering.allocations, 0);
}

#[test]
fn a_shared_block_the_heap_refuses_is_a_refusal_and_frees_what_it_took() {
    let refused = Some(TryReserveError::AllocFailed);
    assert_eq!(
        refusing_above(0, || SharedSlice::try_from_slice(&[1_u8, 2, 3])).err(),
        refused
    );
    assert_eq!(
        refusing_above(0, || SharedStr::try_from_str("held")).err(),
        refused
    );
    assert_eq!(
        refusing_above(0, || try_arc(5_u32)).map(|shared| *shared),
        Err(TryReserveError::AllocFailed)
    );
    let drops = tairix_fuzzseed::Counted::counter();
    let items = vec![
        tairix_fuzzseed::Counted::new(&drops),
        tairix_fuzzseed::Counted::new(&drops),
    ];
    assert!(refusing_above(0, || SharedSlice::try_from_vec(items)).is_err());
    assert_eq!(drops.get(), 2, "the refused vector's items dropped with it");
    let empty = refusing_above(0, || SharedSlice::<u64>::try_from_slice(&[]));
    assert!(
        empty.is_ok_and(|empty| empty.is_empty()),
        "the empty slice takes nothing"
    );
    let (text, taken) = metered(|| SharedStr::try_from_str("held").expect("held"));
    assert_eq!(
        taken.allocations, 1,
        "one block holds the count, the length and the text"
    );
    let ((), freed) = metered(|| drop(text));
    assert_eq!(freed.frees, 1);
}
