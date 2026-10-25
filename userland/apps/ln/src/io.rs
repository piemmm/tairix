//! The seams through which `ln` touches the outside world, and the data they
//! carry.
//!
//! Keeping the filesystem and the terminal behind object-safe traits is what
//! lets the planning logic in [`crate::client`] run against in-memory
//! fixtures with no kernel, mirroring the seam design of the other userland
//! crates (`cp`'s `FileSystem`/`Prompt`, `ls`'s `Listing`).
//!
//! The vocabulary is deliberately the *name as typed*: `ln` creates and
//! replaces **names**, never what a name leads to, so the questions it asks
//! the filesystem are what a name holds and what a path canonically is.

use alloc::string::String;

pub use tairix_abi::fs::FinalLink;
use tairix_abi::{Errno, FileId, FileKind, RealpathMode};

/// A node a name leads to, as `ln`'s checks need it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Node {
    /// What kind of node it is.
    pub kind: FileKind,
    /// Its identity; [`FileId::NONE`] for a backing that offers none, which is
    /// never taken to be the same node as another.
    pub id: FileId,
    /// How many directory entries name it.
    pub links: u32,
}

/// Inspects names and creates links, in place of a taken name when asked.
pub trait FileSystem {
    /// The node `path` leads to, its final component followed only when
    /// `link` says so, or [`None`] when nothing is there (a dangling link,
    /// followed).
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises while reaching `path` — e.g.
    /// [`Errno::PermissionDenied`] when the caller may not search the way
    /// there, or [`Errno::LinkLoop`] for a cycle it follows.
    fn node(&self, path: &str, link: FinalLink) -> Result<Option<Node>, Errno>;

    /// The canonical path of `path`: the one path that names what it
    /// resolves to, with every symbolic link followed and every `..`
    /// applied, as the filesystem itself resolves it.
    ///
    /// `-r` needs it for both halves of the relative target it computes, and
    /// the same-file check to tell one directory entry from another. The tool
    /// never walks links to work this out: the answer must be the
    /// filesystem's own, because a path computed against a different reading
    /// of the tree would name a different node.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises — a permission refusal on a
    /// directory the resolution passes through, or [`Errno::LinkLoop`] for a
    /// cycle.
    fn canonicalize(&self, path: &str, mode: RealpathMode) -> Result<String, Errno>;

    /// Create the symbolic link `link` whose stored target is `target`; with
    /// `replace`, in place of whatever `link` holds, in one step that never
    /// leaves the name missing.
    ///
    /// `target` is stored verbatim and is never resolved here: it may be
    /// relative, may carry `..`, and may name nothing at all, so the created
    /// link may legitimately dangle. Creating it grants no authority over
    /// what it names.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises — [`Errno::AlreadyExists`] for a
    /// taken name without `replace`, [`Errno::NotSupported`] on a mount whose
    /// format stores no links (a permanent limit, not a transient failure), or
    /// a permission or grammar refusal. A refusal leaves `link` as it was.
    fn symlink(&self, target: &str, link: &str, replace: bool) -> Result<(), Errno>;

    /// Add `link` as a second directory entry for the node `target` already
    /// names — a hard link; with `replace`, in place of whatever `link` holds,
    /// in one step made only once the new name exists.
    ///
    /// `dereference` is `-L`: with it the *target's* own final symbolic link
    /// is resolved and the second name is given to what it names; without it
    /// (`-P`, the default) the node the caller spelled gains the name, which
    /// is POSIX `link()`. The new name is never followed either way.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises — [`Errno::AlreadyExists`] for a
    /// taken name without `replace`, [`Errno::NotFound`] for a missing
    /// target, [`Errno::IsADirectory`] for a directory operand (no principal
    /// may give a directory a second name), [`Errno::CrossVolume`] when the
    /// two paths are on different volumes, [`Errno::TooManyLinks`] when the
    /// format's per-node name count would overflow, [`Errno::NotSupported`] on
    /// a format that holds one name per node (a permanent limit), or a
    /// permission refusal. A refusal leaves `link` as it was.
    fn link(&self, target: &str, link: &str, dereference: bool, replace: bool)
        -> Result<(), Errno>;
}

pub use tairix_util::output::Output;

/// Asks the interactive confirmation question (`-i`).
///
/// The production implementation writes `ln: <question> ` to standard error
/// and reads one line from standard input, answering `true` only for an
/// affirmative reply (a leading `y`/`Y`), matching the GNU tool. A declined
/// question skips that link and fails the run, as GNU's does; an unanswerable
/// one fails closed — it is never treated as consent.
pub trait Prompt {
    /// Ask `question` and return whether the user consented.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the console raises — the caller fails closed.
    fn confirm(&self, question: &str) -> Result<bool, Errno>;
}
