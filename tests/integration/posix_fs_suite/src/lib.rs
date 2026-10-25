//! Shared harness for the pjdfstest-equivalent POSIX filesystem
//! conformance suite (`PLAN.md` Stage 5).
//!
//! The suite drives the **real** `arxfs` driver
//! ([`tairix_drv_fs_arxfs::ARXFS`]) through the kernel's **real**
//! filesystem service ([`MountedFilesystemService`]) — the one every `fs_*`
//! syscall is served through — and asserts the POSIX-visible semantics of
//! every filesystem operation the system exposes: directory and file
//! creation, unlink/rmdir, truncate, readdir/stat, the permission model (mode
//! bits, ACLs, and the optional per-inode capability gate), the on-disk
//! layout rules, and the stable errno mapping. It is the analogue of
//! `pjdfstest`: a body of black-box assertions about return values and error
//! codes, run against the production code paths rather than a parallel
//! re-implementation.
//!
//! Like `pjdfstest`, the suite is filesystem-agnostic by construction: it
//! talks to the service and a `drivers/filesystem/*` driver behind the frozen
//! ABI traits, so a second driver can be exercised by swapping the backing
//! constructor. `arxfs` is the first subject because it is the native FS
//! that stores a full per-inode record, which the
//! capability/ACL-gate assertions require.
//!
//! The block-device-over-QEMU mount path is covered separately by the
//! `arxfs`/`fat32` virtio-blk verticals; this crate is the *semantics*
//! suite and runs on the host against the identical driver and kernel code.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::collections::BTreeSet;
use std::sync::Arc;

use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::driver::filesystem::MountFlags;
use tairix_abi::driver::DriverHandle;
use tairix_abi::{DriverError, FS_INITIAL_UMASK, FS_NEW_DIR_MODE, FS_NEW_FILE_MODE};
use tairix_drv_fs_arxfs::{EntropySource, Keying, ARXFS};
use tairix_kernel_core::aspace::OpenBacking;
use tairix_kernel_core::fs::{Acting, At, DriverGuard, FilesystemService, NodeHandle, Start};
use tairix_kernel_core::{DriverLock, LateFilesystem, LateIdentity, MountedFilesystemService};
use tairix_kernel_sec::{GroupRecord, IdentityTable, IdentityTableBuilder, UserRecord};
use tairix_log::{Event, Sink};

/// Deterministic stand-in for the platform RNG seam: a byte counter that gives
/// `ARXFS::format` distinct, reproducible key material and UUID. Test
/// scaffolding only, never a production entropy source.
struct SuiteEntropy {
    next: u8,
}

impl EntropySource for SuiteEntropy {
    fn fill(&mut self, out: &mut [u8]) -> Result<(), DriverError> {
        for byte in out.iter_mut() {
            *byte = self.next;
            self.next = self.next.wrapping_add(1);
        }
        Ok(())
    }
}

/// The identity-table verifier audits its outcome; the suite asserts
/// behaviour, not the audit trail.
struct QuietSink;

impl Sink for QuietSink {
    fn write_event(&self, _event: &Event<'_>) {}
}

pub use tairix_abi::driver::filesystem::{
    FilesystemRead, FilesystemSecurity, FilesystemStats, FilesystemWrite, NodeId, NodeKind,
    NodeSecurity, SecurityAcl, SecuritySubject,
};
pub use tairix_abi::{
    CapabilityId, Errno, FileKind, FileStat, OpenFlags, RenameFlags, UnlinkFlags,
};
pub use tairix_caps::CapabilitySet;
pub use tairix_kernel_core::fs::{
    Attachment, CreateMode, Credentials, FinalLink, Metadata, Mode, Path, ReaddirEntry, Vfs,
    VfsError,
};
pub use tairix_kernel_sec::{GroupId, UserId};

/// Logical block (sector) size of the in-memory device, in bytes. The
/// `arxfs` minimum block size, matching the verticals' 512-byte sectors.
pub const SECTOR_BYTES: usize = 512;

/// Size of the in-memory device, in 512-byte sectors (1 MiB) — large
/// enough for the inode table, bitmap, journal, and a non-trivial data
/// region.
pub const TOTAL_SECTORS: u64 = 2048;

/// Number of inodes the test volume is formatted with.
pub const INODE_COUNT: u32 = 64;

/// Mount point at which the `arxfs` volume is attached in the test VFS.
/// Lives under `/Storage`, the top-level directory for mounted
/// volumes, which is writable in the default layout.
pub const MOUNT: &str = "/Storage/vol";

/// Uid and gid of the volume root `ARXFS::format` lays down, and so of every
/// node a credential with this identity creates. It owns the whole tree, so
/// it stands in for the administrative installer user that lays the volume
/// down.
pub const ROOT_UID: u32 = 0;
/// Owning gid of the `arxfs` volume root and freshly created nodes.
pub const ROOT_GID: u32 = 0;

/// A group the installer belongs to.
pub const STAFF_GID: u32 = 40;
/// A group an ACL entry may grant.
pub const SHARED_GID: u32 = 42;

/// An account the suite's identity table holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Account {
    /// The account's uid.
    pub uid: u32,
    /// Its primary group.
    pub gid: u32,
    /// Its supplementary groups.
    pub groups: &'static [u32],
}

/// The installer, owner of the volume root and of every node it creates, a
/// member of [`STAFF_GID`].
pub const ROOT: Account = Account {
    uid: ROOT_UID,
    gid: ROOT_GID,
    groups: &[STAFF_GID],
};
/// An ordinary account the planted records name as their owner.
pub const OWNER: Account = Account {
    uid: 1000,
    gid: 1000,
    groups: &[],
};
/// An account no planted record names.
pub const STRANGER: Account = Account {
    uid: 2000,
    gid: 2000,
    groups: &[],
};
/// An account in [`SHARED_GID`].
pub const MEMBER: Account = Account {
    uid: 2001,
    gid: 2001,
    groups: &[SHARED_GID],
};

/// Every account the suite acts as.
const ACCOUNTS: [Account; 4] = [ROOT, OWNER, STRANGER, MEMBER];

/// Who a case acts as: an account's uid, with the capabilities its task
/// carries.
#[derive(Copy, Clone, Debug)]
pub struct Caller {
    /// The account's uid.
    pub uid: u32,
    /// The capabilities the caller holds.
    pub caps: CapabilitySet,
}

impl Account {
    /// This account, holding no capability.
    #[must_use]
    pub const fn caller(self) -> Caller {
        self.holding(CapabilitySet::empty())
    }

    /// This account, holding `caps`.
    #[must_use]
    pub const fn holding(self, caps: CapabilitySet) -> Caller {
        Caller {
            uid: self.uid,
            caps,
        }
    }
}

/// The live `arxfs` driver instance the suite drives, bound to the
/// in-memory [`VecBlock`] device.
pub type LiveFs = ARXFS<VecBlock>;

/// An in-memory [`Block`] device backing the test volume. Addresses
/// [`SECTOR_BYTES`]-byte sectors exactly as the virtio-blk device the
/// verticals mount does.
pub struct VecBlock {
    store: Vec<u8>,
}

impl VecBlock {
    /// A zeroed device of `sectors` sectors.
    #[must_use]
    pub fn new(sectors: u64) -> Self {
        let len = usize::try_from(sectors).unwrap_or(0) * SECTOR_BYTES;
        Self {
            store: vec![0u8; len],
        }
    }

    /// Byte span `[start, end)` for `len` bytes at sector `lba`, or an
    /// error if the access is unaligned or out of range.
    fn span(&self, lba: u64, len: usize) -> Result<(usize, usize), DriverError> {
        if len == 0 || !len.is_multiple_of(SECTOR_BYTES) {
            return Err(DriverError::BufferTooSmall);
        }
        let start = usize::try_from(lba)
            .ok()
            .and_then(|l| l.checked_mul(SECTOR_BYTES))
            .ok_or(DriverError::LengthOutOfRange)?;
        let end = start
            .checked_add(len)
            .ok_or(DriverError::LengthOutOfRange)?;
        if end > self.store.len() {
            return Err(DriverError::LengthOutOfRange);
        }
        Ok((start, end))
    }
}

impl Block for VecBlock {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: u32::try_from(SECTOR_BYTES).unwrap_or(0),
            block_count: TOTAL_SECTORS,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let (start, end) = self.span(lba, buf.len())?;
        buf.copy_from_slice(&self.store[start..end]);
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        let (start, end) = self.span(lba, buf.len())?;
        self.store[start..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

/// Parse an absolute path, panicking if it is malformed.
///
/// # Panics
///
/// Panics if `text` is not a valid absolute path. The suite only ever
/// passes path literals it controls, so a failure is a test bug.
#[must_use]
pub fn path(text: &str) -> Path {
    Path::parse(text).expect("test path literal is a valid absolute path")
}

/// A path inside the mounted `arxfs` volume: [`MOUNT`] joined with `rel`
/// (which must not begin with `/`).
#[must_use]
pub fn vol_path(rel: &str) -> String {
    format!("{MOUNT}/{rel}")
}

/// What a creator that names no mode asks of a new file under the initial
/// umask: born `0o644`.
#[must_use]
pub fn new_file() -> CreateMode {
    CreateMode::new(FS_NEW_FILE_MODE, FS_INITIAL_UMASK).expect("a creator's default is in range")
}

/// What a creator that names no mode asks of a new directory under the
/// initial umask: born `0o755`.
#[must_use]
pub fn new_dir() -> CreateMode {
    CreateMode::new(FS_NEW_DIR_MODE, FS_INITIAL_UMASK).expect("a creator's default is in range")
}

/// Build a [`Credentials`] for `(uid, gid)` with no supplementary groups
/// and the borrowed capability set `caps`, for the in-RAM layout tree.
#[must_use]
pub fn cred(uid: u32, gid: u32, caps: &CapabilitySet) -> Credentials<'_> {
    Credentials {
        uid: UserId(uid),
        gid: GroupId(gid),
        supplementary_gids: &[],
        caps,
    }
}

/// A default-layout [`Vfs`] (owner `(ROOT_UID, ROOT_GID)`) with no driver
/// mounted, for the in-RAM layout-enforcement tests.
#[must_use]
pub fn default_layout_vfs() -> Vfs {
    Vfs::with_default_layout(UserId(ROOT_UID), GroupId(ROOT_GID))
}

/// Each entry's kind and name, as a listing reports them.
#[must_use]
pub fn kinds(entries: &[ReaddirEntry]) -> Vec<(FileKind, &str)> {
    entries
        .iter()
        .map(|entry| (entry.kind, entry.name.as_str()))
        .collect()
}

/// A freshly formatted plaintext `arxfs` volume — a shipped root's
/// protection — of the suite's fixed geometry.
fn formatted() -> LiveFs {
    ARXFS::format(
        VecBlock::new(TOTAL_SECTORS),
        INODE_COUNT,
        Keying::Plaintext,
        &mut SuiteEntropy { next: 1 },
    )
    .expect("format the fixed-geometry arxfs test volume")
}

/// The identity table holding every [`Account`] the suite acts as.
fn identity_table() -> IdentityTable {
    let mut builder = IdentityTableBuilder::new();
    let mut gids = BTreeSet::new();
    for account in ACCOUNTS {
        gids.insert(account.gid);
        gids.extend(account.groups.iter().copied());
        builder.push_user(UserRecord {
            uid: UserId(account.uid),
            primary_gid: GroupId(account.gid),
            supplementary_gids: account.groups.iter().copied().map(GroupId).collect(),
            capability_grants: CapabilitySet::empty(),
        });
    }
    for gid in gids {
        builder.push_group(GroupRecord { gid: GroupId(gid) });
    }
    builder
        .verify(&QuietSink)
        .expect("well-formed identity table")
}

/// `path`, an absolute one, as the filesystem service takes it.
fn root(path: &str) -> At<'_> {
    At {
        start: Start::Root,
        path,
    }
}

/// A freshly formatted `arxfs` volume mounted at [`MOUNT`] in a
/// default-layout VFS, served through the kernel's filesystem service as
/// production serves every mounted volume, every [`Account`] in its identity
/// table.
pub struct Volume {
    service: MountedFilesystemService<LiveFs>,
    driver: Arc<DriverLock<LiveFs>>,
}

/// Mount a fresh volume at [`MOUNT`], read-only when `read_only`, so a case
/// can exercise the read-only-mount refusal on a backed subtree.
///
/// # Panics
///
/// Panics if the fixed-geometry volume fails to format or the mount
/// point cannot be laid down — either is a test-harness bug, not a
/// runtime condition.
#[must_use]
pub fn arxfs_volume(read_only: bool) -> Volume {
    let vfs = Vfs::with_default_layout(UserId(ROOT_UID), GroupId(ROOT_GID));
    let handle = DriverHandle::from_raw(0x5f5f).expect("non-zero driver handle");
    let flags = if read_only {
        MountFlags::READ_ONLY
    } else {
        MountFlags::default()
    };
    let template = Metadata::new(UserId(ROOT_UID), GroupId(ROOT_GID), Mode::from_bits(0o755));
    // Attached under `/Storage` as a runtime volume is, its mount point's
    // template travelling with it. The suite's volume is an in-RAM block
    // vector, so it declares no storage medium: unknown is the honest record.
    vfs.mounts_write()
        .attach_at(Attachment::new(handle, None, flags), path(MOUNT), template)
        .expect("mount the arxfs volume");
    let mounted: &'static LateFilesystem<LiveFs> =
        tairix_kernel_core::forever(Box::new(LateFilesystem::new()));
    mounted.install_vfs(vfs).expect("install the vfs");
    let driver = mounted
        .register(handle, formatted(), "vol", "arxfs", [0u8; 16], None)
        .expect("register the volume");
    let identity: &'static LateIdentity =
        tairix_kernel_core::forever(Box::new(LateIdentity::new()));
    identity
        .install(identity_table())
        .expect("install the identity table");
    Volume {
        service: MountedFilesystemService::new(mounted, identity),
        driver,
    }
}

impl Volume {
    /// Open `path` as `who` under `flags`, asking `mode` of a create, and
    /// close it again at once: what an open alone decides.
    pub fn open(
        &self,
        who: Caller,
        path: &str,
        flags: OpenFlags,
        mode: CreateMode,
    ) -> Result<(), Errno> {
        self.through(who, path, flags, mode, |_, _| Ok(()))
    }

    /// Make the regular file `path` as `who`, asking `mode`, as an exclusive
    /// create does.
    pub fn create(&self, who: Caller, path: &str, mode: CreateMode) -> Result<(), Errno> {
        let exclusive = OpenFlags::CREATE
            .union(OpenFlags::EXCLUSIVE)
            .union(OpenFlags::WRITE);
        self.open(who, path, exclusive, mode)
    }

    /// Make the directory `path` as `who`, asking `mode`.
    pub fn mkdir(&self, who: Caller, path: &str, mode: CreateMode) -> Result<(), Errno> {
        self.service.mkdir(who.uid, &who.caps, root(path), mode)
    }

    /// Read up to `buf.len()` bytes of `path` at `offset` as `who`, through
    /// a descriptor of its own.
    pub fn read(
        &self,
        who: Caller,
        path: &str,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, Errno> {
        self.through(
            who,
            path,
            OpenFlags::READ,
            CreateMode::NONE,
            |node, acting| self.service.read_node(node, acting, offset, buf),
        )
    }

    /// Write `data` to `path` at `offset` as `who`, through a descriptor of
    /// its own.
    pub fn write(&self, who: Caller, path: &str, offset: u64, data: &[u8]) -> Result<usize, Errno> {
        self.through(
            who,
            path,
            OpenFlags::WRITE,
            CreateMode::NONE,
            |node, acting| self.service.write_node(node, acting, offset, false, data),
        )
    }

    /// Set the length of `path` to `size` as `who`, through a descriptor of
    /// its own.
    pub fn truncate(&self, who: Caller, path: &str, size: u64) -> Result<(), Errno> {
        self.through(
            who,
            path,
            OpenFlags::WRITE,
            CreateMode::NONE,
            |node, acting| self.service.truncate_node(node, acting, size),
        )
    }

    /// What `path` is, as `who` stats it under `final_link`.
    pub fn stat(&self, who: Caller, path: &str, final_link: FinalLink) -> Result<FileStat, Errno> {
        self.service
            .stat(who.uid, &who.caps, root(path), final_link)
    }

    /// The whole listing of the directory `path` as `who` reads it.
    pub fn list(&self, who: Caller, path: &str) -> Result<Vec<ReaddirEntry>, Errno> {
        self.service.readdir_bounded(
            who.uid,
            &who.caps,
            root(path),
            FinalLink::Follow,
            usize::MAX,
        )
    }

    /// Remove the name `path` as `who`, under `flags`.
    pub fn unlink(&self, who: Caller, path: &str, flags: UnlinkFlags) -> Result<(), Errno> {
        self.service
            .unlink(who.uid, &who.caps, root(path), flags, None)
    }

    /// Move `src` to `dst` as `who`.
    pub fn rename(&self, who: Caller, src: &str, dst: &str) -> Result<(), Errno> {
        let moved = self.service.rename(
            who.uid,
            &who.caps,
            root(src),
            root(dst),
            RenameFlags::empty(),
            None,
        );
        self.settle();
        moved
    }

    /// Give the node `existing` names, its final link treated as
    /// `existing_link`, the second name `new`, as `who`.
    pub fn link(
        &self,
        who: Caller,
        existing: &str,
        new: &str,
        existing_link: FinalLink,
    ) -> Result<(), Errno> {
        let linked =
            self.service
                .link(who.uid, &who.caps, root(existing), root(new), existing_link);
        self.settle();
        linked
    }

    /// Make the symbolic link `path`, storing `target`, as `who`.
    pub fn symlink(&self, who: Caller, path: &str, target: &str) -> Result<(), Errno> {
        self.service.symlink(who.uid, &who.caps, target, root(path))
    }

    /// The target the symbolic link `path` stores, as `who` reads it.
    pub fn readlink(&self, who: Caller, path: &str) -> Result<String, Errno> {
        self.service.readlink(who.uid, &who.caps, root(path))
    }

    /// Set the permission bits of `path` to `mode` as `who`.
    pub fn set_mode(&self, who: Caller, path: &str, mode: u32) -> Result<(), Errno> {
        self.service
            .set_mode(who.uid, &who.caps, root(path), mode, FinalLink::Follow)
    }

    /// Set the owner and group of `path` as `who`.
    pub fn set_owner(&self, who: Caller, path: &str, uid: u32, gid: u32) -> Result<(), Errno> {
        self.service
            .set_owner(who.uid, &who.caps, root(path), uid, gid, FinalLink::Follow)
    }

    /// The volume's driver, held, for a case that stamps or reads a node's
    /// stored record or the volume's space figures behind the service.
    #[must_use]
    pub fn driver(&self) -> DriverGuard<'_, LiveFs> {
        self.driver.lock()
    }

    /// The node `name` names directly under the volume root.
    ///
    /// # Panics
    ///
    /// Panics if `name` is not present at the volume root (a test bug).
    #[must_use]
    pub fn root_node(&self, name: &[u8]) -> NodeId {
        let mut fs = self.driver();
        let root = fs.root();
        fs.lookup(root, name)
            .expect("a node the case created is present at the volume root")
    }

    /// Close the volume and open the bytes it wrote again, in place under
    /// the same registration, so a case can prove a property is **on-disk**
    /// state rather than a mount-time derivation.
    ///
    /// # Panics
    ///
    /// Panics if the volume the suite just wrote does not close or re-open —
    /// a driver defect the suite is there to catch, not a runtime condition.
    pub fn remount(&self) {
        self.settle();
        let mut fs = self.driver();
        // A blank volume stands in while the written one is closed.
        let written = std::mem::replace(&mut *fs, formatted());
        let block = written
            .into_block()
            .expect("the volume closes before it is remounted");
        *fs = ARXFS::open(block, Keying::Plaintext).expect("re-open the volume just written");
    }

    /// `op` through a descriptor of `path` of `who`'s own, opened under
    /// `flags` asking `mode`, closed once `op` returns.
    fn through<R>(
        &self,
        who: Caller,
        path: &str,
        flags: OpenFlags,
        mode: CreateMode,
        op: impl FnOnce(&NodeHandle, &Acting<'_>) -> Result<R, Errno>,
    ) -> Result<R, Errno> {
        let opened = self
            .service
            .open(who.uid, &who.caps, root(path), flags, mode)?;
        let OpenBacking::Node(handle) = OpenBacking::opened(opened) else {
            // The catalog of attached volumes holds no bytes.
            return Err(Errno::IsADirectory);
        };
        let acting = Acting {
            uid: who.uid,
            caps: &who.caps,
            granted: false,
        };
        let done = op(&handle, &acting);
        drop(handle);
        self.settle();
        done
    }

    /// Hand the driver the holds the operations so far let go, so a case
    /// observes the volume as a quiet system would.
    fn settle(&self) {
        self.service.reap(self.driver.nodes());
    }
}
