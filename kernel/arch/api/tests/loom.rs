//! Loom models of the HAL's lock-free protocols: the page-table protocol and
//! the secondary-start handshake.
//!
//! A fault on one CPU walks its active root with no lock while the space's
//! owner changes the same tables on another, so the protocol has two halves:
//! a table is linked with [`LINK_ORDERING`] and followed by a walk that read
//! the link with [`WALK_ORDERING`], and a fault-time update changes a leaf
//! only by an exchange ([`Leaf::set_while`]). Three models: an update racing
//! an unmap, two updates of one leaf, and a walk following a link.
//!
//! A started CPU's answer races the starting CPU's deadline, settled on one
//! compare-and-swap ([`settle_start`]) or, where the started CPU cannot take
//! part in one, by a verdict it waits on ([`StartVerdict`]). Two models: each
//! settles on one outcome both CPUs act on. Driven by `cargo xtask loom`;
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
use tairix_arch_api::{
    answer_start, await_start_answer, await_start_verdict, awaiting_answer, settle_start,
    StartAnswer, StartVerdict, StartWord,
};

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

/// The widest admission a port's fault-time update uses: any valid leaf.
fn valid(desc: u64) -> bool {
    desc & VALID != 0
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
                .set_while(ACCESSED, valid)
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

/// Two CPUs fault on one leaf whose access bit is clear: whichever sets it,
/// both retry, and the leaf ends set.
#[test]
fn two_fault_time_updates_of_one_leaf_both_retry() {
    loom::model(|| {
        let slot = Arc::new(Slot(AtomicU64::new(VALID)));
        let fault = || {
            let slot = Arc::clone(&slot);
            thread::spawn(move || {
                let seen = slot.0.load(WALK_ORDERING);
                Leaf {
                    slot: &*slot,
                    seen,
                    shift: 12,
                }
                .set_while(ACCESSED, valid)
            })
        };
        let (first, second) = (fault(), fault());
        assert_eq!(first.join().unwrap(), Some(VALID | ACCESSED));
        assert_eq!(second.join().unwrap(), Some(VALID | ACCESSED));
        assert_eq!(slot.0.load(Ordering::Relaxed), VALID | ACCESSED);
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

/// A start word the model checker interleaves.
struct Word(AtomicU64);

impl StartWord for Word {
    fn read(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    fn exchange(&self, current: u64, new: u64) -> Result<u64, u64> {
        self.0
            .compare_exchange(current, new, Ordering::AcqRel, Ordering::Acquire)
    }
}

/// The started CPU answers as the starting CPU's wait runs out: exactly one
/// of the answer and the withdrawal wins, and the starting CPU settles on the
/// outcome the started one acts on.
#[test]
fn a_start_and_its_answer_settle_on_one_outcome() {
    loom::model(|| {
        let word = Arc::new(Word(AtomicU64::new(awaiting_answer(1))));
        let started = {
            let word = Arc::clone(&word);
            thread::spawn(move || answer_start(&*word, 1, StartAnswer::Joined))
        };
        let settled = settle_start(&*word, 1, || true);
        let runs = started.join().unwrap();
        assert_eq!(runs, settled == Some(StartAnswer::Joined));
    });
}

/// A core answers and waits for its verdict, however long that takes, while
/// the starting CPU waits a bounded while for the answer: the core runs
/// exactly when the starting CPU took its answer, and never on a start it
/// gave up on.
#[test]
fn a_core_runs_only_on_the_verdict_that_took_its_answer() {
    loom::model(|| {
        let answer = Arc::new(AtomicU64::new(0));
        let verdict = Arc::new(AtomicU64::new(0));
        let core = {
            let (answer, verdict) = (Arc::clone(&answer), Arc::clone(&verdict));
            thread::spawn(move || {
                answer.store(StartAnswer::Joined.word(1), Ordering::Release);
                await_start_verdict(|| verdict.load(Ordering::Acquire), 1, thread::yield_now)
                    == StartVerdict::Taken
            })
        };
        let mut reads = 0;
        let read = await_start_answer(
            || answer.load(Ordering::Acquire),
            1,
            || {
                reads += 1;
                reads > 1
            },
        );
        let given = StartVerdict::on(read);
        verdict.store(given.word(1), Ordering::Release);
        let runs = core.join().unwrap();
        assert_eq!(runs, given == StartVerdict::Taken);
        assert_eq!(runs, read == Some(StartAnswer::Joined));
    });
}
