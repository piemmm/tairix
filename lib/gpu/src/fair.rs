//! A lock whose lockers have it in turn, in the order they asked.
//!
//! `lib/rt`'s mutex lets a thread that locks again at once win against one it
//! has just woken, so a thread looping on a lock can keep another out for as
//! long as it loops. A memory's words and the device's model are locked by the
//! CPU's threads and by the work alike, each for as long as its step takes, so
//! each locker waits its turn instead.

use core::ops::{Deref, DerefMut};

use tairix_abi::Duration64;
use tairix_rt::sync::{Condvar, Mutex, MutexGuard};

/// How many places a lock's waiters park in, each in its turn's: a turn's end
/// wakes the next turn's place alone, so as many as this waiting at once are
/// woken one at a time, and more share each place.
const PLACES: usize = 8;

/// A `T` its lockers have in turn.
pub(crate) struct Fair<T> {
    value: Mutex<T>,
    turns: Mutex<Turns>,
    places: [Condvar; PLACES],
}

/// The order a lock's lockers asked in: the next turn to give, the one being
/// served, and how many are parked in each place.
struct Turns {
    given: usize,
    served: usize,
    parked: [usize; PLACES],
}

impl Turns {
    /// End the turn being served, answering the next turn's place where a
    /// locker is parked in it, to be woken.
    fn end(&mut self) -> Option<usize> {
        self.served = self.served.wrapping_add(1);
        let place = self.served % PLACES;
        (self.parked[place] > 0).then_some(place)
    }
}

/// A [`Fair`]'s value, held in its locker's turn.
pub(crate) struct FairGuard<'a, T> {
    guard: MutexGuard<'a, T>,
    // After the guard, so the value is free before the next turn is served.
    turn: Turn<'a, T>,
}

/// A locker's turn, handed to the next as it ends.
struct Turn<'a, T>(&'a Fair<T>);

impl<T> Turn<'_, T> {
    /// End it under `turns`, which the caller holds, answering the place to
    /// wake.
    fn end_under(self, turns: &mut Turns) -> Option<usize> {
        core::mem::forget(self);
        #[cfg(test)]
        held::let_go();
        turns.end()
    }
}

impl<T> Drop for Turn<'_, T> {
    fn drop(&mut self) {
        #[cfg(test)]
        held::let_go();
        let woken = self.0.turns.lock().end();
        if let Some(place) = woken {
            self.0.wake(place);
        }
    }
}

impl<T> Deref for FairGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T> DerefMut for FairGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<T> Fair<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self {
            value: Mutex::new(value),
            turns: Mutex::new(Turns {
                given: 0,
                served: 0,
                parked: [0; PLACES],
            }),
            places: [const { Condvar::new() }; PLACES],
        }
    }

    /// The value, once every locker that asked before has had it.
    pub(crate) fn lock(&self) -> FairGuard<'_, T> {
        self.take_turn(self.turns.lock())
    }

    /// Wait on `wake` until notified or `timeout` passes, then have the value
    /// again in a turn of its own. `held`'s turn goes to the next locker as it
    /// parks, and `wake` is sampled before any later turn can start, so a
    /// change made in one and then notified is never missed.
    pub(crate) fn wait_timeout<'a>(
        &'a self,
        held: FairGuard<'a, T>,
        wake: &Condvar,
        timeout: Duration64,
    ) -> FairGuard<'a, T> {
        self.park(held, |turns| wake.wait_timeout(turns, timeout).0)
    }

    /// Let `held` go and hand `wait` the turns, its own ended, to park on and
    /// give back; then have the value again in a turn taken under them, so
    /// a woken waiter never takes the value but in a turn.
    fn park<'a>(
        &'a self,
        held: FairGuard<'a, T>,
        wait: impl FnOnce(MutexGuard<'a, Turns>) -> MutexGuard<'a, Turns>,
    ) -> FairGuard<'a, T> {
        let FairGuard { guard, turn } = held;
        let mut turns = self.turns.lock();
        drop(guard);
        if let Some(place) = turn.end_under(&mut turns) {
            self.wake(place);
        }
        self.take_turn(wait(turns))
    }

    /// The value, once every turn given before the one taken under `turns`
    /// has been served.
    fn take_turn<'a>(&'a self, mut turns: MutexGuard<'a, Turns>) -> FairGuard<'a, T> {
        let mine = turns.given;
        turns.given = turns.given.wrapping_add(1);
        if turns.served != mine {
            let place = mine % PLACES;
            turns.parked[place] += 1;
            while turns.served != mine {
                turns = self.places[place].wait(turns);
            }
            turns.parked[place] -= 1;
        }
        drop(turns);
        let guard = self.value.lock();
        #[cfg(test)]
        held::taken();
        FairGuard {
            guard,
            turn: Turn(self),
        }
    }

    fn wake(&self, place: usize) {
        // Every turn sharing the place wakes to look for its own: the kernel
        // wakes a place's waiters in the order they parked, which need not be
        // their turns'.
        self.places[place].notify_all();
    }
}

/// How many fair locks this thread holds, so a test can see that what must
/// go without one is not let go under one.
#[cfg(test)]
pub(crate) mod held {
    use core::cell::Cell;

    std::thread_local! {
        static HELD: Cell<usize> = const { Cell::new(0) };
        static UNDER: Cell<usize> = const { Cell::new(0) };
        static TAKEN: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn taken() {
        HELD.with(|held| held.set(held.get() + 1));
        TAKEN.with(|taken| taken.set(taken.get() + 1));
    }

    /// How many times this thread has taken a fair lock.
    pub(crate) fn takings() -> usize {
        TAKEN.with(Cell::get)
    }

    pub(super) fn let_go() {
        HELD.with(|held| held.set(held.get().saturating_sub(1)));
    }

    /// Note a drop that should come with no fair lock held.
    pub(crate) fn note_drop() {
        if HELD.with(Cell::get) > 0 {
            UNDER.with(|under| under.set(under.get() + 1));
        }
    }

    /// How many noted drops this thread made under one.
    pub(crate) fn drops_under() -> usize {
        UNDER.with(Cell::get)
    }
}

#[cfg(test)]
#[path = "fair_tests.rs"]
mod tests;
