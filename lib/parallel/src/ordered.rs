//! Pieces that may wait on the pieces before them: a wavefront's rows, each
//! waiting on what the row above has published.
//!
//! [`for_each_ordered`] begins the pieces in order, and a [`Published`] is a
//! value one piece writes once and a later piece parks on until it is written.
//! Together they cannot deadlock: a piece is taken only by a lane that then
//! works it, so every piece before one under way is itself under way or done,
//! and the earliest piece still running waits on nothing.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU32, Ordering};

use tairix_rt::sync::Mutex;

use crate::{for_each, JobRunner};

/// Visit every piece `pieces` yields, numbered in the order it yields them,
/// across `runner`: each of `lanes` takes the next piece whenever it finishes
/// one, so the pieces are begun in order, and each visit is handed its lane's
/// own state.
///
/// A visit may wait on what a piece before its own publishes through a
/// [`Published`], however many of the lanes `runner` runs at once. A runner
/// one thread wide works every piece on one lane, in order, so such a wait
/// never blocks it.
pub fn for_each_ordered<L, I>(
    runner: &dyn JobRunner,
    lanes: &mut [L],
    pieces: I,
    visit: &(dyn Fn(&mut L, usize, I::Item) + Sync),
) where
    L: Send,
    I: Iterator + Send,
    I::Item: Send,
{
    let pieces = Mutex::new(pieces.enumerate());
    for_each(runner, lanes, &|lane| loop {
        // The guard goes with the statement, so no visit holds it.
        let Some((number, piece)) = pieces.lock().next() else {
            return;
        };
        visit(lane, number, piece);
    });
}

/// A slot's state: nothing in it yet.
const EMPTY: u32 = 0;
/// A publisher holds it and is writing its value.
const WRITING: u32 = 1;
/// Its value is written.
const SET: u32 = 2;
/// The bit saying a thread sleeps on the slot.
const WAITING: u32 = 1 << 31;

/// A slot's state word, and how a thread sleeps on it until it changes: what
/// [`claim`], [`settle`] and [`await_set`] work over. [`Published`]'s is a
/// futex; the model checker's is the other.
pub trait SlotWord {
    /// The word, acquiring what was released with it.
    fn load(&self) -> u32;

    /// Store `value`, releasing what was written before, and answer what the
    /// word held.
    fn swap(&self, value: u32) -> u32;

    /// Store `new` where the word holds `current`, answering what it held
    /// either way, acquired.
    fn compare_exchange(&self, current: u32, new: u32) -> Result<u32, u32>;

    /// Sleep until woken, unless the word no longer holds `expected`. A wake
    /// may be spurious.
    fn sleep(&self, expected: u32);

    /// Wake every thread sleeping on the word.
    fn wake(&self);
}

/// Take the empty slot `word` for writing, answering whether this call took
/// it: none but the one that does may write its value.
pub fn claim(word: &impl SlotWord) -> bool {
    let mut seen = word.load();
    while seen & !WAITING == EMPTY {
        match word.compare_exchange(seen, WRITING | (seen & WAITING)) {
            Ok(_) => return true,
            Err(now) => seen = now,
        }
    }
    false
}

/// Mark the slot `word`, claimed, written, waking whoever waits for it.
pub fn settle(word: &impl SlotWord) {
    if word.swap(SET) & WAITING != 0 {
        word.wake();
    }
}

/// Wait until the slot `word` is written, acquiring what its writer wrote.
pub fn await_set(word: &impl SlotWord) {
    let mut seen = word.load();
    while seen & !WAITING != SET {
        if seen & WAITING == 0 {
            // Announced before sleeping, so the settle after it wakes the sleep.
            match word.compare_exchange(seen, seen | WAITING) {
                Ok(_) => seen |= WAITING,
                Err(now) => {
                    seen = now;
                    continue;
                }
            }
        }
        word.sleep(seen);
        seen = word.load();
    }
}

/// A value one thread writes once and others wait for: a reader parks until it
/// is written, and the write wakes it only when one does.
pub struct Published<T> {
    state: AtomicU32,
    value: UnsafeCell<MaybeUninit<T>>,
}

// SAFETY: a `Published` hands its value to the threads that read it, so it may
// cross threads when the value may.
unsafe impl<T: Send> Send for Published<T> {}
// SAFETY: the value is written once, by the one publisher whose claim moved the
// state out of `EMPTY`, before the release that marks it `SET`; it is only read
// after an acquire that saw `SET`, and only through shared borrows, so a shared
// `Published` hands out shared borrows of a value that is never written again.
unsafe impl<T: Send + Sync> Sync for Published<T> {}

/// A slot's state word as the futex calls take it.
struct Futex<'a>(&'a AtomicU32);

impl SlotWord for Futex<'_> {
    fn load(&self) -> u32 {
        self.0.load(Ordering::Acquire)
    }

    fn swap(&self, value: u32) -> u32 {
        self.0.swap(value, Ordering::Release)
    }

    fn compare_exchange(&self, current: u32, new: u32) -> Result<u32, u32> {
        self.0
            .compare_exchange(current, new, Ordering::Acquire, Ordering::Acquire)
    }

    fn sleep(&self, expected: u32) {
        crate::pool::wait(self.0, expected);
    }

    fn wake(&self) {
        crate::pool::wake(self.0, u32::MAX);
    }
}

impl<T> Published<T> {
    /// A slot with nothing in it.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(EMPTY),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Write `value`, waking whoever waits for it; answered back where the
    /// slot already holds one or is being written.
    ///
    /// # Errors
    ///
    /// `value` itself, unwritten, where the slot was not empty.
    pub fn publish(&self, value: T) -> Result<(), T> {
        let word = Futex(&self.state);
        if !claim(&word) {
            return Err(value);
        }
        // SAFETY: the claim moved the state out of `EMPTY`, which no other
        // claim can, and no reader reads the value before the settle below
        // marks it `SET`, so this write is the only access there is.
        unsafe { (*self.value.get()).write(value) };
        settle(&word);
        Ok(())
    }

    /// The value, once it is written: the caller parks until it is.
    pub fn wait(&self) -> &T {
        await_set(&Futex(&self.state));
        // SAFETY: `await_set` returned on an acquire that saw `SET`, which the
        // publisher released only after writing the value, so the value is
        // initialised, and it is never written again while `self` is shared.
        unsafe { (*self.value.get()).assume_init_ref() }
    }

    /// The value, if it is written yet.
    #[must_use]
    pub fn get(&self) -> Option<&T> {
        if self.state.load(Ordering::Acquire) & !WAITING != SET {
            return None;
        }
        // SAFETY: as `wait`: the acquire saw `SET`.
        Some(unsafe { (*self.value.get()).assume_init_ref() })
    }

    /// Empty the slot again, answering what it held: exclusive, so nobody
    /// reads or writes it meanwhile.
    pub fn reset(&mut self) -> Option<T> {
        let state = core::mem::replace(self.state.get_mut(), EMPTY);
        if state & !WAITING != SET {
            return None;
        }
        // SAFETY: the state was `SET`, so the value is initialised; it is now
        // `EMPTY`, so it is never read again until a publish writes it anew.
        Some(unsafe { self.value.get_mut().assume_init_read() })
    }
}

impl<T> Default for Published<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Drop for Published<T> {
    fn drop(&mut self) {
        drop(self.reset());
    }
}

impl<T: core::fmt::Debug> core::fmt::Debug for Published<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("Published").field(&self.get()).finish()
    }
}

#[cfg(test)]
#[path = "ordered_tests.rs"]
mod tests;
