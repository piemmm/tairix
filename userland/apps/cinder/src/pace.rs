//! When the frame loop next wakes, and how far a wake advances him.
//!
//! A frame while Cinder moves or owes the desktop his first picture; while he
//! is still, only his next reconsideration, so a nap costs one wake a few
//! seconds rather than one a frame — and still ends.

use tairix_theme::motion::seconds;

use crate::mind::RECONSIDER_SECONDS;

/// The frames one wake may advance him by while he moves: a stall longer than
/// that is taken as that, so he never crosses the desktop in one step.
const LONGEST_MOVING_FRAMES: u64 = 4;

/// What the loop's next wake depends on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pace {
    /// Whether anything about him moves from frame to frame.
    pub moving: bool,
    /// Whether he is on the desktop and has not been shown there yet.
    pub unshown: bool,
    /// Seconds until his mind reconsiders what he is doing.
    pub reconsider_in: f64,
}

impl Pace {
    /// The monotonic instant to wake at, `now_ns` being now and `frame_ns`
    /// the desktop's frame period.
    #[must_use]
    pub fn wake_at(self, now_ns: u64, frame_ns: u64) -> u64 {
        let wait = if self.moving || self.unshown {
            frame_ns
        } else {
            nanos(self.reconsider_in).max(frame_ns)
        };
        now_ns.saturating_add(wait)
    }

    /// The seconds a wake `since_ns` after the last advances him by: what
    /// passed, but no more than a few frames while he moves, or than one
    /// reconsideration while he is still.
    #[must_use]
    pub fn step(self, since_ns: u64, frame_ns: u64) -> f64 {
        let longest = if self.moving {
            seconds(frame_ns.saturating_mul(LONGEST_MOVING_FRAMES))
        } else {
            RECONSIDER_SECONDS
        };
        seconds(since_ns).min(longest)
    }
}

/// `whole` seconds, held to one reconsideration, as nanoseconds.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to a few seconds, which a u64 of nanoseconds holds exactly"
)]
fn nanos(whole: f64) -> u64 {
    (whole.clamp(0.0, RECONSIDER_SECONDS) * 1e9) as u64
}

#[cfg(test)]
#[path = "pace_tests.rs"]
mod tests;
