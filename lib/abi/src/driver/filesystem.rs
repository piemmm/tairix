//! Filesystem driver class (`drivers/filesystem/*`).
//!
//! A filesystem driver attaches a block-backed image to a mount point
//! and detaches it again. Path resolution, permission enforcement,
//! and the VFS itself live in `kernel/core` (Stage 5); this trait
//! exposes only the operations the host needs in order to wire a
//! driver into the mount table.
//!
//! The block-device side of the mount lives behind the
//! [`block::Block`](crate::driver::block::Block) trait; this module
//! does not duplicate it.

use super::{DriverError, DriverHandle};
use crate::fs::SeekRegion;
use crate::time::Time64;
use crate::CapabilityId;

/// Mount-flag bitmap, frozen at `abi-v1`.
///
/// Unknown bits must be zero; a non-zero unknown bit causes
/// [`DriverError::OutOfRange`] at mount time.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
pub struct MountFlags(u32);

impl MountFlags {
    /// Mount the filesystem read-only.
    pub const READ_ONLY: Self = Self(1 << 0);
    /// Refuse to run a program the mount serves.
    pub const NOEXEC: Self = Self(1 << 1);

    /// Bitwise OR of every flag defined in `abi-v1`. Any bit outside
    /// this mask is reserved and rejected on the wire.
    pub const KNOWN_MASK: Self = Self(Self::READ_ONLY.0 | Self::NOEXEC.0);

    /// Raw bitmap.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Construct a [`MountFlags`] from a raw bitmap.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::OutOfRange`] if `raw` has bits set
    /// outside [`Self::KNOWN_MASK`].
    ///
    /// # Capabilities
    ///
    /// None.
    pub const fn from_bits(raw: u32) -> Result<Self, DriverError> {
        if raw & !Self::KNOWN_MASK.0 != 0 {
            return Err(DriverError::OutOfRange);
        }
        Ok(Self(raw))
    }

    /// Returns `true` iff every bit in `other` is set in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The set of flags present in either `self` or `other`.
    ///
    /// Both operands are already-validated [`MountFlags`], so the union
    /// stays within [`Self::KNOWN_MASK`] and needs no re-validation.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Trait every filesystem driver implements.
///
/// # Capabilities
///
/// The trait's `mount` / `unmount` methods are gated by
/// [`CapabilityId::FS_MOUNT`](crate::CapabilityId::FS_MOUNT) at the
/// dispatch site, on top of the load-time
/// [`CapabilityId::DRV_LOAD`](crate::CapabilityId::DRV_LOAD) check.
pub trait Filesystem {
    /// Attach a backing image and bring the filesystem online.
    ///
    /// `source_block_handle` is the kernel-issued
    /// [`DriverHandle`] for the
    /// underlying [`block::Block`](crate::driver::block::Block) device,
    /// not a path — path resolution belongs to the VFS, not to this
    /// trait.
    ///
    /// # Errors
    ///
    /// * [`DriverError::PermissionDenied`] if the dispatcher has not
    ///   verified `CAP_FS_MOUNT`.
    /// * [`DriverError::BadMagic`] if the on-disk filesystem
    ///   superblock fails validation.
    /// * [`DriverError::DeviceFault`] if the block device reports an
    ///   unrecoverable read error.
    /// * [`DriverError::Busy`] if the filesystem is already mounted.
    ///
    /// # Capabilities
    ///
    /// Requires [`CapabilityId::FS_MOUNT`](crate::CapabilityId::FS_MOUNT).
    fn mount(
        &mut self,
        source_block_handle: crate::driver::DriverHandle,
        flags: MountFlags,
    ) -> Result<(), DriverError>;

    /// Detach the filesystem.
    ///
    /// Implementations must flush any in-flight write before
    /// returning success.
    ///
    /// # Errors
    ///
    /// * [`DriverError::PermissionDenied`] if the dispatcher has not
    ///   verified `CAP_FS_MOUNT`.
    /// * [`DriverError::Busy`] if the filesystem still has open
    ///   files.
    /// * [`DriverError::NotFound`] if the filesystem is not mounted.
    ///
    /// # Capabilities
    ///
    /// Requires [`CapabilityId::FS_MOUNT`](crate::CapabilityId::FS_MOUNT).
    fn unmount(&mut self) -> Result<(), DriverError>;
}

/// Opaque identifier for a node (file or directory) within a single
/// mounted filesystem.
///
/// A `NodeId` is minted by a [`FilesystemRead`] implementation and is
/// meaningful only to the implementation that issued it; the VFS treats
/// it as an opaque token. The all-zero value is reserved as
/// [`NodeId::NONE`] and is never returned for a live node.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct NodeId(u64);

impl NodeId {
    /// The reserved "no node" sentinel.
    pub const NONE: Self = Self(0);

    /// Wrap a driver-defined raw value as a [`NodeId`].
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The driver-defined raw value behind this [`NodeId`].
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// The kind of a filesystem node.
///
/// A closed set: a format that stores a kind this does not name reports
/// [`DriverError::Unsupported`] rather than mapping it onto the nearest
/// match, so an unrecognised on-disk kind can never be mistaken for a
/// directory the VFS would descend or a file it would read.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum NodeKind {
    /// A directory whose children are enumerable with
    /// [`FilesystemRead::read_dir`].
    Directory = 0,
    /// A regular file whose bytes are readable with
    /// [`FilesystemRead::read_at`].
    RegularFile = 1,
    /// A symbolic link whose target path is read with
    /// [`FilesystemRead::read_link`] and which is created only by
    /// [`FilesystemWrite::create_link`] — never by
    /// [`FilesystemWrite::create`], which carries no target to store.
    Symlink = 2,
}

/// How a format matches a looked-up name against the entries it stores.
///
/// A consumer that keys anything on a name — a change journal, a lookup
/// cache — is only exact on a format whose lookup is exact: on a folding
/// format a lookup can resolve a spelling other than the one stored.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NameMatching {
    /// A name matches only the entry stored with exactly the same bytes.
    Exact,
    /// ASCII letters match regardless of case (FAT, `FileCore`).
    AsciiCaseInsensitive,
}

impl NameMatching {
    /// Whether `a` and `b` name the same entry under this rule — the one
    /// definition every consumer of the rule compares through.
    #[must_use]
    pub fn matches(self, a: &[u8], b: &[u8]) -> bool {
        match self {
            Self::Exact => a == b,
            Self::AsciiCaseInsensitive => a.eq_ignore_ascii_case(b),
        }
    }
}

/// How long a node keeps the [`NodeId`] a lookup answered for it.
///
/// A consumer that keeps a name's node across operations — a lookup cache —
/// keeps none on a format whose ids follow its files' contents.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NodeIdentity {
    /// A node is reported under one id for as long as it keeps its name, and
    /// a pin does not change it (ARXFS, ext4, UDF, ISO 9660).
    Stable,
    /// The id a node is reported under may change after a write, a truncate,
    /// a rename, a pin or the last unpin: FAT32 and ADFS report a pinned node
    /// under a token, and ADFS names any other object by where its data lies,
    /// so a first allocation, a relocation and a truncate to nothing move it.
    ContentDerived,
}

/// Structural metadata about a node, returned by
/// [`FilesystemRead::node_info`].
///
/// This is *structural* information only — its `size` and `kind` come
/// from the on-disk layout. Ownership, mode bits, ACLs, and the
/// capability gate live in the VFS metadata, not here; a read driver
/// never makes a permission decision (the VFS is
/// the policy point, the driver is raw structural I/O).
#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NodeInfo {
    /// Whether the node is a directory or a regular file.
    pub kind: NodeKind,
    /// How many directory entries name this node — POSIX `st_nlink`, read
    /// from the format and never derived.
    ///
    /// A format that records no count reports `1`: a node reached through a
    /// directory entry has at least the one name the caller just walked, and
    /// that is a fact rather than a guess. A held node no entry names reports
    /// `0` ([`FilesystemRead::pin`]). A directory's count includes its own `.`
    /// and each child's `..`, so an empty directory is `2`.
    ///
    /// This is the driver's answer because only the format knows it. The VFS
    /// carries it up to [`FileStat`](crate::fs::FileStat) unchanged rather
    /// than counting names itself, which it could not do without walking
    /// every directory on the volume.
    pub nlink: u32,
    /// The node's number as `stat` reports it, what
    /// [`FileId::node`](crate::fs::FileId::node) holds: the format's own
    /// number for it (an inode, a record address, an entry's location).
    ///
    /// Equal to the raw value of the node's natural id, the one lookups and
    /// listings answer while no pin holds it; a format that reports a pinned
    /// node under a token keeps this number for it.
    pub ino: u64,
    /// File length in bytes. Always `0` for a directory.
    pub size: u64,
    /// Bytes of on-disk storage the node's data occupies — the real
    /// allocation the format tracks (ext4 `i_blocks`, a FAT cluster
    /// chain, `ARXFS` mapped extents), never a value derived from `size`
    /// when the format knows better. `0` for a node whose data occupies
    /// no dedicated blocks (an empty file, or a directory whose entries
    /// live in shared metadata structures).
    pub allocated: u64,
    /// The node's four timestamps, read from the format in the *same*
    /// structural read as `kind`/`size` — so a caller never pays a second
    /// inode read (nor a second on-disk walk) to learn a node's times. A
    /// stamp the format does not keep is [`Time64::UNIX_EPOCH`] (ARXFS, for
    /// instance, tracks no access time), never a fabricated wall time.
    pub times: NodeTimes,
    /// The node's content generation: a value the volume assigns to each
    /// version of the node's data and never hands out twice — not after a
    /// crash, and not after a fall-back to an older committed root — so the
    /// node's identity and this name exactly one version of its content.
    /// Metadata changes leave it alone and no caller can set it.
    ///
    /// [`Self::NO_CONTENT_GEN`] where the format keeps none (ext4, FAT32,
    /// ADFS), and a consumer then treats no value as exact.
    pub content_gen: u64,
}

impl NodeInfo {
    /// The [`content_gen`](Self::content_gen) a format that keeps none
    /// reports: no version of any content is ever named by it.
    pub const NO_CONTENT_GEN: u64 = 0;

    /// The [`nlink`](Self::nlink) a format that records no per-node name
    /// count reports.
    ///
    /// Such a format also has no second-name object, so one name is not a
    /// floor it might exceed — it is the whole truth: the caller reached the
    /// node through exactly one directory entry and no other can exist. The
    /// one definition every such driver reads, so two of them cannot drift
    /// into disagreeing about what "no count" means.
    pub const SINGLE_NAME: u32 = 1;
}

/// A single entry yielded by [`FilesystemRead::read_dir`].
///
/// The entry's name is written into the caller-provided buffer; this
/// struct carries the entry's identity, its structural metadata, the
/// number of name bytes written, and the cursor that resumes iteration
/// after it, keeping the read surface allocation-free.
///
/// The entry carries the child's full [`NodeInfo`] because the driver has
/// the child's metadata in hand while producing the entry: a listing
/// consumer (`du`, `ls`) would otherwise re-resolve every child by path —
/// a fresh full walk per entry on an uncached, authenticated volume.
///
/// The same reasoning carries the child's timestamps ([`NodeInfo::times`]):
/// the listing driver has already read the child's inode or directory
/// record, and a format such as FAT stores the stamps *only* in the
/// parent's directory record, so the listing is the one place every driver
/// can report them without a second walk.
#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DirEntry {
    /// The child node's identifier.
    pub node: NodeId,
    /// The child's structural metadata (kind, size, allocated bytes, and
    /// its four timestamps). The listing driver has already read the
    /// child's inode or directory record, so the stamps come free with the
    /// entry — a listing consumer (`ls -lt`, a file manager) reads them
    /// from [`NodeInfo::times`] rather than re-`stat`ing each child.
    pub info: NodeInfo,
    /// Opaque cursor resuming iteration at the entry *after* this one:
    /// pass it back to [`FilesystemRead::read_dir`], with this entry's name,
    /// to continue the listing without rescanning from the start.
    pub next_cursor: u64,
}

/// What a [`FilesystemRead::read_dir`] visitor does with the entry it is
/// handed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DirVisit {
    /// Take this entry and hand over the next.
    Take,
    /// Leave this entry unread and end the call: the listing resumes at it.
    Stop,
}

/// Read-only structural access to a mounted filesystem.
///
/// This is the **versioned `abi-v1` extension** the VFS uses to
/// delegate path-resolution I/O to a block-backed
/// `drivers/filesystem/*` driver. It is deliberately a *separate*
/// trait from [`Filesystem`] (which remains mount/unmount only and
/// frozen): new behaviour ships as a new trait, never by widening a
/// shipped one. [`FilesystemWrite`] is the mutating surface.
///
/// Implementations expose raw structural access and make **no**
/// permission decisions: the VFS authorises every traversal against
/// the model before calling here. Names are
/// raw on-disk bytes; case-folding and Unicode normalisation policy
/// belong to the VFS, not the driver.
///
/// # Holds
///
/// [`pin`](Self::pin) holds a node for the kernel. While a node is held,
/// every id the driver reports for it — a lookup, a listing,
/// [`parent`](Self::parent), the root — is the id `pin` answered.
///
/// A held node that loses its last name — a removal, or a rename replacing
/// it — is kept with its data until its last [`unpin`](Self::unpin) frees it.
/// Until then it takes byte reads and writes, truncates, `node_info`, and
/// security and attribute reads and changes, while [`FilesystemWrite::link`]
/// refuses it. A held directory that loses its name takes no entries: a
/// lookup in it, its `parent`, and a create, link or rename into it answer
/// [`DriverError::NotFound`], and it lists nothing.
///
/// A node no name and no hold reaches is [`DriverError::NotFound`] to every
/// call.
///
/// # Capabilities
///
/// Calls are reached only through the kernel-issued
/// [`DriverHandle`] the host minted at
/// load time ([`CapabilityId::DRV_LOAD`](crate::CapabilityId::DRV_LOAD)).
pub trait FilesystemRead {
    /// The identifier of the filesystem's root directory.
    fn root(&self) -> NodeId;

    /// How [`lookup`](Self::lookup) matches a name. Required rather than
    /// defaulted: only the format knows, and a guess of
    /// [`NameMatching::Exact`] would be wrong silently.
    fn name_matching(&self) -> NameMatching;

    /// How long a node keeps its id. Required for the same reason as
    /// [`name_matching`](Self::name_matching): only the format knows.
    fn node_identity(&self) -> NodeIdentity;

    /// Report the structural metadata of `node`.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] if `node` does not name a live node.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block read.
    fn node_info(&mut self, node: NodeId) -> Result<NodeInfo, DriverError>;

    /// Resolve a single path component `name` within directory `dir`.
    ///
    /// `name` is a single component: it contains no path separator and
    /// is neither `.` nor `..` (the VFS resolves those itself).
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if `dir` is not a directory.
    /// * [`DriverError::NotFound`] if no child named `name` exists.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block read.
    fn lookup(&mut self, dir: NodeId, name: &[u8]) -> Result<NodeId, DriverError>;

    /// The directory holding directory `dir`'s name — its `..` as the format
    /// records it. The root answers itself.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] if `dir` names no live directory (one
    ///   removed included).
    /// * [`DriverError::Unsupported`] if `dir` is not a directory.
    /// * [`DriverError::DeviceFault`] on an unrecoverable read.
    fn parent(&mut self, dir: NodeId) -> Result<NodeId, DriverError>;

    /// Read up to `buf.len()` bytes from `file` starting at byte
    /// `offset`, returning the number of bytes read.
    ///
    /// A return value shorter than `buf.len()` indicates end-of-file;
    /// reading at or past the file's size returns `0`.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if `file` is a directory.
    /// * [`DriverError::NotFound`] if `file` does not name a live node.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block read.
    fn read_at(&mut self, file: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize, DriverError>;

    /// Read the target path of the symbolic link `link` into `out`,
    /// returning its length in bytes.
    ///
    /// The target is returned exactly as stored — not resolved, not
    /// normalised, and with no terminator — because resolution is the VFS's
    /// policy decision, made component by component under the caller's
    /// attested identity, never the driver's.
    ///
    /// A format that has no symbolic links leaves this defaulted, so it
    /// refuses rather than inventing a target.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if the format stores no links, or if
    ///   `link` is not a [`NodeKind::Symlink`].
    /// * [`DriverError::NotFound`] if `link` does not name a live node.
    /// * [`DriverError::BufferTooSmall`] if the target does not fit `out`.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block read.
    fn read_link(&mut self, link: NodeId, out: &mut [u8]) -> Result<usize, DriverError> {
        let _ = (link, out);
        Err(DriverError::Unsupported)
    }

    /// Where the next region of `region`'s kind begins at or after byte
    /// `offset` of the regular file `file`, or `None` when none does: the
    /// offset is at or past the end, or only holes follow it.
    ///
    /// A format that keeps no holes leaves this defaulted, and its files are
    /// data throughout ([`SeekRegion::in_solid_file`]). A wrapper forwards
    /// it, since in a wrapper the default would hide the holes of the driver
    /// beneath.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if `file` is not a regular file.
    /// * [`DriverError::NotFound`] if `file` does not name a live node.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block read.
    fn seek_region(
        &mut self,
        file: NodeId,
        offset: u64,
        region: SeekRegion,
    ) -> Result<Option<u64>, DriverError> {
        let info = self.node_info(file)?;
        if info.kind != NodeKind::RegularFile {
            return Err(DriverError::Unsupported);
        }
        Ok(region.in_solid_file(offset, info.size))
    }

    /// Hand `visit` the children of directory `dir` in order, from `cursor`
    /// on, each with its name, until it answers [`DirVisit::Stop`] or the
    /// directory ends.
    ///
    /// `cursor` is an **opaque resume token** (the `getdents` `d_off`
    /// model): `0` starts the listing, and each entry carries the
    /// [`DirEntry::next_cursor`] that resumes after it. `after` is the name of
    /// the entry that cursor came from, empty at the start; a format whose
    /// entries move when others are added or removed resumes from it rather
    /// than from a position. One call reads each directory block it crosses
    /// once, however many entries the block holds.
    ///
    /// A listing may span calls between which the directory changes, and it
    /// keeps the POSIX stream contract across them: an entry present for the
    /// whole listing is handed over exactly once, while one added or removed
    /// meanwhile may or may not be. Every format's tokens must uphold it.
    ///
    /// A token, whether stale or never returned, is confined to `dir`: it is
    /// bounds-checked against that directory and never reads outside it — an
    /// out-of-range one simply ends the listing, never undefined behaviour.
    /// A corrupt directory still ends: no sequence of tokens revisits a
    /// position.
    ///
    /// Iteration order is the implementation's stable on-disk order. Entries
    /// handed over before an error were read; the caller decides whether a
    /// partial batch is usable.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if `dir` is not a directory.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block read or a
    ///   malformed directory.
    fn read_dir(
        &mut self,
        dir: NodeId,
        cursor: u64,
        after: &[u8],
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError>;

    /// Hold `node` for the kernel. The id returned names this object until
    /// the matching [`unpin`](Self::unpin): through renames of it and of its
    /// ancestors, writes and truncates, and the loss of its last name; no
    /// other node is given it within the mount while it is held. Holds count:
    /// every `pin` is matched by one `unpin`, and every hold of one node
    /// answers the same id.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] if `node` names no live node.
    /// * [`DriverError::OutOfMemory`] if the hold cannot be recorded.
    /// * [`DriverError::DeviceFault`] on an unrecoverable read.
    fn pin(&mut self, node: NodeId) -> Result<NodeId, DriverError>;

    /// Release one hold of `pinned`, an id [`pin`](Self::pin) answered. When
    /// the last hold of a node no name reaches goes, the node is freed here.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] if no hold of `pinned` is outstanding.
    /// * [`DriverError::DeviceFault`] if freeing the node failed; the hold is
    ///   released all the same.
    fn unpin(&mut self, pinned: NodeId) -> Result<Unpinned, DriverError>;

    /// Release every hold, freeing each held node no name reaches: the step
    /// before a forced detach.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] if a free failed; every hold is released
    /// all the same.
    fn release_pins(&mut self) -> Result<(), DriverError>;
}

/// What a [`FilesystemRead::unpin`] left behind.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Unpinned {
    /// Other holds of the node remain.
    Held,
    /// That was the last hold; the node keeps a name.
    Released,
    /// That was the last hold of a node no name reached, which is now freed.
    Freed,
}

/// What the host does for a mounted volume that the driver, which owns no
/// thread and runs only inside a caller's operation, cannot: run its
/// write-back timer, tell it the time, and pass on the news that its space ran
/// into its reserve.
///
/// A filesystem may keep one transaction open across several operations, so
/// that the commit — its barrier, its root, and the metadata blocks every
/// operation in it rewrote — costs once per burst rather than once per
/// operation. That trades recency, which is only bounded if something
/// publishes the transaction when the volume falls quiet. So the host runs the
/// timer and the driver names the instant — the only direction that cannot
/// lose a transaction, since the driver alone knows one is open.
pub trait VolumeHost: Send + Sync {
    /// Monotonic nanoseconds, or [`None`] where the host has no monotonic
    /// clock. A driver that cannot measure elapsed time publishes every
    /// operation instead of deferring against a clock it does not have.
    fn now_ns(&self) -> Option<u64>;

    /// The instant to stamp an operation with: the wall clock's reading, once
    /// a time source has set it and while that reading is plausible
    /// ([`crate::time::is_plausible_wall_time`]).
    ///
    /// [`None`] means the time is unknown. The driver then records its
    /// format's own "no stamp" spelling, so nothing a later reader would take
    /// for a real date reaches the medium. Called on the same terms as
    /// [`Self::writeback_due`].
    fn wall_time(&self) -> Option<Time64>;

    /// The volume registered as `volume` holds an open transaction due at
    /// absolute monotonic `deadline_ns`, or holds none (`None`).
    ///
    /// Called from inside the driver, under whatever lock serialises it, so
    /// an implementation records the deadline and returns: it must not park,
    /// perform I/O, or take a lock its caller may already hold.
    fn writeback_due(&self, volume: DriverHandle, deadline_ns: Option<u64>);

    /// The volume registered as `volume` has run out of space outside its
    /// reserve (`in_reserve`), or has room outside it again.
    ///
    /// Reported on the transition only, from inside the driver, on the same
    /// terms as [`Self::writeback_due`].
    fn reserve_crossed(&self, volume: DriverHandle, in_reserve: bool);
}

/// Who an allocating operation's space is for, which decides whether it may
/// draw on the volume's reserve once everything else is gone.
///
/// The class follows the operation's destination, never its writer: the
/// kernel sets it from the projection a write lands through, so a privileged
/// service writing into a user's home is ordinary, and nothing a user writes
/// can reach the reserve.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum AllocationClass {
    /// User data, the default: refused once only the reserve remains.
    #[default]
    Ordinary,
    /// The system's own bounded write set — its logs, its settings, its
    /// account databases — which may draw on the reserve.
    System,
}

/// Mutating structural access to a mounted filesystem.
///
/// This is the **versioned `abi-v1` extension** the VFS uses to delegate
/// write-path I/O to a block-backed `drivers/filesystem/*` driver, the
/// symmetric counterpart to [`FilesystemRead`]. Like that trait it is a
/// *separate* trait, never a widening of the frozen [`Filesystem`]
/// mount/unmount surface or of [`FilesystemRead`]: new behaviour ships as
/// a new trait.
///
/// # How a mutation names its target
///
/// A name mutation — [`create`](Self::create),
/// [`create_link`](Self::create_link), [`link`](Self::link),
/// [`remove`](Self::remove) and [`rename`](Self::rename) — changes a
/// directory's entries, so it takes a (`dir`, `name`) pair. `name` is a
/// single path component containing no separator and is neither `.` nor
/// `..` (the VFS resolves those itself). A byte write —
/// [`write_at`](Self::write_at), [`truncate`](Self::truncate) and
/// [`punch`](Self::punch) — takes the node it changes.
///
/// A pinned id ([`FilesystemRead::pin`]) is valid for a byte write until its
/// last unpin. An id a lookup or listing answered for an unheld node is valid
/// only until the next name mutation in its directory, and on a
/// [`NodeIdentity::ContentDerived`] format only until the next mutation of
/// the node, so a write through one resolves the name under the same hold of
/// the driver's lock.
///
/// A create hands over the whole record the node is born with, and a format
/// keeps what of it it can store.
///
/// # No permission decisions
///
/// As with [`FilesystemRead`], implementations expose raw structural
/// mutation and make **no** permission decision: the VFS authorises every
/// write against the model before calling here.
///
/// # Capabilities
///
/// Calls are reached only through the kernel-issued
/// [`DriverHandle`] the host minted at load
/// time, and the VFS additionally requires the mount to be writable (a
/// mount carrying [`MountFlags::READ_ONLY`] is never delegated a write).
pub trait FilesystemWrite {
    /// Create an empty child `name` of kind `kind` in directory `dir`,
    /// returning the new node's [`NodeId`].
    ///
    /// The child is created with zero length and (for a directory) with
    /// its `.`/`..` links in place, born with `record` wherever the format
    /// stores ownership and mode; a format that stores none ignores it, and
    /// its crate docs say so. Implementations need not allocate on-disk data
    /// for a zero-length regular file.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if `dir` is not a directory, or if
    ///   `kind` is [`NodeKind::Symlink`] — a link carries a target this
    ///   call has nowhere to put, so it is created only by
    ///   [`create_link`](Self::create_link) and never as a side effect of
    ///   an empty-child create.
    /// * [`DriverError::NotFound`] if `dir` is a held directory that has lost
    ///   its name.
    /// * [`DriverError::AlreadyExists`] if a child named `name` already
    ///   exists.
    /// * [`DriverError::LengthOutOfRange`] if `name` is empty or longer
    ///   than the filesystem's maximum component length.
    /// * [`DriverError::NoSpace`] if the volume cannot allocate the inode
    ///   (or a directory's initial data block) because it is full.
    /// * [`DriverError::DeviceFault`] if a block write fails.
    fn create(
        &mut self,
        dir: NodeId,
        name: &[u8],
        kind: NodeKind,
        record: &NodeSecurity,
    ) -> Result<NodeId, DriverError>;

    /// Create a symbolic link named `name` in directory `dir` whose stored
    /// target is `target`, returning the new node's [`NodeId`].
    ///
    /// `target` is stored verbatim; the driver neither resolves nor
    /// validates it as a path, so a link may legitimately dangle. The VFS
    /// bounds its length before the call. The link is born with `record` as
    /// [`create`](Self::create)'s node is.
    ///
    /// A format that has no symbolic links leaves this defaulted, so it
    /// refuses creation rather than substituting a copy or an empty file.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if the format stores no links, or if
    ///   `dir` is not a directory.
    /// * [`DriverError::NotFound`] if `dir` is a held directory that has lost
    ///   its name.
    /// * [`DriverError::AlreadyExists`] if a child named `name` already
    ///   exists.
    /// * [`DriverError::LengthOutOfRange`] if `name` is empty or longer than
    ///   the filesystem's maximum component length, or if `target` is empty
    ///   or longer than the format can store.
    /// * [`DriverError::NoSpace`] if the volume cannot allocate the node.
    /// * [`DriverError::DeviceFault`] if a block write fails.
    fn create_link(
        &mut self,
        dir: NodeId,
        name: &[u8],
        target: &[u8],
        record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        let _ = (dir, name, target, record);
        Err(DriverError::Unsupported)
    }

    /// Add `name` in directory `dir` as a second directory entry for the
    /// existing node `node` — a hard link — raising the node's
    /// [`NodeInfo::nlink`] count by one.
    ///
    /// The node gains a name, not a copy: both entries reach one inode, so a
    /// write through either is visible through the other, and the node's
    /// storage survives until the *last* name is unlinked and no hold
    /// remains. A driver that
    /// implements this must therefore make [`remove`](Self::remove) decrement
    /// the count and free only at zero — implementing one without the other
    /// leaks storage or frees data another name still reaches.
    ///
    /// `dir` and `node` are on the same mounted volume: a directory entry
    /// cannot address an inode in another backing, and the VFS refuses a
    /// cross-volume pair before delegating. The VFS likewise refuses a
    /// directory before the call; a driver still checks, because it owns the
    /// invariant that its own tree stays a tree.
    ///
    /// A format with no second-name concept leaves this defaulted and
    /// refuses, rather than substituting a copy that would silently diverge
    /// from the original on the next write.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if the format stores only one name per
    ///   node, if `dir` is not a directory, or if `node` is a directory.
    /// * [`DriverError::AlreadyExists`] if a child named `name` already
    ///   exists.
    /// * [`DriverError::NotFound`] if `node` names no node with a name left,
    ///   or `dir` is a held directory that has lost its name.
    /// * [`DriverError::LengthOutOfRange`] if `name` is empty or longer than
    ///   the filesystem's maximum component length.
    /// * [`DriverError::TooManyLinks`] if the node already carries as many
    ///   names as the format can record — a fixed on-disk bound, so the
    ///   create fails closed rather than wrapping the count.
    /// * [`DriverError::NoSpace`] if the volume cannot grow the directory.
    /// * [`DriverError::DeviceFault`] if a block write fails.
    fn link(&mut self, dir: NodeId, name: &[u8], node: NodeId) -> Result<(), DriverError> {
        let _ = (dir, name, node);
        Err(DriverError::Unsupported)
    }

    /// Write `data` into the regular file `file` starting at byte `offset`,
    /// returning the number of bytes written.
    ///
    /// Writing past the current end of the file extends it, allocating
    /// backing storage as needed and updating the recorded length. A
    /// write whose `offset` is beyond the current length zero-fills the
    /// gap.
    ///
    /// **The count may be short of `data`.** An implementation is free to
    /// store fewer bytes and report how many, exactly as `write(2)` is: a
    /// driver that bounds the memory one call may pin uses a short count as
    /// its back-pressure, so a caller that needs every byte on the volume
    /// loops — or calls [`write_all`](Self::write_all), which loops once for
    /// everybody.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if `file` is a directory, or a link the
    ///   format refuses to write through.
    /// * [`DriverError::NotFound`] if `file` names no live node.
    /// * [`DriverError::NoSpace`] if the volume cannot allocate a data
    ///   block to back the write because it is full.
    /// * [`DriverError::DeviceFault`] if a block write fails.
    fn write_at(&mut self, file: NodeId, offset: u64, data: &[u8]) -> Result<usize, DriverError>;

    /// Write the whole of `data` into the regular file `file` starting at
    /// byte `offset`, resuming across short writes.
    ///
    /// [`write_at`](Self::write_at) may legitimately store less than it was
    /// handed, so every caller that needs the whole value on the volume — a
    /// settings document, a key file, a planted image payload, an account
    /// database — has to loop. Looping here rather than at each of those
    /// call sites is what keeps them from disagreeing about it, and what
    /// stops a caller mistaking back-pressure for a failure.
    ///
    /// # Errors
    ///
    /// Whatever [`write_at`](Self::write_at) reports, plus
    /// [`DriverError::NoSpace`] if a call stores nothing while bytes remain:
    /// an implementation that makes no progress is refusing the write, and
    /// retrying it forever is not an answer.
    fn write_all(&mut self, file: NodeId, offset: u64, data: &[u8]) -> Result<(), DriverError> {
        let mut done = 0usize;
        while done < data.len() {
            let at = offset
                .checked_add(done as u64)
                .ok_or(DriverError::OutOfRange)?;
            let written = self.write_at(file, at, &data[done..])?;
            if written == 0 {
                return Err(DriverError::NoSpace);
            }
            done = done.checked_add(written).ok_or(DriverError::OutOfRange)?;
            if done > data.len() {
                // A driver claiming more than it was handed has lost track of
                // the write; fail closed rather than trust the count.
                return Err(DriverError::DeviceFault);
            }
        }
        Ok(())
    }

    /// Set the length of the regular file `file` to `size`, freeing or
    /// zero-extending its backing storage as needed.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if `file` is a directory, or a link the
    ///   format refuses to write through.
    /// * [`DriverError::NotFound`] if `file` names no live node.
    /// * [`DriverError::NoSpace`] if zero-extending the file cannot
    ///   allocate a data block because the volume is full.
    /// * [`DriverError::DeviceFault`] if a block write fails.
    fn truncate(&mut self, file: NodeId, size: u64) -> Result<(), DriverError>;

    /// Make bytes `[offset, offset + len)` of the regular file `file` read as
    /// zeros and release the storage they occupied, leaving the file's length
    /// unchanged: a punched hole.
    ///
    /// The range is clipped to the file's length — past the end there is
    /// nothing stored to release — and an empty range changes nothing. Part
    /// of a range the format cannot release, such as the partial ends of an
    /// allocation unit, is zeroed in place. A format that keeps no holes
    /// leaves this defaulted and refuses, never substituting a write of zeros
    /// that would keep the storage.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if the format keeps no holes, or
    ///   `file` is a directory.
    /// * [`DriverError::NotFound`] if `file` names no live node.
    /// * [`DriverError::OutOfRange`] if `offset + len` overflows.
    /// * [`DriverError::NoSpace`] if releasing the range needs metadata the
    ///   volume cannot allocate (an extent split).
    /// * [`DriverError::DeviceFault`] on an unrecoverable block access.
    fn punch(&mut self, file: NodeId, offset: u64, len: u64) -> Result<(), DriverError> {
        let _ = (file, offset, len);
        Err(DriverError::Unsupported)
    }

    /// Unlink the child `name` from directory `dir`, freeing its backing
    /// storage once no other name and no hold ([`FilesystemRead::pin`])
    /// reaches it.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if `dir` is not a directory.
    /// * [`DriverError::NotFound`] if no child named `name` exists.
    /// * [`DriverError::DirectoryNotEmpty`] if `name` is a non-empty
    ///   directory.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block write.
    fn remove(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError>;

    /// Move the child `src_name` of directory `src_dir` so that it becomes
    /// the child `dst_name` of directory `dst_dir`, preserving the moved
    /// node's contents and metadata. Its id survives the move only on a
    /// format that numbers its nodes: FAT names a file by where its entry
    /// lies, so a moved file answers a new one.
    ///
    /// `src_name` and `dst_name` are each a single path component
    /// containing no separator and neither `.` nor `..` (the VFS resolves
    /// those itself). `src_dir` and `dst_dir` may be the same directory (a
    /// pure within-directory rename) or different directories on the same
    /// mounted volume.
    ///
    /// # Replacing an existing destination
    ///
    /// If `dst_name` already names an entry in `dst_dir` it is atomically
    /// replaced, subject to kind compatibility: a regular file may replace
    /// a regular file, and a directory may replace an **empty** directory.
    /// The replaced node's storage is freed as [`remove`](Self::remove) frees
    /// it. Replacing a directory
    /// with a non-directory, or a non-directory with a directory, is
    /// refused.
    ///
    /// A rename whose source and destination name the *same* entry
    /// (`src_dir == dst_dir` and `src_name == dst_name`) succeeds and
    /// changes nothing.
    ///
    /// When a directory is moved to a different parent, its `..` link is
    /// repointed at `dst_dir` and both parents' link counts are adjusted.
    /// Moving a directory into itself or into its own subtree is refused
    /// (it would detach the cycle from the tree).
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if `src_dir` or `dst_dir` is not a
    ///   directory, or a kind-incompatible replacement is attempted
    ///   (a file over a directory, or a directory over a file).
    /// * [`DriverError::NotFound`] if `src_name` does not exist in
    ///   `src_dir`, or `dst_dir` is a held directory that has lost its name.
    /// * [`DriverError::DirectoryNotEmpty`] if `dst_name` is a non-empty
    ///   directory.
    /// * [`DriverError::DirectoryCycle`] if the move would place a directory
    ///   inside its own subtree.
    /// * [`DriverError::LengthOutOfRange`] if `dst_name` is empty or longer
    ///   than the filesystem's maximum component length.
    /// * [`DriverError::NoSpace`] if `dst_dir` cannot grow to hold the new
    ///   entry because the volume is full.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block write.
    fn rename(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
    ) -> Result<(), DriverError>;

    /// Make every write the filesystem has accepted durable on stable
    /// media, returning only once the backing device confirms it.
    ///
    /// This is the durability contract behind the `fs_sync` syscall. A
    /// driver must (1) push any metadata or data it still buffers to the
    /// block device, and (2) force the device's own volatile write cache
    /// to the medium by calling the backing device's
    /// [`Block::flush`](super::block::Block::flush). Writing "through" to
    /// the device synchronously is **not** sufficient: a completed block
    /// write only means the device *accepted* the bytes, which may still
    /// sit in its volatile cache until a flush commits them. A driver that
    /// returns `Ok(())` without forcing the device cache reports
    /// durability it has not delivered.
    ///
    /// A driver with no backing device (an in-RAM filesystem) or a
    /// read-only mount that never wrote has nothing to commit and returns
    /// `Ok(())` truthfully.
    ///
    /// # Errors
    ///
    /// * [`DriverError::DeviceFault`] if the device cannot confirm the
    ///   data is on stable media; the caller fails the sync closed rather
    ///   than reporting durability it cannot vouch for.
    fn flush(&mut self) -> Result<(), DriverError>;

    /// Install the host for this mount, registered under `volume`.
    ///
    /// Called once, by the host, as the mount is registered — so a driver
    /// that defers durability can only ever do so with a timer that will
    /// publish it. A driver that publishes at every operation has no
    /// deadline to report and leaves this defaulted; one that *does* defer
    /// overrides it and, from then on, reports each transaction's deadline
    /// through [`VolumeHost::writeback_due`] as the transaction opens and
    /// reports `None` as it closes.
    ///
    /// A driver never given a host must publish eagerly rather than hold a
    /// transaction nothing will close.
    fn set_volume_host(&mut self, volume: DriverHandle, host: &'static dyn VolumeHost) {
        let _ = (volume, host);
    }

    /// Set the class the operations that follow allocate under, until the
    /// next call.
    ///
    /// The kernel scopes each setting to one operation and puts
    /// [`AllocationClass::Ordinary`] back after it. A format that keeps no
    /// reserve leaves this defaulted; a wrapper forwards it.
    fn set_allocation_class(&mut self, class: AllocationClass) {
        let _ = class;
    }
}

/// Maximum number of inline ACL entries a [`NodeSecurity`] record carries.
///
/// Eight inline entries keep the record fixed-size and allocation-free,
/// matching the per-inode inline-ACL budget a `drivers/filesystem/*`
/// driver stores for the model.
pub const MAX_ACL_ENTRIES: usize = 8;

/// The principal a [`SecurityAcl`] entry grants rights to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SecuritySubject {
    /// The user with this uid.
    User(u32),
    /// The group with this gid (matched against a caller's primary and
    /// supplementary groups by the VFS).
    Group(u32),
}

/// One inline access-control-list entry of a node's security record.
///
/// `perms` is a POSIX-style `rwx` triad in its low three bits (`0b100`
/// read, `0b010` write, `0b001` execute/search) for `subject`. How it
/// composes with the mode bits is the record's [`AclModel`]: added to them,
/// or as a POSIX access-ACL entry under its mask.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SecurityAcl {
    /// The user or group the entry grants rights to.
    pub subject: SecuritySubject,
    /// The `rwx` permission bits granted to `subject`.
    pub perms: u8,
}

/// How a [`NodeSecurity`] record's ACL entries combine with its mode bits.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum AclModel {
    /// Each entry grants its permissions on top of the mode bits.
    #[default]
    Additive,
    /// A POSIX.1e access ACL, as ext4, XFS and btrfs store one: the owner is
    /// decided by the owner triad alone, a named user by its entry under
    /// `mask`, a member of the owning group or of a named group by the union
    /// of those entries under `mask` — and refused if they do not grant it,
    /// never falling through to the other triad — and anyone else by the
    /// other triad. `group_obj` is the owning group's own entry; the mode's
    /// group triad then mirrors `mask`. Each is an `rwx` triad.
    Posix {
        /// The owning group's entry.
        group_obj: u8,
        /// The ceiling on every named entry and on the group class.
        mask: u8,
    },
}

/// The complete security record a filesystem driver stores for one
/// node, surfaced to the VFS through [`FilesystemSecurity::security`].
///
/// This is an in-process policy record the VFS consumes, not a serialized
/// wire type: each `drivers/filesystem/*` driver owns its own on-disk
/// encoding and translates to and from this shape. The driver stores the
/// record but makes **no** permission decision from it (the VFS is the policy point).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NodeSecurity {
    /// POSIX mode bits (type bits are not stored here; see [`NodeKind`]).
    pub mode: u32,
    /// Owning user id.
    pub uid: u32,
    /// Owning group id.
    pub gid: u32,
    /// An optional capability the caller must hold to access the node at
    /// all, on top of the mode/ACL checks (`None` = no capability gate).
    pub required_cap: Option<CapabilityId>,
    /// How the ACL entries combine with the mode bits.
    pub acl_model: AclModel,
    acl: [SecurityAcl; MAX_ACL_ENTRIES],
    acl_len: usize,
}

impl NodeSecurity {
    /// A record owned by `(uid, gid)` with mode `mode`, no ACL entries,
    /// and no capability gate.
    #[must_use]
    pub const fn new(mode: u32, uid: u32, gid: u32) -> Self {
        Self {
            mode,
            uid,
            gid,
            required_cap: None,
            acl_model: AclModel::Additive,
            acl: [SecurityAcl {
                subject: SecuritySubject::User(0),
                perms: 0,
            }; MAX_ACL_ENTRIES],
            acl_len: 0,
        }
    }

    /// The record a format that stores no ownership reports for a node of
    /// `kind`: owned by the system user and group and writable by that owner
    /// alone. A removable volume's mount policy maps it for the volume's
    /// users ([`FilesystemSecurity::records_ownership`]).
    #[must_use]
    pub const fn unrecorded(kind: NodeKind) -> Self {
        let mode = match kind {
            NodeKind::Directory => 0o755,
            NodeKind::RegularFile | NodeKind::Symlink => 0o644,
        };
        Self::new(mode, 0, 0)
    }

    /// The record a node of `kind` is born with, owned by `(uid, gid)`, when
    /// its creator asks for no mode and keeps the initial umask: `0o755` for a
    /// directory, `0o644` for a file, and `0o777` for a link, whose own mode
    /// never gates access.
    #[must_use]
    pub const fn created(kind: NodeKind, uid: u32, gid: u32) -> Self {
        let mode = match kind {
            NodeKind::Directory => crate::fs::FS_NEW_DIR_MODE & !crate::fs::FS_INITIAL_UMASK,
            NodeKind::RegularFile => crate::fs::FS_NEW_FILE_MODE & !crate::fs::FS_INITIAL_UMASK,
            NodeKind::Symlink => crate::fs::FS_LINK_MODE,
        };
        Self::new(mode, uid, gid)
    }

    /// The node's ACL entries.
    #[must_use]
    pub fn acl(&self) -> &[SecurityAcl] {
        &self.acl[..self.acl_len]
    }

    /// Append an ACL entry.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] if the record already holds
    /// [`MAX_ACL_ENTRIES`] entries.
    pub fn push_acl(&mut self, entry: SecurityAcl) -> Result<(), DriverError> {
        if self.acl_len >= MAX_ACL_ENTRIES {
            return Err(DriverError::LengthOutOfRange);
        }
        self.acl[self.acl_len] = entry;
        self.acl_len += 1;
        Ok(())
    }
}

/// Per-node security access to a mounted filesystem.
///
/// This is a **versioned `abi-v1` extension** — a *separate* trait from
/// [`FilesystemRead`] / [`FilesystemWrite`], never a widening of either
/// nor of the frozen [`Filesystem`]; new behaviour ships as a new trait.
/// Every mounted driver implements it, and the VFS judges each node against
/// the record it reports. A format that stores the whole record per node
/// (ARXFS) reports and stores it; one that stores part of it (ext4's owner,
/// group, and mode) reports what it holds and refuses to store what it
/// cannot; one that stores none (FAT32) reports one fixed record for every
/// node.
///
/// The driver only *stores and reports* the record; it makes no
/// permission decision (the VFS is the policy point, and every caller of
/// [`set_security`](Self::set_security) — today the kernel's
/// `CAP_USER_ADMIN` account-administration engine — authorises the write
/// before delegating here).
///
/// # Capabilities
///
/// Calls are reached only through the kernel-issued
/// [`DriverHandle`] the host minted at load
/// time ([`CapabilityId::DRV_LOAD`](crate::CapabilityId::DRV_LOAD)).
pub trait FilesystemSecurity {
    /// Report the security record stored for `node`.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] if `node` does not name a live node.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block read.
    fn security(&mut self, node: NodeId) -> Result<NodeSecurity, DriverError>;

    /// Replace the security record stored for `node`.
    ///
    /// # Errors
    ///
    /// * [`DriverError::PermissionDenied`] if the volume is mounted
    ///   read-only — refused before any state is touched.
    /// * [`DriverError::NotFound`] if `node` does not name a live node.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block write.
    fn set_security(&mut self, node: NodeId, security: NodeSecurity) -> Result<(), DriverError>;

    /// Whether the format records an owner and mode for `node`, rather than
    /// [`security`](Self::security) reporting the fixed record a format
    /// that keeps none stands in with.
    ///
    /// A removable volume's mount policy grants its users access to the nodes
    /// whose ownership is not recorded, and leaves a recorded owner's rules
    /// alone — so one volume may hold both, as a UDF disc written by two
    /// systems does.
    ///
    /// # Errors
    ///
    /// As [`security`](Self::security).
    fn records_ownership(&mut self, node: NodeId) -> Result<bool, DriverError> {
        self.security(node).map(|_| true)
    }
}

/// The four timestamps stored for a filesystem node.
///
/// Every field is a 64-bit-native [`Time64`]: absolute
/// time is never a seconds-only scalar, so the full pre-1970 and
/// post-2038 range round-trips without truncation. The four instants
/// follow the POSIX model:
///
/// * `created` — set once when the node is created and never changed.
/// * `modified` — last change to the node's *contents* (mtime).
/// * `accessed` — last access to the node's contents (atime).
/// * `changed` — last change to the node's *metadata* (ctime).
#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub struct NodeTimes {
    /// Creation instant; set once and never changed.
    pub created: Time64,
    /// Last contents-modification instant (mtime).
    pub modified: Time64,
    /// Last access instant (atime).
    pub accessed: Time64,
    /// Last metadata-change instant (ctime).
    pub changed: Time64,
}

/// Per-node extended-attribute access to a mounted filesystem.
///
/// This is a **versioned `abi-v1` extension** — a *separate* trait from
/// [`FilesystemRead`] / [`FilesystemWrite`] / [`FilesystemSecurity`],
/// never a widening of any of them nor of the frozen
/// [`Filesystem`]; new behaviour ships as a new trait. A driver whose on-disk
/// format can hold a general-purpose, namespaced `key → value` store per inode
/// implements it so the VFS can offer extended attributes and preserve foreign
/// per-file metadata (Acorn/Amiga/Atari/Mac) across a copy; a driver whose
/// format has nowhere to keep them simply does not implement it.
///
/// # The key grammar and bounds live in `lib/fsmeta`
///
/// A `key` is a namespaced, byte-for-byte case-sensitive
/// `tairix_fsmeta`-grammar key (`namespace.rest`, e.g. `acorn.filetype`). The
/// driver validates every key and value against that shared grammar and the
/// fixed security bounds and **fails closed** on any violation — an unknown
/// namespace, a malformed key, or an oversize value is rejected, never stored.
/// Values are opaque bytes; the driver never interprets them.
///
/// # No permission decisions
///
/// As with the sibling traits, the driver makes **no** permission decision:
/// the VFS authorises every attribute operation against the model before
/// calling here. A key's *namespace* decides its access class — the `user`,
/// `acorn`, `amiga`, `atari`, `mac`, and `tairix` namespaces are ordinary file
/// metadata governed by the file's own owner/mode/ACL, while the `system` and
/// `trusted` namespaces guard a security boundary the VFS gates with a
/// capability before delegating. A caller not permitted a namespace never
/// reaches the driver for it, and [`list_attr`](FilesystemAttrs::list_attr)
/// enumerates only keys whose namespace the caller may read.
///
/// # Buffers, not allocation
///
/// [`get_attr`](FilesystemAttrs::get_attr) and
/// [`list_attr`](FilesystemAttrs::list_attr) write into a caller-provided
/// buffer and report the byte count, mirroring
/// [`read_dir`](FilesystemRead::read_dir); a value or key that does not fit is
/// [`DriverError::BufferTooSmall`], not a truncation.
///
/// # Capabilities
///
/// Calls are reached only through the kernel-issued
/// [`DriverHandle`] the host minted at load
/// time ([`CapabilityId::DRV_LOAD`](crate::CapabilityId::DRV_LOAD)), and the
/// VFS additionally requires the mount to be writable for
/// [`set_attr`](FilesystemAttrs::set_attr) /
/// [`remove_attr`](FilesystemAttrs::remove_attr) (a mount carrying
/// [`MountFlags::READ_ONLY`] is never delegated a mutation).
pub trait FilesystemAttrs {
    /// Read the value of attribute `key` on `node` into `value_out`,
    /// returning the number of bytes written, or `None` if `node` carries no
    /// such attribute.
    ///
    /// # Errors
    ///
    /// * [`DriverError::OutOfRange`] if `key` is not a valid namespaced key.
    /// * [`DriverError::BufferTooSmall`] if the value does not fit `value_out`.
    /// * [`DriverError::NotFound`] if `node` does not name a live node.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block read.
    fn get_attr(
        &mut self,
        node: NodeId,
        key: &[u8],
        value_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError>;

    /// Set attribute `key` on `node` to `value`, inserting or replacing it in
    /// one copy-on-write transaction.
    ///
    /// # Errors
    ///
    /// * [`DriverError::OutOfRange`] if `key` is not a valid namespaced key
    ///   (unknown namespace, malformed bytes).
    /// * [`DriverError::LengthOutOfRange`] if `key` or `value` exceeds its
    ///   fixed bound.
    /// * [`DriverError::NoSpace`] if the attribute count, total attribute
    ///   bytes, or the metadata block would be exceeded.
    /// * [`DriverError::NotFound`] if `node` does not name a live node.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block read/write.
    fn set_attr(&mut self, node: NodeId, key: &[u8], value: &[u8]) -> Result<(), DriverError>;

    /// Yield the `index`-th attribute key of `node`, writing it into
    /// `key_out` and returning its length. Returns `Ok(None)` once `index` is
    /// past the last attribute, which is how a caller detects the end.
    ///
    /// Iteration order is the stable on-disk order.
    ///
    /// # Errors
    ///
    /// * [`DriverError::BufferTooSmall`] if the key does not fit `key_out`.
    /// * [`DriverError::NotFound`] if `node` does not name a live node.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block read.
    fn list_attr(
        &mut self,
        node: NodeId,
        index: u64,
        key_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError>;

    /// Remove attribute `key` from `node` in one copy-on-write transaction.
    ///
    /// # Errors
    ///
    /// * [`DriverError::OutOfRange`] if `key` is not a valid namespaced key.
    /// * [`DriverError::NotFound`] if `node` does not name a live node, or
    ///   carries no such attribute.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block read/write.
    fn remove_attr(&mut self, node: NodeId, key: &[u8]) -> Result<(), DriverError>;
}

/// The combined per-node view an attribute operation runs against: path
/// resolution ([`FilesystemRead`]), the per-inode security records the VFS
/// authorises with ([`FilesystemSecurity`]), the attribute store itself
/// ([`FilesystemAttrs`]), and the class a mutation of it allocates under
/// ([`FilesystemWrite`]) — one object, because the secured VFS must resolve
/// and authorise on the *same* driver it then reads or mutates.
///
/// Blanket-implemented for every type carrying the four traits; never
/// implemented by hand.
pub trait FilesystemAttrsFs:
    FilesystemRead + FilesystemSecurity + FilesystemAttrs + FilesystemWrite
{
}

impl<T: FilesystemRead + FilesystemSecurity + FilesystemAttrs + FilesystemWrite + ?Sized>
    FilesystemAttrsFs for T
{
}

/// Opt-in discovery of a driver's extended-attribute support.
///
/// [`FilesystemAttrs`] is deliberately implemented only by drivers whose
/// on-disk format can hold attributes, so a type-erased mount (the kernel's
/// `Box<dyn KernelFs>`) cannot require it as a bound. This facet is the
/// honest bridge: every mountable driver implements the *provider*, and the
/// default answer is `None` — "this format stores no attributes" — so an
/// `fs_attr_*` call on such a mount fails closed with a typed refusal. A
/// driver that does implement [`FilesystemAttrs`] overrides
/// [`attrs_fs`](Self::attrs_fs) to return itself; a caching or forwarding
/// wrapper delegates to its inner driver. The facet grants nothing: the VFS
/// still authorises every operation against the per-inode model before the
/// returned view is touched.
pub trait FilesystemAttrsProvider {
    /// The attribute-capable view of this driver, or `None` when its
    /// on-disk format has nowhere to store extended attributes.
    fn attrs_fs(&mut self) -> Option<&mut dyn FilesystemAttrsFs> {
        None
    }
}

/// The space accounting a mounted volume reports about itself.
///
/// Sizes are counted in whole blocks of `block_size` bytes — the unit the
/// mounted format actually allocates in — so a consumer multiplies rather
/// than guessing a divisor. Every count is 64-bit: a volume, and therefore
/// its block counts, may exceed what 32 bits (or pointer width) can hold.
///
/// `avail_blocks` is the portion of `free_blocks` an ordinary data
/// allocation may consume; a format that holds blocks back (e.g. a metadata
/// reserve that keeps a full volume repairable) reports the smaller number
/// here, so `avail_blocks <= free_blocks` always holds and a consumer is
/// never promised space the driver would refuse.
///
/// `files` / `files_free` report the volume's inode capacity for a format
/// with a fixed inode table. A format whose inodes are allocated dynamically
/// (`arxfs`) has no fixed capacity to report and carries `0` in both — the
/// honest "untracked" answer, never a fabricated total.
#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub struct VolumeStats {
    /// The allocation unit, in bytes, the block counts are denominated in.
    pub block_size: u32,
    /// The volume is down to its reserve: less than one operation's worth is
    /// left outside it, so an ordinary write may be refused while the
    /// system's own writes and deletes go on. Always `false` for a format
    /// that keeps no reserve.
    pub in_reserve: bool,
    /// Total data blocks the volume holds.
    pub total_blocks: u64,
    /// Blocks currently unallocated.
    pub free_blocks: u64,
    /// Blocks an ordinary data allocation may still consume
    /// (`free_blocks` minus any reserve the format withholds).
    pub avail_blocks: u64,
    /// Total inode capacity, or `0` when the format tracks no fixed table.
    pub files: u64,
    /// Free inodes, or `0` when the format tracks no fixed table.
    pub files_free: u64,
}

/// Whole-volume space statistics for a mounted filesystem.
///
/// This is a **versioned `abi-v1` extension** — a *separate* trait from
/// [`FilesystemRead`] / [`FilesystemWrite`] / [`FilesystemSecurity`] /
/// [`FilesystemAttrs`], never a widening of any
/// of them nor of the frozen [`Filesystem`]; new behaviour ships as a new
/// trait. Every mountable driver implements it: a volume that can be
/// mounted always has a size, and reporting it is a read of the driver's
/// own accounting, never a device walk.
///
/// The driver only *reports* the numbers; it makes no permission decision
/// (the query is authorised kernel-side before the driver is reached).
///
/// # Capabilities
///
/// Calls are reached only through the kernel-issued
/// [`DriverHandle`] the host minted at load
/// time ([`CapabilityId::DRV_LOAD`](crate::CapabilityId::DRV_LOAD)).
pub trait FilesystemStats {
    /// Report the volume's current space accounting.
    ///
    /// # Errors
    ///
    /// * [`DriverError::DeviceFault`] on an unrecoverable failure of the
    ///   underlying device (a driver that keeps its accounting in memory
    ///   never fails).
    fn stats(&mut self) -> Result<VolumeStats, DriverError>;
}

/// A wall clock for a filesystem driver's tests, so no driver's suite carries
/// its own [`VolumeHost`].
#[cfg(feature = "test-util")]
pub mod clock {
    extern crate alloc;

    use alloc::boxed::Box;

    use super::VolumeHost;
    use crate::time::Time64;
    use crate::DriverHandle;

    /// A host whose wall clock reads one fixed instant, or no time, and which
    /// has no monotonic clock — so a driver under it publishes every
    /// operation, exactly as with no host, and differs only in what it
    /// stamps. A test moves time on by installing another.
    pub struct TestClock(Option<Time64>);

    impl TestClock {
        /// A clock reading `wall`, leaked to the `'static` a driver keeps.
        #[must_use]
        pub fn at(wall: Option<Time64>) -> &'static Self {
            Box::leak(Box::new(Self(wall)))
        }

        /// The handle a test installs its one volume's host under.
        #[must_use]
        pub fn volume() -> DriverHandle {
            DriverHandle::from_raw(1).expect("a non-zero test handle")
        }
    }

    impl VolumeHost for TestClock {
        fn now_ns(&self) -> Option<u64> {
            None
        }

        fn wall_time(&self) -> Option<Time64> {
            self.0
        }

        fn writeback_due(&self, _volume: DriverHandle, _deadline_ns: Option<u64>) {}

        fn reserve_crossed(&self, _volume: DriverHandle, _in_reserve: bool) {}
    }
}

/// Listing fixtures for a filesystem driver's tests, so no driver's suite
/// carries its own walk of the [`FilesystemRead::read_dir`] visitor.
#[cfg(feature = "test-util")]
pub mod listing {
    extern crate alloc;

    use alloc::vec::Vec;

    use super::{DirEntry, DirVisit, FilesystemRead, NodeId};
    use crate::DriverError;

    /// Every entry of `dir` from `cursor` on, read in one call, with its
    /// name. `after` is the name `cursor` came from, empty at the start.
    ///
    /// # Errors
    ///
    /// Whatever the driver's `read_dir` refuses with.
    pub fn listed<F: FilesystemRead + ?Sized>(
        fs: &mut F,
        dir: NodeId,
        cursor: u64,
        after: &[u8],
    ) -> Result<Vec<(DirEntry, Vec<u8>)>, DriverError> {
        let mut out = Vec::new();
        fs.read_dir(dir, cursor, after, &mut |entry, name| {
            out.push((*entry, name.to_vec()));
            DirVisit::Take
        })?;
        Ok(out)
    }

    /// The first entry of `dir` from `cursor` on, with its name. The call
    /// stops at it, so it is the only entry the driver reads.
    ///
    /// # Errors
    ///
    /// Whatever the driver's `read_dir` refuses with.
    pub fn first_listed<F: FilesystemRead + ?Sized>(
        fs: &mut F,
        dir: NodeId,
        cursor: u64,
        after: &[u8],
    ) -> Result<Option<(DirEntry, Vec<u8>)>, DriverError> {
        let mut first = None;
        fs.read_dir(dir, cursor, after, &mut |entry, name| {
            first = Some((*entry, name.to_vec()));
            DirVisit::Stop
        })?;
        Ok(first)
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;
    use crate::driver::DriverHandle;

    #[test]
    fn known_mask_covers_every_named_flag() {
        let all = MountFlags::READ_ONLY.bits() | MountFlags::NOEXEC.bits();
        assert_eq!(MountFlags::KNOWN_MASK.bits(), all);
    }

    #[test]
    fn from_bits_rejects_unknown() {
        assert_eq!(MountFlags::from_bits(1 << 31), Err(DriverError::OutOfRange));
        assert_eq!(MountFlags::from_bits(1 << 2), Err(DriverError::OutOfRange));
    }

    #[test]
    fn union_combines_known_flags() {
        let combined = MountFlags::READ_ONLY.union(MountFlags::NOEXEC);
        assert!(combined.contains(MountFlags::READ_ONLY));
        assert!(combined.contains(MountFlags::NOEXEC));
        assert_eq!(combined, MountFlags::KNOWN_MASK);
    }

    #[test]
    fn contains_is_bitwise_subset() {
        let Ok(f) = MountFlags::from_bits(MountFlags::NOEXEC.bits()) else {
            unreachable!("known bits")
        };
        assert!(f.contains(MountFlags::NOEXEC));
        assert!(!f.contains(MountFlags::READ_ONLY));
    }

    struct MockFs {
        mounted: bool,
    }

    impl Filesystem for MockFs {
        fn mount(&mut self, _src: DriverHandle, _flags: MountFlags) -> Result<(), DriverError> {
            if self.mounted {
                return Err(DriverError::Busy);
            }
            self.mounted = true;
            Ok(())
        }

        fn unmount(&mut self) -> Result<(), DriverError> {
            if !self.mounted {
                return Err(DriverError::NotFound);
            }
            self.mounted = false;
            Ok(())
        }
    }

    #[test]
    fn mock_round_trip() {
        let Ok(handle) = DriverHandle::from_raw(1) else {
            unreachable!("1 is non-zero")
        };
        let mut fs = MockFs { mounted: false };
        assert!(fs.mount(handle, MountFlags::READ_ONLY).is_ok());
        assert_eq!(
            fs.mount(handle, MountFlags::READ_ONLY),
            Err(DriverError::Busy)
        );
        assert!(fs.unmount().is_ok());
        assert_eq!(fs.unmount(), Err(DriverError::NotFound));
    }

    #[test]
    fn name_matching_compares_by_its_own_rule() {
        assert!(NameMatching::Exact.matches(b"Foo", b"Foo"));
        assert!(!NameMatching::Exact.matches(b"Foo", b"foo"));
        assert!(NameMatching::AsciiCaseInsensitive.matches(b"Foo.TXT", b"foo.txt"));
        assert!(!NameMatching::AsciiCaseInsensitive.matches(b"foo", b"food"));
        // Only ASCII letters fold.
        assert!(!NameMatching::AsciiCaseInsensitive.matches("é".as_bytes(), "É".as_bytes()));
    }

    #[test]
    fn node_id_round_trips_raw_value() {
        let id = NodeId::from_raw(0xDEAD_BEEF);
        assert_eq!(id.raw(), 0xDEAD_BEEF);
        assert_eq!(NodeId::NONE.raw(), 0);
        assert_ne!(id, NodeId::NONE);
    }

    /// A fixed, allocation-free `FilesystemRead` over a root directory
    /// holding a single regular file `"readme"` with three bytes of
    /// content. Exercises the whole `abi-v1` read surface.
    struct MockReadFs;

    const ROOT: NodeId = NodeId::from_raw(1);
    const FILE: NodeId = NodeId::from_raw(2);
    const FILE_NAME: &[u8] = b"readme";
    const FILE_BODY: &[u8] = b"abc";

    impl FilesystemRead for MockReadFs {
        fn root(&self) -> NodeId {
            ROOT
        }

        fn name_matching(&self) -> super::NameMatching {
            super::NameMatching::Exact
        }

        fn node_identity(&self) -> super::NodeIdentity {
            super::NodeIdentity::Stable
        }

        fn node_info(&mut self, node: NodeId) -> Result<NodeInfo, DriverError> {
            if node == ROOT {
                Ok(NodeInfo {
                    kind: NodeKind::Directory,
                    nlink: 2,
                    ino: ROOT.raw(),
                    size: 0,
                    allocated: 0,
                    times: NodeTimes::default(),
                    content_gen: 0,
                })
            } else if node == FILE {
                Ok(NodeInfo {
                    kind: NodeKind::RegularFile,
                    nlink: 1,
                    ino: FILE.raw(),
                    size: FILE_BODY.len() as u64,
                    allocated: FILE_BODY.len() as u64,
                    times: NodeTimes::default(),
                    content_gen: 0,
                })
            } else {
                Err(DriverError::NotFound)
            }
        }

        fn lookup(&mut self, dir: NodeId, name: &[u8]) -> Result<NodeId, DriverError> {
            if dir != ROOT {
                return Err(DriverError::Unsupported);
            }
            if name == FILE_NAME {
                Ok(FILE)
            } else {
                Err(DriverError::NotFound)
            }
        }

        fn parent(&mut self, dir: NodeId) -> Result<NodeId, DriverError> {
            if dir == ROOT {
                Ok(ROOT)
            } else if dir == FILE {
                Err(DriverError::Unsupported)
            } else {
                Err(DriverError::NotFound)
            }
        }

        fn read_at(
            &mut self,
            file: NodeId,
            offset: u64,
            buf: &mut [u8],
        ) -> Result<usize, DriverError> {
            if file != FILE {
                return Err(DriverError::Unsupported);
            }
            let Ok(start) = usize::try_from(offset) else {
                return Ok(0);
            };
            if start >= FILE_BODY.len() {
                return Ok(0);
            }
            let n = core::cmp::min(buf.len(), FILE_BODY.len() - start);
            buf[..n].copy_from_slice(&FILE_BODY[start..start + n]);
            Ok(n)
        }

        fn read_dir(
            &mut self,
            dir: NodeId,
            cursor: u64,
            _after: &[u8],
            visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
        ) -> Result<(), DriverError> {
            if dir != ROOT {
                return Err(DriverError::Unsupported);
            }
            if cursor != 0 {
                return Ok(());
            }
            let entry = DirEntry {
                node: FILE,
                info: NodeInfo {
                    kind: NodeKind::RegularFile,
                    nlink: 1,
                    ino: FILE.raw(),
                    size: FILE_BODY.len() as u64,
                    allocated: FILE_BODY.len() as u64,
                    times: NodeTimes::default(),
                    content_gen: 0,
                },
                next_cursor: 1,
            };
            visit(&entry, FILE_NAME);
            Ok(())
        }

        fn pin(&mut self, node: NodeId) -> Result<NodeId, DriverError> {
            self.node_info(node).map(|_| node)
        }

        fn unpin(&mut self, _pinned: NodeId) -> Result<Unpinned, DriverError> {
            Ok(Unpinned::Released)
        }

        fn release_pins(&mut self) -> Result<(), DriverError> {
            Ok(())
        }
    }

    #[test]
    fn mock_read_fs_lookup_and_read() {
        let mut fs = MockReadFs;
        assert_eq!(fs.root(), ROOT);
        let file = fs.lookup(fs.root(), FILE_NAME).expect("file present");
        assert_eq!(file, FILE);
        let info = fs.node_info(file).expect("info");
        assert_eq!(info.kind, NodeKind::RegularFile);
        assert_eq!(info.ino, file.raw());
        assert_eq!(info.size, 3);

        let mut buf = [0u8; 8];
        let n = fs.read_at(file, 0, &mut buf).expect("read");
        assert_eq!(&buf[..n], FILE_BODY);
        // Reading at EOF yields zero bytes.
        assert_eq!(fs.read_at(file, 3, &mut buf), Ok(0));
    }

    #[test]
    fn mock_read_fs_dir_iteration_terminates() {
        let mut fs = MockReadFs;
        let mut seen = alloc::vec::Vec::new();
        fs.read_dir(ROOT, 0, &[], &mut |entry, name| {
            seen.push((*entry, name.to_vec()));
            DirVisit::Take
        })
        .expect("lists");
        let [(entry, name)] = seen.as_slice() else {
            panic!("one entry, not {}", seen.len());
        };
        assert_eq!(entry.node, FILE);
        assert_eq!(entry.info.kind, NodeKind::RegularFile);
        assert_eq!(entry.info.size, FILE_BODY.len() as u64);
        assert_eq!(name.as_slice(), FILE_NAME);
        let mut more = 0;
        fs.read_dir(ROOT, entry.next_cursor, FILE_NAME, &mut |_, _| {
            more += 1;
            DirVisit::Take
        })
        .expect("lists");
        assert_eq!(more, 0, "the listing ended");
    }

    #[test]
    fn mock_read_fs_lookup_in_non_dir_is_unsupported() {
        let mut fs = MockReadFs;
        assert_eq!(fs.lookup(FILE, FILE_NAME), Err(DriverError::Unsupported));
    }

    #[test]
    fn mock_read_fs_parent_of_the_root_is_the_root() {
        let mut fs = MockReadFs;
        assert_eq!(fs.parent(ROOT), Ok(ROOT));
        assert_eq!(fs.parent(FILE), Err(DriverError::Unsupported));
        assert_eq!(fs.parent(NodeId::from_raw(9)), Err(DriverError::NotFound));
    }

    #[test]
    fn mock_read_fs_holds_a_live_node_under_its_own_id() {
        let mut fs = MockReadFs;
        assert_eq!(fs.pin(FILE), Ok(FILE));
        assert_eq!(fs.unpin(FILE), Ok(Unpinned::Released));
        assert_eq!(fs.pin(NodeId::from_raw(9)), Err(DriverError::NotFound));
        assert_eq!(fs.release_pins(), Ok(()));
    }

    /// A `FilesystemWrite` that stores at most `step` bytes per call, as a
    /// driver bounding the memory one write may pin does.
    struct ShortWriteFs {
        step: usize,
        body: [u8; 8],
        len: usize,
        calls: usize,
    }

    impl FilesystemWrite for ShortWriteFs {
        fn create(
            &mut self,
            _: NodeId,
            _: &[u8],
            _: NodeKind,
            _: &NodeSecurity,
        ) -> Result<NodeId, DriverError> {
            Err(DriverError::Unsupported)
        }

        fn write_at(&mut self, _: NodeId, offset: u64, data: &[u8]) -> Result<usize, DriverError> {
            self.calls += 1;
            let start = usize::try_from(offset).map_err(|_| DriverError::OutOfRange)?;
            let take = data.len().min(self.step);
            let end = start.checked_add(take).ok_or(DriverError::OutOfRange)?;
            let slot = self
                .body
                .get_mut(start..end)
                .ok_or(DriverError::DeviceFault)?;
            slot.copy_from_slice(&data[..take]);
            self.len = self.len.max(end);
            Ok(take)
        }

        fn truncate(&mut self, _: NodeId, _: u64) -> Result<(), DriverError> {
            Err(DriverError::Unsupported)
        }

        fn remove(&mut self, _: NodeId, _: &[u8]) -> Result<(), DriverError> {
            Err(DriverError::Unsupported)
        }

        fn rename(&mut self, _: NodeId, _: &[u8], _: NodeId, _: &[u8]) -> Result<(), DriverError> {
            Err(DriverError::Unsupported)
        }

        fn flush(&mut self) -> Result<(), DriverError> {
            Ok(())
        }
    }

    #[test]
    fn write_all_resumes_across_short_writes() {
        let mut fs = ShortWriteFs {
            step: 2,
            body: [0; 8],
            len: 0,
            calls: 0,
        };
        assert_eq!(fs.write_all(W_FILE, 0, b"abcdef"), Ok(()));
        assert_eq!(&fs.body[..6], b"abcdef");
        assert_eq!(fs.calls, 3, "two bytes a call, six bytes, three calls");
        // Each resumed call must land at the offset the last one reached, not
        // back at the start.
        assert_eq!(fs.len, 6);
    }

    #[test]
    fn write_all_refuses_a_driver_that_makes_no_progress() {
        // A zero count with bytes left is a refusal, not back-pressure:
        // retrying it forever is not an answer.
        let mut fs = ShortWriteFs {
            step: 0,
            body: [0; 8],
            len: 0,
            calls: 0,
        };
        assert_eq!(fs.write_all(W_FILE, 0, b"ab"), Err(DriverError::NoSpace));
        assert_eq!(fs.calls, 1, "a stalled write is refused, never looped");
    }

    #[test]
    fn write_all_of_nothing_touches_the_driver_not_at_all() {
        let mut fs = ShortWriteFs {
            step: 2,
            body: [0; 8],
            len: 0,
            calls: 0,
        };
        assert_eq!(fs.write_all(W_FILE, 0, &[]), Ok(()));
        assert_eq!(fs.calls, 0);
    }

    /// A minimal `FilesystemWrite` holding one regular file directly under a
    /// root directory, its name mutations taking `(dir, name)` and its byte
    /// writes the node. It exercises the whole `abi-v1` write surface:
    /// create, extend via `write_at`, `truncate`, `remove`, and `rename`
    /// (which re-labels the single file within the root, keeping its id).
    struct MockWriteFs {
        present: bool,
        name: [u8; 8],
        name_len: usize,
        body: [u8; 8],
        len: usize,
    }

    const W_ROOT: NodeId = NodeId::from_raw(1);
    const W_FILE: NodeId = NodeId::from_raw(2);
    const W_NAME: &[u8] = b"data";
    const W_RECORD: NodeSecurity = NodeSecurity::new(0o644, 0, 0);

    impl MockWriteFs {
        fn empty() -> Self {
            Self {
                present: false,
                name: [0; 8],
                name_len: 0,
                body: [0; 8],
                len: 0,
            }
        }

        fn name(&self) -> &[u8] {
            &self.name[..self.name_len]
        }

        fn store_name(&mut self, name: &[u8]) -> Result<(), DriverError> {
            if name.is_empty() || name.len() > self.name.len() {
                return Err(DriverError::LengthOutOfRange);
            }
            self.name[..name.len()].copy_from_slice(name);
            self.name_len = name.len();
            Ok(())
        }

        /// Refuse a byte write to anything but the live file.
        fn writable(&self, file: NodeId) -> Result<(), DriverError> {
            if file == W_ROOT {
                return Err(DriverError::Unsupported);
            }
            if file != W_FILE || !self.present {
                return Err(DriverError::NotFound);
            }
            Ok(())
        }
    }

    impl FilesystemWrite for MockWriteFs {
        fn create(
            &mut self,
            dir: NodeId,
            name: &[u8],
            kind: NodeKind,
            _record: &NodeSecurity,
        ) -> Result<NodeId, DriverError> {
            if dir != W_ROOT {
                return Err(DriverError::Unsupported);
            }
            if kind != NodeKind::RegularFile {
                return Err(DriverError::Unsupported);
            }
            if self.present {
                return Err(DriverError::AlreadyExists);
            }
            self.store_name(name)?;
            self.present = true;
            self.len = 0;
            Ok(W_FILE)
        }

        fn write_at(
            &mut self,
            file: NodeId,
            offset: u64,
            data: &[u8],
        ) -> Result<usize, DriverError> {
            self.writable(file)?;
            let start = usize::try_from(offset).map_err(|_| DriverError::LengthOutOfRange)?;
            let end = start
                .checked_add(data.len())
                .ok_or(DriverError::LengthOutOfRange)?;
            if end > self.body.len() {
                return Err(DriverError::DeviceFault);
            }
            for byte in &mut self.body[self.len..start] {
                *byte = 0;
            }
            self.body[start..end].copy_from_slice(data);
            self.len = self.len.max(end);
            Ok(data.len())
        }

        fn truncate(&mut self, file: NodeId, size: u64) -> Result<(), DriverError> {
            self.writable(file)?;
            let new = usize::try_from(size).map_err(|_| DriverError::LengthOutOfRange)?;
            if new > self.body.len() {
                return Err(DriverError::DeviceFault);
            }
            for byte in &mut self.body[self.len.min(new)..new] {
                *byte = 0;
            }
            self.len = new;
            Ok(())
        }

        fn remove(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError> {
            if dir != W_ROOT {
                return Err(DriverError::Unsupported);
            }
            if !self.present || name != self.name() {
                return Err(DriverError::NotFound);
            }
            self.present = false;
            self.len = 0;
            self.name_len = 0;
            Ok(())
        }

        fn rename(
            &mut self,
            src_dir: NodeId,
            src_name: &[u8],
            dst_dir: NodeId,
            dst_name: &[u8],
        ) -> Result<(), DriverError> {
            if src_dir != W_ROOT || dst_dir != W_ROOT {
                return Err(DriverError::Unsupported);
            }
            if !self.present || src_name != self.name() {
                return Err(DriverError::NotFound);
            }
            if dst_name == src_name {
                return Ok(());
            }
            // The model holds a single file, so the destination name can
            // never already be in use; re-label the present file.
            self.store_name(dst_name)
        }

        fn flush(&mut self) -> Result<(), DriverError> {
            Ok(())
        }
    }

    #[test]
    fn mock_write_fs_round_trip() {
        let mut fs = MockWriteFs::empty();
        let file = fs
            .create(W_ROOT, W_NAME, NodeKind::RegularFile, &W_RECORD)
            .expect("create");
        assert_eq!(file, W_FILE);
        // Creating it again is rejected as a taken name, never as a
        // transient a caller would retry.
        assert_eq!(
            fs.create(W_ROOT, W_NAME, NodeKind::RegularFile, &W_RECORD),
            Err(DriverError::AlreadyExists)
        );
        assert_eq!(fs.write_at(file, 0, b"abc"), Ok(3));
        assert_eq!(fs.len, 3);
        // Writing past the end zero-fills the gap and extends.
        assert_eq!(fs.write_at(file, 5, b"Z"), Ok(1));
        assert_eq!(&fs.body[..fs.len], b"abc\0\0Z");
        fs.truncate(file, 2).expect("shrink");
        assert_eq!(&fs.body[..fs.len], b"ab");
        assert!(fs.flush().is_ok());
        fs.remove(W_ROOT, W_NAME).expect("unlink");
        assert_eq!(fs.write_at(file, 0, b"x"), Err(DriverError::NotFound));
    }

    #[test]
    fn mock_write_fs_rejects_non_root_dir() {
        let mut fs = MockWriteFs::empty();
        assert_eq!(
            fs.create(W_FILE, W_NAME, NodeKind::RegularFile, &W_RECORD),
            Err(DriverError::Unsupported)
        );
        assert_eq!(fs.remove(W_FILE, W_NAME), Err(DriverError::Unsupported));
        // A directory's bytes are its entries, never a write's.
        assert_eq!(fs.write_at(W_ROOT, 0, b"x"), Err(DriverError::Unsupported));
        assert_eq!(fs.truncate(W_ROOT, 0), Err(DriverError::Unsupported));
    }

    #[test]
    fn mock_write_fs_rename_relabels_the_file() {
        let mut fs = MockWriteFs::empty();
        let file = fs
            .create(W_ROOT, W_NAME, NodeKind::RegularFile, &W_RECORD)
            .expect("create");
        assert_eq!(fs.write_at(file, 0, b"hi"), Ok(2));
        // Renaming a missing source fails closed.
        assert_eq!(
            fs.rename(W_ROOT, b"absent", W_ROOT, b"moved"),
            Err(DriverError::NotFound)
        );
        // The self-rename is a no-op success.
        assert_eq!(fs.rename(W_ROOT, W_NAME, W_ROOT, W_NAME), Ok(()));
        // Moving to a new name re-labels the file: the old name is gone, and
        // the node keeps its id and its contents.
        assert_eq!(fs.rename(W_ROOT, W_NAME, W_ROOT, b"moved"), Ok(()));
        assert_eq!(
            fs.rename(W_ROOT, W_NAME, W_ROOT, b"again"),
            Err(DriverError::NotFound)
        );
        assert_eq!(fs.write_at(file, 2, b"!"), Ok(1));
        assert_eq!(&fs.body[..fs.len], b"hi!");
        // A non-root directory is refused.
        assert_eq!(
            fs.rename(W_FILE, b"moved", W_ROOT, b"x"),
            Err(DriverError::Unsupported)
        );
    }

    #[test]
    fn node_security_acl_is_bounded() {
        let mut sec = NodeSecurity::new(0o640, 7, 9);
        assert!(sec.acl().is_empty());
        assert_eq!(sec.required_cap, None);
        for _ in 0..MAX_ACL_ENTRIES {
            sec.push_acl(SecurityAcl {
                subject: SecuritySubject::User(1),
                perms: 0b100,
            })
            .expect("within bound");
        }
        assert_eq!(sec.acl().len(), MAX_ACL_ENTRIES);
        assert_eq!(
            sec.push_acl(SecurityAcl {
                subject: SecuritySubject::Group(2),
                perms: 0b010,
            }),
            Err(DriverError::LengthOutOfRange)
        );
    }

    /// A node whose stored record the VFS reads and replaces through the
    /// trait.
    struct MockSecurityFs {
        sec: NodeSecurity,
    }

    impl FilesystemSecurity for MockSecurityFs {
        fn security(&mut self, node: NodeId) -> Result<NodeSecurity, DriverError> {
            if node == NodeId::NONE {
                return Err(DriverError::NotFound);
            }
            Ok(self.sec)
        }

        fn set_security(
            &mut self,
            node: NodeId,
            security: NodeSecurity,
        ) -> Result<(), DriverError> {
            if node == NodeId::NONE {
                return Err(DriverError::NotFound);
            }
            self.sec = security;
            Ok(())
        }
    }

    #[test]
    fn mock_security_fs_reports_stored_record() {
        let mut sec = NodeSecurity::new(0o600, 7, 9);
        sec.required_cap = Some(CapabilityId::AUDIT_READ);
        sec.push_acl(SecurityAcl {
            subject: SecuritySubject::Group(11),
            perms: 0b110,
        })
        .expect("acl");
        let mut fs = MockSecurityFs { sec };
        assert_eq!(fs.security(NodeId::from_raw(1)), Ok(sec));
        assert_eq!(fs.security(NodeId::NONE), Err(DriverError::NotFound));

        let replacement = NodeSecurity::new(0o750, 8, 10);
        assert_eq!(
            fs.set_security(NodeId::NONE, replacement),
            Err(DriverError::NotFound)
        );
        assert_eq!(fs.security(NodeId::from_raw(1)), Ok(sec));
        assert_eq!(fs.set_security(NodeId::from_raw(1), replacement), Ok(()));
        assert_eq!(fs.security(NodeId::from_raw(1)), Ok(replacement));
    }

    #[test]
    fn a_created_node_s_mode_follows_its_kind() {
        let dir = NodeSecurity::created(NodeKind::Directory, 7, 9);
        assert_eq!((dir.mode, dir.uid, dir.gid), (0o755, 7, 9));
        assert_eq!(dir.required_cap, None);
        assert!(dir.acl().is_empty());
        assert_eq!(
            NodeSecurity::created(NodeKind::RegularFile, 7, 9),
            NodeSecurity::new(0o644, 7, 9)
        );
        assert_eq!(
            NodeSecurity::created(NodeKind::Symlink, 7, 9),
            NodeSecurity::new(0o777, 7, 9)
        );
    }

    /// A node holding a single extended attribute in fixed buffers, enough to
    /// exercise the [`FilesystemAttrs`] contract (buffer sizing, `None` for
    /// absent, `NotFound` for a dead node) without an allocator. The key
    /// grammar and bounds are validated in `lib/fsmeta` and by the `ARXFS`
    /// integration tests, not here.
    struct MockAttrsFs {
        key: [u8; 64],
        key_len: usize,
        value: [u8; 64],
        value_len: usize,
        present: bool,
    }

    impl MockAttrsFs {
        fn empty() -> Self {
            Self {
                key: [0u8; 64],
                key_len: 0,
                value: [0u8; 64],
                value_len: 0,
                present: false,
            }
        }

        fn matches(&self, key: &[u8]) -> bool {
            self.present && &self.key[..self.key_len] == key
        }
    }

    impl FilesystemAttrs for MockAttrsFs {
        fn get_attr(
            &mut self,
            node: NodeId,
            key: &[u8],
            value_out: &mut [u8],
        ) -> Result<Option<usize>, DriverError> {
            if node == NodeId::NONE {
                return Err(DriverError::NotFound);
            }
            if !self.matches(key) {
                return Ok(None);
            }
            if value_out.len() < self.value_len {
                return Err(DriverError::BufferTooSmall);
            }
            value_out[..self.value_len].copy_from_slice(&self.value[..self.value_len]);
            Ok(Some(self.value_len))
        }

        fn set_attr(&mut self, node: NodeId, key: &[u8], value: &[u8]) -> Result<(), DriverError> {
            if node == NodeId::NONE {
                return Err(DriverError::NotFound);
            }
            if key.len() > self.key.len() || value.len() > self.value.len() {
                return Err(DriverError::NoSpace);
            }
            self.key[..key.len()].copy_from_slice(key);
            self.key_len = key.len();
            self.value[..value.len()].copy_from_slice(value);
            self.value_len = value.len();
            self.present = true;
            Ok(())
        }

        fn list_attr(
            &mut self,
            node: NodeId,
            index: u64,
            key_out: &mut [u8],
        ) -> Result<Option<usize>, DriverError> {
            if node == NodeId::NONE {
                return Err(DriverError::NotFound);
            }
            if index != 0 || !self.present {
                return Ok(None);
            }
            if key_out.len() < self.key_len {
                return Err(DriverError::BufferTooSmall);
            }
            key_out[..self.key_len].copy_from_slice(&self.key[..self.key_len]);
            Ok(Some(self.key_len))
        }

        fn remove_attr(&mut self, node: NodeId, key: &[u8]) -> Result<(), DriverError> {
            if node == NodeId::NONE {
                return Err(DriverError::NotFound);
            }
            if !self.matches(key) {
                return Err(DriverError::NotFound);
            }
            self.present = false;
            Ok(())
        }
    }

    #[test]
    fn mock_attrs_fs_round_trips() {
        let mut fs = MockAttrsFs::empty();
        let node = NodeId::from_raw(1);
        assert_eq!(fs.set_attr(node, b"user.comment", b"hi"), Ok(()));

        let mut buf = [0u8; 16];
        assert_eq!(fs.get_attr(node, b"user.comment", &mut buf), Ok(Some(2)));
        assert_eq!(&buf[..2], b"hi");
        assert_eq!(fs.get_attr(node, b"user.absent", &mut buf), Ok(None));

        // A too-small value buffer fails closed rather than truncating.
        let mut tiny = [0u8; 1];
        assert_eq!(
            fs.get_attr(node, b"user.comment", &mut tiny),
            Err(DriverError::BufferTooSmall)
        );

        // Listing yields the one key then terminates.
        let mut key_buf = [0u8; 64];
        assert_eq!(fs.list_attr(node, 0, &mut key_buf), Ok(Some(12)));
        assert_eq!(&key_buf[..12], b"user.comment");
        assert_eq!(fs.list_attr(node, 1, &mut key_buf), Ok(None));

        assert_eq!(fs.remove_attr(node, b"user.comment"), Ok(()));
        assert_eq!(
            fs.remove_attr(node, b"user.comment"),
            Err(DriverError::NotFound)
        );
        assert_eq!(fs.get_attr(node, b"user.comment", &mut buf), Ok(None));

        // A dead node fails closed on every operation.
        assert_eq!(
            fs.get_attr(NodeId::NONE, b"user.comment", &mut buf),
            Err(DriverError::NotFound)
        );
        assert_eq!(
            fs.set_attr(NodeId::NONE, b"user.comment", b"x"),
            Err(DriverError::NotFound)
        );
    }

    /// Reports a fixed accounting; a zeroed record models an untracked
    /// inode table.
    struct MockStatsFs {
        stats: VolumeStats,
    }

    impl FilesystemStats for MockStatsFs {
        fn stats(&mut self) -> Result<VolumeStats, DriverError> {
            Ok(self.stats)
        }
    }

    #[test]
    fn volume_stats_holds_its_reserve_flag_in_the_block_size_pad() {
        // The C view names the byte at this offset, and the mount record's
        // wire image puts it in the same place.
        assert_eq!(core::mem::offset_of!(VolumeStats, in_reserve), 4);
        assert_eq!(core::mem::offset_of!(VolumeStats, total_blocks), 8);
        assert_eq!(core::mem::size_of::<VolumeStats>(), 48);
    }

    #[test]
    fn mock_stats_fs_reports_stored_record() {
        let stats = VolumeStats {
            block_size: 4096,
            in_reserve: false,
            total_blocks: 1024,
            free_blocks: 512,
            avail_blocks: 480,
            files: 0,
            files_free: 0,
        };
        let mut fs = MockStatsFs { stats };
        let reported = fs.stats().expect("stats");
        assert_eq!(reported, stats);
        // The contract: available never exceeds free, free never exceeds
        // total, and the dynamic-inode answer is the zero pair.
        assert!(reported.avail_blocks <= reported.free_blocks);
        assert!(reported.free_blocks <= reported.total_blocks);
        assert_eq!((reported.files, reported.files_free), (0, 0));
    }
}
