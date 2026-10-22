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
pub use tairix_fscopy::{Ancestry, AttributeStore, PathFiles};

/// What kind of object a path or directory entry is, as far as `mv` cares.
///
/// The distinction matters on the move across volumes, and to the rule a
/// rename keeps: a directory replaces only an empty directory, and nothing
/// else replaces a directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    /// A directory, whose entries a move across volumes reproduces.
    Directory,
    /// Any other non-link object, copied as a byte stream across volumes.
    File,
    /// A symbolic link, moved as itself: across volumes the same target is
    /// stored in a new link, never touching what it names.
    Symlink,
}

/// The result of a successful [`FileSystem::rename`].
///
/// A rename within a single filesystem is atomic and is the whole move. A
/// rename whose source and destination live on different filesystems cannot
/// be; rather than overload an [`Errno`], the seam reports
/// [`RenameOutcome::CrossDevice`] so the engine moves the source across.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenameOutcome {
    /// The source was renamed onto the destination atomically; the move is
    /// complete.
    Renamed,
    /// The source and destination are on different filesystems, so the
    /// engine must copy the source across and then remove it.
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

/// Who owns a node, and its permission bits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Owner {
    /// The permission bits, within `tairix_abi::FS_MODE_MASK`.
    pub mode: u32,
    /// The owning user.
    pub uid: u32,
    /// The owning group.
    pub gid: u32,
}

/// Inspects paths, renames them, and — to move a source across volumes —
/// reads it, builds its replacement, and removes it.
///
/// The client first asks [`kind`](FileSystem::kind) what a source is, then
/// tries [`rename`](FileSystem::rename). A [`RenameOutcome::Renamed`] is the
/// whole move. A [`RenameOutcome::CrossDevice`] builds the source's copy
/// under a [`staging_name`](FileSystem::staging_name) beside the destination
/// — a file through [`create`](FileSystem::create) and the files it reaches
/// by path ([`PathFiles`]), a directory through [`mkdir`](FileSystem::mkdir)
/// and a walk of [`read_dir`](FileSystem::read_dir), each node given the
/// source's [`Owner`] — [`sync`](FileSystem::sync)s it, renames it onto the
/// destination, syncs again, and only then removes the source with
/// [`remove_file`](FileSystem::remove_file) and
/// [`remove_dir`](FileSystem::remove_dir). Before moving a directory its
/// [`Ancestry`] says whether the destination lies inside it.
pub trait FileSystem: AttributeStore + PathFiles + Ancestry {
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
    /// live on different filesystems.
    ///
    /// # Errors
    ///
    /// The kernel's refusal: [`Errno::AlreadyExists`] for an occupied `dest`
    /// under [`RenameFlags::NO_REPLACE`], [`Errno::IsADirectory`] and
    /// [`Errno::NotADirectory`] for a kind that cannot replace the other,
    /// [`Errno::NotEmpty`] for a directory onto a directory holding anything,
    /// [`Errno::OutOfRange`] for a directory into its own subtree.
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
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises — [`Errno::AlreadyExists`] for a
    /// taken name.
    fn mkdir(&self, path: &str) -> Result<(), Errno>;

    /// Create the empty regular file `path`, ready to be written from offset
    /// `0`. A taken name is refused, never truncated, and a final link is not
    /// followed: the move only ever creates under a fresh staging name.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises — [`Errno::AlreadyExists`] for a
    /// taken name.
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

    /// The [`Owner`] of the node `path` names itself.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises while inspecting it.
    fn owner(&self, path: &str) -> Result<Owner, Errno>;

    /// Set the permission bits of `path`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal — [`Errno::NotSupported`] on a format that keeps
    /// no permission bits.
    fn set_mode(&self, path: &str, mode: u32) -> Result<(), Errno>;

    /// Give `path` to `uid` and `gid`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal — [`Errno::PermissionDenied`] when the caller may
    /// not give a node away, [`Errno::NotSupported`] on a format that keeps
    /// no owners.
    fn set_owner(&self, path: &str, uid: u32, gid: u32) -> Result<(), Errno>;

    /// A fresh name beside `target`, in its own directory, that `target`'s
    /// replacement is built under and renamed from.
    ///
    /// # Errors
    ///
    /// The refusal to draw one.
    fn staging_name(&self, target: &str) -> Result<String, Errno>;

    /// Put every write made so far on the medium. `path`, which must exist,
    /// is how the caller reaches the kernel; the kernel commits every volume.
    ///
    /// # Errors
    ///
    /// The commit's refusal.
    fn sync(&self, path: &str) -> Result<(), Errno>;
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
