//! When the login screen next has something to do.
//!
//! The event loop parks on the seat's input and wakes on a deadline rather
//! than polling. Without an input event the clock reaching the next minute
//! and a lockout counting down repaint, and a screen left untouched long
//! enough puts its display to sleep; this module times those three.

use tairix_abi::driver::display::RefreshRate;
use tairix_abi::time::Duration64;
use tairix_abi::WAITSET_TIMEOUT_NONE;
use tairix_greeter::Cooldown;

/// Nanoseconds in one second.
const NANOS_PER_SEC: u64 = tairix_abi::time::NANOS_PER_SEC as u64;

/// Milliseconds in one second, the unit a theme authors a duration in.
const MILLIS_PER_SEC: u64 = 1_000;

/// How long the login screen waits without input before it puts its display
/// to sleep.
pub const ENERGY_SAVING_AFTER_NS: u64 = 30 * 60 * NANOS_PER_SEC;

/// When the login screen last saw input, and so when it puts its display to
/// sleep.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Idle {
    since_ns: u64,
}

impl Idle {
    /// Idle from `now_ns`.
    #[must_use]
    pub const fn new(now_ns: u64) -> Self {
        Self { since_ns: now_ns }
    }

    /// Input arrived at `now_ns`, so the wait starts again.
    pub fn input(&mut self, now_ns: u64) {
        self.since_ns = now_ns;
    }

    /// Nanoseconds from `now_ns` until the display is owed its sleep; zero
    /// once it is.
    #[must_use]
    pub const fn timeout(&self, now_ns: u64) -> u64 {
        self.since_ns
            .saturating_add(ENERGY_SAVING_AFTER_NS)
            .saturating_sub(now_ns)
    }

    /// Whether the screen has been left alone long enough at `now_ns` for
    /// its display to sleep.
    #[must_use]
    pub const fn is_due(&self, now_ns: u64) -> bool {
        self.timeout(now_ns) == 0
    }
}

/// The relative nanosecond timeout for the next park before the clock or a
/// lockout needs a repaint.
///
/// `clock` is the nanoseconds until the clock next changes, or `None` where
/// it shows nothing that will: no trusted wall time is held, or the range
/// holds no later minute. [`WAITSET_TIMEOUT_NONE`] means nothing either draws
/// changes until an input event arrives.
#[must_use]
pub fn park_timeout(clock: Option<u64>, cooldown_remaining: Duration64) -> u64 {
    match (clock, Cooldown::tick_of(cooldown_remaining)) {
        (Some(clock), Some(tick)) => clock.min(tick),
        (Some(only), None) | (None, Some(only)) => only,
        (None, None) => WAITSET_TIMEOUT_NONE,
    }
}

/// The most frames an animation of `duration_ms` can ever need on a display
/// refreshing at `refresh`.
///
/// What bounds a loop that presents an animation to its end: a clock that
/// stopped, or a seat that reads ready forever, cannot then hold a finished
/// login screen on the display. One more than the span divides into, so the
/// frame that lands exactly on the end is still drawn.
#[must_use]
pub fn frame_budget(duration_ms: u16, refresh: RefreshRate) -> u32 {
    let span = u64::from(duration_ms).saturating_mul(NANOS_PER_SEC / MILLIS_PER_SEC);
    u32::try_from(span / refresh.frame_ns())
        .unwrap_or(u32::MAX)
        .saturating_add(1)
}

#[cfg(test)]
#[path = "wait_tests.rs"]
mod tests;
