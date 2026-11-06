//! [`OrderedMap`] unit tests: the shape every operation leaves, agreement
//! with `alloc`'s ordered map after every step, the mutable walks' relayout,
//! and the comparisons a lookup is gated on.

extern crate std;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;
use core::cmp::Ordering;
use core::fmt::Debug;
use core::ops::{Bound, RangeBounds};

use tairix_fuzzseed::{Counted, Prng};

use super::*;

/// Comparisons a lookup makes in one node: a binary search over at most
/// eleven keys.
const PER_NODE: usize = 5;

/// Sweep length: the stated count normally, a bounded slice under Miri, which
/// interprets every operation; the wide search is the ordinary stage's.
const fn sweep(full: u64) -> u64 {
    if cfg!(miri) && full > 300 {
        300
    } else {
        full
    }
}

/// What a walk of the whole tree from its root saw.
struct Seen<'a, K> {
    entries: usize,
    /// The leaves and the interior nodes.
    nodes: (usize, usize),
    last: Option<&'a K>,
}

impl<'a, K: Ord + Debug> Seen<'a, K> {
    fn key(&mut self, key: &'a K) {
        if let Some(last) = self.last {
            assert!(last < key, "{last:?} is not before {key:?}");
        }
        self.last = Some(key);
        self.entries += 1;
    }
}

/// Check the subtree at `node`, `depth` levels down, and every key in it.
fn check_subtree<'a, K: Ord + Debug, V>(
    map: &'a OrderedMap<K, V>,
    node: u32,
    depth: usize,
    seen: &mut Seen<'a, K>,
) {
    let level = map.height - depth;
    let held = map.node(node, level).expect("a named node");
    assert_eq!(held.next_free, NIL, "node {node} is reachable but freed");
    assert_eq!(held.keys.len(), held.vals.len());
    assert!(held.keys.len() <= CAP, "node {node} passed its bound");
    if depth == 1 {
        assert_eq!((node, held.parent), (map.root, NIL));
        assert!(!held.keys.is_empty(), "an empty root");
    } else {
        assert!(
            held.keys.len() >= MIN,
            "node {node} holds {}",
            held.keys.len()
        );
    }
    if level == 0 {
        seen.nodes.0 += 1;
        for key in &held.keys {
            seen.key(key);
        }
        return;
    }
    seen.nodes.1 += 1;
    let edges = &map.interiors.nodes[node as usize].edges;
    assert_eq!(edges.len(), held.keys.len() + 1);
    for (at, &child) in edges.iter().enumerate() {
        let below = map.node(child, level - 1).expect("a named child");
        assert_eq!(below.parent, node, "child {child} names another parent");
        check_subtree(map, child, depth + 1, seen);
        if let Some(key) = held.keys.get(at) {
            seen.key(key);
        }
    }
}

/// Every invariant the tree keeps: keys strictly ascending, every node but
/// the root at least `MIN` full and none past `CAP`, an interior node one
/// child more than it has keys, each child naming its parent, every leaf at
/// the height, `len` the entries held, and every live node of either arena
/// reachable.
fn assert_shape<K: Ord + Debug, V>(map: &OrderedMap<K, V>) {
    let mut seen = Seen {
        entries: 0,
        nodes: (0, 0),
        last: None,
    };
    if map.root == NIL {
        assert_eq!((map.len, map.height), (0, 0));
    } else {
        check_subtree(map, map.root, 1, &mut seen);
    }
    assert_eq!(seen.entries, map.len, "the length drifted");
    let live = (map.leaves.live(), map.interiors.live());
    assert_eq!(seen.nodes, live, "a live node is unreachable");
}

/// The keys mapped to themselves, inserted in an order drawn from `seed`.
fn shuffled(keys: impl Iterator<Item = u64>, seed: u64) -> OrderedMap<u64, u64> {
    let mut order: Vec<u64> = keys.collect();
    let mut rng = Prng::new(seed);
    for at in (1..order.len()).rev() {
        order.swap(at, rng.below(at + 1));
    }
    let mut map = OrderedMap::new();
    for key in order {
        assert_eq!(map.try_insert(key, key), Ok(None));
    }
    map
}

/// A bound at `at`: included, excluded, or none.
fn draw_bound(rng: &mut Prng, at: u64) -> Bound<u64> {
    match rng.below(3) {
        0 => Bound::Included(at),
        1 => Bound::Excluded(at),
        _ => Bound::Unbounded,
    }
}

#[test]
fn an_empty_map_holds_nothing_and_finds_nothing() {
    let mut map: OrderedMap<u64, u64> = OrderedMap::new();
    assert!(map.is_empty());
    assert_eq!((map.len(), map.height(), map.allocated_bytes()), (0, 0, 0));
    assert_eq!(map.get(&1), None);
    assert_eq!((map.first_key_value(), map.last_key_value()), (None, None));
    assert_eq!(map.iter().len(), 0);
    assert_eq!(map.range(..).next_back(), None);
    assert_eq!(map.iter_mut().next(), None);
    assert_eq!(map.range_mut(..).next_back(), None);
    assert_eq!(
        (map.pop_first(), map.pop_last(), map.remove(&1)),
        (None, None, None)
    );
    assert_shape(&map);
    assert_eq!(map.into_iter().next(), None);
}

#[test]
fn an_insertion_answers_the_value_it_replaces() {
    let mut map = OrderedMap::new();
    assert_eq!(map.try_insert(5, 'a'), Ok(None));
    assert_eq!(
        map.try_insert(5, 'b'),
        Ok(Some('a')),
        "a replaced value is answered"
    );
    assert_eq!(map.len(), 1);
    assert_eq!(map.get_key_value(&5), Some((&5, &'b')));
    *map.get_mut(&5).unwrap() = 'c';
    assert!(map.contains_key(&5) && !map.contains_key(&6));
    assert_eq!(map.remove_entry(&5), Some((5, 'c')));
    assert_eq!(map.remove(&5), None);
    assert!(map.is_empty());
    assert_eq!(
        (map.height(), map.leaves.live(), map.interiors.live()),
        (0, 0, 0),
        "an emptied map holds no node"
    );
}

#[test]
fn every_order_of_insertion_leaves_one_ordered_tree() {
    let count = sweep(3_000);
    let orders: [Vec<u64>; 3] = [
        (0..count).collect(),
        (0..count).rev().collect(),
        (0..count).step_by(2).chain((1..count).step_by(2)).collect(),
    ];
    for order in orders {
        let mut map = OrderedMap::new();
        for &key in &order {
            assert_eq!(map.try_insert(key, key * 3), Ok(None));
        }
        assert_shape(&map);
        let expected = (0..count).map(|key| (key, key * 3));
        assert!(map.iter().map(|(&key, &value)| (key, value)).eq(expected));
        assert!(map.keys().rev().copied().eq((0..count).rev()));
        assert!(map.values().copied().eq((0..count).map(|key| key * 3)));
    }
    let map = shuffled(0..count, 0x5eed);
    assert_shape(&map);
    assert_eq!(map.len(), usize::try_from(count).unwrap());
}

/// One write to the map and to its model, their answers compared.
fn write_step(
    rng: &mut Prng,
    map: &mut OrderedMap<u64, u64>,
    model: &mut BTreeMap<u64, u64>,
    key: u64,
    stamp: u64,
) {
    match rng.below(10) {
        0..=3 => assert_eq!(map.try_insert(key, stamp), Ok(model.insert(key, stamp))),
        4 | 5 => assert_eq!(map.remove(&key), model.remove(&key)),
        6 => assert_eq!(map.pop_first(), model.pop_first()),
        7 => assert_eq!(map.pop_last(), model.pop_last()),
        8 => {
            let held = map.get_mut(&key).map(|value| {
                *value ^= stamp;
                *value
            });
            let expected = model.get_mut(&key).map(|value| {
                *value ^= stamp;
                *value
            });
            assert_eq!(held, expected);
        }
        _ if rng.below(16) == 0 => {
            let every = 2 + stamp % 5;
            map.retain(|key, _| key % every != 0);
            model.retain(|key, _| key % every != 0);
        }
        _ => {}
    }
}

/// Reads of the map, checked against its model.
fn read_step(rng: &mut Prng, map: &OrderedMap<u64, u64>, model: &BTreeMap<u64, u64>, key: u64) {
    assert_eq!(map.get(&key), model.get(&key));
    assert_eq!(map.contains_key(&key), model.contains_key(&key));
    assert_eq!(map.first_key_value(), model.first_key_value());
    assert_eq!(map.last_key_value(), model.last_key_value());
    if rng.below(8) == 0 {
        let reach = key.saturating_add(rng.next_u64() % 4096);
        let bounds = (draw_bound(rng, key), draw_bound(rng, reach));
        let expected: Vec<(&u64, &u64)> = model
            .iter()
            .filter(|(key, _)| bounds.contains(*key))
            .collect();
        assert_eq!(
            map.range(bounds).collect::<Vec<_>>(),
            expected,
            "{bounds:?}"
        );
        assert!(map.range(bounds).rev().eq(expected.iter().rev().copied()));
    }
}

#[test]
fn every_operation_agrees_with_alloc_s_ordered_map() {
    let mut rng = Prng::new(0x0bde_43d2_9c51_7a6e);
    let mut map = OrderedMap::new();
    let mut model = BTreeMap::new();
    for step in 0..sweep(6_000) {
        let key = match rng.below(3) {
            0 => rng.below(2_000) as u64,
            1 => u64::MAX - rng.below(64) as u64,
            _ => rng.next_u64(),
        };
        write_step(&mut rng, &mut map, &mut model, key, step);
        read_step(&mut rng, &map, &model, key);
        assert_eq!(map.len(), model.len(), "step {step}");
        assert_shape(&map);
    }
    assert!(map.iter().eq(model.iter()));
}

/// A range of `map` against the `keys` its bounds admit, walked from the
/// front, from the back, and from both ends in turn.
fn check_range(map: &OrderedMap<u64, u64>, keys: &[u64], bounds: (Bound<u64>, Bound<u64>)) {
    let expected: Vec<u64> = keys
        .iter()
        .copied()
        .filter(|key| bounds.contains(key))
        .collect();
    let forward: Vec<u64> = map.range(bounds).map(|(&key, _)| key).collect();
    assert_eq!(forward, expected, "{bounds:?}");
    let backward = map.range(bounds).rev().map(|(&key, _)| key);
    assert!(
        backward.eq(expected.iter().rev().copied()),
        "{bounds:?} backward"
    );
    let mut cursor = map.range(bounds);
    let mut ends = (Vec::new(), Vec::new());
    for turn in 0usize.. {
        let taken = if turn.is_multiple_of(2) {
            cursor.next()
        } else {
            cursor.next_back()
        };
        let Some((&key, _)) = taken else {
            break;
        };
        if turn.is_multiple_of(2) {
            ends.0.push(key);
        } else {
            ends.1.push(key);
        }
    }
    ends.0.extend(ends.1.iter().rev());
    assert_eq!(ends.0, expected, "{bounds:?} from both ends");
}

#[test]
fn every_bound_shape_admits_exactly_what_it_says() {
    let keys: Vec<u64> = (0..60).map(|key| key * 2).collect();
    let map = shuffled(keys.iter().copied(), 7);
    let stride = if cfg!(miri) { 17 } else { 1 };
    let points: Vec<u64> = (0..=121).step_by(stride).collect();
    let shapes = |at: u64| [Bound::Included(at), Bound::Excluded(at), Bound::Unbounded];
    for &low in &points {
        for start in shapes(low) {
            for &high in &points {
                for end in shapes(high) {
                    check_range(&map, &keys, (start, end));
                }
            }
        }
    }
}

#[test]
fn a_mutable_walk_from_both_ends_hands_out_each_value_once_in_order() {
    for count in [0, 1, 11, 12, 100, sweep(3_000)] {
        let mut map = shuffled(0..count, count);
        let mut rng = Prng::new(count ^ 0xa5a5);
        let mut cursor = map.iter_mut();
        let (mut low, mut high) = (0, count);
        while low < high {
            if rng.below(2) == 0 {
                let (key, value) = cursor.next().unwrap();
                assert_eq!(*key, low);
                *value += 1 << 40;
                low += 1;
            } else {
                high -= 1;
                let (key, value) = cursor.next_back().unwrap();
                assert_eq!(*key, high);
                *value += 1 << 40;
            }
            assert_eq!(cursor.len(), usize::try_from(high - low).unwrap());
        }
        assert!(cursor.next().is_none() && cursor.next_back().is_none());
        drop(cursor);
        assert_shape(&map);
        assert!(map.iter().all(|(&key, &value)| value == key + (1 << 40)));
        for value in map.values_mut() {
            *value -= 1 << 40;
        }
        assert!(map.iter().all(|(&key, &value)| value == key));
        assert_shape(&map);
    }
}

#[test]
fn a_mutable_range_changes_exactly_what_it_admits_and_leaves_the_tree_whole() {
    let mut map = shuffled((0..80).map(|key| key * 2), 3);
    let mut model: BTreeMap<u64, u64> = (0..80).map(|key| (key * 2, key * 2)).collect();
    let mut rng = Prng::new(11);
    for round in 0..sweep(400) {
        let (low, high) = (rng.below(170) as u64, rng.below(170) as u64);
        let bounds = (draw_bound(&mut rng, low), draw_bound(&mut rng, high));
        let expected: Vec<u64> = model
            .keys()
            .copied()
            .filter(|key| bounds.contains(key))
            .collect();
        // Every eighth walk is abandoned part-way.
        let abandon = round
            .is_multiple_of(8)
            .then(|| rng.below(expected.len() + 1));
        let mut cursor = map.range_mut(bounds);
        let mut ends = (Vec::new(), Vec::new());
        while abandon != Some(ends.0.len() + ends.1.len()) {
            let from_front = rng.below(2) == 0;
            let taken = if from_front {
                cursor.next()
            } else {
                cursor.next_back()
            };
            let Some((&key, value)) = taken else {
                break;
            };
            *value += 1;
            if from_front {
                ends.0.push(key);
            } else {
                ends.1.push(key);
            }
        }
        drop(cursor);
        for key in ends.0.iter().chain(&ends.1) {
            *model.get_mut(key).unwrap() += 1;
        }
        if abandon.is_none() {
            ends.0.extend(ends.1.iter().rev());
            assert_eq!(ends.0, expected, "{bounds:?}");
        }
        assert_shape(&map);
        assert!(map.iter().eq(model.iter()), "round {round}");
    }
}

/// A mutable range walk over a tree three or more levels tall leaves it
/// whole: the nodes beside the range under its ends' first paths stay
/// unopened, and a swap that moves one after its parent's frame is used up
/// still re-points that parent.
#[test]
fn a_mutable_range_over_a_tall_tree_re_points_the_nodes_beside_it() {
    let count = sweep(3_000);
    let mut map = OrderedMap::new();
    for key in 0..count {
        map.try_insert(key, key).unwrap();
    }
    assert!(map.height() >= 3, "height {}", map.height());
    let mut model: BTreeMap<u64, u64> = (0..count).map(|key| (key, key)).collect();
    let mut rng = Prng::new(0x7a11);
    for round in 0..sweep(200) {
        let mut span = |len: u64| u64::try_from(rng.below(usize::try_from(len).unwrap())).unwrap();
        let low = span(count);
        let high = low + span(count - low + 1);
        let mut cursor = map.range_mut(low..high);
        let mut touched = Vec::new();
        loop {
            let taken = if rng.below(2) == 0 {
                cursor.next()
            } else {
                cursor.next_back()
            };
            let Some((&key, value)) = taken else {
                break;
            };
            *value += 1;
            touched.push(key);
        }
        drop(cursor);
        for key in touched {
            *model.get_mut(&key).unwrap() += 1;
        }
        assert_shape(&map);
        assert!(map.iter().eq(model.iter()), "round {round}: {low}..{high}");
        assert!(model.iter().all(|(key, value)| map.get(key) == Some(value)));
    }
}

#[test]
fn an_owning_walk_takes_entries_in_order_from_both_ends() {
    for count in [0, 1, 11, 12, 133, sweep(2_000)] {
        let map = shuffled(0..count, count + 1);
        let mut rng = Prng::new(count);
        let mut owned = map.into_iter();
        let (mut low, mut high) = (0, count);
        while low < high {
            if rng.below(2) == 0 {
                assert_eq!(owned.next(), Some((low, low)));
                low += 1;
            } else {
                high -= 1;
                assert_eq!(owned.next_back(), Some((high, high)));
            }
            assert_eq!(owned.len(), usize::try_from(high - low).unwrap());
        }
        assert_eq!((owned.next(), owned.next_back()), (None, None));
    }
}

#[test]
fn every_value_is_dropped_exactly_once() {
    let drops = Counted::counter();
    let mut made = 0;
    let mut map: OrderedMap<u64, Counted> = OrderedMap::new();
    for key in 0..300 {
        assert!(map.try_insert(key, Counted::new(&drops)).unwrap().is_none());
        made += 1;
    }
    for key in 0..20 {
        assert!(map.try_insert(key, Counted::new(&drops)).unwrap().is_some());
        made += 1;
    }
    for key in 20..60 {
        assert!(map.remove(&key).is_some());
    }
    map.retain(|key, _| key % 3 != 0);
    drop(map.pop_first());
    drop(map.pop_last());
    for value in map.values_mut() {
        *value = Counted::new(&drops);
        made += 1;
    }
    let mut owned = map.into_iter();
    drop((owned.next(), owned.next_back()));
    drop(owned);
    assert_eq!(drops.get(), made);
}

/// A key that counts every comparison made of it.
struct Probed<'c> {
    key: u64,
    comparisons: &'c Cell<usize>,
}

impl PartialEq for Probed<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Probed<'_> {}

impl PartialOrd for Probed<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Probed<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.comparisons.set(self.comparisons.get() + 1);
        self.key.cmp(&other.key)
    }
}

/// The counter gate: no lookup compares more than five keys a level, and the
/// tree stands no taller than its fill floor allows, so a lookup costs at
/// most about twice the binary logarithm of the entries.
#[test]
fn a_lookup_compares_at_most_five_keys_a_level() {
    let comparisons = Cell::new(0);
    for count in [64, 1_000, sweep(100_000)] {
        let mut keys: Vec<u64> = (0..count).collect();
        let mut rng = Prng::new(count);
        for at in (1..keys.len()).rev() {
            keys.swap(at, rng.below(at + 1));
        }
        let mut map = OrderedMap::new();
        for &key in &keys {
            let probe = Probed {
                key,
                comparisons: &comparisons,
            };
            map.try_insert(probe, ()).unwrap();
        }
        let entries = usize::try_from(count).unwrap();
        assert!(
            map.height() <= tallest(entries),
            "{count} entries stand {}",
            map.height()
        );
        let bits = usize::try_from(usize::BITS - entries.leading_zeros()).unwrap();
        let bound = PER_NODE * map.height();
        assert!(
            bound <= 2 * bits + PER_NODE,
            "{bound} comparisons for {count}"
        );
        for key in 0..count + 8 {
            comparisons.set(0);
            let probe = Probed {
                key,
                comparisons: &comparisons,
            };
            assert_eq!(map.contains_key(&probe), key < count);
            assert!(
                comparisons.get() <= bound,
                "{} for {key} of {count}",
                comparisons.get()
            );
        }
    }
}

/// The slots both arenas hold, leaves first.
fn capacities<K, V>(map: &OrderedMap<K, V>) -> (usize, usize) {
    (map.leaves.nodes.capacity(), map.interiors.nodes.capacity())
}

/// A map that stays one node takes no spare node to reserve for: up to two
/// floors' worth of entries no split can come of.
#[test]
fn a_reserve_a_single_node_absorbs_takes_no_spare_node() {
    let mut map = OrderedMap::new();
    map.try_insert(0u64, 0u64).unwrap();
    let held = capacities(&map);
    let room = 2 * super::MIN - map.len();
    map.try_reserve(room).unwrap();
    assert_eq!(capacities(&map), held, "a spare node was taken");
    for key in 1..=room as u64 {
        map.try_insert(key, key).unwrap();
    }
    assert_eq!(capacities(&map), held);
    assert_eq!(map.height(), 1, "the reserved entries fit the one node");
}

#[test]
fn a_reserved_run_of_insertions_never_grows_the_arena() {
    let mut rng = Prng::new(0x7e5e_57ed);
    let mut map = OrderedMap::new();
    let (mut ascending, mut descending) = (1u64 << 62, 1u64 << 62);
    for round in 0..sweep(300) {
        let batch = 1 + rng.below(40);
        map.try_reserve(batch).unwrap();
        let capacity = capacities(&map);
        for _ in 0..batch {
            let key = match rng.below(3) {
                0 => {
                    ascending += 1;
                    ascending
                }
                1 => {
                    descending -= 1;
                    descending
                }
                _ => rng.next_u64(),
            };
            map.try_insert(key, round).unwrap();
        }
        assert_eq!(
            capacities(&map),
            capacity,
            "round {round}: a reserved insertion grew an arena"
        );
        if round.is_multiple_of(3) {
            let victims: Vec<u64> = map.keys().copied().step_by(4).collect();
            for key in victims {
                map.remove(&key);
            }
        }
    }
    assert_shape(&map);
}

#[test]
fn shrinking_holds_only_the_live_nodes_and_keeps_every_entry() {
    let count = sweep(4_000);
    let mut map = shuffled(0..count, 4);
    let peak = map.allocated_bytes();
    map.retain(|key, _| key.is_multiple_of(16));
    map.try_shrink_to_fit().unwrap();
    assert_shape(&map);
    assert_eq!(
        (map.leaves.free_count(), map.interiors.free_count()),
        (0, 0)
    );
    assert_eq!(map.leaves.nodes.len(), map.leaves.live());
    assert_eq!(map.interiors.nodes.len(), map.interiors.live());
    assert!(map.leaves.nodes.fitted() && map.interiors.nodes.fitted());
    assert!(
        map.allocated_bytes() < peak / 4,
        "{} of {peak}",
        map.allocated_bytes()
    );
    for key in 0..count {
        assert_eq!(map.get(&key), key.is_multiple_of(16).then_some(&key));
    }
}

#[test]
fn clearing_gives_back_every_node() {
    let mut map = shuffled(0..sweep(1_000), 5);
    assert!(map.allocated_bytes() > 0);
    map.clear();
    assert!(map.is_empty());
    assert_eq!((map.allocated_bytes(), map.height()), (0, 0));
    assert_eq!(map.get(&3), None);
    assert_eq!(map.try_insert(3, 3), Ok(None));
    assert_shape(&map);
}

#[test]
fn a_copy_matches_node_for_node_and_changes_apart() {
    let mut map = shuffled(0..sweep(500), 9);
    map.retain(|key, _| key % 5 != 0);
    let mut copy = map.try_clone().unwrap();
    assert_eq!(copy, map);
    assert_shape(&copy);
    assert_eq!(copy.try_insert(1, 7), Ok(Some(1)));
    assert_ne!(copy, map);
    assert_eq!(map.get(&1), Some(&1), "the original is untouched");
    assert_eq!(format!("{:?}", shuffled(0..3, 1)), "{0: 0, 1: 1, 2: 2}");
}

#[test]
fn a_key_is_found_through_what_it_borrows_as() {
    let mut map: OrderedMap<String, usize> = OrderedMap::new();
    for (at, word) in ["delta", "alpha", "charlie", "bravo"]
        .into_iter()
        .enumerate()
    {
        assert_eq!(map.try_insert(String::from(word), at), Ok(None));
    }
    assert_eq!(map.get("charlie"), Some(&2));
    assert!(map.contains_key("alpha") && !map.contains_key("echo"));
    let between: Vec<&str> = map
        .range::<str, _>((Bound::Included("b"), Bound::Excluded("d")))
        .map(|(word, _)| word.as_str())
        .collect();
    assert_eq!(between, ["bravo", "charlie"]);
    assert_eq!(map.remove("delta"), Some(0));
    assert_eq!(map.first_key_value(), Some((&String::from("alpha"), &1)));
}

#[test]
fn retain_keeps_exactly_what_it_accepts_in_key_order() {
    let count = sweep(2_000);
    let mut map = shuffled(0..count, 17);
    let mut visited = Vec::new();
    map.retain(|&key, value| {
        visited.push(key);
        *value += 1;
        key % 3 == 1
    });
    assert!(visited.iter().copied().eq(0..count), "visited out of order");
    assert_shape(&map);
    let kept = (0..count)
        .filter(|key| key % 3 == 1)
        .map(|key| (key, key + 1));
    assert!(map.iter().map(|(&key, &value)| (key, value)).eq(kept));
    map.retain(|_, _| false);
    assert!(map.is_empty());
    assert_shape(&map);
}

/// The tree stands no taller, and holds no more leaves or interior nodes,
/// than the reservation bounds allow its entries.
fn assert_within_bounds<K, V>(map: &OrderedMap<K, V>) {
    assert!(map.height() <= tallest(map.len()));
    assert!(map.leaves.live() <= most_leaves(map.len()));
    assert!(map.interiors.live() <= most_interiors(map.len()));
}

#[test]
fn the_reservation_bounds_hold_for_every_tree_they_describe() {
    assert_eq!([0, 1, 10, 11, 70, 71].map(tallest), [0, 1, 1, 2, 2, 3]);
    assert_eq!([0, 1, 10, 11, 16, 17].map(most_leaves), [0, 1, 1, 2, 2, 3]);
    assert_eq!([0, 10, 11, 71].map(most_interiors), [0, 0, 1, 3]);
    assert!(tallest(usize::MAX) < PATHS);
    let mut map = OrderedMap::new();
    for key in 0..sweep(5_000) {
        map.try_insert(key, ()).unwrap();
        assert_within_bounds(&map);
    }
    while map.pop_first().is_some() {
        assert_within_bounds(&map);
    }
}

#[test]
fn no_tree_the_arena_can_index_outgrows_a_walk_s_frames() {
    let fanout = u64::try_from(B).unwrap();
    // The fewest nodes a tree `height` tall holds: a root of two children,
    // and every interior node below it of `B`.
    let fewest = |height: u32| 1 + 2 * (fanout.pow(height - 1) - 1) / (fanout - 1);
    let height = u32::try_from(MAX_HEIGHT).unwrap();
    assert!(u32::try_from(fewest(height)).is_ok());
    assert!(fewest(height + 1) > u64::from(u32::MAX));
    // And the fewest leaves, which an arena of their own indexes.
    let fewest_leaves = |height: u32| 2 * fanout.pow(height - 2);
    assert!(fewest_leaves(height + 1) > u64::from(u32::MAX));
}

#[test]
fn a_key_is_refiled_only_where_its_order_holds() {
    let mut map = shuffled((1..=50).map(|key| key * 10), 13);
    // Interior entries among them as well as leaf ones.
    for key in (1..=50).map(|key| key * 10) {
        assert!(map.rekey(&key, key - 1), "{key} between its neighbours");
        assert!(map.rekey(&(key - 1), key), "{key} back");
    }
    assert!(!map.rekey(&200, 190), "onto its predecessor");
    assert!(!map.rekey(&200, 210), "onto its successor");
    assert!(!map.rekey(&200, 215), "past its successor");
    assert!(!map.rekey(&7, 8), "nothing filed there");
    assert!(
        map.rekey(&10, 1) && map.rekey(&500, 9_999),
        "either end moves outward"
    );
    assert_eq!(map.get(&1), Some(&10));
    assert_eq!(map.get(&9_999), Some(&500));
    assert_shape(&map);
}

/// The layout the arenas were split for: a leaf holds its entries and two
/// links and nothing else, and an interior node holds a leaf's fields and its
/// children inline. On a 64-bit host an `OrderedSet<u64>`'s leaf is 112 bytes
/// and its interior node 168, an `OrderedMap<u64, u64>`'s 200 and 256, where
/// one shape for both was 168 and 256.
#[test]
fn a_leaf_carries_no_children_and_an_interior_node_carries_them_inline() {
    fn check<V>() {
        let entries = mem::size_of::<ArrayVec<u64, CAP>>() + mem::size_of::<ArrayVec<V, CAP>>();
        let links = 2 * mem::size_of::<u32>();
        assert!(
            mem::size_of::<Leaf<u64, V>>() <= entries + links,
            "a leaf holds more than its entries and links"
        );
        let edges = mem::size_of::<ArrayVec<u32, EDGES>>();
        assert!(
            mem::size_of::<Interior<u64, V>>() >= mem::size_of::<Leaf<u64, V>>() + edges,
            "an interior node holds its children elsewhere"
        );
    }
    check::<()>();
    check::<u64>();
}
