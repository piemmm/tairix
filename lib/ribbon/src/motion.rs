//! The ribbon's own clock: when it next moves, and how far a frame moves it.

use tairix_theme::Timeline;

/// How often the ribbon draws while it moves: every other frame the display
/// would, since nothing in it moves fast enough for the frames between to
/// show.
pub const FRAME_NS: u64 = 2 * Timeline::FRAME_NS;

/// The most frames one step carries the ribbon, however late it came: a late
/// wake moves it a few frames on rather than all the way to the clock.
const MOST_FRAMES: u64 = 4;

/// Where the ribbon stands in its own time, and when it moves next.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Motion {
    /// When it last moved, and the seconds it had moved for by then; `None`
    /// while it holds still.
    moving: Option<(u64, f64)>,
    /// When its next frame is due.
    next_ns: u64,
}

impl Motion {
    /// A ribbon setting off at `now_ns`, or holding still when `still`.
    #[must_use]
    pub fn new(now_ns: u64, still: bool) -> Self {
        Self {
            moving: (!still).then_some((now_ns, 0.0)),
            next_ns: now_ns.saturating_add(FRAME_NS),
        }
    }

    /// When the next frame is due, or `None` while the ribbon holds still.
    #[must_use]
    pub fn due_ns(&self) -> Option<u64> {
        self.moving.map(|_| self.next_ns)
    }

    /// Whether a frame is due at `now_ns`.
    #[must_use]
    pub fn frame_due(&self, now_ns: u64) -> bool {
        self.moving.is_some() && now_ns >= self.next_ns
    }

    /// Move on to `now_ns`, answering the seconds of motion the ribbon now
    /// stands at, with its next frame due a frame later. A ribbon holding
    /// still stays at the start of its time.
    pub fn advance(&mut self, now_ns: u64) -> f64 {
        let Some((last_ns, moved_for)) = self.moving.as_mut() else {
            return 0.0;
        };
        let step = now_ns.saturating_sub(*last_ns).min(MOST_FRAMES * FRAME_NS);
        *last_ns = now_ns;
        *moved_for += seconds(step);
        self.next_ns = now_ns.saturating_add(FRAME_NS);
        *moved_for
    }
}

/// `whole` nanoseconds as seconds.
#[allow(
    clippy::cast_precision_loss,
    reason = "a monotonic span; microsecond precision is ample for motion"
)]
pub(crate) fn seconds(whole: u64) -> f64 {
    whole as f64 / 1e9
}

#[cfg(test)]
#[path = "motion_tests.rs"]
mod tests;
