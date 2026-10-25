//! The seams through which `cp` touches the outside world, and the data they
//! carry.
//!
//! A copy reaches files through the shared tree seam (`tairix_fscopy::tree`):
//! a tree is walked through directories held open and each entry copied by
//! name from there, into directories the copy made or opened by name too.
//! What `cp` needs beyond it is [`FileSystem`]: an operand read or written
//! through its final link, as GNU's `cp` reads and writes one, a resolved
//! spelling, and a link put in a name's place in one step. Keeping them
//! behind traits is what lets the copy logic in [`crate::client`] run against
//! the in-memory fixture with no kernel.

use alloc::string::String;
use tairix_abi::{Errno, FileId, FileStat};

pub use tairix_abi::fs::FinalLink;
pub use tairix_fscopy::tree::{Dir, Entry, Kind, Node, Roots};
pub use tairix_util::output::Output;

/// What one look at a node learned: what it is, and the identity that tells
/// a second *name* for one node from a second node.
///
/// `--preserve=links` needs exactly the identity and name count: without
/// them two sources naming one node are indistinguishable from two files and
/// the copy would duplicate the data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Probe {
    /// What kind of node it is.
    pub kind: Kind,
    /// Its stable system-wide identity. [`FileId::NONE`] for a backing that
    /// offers none, which is therefore never compared for equality.
    pub id: FileId,
    /// How many directory entries name it. A node named once cannot be
    /// reached twice, which is what bounds `--preserve=links`' map to the
    /// hard links a copy actually meets.
    pub nlink: u32,
}

impl Probe {
    /// What its directory's listing said of `entry`.
    #[must_use]
    pub const fn listed(entry: &Entry) -> Self {
        Self {
            kind: entry.kind,
            id: entry.id,
            nlink: entry.nlink,
        }
    }

    /// What `stat` says of a node.
    #[must_use]
    pub const fn of(stat: &FileStat) -> Self {
        Self {
            kind: Kind::of(stat.kind),
            id: stat.id,
            nlink: stat.nlink,
        }
    }
}

/// What a copy reaches beyond the shared tree seam, each by an operand's
/// path.
pub trait FileSystem: Roots {
    /// The file `path` names, opened for reading through a final link: a
    /// source operand read as GNU's `cp` reads one.
    ///
    /// # Errors
    ///
    /// The open's refusal.
    fn open_source(&self, path: &str) -> Result<<Self::Dir as Dir>::File, Errno>;

    /// The existing file `path` names, opened for writing through a final
    /// link and emptied: a destination a byte copy writes as GNU's writes
    /// one, keeping its own mode.
    ///
    /// # Errors
    ///
    /// The open's refusal — [`Errno::PermissionDenied`] for a destination
    /// that may not be written, which `-f` removes and creates afresh.
    fn truncate_existing(&self, path: &str) -> Result<<Self::Dir as Dir>::File, Errno>;

    /// `path` as the kernel resolves it, every link on the way and at its end
    /// followed: what `-l` links when it follows a link.
    ///
    /// # Errors
    ///
    /// The resolution's refusal.
    fn canonical(&self, path: &str) -> Result<String, Errno>;

    /// Create the symbolic link `link` storing `target` verbatim; with
    /// `replace`, in place of whatever `link` holds, in one step that leaves
    /// it as it was when the link cannot be made.
    ///
    /// # Errors
    ///
    /// [`Errno::AlreadyExists`] for a taken name without `replace`, or the
    /// link's refusal.
    fn symlink(&self, target: &str, link: &str, replace: bool) -> Result<(), Errno>;

    /// Add `new` as a second name for the node `existing` names, neither name
    /// followed; with `replace`, in place of whatever `new` holds, in one step
    /// made only once the second name exists.
    ///
    /// # Errors
    ///
    /// [`Errno::AlreadyExists`] for a taken name without `replace`,
    /// [`Errno::CrossVolume`] across volumes, or the link's refusal.
    fn link(&self, existing: &str, new: &str, replace: bool) -> Result<(), Errno>;
}

/// Asks the interactive confirmation question (`-i`).
///
/// The production implementation writes `cp: <question> ` to standard
/// error and reads one line from standard input, answering `true` only
/// for an affirmative reply (a leading `y`/`Y`), matching the GNU tool.
/// A declined question skips the copy; an unanswerable one fails closed —
/// it is never treated as consent.
pub trait Prompt {
    /// Ask `question` and return whether the user consented.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the console raises — the caller fails closed.
    fn confirm(&self, question: &str) -> Result<bool, Errno>;
}
