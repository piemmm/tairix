//! `readdir`/`stat` conformance: directory listing, `ENOTDIR` on listing
//! a file, and the size/kind a `stat` reports.

use tairix_test_posix_fs_suite::*;

#[test]
fn readdir_lists_created_children() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("alpha"), new_file())
        .expect("create alpha");
    vol.create(owner, &vol_path("beta"), new_file())
        .expect("create beta");
    vol.mkdir(owner, &vol_path("gamma"), new_dir())
        .expect("mkdir gamma");

    let entries = vol.list(owner, MOUNT).expect("list mount root");
    let mut names = kinds(&entries);
    names.sort_by_key(|&(_, name)| name);
    assert_eq!(
        names,
        [
            (FileKind::Regular, "alpha"),
            (FileKind::Regular, "beta"),
            (FileKind::Directory, "gamma"),
        ]
    );
}

#[test]
fn readdir_of_a_file_is_not_a_directory() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("file"), new_file())
        .expect("create");
    assert_eq!(
        vol.list(owner, &vol_path("file")),
        Err(Errno::NotADirectory)
    );
}

#[test]
fn readdir_of_empty_directory_is_empty() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.mkdir(owner, &vol_path("empty"), new_dir())
        .expect("mkdir");
    let entries = vol.list(owner, &vol_path("empty")).expect("list");
    assert!(entries.is_empty());
}

#[test]
fn stat_reports_size_after_writes() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("sized"), new_file())
        .expect("create");
    vol.write(owner, &vol_path("sized"), 0, b"twelve bytes")
        .expect("write");

    let info = vol
        .stat(owner, &vol_path("sized"), FinalLink::Follow)
        .expect("stat");
    assert_eq!(info.kind, FileKind::Regular);
    assert_eq!(info.size, 12);
}

#[test]
fn stat_of_a_missing_path_is_not_found() {
    let vol = arxfs_volume(false);
    assert_eq!(
        vol.stat(ROOT.caller(), &vol_path("ghost"), FinalLink::Follow)
            .map(|info| info.size),
        Err(Errno::NotFound)
    );
}
