//! The one-shot announcement that a session-owned surface which arms its
//! input — the picker, the resolution prompt — is on screen and answers it.
//!
//! Such a surface ignores a choosing click or key until it has been on screen
//! for the arm delay, so the announcement waits for both a presented frame
//! carrying it and the arm: an input it invites is never one it drops.

/// How far a surface's announcement has come.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum Announced {
    /// No presented frame has carried it.
    #[default]
    Unseen,
    /// One has; the announcement waits on its arm.
    Presented,
    /// It has been announced.
    Shown,
}

impl Announced {
    /// Take in a frame carrying the surface having been `presented`, and the
    /// arm falling due at `armed_at`, at `now_ns`: whether to announce now.
    /// Answers `true` once.
    pub(crate) fn advance(&mut self, presented: bool, armed_at: Option<u64>, now_ns: u64) -> bool {
        if *self == Self::Unseen && presented {
            *self = Self::Presented;
        }
        if *self == Self::Presented && armed_at.is_some_and(|at| now_ns >= at) {
            *self = Self::Shown;
            return true;
        }
        false
    }

    /// When an announcement held back for the arm at `armed_at` falls due.
    pub(crate) fn due(self, armed_at: Option<u64>) -> Option<u64> {
        armed_at.filter(|_| self == Self::Presented)
    }
}

#[cfg(test)]
#[path = "shown_tests.rs"]
mod tests;
