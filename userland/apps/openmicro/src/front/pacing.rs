//! How far the machine runs next and when: just ahead of the audio device
//! while its sound is heard, by the clock while it is not, and as fast as the
//! worker runs it when asked to or while a tape or a disc loads fast in
//! silence; and the sound the audio ring had no room for.

use alloc::vec::Vec;

use tairix_abi::Errno;

use crate::front::library::FULL_VOLUME;
use crate::machine::Timing;
use crate::worker::protocol::{MAX_SLICE, SAMPLE_BYTES, SAMPLE_RATE};

/// How fast the machine runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Speed {
    /// The machine's own speed, its sound heard.
    #[default]
    Normal,
    /// Twice that, silent.
    Double,
    /// As fast as the worker can run it, silent.
    Fastest,
}

/// Slices a second of machine time is cut into while the clock paces it:
/// 20 ms each.
const CLOCK_SLICES: u64 = 50;
/// Slices a second is cut into unpaced: 100 ms each, so each round trip
/// carries enough to be worth it.
const FAST_SLICES: u64 = 10;
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

/// When the machine's sound last moved, for fast loading: a load is hurried
/// only after a second of the machine's time without sound, so a loader's
/// music, rests and all, plays as it would.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Quiet {
    heard_at: Option<u64>,
}

impl Quiet {
    /// The slice ending at machine cycle `at` moved the sound, or not.
    pub const fn slice(&mut self, at: u64, audible: bool) {
        if audible {
            self.heard_at = Some(at);
        }
    }

    /// Whether a machine keeping `timing` has been silent for a second at
    /// cycle `now`.
    #[must_use]
    pub fn settled(&self, now: u64, timing: Timing) -> bool {
        self.heard_at
            .is_none_or(|at| now >= at.saturating_add(timing.clock()))
    }
}

/// Sound the audio ring had no room for, played ahead of whatever follows,
/// so that a slice's last frames are never dropped.
#[derive(Debug, Default)]
pub struct Backlog {
    bytes: Vec<u8>,
}

impl Backlog {
    /// Frames waiting for room: queued, as far as the pace is concerned.
    #[must_use]
    pub fn frames(&self) -> u32 {
        u32::try_from(self.bytes.len() / SAMPLE_BYTES).unwrap_or(u32::MAX)
    }

    /// `samples`, signed 16-bit, at `volume` thousandths of their level,
    /// after what waits, offered to `write`, which answers the bytes the ring
    /// took; the rest waits.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] where the backlog cannot hold `samples`, or what
    /// `write` refuses.
    pub fn play(
        &mut self,
        samples: &[u8],
        volume: u16,
        write: impl FnOnce(&[u8]) -> Result<usize, Errno>,
    ) -> Result<(), Errno> {
        self.bytes
            .try_reserve(samples.len())
            .map_err(|_| Errno::OutOfMemory)?;
        let volume = i32::from(volume.min(FULL_VOLUME));
        self.bytes.extend(
            samples
                .as_chunks::<SAMPLE_BYTES>()
                .0
                .iter()
                .flat_map(|&pair| {
                    let level =
                        i32::from(i16::from_le_bytes(pair)) * volume / i32::from(FULL_VOLUME);
                    i16::try_from(level).unwrap_or_default().to_le_bytes()
                }),
        );
        let taken = write(&self.bytes)?.min(self.bytes.len());
        self.bytes.drain(..taken);
        Ok(())
    }

    /// Drop what waits: the sound it belongs to has stopped.
    pub fn forget(&mut self) {
        self.bytes.clear();
    }
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pacer {
    timing: Timing,
    speed: Speed,
    paused: bool,
    anchor: Option<Anchor>,
}

impl Pacer {
    /// A machine keeping `timing`, running at its own speed.
    #[must_use]
    pub const fn new(timing: Timing) -> Self {
        Self {
            timing,
            speed: Speed::Normal,
            paused: false,
            anchor: None,
        }
    }

    /// The timing it paces.
    #[must_use]
    pub const fn timing(&self) -> Timing {
        self.timing
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
    /// sound has a stream, `hurried` while a tape or a disc loads fast.
    /// Called only when a slice can be asked for: an answered `Run` is taken
    /// as asked.
    pub fn next(&mut self, now_ns: u64, ring: Option<Ring>, hurried: bool) -> Next {
        if self.paused {
            return Next::Wait;
        }
        if self.speed == Speed::Fastest || hurried {
            self.anchor = None;
            return Next::Run {
                cycles: self.slice(FAST_SLICES),
                sound: false,
            };
        }
        if let (Speed::Normal, Some(ring)) = (self.speed, ring) {
            self.anchor = None;
            let short = ring.target.saturating_sub(ring.queued);
            return if short > 0 && short >= LEAST_FRAMES.min(ring.target) {
                Next::Run {
                    cycles: cycles_for(self.timing, short),
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
        let second = self.timing.clock() * rate;
        let due = anchor
            .at_ns
            .saturating_add(anchor.cycles.saturating_mul(NANOS) / second);
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
        let slice = self.slice(CLOCK_SLICES);
        let cycles = anchor.cycles + u64::from(slice);
        self.anchor = Some(Anchor {
            at_ns: anchor.at_ns + cycles / second * NANOS,
            cycles: cycles % second,
            rate,
        });
        Next::Run {
            cycles: slice,
            sound: false,
        }
    }

    /// A second of the machine's time cut into `slices`.
    fn slice(&self, slices: u64) -> u32 {
        u32::try_from(self.timing.clock() / slices).unwrap_or(MAX_SLICE)
    }
}

/// The cycles of a machine keeping `timing` that make `frames` frames of
/// sound, at most a second's.
fn cycles_for(timing: Timing, frames: u32) -> u32 {
    let clock = timing.clock();
    let cycles = (u64::from(frames) * clock).div_ceil(u64::from(SAMPLE_RATE));
    u32::try_from(cycles.min(clock)).unwrap_or(MAX_SLICE)
}

#[cfg(test)]
#[path = "pacing_tests.rs"]
mod tests;
