//! Loom model of the shared slice's holder count.
//!
//! Driven by `cargo xtask loom`, which is the only thing that builds this
//! file: under `--cfg loom` the count is loom's atomic, so the model drives
//! every interleaving of the holders coming and going. Directly, that is:
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test -p tairix-collections --test loom --release
//! ```
//!
//! Without the `loom` cfg the file compiles to an empty test binary.
//!
//! Each element writes to a loom cell as it is dropped, and every holder
//! reads that cell before letting go, so a free not ordered after some
//! holder's last read is a data race the model reports; and each element
//! counts its drops, so a block freed twice, or never, fails the model.

#![cfg(loom)]

use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::sync::Arc;
use loom::thread;

use tairix_collections::SharedSlice;

/// An element a holder reads and its drop writes.
struct Witness {
    value: UnsafeCell<usize>,
    drops: Arc<AtomicUsize>,
}

// SAFETY: holders only read `value` while the element is shared; the one
// write is its drop, after every holder has let go — which is the ordering
// the model checks.
unsafe impl Sync for Witness {}

impl Witness {
    fn new(value: usize, drops: &Arc<AtomicUsize>) -> Self {
        Self {
            value: UnsafeCell::new(value),
            drops: Arc::clone(drops),
        }
    }

    fn read(&self) -> usize {
        // SAFETY: nothing writes the value while a holder can read it.
        self.value.with(|value| unsafe { *value })
    }
}

impl Drop for Witness {
    fn drop(&mut self) {
        // SAFETY: the element is being dropped, so it is no longer shared.
        self.value.with_mut(|value| unsafe { *value = 0 });
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

fn shared(drops: &Arc<AtomicUsize>) -> SharedSlice<Witness> {
    SharedSlice::try_from_vec(vec![Witness::new(7, drops), Witness::new(8, drops)]).expect("held")
}

#[test]
fn loom_the_last_holder_frees_once_after_every_holders_use() {
    loom::model(|| {
        let drops = Arc::new(AtomicUsize::new(0));
        let first = shared(&drops);
        let second = first.clone();
        let other = thread::spawn(move || {
            assert_eq!(second[0].read() + second[1].read(), 15);
            drop(second);
        });
        assert_eq!(first[1].read(), 8);
        drop(first);
        other.join().expect("the other holder");
        assert_eq!(
            drops.load(Ordering::Relaxed),
            2,
            "each element dropped once"
        );
    });
}

#[test]
fn loom_a_holder_made_from_another_keeps_the_block_as_the_first_lets_go() {
    loom::model(|| {
        let drops = Arc::new(AtomicUsize::new(0));
        let first = shared(&drops);
        let second = first.clone();
        let other = thread::spawn(move || {
            let third = second.clone();
            drop(second);
            assert_eq!(third[0].read(), 7);
        });
        drop(first);
        other.join().expect("the other holder");
        assert_eq!(
            drops.load(Ordering::Relaxed),
            2,
            "each element dropped once"
        );
    });
}
