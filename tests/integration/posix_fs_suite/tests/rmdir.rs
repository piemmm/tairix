//! `rmdir(2)` conformance: removing an empty directory, `ENOTEMPTY` for a
//! populated one, `ENOTDIR` for a file, and `ENOENT`.
//!
//! A removal under `UnlinkFlags::DIRECTORY` is the `rmdir` posture: the
//! filesystem decides the node's kind atomically in the same walk that
//! removes it, never a caller-side stat.

use tairix_test_posix_fs_suite::*;

#[test]
fn rmdir_removes_an_empty_directory() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.mkdir(owner, &vol_path("empty"), new_dir())
        .expect("mkdir");
    vol.unlink(owner, &vol_path("empty"), UnlinkFlags::DIRECTORY)
        .expect("rmdir");

    assert_eq!(
        vol.stat(owner, &vol_path("empty"), FinalLink::Follow)
            .map(|info| info.kind),
        Err(Errno::NotFound)
    );
}

#[test]
fn rmdir_of_non_empty_directory_is_not_empty() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.mkdir(owner, &vol_path("populated"), new_dir())
        .expect("mkdir");
    vol.create(owner, &vol_path("populated/child"), new_file())
        .expect("create child");

    assert_eq!(
        vol.unlink(owner, &vol_path("populated"), UnlinkFlags::DIRECTORY),
        Err(Errno::NotEmpty)
    );
}

#[test]
fn rmdir_succeeds_once_the_directory_is_emptied() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.mkdir(owner, &vol_path("d"), new_dir()).expect("mkdir");
    vol.create(owner, &vol_path("d/child"), new_file())
        .expect("create child");
    vol.unlink(owner, &vol_path("d/child"), UnlinkFlags::empty())
        .expect("remove child");
    vol.unlink(owner, &vol_path("d"), UnlinkFlags::DIRECTORY)
        .expect("rmdir once emptied");
}

#[test]
fn rmdir_of_a_file_is_not_a_directory() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("plain"), new_file())
        .expect("create");
    assert_eq!(
        vol.unlink(owner, &vol_path("plain"), UnlinkFlags::DIRECTORY),
        Err(Errno::NotADirectory)
    );
    // The refused removal left the file untouched.
    vol.stat(owner, &vol_path("plain"), FinalLink::Follow)
        .expect("file survives");
}

#[test]
fn rmdir_missing_directory_is_not_found() {
    let vol = arxfs_volume(false);
    assert_eq!(
        vol.unlink(ROOT.caller(), &vol_path("nope"), UnlinkFlags::DIRECTORY),
        Err(Errno::NotFound)
    );
}
