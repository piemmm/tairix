//! Host unit tests for the `/System` `fs_*` mount wiring (`system_mount`).
//!
//! These cover the piece that is testable without a live disk or the global
//! boot statics: the production VFS layout the `/System` volume is mounted
//! under ([`system_vfs`]). The `Box<dyn KernelFs>` forwarding impls are
//! covered by the shared wrapper conformance suite (`crate::kernel_fs`),
//! and the live `install_system_mount` path (a second `'static` window onto
//! the boot disk) is exercised by the FS QEMU vertical.

use tairix_kernel_core::Path;

use super::system_vfs;

#[test]
fn system_vfs_mounts_the_writable_volume_as_root() {
    // The writable root volume *is* `/`, and `/Users` is a plain directory
    // of it rather than a second mount of the same volume, so a rename
    // between `/` and `/Users` stays on one mount.
    let vfs = system_vfs().expect("the production VFS builds");
    let mounts = vfs.mounts();
    let root = mounts.resolve(&Path::parse("/").expect("valid"));
    assert_eq!(root.path(), &Path::parse("/").expect("valid"));
    assert!(!root.is_read_only(), "/ is writable");
    assert!(
        root.backing().is_some(),
        "/ is driver-backed by the writable root volume"
    );
    assert!(
        root.backing_subtree().is_empty(),
        "/ is a whole-volume mount (no rebasing)"
    );
    let home = mounts.resolve(&Path::parse("/Users/alice/file").expect("valid"));
    assert_eq!(home.path(), &Path::root(), "/Users is a directory of /");
}

/// `/Storage` is the catalog of attached volumes: unbacked, so nothing is
/// stored in it, and read-only, so nothing can be.
#[test]
fn system_vfs_leaves_the_storage_catalog_unbacked() {
    let vfs = system_vfs().expect("the production VFS builds");
    let mounts = vfs.mounts();
    let catalog = mounts.resolve(&Path::parse("/Storage/stick/file").expect("valid"));
    assert_eq!(catalog.path(), &Path::parse("/Storage").expect("valid"));
    assert_eq!(catalog.backing(), None);
    assert!(catalog.is_read_only());
    assert!(
        mounts
            .resolve(&Path::parse("/Apps/x").expect("valid"))
            .path()
            .is_root(),
        "there is no /Apps mount"
    );
}

#[test]
fn system_vfs_shadows_root_with_the_read_only_system_volume() {
    // The read-only `/System` volume is mounted *over* the writable root at
    // `/System`: a path under `/System` resolves to a read-only,
    // driver-backed mount on a *different* volume from `/`, so reads delegate
    // to the immutable volume and writes are refused.
    let vfs = system_vfs().expect("the production VFS builds");
    let root_handle = vfs
        .mounts()
        .resolve(&Path::parse("/").expect("valid"))
        .backing()
        .expect("/ is driver-backed");

    let under_system = Path::parse("/System/Drivers/x").expect("valid path");
    let mounts = vfs.mounts();
    let mount = mounts.resolve(&under_system);
    assert_eq!(mount.path(), &Path::parse("/System").expect("valid"));
    assert!(mount.is_read_only(), "/System is mounted read-only");
    let system_handle = mount
        .backing()
        .expect("/System is driver-backed so the VFS delegates to the live volume");
    assert!(
        mount.backing_subtree().is_empty(),
        "/System is a whole-volume mount (its content is the volume root)"
    );
    assert_ne!(
        system_handle, root_handle,
        "/System is a different volume from the writable root"
    );
}

#[test]
fn system_vfs_carves_logs_and_settings_back_to_the_writable_volume() {
    use tairix_abi::driver::filesystem::MountFlags;

    // `/System/Logs` and `/System/Settings` are the only writable paths
    // beneath `/System`: each is a `noexec` writable sub-mount of
    // the *writable root volume* (the same handle that backs `/`), rebased
    // onto that volume's own `/System/<name>` directory. `MountTable`
    // longest-prefix resolution makes the writable child shadow the read-only
    // `/System`.
    let vfs = system_vfs().expect("the production VFS builds");
    let root_handle = vfs
        .mounts()
        .resolve(&Path::parse("/").expect("valid"))
        .backing()
        .expect("/ is driver-backed");
    let system_handle = vfs
        .mounts()
        .resolve(&Path::parse("/System/Drivers/x").expect("valid"))
        .backing()
        .expect("/System is driver-backed");

    let mounts = vfs.mounts();
    for name in ["Logs", "Settings"] {
        let under = Path::parse(&alloc::format!("/System/{name}/file")).expect("valid path");
        let mount = mounts.resolve(&under);
        assert_eq!(
            mount.path(),
            &Path::parse(&alloc::format!("/System/{name}")).expect("valid"),
            "the writable {name} sub-mount shadows the read-only /System"
        );
        assert!(!mount.is_read_only(), "/System/{name} is writable");
        assert_eq!(
            mount.flags(),
            MountFlags::NOEXEC,
            "/System/{name} is mounted noexec"
        );
        let handle = mount
            .backing()
            .expect("the writable sub-mount is driver-backed");
        assert_eq!(
            handle, root_handle,
            "the writable {name} subtree is the one writable root volume"
        );
        assert_ne!(
            handle, system_handle,
            "the writable backing is a different volume from read-only /System"
        );
        // Rebased onto the backing volume's own `/System/<name>` directory.
        assert_eq!(
            mount.backing_subtree(),
            &[
                alloc::string::String::from("System"),
                alloc::string::String::from(name),
            ],
            "/System/{name} is rebased onto the volume's own /System/{name}"
        );
    }
}

/// The default install is two volumes and five projections: the root volume
/// at `/` and carved out at the two writable `/System` subtrees, the system
/// volume at `/System`, and the catalog showing no volume. Each volume is
/// attached once, with the flags its every projection keeps.
#[test]
fn system_vfs_attaches_two_volumes_under_five_projections() {
    use tairix_abi::driver::filesystem::MountFlags;

    let vfs = system_vfs().expect("the production VFS builds");
    let mounts = vfs.mounts();
    let volumes = mounts.volumes().expect("volumes");
    let shown: alloc::vec::Vec<_> = volumes
        .iter()
        .map(|(volume, at)| {
            (
                volume.flags(),
                at.as_ref().map(|at| at.owned().expect("held")),
            )
        })
        .collect();
    let path = |text: &str| Path::parse(text).expect("valid");
    assert_eq!(
        shown,
        [
            (MountFlags::default(), Some(path("/"))),
            (MountFlags::READ_ONLY, Some(path("/System"))),
        ]
    );
    let projections: alloc::vec::Vec<_> = mounts
        .iter()
        .map(|mount| {
            (
                mount.path().owned().expect("held"),
                mount.flags(),
                mount.backing().is_some(),
            )
        })
        .collect();
    assert_eq!(
        projections,
        [
            (path("/"), MountFlags::default(), true),
            (path("/System"), MountFlags::READ_ONLY, true),
            (path("/System/Logs"), MountFlags::NOEXEC, true),
            (path("/System/Settings"), MountFlags::NOEXEC, true),
            (
                path("/Storage"),
                MountFlags::READ_ONLY.union(MountFlags::NOEXEC),
                false
            ),
        ]
    );
}

/// The root volume's reserve over the system's own write set, at the sizes
/// `docs/src/filesystem/arxfs-spec.md` states for 512-byte blocks.
#[test]
fn the_root_reserve_over_the_system_write_set_matches_its_stated_sizes() {
    use tairix_drv_fs_arxfs::system_reserve_blocks;

    use super::SYSTEM_WRITE_SET;

    const MIB: u64 = 1 << 20;
    let reserve = |volume: u64| system_reserve_blocks(512, volume / 512, &SYSTEM_WRITE_SET) * 512;
    assert_eq!(reserve(MIB), 128 * 1024);
    assert_eq!(reserve(64 * MIB), 8 * MIB);
    assert_eq!(reserve(512 * MIB), 16 * MIB);
    assert_eq!(reserve(4 << 30), 128 * MIB);
    assert_eq!(reserve(100 << 40), 128 * MIB);
}

/// The two writable grafts take the system's own writes; every other
/// projection's writes are ordinary.
#[test]
fn the_system_grafts_write_under_the_system_class() {
    use tairix_abi::driver::filesystem::AllocationClass;

    let vfs = system_vfs().expect("the production VFS builds");
    let mounts = vfs.mounts();
    let class = |path: &str| mounts.resolve(&Path::parse(path).expect("valid")).class();
    assert_eq!(class("/System/Logs/x"), AllocationClass::System);
    assert_eq!(class("/System/Settings/x"), AllocationClass::System);
    assert_eq!(class("/Users/ada/x"), AllocationClass::Ordinary);
    assert_eq!(class("/x"), AllocationClass::Ordinary);
}

/// Every boot service is loaded from `/System` before the root volume is
/// unlocked, while only the `/System` driver is registered: the production
/// layout serves its bundles then, and fails closed for the root's own paths.
#[test]
fn a_service_bundle_is_read_from_system_before_the_root_is_served() {
    use alloc::boxed::Box;
    use tairix_abi::driver::filesystem::NodeSecurity;
    use tairix_abi::driver::filesystem::{FilesystemRead as _, FilesystemWrite as _, NodeKind};
    use tairix_abi::driver::DriverHandle;
    use tairix_abi::Errno;
    use tairix_caps::CapabilitySet;
    use tairix_kernel_core::fs::memfs::RwMockFs;
    use tairix_kernel_core::fs::{At, FinalLink, Start};
    use tairix_kernel_core::MountedFilesystemService;
    use tairix_kernel_core::{FilesystemService as _, LateFilesystem, LateIdentity};
    use tairix_kernel_sec::{GroupId, GroupRecord, IdentityTableBuilder, UserId, UserRecord};

    use super::SYSTEM_MOUNT_HANDLE;
    use crate::kernel_fs::KernelFs;
    use crate::test_support::NullSink;

    const SERVICE: u32 = 600;
    let shipped = NodeSecurity::new(0o755, 0, 0);
    let mut system = RwMockFs::new();
    let mut dir = system.root();
    for name in [&b"Services"[..], b"svc.app"] {
        dir = system
            .create(dir, name, NodeKind::Directory, &shipped)
            .expect("directory");
    }
    let run = system
        .create(dir, b"Run", NodeKind::RegularFile, &shipped)
        .expect("Run");
    system.write_at(run, 0, b"R").expect("contents");

    let cell: &'static LateFilesystem<Box<dyn KernelFs>> =
        Box::leak(Box::new(LateFilesystem::new()));
    cell.install_vfs(system_vfs().expect("the production VFS builds"))
        .expect("install");
    let handle = DriverHandle::from_raw(SYSTEM_MOUNT_HANDLE).expect("handle");
    cell.register(
        handle,
        Box::new(system),
        "ARXFSSystem",
        "arxfs",
        [1; 16],
        None,
    )
    .expect("register /System");
    let mut identity = IdentityTableBuilder::new();
    identity.push_group(GroupRecord {
        gid: GroupId(SERVICE),
    });
    identity.push_user(UserRecord {
        uid: UserId(SERVICE),
        primary_gid: GroupId(SERVICE),
        supplementary_gids: alloc::vec::Vec::new(),
        capability_grants: CapabilitySet::empty(),
    });
    let identities: &'static LateIdentity = Box::leak(Box::new(LateIdentity::new()));
    identities
        .install(identity.verify(&NullSink).expect("identity table"))
        .expect("install identity");
    let fs = MountedFilesystemService::new(cell, identities);
    let caps = CapabilitySet::empty();
    let at = |path| At {
        start: Start::Root,
        path,
    };

    let listed = fs
        .readdir_bounded(
            SERVICE,
            &caps,
            at("/System/Services/svc.app"),
            FinalLink::Follow,
            8,
        )
        .expect("the bundle lists");
    assert_eq!(listed.len(), 1);
    let mut buf = [0u8; 1];
    assert_eq!(
        fs.read_program(
            SERVICE,
            &caps,
            at("/System/Services/svc.app/Run"),
            0,
            &mut buf
        ),
        Ok(1)
    );
    for path in ["/", "/Users", "/System/Logs", "/System/Settings"] {
        assert_eq!(
            fs.readdir_bounded(SERVICE, &caps, at(path), FinalLink::Follow, 8)
                .map(|_| ()),
            Err(Errno::NotImplemented),
            "{path}"
        );
    }
}

/// A volume cache the heap cannot hold is refused, and the claim on its
/// change table let go with it, rather than the kernel aborting.
#[test]
fn a_volume_cache_the_heap_cannot_hold_is_refused() {
    use alloc::boxed::Box;
    use tairix_kernel_core::fs::memfs::RwMockFs;
    use tairix_kernel_core::fs::CachedFs;

    use crate::test_support::NullSink;

    struct Ample;
    impl tairix_reclaim::FreeMemorySource for Ample {
        fn free_bytes(&self) -> usize {
            1 << 30
        }
        fn total_bytes(&self) -> usize {
            1 << 30
        }
    }
    static AMPLE: Ample = Ample;
    let pressure: &'static tairix_reclaim::MemoryPressure =
        Box::leak(Box::new(tairix_reclaim::MemoryPressure::over(&AMPLE)));
    let watches: &'static tairix_kernel_core::fswatch::WatchRegistry =
        Box::leak(Box::new(tairix_kernel_core::fswatch::WatchRegistry::new()));
    let identity = [0x3C; 16];
    let whole = core::mem::size_of::<CachedFs<RwMockFs>>();
    let built = tairix_fuzzseed::meter::refusing_above(whole - 1, || {
        super::cached(
            RwMockFs::new(),
            1,
            identity,
            pressure,
            &NullSink,
            Some(watches),
        )
        .map(drop)
    });
    assert_eq!(built, Err(tairix_abi::Errno::OutOfMemory));
    assert!(
        watches
            .claim(
                identity,
                tairix_abi::driver::filesystem::NameMatching::Exact,
                pressure
            )
            .is_some(),
        "the refused cache's change table is free again"
    );
    assert!(super::cached(RwMockFs::new(), 1, [0x3D; 16], pressure, &NullSink, None).is_ok());
}
