//! How far the machine runs next and when: just ahead of the audio device
//! while its sound is heard, by the clock while it is not, and as fast as the
//! worker runs it when asked to or while a tape loads fast.

use crate::machine::CLOCK;
use crate::worker::protocol::{MAX_SLICE, SAMPLE_RATE};

/// How fast the machine runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Speed {
    /// A BBC Micro's own speed, its sound heard.
    #[default]
    Normal,
    /// Twice that, silent.
    Double,
    /// As fast as the worker can run it, silent.
    Fastest,
}

/// Machine time a clock-paced slice runs: 20 ms.
pub const CLOCK_SLICE: u32 = 40_000;
/// Machine time an unpaced slice runs: 100 ms, so each round trip carries
/// enough to be worth it.
pub const FAST_SLICE: u32 = 200_000;
/// The least sound a slice tops the audio ring up by: 5 ms of it, so a ring
/// drained a few frames at a time is not refilled a few frames at a time.
const LEAST_FRAMES: u32 = 240;
/// How far behind the clock the machine may fall before it gives the time up
/// rather than racing to make it back.
const MOST_BEHIND_NS: u64 = 100_000_000;
const NANOS: u64 = 1_000_000_000;

/// The audio stream's ring, as the pacer is told of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ring {
    /// Frames written and not yet played.
    pub queued: u32,
    /// Frames to keep it at.
    pub target: u32,
}

/// What to do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Run a slice of `cycles`, its sound kept if `sound`.
    Run {
        /// The cycles to run.
        cycles: u32,
        /// Whether its sound is to be heard.
        sound: bool,
    },
    /// Run nothing before this monotonic instant, in nanoseconds.
    At(u64),
    /// Run nothing until the audio device plays some of its ring, or the
    /// machine is resumed.
    Wait,
}

/// Where the clock's pace was taken from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Anchor {
    at_ns: u64,
    cycles: u64,
    rate: u64,
}

/// The machine's pace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pacer {
    speed: Speed,
    paused: bool,
    anchor: Option<Anchor>,
}

impl Pacer {
    /// A machine running at its own speed.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            speed: Speed::Normal,
            paused: false,
            anchor: None,
        }
    }

    /// How fast it runs.
    #[must_use]
    pub const fn speed(&self) -> Speed {
        self.speed
    }

    /// Whether it is paused.
    #[must_use]
    pub const fn paused(&self) -> bool {
        self.paused
    }

    /// Run at `speed` from now on.
    pub fn set_speed(&mut self, speed: Speed) {
        self.speed = speed;
        self.anchor = None;
    }

    /// Pause, or resume from now.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        self.anchor = None;
    }

    /// What to run at `now_ns`, with the audio ring as `ring` says while the
    /// sound has a stream, `hurried` while a tape loads fast. Called only when
    /// a slice can be asked for: an answered `Run` is taken as asked.
    pub fn next(&mut self, now_ns: u64, ring: Option<Ring>, hurried: bool) -> Next {
        if self.paused {
            return Next::Wait;
        }
        if self.speed == Speed::Fastest || hurried {
            self.anchor = None;
            return Next::Run {
                cycles: FAST_SLICE,
                sound: false,
            };
        }
        if let (Speed::Normal, Some(ring)) = (self.speed, ring) {
            self.anchor = None;
            let short = ring.target.saturating_sub(ring.queued);
            return if short > 0 && short >= LEAST_FRAMES.min(ring.target) {
                Next::Run {
                    cycles: cycles_for(short),
                    sound: true,
                }
            } else {
                Next::Wait
            };
        }
        let rate = if self.speed == Speed::Double { 2 } else { 1 };
        let fresh = Anchor {
            at_ns: now_ns,
            cycles: 0,
            rate,
        };
        let anchor = self
            .anchor
            .filter(|anchor| anchor.rate == rate)
            .unwrap_or(fresh);
        let due = anchor
            .at_ns
            .saturating_add(anchor.cycles.saturating_mul(NANOS) / (CLOCK * rate));
        if now_ns < due {
            self.anchor = Some(anchor);
            return Next::At(due);
        }
        let anchor = if now_ns - due > MOST_BEHIND_NS {
            fresh
        } else {
            anchor
        };
        // Whole seconds of machine time move into the anchor's instant, so
        // the cycles counted from it stay too few to overflow in nanoseconds.
        let cycles = anchor.cycles + u64::from(CLOCK_SLICE);
        let second = CLOCK * rate;
        self.anchor = Some(Anchor {
            at_ns: anchor.at_ns + cycles / second * NANOS,
            cycles: cycles % second,
            rate,
        });
        Next::Run {
            cycles: CLOCK_SLICE,
            sound: false,
        }
    }
}

/// The cycles that make `frames` frames of sound, at most a slice's.
#[must_use]
pub fn cycles_for(frames: u32) -> u32 {
    let cycles = (u64::from(frames) * CLOCK).div_ceil(u64::from(SAMPLE_RATE));
    u32::try_from(cycles).map_or(MAX_SLICE, |cycles| cycles.min(MAX_SLICE))
}

#[cfg(test)]
#[path = "pacing_tests.rs"]
mod tests;
