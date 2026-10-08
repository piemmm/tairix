extern crate std;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::*;

const DIRTY: usize = 0;
const WRITEBACK: usize = 1;

/// A small xorshift stream, so each run draws the same operations.
struct Stream(u64);

impl Stream {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn a_value_is_found_at_its_key_and_nowhere_else() {
    let mut tree: RadixTree<u32> = RadixTree::new();
    assert_eq!(tree.height(), 0);
    assert_eq!(tree.try_insert(5, 50), Ok(None));
    assert_eq!(
        tree.try_insert(5, 51),
        Ok(Some(50)),
        "a replaced value is answered"
    );
    assert_eq!(tree.get(5), Some(&51));
    assert_eq!(tree.get(6), None);
    assert_eq!(tree.get(5 + 64), None, "a key past the tree's height");
    assert_eq!(tree.len(), 1);
    *tree.get_mut(5).unwrap() = 52;
    assert_eq!(tree.remove(5), Some(52));
    assert_eq!(tree.remove(5), None);
    assert!(tree.is_empty());
    assert_eq!(
        tree.resident_nodes(),
        (0, 0),
        "an emptied tree holds no node"
    );
}

#[test]
fn the_tree_grows_to_its_largest_key_and_shrinks_back() {
    let mut tree: RadixTree<u64> = RadixTree::new();
    for key in [0, 63, 64, 4095, 4096, 1 << 40, u64::MAX] {
        tree.try_insert(key, key).unwrap();
        assert!(tree.height() <= MAX_HEIGHT);
    }
    assert_eq!(tree.height(), MAX_HEIGHT, "the last key needs every level");
    for key in [0, 63, 64, 4095, 4096, 1 << 40, u64::MAX] {
        assert_eq!(tree.get(key), Some(&key));
    }
    tree.remove(u64::MAX);
    assert_eq!(tree.height(), 7, "1 << 40 needs seven levels");
    tree.remove(1 << 40);
    assert_eq!(tree.height(), 3, "4096 needs three");
    tree.remove(4096);
    tree.remove(4095);
    tree.remove(64);
    assert_eq!(tree.height(), 1, "63 fits one leaf");
    assert_eq!(tree.resident_nodes(), (0, 1));
}

#[test]
fn successors_and_predecessors_cross_empty_subtrees_at_every_level() {
    let mut tree: RadixTree<()> = RadixTree::new();
    let keys = [
        3,
        64 * 63 + 63,
        64 * 64,
        1 << 30,
        (1 << 30) + 1,
        u64::MAX - 1,
    ];
    for key in keys {
        tree.try_insert(key, ()).unwrap();
    }
    for (index, &key) in keys.iter().enumerate() {
        assert_eq!(tree.next(key).map(|(at, ())| at), Some(key));
        assert_eq!(tree.prev(key).map(|(at, ())| at), Some(key));
        assert_eq!(
            tree.next(key + 1).map(|(at, ())| at),
            keys.get(index + 1).copied(),
            "after {key:#x}"
        );
        assert_eq!(
            tree.prev(key - 1).map(|(at, ())| at),
            index.checked_sub(1).map(|before| keys[before]),
            "before {key:#x}"
        );
    }
    assert_eq!(tree.prev(2), None);
    assert_eq!(tree.next(u64::MAX), None);
    assert_eq!(tree.prev(u64::MAX).map(|(at, ())| at), Some(u64::MAX - 1));
}

/// A predecessor found in an earlier slot of the top level is that slot's
/// greatest key: every digit below the top's, all sixty bits of them.
#[test]
fn a_predecessor_in_an_earlier_top_slot_is_its_greatest_key() {
    let mut tree: RadixTree<()> = RadixTree::new();
    tree.try_insert(0x5b51_c992_3410_0000, ()).unwrap();
    tree.try_insert(0xfb5a_3753_9830_0000, ()).unwrap();
    assert_eq!(
        tree.prev(0x7042_8c42_283d_eed3).map(|(at, ())| at),
        Some(0x5b51_c992_3410_0000)
    );
}

#[test]
fn a_walk_from_a_key_is_a_gang_lookup_in_key_order() {
    let mut tree: RadixTree<u64> = RadixTree::new();
    let keys = if cfg!(miri) { 200 } else { 2000 };
    for key in (0..keys).map(|k| k * 37) {
        tree.try_insert(key, key + 1).unwrap();
    }
    let gang: Vec<(u64, u64)> = tree.iter_from(1000).take(4).map(|(k, v)| (k, *v)).collect();
    assert_eq!(
        gang,
        [(1036, 1037), (1073, 1074), (1110, 1111), (1147, 1148)]
    );
    assert_eq!(tree.iter().count(), usize::try_from(keys).unwrap());
    assert!(tree.iter().zip(tree.iter().skip(1)).all(|(a, b)| a.0 < b.0));
}

#[test]
fn a_tagged_walk_visits_only_tagged_entries_and_a_removal_untags() {
    let mut tree: RadixTree<u64, 2> = RadixTree::new();
    let keys = if cfg!(miri) { 1000 } else { 5000 };
    for key in (0..keys).chain([4999]) {
        tree.try_insert(key * 1000, key).unwrap();
    }
    for key in [7, 70, 700, 4999] {
        assert!(tree.set_tag(key * 1000, DIRTY));
    }
    assert!(!tree.set_tag(1, DIRTY), "no entry at 1 to tag");
    assert!(!tree.set_tag(0, 2), "a tag past the tree's is none");
    let dirty: Vec<u64> = tree.iter_tagged(0, DIRTY).map(|(_, v)| *v).collect();
    assert_eq!(dirty, [7, 70, 700, 4999]);
    assert!(!tree.any_tagged(WRITEBACK));
    assert!(tree.clear_tag(70_000, DIRTY));
    assert!(!tree.clear_tag(70_000, DIRTY), "already clear");
    assert!(tree.set_tag(70_000, WRITEBACK));
    assert_eq!(tree.next_tagged(8000, DIRTY).map(|(k, _)| k), Some(700_000));
    assert_eq!(tree.try_insert(700_000, 1), Ok(Some(700)));
    assert!(
        tree.is_tagged(700_000, DIRTY),
        "a replaced value keeps its key's tags"
    );
    tree.remove(700_000);
    tree.remove(7000);
    tree.remove(4_999_000);
    assert!(
        !tree.any_tagged(DIRTY),
        "removing every tagged entry clears the tag up the tree"
    );
    assert!(tree.any_tagged(WRITEBACK));
}

/// A reserved insertion draws nothing more from the allocator, whether it is
/// made through `try_insert` or straight into the room: every node it needs
/// is already in an arena.
#[test]
fn an_insertion_reserved_for_allocates_nothing() {
    let mut tree: RadixTree<u64> = RadixTree::new();
    let mut stream = Stream(0x9E37_79B9_7F4A_7C15);
    let rounds = if cfg!(miri) { 60 } else { 500 };
    for round in 0..rounds {
        let key = stream.next() >> (stream.next() % 64);
        tree.try_reserve_key(key).unwrap();
        let capacity = (tree.inner.nodes.capacity(), tree.leaves.nodes.capacity());
        let (inner, leaves) = tree.missing(key);
        let free = (tree.inner.free_count, tree.leaves.free_count);
        let lengths = (tree.inner.nodes.len(), tree.leaves.nodes.len());
        assert!(capacity.0 - lengths.0 + free.0 >= inner, "{key:#x}");
        assert!(capacity.1 - lengths.1 + free.1 >= leaves, "{key:#x}");
        if round % 2 == 0 {
            tree.insert_reserved(key, key);
        } else {
            tree.try_insert(key, key).unwrap();
        }
        assert_eq!(tree.get(key), Some(&key));
        assert_eq!(
            (tree.inner.nodes.capacity(), tree.leaves.nodes.capacity()),
            capacity,
            "inserting {key:#x} grew an arena"
        );
        if stream.next().is_multiple_of(3) {
            let victim = tree.iter_from(stream.next()).next().map(|(k, _)| k);
            if let Some(victim) = victim {
                tree.remove(victim);
            }
        }
    }
}

/// The nodes resident are the live keys' paths: churn through the whole key
/// space leaves no more than the survivors need, freed nodes reused first.
#[test]
fn churn_leaves_only_the_live_keys_paths_resident() {
    let mut tree: RadixTree<u64> = RadixTree::new();
    let mut stream = Stream(7);
    let mut live = Vec::new();
    let rounds = if cfg!(miri) { 600 } else { 20_000 };
    for _ in 0..rounds {
        let key = stream.next();
        if tree.try_insert(key, key).unwrap().is_none() {
            live.push(key);
        }
        if live.len() > 64 {
            let victim = live.swap_remove(usize::try_from(stream.next() % 64).unwrap());
            assert_eq!(tree.remove(victim), Some(victim));
        }
    }
    let (inner, leaves) = tree.resident_nodes();
    assert!(
        leaves <= live.len(),
        "{leaves} leaves for {} keys",
        live.len()
    );
    assert!(
        inner <= live.len() * (MAX_HEIGHT - 1),
        "{inner} interior nodes"
    );
    let arena = tree.inner.nodes.len() + tree.leaves.nodes.len();
    assert!(
        arena < 2 * 65 * MAX_HEIGHT,
        "{arena} nodes for a peak of 65 keys"
    );
}

/// Every operation agrees with an ordered map after every step, over keys
/// drawn dense, sparse and clustered at the ends of the key space.
#[test]
fn every_operation_agrees_with_an_ordered_map() {
    let rounds = if cfg!(miri) { 300 } else { 20_000 };
    let mut tree: RadixTree<u64, 1> = RadixTree::new();
    let mut model: BTreeMap<u64, (u64, bool)> = BTreeMap::new();
    let mut stream = Stream(0x1234_5678_9ABC_DEF1);
    for round in 0..rounds {
        let draw = stream.next();
        let key = match draw % 4 {
            0 => draw >> 52,
            1 => u64::MAX - (draw >> 54),
            2 => (draw >> 20) << 20,
            _ => stream.next(),
        };
        match stream.next() % 6 {
            0 | 1 => {
                let old = tree.try_insert(key, round).unwrap();
                let tagged = model.get(&key).is_some_and(|&(_, t)| t);
                assert_eq!(old, model.insert(key, (round, tagged)).map(|(v, _)| v));
            }
            2 => {
                assert_eq!(tree.remove(key), model.remove(&key).map(|(v, _)| v));
            }
            3 => {
                let present = model.get_mut(&key).map(|entry| entry.1 = true).is_some();
                assert_eq!(tree.set_tag(key, DIRTY), present);
            }
            4 => {
                let tagged = model
                    .get_mut(&key)
                    .is_some_and(|entry| core::mem::replace(&mut entry.1, false));
                assert_eq!(tree.clear_tag(key, DIRTY), tagged);
            }
            _ => {}
        }
        assert_eq!(tree.len(), model.len());
        assert_eq!(tree.get(key), model.get(&key).map(|(v, _)| v));
        let probe = stream.next();
        assert_eq!(
            tree.next(probe).map(|(k, v)| (k, *v)),
            model.range(probe..).next().map(|(&k, &(v, _))| (k, v))
        );
        assert_eq!(
            tree.prev(probe).map(|(k, v)| (k, *v)),
            model
                .range(..=probe)
                .next_back()
                .map(|(&k, &(v, _))| (k, v))
        );
        assert_eq!(
            tree.next_tagged(probe, DIRTY).map(|(k, _)| k),
            model.range(probe..).find(|(_, &(_, t))| t).map(|(&k, _)| k)
        );
    }
    assert!(tree
        .iter()
        .map(|(k, v)| (k, *v))
        .eq(model.iter().map(|(&k, &(v, _))| (k, v))));
}

/// A walk resumes from the path it last took, so it crosses a leaf's last
/// slot, and every level's, into the next subtree, and a tagged walk crosses
/// untagged subtrees, exactly as a fresh search from each key would.
#[test]
fn a_walk_crosses_every_level_s_last_slot_into_the_next_subtree() {
    let mut tree: RadixTree<u64, 1> = RadixTree::new();
    let mut model = BTreeMap::new();
    for level in 0..MAX_HEIGHT {
        let edge = below(level);
        for key in [edge - 1, edge, edge.wrapping_add(1), edge.wrapping_add(64)] {
            tree.try_insert(key, key).unwrap();
            model.insert(key, key);
        }
    }
    for key in [u64::MAX - 1, u64::MAX] {
        tree.try_insert(key, key).unwrap();
        model.insert(key, key);
    }
    let walked: Vec<u64> = tree.iter().map(|(key, _)| key).collect();
    assert_eq!(walked, model.keys().copied().collect::<Vec<_>>());
    for from in [0, 62, 63, 64, 4095, 4096, 1 << 40, u64::MAX] {
        let walked: Vec<u64> = tree.iter_from(from).map(|(key, _)| key).collect();
        assert_eq!(
            walked,
            model.range(from..).map(|(key, _)| *key).collect::<Vec<_>>()
        );
    }
    let tagged: Vec<u64> = model.keys().copied().step_by(3).collect();
    for &key in &tagged {
        assert!(tree.set_tag(key, DIRTY));
    }
    let walked: Vec<u64> = tree.iter_tagged(0, DIRTY).map(|(key, _)| key).collect();
    assert_eq!(walked, tagged);
}

/// Compaction gives back what removals freed and spare room, moving live
/// nodes forward, every entry and tag where it was; clearing gives back all.
#[test]
fn shrinking_holds_only_the_live_paths_and_keeps_every_entry() {
    let mut tree: RadixTree<u64, 1> = RadixTree::new();
    let mut stream = Stream(0x1234_5678);
    let keys: Vec<u64> = (0..if cfg!(miri) { 300 } else { 4000 })
        .map(|_| stream.next())
        .collect();
    for &key in &keys {
        tree.try_insert(key, key).unwrap();
    }
    let peak = tree.allocated_bytes();
    let kept: Vec<u64> = keys.iter().copied().step_by(16).collect();
    for &key in &keys {
        if !kept.contains(&key) {
            tree.remove(key);
        } else if key % 2 == 0 {
            tree.set_tag(key, DIRTY);
        }
    }
    tree.try_shrink_to_fit().unwrap();
    let (inner, leaves) = tree.resident_nodes();
    assert_eq!(
        (tree.inner.nodes.len(), tree.leaves.nodes.len()),
        (inner, leaves)
    );
    assert_eq!(tree.inner.nodes.capacity(), inner);
    assert_eq!(tree.leaves.nodes.capacity(), leaves);
    assert!(
        tree.allocated_bytes() < peak / 4,
        "{} of {peak}",
        tree.allocated_bytes()
    );
    for &key in &kept {
        assert_eq!(tree.get(key), Some(&key));
        assert_eq!(tree.is_tagged(key, DIRTY), key % 2 == 0);
    }
    assert_eq!(tree.len(), kept.len());
    let walked: Vec<u64> = tree.iter().map(|(key, _)| key).collect();
    let mut sorted = kept.clone();
    sorted.sort_unstable();
    assert_eq!(walked, sorted);
    for &key in &kept {
        assert_eq!(tree.remove(key), Some(key));
    }
    assert!(tree.is_empty());
    tree.try_insert(5, 5).unwrap();
    tree.clear();
    assert!(tree.is_empty());
    assert_eq!(tree.allocated_bytes(), 0);
    assert_eq!(tree.get(5), None);
}

/// The bytes-per-entry gate: dense keys, compacted, cost little more than
/// the value slot each takes.
#[test]
fn dense_keys_cost_little_more_than_their_values() {
    let mut tree: RadixTree<u64> = RadixTree::new();
    let entries = if cfg!(miri) { 4096 } else { 1 << 16 };
    for key in 0..entries {
        tree.try_insert(key, key).unwrap();
    }
    tree.try_shrink_to_fit().unwrap();
    let slot = core::mem::size_of::<Option<u64>>();
    let per_entry = tree.allocated_bytes() / tree.len();
    assert!(per_entry <= slot + slot / 4, "{per_entry} bytes per entry");
}
