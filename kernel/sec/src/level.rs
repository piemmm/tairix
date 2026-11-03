//! [`ProcessLevel`]: the scheduling level a process's threads run at.
//!
//! A level is the process's, not one thread's: every thread runs at it and a
//! thread created later joins at it. It is two facts — the level the
//! process's principal set, and whether the process holds a seat's display —
//! because whoever owns the screen the user is looking at is served in the
//! strongest band for exactly as long as it owns it, and drops back to the
//! level that was set when it stops.

use core::sync::atomic::{AtomicU8, Ordering};

use tairix_abi::SchedPriority;
use tairix_sync::{SpinLock, SpinLockGuard};

/// The bit of a level's word saying the process holds a seat's display.
const DISPLAY_HELD: u8 = 1 << 2;

/// The bits of a level's word holding the level the principal set.
const SET_LEVEL: u8 = 0b11;

/// The scheduling level a process's threads run at.
#[derive(Debug)]
pub struct ProcessLevel {
    /// Held across a change and the fan-out that brings each thread to it,
    /// so the change made last is the level every thread is left at.
    change: SpinLock<()>,
    word: AtomicU8,
}

impl Default for ProcessLevel {
    fn default() -> Self {
        Self {
            change: SpinLock::new(()),
            word: AtomicU8::new(level_bits(SchedPriority::Normal)),
        }
    }
}

impl ProcessLevel {
    /// The level the process's threads run at.
    #[must_use]
    pub fn effective(&self) -> SchedPriority {
        effective(self.word.load(Ordering::Acquire))
    }

    /// Begin a change: it holds off every other until it is dropped, which
    /// is once each thread has been brought to its outcome.
    pub fn change(&self) -> LevelChange<'_> {
        LevelChange {
            _held: self.change.lock(),
            word: &self.word,
        }
    }
}

/// A change to a [`ProcessLevel`] in progress, serialised against every other.
pub struct LevelChange<'a> {
    _held: SpinLockGuard<'a, ()>,
    word: &'a AtomicU8,
}

impl LevelChange<'_> {
    /// The level the process's principal set, beneath any display it holds.
    #[must_use]
    pub fn set_level(&self) -> SchedPriority {
        set_level(self.word.load(Ordering::Acquire))
    }

    /// The level the process's threads run at.
    #[must_use]
    pub fn effective(&self) -> SchedPriority {
        effective(self.word.load(Ordering::Acquire))
    }

    /// Make `level` the one the process's principal set.
    pub fn set(&self, level: SchedPriority) {
        let held = self.word.load(Ordering::Acquire) & DISPLAY_HELD;
        self.word.store(held | level_bits(level), Ordering::Release);
    }

    /// Note whether the process holds a seat's display.
    pub fn hold_display(&self, held: bool) {
        let level = self.word.load(Ordering::Acquire) & SET_LEVEL;
        let display = if held { DISPLAY_HELD } else { 0 };
        self.word.store(level | display, Ordering::Release);
    }
}

/// `level` as the low bits of a level's word: its discriminant, 1 to 3.
const fn level_bits(level: SchedPriority) -> u8 {
    match level {
        SchedPriority::High => 1,
        SchedPriority::Normal => 2,
        SchedPriority::Low => 3,
    }
}

/// The level the principal set, as `word` records it.
fn set_level(word: u8) -> SchedPriority {
    SchedPriority::from_u32(u32::from(word & SET_LEVEL)).unwrap_or(SchedPriority::Normal)
}

/// The level a process whose level's word is `word` runs at.
fn effective(word: u8) -> SchedPriority {
    if word & DISPLAY_HELD != 0 {
        SchedPriority::High
    } else {
        set_level(word)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_starts_at_the_normal_level_holding_no_display() {
        let level = ProcessLevel::default();
        assert_eq!(level.effective(), SchedPriority::Normal);
        assert_eq!(level.change().set_level(), SchedPriority::Normal);
    }

    #[test]
    fn a_display_raises_the_process_to_the_strongest_band_while_held() {
        let level = ProcessLevel::default();
        level.change().set(SchedPriority::Low);
        level.change().hold_display(true);
        assert_eq!(level.effective(), SchedPriority::High);
        assert_eq!(level.change().set_level(), SchedPriority::Low);
        level.change().hold_display(false);
        assert_eq!(level.effective(), SchedPriority::Low);
    }

    #[test]
    fn a_level_set_while_the_display_is_held_applies_once_it_is_not() {
        let level = ProcessLevel::default();
        level.change().hold_display(true);
        let change = level.change();
        change.set(SchedPriority::Low);
        assert_eq!(change.effective(), SchedPriority::High);
        change.hold_display(false);
        assert_eq!(change.effective(), SchedPriority::Low);
    }

    #[test]
    fn every_level_round_trips_through_the_word() {
        for set in [
            SchedPriority::High,
            SchedPriority::Normal,
            SchedPriority::Low,
        ] {
            for held in [false, true] {
                let level = ProcessLevel::default();
                let change = level.change();
                change.set(set);
                change.hold_display(held);
                assert_eq!(change.set_level(), set);
                let expected = if held { SchedPriority::High } else { set };
                assert_eq!(change.effective(), expected);
            }
        }
    }
}
