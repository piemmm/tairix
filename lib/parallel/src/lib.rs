//! Bounded data-parallel work: how a pass hands the machine's other cores the
//! work it has already proved independent.
//!
//! A pass that has split its output into pieces no two of which read or write
//! the same byte — a compositor's row bands, a blur's column sub-bands — states
//! that by handing the pieces here. Nothing in this crate discovers
//! independence; it is the caller's proof, and the caller keeps it.
//!
//! # The three parts
//!
//! * [`JobRunner`] — the contract. A pass names it, so the pass compiles and
//!   runs without a thread anywhere near it: [`Serial`] is a runner, and it is
//!   the whole of what an in-kernel or single-core consumer links.
//! * [`for_each`] — the one place an index becomes an element. A runner deals
//!   in indices because that is all it can share between threads; a pass wants
//!   its own `&mut` piece. That conversion is the crate's single `unsafe`
//!   block, and it lives here rather than in every pass. [`for_each_drawn`]
//!   hands out pieces an iterator splits off instead, so a pass need not
//!   gather them into a list first.
//! * [`Pool`] (feature `pool`) — the fork-join worker pool over `lib/rt`
//!   threads. Workers park on a futex between dispatches and never spin.
//!
//! # Sizing
//!
//! [`bands`] is the one split policy. A caller says how many units of work it
//! has and how few units are worth a hand-off; the answer is how many pieces to
//! make. Work below one piece's worth runs on the calling thread with no
//! atomics and no syscall, so a small repaint costs exactly what it did before
//! a pool existed.
//!
//! [`Pool`]: pool::Pool

#![no_std]

#[cfg(feature = "pool")]
extern crate alloc;

// `Threaded` needs real host threads, and thus `std`. It is reached only
// through a `dev-dependencies` edge, so a shipping build never enables the
// feature and stays `no_std`; one that did would fail to link rather than
// quietly acquire a host runtime.
#[cfg(any(test, feature = "test-util"))]
extern crate std;

#[cfg(feature = "pool")]
pub mod pool;

#[cfg(feature = "pool")]
pub use pool::Pool;

use tairix_inline::ArrayVec;

/// How many pieces of work a runner can make progress on at once, and how to
/// run them.
///
/// # Safety
///
/// [`for_each`] hands each index an exclusive borrow of one element, so an
/// implementation must guarantee both of:
///
/// 1. **Each index is passed to `job` at most once, and never concurrently with
///    itself.** Two live invocations for the same index would alias one `&mut`.
/// 2. **`run` does not return until every invocation it made has returned.**
///    The caller's borrow of the elements ends when `run` returns, so an
///    invocation still running past it would hold a dangling reference.
///
/// Indices `job` is *not* passed are the implementation's business: a runner
/// that skips one simply leaves that element unvisited, which is a correctness
/// bug in the runner and not unsoundness. Both obligations above are memory
/// safety, which is why this trait is `unsafe`.
pub unsafe trait JobRunner: Sync {
    /// How many of this runner's jobs can be in progress at once — `1` for a
    /// runner that runs them on the calling thread.
    ///
    /// A caller splits its work into [`bands`] pieces derived from this, so a
    /// runner reporting `1` is asked for one piece and pays nothing for the
    /// machinery it does not use.
    fn width(&self) -> usize;

    /// Run `job(index)` for every `index` in `0..count`, in any order and
    /// possibly concurrently, returning once every one of them has returned.
    fn run(&self, count: usize, job: &(dyn Fn(usize) + Sync));
}

/// The runner that runs every job on the calling thread, in index order.
///
/// This is not a fallback or a stub: it is the correct runner wherever there is
/// no second core to use, no thread to create (an in-kernel consumer), or no
/// reason to hand work off. A pass written against [`JobRunner`] is complete
/// with only this.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Serial;

// SAFETY: `run` calls `job` exactly once for each index of `0..count`, from the
// calling thread, and the loop has finished before it returns — so no index is
// ever live twice and nothing outlives the call.
unsafe impl JobRunner for Serial {
    fn width(&self) -> usize {
        1
    }

    fn run(&self, count: usize, job: &(dyn Fn(usize) + Sync)) {
        for index in 0..count {
            job(index);
        }
    }
}

/// The [`Serial`] runner, for a caller that needs a `&'static dyn JobRunner`
/// and has no pool.
pub static SERIAL: Serial = Serial;

/// How much more finely than [`JobRunner::width`] work is split.
///
/// Pieces are claimed dynamically, so splitting finer than the runner is wide
/// costs one atomic increment per extra piece and buys back the case that
/// actually bites on a loaded machine: a core taken by another tenant leaves
/// one participant late, and with a piece each the whole pass waits for it.
/// With several pieces each, a straggler holds up one small piece and the
/// others absorb its share.
const OVERSUBSCRIPTION: usize = 4;

/// How many pieces `units` units of work should be split into for `runner`,
/// where a piece carrying fewer than `grain` units is not worth handing off.
///
/// `0` for no work at all, `1` whenever the work is too small to split or the
/// runner is one thread wide — in which case a caller runs its loop exactly as
/// it would with no pool at all.
#[must_use]
pub fn bands(runner: &dyn JobRunner, units: usize, grain: usize) -> usize {
    if units == 0 {
        return 0;
    }
    let width = runner.width().max(1);
    if width == 1 {
        return 1;
    }
    // Whole pieces of at least `grain` units, so the last piece is the only one
    // that can be short and a tiny job is never fragmented.
    let affordable = units / grain.max(1);
    affordable.clamp(1, width.saturating_mul(OVERSUBSCRIPTION))
}

/// How many of `units` units each of `pieces` pieces takes, the last taking
/// what is left: at least one, so a split never makes a piece of nothing.
#[must_use]
pub const fn piece_len(units: usize, pieces: usize) -> usize {
    let len = units.div_ceil(if pieces == 0 { 1 } else { pieces });
    if len == 0 {
        1
    } else {
        len
    }
}

/// A runner that reports a width it does not have and runs its pieces
/// **backwards** on the calling thread.
///
/// Test scaffolding, behind the `test-util` feature: a pass proves its output
/// does not depend on which piece runs first by composing the same input
/// through [`SERIAL`] and through this, and comparing bytes. Running the pieces
/// on one thread is what makes that a proof about the *split* rather than about
/// thread timing — bit-identity cannot be a matter of luck.
///
/// It lives here rather than in each pass's own tests because the `unsafe impl`
/// belongs beside the trait whose obligations it discharges, and because three
/// crates were otherwise spelling the same eight lines.
#[cfg(any(test, feature = "test-util"))]
#[derive(Debug, Default)]
pub struct Reversed {
    width: usize,
    /// The largest number of pieces any dispatch asked for, so a test can see
    /// whether the work was split at all rather than assuming it was.
    widest: core::sync::atomic::AtomicUsize,
    /// How many dispatches it has run, so a test can see how often the work
    /// waited on its slowest piece.
    dispatches: core::sync::atomic::AtomicUsize,
}

#[cfg(any(test, feature = "test-util"))]
impl Reversed {
    /// A runner claiming to be `width` participants wide.
    #[must_use]
    pub const fn new(width: usize) -> Self {
        Self {
            width,
            widest: core::sync::atomic::AtomicUsize::new(0),
            dispatches: core::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// The most pieces any dispatch on this runner asked for.
    #[must_use]
    pub fn widest(&self) -> usize {
        self.widest.load(core::sync::atomic::Ordering::Relaxed)
    }

    /// How many dispatches this runner has run.
    #[must_use]
    pub fn dispatches(&self) -> usize {
        self.dispatches.load(core::sync::atomic::Ordering::Relaxed)
    }
}

// SAFETY: each index of `0..count` is passed exactly once, from the calling
// thread, and every call has returned before `run` does.
#[cfg(any(test, feature = "test-util"))]
unsafe impl JobRunner for Reversed {
    fn width(&self) -> usize {
        self.width
    }

    fn run(&self, count: usize, job: &(dyn Fn(usize) + Sync)) {
        self.widest
            .fetch_max(count, core::sync::atomic::Ordering::Relaxed);
        self.dispatches
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        for index in (0..count).rev() {
            job(index);
        }
    }
}

/// A runner that spreads its pieces over `width` real host threads, each
/// claiming indices from one shared cursor.
///
/// Test scaffolding, behind the `test-util` feature, and the companion to
/// [`Reversed`]: that one proves a split is order-independent on a single
/// thread, this one proves the pieces really are disjoint when they run at
/// once. Only a genuinely concurrent runner can, and it is what puts the
/// element hand-off below under a thread sanitiser or an interpreter's
/// data-race detector.
///
/// Spawning is per dispatch rather than from a held pool because the subject
/// is the hand-off, not the scheduling: a pool would add its own
/// synchronisation between the jobs and blunt exactly what this is for.
#[cfg(any(test, feature = "test-util"))]
#[derive(Copy, Clone, Debug)]
pub struct Threaded {
    width: usize,
}

#[cfg(any(test, feature = "test-util"))]
impl Threaded {
    /// A runner spreading its pieces over `width` threads.
    ///
    /// A zero width still runs on one thread: the split asks for at least one
    /// piece, and a runner that spawned nothing would leave it unvisited.
    #[must_use]
    pub const fn new(width: usize) -> Self {
        Self { width }
    }
}

// SAFETY: every index of `0..count` is handed to `job` at most once — the
// cursor's `fetch_add` gives each index to exactly one thread — and `scope`
// joins every thread before it returns, so no invocation outlives the call.
#[cfg(any(test, feature = "test-util"))]
unsafe impl JobRunner for Threaded {
    fn width(&self) -> usize {
        self.width
    }

    fn run(&self, count: usize, job: &(dyn Fn(usize) + Sync)) {
        let next = core::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..self.width.max(1) {
                scope.spawn(|| loop {
                    let index = next.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    if index >= count {
                        return;
                    }
                    job(index);
                });
            }
        });
    }
}

/// A raw pointer to the elements of one [`for_each`] call, shared with that
/// call's jobs.
///
/// Private and reachable only from [`for_each`], so the `Send` / `Sync` claims
/// below cannot be borrowed by anything that does not hold [`JobRunner`]'s
/// obligations. The accessor exists so a job closure captures the whole wrapper
/// rather than the bare pointer field, which is what carries those claims into
/// the closure's own auto-traits.
struct Elements<T>(*mut T);

impl<T> Elements<T> {
    /// The address of element `index`.
    const fn at(&self, index: usize) -> *mut T {
        // SAFETY: the caller checks `index` against the slice's length before
        // dereferencing; forming the address itself stays in bounds because of
        // that check, and this returns a raw pointer rather than a borrow.
        unsafe { self.0.add(index) }
    }
}

// SAFETY: the pointer is only ever offset to an index the runner passed to
// exactly one live job, so the access it grants is an exclusive borrow of one
// `T` — sending that between threads is sending a `T`.
unsafe impl<T: Send> Send for Elements<T> {}
// SAFETY: as above; sharing the pointer between jobs grants each of them a
// different element.
unsafe impl<T: Send> Sync for Elements<T> {}

/// Visit each element of `items` exactly once — `visit(&mut items[i])` for every
/// `i` — spread across `runner`, returning when every element has been visited.
///
/// This is the crate's whole surface for a pass: the pass splits its output into
/// `items` (whose disjointness it proved by construction, typically with
/// `split_at_mut` / `chunks_mut`), and this hands each piece to whichever
/// participant claims it.
///
/// An empty slice does nothing and a single element is visited on the calling
/// thread, so neither reaches the runner.
pub fn for_each<T: Send>(runner: &dyn JobRunner, items: &mut [T], visit: &(dyn Fn(&mut T) + Sync)) {
    let count = items.len();
    if count <= 1 {
        if let Some(only) = items.first_mut() {
            visit(only);
        }
        return;
    }
    let elements = Elements(items.as_mut_ptr());
    runner.run(count, &|index| {
        // A runner that hands out an index it was never given would be unsound
        // rather than merely wrong, so the bound is re-checked here: the worst a
        // broken runner can then do is leave an element unvisited.
        if index >= count {
            return;
        }
        // SAFETY: `index < count`, so the address is inside the slice the caller
        // still borrows exclusively; `JobRunner` guarantees no other live job
        // holds this index, so the borrow is unique; and `run` does not return
        // until this job has, so the slice outlives it.
        let item = unsafe { &mut *elements.at(index) };
        visit(item);
    });
}

/// How many pieces a drawn dispatch gathers on the dispatching thread's own
/// stack: a round of [`OVERSUBSCRIPTION`] pieces for each participant of a
/// runner up to 32 wide, with no allocation.
const DRAWN_BATCH: usize = 32 * OVERSUBSCRIPTION;

/// Visit every piece `pieces` yields exactly once, spread across `runner`,
/// returning when every one has been visited.
///
/// For pieces a pass splits off one borrow at a time — row bands, a buffer cut
/// into parts of differing lengths — which [`for_each`] would need gathered
/// into a list first. The dispatching thread draws them a round at a time and
/// the runner's index `i` visits the round's piece `i`, so no participant
/// waits on a lock and the iterator runs only where it was made. A round is
/// four pieces for each participant, so a straggler is absorbed however wide
/// the runner: on the dispatching thread's stack for a runner up to 32 wide,
/// and past that from one buffer reserved for the dispatch, or a stack round
/// at a time should that be refused. The pieces must be independent, as
/// [`for_each`]'s elements are.
///
/// A runner one thread wide visits the pieces in order on the calling thread.
pub fn for_each_drawn<I>(runner: &dyn JobRunner, pieces: I, visit: &(dyn Fn(I::Item) + Sync))
where
    I: Iterator,
    I::Item: Send,
{
    fold_drawn(runner, pieces, (), &|piece| visit(piece), &|(), ()| ());
}

/// [`for_each_drawn`], joining what each visit answers onto `start`.
///
/// The answers are joined on the calling thread in the order the pieces were
/// drawn, so the result is the serial fold's whichever runner visited them and
/// in whatever order — a floating-point sum included.
pub fn fold_drawn<I, R>(
    runner: &dyn JobRunner,
    mut pieces: I,
    start: R,
    visit: &(dyn Fn(I::Item) -> R + Sync),
    join: &(dyn Fn(R, R) -> R + Sync),
) -> R
where
    I: Iterator,
    I::Item: Send,
    R: Copy + Send,
{
    if runner.width() <= 1 {
        return pieces.fold(start, |joined, piece| join(joined, visit(piece)));
    }
    #[cfg(feature = "pool")]
    if let Some(mut round) = HeapRound::for_runner(runner) {
        return fold_rounds(runner, &mut round, &mut pieces, start, visit, join);
    }
    let mut round = ArrayVec::<Slot<I::Item, R>, DRAWN_BATCH>::new();
    fold_rounds(runner, &mut round, &mut pieces, start, visit, join)
}

/// A piece waiting to be visited, and what its visit answered.
type Slot<T, R> = (Option<T>, Option<R>);

/// Where a drawn dispatch holds a round of pieces while the runner visits it.
trait Round<T> {
    /// Fill a new round from `pieces`, answering whether it took a whole one,
    /// so more may follow.
    fn refill(&mut self, pieces: &mut dyn Iterator<Item = T>) -> bool;

    fn slots(&mut self) -> &mut [T];
}

impl<T, const N: usize> Round<T> for ArrayVec<T, N> {
    fn refill(&mut self, pieces: &mut dyn Iterator<Item = T>) -> bool {
        self.clear();
        for piece in pieces.take(N) {
            // `take` stops at the capacity, so there is always room.
            let _ = self.try_push(piece);
        }
        self.is_full()
    }

    fn slots(&mut self) -> &mut [T] {
        self
    }
}

/// A round for a runner wider than a stack round holds, reserved once for
/// the dispatch.
#[cfg(feature = "pool")]
struct HeapRound<T> {
    slots: alloc::vec::Vec<T>,
    len: usize,
}

#[cfg(feature = "pool")]
impl<T> HeapRound<T> {
    /// The round `runner` wants, when that is more than a stack round and the
    /// heap holds it.
    fn for_runner(runner: &dyn JobRunner) -> Option<Self> {
        let len = runner.width().saturating_mul(OVERSUBSCRIPTION);
        if len <= DRAWN_BATCH {
            return None;
        }
        let mut slots = alloc::vec::Vec::new();
        slots.try_reserve_exact(len).ok()?;
        Some(Self { slots, len })
    }
}

#[cfg(feature = "pool")]
impl<T> Round<T> for HeapRound<T> {
    fn refill(&mut self, pieces: &mut dyn Iterator<Item = T>) -> bool {
        self.slots.clear();
        // Reserved for `len`, so filling it never moves it.
        self.slots.extend(pieces.take(self.len));
        self.slots.len() == self.len
    }

    fn slots(&mut self) -> &mut [T] {
        &mut self.slots
    }
}

/// Hand `pieces` out a round at a time, joining the answers in the order the
/// pieces were drawn.
fn fold_rounds<I, R>(
    runner: &dyn JobRunner,
    round: &mut impl Round<Slot<I::Item, R>>,
    pieces: &mut I,
    start: R,
    visit: &(dyn Fn(I::Item) -> R + Sync),
    join: &(dyn Fn(R, R) -> R + Sync),
) -> R
where
    I: Iterator,
    I::Item: Send,
    R: Copy + Send,
{
    let mut joined = start;
    loop {
        let full = round.refill(&mut pieces.by_ref().map(|piece| (Some(piece), None)));
        let slots = round.slots();
        for_each(runner, slots, &|(piece, answer)| {
            if let Some(piece) = piece.take() {
                *answer = Some(visit(piece));
            }
        });
        // A runner that skipped an index left its piece unvisited and its
        // answer unjoined.
        for (_, answer) in &*slots {
            if let Some(answer) = answer {
                joined = join(joined, *answer);
            }
        }
        if !full {
            return joined;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runner that drops the last job, so the "an unvisited element is a bug,
    /// not unsoundness" claim is exercised rather than only argued.
    struct Forgetful;

    // SAFETY: the indices it does pass are each passed once, on the calling
    // thread, and all have returned before `run` does. Skipping one is
    // permitted.
    unsafe impl JobRunner for Forgetful {
        fn width(&self) -> usize {
            2
        }

        fn run(&self, count: usize, job: &(dyn Fn(usize) + Sync)) {
            for index in 0..count.saturating_sub(1) {
                job(index);
            }
        }
    }

    #[test]
    fn each_element_is_visited_exactly_once() {
        let mut items = [0u32; 16];
        for_each(&SERIAL, &mut items, &|item| *item += 1);
        assert!(items.iter().all(|&count| count == 1));
    }

    /// The property every parallel pass depends on: the result cannot depend on
    /// which piece runs first.
    #[test]
    fn the_order_pieces_run_in_does_not_change_the_result() {
        let mut forwards = [0u32; 16];
        let mut backwards = [0u32; 16];
        let stamp = |slot: &mut u32| *slot = *slot * 2 + 1;
        for_each(&SERIAL, &mut forwards, &stamp);
        for_each(&Reversed::new(4), &mut backwards, &stamp);
        assert_eq!(forwards, backwards);
    }

    /// The scaffolding reports what it was asked to run: how many dispatches,
    /// and the widest of them.
    #[test]
    fn a_reversed_runner_counts_its_dispatches_and_their_width() {
        let runner = Reversed::new(4);
        let mut items = [0u32; 6];
        for_each(&runner, &mut items, &|item| *item += 1);
        for_each(&runner, &mut items[..3], &|item| *item += 1);
        assert_eq!(runner.dispatches(), 2);
        assert_eq!(runner.widest(), 6);
        assert_eq!(items, [2, 2, 2, 1, 1, 1]);
    }

    /// The hand-off under real concurrency: the pieces are disjoint, so
    /// running them at once must write exactly what running them in turn
    /// wrote. An interpreter's data-race detector reads this as the claim
    /// about `Elements` that a single-threaded runner structurally cannot
    /// make.
    #[test]
    fn running_the_pieces_at_once_writes_what_running_them_in_turn_wrote() {
        let mut serial = [0u64; 256];
        let mut concurrent = [0u64; 256];
        let stamp = |slot: &mut u64| *slot = slot.wrapping_mul(2).wrapping_add(0x9E37_79B9);
        for_each(&SERIAL, &mut serial, &stamp);
        for_each(&Threaded::new(4), &mut concurrent, &stamp);
        assert_eq!(serial, concurrent);
    }

    /// A runner claiming no width still visits every element, because the
    /// split asks for a piece even when nothing can run in parallel.
    #[test]
    fn a_threaded_runner_of_no_width_still_visits_every_element() {
        let mut items = [0u32; 8];
        for_each(&Threaded::new(0), &mut items, &|item| *item += 1);
        assert!(items.iter().all(|&count| count == 1));
    }

    #[test]
    fn an_empty_slice_visits_nothing_and_one_element_is_visited_on_the_caller() {
        let mut nothing: [u32; 0] = [];
        for_each(&Reversed::new(4), &mut nothing, &|_| {
            panic!("no element to visit")
        });

        let mut one = [7u32];
        for_each(&Reversed::new(4), &mut one, &|item| *item += 1);
        assert_eq!(one, [8]);
    }

    /// A runner that skips a piece leaves that element alone. It is a defect in
    /// the runner, and the point here is that it is not memory-unsafe: the test
    /// runs clean under Miri-style aliasing rules because no borrow escapes.
    #[test]
    fn a_runner_that_skips_a_piece_leaves_that_element_unvisited() {
        let mut items = [0u32; 4];
        for_each(&Forgetful, &mut items, &|item| *item += 1);
        assert_eq!(items, [1, 1, 1, 0]);
    }

    /// A drawn piece is visited exactly once whoever draws it, and whichever
    /// order the runner draws in, the pieces write what running them in turn
    /// wrote.
    #[test]
    fn every_drawn_piece_is_visited_once_by_any_runner() {
        let stamp = |piece: &mut [u64]| {
            for slot in piece {
                *slot = slot.wrapping_mul(3).wrapping_add(0x9E37_79B9);
            }
        };
        let mut serial = [1u64; 257];
        for_each_drawn(&SERIAL, serial.chunks_mut(16), &stamp);
        let runners: [&dyn JobRunner; 3] =
            [&Reversed::new(4), &Threaded::new(4), &Threaded::new(0)];
        for runner in runners {
            let mut drawn = [1u64; 257];
            for_each_drawn(runner, drawn.chunks_mut(16), &stamp);
            assert_eq!(serial, drawn);
        }
    }

    /// Pieces of differing lengths, the case a list of `for_each` elements
    /// would have had to be gathered for.
    #[test]
    fn a_fold_over_drawn_pieces_joins_every_answer_once() {
        fn split(values: &mut [u32]) -> [&mut [u32]; 5] {
            let mut rest = values;
            [1usize, 7, 0, 20, 36].map(|len| {
                let (part, after) = core::mem::take(&mut rest).split_at_mut(len);
                rest = after;
                part
            })
        }
        let mut values: [u32; 64] = core::array::from_fn(|at| u32::try_from(at).unwrap_or(0));
        let total = |runner: &dyn JobRunner, values: &mut [u32]| {
            fold_drawn(
                runner,
                split(values).into_iter(),
                0u64,
                &|part| {
                    for value in part.iter_mut() {
                        *value += 1;
                    }
                    part.iter().map(|&value| u64::from(value)).sum()
                },
                &|a, b| a + b,
            )
        };
        let mut drawn = values;
        assert_eq!(total(&SERIAL, &mut values), (1..=64).sum::<u64>());
        assert_eq!(total(&Threaded::new(4), &mut drawn), (1..=64).sum::<u64>());
        assert_eq!(values, drawn);
    }

    /// Fewer than two pieces never reach the runner, so a single band costs no
    /// dispatch and no lock.
    #[test]
    fn fewer_than_two_drawn_pieces_are_visited_without_a_dispatch() {
        let runner = Reversed::new(4);
        let mut values = [0u32; 4];
        for_each_drawn(&runner, values.chunks_mut(4), &|piece| piece[0] += 1);
        for_each_drawn(&runner, values.chunks_mut(4).take(0), &|_| {
            panic!("no piece")
        });
        assert_eq!(runner.dispatches(), 0);
        for_each_drawn(&runner, values.chunks_mut(2), &|piece| piece[1] += 1);
        assert_eq!(runner.dispatches(), 1);
        assert_eq!(values, [1, 1, 0, 1]);
    }

    /// The order-independence proof every drawn pass leans on is only a proof
    /// if `Reversed` really reverses drawn work: piece `0` must be visited last.
    #[test]
    fn a_reversed_runner_visits_drawn_pieces_last_first() {
        let visits = core::sync::atomic::AtomicU32::new(0);
        let mut order = [u32::MAX; 5];
        for_each_drawn(&Reversed::new(4), order.chunks_mut(1), &|piece| {
            piece[0] = visits.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        });
        assert_eq!(order, [4, 3, 2, 1, 0]);
    }

    /// More pieces than one batch holds are handed out a batch at a time, and
    /// every one is still visited exactly once.
    #[test]
    fn drawn_pieces_past_one_batch_are_each_visited_once() {
        let runner = Reversed::new(4);
        let mut counts = [0u32; 3 * DRAWN_BATCH + 5];
        for_each_drawn(&runner, counts.chunks_mut(1), &|piece| piece[0] += 1);
        assert!(counts.iter().all(|&count| count == 1));
        assert_eq!(runner.dispatches(), 4);
        assert_eq!(runner.widest(), DRAWN_BATCH);
        let mut threaded = [0u32; 3 * DRAWN_BATCH + 5];
        for_each_drawn(&Threaded::new(4), threaded.chunks_mut(1), &|piece| {
            piece[0] += 1;
        });
        assert_eq!(counts, threaded);
    }

    /// A runner wider than a stack round holds is still handed four pieces for
    /// each participant a round, so no round leaves its stragglers unabsorbed,
    /// and every piece is visited once with the answers joined in order.
    #[test]
    fn a_wide_runner_is_handed_a_whole_round_for_every_participant() {
        let width = 2 * DRAWN_BATCH / OVERSUBSCRIPTION;
        let round = width * OVERSUBSCRIPTION;
        let runner = Reversed::new(width);
        let mut counts = alloc::vec![0u32; 2 * round + 7];
        for_each_drawn(&runner, counts.chunks_mut(1), &|piece| piece[0] += 1);
        assert!(counts.iter().all(|&count| count == 1));
        assert_eq!(runner.dispatches(), 3);
        assert_eq!(runner.widest(), round);
        let sum = |runner: &dyn JobRunner| {
            let mut values: alloc::vec::Vec<u64> = (0..round as u64 + 3).collect();
            fold_drawn(
                runner,
                values.chunks_mut(1),
                1u64,
                &|piece| piece[0],
                &|joined, next| joined.wrapping_mul(31).wrapping_add(next),
            )
        };
        assert_eq!(sum(&Reversed::new(width)), sum(&SERIAL));
    }

    /// The length an iterator reports is never what decides which pieces are
    /// visited: one that under-reports still has every piece visited.
    #[test]
    fn an_iterator_that_misreports_its_length_still_has_every_piece_visited() {
        /// Claims two pieces, whatever it holds.
        struct Understated<I>(I);
        impl<I: Iterator> Iterator for Understated<I> {
            type Item = I::Item;
            fn next(&mut self) -> Option<I::Item> {
                self.0.next()
            }
            fn size_hint(&self) -> (usize, Option<usize>) {
                (2, Some(2))
            }
        }
        let runners: [&dyn JobRunner; 3] = [&SERIAL, &Reversed::new(4), &Threaded::new(4)];
        for runner in runners {
            let mut counts = [0u32; 9];
            for_each_drawn(runner, Understated(counts.chunks_mut(1)), &|piece| {
                piece[0] += 1;
            });
            assert_eq!(counts, [1; 9]);
        }
    }

    /// The answers are joined in the order the pieces were drawn, so a join
    /// that cares about order — a floating-point sum does — gives the serial
    /// result on every runner.
    #[test]
    fn a_fold_joins_in_the_order_the_pieces_were_drawn() {
        let fold = |runner: &dyn JobRunner| {
            let mut values: [u64; 150] = core::array::from_fn(|at| at as u64);
            fold_drawn(
                runner,
                values.chunks_mut(1),
                7u64,
                &|piece| piece[0],
                &|joined, next| joined.wrapping_mul(31).wrapping_add(next),
            )
        };
        let serial = fold(&SERIAL);
        assert_eq!(fold(&Reversed::new(4)), serial);
        assert_eq!(fold(&Threaded::new(4)), serial);
    }

    /// A runner that skips an index leaves a piece undrawn — a defect in the
    /// runner, and not memory-unsafe: nothing of it is ever handed out.
    #[test]
    fn a_runner_that_skips_a_draw_leaves_that_piece_unvisited() {
        let mut values = [0u32; 4];
        for_each_drawn(&Forgetful, values.chunks_mut(1), &|piece| piece[0] += 1);
        assert_eq!(values, [1, 1, 1, 0]);
    }

    #[test]
    fn no_work_is_split_into_no_pieces() {
        assert_eq!(bands(&Reversed::new(8), 0, 1), 0);
        assert_eq!(bands(&SERIAL, 0, 1), 0);
    }

    #[test]
    fn a_one_thread_wide_runner_is_always_asked_for_one_piece() {
        assert_eq!(bands(&SERIAL, 1_000_000, 1), 1);
    }

    #[test]
    fn work_below_one_grain_is_not_split() {
        assert_eq!(bands(&Reversed::new(8), 15, 16), 1);
        assert_eq!(bands(&Reversed::new(8), 16, 16), 1);
        assert_eq!(bands(&Reversed::new(8), 31, 16), 1);
        assert_eq!(bands(&Reversed::new(8), 32, 16), 2);
    }

    #[test]
    fn the_split_never_exceeds_the_runner_s_oversubscribed_width() {
        let runner = Reversed::new(4);
        assert_eq!(
            bands(&runner, 1_000_000, 1),
            4 * OVERSUBSCRIPTION,
            "however much work there is, the pieces are bounded by the runner"
        );
    }

    /// A zero grain would divide by zero; it reads as one unit per piece.
    #[test]
    fn a_zero_grain_reads_as_one_unit_per_piece() {
        assert_eq!(bands(&Reversed::new(2), 3, 0), 3);
    }

    #[test]
    fn a_piece_takes_its_share_rounded_up_and_never_nothing() {
        assert_eq!(piece_len(10, 3), 4);
        assert_eq!(piece_len(9, 3), 3);
        assert_eq!(piece_len(0, 3), 1);
        assert_eq!(piece_len(5, 0), 5);
        assert_eq!(piece_len(usize::MAX, 1), usize::MAX);
    }

    /// A runner claiming an absurd width must not overflow the bound.
    #[test]
    fn an_absurd_width_still_yields_a_usable_split() {
        assert_eq!(bands(&Reversed::new(usize::MAX), 10, 1), 10);
    }
}
