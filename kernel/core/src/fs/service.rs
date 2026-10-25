//! The kernel-side filesystem-operation seam the userland `fs_*` syscalls
//! route through.
//!
//! The mounted volume's filesystem driver is owned for the life of the
//! system by the single disk-owning kthread (`tairix-kernel`'s driver-store
//! service); the block device cannot be shared and an operation may park on
//! the device completion IRQ. The `fs_*` syscall handlers therefore do not
//! borrow the filesystem directly — they call this [`FilesystemService`],
//! whose production implementation routes each request to that disk-owning
//! service. The handler supplies a **kernel-attested** identity (an owning uid
//! and effective capability set, taken from a task's
//! [`tairix_kernel_sec::TaskCapabilities`], never anything the caller
//! supplied); the service resolves its groups from the system identity table
//! and authorises every operation through the secured VFS, so every per-inode
//! owner/mode/ACL/capability and mount-flag check stays kernel-side and fails
//! closed.
//!
//! A name operation resolves an [`At`] — a path, and where it starts when
//! relative — under the caller's identity. A byte operation runs on the node
//! a descriptor pinned at open ([`NodeHandle`]), under the identity the
//! descriptor's [`Authority`](super::node::Authority) names, and looks at no
//! path.
//!
//! Until the boot path installs a real service the handlers hold
//! [`NULL_FILESYSTEM`], whose every operation fails closed with
//! [`Errno::NotImplemented`] — a kernel with no mounted filesystem never
//! fabricates a result.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::delegate::FinalLink;
use super::listing::Listing;
use super::mount::Projection;
use super::node::{Acting, At, Cwd, NodeHandle, NodeRef, Start, VolumeNodes};
use super::perm::{Access, CreateMode, Judged};
use tairix_abi::driver::filesystem::{DirVisit, NodeKind};
use tairix_abi::fs::{Access as Wanted, NodeExpect};

use tairix_abi::sysinfo::{
    MountRecord, ViewBindingRecord, VolumeIoHealthRecord, VolumeIoQueueRecord, VolumeIoStatsRecord,
};
use tairix_abi::time::Time64;
use tairix_abi::{
    CapabilityQuery, DirEntry, Errno, FileId, FileKind, FileStat, OpenFlags, RealpathMode,
    RenameFlags, SeekRegion, UnlinkFlags,
};
use zeroize::Zeroize;

/// One directory entry as [`FilesystemService::readdir_node`] reports it: the
/// child's kind, its apparent and allocated sizes, its identity and name
/// count, and its name.
///
/// The metadata rides along with the listing because the mounted filesystem
/// already holds each child's while producing the entry; a
/// consumer that needs it (`du`) reads the one listing instead of
/// re-resolving every child by path, which on an uncached, authenticated
/// volume is a fresh full walk per child.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReaddirEntry {
    /// Whether the entry names a regular file or a directory.
    pub kind: FileKind,
    /// Apparent length in bytes; `0` for a directory.
    pub size: u64,
    /// Bytes of on-disk storage the entry's data occupies, as the mounted
    /// format's own allocation tracking reports it.
    pub allocated: u64,
    /// The entry's last contents-modification instant, as the mounted
    /// format stores it ([`Time64::UNIX_EPOCH`] for a backing with no
    /// per-node stamp).
    pub modified: Time64,
    /// The node this name resolves to — the same identity [`FileStat::id`]
    /// reports for it, so two names for one node carry one `id`.
    /// [`FileId::NONE`] for an entry the covering volume holds no node for
    /// (a covered mount point listed in its parent).
    pub id: FileId,
    /// How many directory entries name this node, as the format records it
    /// ([`NodeInfo::SINGLE_NAME`](tairix_abi::driver::filesystem::NodeInfo::SINGLE_NAME)
    /// for a format that keeps no count).
    pub nlink: u32,
    /// The node's content generation, as [`FileStat::content_gen`] reports
    /// it for the same node.
    pub content_gen: u64,
    /// The entry's name (a single component, never `.`/`..`).
    pub name: String,
}

impl ReaddirEntry {
    /// The owned form of the streamed `entry`.
    ///
    /// # Errors
    ///
    /// [`Errno::DeviceFault`] for a name that is not UTF-8, which the VFS has
    /// already refused, and [`Errno::OutOfMemory`] when the name cannot be
    /// held.
    pub fn from_wire(entry: &DirEntry<'_>) -> Result<Self, Errno> {
        let name = core::str::from_utf8(entry.name).map_err(|_| Errno::DeviceFault)?;
        let mut owned = String::new();
        owned
            .try_reserve_exact(name.len())
            .map_err(|_| Errno::OutOfMemory)?;
        owned.push_str(name);
        Ok(Self::named(entry, owned))
    }

    /// `entry`'s record under the owned `name`.
    #[must_use]
    pub fn named(entry: &DirEntry<'_>, name: String) -> Self {
        Self {
            kind: entry.kind,
            size: entry.size,
            allocated: entry.allocated,
            modified: entry.modified,
            id: entry.id,
            nlink: entry.nlink,
            content_gen: entry.content_gen,
            name,
        }
    }

    /// This entry as the `fs_readdir` stream carries it.
    #[must_use]
    pub fn wire(&self) -> DirEntry<'_> {
        DirEntry {
            kind: self.kind,
            size: self.size,
            allocated: self.allocated,
            modified: self.modified,
            id: self.id,
            nlink: self.nlink,
            content_gen: self.content_gen,
            name: self.name.as_bytes(),
        }
    }
}

/// A name is decrypted user data: wiped before its allocation is released, as
/// the volume cache wipes its own copies.
impl Drop for ReaddirEntry {
    fn drop(&mut self) {
        self.name.zeroize();
    }
}

/// What [`FilesystemService::lookup_entries`] found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LookedUp {
    /// The directory looked in, as `stat` reports its identity.
    pub dir: FileId,
    /// Each asked-for name's record, in order; [`None`] where it is absent.
    pub entries: Vec<Option<ReaddirEntry>>,
    /// The mounts directly beneath the directory, as its listing shows them.
    pub children: super::ChildMounts,
}

/// What [`FilesystemService::open`] opened.
#[derive(Debug)]
pub enum Opened {
    /// A node of a volume, pinned for as long as a descriptor holds it.
    Node {
        /// The pin.
        node: NodeRef,
        /// The projection the node was opened through.
        projection: Arc<Projection>,
        /// The node's kind.
        kind: NodeKind,
        /// The path it was opened as: a label, never resolved.
        opened_as: Arc<String>,
        /// Whether the node is the projection's floor, whose listing shows
        /// the projections attached beneath it.
        at_floor: bool,
    },
    /// The catalog of attached volumes a projection with no volume behind it
    /// presents.
    Catalog(Arc<Projection>),
}

/// The set of secured filesystem operations a userland `fs_*` syscall needs.
///
/// A name operation receives the caller's **attested** identity — `uid` is
/// the task's owning user id and `caps` its effective capability set, both
/// kernel-sourced — and resolves its [`At`] under it: an absolute path from
/// the view root, a relative one from the start it names. Every operation
/// runs as the caller whatever start it was given, and the start's own
/// search permission is checked on every call. A node operation receives the
/// descriptor's [`NodeHandle`] and the [`Acting`] identity the syscall layer
/// built from its authority. Either way the implementation builds the full
/// VFS [`Credentials`](crate::fs::Credentials), resolving the primary and
/// supplementary groups from the system identity table, before authorising
/// the operation. It never trusts a caller-supplied identity, and fails
/// closed on every error and uninitialised path.
pub trait FilesystemService: Send + Sync {
    /// Resolve `at` with `flags` under the caller's attested identity,
    /// applying the create/exclusive/truncate/directory semantics
    /// [`OpenFlags`] encodes, confirm the access is permitted, and pin what
    /// it names. A create asks `mode`, which an open that creates nothing
    /// does not read; [`OpenFlags::CREATE`] with [`OpenFlags::DIRECTORY`] and
    /// [`OpenFlags::EXCLUSIVE`] makes the directory and opens it, asking no
    /// read or write access of it.
    ///
    /// The handler records a descriptor only after this succeeds, so a
    /// refused open never produces one.
    ///
    /// # Errors
    ///
    /// The stable [`Errno`] the secured VFS maps the refusal to (a missing
    /// path, a permission or mount-flag denial, an existing path under
    /// [`OpenFlags::EXCLUSIVE`], a non-directory under
    /// [`OpenFlags::DIRECTORY`]), [`Errno::Stale`] for a start whose
    /// projection or registration is gone, or [`Errno::NotImplemented`] when
    /// no filesystem is mounted.
    fn open(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        at: At<'_>,
        flags: OpenFlags,
        mode: CreateMode,
    ) -> Result<Opened, Errno>;

    /// Read up to `buf.len()` bytes of the file `at` names, at byte `offset`,
    /// as a program to run, which every file of a bundle the spawn path loads
    /// is.
    ///
    /// # Errors
    ///
    /// [`Errno::PermissionDenied`] when the projection serving it is
    /// `noexec`; otherwise the stable [`Errno`] for the VFS refusal. The
    /// default fails closed with [`Errno::NotImplemented`].
    fn read_program(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _at: At<'_>,
        _offset: u64,
        _buf: &mut [u8],
    ) -> Result<usize, Errno> {
        Err(Errno::NotImplemented)
    }

    /// Every entry of the directory `at` names, owned, for a kernel walk that
    /// acts on each once the listing is read: its own entries, then, for a
    /// projection's floor, the projections attached beneath it that the
    /// volume holds no entry for.
    ///
    /// `final_link` is the resolution posture of the walk: under
    /// [`FinalLink::Keep`] a final symbolic link is *not* a directory, so it
    /// is refused rather than listing the link's target.
    ///
    /// # Errors
    ///
    /// The stable [`Errno`] for the VFS refusal (not a directory, permission
    /// denied), and [`Errno::LimitExceeded`] past `limit` entries, so no
    /// directory makes the kernel hold more than its caller budgeted. The
    /// default fails closed with [`Errno::NotImplemented`].
    fn readdir_bounded(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _at: At<'_>,
        _final_link: FinalLink,
        _limit: usize,
    ) -> Result<Vec<ReaddirEntry>, Errno> {
        Err(Errno::NotImplemented)
    }

    /// For each of `names`, what a listing of the directory `at` names would
    /// report for it now, or [`None`] where no such entry exists — under the
    /// same authority, without reading the whole listing. With no names it is
    /// the listing's authorisation alone.
    ///
    /// Defaulted to fail closed for a service with no change notification.
    ///
    /// # Errors
    ///
    /// As [`readdir_bounded`](Self::readdir_bounded).
    fn lookup_entries(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        at: At<'_>,
        final_link: FinalLink,
        names: &[&[u8]],
    ) -> Result<LookedUp, Errno> {
        let _ = (uid, caps, at, final_link, names);
        Err(Errno::NotImplemented)
    }

    /// How many times the mount table has changed: a directory watch rescans
    /// when it moved, since its listing's mount points may have.
    fn mount_epoch(&self) -> u64 {
        0
    }

    /// Report the structural metadata of the node `at` names.
    ///
    /// `final_link` selects between the POSIX `stat` and `lstat` readings:
    /// [`FinalLink::Keep`] — the posture an [`OpenFlags::NO_FOLLOW`]
    /// descriptor carries — reports a final symbolic link itself, including
    /// a dangling one that following would report as absent.
    ///
    /// # Errors
    ///
    /// The stable [`Errno`] for the VFS refusal, or [`Errno::NotImplemented`]
    /// when no filesystem is mounted.
    fn stat(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        at: At<'_>,
        final_link: FinalLink,
    ) -> Result<FileStat, Errno>;

    /// Create a symbolic link at `at` whose stored target is `target`.
    ///
    /// `target` is stored verbatim and is never resolved here — it is data,
    /// not a path the kernel walks — so the call authorises only the right
    /// to create a name in the link's own parent, and the link may
    /// legitimately dangle. Its grammar is checked before anything else.
    ///
    /// # Errors
    ///
    /// The stable [`Errno`] for the VFS refusal (an existing name, a
    /// read-only mount, a permission denial), [`Errno::OutOfRange`] for a
    /// target that fails the link-target grammar,
    /// [`Errno::NotSupported`] when the format has no link object type, or
    /// [`Errno::NotImplemented`] when no filesystem is mounted.
    fn symlink(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        target: &str,
        at: At<'_>,
    ) -> Result<(), Errno>;

    /// Read the stored target of the symbolic link `at` names.
    ///
    /// The final component is never followed and the target comes back
    /// exactly as it was stored, still unresolved.
    ///
    /// # Errors
    ///
    /// The stable [`Errno`] for the VFS refusal, [`Errno::OutOfRange`] when
    /// `at` names something other than a symbolic link,
    /// [`Errno::NotSupported`] when the format stores no links, or
    /// [`Errno::NotImplemented`] when no filesystem is mounted.
    fn readlink(&self, uid: u32, caps: &dyn CapabilityQuery, at: At<'_>) -> Result<String, Errno>;

    /// Canonicalise `at`: the one path that names what it resolves to, with
    /// every symbolic link followed and every `..` applied to the nodes the
    /// resolution really traversed. A path relative to a directory the caller
    /// holds is spelled from that directory's canonical path now, however it
    /// has been renamed since it was opened.
    ///
    /// `mode` decides only how much of the path must exist. The answer holds
    /// no `.`, no `..`, and no symbolic link, and is always a path this same
    /// service would accept back.
    ///
    /// # Errors
    ///
    /// The stable [`Errno`] for the VFS refusal, [`Errno::NotFound`] when
    /// `mode` requires a component that does not exist, [`Errno::LinkLoop`]
    /// for a cycle or an over-budget chain, or [`Errno::NotImplemented`]
    /// when no filesystem is mounted.
    fn realpath(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        at: At<'_>,
        mode: RealpathMode,
    ) -> Result<String, Errno>;

    /// Add `new` as a second name for the node `existing` already names — a
    /// hard link.
    ///
    /// `existing_link` selects whether the existing name's final symbolic
    /// link is resolved ([`FinalLink::Follow`], `ln -L`) or the link itself
    /// gains the second name ([`FinalLink::Keep`], POSIX `link()`); the new
    /// name is never followed. Both must lie in one projection, and the new
    /// name is authorised as a create in its own parent, conferring no
    /// authority the caller did not already hold.
    ///
    /// # Errors
    ///
    /// The stable [`Errno`] for the VFS refusal (an existing name, a
    /// read-only mount, a permission denial), [`Errno::IsADirectory`] for a
    /// directory, [`Errno::CrossVolume`] for two projections,
    /// [`Errno::TooManyLinks`] when the format's name count would overflow,
    /// [`Errno::NotSupported`] when the format holds one name per node, or
    /// [`Errno::NotImplemented`] when no filesystem is mounted.
    fn link(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        existing: At<'_>,
        new: At<'_>,
        existing_link: FinalLink,
    ) -> Result<(), Errno>;

    /// Flush every mounted volume's pending writes, each one attempted even
    /// when another refuses.
    ///
    /// # Errors
    ///
    /// The first refusal, or [`Errno::NotImplemented`] when no filesystem is
    /// mounted.
    fn sync_all(&self, uid: u32, caps: &dyn CapabilityQuery) -> Result<(), Errno>;

    /// Create a directory at `at`, asking `mode`.
    ///
    /// # Errors
    ///
    /// The stable [`Errno`] for the VFS refusal (an existing path, a
    /// read-only mount, a permission denial), or [`Errno::NotImplemented`]
    /// when no filesystem is mounted.
    fn mkdir(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        at: At<'_>,
        mode: CreateMode,
    ) -> Result<(), Errno>;

    /// Remove the file or empty directory `at` names.
    ///
    /// With [`UnlinkFlags::DIRECTORY`] the removal succeeds only when the
    /// name is an (empty) directory, and with `expect` only while the name
    /// holds the node it records — [`UnlinkFlags::EXPECT_NODE`] comparing
    /// the node alone, [`UnlinkFlags::EXPECT_UNCHANGED`] every field — each
    /// decided under the volume's lock that removes it. A node a descriptor
    /// holds keeps its storage until the last descriptor lets it go.
    ///
    /// # Errors
    ///
    /// The stable [`Errno`] for the VFS refusal (a missing path, a non-empty
    /// directory, a non-directory under [`UnlinkFlags::DIRECTORY`], a
    /// read-only mount, a permission denial), [`Errno::Stale`] when the node
    /// is not the one expected, which changes nothing, [`Errno::OutOfRange`]
    /// for an expectation the flags do not name, or [`Errno::NotImplemented`]
    /// when no filesystem is mounted.
    fn unlink(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        at: At<'_>,
        flags: UnlinkFlags,
        expect: Option<&NodeExpect>,
    ) -> Result<(), Errno>;

    /// Move the file or directory `src` names to `dst`, preserving its
    /// identity and contents. Both must lie in one projection. Under
    /// [`RenameFlags::NO_REPLACE`] a destination naming any entry but the
    /// source's own refuses the move, and under [`RenameFlags::EXPECT_NODE`]
    /// the source must still be the node `expect` records; both are decided
    /// under the volume's lock.
    ///
    /// # Errors
    ///
    /// The stable [`Errno`] for the VFS refusal (a missing source, a
    /// read-only mount, a permission denial, a non-empty directory
    /// destination, an occupied destination under `NO_REPLACE`, a move
    /// between projections), [`Errno::Stale`] when the source is not the
    /// node expected, which changes nothing, or [`Errno::NotImplemented`]
    /// when no filesystem is mounted.
    fn rename(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        src: At<'_>,
        dst: At<'_>,
        flags: RenameFlags,
        expect: Option<&NodeExpect>,
    ) -> Result<(), Errno>;

    /// Set the permission bits of the node `at` names to `mode` (the
    /// `chmod(2)` shape), leaving ownership, ACL, and capability gate
    /// untouched; under [`FinalLink::Keep`] a final link itself.
    ///
    /// Only the node's owner may change its mode; holding a capability
    /// grants no override.
    ///
    /// # Errors
    ///
    /// The stable [`Errno`] for the VFS refusal (a missing path, a
    /// non-owner caller, a read-only mount), [`Errno::OutOfRange`] for a
    /// mode carrying a bit above the permission mask, or
    /// [`Errno::NotImplemented`] when no filesystem is mounted.
    fn set_mode(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        at: At<'_>,
        mode: u32,
        final_link: FinalLink,
    ) -> Result<(), Errno>;

    /// Set the owning user and/or group of the node `at` names (the
    /// `chown(2)` / `chgrp(2)` shape), leaving its mode's permission triads,
    /// ACL, and capability gate otherwise untouched; under
    /// [`FinalLink::Keep`] a final link itself. Either of `new_uid` /
    /// `new_gid` may be [`tairix_abi::fs::FS_OWNER_UNCHANGED`] to leave that
    /// field.
    ///
    /// The secured VFS owns the authorisation: reassigning the uid, or
    /// setting a gid the caller is not a member of, requires
    /// [`tairix_abi::CapabilityId::FS_CHOWN`]; otherwise only the node's
    /// owner may change the group, and only to a group they belong to. Any
    /// change clears the set-*id* bits.
    ///
    /// Defaults to [`Errno::NotImplemented`] so a service that cannot honour
    /// the change fails closed.
    ///
    /// # Errors
    ///
    /// The stable [`Errno`] for the VFS refusal (a missing path, an
    /// unauthorised caller, a read-only mount), or [`Errno::NotImplemented`]
    /// when no filesystem is mounted or the format keeps no ownership.
    fn set_owner(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _at: At<'_>,
        _new_uid: u32,
        _new_gid: u32,
        _final_link: FinalLink,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    /// Whether the caller may have the node `at` names as `want` asks,
    /// answered as the caller: its mode bits, its ACL and its capability gate
    /// under the caller's own identity, then the projection's flags — a write
    /// on a read-only projection is [`Errno::ReadOnly`], running a regular
    /// file on a `noexec` one [`Errno::PermissionDenied`]. With nothing asked
    /// it answers whether the node exists. Under [`FinalLink::Keep`] it asks
    /// about a final link itself.
    ///
    /// # Errors
    ///
    /// The first refusal, the walk's, or [`Errno::NotImplemented`] for a
    /// service that resolves nothing, which the default is.
    fn access(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _at: At<'_>,
        _want: Wanted,
        _final_link: FinalLink,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    /// The working directory `at` names, which must be a directory the
    /// caller may search, pinned for as long as it is held: the view root's
    /// floor answers [`Cwd::Root`], a projection no volume backs
    /// [`Cwd::Catalog`].
    ///
    /// # Errors
    ///
    /// [`Errno::NotADirectory`], [`Errno::PermissionDenied`], the walk's
    /// refusal, or [`Errno::NotImplemented`] for a service that resolves
    /// nothing, which the default is.
    fn chdir(&self, _uid: u32, _caps: &dyn CapabilityQuery, _at: At<'_>) -> Result<Cwd, Errno> {
        Err(Errno::NotImplemented)
    }

    /// The canonical path of `start`, as `fs_getcwd` answers it: the label a
    /// held directory was opened as, while it still reaches that directory by
    /// its own names; otherwise the names found climbing from it to its
    /// projection's floor, each in its parent's listing, which the caller
    /// must be able to read.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for a directory no name reaches any more,
    /// [`Errno::Stale`] for one whose projection or registration is gone, or
    /// [`Errno::NotImplemented`] for a service that resolves nothing, which
    /// the default is.
    fn canonical_of(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _start: Start<'_>,
    ) -> Result<String, Errno> {
        Err(Errno::NotImplemented)
    }

    /// Read up to `buf.len()` bytes of the node `node` holds at byte
    /// `offset`, as `who`, returning the number read (`0` at end of file).
    ///
    /// Every node operation is refused with [`Errno::Stale`] once the node's
    /// projection has left the mount table or its registration is retired,
    /// judges the node's capability gate and, unless the owner's exemption
    /// applies ([`Metadata::authorize_held`](crate::fs::Metadata::authorize_held)),
    /// its mode and ACL, and searches nothing above it.
    ///
    /// # Errors
    ///
    /// [`Errno::Stale`], [`Errno::IsADirectory`], [`Errno::PermissionDenied`],
    /// or the driver's refusal. The default fails closed with
    /// [`Errno::NotImplemented`].
    fn read_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _offset: u64,
        _buf: &mut [u8],
    ) -> Result<usize, Errno> {
        Err(Errno::NotImplemented)
    }

    /// Write `data` to the node at byte `offset`, or at its current end when
    /// `append`, as `who`, returning the number of bytes written.
    ///
    /// # Errors
    ///
    /// As [`read_node`](Self::read_node), and [`Errno::ReadOnly`] on a
    /// read-only projection.
    fn write_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _offset: u64,
        _append: bool,
        _data: &[u8],
    ) -> Result<usize, Errno> {
        Err(Errno::NotImplemented)
    }

    /// Set the length of the node's file to `size` bytes, as `who`.
    ///
    /// # Errors
    ///
    /// As [`write_node`](Self::write_node).
    fn truncate_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _size: u64,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    /// Punch a hole of `len` bytes at `offset` in the node's file, as `who`:
    /// the range reads as zeros and its storage is released, the file's
    /// length unchanged.
    ///
    /// # Errors
    ///
    /// [`Errno::NotSupported`] where the format keeps no holes, else as
    /// [`write_node`](Self::write_node).
    fn punch_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _offset: u64,
        _len: u64,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    /// Where the next region of `region`'s kind begins at or after `offset`
    /// of the node's file, as `who`.
    ///
    /// # Errors
    ///
    /// [`Errno::NoData`] when none does — the offset is at or past the end,
    /// or only holes follow it — else as [`read_node`](Self::read_node).
    fn seek_region_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _offset: u64,
        _region: SeekRegion,
    ) -> Result<u64, Errno> {
        Err(Errno::NotImplemented)
    }

    /// Report the node's structural metadata, which needs no access to it:
    /// a node no name reaches any more reports a name count of `0`.
    ///
    /// # Errors
    ///
    /// [`Errno::Stale`], or the driver's refusal. The default fails closed
    /// with [`Errno::NotImplemented`].
    fn stat_node(&self, _node: &NodeHandle, _who: &Acting<'_>) -> Result<FileStat, Errno> {
        Err(Errno::NotImplemented)
    }

    /// Hand `each` the entries of the node's directory from where `at`
    /// stands — its own entries, then, for a projection's floor, the
    /// projections attached beneath it that the volume holds no entry for —
    /// until it answers [`DirVisit::Stop`] or the listing ends, moving `at`
    /// past every entry it takes. A directory no name reaches any more lists
    /// nothing.
    ///
    /// # Errors
    ///
    /// [`Errno::NotADirectory`], else as [`read_node`](Self::read_node).
    fn readdir_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _at: &mut Listing,
        _each: &mut dyn FnMut(&DirEntry<'_>) -> DirVisit,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    /// For each of `names`, what [`readdir_node`](Self::readdir_node) would
    /// report for it now, or [`None`] where no such entry exists. With no
    /// names it is the listing's authorisation alone.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for a directory no name reaches any more, else as
    /// [`readdir_node`](Self::readdir_node).
    fn lookup_entries_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _names: &[&[u8]],
    ) -> Result<LookedUp, Errno> {
        Err(Errno::NotImplemented)
    }

    /// Flush the pending writes of the node's volume, and no other volume's.
    ///
    /// # Errors
    ///
    /// [`Errno::Stale`], or the driver's refusal. The default fails closed
    /// with [`Errno::NotImplemented`].
    fn sync_node(&self, _node: &NodeHandle, _who: &Acting<'_>) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    /// Set the permission bits of the node `node` holds to `mode`, as `who`:
    /// the `fchmod(2)` shape of [`set_mode`](Self::set_mode). Only the
    /// node's owner may, and never through a grant, whose holder could not
    /// restore what it changed.
    ///
    /// # Errors
    ///
    /// [`Errno::Stale`], [`Errno::ReadOnly`] on a read-only projection,
    /// [`Errno::PermissionDenied`], [`Errno::OutOfRange`] for a mode past
    /// the permission mask, or the driver's refusal. The default fails
    /// closed with [`Errno::NotImplemented`].
    fn set_mode_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _mode: u32,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    /// Set the owning user and/or group of the node `node` holds, as `who`:
    /// the `fchown(2)` shape of [`set_owner`](Self::set_owner), refused
    /// through a grant.
    ///
    /// # Errors
    ///
    /// As [`set_mode_node`](Self::set_mode_node).
    fn set_owner_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _new_uid: u32,
        _new_gid: u32,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    /// Read the extended attribute `key` of the node `node` holds into
    /// `value_out`, as `who`, returning the value's byte count (the
    /// `fgetxattr(2)` shape).
    ///
    /// The key must satisfy the shared `lib/fsmeta` grammar, `who` must be
    /// able to read the node as a read through the descriptor would, and the
    /// privileged namespaces are refused.
    ///
    /// # Errors
    ///
    /// [`Errno::Stale`], [`Errno::NoData`] when the node carries no such
    /// attribute, [`Errno::BufferTooSmall`] when the value does not fit,
    /// [`Errno::OutOfRange`] for a malformed key, [`Errno::NotSupported`]
    /// when the format stores no attributes, or [`Errno::PermissionDenied`].
    /// The default fails closed with [`Errno::NotImplemented`].
    fn attr_get_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _key: &[u8],
        _value_out: &mut [u8],
    ) -> Result<usize, Errno> {
        Err(Errno::NotImplemented)
    }

    /// Set (insert or replace) the extended attribute `key` of the node
    /// `node` holds to `value`, as `who` (the `fsetxattr(2)` shape): write
    /// permission in place of read, on a writable projection.
    ///
    /// # Errors
    ///
    /// As [`attr_get_node`](Self::attr_get_node), [`Errno::ReadOnly`] on a
    /// read-only projection, and [`Errno::NoSpace`] at the per-inode
    /// attribute bounds.
    fn attr_set_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _key: &[u8],
        _value: &[u8],
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    /// Yield the `index`-th visible extended-attribute key of the node
    /// `node` holds into `key_out`, as `who`, returning its byte count, or
    /// `None` once `index` is past the last visible attribute. Keys `who`
    /// may not read are omitted, never revealed.
    ///
    /// # Errors
    ///
    /// As [`attr_get_node`](Self::attr_get_node).
    fn attr_list_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _index: u64,
        _key_out: &mut [u8],
    ) -> Result<Option<usize>, Errno> {
        Err(Errno::NotImplemented)
    }

    /// Remove the extended attribute `key` of the node `node` holds, as
    /// `who` (the `fremovexattr(2)` shape), gated as
    /// [`attr_set_node`](Self::attr_set_node).
    ///
    /// # Errors
    ///
    /// As [`attr_set_node`](Self::attr_set_node).
    fn attr_remove_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _key: &[u8],
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    /// Hand `each` the projections attached beneath the catalog `catalog`,
    /// in name order, from where `at` stands, as `uid` holding `caps`.
    ///
    /// # Errors
    ///
    /// [`Errno::Stale`] once the catalog has left the mount table,
    /// [`Errno::PermissionDenied`] when the caller may not list it. The
    /// default fails closed with [`Errno::NotImplemented`].
    fn readdir_catalog(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _catalog: &Projection,
        _at: &mut Listing,
        _each: &mut dyn FnMut(&DirEntry<'_>) -> DirVisit,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    /// Tell the driver of `volume` about the pins its descriptors released,
    /// if its lock is free this instant; the write-back flusher reaps
    /// whatever this leaves. A retired registration's releases are dropped.
    fn reap(&self, _volume: &VolumeNodes) {}

    /// Whether `who` may have `want` access to `node` through its descriptor,
    /// its gate, mode and ACL judged as `judged` says.
    ///
    /// # Errors
    ///
    /// [`Errno::PermissionDenied`], [`Errno::Stale`] for a node whose
    /// projection or registration is gone, or [`Errno::NotImplemented`] for a
    /// service that serves no node.
    fn access_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _want: Access,
        _judged: Judged,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    /// The watch table of the registration `node` is pinned on, which alone
    /// reports changes to that pin. [`None`] while the registration is gone,
    /// or for a volume nothing reports changes for.
    fn watch_table(&self, _node: &NodeHandle) -> Option<Arc<crate::fswatch::VolumeWatch>> {
        None
    }

    /// Snapshot every attached volume whose driver is online as a wire-ready
    /// [`MountRecord`], in attach order, for the System Information
    /// introspection feed.
    ///
    /// This is a read-only, system-wide, secret-free observation — it names
    /// the mounted volumes and their permission flags, never file contents —
    /// so it is ungated at this seam (the `sysinfo_introspect` syscall that
    /// reaches it is itself capability-gated, and the `sysinfod` broker
    /// applies any per-client policy). The default returns an empty snapshot,
    /// so a service that owns no mount table (the fail-closed
    /// [`NullFilesystemService`]) truthfully reports "no mounts" rather than
    /// fabricating one.
    fn mount_snapshot(&self) -> Vec<MountRecord> {
        Vec::new()
    }

    /// Snapshot every view binding as a wire-ready [`ViewBindingRecord`], in
    /// mount-table order: each onto a volume [`mount_snapshot`] reports, and
    /// each policy-only binding showing none. Ungated and empty by default on
    /// the same terms.
    ///
    /// [`mount_snapshot`]: Self::mount_snapshot
    fn view_snapshot(&self) -> Vec<ViewBindingRecord> {
        Vec::new()
    }

    /// Snapshot every fault-aware block-backed volume's live I/O health as
    /// wire-ready [`VolumeIoHealthRecord`]s, one per volume whose backing
    /// device the kernel observes, for the `sysinfo` volume-health query
    /// (`plans/FIX-IO.md` IO5).
    ///
    /// Like [`mount_snapshot`](Self::mount_snapshot) this is a read-only,
    /// system-wide, secret-free observation (the per-device outcome tallies
    /// and current availability, never file contents); the
    /// `sysinfo_introspect` syscall that reaches it is capability-gated and
    /// the `sysinfod` broker applies the per-client `CAP_SYSINFO_KERNEL`
    /// policy. The default returns an empty snapshot, so a service owning no
    /// mount table (the fail-closed [`NullFilesystemService`]) truthfully
    /// reports "no volumes" rather than fabricating one.
    fn volume_io_health_snapshot(&self) -> Vec<VolumeIoHealthRecord> {
        Vec::new()
    }

    /// Snapshot every fault-aware block-backed volume's cumulative I/O
    /// service counters as wire-ready [`VolumeIoStatsRecord`]s, in the same
    /// order and keyed the same way as
    /// [`volume_io_health_snapshot`](Self::volume_io_health_snapshot), for
    /// the `sysinfo` per-volume service query.
    ///
    /// A read-only, system-wide, secret-free observation like its health
    /// sibling — cumulative bytes, completed requests, device-busy time and
    /// summed waits, never file contents — and ungated at the broker for the
    /// same reason `CPU_TIME_STATS` is: a utilisation figure is one every
    /// user may see. The default returns an empty snapshot, so a service
    /// owning no mount table truthfully reports "no volumes".
    fn volume_io_stats_snapshot(&self) -> Vec<VolumeIoStatsRecord> {
        Vec::new()
    }

    /// Snapshot every fault-aware block-backed volume's live queue occupancy
    /// and the per-device budget it is bounded by, as wire-ready
    /// [`VolumeIoQueueRecord`]s in the same order and keyed the same way as
    /// [`volume_io_health_snapshot`](Self::volume_io_health_snapshot), for
    /// the `sysinfo` per-volume queue query.
    ///
    /// Read-only and secret-free, but a queue depth is a driver and
    /// scheduler internal, so the broker gates it on `CAP_SYSINFO_KERNEL`
    /// where the service counters are ungated. The default returns an empty
    /// snapshot.
    fn volume_io_queue_snapshot(&self) -> Vec<VolumeIoQueueRecord> {
        Vec::new()
    }

    /// Run `remove` — the caller's hardware-tree removal closure — but only
    /// while no attached volume is served from one of `endpoints`, deciding
    /// the busy check and the removal together so an attach cannot race in
    /// between.
    ///
    /// This backs the orderly (stop-if-idle) `hw_remove_node`: `endpoints`
    /// are the block-service endpoint base ids the node being retired
    /// declares. A real disk-backed service holds its mount registry's lock
    /// across *both* the busy check and `remove`, so the check and the
    /// removal are atomic with respect to a concurrent attach (which
    /// registers under the same lock). If any attached volume is served from
    /// an endpoint in `endpoints` the service returns [`Errno::Busy`] and
    /// **never calls `remove`** (fail closed, nothing removed); otherwise it
    /// calls `remove` and returns its result — the ids of every node the
    /// removal retired.
    ///
    /// The default holds no volume registry, so it can never be busy: it
    /// simply calls `remove`. A service owning no mount table (the
    /// fail-closed [`NullFilesystemService`]) thus never spuriously refuses a
    /// removal.
    ///
    /// # Errors
    ///
    /// [`Errno::Busy`] when a volume is still attached on one of `endpoints`;
    /// otherwise whatever `remove` returns.
    fn remove_if_endpoints_idle(
        &self,
        _endpoints: &[u64],
        remove: &mut dyn FnMut() -> Result<Vec<u32>, Errno>,
    ) -> Result<Vec<u32>, Errno> {
        remove()
    }
}

/// The fail-closed default filesystem service: every operation reports
/// [`Errno::NotImplemented`].
///
/// Held by the syscall handlers until the boot path installs the real
/// disk-backed service (mirrors [`crate::users::NULL_USERS_DB`] and
/// [`crate::hwtree::NULL_HW_TREE`]). A kernel with no mounted filesystem thus
/// refuses every `fs_*` syscall rather than fabricating a handle or a read.
pub struct NullFilesystemService;

impl FilesystemService for NullFilesystemService {
    fn open(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _at: At<'_>,
        _flags: OpenFlags,
        _mode: CreateMode,
    ) -> Result<Opened, Errno> {
        Err(Errno::NotImplemented)
    }

    fn stat(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _at: At<'_>,
        _final_link: FinalLink,
    ) -> Result<FileStat, Errno> {
        Err(Errno::NotImplemented)
    }

    fn symlink(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _target: &str,
        _at: At<'_>,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn readlink(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _at: At<'_>,
    ) -> Result<String, Errno> {
        Err(Errno::NotImplemented)
    }

    fn realpath(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _at: At<'_>,
        _mode: RealpathMode,
    ) -> Result<String, Errno> {
        Err(Errno::NotImplemented)
    }

    fn link(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _existing: At<'_>,
        _new: At<'_>,
        _existing_link: FinalLink,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn sync_all(&self, _uid: u32, _caps: &dyn CapabilityQuery) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn mkdir(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _at: At<'_>,
        _mode: CreateMode,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn unlink(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _at: At<'_>,
        _flags: UnlinkFlags,
        _expect: Option<&NodeExpect>,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn rename(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _src: At<'_>,
        _dst: At<'_>,
        _flags: RenameFlags,
        _expect: Option<&NodeExpect>,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn set_mode(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _at: At<'_>,
        _mode: u32,
        _final_link: FinalLink,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }
}

/// The shared fail-closed filesystem service the handlers hold until the
/// boot path installs the real one.
pub static NULL_FILESYSTEM: NullFilesystemService = NullFilesystemService;
