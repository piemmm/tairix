//! The drives' lamps as the front-end is told them: each lit while its drive
//! works and for a moment after, so an access too short to see is still seen,
//! and told only when what they show changes.

use crate::board::Lights;
use crate::time::{Nanos, NANOS_PER_MILLISECOND};

/// How long a lamp stays lit after its drive stops working: long enough to
/// be seen.
pub const HOLD: Nanos = 60 * NANOS_PER_MILLISECOND;

/// Diskette drives a board has, then fixed disks.
const FLOPPIES: usize = 4;
const LAMPS: usize = FLOPPIES + 2;

/// When each drive was last seen working, and what the front-end was told.
#[derive(Debug, Default)]
pub struct Lamps {
    seen: [Option<Nanos>; LAMPS],
    told: Lights,
}

/// Whether lamp `n` is lit in `lights`.
const fn lit(lights: Lights, n: usize) -> bool {
    if n < FLOPPIES {
        lights.floppy & (1 << n) != 0
    } else {
        lights.fixed & (1 << (n - FLOPPIES)) != 0
    }
}

impl Lamps {
    /// Note the drives working as `lights` says at emulated time `now`: what
    /// the lamps show, if that is not what the front-end was last told.
    pub fn observe(&mut self, lights: Lights, now: Nanos) -> Option<Lights> {
        let mut shown = Lights::default();
        for (n, seen) in self.seen.iter_mut().enumerate() {
            if lit(lights, n) {
                *seen = Some(now);
            }
            if seen.is_some_and(|at| now.saturating_sub(at) < HOLD) {
                if n < FLOPPIES {
                    shown.floppy |= 1 << n;
                } else {
                    shown.fixed |= 1 << (n - FLOPPIES);
                }
            }
        }
        (shown != self.told).then(|| {
            self.told = shown;
            shown
        })
    }
}

#[cfg(test)]
#[path = "lamps_tests.rs"]
mod tests;
