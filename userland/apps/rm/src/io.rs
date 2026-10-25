//! The seams through which `rm` touches the terminal. The filesystem is the
//! shared tree seam, `tairix_fscopy::tree`.
//!
//! Keeping the terminal behind object-safe traits is what lets the removal
//! logic in [`crate::client`] run against in-memory fixtures with no kernel.

use tairix_abi::Errno;

pub use tairix_util::output::Output;

/// Asks the confirmation questions: `-i` / `-I`'s, and the write-protected
/// one asked at a terminal.
///
/// The production implementation writes `rm: <question> ` to standard
/// error and reads one line from standard input, answering `true` only
/// for an affirmative reply (a leading `y`/`Y`), matching the GNU tool.
/// A declined question skips the object; an unanswerable one fails
/// closed — it is never treated as consent.
pub trait Prompt {
    /// Ask `question` and return whether the user consented.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the console raises — the caller fails closed.
    fn confirm(&self, question: &str) -> Result<bool, Errno>;

    /// Whether the replies come from a terminal, where a write-protected
    /// file is asked about before it is removed.
    fn at_terminal(&self) -> bool;
}
