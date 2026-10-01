//! When the login screen next has something to do.
//!
//! The event loop parks on the seat's input and wakes on a deadline rather
//! than polling. Without an input event the clock reaching the next minute
//! and a lockout counting down repaint, and a screen left untouched long
//! enough puts its display to sleep; this module times those three.

use alloc::string::{String, ToString};

use tairix_abi::time::{Duration64, Time64};
use tairix_abi::WAITSET_TIMEOUT_NONE;
use tairix_theme::Timeline;

/// Nanoseconds in one second.
const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Milliseconds in one second, the unit a theme authors a duration in.
const MILLIS_PER_SEC: u64 = 1_000;

/// Seconds in one minute.
pub(crate) const SECS_PER_MINUTE: i64 = 60;

/// How long the login screen waits without input before it puts its display
/// to sleep.
pub const ENERGY_SAVING_AFTER_NS: u64 = 30 * 60 * NANOS_PER_SEC;

/// The authority's lockout on one account, counted down against the monotonic
/// clock.
///
/// The authority meters each login name on its own, so a lockout belongs to
/// the account it was reported for: it is shown only while the surface asks
/// about that account, and a person who picks another is never held behind
/// the first one's wait. The surface presents a remaining span and reads no
/// clock of its own, so the countdown lives here; being monotonic, a
/// wall-clock correction cannot shorten or lengthen it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Cooldown {
    /// When the lockout ends, and the login name it holds back.
    lockout: Option<(u64, String)>,
}

impl Cooldown {
    /// Record the authority's answer about `account`: a lockout of
    /// `retry_after` starting now, or — for a zero or negative span — none.
    ///
    /// A lockout replaces whichever stood before it. An answer with none
    /// clears only that account's own, so a refusal for one account leaves
    /// another's countdown standing.
    pub fn start(&mut self, now_ns: u64, retry_after: Duration64, account: &str) {
        let span = retry_after.saturating_total_nanos();
        if span > 0 {
            self.lockout = Some((now_ns.saturating_add(span), account.to_string()));
        } else if self
            .lockout
            .as_ref()
            .is_some_and(|(_, held)| held == account)
        {
            self.lockout = None;
        }
    }

    /// How much of the lockout on `asking` — the account the surface is
    /// asking about, if any — is left: zero once it has run out, and for
    /// every other account.
    #[must_use]
    pub fn remaining(&self, now_ns: u64, asking: Option<&str>) -> Duration64 {
        match &self.lockout {
            Some((until, held)) if asking == Some(held.as_str()) => {
                Duration64::from_nanos(until.saturating_sub(now_ns))
            }
            _ => Duration64::ZERO,
        }
    }
}

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
/// [`WAITSET_TIMEOUT_NONE`] means neither does: nothing they draw changes until an input
/// event arrives. `now` is `None` when no trusted wall time is held, in which
/// case there is no clock on the backdrop to keep current either.
#[must_use]
pub fn park_timeout(now: Option<Time64>, cooldown_remaining: Duration64) -> u64 {
    let clock = now.map(nanos_to_next_minute);
    let remaining = cooldown_remaining.saturating_total_nanos();
    let tick = (remaining > 0).then(|| remaining.min(NANOS_PER_SEC));
    match (clock, tick) {
        (Some(clock), Some(tick)) => clock.min(tick),
        (Some(only), None) | (None, Some(only)) => only,
        (None, None) => WAITSET_TIMEOUT_NONE,
    }
}

/// The most frames an animation of `duration_ms` can ever need, at the
/// frame cadence every animation shares.
///
/// What bounds a loop that presents an animation to its end: a clock that
/// stopped, or a seat that reads ready forever, cannot then hold a finished
/// login screen on the display. One more than the span divides into, so the
/// frame that lands exactly on the end is still drawn.
#[must_use]
pub fn frame_budget(duration_ms: u16) -> u32 {
    let span = u64::from(duration_ms).saturating_mul(NANOS_PER_SEC / MILLIS_PER_SEC);
    u32::try_from(span / Timeline::FRAME_NS)
        .unwrap_or(u32::MAX)
        .saturating_add(1)
}

/// Nanoseconds from `now` to the next whole minute.
///
/// Never zero: a reading exactly on a minute boundary has just been drawn,
/// so its next repaint is a whole minute away and a zero timeout would spin.
fn nanos_to_next_minute(now: Time64) -> u64 {
    let into_minute = now.secs().rem_euclid(SECS_PER_MINUTE);
    let secs_left = (SECS_PER_MINUTE - into_minute).unsigned_abs();
    secs_left
        .saturating_mul(NANOS_PER_SEC)
        .saturating_sub(u64::from(now.subsec_nanos()))
}

#[cfg(test)]
#[path = "wait_tests.rs"]
mod tests;
