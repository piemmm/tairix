//! The outcomes of running an `mv` command.

use alloc::string::String;
use core::fmt;
use tairix_abi::Errno;
use tairix_util::quote::quote;

/// Why an `mv` invocation did not complete.
///
/// Each variant names what the GNU tool names in its diagnostic, and leans
/// on the frozen [`Errno`] for the cause, so it invents no parallel error set.
/// A move across volumes names the destination the user gave, never the
/// staging name its replacement was built under.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MvError {
    /// The command line carried an unrecognised option, named fewer than two
    /// operands (`mv` needs at least one source and a destination), or aimed
    /// more than one source at a destination that is not a directory. The
    /// caller should print [`crate::USAGE`]. Nothing is moved.
    Usage,
    /// Inspecting `path` failed — e.g. [`Errno::NotFound`] for a missing
    /// source.
    Stat {
        /// The path that could not be inspected.
        path: String,
        /// Why.
        errno: Errno,
    },
    /// The move of `source` onto `target` was refused, by the kernel's rename
    /// or, across volumes, by the same rule the rename keeps
    /// ([`Errno::NotEmpty`] for a directory onto a directory that is not
    /// empty).
    Rename {
        /// The source, as named.
        source: String,
        /// The destination, as named.
        target: String,
        /// Why.
        errno: Errno,
    },
    /// A directory would move into its own subtree.
    IntoItself {
        /// The directory, as named.
        source: String,
        /// The destination inside it.
        target: String,
    },
    /// A non-directory would replace the directory `target`.
    OverwriteDirectory {
        /// The directory in the way.
        target: String,
    },
    /// The directory `source` would replace the non-directory `target`.
    OverwriteNonDirectory {
        /// The directory being moved.
        source: String,
        /// The non-directory in the way.
        target: String,
    },
    /// `source` would replace `target`, which an earlier operand of the run
    /// put there: `mv a/x b/x d/` would lose `a/x`, whose source is gone.
    JustCreated {
        /// The later operand.
        source: String,
        /// The destination the earlier one made.
        target: String,
    },
    /// Across volumes, reading `path` failed. Nothing was moved.
    Read {
        /// The source being read.
        path: String,
        /// Why.
        errno: Errno,
    },
    /// Across volumes, making the replacement of `path` failed. Nothing was
    /// moved, and `path` holds what it held.
    Create {
        /// The destination being replaced.
        path: String,
        /// Why.
        errno: Errno,
    },
    /// Across volumes, writing the replacement of `path` failed — committing
    /// it to the disk among the reasons. Nothing was moved unless the
    /// destination already holds the whole source, which is then kept along
    /// with its source.
    Write {
        /// The destination being replaced.
        path: String,
        /// Why.
        errno: Errno,
    },
    /// Removing the source `path` after the move failed. The destination
    /// already holds it.
    Remove {
        /// The source that could not be removed.
        path: String,
        /// Why.
        errno: Errno,
    },
    /// Writing to the terminal failed.
    Output(Errno),
    /// Asking the interactive confirmation question failed. Failing closed:
    /// an unanswerable prompt never counts as consent.
    Prompt(Errno),
    /// Across volumes, the destination failed an extended attribute
    /// outright, for a reason that is no attribute's: its key, the
    /// destination, and the refusal. The source is kept.
    Attribute {
        /// The attribute refused.
        key: String,
        /// The destination it was refused on.
        target: String,
        /// The refusal.
        error: Errno,
    },
}

impl fmt::Display for MvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => f.write_str("invalid usage"),
            Self::Stat { path, errno } => write!(f, "cannot stat {}: {errno}", quote(path)),
            Self::Rename {
                source,
                target,
                errno,
            } => write!(
                f,
                "cannot move {} to {}: {errno}",
                quote(source),
                quote(target)
            ),
            Self::IntoItself { source, target } => write!(
                f,
                "cannot move {} to a subdirectory of itself, {}",
                quote(source),
                quote(target)
            ),
            Self::OverwriteDirectory { target } => write!(
                f,
                "cannot overwrite directory {} with non-directory",
                quote(target)
            ),
            Self::JustCreated { source, target } => write!(
                f,
                "will not overwrite just-created {} with {}",
                quote(target),
                quote(source)
            ),
            Self::OverwriteNonDirectory { source, target } => write!(
                f,
                "cannot overwrite non-directory {} with directory {}",
                quote(target),
                quote(source)
            ),
            Self::Read { path, errno } => write!(f, "error reading {}: {errno}", quote(path)),
            Self::Create { path, errno } => write!(f, "cannot create {}: {errno}", quote(path)),
            Self::Write { path, errno } => write!(f, "error writing {}: {errno}", quote(path)),
            Self::Remove { path, errno } => write!(f, "cannot remove {}: {errno}", quote(path)),
            Self::Output(errno) => write!(f, "terminal write failed: {errno}"),
            Self::Prompt(errno) => write!(f, "cannot read confirmation: {errno}"),
            Self::Attribute { key, target, error } => write!(
                f,
                "setting attribute '{}' for {}: {error}",
                tairix_fsmeta::attr::display_value(key.as_bytes()),
                quote(target)
            ),
        }
    }
}
