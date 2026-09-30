//! Shake to find the pointer: moved quickly back and forth, the pointer grows
//! for a moment so it can be found at a glance, and settles back once it
//! rests.
//!
//! A shake is a rhythm of quick strokes across, each reversing the last:
//! [`SHAKE_STROKES`] in a row, each at least [`STROKE_MIN_PX`] wide, done
//! within [`STROKE_MAX_NS`], and more across than down. A stroke too short,
//! too slow or too steep breaks the rhythm, so an ordinary movement however
//! fast never counts, and neither does the slower back-and-forth of aiming at
//! something.

use tairix_geometry::{Point, Scale};
use tairix_theme::{MotionInteraction, MotionTheme, Timeline};
use tairix_wm::FULLY_ENLARGED;

/// The least a stroke covers across, in logical pixels.
const STROKE_MIN_PX: u32 = 40;

/// The longest one stroke may take.
const STROKE_MAX_NS: u64 = 220_000_000;

/// Quick strokes in a row that make a shake: over there, back, and over
/// there again.
const SHAKE_STROKES: u8 = 3;

/// How long the pointer stays grown after the stroke that last kept it so,
/// before it settles back.
const HOLD_NS: u64 = 450_000_000;

/// One direction of travel across, as it has gone so far.
#[derive(Copy, Clone, Debug)]
struct Run {
    rightward: bool,
    began_ns: u64,
    across: u32,
    down: u32,
}

/// The pointer's size moving from one level to another over a timeline.
#[derive(Copy, Clone, Debug)]
struct Ramp {
    timeline: Timeline,
    from: u16,
    to: u16,
}

impl Ramp {
    const AT_REST: Self = Self {
        timeline: Timeline::SETTLED,
        from: 0,
        to: 0,
    };

    /// How far grown the pointer is at `now_ns`, in permille.
    fn level(self, now_ns: u64) -> u16 {
        if !self.timeline.running() {
            return self.to;
        }
        let eased = i32::from(self.timeline.eased(now_ns));
        let span = i32::from(self.to) - i32::from(self.from);
        let level = i32::from(self.from) + span * eased / i32::from(u8::MAX);
        u16::try_from(level.clamp(0, i32::from(FULLY_ENLARGED))).unwrap_or(self.to)
    }
}

/// The shake detector, and how far grown the pointer is.
#[derive(Clone, Debug)]
pub struct Shake {
    /// The last position seen and when.
    last: Option<(u64, Point)>,
    run: Option<Run>,
    strokes: u8,
    /// When a stroke last kept the pointer grown.
    shaken_ns: Option<u64>,
    ramp: Ramp,
}

impl Default for Shake {
    fn default() -> Self {
        Self::new()
    }
}

impl Shake {
    /// A detector that has seen nothing, and a pointer at its own size.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last: None,
            run: None,
            strokes: 0,
            shaken_ns: None,
            ramp: Ramp::AT_REST,
        }
    }

    /// Take in that the pointer is at `at` at `now_ns`, on an output at
    /// `scale`.
    pub fn observe(&mut self, now_ns: u64, at: Point, scale: Scale) {
        let Some((then, from)) = self.last.replace((now_ns, at)) else {
            return;
        };
        let dx = i64::from(at.x) - i64::from(from.x);
        let dy = i64::from(at.y) - i64::from(from.y);
        let across = u32::try_from(dx.unsigned_abs()).unwrap_or(u32::MAX);
        let down = u32::try_from(dy.unsigned_abs()).unwrap_or(u32::MAX);
        if dx == 0 {
            if let Some(run) = &mut self.run {
                run.down = run.down.saturating_add(down);
            }
            return;
        }
        let rightward = dx > 0;
        if let Some(run) = self.run.as_mut().filter(|run| run.rightward == rightward) {
            run.across = run.across.saturating_add(across);
            run.down = run.down.saturating_add(down);
            return;
        }
        if let Some(ended) = self.run {
            let quick = then.saturating_sub(ended.began_ns) <= STROKE_MAX_NS;
            let wide = ended.across >= scale.scale_length(STROKE_MIN_PX);
            let level = ended.down <= ended.across;
            self.strokes = if quick && wide && level {
                self.strokes.saturating_add(1)
            } else {
                0
            };
            if self.strokes >= SHAKE_STROKES {
                self.shaken_ns = Some(then);
            }
        }
        self.run = Some(Run {
            rightward,
            began_ns: then,
            across,
            down,
        });
    }

    /// How far grown the pointer is at `now_ns`, in permille of
    /// [`FULLY_ENLARGED`], moving between its sizes over `motion`'s timings.
    pub fn level(&mut self, now_ns: u64, motion: MotionTheme) -> u16 {
        let target = if self.held(now_ns) { FULLY_ENLARGED } else { 0 };
        if target != self.ramp.to {
            let interaction = if target == 0 {
                MotionInteraction::PointerRestore
            } else {
                MotionInteraction::PointerEnlarge
            };
            self.ramp = Ramp {
                timeline: Timeline::start(now_ns, motion.duration(interaction)),
                from: self.ramp.level(now_ns),
                to: target,
            };
        }
        let level = self.ramp.level(now_ns);
        if self.ramp.timeline.finished(now_ns) {
            self.ramp.timeline.settle();
        }
        level
    }

    /// Whether a shake still holds the pointer grown at `now_ns`.
    fn held(&self, now_ns: u64) -> bool {
        self.shaken_ns
            .is_some_and(|at| now_ns.saturating_sub(at) < HOLD_NS)
    }

    /// Nanoseconds until the pointer's size next changes, or `None` while it
    /// rests at its own size.
    #[must_use]
    pub fn next_frame_in(&self, now_ns: u64) -> Option<u64> {
        if let Some(next) = self.ramp.timeline.next_frame_in(now_ns) {
            return Some(next);
        }
        if self.ramp.to != FULLY_ENLARGED {
            return None;
        }
        // Grown and holding: the next change is the hold running out.
        let at = self.shaken_ns?;
        Some(at.saturating_add(HOLD_NS).saturating_sub(now_ns))
    }

    /// Forget everything seen and put the pointer back at its own size at
    /// once.
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
#[path = "shake_tests.rs"]
mod tests;
