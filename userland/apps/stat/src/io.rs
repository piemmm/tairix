//! The seams through which `stat` reaches the outside world, and the data
//! they carry.
//!
//! `stat` is a *reporter*: it reads facts and renders them. Keeping every
//! fact behind an object-safe trait is what lets the whole rendering surface
//! — every specifier, both vocabularies, and each refusal — run against
//! in-memory fixtures with no kernel, the seam discipline of the sibling
//! tools (`df`'s `Mounts`, `du`'s `Walk`, `ln`'s `FileSystem`).

use alloc::string::String;

use tairix_abi::{Errno, FileStat};
use tairix_procinfo::Namespace;

/// The facts a filesystem answers about one path.
pub trait Filesystem {
    /// The node `path` names, described.
    ///
    /// `dereference` is `-L`: with it a final symbolic link is resolved and
    /// the report describes what it names; without it the link itself is
    /// described, which is what makes `stat` the tool that can see a link at
    /// all.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the kernel raises — [`Errno::NotFound`] for an absent
    /// name, or a permission refusal on a directory the resolution passes
    /// through.
    fn stat(&self, path: &str, dereference: bool) -> Result<FileStat, Errno>;

    /// The stored target of the symbolic link at `path`, exactly as stored.
    ///
    /// Read only for `%N`, which shows a link beside what it names.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the kernel raises, including [`Errno::OutOfRange`] when
    /// `path` is not a symbolic link.
    fn read_link(&self, path: &str) -> Result<String, Errno>;

    /// The canonical path of `path` — every link followed, every `..`
    /// applied — as the kernel itself resolves it.
    ///
    /// `%m` needs it: the binding holding a path is the one covering the
    /// path's *canonical* spelling, so a link into another volume reports the
    /// volume it really lands on rather than the one its name was typed
    /// under.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the kernel raises.
    fn canonicalize(&self, path: &str) -> Result<String, Errno>;
}

/// Reads the namespace: the attached volumes and the view bindings onto
/// them.
pub trait Mounts {
    /// The namespace as the system reports it.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the System Information service raises.
    fn namespace(&self) -> Result<Namespace, Errno>;
}

/// Resolves a numeric owner id to an account name (`%U`).
pub trait Names {
    /// The account name owning `uid`, or [`None`] when the directory holds
    /// no entry for it.
    ///
    /// A uid with no name renders as GNU's `UNKNOWN` rather than as the
    /// number, so a name field never quietly becomes a numeric one.
    fn user(&self, uid: u32) -> Option<String>;
}

pub use tairix_util::output::Output;
