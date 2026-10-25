//! The [`Fs`] seam: everything the file manager asks of the filesystem.
//!
//! The model is I/O-free; every directory read and free-space query goes
//! through this trait. The `Run` binary implements it over the
//! kernel-authorised `fs_*` syscalls (every per-inode and mount check stays
//! kernel-side, and a refusal comes back as the frozen [`Errno`] the model
//! surfaces on its message line); the tests implement it over in-memory
//! trees, so the whole session is drivable without a kernel. A copy, move or
//! removal is only started here: the shared engine carries it out through
//! the directories it holds.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Time64;
use tairix_abi::{Errno, FileId, FileKind};
use tairix_fscopy::op::{OpError, Verb};
use tairix_fscopy::{Landing, Unresolved};
use tairix_procinfo::VolumeBytes;

use crate::ops::Run;

/// One directory entry as the listing reports it: exactly the fields the
/// kernel's `fs_readdir` stream carries per entry, so a listing never costs
/// a per-child stat.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsEntry {
    /// The entry's name within its directory (a single component).
    pub name: String,
    /// The entry's **own** kind, as the stream reports it: a symbolic link
    /// arrives as [`FileKind::Symlink`], never as what it names.
    pub kind: FileKind,
    /// The node the stream names, or [`FileId::NONE`] where it names none
    /// (a mount point): an operation started on the entry refuses another.
    pub id: FileId,
    /// Apparent length in bytes; `0` for a directory.
    pub size: u64,
    /// Last contents-modification instant as the mounted format stores it;
    /// [`Time64::UNIX_EPOCH`] means the backing keeps no stamp and renders
    /// as absent, never as a fabricated date.
    pub modified: Time64,
}

/// One published storage root the volume list (`V`) offers: where it is
/// mounted, what backs it, and its space figures when known.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VolumeInfo {
    /// The mount target — the path the tree opens when the volume is
    /// chosen.
    pub target: String,
    /// The mounted filesystem's type name (`arxfs`, `ext4`, …), as the
    /// mount table reports it.
    pub fstype: String,
    /// What the volume holds, or `None` when it tracks no capacity
    /// (shown as absent, never fabricated).
    pub space: Option<VolumeBytes>,
}

/// The filesystem operations the landed stages perform: listings, the
/// volume list, the mode and attribute editors' reads and writes, the
/// viewers' and searches' streamed read, the destination probe and
/// containment check the planner asks, `mkdir`, and the start of a copy,
/// move or removal. Later stages extend this trait in place together with
/// the callers they introduce.
pub trait Fs {
    /// List every entry of the directory `path`, in any order (the model
    /// sorts them).
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises — e.g. [`Errno::NotFound`] for a
    /// vanished path or [`Errno::PermissionDenied`] when the caller may not
    /// list it. The model surfaces the error and keeps its previous state.
    fn list_dir(&mut self, path: &str) -> Result<Vec<FsEntry>, Errno>;

    /// What the volume backing `path` holds, or `None` when no figure is
    /// available (best-effort; never an error) — an unreachable service, or
    /// a backing mount that tracks no capacity. The header then omits it.
    fn volume_space(&mut self, path: &str) -> Option<VolumeBytes>;

    /// The published storage roots the session can open — the mounted
    /// volumes as the System Information API reports them. Best-effort by
    /// contract: an unreachable service yields an empty list (the volume
    /// list then says no volumes were reported), never an error.
    fn list_volumes(&mut self) -> Vec<VolumeInfo>;

    /// The current permission bits of the entry at `path` (a resolve-only
    /// stat — no read authority is requested), for the mode editor's
    /// pre-filled prompt.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises; the model surfaces it and
    /// opens no prompt.
    fn stat_mode(&mut self, path: &str) -> Result<u32, Errno>;

    /// Set the permission bits of the entry at `path` to `mode` (at most
    /// [`tairix_abi::FS_MODE_MASK`]). The kernel owns the authorisation:
    /// only the entry's owner may change its mode, and a refusal comes
    /// back as the frozen [`Errno`] the model surfaces unchanged.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises; the model reports it and
    /// changes nothing.
    fn set_mode(&mut self, path: &str, mode: u32) -> Result<(), Errno>;

    /// Every visible extended-attribute key of the entry at `path`, in the
    /// backing's stable order (the `lib/fsmeta` `namespace.rest` grammar;
    /// keys the caller may not read are omitted by the kernel, never
    /// shown).
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises; [`Errno::NotSupported`] means
    /// the mounted format stores no attributes, which the attributes view
    /// states honestly rather than treating as an empty set.
    fn attr_list(&mut self, path: &str) -> Result<Vec<String>, Errno>;

    /// The value of extended attribute `key` on the entry at `path`
    /// (opaque bytes; a value may be empty).
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises; [`Errno::NoData`] means the
    /// entry carries no such attribute.
    fn attr_get(&mut self, path: &str, key: &str) -> Result<Vec<u8>, Errno>;

    /// Set (insert or replace) extended attribute `key` on the entry at
    /// `path` to `value`. The kernel owns the authorisation: write
    /// permission on the node, a writable mount, the shared key grammar,
    /// and the fixed size bounds; a refusal comes back as the frozen
    /// [`Errno`] the model surfaces unchanged.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises; the model reports it and
    /// changes nothing.
    fn attr_set(&mut self, path: &str, key: &str, value: &[u8]) -> Result<(), Errno>;

    /// Remove extended attribute `key` from the entry at `path`.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises; [`Errno::NoData`] means no
    /// such attribute was stored.
    fn attr_remove(&mut self, path: &str, key: &str) -> Result<(), Errno>;

    /// The [`FileKind`] of the entry `path` **names** — a resolve-only stat
    /// that does *not* follow a final symbolic link — used to probe a
    /// destination before an operation is planned.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises; [`Errno::NotFound`] means the
    /// path is absent, which the planner treats as "free to create".
    fn stat_kind(&mut self, path: &str) -> Result<FileKind, Errno>;

    /// Where the directory `dir` stands to the directory `src`, judged by
    /// the shared containment check from the directories themselves, every
    /// link on the way to either followed: [`Landing::Same`] for the source
    /// itself, [`Landing::Within`] beneath it at any depth, else
    /// [`Landing::Apart`].
    ///
    /// # Errors
    ///
    /// The side whose directory could not be opened or identified.
    fn landing(&mut self, src: &str, dir: &str) -> Result<Landing, Unresolved>;

    /// Read up to `buf.len()` bytes of the file at `path` from `offset`,
    /// returning the count read (`0` at end of file). A short read (fewer
    /// than requested) occurs only at end of file.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises while reading.
    fn read(&mut self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, Errno>;

    /// Create the directory at `path`, born with `mode` less the umask.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises — e.g. [`Errno::AlreadyExists`].
    fn mkdir(&mut self, path: &str, mode: u32) -> Result<(), Errno>;

    /// Start copying or moving `src`, a path and the node it was listed as,
    /// to `target`: the directories holding them opened by path, and every
    /// entry beneath reached by name from one held, as the shared engine
    /// carries it out.
    ///
    /// # Errors
    ///
    /// The refusal opening either directory, naming its path.
    fn transfer(
        &mut self,
        verb: Verb,
        src: (&str, FileId),
        target: &str,
    ) -> Result<Box<dyn Run>, OpError>;

    /// Start removing `path`, a path and the node it was listed as, and
    /// everything beneath a directory.
    ///
    /// # Errors
    ///
    /// The refusal opening the directory holding it.
    fn remove(&mut self, path: (&str, FileId)) -> Result<Box<dyn Run>, OpError>;
}
