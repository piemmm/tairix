use super::*;

use alloc::collections::BTreeMap;
use alloc::vec;

/// A node store whose blocks hold five entries, so a few dozen extents
/// split leaves, grow the tree, and reach real depth.
struct Store {
    nodes: BTreeMap<u64, Vec<u8>>,
    next: u64,
    node_bytes: usize,
    freed_nodes: Vec<u64>,
    freed_data: u64,
}

impl Store {
    fn new(node_bytes: usize) -> Self {
        Self {
            nodes: BTreeMap::new(),
            next: 1_000_000,
            node_bytes,
            freed_nodes: Vec::new(),
            freed_data: 0,
        }
    }
}

impl NodeStore for Store {
    fn node_bytes(&self) -> usize {
        self.node_bytes
    }

    fn node_capacity(&self) -> usize {
        (self.node_bytes - HEADER) / ENTRY
    }

    fn read_node(&mut self, block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let node = self.nodes.get(&block).ok_or(DriverError::DeviceFault)?;
        buf.copy_from_slice(node);
        Ok(())
    }

    fn write_node(&mut self, block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.nodes.insert(block, buf.to_vec());
        Ok(())
    }

    fn alloc_node(&mut self, _goal: u64) -> Result<u64, DriverError> {
        self.next += 1;
        Ok(self.next)
    }

    fn free_node(&mut self, block: u64) {
        self.nodes.remove(&block);
        self.freed_nodes.push(block);
    }

    fn free_data(&mut self, _start: u64, len: u64) {
        self.freed_data += len;
    }
}

fn root() -> [u8; ROOT_LEN] {
    let mut root = [0u8; ROOT_LEN];
    init_root(&mut root).expect("root");
    root
}

fn ext(logical: u32, phys: u64, len: u32) -> Extent {
    Extent {
        logical,
        phys,
        len,
        unwritten: false,
    }
}

/// Walk the whole tree, checking every invariant a reader relies on, and
/// return its extents in order.
fn walk(root: &[u8], store: &mut Store) -> Vec<Extent> {
    fn visit(buf: &[u8], store: &mut Store, lo: u64, hi: Option<u64>, out: &mut Vec<Extent>) {
        assert_eq!(le16(buf, 0), EXTENT_MAGIC);
        assert!(count(buf) <= max_entries(buf));
        if depth(buf) == 0 {
            for i in 0..count(buf) {
                let e = leaf_entry(buf, i);
                assert!(u64::from(e.logical) >= lo, "extent below its index key");
                assert!(
                    hi.is_none_or(|hi| e.end() <= hi),
                    "extent past the next key"
                );
                if let Some(prev) = out.last() {
                    assert!(
                        prev.end() <= u64::from(e.logical),
                        "extents overlap or are unsorted"
                    );
                }
                out.push(e);
            }
            return;
        }
        assert!(count(buf) > 0, "an index node is never empty");
        for i in 0..count(buf) {
            let key = u64::from(index_key(buf, i));
            let next = (i + 1 < count(buf)).then(|| u64::from(index_key(buf, i + 1)));
            assert!(key >= lo);
            if let Some(next) = next {
                assert!(key < next, "index keys must ascend");
            }
            let mut child = vec![0u8; store.node_bytes];
            store
                .read_node(index_child(buf, i), &mut child)
                .expect("child present");
            assert_eq!(depth(&child), depth(buf) - 1);
            assert_eq!(
                first_key(&child).map(u64::from),
                Some(key),
                "an index key names its child's first block"
            );
            visit(&child, store, key, next.or(hi), out);
        }
    }
    let mut out = Vec::new();
    visit(root, store, 0, None, &mut out);
    out
}

#[test]
fn an_empty_tree_maps_nothing() {
    let root = root();
    let mut store = Store::new(72);
    assert_eq!(
        lookup(&root, &mut store, 7),
        Ok(Mapping::Hole { len: None })
    );
}

#[test]
fn a_lookup_reports_the_run_left_and_the_hole_before_the_next_extent() {
    let mut root = root();
    let mut store = Store::new(72);
    insert(&mut root, &mut store, ext(10, 500, 4)).expect("insert");
    insert(&mut root, &mut store, ext(20, 900, 2)).expect("insert");
    assert_eq!(
        lookup(&root, &mut store, 11),
        Ok(Mapping::Mapped {
            phys: 501,
            len: 3,
            unwritten: false
        })
    );
    assert_eq!(
        lookup(&root, &mut store, 0),
        Ok(Mapping::Hole { len: Some(10) })
    );
    assert_eq!(
        lookup(&root, &mut store, 14),
        Ok(Mapping::Hole { len: Some(6) })
    );
    assert_eq!(
        lookup(&root, &mut store, 22),
        Ok(Mapping::Hole { len: None })
    );
}

#[test]
fn contiguous_appends_extend_one_extent_up_to_its_limit() {
    let mut root = root();
    let mut store = Store::new(72);
    for i in 0..(INIT_MAX + 3) {
        insert(&mut root, &mut store, ext(i, 10_000 + u64::from(i), 1)).expect("append");
    }
    let extents = walk(&root, &mut store);
    assert_eq!(
        extents,
        vec![
            ext(0, 10_000, INIT_MAX),
            ext(INIT_MAX, 10_000 + u64::from(INIT_MAX), 3)
        ]
    );
}

#[test]
fn a_run_filling_the_gap_between_two_extents_joins_all_three() {
    let mut root = root();
    let mut store = Store::new(72);
    insert(&mut root, &mut store, ext(0, 100, 4)).expect("left");
    insert(&mut root, &mut store, ext(8, 108, 4)).expect("right");
    insert(&mut root, &mut store, ext(4, 104, 4)).expect("gap");
    assert_eq!(walk(&root, &mut store), vec![ext(0, 100, 12)]);
}

#[test]
fn an_extent_continued_from_the_left_merges_into_its_right_neighbour() {
    let mut root = root();
    let mut store = Store::new(72);
    insert(&mut root, &mut store, ext(50, 700, 5)).expect("right");
    insert(&mut root, &mut store, ext(45, 695, 5)).expect("before it");
    assert_eq!(walk(&root, &mut store), vec![ext(45, 695, 10)]);
}

#[test]
fn written_and_unwritten_neighbours_never_merge() {
    let mut root = root();
    let mut store = Store::new(72);
    insert(&mut root, &mut store, ext(0, 100, 4)).expect("written");
    insert(
        &mut root,
        &mut store,
        Extent {
            unwritten: true,
            ..ext(4, 104, 4)
        },
    )
    .expect("unwritten");
    assert_eq!(walk(&root, &mut store).len(), 2);
}

/// Insert `n` single-block extents a block apart physically, so none merge,
/// in the order `order` gives.
fn scatter(store: &mut Store, order: &[u32]) -> [u8; ROOT_LEN] {
    let mut root = root();
    for &i in order {
        insert(&mut root, store, ext(i * 2, 1 + u64::from(i) * 3, 1)).expect("insert");
    }
    root
}

#[test]
fn ascending_inserts_split_leaves_and_grow_the_tree_soundly() {
    let mut store = Store::new(72);
    let order: Vec<u32> = (0..600).collect();
    let root = scatter(&mut store, &order);
    assert!(
        depth(&root) >= 3,
        "600 extents in five-entry nodes need depth"
    );
    let extents = walk(&root, &mut store);
    assert_eq!(extents.len(), 600);
    for (i, e) in extents.iter().enumerate() {
        let i = u32::try_from(i).expect("index");
        assert_eq!(*e, ext(i * 2, 1 + u64::from(i) * 3, 1));
        assert_eq!(
            lookup(&root, &mut store, i * 2),
            Ok(Mapping::Mapped {
                phys: 1 + u64::from(i) * 3,
                len: 1,
                unwritten: false
            })
        );
        assert_eq!(
            lookup(&root, &mut store, i * 2 + 1),
            Ok(Mapping::Hole {
                len: (i < 599).then_some(1)
            })
        );
    }
}

#[test]
fn shuffled_inserts_keep_the_tree_sorted_and_keyed() {
    let mut store = Store::new(72);
    // A fixed permutation of 0..400: multiplication by a unit modulo 401.
    let order: Vec<u32> = (1..=400u32).map(|i| (i * 157) % 401 - 1).collect();
    let root = scatter(&mut store, &order);
    let extents = walk(&root, &mut store);
    assert_eq!(extents.len(), 400);
    for (i, e) in extents.iter().enumerate() {
        assert_eq!(u64::from(e.logical), 2 * i as u64);
    }
}

#[test]
fn a_tree_at_the_depth_limit_refuses_to_grow() {
    // Four-entry nodes reach the format's depth limit within a few thousand
    // extents; the insert that would need a sixth level is refused whole.
    let mut store = Store::new(60);
    let mut root = root();
    let mut refused = None;
    for i in 0..20_000u32 {
        match insert(&mut root, &mut store, ext(i * 2, 1 + u64::from(i) * 3, 1)) {
            Ok(()) => {}
            Err(err) => {
                refused = Some((i, err));
                break;
            }
        }
    }
    let (at, err) = refused.expect("the depth limit is reached");
    assert_eq!(err, DriverError::NoSpace);
    assert_eq!(depth(&root), MAX_DEPTH);
    assert_eq!(walk(&root, &mut store).len(), at as usize);
}

#[test]
fn truncation_releases_the_tail_and_keeps_a_sound_prefix() {
    let mut store = Store::new(72);
    let order: Vec<u32> = (0..300).collect();
    let mut root = scatter(&mut store, &order);
    let before = store.nodes.len();
    let done = truncate(&mut root, &mut store, 301, u32::MAX).expect("truncate");
    assert!(done.done);
    let extents = walk(&root, &mut store);
    // Extents start at even blocks, so 0..=300 keeps 151 of them.
    assert_eq!(extents.len(), 151);
    assert_eq!(done.data, 149);
    assert_eq!(store.freed_data, 149);
    assert!(store.nodes.len() < before, "emptied nodes go");
    assert_eq!(usize::try_from(done.nodes), Ok(store.freed_nodes.len()));
}

#[test]
fn a_cut_inside_an_extent_keeps_its_head() {
    let mut root = root();
    let mut store = Store::new(72);
    insert(&mut root, &mut store, ext(0, 100, 10)).expect("insert");
    let done = truncate(&mut root, &mut store, 4, 8).expect("truncate");
    assert!(done.done);
    assert_eq!(done.data, 6);
    assert_eq!(walk(&root, &mut store), vec![ext(0, 100, 4)]);
}

#[test]
fn a_bounded_truncation_finishes_across_passes() {
    let mut store = Store::new(72);
    let order: Vec<u32> = (0..250).collect();
    let mut root = scatter(&mut store, &order);
    let mut passes = 0;
    loop {
        let pass = truncate(&mut root, &mut store, 0, 7).expect("pass");
        passes += 1;
        // Every intermediate tree maps a prefix and is sound.
        let left = walk(&root, &mut store);
        for (i, e) in left.iter().enumerate() {
            assert_eq!(u64::from(e.logical), 2 * i as u64);
        }
        if pass.done {
            break;
        }
        assert!(passes < 1000);
    }
    assert!(passes > 10, "a small budget takes many passes");
    assert_eq!(depth(&root), 0, "an emptied tree returns to an empty root");
    assert_eq!(count(&root), 0);
    assert!(store.nodes.is_empty(), "every node block is released");
    assert_eq!(store.freed_data, 250);
}

fn unwritten(logical: u32, phys: u64, len: u32) -> Extent {
    Extent {
        unwritten: true,
        ..ext(logical, phys, len)
    }
}

#[test]
fn marking_the_middle_of_an_unwritten_extent_splits_it_in_three() {
    let mut root = root();
    let mut store = Store::new(72);
    insert(&mut root, &mut store, unwritten(0, 100, 10)).expect("insert");
    mark_written(&mut root, &mut store, 3, 4).expect("mark");
    assert_eq!(
        walk(&root, &mut store),
        vec![unwritten(0, 100, 3), ext(3, 103, 4), unwritten(7, 107, 3)]
    );
}

#[test]
fn marking_an_edge_or_the_whole_extent_splits_only_what_it_must() {
    let mut root = root();
    let mut store = Store::new(72);
    insert(&mut root, &mut store, unwritten(0, 100, 10)).expect("insert");
    mark_written(&mut root, &mut store, 0, 2).expect("head");
    assert_eq!(
        walk(&root, &mut store),
        vec![ext(0, 100, 2), unwritten(2, 102, 8)]
    );
    mark_written(&mut root, &mut store, 8, 2).expect("tail");
    assert_eq!(
        walk(&root, &mut store),
        vec![ext(0, 100, 2), unwritten(2, 102, 6), ext(8, 108, 2)]
    );
    mark_written(&mut root, &mut store, 2, 6).expect("whole");
    assert_eq!(
        walk(&root, &mut store),
        vec![ext(0, 100, 2), ext(2, 102, 6), ext(8, 108, 2)]
    );
}

#[test]
fn marking_a_written_extent_is_refused() {
    let mut root = root();
    let mut store = Store::new(72);
    insert(&mut root, &mut store, ext(0, 100, 10)).expect("insert");
    assert_eq!(
        mark_written(&mut root, &mut store, 2, 2),
        Err(DriverError::DeviceFault)
    );
}

#[test]
fn an_unwritten_lookup_says_so() {
    let mut root = root();
    let mut store = Store::new(72);
    insert(&mut root, &mut store, unwritten(5, 50, UNWRITTEN_MAX)).expect("insert");
    assert_eq!(
        lookup(&root, &mut store, 6),
        Ok(Mapping::Mapped {
            phys: 51,
            len: u64::from(UNWRITTEN_MAX) - 1,
            unwritten: true
        })
    );
    assert_eq!(
        insert(&mut root, &mut store, unwritten(0, 0, INIT_MAX)),
        Err(DriverError::LengthOutOfRange)
    );
}

#[test]
fn a_corrupt_root_is_refused() {
    let mut root = root();
    put16(&mut root, 0, 0xBEEF);
    let mut store = Store::new(72);
    assert_eq!(lookup(&root, &mut store, 0), Err(DriverError::DeviceFault));
    let mut root = self::root();
    put16(&mut root, 2, 9);
    assert_eq!(lookup(&root, &mut store, 0), Err(DriverError::DeviceFault));
}

#[test]
fn a_child_at_the_wrong_depth_is_refused() {
    let mut store = Store::new(72);
    let order: Vec<u32> = (0..10).collect();
    let root = scatter(&mut store, &order);
    assert!(depth(&root) >= 1);
    let child = index_child(&root, 0);
    let mut node = store.nodes.get(&child).cloned().expect("child");
    put16(&mut node, 6, 3);
    store.nodes.insert(child, node);
    assert_eq!(lookup(&root, &mut store, 0), Err(DriverError::DeviceFault));
}

#[test]
fn the_goal_continues_the_extent_before_a_hole() {
    let mut root = root();
    let mut store = Store::new(72);
    assert_eq!(goal_after(&root, &mut store, 10), Ok(None));
    insert(&mut root, &mut store, ext(0, 100, 4)).expect("insert");
    assert_eq!(goal_after(&root, &mut store, 4), Ok(Some(104)));
    assert_eq!(goal_after(&root, &mut store, 9), Ok(Some(109)));
}

/// A node with no room for an entry is no node — splitting one to make room
/// would count below zero — and an index naming no child is no index: one
/// read as a leaf takes a leaf entry over its index.
#[test]
fn a_node_without_room_or_an_index_without_a_child_is_refused() {
    let mut store = Store::new(60);
    let roomless = 2_000_000u64;
    let mut leaf = vec![0u8; 60];
    put16(&mut leaf, 0, EXTENT_MAGIC);
    store.nodes.insert(roomless, leaf);
    let mut over_roomless = root();
    put16(&mut over_roomless, 6, 1);
    put_index_entry(&mut over_roomless, 0, 0, roomless).expect("index");
    set_count(&mut over_roomless, 1).expect("count");
    assert_eq!(
        insert(&mut over_roomless, &mut store, ext(0, 5, 1)),
        Err(DriverError::DeviceFault)
    );

    let mut childless = root();
    put16(&mut childless, 6, 1);
    assert_eq!(
        insert(&mut childless, &mut store, ext(0, 5, 1)),
        Err(DriverError::DeviceFault)
    );
    assert_eq!(
        lookup(&childless, &mut store, 0).err(),
        Some(DriverError::DeviceFault)
    );
}

/// An extent may reach the last logical block but one: one reaching the
/// last ends at 2^32, which reads as a run that wraps.
#[test]
fn an_extent_ending_at_the_top_of_the_logical_space_is_refused() {
    let mut store = Store::new(60);
    let mut root = root();
    assert_eq!(
        insert(&mut root, &mut store, ext(u32::MAX - 1, 5, 2)),
        Err(DriverError::LengthOutOfRange)
    );
    insert(&mut root, &mut store, ext(u32::MAX - 1, 5, 1)).expect("the last block but one");
}
