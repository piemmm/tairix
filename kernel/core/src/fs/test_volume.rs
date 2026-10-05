//! Host-test fixture: an in-memory volume mounted at [`MOUNT`] behind the
//! secured service, shared by the service, change-notification and syscall
//! suites so none of them carries its own copy.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::blkio::BlkDeviceClass;
use tairix_abi::driver::filesystem::{FilesystemRead, MountFlags, NodeSecurity};
use tairix_abi::driver::DriverHandle;
use tairix_caps::CapabilitySet;
use tairix_kernel_sec::{
    GroupId, GroupRecord, IdentityTable, IdentityTableBuilder, UserId, UserRecord,
};
use tairix_log::{Event, Sink};
use tairix_reclaim::{CacheBudget, ReclaimOwner};

use super::memfs::RwMockFs;
use super::perm::Credentials;
use super::{
    CachedFs, LateFilesystem, LateIdentity, Mode, MountBacking, MountedFilesystemService, Path, Vfs,
};
use crate::fswatch::{VolumeWatch, WatchRegistry};
use crate::test_pressure::unpressured;
use crate::test_sink::TestSink;

pub(crate) const TEST_UID: u32 = 1000;
pub(crate) const TEST_GID: u32 = 1000;
/// The mount point the in-memory driver is mounted at.
pub(crate) const MOUNT: &str = "/Storage/vol";
/// The storage medium the fixture's block device declares, so the snapshot
/// can be checked against a value only the attach path could have supplied.
pub(crate) const MOUNT_MEDIUM: BlkDeviceClass = BlkDeviceClass::SolidState;

/// A sink that discards every event — the identity-table verifier audits its
/// outcome but these tests assert behaviour, not the audit trail.
pub(crate) struct NullSink;
impl Sink for NullSink {
    fn write_event(&self, _event: &Event<'_>) {}
}

pub(crate) fn caps() -> CapabilitySet {
    CapabilitySet::empty()
}

/// An identity table holding exactly the test principal (uid/gid 1000).
pub(crate) fn identity_table() -> IdentityTable {
    let mut builder = IdentityTableBuilder::new();
    builder.push_group(GroupRecord {
        gid: GroupId(TEST_GID),
    });
    builder.push_user(UserRecord {
        uid: UserId(TEST_UID),
        primary_gid: GroupId(TEST_GID),
        supplementary_gids: Vec::new(),
        capability_grants: CapabilitySet::empty(),
    });
    builder
        .verify(&NullSink)
        .expect("well-formed identity table")
}

/// A default-layout VFS with the in-memory driver mounted at [`MOUNT`],
/// read-only when `read_only`.
pub(crate) fn vfs(read_only: bool) -> Vfs {
    let mut vfs = Vfs::with_default_layout(UserId(TEST_UID), GroupId(TEST_GID));
    let caps = caps();
    let cred = Credentials {
        uid: UserId(TEST_UID),
        gid: GroupId(TEST_GID),
        supplementary_gids: &[],
        caps: &caps,
    };
    let mount = Path::parse(MOUNT).expect("valid mount path");
    vfs.mkdir(&cred, &mount, Mode::from_bits(0o755))
        .expect("create mount point");
    let handle = DriverHandle::from_raw(9).expect("non-zero handle");
    let flags = if read_only {
        MountFlags::READ_ONLY
    } else {
        MountFlags::from_bits(0).expect("empty flags")
    };
    // The fixture stands in for a volume attached from a classified block
    // device, so the medium it reports is the one the device declared.
    vfs.mounts_write()
        .mount(
            mount,
            flags,
            Some(MountBacking::new(handle, Some(MOUNT_MEDIUM))),
        )
        .expect("mount backed");
    vfs
}

/// The in-memory driver, with its root and created files owned by the test
/// principal so it can traverse, create, and write.
pub(crate) fn driver() -> RwMockFs {
    let mut fs = RwMockFs::new().with_create_owner(TEST_UID, TEST_GID, 0o644);
    fs.set_root_security(NodeSecurity::new(0o755, TEST_UID, TEST_GID));
    fs
}

/// A driver owned by the test principal whose created nodes are
/// world-traversable directories (mode `0o755`), so a rebased mount can walk
/// the backing-subtree directories it pre-creates.
pub(crate) fn dir_driver() -> RwMockFs {
    let mut fs = RwMockFs::new().with_create_owner(TEST_UID, TEST_GID, 0o755);
    fs.set_root_security(NodeSecurity::new(0o755, TEST_UID, TEST_GID));
    fs
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
    let fs = dir_driver().with_exact_names();
    let registry: &'static WatchRegistry = Box::leak(Box::new(WatchRegistry::new()));
    let claim = registry
        .claim(volume, fs.name_matching(), unpressured())
        .expect("claims");
    let watch = Arc::clone(claim.table());
    let reference = claim.reference();
    let sink: &'static TestSink = Box::leak(Box::new(TestSink::new()));
    let cached = CachedFs::new(
        fs,
        CacheBudget::from_backing(16 << 20),
        ReclaimOwner::FilesystemVolume { volume: 9 },
        unpressured(),
        sink,
    )
    .with_watch(claim);
    let cell: &'static LateFilesystem<CachedFs<RwMockFs>> =
        Box::leak(Box::new(LateFilesystem::new()));
    cell.install_vfs(vfs(false)).expect("install vfs");
    cell.register(
        DriverHandle::from_raw(9).expect("handle"),
        cached,
        "vol",
        "memfs",
        volume,
        Some(reference),
    )
    .expect("register");
    let identity: &'static LateIdentity = Box::leak(Box::new(LateIdentity::new()));
    identity.install(identity_table()).expect("identity");
    let service: &'static MountedFilesystemService<CachedFs<RwMockFs>> =
        Box::leak(Box::new(MountedFilesystemService::new(cell, identity)));
    Watched {
        service,
        watch,
        registry,
        mounted: cell,
    }
}
