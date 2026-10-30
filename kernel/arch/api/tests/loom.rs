//! Loom models of the lock-free page-table protocol.
//!
//! A fault on one CPU walks its active root with no lock while the space's
//! owner changes the same tables on another, so the protocol has two halves:
//! a table is linked with [`LINK_ORDERING`] and followed by a walk that read
//! the link with [`WALK_ORDERING`], and a fault-time update changes a leaf
//! only by an exchange ([`Leaf::set_while`]). Driven by `cargo xtask loom`;
//! directly, that is:
//!
//! ```text
//! cargo test -p tairix-arch-api --features loom --test loom --release
//! ```

#![cfg(feature = "loom")]

use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicU64, Ordering};
use loom::sync::Arc;
use loom::thread;

use tairix_arch_api::frames::{DescriptorSlot, Leaf, LINK_ORDERING, WALK_ORDERING};

const VALID: u64 = 1;
const ACCESSED: u64 = 1 << 10;

/// One descriptor whose accesses the model checker interleaves.
struct Slot(AtomicU64);

impl DescriptorSlot for Slot {
    fn exchange(&self, current: u64, new: u64) -> Result<u64, u64> {
        self.0
            .compare_exchange(current, new, Ordering::Relaxed, Ordering::Relaxed)
    }
}

fn accessed_clear(desc: u64) -> bool {
    desc & VALID != 0 && desc & ACCESSED == 0
}

/// The fault walks a valid leaf with its access bit clear and sets the bit
/// while the owner unmaps the page. Whichever lands first, the unmap stands:
/// a read-modify-write of the walk's reading would map the freed frame again.
#[test]
fn a_fault_time_update_never_restores_an_unmapped_leaf() {
    loom::model(|| {
        let slot = Arc::new(Slot(AtomicU64::new(VALID)));
        let fault = {
            let slot = Arc::clone(&slot);
            thread::spawn(move || {
                let seen = slot.0.load(WALK_ORDERING);
                Leaf {
                    slot: &*slot,
                    seen,
                    shift: 12,
                }
                .set_while(ACCESSED, accessed_clear)
            })
        };
        let unmapped = slot.0.swap(0, Ordering::Relaxed);
        let set = fault.join().unwrap();
        assert_eq!(slot.0.load(Ordering::Relaxed), 0, "the unmap stands");
        assert_ne!(unmapped & VALID, 0, "the unmap took the mapping");
        if set.is_some() {
            assert_ne!(
                unmapped & ACCESSED,
                0,
                "an update that took effect preceded the unmap"
            );
        }
    });
}

/// The owner zeroes a fresh table and links it while a fault walks the
/// parent: a walk that reads the link sees the zeroed entries, never the
/// frame's earlier contents.
#[test]
fn a_walk_that_follows_a_link_sees_the_table_it_links() {
    loom::model(|| {
        let link = Arc::new(AtomicU64::new(0));
        let entry = Arc::new(UnsafeCell::new(u64::MAX));
        let walk = {
            let link = Arc::clone(&link);
            let entry = Arc::clone(&entry);
            thread::spawn(move || {
                if link.load(WALK_ORDERING) != 0 {
                    // SAFETY: the link was read, so the owner's zeroing
                    // happened before this read and no write follows it.
                    entry.with(|entry| assert_eq!(unsafe { *entry }, 0));
                }
            })
        };
        // SAFETY: no walk reaches the table before the link below.
        entry.with_mut(|entry| unsafe { *entry = 0 });
        link.store(VALID, LINK_ORDERING);
        walk.join().unwrap();
    });
}
