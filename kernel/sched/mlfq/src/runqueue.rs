//! Per-CPU bounded work-stealing queue.
//!
//! Each CPU owns one [`RunDeque`] per [`crate::Priority`] band, holding the
//! ready tasks themselves so a dispatch needs no registry lookup. Producers
//! push at the *bottom* end and every consumer — the owner included — takes
//! from the *top*, which keeps each band FIFO, as MLFQ fairness requires.
//! Consumption is lock-free: the loser of the `top` compare-exchange reports
//! [`Steal::Retry`] and the scheduler retries a bounded number of times.
//!
//! The ring is the bounded Chase–Lev deque, consumed only through `steal`:
//!
//! > Chase, D. & Lev, Y. *Dynamic Circular Work-Stealing Deque*. SPAA '05.
//!
//! with the orderings of Lê et al., *Correct and Efficient Work-Stealing for
//! Weak Memory Models* (`PPoPP` '13): a `Release` fence between a slot store
//! and the `bottom` advance, paired with each consumer's `Acquire` of
//! `bottom`, and a `SeqCst` fence and compare-exchange on `top`.
//!
//! Chase–Lev has one producer. A task is pushed by whichever CPU spawns, wakes
//! or migrates it, so producers take [`RunDeque::push`]'s lock, which makes
//! them one producer at a time; the lock is never held by a consumer.
//!
//! A slot owns the [`Arc`] pushed into it until the one consumer whose
//! compare-exchange claims its index takes it. The ring is bounded and a full
//! ring refuses a push, so no slot is rewritten while its index is unclaimed.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::marker::PhantomData;
use core::ptr;

use tairix_sync::SpinLock;

use crate::loom_compat::{fence, AtomicI64, AtomicPtr, Ordering};

/// Outcome of a stealing attempt.
#[derive(Debug, Eq, PartialEq)]
pub enum Steal<T> {
    /// Deque was empty at the time of the read.
    Empty,
    /// A concurrent consumer claimed the entry first. The caller should
    /// retry — typically against a different victim.
    Retry,
    /// The entry at the top of the deque, now the caller's.
    Stolen(T),
}

/// A bounded work-stealing deque of ready tasks.
///
/// Any CPU may [`push`](Self::push) (producers are serialised) and any CPU may
/// [`steal`](Self::steal) (lock-free).
pub struct RunDeque<T> {
    /// Slot storage. Capacity is `mask + 1`, a power of two.
    buf: Box<[AtomicPtr<T>]>,
    /// `capacity - 1`; used to index slots without a `%` instruction.
    mask: usize,
    /// Consumer end. Monotonically non-decreasing.
    top: AtomicI64,
    /// Producer end. Monotonically non-decreasing.
    bottom: AtomicI64,
    /// Serialises producers, so the ring has one producer at a time.
    producers: SpinLock<()>,
    /// The deque owns an `Arc<T>` per queued slot, so it is `Send` and `Sync`
    /// exactly when moving one between CPUs is.
    owns: PhantomData<Arc<T>>,
}

impl<T> RunDeque<T> {
    /// Construct a deque of `capacity` slots.
    ///
    /// Returns `None` if `capacity` is not a power of two, is less than 2,
    /// or exceeds `i64::MAX`, so the scheduler can surface the configuration
    /// error through `SchedError`.
    #[must_use]
    pub fn try_new(capacity: usize) -> Option<Self> {
        if capacity < 2 || !capacity.is_power_of_two() || i64::try_from(capacity).is_err() {
            return None;
        }
        let mut slots = alloc::vec::Vec::with_capacity(capacity);
        slots.resize_with(capacity, || AtomicPtr::new(ptr::null_mut()));
        Some(Self {
            buf: slots.into_boxed_slice(),
            mask: capacity - 1,
            top: AtomicI64::new(0),
            bottom: AtomicI64::new(0),
            producers: SpinLock::new(()),
            owns: PhantomData,
        })
    }

    /// Returns the deque's fixed capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.mask + 1
    }

    /// The slot index `i` lands in. Indices stay non-negative, and the low
    /// bits the mask keeps are unaffected by the cast.
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    fn slot(&self, i: i64) -> &AtomicPtr<T> {
        &self.buf[(i as usize) & self.mask]
    }

    /// Whether the deque appears to hold at least one ready task.
    ///
    /// Exact without concurrent consumers; otherwise a hint that may miss a
    /// just-pushed or just-stolen entry. It only decides whether to arm the
    /// tickless preemption timer, where a wrong answer costs one quantum.
    #[must_use]
    pub fn has_ready(&self) -> bool {
        let b = self.bottom.load(Ordering::Acquire);
        let t = self.top.load(Ordering::Acquire);
        b.wrapping_sub(t) > 0
    }

    /// Number of items currently in the deque: exact without concurrent
    /// consumers, in `[0, capacity]` otherwise. An observation only.
    #[must_use]
    pub fn len_approx(&self) -> usize {
        let b = self.bottom.load(Ordering::Acquire);
        let t = self.top.load(Ordering::Acquire);
        usize::try_from(b.wrapping_sub(t)).map_or(0, |len| len.min(self.capacity()))
    }

    /// Push `task` at the bottom end, handing it back if the deque is full so
    /// the caller can place it elsewhere.
    pub fn push(&self, task: Arc<T>) -> Result<(), Arc<T>> {
        let _producer = self.producers.lock();
        // The lock orders this load after the last producer's store.
        let b = self.bottom.load(Ordering::Relaxed);
        let t = self.top.load(Ordering::Acquire);
        // A stale `t` only makes the ring look fuller, never emptier.
        if usize::try_from(b.wrapping_sub(t)).is_ok_and(|len| len >= self.capacity()) {
            return Err(task);
        }
        self.slot(b)
            .store(Arc::into_raw(task).cast_mut(), Ordering::Relaxed);
        fence(Ordering::Release);
        self.bottom.store(b.wrapping_add(1), Ordering::Relaxed);
        Ok(())
    }

    /// Take the task at the top end. Safe to call from any CPU, including the
    /// owner.
    pub fn steal(&self) -> Steal<Arc<T>> {
        let t = self.top.load(Ordering::Acquire);
        fence(Ordering::SeqCst);
        let b = self.bottom.load(Ordering::Acquire);
        if b.wrapping_sub(t) <= 0 {
            return Steal::Empty;
        }
        let raw = self.slot(t).load(Ordering::Relaxed);
        if self
            .top
            .compare_exchange(t, t.wrapping_add(1), Ordering::SeqCst, Ordering::Relaxed)
            .is_err()
            || raw.is_null()
        {
            return Steal::Retry;
        }
        // SAFETY: `raw` is what the push of index `t` stored, from
        // `Arc::into_raw`: the `Acquire` of `bottom > t` makes that store
        // visible, and no later push rewrote the slot, since one would have
        // needed `top > t` and `top` was `t` until this compare-exchange.
        // Claiming index `t` makes this call the one owner of that `Arc`.
        Steal::Stolen(unsafe { Arc::from_raw(raw) })
    }
}

impl<T> Drop for RunDeque<T> {
    fn drop(&mut self) {
        let t = self.top.load(Ordering::Relaxed);
        let b = self.bottom.load(Ordering::Relaxed);
        let mut i = t;
        while i < b {
            let raw = self.slot(i).load(Ordering::Relaxed);
            if !raw.is_null() {
                // SAFETY: index `i` lies in `[top, bottom)`, so no consumer
                // claimed it and its slot still owns the `Arc` its push stored;
                // `&mut self` rules out any concurrent claim.
                drop(unsafe { Arc::from_raw(raw) });
            }
            i = i.wrapping_add(1);
        }
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use core::sync::atomic::{AtomicBool, AtomicUsize};

    fn drain(q: &RunDeque<u64>) -> Vec<u64> {
        let mut out = Vec::new();
        loop {
            match q.steal() {
                Steal::Stolen(v) => out.push(*v),
                Steal::Empty => return out,
                Steal::Retry => {}
            }
        }
    }

    #[test]
    fn rejects_non_pow2_capacity() {
        assert!(RunDeque::<u64>::try_new(3).is_none());
        assert!(RunDeque::<u64>::try_new(0).is_none());
        assert!(RunDeque::<u64>::try_new(1).is_none());
        assert!(RunDeque::<u64>::try_new(2).is_some());
        assert!(RunDeque::<u64>::try_new(1024).is_some());
    }

    #[test]
    fn steal_is_fifo_relative_to_push() {
        let q = RunDeque::try_new(8).expect("cap 8");
        for i in 1..=4u64 {
            q.push(Arc::new(i)).expect("space");
        }
        assert_eq!(drain(&q), [1, 2, 3, 4]);
    }

    #[test]
    fn push_hands_the_task_back_when_full() {
        let q = RunDeque::try_new(2).expect("cap 2");
        q.push(Arc::new(1)).expect("first");
        q.push(Arc::new(2)).expect("second");
        assert_eq!(q.push(Arc::new(3)).map_err(|task| *task), Err(3));
    }

    #[test]
    fn steal_on_empty_returns_empty_repeatedly() {
        let q = RunDeque::try_new(4).expect("cap 4");
        assert_eq!(q.steal(), Steal::Empty);
        assert_eq!(q.steal(), Steal::Empty);
        q.push(Arc::new(7)).expect("space");
        assert_eq!(q.steal(), Steal::Stolen(Arc::new(7)));
        assert_eq!(q.steal(), Steal::Empty);
    }

    #[test]
    fn steal_then_push_reuses_slots() {
        let q = RunDeque::try_new(4).expect("cap 4");
        for i in 1..=4u64 {
            q.push(Arc::new(i)).expect("space");
        }
        assert_eq!(drain(&q).len(), 4);
        for i in 10..14u64 {
            q.push(Arc::new(i)).expect("space");
        }
        assert_eq!(q.len_approx(), 4);
    }

    /// A slot owns its task: a stolen one is handed over with its reference,
    /// and a deque dropped with tasks queued releases each exactly once.
    #[test]
    fn a_queued_task_is_owned_until_stolen_or_the_deque_drops() {
        let task = Arc::new(5u64);
        let q = RunDeque::try_new(4).expect("cap 4");
        q.push(Arc::clone(&task)).expect("space");
        q.push(Arc::clone(&task)).expect("space");
        assert_eq!(Arc::strong_count(&task), 3);
        let Steal::Stolen(stolen) = q.steal() else {
            panic!("a queued task is stolen");
        };
        assert!(Arc::ptr_eq(&stolen, &task));
        drop(stolen);
        assert_eq!(
            Arc::strong_count(&task),
            2,
            "a steal leaves the slot behind"
        );
        drop(q);
        assert_eq!(Arc::strong_count(&task), 1, "the queued one is released");
    }

    /// Counts its own drops, so a task dropped twice or never shows.
    struct Counted<'a> {
        id: u64,
        drops: &'a AtomicUsize,
    }

    impl Drop for Counted<'_> {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Several producers and consumers at once: every task pushed is taken
    /// exactly once and released exactly once. Concurrent producers on one
    /// deque are what spawn, a remote wake and a migrating yield amount to.
    #[test]
    fn concurrent_producers_and_consumers_hand_off_every_task_once() {
        use std::sync::Mutex;
        use std::thread;

        let producers = 3u64;
        let per_producer = if cfg!(miri) { 40 } else { 2_000 };
        let consumers = 3;
        let drops = AtomicUsize::new(0);
        let q = RunDeque::try_new(64).expect("cap");
        let pushed = AtomicUsize::new(0);
        let done = AtomicBool::new(false);
        let taken = Mutex::new(Vec::new());

        thread::scope(|scope| {
            for p in 0..producers {
                let (q, drops, pushed) = (&q, &drops, &pushed);
                scope.spawn(move || {
                    for n in 0..per_producer {
                        let mut task = Arc::new(Counted {
                            id: p * per_producer + n,
                            drops,
                        });
                        while let Err(back) = q.push(task) {
                            task = back;
                            thread::yield_now();
                        }
                        pushed.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
            for _ in 0..consumers {
                let (q, done, taken) = (&q, &done, &taken);
                scope.spawn(move || {
                    let mut local = Vec::new();
                    loop {
                        match q.steal() {
                            Steal::Stolen(task) => local.push(task.id),
                            Steal::Retry => {}
                            Steal::Empty if done.load(Ordering::Acquire) => break,
                            Steal::Empty => thread::yield_now(),
                        }
                    }
                    taken.lock().expect("unpoisoned").extend(local);
                });
            }
            let total = usize::try_from(producers * per_producer).expect("small");
            while pushed.load(Ordering::Acquire) < total {
                thread::yield_now();
            }
            done.store(true, Ordering::Release);
        });

        let mut all = taken.into_inner().expect("unpoisoned");
        loop {
            match q.steal() {
                Steal::Stolen(task) => all.push(task.id),
                Steal::Retry => {}
                Steal::Empty => break,
            }
        }
        all.sort_unstable();
        let expected: Vec<u64> = (0..producers * per_producer).collect();
        assert_eq!(all, expected, "no task lost or duplicated");
        assert_eq!(
            drops.load(Ordering::Relaxed),
            expected.len(),
            "each released once"
        );
    }
}
