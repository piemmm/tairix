//! `unlink(2)` conformance: removing a file, `ENOENT`, and reuse of the
//! freed name.

use tairix_test_posix_fs_suite::*;

#[test]
fn unlink_removes_a_file() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("gone"), new_file())
        .expect("create");
    vol.unlink(owner, &vol_path("gone"), UnlinkFlags::empty())
        .expect("remove");

    assert_eq!(
        vol.stat(owner, &vol_path("gone"), FinalLink::Follow)
            .map(|info| info.kind),
        Err(Errno::NotFound)
    );
}

#[test]
fn unlink_missing_file_is_not_found() {
    let vol = arxfs_volume(false);
    assert_eq!(
        vol.unlink(ROOT.caller(), &vol_path("never"), UnlinkFlags::empty()),
        Err(Errno::NotFound)
    );
}

#[test]
fn the_name_is_reusable_after_unlink() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("recycle"), new_file())
        .expect("create");
    vol.unlink(owner, &vol_path("recycle"), UnlinkFlags::empty())
        .expect("remove");
    vol.create(owner, &vol_path("recycle"), new_file())
        .expect("re-create the freed name");
}

#[test]
fn unlink_leaves_sibling_files_intact() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("keep"), new_file())
        .expect("create keep");
    vol.write(owner, &vol_path("keep"), 0, b"intact")
        .expect("write keep");
    vol.create(owner, &vol_path("drop"), new_file())
        .expect("create drop");
    vol.unlink(owner, &vol_path("drop"), UnlinkFlags::empty())
        .expect("remove drop");

    let mut buf = [0u8; 6];
    let read = vol
        .read(owner, &vol_path("keep"), 0, &mut buf)
        .expect("read keep");
    assert_eq!(&buf[..read], b"intact");
}

#[test]
fn in_a_sticky_directory_only_the_entry_s_or_the_directory_s_owner_unlinks() {
    let vol = arxfs_volume(false);
    let root = ROOT.caller();
    let (writer, other) = (OWNER.caller(), STRANGER.caller());
    vol.mkdir(root, &vol_path("drop"), new_dir())
        .expect("mkdir");
    vol.set_mode(root, &vol_path("drop"), 0o1777)
        .expect("sticky");
    for name in ["drop/a", "drop/b"] {
        vol.create(writer, &vol_path(name), new_file())
            .expect("a writer of the drop folder creates");
    }
    assert_eq!(
        vol.unlink(other, &vol_path("drop/a"), UnlinkFlags::empty()),
        Err(Errno::PermissionDenied)
    );
    vol.unlink(writer, &vol_path("drop/a"), UnlinkFlags::empty())
        .expect("the entry's owner");
    vol.unlink(root, &vol_path("drop/b"), UnlinkFlags::empty())
        .expect("the directory's owner");
}
