//! Behavioural tests for view binds (`plans/NEW-NAMESPACE.md` NS-4a): who may
//! bind which volume where, what a walk finds through a bind and above it, and
//! what a bind keeps from moving while it stands.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{
    DirVisit, FilesystemRead, FilesystemWrite, MountFlags, NodeKind, NodeSecurity,
};
use tairix_abi::driver::DriverHandle;
use tairix_abi::volume::ViewBindRequest;
use tairix_abi::{CapabilityId, Errno, OpenFlags, RealpathMode, RenameFlags, UnlinkFlags};
use tairix_caps::CapabilitySet;
use tairix_kernel_sec::{GroupId, UserId};

use super::{LateFilesystem, LateIdentity, MountedFilesystemService};
use crate::audit::AuditEvent;
use crate::fs::memfs::RwMockFs;
use crate::fs::service::FilesystemService;
use crate::fs::test_volume::{
    at, caps, close, identity_of, list_path, own_handle, read_path, Account, TEST_ACCOUNT, TEST_UID,
};
use crate::fs::{Attachment, BindsWithin, FinalLink, Metadata, Mode, Path, Vfs, VfsError};
use crate::test_sink::TestSink;
use crate::wallclock::KernelWallClock;

const ANN: u32 = TEST_UID;
const BOB: u32 = 2000;
const ROOT: u64 = 0x11;
const SHARE: u64 = 0x22;
const SHARE_ID: [u8; 16] = [0x5A; 16];
/// `/Users`, `ann`, `Home` and `nas`: what a bind over `nas` pins.
const CHAIN: usize = 4;

fn handle(raw: u64) -> DriverHandle {
    DriverHandle::from_raw(raw).expect("non-zero handle")
}

fn holder() -> CapabilitySet {
    let mut caps = CapabilitySet::empty();
    caps.insert(CapabilityId::FS_MOUNT);
    caps
}

fn record(mode: u32, uid: u32) -> NodeSecurity {
    NodeSecurity::new(mode, uid, uid)
}

/// The root volume: `/Users/ann/Home` holding an empty `nas`, a `full`
/// directory and a link to `nas`, and `/Users/bob/x` beside bob's open
/// directory holding one of ann's.
fn root_volume() -> RwMockFs {
    let mut fs = RwMockFs::new().with_exact_names();
    let dir = |fs: &mut RwMockFs, parent, name: &[u8], uid| {
        fs.create(parent, name, NodeKind::Directory, &record(0o755, uid))
            .expect("directory")
    };
    let root = fs.root();
    let users = dir(&mut fs, root, b"Users", 0);
    let ann = dir(&mut fs, users, b"ann", ANN);
    let home = dir(&mut fs, ann, b"Home", ANN);
    dir(&mut fs, home, b"nas", ANN);
    let full = dir(&mut fs, home, b"full", ANN);
    fs.create(full, b"kept", NodeKind::RegularFile, &record(0o644, ANN))
        .expect("file");
    fs.create_link(home, b"link", b"nas", &record(0o777, ANN))
        .expect("link");
    let bob = dir(&mut fs, users, b"bob", BOB);
    dir(&mut fs, bob, b"x", BOB);
    let open = fs
        .create(bob, b"open", NodeKind::Directory, &record(0o777, BOB))
        .expect("directory");
    dir(&mut fs, open, b"ann", ANN);
    fs
}

/// The network volume: `hello`, and `sub` holding `inner`.
fn share_volume() -> RwMockFs {
    let mut fs = RwMockFs::new().with_exact_names();
    fs.set_root_security(record(0o755, ANN));
    let root = fs.root();
    let hello = fs
        .create(root, b"hello", NodeKind::RegularFile, &record(0o644, ANN))
        .expect("file");
    fs.write_at(hello, 0, b"hi").expect("body");
    let sub = fs
        .create(root, b"sub", NodeKind::Directory, &record(0o755, ANN))
        .expect("directory");
    fs.create(sub, b"inner", NodeKind::RegularFile, &record(0o644, ANN))
        .expect("file");
    fs
}

struct Fixture {
    svc: MountedFilesystemService<RwMockFs>,
    mounted: &'static LateFilesystem<RwMockFs>,
    audit: &'static TestSink,
}

impl Fixture {
    /// The pins the root volume's registration holds.
    fn root_pins(&self) -> usize {
        self.mounted
            .nodes(handle(ROOT))
            .expect("registered")
            .outstanding()
    }

    fn stat_kind(&self, path: &str) -> Result<tairix_abi::FileKind, Errno> {
        self.svc
            .stat(ANN, &caps(), at(path), FinalLink::Follow)
            .map(|stat| stat.kind)
    }

    fn realpath(&self, path: &str) -> Result<String, Errno> {
        self.svc
            .realpath(ANN, &caps(), at(path), RealpathMode::Existing)
    }

    fn names(&self, path: &str) -> Vec<String> {
        let mut names = Vec::new();
        list_path(
            &self.svc,
            ANN,
            &caps(),
            path,
            OpenFlags::DIRECTORY,
            &mut |entry| {
                names.push(String::from_utf8_lossy(entry.name).into_owned());
                DirVisit::Take
            },
        )
        .expect("lists");
        names.sort();
        names
    }
}

/// `/` backed by [`root_volume`], and the network volume ann attached shown
/// at `/Storage/share`.
fn fixture() -> Fixture {
    let vfs = Vfs::with_default_layout(UserId(0), GroupId(0));
    {
        let mut mounts = vfs.mounts_write();
        mounts
            .attach(Attachment::new(handle(ROOT), None, MountFlags::default()))
            .expect("attach the root volume");
        mounts.back_root(handle(ROOT)).expect("back /");
        mounts
            .attach_at(
                Attachment::new(handle(SHARE), None, MountFlags::NOEXEC),
                Path::parse("/Storage/share").expect("path"),
                Metadata::new(UserId(ANN), GroupId(ANN), Mode::from_bits(0o700)),
            )
            .expect("in the catalog");
    }
    let mounted: &'static LateFilesystem<RwMockFs> =
        crate::forever(Box::new(LateFilesystem::new()));
    mounted.install_vfs(vfs).expect("install vfs");
    let audit: &'static TestSink = crate::forever(Box::new(TestSink::new()));
    let clock: &'static KernelWallClock = crate::forever(Box::new(KernelWallClock::new()));
    mounted
        .install_volume_host(mounted, audit, clock)
        .expect("host");
    mounted
        .register(
            handle(ROOT),
            root_volume(),
            "root",
            "memfs",
            [0x11; 16],
            None,
        )
        .expect("root");
    mounted
        .register_network(
            handle(SHARE),
            [share_volume()],
            "//nas/share",
            "smb3",
            SHARE_ID,
            Some(ANN),
        )
        .expect("share");
    let identity: &'static LateIdentity = crate::forever(Box::new(LateIdentity::new()));
    let bob = Account {
        uid: BOB,
        gid: BOB,
        groups: &[],
    };
    identity
        .install(identity_of(&[TEST_ACCOUNT, bob]))
        .expect("identity");
    Fixture {
        svc: MountedFilesystemService::new(mounted, identity),
        mounted,
        audit,
    }
}

fn bind(target: &str) -> ViewBindRequest<'_> {
    ViewBindRequest {
        volume_id: SHARE_ID,
        subtree: "/",
        target,
        narrowing: MountFlags::default(),
    }
}

const NAS: &str = "/Users/ann/Home/nas";

#[test]
fn an_owner_binds_its_volume_over_an_empty_directory_it_owns() {
    let f = fixture();
    assert_eq!(f.svc.view_bind(ANN, &caps(), &bind(NAS)), Ok(()));
    assert_eq!(f.names(NAS), ["hello", "sub"]);
    assert_eq!(
        f.stat_kind("/Users/ann/Home/nas/sub/inner"),
        Ok(tairix_abi::FileKind::Regular)
    );
    let mut body = [0u8; 2];
    assert_eq!(
        read_path(
            &f.svc,
            ANN,
            &caps(),
            "/Users/ann/Home/nas/hello",
            0,
            &mut body
        ),
        Ok(2)
    );
    assert_eq!(&body, b"hi");
    assert_eq!(
        f.realpath("/Users/ann/Home/nas/sub"),
        Ok(String::from("/Users/ann/Home/nas/sub"))
    );
    assert_eq!(f.root_pins(), CHAIN, "the directory and those down to it");
    let listed = f.svc.view_snapshot().expect("snapshot");
    let shown = listed
        .iter()
        .find(|view| view.target_bytes() == NAS.as_bytes())
        .expect("listed");
    assert_eq!(shown.binder(), Some(ANN));
    assert_eq!(shown.volume_id(), SHARE_ID);
    assert_eq!(shown.flags(), MountFlags::NOEXEC, "its volume's flags");
    assert!(f.audit.event_ids().contains(&AuditEvent::ViewBound.id().0));
}

#[test]
fn a_dot_dot_from_a_binds_floor_reaches_the_directory_holding_what_it_covers() {
    let f = fixture();
    f.svc.view_bind(ANN, &caps(), &bind(NAS)).expect("binds");
    assert_eq!(
        f.realpath("/Users/ann/Home/nas/.."),
        Ok(String::from("/Users/ann/Home"))
    );
    assert_eq!(
        f.realpath("/Users/ann/Home/nas/sub/../../full/kept"),
        Ok(String::from("/Users/ann/Home/full/kept"))
    );
    let cwd = f
        .svc
        .chdir(ANN, &caps(), at("/Users/ann/Home/nas/sub"))
        .expect("into the bind");
    let up = crate::fs::At {
        start: cwd.start(),
        path: "../..",
    };
    assert_eq!(
        f.svc.realpath(ANN, &caps(), up, RealpathMode::Existing),
        Ok(String::from("/Users/ann/Home"))
    );
}

#[test]
fn without_the_mount_authority_only_ones_own_volume_binds_and_only_over_ones_own_empty_directory() {
    let f = fixture();
    for (who, target, refused) in [
        (BOB, "/Users/bob/x", Errno::PermissionDenied),
        (ANN, "/Users/ann/Home/full", Errno::NotEmpty),
        (ANN, "/Users/bob/x", Errno::PermissionDenied),
        (ANN, "/Users/bob/open/ann", Errno::PermissionDenied),
        (ANN, "/Storage/alias", Errno::PermissionDenied),
        (ANN, "/System/x", Errno::PermissionDenied),
        (ANN, "/Users", Errno::PermissionDenied),
        (ANN, "/", Errno::PermissionDenied),
        (ANN, "/Users/ann/Home/link", Errno::LinkLoop),
        (ANN, "/Users/ann/Home/absent", Errno::NotFound),
        (ANN, "/Users/ann/Home/full/kept", Errno::NotADirectory),
    ] {
        assert_eq!(
            f.svc.view_bind(who, &caps(), &bind(target)),
            Err(refused),
            "{target}"
        );
    }
    assert_eq!(f.root_pins(), 0, "a refused bind keeps nothing pinned");
    assert!(f
        .audit
        .event_ids()
        .contains(&AuditEvent::ViewBindRefused.id().0));
    let unknown = ViewBindRequest {
        volume_id: [7; 16],
        ..bind(NAS)
    };
    assert_eq!(
        f.svc.view_bind(ANN, &caps(), &unknown),
        Err(Errno::NotFound)
    );
}

#[test]
fn the_mount_authority_binds_any_volume_over_any_directory_and_in_the_catalog() {
    let f = fixture();
    assert_eq!(
        f.svc.view_bind(BOB, &holder(), &bind("/Users/bob/x")),
        Ok(())
    );
    assert_eq!(
        f.svc
            .view_bind(ANN, &holder(), &bind("/Users/bob/open/ann")),
        Ok(()),
        "a holder may hold another's directories where they are"
    );
    assert_eq!(
        f.svc
            .view_bind(BOB, &holder(), &bind("/Users/ann/Home/full")),
        Ok(()),
        "a holder may cover a directory that holds entries"
    );
    assert_eq!(f.names("/Users/ann/Home/full"), ["hello", "sub"]);
    assert_eq!(
        f.svc.view_bind(BOB, &holder(), &bind("/Storage/alias")),
        Ok(())
    );
    assert_eq!(
        f.stat_kind("/Storage/alias/hello"),
        Ok(tairix_abi::FileKind::Regular)
    );
    assert_eq!(f.names("/Storage"), ["alias", "share"]);
    for (target, refused) in [
        ("/Storage/alias", Errno::AlreadyExists),
        ("/Storage/-bad", Errno::OutOfRange),
        ("/System/Logs", Errno::PermissionDenied),
    ] {
        assert_eq!(
            f.svc.view_bind(BOB, &holder(), &bind(target)),
            Err(refused),
            "{target}"
        );
    }
}

#[test]
fn a_bind_shows_a_subtree_its_binder_can_reach() {
    let f = fixture();
    let sub = ViewBindRequest {
        subtree: "/sub",
        ..bind(NAS)
    };
    assert_eq!(f.svc.view_bind(ANN, &caps(), &sub), Ok(()));
    assert_eq!(f.names(NAS), ["inner"]);
    let listed = f.svc.view_snapshot().expect("snapshot");
    let shown = listed
        .iter()
        .find(|view| view.target_bytes() == NAS.as_bytes())
        .expect("listed");
    assert_eq!(shown.subtree_bytes(), b"/sub");
    for (subtree, refused) in [
        ("/absent", Errno::NotFound),
        ("/hello", Errno::NotADirectory),
    ] {
        let request = ViewBindRequest {
            subtree,
            ..bind("/Users/bob/x")
        };
        assert_eq!(
            f.svc.view_bind(BOB, &holder(), &request),
            Err(refused),
            "{subtree}"
        );
    }
}

#[test]
fn nothing_on_the_way_to_a_bind_is_renamed_or_removed_while_it_stands() {
    let f = fixture();
    f.svc.view_bind(ANN, &caps(), &bind(NAS)).expect("binds");
    let rename = |from: &str, to: &str| {
        f.svc
            .rename(ANN, &caps(), at(from), at(to), RenameFlags::default(), None)
    };
    assert_eq!(
        rename("/Users/ann/Home", "/Users/ann/Home2"),
        Err(Errno::Busy)
    );
    assert_eq!(
        rename("/Users/ann/Home/full", "/Users/ann/Home/nas2"),
        Ok(()),
        "a directory beside the bind moves"
    );
    assert_eq!(
        rename("/Users/ann/Home/nas", "/Users/ann/Home/moved"),
        Err(Errno::OutOfRange),
        "what a bind covers has no name of its own"
    );
    assert_eq!(
        f.svc
            .unlink(ANN, &caps(), at(NAS), UnlinkFlags::DIRECTORY, None),
        Err(Errno::OutOfRange)
    );
    assert_eq!(f.svc.view_unbind(ANN, &caps(), NAS), Ok(()));
    assert_eq!(
        rename("/Users/ann/Home", "/Users/ann/Home2"),
        Ok(()),
        "unbound, it moves again"
    );
}

#[test]
fn a_binder_or_the_mount_authority_unbinds_and_the_directory_shows_again() {
    let f = fixture();
    f.svc.view_bind(ANN, &caps(), &bind(NAS)).expect("binds");
    assert_eq!(
        f.svc.view_unbind(BOB, &caps(), NAS),
        Err(Errno::PermissionDenied)
    );
    assert!(f
        .audit
        .event_ids()
        .contains(&AuditEvent::ViewUnbindRefused.id().0));
    assert_eq!(f.svc.view_unbind(ANN, &caps(), NAS), Ok(()));
    assert!(f.names(NAS).is_empty(), "the empty directory again");
    assert_eq!(f.root_pins(), 0, "its pins let go");
    assert_eq!(f.svc.view_unbind(ANN, &caps(), NAS), Err(Errno::NotFound));
    assert_eq!(
        f.svc.view_unbind(ANN, &holder(), "/Storage/share"),
        Err(Errno::OutOfRange),
        "an attachment is detached, never unbound"
    );
    f.svc.view_bind(ANN, &caps(), &bind(NAS)).expect("binds");
    assert_eq!(f.svc.view_unbind(BOB, &holder(), NAS), Ok(()));
}

#[test]
fn a_descriptor_opened_through_a_bind_answers_stale_once_it_is_gone() {
    let f = fixture();
    f.svc.view_bind(ANN, &caps(), &bind(NAS)).expect("binds");
    let held = own_handle(
        &f.svc,
        ANN,
        &caps(),
        "/Users/ann/Home/nas/hello",
        OpenFlags::READ,
    )
    .expect("opens");
    f.svc.view_unbind(ANN, &caps(), NAS).expect("unbinds");
    let mut body = [0u8; 2];
    assert_eq!(
        f.svc.read_node(
            &held,
            &crate::fs::test_volume::acting(ANN, &caps()),
            0,
            &mut body
        ),
        Err(Errno::Stale)
    );
    assert_eq!(f.root_pins(), CHAIN, "held until the descriptor goes");
    close(&f.svc, held);
    assert_eq!(f.root_pins(), 0);
}

#[test]
fn a_bind_leaves_with_its_volume() {
    let f = fixture();
    f.svc.view_bind(ANN, &caps(), &bind(NAS)).expect("binds");
    let vfs = f.mounted.vfs().expect("vfs");
    vfs.mounts_write()
        .detach(handle(SHARE), BindsWithin::Refuse)
        .expect("the volume detaches");
    assert!(f.names(NAS).is_empty());
    assert_eq!(f.root_pins(), 0);
    assert_eq!(f.svc.view_unbind(ANN, &caps(), NAS), Err(Errno::NotFound));
}

#[test]
fn a_bind_stands_within_an_attached_volume_or_another_bind() {
    let f = fixture();
    // Bob's own directory of the root volume, shown inside the share and
    // inside a bind of it.
    let bobs = |target| ViewBindRequest {
        volume_id: [0x11; 16],
        subtree: "/Users/bob",
        target,
        narrowing: MountFlags::default(),
    };
    assert_eq!(
        f.svc.view_bind(BOB, &holder(), &bobs("/Storage/share/sub")),
        Ok(()),
        "within an attached volume"
    );
    assert_eq!(f.names("/Storage/share/sub"), ["open", "x"]);
    assert_eq!(f.svc.view_bind(ANN, &caps(), &bind(NAS)), Ok(()));
    assert_eq!(
        f.svc
            .view_bind(BOB, &holder(), &bobs("/Users/ann/Home/nas/sub")),
        Ok(()),
        "within another bind"
    );
    assert_eq!(f.names("/Users/ann/Home/nas/sub"), ["open", "x"]);
    assert_eq!(
        f.realpath("/Users/ann/Home/nas/sub/x/../.."),
        Ok(String::from("/Users/ann/Home/nas"))
    );
    assert_eq!(
        f.svc.view_unbind(ANN, &caps(), NAS),
        Err(Errno::NotEmpty),
        "a bind stands within it"
    );
    let vfs = f.mounted.vfs().expect("vfs");
    assert_eq!(
        vfs.mounts_write()
            .detach(handle(SHARE), BindsWithin::Refuse),
        Err(VfsError::NotEmpty),
        "a plain detach waits for the binds of other volumes within it"
    );
    vfs.mounts_write()
        .detach(handle(SHARE), BindsWithin::Withdraw)
        .expect("a forced one withdraws them first");
    assert!(f.names(NAS).is_empty());
    assert_eq!(f.root_pins(), 0);
}
