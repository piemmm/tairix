//! Loom models of the slot an ordered dispatch's pieces wait on.
//!
//! The protocol is generic over `SlotWord`, so the models drive the real
//! `claim`, `settle` and `await_set` over loom's own atomics. The sleep is a
//! futex's: the word is checked and the sleeper queued under one lock a wake
//! also takes, so a wake that misses its sleeper leaves the model deadlocked.
//! Driven by `cargo xtask loom`; directly, that is:
//!
//! ```text
//! cargo test -p tairix-parallel --features loom --test loom --release
//! ```

#![cfg(feature = "loom")]

use loom::sync::atomic::{AtomicU32, Ordering};
use loom::sync::{Arc, Condvar, Mutex};
use loom::thread;

use tairix_parallel::ordered::{await_set, claim, settle, SlotWord};

/// A slot's state word whose sleep and wake are a futex's, and the value the
/// slot holds, written relaxed so only the protocol orders it.
struct Slot {
    state: AtomicU32,
    value: AtomicU32,
    /// Wakes so far; a sleeper waits for this to move.
    wakes: Mutex<u64>,
    woken: Condvar,
}

impl Slot {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU32::new(0),
            value: AtomicU32::new(0),
            wakes: Mutex::new(0),
            woken: Condvar::new(),
        })
    }

    /// Publish `value`, answering whether this call wrote it.
    fn publish(&self, value: u32) -> bool {
        if !claim(self) {
            return false;
        }
        self.value.store(value, Ordering::Relaxed);
        settle(self);
        true
    }

    fn wait(&self) -> u32 {
        await_set(self);
        self.value.load(Ordering::Relaxed)
    }
}

impl SlotWord for Slot {
    fn load(&self) -> u32 {
        self.state.load(Ordering::Acquire)
    }

    fn swap(&self, value: u32) -> u32 {
        self.state.swap(value, Ordering::Release)
    }

    fn compare_exchange(&self, current: u32, new: u32) -> Result<u32, u32> {
        self.state
            .compare_exchange(current, new, Ordering::Acquire, Ordering::Acquire)
    }

    fn sleep(&self, expected: u32) {
        let mut wakes = self.wakes.lock().expect("the model never poisons");
        if self.state.load(Ordering::Relaxed) != expected {
            return;
        }
        let seen = *wakes;
        while *wakes == seen {
            wakes = self.woken.wait(wakes).expect("the model never poisons");
        }
    }

    fn wake(&self) {
        let mut wakes = self.wakes.lock().expect("the model never poisons");
        *wakes += 1;
        self.woken.notify_all();
    }
}

/// A waiter racing the write it waits for wakes, and reads what was written.
#[test]
fn a_wait_ends_on_the_write_and_reads_it() {
    loom::model(|| {
        let slot = Slot::new();
        let writer = {
            let slot = Arc::clone(&slot);
            thread::spawn(move || assert!(slot.publish(7)))
        };
        assert_eq!(slot.wait(), 7);
        writer.join().expect("the writer finishes");
    });
}

/// Two waiters and two would-be writers: one write lands, both waiters wake
/// to it.
#[test]
fn one_of_two_writes_lands_and_every_waiter_reads_it() {
    loom::model(|| {
        let slot = Slot::new();
        let waiter = {
            let slot = Arc::clone(&slot);
            thread::spawn(move || slot.wait())
        };
        let rival = {
            let slot = Arc::clone(&slot);
            thread::spawn(move || slot.publish(2))
        };
        let mine = slot.publish(1);
        let theirs = rival.join().expect("the rival finishes");
        assert!(mine != theirs, "exactly one write lands");
        let landed = if mine { 1 } else { 2 };
        assert_eq!(waiter.join().expect("the waiter wakes"), landed);
        assert_eq!(slot.wait(), landed);
    });
}
