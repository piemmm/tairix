//! The futex: the one generic blocking primitive userland builds its mutex,
//! condition variable, and thread join over (`plans/THREADS.md` decision 5).
//!
//! A userland lock is a word in the process's own memory. While it is
//! uncontended, acquiring and releasing it is a pair of atomic operations and
//! the kernel never hears about it at all; only when a thread must actually
//! *wait* does it enter the kernel, name the word, and park.
//!
//! # The key
//!
//! A wait key is `(ProcessId, user VA)` (decision 6). Address spaces are
//! per-process and hardware-isolated, so the same virtual address in two
//! processes names two unrelated words, and a key is unforgeable: the process
//! half comes from the kernel-attested capability record, never from the
//! caller. Cross-process (shared-memory-backed) futexes are a different
//! abstraction and deliberately absent.
//!
//! # Structure
//!
//! Every user thread is given its [`FutexWaiter`] when it is admitted, and
//! each wait it makes links that record rather than anything new, so a futex
//! wait needs no memory: on an exhausted heap a contended lock still parks its
//! waiter instead of failing it into a retry loop. A waiter is linked, in
//! arrival order, into the chain of the bucket its key hashes to, and a wake
//! walks that chain releasing the oldest waiters on its key. The buckets are
//! sized from the discovered CPU count, so a chain holds few waiters.
//!
//! A timed wait arms the row its thread was given on
//! [`crate::waitq::FUTEX_TIMER_WAITQ`] at admission, so the timed sweep and
//! the one-shot arming find a futex deadline exactly as any other.
//!
//! No path holds a bucket lock across an `unpark`: a wake unlinks a batch of
//! waiters under the lock and releases them once it is dropped.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::convert::Infallible;
use core::hash::Hasher;

use tairix_hash::{HashSeed, SipHash13};
use tairix_inline::ArrayVec;
use tairix_kernel_sched_api::TaskId;
use tairix_kernel_sec::ProcessId;
use tairix_sync::once::OnceCell;
use tairix_sync::SpinLock;

use crate::waitq::WaitQueueArch;

/// A futex wait key: the process that owns the address space and the user
/// virtual address of the 32-bit word inside it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct FutexKey {
    /// The owning process, resolved from the caller's kernel-attested
    /// capability record.
    pub process: ProcessId,
    /// The word's user virtual address, naturally aligned.
    pub uaddr: u64,
}

/// One thread's futex wait: made when the thread is admitted, and linked by
/// every wait it makes.
pub struct FutexWaiter {
    task: TaskId,
    /// Changed only under the lock of the bucket it is filed in; the lock
    /// makes it mutable, the bucket's makes it consistent.
    link: SpinLock<Link>,
}

struct Link {
    filed: Option<Filed>,
    next: Option<Arc<FutexWaiter>>,
    prev: Weak<FutexWaiter>,
}

/// The key a waiter waits on, the bucket of its table it is linked in, and
/// the arrival it was linked as there.
#[derive(Copy, Clone)]
struct Filed {
    key: FutexKey,
    bucket: usize,
    /// Unique within the bucket, so a wake that let go of the lock can tell
    /// whether the waiter it stopped at is still where it left it.
    arrival: u64,
}

impl FutexWaiter {
    /// `task`'s waiter, linked nowhere.
    #[must_use]
    pub const fn new(task: TaskId) -> Self {
        Self {
            task,
            link: SpinLock::new(Link {
                filed: None,
                next: None,
                prev: Weak::new(),
            }),
        }
    }

    fn filed(&self) -> Option<Filed> {
        self.link.lock().filed
    }
}

/// One bucket's linked waiters, oldest first.
struct Chain {
    head: Option<Arc<FutexWaiter>>,
    tail: Weak<FutexWaiter>,
    arrivals: u64,
    /// Waiters the wakes have examined, so a test can hold a wake to one walk.
    #[cfg(test)]
    walked: usize,
}

impl Chain {
    const fn new() -> Self {
        Self {
            head: None,
            tail: Weak::new(),
            arrivals: 0,
            #[cfg(test)]
            walked: 0,
        }
    }

    /// Link `waiter` last, on `key`, in the bucket at `bucket`. Each link lock
    /// is taken alone: the bucket's lock, held by the caller, is what keeps
    /// the chain consistent.
    fn push(&mut self, waiter: &Arc<FutexWaiter>, key: FutexKey, bucket: usize) {
        let arrival = self.arrivals;
        self.arrivals = arrival.wrapping_add(1);
        {
            let mut link = waiter.link.lock();
            link.filed = Some(Filed {
                key,
                bucket,
                arrival,
            });
            link.next = None;
            link.prev = self.tail.clone();
        }
        match self.tail.upgrade() {
            Some(tail) => tail.link.lock().next = Some(Arc::clone(waiter)),
            None => self.head = Some(Arc::clone(waiter)),
        }
        self.tail = Arc::downgrade(waiter);
    }

    /// Unlink `waiter`, which is linked here. The caller holds a share of it,
    /// so dropping the chain's cannot free it under this call.
    fn unlink(&mut self, waiter: &FutexWaiter) {
        let (next, prev) = {
            let mut link = waiter.link.lock();
            link.filed = None;
            (link.next.take(), core::mem::take(&mut link.prev))
        };
        match prev.upgrade() {
            Some(before) => before.link.lock().next.clone_from(&next),
            None => self.head.clone_from(&next),
        }
        match next {
            Some(after) => after.link.lock().prev = prev,
            None => self.tail = prev,
        }
    }
}

impl Drop for Chain {
    /// Iteratively, so a long chain cannot recurse through its links.
    fn drop(&mut self) {
        let mut next = self.head.take();
        while let Some(waiter) = next {
            let mut link = waiter.link.lock();
            link.filed = None;
            next = link.next.take();
        }
    }
}

type Bucket = SpinLock<Chain>;

/// Buckets per discovered CPU.
///
/// A bucket is a lock and two words, so many are cheap, and a chain walked by
/// a wake stays short however many threads wait; sized from the hardware, so a
/// large machine is not crowded onto a small machine's table.
const BUCKETS_PER_CPU: usize = 64;

/// Waiters a wake unlinks per hold of a bucket's lock: the granule that keeps
/// the wake allocation-free and each hold short, not a bound on how many it
/// wakes.
const WAKE_BATCH: usize = 32;

/// The resolved bucket table, published **exactly once** — by
/// [`init_buckets`] at boot, or by the first resolution that finds none.
///
/// An *empty* table is the published "nothing ever sized one" answer, so that
/// decision costs no allocation and cannot fail; [`buckets`] maps it onto
/// [`FALLBACK_BUCKET`].
static BUCKETS: OnceCell<Vec<Bucket>> = OnceCell::new();

/// The single bucket a build that never sized the table uses: a contention
/// choice, not a correctness one, so a futex still blocks and wakes exactly
/// as specified.
static FALLBACK_BUCKET: Bucket = SpinLock::new(Chain::new());

/// Size the bucket table from the discovered CPU count.
///
/// Called once per boot, necessarily **before the first key resolves** — a
/// table installed after that is refused, because a waiter records the index
/// of the bucket it is linked in, and swapping tables would strand it where no
/// waker looks.
pub fn init_buckets(cpu_count: usize) {
    if BUCKETS.is_initialised() {
        return;
    }
    let count = cpu_count.max(1).saturating_mul(BUCKETS_PER_CPU);
    let mut buckets = Vec::new();
    if buckets.try_reserve_exact(count).is_err() {
        // Deterministic OOM, never a panic: the single-bucket table stands.
        return;
    }
    for _ in 0..count {
        buckets.push(SpinLock::new(Chain::new()));
    }
    let _ = BUCKETS.set(buckets);
}

/// The live bucket table, fixed for the rest of this boot by the first
/// resolution: boot's sized table, or the empty "never sized" one.
fn buckets() -> &'static [Bucket] {
    match BUCKETS.get_or_try_init(|| Ok::<Vec<Bucket>, Infallible>(Vec::new())) {
        Ok(table) if !table.is_empty() => table.as_slice(),
        _ => core::slice::from_ref(&FALLBACK_BUCKET),
    }
}

/// The index `key` hashes to in a table of `len` buckets.
///
/// Keyed, because the address half of a key is chosen by the caller: with a
/// predictable hash a process could pick wait addresses that all land in one
/// bucket, lengthening one chain that other processes' keys share. The
/// per-boot key ([`tairix_hash`]) is what it cannot compute. Shifting the
/// address by two drops the always-zero alignment bits, so adjacent words in
/// one lock array still spread.
///
/// A boot whose CSPRNG never seeded hashes unkeyed; that is a contention
/// property rather than an authority one. A `len` of zero folds to one.
fn bucket_index(seed: HashSeed, key: FutexKey, len: usize) -> usize {
    let mut hasher = SipHash13::new(seed);
    hasher.write_u64(key.process.0);
    hasher.write_u64(key.uaddr >> 2);
    usize::try_from(hasher.finish() % len.max(1) as u64).unwrap_or(0)
}

/// A bucket table: the boot's live one, or a host test's own.
#[derive(Clone, Copy)]
struct Table<'a> {
    /// Never empty: the live table falls back to one bucket.
    buckets: &'a [Bucket],
}

impl Table<'static> {
    fn live() -> Self {
        Self { buckets: buckets() }
    }
}

impl<'a> Table<'a> {
    fn index_of(self, key: FutexKey) -> usize {
        let seed = tairix_hash::published().unwrap_or(HashSeed::UNKEYED);
        bucket_index(seed, key, self.buckets.len())
    }

    /// The bucket at `index`, which [`Self::index_of`] keeps in range.
    fn bucket(self, index: usize) -> &'a Bucket {
        &self.buckets[index]
    }

    fn file(self, waiter: &Arc<FutexWaiter>, key: FutexKey) {
        self.unfile(waiter);
        let bucket = self.index_of(key);
        self.bucket(bucket).lock().push(waiter, key, bucket);
    }

    fn unfile(self, waiter: &Arc<FutexWaiter>) -> bool {
        let Some(filed) = waiter.filed() else {
            return false;
        };
        let mut chain = self.bucket(filed.bucket).lock();
        // Only its own thread links a waiter, so once read it is either still
        // linked in that bucket or a wake has unlinked it since.
        if waiter.filed().is_none() {
            return false;
        }
        chain.unlink(waiter);
        true
    }

    /// Wake up to `count` of the waiters on `key` linked when the wake began.
    ///
    /// The chain is in arrival order, so the first round pins the arrival it
    /// reached and the walk stops there: a waiter linked while the wake runs
    /// waits for the next one, which also bounds a wake against a thread that
    /// keeps waiting again. Each later round resumes at the waiter the last
    /// stopped before, so nothing is walked twice however many rounds a wake
    /// takes.
    fn wake(self, arch: &dyn WaitQueueArch, key: FutexKey, count: usize) -> usize {
        let index = self.index_of(key);
        let bucket = self.bucket(index);
        let mut woken = 0usize;
        let mut end: Option<u64> = None;
        let mut resume: Option<(Arc<FutexWaiter>, u64)> = None;
        while woken < count {
            let mut batch: ArrayVec<Arc<FutexWaiter>, WAKE_BATCH> = ArrayVec::new();
            {
                let mut chain = bucket.lock();
                let end = *end.get_or_insert(chain.arrivals);
                // Still linked as the arrival it was, a waiter has nothing
                // unexamined ahead of it. One that left, or was linked again,
                // while the lock was down proves nothing, so the walk restarts.
                let mut cursor = match resume.take() {
                    Some((waiter, arrival))
                        if waiter.filed().is_some_and(|filed| {
                            filed.bucket == index && filed.arrival == arrival
                        }) =>
                    {
                        Some(waiter)
                    }
                    _ => chain.head.clone(),
                };
                while let Some(waiter) = cursor {
                    let (next, filed) = {
                        let link = waiter.link.lock();
                        (link.next.clone(), link.filed)
                    };
                    let Some(filed) = filed.filter(|filed| filed.arrival < end) else {
                        break;
                    };
                    if woken + batch.len() == count || batch.is_full() {
                        resume = Some((waiter, filed.arrival));
                        break;
                    }
                    #[cfg(test)]
                    {
                        chain.walked += 1;
                    }
                    if filed.key == key {
                        chain.unlink(&waiter);
                        let unlinked = batch.try_push(waiter);
                        debug_assert!(unlinked.is_ok(), "a batch took past its room");
                    }
                    cursor = next;
                }
            }
            // A waiter whose thread can never run again is unlinked but not
            // counted, so it cannot swallow a wake the next waiter is owed.
            for waiter in &batch {
                if arch.unpark(waiter.task) {
                    woken += 1;
                }
            }
            if resume.is_none() {
                break;
            }
        }
        woken
    }
}

/// Link `waiter` as waiting on `key`, last among its bucket's waiters.
///
/// The caller re-tests the word *after* this returns and parks only if it
/// still holds the expected value, which is the lost-wake discipline: a wake
/// that lands between the test and the park finds the waiter linked. Needs no
/// memory, so it cannot fail.
pub fn file(waiter: &Arc<FutexWaiter>, key: FutexKey) {
    Table::live().file(waiter, key);
}

/// Unlink `waiter`, answering whether it was still linked: `false` means a
/// wake took it first.
pub fn unfile(waiter: &Arc<FutexWaiter>) -> bool {
    Table::live().unfile(waiter)
}

/// Wake the `count` oldest threads waiting on `key`, returning how many wakes
/// landed.
///
/// Waking nobody is success: by the lost-wake discipline a thread that has not
/// parked yet re-tests the word itself.
pub fn wake(arch: &dyn WaitQueueArch, key: FutexKey, count: usize) -> usize {
    Table::live().wake(arch, key, count)
}

/// Wake the `count` oldest threads of `process` waiting on `uaddr`, through the
/// boot-installed wait-queue hook.
///
/// The form every in-kernel caller uses: the `futex_wake` syscall handler and
/// the thread-exit clear-on-exit notification. A build with no hook installed
/// can have nothing parked, so waking nobody is the honest answer.
#[must_use]
pub fn wake_installed(process: ProcessId, uaddr: u64, count: usize) -> usize {
    let Some(arch) = crate::waitq::wait_arch() else {
        return 0;
    };
    wake(arch, FutexKey { process, uaddr }, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::waitq::NO_DEADLINE;

    extern crate std;

    use core::cell::RefCell;

    /// A [`WaitQueueArch`] double recording every `unpark`, refusing the
    /// tasks it is told can never run again.
    struct MockArch<'a> {
        unparked: RefCell<Vec<TaskId>>,
        retired: Vec<TaskId>,
        /// Run at the first `unpark`, while the waking bucket's lock is down:
        /// what another thread can do between a wake's rounds.
        between_rounds: RefCell<Option<std::boxed::Box<dyn FnOnce() + 'a>>>,
    }

    impl<'a> MockArch<'a> {
        fn new() -> Self {
            Self::retiring(&[])
        }

        fn retiring(retired: &[TaskId]) -> Self {
            // Room up front, so recording a wake allocates nothing in a test
            // that refuses the heap.
            Self {
                unparked: RefCell::new(Vec::with_capacity(4 * WAKE_BATCH)),
                retired: retired.to_vec(),
                between_rounds: RefCell::new(None),
            }
        }

        fn between_rounds(self, act: impl FnOnce() + 'a) -> Self {
            *self.between_rounds.borrow_mut() = Some(std::boxed::Box::new(act));
            self
        }

        fn taken(&self) -> Vec<TaskId> {
            core::mem::take(&mut self.unparked.borrow_mut())
        }
    }

    // SAFETY-INVARIANT: the double is only ever driven from one test thread
    // at a time; the `Sync` claim exists solely to satisfy the trait bound.
    unsafe impl Sync for MockArch<'_> {}

    impl WaitQueueArch for MockArch<'_> {
        fn unpark(&self, task: TaskId) -> bool {
            let act = self.between_rounds.borrow_mut().take();
            if let Some(act) = act {
                act();
            }
            self.unparked.borrow_mut().push(task);
            !self.retired.contains(&task)
        }

        fn now_ns(&self) -> u64 {
            0
        }

        fn set_wakeup(&self, _deadline_ns: Option<u64>) {}
    }

    fn key(process: u64, uaddr: u64) -> FutexKey {
        FutexKey {
            process: ProcessId(process),
            uaddr,
        }
    }

    fn waiter(task: TaskId) -> Arc<FutexWaiter> {
        Arc::new(FutexWaiter::new(task))
    }

    /// Buckets for a test's own table, out of every sibling test's reach.
    fn own_buckets(len: usize) -> Vec<Bucket> {
        (0..len).map(|_| SpinLock::new(Chain::new())).collect()
    }

    fn table(buckets: &[Bucket]) -> Table<'_> {
        assert!(!buckets.is_empty(), "a table holds a bucket");
        Table { buckets }
    }

    /// A futex wait links only the record its thread was admitted with, so it
    /// files, wakes and leaves on a heap that refuses every allocation.
    ///
    /// Regression: a key's first waiter allocated its key a queue, its place
    /// in the bucket and its own row, so a contended lock under memory
    /// pressure answered `OutOfMemory` at once and its caller spun.
    #[test]
    fn a_wait_files_wakes_and_leaves_with_no_memory_to_spare() {
        let buckets = own_buckets(BUCKETS_PER_CPU);
        let table = table(&buckets);
        let k = key(0x9001, 0x4000);
        let (first, second) = (waiter(1), waiter(2));
        let arch = MockArch::new();
        crate::test_alloc::refuse_allocations_on_current_thread(true);
        table.file(&first, k);
        table.file(&second, k);
        let woken = table.wake(&arch, k, 1);
        let first_left = table.unfile(&first);
        let second_left = table.unfile(&second);
        crate::test_alloc::refuse_allocations_on_current_thread(false);
        assert_eq!(woken, 1);
        assert_eq!(arch.taken(), std::vec![1]);
        assert!(!first_left, "the wake unlinked the first waiter");
        assert!(second_left, "the second was still linked");
        assert_eq!(table.wake(&arch, k, usize::MAX), 0, "nobody is left");
    }

    #[test]
    fn wake_releases_the_oldest_waiters_first_and_bounds_the_count() {
        let buckets = own_buckets(BUCKETS_PER_CPU);
        let table = table(&buckets);
        let k = key(0x9002, 0x4000);
        let arch = MockArch::new();
        let waiters: Vec<_> = (1..=3).map(waiter).collect();
        for w in &waiters {
            table.file(w, k);
        }
        assert_eq!(table.wake(&arch, k, 2), 2);
        assert_eq!(arch.taken(), std::vec![1, 2]);
        assert_eq!(table.wake(&arch, k, usize::MAX), 1, "exactly those present");
        assert_eq!(arch.taken(), std::vec![3]);
    }

    /// Keys sharing a chain stay apart: a wake releases only its own key's
    /// waiters, in their order, and leaves the others linked in theirs.
    #[test]
    fn a_wake_passes_over_other_keys_sharing_its_chain() {
        let buckets = own_buckets(1);
        let table = table(&buckets);
        let (mine, theirs) = (key(0x9003, 0x4000), key(0x9003, 0x4004));
        let arch = MockArch::new();
        let waiters: Vec<_> = (1..=4).map(waiter).collect();
        for (w, k) in waiters.iter().zip([theirs, mine, theirs, mine]) {
            table.file(w, k);
        }
        assert_eq!(table.wake(&arch, mine, usize::MAX), 2);
        assert_eq!(arch.taken(), std::vec![2, 4]);
        assert_eq!(table.wake(&arch, theirs, 1), 1);
        assert_eq!(arch.taken(), std::vec![1]);
        assert!(table.unfile(&waiters[2]), "the last waiter kept its link");
    }

    #[test]
    fn two_processes_at_one_address_are_distinct_keys() {
        let buckets = own_buckets(1);
        let table = table(&buckets);
        let (mine, theirs) = (key(0x9004, 0x4000), key(0x9005, 0x4000));
        let arch = MockArch::new();
        let w = waiter(7);
        table.file(&w, mine);
        assert_eq!(
            table.wake(&arch, theirs, usize::MAX),
            0,
            "another process's word must not reach my waiter"
        );
        assert_eq!(table.wake(&arch, mine, usize::MAX), 1);
    }

    /// A waiter whose thread can never run again is unlinked by the wake that
    /// reaches it, and the counted wake passes on to the live waiter behind.
    #[test]
    fn a_dead_waiter_cannot_swallow_a_counted_wake() {
        let buckets = own_buckets(BUCKETS_PER_CPU);
        let table = table(&buckets);
        let k = key(0x9006, 0x4000);
        let arch = MockArch::retiring(&[11]);
        let (dead, live) = (waiter(11), waiter(12));
        table.file(&dead, k);
        table.file(&live, k);
        assert_eq!(table.wake(&arch, k, 1), 1, "the live waiter was woken");
        assert_eq!(arch.taken(), std::vec![11, 12]);
        assert!(!table.unfile(&dead), "and the dead one unlinked");
    }

    /// A wake reaching more waiters than one batch unlinks releases every one
    /// of them, in arrival order, across the seams between batches.
    #[test]
    fn a_wake_past_one_batch_releases_every_waiter_in_order() {
        let buckets = own_buckets(1);
        let table = table(&buckets);
        let k = key(0x9007, 0x4000);
        let other = key(0x9007, 0x4004);
        let arch = MockArch::new();
        let filed = 2 * WAKE_BATCH + 3;
        let ids: Vec<TaskId> = (1u64..).take(filed).collect();
        let waiters: Vec<_> = ids.iter().map(|&id| waiter(id)).collect();
        let bystander = waiter(10_000);
        table.file(&bystander, other);
        for w in &waiters {
            table.file(w, k);
        }
        assert_eq!(table.wake(&arch, k, usize::MAX), filed);
        assert_eq!(arch.taken(), ids);
        assert!(
            table.unfile(&bystander),
            "another key's waiter stayed linked"
        );
    }

    /// A wake walks its chain once, however many batches it takes.
    ///
    /// Regression: every round restarted at the head, so a broadcast behind
    /// other keys' waiters walked those once per batch.
    #[test]
    fn a_wake_walks_its_chain_once_across_batches() {
        let buckets = own_buckets(1);
        let table = table(&buckets);
        let (k, other) = (key(0x900b, 0x4000), key(0x900b, 0x4004));
        let arch = MockArch::new();
        let ahead: Vec<_> = (1..=8).map(waiter).collect();
        let behind: Vec<_> = (100u64..).take(3 * WAKE_BATCH).map(waiter).collect();
        for w in &ahead {
            table.file(w, other);
        }
        for w in &behind {
            table.file(w, k);
        }
        assert_eq!(table.wake(&arch, k, usize::MAX), behind.len());
        assert_eq!(buckets[0].lock().walked, ahead.len() + behind.len());
    }

    /// A waiter linked while a wake runs waits for the next wake, so a thread
    /// that keeps waiting again cannot keep a broadcast walking.
    ///
    /// Regression: each round walked to the chain's tail, reaching whatever
    /// had been linked since, so a thread re-filing on the key could hold a
    /// wake of every waiter on another CPU for as long as it kept going.
    #[test]
    fn a_waiter_linked_during_a_wake_waits_for_the_next() {
        let buckets = own_buckets(1);
        let table = table(&buckets);
        let k = key(0x900c, 0x4000);
        let waiters: Vec<_> = (1u64..).take(WAKE_BATCH + 1).map(waiter).collect();
        for w in &waiters {
            table.file(w, k);
        }
        let late = waiter(999);
        let arch = MockArch::new().between_rounds(|| table.file(&late, k));
        assert_eq!(table.wake(&arch, k, usize::MAX), waiters.len());
        assert!(table.unfile(&late), "the late waiter is still linked");
    }

    /// The waiter a round stopped before is no place to resume once it has
    /// left, or been linked again behind the rest, while the lock was down:
    /// the next round walks from the head and wakes every earlier waiter.
    #[test]
    fn a_round_resumes_only_where_its_seam_waiter_still_stands() {
        let k = key(0x900d, 0x4000);
        for linked_again in [false, true] {
            let buckets = own_buckets(1);
            let table = table(&buckets);
            let waiters: Vec<_> = (1u64..).take(WAKE_BATCH + 4).map(waiter).collect();
            for w in &waiters {
                table.file(w, k);
            }
            let seam = &waiters[WAKE_BATCH];
            let arch = MockArch::new().between_rounds(|| {
                if linked_again {
                    table.file(seam, k);
                } else {
                    assert!(table.unfile(seam));
                }
            });
            let mut expected: Vec<TaskId> = (1u64..).take(WAKE_BATCH + 4).collect();
            expected.remove(WAKE_BATCH);
            assert_eq!(table.wake(&arch, k, usize::MAX), expected.len());
            assert_eq!(arch.taken(), expected, "linked again: {linked_again}");
            assert_eq!(
                table.unfile(seam),
                linked_again,
                "linked again after the wake began, it waits for the next"
            );
        }
    }

    /// Re-filing a waiter that is still linked moves it rather than linking it
    /// twice, so one thread is never woken as two.
    #[test]
    fn a_waiter_filed_again_is_linked_once_under_its_new_key() {
        let buckets = own_buckets(1);
        let table = table(&buckets);
        let (old, new) = (key(0x9008, 0x4000), key(0x9008, 0x4004));
        let arch = MockArch::new();
        let w = waiter(21);
        table.file(&w, old);
        table.file(&w, new);
        assert_eq!(table.wake(&arch, old, usize::MAX), 0);
        assert_eq!(table.wake(&arch, new, usize::MAX), 1);
        assert_eq!(arch.taken(), std::vec![21]);
    }

    /// The waiter unlinked from the middle, the head and the tail of a chain
    /// leaves the rest linked in order.
    #[test]
    fn unfiling_from_any_place_keeps_the_rest_in_order() {
        let buckets = own_buckets(1);
        let table = table(&buckets);
        let k = key(0x9009, 0x4000);
        let arch = MockArch::new();
        let waiters: Vec<_> = (1..=5).map(waiter).collect();
        for w in &waiters {
            table.file(w, k);
        }
        for index in [2, 0, 4] {
            assert!(table.unfile(&waiters[index]));
        }
        assert_eq!(table.wake(&arch, k, usize::MAX), 2);
        assert_eq!(arch.taken(), std::vec![2, 4]);
    }

    #[test]
    fn a_relative_timeout_becomes_an_absolute_deadline_that_cannot_read_as_none() {
        assert_eq!(crate::waitq::deadline_for(1_000, 500), 1_500);
        assert_eq!(
            crate::waitq::deadline_for(0, 0),
            0,
            "an elapsed timeout is due at once"
        );
        assert_eq!(
            crate::waitq::deadline_for(1_000, u64::MAX),
            NO_DEADLINE,
            "the ABI's u64::MAX means explicit wake only"
        );
        // A span that saturates must still name a real deadline: rounding up to
        // the sentinel would turn a timed wait into an indefinite one.
        let saturated = crate::waitq::deadline_for(u64::MAX - 1, u64::MAX - 1);
        assert_eq!(saturated, NO_DEADLINE - 1);
        assert_ne!(saturated, NO_DEADLINE);
    }

    /// A fixed key for the pure index assertions, so they do not depend on
    /// whether the host test binary ever published one.
    const TEST_SEED: HashSeed = HashSeed::from_words(0x0f1e_2d3c_4b5a_6978, 0x8796_a5b4_c3d2_e1f0);

    /// The key is what stops a caller choosing wait addresses that all crowd
    /// one chain: the same addresses must land differently under a different
    /// key, or the hash is predictable.
    #[test]
    fn the_key_decides_where_a_chosen_address_lands() {
        let len = 8 * BUCKETS_PER_CPU;
        let other = HashSeed::from_words(0x0f1e_2d3c_4b5a_6978, 0x8796_a5b4_c3d2_e1f1);
        let moved = (0..64u64)
            .filter(|&word| {
                let k = key(0x9200, 0x8000 + word * 4);
                bucket_index(TEST_SEED, k, len) != bucket_index(other, k, len)
            })
            .count();
        // Landing in the same bucket by chance is a 1-in-`len` event per
        // address, so all but a few of any chosen run must move.
        assert!(
            moved >= 48,
            "only {moved} of 64 chosen addresses moved when the key changed"
        );
    }

    /// Asserted against the pure index function rather than by installing a
    /// table: the live table is fixed for the boot by its first use.
    #[test]
    fn adjacent_words_of_one_lock_array_spread_across_buckets() {
        let len = 8 * BUCKETS_PER_CPU;
        let mut seen = alloc::collections::BTreeSet::new();
        for word in 0..16u64 {
            seen.insert(bucket_index(TEST_SEED, key(0x9008, 0x4000 + word * 4), len));
        }
        assert!(
            seen.len() > 1,
            "16 adjacent lock words landed in a single bucket"
        );
    }

    /// The process id is mixed, not just the address.
    #[test]
    fn distinct_processes_at_one_address_spread_across_buckets() {
        let len = 8 * BUCKETS_PER_CPU;
        let mut seen = alloc::collections::BTreeSet::new();
        for process in 0..16u64 {
            seen.insert(bucket_index(TEST_SEED, key(0x9100 + process, 0x4000), len));
        }
        assert!(
            seen.len() > 1,
            "16 processes' words at one address landed in a single bucket"
        );
    }

    #[test]
    fn a_bucket_index_is_always_in_range() {
        for len in [1usize, 2, 3, 4 * BUCKETS_PER_CPU, 128 * BUCKETS_PER_CPU] {
            for word in 0..64u64 {
                let index = bucket_index(TEST_SEED, key(word, word * 4), len);
                assert!(index < len, "index {index} out of a {len}-bucket table");
            }
        }
    }

    /// The table is fixed for the boot by its first use: a sizing that arrives
    /// after a key has resolved is refused rather than stranding a linked
    /// waiter in a bucket no waker looks in.
    ///
    /// Every sibling test binary resolves a key first, so by the time this runs
    /// the table is already latched — precisely the state being asserted
    /// (`plans/OPEN-DEFECTS.md` D45).
    #[test]
    fn a_resolved_table_is_never_swapped_out_from_under_a_live_key() {
        let w = waiter(crate::test_boot::claim_task());
        let k = key(0x900a, 0x4000);
        file(&w, k);
        let len_before = buckets().len();
        init_buckets(8);
        assert_eq!(buckets().len(), len_before, "the live table was replaced");
        assert_eq!(wake(&MockArch::new(), k, 1), 1, "the waiter is still found");
        assert!(!unfile(&w));
    }
}
