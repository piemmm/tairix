//! File creation and I/O conformance (`open(2)` with `O_CREAT`, `read`,
//! `write`): creation, `EEXIST`, `ENOENT`, `EISDIR`, `ENOTDIR`, and the
//! round-trip of bytes through the driver.

use tairix_test_posix_fs_suite::*;

#[test]
fn create_makes_an_empty_regular_file() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("f"), new_file())
        .expect("create");

    let info = vol
        .stat(owner, &vol_path("f"), FinalLink::Follow)
        .expect("stat");
    assert_eq!(info.kind, FileKind::Regular);
    assert_eq!(info.size, 0);
}

#[test]
fn create_existing_name_is_already_exists() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("dup"), new_file())
        .expect("first create");
    assert_eq!(
        vol.create(owner, &vol_path("dup"), new_file()),
        Err(Errno::AlreadyExists)
    );
}

#[test]
fn create_in_missing_directory_is_not_found() {
    let vol = arxfs_volume(false);
    assert_eq!(
        vol.create(ROOT.caller(), &vol_path("nodir/f"), new_file()),
        Err(Errno::NotFound)
    );
}

#[test]
fn write_then_read_round_trips_across_a_block_boundary() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("data.bin"), new_file())
        .expect("create");

    // Larger than one 512-byte block to exercise multi-block I/O.
    let payload: Vec<u8> = (0..1000u32)
        .map(|i| u8::try_from(i % 251).unwrap_or(0))
        .collect();
    let written = vol
        .write(owner, &vol_path("data.bin"), 0, &payload)
        .expect("write");
    assert_eq!(written, payload.len());

    let mut buf = vec![0u8; payload.len()];
    let read = vol
        .read(owner, &vol_path("data.bin"), 0, &mut buf)
        .expect("read");
    assert_eq!(read, payload.len());
    assert_eq!(buf, payload);
}

#[test]
fn write_at_offset_leaves_a_sparse_zero_prefix() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("sparse"), new_file())
        .expect("create");
    vol.write(owner, &vol_path("sparse"), 8, b"tail")
        .expect("write at offset 8");

    let mut buf = [0xAAu8; 12];
    let read = vol
        .read(owner, &vol_path("sparse"), 0, &mut buf)
        .expect("read");
    assert_eq!(read, 12);
    assert_eq!(&buf[..8], &[0u8; 8]);
    assert_eq!(&buf[8..], b"tail");
}

#[test]
fn write_to_a_directory_is_is_a_directory() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.mkdir(owner, &vol_path("d"), new_dir()).expect("mkdir");
    assert_eq!(
        vol.write(owner, &vol_path("d"), 0, b"x"),
        Err(Errno::IsADirectory)
    );
}

#[test]
fn read_of_a_directory_is_is_a_directory() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.mkdir(owner, &vol_path("d"), new_dir()).expect("mkdir");
    let mut buf = [0u8; 4];
    assert_eq!(
        vol.read(owner, &vol_path("d"), 0, &mut buf),
        Err(Errno::IsADirectory)
    );
}

#[test]
fn read_past_end_of_file_returns_zero_bytes() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();

    vol.create(owner, &vol_path("short"), new_file())
        .expect("create");
    vol.write(owner, &vol_path("short"), 0, b"hi")
        .expect("write");

    let mut buf = [0u8; 8];
    let read = vol
        .read(owner, &vol_path("short"), 16, &mut buf)
        .expect("read past EOF");
    assert_eq!(read, 0);
}

/// A create keeps the bits it asked for less the umask, and in a setgid
/// directory takes that directory's group, which a new directory inherits
/// with the setgid bit.
#[test]
fn a_create_is_born_with_its_mode_less_the_umask_in_a_setgid_directorys_group() {
    let vol = arxfs_volume(false);
    let owner = ROOT.caller();
    let asked = |mode| CreateMode::new(mode, 0o027).expect("in range");

    let shared = vol_path("shared");
    vol.mkdir(owner, &shared, asked(0o777)).expect("mkdir");
    vol.set_owner(owner, &shared, ROOT_UID, STAFF_GID)
        .expect("chgrp to a group the owner is in");
    vol.set_mode(owner, &shared, 0o2770).expect("setgid");
    vol.create(owner, &vol_path("shared/f"), asked(0o666))
        .expect("create");
    vol.mkdir(owner, &vol_path("shared/d"), asked(0o777))
        .expect("mkdir inside");

    for (name, mode) in [("shared/f", 0o640), ("shared/d", 0o2750)] {
        let info = vol
            .stat(owner, &vol_path(name), FinalLink::Follow)
            .expect("stat");
        assert_eq!(info.mode, mode, "{name}");
        assert_eq!(info.gid, STAFF_GID, "{name}");
    }
}
