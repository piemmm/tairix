//! Host-test fixture: an in-memory volume mounted at [`MOUNT`] behind the
//! secured service, shared by the service, delegation, change-notification
//! and syscall suites so none of them carries its own copy, and the open →
//! node operation → close a test makes through a path in one call.

use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::blkio::{BlkMedium, BlkTransport};
use tairix_abi::driver::filesystem::{
    DirEntry, DirVisit, FilesystemAttrsProvider, FilesystemRead, FilesystemSecurity,
    FilesystemStats, FilesystemWrite, MountFlags, NodeId, NodeInfo, NodeKind, NodeSecurity,
    Unpinned, VolumeHost, VolumeStats,
};
use tairix_abi::driver::{DriverError, DriverHandle};
use tairix_abi::{
    CapabilityQuery, Errno, OpenFlags, SeekRegion, FS_INITIAL_UMASK, FS_NEW_DIR_MODE,
    FS_NEW_FILE_MODE,
};
use tairix_caps::CapabilitySet;
use tairix_kernel_sec::{
    GroupId, GroupRecord, IdentityTable, IdentityTableBuilder, UserId, UserRecord,
};
use tairix_log::{Event, Sink};
use tairix_reclaim::{CacheBudget, ReclaimOwner};

use super::memfs::RwMockFs;
use super::{
    Acting, At, Attachment, CachedFs, CreateMode, DriverLock, FilesystemService, LateFilesystem,
    LateIdentity, Listing, Metadata, Mode, MountedFilesystemService, NodeHandle, Opened, Path,
    Projection, Start, Vfs,
};
use crate::aspace::OpenBacking;
use crate::fswatch::{VolumeWatch, WatchRegistry};
use crate::test_pressure::unpressured;
use crate::test_sink::TestSink;

pub(crate) const TEST_UID: u32 = 1000;
pub(crate) const TEST_GID: u32 = 1000;
/// The record the fixtures plant their own nodes with: the test principal's,
/// world-traversable.
pub(crate) const PLANTED: NodeSecurity = NodeSecurity::new(0o755, TEST_UID, TEST_GID);
/// The mount point the in-memory driver is mounted at.
pub(crate) const MOUNT: &str = "/Storage/vol";
/// The storage medium the fixture's block device declares, so the snapshot
/// can be checked against a value only the attach path could have supplied.
pub(crate) const MOUNT_MEDIUM: BlkMedium = BlkMedium::of_transport(BlkTransport::Nvme);

/// A sink that discards every event — the identity-table verifier audits its
/// outcome but these tests assert behaviour, not the audit trail.
pub(crate) struct NullSink;
impl Sink for NullSink {
    fn write_event(&self, _event: &Event<'_>) {}
}

pub(crate) fn caps() -> CapabilitySet {
    CapabilitySet::empty()
}

/// An account an identity table holds: its uid, its primary group and its
/// supplementary groups.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Account<'a> {
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) groups: &'a [u32],
}

/// The test principal, in no group but its own.
pub(crate) const TEST_ACCOUNT: Account<'static> = Account {
    uid: TEST_UID,
    gid: TEST_GID,
    groups: &[],
};

/// An identity table holding `accounts`, with a record of every group one of
/// them is in.
pub(crate) fn identity_of(accounts: &[Account<'_>]) -> IdentityTable {
    let mut builder = IdentityTableBuilder::new();
    let mut gids = BTreeSet::new();
    for account in accounts {
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
        .verify(&NullSink)
        .expect("well-formed identity table")
}

/// An identity table holding exactly the test principal (uid/gid 1000).
pub(crate) fn identity_table() -> IdentityTable {
    identity_of(&[TEST_ACCOUNT])
}

/// The handle the test volume is attached under.
pub(crate) fn volume_handle() -> DriverHandle {
    DriverHandle::from_raw(9).expect("non-zero handle")
}

/// `name` beneath [`MOUNT`].
pub(crate) fn path(name: &str) -> String {
    alloc::format!("{MOUNT}/{name}")
}

/// A default-layout VFS with the in-memory driver mounted at [`MOUNT`],
/// read-only when `read_only`.
pub(crate) fn vfs(read_only: bool) -> Vfs {
    let vfs = Vfs::with_default_layout(UserId(TEST_UID), GroupId(TEST_GID));
    let mount = Path::parse(MOUNT).expect("valid mount path");
    let handle = volume_handle();
    let flags = if read_only {
        MountFlags::READ_ONLY
    } else {
        MountFlags::from_bits(0).expect("empty flags")
    };
    let template = Metadata::new(UserId(TEST_UID), GroupId(TEST_GID), Mode::from_bits(0o755));
    // The fixture stands in for a volume attached from a classified block
    // device, so the medium it reports is the one the device declared.
    vfs.mounts_write()
        .attach_at(
            Attachment::new(handle, Some(MOUNT_MEDIUM), flags),
            mount,
            template,
        )
        .expect("mount backed");
    vfs
}

/// The in-memory driver, its root owned by the test principal so it can
/// traverse, create, and write.
pub(crate) fn driver() -> RwMockFs {
    let mut fs = RwMockFs::new();
    fs.set_root_security(PLANTED);
    fs
}

/// What [`serve`] stands up: the service, the cell its volumes are
/// registered in, and each volume's driver in the order given, for a test to
/// reach behind the service.
pub(crate) struct Serving<F: 'static> {
    pub(crate) service: MountedFilesystemService<F>,
    pub(crate) mounted: &'static LateFilesystem<F>,
    pub(crate) drivers: Vec<Arc<DriverLock<F>>>,
}

/// The service over `vfs`, each of `volumes` registered under its handle,
/// with an identity table holding `accounts`.
pub(crate) fn serve<F>(
    vfs: Vfs,
    volumes: Vec<(DriverHandle, F)>,
    accounts: &[Account<'_>],
) -> Serving<F>
where
    F: FilesystemWrite + Send + 'static,
{
    serve_as(vfs, volumes, accounts, false)
}

/// [`mounted`], its volume registered as a user-space server's.
pub(crate) fn mounted_served<F>(fs: F, accounts: &[Account<'_>]) -> Serving<F>
where
    F: FilesystemWrite + Send + 'static,
{
    serve_as(
        vfs(false),
        alloc::vec![(volume_handle(), fs)],
        accounts,
        true,
    )
}

/// [`serve`], each volume registered as a server's when `served`.
fn serve_as<F>(
    vfs: Vfs,
    volumes: Vec<(DriverHandle, F)>,
    accounts: &[Account<'_>],
    served: bool,
) -> Serving<F>
where
    F: FilesystemWrite + Send + 'static,
{
    let mounted: &'static LateFilesystem<F> = crate::forever(Box::new(LateFilesystem::new()));
    mounted.install_vfs(vfs).expect("install vfs");
    let drivers = volumes
        .into_iter()
        .map(|(handle, fs)| {
            if served {
                mounted.register_served(handle, fs, "vol", "memfs", [0u8; 16], None)
            } else {
                mounted.register(handle, fs, "vol", "memfs", [0u8; 16], None)
            }
            .expect("register driver")
        })
        .collect();
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    identity
        .install(identity_of(accounts))
        .expect("install identity");
    Serving {
        service: MountedFilesystemService::new(mounted, identity),
        mounted,
        drivers,
    }
}

/// [`serve`] over `fs` alone, mounted at [`MOUNT`], read-only when
/// `read_only`.
pub(crate) fn mounted<F>(fs: F, read_only: bool, accounts: &[Account<'_>]) -> Serving<F>
where
    F: FilesystemWrite + Send + 'static,
{
    serve(vfs(read_only), alloc::vec![(volume_handle(), fs)], accounts)
}

/// What [`watched_service`] stands up: the service, the volume's watch table,
/// the registry of the calling test's own that table lives in, and the
/// filesystem the service runs against, for a test that mounts beneath it.
pub(crate) struct Watched {
    pub(crate) service: &'static MountedFilesystemService<CachedFs<RwMockFs>>,
    pub(crate) watch: Arc<VolumeWatch>,
    pub(crate) registry: &'static WatchRegistry,
    pub(crate) mounted: &'static LateFilesystem<CachedFs<RwMockFs>>,
}

/// The service over an exact-name volume at [`MOUNT`] whose every mutation
/// is reported to its claimed watch table, as production registers one.
pub(crate) fn watched_service(volume: [u8; 16]) -> Watched {
    let fs = driver().with_exact_names();
    let registry: &'static WatchRegistry = crate::forever(Box::new(WatchRegistry::new()));
    let claim = registry
        .claim(volume, fs.name_matching(), unpressured())
        .expect("claims");
    let watch = Arc::clone(claim.table());
    let reference = claim.reference();
    let sink: &'static TestSink = crate::forever(Box::new(TestSink::new()));
    let cached = CachedFs::new(
        fs,
        CacheBudget::from_backing(16 << 20),
        ReclaimOwner::FilesystemVolume { volume: 9 },
        unpressured(),
        sink,
    )
    .with_watch(claim);
    let cell: &'static LateFilesystem<CachedFs<RwMockFs>> =
        crate::forever(Box::new(LateFilesystem::new()));
    cell.install_vfs(vfs(false)).expect("install vfs");
    cell.register(
        volume_handle(),
        cached,
        "vol",
        "memfs",
        volume,
        Some(reference),
    )
    .expect("register");
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    identity.install(identity_table()).expect("identity");
    let service: &'static MountedFilesystemService<CachedFs<RwMockFs>> =
        crate::forever(Box::new(MountedFilesystemService::new(cell, identity)));
    Watched {
        service,
        watch,
        registry,
        mounted: cell,
    }
}

/// What a creator that names no mode asks of a new file under the initial
/// umask: born `0o644`.
pub(crate) fn new_file() -> CreateMode {
    CreateMode::new(FS_NEW_FILE_MODE, FS_INITIAL_UMASK).expect("a creator's default is in range")
}

/// What a creator that names no mode asks of a new directory under the
/// initial umask: born `0o755`.
pub(crate) fn new_dir() -> CreateMode {
    CreateMode::new(FS_NEW_DIR_MODE, FS_INITIAL_UMASK).expect("a creator's default is in range")
}

/// What an open under `flags` asks a create for: [`new_file`] when it may
/// create.
pub(crate) fn creating(flags: OpenFlags) -> CreateMode {
    if flags.contains(OpenFlags::CREATE) {
        new_file()
    } else {
        CreateMode::NONE
    }
}

/// `path` as a process that has not changed directory names it.
pub(crate) fn at(path: &str) -> At<'_> {
    At {
        start: Start::Root,
        path,
    }
}

/// The identity the opener's own descriptor acts under: `uid` holding `caps`.
pub(crate) fn acting(uid: u32, caps: &dyn CapabilityQuery) -> Acting<'_> {
    Acting {
        uid,
        caps,
        granted: false,
    }
}

/// What the opener's own descriptor of what `opened` names holds, or the
/// catalog it opened.
fn own(opened: Opened) -> Result<NodeHandle, Arc<Projection>> {
    match OpenBacking::opened(opened) {
        OpenBacking::Node(handle) => Ok(handle),
        OpenBacking::Catalog(catalog) => Err(catalog),
        other => panic!("an open answers a node or the catalog, not {other:?}"),
    }
}

/// The handle the opener's own descriptor of `path`, opened as `uid`
/// holding `caps` under `flags`, holds; the catalog holds none.
pub(crate) fn own_handle(
    svc: &dyn FilesystemService,
    uid: u32,
    caps: &dyn CapabilityQuery,
    path: &str,
    flags: OpenFlags,
) -> Result<NodeHandle, Errno> {
    own(svc.open(uid, caps, at(path), flags, creating(flags))?).map_err(|_| Errno::IsADirectory)
}

/// Open `path` as `uid` holding `caps` under `flags`, and close it again at
/// once: what an open alone decides.
pub(crate) fn open_path(
    svc: &dyn FilesystemService,
    uid: u32,
    caps: &dyn CapabilityQuery,
    path: &str,
    flags: OpenFlags,
) -> Result<(), Errno> {
    release(svc, svc.open(uid, caps, at(path), flags, creating(flags))?);
    Ok(())
}

/// Make the regular file `path` as `uid` holding `caps`, asking `mode`, as
/// an exclusive create does, and close it again at once.
pub(crate) fn create_path(
    svc: &dyn FilesystemService,
    uid: u32,
    caps: &dyn CapabilityQuery,
    path: &str,
    mode: CreateMode,
) -> Result<(), Errno> {
    let flags = OpenFlags::CREATE
        .union(OpenFlags::EXCLUSIVE)
        .union(OpenFlags::WRITE);
    release(svc, svc.open(uid, caps, at(path), flags, mode)?);
    Ok(())
}

/// Let what `opened` holds go as a close does.
fn release(svc: &dyn FilesystemService, opened: Opened) {
    if let Ok(handle) = own(opened) {
        close(svc, handle);
    }
}

/// Let `handle` go as a close does, reaping its pin at once.
pub(crate) fn close(svc: &dyn FilesystemService, handle: NodeHandle) {
    let volume = Arc::clone(handle.node.volume());
    drop(handle);
    svc.reap(&volume);
}

/// `op` through the opener's own descriptor of `path`, opened under `flags`
/// and closed once `op` returns.
pub(crate) fn through<R>(
    svc: &dyn FilesystemService,
    uid: u32,
    caps: &dyn CapabilityQuery,
    path: &str,
    flags: OpenFlags,
    op: impl FnOnce(&NodeHandle, &Acting<'_>) -> Result<R, Errno>,
) -> Result<R, Errno> {
    let handle = own_handle(svc, uid, caps, path, flags)?;
    let done = op(&handle, &acting(uid, caps));
    close(svc, handle);
    done
}

/// Read `path` at `offset` through a descriptor of its own.
pub(crate) fn read_path(
    svc: &dyn FilesystemService,
    uid: u32,
    caps: &dyn CapabilityQuery,
    path: &str,
    offset: u64,
    buf: &mut [u8],
) -> Result<usize, Errno> {
    through(svc, uid, caps, path, OpenFlags::READ, |node, who| {
        svc.read_node(node, who, offset, buf)
    })
}

/// Write `data` to `path` at `offset`, or at its end when `append`, through
/// a descriptor of its own.
pub(crate) fn write_path(
    svc: &dyn FilesystemService,
    uid: u32,
    caps: &dyn CapabilityQuery,
    path: &str,
    offset: u64,
    append: bool,
    data: &[u8],
) -> Result<usize, Errno> {
    through(svc, uid, caps, path, OpenFlags::WRITE, |node, who| {
        svc.write_node(node, who, offset, append, data)
    })
}

/// Set the length of `path` to `size` through a descriptor of its own.
pub(crate) fn truncate_path(
    svc: &dyn FilesystemService,
    uid: u32,
    caps: &dyn CapabilityQuery,
    path: &str,
    size: u64,
) -> Result<(), Errno> {
    through(svc, uid, caps, path, OpenFlags::WRITE, |node, who| {
        svc.truncate_node(node, who, size)
    })
}

/// Punch `len` bytes at `offset` of `path` through a descriptor of its own.
pub(crate) fn punch_path(
    svc: &dyn FilesystemService,
    uid: u32,
    caps: &dyn CapabilityQuery,
    path: &str,
    offset: u64,
    len: u64,
) -> Result<(), Errno> {
    through(svc, uid, caps, path, OpenFlags::WRITE, |node, who| {
        svc.punch_node(node, who, offset, len)
    })
}

/// Where the next region of `region`'s kind begins in `path` at or after
/// `offset`, through a descriptor of its own.
pub(crate) fn seek_region_path(
    svc: &dyn FilesystemService,
    uid: u32,
    caps: &dyn CapabilityQuery,
    path: &str,
    offset: u64,
    region: SeekRegion,
) -> Result<u64, Errno> {
    through(svc, uid, caps, path, OpenFlags::READ, |node, who| {
        svc.seek_region_node(node, who, offset, region)
    })
}

/// Read the attribute `key` of `path` into `out` through a descriptor of its
/// own that asks no byte access.
pub(crate) fn attr_get_path(
    svc: &dyn FilesystemService,
    uid: u32,
    caps: &dyn CapabilityQuery,
    path: &str,
    key: &[u8],
    out: &mut [u8],
) -> Result<usize, Errno> {
    through(svc, uid, caps, path, OpenFlags::empty(), |node, who| {
        svc.attr_get_node(node, who, key, out)
    })
}

/// Set the attribute `key` of `path` to `value`, as [`attr_get_path`] reaches
/// it.
pub(crate) fn attr_set_path(
    svc: &dyn FilesystemService,
    uid: u32,
    caps: &dyn CapabilityQuery,
    path: &str,
    key: &[u8],
    value: &[u8],
) -> Result<(), Errno> {
    through(svc, uid, caps, path, OpenFlags::empty(), |node, who| {
        svc.attr_set_node(node, who, key, value)
    })
}

/// The `index`-th attribute key of `path`, as [`attr_get_path`] reaches it.
pub(crate) fn attr_list_path(
    svc: &dyn FilesystemService,
    uid: u32,
    caps: &dyn CapabilityQuery,
    path: &str,
    index: u64,
    out: &mut [u8],
) -> Result<Option<usize>, Errno> {
    through(svc, uid, caps, path, OpenFlags::empty(), |node, who| {
        svc.attr_list_node(node, who, index, out)
    })
}

/// Remove the attribute `key` of `path`, as [`attr_get_path`] reaches it.
pub(crate) fn attr_remove_path(
    svc: &dyn FilesystemService,
    uid: u32,
    caps: &dyn CapabilityQuery,
    path: &str,
    key: &[u8],
) -> Result<(), Errno> {
    through(svc, uid, caps, path, OpenFlags::empty(), |node, who| {
        svc.attr_remove_node(node, who, key)
    })
}

/// Flush the volume serving `path` through a descriptor of its own; the
/// catalog has nothing to flush.
pub(crate) fn sync_path(
    svc: &dyn FilesystemService,
    uid: u32,
    caps: &dyn CapabilityQuery,
    path: &str,
) -> Result<(), Errno> {
    match own(svc.open(uid, caps, at(path), OpenFlags::empty(), CreateMode::NONE)?) {
        Err(_) => Ok(()),
        Ok(handle) => {
            let done = svc.sync_node(&handle, &acting(uid, caps));
            close(svc, handle);
            done
        }
    }
}

/// The whole listing of `path`, opened under `flags`, handed to `each`
/// through one descriptor of its own.
pub(crate) fn list_path(
    svc: &dyn FilesystemService,
    uid: u32,
    caps: &dyn CapabilityQuery,
    path: &str,
    flags: OpenFlags,
    each: &mut dyn FnMut(&tairix_abi::DirEntry<'_>) -> DirVisit,
) -> Result<(), Errno> {
    match own(svc.open(uid, caps, at(path), flags, creating(flags))?) {
        Err(catalog) => svc.readdir_catalog(uid, caps, &catalog, &mut Listing::default(), each),
        Ok(handle) => {
            let done = svc.readdir_node(&handle, &acting(uid, caps), &mut Listing::default(), each);
            close(svc, handle);
            done
        }
    }
}

/// A driver that records every `flush` and can be made to refuse one, so a
/// test can prove which volumes the flusher published and what a refusal
/// costs. Every other method forwards, so the wrapper is a truthful mount.
pub(crate) struct FlushSpy {
    inner: RwMockFs,
    pub(crate) flushes: u32,
    refuse: bool,
    /// The handle the host installed on this driver, if any.
    pub(crate) host_volume: Option<DriverHandle>,
}

impl FlushSpy {
    pub(crate) fn new() -> Self {
        Self {
            inner: RwMockFs::new(),
            flushes: 0,
            refuse: false,
            host_volume: None,
        }
    }

    pub(crate) fn refusing() -> Self {
        Self {
            refuse: true,
            ..Self::new()
        }
    }
}

impl FilesystemRead for FlushSpy {
    fn root(&self) -> NodeId {
        self.inner.root()
    }

    fn name_matching(&self) -> tairix_abi::driver::filesystem::NameMatching {
        self.inner.name_matching()
    }

    fn node_identity(&self) -> tairix_abi::driver::filesystem::NodeIdentity {
        self.inner.node_identity()
    }

    fn node_info(&mut self, node: NodeId) -> Result<NodeInfo, DriverError> {
        self.inner.node_info(node)
    }

    fn lookup(&mut self, dir: NodeId, name: &[u8]) -> Result<NodeId, DriverError> {
        self.inner.lookup(dir, name)
    }

    fn parent(&mut self, dir: NodeId) -> Result<NodeId, DriverError> {
        self.inner.parent(dir)
    }

    fn read_at(&mut self, file: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize, DriverError> {
        self.inner.read_at(file, offset, buf)
    }

    fn read_dir(
        &mut self,
        dir: NodeId,
        cursor: u64,
        after: &[u8],
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError> {
        self.inner.read_dir(dir, cursor, after, visit)
    }

    fn read_link(&mut self, node: NodeId, out: &mut [u8]) -> Result<usize, DriverError> {
        self.inner.read_link(node, out)
    }

    fn pin(&mut self, node: NodeId) -> Result<NodeId, DriverError> {
        self.inner.pin(node)
    }

    fn unpin(&mut self, pinned: NodeId) -> Result<Unpinned, DriverError> {
        self.inner.unpin(pinned)
    }

    fn release_pins(&mut self) -> Result<(), DriverError> {
        self.inner.release_pins()
    }
}

impl FilesystemWrite for FlushSpy {
    fn create(
        &mut self,
        dir: NodeId,
        name: &[u8],
        kind: NodeKind,
        record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        self.inner.create(dir, name, kind, record)
    }

    fn create_link(
        &mut self,
        dir: NodeId,
        name: &[u8],
        target: &[u8],
        record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        self.inner.create_link(dir, name, target, record)
    }

    fn link(&mut self, dir: NodeId, name: &[u8], node: NodeId) -> Result<(), DriverError> {
        self.inner.link(dir, name, node)
    }

    fn write_at(&mut self, file: NodeId, offset: u64, data: &[u8]) -> Result<usize, DriverError> {
        self.inner.write_at(file, offset, data)
    }

    fn truncate(&mut self, file: NodeId, size: u64) -> Result<(), DriverError> {
        self.inner.truncate(file, size)
    }

    fn remove(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError> {
        self.inner.remove(dir, name)
    }

    fn rename(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        self.inner.rename(src_dir, src_name, dst_dir, dst_name)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.flushes += 1;
        if self.refuse {
            return Err(DriverError::DeviceFault);
        }
        self.inner.flush()
    }

    fn set_volume_host(&mut self, volume: DriverHandle, _host: &'static dyn VolumeHost) {
        self.host_volume = Some(volume);
    }
}

impl FilesystemSecurity for FlushSpy {
    fn security(&mut self, node: NodeId) -> Result<NodeSecurity, DriverError> {
        self.inner.security(node)
    }

    fn set_security(&mut self, node: NodeId, security: NodeSecurity) -> Result<(), DriverError> {
        self.inner.set_security(node, security)
    }
}

impl FilesystemStats for FlushSpy {
    fn stats(&mut self) -> Result<VolumeStats, DriverError> {
        self.inner.stats()
    }
}

impl FilesystemAttrsProvider for FlushSpy {}
