//! `truncate(2)` conformance: shrinking, growing (with a zero-filled
//! extension), `EISDIR`, and `ENOENT`.

use tairix_test_posix_fs_suite::*;

#[test]
fn truncate_shrinks_a_file() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("f"), new_file())
        .expect("create");
    vol.write(owner, &vol_path("f"), 0, b"0123456789")
        .expect("write");

    vol.truncate(owner, &vol_path("f"), 4)
        .expect("truncate to 4");

    let info = vol
        .stat(owner, &vol_path("f"), FinalLink::Follow)
        .expect("stat");
    assert_eq!(info.size, 4);

    let mut buf = [0u8; 16];
    let read = vol.read(owner, &vol_path("f"), 0, &mut buf).expect("read");
    assert_eq!(&buf[..read], b"0123");
}

#[test]
fn truncate_grows_a_file_with_zero_fill() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("g"), new_file())
        .expect("create");
    vol.write(owner, &vol_path("g"), 0, b"abc").expect("write");

    vol.truncate(owner, &vol_path("g"), 8).expect("grow to 8");

    let info = vol
        .stat(owner, &vol_path("g"), FinalLink::Follow)
        .expect("stat");
    assert_eq!(info.size, 8);

    let mut buf = [0xFFu8; 8];
    let read = vol.read(owner, &vol_path("g"), 0, &mut buf).expect("read");
    assert_eq!(read, 8);
    assert_eq!(&buf[..3], b"abc");
    assert_eq!(&buf[3..], &[0u8; 5]);
}

#[test]
fn truncate_of_a_directory_is_is_a_directory() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.mkdir(owner, &vol_path("d"), new_dir()).expect("mkdir");
    assert_eq!(
        vol.truncate(owner, &vol_path("d"), 0),
        Err(Errno::IsADirectory)
    );
}

#[test]
fn truncate_of_a_missing_file_is_not_found() {
    let vol = arxfs_volume(false);
    assert_eq!(
        vol.truncate(ROOT.caller(), &vol_path("missing"), 0),
        Err(Errno::NotFound)
    );
}
