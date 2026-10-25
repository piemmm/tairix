//! permission-model conformance, exercised end-to-end: the VFS
//! applies mode bits, ACL grants, and the optional per-inode capability
//! gate to the record the real `arxfs` driver stores. This is the suite's
//! analogue of `pjdfstest`'s `chmod`/`granular` permission cases, plus the
//! capability gate the charter and `PLAN.md` Stage 5
//! call out explicitly.
//!
//! The decision never branches on `uid == 0`: an owning user is
//! granted by its owner triad, not by being uid 0.

use tairix_test_posix_fs_suite::*;

/// Create `name` at the volume root as the installer (uid 0) and write
/// `body`, then re-stamp its stored record with `sec`.
fn planted_file(vol: &Volume, name: &str, body: &[u8], sec: NodeSecurity) {
    let admin = ROOT.caller();
    vol.create(admin, &vol_path(name), new_file())
        .expect("create as installer");
    vol.write(admin, &vol_path(name), 0, body)
        .expect("write as installer");
    let node = vol.root_node(name.as_bytes());
    vol.driver()
        .set_security(node, sec)
        .expect("re-stamp security record");
}

/// A capability set holding `CAP_AUDIT_READ`.
fn auditing() -> CapabilitySet {
    let mut caps = CapabilitySet::empty();
    caps.insert(CapabilityId::AUDIT_READ);
    caps
}

#[test]
fn owner_can_read_a_private_file_but_a_stranger_cannot() {
    let vol = arxfs_volume(false);
    planted_file(
        &vol,
        "secret",
        b"top secret",
        NodeSecurity::new(0o600, OWNER.uid, OWNER.gid),
    );

    let mut buf = [0u8; 16];
    let read = vol
        .read(OWNER.caller(), &vol_path("secret"), 0, &mut buf)
        .expect("owner reads its own 0600 file");
    assert_eq!(&buf[..read], b"top secret");

    assert_eq!(
        vol.read(STRANGER.caller(), &vol_path("secret"), 0, &mut buf),
        Err(Errno::PermissionDenied)
    );
}

#[test]
fn capability_gate_blocks_read_even_at_mode_0644() {
    // PLAN.md Stage 5: a file marked with a required capability is
    // unreadable without it, even though mode 0644 would otherwise grant the
    // read.
    let vol = arxfs_volume(false);
    let mut sec = NodeSecurity::new(0o644, OWNER.uid, OWNER.gid);
    sec.required_cap = Some(CapabilityId::AUDIT_READ);
    planted_file(&vol, "audit.log", b"audit trail", sec);

    let mut buf = [0u8; 16];
    assert_eq!(
        vol.read(STRANGER.caller(), &vol_path("audit.log"), 0, &mut buf),
        Err(Errno::PermissionDenied)
    );

    let read = vol
        .read(
            STRANGER.holding(auditing()),
            &vol_path("audit.log"),
            0,
            &mut buf,
        )
        .expect("holder of the capability reads the file");
    assert_eq!(&buf[..read], b"audit trail");
}

#[test]
fn acl_grant_allows_a_read_the_mode_bits_would_deny() {
    let vol = arxfs_volume(false);
    let mut sec = NodeSecurity::new(0o600, OWNER.uid, OWNER.gid);
    sec.push_acl(SecurityAcl {
        subject: SecuritySubject::Group(SHARED_GID),
        perms: 0b100,
    })
    .expect("room for one ACL entry");
    planted_file(&vol, "shared", b"shared bytes", sec);

    let mut buf = [0u8; 16];

    // A member of the granted group reads despite the 0600 mode bits.
    let read = vol
        .read(MEMBER.caller(), &vol_path("shared"), 0, &mut buf)
        .expect("ACL grant admits the group member");
    assert_eq!(&buf[..read], b"shared bytes");

    // A user outside the group still falls through to the denying mode.
    assert_eq!(
        vol.read(STRANGER.caller(), &vol_path("shared"), 0, &mut buf),
        Err(Errno::PermissionDenied)
    );
}

#[test]
fn directory_without_search_permission_blocks_traversal() {
    let vol = arxfs_volume(false);
    let admin = ROOT.caller();

    vol.mkdir(admin, &vol_path("priv"), new_dir())
        .expect("mkdir priv");
    vol.create(admin, &vol_path("priv/inside"), new_file())
        .expect("create inside");
    vol.write(admin, &vol_path("priv/inside"), 0, b"hidden")
        .expect("write inside");

    // Restrict the directory to its owner with no access for others.
    let dir = vol.root_node(b"priv");
    vol.driver()
        .set_security(dir, NodeSecurity::new(0o700, OWNER.uid, OWNER.gid))
        .expect("restrict directory");

    let mut buf = [0u8; 16];
    assert_eq!(
        vol.read(STRANGER.caller(), &vol_path("priv/inside"), 0, &mut buf),
        Err(Errno::PermissionDenied)
    );

    // The directory's owner has search permission and reaches the file.
    let read = vol
        .read(OWNER.caller(), &vol_path("priv/inside"), 0, &mut buf)
        .expect("owner traverses its own directory");
    assert_eq!(&buf[..read], b"hidden");
}

#[test]
fn write_into_a_read_only_directory_is_denied() {
    let vol = arxfs_volume(false);

    vol.mkdir(ROOT.caller(), &vol_path("ro"), new_dir())
        .expect("mkdir ro");
    let dir = vol.root_node(b"ro");
    // r-x for the owner: search is allowed, but creating an entry is not.
    vol.driver()
        .set_security(dir, NodeSecurity::new(0o500, OWNER.uid, OWNER.gid))
        .expect("make directory non-writable");

    assert_eq!(
        vol.create(OWNER.caller(), &vol_path("ro/new"), new_file()),
        Err(Errno::PermissionDenied)
    );
}
