//! Cinder's persisted state: how he was feeling and whether he was out.
//!
//! Small on purpose. A companion should be where you left him and in roughly
//! the mood you left him in; everything else is this run's business and is not
//! written anywhere.

use alloc::string::String;
use core::fmt::Write as _;

use tairix_appconf::{as_bool, as_permille, bool_text, Registry, PERMILLE_FULL};

use crate::mind::Needs;

/// One thing kept: a need's level, in parts per thousand, or whereabouts.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Kept {
    /// `energy`.
    Energy,
    /// `play`.
    Play,
    /// `affection`.
    Affection,
    /// `loose`: whether he was out on the desktop.
    Loose,
}

/// What survives a restart.
#[derive(Copy, Clone, Debug, PartialEq, Default)]
pub struct Saved {
    /// How Cinder was feeling.
    pub needs: Needs,
    /// Whether he was out on the desktop.
    pub was_loose: bool,
}

impl Saved {
    const fn level(&mut self, kept: Kept) -> Option<&mut f64> {
        match kept {
            Kept::Energy => Some(&mut self.needs.energy),
            Kept::Play => Some(&mut self.needs.play),
            Kept::Affection => Some(&mut self.needs.affection),
            Kept::Loose => None,
        }
    }
}

/// Levels are kept in parts per thousand, the shared settings grammar's own
/// fixed-point form: exact in the text, legible to anyone reading the file,
/// and needing no float parser to read back. A value this never wrote is not
/// one to believe, so it is refused, never repaired.
impl Registry for Saved {
    type Key = Kept;
    const KEYS: &'static [Kept] = &[Kept::Energy, Kept::Play, Kept::Affection, Kept::Loose];

    fn name(kept: Kept) -> &'static str {
        match kept {
            Kept::Energy => "energy",
            Kept::Play => "play",
            Kept::Affection => "affection",
            Kept::Loose => "loose",
        }
    }

    fn read(&mut self, kept: Kept, text: &str) -> bool {
        match self.level(kept) {
            Some(level) => as_permille(text).is_ok_and(|parts| {
                *level = f64::from(parts) / f64::from(PERMILLE_FULL);
                true
            }),
            None => as_bool(text).is_ok_and(|loose| {
                self.was_loose = loose;
                true
            }),
        }
    }

    fn spell(&self, kept: Kept, out: &mut String) -> bool {
        let level = match kept {
            Kept::Energy => self.needs.energy,
            Kept::Play => self.needs.play,
            Kept::Affection => self.needs.affection,
            Kept::Loose => {
                out.push_str(bool_text(self.was_loose));
                return true;
            }
        };
        write!(out, "{}", permille_of(level)).is_ok()
    }
}

/// `level` as parts per thousand, bounded into range.
fn permille_of(level: f64) -> u32 {
    let scaled = tairix_util::mathf::clamp(level, 0.0, 1.0) * f64::from(PERMILLE_FULL);
    // The clamp bounds this to `0..=PERMILLE_FULL`, well inside a `u32`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        scaled as u32
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
