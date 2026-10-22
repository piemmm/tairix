//! The outcomes of running a `cp` command.

use alloc::string::String;
use core::fmt;
use tairix_abi::Errno;
use tairix_util::quote::{quote, quote_if_needed};

/// Why a `cp` invocation did not complete.
///
/// The variants are deliberately coarse: the CLI surfaces enough to print a
/// useful diagnostic and set a process exit status, while leaning on the
/// frozen [`Errno`] for the wire-level cause so it invents no parallel error
/// set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CpError {
    /// The command line carried an unrecognised option, named fewer than two
    /// operands (`cp` needs at least one source and a destination), or aimed
    /// more than one source at a destination that is not a directory. The
    /// caller should print [`crate::USAGE`]. Nothing is copied.
    Usage,
    /// An option whose complete behaviour needs work this system does not
    /// have yet (`-a`/`--archive` and the `--preserve` set other than
    /// `--preserve=links`). Carries the switch as spelled. Refused rather
    /// than delivered as a silently narrower option.
    Unsupported(String),
    /// A source named a directory but `-r` was not given. Nothing about that
    /// operand is copied (`cp` refuses to recurse implicitly).
    IsDirectory,
    /// A directory source's destination already exists and is not a directory,
    /// so the subtree cannot be reproduced there.
    NotADirectory,
    /// Inspecting `path` failed — [`Errno::NotFound`] for a missing source,
    /// say, or [`Errno::PermissionDenied`] when the caller may not reach it.
    Stat {
        /// The path inspected.
        path: String,
        /// The refusal.
        errno: Errno,
    },
    /// Reading `path` — a file's bytes or attributes, a directory's entries,
    /// a link's target — failed.
    Read {
        /// The path read.
        path: String,
        /// The refusal.
        errno: Errno,
    },
    /// Making the destination `path` — a file, directory or link — failed.
    Create {
        /// The path made.
        path: String,
        /// The refusal.
        errno: Errno,
    },
    /// Writing the destination `path` failed.
    Write {
        /// The path written.
        path: String,
        /// The refusal.
        errno: Errno,
    },
    /// Writing to the terminal failed. Carries the underlying [`Errno`].
    Output(Errno),
    /// Asking the interactive confirmation question failed. Carries the
    /// underlying [`Errno`]. Failing closed: an unanswerable prompt never
    /// counts as consent.
    Prompt(Errno),
    /// The destination is the source's own node — the same name, a link to
    /// it, or another name for it — which a copy would empty before reading.
    /// Nothing is written.
    SameFile {
        /// The source as named.
        source: String,
        /// The destination as named.
        target: String,
    },
    /// A directory's copy would land inside the directory itself — however
    /// the destination is spelled, through a link, a `..` or another name for
    /// the volume — so the copy would copy itself without end. Refused
    /// before anything is written, or, for a directory moved into the source
    /// while the copy runs, when the walk meets it.
    IntoItself {
        /// The directory being copied, as named.
        source: String,
        /// The destination inside it, as named.
        target: String,
    },
    /// `source` would replace `target`, which an earlier operand of the run
    /// put there, so the run would keep only the last of them.
    JustCreated {
        /// The later operand.
        source: String,
        /// The destination the earlier one made.
        target: String,
    },
    /// The destination refused an extended attribute the copy had to carry
    /// (`--preserve=xattr`, or a failure that is no attribute's): its key,
    /// the destination, and the refusal.
    Attribute {
        /// The attribute refused.
        key: String,
        /// The destination it was refused on.
        target: String,
        /// The refusal.
        error: Errno,
    },
}

impl fmt::Display for CpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => f.write_str("invalid usage"),
            Self::Unsupported(switch) => write!(
                f,
                "{} is not available: preserving a node's timestamps needs a call \
                 this system does not have, so the option is refused rather than \
                 honoured in part (--preserve=links,xattr is available)",
                quote_if_needed(switch)
            ),
            Self::IsDirectory => f.write_str("is a directory (use -r to copy)"),
            Self::NotADirectory => f.write_str("destination is not a directory"),
            Self::Stat { path, errno } => write!(f, "cannot stat {}: {errno}", quote(path)),
            Self::Read { path, errno } => write!(f, "error reading {}: {errno}", quote(path)),
            Self::Create { path, errno } => write!(f, "cannot create {}: {errno}", quote(path)),
            Self::Write { path, errno } => write!(f, "error writing {}: {errno}", quote(path)),
            Self::Output(errno) => write!(f, "terminal write failed: {errno}"),
            Self::Prompt(errno) => write!(f, "cannot read confirmation: {errno}"),
            Self::SameFile { source, target } => {
                write!(
                    f,
                    "{} and {} are the same file",
                    quote(source),
                    quote(target)
                )
            }
            Self::IntoItself { source, target } => write!(
                f,
                "cannot copy a directory, {}, into itself, {}",
                quote(source),
                quote(target)
            ),
            Self::JustCreated { source, target } => write!(
                f,
                "will not overwrite just-created {} with {}",
                quote(target),
                quote(source)
            ),
            Self::Attribute { key, target, error } => write!(
                f,
                "setting attribute '{}' for {}: {error}",
                tairix_fsmeta::attr::display_value(key.as_bytes()),
                quote(target)
            ),
        }
    }
}
