//! The seams through which `mv` touches the outside world, and the data they
//! carry.
//!
//! The files a move reaches go through the shared tree seam
//! (`tairix_fscopy::tree`): a source and a destination are opened from the
//! working directory ([`Roots`]) and acted on through the directories that
//! hold them, never by a joined path. Beyond it a move needs a fresh name to
//! stage a copy under ([`Staging`]), the `-i` question ([`Prompt`]), and its
//! terminal ([`Output`]). Keeping them behind traits is what lets the move
//! logic in [`crate::client`] run against the in-memory fixture with no
//! kernel.

use alloc::string::String;
use tairix_abi::Errno;

pub use tairix_fscopy::tree::{Dir, Kind, Node, Roots};
pub use tairix_util::output::Output;

/// Draws the name a move across volumes builds its copy under.
pub trait Staging {
    /// A fresh name beside `name`, in the same directory: the rule every
    /// staged replacement on the system is spelled by.
    ///
    /// # Errors
    ///
    /// The refusal to draw one.
    fn staging_name(&self, name: &str) -> Result<String, Errno>;
}

/// Asks the interactive confirmation question (`-i`).
///
/// The production implementation writes `mv: <question> ` to standard
/// error and reads one line from standard input, answering `true` only
/// for an affirmative reply (a leading `y`/`Y`), matching the GNU tool.
/// A declined question skips the move; an unanswerable one fails closed —
/// it is never treated as consent.
pub trait Prompt {
    /// Ask `question` and return whether the user consented.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the console raises — the caller fails closed.
    fn confirm(&self, question: &str) -> Result<bool, Errno>;
}
