//! Behavioural tests for [`MountedFilesystemService`]: the production
//! `fs_*` producer routing each operation through the secured VFS under a
//! late-installed mount and identity table.
//!
//! The shared in-memory driver fixture (`crate::fs::memfs::RwMockFs`) stands
//! in for a block-backed `drivers/filesystem/*` driver; these tests exercise
//! the *service* layer — fail-closed gating, kernel-attested identity and
//! group resolution, and the `open`/append/`readdir` semantics — not the VFS
//! internals (covered by `delegate_tests`). True lock contention across a
//! task park is exercised by the `SleepLock` tests and the FS QEMU vertical;
//! a host test has no installed scheduler to park on.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::AtomicU8;
use tairix_abi::blkio::{BlkMedium, BlkRotation, BlkRotationEvidence, BlkTransport};

use tairix_abi::blkio::{BlkDeviceClass, BlkIoCounters, BlkQueueCounters};
use tairix_abi::driver::filesystem::{
    DirVisit, FilesystemAttrs as _, FilesystemRead, FilesystemWrite, MountFlags, NodeKind,
    NodeSecurity,
};
use tairix_abi::driver::DriverHandle;
use tairix_abi::sysinfo::MountAvailability;
use tairix_abi::{
    CapabilityId, Errno, FileKind, OpenFlags, RealpathMode, RenameFlags, SeekRegion, UnlinkFlags,
    FS_OWNER_UNCHANGED,
};
use tairix_caps::CapabilitySet;
use tairix_kernel_sec::{GroupId, GroupRecord, IdentityTableBuilder, UserId, UserRecord};

use super::{LateFilesystem, LateIdentity, MountedFilesystemService};
use crate::fs::blkmeter::{BlkHealthCountersAtomic, BlkIoStatsAtomic, VolumeIoSource};
use crate::fs::memfs::{RwMockFs, SYSTEM_ONLY_NAME, SYSTEM_RECORD};
use crate::fs::perm::Credentials;
use crate::fs::service::FilesystemService;
use crate::fs::test_volume::{
    acting, at, attr_get_path, attr_list_path, attr_remove_path, attr_set_path, caps, close,
    driver, identity_table, list_path, mounted, new_dir, new_file, open_path, own_handle, path,
    punch_path, read_path, seek_region_path, sync_path, truncate_path, vfs, write_path, Account,
    FlushSpy, NullSink, MOUNT, MOUNT_MEDIUM, PLANTED, TEST_ACCOUNT, TEST_GID, TEST_UID,
};
use crate::fs::{
    Attachment, Authority, CreateMode, Cwd, FinalLink, Listing, Mode, NodeHandle, Opened, Path,
    Vfs, VfsError,
};

/// A removable volume served by `driver`, attached with the flags every
/// runtime volume carries.
fn removable(driver: DriverHandle) -> Attachment {
    let stick = BlkMedium::of_transport(BlkTransport::Usb).with_rotation(BlkRotation::NonRotating(
        BlkRotationEvidence::RotationRateVpd,
    ));
    Attachment::new(driver, Some(stick), MountFlags::NOEXEC)
}

/// Back the view root with the volume `root` serves and show each of
/// `children` beneath its floor, as `/System` is shown beneath `/`, so a
/// listing merges those projections with the names the volume holds.
fn root_with_children(vfs: &Vfs, root: DriverHandle, children: &[(DriverHandle, &str)]) {
    let mut mounts = vfs.mounts_write();
    mounts
        .attach(Attachment::new(root, None, MountFlags::default()))
        .expect("attach the root volume");
    mounts.back_root(root).expect("back /");
    for &(child, point) in children {
        mounts
            .attach(Attachment::new(child, None, MountFlags::default()))
            .expect("attach");
        mounts
            .mount(
                Path::parse(point).expect("path"),
                MountFlags::default(),
                Some(child),
            )
            .expect("shown beneath /");
    }
}

/// Project `/Users` on its own, so mounts beneath it sit at a projection's
/// floor.
fn users_projection(vfs: &Vfs) {
    vfs.mounts_write()
        .mount(
            Path::parse("/Users").expect("path"),
            MountFlags::default(),
            None,
        )
        .expect("projects /Users");
}

/// Two rebased sub-mounts that share **one** backing driver (the
/// `/System/Logs` + `/System/Settings`-on-one-volume shape) route each to
/// their own backing subtree and stay isolated; a second driver under a
/// different handle is fully independent.
#[test]
fn rebased_submounts_route_to_their_backing_subtree_and_handle() {
    let noexec = MountFlags::NOEXEC;
    let h_shared = DriverHandle::from_raw(9).expect("handle");
    let h_other = DriverHandle::from_raw(10).expect("handle");

    // One driver carrying two backing subtrees, plus an independent second
    // driver. Both are owned by the test principal.
    let mut shared = driver();
    let sroot = shared.root();
    shared
        .create(sroot, b"LogsArea", NodeKind::Directory, &PLANTED)
        .expect("logs subtree");
    shared
        .create(sroot, b"SettingsArea", NodeKind::Directory, &PLANTED)
        .expect("settings subtree");
    let other = driver();

    // The VFS: two rebased mounts on the shared driver and one plain mount on
    // the other, with their in-RAM mount-point directories created so the
    // delegated walk has a permission template.
    let mut vfs = Vfs::with_default_layout(UserId(TEST_UID), GroupId(TEST_GID));
    let caps = caps();
    let cred = Credentials {
        uid: UserId(TEST_UID),
        gid: GroupId(TEST_GID),
        supplementary_gids: &[],
        caps: &caps,
    };
    for mp in ["/Users/logs", "/Users/settings", "/Users/other"] {
        vfs.mkdir(
            &cred,
            &Path::parse(mp).expect("path"),
            Mode::from_bits(0o755),
        )
        .expect("mount point");
    }
    for driver in [h_shared, h_other] {
        vfs.mounts_write()
            .attach(Attachment::new(driver, None, MountFlags::default()))
            .expect("attach");
    }
    users_projection(&vfs);
    for (mount_point, subtree) in [
        ("/Users/logs", "LogsArea"),
        ("/Users/settings", "SettingsArea"),
    ] {
        vfs.mounts_write()
            .mount_rebased(
                Path::parse(mount_point).expect("path"),
                noexec,
                Some(h_shared),
                alloc::vec![alloc::string::String::from(subtree)],
            )
            .expect("rebased mount");
    }
    vfs.mounts_write()
        .mount(
            Path::parse("/Users/other").expect("path"),
            noexec,
            Some(h_other),
        )
        .expect("other mount");

    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    cell.install_vfs(vfs).expect("install vfs");
    cell.register(h_shared, shared, "shared", "memfs", [0u8; 16], None)
        .expect("register shared");
    cell.register(h_other, other, "other", "memfs", [0u8; 16], None)
        .expect("register other");
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    identity.install(identity_table()).expect("identity");
    let svc = MountedFilesystemService::new(cell, identity);

    let create = OpenFlags::CREATE.union(OpenFlags::WRITE);
    open_path(&svc, TEST_UID, &caps, "/Users/logs/a", create).expect("create under logs");
    write_path(&svc, TEST_UID, &caps, "/Users/logs/a", 0, false, b"L").expect("write logs");
    open_path(&svc, TEST_UID, &caps, "/Users/settings/b", create).expect("create under settings");
    write_path(&svc, TEST_UID, &caps, "/Users/settings/b", 0, false, b"S").expect("write settings");
    open_path(&svc, TEST_UID, &caps, "/Users/other/c", create).expect("create under other");
    write_path(&svc, TEST_UID, &caps, "/Users/other/c", 0, false, b"O").expect("write other");

    // Each file reads back through its own mount.
    let read = |path| {
        let mut buf = [0u8; 1];
        read_path(&svc, TEST_UID, &caps, path, 0, &mut buf).map(|_| buf[0])
    };
    assert_eq!(read("/Users/logs/a"), Ok(b'L'));
    assert_eq!(read("/Users/settings/b"), Ok(b'S'));
    assert_eq!(read("/Users/other/c"), Ok(b'O'));

    // The two rebased mounts are isolated: `a` exists only under the logs
    // subtree, `b` only under settings.
    assert_eq!(read("/Users/settings/a"), Err(Errno::NotFound));
    assert_eq!(read("/Users/logs/b"), Err(Errno::NotFound));
    // The second driver is independent: its file is not visible on the
    // shared driver's subtrees.
    assert_eq!(read("/Users/logs/c"), Err(Errno::NotFound));
}

/// Listing a driver-backed floor merges the mount points of its direct-child
/// mounts, as `/` merges `/System`: a mount with no node of its name on the
/// parent volume appears as a structural directory entry, a name the parent
/// volume already holds is not repeated, and the merged name resolves
/// through to the mounted volume's own content.
#[test]
fn readdir_merges_direct_child_mounts_into_the_parent_listing() {
    use tairix_abi::time::Time64;

    let h_parent = DriverHandle::from_raw(9).expect("handle");
    let h_usb = DriverHandle::from_raw(10).expect("handle");
    let h_dup = DriverHandle::from_raw(11).expect("handle");

    // The parent volume holds one plain directory and one whose name
    // collides with a child mount point.
    let mut parent = driver();
    let proot = parent.root();
    parent
        .create(proot, b"existing", NodeKind::Directory, &PLANTED)
        .expect("existing dir");
    parent
        .create(proot, b"dup", NodeKind::Directory, &PLANTED)
        .expect("dup dir");

    // The hotplug volume carries one file, so resolution through the
    // merged name is provable end to end.
    let mut usb = driver();
    let uroot = usb.root();
    usb.create(uroot, b"note", NodeKind::RegularFile, &PLANTED)
        .expect("note file");

    let vfs = Vfs::with_default_layout(UserId(TEST_UID), GroupId(TEST_GID));
    root_with_children(&vfs, h_parent, &[(h_usb, "/usb1"), (h_dup, "/dup")]);

    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    cell.install_vfs(vfs).expect("install vfs");
    cell.register(h_parent, parent, "root", "memfs", [0u8; 16], None)
        .expect("register parent");
    cell.register(h_usb, usb, "usb1", "memfs", [0u8; 16], None)
        .expect("register usb");
    cell.register(h_dup, driver(), "dup", "memfs", [0u8; 16], None)
        .expect("register dup");
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    identity.install(identity_table()).expect("identity");
    let svc = MountedFilesystemService::new(cell, identity);
    let caps = caps();

    let entries = svc
        .readdir_bounded(TEST_UID, &caps, at("/"), FinalLink::Follow, usize::MAX)
        .expect("listing succeeds");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names.contains(&"existing"),
        "parent content listed: {names:?}"
    );
    assert!(names.contains(&"usb1"), "child mount merged: {names:?}");
    // The colliding name appears exactly once (the parent volume's own
    // node; the mount is not repeated behind it).
    assert_eq!(
        names.iter().filter(|n| **n == "dup").count(),
        1,
        "colliding mount name deduplicated: {names:?}"
    );
    let usb1 = entries
        .iter()
        .find(|e| e.name == "usb1")
        .expect("merged entry");
    assert_eq!(usb1.kind, FileKind::Directory);
    assert_eq!((usb1.size, usb1.allocated), (0, 0));
    assert_eq!(usb1.modified, Time64::UNIX_EPOCH);
    // The parent volume holds no node of that name, so it offers no identity
    // or name count for it — a placeholder, never a key a walk may compare.
    assert_eq!(usb1.id, tairix_abi::FileId::NONE);
    assert_eq!(usb1.nlink, NodeInfo::SINGLE_NAME);

    // Read one entry per batch through one listing, the merge resumes in
    // both its parts and lists exactly what one whole read does.
    assert_eq!(names_a_batch_at_a_time(&svc, "/"), names);

    // The merged name is not a phantom: it resolves through to the mounted
    // volume's own content.
    let inside = svc
        .readdir_bounded(TEST_UID, &caps, at("/usb1"), FinalLink::Follow, usize::MAX)
        .expect("child volume lists");
    assert!(inside.iter().any(|e| e.name == "note"), "{inside:?}");
}

/// The production boot shape before the root is unlocked, over a `vfs` that
/// projects `/System` and `/System/Logs`: `/` and `/System/Logs` backed by a
/// root volume no driver serves yet, `/System` by a served one holding
/// `Services/Run`. Answers the cell, the root handle, and the service.
fn before_the_root_is_served(
    vfs: Vfs,
) -> (
    &'static LateFilesystem<RwMockFs>,
    DriverHandle,
    MountedFilesystemService<RwMockFs>,
) {
    let h_root = DriverHandle::from_raw(9).expect("handle");
    let h_system = DriverHandle::from_raw(10).expect("handle");
    let mut system = driver();
    let sroot = system.root();
    let services = system
        .create(sroot, b"Services", NodeKind::Directory, &PLANTED)
        .expect("Services");
    let run = system
        .create(services, b"Run", NodeKind::RegularFile, &PLANTED)
        .expect("Run");
    system.write_at(run, 0, b"R").expect("contents");
    {
        let mut mounts = vfs.mounts_write();
        mounts
            .attach(Attachment::new(h_root, None, MountFlags::default()))
            .expect("attach the root volume");
        mounts
            .attach(Attachment::new(h_system, None, MountFlags::READ_ONLY))
            .expect("attach /System");
        mounts.back_root(h_root).expect("back /");
        mounts
            .set_backing(&Path::parse("/System").expect("path"), h_system, Vec::new())
            .expect("back /System");
        let logs = Path::parse("/System/Logs").expect("path");
        let subtree = logs.components().to_vec();
        mounts
            .set_backing(&logs, h_root, subtree)
            .expect("back /System/Logs");
    }
    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    cell.install_vfs(vfs).expect("install vfs");
    cell.register(h_system, system, "system", "memfs", [0u8; 16], None)
        .expect("register /System");
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    identity.install(identity_table()).expect("identity");
    (cell, h_root, MountedFilesystemService::new(cell, identity))
}

/// Before the volume backing `/` is served, as before the root is unlocked,
/// the projections shown beneath its floor are reached through it by their
/// own names, and nothing the unserved volume holds is: every such walk fails
/// closed as one standing on it does.
#[test]
fn the_projections_beneath_an_unserved_floor_are_reached_through_it() {
    let (cell, h_root, svc) = before_the_root_is_served(Vfs::with_default_layout(
        UserId(TEST_UID),
        GroupId(TEST_GID),
    ));
    let caps = caps();
    let read = |path| {
        let mut buf = [0u8; 1];
        read_path(&svc, TEST_UID, &caps, path, 0, &mut buf).map(|_| buf[0])
    };
    assert_eq!(read("/System/Services/Run"), Ok(b'R'));
    assert_eq!(read("System/Services/Run"), Ok(b'R'), "from the root start");
    assert_eq!(read("/System/../System/Services/Run"), Ok(b'R'));
    assert_eq!(read("/../System/Services/Run"), Ok(b'R'));
    let listed = svc
        .readdir_bounded(
            TEST_UID,
            &caps,
            at("/System/Services"),
            FinalLink::Follow,
            usize::MAX,
        )
        .expect("lists");
    assert!(listed.iter().any(|e| e.name == "Run"), "{listed:?}");

    let dir = OpenFlags::READ.union(OpenFlags::DIRECTORY);
    let create = OpenFlags::CREATE.union(OpenFlags::WRITE);
    for (path, flags) in [
        ("/", dir),
        ("/Users", dir),
        ("/System/..", dir),
        ("/system/Services/Run", OpenFlags::READ),
        ("/System/Logs/x", OpenFlags::READ),
        ("/System/Logs/x", create),
        ("/x", create),
    ] {
        assert_eq!(
            open_path(&svc, TEST_UID, &caps, path, flags),
            Err(Errno::NotImplemented),
            "{path}"
        );
    }

    let mut volume = driver();
    let floor = volume.root();
    volume
        .create(floor, b"Users", NodeKind::Directory, &PLANTED)
        .expect("Users");
    cell.register(h_root, volume, "root", "memfs", [0u8; 16], None)
        .expect("register /");
    assert_eq!(open_path(&svc, TEST_UID, &caps, "/Users", dir), Ok(()));
    assert_eq!(read("/System/Services/Run"), Ok(b'R'));
}

/// What stands for an unserved floor is the boot layout's record of it, so a
/// caller it denies search reaches nothing beneath; once the volume is served
/// its own record governs instead.
#[test]
fn an_unserved_floor_is_searched_under_its_layout_record() {
    let vfs = Vfs::new(crate::fs::Metadata::new(
        UserId(0),
        GroupId(0),
        Mode::from_bits(0o700),
    ));
    for (point, flags) in [
        ("/System", MountFlags::READ_ONLY),
        ("/System/Logs", MountFlags::NOEXEC),
    ] {
        vfs.mounts_write()
            .mount(Path::parse(point).expect("path"), flags, None)
            .expect("projected");
    }
    let (cell, h_root, svc) = before_the_root_is_served(vfs);
    let caps = caps();
    let mut buf = [0u8; 1];
    assert_eq!(
        read_path(&svc, TEST_UID, &caps, "/System/Services/Run", 0, &mut buf),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        read_path(&svc, TEST_UID, &caps, "System/Services/Run", 0, &mut buf),
        Err(Errno::PermissionDenied),
        "the root start is searched too"
    );
    cell.register(h_root, driver(), "root", "memfs", [0u8; 16], None)
        .expect("register /");
    assert_eq!(
        read_path(&svc, TEST_UID, &caps, "/System/Services/Run", 0, &mut buf),
        Ok(1)
    );
}

/// Build a service over freshly leaked mount + identity cells. `mount` and
/// `identity` select whether each cell is installed, so a test can exercise
/// the fail-closed pre-install paths.
fn service(
    mount_installed: bool,
    identity_installed: bool,
    read_only: bool,
) -> MountedFilesystemService<RwMockFs> {
    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    if mount_installed {
        cell.install_vfs(vfs(read_only)).expect("install vfs");
        // The in-memory driver is mounted at [`MOUNT`] under handle 9 (see
        // `vfs`); register it under that same handle so the service routes
        // operations on `/Storage/vol/...` to it.
        let handle = DriverHandle::from_raw(9).expect("non-zero handle");
        cell.register(handle, driver(), "vol", "memfs", [0u8; 16], None)
            .expect("register driver");
    }
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    if identity_installed {
        identity
            .install(identity_table())
            .expect("install identity");
    }
    MountedFilesystemService::new(cell, identity)
}

/// A fully-installed writable service (the common case).
fn ready() -> MountedFilesystemService<RwMockFs> {
    service(true, true, false)
}

/// `/Storage` is the catalog of attached volumes, served with no volume
/// behind it: it stats and opens as a directory, lists exactly the volumes
/// attached beneath it, and refuses anything stored in it.
#[test]
fn the_storage_catalog_lists_its_volumes_and_stores_nothing() {
    let svc = ready();
    let caps = caps();
    let stat = svc
        .stat(TEST_UID, &caps, at("/Storage"), FinalLink::Follow)
        .expect("the catalog stats");
    assert_eq!(stat.kind, FileKind::Directory);
    assert_eq!((stat.uid, stat.gid, stat.mode), (TEST_UID, TEST_GID, 0o755));

    let mut names = Vec::new();
    list_path(
        &svc,
        TEST_UID,
        &caps,
        "/Storage",
        OpenFlags::DIRECTORY,
        &mut |entry| {
            names.push(alloc::string::String::from_utf8_lossy(entry.name).into_owned());
            DirVisit::Take
        },
    )
    .expect("the catalog lists");
    assert_eq!(names, ["vol"], "only the attached volume");
    let looked = svc
        .lookup_entries(
            TEST_UID,
            &caps,
            at("/Storage"),
            FinalLink::Follow,
            &[b"vol", b"absent"],
        )
        .expect("the catalog looks up");
    assert!(looked.entries[0].is_some() && looked.entries[1].is_none());

    assert_eq!(
        open_path(&svc, TEST_UID, &caps, "/Storage", OpenFlags::DIRECTORY),
        Ok(())
    );
    assert_eq!(
        open_path(&svc, TEST_UID, &caps, "/Storage", OpenFlags::READ),
        Err(Errno::IsADirectory)
    );
    assert_eq!(
        svc.realpath(TEST_UID, &caps, at("/Storage"), RealpathMode::Existing),
        Ok(alloc::string::String::from("/Storage"))
    );
    assert_eq!(
        svc.realpath(TEST_UID, &caps, at("/Storage/new"), RealpathMode::Final),
        Ok(alloc::string::String::from("/Storage/new"))
    );
    assert_eq!(
        svc.stat(TEST_UID, &caps, at("/Storage/absent"), FinalLink::Follow),
        Err(Errno::NotFound)
    );

    let read_only = VfsError::ReadOnly.to_errno();
    let create = OpenFlags::CREATE.union(OpenFlags::WRITE);
    assert_eq!(
        open_path(&svc, TEST_UID, &caps, "/Storage/new", create),
        Err(read_only)
    );
    assert_eq!(
        svc.mkdir(TEST_UID, &caps, at("/Storage/new"), new_dir()),
        Err(read_only)
    );
    assert_eq!(
        svc.unlink(
            TEST_UID,
            &caps,
            at("/Storage"),
            UnlinkFlags::DIRECTORY,
            None
        ),
        Err(read_only)
    );
    assert_eq!(
        svc.set_mode(TEST_UID, &caps, at("/Storage"), 0o777, FinalLink::Follow),
        Err(read_only)
    );
    assert_eq!(
        svc.mkdir(TEST_UID, &caps, at("/Storage/absent/deeper"), new_dir()),
        Err(Errno::NotFound),
        "a name below a vacant one is simply not there"
    );
}

#[test]
fn an_uninstalled_mount_fails_closed_not_implemented() {
    // Identity is present, but no volume is mounted: every op is refused
    // exactly like the hollow `NULL_FILESYSTEM`.
    let svc = service(false, true, false);
    let caps = caps();
    let mut buf = [0u8; 4];
    assert_eq!(
        read_path(&svc, TEST_UID, &caps, &path("x"), 0, &mut buf),
        Err(Errno::NotImplemented)
    );
    assert_eq!(
        svc.stat(TEST_UID, &caps, at(&path("x")), FinalLink::Follow),
        Err(Errno::NotImplemented)
    );
}

#[test]
fn an_uninstalled_identity_fails_closed_not_implemented() {
    // The volume is mounted, but the authoritative identity table has not
    // been installed yet (the disk is not unlocked): groups cannot be
    // resolved, so the op is refused rather than run with a guessed identity.
    let svc = service(true, false, false);
    let caps = caps();
    assert_eq!(
        svc.stat(TEST_UID, &caps, at(&path("x")), FinalLink::Follow),
        Err(Errno::NotImplemented)
    );
}

#[test]
fn an_unknown_principal_is_denied() {
    // A uid with no account is denied — never granted a guessed identity, and
    // the refusal does not distinguish "unknown uid".
    let svc = ready();
    let caps = caps();
    assert_eq!(
        svc.stat(4242, &caps, at(&path("x")), FinalLink::Follow),
        Err(Errno::PermissionDenied)
    );
}

#[test]
fn the_system_principal_resolves_before_the_identity_table_installs() {
    // The volume is mounted but the identity table is not installed (the
    // root's unlock has not finished): the kernel-defined system principal
    // (uid 0) resolves to the capability-less bootstrap identity, so its
    // operation reaches the volume and reports the path's real state
    // (`NotFound` here) instead of the pre-fix `NotImplemented` that killed
    // every pre-unlock store-bundle spawn.
    let svc = service(true, false, false);
    let caps = caps();
    assert_eq!(
        svc.stat(0, &caps, at(&path("x")), FinalLink::Follow),
        Err(Errno::NotFound)
    );
}

#[test]
fn an_installed_table_without_uid0_still_resolves_the_system_principal() {
    // An installed table that defines no uid 0 account (the installer-built
    // shape) does not strand the system principal: uid 0 falls back to the
    // bootstrap identity while every other unknown uid stays denied.
    let svc = ready();
    let caps = caps();
    assert_eq!(
        svc.stat(0, &caps, at(&path("x")), FinalLink::Follow),
        Err(Errno::NotFound)
    );
    assert_eq!(
        svc.stat(4242, &caps, at(&path("x")), FinalLink::Follow),
        Err(Errno::PermissionDenied)
    );
}

/// Groups are read from the table in force: a replacement that takes a
/// principal out of a group ends what the group granted at its next
/// operation, through its own open descriptor too, and one that puts it back
/// restores it, however often its groups were resolved before.
#[test]
fn a_replaced_identity_table_binds_a_principals_groups_at_its_next_operation() {
    const READER_UID: u32 = 1001;
    const READER_GID: u32 = 1001;
    let table = |member: bool| {
        let mut builder = IdentityTableBuilder::new();
        builder.push_group(GroupRecord {
            gid: GroupId(TEST_GID),
        });
        builder.push_group(GroupRecord {
            gid: GroupId(READER_GID),
        });
        builder.push_user(UserRecord {
            uid: UserId(TEST_UID),
            primary_gid: GroupId(TEST_GID),
            supplementary_gids: Vec::new(),
            capability_grants: CapabilitySet::empty(),
        });
        builder.push_user(UserRecord {
            uid: UserId(READER_UID),
            primary_gid: GroupId(READER_GID),
            supplementary_gids: if member {
                alloc::vec![GroupId(TEST_GID)]
            } else {
                Vec::new()
            },
            capability_grants: CapabilitySet::empty(),
        });
        builder.verify(&NullSink).expect("well-formed table")
    };
    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    cell.install_vfs(vfs(false)).expect("install vfs");
    cell.register(
        DriverHandle::from_raw(9).expect("handle"),
        driver(),
        "vol",
        "memfs",
        [0u8; 16],
        None,
    )
    .expect("register");
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    identity.install(table(true)).expect("identity");
    let svc = MountedFilesystemService::new(cell, identity);
    let caps = caps();
    let shared = "/Storage/vol/shared";
    open_path(
        &svc,
        TEST_UID,
        &caps,
        shared,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");
    svc.set_mode(TEST_UID, &caps, at(shared), 0o640, FinalLink::Follow)
        .expect("chmod");
    let held =
        own_handle(&svc, READER_UID, &caps, shared, OpenFlags::READ).expect("a member opens");
    let who = acting(READER_UID, &caps);
    let mut buf = [0u8; 1];
    for _ in 0..2 {
        assert_eq!(svc.read_node(&held, &who, 0, &mut buf), Ok(0));
    }
    identity.replace(table(false)).expect("replaces");
    assert_eq!(
        svc.read_node(&held, &who, 0, &mut buf),
        Err(Errno::PermissionDenied),
        "the replaced table's groups bind the open descriptor's next read"
    );
    assert_eq!(
        open_path(&svc, READER_UID, &caps, shared, OpenFlags::READ),
        Err(Errno::PermissionDenied)
    );
    identity.replace(table(true)).expect("replaces");
    assert_eq!(
        svc.read_node(&held, &who, 0, &mut buf),
        Ok(0),
        "and so does a membership given back"
    );
    close(&svc, held);
}

#[test]
fn create_write_read_back_under_attested_identity() {
    let svc = ready();
    let caps = caps();
    let p = path("notes.txt");
    let flags = OpenFlags::CREATE
        .union(OpenFlags::WRITE)
        .union(OpenFlags::READ);
    open_path(&svc, TEST_UID, &caps, &p, flags).expect("create-open");
    assert_eq!(
        write_path(&svc, TEST_UID, &caps, &p, 0, false, b"hello"),
        Ok(5)
    );

    let mut buf = [0u8; 16];
    let n = read_path(&svc, TEST_UID, &caps, &p, 0, &mut buf).expect("read");
    assert_eq!(&buf[..n], b"hello");
}

/// A file's bytes are the file's to grant, never its directory's: one the
/// caller may not write is refused at open, at every write and at a
/// truncate though its directory is the caller's to write.
#[test]
fn a_file_its_caller_may_not_write_is_refused_in_a_writable_directory() {
    let svc = ready();
    let caps = caps();
    let p = create_file(&svc, "locked");
    write_path(&svc, TEST_UID, &caps, &p, 0, false, b"kept").expect("write");
    svc.set_mode(TEST_UID, &caps, at(&p), 0o444, FinalLink::Follow)
        .expect("chmod");

    let denied = Err(Errno::PermissionDenied);
    assert_eq!(
        open_path(&svc, TEST_UID, &caps, &p, OpenFlags::WRITE),
        denied
    );
    assert_eq!(
        open_path(
            &svc,
            TEST_UID,
            &caps,
            &p,
            OpenFlags::WRITE.union(OpenFlags::TRUNCATE)
        ),
        denied
    );
    assert_eq!(
        write_path(&svc, TEST_UID, &caps, &p, 0, false, b"over").map(|_| ()),
        denied
    );
    assert_eq!(truncate_path(&svc, TEST_UID, &caps, &p, 0), denied);
    assert_eq!(
        open_path(&svc, TEST_UID, &caps, &p, OpenFlags::READ),
        Ok(())
    );
    let mut buf = [0u8; 8];
    let n = read_path(&svc, TEST_UID, &caps, &p, 0, &mut buf).expect("read");
    assert_eq!(&buf[..n], b"kept", "nothing was written or cut");
}

/// The other half: a file the caller may write is written though the
/// directory holding it refuses the caller new names.
#[test]
fn a_writable_file_is_written_in_a_directory_its_caller_may_not_write() {
    let svc = ready();
    let caps = caps();
    let dir = path("d");
    svc.mkdir(TEST_UID, &caps, at(&dir), new_dir())
        .expect("mkdir");
    svc.set_mode(TEST_UID, &caps, at(&dir), 0o755, FinalLink::Follow)
        .expect("traversable");
    let file = alloc::format!("{dir}/f");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &file,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");
    svc.set_mode(TEST_UID, &caps, at(&dir), 0o555, FinalLink::Follow)
        .expect("closed");

    assert_eq!(
        open_path(&svc, TEST_UID, &caps, &file, OpenFlags::WRITE),
        Ok(())
    );
    assert_eq!(
        write_path(&svc, TEST_UID, &caps, &file, 0, false, b"edit"),
        Ok(4)
    );
    assert_eq!(truncate_path(&svc, TEST_UID, &caps, &file, 2), Ok(()));
    let mut buf = [0u8; 8];
    let n = read_path(&svc, TEST_UID, &caps, &file, 0, &mut buf).expect("read");
    assert_eq!(&buf[..n], b"ed");
    assert_eq!(
        open_path(
            &svc,
            TEST_UID,
            &caps,
            &alloc::format!("{dir}/g"),
            OpenFlags::CREATE.union(OpenFlags::WRITE)
        ),
        Err(Errno::PermissionDenied),
        "the directory still governs its names"
    );
}

#[test]
fn an_append_write_ignores_the_offset_and_extends() {
    let svc = ready();
    let caps = caps();
    let p = path("log");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &p,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");
    assert_eq!(
        write_path(&svc, TEST_UID, &caps, &p, 0, false, b"abc"),
        Ok(3)
    );
    // The supplied offset (0) is ignored; the append lands at end of file.
    assert_eq!(write_path(&svc, TEST_UID, &caps, &p, 0, true, b"de"), Ok(2));

    let mut buf = [0u8; 8];
    let n = read_path(&svc, TEST_UID, &caps, &p, 0, &mut buf).expect("read");
    assert_eq!(&buf[..n], b"abcde");
}

#[test]
fn stat_reports_kind_size_and_attested_owner() {
    let svc = ready();
    let caps = caps();
    let p = path("f");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &p,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");
    write_path(&svc, TEST_UID, &caps, &p, 0, false, b"1234").expect("write");

    let st = svc
        .stat(TEST_UID, &caps, at(&p), FinalLink::Follow)
        .expect("stat");
    assert_eq!(st.kind, FileKind::Regular);
    assert_eq!(st.size, 4);
    assert_eq!(st.uid, TEST_UID);
    assert_eq!(st.gid, TEST_GID);
    assert_eq!(st.mode, 0o644);
}

/// One batch of the listing of `opened` from `at`, through the opener's own
/// descriptor.
fn a_batch(
    svc: &dyn FilesystemService,
    opened: &Opened,
    at: &mut Listing,
    each: &mut dyn FnMut(&tairix_abi::DirEntry<'_>) -> DirVisit,
) -> Result<(), Errno> {
    let caps = caps();
    let who = acting(TEST_UID, &caps);
    match opened {
        Opened::Catalog(catalog) => svc.readdir_catalog(TEST_UID, &caps, catalog, at, each),
        Opened::Node {
            node,
            projection,
            opened_as,
            at_floor,
            ..
        } => {
            let handle = NodeHandle {
                node: node.clone(),
                projection: Arc::clone(projection),
                authority: Authority::Holder,
                opened_as: Arc::clone(opened_as),
                at_floor: *at_floor,
            };
            svc.readdir_node(&handle, &who, at, each)
        }
    }
}

/// The names `opened` lists, read one entry per batch through one listing.
fn names_of(svc: &dyn FilesystemService, opened: &Opened) -> Vec<alloc::string::String> {
    let mut at = Listing::default();
    let mut names = Vec::new();
    while !at.is_done() {
        let mut took = false;
        a_batch(svc, opened, &mut at, &mut |entry| {
            if took {
                return DirVisit::Stop;
            }
            took = true;
            names.push(alloc::string::String::from_utf8(entry.name.to_vec()).expect("utf8"));
            DirVisit::Take
        })
        .expect("a batch");
        assert!(
            took || at.is_done(),
            "a batch that takes nothing ends the listing"
        );
    }
    names
}

/// The names `path` lists, read one entry per batch through one descriptor.
fn names_a_batch_at_a_time(svc: &dyn FilesystemService, path: &str) -> Vec<alloc::string::String> {
    let opened = svc
        .open(
            TEST_UID,
            &caps(),
            at(path),
            OpenFlags::DIRECTORY,
            CreateMode::NONE,
        )
        .expect("opens");
    names_of(svc, &opened)
}

/// A listing reads the directory its descriptor was opened on: renamed away
/// and replaced at its path between batches, the listing carries on through
/// the original, and a fresh open lists what the path names now.
///
/// Regression: a listing re-resolved its path every batch, so a directory
/// moved under it was refused as stale.
#[test]
fn a_listing_reads_the_directory_it_was_opened_on_however_its_path_moves() {
    let svc = ready();
    let caps = caps();
    let create = |name: &str| {
        open_path(
            &svc,
            TEST_UID,
            &caps,
            &path(name),
            OpenFlags::CREATE.union(OpenFlags::WRITE),
        )
        .expect("create");
    };
    svc.mkdir(TEST_UID, &caps, at(&path("d")), new_dir())
        .expect("mkdir");
    create("d/a");
    create("d/b");
    let opened = svc
        .open(
            TEST_UID,
            &caps,
            at(&path("d")),
            OpenFlags::DIRECTORY,
            CreateMode::NONE,
        )
        .expect("opens");
    let mut listing = Listing::default();
    a_batch(&svc, &opened, &mut listing, &mut |_| DirVisit::Stop).expect("the first batch");
    svc.rename(
        TEST_UID,
        &caps,
        at(&path("d")),
        at(&path("old")),
        RenameFlags::empty(),
        None,
    )
    .expect("rename away");
    svc.mkdir(TEST_UID, &caps, at(&path("d")), new_dir())
        .expect("a new d");
    create("d/c");
    let mut names = Vec::new();
    a_batch(&svc, &opened, &mut listing, &mut |entry| {
        names.push(entry.name.to_vec());
        DirVisit::Take
    })
    .expect("the listing carries on");
    assert_eq!(names, [b"a".to_vec(), b"b".to_vec()]);
    assert_eq!(names_a_batch_at_a_time(&svc, &path("d")), ["c"]);
}

/// A directory removed while a descriptor holds it lists nothing more, and
/// keeps its number from the next directory made while it is held, however
/// eagerly the volume reuses numbers; once let go, its number is free again.
///
/// Regression: the next directory made at the path took the removed one's
/// number, so a listing could not tell the two apart.
#[test]
fn a_held_directory_removed_lists_nothing_and_keeps_its_number() {
    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    cell.install_vfs(vfs(false)).expect("install vfs");
    let handle = DriverHandle::from_raw(9).expect("non-zero handle");
    cell.register(
        handle,
        driver().with_reused_numbers(),
        "vol",
        "memfs",
        [0u8; 16],
        None,
    )
    .expect("register driver");
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    identity.install(identity_table()).expect("identity");
    let svc = MountedFilesystemService::new(cell, identity);
    let caps = caps();
    let id_of = |name: &str| {
        svc.stat(TEST_UID, &caps, at(&path(name)), FinalLink::Follow)
            .expect("stat")
            .id
    };
    svc.mkdir(TEST_UID, &caps, at(&path("d")), new_dir())
        .expect("mkdir");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &path("d/a"),
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");
    let old = id_of("d");
    let held = own_handle(&svc, TEST_UID, &caps, &path("d"), OpenFlags::DIRECTORY).expect("opens");
    svc.unlink(
        TEST_UID,
        &caps,
        at(&path("d/a")),
        UnlinkFlags::empty(),
        None,
    )
    .expect("unlink");
    svc.unlink(
        TEST_UID,
        &caps,
        at(&path("d")),
        UnlinkFlags::DIRECTORY,
        None,
    )
    .expect("rmdir");
    svc.mkdir(TEST_UID, &caps, at(&path("d")), new_dir())
        .expect("a new d");
    assert_ne!(id_of("d"), old, "a held number is not handed out");
    let mut names = Vec::new();
    svc.readdir_node(
        &held,
        &acting(TEST_UID, &caps),
        &mut Listing::default(),
        &mut |entry| {
            names.push(entry.name.to_vec());
            DirVisit::Take
        },
    )
    .expect("a removed directory lists");
    assert!(names.is_empty(), "a removed directory lists nothing");
    close(&svc, held);
    svc.mkdir(TEST_UID, &caps, at(&path("e")), new_dir())
        .expect("mkdir");
    assert_eq!(id_of("e"), old, "let go, its number is free again");
}

/// Listing the mount points beneath a directory enters the driver with no
/// guard held on the mount table: the table's lock prefers writers, so a
/// guard held into a driver deadlocks the next mount against the listing.
#[test]
fn a_listing_holds_no_mount_table_guard_into_its_driver() {
    let h_parent = DriverHandle::from_raw(9).expect("handle");
    let h_usb = DriverHandle::from_raw(10).expect("handle");
    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    let parent = driver().with_lookup_probe(Box::new(move || {
        let vfs = cell.vfs().expect("installed");
        assert!(
            vfs.mounts_unlocked(),
            "a mount-table guard was held into the driver"
        );
    }));
    let vfs = Vfs::with_default_layout(UserId(TEST_UID), GroupId(TEST_GID));
    root_with_children(&vfs, h_parent, &[(h_usb, "/usb1")]);
    cell.install_vfs(vfs).expect("install vfs");
    cell.register(h_parent, parent, "root", "memfs", [0u8; 16], None)
        .expect("register parent");
    cell.register(h_usb, driver(), "usb1", "memfs", [0u8; 16], None)
        .expect("register usb");
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    identity.install(identity_table()).expect("identity");
    let svc = MountedFilesystemService::new(cell, identity);
    let names = names_a_batch_at_a_time(&svc, "/");
    assert!(names.iter().any(|name| name == "usb1"), "{names:?}");
}

#[test]
fn mkdir_then_readdir_reports_each_entrys_kind() {
    let svc = ready();
    let caps = caps();
    svc.mkdir(TEST_UID, &caps, at(&path("sub")), new_dir())
        .expect("mkdir");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &path("file"),
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create file");

    let mut entries = svc
        .readdir_bounded(TEST_UID, &caps, at(MOUNT), FinalLink::Follow, usize::MAX)
        .expect("readdir");
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let kinds: Vec<(FileKind, &str)> = entries.iter().map(|e| (e.kind, e.name.as_str())).collect();
    assert_eq!(
        kinds,
        alloc::vec![(FileKind::Regular, "file"), (FileKind::Directory, "sub"),]
    );
}

/// A listing reports each entry's system-wide identity and name count, so a
/// walk can tell a second *name* for one node from a second node — the fact
/// `du`'s hard-link deduplication rests on. Without the identity on the
/// readdir record the two names below are indistinguishable from two files.
#[test]
fn readdir_reports_one_identity_for_two_names_of_one_node() {
    let svc = ready();
    let caps = caps();
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &path("first"),
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");
    svc.link(
        TEST_UID,
        &caps,
        at(&path("first")),
        at(&path("second")),
        FinalLink::Keep,
    )
    .expect("second name");

    let entries = svc
        .readdir_bounded(TEST_UID, &caps, at(MOUNT), FinalLink::Follow, usize::MAX)
        .expect("readdir");
    let first = entries
        .iter()
        .find(|e| e.name == "first")
        .expect("first name listed");
    let second = entries
        .iter()
        .find(|e| e.name == "second")
        .expect("second name listed");
    assert_eq!(first.id, second.id);
    assert!(!first.id.is_none(), "a real volume names a real node");
    assert_eq!((first.nlink, second.nlink), (2, 2));

    // The listing's identity is the same one `stat` reports for the path, so
    // the two views of a node cannot disagree about what it is.
    let stat = svc
        .stat(TEST_UID, &caps, at(&path("first")), FinalLink::Follow)
        .expect("stat");
    assert_eq!(stat.id, first.id);
    assert_eq!(stat.nlink, first.nlink);
}

/// Two distinct single-named files are two identities, so a deduplicating
/// walk never collapses them.
#[test]
fn readdir_reports_distinct_identities_for_distinct_nodes() {
    let svc = ready();
    let caps = caps();
    for name in ["one", "two"] {
        open_path(
            &svc,
            TEST_UID,
            &caps,
            &path(name),
            OpenFlags::CREATE.union(OpenFlags::WRITE),
        )
        .expect("create");
    }
    let entries = svc
        .readdir_bounded(TEST_UID, &caps, at(MOUNT), FinalLink::Follow, usize::MAX)
        .expect("readdir");
    let ids: Vec<tairix_abi::FileId> = entries.iter().map(|e| e.id).collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    assert!(entries.iter().all(|e| e.nlink == NodeInfo::SINGLE_NAME));
}

/// A node's identity is the number its format gives it, not the id the
/// driver resolves it by: `stat`, a listing and a lookup of entries all report
/// the number, so they agree with each other and with nothing else.
#[test]
fn a_nodes_identity_is_its_formats_number_not_its_id() {
    const BIAS: u64 = 1 << 40;
    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    cell.install_vfs(vfs(false)).expect("install vfs");
    let handle = DriverHandle::from_raw(9).expect("non-zero handle");
    cell.register(
        handle,
        driver().with_ino_bias(BIAS),
        "vol",
        "memfs",
        [0u8; 16],
        None,
    )
    .expect("register driver");
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    identity.install(identity_table()).expect("identity");
    let svc = MountedFilesystemService::new(cell, identity);
    let caps = caps();
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &path("f"),
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");

    let file = svc
        .stat(TEST_UID, &caps, at(&path("f")), FinalLink::Follow)
        .expect("stat");
    assert!(file.id.node > BIAS, "the format's number, not the id");
    let listed = svc
        .readdir_bounded(TEST_UID, &caps, at(MOUNT), FinalLink::Follow, usize::MAX)
        .expect("readdir");
    assert_eq!(
        listed.iter().find(|e| e.name == "f").map(|e| e.id),
        Some(file.id)
    );
    let dir = svc
        .stat(TEST_UID, &caps, at(MOUNT), FinalLink::Follow)
        .expect("stat the directory");
    assert!(dir.id.node > BIAS);
    let names: [&[u8]; 1] = [b"f"];
    let looked = svc
        .lookup_entries(TEST_UID, &caps, at(MOUNT), FinalLink::Follow, &names)
        .expect("looks up");
    assert_eq!(looked.dir, dir.id);
    assert_eq!(looked.entries[0].as_ref().map(|e| e.id), Some(file.id));
}

#[test]
fn unlink_removes_the_file() {
    let svc = ready();
    let caps = caps();
    let p = path("gone");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &p,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");
    svc.unlink(TEST_UID, &caps, at(&p), UnlinkFlags::empty(), None)
        .expect("unlink");
    let mut buf = [0u8; 4];
    assert_eq!(
        read_path(&svc, TEST_UID, &caps, &p, 0, &mut buf),
        Err(Errno::NotFound)
    );
}

#[test]
fn directory_only_unlink_refuses_a_file_and_removes_an_empty_directory() {
    // The `rmdir` posture through the whole service: `DIRECTORY` reaching a
    // file is the dedicated `NotADirectory` errno and the file survives; the
    // same flag removes an (empty) directory.
    let svc = ready();
    let caps = caps();
    let file = path("plain");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &file,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");
    assert_eq!(
        svc.unlink(TEST_UID, &caps, at(&file), UnlinkFlags::DIRECTORY, None),
        Err(Errno::NotADirectory)
    );
    svc.stat(TEST_UID, &caps, at(&file), FinalLink::Follow)
        .expect("file survives the refused dir-only removal");
    let dir = path("empty");
    svc.mkdir(TEST_UID, &caps, at(&dir), new_dir())
        .expect("mkdir");
    svc.unlink(TEST_UID, &caps, at(&dir), UnlinkFlags::DIRECTORY, None)
        .expect("dir-only remove of an empty directory");
    assert_eq!(
        svc.stat(TEST_UID, &caps, at(&dir), FinalLink::Follow),
        Err(Errno::NotFound)
    );
}

#[test]
fn truncate_changes_the_size() {
    let svc = ready();
    let caps = caps();
    let p = path("t");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &p,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");
    write_path(&svc, TEST_UID, &caps, &p, 0, false, b"0123456789").expect("write");
    truncate_path(&svc, TEST_UID, &caps, &p, 4).expect("truncate");
    assert_eq!(
        svc.stat(TEST_UID, &caps, at(&p), FinalLink::Follow)
            .expect("stat")
            .size,
        4
    );
}

#[test]
fn an_exclusive_create_of_an_existing_path_fails_closed() {
    let svc = ready();
    let caps = caps();
    let p = path("dup");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &p,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("first create");
    let exclusive = OpenFlags::CREATE
        .union(OpenFlags::EXCLUSIVE)
        .union(OpenFlags::WRITE);
    // The dedicated `EEXIST` equivalent crosses the ABI boundary intact.
    assert_eq!(
        open_path(&svc, TEST_UID, &caps, &p, exclusive),
        Err(Errno::AlreadyExists)
    );
}

#[test]
fn a_directory_open_of_a_regular_file_fails_closed() {
    let svc = ready();
    let caps = caps();
    let p = path("plain");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &p,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");
    // The dedicated `ENOTDIR` equivalent crosses the ABI boundary intact.
    assert_eq!(
        open_path(&svc, TEST_UID, &caps, &p, OpenFlags::DIRECTORY),
        Err(Errno::NotADirectory)
    );
}

#[test]
fn a_write_to_a_read_only_mount_fails_closed() {
    // The mount carries `READ_ONLY`, so the secured write is refused before
    // the driver is touched.
    let svc = service(true, true, true);
    let caps = caps();
    assert_eq!(
        write_path(&svc, TEST_UID, &caps, &path("x"), 0, false, b"nope"),
        Err(Errno::ReadOnly)
    );
}

#[test]
fn sync_before_a_mount_is_installed_fails_closed() {
    let svc = service(false, true, false);
    let caps = caps();
    assert_eq!(
        sync_path(&svc, TEST_UID, &caps, &path("x")),
        Err(Errno::NotImplemented)
    );
    assert_eq!(svc.sync_all(TEST_UID, &caps), Err(Errno::NotImplemented));
}

#[test]
fn sync_flushes_the_mounted_volume() {
    // The in-memory driver flushes as a no-op; the call still resolves the
    // mount and returns success rather than the hollow `NotImplemented`.
    let svc = ready();
    let caps = caps();
    assert_eq!(sync_path(&svc, TEST_UID, &caps, MOUNT), Ok(()));
    assert_eq!(svc.sync_all(TEST_UID, &caps), Ok(()));
}

/// A descriptor's sync flushes its own volume only: a failing volume beside
/// it is neither flushed nor allowed to fail the call. The whole-system flush
/// still tries every volume and reports the refusal.
#[test]
fn sync_flushes_only_the_volume_serving_the_path() {
    let vfs = vfs(false);
    let bad = DriverHandle::from_raw(10).expect("non-zero handle");
    vfs.mounts_write()
        .attach_at(
            Attachment::new(
                bad,
                Some(MOUNT_MEDIUM),
                MountFlags::from_bits(0).expect("empty flags"),
            ),
            Path::parse("/Storage/bad").expect("valid mount path"),
            crate::fs::Metadata::new(UserId(TEST_UID), GroupId(TEST_GID), Mode::from_bits(0o755)),
        )
        .expect("second volume");
    let cell: &'static LateFilesystem<FlushSpy> = crate::forever(Box::new(LateFilesystem::new()));
    cell.install_vfs(vfs).expect("install vfs");
    let good = DriverHandle::from_raw(9).expect("non-zero handle");
    cell.register(good, FlushSpy::new(), "vol", "spy", [0u8; 16], None)
        .expect("register");
    cell.register(bad, FlushSpy::refusing(), "bad", "spy", [0u8; 16], None)
        .expect("register");
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    identity
        .install(identity_table())
        .expect("install identity");
    let svc = MountedFilesystemService::new(cell, identity);
    let caps = caps();
    let flushes = |volume| cell.driver(volume).expect("registered").lock().flushes;

    assert_eq!(sync_path(&svc, TEST_UID, &caps, MOUNT), Ok(()));
    assert_eq!((flushes(good), flushes(bad)), (1, 0));
    assert_eq!(
        sync_path(&svc, TEST_UID, &caps, "/Storage"),
        Ok(()),
        "the catalog stores nothing to flush"
    );
    assert_eq!(svc.sync_all(TEST_UID, &caps), Err(Errno::DeviceFault));
    assert_eq!(
        (flushes(good), flushes(bad)),
        (2, 1),
        "every volume is tried"
    );
}

#[test]
fn rename_moves_a_file_under_the_attested_identity() {
    let svc = ready();
    let caps = caps();
    let src = path("a");
    let dst = path("b");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &src,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");
    write_path(&svc, TEST_UID, &caps, &src, 0, false, b"hi").expect("write");
    svc.rename(
        TEST_UID,
        &caps,
        at(&src),
        at(&dst),
        RenameFlags::empty(),
        None,
    )
    .expect("rename");
    let mut buf = [0u8; 2];
    assert_eq!(
        read_path(&svc, TEST_UID, &caps, &src, 0, &mut buf),
        Err(Errno::NotFound)
    );
    assert_eq!(read_path(&svc, TEST_UID, &caps, &dst, 0, &mut buf), Ok(2));
    assert_eq!(&buf, b"hi");
}

/// Create `name` holding `body` on `svc`.
fn file_with(svc: &MountedFilesystemService<RwMockFs>, name: &str, body: &[u8]) {
    let caps = caps();
    open_path(
        svc,
        TEST_UID,
        &caps,
        &path(name),
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");
    write_path(svc, TEST_UID, &caps, &path(name), 0, false, body).expect("write");
}

/// The names the mounted volume's root lists.
fn root_names(svc: &MountedFilesystemService<RwMockFs>) -> Vec<Vec<u8>> {
    let mut names = Vec::new();
    list_path(
        svc,
        TEST_UID,
        &caps(),
        MOUNT,
        OpenFlags::DIRECTORY,
        &mut |entry| {
            names.push(entry.name.to_vec());
            DirVisit::Take
        },
    )
    .expect("list");
    names
}

#[test]
fn a_no_replace_rename_refuses_an_occupied_destination_and_keeps_both() {
    let svc = ready();
    let caps = caps();
    file_with(&svc, "a", b"aa");
    file_with(&svc, "b", b"bb");
    assert_eq!(
        svc.rename(
            TEST_UID,
            &caps,
            at(&path("a")),
            at(&path("b")),
            RenameFlags::NO_REPLACE,
            None,
        ),
        Err(Errno::AlreadyExists)
    );
    let mut buf = [0u8; 2];
    assert_eq!(
        read_path(&svc, TEST_UID, &caps, &path("a"), 0, &mut buf),
        Ok(2)
    );
    assert_eq!(&buf, b"aa");
    assert_eq!(
        read_path(&svc, TEST_UID, &caps, &path("b"), 0, &mut buf),
        Ok(2)
    );
    assert_eq!(&buf, b"bb");
}

#[test]
fn a_no_replace_rename_to_a_free_name_moves_the_file() {
    let svc = ready();
    let caps = caps();
    file_with(&svc, "a", b"aa");
    svc.rename(
        TEST_UID,
        &caps,
        at(&path("a")),
        at(&path("c")),
        RenameFlags::NO_REPLACE,
        None,
    )
    .expect("rename");
    assert_eq!(root_names(&svc), [b"c".to_vec()]);
}

/// On a volume that ignores letter case, `B` names the sibling `b`: a
/// no-replace move onto it is a clash, not a new name.
#[test]
fn a_no_replace_rename_refuses_a_sibling_differing_only_in_case() {
    let svc = ready();
    let caps = caps();
    file_with(&svc, "a", b"aa");
    file_with(&svc, "b", b"bb");
    assert_eq!(
        svc.rename(
            TEST_UID,
            &caps,
            at(&path("a")),
            at(&path("B")),
            RenameFlags::NO_REPLACE,
            None,
        ),
        Err(Errno::AlreadyExists)
    );
    let mut buf = [0u8; 2];
    assert_eq!(
        read_path(&svc, TEST_UID, &caps, &path("b"), 0, &mut buf),
        Ok(2)
    );
    assert_eq!(&buf, b"bb");
}

/// A re-spelling of a name on a folding volume names the entry itself, so it
/// is renamed — under either flag — rather than reported done while nothing
/// changed.
#[test]
fn a_rename_respells_its_own_entry_on_a_folding_volume() {
    for flags in [RenameFlags::empty(), RenameFlags::NO_REPLACE] {
        let svc = ready();
        let caps = caps();
        file_with(&svc, "Foo.txt", b"ff");
        svc.rename(
            TEST_UID,
            &caps,
            at(&path("Foo.txt")),
            at(&path("foo.txt")),
            flags,
            None,
        )
        .expect("re-spell");
        assert_eq!(root_names(&svc), [b"foo.txt".to_vec()]);
        let mut buf = [0u8; 2];
        assert_eq!(
            read_path(&svc, TEST_UID, &caps, &path("foo.txt"), 0, &mut buf),
            Ok(2)
        );
        assert_eq!(&buf, b"ff");
    }
}

#[test]
fn rename_of_a_missing_source_fails_closed() {
    let svc = ready();
    let caps = caps();
    assert_eq!(
        svc.rename(
            TEST_UID,
            &caps,
            at(&path("nope")),
            at(&path("x")),
            RenameFlags::empty(),
            None,
        ),
        Err(Errno::NotFound)
    );
}

#[test]
fn rename_on_a_read_only_mount_fails_closed() {
    let svc = service(true, true, true);
    let caps = caps();
    assert_eq!(
        svc.rename(
            TEST_UID,
            &caps,
            at(&path("a")),
            at(&path("b")),
            RenameFlags::empty(),
            None,
        ),
        Err(Errno::ReadOnly)
    );
}

#[test]
fn set_mode_rewrites_the_permission_bits_for_the_owner() {
    let svc = ready();
    let caps = caps();
    let p = path("f");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &p,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");

    svc.set_mode(TEST_UID, &caps, at(&p), 0o600, FinalLink::Follow)
        .expect("chmod");

    let st = svc
        .stat(TEST_UID, &caps, at(&p), FinalLink::Follow)
        .expect("stat");
    assert_eq!(st.mode, 0o600);
    assert_eq!(st.uid, TEST_UID);
    assert_eq!(st.gid, TEST_GID);
}

#[test]
fn set_mode_above_the_permission_mask_fails_closed() {
    // Defence in depth at the service seam: a file-type bit is refused
    // before any resolution, so the record can never be corrupted.
    let svc = ready();
    let caps = caps();
    assert_eq!(
        svc.set_mode(
            TEST_UID,
            &caps,
            at(&path("f")),
            0o10_0644,
            FinalLink::Follow
        ),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn set_mode_on_a_read_only_mount_fails_closed() {
    let svc = service(true, true, true);
    let caps = caps();
    assert_eq!(
        svc.set_mode(TEST_UID, &caps, at(&path("a")), 0o600, FinalLink::Follow),
        Err(Errno::ReadOnly)
    );
}

#[test]
fn set_mode_before_a_mount_is_installed_fails_closed() {
    let svc = service(false, true, false);
    let caps = caps();
    assert_eq!(
        svc.set_mode(TEST_UID, &caps, at(&path("a")), 0o600, FinalLink::Follow),
        Err(Errno::NotImplemented)
    );
}

#[test]
fn rename_before_a_mount_is_installed_fails_closed() {
    let svc = service(false, true, false);
    let caps = caps();
    assert_eq!(
        svc.rename(
            TEST_UID,
            &caps,
            at(&path("a")),
            at(&path("b")),
            RenameFlags::empty(),
            None,
        ),
        Err(Errno::NotImplemented)
    );
}

/// A capability set holding `CAP_FS_CHOWN` — the privileged authority to
/// reassign a node's owner (and to set any group).
fn chown_caps() -> CapabilitySet {
    let mut caps = CapabilitySet::empty();
    caps.insert(CapabilityId::FS_CHOWN);
    caps
}

/// A fully-installed writable service whose identity table also makes the
/// test principal a member of the supplementary group `gid`, so the
/// unprivileged owner-may-set-a-group-they-belong-to branch is exercised.
fn ready_with_supplementary(gid: u32) -> MountedFilesystemService<RwMockFs> {
    let account = Account {
        groups: &[gid],
        ..TEST_ACCOUNT
    };
    mounted(driver(), false, &[account]).service
}

/// A writable service whose identity table holds the test principal and each
/// of `others`, every one in a group of its own.
fn ready_with_users(others: &[u32]) -> MountedFilesystemService<RwMockFs> {
    service_over(driver(), others).0
}

/// A writable service over `fs` at [`MOUNT`], whose identity table holds the
/// test principal and each of `others` in a group of its own, and the cell it
/// serves from.
fn service_over(
    fs: RwMockFs,
    others: &[u32],
) -> (
    MountedFilesystemService<RwMockFs>,
    &'static LateFilesystem<RwMockFs>,
) {
    let accounts: Vec<Account<'_>> = [TEST_UID]
        .iter()
        .chain(others)
        .map(|&uid| Account {
            uid,
            gid: uid,
            groups: &[],
        })
        .collect();
    let serving = mounted(fs, false, &accounts);
    (serving.service, serving.mounted)
}

/// In a sticky directory a writer may create, but only an entry's owner or
/// the directory's owner may unlink it, rename it away, or replace it.
#[test]
fn a_sticky_directory_guards_each_name_for_its_owner_and_the_directory_s() {
    const WRITER: u32 = 1001;
    const OTHER: u32 = 1002;
    let svc = ready_with_users(&[WRITER, OTHER]);
    let caps = caps();
    svc.mkdir(TEST_UID, &caps, at(&path("drop")), new_dir())
        .expect("mkdir");
    svc.set_mode(
        TEST_UID,
        &caps,
        at(&path("drop")),
        0o1777,
        FinalLink::Follow,
    )
    .expect("sticky");
    let create = OpenFlags::CREATE.union(OpenFlags::WRITE);
    for name in ["drop/a", "drop/b"] {
        open_path(&svc, WRITER, &caps, &path(name), create).expect("a writer creates");
    }
    open_path(&svc, OTHER, &caps, &path("drop/mine"), create).expect("a writer creates");
    let unlink =
        |uid, name: &str| svc.unlink(uid, &caps, at(&path(name)), UnlinkFlags::empty(), None);
    let rename = |uid, from: &str, to: &str| {
        svc.rename(
            uid,
            &caps,
            at(&path(from)),
            at(&path(to)),
            RenameFlags::empty(),
            None,
        )
    };
    let denied = Err(Errno::PermissionDenied);
    assert_eq!(unlink(OTHER, "drop/a"), denied);
    assert_eq!(rename(OTHER, "drop/a", "drop/taken"), denied);
    assert_eq!(
        rename(OTHER, "drop/mine", "drop/a"),
        denied,
        "replacing a name removes it"
    );
    assert_eq!(unlink(WRITER, "drop/a"), Ok(()));
    assert_eq!(unlink(TEST_UID, "drop/b"), Ok(()));
    assert_eq!(rename(OTHER, "drop/mine", "drop/moved"), Ok(()));
}

/// Create a regular file owned by the test principal and return its path.
fn create_file(svc: &MountedFilesystemService<RwMockFs>, name: &str) -> alloc::string::String {
    let p = path(name);
    open_path(
        svc,
        TEST_UID,
        &caps(),
        &p,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");
    p
}

#[test]
fn set_owner_reassigns_the_uid_only_for_a_privileged_caller() {
    let svc = ready();
    let p = create_file(&svc, "f");

    // Unprivileged: reassigning the owner is refused and nothing changes.
    assert_eq!(
        svc.set_owner(
            TEST_UID,
            &caps(),
            at(&p),
            2000,
            FS_OWNER_UNCHANGED,
            FinalLink::Follow
        ),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        svc.stat(TEST_UID, &caps(), at(&p), FinalLink::Follow)
            .expect("stat")
            .uid,
        TEST_UID
    );

    // Privileged (`CAP_FS_CHOWN`): the owner is reassigned.
    svc.set_owner(
        TEST_UID,
        &chown_caps(),
        at(&p),
        2000,
        FS_OWNER_UNCHANGED,
        FinalLink::Follow,
    )
    .expect("chown");
    assert_eq!(
        svc.stat(TEST_UID, &caps(), at(&p), FinalLink::Follow)
            .expect("stat")
            .uid,
        2000
    );
}

#[test]
fn set_owner_strips_the_setuid_bit_on_a_change() {
    let svc = ready();
    let p = create_file(&svc, "bin");
    // A setuid, group-executable file.
    svc.set_mode(TEST_UID, &caps(), at(&p), 0o4755, FinalLink::Follow)
        .expect("chmod");

    svc.set_owner(
        TEST_UID,
        &chown_caps(),
        at(&p),
        2000,
        FS_OWNER_UNCHANGED,
        FinalLink::Follow,
    )
    .expect("chown");

    let st = svc
        .stat(TEST_UID, &caps(), at(&p), FinalLink::Follow)
        .expect("stat");
    assert_eq!(st.uid, 2000);
    // The setuid bit is gone; a reassigned file cannot stay setuid.
    assert_eq!(st.mode & 0o7777, 0o0755);
}

#[test]
fn set_owner_keeps_a_shared_directorys_set_group_id_bit() {
    let svc = ready();
    let dir = path("shared");
    svc.mkdir(TEST_UID, &caps(), at(&dir), new_dir())
        .expect("mkdir");
    svc.set_mode(TEST_UID, &caps(), at(&dir), 0o2775, FinalLink::Follow)
        .expect("chmod");

    svc.set_owner(
        TEST_UID,
        &chown_caps(),
        at(&dir),
        2000,
        FS_OWNER_UNCHANGED,
        FinalLink::Follow,
    )
    .expect("chown");

    let st = svc
        .stat(TEST_UID, &caps(), at(&dir), FinalLink::Follow)
        .expect("stat");
    assert_eq!(st.uid, 2000);
    // Its entries go on being born in its group.
    assert_eq!(st.mode & 0o7777, 0o2775);
}

#[test]
fn set_owner_group_change_to_a_non_member_group_is_denied_unprivileged() {
    let svc = ready();
    let p = create_file(&svc, "g");
    // The owner belongs only to gid 1000; setting an unrelated group without
    // `CAP_FS_CHOWN` is refused, and the group is unchanged.
    assert_eq!(
        svc.set_owner(
            TEST_UID,
            &caps(),
            at(&p),
            FS_OWNER_UNCHANGED,
            55,
            FinalLink::Follow
        ),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        svc.stat(TEST_UID, &caps(), at(&p), FinalLink::Follow)
            .expect("stat")
            .gid,
        TEST_GID
    );
}

#[test]
fn set_owner_group_change_to_a_member_group_is_allowed_unprivileged() {
    let svc = ready_with_supplementary(7);
    let p = create_file(&svc, "g");
    // The owner is a member of gid 7, so an unprivileged `chgrp` to it
    // succeeds — no capability needed.
    svc.set_owner(
        TEST_UID,
        &caps(),
        at(&p),
        FS_OWNER_UNCHANGED,
        7,
        FinalLink::Follow,
    )
    .expect("chgrp to a member group");
    assert_eq!(
        svc.stat(TEST_UID, &caps(), at(&p), FinalLink::Follow)
            .expect("stat")
            .gid,
        7
    );
}

#[test]
fn set_owner_privileged_group_change_needs_no_membership() {
    let svc = ready();
    let p = create_file(&svc, "g");
    // A `CAP_FS_CHOWN` holder may set any group, member or not.
    svc.set_owner(
        TEST_UID,
        &chown_caps(),
        at(&p),
        FS_OWNER_UNCHANGED,
        55,
        FinalLink::Follow,
    )
    .expect("privileged chgrp");
    assert_eq!(
        svc.stat(TEST_UID, &caps(), at(&p), FinalLink::Follow)
            .expect("stat")
            .gid,
        55
    );
}

#[test]
fn set_owner_leaving_both_unchanged_is_a_noop_even_unprivileged() {
    let svc = ready();
    let p = create_file(&svc, "n");
    // Both fields sentinel: a no-op that succeeds without `CAP_FS_CHOWN`.
    svc.set_owner(
        TEST_UID,
        &caps(),
        at(&p),
        FS_OWNER_UNCHANGED,
        FS_OWNER_UNCHANGED,
        FinalLink::Follow,
    )
    .expect("no-op chown");
    let st = svc
        .stat(TEST_UID, &caps(), at(&p), FinalLink::Follow)
        .expect("stat");
    assert_eq!(st.uid, TEST_UID);
    assert_eq!(st.gid, TEST_GID);
}

#[test]
fn set_owner_on_a_read_only_mount_fails_closed() {
    let svc = service(true, true, true);
    assert_eq!(
        svc.set_owner(
            TEST_UID,
            &chown_caps(),
            at(&path("a")),
            2000,
            FS_OWNER_UNCHANGED,
            FinalLink::Follow,
        ),
        Err(Errno::ReadOnly)
    );
}

#[test]
fn set_owner_before_a_mount_is_installed_fails_closed() {
    let svc = service(false, true, false);
    assert_eq!(
        svc.set_owner(
            TEST_UID,
            &chown_caps(),
            at(&path("a")),
            2000,
            FS_OWNER_UNCHANGED,
            FinalLink::Follow,
        ),
        Err(Errno::NotImplemented)
    );
}

/// The production `/System` shape: the in-RAM layout and every node of the
/// read-only volume are owned by the system user (uid 0) at mode `0o755`,
/// and an ordinary account (uid 1000, no supplementary groups, no
/// capabilities) lists the mount. The read-only volume also carries `Logs`
/// and `Settings` entries whose *paths* are covered by the writable root
/// volume's rebased sub-mounts (the shipped-image shape), so the listing
/// must not judge those children against the read-only volume's driver.
/// A refusal here is the "cannot list inside /System as a user" regression.
/// The production layout over two memory volumes: the writable root as `/`,
/// a read-only volume over `/System`, and the writable grafts rebased back
/// out of it onto the root's own `/System/Logs` and `/System/Settings`.
fn production_layout() -> MountedFilesystemService<RwMockFs> {
    production_layout_planted(|_| {})
}

/// [`production_layout`], `plant` given the root volume before it is served.
fn production_layout_planted(
    plant: impl FnOnce(&mut RwMockFs),
) -> MountedFilesystemService<RwMockFs> {
    // The read-only `/System` volume: content at the volume root, including
    // the same-named `Logs`/`Settings` the writable mounts shadow.
    let mut system = RwMockFs::new();
    system.set_root_security(NodeSecurity::new(0o755, 0, 0));
    let root = system.root();
    for name in [&b"Kernel"[..], b"Drivers", b"Logs", b"Settings"] {
        system
            .create(root, name, NodeKind::Directory, &SYSTEM_RECORD)
            .expect("mkdir system subdir");
    }

    // The writable root volume: carries its own `/System/Logs` and
    // `/System/Settings` directories the sub-mounts rebase onto.
    let mut rootvol = RwMockFs::new();
    rootvol.set_root_security(NodeSecurity::new(0o755, 0, 0));
    let rv_root = rootvol.root();
    let rv_system = rootvol
        .create(rv_root, b"System", NodeKind::Directory, &SYSTEM_RECORD)
        .expect("mkdir System");
    for name in [&b"Logs"[..], b"Settings"] {
        rootvol
            .create(rv_system, name, NodeKind::Directory, &SYSTEM_RECORD)
            .expect("mkdir writable exception");
    }
    plant(&mut rootvol);

    // The production mount layout: writable root as `/`, read-only volume
    // over `/System`, writable exceptions rebased back out of it.
    let vfs = Vfs::with_default_layout(UserId(0), GroupId(0));
    let system_handle = DriverHandle::from_raw(9).expect("handle");
    let root_handle = DriverHandle::from_raw(10).expect("handle");
    {
        let mut mounts = vfs.mounts_write();
        mounts
            .attach(Attachment::new(root_handle, None, MountFlags::default()))
            .expect("attach the root volume");
        mounts
            .attach(Attachment::new(system_handle, None, MountFlags::READ_ONLY))
            .expect("attach the system volume");
        mounts.back_root(root_handle).expect("back /");
        mounts
            .set_backing(
                &Path::parse("/System").expect("path"),
                system_handle,
                Vec::new(),
            )
            .expect("back /System");
        for sub in ["/System/Logs", "/System/Settings"] {
            let path = Path::parse(sub).expect("path");
            let subtree = path.components().to_vec();
            mounts
                .set_backing(&path, root_handle, subtree)
                .expect("back writable exception");
        }
    }

    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    cell.install_vfs(vfs).expect("install vfs");
    cell.register(system_handle, system, "system", "memfs", [0u8; 16], None)
        .expect("register system");
    cell.register(root_handle, rootvol, "root", "memfs", [0u8; 16], None)
        .expect("register root");
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    identity.install(identity_table()).expect("identity");
    MountedFilesystemService::new(cell, identity)
}

/// Nothing reaches the directory a projection covers: a name a link
/// supplies at the floor is not found there, and a caller's name is matched
/// against the projections shown there as the volume matches its own.
///
/// Regression: a link to `/System` on the root volume, or a caller's
/// `/system` on a volume that folds case, reached the root volume's own
/// covered `System` directory, outside every mount point.
#[test]
fn nothing_reaches_what_a_projection_covers() {
    let svc = production_layout_planted(|rootvol| {
        let root = rootvol.root();
        for (name, target) in [(&b"l"[..], &b"/System"[..]), (b"folded", b"/system")] {
            rootvol
                .create_link(root, name, target, &SYSTEM_RECORD)
                .expect("plant the link");
        }
    });
    let caps = caps();
    let kind = |path: &str| {
        svc.stat(TEST_UID, &caps, at(path), FinalLink::Follow)
            .map(|stat| stat.kind)
    };
    assert_eq!(kind("/l"), Err(Errno::NotFound));
    assert_eq!(kind("/l/Logs"), Err(Errno::NotFound));
    assert_eq!(kind("/folded/Logs"), Err(Errno::NotFound));
    // Only the system volume holds `Kernel`: the caller reached the
    // projection, never the directory beneath it.
    assert_eq!(kind("/System/Kernel"), Ok(FileKind::Directory));
    assert_eq!(kind("/system/Kernel"), Ok(FileKind::Directory));
}

#[test]
fn an_ordinary_user_lists_the_system_owned_read_only_mount() {
    let svc = production_layout();
    let caps = caps();
    let entries = svc
        .readdir_bounded(
            TEST_UID,
            &caps,
            at("/System"),
            FinalLink::Follow,
            usize::MAX,
        )
        .expect("an ordinary user lists /System");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    for expected in ["Kernel", "Drivers", "Logs", "Settings"] {
        assert!(names.contains(&expected), "{expected} listed: {names:?}");
    }
    assert!(entries.iter().all(|e| e.kind == FileKind::Directory));
}

/// The `/Storage` catalog is stored on no volume, so the root volume holds no
/// node of that name: `/` shows it only through the mounts merged beneath it.
#[test]
fn the_root_lists_and_finds_the_storage_catalog() {
    let svc = production_layout();
    let caps = caps();
    let entries = svc
        .readdir_bounded(TEST_UID, &caps, at("/"), FinalLink::Follow, usize::MAX)
        .expect("an ordinary user lists /");
    let catalog = entries
        .iter()
        .find(|e| e.name == tairix_abi::STORAGE_CATALOG)
        .expect("the catalog is listed in /");
    assert_eq!(catalog.kind, FileKind::Directory);
    let looked = svc
        .lookup_entries(
            TEST_UID,
            &caps,
            at("/"),
            FinalLink::Follow,
            &[tairix_abi::STORAGE_CATALOG.as_bytes()],
        )
        .expect("an ordinary user looks a name up in /");
    assert!(looked.entries[0].is_some(), "and found by name");
}

/// The writable grafts take the system's own writes, so a write through one
/// reaches the root volume's driver under the system class — and a write
/// anywhere else on the same volume does not.
#[test]
fn a_write_through_a_system_graft_carries_the_system_class() {
    let svc = production_layout();
    let caps = caps();
    let name = core::str::from_utf8(SYSTEM_ONLY_NAME).expect("ascii");
    let flags = OpenFlags::CREATE.union(OpenFlags::WRITE);
    open_path(
        &svc,
        0,
        &caps,
        &alloc::format!("/System/Settings/{name}"),
        flags,
    )
    .expect("a create through the graft is the system's");
    assert_eq!(
        open_path(&svc, 0, &caps, &alloc::format!("/{name}"), flags),
        Err(Errno::NoSpace),
        "the same create elsewhere on the volume is ordinary"
    );
}

/// The mount snapshot reports each online volume once — its registration
/// names, the flags it was attached with, where its root is shown, and the
/// driver's live accounting — and nothing for the unbacked root or the
/// `/Storage` catalog, which are no volume.
#[test]
fn mount_snapshot_reports_each_online_volume_once() {
    let svc = ready();
    let records = svc.mount_snapshot();
    assert_eq!(records.len(), 1, "only the registered volume: {records:?}");
    let vol = &records[0];
    assert_eq!(vol.source_bytes(), b"vol");
    assert_eq!(vol.target_bytes(), MOUNT.as_bytes());
    assert_eq!(vol.fstype_bytes(), b"memfs");
    assert_eq!(vol.flags(), MountFlags::default());
    // The medium the attach path recorded reaches the record userland reads.
    assert_eq!(vol.medium(), Some(MOUNT_MEDIUM));
    let usage = vol.usage();
    assert_eq!(usage.block_size, 512);
    assert_eq!(usage.total_blocks, 4096);
    assert!(usage.avail_blocks <= usage.free_blocks);
    assert!(usage.free_blocks <= usage.total_blocks);
}

/// A volume busy with a long operation does not hold the mount list up: it
/// is reported with the figures it last gave, and its fresh ones once free.
///
/// Regression: the list waited on every volume's driver lock in turn, so one
/// volume in the middle of a long write stalled every caller of the System
/// Information service past its timeout.
#[test]
fn a_busy_volume_is_listed_with_its_last_figures_without_waiting() {
    let handle = DriverHandle::from_raw(23).expect("handle");
    let vfs = Vfs::with_default_layout(UserId(TEST_UID), GroupId(TEST_GID));
    {
        let mut mounts = vfs.mounts_write();
        mounts
            .attach(Attachment::new(handle, None, MountFlags::default()))
            .expect("attach");
        mounts.back_root(handle).expect("back /");
    }
    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    cell.install_vfs(vfs).expect("install vfs");
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    identity.install(identity_table()).expect("identity");
    let svc = MountedFilesystemService::new(cell, identity);
    let driver = cell
        .register(handle, driver(), "root", "memfs", [7; 16], None)
        .expect("register");
    let free = svc.mount_snapshot()[0].usage();
    assert_eq!(free.total_blocks, 4096, "measured while free");
    let busy = driver.try_lock().expect("held by a long operation");
    let listed = svc.mount_snapshot();
    assert_eq!(listed.len(), 1, "still listed");
    assert_eq!(listed[0].usage(), free, "with the figures it last gave");
    drop(busy);
}

/// A volume shown at several view paths is one volume: the mount list reports
/// it once, from the projection showing its root, reading its accounting
/// once; the view list reports every binding, each naming the volume by its
/// identity. Neither lists a volume whose driver is not yet online.
///
/// Regression: the mount list reported the root volume once per mount carved
/// out of it, each row reading the driver's accounting again.
#[test]
fn a_volume_shown_at_several_paths_is_reported_once() {
    use core::sync::atomic::{AtomicUsize, Ordering};
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&reads);
    let volume = driver().with_stats_probe(Box::new(move || {
        counted.fetch_add(1, Ordering::Relaxed);
    }));
    let handle = DriverHandle::from_raw(21).expect("handle");
    let id = [0x5Au8; 16];
    let vfs = Vfs::with_default_layout(UserId(TEST_UID), GroupId(TEST_GID));
    {
        let mut mounts = vfs.mounts_write();
        mounts
            .attach(Attachment::new(handle, None, MountFlags::default()))
            .expect("attach");
        mounts.back_root(handle).expect("back /");
        for sub in ["/System/Logs", "/System/Settings"] {
            let path = Path::parse(sub).expect("path");
            let subtree = path.components().to_vec();
            mounts.set_backing(&path, handle, subtree).expect("back");
        }
    }
    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    cell.install_vfs(vfs).expect("install vfs");
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    identity.install(identity_table()).expect("identity");
    let svc = MountedFilesystemService::new(cell, identity);

    let views = |svc: &MountedFilesystemService<RwMockFs>| -> Vec<_> {
        svc.view_snapshot()
            .iter()
            .map(|view| {
                (
                    alloc::string::String::from_utf8(view.target_bytes().to_vec()).expect("utf-8"),
                    alloc::string::String::from_utf8(view.subtree_bytes().to_vec()).expect("utf-8"),
                    view.flags(),
                    view.volume_id(),
                )
            })
            .collect()
    };
    let row = |target: &str, subtree: &str, flags, id| {
        (
            alloc::string::String::from(target),
            alloc::string::String::from(subtree),
            flags,
            id,
        )
    };
    let none = [0u8; 16];
    let unbacked = [
        row("/System", "", MountFlags::READ_ONLY, none),
        row(
            "/Storage",
            "",
            MountFlags::READ_ONLY.union(MountFlags::NOEXEC),
            none,
        ),
    ];

    assert!(svc.mount_snapshot().is_empty(), "the driver is not online");
    assert_eq!(
        views(&svc),
        unbacked,
        "only the bindings showing no volume are listed before it is"
    );

    cell.register(handle, volume, "root", "memfs", id, None)
        .expect("register");
    let records = svc.mount_snapshot();
    assert_eq!(records.len(), 1, "one volume: {records:?}");
    assert_eq!(records[0].target_bytes(), b"/");
    assert_eq!(records[0].volume_id(), id);
    assert_eq!(reads.load(Ordering::Relaxed), 1, "its accounting read once");

    let [system, storage] = unbacked;
    assert_eq!(
        views(&svc),
        [
            row("/", "/", MountFlags::default(), id),
            system,
            row("/System/Logs", "/System/Logs", MountFlags::NOEXEC, id),
            row(
                "/System/Settings",
                "/System/Settings",
                MountFlags::NOEXEC,
                id
            ),
            storage,
        ]
    );
}

/// A volume's driver coming, going, or changing availability moves what the
/// mount listing reports though no mount point moved, so each is published
/// as a `Mounts` notice.
///
/// Regression: none was, so an unlocked root — whose mounts exist from boot
/// and gain their driver only at unlock — and a surprise-removed volume
/// reached no subscriber of the notice.
#[test]
fn a_volume_coming_going_or_failing_is_published_as_a_mounts_notice() {
    use tairix_abi::NoticeTopic;
    let generation =
        || crate::notice::generation(NoticeTopic::Mounts, &crate::seat::NULL_SEAT_REGISTRY);
    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    cell.install_vfs(vfs(false)).expect("install vfs");
    let handle = DriverHandle::from_raw(11).expect("handle");

    let before = generation();
    cell.register(handle, driver(), "vol", "memfs", [0u8; 16], None)
        .expect("registered");
    let registered = generation();
    assert!(registered > before, "a volume coming online is published");

    cell.set_availability(handle, MountAvailability::UnavailableLost)
        .expect("marked");
    let failed = generation();
    assert!(
        failed > registered,
        "a volume becoming unavailable is published"
    );

    assert!(cell.unregister(handle).is_some());
    assert!(generation() > failed, "a volume leaving is published");
}

/// The mount snapshot overlays a live device's reported I/O health onto an
/// otherwise-`Available` volume, but a genuine surprise-removal state always
/// wins over the overlay so a vanished volume never masquerades as merely
/// unwell (`plans/FIX-IO.md` IO2/IO3; `plans/DEVICES.md` D4).
#[test]
fn mount_snapshot_overlays_reported_block_health() {
    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    cell.install_vfs(vfs(false)).expect("install vfs");
    let handle = DriverHandle::from_raw(9).expect("handle");
    cell.register(handle, driver(), "vol", "memfs", [0u8; 16], None)
        .expect("register");
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    identity.install(identity_table()).expect("identity");
    let svc = MountedFilesystemService::new(cell, identity);

    let vol_availability = |svc: &MountedFilesystemService<RwMockFs>| {
        svc.mount_snapshot()
            .iter()
            .find(|record| record.source_bytes() == b"vol")
            .expect("the registered volume is listed")
            .availability()
    };

    // With no health source the volume reads plainly available.
    assert_eq!(vol_availability(&svc), MountAvailability::Available);

    // With no I/O source the three per-volume queries list no device.
    assert!(svc.volume_io_health_snapshot().is_empty());
    assert!(svc.volume_io_stats_snapshot().is_empty());
    assert!(svc.volume_io_queue_snapshot().is_empty());

    // A live block-health overlay reporting a blip surfaces as recovering.
    let availability = Arc::new(AtomicU8::new(MountAvailability::Recovering.as_u8()));
    let counters = Arc::new(BlkHealthCountersAtomic::default());
    let stats = Arc::new(BlkIoStatsAtomic::default());
    let budget = BlkDeviceClass::Rotational.budget();
    cell.set_io_source(
        handle,
        VolumeIoSource {
            dev: 0x42,
            device: tairix_abi::blkio::BlkDeviceName::new("mounted-blk"),
            availability: Arc::clone(&availability),
            counters: Arc::clone(&counters),
            stats: Arc::clone(&stats),
            budget,
        },
    )
    .expect("attach I/O source");
    assert_eq!(vol_availability(&svc), MountAvailability::Recovering);

    // The volume-health query now lists exactly this device, naming its
    // serving endpoint and overlaying the same live availability the mount
    // snapshot shows.
    let health_records = svc.volume_io_health_snapshot();
    assert_eq!(health_records.len(), 1);
    assert_eq!(health_records[0].dev(), 0x42);
    assert_eq!(
        health_records[0].availability(),
        MountAvailability::Recovering
    );
    assert_eq!(health_records[0].counters().completions, 0);

    // The service and queue queries list the same one device, keyed and
    // ordered identically, so a client can join the three by volume id — and
    // the queue record carries the budget the depth is read against, from the
    // device's own class rather than a global constant.
    let stats_records = svc.volume_io_stats_snapshot();
    let queue_records = svc.volume_io_queue_snapshot();
    assert_eq!(stats_records.len(), 1);
    assert_eq!(queue_records.len(), 1);
    assert_eq!(stats_records[0].volume_id(), health_records[0].volume_id());
    assert_eq!(queue_records[0].volume_id(), health_records[0].volume_id());
    assert_eq!(stats_records[0].dev(), 0x42);
    assert_eq!(queue_records[0].dev(), 0x42);
    assert_eq!(stats_records[0].counters(), BlkIoCounters::default());
    // The device's own name rides the ungated service record, so a consumer
    // that may read no queue depth and no health can still name what it
    // lists rather than naming a volume on it.
    assert_eq!(stats_records[0].device().as_str(), "mounted-blk");
    assert_eq!(queue_records[0].queue(), BlkQueueCounters::default());
    assert_eq!(queue_records[0].budget_depth(), budget.queue_depth);
    assert_eq!(queue_records[0].budget_deadline_ns(), budget.deadline_ns);

    // A surprise-removal state is authoritative: even with the overlay now
    // claiming the device is fine, the vanished state stands.
    cell.set_availability(handle, MountAvailability::UnavailableDirty)
        .expect("mark unavailable");
    availability.store(
        MountAvailability::Available.as_u8(),
        core::sync::atomic::Ordering::Relaxed,
    );
    assert_eq!(vol_availability(&svc), MountAvailability::UnavailableDirty);

    // Back to a live volume, the overlay's degraded reading shows through.
    cell.set_availability(handle, MountAvailability::Available)
        .expect("mark available");
    availability.store(
        MountAvailability::Degraded.as_u8(),
        core::sync::atomic::Ordering::Relaxed,
    );
    assert_eq!(vol_availability(&svc), MountAvailability::Degraded);
    // The volume-health query reflects the same degraded overlay.
    assert_eq!(
        svc.volume_io_health_snapshot()[0].availability(),
        MountAvailability::Degraded
    );
}

#[test]
fn rename_to_a_path_outside_the_mounted_volume_fails_closed() {
    // The destination resolves to a different, backing-less mount, so the
    // move never escapes the volume; it fails closed rather than fabricating
    // a cross-device move.
    let svc = ready();
    let caps = caps();
    let src = path("a");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &src,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");
    assert_eq!(
        svc.rename(
            TEST_UID,
            &caps,
            at(&src),
            at("/System/Applications/b"),
            RenameFlags::empty(),
            None,
        ),
        Err(Errno::NotFound)
    );
}

/// Build the service over `fs_driver` mounted at [`MOUNT`], with the test
/// principal installed — the harness the extended-attribute tests share.
fn attr_service<F>(fs_driver: F, read_only: bool) -> MountedFilesystemService<F>
where
    F: tairix_abi::driver::filesystem::FilesystemRead
        + tairix_abi::driver::filesystem::FilesystemWrite
        + tairix_abi::driver::filesystem::FilesystemSecurity
        + tairix_abi::driver::filesystem::FilesystemStats
        + tairix_abi::driver::filesystem::FilesystemAttrsProvider
        + Send
        + 'static,
{
    mounted(fs_driver, read_only, &[TEST_ACCOUNT]).service
}

/// Set/get/list/remove round-trip for an ordinary-namespace attribute, with
/// the absent-attribute answer distinct from an empty value.
#[test]
fn attrs_round_trip_through_the_service() {
    let caps = caps();
    let svc = attr_service(driver(), false);
    let create = OpenFlags::CREATE.union(OpenFlags::WRITE);
    let path = "/Storage/vol/f";
    open_path(&svc, TEST_UID, &caps, path, create).expect("create");

    attr_set_path(&svc, TEST_UID, &caps, path, b"user.comment", b"hi").expect("set");
    let mut value = [0u8; 8];
    assert_eq!(
        attr_get_path(&svc, TEST_UID, &caps, path, b"user.comment", &mut value),
        Ok(2)
    );
    assert_eq!(&value[..2], b"hi");

    // An empty value is stored and read back as zero bytes, not "absent".
    attr_set_path(&svc, TEST_UID, &caps, path, b"user.empty", b"").expect("set empty");
    assert_eq!(
        attr_get_path(&svc, TEST_UID, &caps, path, b"user.empty", &mut value),
        Ok(0)
    );

    // The listing yields the visible keys in stored order, then ends.
    let mut key = [0u8; 64];
    assert_eq!(
        attr_list_path(&svc, TEST_UID, &caps, path, 0, &mut key),
        Ok(Some(b"user.comment".len()))
    );
    assert_eq!(&key[..b"user.comment".len()], b"user.comment");
    assert_eq!(
        attr_list_path(&svc, TEST_UID, &caps, path, 1, &mut key),
        Ok(Some(b"user.empty".len()))
    );
    assert_eq!(
        attr_list_path(&svc, TEST_UID, &caps, path, 2, &mut key),
        Ok(None)
    );

    attr_remove_path(&svc, TEST_UID, &caps, path, b"user.comment").expect("remove");
    // Absence is the dedicated code, for get and for a second remove alike.
    assert_eq!(
        attr_get_path(&svc, TEST_UID, &caps, path, b"user.comment", &mut value),
        Err(Errno::NoData)
    );
    assert_eq!(
        attr_remove_path(&svc, TEST_UID, &caps, path, b"user.comment"),
        Err(Errno::NoData)
    );
}

/// Attribute reads need read permission and writes need write permission on
/// the node itself; a read-only mount refuses mutation outright.
#[test]
fn attr_access_follows_the_nodes_own_permissions() {
    // A second principal, in the identity table but with no rights over the
    // test principal's 0o600 file.
    const OTHER_UID: u32 = 1001;
    let mut builder = IdentityTableBuilder::new();
    builder.push_group(GroupRecord {
        gid: GroupId(TEST_GID),
    });
    builder.push_group(GroupRecord { gid: GroupId(1001) });
    builder.push_user(UserRecord {
        uid: UserId(TEST_UID),
        primary_gid: GroupId(TEST_GID),
        supplementary_gids: Vec::new(),
        capability_grants: CapabilitySet::empty(),
    });
    builder.push_user(UserRecord {
        uid: UserId(OTHER_UID),
        primary_gid: GroupId(1001),
        supplementary_gids: Vec::new(),
        capability_grants: CapabilitySet::empty(),
    });
    let table = builder.verify(&NullSink).expect("well-formed table");

    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    cell.install_vfs(vfs(false)).expect("install vfs");
    cell.register(
        DriverHandle::from_raw(9).expect("handle"),
        driver(),
        "vol",
        "memfs",
        [0u8; 16],
        None,
    )
    .expect("register");
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    identity.install(table).expect("identity");
    let svc = MountedFilesystemService::new(cell, identity);

    let caps = caps();
    let create = OpenFlags::CREATE.union(OpenFlags::WRITE);
    let path = "/Storage/vol/private";
    open_path(&svc, TEST_UID, &caps, path, create).expect("create");
    attr_set_path(&svc, TEST_UID, &caps, path, b"user.comment", b"hi").expect("owner sets");
    svc.set_mode(TEST_UID, &caps, at(path), 0o600, FinalLink::Follow)
        .expect("chmod");

    // The other principal can neither read nor write the node's attributes.
    let mut buf = [0u8; 8];
    assert_eq!(
        attr_get_path(&svc, OTHER_UID, &caps, path, b"user.comment", &mut buf),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        attr_list_path(&svc, OTHER_UID, &caps, path, 0, &mut buf),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        attr_set_path(&svc, OTHER_UID, &caps, path, b"user.comment", b"x"),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        attr_remove_path(&svc, OTHER_UID, &caps, path, b"user.comment"),
        Err(Errno::PermissionDenied)
    );
}

/// A read-only mount refuses attribute mutation but still serves reads.
#[test]
fn attr_mutation_on_a_read_only_mount_fails_closed() {
    let caps = caps();
    // Seed the file and its attribute directly on the driver: the mount is
    // read-only, so nothing can be created through the service.
    let mut fs = driver();
    let root = fs.root();
    let node = fs
        .create(root, b"f", NodeKind::RegularFile, &PLANTED)
        .expect("create");
    fs.set_attr(node, b"user.comment", b"hi").expect("seed");
    let svc = attr_service(fs, true);
    let path = "/Storage/vol/f";

    let mut buf = [0u8; 8];
    assert_eq!(
        attr_get_path(&svc, TEST_UID, &caps, path, b"user.comment", &mut buf),
        Ok(2)
    );
    assert_eq!(
        attr_set_path(&svc, TEST_UID, &caps, path, b"user.comment", b"x"),
        Err(Errno::ReadOnly)
    );
    assert_eq!(
        attr_remove_path(&svc, TEST_UID, &caps, path, b"user.comment"),
        Err(Errno::ReadOnly)
    );
}

/// The privileged namespaces are refused for every caller, and a stored
/// privileged key is omitted from the listing — its existence is never
/// revealed, not even as an index gap.
#[test]
fn privileged_namespaces_are_refused_and_hidden() {
    let caps = caps();
    let mut fs = driver();
    let root = fs.root();
    let node = fs
        .create(root, b"f", NodeKind::RegularFile, &PLANTED)
        .expect("create");
    // Stored out-of-band (as a privileged service one day would); the
    // syscall surface itself refuses to write these namespaces.
    fs.set_attr(node, b"system.hidden", b"s").expect("seed");
    fs.set_attr(node, b"user.visible", b"v").expect("seed");
    let svc = attr_service(fs, false);
    let path = "/Storage/vol/f";

    let mut buf = [0u8; 64];
    assert_eq!(
        attr_set_path(&svc, TEST_UID, &caps, path, b"system.hidden", b"x"),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        attr_get_path(&svc, TEST_UID, &caps, path, b"system.hidden", &mut buf),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        attr_remove_path(&svc, TEST_UID, &caps, path, b"trusted.x"),
        Err(Errno::PermissionDenied)
    );
    // The listing shows only the visible key: index 0 is the user key and
    // index 1 is already the end, with no gap betraying the hidden one.
    assert_eq!(
        attr_list_path(&svc, TEST_UID, &caps, path, 0, &mut buf),
        Ok(Some(b"user.visible".len()))
    );
    assert_eq!(&buf[..b"user.visible".len()], b"user.visible");
    assert_eq!(
        attr_list_path(&svc, TEST_UID, &caps, path, 1, &mut buf),
        Ok(None)
    );
}

/// Malformed keys and undersized buffers fail closed with their dedicated
/// codes; nothing is truncated or guessed.
#[test]
fn attr_key_grammar_and_buffers_fail_closed() {
    let caps = caps();
    let svc = attr_service(driver(), false);
    let create = OpenFlags::CREATE.union(OpenFlags::WRITE);
    let path = "/Storage/vol/f";
    open_path(&svc, TEST_UID, &caps, path, create).expect("create");
    attr_set_path(&svc, TEST_UID, &caps, path, b"user.comment", b"hello").expect("set");

    // No `namespace.rest` split, and an unknown namespace: both refused.
    assert_eq!(
        attr_set_path(&svc, TEST_UID, &caps, path, b"nodot", b"x"),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        attr_set_path(&svc, TEST_UID, &caps, path, b"bogus.key", b"x"),
        Err(Errno::OutOfRange)
    );
    // A value that does not fit is refused whole, never truncated.
    let mut tiny = [0u8; 2];
    assert_eq!(
        attr_get_path(&svc, TEST_UID, &caps, path, b"user.comment", &mut tiny),
        Err(Errno::BufferTooSmall)
    );
    let mut tiny_key = [0u8; 4];
    assert_eq!(
        attr_list_path(&svc, TEST_UID, &caps, path, 0, &mut tiny_key),
        Err(Errno::BufferTooSmall)
    );
    // A missing path is the path's own error, not an attribute answer.
    let mut buf = [0u8; 8];
    assert_eq!(
        attr_get_path(
            &svc,
            TEST_UID,
            &caps,
            "/Storage/vol/absent",
            b"user.x",
            &mut buf
        ),
        Err(Errno::NotFound)
    );
}

use tairix_abi::driver::filesystem::{
    DirEntry, FilesystemAttrsProvider, FilesystemSecurity, FilesystemStats, NodeId, NodeInfo,
    VolumeStats,
};
use tairix_abi::DriverError;

/// [`RwMockFs`] minus its attribute store: the facet keeps its default
/// `None` answer, standing in for a FAT32/ext4-class mount.
struct NoAttrsFs(RwMockFs);
impl FilesystemRead for NoAttrsFs {
    fn root(&self) -> NodeId {
        self.0.root()
    }

    fn name_matching(&self) -> tairix_abi::driver::filesystem::NameMatching {
        self.0.name_matching()
    }

    fn node_identity(&self) -> tairix_abi::driver::filesystem::NodeIdentity {
        tairix_abi::driver::filesystem::NodeIdentity::Stable
    }
    fn node_info(&mut self, node: NodeId) -> Result<NodeInfo, DriverError> {
        self.0.node_info(node)
    }
    fn lookup(&mut self, dir: NodeId, name: &[u8]) -> Result<NodeId, DriverError> {
        self.0.lookup(dir, name)
    }
    fn parent(&mut self, dir: NodeId) -> Result<NodeId, DriverError> {
        self.0.parent(dir)
    }
    fn read_at(&mut self, file: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize, DriverError> {
        self.0.read_at(file, offset, buf)
    }
    fn read_dir(
        &mut self,
        dir: NodeId,
        cursor: u64,
        after: &[u8],
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError> {
        self.0.read_dir(dir, cursor, after, visit)
    }
    fn pin(&mut self, node: NodeId) -> Result<NodeId, DriverError> {
        self.0.pin(node)
    }
    fn unpin(
        &mut self,
        pinned: NodeId,
    ) -> Result<tairix_abi::driver::filesystem::Unpinned, DriverError> {
        self.0.unpin(pinned)
    }
    fn release_pins(&mut self) -> Result<(), DriverError> {
        self.0.release_pins()
    }
}
impl FilesystemWrite for NoAttrsFs {
    fn create(
        &mut self,
        dir: NodeId,
        name: &[u8],
        kind: NodeKind,
        record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        self.0.create(dir, name, kind, record)
    }
    fn write_at(&mut self, file: NodeId, offset: u64, data: &[u8]) -> Result<usize, DriverError> {
        self.0.write_at(file, offset, data)
    }
    fn truncate(&mut self, file: NodeId, size: u64) -> Result<(), DriverError> {
        self.0.truncate(file, size)
    }
    fn remove(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError> {
        self.0.remove(dir, name)
    }
    fn rename(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        self.0.rename(src_dir, src_name, dst_dir, dst_name)
    }
    fn flush(&mut self) -> Result<(), DriverError> {
        self.0.flush()
    }
}
impl FilesystemSecurity for NoAttrsFs {
    fn security(
        &mut self,
        node: NodeId,
    ) -> Result<tairix_abi::driver::filesystem::NodeSecurity, DriverError> {
        self.0.security(node)
    }
    fn set_security(
        &mut self,
        node: NodeId,
        security: tairix_abi::driver::filesystem::NodeSecurity,
    ) -> Result<(), DriverError> {
        self.0.set_security(node, security)
    }
}
impl FilesystemStats for NoAttrsFs {
    fn stats(&mut self) -> Result<VolumeStats, DriverError> {
        self.0.stats()
    }
}
impl FilesystemAttrsProvider for NoAttrsFs {}

/// A driver whose format stores no attributes answers every attribute call
/// with the typed unsupported-backing refusal, decided per mount through
/// the attribute facet.
#[test]
fn attrs_on_an_unsupporting_backing_are_a_typed_refusal() {
    let caps = caps();
    let svc = attr_service(NoAttrsFs(driver()), false);
    let create = OpenFlags::CREATE.union(OpenFlags::WRITE);
    let path = "/Storage/vol/f";
    open_path(&svc, TEST_UID, &caps, path, create).expect("create");

    let mut buf = [0u8; 8];
    assert_eq!(
        attr_get_path(&svc, TEST_UID, &caps, path, b"user.x", &mut buf),
        Err(Errno::NotSupported)
    );
    assert_eq!(
        attr_set_path(&svc, TEST_UID, &caps, path, b"user.x", b"v"),
        Err(Errno::NotSupported)
    );
    assert_eq!(
        attr_list_path(&svc, TEST_UID, &caps, path, 0, &mut buf),
        Err(Errno::NotSupported)
    );
    assert_eq!(
        attr_remove_path(&svc, TEST_UID, &caps, path, b"user.x"),
        Err(Errno::NotSupported)
    );
}

// --- symbolic links at the service seam --------------------------------
//
// The VFS's own resolution matrix lives in `delegate_tests`; what these
// cases pin down is the *service*: the two new operations, and the follow
// posture an open fixes travelling with its descriptor so a later stat or
// listing cannot contradict it.

#[test]
fn symlink_then_readlink_round_trips_the_stored_target() {
    let svc = ready();
    let caps = caps();
    let link = path("alias");

    svc.symlink(TEST_UID, &caps, "/target/name", at(&link))
        .expect("create the link");
    assert_eq!(
        svc.readlink(TEST_UID, &caps, at(&link)),
        Ok(alloc::string::String::from("/target/name"))
    );
    // A second link of the same name is refused, not silently replaced.
    assert_eq!(
        svc.symlink(TEST_UID, &caps, "/other", at(&link)),
        Err(Errno::AlreadyExists)
    );
}

#[test]
fn readlink_of_a_non_link_fails_closed() {
    let svc = ready();
    let caps = caps();
    let file = path("plain");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &file,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");

    assert_eq!(
        svc.readlink(TEST_UID, &caps, at(&file)),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        svc.readlink(TEST_UID, &caps, at(&path("absent"))),
        Err(Errno::NotFound)
    );
}

#[test]
fn symlink_on_a_read_only_mount_is_refused() {
    let svc = service(true, true, true);
    let caps = caps();
    assert_eq!(
        svc.symlink(TEST_UID, &caps, "/target", at(&path("alias"))),
        Err(Errno::ReadOnly)
    );
}

#[test]
fn stat_describes_the_link_under_keep_and_its_target_under_follow() {
    let svc = ready();
    let caps = caps();
    let file = path("real");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &file,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");
    write_path(&svc, TEST_UID, &caps, &file, 0, false, b"1234").expect("write");
    let link = path("alias");
    let target = alloc::format!("/{}", "real");
    svc.symlink(TEST_UID, &caps, &target, at(&link))
        .expect("create the link");

    let kept = svc
        .stat(TEST_UID, &caps, at(&link), FinalLink::Keep)
        .expect("lstat");
    assert_eq!(kept.kind, FileKind::Symlink);
    assert_eq!(kept.size, target.len() as u64);

    let followed = svc
        .stat(TEST_UID, &caps, at(&link), FinalLink::Follow)
        .expect("stat");
    assert_eq!(followed.kind, FileKind::Regular);
    assert_eq!(followed.size, 4);
}

#[test]
fn an_open_asking_for_bytes_of_a_link_it_may_not_follow_is_refused() {
    let svc = ready();
    let caps = caps();
    let link = path("alias");
    svc.symlink(TEST_UID, &caps, "/nowhere", at(&link))
        .expect("create the link");

    // A link stores a path, not bytes: `NO_FOLLOW` plus byte access over one
    // that really is a link is the documented `LinkLoop` refusal.
    for flags in [
        OpenFlags::NO_FOLLOW.union(OpenFlags::READ),
        OpenFlags::NO_FOLLOW.union(OpenFlags::WRITE),
    ] {
        assert_eq!(
            open_path(&svc, TEST_UID, &caps, &link, flags),
            Err(Errno::LinkLoop),
            "{flags:?} over a link must not yield a byte handle"
        );
    }
    // The resolve-only handle — the `lstat` posture — is what succeeds, even
    // though the target does not exist.
    open_path(&svc, TEST_UID, &caps, &link, OpenFlags::NO_FOLLOW)
        .expect("a resolve-only handle on the link itself");
}

#[test]
fn no_follow_does_not_disturb_an_open_whose_final_component_is_not_a_link() {
    // The flag combination is legal because the final component usually is
    // not a link; only a real one refuses.
    let svc = ready();
    let caps = caps();
    let file = path("plain");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &file,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");

    open_path(
        &svc,
        TEST_UID,
        &caps,
        &file,
        OpenFlags::NO_FOLLOW.union(OpenFlags::READ),
    )
    .expect("a regular file reads fine under NO_FOLLOW");
}

#[test]
fn an_open_that_follows_a_link_to_a_directory_still_refuses_byte_access() {
    let svc = ready();
    let caps = caps();
    svc.mkdir(TEST_UID, &caps, at(&path("dir")), new_dir())
        .expect("mkdir");
    let link = path("todir");
    svc.symlink(TEST_UID, &caps, "/dir", at(&link))
        .expect("create the link");

    // Following reaches the directory, so the ordinary "no byte access to a
    // directory" rule applies to what the link names.
    assert_eq!(
        open_path(&svc, TEST_UID, &caps, &link, OpenFlags::READ),
        Err(Errno::IsADirectory)
    );
    // And a directory open through the link succeeds, because it is one.
    open_path(&svc, TEST_UID, &caps, &link, OpenFlags::DIRECTORY)
        .expect("a directory open follows the link");
    // Kept, the same name is not a directory at all.
    assert_eq!(
        open_path(
            &svc,
            TEST_UID,
            &caps,
            &link,
            OpenFlags::DIRECTORY.union(OpenFlags::NO_FOLLOW)
        ),
        Err(Errno::NotADirectory)
    );
}

#[test]
fn readdir_under_keep_refuses_a_link_rather_than_listing_its_target() {
    let svc = ready();
    let caps = caps();
    svc.mkdir(TEST_UID, &caps, at(&path("dir")), new_dir())
        .expect("mkdir");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &path("dir/inside"),
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create inside");
    let link = path("todir");
    svc.symlink(TEST_UID, &caps, "/dir", at(&link))
        .expect("create the link");

    let followed = svc
        .readdir_bounded(TEST_UID, &caps, at(&link), FinalLink::Follow, usize::MAX)
        .expect("following lists the target");
    assert!(followed.iter().any(|e| e.name == "inside"), "{followed:?}");
    assert_eq!(
        svc.readdir_bounded(TEST_UID, &caps, at(&link), FinalLink::Keep, usize::MAX),
        Err(Errno::NotADirectory)
    );
}

#[test]
fn readdir_reports_a_link_entry_as_a_link_never_as_its_target() {
    let svc = ready();
    let caps = caps();
    svc.mkdir(TEST_UID, &caps, at(&path("dir")), new_dir())
        .expect("mkdir");
    svc.symlink(TEST_UID, &caps, "/dir", at(&path("todir")))
        .expect("create the link");

    let mut entries = svc
        .readdir_bounded(TEST_UID, &caps, at(MOUNT), FinalLink::Follow, usize::MAX)
        .expect("readdir");
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let kinds: Vec<(FileKind, &str)> = entries.iter().map(|e| (e.kind, e.name.as_str())).collect();
    assert_eq!(
        kinds,
        alloc::vec![(FileKind::Directory, "dir"), (FileKind::Symlink, "todir")]
    );
}

#[test]
fn an_uninstalled_mount_refuses_both_link_operations() {
    let svc = service(false, true, false);
    let caps = caps();
    assert_eq!(
        svc.symlink(TEST_UID, &caps, "/t", at(&path("alias"))),
        Err(Errno::NotImplemented)
    );
    assert_eq!(
        svc.readlink(TEST_UID, &caps, at(&path("alias"))),
        Err(Errno::NotImplemented)
    );
}

#[test]
fn an_open_creating_through_a_dangling_link_creates_the_target() {
    // The `O_CREAT` half of "a write follows a final link": the create lands
    // where the link points, so the link stops dangling rather than the open
    // reporting the link's own name as already taken.
    let svc = ready();
    let caps = caps();
    let link = path("alias");
    svc.symlink(TEST_UID, &caps, "/made-later", at(&link))
        .expect("create the link");

    open_path(
        &svc,
        TEST_UID,
        &caps,
        &link,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create through the dangling link");

    let target = svc
        .stat(TEST_UID, &caps, at(&path("made-later")), FinalLink::Keep)
        .expect("the target was created");
    assert_eq!(target.kind, FileKind::Regular);
    // The link itself is untouched and now resolves.
    assert_eq!(
        svc.stat(TEST_UID, &caps, at(&link), FinalLink::Keep)
            .expect("the link survived")
            .kind,
        FileKind::Symlink
    );
    assert_eq!(
        svc.stat(TEST_UID, &caps, at(&link), FinalLink::Follow)
            .expect("the link resolves")
            .kind,
        FileKind::Regular
    );
}

/// An exclusive create never follows a final link, dangling or live: the
/// name is taken, as POSIX requires, so a link planted where a program
/// creates exclusively cannot turn the create onto the file it names.
///
/// Regression: the open followed the link and made its target.
#[test]
fn an_exclusive_create_refuses_a_final_link_rather_than_following_it() {
    let svc = ready();
    let caps = caps();
    let link = path("alias");
    svc.symlink(TEST_UID, &caps, "/made-later", at(&link))
        .expect("create the link");
    let made = || svc.stat(TEST_UID, &caps, at(&path("made-later")), FinalLink::Keep);
    let exclusive = OpenFlags::CREATE
        .union(OpenFlags::EXCLUSIVE)
        .union(OpenFlags::WRITE);
    assert_eq!(
        open_path(&svc, TEST_UID, &caps, &link, exclusive),
        Err(Errno::AlreadyExists)
    );
    assert_eq!(made().err(), Some(Errno::NotFound));
    let directory = OpenFlags::CREATE
        .union(OpenFlags::DIRECTORY)
        .union(OpenFlags::EXCLUSIVE);
    assert_eq!(
        svc.open(TEST_UID, &caps, at(&link), directory, new_dir())
            .err(),
        Some(Errno::AlreadyExists)
    );
    assert_eq!(made().err(), Some(Errno::NotFound));
}

#[test]
fn writes_through_a_link_reach_the_target_and_leave_the_link_intact() {
    let svc = ready();
    let caps = caps();
    let file = path("real");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &file,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");
    let link = path("alias");
    svc.symlink(TEST_UID, &caps, "/real", at(&link))
        .expect("create the link");

    assert_eq!(
        write_path(&svc, TEST_UID, &caps, &link, 0, false, b"payload"),
        Ok(7)
    );
    let mut buf = [0u8; 16];
    assert_eq!(read_path(&svc, TEST_UID, &caps, &file, 0, &mut buf), Ok(7));
    assert_eq!(&buf[..7], b"payload");
    // An append resolves the size through the link too.
    assert_eq!(
        write_path(&svc, TEST_UID, &caps, &link, 0, true, b"!"),
        Ok(1)
    );
    assert_eq!(
        svc.stat(TEST_UID, &caps, at(&file), FinalLink::Follow)
            .expect("stat the target")
            .size,
        8
    );
    // Truncate-on-open reaches the target, not the link.
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &link,
        OpenFlags::WRITE.union(OpenFlags::TRUNCATE),
    )
    .expect("open the link for truncation");
    assert_eq!(
        svc.stat(TEST_UID, &caps, at(&file), FinalLink::Follow)
            .expect("stat the target")
            .size,
        0
    );
    assert_eq!(
        svc.readlink(TEST_UID, &caps, at(&link)),
        Ok(alloc::string::String::from("/real"))
    );
}

#[test]
fn mkdir_over_a_link_reports_it_as_already_existing() {
    let svc = ready();
    let caps = caps();
    let link = path("alias");
    svc.symlink(TEST_UID, &caps, "/absent", at(&link))
        .expect("create the link");

    assert_eq!(
        svc.mkdir(TEST_UID, &caps, at(&link), new_dir()),
        Err(Errno::AlreadyExists)
    );
    assert_eq!(
        svc.stat(TEST_UID, &caps, at(&path("absent")), FinalLink::Keep),
        Err(Errno::NotFound)
    );
}

/// A symbolic link stored inside a **sub-mount** cannot resolve to a node
/// the mount does not project: an absolute target names the mount's own
/// root, and `..` cannot climb above it.
///
/// The volume carries a decoy `inside` at its own root and the real one
/// inside the projected subtree, so each link's resolution names exactly one
/// of them and the assertion distinguishes a clamped walk from a walk floored
/// at the driver root.
#[test]
fn a_link_in_a_submount_cannot_resolve_outside_the_projected_subtree() {
    let h = DriverHandle::from_raw(9).expect("handle");

    let mut volume = driver();
    let vroot = volume.root();
    // The decoy: same name, at the volume root, outside what the mount
    // projects.
    let decoy = volume
        .create(vroot, b"inside", NodeKind::RegularFile, &PLANTED)
        .expect("decoy");
    let area = volume
        .create(vroot, b"Area", NodeKind::Directory, &PLANTED)
        .expect("subtree");
    let inside = volume
        .create(area, b"inside", NodeKind::RegularFile, &PLANTED)
        .expect("projected file");
    volume
        .create_link(area, b"abs", b"/inside", &PLANTED)
        .expect("absolute link");
    volume
        .create_link(area, b"up", b"../inside", &PLANTED)
        .expect("ascending link");
    volume.write_at(decoy, 0, b"V").expect("decoy contents");
    volume
        .write_at(inside, 0, b"I")
        .expect("projected contents");

    let mut vfs = Vfs::with_default_layout(UserId(TEST_UID), GroupId(TEST_GID));
    let caps = caps();
    let cred = Credentials {
        uid: UserId(TEST_UID),
        gid: GroupId(TEST_GID),
        supplementary_gids: &[],
        caps: &caps,
    };
    vfs.mkdir(
        &cred,
        &Path::parse("/Users/area").expect("path"),
        Mode::from_bits(0o755),
    )
    .expect("mount point");
    vfs.mounts_write()
        .attach(Attachment::new(h, None, MountFlags::default()))
        .expect("attach");
    users_projection(&vfs);
    vfs.mounts_write()
        .mount_rebased(
            Path::parse("/Users/area").expect("path"),
            MountFlags::NOEXEC,
            Some(h),
            alloc::vec![alloc::string::String::from("Area")],
        )
        .expect("rebased mount");

    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    cell.install_vfs(vfs).expect("install vfs");
    cell.register(h, volume, "vol", "memfs", [0u8; 16], None)
        .expect("register");
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    identity.install(identity_table()).expect("identity");
    let svc = MountedFilesystemService::new(cell, identity);

    let mut buf = [0u8; 1];
    // An absolute target names the mount's own root, so it reaches the
    // projected file rather than the volume-root decoy.
    assert_eq!(
        read_path(&svc, TEST_UID, &caps, "/Users/area/abs", 0, &mut buf),
        Ok(1)
    );
    assert_eq!(&buf, b"I");
    // `..` is floored at the projected root, so ascending from the subtree
    // stays inside it.
    assert_eq!(
        read_path(&svc, TEST_UID, &caps, "/Users/area/up", 0, &mut buf),
        Ok(1)
    );
    assert_eq!(&buf, b"I");
}

// --- canonicalisation at the service seam ------------------------------
//
// The resolution matrix itself is the VFS's (`delegate_tests`); what these
// cases pin down is the answer's *spelling* — a `/`-view path in the
// caller's own namespace, mount point included — and the three readings of
// how much of a path must exist.

#[test]
fn realpath_resolves_every_link_and_reports_a_view_path() {
    let svc = ready();
    let caps = caps();
    let create = OpenFlags::CREATE.union(OpenFlags::WRITE);

    svc.mkdir(TEST_UID, &caps, at(&path("dir")), new_dir())
        .expect("dir");
    open_path(&svc, TEST_UID, &caps, &path("dir/file"), create).expect("file");
    // An interior link and a final link, so one answer proves both are
    // followed.
    svc.symlink(TEST_UID, &caps, "/dir", at(&path("d")))
        .expect("interior link");
    svc.symlink(TEST_UID, &caps, "file", at(&path("dir/f")))
        .expect("final link");

    let canonical = alloc::format!("{MOUNT}/dir/file");
    for spelling in ["dir/file", "d/file", "dir/f", "d/f"] {
        assert_eq!(
            svc.realpath(TEST_UID, &caps, at(&path(spelling)), RealpathMode::Existing),
            Ok(canonical.clone()),
            "canonicalising {spelling}"
        );
    }
    // The mount point itself is its own canonical name.
    assert_eq!(
        svc.realpath(TEST_UID, &caps, at(MOUNT), RealpathMode::Existing),
        Ok(alloc::string::String::from(MOUNT))
    );
}

#[test]
fn realpath_resolves_dot_dot_through_a_link_physically() {
    let svc = ready();
    let caps = caps();
    let create = OpenFlags::CREATE.union(OpenFlags::WRITE);

    // A caller may not spell `..` — the path grammar refuses it — so the
    // only `..` a resolution ever meets comes from a link's stored target.
    // `here/link` names `/there`, and `/there/up` stores `../sibling`: the
    // `..` must pop the directory the walk really came through (`/there`),
    // reaching the real `/sibling`, never the `/here/sibling` decoy a
    // lexical collapse of `here/link/..` would name. Both exist, so a
    // lexical answer would look plausible.
    svc.mkdir(TEST_UID, &caps, at(&path("here")), new_dir())
        .expect("here");
    svc.mkdir(TEST_UID, &caps, at(&path("there")), new_dir())
        .expect("there");
    open_path(&svc, TEST_UID, &caps, &path("sibling"), create).expect("real sibling");
    open_path(&svc, TEST_UID, &caps, &path("here/sibling"), create).expect("decoy sibling");
    svc.symlink(TEST_UID, &caps, "/there", at(&path("here/link")))
        .expect("link");
    svc.symlink(TEST_UID, &caps, "../sibling", at(&path("there/up")))
        .expect("ascending link");

    assert_eq!(
        svc.realpath(
            TEST_UID,
            &caps,
            at(&path("here/link/up")),
            RealpathMode::Existing
        ),
        Ok(alloc::format!("{MOUNT}/sibling"))
    );
}

#[test]
fn realpath_modes_decide_how_much_of_the_path_must_exist() {
    let svc = ready();
    let caps = caps();

    svc.mkdir(TEST_UID, &caps, at(&path("dir")), new_dir())
        .expect("dir");

    // A vacant final name: refused by `-e`, named by `-f` and `-m`.
    let vacant = path("dir/absent");
    let vacant_canonical = alloc::format!("{MOUNT}/dir/absent");
    assert_eq!(
        svc.realpath(TEST_UID, &caps, at(&vacant), RealpathMode::Existing),
        Err(Errno::NotFound)
    );
    for mode in [RealpathMode::Final, RealpathMode::Missing] {
        assert_eq!(
            svc.realpath(TEST_UID, &caps, at(&vacant), mode),
            Ok(vacant_canonical.clone())
        );
    }

    // A missing *ancestor*: only `-m` names it.
    let deep = path("gone/away/leaf");
    for mode in [RealpathMode::Existing, RealpathMode::Final] {
        assert_eq!(
            svc.realpath(TEST_UID, &caps, at(&deep), mode),
            Err(Errno::NotFound)
        );
    }
    assert_eq!(
        svc.realpath(TEST_UID, &caps, at(&deep), RealpathMode::Missing),
        Ok(alloc::format!("{MOUNT}/gone/away/leaf"))
    );
}

#[test]
fn realpath_of_a_dangling_link_follows_its_target() {
    let svc = ready();
    let caps = caps();

    svc.mkdir(TEST_UID, &caps, at(&path("dir")), new_dir())
        .expect("dir");
    // A dangling link whose parent exists: `-f` names the target, `-e`
    // refuses it. The link's own name is never the answer.
    svc.symlink(TEST_UID, &caps, "/dir/nothing", at(&path("dangling")))
        .expect("dangling link");
    let link = path("dangling");
    assert_eq!(
        svc.realpath(TEST_UID, &caps, at(&link), RealpathMode::Existing),
        Err(Errno::NotFound)
    );
    assert_eq!(
        svc.realpath(TEST_UID, &caps, at(&link), RealpathMode::Final),
        Ok(alloc::format!("{MOUNT}/dir/nothing"))
    );

    // A dangling link whose target's *parent* is absent too: `-f` refuses
    // it (only the last component may be missing), `-m` names it.
    svc.symlink(TEST_UID, &caps, "/gone/leaf", at(&path("deeper")))
        .expect("deeper link");
    let deeper = path("deeper");
    assert_eq!(
        svc.realpath(TEST_UID, &caps, at(&deeper), RealpathMode::Final),
        Err(Errno::NotFound)
    );
    assert_eq!(
        svc.realpath(TEST_UID, &caps, at(&deeper), RealpathMode::Missing),
        Ok(alloc::format!("{MOUNT}/gone/leaf"))
    );
}

#[test]
fn realpath_refuses_a_cycle_and_needs_search_permission_on_the_way() {
    let svc = ready();
    let caps = caps();

    svc.symlink(TEST_UID, &caps, "/loop", at(&path("loop")))
        .expect("self-cycle");
    for mode in [
        RealpathMode::Existing,
        RealpathMode::Final,
        RealpathMode::Missing,
    ] {
        assert_eq!(
            svc.realpath(TEST_UID, &caps, at(&path("loop")), mode),
            Err(Errno::LinkLoop),
            "a cycle is refused under every reading"
        );
    }

    // A directory nobody may search — its owner included, since TAIRiX has
    // no ambient root — hides what is under it from canonicalisation exactly
    // as it does from a read. Asked under the most permissive reading, so it
    // is the permission check refusing and not the absence.
    svc.mkdir(TEST_UID, &caps, at(&path("closed")), new_dir())
        .expect("closed");
    svc.set_mode(
        TEST_UID,
        &caps,
        at(&path("closed")),
        0o600,
        FinalLink::Follow,
    )
    .expect("drop the search bit");
    assert_eq!(
        svc.realpath(
            TEST_UID,
            &caps,
            at(&path("closed/inside")),
            RealpathMode::Missing
        ),
        Err(Errno::PermissionDenied)
    );
}

#[test]
fn realpath_answers_a_submount_with_its_own_mount_point() {
    // A sub-mount projects a subtree, so a canonical path names the mount
    // point rather than the subtree's path on the backing volume — and a
    // link inside it that tries to escape resolves within the projection.
    let h = DriverHandle::from_raw(9).expect("handle");
    let mut volume = driver();
    let vroot = volume.root();
    let area = volume
        .create(vroot, b"Area", NodeKind::Directory, &PLANTED)
        .expect("subtree");
    volume
        .create(area, b"inside", NodeKind::RegularFile, &PLANTED)
        .expect("file");
    volume
        .create_link(area, b"up", b"../inside", &PLANTED)
        .expect("ascending link");

    let mut vfs = Vfs::with_default_layout(UserId(TEST_UID), GroupId(TEST_GID));
    let caps = caps();
    let cred = Credentials {
        uid: UserId(TEST_UID),
        gid: GroupId(TEST_GID),
        supplementary_gids: &[],
        caps: &caps,
    };
    vfs.mkdir(
        &cred,
        &Path::parse("/Users/area").expect("path"),
        Mode::from_bits(0o755),
    )
    .expect("mount point");
    vfs.mounts_write()
        .attach(Attachment::new(h, None, MountFlags::default()))
        .expect("attach");
    users_projection(&vfs);
    vfs.mounts_write()
        .mount_rebased(
            Path::parse("/Users/area").expect("path"),
            MountFlags::NOEXEC,
            Some(h),
            alloc::vec![alloc::string::String::from("Area")],
        )
        .expect("rebased mount");
    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    cell.install_vfs(vfs).expect("install vfs");
    cell.register(h, volume, "vol", "memfs", [0u8; 16], None)
        .expect("register");
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    identity.install(identity_table()).expect("identity");
    let svc = MountedFilesystemService::new(cell, identity);

    let canonical = alloc::string::String::from("/Users/area/inside");
    assert_eq!(
        svc.realpath(
            TEST_UID,
            &caps,
            at("/Users/area/inside"),
            RealpathMode::Existing
        ),
        Ok(canonical.clone())
    );
    assert_eq!(
        svc.realpath(
            TEST_UID,
            &caps,
            at("/Users/area/up"),
            RealpathMode::Existing
        ),
        Ok(canonical)
    );
}

mod watch {
    use super::*;
    use crate::fs::test_volume::{watched_service, Watched};
    use crate::fswatch::Drain;

    fn create(svc: &impl FilesystemService, path: &str) {
        open_path(
            svc,
            TEST_UID,
            &caps(),
            path,
            OpenFlags::CREATE.union(OpenFlags::WRITE),
        )
        .expect("create");
    }

    /// A pin reports to its own registration's watch table: once the volume
    /// has gone and its identity is claimed afresh, the old registration's
    /// pin reaches no table, never the new one that shares its identity.
    #[test]
    fn a_pins_watch_table_is_its_own_registrations() {
        let volume = [0x74; 16];
        let Watched {
            service: svc,
            watch,
            registry,
            mounted,
        } = watched_service(volume);
        create(svc, "/Storage/vol/f");
        let old =
            own_handle(svc, TEST_UID, &caps(), "/Storage/vol/f", OpenFlags::READ).expect("opens");
        assert!(svc
            .watch_table(&old)
            .is_some_and(|table| Arc::ptr_eq(&table, &watch)));
        let handle = DriverHandle::from_raw(9).expect("non-zero handle");
        drop(mounted.unregister(handle).expect("registered"));
        let claim = registry
            .claim(
                volume,
                tairix_abi::driver::filesystem::NameMatching::Exact,
                crate::test_pressure::unpressured(),
            )
            .expect("the identity is claimed afresh");
        assert!(
            svc.watch_table(&old).is_none(),
            "the old registration's pin reports to no table"
        );
        drop(claim);
        close(svc, old);
    }

    /// A directory made through the service is reported by name, costing its
    /// parent's watchers no rescan, while a change of the new directory's own
    /// mode makes its watchers rescan.
    #[test]
    fn a_mkdir_reports_its_name_and_a_mode_change_rescans_its_own_watchers() {
        let Watched {
            service: svc,
            watch,
            ..
        } = watched_service([0x73; 16]);
        let node_of = |path: &str| {
            svc.stat(TEST_UID, &caps(), at(path), FinalLink::Follow)
                .expect("stats")
                .id
                .node
        };
        let root = node_of(MOUNT);
        let parent = watch.watch(root, 0).expect("arms").0;
        svc.mkdir(TEST_UID, &caps(), at("/Storage/vol/made"), new_dir())
            .expect("mkdir");
        let made = node_of("/Storage/vol/made");
        let inner = watch.watch(made, 0).expect("arms").0;
        assert!(matches!(
            watch.take(root, parent, usize::MAX, false),
            Ok(Some(Drain::Names { .. }))
        ));
        svc.set_mode(
            TEST_UID,
            &caps(),
            at("/Storage/vol/made"),
            0o700,
            FinalLink::Follow,
        )
        .expect("chmod");
        assert!(matches!(
            watch.take(made, inner, usize::MAX, false),
            Ok(Some(Drain::Rescan { .. }))
        ));
    }

    #[test]
    fn lookup_entries_reports_what_readdir_does_for_those_names() {
        let svc = watched_service([0x70; 16]).service;
        create(svc, "/Storage/vol/a.txt");
        write_path(
            svc,
            TEST_UID,
            &caps(),
            "/Storage/vol/a.txt",
            0,
            false,
            b"moo",
        )
        .expect("write");
        svc.mkdir(TEST_UID, &caps(), at("/Storage/vol/d"), new_dir())
            .expect("mkdir");
        let listing = svc
            .readdir_bounded(TEST_UID, &caps(), at(MOUNT), FinalLink::Follow, usize::MAX)
            .expect("lists");
        let names: [&[u8]; 3] = [b"a.txt", b"d", b"absent"];
        let found = svc
            .lookup_entries(TEST_UID, &caps(), at(MOUNT), FinalLink::Follow, &names)
            .expect("looks up");
        let listed = |name: &str| listing.iter().find(|e| e.name == name).cloned();
        assert_eq!(found.entries[0], listed("a.txt"));
        assert_eq!(found.entries[1], listed("d"));
        assert_eq!(found.entries[2], None);
        let stat = svc
            .stat(TEST_UID, &caps(), at(MOUNT), FinalLink::Follow)
            .expect("stats");
        assert_eq!(
            found.dir, stat.id,
            "the directory's own identity comes back"
        );
    }

    #[test]
    fn lookup_entries_is_refused_where_a_listing_would_be() {
        let svc = watched_service([0x71; 16]).service;
        svc.mkdir(TEST_UID, &caps(), at("/Storage/vol/private"), new_dir())
            .expect("mkdir");
        svc.set_mode(
            TEST_UID,
            &caps(),
            at("/Storage/vol/private"),
            0o300,
            FinalLink::Follow,
        )
        .expect("chmod");
        assert_eq!(
            svc.lookup_entries(
                TEST_UID,
                &caps(),
                at("/Storage/vol/private"),
                FinalLink::Follow,
                &[]
            )
            .map(|_| ()),
            Err(Errno::PermissionDenied)
        );
        create(svc, "/Storage/vol/file");
        assert_eq!(
            svc.lookup_entries(
                TEST_UID,
                &caps(),
                at("/Storage/vol/file"),
                FinalLink::Follow,
                &[]
            )
            .map(|_| ()),
            Err(Errno::NotADirectory)
        );
    }

    #[test]
    fn every_kind_of_mutation_through_the_service_reaches_the_journal() {
        let Watched {
            service: svc,
            watch,
            ..
        } = watched_service([0x72; 16]);
        let dir = svc
            .stat(TEST_UID, &caps(), at(MOUNT), FinalLink::Follow)
            .expect("stats")
            .id;
        let watcher = watch.watch(dir.node, 0).expect("arms").0;
        let drained = || match watch
            .take(dir.node, watcher, usize::MAX, false)
            .expect("copies out")
        {
            Some(Drain::Names { names, upto, .. }) => {
                watch.commit(dir.node, watcher, upto, 0);
                names
                    .iter()
                    .map(|n| alloc::string::String::from_utf8(n.to_vec()).expect("utf-8"))
                    .collect::<Vec<_>>()
            }
            _ => panic!("expected names"),
        };
        create(svc, "/Storage/vol/new.txt");
        assert_eq!(drained(), ["new.txt"]);
        write_path(
            svc,
            TEST_UID,
            &caps(),
            "/Storage/vol/new.txt",
            0,
            true,
            b"line\n",
        )
        .expect("append");
        assert_eq!(drained(), ["new.txt"]);
        svc.set_mode(
            TEST_UID,
            &caps(),
            at("/Storage/vol/new.txt"),
            0o600,
            FinalLink::Follow,
        )
        .expect("chmod");
        assert_eq!(drained(), ["new.txt"], "a metadata change is reported too");
        svc.rename(
            TEST_UID,
            &caps(),
            at("/Storage/vol/new.txt"),
            at("/Storage/vol/old.txt"),
            RenameFlags::empty(),
            None,
        )
        .expect("rename");
        assert_eq!(drained(), ["new.txt", "old.txt"]);
        svc.mkdir(TEST_UID, &caps(), at("/Storage/vol/sub"), new_dir())
            .expect("mkdir");
        assert_eq!(drained(), ["sub"]);
        create(svc, "/Storage/vol/sub/inner");
        assert_eq!(drained(), ["sub"], "the subfolder's own entry changed");
        svc.unlink(
            TEST_UID,
            &caps(),
            at("/Storage/vol/old.txt"),
            UnlinkFlags::empty(),
            None,
        )
        .expect("unlink");
        assert_eq!(drained(), ["old.txt"]);
    }
}

/// A program is read only from a mount that runs programs, judged on the mount
/// the bytes come from: a link resolves inside its own mount, so one on the
/// root volume cannot reach the `noexec` stick, and one on the stick is still
/// refused. Reading the same files as data is unaffected.
///
/// Regression: no read a spawn made consulted `noexec`.
#[test]
fn a_program_is_read_only_from_a_mount_that_runs_programs() {
    let h_root = DriverHandle::from_raw(31).expect("handle");
    let h_stick = DriverHandle::from_raw(32).expect("handle");
    let mut root = driver();
    let root_dir = root.root();
    let run = root
        .create(root_dir, b"run", NodeKind::RegularFile, &PLANTED)
        .expect("run");
    root.write_at(run, 0, b"R").expect("contents");
    root.create_link(root_dir, b"onto", b"/Storage/stick/run", &PLANTED)
        .expect("link");
    let mut stick = driver();
    let stick_dir = stick.root();
    let run = stick
        .create(stick_dir, b"run", NodeKind::RegularFile, &PLANTED)
        .expect("run");
    stick.write_at(run, 0, b"S").expect("contents");
    stick
        .create_link(stick_dir, b"hop", b"/run", &PLANTED)
        .expect("link");

    let vfs = Vfs::with_default_layout(UserId(TEST_UID), GroupId(TEST_GID));
    {
        let mut mounts = vfs.mounts_write();
        mounts
            .attach(Attachment::new(h_root, None, MountFlags::default()))
            .expect("attach");
        mounts.back_root(h_root).expect("back /");
        mounts
            .attach_at(
                removable(h_stick),
                Path::parse("/Storage/stick").expect("path"),
                crate::fs::Metadata::new(
                    UserId(TEST_UID),
                    GroupId(TEST_GID),
                    Mode::from_bits(0o755),
                ),
            )
            .expect("stick");
    }
    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    cell.install_vfs(vfs).expect("install vfs");
    cell.register(h_root, root, "root", "memfs", [0u8; 16], None)
        .expect("register root");
    cell.register(h_stick, stick, "stick", "memfs", [0u8; 16], None)
        .expect("register stick");
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    identity.install(identity_table()).expect("identity");
    let svc = MountedFilesystemService::new(cell, identity);

    let caps = caps();
    let program = |path| {
        let mut buf = [0u8; 1];
        svc.read_program(TEST_UID, &caps, at(path), 0, &mut buf)
            .map(|_| buf[0])
    };
    let data = |path| {
        let mut buf = [0u8; 1];
        read_path(&svc, TEST_UID, &caps, path, 0, &mut buf).map(|_| buf[0])
    };
    assert_eq!(program("/run"), Ok(b'R'));
    assert_eq!(program("/Storage/stick/run"), Err(Errno::PermissionDenied));
    assert_eq!(data("/Storage/stick/run"), Ok(b'S'));
    // The root volume's link resolves on the root volume, which stores no
    // `/Storage`.
    assert_eq!(program("/onto"), Err(Errno::NotFound));
    assert_eq!(program("/Storage/stick/hop"), Err(Errno::PermissionDenied));
    assert_eq!(data("/Storage/stick/hop"), Ok(b'S'));
}

/// A hole-aware seek answers through the secured VFS: it reports the file's
/// real holes, `NoData` once only holes follow, and needs read permission.
#[test]
fn a_seek_reports_holes_through_the_secured_path() {
    let svc = ready();
    let caps = caps();
    let file = path("sparse");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &file,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");
    assert_eq!(
        write_path(&svc, TEST_UID, &caps, &file, 100, false, b"data"),
        Ok(4)
    );
    assert_eq!(
        seek_region_path(&svc, TEST_UID, &caps, &file, 0, SeekRegion::Data),
        Ok(100)
    );
    assert_eq!(
        seek_region_path(&svc, TEST_UID, &caps, &file, 100, SeekRegion::Hole),
        Ok(104)
    );
    assert_eq!(
        seek_region_path(&svc, TEST_UID, &caps, &file, 104, SeekRegion::Data),
        Err(Errno::NoData)
    );
    assert_eq!(
        seek_region_path(&svc, TEST_UID, &caps, MOUNT, 0, SeekRegion::Data),
        Err(Errno::IsADirectory)
    );
    svc.set_mode(TEST_UID, &caps, at(&file), 0o200, FinalLink::Follow)
        .expect("write-only");
    assert_eq!(
        seek_region_path(&svc, TEST_UID, &caps, &file, 0, SeekRegion::Data),
        Err(Errno::PermissionDenied),
        "where the holes lie is what reading the file reveals"
    );
}

/// A punch is gated as a write of the same bytes: it releases the range on a
/// writable file and is refused on a read-only mount.
#[test]
fn a_punch_is_gated_as_a_write() {
    let svc = ready();
    let caps = caps();
    let file = path("punched");
    open_path(
        &svc,
        TEST_UID,
        &caps,
        &file,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create");
    assert_eq!(
        write_path(&svc, TEST_UID, &caps, &file, 0, false, b"0123456789"),
        Ok(10)
    );
    assert_eq!(punch_path(&svc, TEST_UID, &caps, &file, 2, 4), Ok(()));
    let mut buf = [0xFFu8; 10];
    assert_eq!(read_path(&svc, TEST_UID, &caps, &file, 0, &mut buf), Ok(10));
    assert_eq!(&buf, b"01\0\0\0\x006789");
    assert_eq!(
        seek_region_path(&svc, TEST_UID, &caps, &file, 0, SeekRegion::Hole),
        Ok(2)
    );
    svc.set_mode(TEST_UID, &caps, at(&file), 0o400, FinalLink::Follow)
        .expect("read-only file");
    assert_eq!(
        punch_path(&svc, TEST_UID, &caps, &file, 0, 1),
        Err(Errno::PermissionDenied)
    );

    let read_only = service(true, true, true);
    assert_eq!(
        punch_path(&read_only, TEST_UID, &caps, &path("x"), 0, 1),
        Err(Errno::ReadOnly)
    );
}

/// Descriptors bound for life to the node they opened (`plans/OPENFILE.md`):
/// nothing after the open looks at a path.
mod held {
    use super::*;
    use crate::fs::{Acting, VolumeNodes};

    /// What `path` holds, read through a descriptor of its own.
    fn contents(svc: &MountedFilesystemService<RwMockFs>, path: &str) -> Vec<u8> {
        let mut buf = [0u8; 64];
        let read = read_path(svc, TEST_UID, &caps(), path, 0, &mut buf).expect("reads");
        buf[..read].to_vec()
    }

    fn id_of(svc: &MountedFilesystemService<RwMockFs>, path: &str) -> tairix_abi::FileId {
        svc.stat(TEST_UID, &caps(), at(path), FinalLink::Follow)
            .expect("stats")
            .id
    }

    /// A descriptor keeps the file it opened through a rename of it and a new
    /// file made at its old name: it reads the original bytes, and its writes
    /// land in the renamed file.
    ///
    /// Regression: a descriptor re-resolved its path on every operation, so
    /// it read and wrote whatever came to bear the name.
    #[test]
    fn a_descriptor_keeps_its_file_through_a_rename_and_a_new_file_at_its_name() {
        let (svc, _) = service_over(driver(), &[]);
        let caps = caps();
        let who = acting(TEST_UID, &caps);
        file_with(&svc, "a", b"old");
        let held = own_handle(
            &svc,
            TEST_UID,
            &caps,
            &path("a"),
            OpenFlags::READ.union(OpenFlags::WRITE),
        )
        .expect("opens");
        svc.rename(
            TEST_UID,
            &caps,
            at(&path("a")),
            at(&path("b")),
            RenameFlags::empty(),
            None,
        )
        .expect("rename");
        file_with(&svc, "a", b"new");
        let mut buf = [0u8; 8];
        let read = svc.read_node(&held, &who, 0, &mut buf).expect("reads");
        assert_eq!(&buf[..read], b"old");
        svc.write_node(&held, &who, 0, false, b"X").expect("writes");
        assert_eq!(contents(&svc, &path("b")), b"Xld");
        assert_eq!(
            contents(&svc, &path("a")),
            b"new",
            "the new file is untouched"
        );
        close(&svc, held);
    }

    /// A descriptor whose file was renamed away never reaches a link planted
    /// at its old name.
    #[test]
    fn a_descriptor_never_reaches_a_link_planted_at_its_old_name() {
        let (svc, _) = service_over(driver(), &[]);
        let caps = caps();
        file_with(&svc, "victim", b"secret");
        file_with(&svc, "mine", b"mine");
        let held =
            own_handle(&svc, TEST_UID, &caps, &path("mine"), OpenFlags::WRITE).expect("opens");
        svc.rename(
            TEST_UID,
            &caps,
            at(&path("mine")),
            at(&path("moved")),
            RenameFlags::empty(),
            None,
        )
        .expect("rename away");
        svc.symlink(TEST_UID, &caps, "victim", at(&path("mine")))
            .expect("plant a link");
        svc.write_node(&held, &acting(TEST_UID, &caps), 0, false, b"MINE")
            .expect("writes");
        assert_eq!(contents(&svc, &path("victim")), b"secret");
        assert_eq!(contents(&svc, &path("moved")), b"MINE");
        close(&svc, held);
    }

    /// An open file unlinked reads on with no name, its number kept from the
    /// next node made, and its close frees it.
    #[test]
    fn an_unlinked_open_file_reads_on_with_no_name_and_its_close_frees_it() {
        let (svc, _) = service_over(driver().with_reused_numbers(), &[]);
        let caps = caps();
        let who = acting(TEST_UID, &caps);
        file_with(&svc, "doomed", b"still here");
        let number = id_of(&svc, &path("doomed"));
        let held =
            own_handle(&svc, TEST_UID, &caps, &path("doomed"), OpenFlags::READ).expect("opens");
        svc.unlink(
            TEST_UID,
            &caps,
            at(&path("doomed")),
            UnlinkFlags::empty(),
            None,
        )
        .expect("unlink");
        let mut buf = [0u8; 16];
        let read = svc.read_node(&held, &who, 0, &mut buf).expect("reads");
        assert_eq!(&buf[..read], b"still here");
        assert_eq!(svc.stat_node(&held, &who).map(|stat| stat.nlink), Ok(0));
        file_with(&svc, "next", b"x");
        assert_ne!(id_of(&svc, &path("next")), number, "a held number is kept");
        close(&svc, held);
        file_with(&svc, "after", b"y");
        assert_eq!(id_of(&svc, &path("after")), number, "freed at the close");
    }

    /// A node's mode and ACL are judged on every operation: an owner's chmod
    /// revokes a grant and another principal's own descriptor, while the
    /// owner's own descriptor reads on, since the owner could restore it.
    #[test]
    fn a_chmod_revokes_a_grant_and_another_principals_descriptor_but_not_the_owners() {
        let other = 2000;
        let (svc, _) = service_over(driver(), &[other]);
        let caps = caps();
        file_with(&svc, "shared", b"data");
        let owners = own_handle(&svc, TEST_UID, &caps, &path("shared"), OpenFlags::READ)
            .expect("the owner opens");
        let others = own_handle(&svc, other, &caps, &path("shared"), OpenFlags::READ)
            .expect("another principal opens");
        svc.set_mode(
            TEST_UID,
            &caps,
            at(&path("shared")),
            0o000,
            FinalLink::Follow,
        )
        .expect("chmod");
        let mut buf = [0u8; 4];
        assert_eq!(
            svc.read_node(&owners, &acting(TEST_UID, &caps), 0, &mut buf),
            Ok(4)
        );
        let grant = Acting {
            uid: TEST_UID,
            caps: &caps,
            granted: true,
        };
        assert_eq!(
            svc.read_node(&owners, &grant, 0, &mut buf),
            Err(Errno::PermissionDenied),
            "a grant has no owner's exemption"
        );
        assert_eq!(
            svc.read_node(&others, &acting(other, &caps), 0, &mut buf),
            Err(Errno::PermissionDenied)
        );
        close(&svc, owners);
        close(&svc, others);
    }

    /// The descriptor that created a file writes it after its owner made it
    /// read-only, as POSIX's `open(O_CREAT, 0444)` then `write` does; a new
    /// open is judged on the mode.
    #[test]
    fn the_creating_descriptor_writes_a_file_its_owner_made_read_only() {
        let (svc, _) = service_over(driver(), &[]);
        let caps = caps();
        let created = own_handle(
            &svc,
            TEST_UID,
            &caps,
            &path("ro"),
            OpenFlags::CREATE.union(OpenFlags::WRITE),
        )
        .expect("creates");
        svc.set_mode(TEST_UID, &caps, at(&path("ro")), 0o444, FinalLink::Follow)
            .expect("chmod");
        assert_eq!(
            svc.write_node(&created, &acting(TEST_UID, &caps), 0, false, b"ok"),
            Ok(2)
        );
        assert_eq!(
            open_path(&svc, TEST_UID, &caps, &path("ro"), OpenFlags::WRITE),
            Err(Errno::PermissionDenied)
        );
        // Asked about, it answers as the path does: read-only.
        let ask = |judged| {
            svc.access_node(
                &created,
                &acting(TEST_UID, &caps),
                crate::fs::Access::Write,
                judged,
            )
        };
        assert_eq!(ask(crate::fs::Judged::AsOperation), Ok(()));
        assert_eq!(
            ask(crate::fs::Judged::AsQuery),
            Err(Errno::PermissionDenied)
        );
        close(&svc, created);
    }

    /// A node's capability gate is judged on the set each operation runs
    /// with, so a descriptor's next read stops once its capability goes.
    #[test]
    fn a_capability_gate_binds_each_operation_to_the_set_it_runs_with() {
        let (svc, cell) = service_over(driver(), &[]);
        file_with(&svc, "gated", b"audit");
        {
            let driver = cell
                .driver(DriverHandle::from_raw(9).expect("handle"))
                .expect("registered");
            let mut fs = driver.lock();
            let root = fs.root();
            let node = fs.lookup(root, b"gated").expect("found");
            let mut sec = fs.security(node).expect("security");
            sec.required_cap = Some(CapabilityId::AUDIT_READ);
            fs.set_security(node, sec).expect("gates");
        }
        let mut holding = CapabilitySet::empty();
        holding.insert(CapabilityId::AUDIT_READ);
        let held = own_handle(&svc, TEST_UID, &holding, &path("gated"), OpenFlags::READ)
            .expect("a holder opens");
        let mut buf = [0u8; 5];
        assert_eq!(
            svc.read_node(&held, &acting(TEST_UID, &holding), 0, &mut buf),
            Ok(5)
        );
        let narrowed = CapabilitySet::empty();
        assert_eq!(
            svc.read_node(&held, &acting(TEST_UID, &narrowed), 0, &mut buf),
            Err(Errno::PermissionDenied),
            "the owner's exemption never passes the gate"
        );
        close(&svc, held);
    }

    /// A descriptor whose projection left the mount table is stale, and one
    /// whose registration was retired is never redirected to a volume
    /// attached under the same name and handle since; a fresh open reaches
    /// the new volume.
    #[test]
    fn a_withdrawn_or_retired_descriptor_is_stale_and_never_reaches_a_successor() {
        let (svc, cell) = service_over(driver(), &[]);
        let caps = caps();
        let who = acting(TEST_UID, &caps);
        let handle = DriverHandle::from_raw(9).expect("handle");
        file_with(&svc, "f", b"one");
        let held = own_handle(&svc, TEST_UID, &caps, &path("f"), OpenFlags::READ).expect("opens");
        let vfs = cell.vfs().expect("installed");
        vfs.mounts_write().detach(handle).expect("detach");
        let mut buf = [0u8; 3];
        assert_eq!(svc.read_node(&held, &who, 0, &mut buf), Err(Errno::Stale));

        cell.unregister(handle).expect("registered");
        let mut next = driver();
        let root = next.root();
        let file = next
            .create(root, b"f", NodeKind::RegularFile, &PLANTED)
            .expect("create");
        next.write_at(file, 0, b"two").expect("write");
        vfs.mounts_write()
            .attach_at(
                Attachment::new(handle, Some(MOUNT_MEDIUM), MountFlags::default()),
                Path::parse(MOUNT).expect("path"),
                crate::fs::Metadata::new(
                    UserId(TEST_UID),
                    GroupId(TEST_GID),
                    Mode::from_bits(0o755),
                ),
            )
            .expect("another volume at the same name");
        cell.register(handle, next, "vol", "memfs", [0u8; 16], None)
            .expect("register");
        assert_eq!(svc.read_node(&held, &who, 0, &mut buf), Err(Errno::Stale));
        assert_eq!(svc.stat_node(&held, &who).map(|_| ()), Err(Errno::Stale));
        assert_eq!(contents(&svc, &path("f")), b"two");
        close(&svc, held);
    }

    /// A stand-in registered under a registration takes it over in place, so
    /// the descriptors opened through it answer the stand-in; no other
    /// registration may take the handle.
    #[test]
    fn a_stand_in_takes_over_the_registration_its_descriptors_answer_through() {
        let cell: &'static LateFilesystem<FlushSpy> =
            crate::forever(Box::new(LateFilesystem::new()));
        cell.install_vfs(vfs(false)).expect("install vfs");
        let handle = DriverHandle::from_raw(9).expect("handle");
        cell.register(handle, FlushSpy::new(), "vol", "spy", [0u8; 16], None)
            .expect("register");
        let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
        identity.install(identity_table()).expect("identity");
        let svc = MountedFilesystemService::new(cell, identity);
        let caps = caps();
        let who = acting(TEST_UID, &caps);
        let held = own_handle(&svc, TEST_UID, &caps, MOUNT, OpenFlags::empty()).expect("opens");
        assert_eq!(svc.sync_node(&held, &who), Ok(()));
        let nodes = cell.nodes(handle).expect("registered");
        cell.register_with_nodes(
            handle,
            FlushSpy::refusing(),
            "vol",
            "spy",
            [0u8; 16],
            None,
            Arc::clone(&nodes),
        )
        .expect("the stand-in takes over");
        assert_eq!(
            svc.sync_node(&held, &who),
            Err(Errno::DeviceFault),
            "answered by the stand-in, not stale"
        );
        assert_eq!(
            cell.register_with_nodes(
                handle,
                FlushSpy::new(),
                "vol",
                "spy",
                [0u8; 16],
                None,
                Arc::new(VolumeNodes::new(u64::MAX)),
            )
            .map(drop),
            Err(crate::fs::RegisterError::AlreadyRegistered)
        );
        close(&svc, held);
    }

    /// A descriptor dropped where no close reaps it keeps its pin until the
    /// write-back flusher's next pass, which releases it.
    #[test]
    fn the_write_back_pass_reaps_what_a_drop_left() {
        let (svc, cell) = service_over(driver(), &[]);
        let caps = caps();
        file_with(&svc, "doomed", b"x");
        let held =
            own_handle(&svc, TEST_UID, &caps, &path("doomed"), OpenFlags::READ).expect("opens");
        let pinned = held.node.pinned();
        svc.unlink(
            TEST_UID,
            &caps,
            at(&path("doomed")),
            UnlinkFlags::empty(),
            None,
        )
        .expect("unlink");
        drop(held);
        let driver = cell
            .driver(DriverHandle::from_raw(9).expect("handle"))
            .expect("registered");
        assert!(driver.lock().node_info(pinned).is_ok(), "still pinned");
        let sink = crate::test_sink::TestSink::new();
        crate::fs::writeback::publish_due(cell, &sink, 0);
        assert_eq!(
            driver.lock().node_info(pinned).map(|_| ()),
            Err(tairix_abi::driver::DriverError::NotFound),
            "the pass released it, freeing the nameless node"
        );
    }

    /// A served volume's release a drop left is never the flusher's, whose
    /// pass must not wait on a server; the next operation on the volume hands
    /// it over before it runs.
    #[test]
    fn a_served_volume_is_reaped_by_its_next_operation_never_the_flusher() {
        let (serving, pinned) = served_release_left_by_a_drop();
        let svc = &serving.service;
        let driver = &serving.drivers[0];
        let sink = crate::test_sink::TestSink::new();
        crate::fs::writeback::publish_due(serving.mounted, &sink, 0);
        assert!(
            driver.lock().node_info(pinned).is_ok(),
            "the flusher left it"
        );
        let mut back = [0u8; 1];
        read_path(svc, TEST_UID, &caps(), &path("other"), 0, &mut back).expect("reads");
        assert_eq!(
            driver.lock().node_info(pinned).map(|_| ()),
            Err(tairix_abi::driver::DriverError::NotFound),
            "the read let it go first, freeing the nameless node"
        );
    }

    /// A sync hands a served volume's waiting releases over before it
    /// flushes, so what they free is committed with the rest.
    #[test]
    fn a_sync_hands_a_served_volumes_releases_over_first() {
        let (serving, pinned) = served_release_left_by_a_drop();
        serving.service.sync_all(TEST_UID, &caps()).expect("syncs");
        assert_eq!(
            serving.drivers[0].lock().node_info(pinned).map(|_| ()),
            Err(tairix_abi::driver::DriverError::NotFound),
        );
    }

    /// A served volume holding the files `doomed` and `other`, and the pin of
    /// `doomed`, unlinked while open, whose release a dropped descriptor left
    /// queued.
    fn served_release_left_by_a_drop() -> (
        crate::fs::test_volume::Serving<RwMockFs>,
        tairix_abi::driver::filesystem::NodeId,
    ) {
        let serving = crate::fs::test_volume::mounted_served(driver(), &[TEST_ACCOUNT]);
        let svc = &serving.service;
        let caps = caps();
        file_with(svc, "doomed", b"x");
        file_with(svc, "other", b"y");
        let held =
            own_handle(svc, TEST_UID, &caps, &path("doomed"), OpenFlags::READ).expect("opens");
        let pinned = held.node.pinned();
        svc.unlink(
            TEST_UID,
            &caps,
            at(&path("doomed")),
            UnlinkFlags::empty(),
            None,
        )
        .expect("unlink");
        drop(held);
        (serving, pinned)
    }

    /// A working directory is a directory its caller may search, pinned as
    /// it is entered: a catalog is held as the catalog, and the view root
    /// pins nothing.
    #[test]
    fn a_working_directory_is_a_searchable_directory_pinned_as_it_is_entered() {
        let svc = ready();
        let caps = caps();
        svc.mkdir(TEST_UID, &caps, at(&path("d")), new_dir())
            .expect("mkdir");
        file_with(&svc, "f", b"");
        let Ok(Cwd::Dir(cwd)) = svc.chdir(TEST_UID, &caps, at(&path("d"))) else {
            panic!("a directory is a working directory");
        };
        assert_eq!(*cwd.opened_as, path("d"));
        assert_eq!(
            svc.chdir(TEST_UID, &caps, at(&path("f"))).err(),
            Some(Errno::NotADirectory)
        );
        assert_eq!(
            svc.chdir(TEST_UID, &caps, at(&path("none"))).err(),
            Some(Errno::NotFound)
        );
        assert!(matches!(
            svc.chdir(TEST_UID, &caps, at("/Storage")),
            Ok(Cwd::Catalog(_))
        ));
        assert!(matches!(svc.chdir(TEST_UID, &caps, at("/")), Ok(Cwd::Root)));
        svc.set_mode(TEST_UID, &caps, at(&path("d")), 0o644, FinalLink::Follow)
            .expect("chmod");
        assert_eq!(
            svc.chdir(TEST_UID, &caps, at(&path("d"))).err(),
            Some(Errno::PermissionDenied),
            "a directory its caller may not search"
        );
        close(&svc, cwd);
    }

    /// A descriptor of the catalog lists it until its projection leaves the
    /// mount table, here replaced by a backed one.
    #[test]
    fn a_catalog_descriptor_lists_until_its_projection_leaves() {
        let (svc, cell) = service_over(driver(), &[]);
        let caps = caps();
        let Ok(Opened::Catalog(catalog)) = svc.open(
            TEST_UID,
            &caps,
            at("/Storage"),
            OpenFlags::DIRECTORY,
            CreateMode::NONE,
        ) else {
            panic!("the catalog opens as itself");
        };
        let mut names = Vec::new();
        svc.readdir_catalog(
            TEST_UID,
            &caps,
            &catalog,
            &mut Listing::default(),
            &mut |entry| {
                names.push(entry.name.to_vec());
                DirVisit::Take
            },
        )
        .expect("lists");
        assert_eq!(names, [b"vol".to_vec()]);
        cell.vfs()
            .expect("installed")
            .mounts_write()
            .set_backing(
                &Path::parse("/Storage").expect("path"),
                DriverHandle::from_raw(9).expect("handle"),
                Vec::new(),
            )
            .expect("backs the catalog");
        assert_eq!(
            svc.readdir_catalog(
                TEST_UID,
                &caps,
                &catalog,
                &mut Listing::default(),
                &mut |_| DirVisit::Take
            ),
            Err(Errno::Stale)
        );
    }

    /// A directory removed while a descriptor holds it answers a lookup as
    /// gone, which is how its watchers learn of it.
    #[test]
    fn a_removed_held_directory_answers_its_lookup_as_gone() {
        let (svc, _) = service_over(driver(), &[]);
        let caps = caps();
        let who = acting(TEST_UID, &caps);
        svc.mkdir(TEST_UID, &caps, at(&path("d")), new_dir())
            .expect("mkdir");
        let held =
            own_handle(&svc, TEST_UID, &caps, &path("d"), OpenFlags::DIRECTORY).expect("opens");
        assert!(svc.lookup_entries_node(&held, &who, &[]).is_ok());
        svc.unlink(
            TEST_UID,
            &caps,
            at(&path("d")),
            UnlinkFlags::DIRECTORY,
            None,
        )
        .expect("rmdir");
        assert_eq!(
            svc.lookup_entries_node(&held, &who, &[]).map(|_| ()),
            Err(Errno::NotFound)
        );
        close(&svc, held);
    }

    /// An open whose pin the driver refuses takes back the name it made, and
    /// gives back the release slot it reserved.
    #[test]
    fn an_open_whose_pin_is_refused_takes_back_the_name_it_made() {
        let (svc, cell) = service_over(driver(), &[]);
        let caps = caps();
        let driver = cell
            .driver(DriverHandle::from_raw(9).expect("handle"))
            .expect("registered");
        driver.lock().refuse_pins(Some(DriverError::OutOfMemory));
        let flags = OpenFlags::WRITE
            .union(OpenFlags::CREATE)
            .union(OpenFlags::EXCLUSIVE);
        assert_eq!(
            open_path(&svc, TEST_UID, &caps, &path("f"), flags),
            Err(Errno::OutOfMemory)
        );
        let mut fs = driver.lock();
        let root = fs.root();
        assert_eq!(fs.lookup(root, b"f"), Err(DriverError::NotFound));
        assert_eq!(driver.nodes().outstanding(), 0, "the slot was given back");
    }

    /// A truncating open truncates last, so one refused its pin leaves the
    /// file's bytes.
    #[test]
    fn a_truncating_open_refused_its_pin_leaves_the_bytes() {
        let (svc, cell) = service_over(driver(), &[]);
        let caps = caps();
        file_with(&svc, "f", b"kept");
        let driver = cell
            .driver(DriverHandle::from_raw(9).expect("handle"))
            .expect("registered");
        driver.lock().refuse_pins(Some(DriverError::OutOfMemory));
        let flags = OpenFlags::WRITE.union(OpenFlags::TRUNCATE);
        assert!(open_path(&svc, TEST_UID, &caps, &path("f"), flags).is_err());
        driver.lock().refuse_pins(None);
        assert_eq!(contents(&svc, &path("f")), b"kept");
    }

    /// A byte operation after a principal's first takes its groups from the
    /// service's cache, so nothing on its way allocates.
    #[test]
    fn a_warm_byte_operation_allocates_nothing() {
        use crate::test_alloc::{opt_in_current_thread, opt_out_current_thread, LiveBytes};
        static COUNTER: LiveBytes = LiveBytes::new();
        let svc = ready();
        let caps = caps();
        file_with(&svc, "f", b"x");
        let held = own_handle(&svc, TEST_UID, &caps, &path("f"), OpenFlags::READ).expect("opens");
        let who = acting(TEST_UID, &caps);
        let mut buf = [0u8; 1];
        assert_eq!(svc.read_node(&held, &who, 0, &mut buf), Ok(1));
        opt_in_current_thread(&COUNTER);
        let read = svc.read_node(&held, &who, 0, &mut buf);
        opt_out_current_thread();
        assert_eq!(read, Ok(1));
        assert_eq!(COUNTER.allocations(), 0, "a warm read allocated");
        close(&svc, held);
    }

    /// A projection's floor no name reaches any more lists nothing, the
    /// projections attached beneath its name included.
    #[test]
    fn a_removed_floor_lists_nothing() {
        let handle = DriverHandle::from_raw(9).expect("handle");
        let mut fs = driver();
        let root = fs.root();
        fs.create(root, b"Sub", NodeKind::Directory, &PLANTED)
            .expect("subtree");
        let mut vfs = Vfs::with_default_layout(UserId(TEST_UID), GroupId(TEST_GID));
        let caps = caps();
        let cred = Credentials {
            uid: UserId(TEST_UID),
            gid: GroupId(TEST_GID),
            supplementary_gids: &[],
            caps: &caps,
        };
        for point in ["/Users/whole", "/Users/sub", "/Users/sub/inner"] {
            vfs.mkdir(
                &cred,
                &Path::parse(point).expect("path"),
                Mode::from_bits(0o755),
            )
            .expect("mount point");
        }
        vfs.mounts_write()
            .attach(Attachment::new(handle, None, MountFlags::default()))
            .expect("attach");
        users_projection(&vfs);
        {
            let mut mounts = vfs.mounts_write();
            mounts
                .mount(
                    Path::parse("/Users/whole").expect("path"),
                    MountFlags::default(),
                    Some(handle),
                )
                .expect("the whole volume");
            mounts
                .mount_rebased(
                    Path::parse("/Users/sub").expect("path"),
                    MountFlags::default(),
                    Some(handle),
                    alloc::vec![alloc::string::String::from("Sub")],
                )
                .expect("its subtree");
            mounts
                .mount(
                    Path::parse("/Users/sub/inner").expect("path"),
                    MountFlags::default(),
                    None,
                )
                .expect("a projection beneath the subtree");
        }
        let cell: &'static LateFilesystem<RwMockFs> =
            crate::forever(Box::new(LateFilesystem::new()));
        cell.install_vfs(vfs).expect("install vfs");
        cell.register(handle, fs, "vol", "memfs", [0u8; 16], None)
            .expect("register");
        let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
        identity.install(identity_table()).expect("identity");
        let svc = MountedFilesystemService::new(cell, identity);
        let who = acting(TEST_UID, &caps);
        let names = |held: &NodeHandle| {
            let mut names = Vec::new();
            let listed = svc.readdir_node(held, &who, &mut Listing::default(), &mut |entry| {
                names.push(entry.name.to_vec());
                DirVisit::Take
            });
            listed.map(|()| names)
        };
        let floor = own_handle(&svc, TEST_UID, &caps, "/Users/sub", OpenFlags::DIRECTORY)
            .expect("opens the floor");
        assert_eq!(names(&floor), Ok(alloc::vec![b"inner".to_vec()]));
        svc.unlink(
            TEST_UID,
            &caps,
            at("/Users/whole/Sub"),
            UnlinkFlags::DIRECTORY,
            None,
        )
        .expect("rmdir");
        assert_eq!(names(&floor), Ok(Vec::new()));
        close(&svc, floor);
    }
}

/// Queue the release of a pin on a fresh file of the volume `driver` serves,
/// answering the volume's registration and the pinned id.
fn queue_a_release(
    driver: &crate::fs::DriverLock<RwMockFs>,
) -> (
    Arc<crate::fs::VolumeNodes>,
    tairix_abi::driver::filesystem::NodeId,
) {
    let pinned = {
        let mut fs = driver.lock();
        let root = fs.root();
        let file = fs
            .create(root, b"held", NodeKind::RegularFile, &SYSTEM_RECORD)
            .expect("create");
        fs.pin(file).expect("pin")
    };
    let nodes = Arc::clone(driver.nodes());
    let slot = nodes.admit().expect("a slot");
    drop(crate::fs::NodeRef::new(slot, pinned, NodeKind::RegularFile).expect("a pin"));
    (nodes, pinned)
}

/// The flusher only tries a volume's lock: a pass made while another holds
/// one volume reaps the rest and leaves that one due, so a long operation
/// never stalls the flusher. The pass below runs on the thread holding the
/// busy volume, so waiting for its lock would never return.
#[test]
fn a_flusher_pass_leaves_a_busy_volume_due_and_reaps_the_others() {
    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    let register = |raw, name| {
        let handle = DriverHandle::from_raw(raw).expect("non-zero handle");
        cell.register(handle, RwMockFs::new(), name, "memfs", [0u8; 16], None)
            .expect("register")
    };
    let (busy, free) = (register(21, "busy"), register(22, "free"));
    let (busy_nodes, busy_pin) = queue_a_release(&busy);
    let (free_nodes, free_pin) = queue_a_release(&free);
    let held = busy.lock();
    cell.reap_due();
    assert!(busy_nodes.is_reap_due(), "a busy volume stays due");
    assert!(!free_nodes.is_reap_due());
    assert_eq!(
        free.lock().unpin(free_pin),
        Err(tairix_abi::driver::DriverError::NotFound),
        "the free volume was reaped"
    );
    drop(held);
    cell.reap_due();
    assert!(!busy_nodes.is_reap_due());
    assert_eq!(
        busy.lock().unpin(busy_pin),
        Err(tairix_abi::driver::DriverError::NotFound),
        "the volume was reaped once it was free"
    );
}

/// A driver is held from before its entry is visible until it has its host,
/// so no operation reaches it first: one that did, on a server's proxy, would
/// wait on a server that is inside the very call serving it. The driver it
/// replaces in place is dropped once the successor is visible, and finds the
/// successor already held.
#[test]
fn a_driver_is_held_before_its_entry_is_visible() {
    use core::sync::atomic::{AtomicBool, Ordering};
    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    let held: &'static AtomicBool = crate::forever(Box::new(AtomicBool::new(false)));
    let handle = DriverHandle::from_raw(32).expect("non-zero handle");
    let first = cell
        .register(handle, RwMockFs::new(), "vol", "memfs", [0u8; 16], None)
        .expect("register");
    let nodes = Arc::clone(first.nodes());
    first.lock().on_drop(Box::new(move || {
        let successor = cell.driver(handle).expect("visible");
        held.store(successor.try_lock().is_none(), Ordering::Relaxed);
    }));
    drop(first);
    let second = cell
        .register_with_nodes(
            handle,
            RwMockFs::new(),
            "vol",
            "memfs",
            [0u8; 16],
            None,
            nodes,
        )
        .expect("replaced in place");
    assert!(held.load(Ordering::Relaxed), "the successor was held");
    assert!(second.try_lock().is_some(), "and is free once registered");
}

/// A pass walks registrations by instance number, so one withdrawn behind its
/// cursor moves none it has still to visit.
#[test]
fn a_flusher_pass_finds_a_due_volume_past_one_withdrawn_beneath_it() {
    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    let handles = [31, 32, 33].map(|raw| DriverHandle::from_raw(raw).expect("non-zero handle"));
    let drivers = handles.map(|handle| {
        cell.register(handle, RwMockFs::new(), "vol", "memfs", [0u8; 16], None)
            .expect("register")
    });
    let (second, _) = queue_a_release(&drivers[1]);
    let (third, _) = queue_a_release(&drivers[2]);
    assert!(cell.unregister(handles[0]).is_some());
    let next = cell.next_reap_due(Some(second.instance()));
    assert!(next.is_some_and(|(_, nodes)| Arc::ptr_eq(&nodes, &third)));
    cell.reap_due();
    assert!(!second.is_reap_due() && !third.is_reap_due());
}

/// A registration is found by its instance, and the flusher walks the due ones
/// in instance order, whatever order the registrations arrived in: the registry
/// is kept sorted for its binary searches.
#[test]
fn registrations_arriving_out_of_order_are_kept_in_instance_order() {
    let cell: &'static LateFilesystem<RwMockFs> = crate::forever(Box::new(LateFilesystem::new()));
    let later = Arc::new(crate::fs::VolumeNodes::new(9));
    let earlier = Arc::new(crate::fs::VolumeNodes::new(3));
    let between = Arc::new(crate::fs::VolumeNodes::new(5));
    let mut drivers = Vec::new();
    for (raw, nodes) in [(41, &later), (42, &earlier), (43, &between)] {
        let handle = DriverHandle::from_raw(raw).expect("non-zero handle");
        drivers.push(
            cell.register_with_nodes(
                handle,
                RwMockFs::new(),
                "vol",
                "memfs",
                [0u8; 16],
                None,
                Arc::clone(nodes),
            )
            .expect("register"),
        );
    }
    for nodes in [&later, &earlier, &between] {
        assert!(cell.registered(nodes.instance()).is_some());
    }
    assert!(cell.registered(4).is_none());
    for driver in &drivers {
        let _ = queue_a_release(driver);
    }
    let order: Vec<u64> = core::iter::successors(cell.next_reap_due(None), |(_, nodes)| {
        cell.next_reap_due(Some(nodes.instance()))
    })
    .map(|(_, nodes)| nodes.instance())
    .collect();
    assert_eq!(order, [3, 5, 9]);
}

/// Paths resolved from a working directory or a held directory, `..` taken
/// physically across projections, and the expectations a removal or a rename
/// is held to.
mod resolve {
    use super::*;
    use crate::fs::{Acting, At, Metadata, Start};
    use tairix_abi::fs::{Access as Wanted, NodeExpect};
    use tairix_abi::time::Time64;

    /// `path` from where `cwd` stands.
    fn from<'a>(cwd: &'a Cwd, path: &'a str) -> At<'a> {
        At {
            start: cwd.start(),
            path,
        }
    }

    /// `path` from the directory `held` holds.
    fn beneath<'a>(held: &'a NodeHandle, path: &'a str) -> At<'a> {
        At {
            start: Start::Dir(held),
            path,
        }
    }

    /// `path`, entered as the test principal's working directory.
    fn enter(svc: &dyn FilesystemService, at: At<'_>) -> Cwd {
        svc.chdir(TEST_UID, &caps(), at)
            .expect("a working directory")
    }

    /// Let `cwd` go, reaping what it pins.
    fn leave(svc: &dyn FilesystemService, cwd: Cwd) {
        if let Cwd::Dir(handle) = cwd {
            close(svc, handle);
        }
    }

    /// The bytes of the file `at` names, read through a descriptor of its
    /// own.
    fn bytes(svc: &dyn FilesystemService, at: At<'_>) -> Result<Vec<u8>, Errno> {
        let caps = caps();
        let opened = svc.open(TEST_UID, &caps, at, OpenFlags::READ, CreateMode::NONE)?;
        let Opened::Node {
            node,
            projection,
            opened_as,
            at_floor,
            ..
        } = opened
        else {
            return Err(Errno::IsADirectory);
        };
        let handle = NodeHandle {
            node,
            projection,
            authority: Authority::Holder,
            opened_as,
            at_floor,
        };
        let mut buf = [0u8; 32];
        let read = svc.read_node(&handle, &acting(TEST_UID, &caps), 0, &mut buf);
        close(svc, handle);
        Ok(buf[..read?].to_vec())
    }

    fn canonical(svc: &dyn FilesystemService, at: At<'_>) -> Result<alloc::string::String, Errno> {
        svc.realpath(TEST_UID, &caps(), at, RealpathMode::Existing)
    }

    /// The service over `vfs`, each of `volumes` registered under its handle.
    fn serving(vfs: Vfs, volumes: Vec<(u64, RwMockFs)>) -> MountedFilesystemService<RwMockFs> {
        let cell: &'static LateFilesystem<RwMockFs> =
            crate::forever(Box::new(LateFilesystem::new()));
        cell.install_vfs(vfs).expect("install vfs");
        for (raw, fs) in volumes {
            let handle = DriverHandle::from_raw(raw).expect("handle");
            cell.register(handle, fs, "vol", "memfs", [0u8; 16], None)
                .expect("register");
        }
        let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
        identity.install(identity_table()).expect("identity");
        MountedFilesystemService::new(cell, identity)
    }

    /// The default layout with each of `points` attached as a removable
    /// volume beneath the catalog, under handles from 11 up.
    fn removables(points: &[&str]) -> Vfs {
        let vfs = Vfs::with_default_layout(UserId(TEST_UID), GroupId(TEST_GID));
        let template = Metadata::new(UserId(TEST_UID), GroupId(TEST_GID), Mode::from_bits(0o755));
        for (raw, point) in (11..).zip(points) {
            let handle = DriverHandle::from_raw(raw).expect("handle");
            vfs.mounts_write()
                .attach_at(
                    removable(handle),
                    Path::parse(point).expect("path"),
                    template.clone(),
                )
                .expect("attached beneath the catalog");
        }
        vfs
    }

    /// An expectation of the node `node` alone.
    fn node_alone(id: tairix_abi::FileId) -> NodeExpect {
        NodeExpect {
            id,
            content_gen: 0,
            size: 0,
            modified: Time64::UNIX_EPOCH,
            changed: Time64::UNIX_EPOCH,
        }
    }

    /// A working directory is its directory, not the path it was entered
    /// by: it survives a rename, and once its label reaches another node
    /// its canonical path is found by climbing from it.
    ///
    /// Regression: the working directory was a string, so a rename left it
    /// naming nothing and a new directory at the old name took its place.
    #[test]
    fn a_working_directory_survives_a_rename_and_is_found_afresh() {
        let svc = ready();
        let caps = caps();
        svc.mkdir(TEST_UID, &caps, at(&path("d")), new_dir())
            .expect("mkdir");
        file_with(&svc, "d/inside", b"in");
        let cwd = enter(&svc, at(&path("d")));
        svc.rename(
            TEST_UID,
            &caps,
            at(&path("d")),
            at(&path("e")),
            RenameFlags::empty(),
            None,
        )
        .expect("rename");
        svc.mkdir(TEST_UID, &caps, at(&path("d")), new_dir())
            .expect("a new directory at the old name");
        assert_eq!(
            svc.canonical_of(TEST_UID, &caps, cwd.start()),
            Ok(path("e"))
        );
        assert_eq!(bytes(&svc, from(&cwd, "inside")), Ok(b"in".to_vec()));
        assert_eq!(canonical(&svc, from(&cwd, "inside")), Ok(path("e/inside")));
        leave(&svc, cwd);
    }

    /// A working directory removed under its holder names nothing: its
    /// canonical path and every path from it are not found, and nothing can
    /// be made in it.
    #[test]
    fn a_removed_working_directory_is_not_found() {
        let svc = ready();
        let caps = caps();
        svc.mkdir(TEST_UID, &caps, at(&path("d")), new_dir())
            .expect("mkdir");
        let cwd = enter(&svc, at(&path("d")));
        svc.unlink(
            TEST_UID,
            &caps,
            at(&path("d")),
            UnlinkFlags::DIRECTORY,
            None,
        )
        .expect("rmdir");
        assert_eq!(
            svc.canonical_of(TEST_UID, &caps, cwd.start()),
            Err(Errno::NotFound)
        );
        assert_eq!(
            svc.mkdir(TEST_UID, &caps, from(&cwd, "x"), new_dir()),
            Err(Errno::NotFound)
        );
        assert_eq!(
            svc.stat(TEST_UID, &caps, from(&cwd, ".."), FinalLink::Follow)
                .err(),
            Some(Errno::NotFound)
        );
        leave(&svc, cwd);
    }

    /// `..` leaves the directory a walk stands in, which a link it followed
    /// chose, never the text before it.
    ///
    /// Regression: `..` was collapsed against the path's text, so `cd l; ..`
    /// returned to the link's directory rather than its target's parent.
    #[test]
    fn dot_dot_from_a_directory_entered_through_a_link_is_physical() {
        let svc = ready();
        let caps = caps();
        for dir in ["a", "a/b"] {
            svc.mkdir(TEST_UID, &caps, at(&path(dir)), new_dir())
                .expect("mkdir");
        }
        file_with(&svc, "a/marker", b"a");
        svc.symlink(TEST_UID, &caps, "a/b", at(&path("l")))
            .expect("link");
        let cwd = enter(&svc, at(&path("l")));
        assert_eq!(
            svc.canonical_of(TEST_UID, &caps, cwd.start()),
            Ok(path("a/b"))
        );
        assert_eq!(canonical(&svc, from(&cwd, "..")), Ok(path("a")));
        assert_eq!(bytes(&svc, from(&cwd, "../marker")), Ok(b"a".to_vec()));
        assert_eq!(
            canonical(&svc, at(&alloc::format!("{MOUNT}/l/../marker"))),
            Ok(path("a/marker"))
        );
        leave(&svc, cwd);
    }

    /// A caller's `..` at a volume's floor climbs into the catalog it is
    /// shown in and on into a sibling volume; one a link supplies stays on
    /// the link's own volume.
    #[test]
    fn a_callers_dot_dot_crosses_projections_and_a_links_stays_on_its_volume() {
        let mut usb = driver();
        let root = usb.root();
        let decoy = usb
            .create(root, b"usb2", NodeKind::Directory, &PLANTED)
            .expect("a directory of the sibling's name");
        let inside = usb
            .create(decoy, b"x", NodeKind::RegularFile, &PLANTED)
            .expect("its file");
        usb.write_at(inside, 0, b"inside").expect("fill");
        usb.create_link(root, b"up", b"../usb2/x", &PLANTED)
            .expect("an ascending link");
        let mut sibling = driver();
        let sibling_root = sibling.root();
        let other = sibling
            .create(sibling_root, b"x", NodeKind::RegularFile, &PLANTED)
            .expect("the sibling's file");
        sibling.write_at(other, 0, b"other").expect("fill");
        let svc = serving(
            removables(&["/Storage/usb", "/Storage/usb2"]),
            alloc::vec![(11, usb), (12, sibling)],
        );
        let caps = caps();

        let cwd = enter(&svc, at("/Storage/usb"));
        assert_eq!(canonical(&svc, from(&cwd, "..")), Ok("/Storage".into()));
        assert_eq!(bytes(&svc, from(&cwd, "../usb2/x")), Ok(b"other".to_vec()));
        assert_eq!(bytes(&svc, from(&cwd, "up")), Ok(b"inside".to_vec()));
        assert!(matches!(
            svc.chdir(TEST_UID, &caps, from(&cwd, "..")),
            Ok(Cwd::Catalog(_))
        ));
        assert_eq!(
            canonical(&svc, at("/../Storage/usb/..")),
            Ok("/Storage".into())
        );
        leave(&svc, cwd);
    }

    /// Each `..` asks search permission of the directory it leaves, and
    /// leaves only a directory.
    ///
    /// Regression: `d/..` was spelled away before the walk, so it reached
    /// the parent of a directory its caller may not search, or of a file.
    #[test]
    fn a_dot_dot_needs_search_on_the_directory_it_leaves() {
        let svc = ready();
        let caps = caps();
        svc.mkdir(TEST_UID, &caps, at(&path("d")), new_dir())
            .expect("mkdir");
        svc.set_mode(TEST_UID, &caps, at(&path("d")), 0o600, FinalLink::Follow)
            .expect("closed to search");
        file_with(&svc, "f", b"");
        let stat = |text: &str| {
            svc.stat(TEST_UID, &caps, at(&path(text)), FinalLink::Follow)
                .map(|stat| stat.kind)
        };
        assert_eq!(stat("d/.."), Err(Errno::PermissionDenied));
        assert_eq!(stat("f/.."), Err(Errno::NotADirectory));
        assert_eq!(stat("f/../f"), Err(Errno::NotADirectory));
    }

    /// A removal or a rename holding an expectation acts only on the node it
    /// expects, decided under the volume's lock, and otherwise changes
    /// nothing.
    #[test]
    fn a_stale_expectation_changes_nothing() {
        let svc = ready();
        let caps = caps();
        file_with(&svc, "f", b"one");
        let stat = svc
            .stat(TEST_UID, &caps, at(&path("f")), FinalLink::Keep)
            .expect("stat");
        let wrong = node_alone(tairix_abi::FileId {
            node: stat.id.node + 1,
            ..stat.id
        });
        // The node's number on another volume, as a volume swapped in under
        // the same name would number one of its own.
        let elsewhere = node_alone(tairix_abi::FileId {
            volume: [0x5A; 16],
            ..stat.id
        });
        assert_eq!(
            svc.unlink(
                TEST_UID,
                &caps,
                at(&path("f")),
                UnlinkFlags::EXPECT_NODE,
                Some(&elsewhere)
            ),
            Err(Errno::Stale)
        );
        assert_eq!(
            svc.rename(
                TEST_UID,
                &caps,
                at(&path("f")),
                at(&path("g")),
                RenameFlags::EXPECT_NODE,
                Some(&elsewhere),
            ),
            Err(Errno::Stale)
        );
        assert_eq!(
            svc.unlink(
                TEST_UID,
                &caps,
                at(&path("f")),
                UnlinkFlags::EXPECT_NODE,
                Some(&wrong)
            ),
            Err(Errno::Stale)
        );
        let unchanged = NodeExpect {
            id: stat.id,
            content_gen: stat.content_gen,
            size: stat.size,
            modified: stat.times.modified,
            changed: stat.times.changed,
        };
        write_path(&svc, TEST_UID, &caps, &path("f"), 3, false, b"two").expect("grow");
        assert_eq!(
            svc.unlink(
                TEST_UID,
                &caps,
                at(&path("f")),
                UnlinkFlags::EXPECT_UNCHANGED,
                Some(&unchanged)
            ),
            Err(Errno::Stale)
        );
        assert_eq!(
            svc.rename(
                TEST_UID,
                &caps,
                at(&path("f")),
                at(&path("g")),
                RenameFlags::EXPECT_NODE,
                Some(&wrong)
            ),
            Err(Errno::Stale)
        );
        assert_eq!(bytes(&svc, at(&path("f"))), Ok(b"onetwo".to_vec()));
        assert_eq!(
            svc.stat(TEST_UID, &caps, at(&path("g")), FinalLink::Keep)
                .err(),
            Some(Errno::NotFound)
        );

        let its_own = node_alone(stat.id);
        svc.rename(
            TEST_UID,
            &caps,
            at(&path("f")),
            at(&path("g")),
            RenameFlags::EXPECT_NODE,
            Some(&its_own),
        )
        .expect("the node expected moves");
        svc.unlink(
            TEST_UID,
            &caps,
            at(&path("g")),
            UnlinkFlags::EXPECT_NODE,
            Some(&its_own),
        )
        .expect("the node expected goes");
    }

    /// An expectation goes with the flag naming it, and one of the node
    /// alone asks nothing else of it.
    #[test]
    fn an_expectation_its_flags_do_not_name_is_out_of_range() {
        let svc = ready();
        let caps = caps();
        file_with(&svc, "f", b"");
        let id = svc
            .stat(TEST_UID, &caps, at(&path("f")), FinalLink::Keep)
            .expect("stat")
            .id;
        let alone = node_alone(id);
        let padded = NodeExpect { size: 1, ..alone };
        let unlink = |flags, expect| svc.unlink(TEST_UID, &caps, at(&path("f")), flags, expect);
        assert_eq!(
            unlink(UnlinkFlags::empty(), Some(&alone)),
            Err(Errno::OutOfRange)
        );
        assert_eq!(
            unlink(UnlinkFlags::EXPECT_NODE, None),
            Err(Errno::OutOfRange)
        );
        assert_eq!(
            unlink(UnlinkFlags::EXPECT_UNCHANGED, None),
            Err(Errno::OutOfRange)
        );
        assert_eq!(
            unlink(UnlinkFlags::EXPECT_NODE, Some(&padded)),
            Err(Errno::OutOfRange)
        );
        let rename = |flags, expect| {
            svc.rename(
                TEST_UID,
                &caps,
                at(&path("f")),
                at(&path("g")),
                flags,
                expect,
            )
        };
        assert_eq!(
            rename(RenameFlags::empty(), Some(&alone)),
            Err(Errno::OutOfRange)
        );
        assert_eq!(
            rename(RenameFlags::EXPECT_NODE, Some(&padded)),
            Err(Errno::OutOfRange)
        );
        assert!(svc
            .stat(TEST_UID, &caps, at(&path("f")), FinalLink::Keep)
            .is_ok());
    }

    /// A path from a directory descriptor is canonicalised from where that
    /// directory stands now, however it has moved since it was opened.
    #[test]
    fn a_path_from_a_renamed_directory_descriptor_is_canonicalised_exactly() {
        let svc = ready();
        let caps = caps();
        svc.mkdir(TEST_UID, &caps, at(&path("d")), new_dir())
            .expect("mkdir");
        let held =
            own_handle(&svc, TEST_UID, &caps, &path("d"), OpenFlags::DIRECTORY).expect("opens");
        svc.rename(
            TEST_UID,
            &caps,
            at(&path("d")),
            at(&path("e")),
            RenameFlags::empty(),
            None,
        )
        .expect("rename");
        let final_name = svc.realpath(TEST_UID, &caps, beneath(&held, "x"), RealpathMode::Final);
        assert_eq!(final_name, Ok(path("e/x")));
        assert_eq!(canonical(&svc, beneath(&held, "..")), Ok(MOUNT.into()));
        assert_eq!(canonical(&svc, beneath(&held, ".")), Ok(path("e")));
        close(&svc, held);
    }

    /// `CREATE | DIRECTORY | EXCLUSIVE` makes the directory and answers it
    /// open, with no byte access; any other spelling of it makes nothing.
    #[test]
    fn creating_a_directory_answers_it_open() {
        let svc = ready();
        let caps = caps();
        let flags = OpenFlags::CREATE
            .union(OpenFlags::DIRECTORY)
            .union(OpenFlags::EXCLUSIVE);
        let opened = svc.open(TEST_UID, &caps, at(&path("n")), flags, new_dir());
        let Ok(Opened::Node {
            kind,
            node,
            projection,
            opened_as,
            at_floor,
        }) = opened
        else {
            panic!("the new directory opens");
        };
        assert_eq!(kind, NodeKind::Directory);
        let held = NodeHandle {
            node,
            projection,
            authority: Authority::Holder,
            opened_as,
            at_floor,
        };
        let who = acting(TEST_UID, &caps);
        let stat = svc.stat_node(&held, &who).expect("stats");
        assert_eq!((stat.kind, stat.mode), (FileKind::Directory, 0o755));
        svc.mkdir(TEST_UID, &caps, beneath(&held, "inner"), new_dir())
            .expect("the descriptor names its directory");
        close(&svc, held);

        assert_eq!(
            open_path(&svc, TEST_UID, &caps, &path("n"), flags).err(),
            Some(Errno::AlreadyExists)
        );
        let refused = [
            OpenFlags::CREATE.union(OpenFlags::DIRECTORY),
            flags.union(OpenFlags::READ),
            flags.union(OpenFlags::WRITE),
        ];
        for asked in refused {
            assert_eq!(
                svc.open(TEST_UID, &caps, at(&path("m")), asked, new_dir())
                    .err(),
                Some(Errno::OutOfRange),
                "{asked:?}"
            );
        }
        assert_eq!(
            svc.stat(TEST_UID, &caps, at(&path("m")), FinalLink::Keep)
                .err(),
            Some(Errno::NotFound)
        );
    }

    /// A directory no name reaches takes no new name, and has no parent.
    #[test]
    fn a_removed_held_directory_takes_no_new_name() {
        let svc = ready();
        let caps = caps();
        svc.mkdir(TEST_UID, &caps, at(&path("d")), new_dir())
            .expect("mkdir");
        file_with(&svc, "g", b"");
        let held =
            own_handle(&svc, TEST_UID, &caps, &path("d"), OpenFlags::DIRECTORY).expect("opens");
        svc.unlink(
            TEST_UID,
            &caps,
            at(&path("d")),
            UnlinkFlags::DIRECTORY,
            None,
        )
        .expect("rmdir");
        assert_eq!(
            svc.mkdir(TEST_UID, &caps, beneath(&held, "x"), new_dir()),
            Err(Errno::NotFound)
        );
        let create = OpenFlags::CREATE.union(OpenFlags::WRITE);
        assert_eq!(
            svc.open(TEST_UID, &caps, beneath(&held, "f"), create, new_file())
                .err(),
            Some(Errno::NotFound)
        );
        assert_eq!(
            svc.rename(
                TEST_UID,
                &caps,
                at(&path("g")),
                beneath(&held, "g"),
                RenameFlags::empty(),
                None
            ),
            Err(Errno::NotFound)
        );
        assert_eq!(
            svc.link(
                TEST_UID,
                &caps,
                at(&path("g")),
                beneath(&held, "g"),
                FinalLink::Keep
            ),
            Err(Errno::NotFound)
        );
        assert_eq!(
            svc.stat(TEST_UID, &caps, beneath(&held, ".."), FinalLink::Follow)
                .err(),
            Some(Errno::NotFound)
        );
        close(&svc, held);
    }

    /// A start whose projection has left the mount table is stale, and an
    /// absolute path ignores its start.
    #[test]
    fn a_start_whose_projection_has_gone_is_stale() {
        let (svc, cell) = service_over(driver(), &[]);
        let caps = caps();
        svc.mkdir(TEST_UID, &caps, at(&path("d")), new_dir())
            .expect("mkdir");
        let held =
            own_handle(&svc, TEST_UID, &caps, &path("d"), OpenFlags::DIRECTORY).expect("opens");
        let Ok(Opened::Catalog(catalog)) = svc.open(
            TEST_UID,
            &caps,
            at("/Storage"),
            OpenFlags::DIRECTORY,
            CreateMode::NONE,
        ) else {
            panic!("the catalog opens as itself");
        };
        let in_catalog = At {
            start: Start::Catalog(&catalog),
            path: "vol",
        };
        assert!(svc
            .stat(TEST_UID, &caps, in_catalog, FinalLink::Follow)
            .is_ok());
        let vfs = cell.vfs().expect("installed");
        let handle = DriverHandle::from_raw(9).expect("handle");
        vfs.mounts_write()
            .set_backing(&Path::parse("/Storage").expect("path"), handle, Vec::new())
            .expect("replaces the catalog");
        vfs.mounts_write().detach(handle).expect("detach");
        let stat = |at| {
            svc.stat(TEST_UID, &caps, at, FinalLink::Follow)
                .map(|s| s.kind)
        };
        assert_eq!(stat(beneath(&held, "x")), Err(Errno::Stale));
        assert_eq!(stat(in_catalog), Err(Errno::Stale));
        assert_eq!(stat(beneath(&held, "/")), Ok(FileKind::Directory));
        close(&svc, held);
    }

    /// `fs_access` answers as the caller, each access it is asked for judged
    /// by the node's record; asked nothing, it answers whether the node
    /// exists; under `Keep` it asks about a final link itself.
    #[test]
    fn access_answers_each_access_as_the_caller() {
        const OTHER: u32 = 1001;
        let svc = ready_with_users(&[OTHER]);
        let caps = caps();
        file_with(&svc, "f", b"x");
        svc.set_mode(TEST_UID, &caps, at(&path("f")), 0o640, FinalLink::Follow)
            .expect("chmod");
        svc.symlink(TEST_UID, &caps, "f", at(&path("l")))
            .expect("link");
        let access = |uid, name: &str, want, final_link| {
            svc.access(uid, &caps, at(&path(name)), want, final_link)
        };
        let follow = FinalLink::Follow;
        assert_eq!(access(TEST_UID, "f", Wanted::READ, follow), Ok(()));
        assert_eq!(
            access(TEST_UID, "f", Wanted::READ.union(Wanted::WRITE), follow),
            Ok(())
        );
        assert_eq!(
            access(TEST_UID, "f", Wanted::EXEC, follow),
            Err(Errno::PermissionDenied)
        );
        assert_eq!(
            access(OTHER, "f", Wanted::READ, follow),
            Err(Errno::PermissionDenied)
        );
        assert_eq!(access(OTHER, "f", Wanted::empty(), follow), Ok(()));
        assert_eq!(
            access(OTHER, "absent", Wanted::empty(), follow),
            Err(Errno::NotFound)
        );
        assert_eq!(access(OTHER, "l", Wanted::READ, FinalLink::Keep), Ok(()));
        assert_eq!(
            access(OTHER, "l", Wanted::READ, follow),
            Err(Errno::PermissionDenied)
        );
    }

    /// After the node's own verdict `fs_access` adds the projection's: no
    /// write where nothing may be written, and no program run where
    /// programs may not run.
    #[test]
    fn access_adds_the_projections_flags_to_the_nodes_verdict() {
        let caps = caps();
        let follow = FinalLink::Follow;
        let read_only = service(true, true, true);
        assert_eq!(
            read_only.access(TEST_UID, &caps, at(MOUNT), Wanted::WRITE, follow),
            Err(Errno::ReadOnly)
        );
        assert_eq!(
            read_only.access(TEST_UID, &caps, at(MOUNT), Wanted::READ, follow),
            Ok(())
        );
        assert_eq!(
            read_only.access(TEST_UID, &caps, at("/Storage"), Wanted::WRITE, follow),
            Err(Errno::ReadOnly)
        );

        let mut fs = driver();
        let root = fs.root();
        fs.create(root, b"run", NodeKind::RegularFile, &PLANTED)
            .expect("a program");
        fs.create(root, b"dir", NodeKind::Directory, &PLANTED)
            .expect("a directory");
        let noexec = serving(removables(&["/Storage/usb"]), alloc::vec![(11, fs)]);
        let access = |name: &str, want| {
            noexec.access(
                TEST_UID,
                &caps,
                at(&alloc::format!("/Storage/usb/{name}")),
                want,
                follow,
            )
        };
        assert_eq!(access("run", Wanted::EXEC), Err(Errno::PermissionDenied));
        assert_eq!(access("run", Wanted::READ), Ok(()));
        assert_eq!(access("dir", Wanted::EXEC), Ok(()));
    }

    /// A mode change through a descriptor is its owner's, and never one a
    /// grant makes.
    #[test]
    fn a_mode_change_through_a_descriptor_is_never_a_grants() {
        let svc = ready();
        let caps = caps();
        file_with(&svc, "f", b"x");
        let held =
            own_handle(&svc, TEST_UID, &caps, &path("f"), OpenFlags::empty()).expect("opens");
        let own = acting(TEST_UID, &caps);
        assert_eq!(svc.set_mode_node(&held, &own, 0o600), Ok(()));
        assert_eq!(
            svc.stat(TEST_UID, &caps, at(&path("f")), FinalLink::Follow)
                .map(|stat| stat.mode),
            Ok(0o600)
        );
        let granted = Acting {
            uid: TEST_UID,
            caps: &caps,
            granted: true,
        };
        assert_eq!(
            svc.set_mode_node(&held, &granted, 0o644),
            Err(Errno::PermissionDenied)
        );
        assert_eq!(
            svc.set_mode_node(&held, &own, 0o10_0644),
            Err(Errno::OutOfRange)
        );
        close(&svc, held);
    }
}
