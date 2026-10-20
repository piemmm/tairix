//! The session's frame deadline: at most one composite per frame period,
//! however many wakes produced damage inside it.
//!
//! Without it the desktop composites once per *wake*. A hand on the mouse
//! delivers pointer samples far faster than any screen can show them, so a
//! drag spends whole frames' worth of blending on pixels the next sample
//! overwrites before a scan-out ever reads them. Damage accumulates in the
//! compositor between deadlines instead, and is composited once when the
//! next one arrives.
//!
//! # Latency is paid only where a frame would have been wasted
//!
//! A frame whose period has already elapsed — the first after an idle
//! desktop, a click, a keystroke, any interaction slower than the display —
//! is admitted on the very wake that produced it. Only a producer outrunning
//! the screen is held, and only until the frame it is racing.
//!
//! A held frame shortens the session's park to the moment it comes due,
//! through the same [`park_within`] fold the clock and every animated surface
//! use. Nothing held means nothing armed: an idle desktop parks indefinitely
//! and the pacer costs it not one wake.
//!
//! # The period is the output's own
//!
//! The period is one frame of the rate the output scans at
//! ([`RefreshRate`]), handed in by the compositor that holds it, which is the
//! same fact whether a frame carries an animation step or a drag — so every
//! animated surface paces by the same rate, and none is woken for a frame the
//! pacer would then refuse. What the session animates is stepped only on a
//! frame that is [due](FramePacer::due), so a burst of input between frames
//! costs no animation step at all.
//!
//! # A display that flips paces the frames itself
//!
//! A display whose engine reads the frame it shows takes a present at its next
//! frame start and posts the latch when it has. Until then it is not ready,
//! and a frame produced meanwhile is held without a deadline: the latch is
//! what wakes the session, and the next frame is admitted on that wake. Such
//! a display presents at most one frame per refresh by construction, so the
//! period no longer applies to it.

use tairix_abi::driver::display::RefreshRate;

use crate::switchuser::park_within;

/// The session's frame pacer: when the last composite was admitted, and
/// whether one is being held for the next deadline.
///
/// One per session, driven from the run loop's frame path. It holds no copy
/// of the damage — the compositor owns that, and answers whether a composite
/// would recompose a pixel — so the two cannot disagree about whether a
/// frame is owed.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct FramePacer {
    /// When the last *composited* frame was admitted. `None` until the first,
    /// so a desktop coming up draws at once.
    last_ns: Option<u64>,
    /// Whether damage is being held for the next deadline. Drives
    /// [`park_deadline_ns`](Self::park_deadline_ns).
    held: bool,
    /// Whether the damage held waits for the display's latch, which wakes
    /// the session itself, rather than for the period.
    for_flip: bool,
    /// Whether the display took the last frame at its next flip, and so
    /// paces frames by its flips.
    flips: bool,
}

impl FramePacer {
    /// A session that has composited nothing yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last_ns: None,
            held: false,
            for_flip: false,
            flips: false,
        }
    }

    /// Whether the frame this wake produced is composited and presented now,
    /// rather than held for the next deadline or the display's latch.
    ///
    /// `damaged` is the compositor's own answer to whether a composite would
    /// recompose a pixel, `ready` the display's to whether it may be handed a
    /// frame, and `refresh` the rate it scans at. An undamaged frame is never
    /// held and never starts the period: presenting it moves nothing, and it is
    /// what re-reads the frame counters as idle, so holding it would suppress
    /// that reading and starting the period would delay the next real frame
    /// behind it.
    ///
    /// A clock that has jumped behind the last frame admits rather than
    /// stalling the screen for the length of the jump.
    pub fn admit(&mut self, now_ns: u64, damaged: bool, ready: bool, refresh: RefreshRate) -> bool {
        if damaged && !ready {
            self.held = true;
            self.for_flip = true;
            return false;
        }
        self.for_flip = false;
        if damaged && !self.flips && self.remaining_ns(now_ns, refresh) != 0 {
            self.held = true;
            return false;
        }
        self.held = false;
        if damaged {
            self.last_ns = Some(now_ns);
        }
        true
    }

    /// Note how the display took the frame last admitted: `queued` for one
    /// it shows from its next flip, which paces the frames from then on.
    pub fn presented(&mut self, queued: bool) {
        self.flips = queued;
    }

    /// Whether a frame drawn at `now_ns` on an output refreshing at `refresh`
    /// is due: what the session steps everything it animates on, so an
    /// animation costs one step a frame however often input wakes the loop.
    #[must_use]
    pub fn due(&self, now_ns: u64, refresh: RefreshRate) -> bool {
        self.remaining_ns(now_ns, refresh) == 0
    }

    /// `park_ns` shortened to when an animation stepped only on a
    /// [`due`](Self::due) frame at `refresh` takes the step it owes `step_ns`
    /// from now — none at
    /// [`WAITSET_TIMEOUT_NONE`](tairix_abi::WAITSET_TIMEOUT_NONE) — but never
    /// to before that frame, so one left overdue between frames waits for it
    /// rather than spinning the loop.
    #[must_use]
    pub fn step_deadline_ns(
        &self,
        now_ns: u64,
        park_ns: u64,
        step_ns: u64,
        refresh: RefreshRate,
    ) -> u64 {
        park_ns.min(step_ns.max(self.remaining_ns(now_ns, refresh)))
    }

    /// `park_ns` shortened to the moment a held frame comes due at
    /// `refresh`, or left exactly as it is when nothing is held.
    ///
    /// This is the whole of the pacer's wake behaviour: one shot per held
    /// frame, and none at all otherwise. The deadline is never shortened to
    /// nothing while a frame is genuinely being held —
    /// [`admit`](Self::admit) holds only what is not yet due — so the loop
    /// cannot spin between a refusal and its deadline.
    #[must_use]
    pub fn park_deadline_ns(&self, now_ns: u64, park_ns: u64, refresh: RefreshRate) -> u64 {
        park_within(
            park_ns,
            (self.held && !self.for_flip).then(|| self.remaining_ns(now_ns, refresh)),
        )
    }

    /// Nanoseconds left of the current frame period at `refresh`: `0` once
    /// the next frame is due, before the first frame, and on a clock that
    /// jumped backwards.
    fn remaining_ns(&self, now_ns: u64, refresh: RefreshRate) -> u64 {
        self.last_ns.map_or(0, |last| {
            now_ns
                .checked_sub(last)
                .map_or(0, |elapsed| refresh.frame_ns().saturating_sub(elapsed))
        })
    }
}

#[cfg(test)]
#[path = "pace_tests.rs"]
mod tests;
