//! Errno conformance: the stable user/kernel [`Errno`] a failed operation
//! surfaces at the syscall boundary. The refusals a tool must tell apart
//! carry dedicated codes (`AlreadyExists`, `NotADirectory`, `IsADirectory`,
//! `NotEmpty`, `TooManyLinks`, `ReadOnly`, and `DeviceFault` — the `EIO`
//! analogue an unrecoverable backing fault reports); `abi-v1` still has no dedicated
//! `EINVAL`, so a malformed path or attribute key — and a rename that would
//! make a directory its own descendant — remains many-to-one (documented on
//! `VfsError::to_errno`). This suite pins the contract so a future change
//! cannot silently alter it.

use tairix_test_posix_fs_suite::*;

#[test]
fn vfs_error_maps_to_the_documented_stable_errno() {
    assert_eq!(VfsError::NotFound.to_errno(), Errno::NotFound);
    assert_eq!(
        VfsError::PermissionDenied.to_errno(),
        Errno::PermissionDenied
    );
    assert_eq!(VfsError::ReadOnly.to_errno(), Errno::ReadOnly);
    assert_eq!(VfsError::InvalidPath.to_errno(), Errno::OutOfRange);
    assert_eq!(VfsError::NotADirectory.to_errno(), Errno::NotADirectory);
    assert_eq!(VfsError::IsADirectory.to_errno(), Errno::IsADirectory);
    assert_eq!(VfsError::AlreadyExists.to_errno(), Errno::AlreadyExists);
    assert_eq!(VfsError::NotEmpty.to_errno(), Errno::NotEmpty);
    assert_eq!(VfsError::DirectoryCycle.to_errno(), Errno::OutOfRange);
    assert_eq!(VfsError::CrossVolume.to_errno(), Errno::CrossVolume);
    assert_eq!(VfsError::TooManyLinks.to_errno(), Errno::TooManyLinks);
    assert_eq!(VfsError::Io.to_errno(), Errno::DeviceFault);
}

#[test]
fn a_real_missing_lookup_surfaces_enoent() {
    let vol = arxfs_volume(false);
    assert_eq!(
        vol.stat(ROOT.caller(), &vol_path("absent"), FinalLink::Follow)
            .map(|info| info.size),
        Err(Errno::NotFound)
    );
}

#[test]
fn a_real_permission_denial_surfaces_eacces() {
    let vol = arxfs_volume(false);
    let admin = ROOT.caller();

    vol.create(admin, &vol_path("guarded"), new_file())
        .expect("create");
    let node = vol.root_node(b"guarded");
    vol.driver()
        .set_security(node, NodeSecurity::new(0o600, OWNER.uid, OWNER.gid))
        .expect("make owner-only");

    let mut buf = [0u8; 4];
    assert_eq!(
        vol.read(STRANGER.caller(), &vol_path("guarded"), 0, &mut buf),
        Err(Errno::PermissionDenied)
    );
}

/// The three structural conflicts a real volume can raise reach user space
/// as three different codes.
///
/// They shared one driver value until each was given its own, so which one a
/// tool saw depended on the mapping the call site happened to pick — and a
/// taken name could arrive as `EWOULDBLOCK`, advice to retry something no
/// retry clears.
#[test]
fn each_structural_conflict_surfaces_its_own_errno() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    for dir in ["outer", "outer/inner", "spare"] {
        vol.mkdir(owner, &vol_path(dir), new_dir())
            .unwrap_or_else(|e| panic!("mkdir {dir}: {e:?}"));
    }

    let taken = vol
        .mkdir(owner, &vol_path("outer"), new_dir())
        .expect_err("the name is taken");
    let populated = vol
        .rename(owner, &vol_path("spare"), &vol_path("outer"))
        .expect_err("the destination still holds an entry");
    let cycle = vol
        .rename(owner, &vol_path("outer"), &vol_path("outer/inner/outer"))
        .expect_err("a directory cannot become its own descendant");

    assert_eq!(taken, Errno::AlreadyExists);
    assert_eq!(populated, Errno::NotEmpty);
    assert_eq!(cycle, Errno::OutOfRange);
    for errno in [taken, populated, cycle] {
        assert_ne!(errno, Errno::WouldBlock, "no conflict invites a retry");
    }
}
