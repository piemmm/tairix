use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::{descriptors, DescriptorSlot, Leaf, PAGE_TABLE_ENTRIES};

const VALID: u64 = 1;
const ACCESSED: u64 = 1 << 10;
const DIRTY: u64 = 1 << 7;
const WRITABLE: u64 = 1 << 2;

fn accessed_clear(desc: u64) -> bool {
    desc & VALID != 0 && desc & ACCESSED == 0
}

fn leaf(slot: &AtomicU64, seen: u64) -> Leaf<'_> {
    Leaf {
        slot,
        seen,
        shift: 12,
    }
}

/// A slot that counts the exchanges an update makes.
struct Counted {
    word: AtomicU64,
    exchanges: AtomicUsize,
}

impl DescriptorSlot for Counted {
    fn exchange(&self, current: u64, new: u64) -> Result<u64, u64> {
        self.exchanges.fetch_add(1, Ordering::Relaxed);
        self.word.exchange(current, new)
    }
}

#[repr(C, align(4096))]
struct Table([u64; PAGE_TABLE_ENTRIES]);

#[test]
fn the_view_reads_and_writes_the_table_itself() {
    let mut table = Table([0; PAGE_TABLE_ENTRIES]);
    table.0[7] = 0x1234;
    // SAFETY: the table outlives the view, and nothing else reaches it while
    // the view is used.
    let view = unsafe { descriptors(&raw mut table.0) };
    assert_eq!(view[7].load(Ordering::Relaxed), 0x1234);
    view[511].store(0xABCD, Ordering::Relaxed);
    assert_eq!(table.0[511], 0xABCD);
}

#[test]
fn an_update_sets_its_bits_on_a_leaf_it_admits() {
    let slot = AtomicU64::new(VALID);
    assert_eq!(
        leaf(&slot, VALID).set_while(ACCESSED, accessed_clear),
        Some(VALID | ACCESSED)
    );
    assert_eq!(slot.load(Ordering::Relaxed), VALID | ACCESSED);
}

#[test]
fn an_update_never_restores_a_leaf_cleared_since_its_walk() {
    // The walk read a valid leaf; an unmap cleared it before the update. A
    // read-modify-write of the stale reading would map the freed frame again.
    let slot = AtomicU64::new(0);
    assert_eq!(leaf(&slot, VALID).set_while(ACCESSED, accessed_clear), None);
    assert_eq!(slot.load(Ordering::Relaxed), 0);
}

#[test]
fn an_update_refuses_a_leaf_re_permissioned_since_its_walk() {
    let slot = AtomicU64::new(VALID);
    let writable = |desc: u64| desc & VALID != 0 && desc & WRITABLE != 0;
    assert_eq!(
        leaf(&slot, VALID | WRITABLE).set_while(ACCESSED | DIRTY, writable),
        None
    );
    assert_eq!(slot.load(Ordering::Relaxed), VALID);
}

#[test]
fn an_update_keeps_what_changed_since_its_walk() {
    let slot = AtomicU64::new(VALID | DIRTY);
    assert_eq!(
        leaf(&slot, VALID).set_while(ACCESSED, accessed_clear),
        Some(VALID | DIRTY | ACCESSED)
    );
    assert_eq!(slot.load(Ordering::Relaxed), VALID | DIRTY | ACCESSED);
}

#[test]
fn an_update_whose_bits_are_set_writes_nothing() {
    let slot = Counted {
        word: AtomicU64::new(VALID | ACCESSED),
        exchanges: AtomicUsize::new(0),
    };
    let held = Leaf {
        slot: &slot,
        seen: VALID | ACCESSED,
        shift: 12,
    };
    assert_eq!(
        held.set_while(ACCESSED, |desc| desc & VALID != 0),
        Some(VALID | ACCESSED)
    );
    assert_eq!(slot.exchanges.load(Ordering::Relaxed), 0);
}

#[test]
fn an_update_refused_from_the_start_writes_nothing() {
    let slot = Counted {
        word: AtomicU64::new(VALID | ACCESSED),
        exchanges: AtomicUsize::new(0),
    };
    let held = Leaf {
        slot: &slot,
        seen: VALID | ACCESSED,
        shift: 12,
    };
    assert_eq!(held.set_while(ACCESSED, accessed_clear), None);
    assert_eq!(slot.exchanges.load(Ordering::Relaxed), 0);
    assert_eq!(slot.word.load(Ordering::Relaxed), VALID | ACCESSED);
}
