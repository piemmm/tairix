//! Best times: the one thing this game keeps between sessions.
//!
//! A closed registry of dotted keys in the OS app-data store, reached through
//! [`tairix_appdata`] — so it is private to this application, gated on the
//! kernel-attested bundle identity, and readable or writable by no other app
//! the user launches. Nothing here performs I/O of its own; the caller supplies
//! the handle, and the write itself is handed to a worker rather than run on
//! the event loop.
//!
//! Only the three preset boards have a best time. A custom board is a size the
//! player invented, so two custom games are not the same game and a time on one
//! says nothing about a time on the other.
//!
//! A stored value the registry refuses — a time outside the bounds, or text
//! that is not a number — leaves that entry empty and is *named* to the caller,
//! which reports it. One corrupt entry therefore costs only itself and can
//! never become a time nobody played.

use alloc::string::String;
use core::fmt::Write as _;

use tairix_appconf::{as_u32, Registry};

use crate::board::Difficulty;

/// The longest time the store will keep, in seconds.
///
/// The readout shows three digits, so a longer game has no reading to display
/// and no claim to being a best time. A fixed bound on a value that may arrive
/// from a hand-edited document, not a capacity.
pub const MAX_TIME_SECS: u32 = 999;

/// A board a best time is kept for: a preset, never a custom size.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Preset {
    /// [`Difficulty::Beginner`].
    Beginner,
    /// [`Difficulty::Intermediate`].
    Intermediate,
    /// [`Difficulty::Expert`].
    Expert,
}

impl Preset {
    /// Every preset, in the order [`Difficulty::PRESETS`] lists them.
    pub const ALL: [Self; 3] = [Self::Beginner, Self::Intermediate, Self::Expert];

    /// The preset `difficulty` is, if it is one.
    #[must_use]
    pub const fn of(difficulty: Difficulty) -> Option<Self> {
        match difficulty {
            Difficulty::Beginner => Some(Self::Beginner),
            Difficulty::Intermediate => Some(Self::Intermediate),
            Difficulty::Expert => Some(Self::Expert),
            Difficulty::Custom(_) => None,
        }
    }

    /// The store key its best time is kept under.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Beginner => "best.beginner",
            Self::Intermediate => "best.intermediate",
            Self::Expert => "best.expert",
        }
    }
}

/// The best time on each preset board.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct BestTimes {
    beginner: Option<u32>,
    intermediate: Option<u32>,
    expert: Option<u32>,
}

impl BestTimes {
    /// The best time on `difficulty`, in seconds.
    #[must_use]
    pub const fn best(&self, difficulty: Difficulty) -> Option<u32> {
        match Preset::of(difficulty) {
            Some(preset) => self.of(preset),
            None => None,
        }
    }

    const fn of(&self, preset: Preset) -> Option<u32> {
        match preset {
            Preset::Beginner => self.beginner,
            Preset::Intermediate => self.intermediate,
            Preset::Expert => self.expert,
        }
    }

    const fn slot(&mut self, preset: Preset) -> &mut Option<u32> {
        match preset {
            Preset::Beginner => &mut self.beginner,
            Preset::Intermediate => &mut self.intermediate,
            Preset::Expert => &mut self.expert,
        }
    }

    /// Record `secs` on `difficulty`, reporting whether it is a new best.
    ///
    /// A custom board keeps no time, and a time past the bound is not a record
    /// — a game nobody could read the clock on is not one to beat.
    pub fn record(&mut self, difficulty: Difficulty, secs: u32) -> bool {
        let Some(preset) = Preset::of(difficulty) else {
            return false;
        };
        if secs == 0 || secs > MAX_TIME_SECS || self.of(preset).is_some_and(|best| best <= secs) {
            return false;
        }
        *self.slot(preset) = Some(secs);
        true
    }

    /// Forget every best time.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Whether any preset has a time recorded.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.beginner.is_none() && self.intermediate.is_none() && self.expert.is_none()
    }
}

impl Registry for BestTimes {
    type Key = Preset;
    const KEYS: &'static [Preset] = &Preset::ALL;

    fn name(preset: Preset) -> &'static str {
        preset.name()
    }

    fn read(&mut self, preset: Preset, text: &str) -> bool {
        match as_u32(text) {
            Ok(secs) if secs > 0 && secs <= MAX_TIME_SECS => {
                *self.slot(preset) = Some(secs);
                true
            }
            _ => false,
        }
    }

    fn spell(&self, preset: Preset, out: &mut String) -> bool {
        self.of(preset)
            .is_some_and(|secs| write!(out, "{secs}").is_ok())
    }
}

#[cfg(test)]
#[path = "scores_tests.rs"]
mod tests;
