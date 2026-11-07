use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::frames::{pool_slot_of, PageTableFrames, TableFrame};

const MIB: u64 = 1 << 20;

#[repr(C, align(4096))]
struct Table([u64; PAGE_TABLE_ENTRIES]);

/// A bump pool over static storage, whose `phys` is each slot's address.
struct Frames<const N: usize> {
    storage: [UnsafeCell<Table>; N],
    used: AtomicUsize,
}

// SAFETY: each slot is handed out once, by the monotonic counter.
unsafe impl<const N: usize> Sync for Frames<N> {}

impl<const N: usize> Frames<N> {
    const fn new() -> Self {
        #[allow(clippy::declare_interior_mutable_const)] // copied per slot on purpose
        const ZERO: UnsafeCell<Table> = UnsafeCell::new(Table([0; PAGE_TABLE_ENTRIES]));
        Self {
            storage: [ZERO; N],
            used: AtomicUsize::new(0),
        }
    }

    fn drawn(&self) -> usize {
        self.used.load(Ordering::SeqCst).min(N)
    }
}

impl<const N: usize> PageTableFrames for Frames<N> {
    fn alloc_table(&self) -> Option<TableFrame> {
        let index = self.used.fetch_add(1, Ordering::SeqCst);
        let cell = self.storage.get(index)?;
        // SAFETY: the counter hands each slot out once.
        let entries = &mut unsafe { &mut *cell.get() }.0;
        let phys = entries.as_ptr() as u64;
        Some(TableFrame { phys, entries })
    }

    fn table_at(&self, phys: u64) -> Option<*mut [u64; PAGE_TABLE_ENTRIES]> {
        let index = pool_slot_of(self.storage.as_ptr() as u64, N, phys)?;
        Some(self.storage[index].get().cast())
    }

    fn free_table(&self, _phys: u64) {}
}

/// Leaves that spell their inputs back: the address, the access in the low
/// byte, and a mark for a block or a table.
struct Spelled;

const BLOCK_MARK: u64 = 1 << 62;
const TABLE_MARK: u64 = 1 << 63;

impl ImageLeaves for Spelled {
    type Access = u8;

    fn leaf(&self, addr: u64, access: u8, block: bool) -> u64 {
        addr | u64::from(access) | if block { BLOCK_MARK } else { 0 }
    }

    fn table(&self, phys: u64) -> u64 {
        phys | TABLE_MARK
    }
}

fn span(start: u64, end: u64) -> Span {
    Span { start, end }
}

fn run(start: u64, end: u64, access: u8) -> Run<u8> {
    Run {
        span: span(start, end),
        access,
    }
}

#[test]
fn a_span_splits_only_a_block_one_of_its_ends_falls_inside() {
    let block = span(2 * MIB, 4 * MIB);
    assert!(!block.splits(2 * MIB, 2 * MIB), "a span that is the block");
    assert!(
        !span(MIB, 8 * MIB).splits(2 * MIB, 2 * MIB),
        "one that covers it"
    );
    assert!(
        !span(4 * MIB, 6 * MIB).splits(2 * MIB, 2 * MIB),
        "one that starts where it ends"
    );
    assert!(span(3 * MIB, 6 * MIB).splits(2 * MIB, 2 * MIB), "its start");
    assert!(span(MIB, 3 * MIB).splits(2 * MIB, 2 * MIB), "its end");
    assert!(
        !span(3 * MIB, 3 * MIB).splits(2 * MIB, 2 * MIB),
        "an empty span inside it"
    );
}

#[test]
fn a_block_one_run_fills_is_one_leaf_and_only_a_split_block_a_table() {
    static FRAMES: Frames<4> = Frames::new();
    let base = 3 * GIGABYTE;
    let runs = [
        run(base + 2 * MIB, base + 8 * MIB, 1),
        run(base + 8 * MIB, base + 8 * MIB + 0x3000, 2),
        run(base + 8 * MIB + 0x3000, base + 12 * MIB, 3),
    ];
    let mut table = [0; PAGE_TABLE_ENTRIES];
    map_gigabyte(&mut table, base, &runs, None, &Spelled, &FRAMES).expect("mapped");
    assert_eq!(table[0], 0, "a block no run reaches");
    assert_eq!(table[1], (base + 2 * MIB) | 1 | BLOCK_MARK);
    assert_eq!(table[3], (base + 6 * MIB) | 1 | BLOCK_MARK);
    assert_eq!(table[4] & TABLE_MARK, TABLE_MARK, "two runs share block 4");
    assert_eq!(table[5], (base + 10 * MIB) | 3 | BLOCK_MARK);
    assert_eq!(table[6], 0, "past every run");
    assert_eq!(FRAMES.drawn(), 1, "the one split block");
    let pages = FRAMES
        .table_at(table[4] & !TABLE_MARK)
        .expect("the split block's table");
    // SAFETY: the test's own table, only read.
    let pages = unsafe { &*pages };
    assert_eq!(pages[0], (base + 8 * MIB) | 2);
    assert_eq!(pages[2], (base + 8 * MIB + 0x2000) | 2);
    assert_eq!(pages[3], (base + 8 * MIB + 0x3000) | 3);
    assert_eq!(pages[511], (base + 10 * MIB - 0x1000) | 3);
}

#[test]
fn pages_no_run_holds_take_the_outside_access() {
    static FRAMES: Frames<2> = Frames::new();
    let runs = [run(MIB, 2 * MIB, 1)];
    let mut table = [0; PAGE_TABLE_ENTRIES];
    map_gigabyte(&mut table, 0, &runs, Some(9), &Spelled, &FRAMES).expect("mapped");
    let pages = FRAMES.table_at(table[0] & !TABLE_MARK).expect("block 0");
    // SAFETY: the test's own table, only read.
    let pages = unsafe { &*pages };
    assert_eq!(pages[0], 9, "below the run");
    assert_eq!(pages[256], MIB | 1, "the run");
    assert_eq!(table[1], (2 * MIB) | 9 | BLOCK_MARK, "above it, whole");
    assert_eq!(table[511], (1022 * MIB) | 9 | BLOCK_MARK);
}

#[test]
fn a_base_off_a_gigabyte_or_an_exhausted_source_maps_nothing_whole() {
    static NONE: Frames<0> = Frames::new();
    let mut table = [0; PAGE_TABLE_ENTRIES];
    assert_eq!(
        map_gigabyte(&mut table, BLOCK, &[], None, &Spelled, &NONE),
        None,
        "a base inside a gigabyte"
    );
    let split = [run(MIB, 3 * MIB, 1)];
    assert_eq!(
        map_gigabyte(&mut table, 0, &split, None, &Spelled, &NONE),
        None,
        "a split block with no table to give it"
    );
    assert_eq!(split_blocks(split.len()), 2);
}
