use super::*;

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec;
use alloc::vec::Vec;

/// Four pointers per indirect block, so the single, double and triple trees
/// cover logical blocks 12..16, 16..32 and 32..96.
const PPB: u64 = 4;

struct Store {
    blocks: BTreeMap<u64, Vec<u8>>,
    /// The next block a data or indirect allocation hands out.
    next: u64,
    used: BTreeSet<u64>,
    freed_data: u64,
    freed_ptrs: u64,
    /// The goal each allocation was asked for, in order.
    goals: Vec<u64>,
}

impl Store {
    fn new() -> Self {
        Self {
            blocks: BTreeMap::new(),
            next: 100,
            used: BTreeSet::new(),
            freed_data: 0,
            freed_ptrs: 0,
            goals: Vec::new(),
        }
    }
}

impl MapStore for Store {
    fn ptrs_per_block(&self) -> u64 {
        PPB
    }

    fn read_ptrs(&mut self, block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        buf.copy_from_slice(self.blocks.get(&block).ok_or(DriverError::DeviceFault)?);
        Ok(())
    }

    fn write_ptrs(&mut self, block: u64, buf: &[u8]) -> Result<(), DriverError> {
        self.blocks.insert(block, buf.to_vec());
        Ok(())
    }

    fn alloc_data(&mut self, goal: u64, want: u32) -> Result<(u64, u32), DriverError> {
        self.goals.push(goal);
        let first = self.next;
        self.next += u64::from(want);
        for b in first..self.next {
            self.used.insert(b);
        }
        Ok((first, want))
    }

    fn alloc_ptr_block(&mut self, goal: u64) -> Result<u64, DriverError> {
        self.goals.push(goal);
        let block = self.next;
        self.next += 1;
        self.used.insert(block);
        Ok(block)
    }

    fn free_data(&mut self, start: u64, len: u64) {
        for b in start..start + len {
            assert!(
                self.used.remove(&b),
                "data block {b} freed twice or never used"
            );
        }
        self.freed_data += len;
    }

    fn free_ptr_block(&mut self, block: u64) {
        assert!(
            self.used.remove(&block),
            "indirect block {block} freed twice"
        );
        self.blocks.remove(&block);
        self.freed_ptrs += 1;
    }
}

/// Map every logical block in `range` one allocation at a time.
fn map_all(iblock: &mut [u8], store: &mut Store, range: core::ops::Range<u64>) {
    let mut logical = range.start;
    while logical < range.end {
        let want = u32::try_from(range.end - logical).expect("fits");
        let got = alloc(iblock, store, logical, want, 0).expect("alloc");
        assert!(got.len >= 1);
        logical += u64::from(got.len);
    }
}

fn resolve(iblock: &[u8], store: &mut Store, logical: u64) -> Option<u64> {
    match lookup(iblock, store, logical).expect("lookup") {
        Mapping::Mapped { phys, .. } => Some(phys),
        Mapping::Hole { .. } => None,
    }
}

#[test]
fn the_addressable_range_ends_past_the_triple_tree() {
    assert_eq!(limit(PPB), 96);
    assert_eq!(limit(1024), 12 + 1024 + 1024 * 1024 + 1024 * 1024 * 1024);
}

#[test]
fn every_level_of_the_map_is_written_and_read_back() {
    let mut iblock = [0u8; 60];
    let mut store = Store::new();
    map_all(&mut iblock, &mut store, 0..96);
    let mut seen = BTreeSet::new();
    for logical in 0..96 {
        let phys = resolve(&iblock, &mut store, logical).expect("mapped");
        assert!(seen.insert(phys), "two logical blocks share one physical");
    }
    assert_eq!(
        alloc(&mut iblock, &mut store, 96, 1, 0).err(),
        Some(DriverError::LengthOutOfRange)
    );
}

#[test]
fn a_lookup_reports_runs_and_holes() {
    let mut iblock = [0u8; 60];
    let mut store = Store::new();
    let got = alloc(&mut iblock, &mut store, 2, 5, 0).expect("alloc");
    assert_eq!(got.len, 5);
    assert_eq!(
        lookup(&iblock, &mut store, 3),
        Ok(Mapping::Mapped {
            phys: got.phys + 1,
            len: 4,
            unwritten: false
        })
    );
    assert_eq!(
        lookup(&iblock, &mut store, 0),
        Ok(Mapping::Hole { len: Some(2) })
    );
    // The whole double tree is absent: one hole spans it.
    assert_eq!(
        lookup(&iblock, &mut store, 20),
        Ok(Mapping::Hole { len: Some(12) })
    );
    assert_eq!(
        lookup(&iblock, &mut store, 200),
        Ok(Mapping::Hole { len: None })
    );
}

/// Each block is asked for where the file most likely continues: past the
/// nearest mapped pointer before it in its array, else past the indirect
/// block holding the array, else at the caller's goal.
#[test]
fn an_allocation_aims_just_past_the_files_previous_block() {
    let mut iblock = vec![0u8; 60];
    let mut store = Store::new();
    for logical in [0, 3, 12] {
        alloc(&mut iblock, &mut store, logical, 1, 7).expect("alloc");
    }
    assert_eq!(
        store.goals,
        [7, 101, 102, 103],
        "the first block takes the caller's goal; the direct block past block \
         100; the single indirect block past the last direct one; its first \
         data block past the indirect block"
    );
}

#[test]
fn an_allocation_stops_at_its_pointer_array() {
    let mut iblock = [0u8; 60];
    let mut store = Store::new();
    // Logical 10 has two direct slots left before the single tree begins.
    let got = alloc(&mut iblock, &mut store, 10, 8, 0).expect("alloc");
    assert_eq!(got.len, 2);
    assert_eq!(got.meta, 0);
    let got = alloc(&mut iblock, &mut store, 12, 8, 0).expect("alloc");
    assert_eq!((got.len, got.meta), (4, 1));
}

#[test]
fn truncating_everything_releases_every_block() {
    let mut iblock = [0u8; 60];
    let mut store = Store::new();
    map_all(&mut iblock, &mut store, 0..96);
    let done = truncate(&mut iblock, &mut store, 0, u32::MAX).expect("truncate");
    assert!(done.done);
    assert!(store.used.is_empty(), "nothing may stay allocated");
    assert_eq!(done.data, 96);
    assert_eq!(done.data, store.freed_data);
    assert_eq!(done.nodes, store.freed_ptrs);
    assert_eq!(iblock, [0u8; 60]);
}

#[test]
fn a_cut_inside_the_double_tree_keeps_everything_before_it() {
    let mut iblock = [0u8; 60];
    let mut store = Store::new();
    map_all(&mut iblock, &mut store, 0..96);
    let before: Vec<Option<u64>> = (0..21).map(|l| resolve(&iblock, &mut store, l)).collect();
    let done = truncate(&mut iblock, &mut store, 21, u32::MAX).expect("truncate");
    assert!(done.done);
    for (logical, &kept) in (0..).zip(&before) {
        assert_eq!(resolve(&iblock, &mut store, logical), kept);
    }
    for logical in 21..96 {
        assert_eq!(resolve(&iblock, &mut store, logical), None);
    }
    assert_eq!(get(&iblock, TRIPLE_SLOT), 0, "the triple tree goes");
    assert_eq!(store.freed_data, 96 - 21);
}

#[test]
fn a_bounded_truncation_finishes_across_passes_leaving_a_mapped_prefix() {
    let mut iblock = [0u8; 60];
    let mut store = Store::new();
    // One block at a time so no two data pointers are consecutive and every
    // release is its own unit of budget.
    for logical in 0..96u64 {
        let got = alloc(&mut iblock, &mut store, logical, 1, 0).expect("alloc");
        store.next += 1;
        assert_eq!(got.len, 1);
    }
    let mut passes = 0;
    loop {
        let pass = truncate(&mut iblock, &mut store, 5, 3).expect("pass");
        passes += 1;
        let mapped: Vec<u64> = (0..96)
            .filter(|&l| resolve(&iblock, &mut store, l).is_some())
            .collect();
        let last = mapped.last().copied().expect("the head stays");
        assert_eq!(mapped, (0..=last).collect::<Vec<_>>(), "always a prefix");
        if pass.done {
            break;
        }
        assert!(passes < 500);
    }
    assert!(passes > 5);
    assert_eq!(
        (0..96)
            .filter(|&l| resolve(&iblock, &mut store, l).is_some())
            .count(),
        5
    );
}

#[test]
fn freeing_a_hole_riddled_tree_drops_indirect_blocks_left_empty() {
    let mut iblock = [0u8; 60];
    let mut store = Store::new();
    // One block deep in the double tree and one direct block.
    alloc(&mut iblock, &mut store, 1, 1, 0).expect("direct");
    let got = alloc(&mut iblock, &mut store, 30, 1, 0).expect("double");
    assert_eq!(got.meta, 2);
    let done = truncate(&mut iblock, &mut store, 2, u32::MAX).expect("truncate");
    assert!(done.done);
    assert_eq!((done.data, done.nodes), (1, 2));
    assert_eq!(store.used.len(), 1);
}

#[test]
fn a_block_number_beyond_32_bits_is_refused() {
    struct High(Store);
    impl MapStore for High {
        fn ptrs_per_block(&self) -> u64 {
            PPB
        }
        fn read_ptrs(&mut self, block: u64, buf: &mut [u8]) -> Result<(), DriverError> {
            self.0.read_ptrs(block, buf)
        }
        fn write_ptrs(&mut self, block: u64, buf: &[u8]) -> Result<(), DriverError> {
            self.0.write_ptrs(block, buf)
        }
        fn alloc_data(&mut self, _goal: u64, want: u32) -> Result<(u64, u32), DriverError> {
            Ok((1 << 32, want))
        }
        fn alloc_ptr_block(&mut self, goal: u64) -> Result<u64, DriverError> {
            self.0.alloc_ptr_block(goal)
        }
        fn free_data(&mut self, start: u64, len: u64) {
            self.0.free_data(start, len);
        }
        fn free_ptr_block(&mut self, block: u64) {
            self.0.free_ptr_block(block);
        }
    }
    let mut iblock = vec![0u8; 60];
    let mut store = High(Store::new());
    assert_eq!(
        alloc(&mut iblock, &mut store, 0, 1, 0).err(),
        Some(DriverError::DeviceFault)
    );
}
