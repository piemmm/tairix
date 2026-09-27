//! Leaving: how a session that is ending — logged out, or told to end by its
//! authority — lets its applications close before it goes.
//!
//! Every top-level window is asked to close, exactly as its close button
//! would, and the session keeps serving while they do, so an application can
//! finish what it was doing through the window server it still has. The
//! session leaves once no application window is open or
//! [`SESSION_CLOSE_GRACE`] has passed; the kernel then ends whatever is still
//! running in the session (`docs/src/architecture/sessions.md`). A window
//! opened while the session is leaving is asked in turn, so nothing started
//! late is left to hold the exit open.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use tairix_abi::session_ipc::SESSION_CLOSE_GRACE;

/// A session on its way out.
#[derive(Debug)]
pub struct Departure {
    deadline_ns: u64,
    exit_code: i32,
    asked: BTreeSet<u64>,
}

impl Departure {
    /// Begin leaving at `now_ns`, to exit with `exit_code`.
    #[must_use]
    pub fn begin(now_ns: u64, exit_code: i32) -> Self {
        Self {
            deadline_ns: now_ns.saturating_add(SESSION_CLOSE_GRACE.saturating_total_nanos()),
            exit_code,
            asked: BTreeSet::new(),
        }
    }

    /// The windows among `open` not yet asked to close, each marked asked.
    pub fn unasked(&mut self, open: impl IntoIterator<Item = u64>) -> Vec<u64> {
        open.into_iter()
            .filter(|&window| self.asked.insert(window))
            .collect()
    }

    /// Whether the session may leave now: no application window is open, or
    /// the grace has run out.
    #[must_use]
    pub fn is_complete(&self, now_ns: u64, windows_open: bool) -> bool {
        !windows_open || now_ns >= self.deadline_ns
    }

    /// Tighten the loop's park so the grace's end wakes it.
    #[must_use]
    pub fn park_deadline_ns(&self, park: u64) -> u64 {
        park.min(self.deadline_ns)
    }

    /// The code the session exits with once it has left.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        self.exit_code
    }
}

#[cfg(test)]
#[path = "depart_tests.rs"]
mod tests;
