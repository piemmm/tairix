//! `mkdir(2)` conformance: directory creation, `EEXIST`, `ENOENT`,
//! `ENOTDIR`, and the refusal to mutate the mount root itself.
//!
//! Every case drives the real `arxfs` driver through the kernel's filesystem
//! service, the path every `fs_mkdir` takes.

use tairix_test_posix_fs_suite::*;

#[test]
fn mkdir_creates_a_directory() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.mkdir(owner, &vol_path("d"), new_dir())
        .expect("mkdir succeeds");

    let info = vol
        .stat(owner, &vol_path("d"), FinalLink::Follow)
        .expect("stat the new directory");
    assert_eq!(info.kind, FileKind::Directory);
}

#[test]
fn mkdir_creates_nested_directories() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.mkdir(owner, &vol_path("a"), new_dir())
        .expect("mkdir a");
    vol.mkdir(owner, &vol_path("a/b"), new_dir())
        .expect("mkdir a/b");

    let entries = vol.list(owner, &vol_path("a")).expect("list a");
    assert_eq!(kinds(&entries), [(FileKind::Directory, "b")]);
}

#[test]
fn mkdir_existing_name_is_already_exists() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.mkdir(owner, &vol_path("dup"), new_dir())
        .expect("first mkdir");
    assert_eq!(
        vol.mkdir(owner, &vol_path("dup"), new_dir()),
        Err(Errno::AlreadyExists)
    );
}

#[test]
fn mkdir_over_existing_file_is_already_exists() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("f"), new_file())
        .expect("create file");
    assert_eq!(
        vol.mkdir(owner, &vol_path("f"), new_dir()),
        Err(Errno::AlreadyExists)
    );
}

#[test]
fn mkdir_in_missing_parent_is_not_found() {
    let vol = arxfs_volume(false);
    assert_eq!(
        vol.mkdir(ROOT.caller(), &vol_path("missing/child"), new_dir()),
        Err(Errno::NotFound)
    );
}

#[test]
fn mkdir_with_file_as_parent_is_not_a_directory() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("file"), new_file())
        .expect("create file");
    assert_eq!(
        vol.mkdir(owner, &vol_path("file/child"), new_dir()),
        Err(Errno::NotADirectory)
    );
}

#[test]
fn mkdir_of_the_mount_root_itself_finds_it_there() {
    let vol = arxfs_volume(false);
    assert_eq!(
        vol.mkdir(ROOT.caller(), MOUNT, new_dir()),
        Err(Errno::AlreadyExists)
    );
}
