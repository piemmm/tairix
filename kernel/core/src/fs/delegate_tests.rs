//! Behavioural tests for driver delegation: a path resolved beneath a
//! driver-backed projection, every node judged by the record its driver
//! stores, served through the production filesystem service over the
//! in-memory driver.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{
    FilesystemRead, FilesystemSecurity, FilesystemWrite, MountFlags, NodeId, NodeKind,
    NodeSecurity, NodeTimes,
};
use tairix_abi::driver::{DriverError, DriverHandle};
use tairix_abi::fs::{OpenFlags, RealpathMode};
use tairix_abi::time::Time64;
use tairix_abi::{CapabilityId, Errno, FileKind, RenameFlags, UnlinkFlags};
use tairix_caps::CapabilitySet;
use tairix_kernel_sec::{GroupId, UserId};

use crate::fs::delegate::SYMLINK_HOP_MAX;
use crate::fs::memfs::{RwMockFs, ADMIN_GID, ADMIN_RECORD, ADMIN_UID};
use crate::fs::test_volume::{
    at, caps, create_path, mounted, new_dir, new_file, open_path, path, read_path, serve,
    truncate_path, volume_handle, write_path, Account, Serving, MOUNT,
};
use crate::fs::{
    Attachment, CreateMode, FilesystemService, FinalLink, Path, ReaddirEntry, Vfs, VfsError,
    MAX_COMPONENT_LEN, MAX_PATH_COMPONENTS,
};

/// The account the in-memory driver's root and fixture records belong to.
const ADMIN: Account<'static> = Account {
    uid: ADMIN_UID,
    gid: ADMIN_GID,
    groups: &[],
};

/// An account no fixture record names.
const OTHER: Account<'static> = Account {
    uid: 9,
    gid: 9,
    groups: &[],
};

/// A creator whose group is not numbered as its uid is.
const USER: Account<'static> = Account {
    uid: 7,
    gid: 3,
    groups: &[],
};

/// A group a setgid directory hands down.
const SHARED_GID: u32 = 50;

/// [`USER`], a member of [`SHARED_GID`] too.
const IN_SHARED: Account<'static> = Account {
    groups: &[SHARED_GID],
    ..USER
};

/// The stamp every node of [`docs_volume`] reports, so a listing is seen to
/// carry it through unchanged.
const MOCK_MODIFIED: Time64 = Time64::from_secs(1_234_567);

const MOCK_TIMES: NodeTimes = NodeTimes {
    created: Time64::UNIX_EPOCH,
    modified: MOCK_MODIFIED,
    accessed: Time64::UNIX_EPOCH,
    changed: Time64::UNIX_EPOCH,
};

const KERNEL_BODY: &[u8] = b"ELF\0";
const README_BODY: &[u8] = b"hello world";
const SECRET_BODY: &[u8] = b"top secret";

fn p(text: &str) -> Path {
    Path::parse(text).expect("valid path")
}

/// A capability set holding `CAP_AUDIT_READ`.
fn auditing() -> CapabilitySet {
    let mut caps = CapabilitySet::empty();
    caps.insert(CapabilityId::AUDIT_READ);
    caps
}

fn mk_dir(fs: &mut RwMockFs, parent: NodeId, name: &str) -> NodeId {
    fs.create(parent, name.as_bytes(), NodeKind::Directory, &ADMIN_RECORD)
        .expect("mkdir in mock")
}

fn mk_file(fs: &mut RwMockFs, parent: NodeId, name: &str, body: &[u8]) {
    let file = fs
        .create(
            parent,
            name.as_bytes(),
            NodeKind::RegularFile,
            &ADMIN_RECORD,
        )
        .expect("create in mock");
    fs.write_at(file, 0, body).expect("write in mock");
}

fn mk_link(fs: &mut RwMockFs, parent: NodeId, name: &str, target: &str) {
    fs.create_link(parent, name.as_bytes(), target.as_bytes(), &ADMIN_RECORD)
        .expect("create_link in mock");
}

/// A volume holding a directory and two files, every node stamped
/// [`MOCK_TIMES`]:
///
/// ```text
/// /                 (root, dir)
/// ├── docs/         (dir)
/// │   └── readme.txt  "hello world"
/// └── kernel.img    "ELF\0"
/// ```
fn docs_volume() -> RwMockFs {
    let mut fs = RwMockFs::new().with_times(MOCK_TIMES);
    let root = fs.root();
    let docs = mk_dir(&mut fs, root, "docs");
    mk_file(&mut fs, docs, "readme.txt", README_BODY);
    mk_file(&mut fs, root, "kernel.img", KERNEL_BODY);
    fs
}

/// A volume holding `secret.txt`, owned by [`USER`], owner-only and gated on
/// `CAP_AUDIT_READ`.
fn secret_volume() -> RwMockFs {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let mut record = NodeSecurity::new(0o600, USER.uid, 0);
    record.required_cap = Some(CapabilityId::AUDIT_READ);
    let file = fs
        .create(root, b"secret.txt", NodeKind::RegularFile, &record)
        .expect("create in mock");
    fs.write_at(file, 0, SECRET_BODY).expect("write in mock");
    fs
}

/// A default-layout VFS whose root projection the test volume backs, as an
/// installed system's root volume backs `/`.
fn root_backed() -> Vfs {
    let vfs = Vfs::with_default_layout(UserId(ADMIN_UID), GroupId(ADMIN_GID));
    {
        let mut mounts = vfs.mounts_write();
        mounts
            .attach(Attachment::new(
                volume_handle(),
                None,
                MountFlags::default(),
            ))
            .expect("attach the root volume");
        mounts
            .back_root(volume_handle())
            .expect("back the root mount");
    }
    vfs
}

/// The service over `fs` backing `/`, with `accounts`.
fn at_root(fs: RwMockFs, accounts: &[Account<'_>]) -> Serving<RwMockFs> {
    serve(root_backed(), alloc::vec![(volume_handle(), fs)], accounts)
}

/// The handle a second volume is attached under.
fn second_volume() -> DriverHandle {
    DriverHandle::from_raw(11).expect("non-zero handle")
}

/// [`at_root`] over an empty volume, with a second at `/Storage/usb0`.
fn two_volumes(accounts: &[Account<'_>]) -> Serving<RwMockFs> {
    let vfs = root_backed();
    {
        let mut mounts = vfs.mounts_write();
        mounts
            .attach(Attachment::new(
                second_volume(),
                None,
                MountFlags::default(),
            ))
            .expect("attach the second volume");
        mounts
            .mount(
                p("/Storage/usb0"),
                MountFlags::default(),
                Some(second_volume()),
            )
            .expect("mount the second volume");
    }
    let volumes = alloc::vec![
        (volume_handle(), RwMockFs::new()),
        (second_volume(), RwMockFs::new()),
    ];
    serve(vfs, volumes, accounts)
}

/// The record the first volume stores for `name` at its root.
fn record_of(serving: &Serving<RwMockFs>, name: &[u8]) -> NodeSecurity {
    let mut fs = serving.drivers[0].lock();
    let root = fs.root();
    let node = fs.lookup(root, name).expect("the name resolves");
    fs.security(node).expect("the node has a record")
}

/// Gate the node `name` names at the first volume's root on
/// `CAP_AUDIT_READ`.
fn gate_child(serving: &Serving<RwMockFs>, name: &[u8]) {
    let mut fs = serving.drivers[0].lock();
    let root = fs.root();
    let node = fs.lookup(root, name).expect("node resolves");
    let mut sec = fs.security(node).expect("record exists");
    sec.required_cap = Some(CapabilityId::AUDIT_READ);
    fs.set_security(node, sec).expect("gate installed");
}

/// Each entry's kind and name, as a listing reports them.
fn kinds(entries: &[ReaddirEntry]) -> Vec<(FileKind, &str)> {
    entries
        .iter()
        .map(|entry| (entry.kind, entry.name.as_str()))
        .collect()
}

/// The whole listing of `path`, as `uid` asks it under `final_link`.
fn listing(
    svc: &dyn FilesystemService,
    uid: u32,
    path: &str,
    final_link: FinalLink,
) -> Result<Vec<ReaddirEntry>, Errno> {
    svc.readdir_bounded(uid, &caps(), at(path), final_link, usize::MAX)
}

/// The bytes `path` holds, read as `uid` through a descriptor of its own.
fn read_text(svc: &dyn FilesystemService, uid: u32, path: &str) -> String {
    let mut buf = [0u8; 64];
    let n = read_path(svc, uid, &caps(), path, 0, &mut buf).expect("read through the link");
    String::from_utf8(buf[..n].to_vec()).expect("utf-8 body")
}

#[test]
fn delegated_read_resolves_through_subdir() {
    let serving = mounted(docs_volume(), true, &[ADMIN]);
    assert_eq!(
        read_text(&serving.service, ADMIN_UID, &path("docs/readme.txt")).as_bytes(),
        README_BODY
    );
}

#[test]
fn delegated_read_honours_offset_and_eof() {
    let serving = mounted(docs_volume(), true, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let readme = path("docs/readme.txt");
    let mut buf = [0u8; 32];
    let n = read_path(svc, ADMIN_UID, &caps, &readme, 6, &mut buf).expect("read");
    assert_eq!(&buf[..n], b"world");
    // Reading at EOF yields zero bytes.
    let end = README_BODY.len() as u64;
    assert_eq!(
        read_path(svc, ADMIN_UID, &caps, &readme, end, &mut buf),
        Ok(0)
    );
}

/// Canonicalisation spells a `/`-view path: the mount point followed by the
/// canonical remainder, and the mount point itself for the mount point.
#[test]
fn delegated_realpath_spells_a_view_path_under_the_mount_point() {
    let serving = mounted(docs_volume(), true, &[ADMIN]);
    let caps = caps();
    let canonical = |name: &str, mode| serving.service.realpath(ADMIN_UID, &caps, at(name), mode);
    let readme = path("docs/readme.txt");
    assert_eq!(canonical(&readme, RealpathMode::Existing), Ok(readme));
    // The mount point is its own canonical name: the walk lands on the
    // projected root, which contributes no component of its own.
    assert_eq!(
        canonical(MOUNT, RealpathMode::Existing),
        Ok(String::from(MOUNT))
    );
    // A vacant final name is refused under the strict reading and carried
    // into the answer under the tolerant one.
    let vacant = path("docs/absent");
    assert_eq!(
        canonical(&vacant, RealpathMode::Existing),
        Err(Errno::NotFound)
    );
    assert_eq!(canonical(&vacant, RealpathMode::Final), Ok(vacant));
    // Only `Missing` names a path whose *ancestor* is absent too.
    let deep = path("gone/away/leaf");
    assert_eq!(canonical(&deep, RealpathMode::Final), Err(Errno::NotFound));
    assert_eq!(canonical(&deep, RealpathMode::Missing), Ok(deep));

    // A directory the caller may not search hides what is under it from
    // canonicalisation exactly as it does from a read.
    let mut fs = docs_volume();
    fs.set_root_security(NodeSecurity::new(0o600, ADMIN_UID, ADMIN_GID));
    let closed = mounted(fs, true, &[ADMIN]);
    assert_eq!(
        closed.service.realpath(
            ADMIN_UID,
            &caps,
            at(&path("gone/away/leaf")),
            RealpathMode::Missing
        ),
        Err(Errno::PermissionDenied)
    );
}

#[test]
fn delegated_list_of_mount_point_lists_driver_root() {
    let serving = mounted(docs_volume(), true, &[ADMIN]);
    let entries = listing(&serving.service, ADMIN_UID, MOUNT, FinalLink::Follow)
        .expect("list the mount root");
    assert_eq!(
        kinds(&entries),
        [
            (FileKind::Directory, "docs"),
            (FileKind::Regular, "kernel.img")
        ]
    );
}

#[test]
fn delegated_list_of_subdir() {
    let serving = mounted(docs_volume(), true, &[ADMIN]);
    let entries = listing(
        &serving.service,
        ADMIN_UID,
        &path("docs"),
        FinalLink::Follow,
    )
    .expect("list the subdirectory");
    let seen: Vec<(FileKind, u64, Time64, &str)> = entries
        .iter()
        .map(|entry| (entry.kind, entry.size, entry.modified, entry.name.as_str()))
        .collect();
    // The listing carries the child's own size and modification stamp,
    // read once by the driver.
    assert_eq!(
        seen,
        [(
            FileKind::Regular,
            README_BODY.len() as u64,
            MOCK_MODIFIED,
            "readme.txt"
        )]
    );
}

#[test]
fn delegated_stat_reports_kind_size_and_the_nodes_own_record() {
    let mut fs = docs_volume();
    let root = fs.root();
    let kernel = fs.lookup(root, b"kernel.img").expect("the file");
    fs.set_security(kernel, NodeSecurity::new(0o640, ADMIN_UID, ADMIN_GID))
        .expect("a record of its own");
    let serving = mounted(fs, true, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let file = svc
        .stat(ADMIN_UID, &caps, at(&path("kernel.img")), FinalLink::Follow)
        .expect("stat file");
    assert_eq!(file.kind, FileKind::Regular);
    assert_eq!(file.size, KERNEL_BODY.len() as u64);
    // The node's own record, not the floor's above it.
    assert_eq!((file.uid, file.mode), (ADMIN_UID, 0o640));

    let dir = svc
        .stat(ADMIN_UID, &caps, at(&path("docs")), FinalLink::Follow)
        .expect("stat dir");
    assert_eq!(dir.kind, FileKind::Directory);
    assert_eq!(dir.size, 0);
}

#[test]
fn delegated_read_of_directory_is_is_a_directory() {
    let serving = mounted(docs_volume(), true, &[ADMIN]);
    let mut buf = [0u8; 8];
    assert_eq!(
        read_path(
            &serving.service,
            ADMIN_UID,
            &caps(),
            &path("docs"),
            0,
            &mut buf
        ),
        Err(Errno::IsADirectory)
    );
}

#[test]
fn delegated_list_of_file_is_not_a_directory() {
    let serving = mounted(docs_volume(), true, &[ADMIN]);
    assert_eq!(
        listing(
            &serving.service,
            ADMIN_UID,
            &path("kernel.img"),
            FinalLink::Follow
        ),
        Err(Errno::NotADirectory)
    );
}

#[test]
fn delegated_missing_child_is_not_found() {
    let serving = mounted(docs_volume(), true, &[ADMIN]);
    let mut buf = [0u8; 8];
    assert_eq!(
        read_path(
            &serving.service,
            ADMIN_UID,
            &caps(),
            &path("docs/nope"),
            0,
            &mut buf
        ),
        Err(Errno::NotFound)
    );
}

#[test]
fn delegation_on_non_backed_mount_is_not_found() {
    // `/Users` is not driver-backed, so no driver serves it.
    let serving = mounted(docs_volume(), true, &[ADMIN]);
    let mut buf = [0u8; 8];
    assert_eq!(
        read_path(
            &serving.service,
            ADMIN_UID,
            &caps(),
            "/Users/admin/x",
            0,
            &mut buf
        ),
        Err(Errno::NotFound)
    );
}

#[test]
fn delegated_traversal_enforces_search_permission_on_the_floor() {
    // A floor only its owner may search: another user is refused on the
    // way down, the owner is not.
    let mut fs = docs_volume();
    fs.set_root_security(NodeSecurity::new(0o700, ADMIN_UID, ADMIN_GID));
    let serving = mounted(fs, true, &[ADMIN, OTHER]);
    let svc = &serving.service;
    let readme = path("docs/readme.txt");
    let mut buf = [0u8; 8];
    assert_eq!(
        read_path(svc, OTHER.uid, &caps(), &readme, 0, &mut buf),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(read_text(svc, ADMIN_UID, &readme).as_bytes(), README_BODY);
}

#[test]
fn device_fault_surfaces_as_io() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    mk_file(&mut fs, root, "x", b"abc");
    let serving = mounted(
        fs.with_reads_refused(DriverError::DeviceFault),
        true,
        &[ADMIN],
    );
    let mut buf = [0u8; 8];
    assert_eq!(
        read_path(
            &serving.service,
            ADMIN_UID,
            &caps(),
            &path("x"),
            0,
            &mut buf
        ),
        Err(VfsError::Io.to_errno())
    );
}

#[test]
fn a_listed_name_no_path_could_hold_surfaces_as_io() {
    for listed in [
        &[0xff, 0xff][..],
        b"",
        b".",
        b"..",
        b"../escape",
        b"a/b",
        b"nul\0",
    ] {
        let mut fs = RwMockFs::new();
        let root = fs.root();
        mk_file(&mut fs, root, "x", b"");
        let serving = mounted(fs.with_entries_listed_as(listed), true, &[ADMIN]);
        assert_eq!(
            listing(&serving.service, ADMIN_UID, MOUNT, FinalLink::Follow),
            Err(VfsError::Io.to_errno()),
            "{listed:?}"
        );
    }
}

#[test]
fn delegated_create_then_write_and_read_back() {
    let serving = mounted(RwMockFs::new(), false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let notes = path("notes.txt");
    create_path(svc, ADMIN_UID, &caps, &notes, new_file()).expect("create");
    assert_eq!(
        write_path(svc, ADMIN_UID, &caps, &notes, 0, false, b"hi there"),
        Ok(8)
    );
    assert_eq!(read_text(svc, ADMIN_UID, &notes), "hi there");
}

#[test]
fn delegated_mkdir_then_create_inside() {
    let serving = mounted(RwMockFs::new(), false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    svc.mkdir(ADMIN_UID, &caps, at(&path("sub")), new_dir())
        .expect("mkdir");
    let inner = path("sub/inner.bin");
    create_path(svc, ADMIN_UID, &caps, &inner, new_file()).expect("create inside");
    write_path(svc, ADMIN_UID, &caps, &inner, 0, false, b"nested").expect("write inside");
    let entries = listing(svc, ADMIN_UID, &path("sub"), FinalLink::Follow).expect("list");
    assert_eq!(kinds(&entries), [(FileKind::Regular, "inner.bin")]);
}

#[test]
fn delegated_truncate_changes_size() {
    let serving = mounted(RwMockFs::new(), false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let file = path("t.bin");
    create_path(svc, ADMIN_UID, &caps, &file, new_file()).expect("create");
    write_path(svc, ADMIN_UID, &caps, &file, 0, false, b"0123456789").expect("write");
    truncate_path(svc, ADMIN_UID, &caps, &file, 4).expect("truncate");
    assert_eq!(
        svc.stat(ADMIN_UID, &caps, at(&file), FinalLink::Follow)
            .expect("stat")
            .size,
        4
    );
}

#[test]
fn delegated_remove_unlinks() {
    let serving = mounted(RwMockFs::new(), false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let file = path("gone.txt");
    create_path(svc, ADMIN_UID, &caps, &file, new_file()).expect("create");
    svc.unlink(ADMIN_UID, &caps, at(&file), UnlinkFlags::empty(), None)
        .expect("remove");
    let mut buf = [0u8; 4];
    assert_eq!(
        read_path(svc, ADMIN_UID, &caps, &file, 0, &mut buf),
        Err(Errno::NotFound)
    );
}

#[test]
fn delegated_create_existing_is_already_exists() {
    let serving = mounted(RwMockFs::new(), false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let file = path("dup.txt");
    create_path(svc, ADMIN_UID, &caps, &file, new_file()).expect("create");
    assert_eq!(
        create_path(svc, ADMIN_UID, &caps, &file, new_file()),
        Err(Errno::AlreadyExists)
    );
}

#[test]
fn delegated_write_to_directory_is_is_a_directory() {
    let serving = mounted(RwMockFs::new(), false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    svc.mkdir(ADMIN_UID, &caps, at(&path("d")), new_dir())
        .expect("mkdir");
    assert_eq!(
        write_path(svc, ADMIN_UID, &caps, &path("d"), 0, false, b"x"),
        Err(Errno::IsADirectory)
    );
}

#[test]
fn delegated_remove_non_empty_directory_is_not_empty() {
    let serving = mounted(RwMockFs::new(), false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    svc.mkdir(ADMIN_UID, &caps, at(&path("d")), new_dir())
        .expect("mkdir");
    create_path(svc, ADMIN_UID, &caps, &path("d/f"), new_file()).expect("create child");
    assert_eq!(
        svc.unlink(ADMIN_UID, &caps, at(&path("d")), UnlinkFlags::empty(), None),
        Err(Errno::NotEmpty)
    );
}

#[test]
fn delegated_dir_only_remove_of_a_file_is_not_a_directory() {
    // The atomic `rmdir` posture: a directory-only removal reaching a file
    // is refused in the same locked walk that would remove it, and the file
    // survives.
    let serving = mounted(RwMockFs::new(), false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let file = path("plain.txt");
    create_path(svc, ADMIN_UID, &caps, &file, new_file()).expect("create");
    assert_eq!(
        svc.unlink(ADMIN_UID, &caps, at(&file), UnlinkFlags::DIRECTORY, None),
        Err(Errno::NotADirectory)
    );
    svc.stat(ADMIN_UID, &caps, at(&file), FinalLink::Follow)
        .expect("file survives");
}

#[test]
fn delegated_dir_only_remove_of_an_empty_directory_succeeds() {
    let serving = mounted(RwMockFs::new(), false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let dir = path("d");
    svc.mkdir(ADMIN_UID, &caps, at(&dir), new_dir())
        .expect("mkdir");
    svc.unlink(ADMIN_UID, &caps, at(&dir), UnlinkFlags::DIRECTORY, None)
        .expect("dir-only remove of an empty directory");
    assert_eq!(
        svc.stat(ADMIN_UID, &caps, at(&dir), FinalLink::Follow),
        Err(Errno::NotFound)
    );
}

#[test]
fn delegated_write_on_read_only_mount_is_read_only() {
    let serving = mounted(RwMockFs::new(), true, &[ADMIN]);
    assert_eq!(
        create_path(&serving.service, ADMIN_UID, &caps(), &path("x"), new_file()),
        Err(Errno::ReadOnly)
    );
}

#[test]
fn delegated_create_without_write_permission_is_denied() {
    // The floor is the admin's `0o755`: another user may search it but not
    // write it.
    let serving = mounted(RwMockFs::new(), false, &[ADMIN, OTHER]);
    assert_eq!(
        create_path(&serving.service, OTHER.uid, &caps(), &path("x"), new_file()),
        Err(Errno::PermissionDenied)
    );
}

/// A mount root exists, so making it is refused as making any existing
/// directory is, and no name of a directory on its volume holds it to remove.
#[test]
fn a_mount_root_exists_and_no_removal_names_it() {
    let serving = mounted(RwMockFs::new(), false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    assert_eq!(
        svc.mkdir(ADMIN_UID, &caps, at(MOUNT), new_dir()),
        Err(Errno::AlreadyExists)
    );
    assert_eq!(
        svc.unlink(ADMIN_UID, &caps, at(MOUNT), UnlinkFlags::DIRECTORY, None),
        Err(VfsError::InvalidPath.to_errno())
    );
}

#[test]
fn delegated_create_of_legacy_top_level_name_is_allowed() {
    // With the writable root volume backing `/`, making a legacy POSIX
    // top-level name is *not* refused by the VFS: the OS never authors these
    // names, but it does not police a user's own request — a top-level
    // create is governed by write permission on the root directory like any
    // other. Rename is likewise not policed.
    let serving = at_root(RwMockFs::new(), &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    for name in ["/etc", "/home", "/usr", "/bin", "/tmp", "/dev", "/proc"] {
        svc.mkdir(ADMIN_UID, &caps, at(name), new_dir())
            .unwrap_or_else(|e| panic!("mkdir of {name} is allowed, got {e:?}"));
    }
    svc.mkdir(ADMIN_UID, &caps, at("/Scratch"), new_dir())
        .expect("create a renameable source");
    svc.rename(
        ADMIN_UID,
        &caps,
        at("/Scratch"),
        at("/var"),
        RenameFlags::empty(),
        None,
    )
    .expect("a rename into a legacy top-level name is allowed");
}

#[test]
fn delegated_rename_across_two_backed_mounts_is_cross_volume() {
    // A rename cannot preserve a node's identity across two independent
    // backings, so it is refused with the dedicated `CrossVolume` error
    // (the EXDEV-equivalent `mv` falls back to copy-then-remove on) —
    // never a generic path error a caller could not act on.
    let serving = two_volumes(&[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    svc.mkdir(ADMIN_UID, &caps, at("/Scratch"), new_dir())
        .expect("create a renameable source on the root volume");
    assert_eq!(
        svc.rename(
            ADMIN_UID,
            &caps,
            at("/Scratch"),
            at("/Storage/usb0/Scratch"),
            RenameFlags::empty(),
            None
        ),
        Err(Errno::CrossVolume)
    );
}

#[test]
fn delegated_link_gives_one_node_a_second_name() {
    // The defining property at the VFS seam: both names resolve to one node,
    // the count rises, and nothing was copied.
    let serving = at_root(RwMockFs::new(), &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    create_path(svc, ADMIN_UID, &caps, "/file", new_file()).expect("create the node");
    write_path(svc, ADMIN_UID, &caps, "/file", 0, false, b"body").expect("write it");

    svc.link(ADMIN_UID, &caps, at("/file"), at("/alias"), FinalLink::Keep)
        .expect("add a second name");
    let first = svc
        .stat(ADMIN_UID, &caps, at("/file"), FinalLink::Keep)
        .expect("stat the first name");
    let second = svc
        .stat(ADMIN_UID, &caps, at("/alias"), FinalLink::Keep)
        .expect("stat the second name");
    assert_eq!(first.id, second.id);
    assert_eq!(second.nlink, 2);

    // One node, so a write through either name is one file's bytes.
    assert_eq!(read_text(svc, ADMIN_UID, "/alias"), "body");
}

#[test]
fn delegated_link_refuses_a_directory_before_touching_the_new_parent() {
    // The refusal is the VFS's, not each driver's: the tree staying a tree is
    // what makes the resolver's physical `..` well-defined.
    let serving = at_root(RwMockFs::new(), &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    svc.mkdir(ADMIN_UID, &caps, at("/dir"), new_dir())
        .expect("create a directory");
    assert_eq!(
        svc.link(ADMIN_UID, &caps, at("/dir"), at("/alias"), FinalLink::Keep),
        Err(Errno::IsADirectory)
    );
    assert_eq!(
        svc.stat(ADMIN_UID, &caps, at("/alias"), FinalLink::Keep),
        Err(Errno::NotFound)
    );
}

#[test]
fn delegated_link_takes_its_posture_from_the_caller_for_the_existing_name() {
    // `Keep` is POSIX `link()` (the link itself gains the name) and `Follow`
    // is `linkat(AT_SYMLINK_FOLLOW)` (its target does). The *new* name is
    // never followed under either.
    let serving = at_root(RwMockFs::new(), &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    create_path(svc, ADMIN_UID, &caps, "/target", new_file()).expect("create the target");
    svc.symlink(ADMIN_UID, &caps, "/target", at("/sym"))
        .expect("create the symbolic link");

    svc.link(ADMIN_UID, &caps, at("/sym"), at("/kept"), FinalLink::Keep)
        .expect("name the link itself");
    assert_eq!(
        svc.stat(ADMIN_UID, &caps, at("/kept"), FinalLink::Keep)
            .expect("stat")
            .kind,
        FileKind::Symlink
    );

    svc.link(
        ADMIN_UID,
        &caps,
        at("/sym"),
        at("/followed"),
        FinalLink::Follow,
    )
    .expect("name what the link names");
    assert_eq!(
        svc.stat(ADMIN_UID, &caps, at("/followed"), FinalLink::Keep)
            .expect("stat")
            .kind,
        FileKind::Regular
    );
}

#[test]
fn delegated_link_refuses_a_taken_name_and_a_missing_source() {
    let serving = at_root(RwMockFs::new(), &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    create_path(svc, ADMIN_UID, &caps, "/file", new_file()).expect("create the node");
    create_path(svc, ADMIN_UID, &caps, "/taken", new_file()).expect("create the occupant");
    assert_eq!(
        svc.link(ADMIN_UID, &caps, at("/file"), at("/taken"), FinalLink::Keep),
        Err(Errno::AlreadyExists)
    );
    assert_eq!(
        svc.link(
            ADMIN_UID,
            &caps,
            at("/absent"),
            at("/alias"),
            FinalLink::Keep
        ),
        Err(Errno::NotFound)
    );
    // Neither refusal raised the count or left a name behind.
    assert_eq!(
        svc.stat(ADMIN_UID, &caps, at("/file"), FinalLink::Keep)
            .expect("stat")
            .nlink,
        1
    );
}

#[test]
fn delegated_link_reports_a_format_that_holds_one_name_per_node() {
    // The permanent format limit a caller can tell apart from a structural
    // refusal, exactly as `symlink` reports one.
    let serving = at_root(RwMockFs::new().without_links(), &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    // The existing name resolves, so the refusal is the format's own rather
    // than a missing source.
    create_path(svc, ADMIN_UID, &caps, "/file", new_file())
        .expect("the format creates ordinary files perfectly well");
    assert_eq!(
        svc.link(ADMIN_UID, &caps, at("/file"), at("/alias"), FinalLink::Keep),
        Err(Errno::NotSupported)
    );
}

#[test]
fn delegated_link_at_the_formats_name_ceiling_fails_closed() {
    // The per-node count is a fixed on-disk bound, not a capacity to grow:
    // one more name fails closed with its own errno rather than wrapping a
    // count whose zero would free storage a live name still reaches.
    let serving = at_root(RwMockFs::new(), &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    create_path(svc, ADMIN_UID, &caps, "/file", new_file()).expect("create the node");
    {
        let mut fs = serving.drivers[0].lock();
        let root = fs.root();
        let node = fs.lookup(root, b"file").expect("the node");
        fs.set_link_count(node, u32::MAX);
    }
    assert_eq!(
        svc.link(ADMIN_UID, &caps, at("/file"), at("/alias"), FinalLink::Keep),
        Err(Errno::TooManyLinks)
    );
    assert_eq!(
        svc.stat(ADMIN_UID, &caps, at("/alias"), FinalLink::Keep),
        Err(Errno::NotFound),
        "nothing was created"
    );
}

#[test]
fn delegated_link_across_two_backed_mounts_is_cross_volume() {
    // A hard link is a second directory entry for one inode, and a directory
    // entry addresses an inode in its own backing — so the pair cannot span
    // two volumes, and the refusal is the same dedicated `CrossVolume` a
    // rename gives rather than a generic path error.
    let serving = two_volumes(&[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    create_path(svc, ADMIN_UID, &caps, "/file", new_file())
        .expect("create a linkable node on the root volume");
    assert_eq!(
        svc.link(
            ADMIN_UID,
            &caps,
            at("/file"),
            at("/Storage/usb0/alias"),
            FinalLink::Keep
        ),
        Err(Errno::CrossVolume)
    );
}

/// The node's own record is the one judged: an owner-only file gated on a
/// capability refuses a principal that neither owns it nor holds the
/// capability, though that principal owns the floor above it.
#[test]
fn a_read_honours_the_nodes_own_owner_and_capability_gate() {
    let serving = mounted(secret_volume(), true, &[ADMIN, USER]);
    let mut buf = [0u8; 32];
    assert_eq!(
        read_path(
            &serving.service,
            ADMIN_UID,
            &caps(),
            &path("secret.txt"),
            0,
            &mut buf
        ),
        Err(Errno::PermissionDenied)
    );
}

#[test]
fn a_read_is_allowed_the_owner_holding_the_capability() {
    let serving = mounted(secret_volume(), true, &[USER]);
    let mut buf = [0u8; 32];
    let n = read_path(
        &serving.service,
        USER.uid,
        &auditing(),
        &path("secret.txt"),
        0,
        &mut buf,
    )
    .expect("the owner holding the capability reads it");
    assert_eq!(&buf[..n], SECRET_BODY);
}

#[test]
fn a_stat_reports_the_nodes_own_record() {
    let serving = mounted(secret_volume(), true, &[USER]);
    let info = serving
        .service
        .stat(
            USER.uid,
            &auditing(),
            at(&path("secret.txt")),
            FinalLink::Follow,
        )
        .expect("stat");
    assert_eq!(info.kind, FileKind::Regular);
    assert_eq!(info.size, SECRET_BODY.len() as u64);
    assert_eq!((info.uid, info.mode), (USER.uid, 0o600));
}

#[test]
fn a_listing_of_the_mount_root_lists_the_driver_root() {
    // The driver root is `0o755`, world-readable, so listing it is allowed.
    let serving = mounted(secret_volume(), true, &[ADMIN]);
    let entries =
        listing(&serving.service, ADMIN_UID, MOUNT, FinalLink::Follow).expect("list the root");
    assert_eq!(kinds(&entries), [(FileKind::Regular, "secret.txt")]);
}

/// A create is judged by the record of the directory it lands in: a floor
/// another owns, owner-only, refuses everyone but that owner.
#[test]
fn a_create_honours_the_record_of_its_parent() {
    let mut fs = RwMockFs::new();
    fs.set_root_security(NodeSecurity::new(0o700, USER.uid, 0));
    let serving = mounted(fs, false, &[ADMIN, USER]);
    let svc = &serving.service;
    let caps = caps();
    assert_eq!(
        create_path(svc, ADMIN_UID, &caps, &path("b.txt"), new_file()),
        Err(Errno::PermissionDenied)
    );
    create_path(svc, USER.uid, &caps, &path("c.txt"), new_file())
        .expect("the directory's owner creates in it");
}

#[test]
fn a_set_mode_by_the_owner_rewrites_only_the_mode() {
    let serving = mounted(RwMockFs::new(), false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let file = path("a.txt");
    create_path(svc, ADMIN_UID, &caps, &file, new_file()).expect("create");

    svc.set_mode(ADMIN_UID, &caps, at(&file), 0o640, FinalLink::Follow)
        .expect("owner chmod");

    // The mode changed; ownership and the (absent) capability gate did not.
    let stat = svc
        .stat(ADMIN_UID, &caps, at(&file), FinalLink::Follow)
        .expect("stat");
    assert_eq!(
        (stat.mode, stat.uid, stat.gid),
        (0o640, ADMIN_UID, ADMIN_GID)
    );
    assert_eq!(record_of(&serving, b"a.txt").required_cap, None);
}

#[test]
fn a_set_mode_by_a_non_owner_is_denied_even_with_write_access() {
    let serving = mounted(RwMockFs::new(), false, &[ADMIN, OTHER]);
    let svc = &serving.service;
    let caps = caps();
    let file = path("a.txt");
    create_path(svc, ADMIN_UID, &caps, &file, new_file()).expect("create");
    // World-writable: write access must still not grant chmod.
    svc.set_mode(ADMIN_UID, &caps, at(&file), 0o777, FinalLink::Follow)
        .expect("owner opens the file up");

    assert_eq!(
        svc.set_mode(OTHER.uid, &caps, at(&file), 0o600, FinalLink::Follow),
        Err(Errno::PermissionDenied)
    );
    // The refused change did not land.
    assert_eq!(
        svc.stat(ADMIN_UID, &caps, at(&file), FinalLink::Follow)
            .expect("stat")
            .mode,
        0o777
    );
}

#[test]
fn a_set_mode_honours_the_nodes_capability_gate() {
    let serving = mounted(RwMockFs::new(), false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let file = path("a.txt");
    create_path(svc, ADMIN_UID, &caps, &file, new_file()).expect("create");
    gate_child(&serving, b"a.txt");

    // The owner without the capability is refused; with it, allowed.
    assert_eq!(
        svc.set_mode(ADMIN_UID, &caps, at(&file), 0o600, FinalLink::Follow),
        Err(Errno::PermissionDenied)
    );
    svc.set_mode(ADMIN_UID, &auditing(), at(&file), 0o600, FinalLink::Follow)
        .expect("gated owner chmod");
    // The gate itself survived the mode change.
    let after = record_of(&serving, b"a.txt");
    assert_eq!(after.required_cap, Some(CapabilityId::AUDIT_READ));
    assert_eq!(after.mode, 0o600);
}

#[test]
fn a_set_mode_on_a_read_only_mount_is_read_only() {
    let serving = mounted(RwMockFs::new(), true, &[ADMIN]);
    assert_eq!(
        serving.service.set_mode(
            ADMIN_UID,
            &caps(),
            at(&path("a.txt")),
            0o640,
            FinalLink::Follow
        ),
        Err(Errno::ReadOnly)
    );
}

#[test]
fn a_set_mode_of_a_missing_path_is_not_found() {
    let serving = mounted(RwMockFs::new(), false, &[ADMIN]);
    assert_eq!(
        serving.service.set_mode(
            ADMIN_UID,
            &caps(),
            at(&path("absent")),
            0o640,
            FinalLink::Follow
        ),
        Err(Errno::NotFound)
    );
}

/// A created node is born owned by its creator, with the mode its kind
/// gives it, so the creator reads a file it just made.
#[test]
fn a_create_is_born_owned_by_its_creator() {
    let mut fs = RwMockFs::new();
    fs.set_root_security(NodeSecurity::new(0o777, ADMIN_UID, ADMIN_GID));
    let serving = mounted(fs, false, &[USER]);
    let svc = &serving.service;
    let caps = caps();
    let mine = path("mine.txt");

    create_path(svc, USER.uid, &caps, &mine, new_file()).expect("create");

    assert_eq!(
        record_of(&serving, b"mine.txt"),
        NodeSecurity::new(0o644, USER.uid, USER.gid)
    );
    let mut buf = [0u8; 4];
    assert_eq!(read_path(svc, USER.uid, &caps, &mine, 0, &mut buf), Ok(0));
}

/// A created directory is born owned by its creator, who may populate it
/// while another user may not.
#[test]
fn a_mkdir_is_born_owned_by_its_creator() {
    let mut fs = RwMockFs::new();
    fs.set_root_security(NodeSecurity::new(0o777, ADMIN_UID, ADMIN_GID));
    let serving = mounted(fs, false, &[USER, OTHER]);
    let svc = &serving.service;
    let caps = caps();

    svc.mkdir(USER.uid, &caps, at(&path("scratch")), new_dir())
        .expect("mkdir");
    assert_eq!(
        record_of(&serving, b"scratch"),
        NodeSecurity::new(0o755, USER.uid, USER.gid)
    );
    create_path(svc, USER.uid, &caps, &path("scratch/unit.bin"), new_file())
        .expect("create inside its own directory");
    assert_eq!(
        create_path(svc, OTHER.uid, &caps, &path("scratch/theirs"), new_file()),
        Err(Errno::PermissionDenied)
    );
}

/// A create is born with the bits it asked for less its creator's umask,
/// and a link with every bit whatever the mask.
#[test]
fn a_create_is_born_with_its_mode_less_the_umask() {
    let mut fs = RwMockFs::new();
    fs.set_root_security(NodeSecurity::new(0o777, ADMIN_UID, ADMIN_GID));
    let serving = mounted(fs, false, &[USER]);
    let svc = &serving.service;
    let caps = caps();
    let asked = |mode| CreateMode::new(mode, 0o027).expect("in range");

    create_path(svc, USER.uid, &caps, &path("f"), asked(0o666)).expect("create");
    svc.mkdir(USER.uid, &caps, at(&path("d")), asked(0o777))
        .expect("mkdir");
    svc.symlink(USER.uid, &caps, "f", at(&path("l")))
        .expect("symlink");
    for (name, mode) in [(&b"f"[..], 0o640), (&b"d"[..], 0o750), (&b"l"[..], 0o777)] {
        assert_eq!(
            record_of(&serving, name),
            NodeSecurity::new(mode, USER.uid, USER.gid)
        );
    }
}

/// What is made in a setgid directory takes the directory's group, and a
/// directory its setgid bit too; a file whose creator is not in that group
/// loses the setgid bit it asked for.
#[test]
fn a_setgid_directory_hands_down_its_group() {
    let mut fs = RwMockFs::new();
    fs.set_root_security(NodeSecurity::new(0o2777, ADMIN_UID, SHARED_GID));
    let serving = mounted(fs, false, &[USER]);
    let svc = &serving.service;
    let caps = caps();
    let asked = |mode| CreateMode::new(mode, 0o022).expect("in range");

    create_path(svc, USER.uid, &caps, &path("f"), asked(0o2666)).expect("create");
    svc.mkdir(USER.uid, &caps, at(&path("d")), asked(0o777))
        .expect("mkdir");
    assert_eq!(
        record_of(&serving, b"f"),
        NodeSecurity::new(0o644, USER.uid, SHARED_GID)
    );
    assert_eq!(
        record_of(&serving, b"d"),
        NodeSecurity::new(0o2755, USER.uid, SHARED_GID)
    );
}

/// A file keeps the setgid bit it asked for in a setgid directory whose
/// group its creator belongs to; outside a setgid directory a create takes
/// its creator's own group.
#[test]
fn a_file_keeps_setgid_in_a_group_its_creator_belongs_to() {
    let mut fs = RwMockFs::new();
    fs.set_root_security(NodeSecurity::new(0o2777, ADMIN_UID, SHARED_GID));
    let serving = mounted(fs, false, &[IN_SHARED]);
    let svc = &serving.service;
    let caps = caps();
    let asked = |mode| CreateMode::new(mode, 0o022).expect("in range");

    create_path(svc, USER.uid, &caps, &path("f"), asked(0o2666)).expect("create");
    assert_eq!(
        record_of(&serving, b"f"),
        NodeSecurity::new(0o2644, USER.uid, SHARED_GID)
    );

    serving.drivers[0]
        .lock()
        .set_root_security(NodeSecurity::new(0o777, ADMIN_UID, SHARED_GID));
    svc.mkdir(USER.uid, &caps, at(&path("d")), asked(0o777))
        .expect("mkdir");
    assert_eq!(
        record_of(&serving, b"d"),
        NodeSecurity::new(0o755, USER.uid, USER.gid)
    );
}

/// A corrupt directory whose cursor never advances must fail the listing
/// closed, never spin the kernel forever.
#[test]
fn a_listing_whose_cursor_never_advances_fails_closed() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    mk_file(&mut fs, root, "x", b"");
    let serving = mounted(fs.with_stuck_cursor(), true, &[ADMIN]);
    assert_eq!(
        listing(&serving.service, ADMIN_UID, MOUNT, FinalLink::Follow),
        Err(VfsError::Io.to_errno())
    );
}

#[test]
fn resolution_follows_a_link_to_its_target() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    mk_file(&mut fs, root, "real.txt", b"payload");
    mk_link(&mut fs, root, "alias", "/real.txt");
    let serving = mounted(fs, false, &[ADMIN]);

    assert_eq!(
        read_text(&serving.service, ADMIN_UID, &path("alias")),
        "payload"
    );
}

#[test]
fn resolution_follows_a_link_used_as_an_interior_directory() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let real = mk_dir(&mut fs, root, "real");
    mk_file(&mut fs, real, "leaf", b"inside");
    mk_link(&mut fs, root, "via", "/real");
    let serving = mounted(fs, false, &[ADMIN]);

    // `via` is not the final component here: it is being used as a
    // directory, so what matters is what it names.
    assert_eq!(
        read_text(&serving.service, ADMIN_UID, &path("via/leaf")),
        "inside"
    );
}

#[test]
fn a_link_cycle_is_refused_rather_than_walked() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    mk_link(&mut fs, root, "a", "/b");
    mk_link(&mut fs, root, "b", "/a");
    let serving = mounted(fs, false, &[ADMIN]);

    let mut buf = [0u8; 8];
    assert_eq!(
        read_path(
            &serving.service,
            ADMIN_UID,
            &caps(),
            &path("a"),
            0,
            &mut buf
        ),
        Err(Errno::LinkLoop)
    );
}

#[test]
fn a_self_referential_link_is_refused() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    mk_link(&mut fs, root, "loop", "/loop");
    let serving = mounted(fs, false, &[ADMIN]);

    let mut buf = [0u8; 8];
    assert_eq!(
        read_path(
            &serving.service,
            ADMIN_UID,
            &caps(),
            &path("loop"),
            0,
            &mut buf
        ),
        Err(Errno::LinkLoop)
    );
}

#[test]
fn a_chain_longer_than_the_hop_budget_is_refused_and_a_shorter_one_resolves() {
    // `hop{n}` -> `hop{n-1}` -> ... -> `hop0` -> `real.txt`.
    let chain = |links: usize| {
        let mut fs = RwMockFs::new();
        let root = fs.root();
        mk_file(&mut fs, root, "real.txt", b"end");
        mk_link(&mut fs, root, "hop0", "/real.txt");
        for i in 1..links {
            let target = alloc::format!("/hop{}", i - 1);
            mk_link(&mut fs, root, &alloc::format!("hop{i}"), &target);
        }
        mounted(fs, false, &[ADMIN])
    };

    // Exactly at the budget.
    let at_budget = chain(SYMLINK_HOP_MAX);
    let last = alloc::format!("hop{}", SYMLINK_HOP_MAX - 1);
    assert_eq!(
        read_text(&at_budget.service, ADMIN_UID, &path(&last)),
        "end"
    );

    let over = chain(SYMLINK_HOP_MAX + 2);
    let last = alloc::format!("hop{}", SYMLINK_HOP_MAX + 1);
    let mut buf = [0u8; 8];
    assert_eq!(
        read_path(&over.service, ADMIN_UID, &caps(), &path(&last), 0, &mut buf),
        Err(Errno::LinkLoop)
    );
}

#[test]
fn a_dangling_link_reports_not_found_not_a_loop() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    mk_link(&mut fs, root, "nowhere", "/absent");
    let serving = mounted(fs, false, &[ADMIN]);

    let mut buf = [0u8; 8];
    assert_eq!(
        read_path(
            &serving.service,
            ADMIN_UID,
            &caps(),
            &path("nowhere"),
            0,
            &mut buf
        ),
        Err(Errno::NotFound)
    );
}

#[test]
fn dot_dot_after_a_link_is_physical_rather_than_lexical() {
    // The escape this guards: collapsing `/a/via/../other` textually gives
    // `/a/other`, which is NOT where the walk actually stands once `via`
    // has been followed to `/b/c`. Physical resolution reads `/b/other`.
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let a = mk_dir(&mut fs, root, "a");
    mk_file(&mut fs, a, "other", b"lexical-would-read-this");
    let b = mk_dir(&mut fs, root, "b");
    let c = mk_dir(&mut fs, b, "c");
    mk_file(&mut fs, c, "leaf", b"target");
    mk_file(&mut fs, b, "other", b"physical-reads-this");
    mk_link(&mut fs, a, "via", "/b/c");
    // A caller's own path is not where the `..` under test comes from: a
    // link target carries it.
    mk_link(&mut fs, a, "up", "/b/c/../other");
    let serving = mounted(fs, false, &[ADMIN]);

    assert_eq!(
        read_text(&serving.service, ADMIN_UID, &path("a/up")),
        "physical-reads-this"
    );
}

#[test]
fn dot_dot_cannot_climb_out_of_the_volume() {
    // The stack starts at the mounted volume's own root and `..` never pops
    // past it, so a foreign volume's link cannot name anything above the
    // mount point. `/..` is `/`, as POSIX specifies.
    let mut fs = RwMockFs::new();
    let root = fs.root();
    mk_file(&mut fs, root, "confined.txt", b"still-inside");
    mk_link(&mut fs, root, "escape", "/../../../../confined.txt");
    let serving = mounted(fs, false, &[ADMIN]);

    assert_eq!(
        read_text(&serving.service, ADMIN_UID, &path("escape")),
        "still-inside"
    );
}

#[test]
fn search_permission_is_required_on_a_directory_a_link_leads_through() {
    // Following a link does not bypass the per-component check: the spliced
    // components are authorised exactly as typed ones are.
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let closed = mk_dir(&mut fs, root, "closed");
    mk_file(&mut fs, closed, "secret", b"classified");
    mk_link(&mut fs, root, "peek", "/closed/secret");
    // Owner-only search on the directory the link leads through.
    fs.set_security(closed, NodeSecurity::new(0o700, ADMIN_UID, ADMIN_GID))
        .expect("tighten the directory");
    let serving = mounted(fs, false, &[ADMIN, OTHER]);
    let svc = &serving.service;

    // The owner still reads it through the link...
    assert_eq!(read_text(svc, ADMIN_UID, &path("peek")), "classified");
    // ...and a stranger is refused at the traversed directory, not handed
    // the target because a link named it.
    let mut buf = [0u8; 16];
    assert_eq!(
        read_path(svc, OTHER.uid, &caps(), &path("peek"), 0, &mut buf),
        Err(Errno::PermissionDenied)
    );
}

#[test]
fn keeping_the_final_link_stats_the_link_and_following_stats_its_target() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    mk_file(&mut fs, root, "real.txt", b"payload");
    mk_link(&mut fs, root, "alias", "/real.txt");
    let serving = mounted(fs, false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let alias = path("alias");

    let kept = svc
        .stat(ADMIN_UID, &caps, at(&alias), FinalLink::Keep)
        .expect("stat the link itself");
    assert_eq!(kept.kind, FileKind::Symlink);
    // A link's size is its target's length, which is how `ls -l` renders it.
    assert_eq!(kept.size, "/real.txt".len() as u64);

    let followed = svc
        .stat(ADMIN_UID, &caps, at(&alias), FinalLink::Follow)
        .expect("stat through the link");
    assert_eq!(followed.kind, FileKind::Regular);
    assert_eq!(followed.size, "payload".len() as u64);
    // The two postures name different nodes, not merely different sizes.
    assert_ne!(kept.id, followed.id);
}

#[test]
fn a_dangling_link_is_statable_when_the_final_link_is_kept() {
    // The load-bearing case for `ls -l`: a link whose target is absent is
    // still a real directory entry, and only `Keep` can describe it.
    let mut fs = RwMockFs::new();
    let root = fs.root();
    mk_link(&mut fs, root, "nowhere", "/absent");
    let serving = mounted(fs, false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let nowhere = path("nowhere");

    let kept = svc
        .stat(ADMIN_UID, &caps, at(&nowhere), FinalLink::Keep)
        .expect("the link itself exists");
    assert_eq!(kept.kind, FileKind::Symlink);
    assert_eq!(
        svc.stat(ADMIN_UID, &caps, at(&nowhere), FinalLink::Follow),
        Err(Errno::NotFound)
    );
}

#[test]
fn keeping_the_final_link_refuses_to_list_it_as_a_directory() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let real = mk_dir(&mut fs, root, "real");
    mk_file(&mut fs, real, "leaf", b"inside");
    mk_link(&mut fs, root, "via", "/real");
    let serving = mounted(fs, false, &[ADMIN]);
    let svc = &serving.service;
    let via = path("via");

    // A `NO_FOLLOW` descriptor names the link, and a link is not a
    // directory — so its target's entries are never listed in its place.
    assert_eq!(
        listing(svc, ADMIN_UID, &via, FinalLink::Keep),
        Err(Errno::NotADirectory)
    );
    let entries =
        listing(svc, ADMIN_UID, &via, FinalLink::Follow).expect("following lists the target");
    assert_eq!(kinds(&entries), [(FileKind::Regular, "leaf")]);
}

#[test]
fn readlink_returns_the_stored_target_verbatim() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let sub = mk_dir(&mut fs, root, "sub");
    mk_file(&mut fs, root, "real.txt", b"payload");
    // A relative target carrying `..` comes back exactly as authored: the
    // target is data, so `readlink` neither normalises nor resolves it.
    mk_link(&mut fs, sub, "up", "../real.txt");
    mk_link(&mut fs, root, "abs", "/real.txt");
    let serving = mounted(fs, false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();

    assert_eq!(
        svc.readlink(ADMIN_UID, &caps, at(&path("sub/up"))),
        Ok(String::from("../real.txt"))
    );
    assert_eq!(
        svc.readlink(ADMIN_UID, &caps, at(&path("abs"))),
        Ok(String::from("/real.txt"))
    );
}

#[test]
fn readlink_reads_a_dangling_link_and_refuses_a_non_link() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    mk_file(&mut fs, root, "plain.txt", b"bytes");
    mk_dir(&mut fs, root, "dir");
    mk_link(&mut fs, root, "nowhere", "/absent");
    let serving = mounted(fs, false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let readlink = |name: &str| svc.readlink(ADMIN_UID, &caps, at(&path(name)));

    // The target is readable even though nothing is there to reach.
    assert_eq!(readlink("nowhere"), Ok(String::from("/absent")));
    // Neither a file nor a directory has a target; both fail closed rather
    // than handing back their own bytes or name.
    let not_a_link = Err(VfsError::InvalidPath.to_errno());
    assert_eq!(readlink("plain.txt"), not_a_link);
    assert_eq!(readlink("dir"), not_a_link);
    assert_eq!(readlink("absent"), Err(Errno::NotFound));
}

#[test]
fn readlink_needs_search_permission_on_the_way_to_the_link() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let closed = mk_dir(&mut fs, root, "closed");
    mk_link(&mut fs, closed, "alias", "/elsewhere");
    fs.set_security(closed, NodeSecurity::new(0o700, ADMIN_UID, ADMIN_GID))
        .expect("tighten the directory");
    let serving = mounted(fs, false, &[ADMIN, OTHER]);
    let svc = &serving.service;
    let caps = caps();
    let alias = path("closed/alias");

    assert_eq!(
        svc.readlink(ADMIN_UID, &caps, at(&alias)),
        Ok(String::from("/elsewhere"))
    );
    assert_eq!(
        svc.readlink(OTHER.uid, &caps, at(&alias)),
        Err(Errno::PermissionDenied)
    );
}

#[test]
fn a_created_link_resolves_and_reads_back_its_target() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    mk_file(&mut fs, root, "real.txt", b"payload");
    let serving = mounted(fs, false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let alias = path("alias");

    svc.symlink(ADMIN_UID, &caps, "/real.txt", at(&alias))
        .expect("create the link");
    assert_eq!(
        svc.readlink(ADMIN_UID, &caps, at(&alias)),
        Ok(String::from("/real.txt"))
    );
    // The whole round trip through the VFS: created here, then followed.
    assert_eq!(read_text(svc, ADMIN_UID, &alias), "payload");
}

#[test]
fn a_created_link_is_owned_by_its_creator() {
    // As for `create`: the link is born owned by its creator.
    let mut fs = RwMockFs::new();
    fs.set_root_security(NodeSecurity::new(0o777, ADMIN_UID, ADMIN_GID));
    let serving = mounted(fs, false, &[USER]);
    let svc = &serving.service;
    let caps = caps();
    let mine = path("mine");

    svc.symlink(USER.uid, &caps, "/target", at(&mine))
        .expect("create the link");
    let info = svc
        .stat(USER.uid, &caps, at(&mine), FinalLink::Keep)
        .expect("stat the new link");
    assert_eq!((info.uid, info.gid), (USER.uid, USER.gid));
}

#[test]
fn creating_a_link_refuses_an_existing_name_and_an_unwalkable_target() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    mk_file(&mut fs, root, "taken", b"x");
    let serving = mounted(fs, false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();

    assert_eq!(
        svc.symlink(ADMIN_UID, &caps, "/elsewhere", at(&path("taken"))),
        Err(Errno::AlreadyExists)
    );
    // The grammar is checked before anything is written, so a target this
    // resolver could never walk is refused rather than stored as a link
    // that can only ever fail.
    let fresh = path("fresh");
    let unwalkable = Err(VfsError::InvalidPath.to_errno());
    assert_eq!(svc.symlink(ADMIN_UID, &caps, "", at(&fresh)), unwalkable);
    let over_long_component = alloc::format!("/{}", "n".repeat(MAX_COMPONENT_LEN + 1));
    assert_eq!(
        svc.symlink(ADMIN_UID, &caps, &over_long_component, at(&fresh)),
        unwalkable
    );
    let too_many_steps = "/a".repeat(MAX_PATH_COMPONENTS + 1);
    assert_eq!(
        svc.symlink(ADMIN_UID, &caps, &too_many_steps, at(&fresh)),
        unwalkable
    );
    // A refused create leaves no name behind.
    assert_eq!(
        svc.stat(ADMIN_UID, &caps, at(&fresh), FinalLink::Keep),
        Err(Errno::NotFound)
    );
}

#[test]
fn creating_a_link_needs_write_permission_on_the_parent_and_a_writable_mount() {
    let caps = caps();
    let alias = path("alias");

    // Search but no write on the floor: a stranger cannot add a name.
    let writable = mounted(RwMockFs::new(), false, &[ADMIN, OTHER]);
    assert_eq!(
        writable.service.symlink(OTHER.uid, &caps, "/t", at(&alias)),
        Err(Errno::PermissionDenied)
    );

    // A read-only mount refuses before the driver is reached at all.
    let read_only = mounted(RwMockFs::new(), true, &[ADMIN]);
    assert_eq!(
        read_only
            .service
            .symlink(ADMIN_UID, &caps, "/t", at(&alias)),
        Err(Errno::ReadOnly)
    );
}

#[test]
fn a_format_without_links_refuses_rather_than_approximating_one() {
    let serving = mounted(RwMockFs::new().without_links(), false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let alias = path("alias");

    // The refusal is the permanent format limit, not "you used a file as a
    // directory": a caller must be able to tell the two apart.
    assert_eq!(
        svc.symlink(ADMIN_UID, &caps, "/target", at(&alias)),
        Err(Errno::NotSupported)
    );
    // And nothing was created in its place — no regular file holding a path.
    assert_eq!(
        svc.stat(ADMIN_UID, &caps, at(&alias), FinalLink::Keep),
        Err(Errno::NotFound)
    );
    // A plain create on the same driver still works, so the refusal is
    // about links and not about the mount.
    create_path(svc, ADMIN_UID, &caps, &path("plain"), new_file()).expect("a regular file is fine");
}

#[test]
fn the_open_posture_is_derived_from_the_no_follow_flag() {
    // One definition of the mapping, so an open and every operation later
    // served for its descriptor cannot disagree about following.
    assert_eq!(FinalLink::for_open(OpenFlags::empty()), FinalLink::Follow);
    assert_eq!(FinalLink::for_open(OpenFlags::READ), FinalLink::Follow);
    assert_eq!(FinalLink::for_open(OpenFlags::NO_FOLLOW), FinalLink::Keep);
    assert_eq!(
        FinalLink::for_open(OpenFlags::NO_FOLLOW.union(OpenFlags::READ)),
        FinalLink::Keep
    );
}

#[test]
fn writing_through_a_final_link_writes_the_target_not_the_link() {
    // POSIX writes what a link names, so the node the write reaches is the
    // one the link resolves to, not the link.
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let sub = mk_dir(&mut fs, root, "sub");
    mk_file(&mut fs, sub, "real.txt", b"old");
    mk_link(&mut fs, root, "alias", "/sub/real.txt");
    let serving = mounted(fs, false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let alias = path("alias");

    assert_eq!(
        write_path(svc, ADMIN_UID, &caps, &alias, 0, false, b"new"),
        Ok(3)
    );
    // The target changed...
    assert_eq!(read_text(svc, ADMIN_UID, &path("sub/real.txt")), "new");
    // ...and the link still names it, rather than having been overwritten
    // with the bytes.
    assert_eq!(
        svc.readlink(ADMIN_UID, &caps, at(&alias)),
        Ok(String::from("/sub/real.txt"))
    );
}

#[test]
fn truncating_through_a_final_link_truncates_the_target() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    mk_file(&mut fs, root, "real.txt", b"payload");
    mk_link(&mut fs, root, "alias", "real.txt");
    let serving = mounted(fs, false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let alias = path("alias");

    truncate_path(svc, ADMIN_UID, &caps, &alias, 3).expect("truncate through the link");
    assert_eq!(read_text(svc, ADMIN_UID, &path("real.txt")), "pay");
    assert_eq!(
        svc.readlink(ADMIN_UID, &caps, at(&alias)),
        Ok(String::from("real.txt"))
    );
}

#[test]
fn a_write_through_a_link_is_judged_by_its_target_not_the_links_directory() {
    // Write permission on the directory a link sits in grants nothing over
    // what the link names: the write is judged by the target's own record.
    let mut fs = RwMockFs::new();
    fs.set_root_security(NodeSecurity::new(0o777, ADMIN_UID, ADMIN_GID));
    let root = fs.root();
    let locked = mk_dir(&mut fs, root, "locked");
    mk_file(&mut fs, locked, "real.txt", b"old");
    mk_link(&mut fs, root, "alias", "/locked/real.txt");
    let serving = mounted(fs, false, &[ADMIN, OTHER]);
    let svc = &serving.service;
    let caps = caps();
    let alias = path("alias");

    assert_eq!(
        write_path(svc, OTHER.uid, &caps, &alias, 0, false, b"new"),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        write_path(svc, ADMIN_UID, &caps, &alias, 0, false, b"new"),
        Ok(3),
        "the target's owner writes through the same link"
    );
}

#[test]
fn a_write_through_a_dangling_link_reports_not_found() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    mk_link(&mut fs, root, "alias", "/nowhere");
    let serving = mounted(fs, false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let alias = path("alias");

    assert_eq!(
        write_path(svc, ADMIN_UID, &caps, &alias, 0, false, b"new"),
        Err(Errno::NotFound)
    );
    assert_eq!(
        truncate_path(svc, ADMIN_UID, &caps, &alias, 0),
        Err(Errno::NotFound)
    );
    // Nothing was created in the attempt.
    assert_eq!(
        svc.stat(ADMIN_UID, &caps, at(&path("nowhere")), FinalLink::Keep),
        Err(Errno::NotFound)
    );
}

#[test]
fn creating_through_a_dangling_link_creates_the_target() {
    // `open(O_CREAT)` on a dangling link creates what the link names, so the
    // link stops dangling rather than the create reporting the link's own
    // name as taken.
    let mut fs = RwMockFs::new();
    let root = fs.root();
    mk_link(&mut fs, root, "alias", "/made-later");
    let serving = mounted(fs, false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let alias = path("alias");
    let stat = |name: &str, final_link| svc.stat(ADMIN_UID, &caps, at(name), final_link);

    open_path(
        svc,
        ADMIN_UID,
        &caps,
        &alias,
        OpenFlags::CREATE.union(OpenFlags::WRITE),
    )
    .expect("create through the dangling link");
    // The target now exists as a regular file...
    assert_eq!(
        stat(&path("made-later"), FinalLink::Keep)
            .expect("the target was created")
            .kind,
        FileKind::Regular
    );
    // ...and the link is still a link, now resolving.
    assert_eq!(
        stat(&alias, FinalLink::Keep)
            .expect("the link survived")
            .kind,
        FileKind::Symlink
    );
    assert_eq!(
        stat(&alias, FinalLink::Follow)
            .expect("the link now resolves")
            .kind,
        FileKind::Regular
    );
}

#[test]
fn mkdir_over_a_link_is_refused_rather_than_following_it() {
    // POSIX `mkdir` never follows a final link, so it can neither replace a
    // live link nor quietly make the directory a dangling one names.
    let mut fs = RwMockFs::new();
    let root = fs.root();
    mk_dir(&mut fs, root, "real");
    mk_link(&mut fs, root, "live", "/real");
    mk_link(&mut fs, root, "dead", "/absent");
    let serving = mounted(fs, false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();

    for name in ["live", "dead"] {
        assert_eq!(
            svc.mkdir(ADMIN_UID, &caps, at(&path(name)), new_dir()),
            Err(Errno::AlreadyExists),
            "{name}"
        );
    }
    assert_eq!(
        svc.stat(ADMIN_UID, &caps, at(&path("absent")), FinalLink::Keep),
        Err(Errno::NotFound)
    );
}

#[test]
fn unlink_and_rename_still_act_on_the_link_itself() {
    // The counterpart of the following writes above: the namespace
    // operations keep the name as typed.
    let mut fs = RwMockFs::new();
    let root = fs.root();
    mk_file(&mut fs, root, "real.txt", b"payload");
    mk_link(&mut fs, root, "alias", "/real.txt");
    mk_link(&mut fs, root, "moved", "/real.txt");
    let serving = mounted(fs, false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();

    svc.rename(
        ADMIN_UID,
        &caps,
        at(&path("moved")),
        at(&path("renamed")),
        RenameFlags::empty(),
        None,
    )
    .expect("rename the link");
    assert_eq!(
        svc.readlink(ADMIN_UID, &caps, at(&path("renamed"))),
        Ok(String::from("/real.txt"))
    );

    svc.unlink(
        ADMIN_UID,
        &caps,
        at(&path("alias")),
        UnlinkFlags::empty(),
        None,
    )
    .expect("unlink the link");
    // The target is untouched by either.
    assert_eq!(read_text(svc, ADMIN_UID, &path("real.txt")), "payload");
}

#[test]
fn a_removal_of_a_gated_node_needs_the_capability() {
    // Write permission on the parent is what authorises removing an entry,
    // so without this the owner of a gated node's *parent* could unlink the
    // gate away and create an ungated node of the same name — and the
    // service that reaches the tree only by holding the capability would
    // then walk into the replacement.
    let serving = mounted(RwMockFs::new(), false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let gated = path("gated.txt");
    create_path(svc, ADMIN_UID, &caps, &gated, new_file()).expect("create");
    gate_child(&serving, b"gated.txt");

    assert_eq!(
        svc.unlink(ADMIN_UID, &caps, at(&gated), UnlinkFlags::empty(), None),
        Err(Errno::PermissionDenied)
    );
    svc.unlink(
        ADMIN_UID,
        &auditing(),
        at(&gated),
        UnlinkFlags::empty(),
        None,
    )
    .expect("the capability holder may remove it");
}

#[test]
fn a_rename_of_a_gated_node_needs_the_capability() {
    // The same-parent rename is the sharpest case: `rename` only authorises
    // the moved node itself when the parents differ, so a gated directory
    // could otherwise be moved aside within its own parent.
    let serving = mounted(RwMockFs::new(), false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let src = path("gated");
    let aside = path("stolen");
    svc.mkdir(ADMIN_UID, &caps, at(&src), new_dir())
        .expect("mkdir");
    gate_child(&serving, b"gated");

    let rename = |caps: &CapabilitySet| {
        svc.rename(
            ADMIN_UID,
            caps,
            at(&src),
            at(&aside),
            RenameFlags::empty(),
            None,
        )
    };
    assert_eq!(rename(&caps), Err(Errno::PermissionDenied));
    rename(&auditing()).expect("the capability holder may move it");
}

#[test]
fn a_rename_over_a_gated_destination_needs_the_capability() {
    // Replacing a name destroys its occupant, so a gated destination is
    // guarded exactly as a gated source is.
    let serving = mounted(RwMockFs::new(), false, &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    let src = path("plain.txt");
    let dst = path("gated.txt");
    create_path(svc, ADMIN_UID, &caps, &src, new_file()).expect("create source");
    create_path(svc, ADMIN_UID, &caps, &dst, new_file()).expect("create destination");
    gate_child(&serving, b"gated.txt");

    assert_eq!(
        svc.rename(
            ADMIN_UID,
            &caps,
            at(&src),
            at(&dst),
            RenameFlags::empty(),
            None
        ),
        Err(Errno::PermissionDenied)
    );
}

/// What a create, a mkdir and a symlink of `/x` each answer, as the admin, on
/// a root volume refusing every mutation with `refusal`.
fn refused_creates(refusal: DriverError) -> [Result<(), Errno>; 3] {
    let serving = at_root(RwMockFs::new().with_mutations_refused(refusal), &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    [
        create_path(svc, ADMIN_UID, &caps, "/x", new_file()),
        svc.mkdir(ADMIN_UID, &caps, at("/x"), new_dir()),
        svc.symlink(ADMIN_UID, &caps, "/target", at("/x")),
    ]
}

/// A driver-reported taken name reaches the caller as a taken name, whatever
/// operation met it.
///
/// The VFS pre-checks the name and answers `AlreadyExists` itself on the
/// ordinary path, so this is the window the pre-check cannot cover: a name
/// that appears between the check and the driver call. It used to surface as
/// an I/O error on `create` (and as `EWOULDBLOCK` to anything reaching the
/// driver without the VFS's per-operation mapping), because one driver value
/// carried three meanings.
#[test]
fn a_driver_reported_taken_name_is_already_exists_on_every_surface() {
    for outcome in refused_creates(DriverError::AlreadyExists) {
        assert_eq!(outcome, Err(Errno::AlreadyExists));
    }
}

/// A populated directory and a self-descending move are each reported as
/// themselves, so `rmdir` and `mv` can tell them apart.
///
/// Rename has no VFS-side pre-check for either, so both answers here come
/// from the driver. The move under itself used to arrive as `NotEmpty` —
/// advice to empty a destination that emptying could never make lawful.
#[test]
fn a_driver_reported_structural_refusal_keeps_its_own_class() {
    let serving = at_root(RwMockFs::new(), &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    for dir in ["/dir", "/dir/inner", "/spare"] {
        svc.mkdir(ADMIN_UID, &caps, at(dir), new_dir())
            .expect("mkdir");
    }
    let rename = |from: &str, to: &str| {
        svc.rename(
            ADMIN_UID,
            &caps,
            at(from),
            at(to),
            RenameFlags::empty(),
            None,
        )
    };
    assert_eq!(
        svc.unlink(ADMIN_UID, &caps, at("/dir"), UnlinkFlags::DIRECTORY, None),
        Err(Errno::NotEmpty)
    );
    assert_eq!(rename("/spare", "/dir"), Err(Errno::NotEmpty));

    // Moving a directory under itself can never be made lawful, so it is not
    // the emptiable `NotEmpty` and not a retryable transient.
    assert_eq!(
        rename("/dir", "/dir/inner/self"),
        Err(VfsError::DirectoryCycle.to_errno())
    );
    assert_eq!(VfsError::DirectoryCycle.to_errno(), Errno::OutOfRange);
}

/// A full volume and exhausted memory reach the caller as themselves, never
/// as the I/O error a faulted device is: a full disk used to read as `EIO`.
#[test]
fn a_full_volume_and_exhausted_memory_are_reported_as_themselves() {
    for (refusal, reported, errno) in [
        (DriverError::NoSpace, VfsError::NoSpace, Errno::NoSpace),
        (
            DriverError::OutOfMemory,
            VfsError::OutOfMemory,
            Errno::OutOfMemory,
        ),
    ] {
        for outcome in refused_creates(refusal) {
            assert_eq!(outcome, Err(errno));
        }
        assert_eq!(reported.to_errno(), errno);
    }
}

/// A driver that has stopped writing — a volume halted after a failed commit,
/// behind a mount that is itself writable — is read-only to its caller, never
/// the device fault it used to collapse into.
#[test]
fn a_driver_that_stopped_writing_is_reported_read_only() {
    for outcome in refused_creates(DriverError::ReadOnly) {
        assert_eq!(outcome, Err(Errno::ReadOnly));
    }
    assert_eq!(
        super::map_attr_driver_error(DriverError::ReadOnly),
        VfsError::ReadOnly,
        "an attribute write it refuses reads the same"
    );
    assert_eq!(VfsError::ReadOnly.to_errno(), Errno::ReadOnly);
}

/// An attribute namespace the format has nowhere to keep — ADFS asked to
/// store a `user.*` key — is the format's limit, never a device fault: a copy
/// must be able to report it as not carried and go on.
#[test]
fn an_attribute_the_format_cannot_keep_is_not_supported_not_a_fault() {
    assert_eq!(
        super::map_attr_driver_error(DriverError::Unsupported),
        VfsError::NotSupported
    );
    assert_eq!(VfsError::NotSupported.to_errno(), Errno::NotSupported);
    assert_eq!(
        super::map_attr_driver_error(DriverError::DeviceFault),
        VfsError::Io,
        "a real fault still reads as one"
    );
}

/// A change the volume's own record forbids — a node `chattr` holds fixed —
/// is refused to its caller, never reported as the device fault it used to
/// collapse into.
#[test]
fn a_change_the_volume_forbids_is_refused_not_a_fault() {
    let [create, ..] = refused_creates(DriverError::PermissionDenied);
    assert_eq!(create, Err(Errno::PermissionDenied));
}

/// A genuinely transient driver refusal keeps meaning "retry": it is not
/// read as any of the structural conflicts that used to share its value.
#[test]
fn a_driver_reported_transient_is_never_read_as_a_name_conflict() {
    for outcome in refused_creates(DriverError::Busy) {
        assert_eq!(outcome, Err(VfsError::Io.to_errno()));
    }
    assert_eq!(DriverError::Busy.as_errno(), Errno::WouldBlock);
}

/// A create hands its record over in the one driver call, so a volume that
/// refuses every later security store still takes it whole, born with the
/// creator's record.
#[test]
fn a_create_needs_no_security_store_after_it() {
    let mut fs = RwMockFs::new().with_security_refused(DriverError::DeviceFault);
    fs.set_root_security(NodeSecurity::new(0o777, ADMIN_UID, ADMIN_GID));
    let serving = at_root(fs, &[USER]);
    let svc = &serving.service;
    let caps = caps();
    create_path(svc, USER.uid, &caps, "/made", new_file()).expect("create");
    svc.mkdir(USER.uid, &caps, at("/folder"), new_dir())
        .expect("mkdir");
    svc.symlink(USER.uid, &caps, "/made", at("/link"))
        .expect("symlink");
    for (name, mode) in [
        (&b"made"[..], 0o644),
        (&b"folder"[..], 0o755),
        (&b"link"[..], 0o777),
    ] {
        assert_eq!(
            record_of(&serving, name),
            NodeSecurity::new(mode, USER.uid, USER.gid)
        );
    }
}

/// A record the mounted format cannot hold is refused as unsupported, never
/// as a path problem the caller could fix: a mode change on such a volume
/// used to read as "not a directory".
#[test]
fn a_record_the_format_cannot_hold_is_not_supported() {
    let mut fs = RwMockFs::new();
    let root = fs.root();
    mk_file(&mut fs, root, "file", b"");
    let serving = at_root(fs.with_security_refused(DriverError::Unsupported), &[ADMIN]);
    assert_eq!(
        serving
            .service
            .set_mode(ADMIN_UID, &caps(), at("/file"), 0o600, FinalLink::Follow),
        Err(Errno::NotSupported)
    );
    assert_eq!(VfsError::NotSupported.to_errno(), Errno::NotSupported);
}

/// A rename between kinds is refused as POSIX names it: a directory cannot
/// replace a non-directory, and a non-directory cannot replace a directory.
#[test]
fn a_rename_between_kinds_is_refused_by_which_kind_moves() {
    let serving = at_root(RwMockFs::new(), &[ADMIN]);
    let svc = &serving.service;
    let caps = caps();
    create_path(svc, ADMIN_UID, &caps, "/file", new_file()).expect("create");
    svc.mkdir(ADMIN_UID, &caps, at("/dir"), new_dir())
        .expect("mkdir");
    let rename = |from: &str, to: &str| {
        svc.rename(
            ADMIN_UID,
            &caps,
            at(from),
            at(to),
            RenameFlags::empty(),
            None,
        )
    };
    assert_eq!(rename("/dir", "/file"), Err(Errno::NotADirectory));
    assert_eq!(rename("/file", "/dir"), Err(Errno::IsADirectory));
}
