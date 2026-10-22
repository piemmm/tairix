//! The seams through which `mv` touches the outside world, and the data they
//! carry.
//!
//! Keeping the filesystem and the terminal behind object-safe traits is what
//! lets the move logic in [`crate::client`] run against in-memory fixtures
//! with no kernel, mirroring the seam design of the other userland crates
//! (`init`'s `Spawner`/`Reaper`, `login`'s `Prompt`, `sysinfo`'s `Transport`,
//! `cat`'s `FileSource`, `ls`'s `Listing`, `rm`'s `Removal`, `cp`'s
//! `FileSystem`).

use alloc::string::String;
use tairix_abi::{Errno, RenameFlags};
pub use tairix_fscopy::{AttributeStore, PathFiles};

/// What kind of object a path or directory entry is, as far as `mv` cares.
///
/// The distinction `mv` needs only matters on the cross-device fallback: a
/// directory is reproduced by [`FileSystem::mkdir`] and a recursive descent,
/// a symbolic link is reproduced as a link, and everything else is copied as
/// a stream of bytes. An in-filesystem rename moves every kind atomically
/// without caring which it is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    /// A directory, whose entries the cross-device fallback reproduces under
    /// the destination before removing the source.
    Directory,
    /// Any other non-link object, copied as a byte stream on the fallback.
    File,
    /// A symbolic link, moved as itself: the fallback stores the same target
    /// in a link at the destination and unlinks the source link, never
    /// touching what it names.
    Symlink,
}

/// The result of a successful [`FileSystem::rename`].
///
/// A rename within a single filesystem is atomic and is the whole move. A
/// rename whose source and destination live on different filesystems cannot
/// be atomic; rather than overload an [`Errno`], the seam reports
/// [`RenameOutcome::CrossDevice`] so the engine can fall back to the POSIX
/// copy-then-remove relocation. This keeps the boundary case an explicit,
/// non-error outcome rather than a magic error code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenameOutcome {
    /// The source was renamed onto the destination atomically; the move is
    /// complete.
    Renamed,
    /// The source and destination are on different filesystems, so an atomic
    /// rename is impossible. The engine must copy the source to the
    /// destination and then remove the source.
    CrossDevice,
}

/// One directory entry: a name and its [`EntryKind`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    /// The entry's name within its directory (not a full path, and never
    /// `.` or `..` — the seam does not surface those).
    pub name: String,
    /// What kind of object the entry is.
    pub kind: EntryKind,
}

/// Inspects paths, renames them, and — for the cross-device fallback — reads
/// sources, creates destinations, and removes sources.
///
/// The client first asks [`kind`](FileSystem::kind) what a source is, then
/// tries [`rename`](FileSystem::rename). A [`RenameOutcome::Renamed`] is the
/// whole move. A [`RenameOutcome::CrossDevice`] drives the fallback: a
/// non-directory source is copied, through the files it reaches by path
/// ([`PathFiles`]), into a destination created with
/// [`create`](FileSystem::create); a directory source is reproduced with
/// [`mkdir`](FileSystem::mkdir) and a recursive walk
/// ([`read_dir`](FileSystem::read_dir)); then the source is removed depth-first
/// with [`remove_file`](FileSystem::remove_file) and
/// [`remove_dir`](FileSystem::remove_dir).
pub trait FileSystem: AttributeStore + PathFiles {
    /// Return the [`EntryKind`] of the object `path` names itself: a final
    /// symbolic link reports [`EntryKind::Symlink`]. A missing path reports
    /// [`Errno::NotFound`], which the client treats as "absent" when probing a
    /// destination.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises — e.g. [`Errno::NotFound`] for a
    /// missing path or [`Errno::PermissionDenied`] when the caller may not
    /// reach it.
    fn kind(&self, path: &str) -> Result<EntryKind, Errno>;

    /// Return the [`EntryKind`] of what `path` resolves to, a final symbolic
    /// link followed: whether a destination is a directory to move into.
    ///
    /// # Errors
    ///
    /// As [`kind`](FileSystem::kind); a dangling link is
    /// [`Errno::NotFound`].
    fn resolved_kind(&self, path: &str) -> Result<EntryKind, Errno>;

    /// The target the symbolic link at `path` stores, exactly as stored.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises while reading the link.
    fn read_link(&self, path: &str) -> Result<String, Errno>;

    /// Create the symbolic link `link` storing `target` verbatim.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises — [`Errno::AlreadyExists`] for a
    /// taken name, [`Errno::NotSupported`] on a format that stores no links.
    fn symlink(&self, target: &str, link: &str) -> Result<(), Errno>;

    /// Rename `source` onto `dest`, replacing an existing `dest` of a
    /// compatible kind unless `flags` refuses the replacement.
    ///
    /// Returns [`RenameOutcome::Renamed`] when the rename completed
    /// atomically, or [`RenameOutcome::CrossDevice`] when `source` and `dest`
    /// live on different filesystems and the caller must relocate by copying
    /// and removing instead.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises — e.g. [`Errno::PermissionDenied`]
    /// for an unwritable destination directory, or [`Errno::AlreadyExists`]
    /// for an occupied one under [`RenameFlags::NO_REPLACE`].
    fn rename(&self, source: &str, dest: &str, flags: RenameFlags) -> Result<RenameOutcome, Errno>;

    /// Return the entry at position `index` in the directory `path`, or
    /// [`None`] once `index` is past the last entry.
    ///
    /// The client reads a directory by calling this with `index` `0, 1, 2, …`
    /// until it returns [`None`]. The entries `.` and `..` are never
    /// returned.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises while reading the directory.
    fn read_dir(&self, path: &str, index: u64) -> Result<Option<Entry>, Errno>;

    /// Create the directory `path`.
    ///
    /// The client calls this only after observing that `path` does not exist.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises — e.g. [`Errno::PermissionDenied`].
    fn mkdir(&self, path: &str) -> Result<(), Errno>;

    /// Create (or truncate to empty) the regular file `path`, ready to be
    /// written from offset `0`.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises — e.g. [`Errno::PermissionDenied`].
    fn create(&self, path: &str) -> Result<(), Errno>;

    /// Remove the non-directory object at `path` (unlink one link).
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises — e.g. [`Errno::PermissionDenied`].
    fn remove_file(&self, path: &str) -> Result<(), Errno>;

    /// Remove the directory `path`, which the client has already emptied.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises — e.g. [`Errno::PermissionDenied`].
    fn remove_dir(&self, path: &str) -> Result<(), Errno>;
}

pub use tairix_util::output::Output;

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
