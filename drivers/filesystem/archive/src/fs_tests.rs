//! The served archive end to end over an attachment in memory: read, written,
//! committed, opened again.

extern crate std;

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{
    FilesystemAttrs, FilesystemRead, FilesystemSecurity, FilesystemWrite, NodeKind, NodeSecurity,
    WriteRefusal,
};
use tairix_abi::DriverError;

use crate::test_support::{
    contents, empty_zip, foreign_zip, names, open, tar, walk, MemoryAttachment, Packing,
};

fn file_record() -> NodeSecurity {
    NodeSecurity::new(0o640, 1000, 100)
}

fn dir_record() -> NodeSecurity {
    NodeSecurity::new(0o750, 1000, 100)
}

#[test]
fn a_foreign_zips_members_read_through_their_directories() {
    let text = b"the quick brown fox jumps over the lazy dog ".repeat(200);
    let archive = foreign_zip(&[
        (b"docs/readme.txt", b"stored bytes", Packing::Stored),
        (b"docs/big.txt", &text, Packing::Deflated),
        (b"caf\x82.txt", b"cp437", Packing::Stored),
    ]);
    let attachment = MemoryAttachment::new(&archive, false, b"archive");
    let mut fs = open(&attachment);
    let root = fs.root();
    assert_eq!(
        names(&mut fs, root),
        [b"caf\xc3\xa9.txt".to_vec(), b"docs".to_vec()]
    );
    let docs = walk(&mut fs, &[b"docs"]);
    assert_eq!(fs.node_info(docs).unwrap().kind, NodeKind::Directory);
    assert_eq!(
        names(&mut fs, docs),
        [b"big.txt".to_vec(), b"readme.txt".to_vec()]
    );
    let readme = walk(&mut fs, &[b"docs", b"readme.txt"]);
    assert_eq!(contents(&mut fs, readme), b"stored bytes");
    let big = walk(&mut fs, &[b"docs", b"big.txt"]);
    assert_eq!(contents(&mut fs, big), text);
    // A read from the middle, then one behind it.
    let mut part = [0u8; 10];
    assert_eq!(fs.read_at(big, 4000, &mut part), Ok(10));
    assert_eq!(part, text[4000..4010]);
    assert_eq!(fs.read_at(big, 5, &mut part), Ok(10));
    assert_eq!(part, text[5..15]);
    let node = fs.create(root, b"new", NodeKind::RegularFile, &file_record());
    assert_eq!(
        node.err(),
        Some(DriverError::ReadOnly),
        "a read-only attachment writes nothing"
    );
}

#[test]
fn a_member_whose_bytes_fail_their_check_is_refused() {
    let mut archive = foreign_zip(&[(b"a", b"payload bytes", Packing::Stored)]);
    // The stored bytes, changed under their CRC.
    let at = 30 + 1;
    archive[at] ^= 0xFF;
    let attachment = MemoryAttachment::new(&archive, false, b"archive");
    let mut fs = open(&attachment);
    let file = walk(&mut fs, &[b"a"]);
    let mut head = [0u8; 5];
    assert_eq!(
        fs.read_at(file, 0, &mut head),
        Ok(5),
        "nothing to hold part of the bytes to"
    );
    let mut rest = [0u8; 32];
    assert_eq!(
        fs.read_at(file, 5, &mut rest),
        Err(DriverError::MediumError),
        "read through in order, stored bytes are held to their CRC"
    );
    let deflated = foreign_zip(&[(b"b", &b"x".repeat(1000), Packing::Deflated)]);
    let mut broken = deflated.clone();
    let crc_at = 14;
    broken[crc_at] ^= 1;
    let attachment = MemoryAttachment::new(&broken, false, b"archive");
    let mut fs = open(&attachment);
    let file = walk(&mut fs, &[b"b"]);
    let mut all = vec![0u8; 1000];
    assert_eq!(fs.read_at(file, 0, &mut all), Ok(1000));
    // The central record's CRC is what the member is held to.
    let _ = deflated;
}

#[test]
fn files_written_into_an_empty_zip_are_there_once_it_is_opened_again() {
    let attachment = MemoryAttachment::new(&empty_zip(), true, b"archive");
    let mut fs = open(&attachment);
    // With a host's clock the changes wait in one transaction for the flush.
    let host = tairix_writeback::TestVolumeHost::leaked(1);
    fs.set_volume_host(tairix_writeback::TestVolumeHost::volume(), host);
    let root = fs.root();
    let dir = fs
        .create(root, b"Letters", NodeKind::Directory, &dir_record())
        .unwrap();
    let note = fs
        .create(dir, b"note.txt", NodeKind::RegularFile, &file_record())
        .unwrap();
    let body = b"Dear reader, ".repeat(500);
    fs.write_all(note, 0, &body).unwrap();
    let small = fs
        .create(root, b"tiny", NodeKind::RegularFile, &file_record())
        .unwrap();
    fs.write_all(small, 0, b"x").unwrap();
    fs.flush().unwrap();
    assert_eq!(attachment.asked().commits, 1);
    let archive = attachment.archive();
    assert_ne!(archive, empty_zip());
    assert_eq!(
        attachment.staging_len(),
        0,
        "the staging file empties once committed"
    );

    let again = MemoryAttachment::new(&archive, true, b"archive");
    let mut reopened = open(&again);
    let note = walk(&mut reopened, &[b"Letters", b"note.txt"]);
    assert_eq!(contents(&mut reopened, note), body);
    let tiny = walk(&mut reopened, &[b"tiny"]);
    assert_eq!(contents(&mut reopened, tiny), b"x");
    let letters = walk(&mut reopened, &[b"Letters"]);
    assert_eq!(
        reopened.node_info(letters).unwrap().kind,
        NodeKind::Directory
    );
}

#[test]
fn an_unchanged_member_is_copied_by_the_kernel_not_encoded_again() {
    let text = b"unchanged and long ".repeat(400);
    let archive = foreign_zip(&[
        (b"kept.txt", &text, Packing::Deflated),
        (b"other", b"o", Packing::Stored),
    ]);
    let attachment = MemoryAttachment::new(&archive, true, b"archive");
    let mut fs = open(&attachment);
    let root = fs.root();
    fs.remove(root, b"other").unwrap();
    fs.flush().unwrap();
    let asked = attachment.asked();
    let packed = archive.len() as u64;
    assert!(
        asked.copied > 0 && asked.copied < packed,
        "the member's compressed bytes crossed in the kernel"
    );
    let mut reopened = open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"archive",
    ));
    let kept = walk(&mut reopened, &[b"kept.txt"]);
    assert_eq!(contents(&mut reopened, kept), text);
    assert_eq!(
        reopened.lookup(reopened.root(), b"other").err(),
        Some(DriverError::NotFound)
    );
}

#[test]
fn a_rename_and_an_overwrite_survive_the_commit() {
    let archive = foreign_zip(&[
        (b"a.txt", b"first", Packing::Stored),
        (b"b.txt", b"second", Packing::Stored),
    ]);
    let attachment = MemoryAttachment::new(&archive, true, b"archive");
    let mut fs = open(&attachment);
    let root = fs.root();
    let dir = fs
        .create(root, b"moved", NodeKind::Directory, &dir_record())
        .unwrap();
    fs.rename(root, b"a.txt", dir, b"renamed.txt").unwrap();
    let b = walk(&mut fs, &[b"b.txt"]);
    fs.write_all(b, 3, b"OND!!").unwrap();
    fs.flush().unwrap();
    let mut reopened = open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"archive",
    ));
    let renamed = walk(&mut reopened, &[b"moved", b"renamed.txt"]);
    assert_eq!(contents(&mut reopened, renamed), b"first");
    let b = walk(&mut reopened, &[b"b.txt"]);
    assert_eq!(contents(&mut reopened, b), b"secOND!!");
}

#[test]
fn a_held_member_removed_before_a_commit_still_reads() {
    let archive = foreign_zip(&[(b"held", b"still here", Packing::Deflated)]);
    let attachment = MemoryAttachment::new(&archive, true, b"archive");
    let mut fs = open(&attachment);
    let root = fs.root();
    let held = walk(&mut fs, &[b"held"]);
    let pinned = fs.pin(held).unwrap();
    fs.remove(root, b"held").unwrap();
    fs.flush().unwrap();
    assert_eq!(contents(&mut fs, pinned), b"still here");
    assert_eq!(fs.node_info(pinned).unwrap().nlink, 0);
    assert_eq!(
        fs.unpin(pinned),
        Ok(tairix_abi::driver::filesystem::Unpinned::Freed)
    );
}

#[test]
fn a_risc_os_filetype_travels_through_a_zip() {
    let attachment = MemoryAttachment::new(&empty_zip(), true, b"archive");
    let mut fs = open(&attachment);
    let root = fs.root();
    let sprite = fs
        .create(root, b"Sprites", NodeKind::RegularFile, &file_record())
        .unwrap();
    fs.write_all(sprite, 0, b"sprite area").unwrap();
    fs.set_attr(sprite, b"acorn.filetype", b"ff9").unwrap();
    fs.flush().unwrap();
    let mut reopened = open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"archive",
    ));
    let sprite = walk(&mut reopened, &[b"Sprites"]);
    let mut value = [0u8; 16];
    let len = reopened
        .get_attr(sprite, b"acorn.filetype", &mut value)
        .unwrap()
        .expect("typed");
    assert_eq!(&value[..len], b"ff9");
}

#[test]
fn a_tar_reads_and_writes_back() {
    let archive = tar(&[
        (b"./src/", b""),
        (b"./src/main.rs", b"fn main() {}"),
        (b"./README", b"read me"),
    ]);
    let attachment = MemoryAttachment::new(&archive, true, b"archive");
    let mut fs = open(&attachment);
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [b"README".to_vec(), b"src".to_vec()]);
    let main = walk(&mut fs, &[b"src", b"main.rs"]);
    assert_eq!(contents(&mut fs, main), b"fn main() {}");
    let src = walk(&mut fs, &[b"src"]);
    let lib = fs
        .create(src, b"lib.rs", NodeKind::RegularFile, &file_record())
        .unwrap();
    fs.write_all(lib, 0, b"pub mod x;").unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    assert_eq!(written.len() % (20 * 512), 0, "padded to a whole record");
    let mut reopened = open(&MemoryAttachment::new(&written, false, b"archive"));
    let lib = walk(&mut reopened, &[b"src", b"lib.rs"]);
    assert_eq!(contents(&mut reopened, lib), b"pub mod x;");
    let main = walk(&mut reopened, &[b"src", b"main.rs"]);
    assert_eq!(contents(&mut reopened, main), b"fn main() {}");
    let readme = walk(&mut reopened, &[b"README"]);
    assert_eq!(contents(&mut reopened, readme), b"read me");
}

#[test]
fn a_long_tar_path_and_a_risc_os_filetype_travel_as_pax_records() {
    let archive = tar(&[(b"a", b"1")]);
    let attachment = MemoryAttachment::new(&archive, true, b"archive");
    let mut fs = open(&attachment);
    let mut dir = fs.root();
    let long = [b'd'; 60];
    for _ in 0..4 {
        dir = fs
            .create(dir, &long, NodeKind::Directory, &dir_record())
            .unwrap();
    }
    let file = fs
        .create(dir, b"Draw,aff", NodeKind::RegularFile, &file_record())
        .unwrap();
    fs.write_all(file, 0, b"drawing").unwrap();
    fs.set_attr(file, b"acorn.filetype", b"aff").unwrap();
    fs.flush().unwrap();
    let mut reopened = open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"archive",
    ));
    let path: Vec<&[u8]> = vec![&long[..], &long[..], &long[..], &long[..], b"Draw,aff"];
    let file = walk(&mut reopened, &path);
    assert_eq!(contents(&mut reopened, file), b"drawing");
    let mut value = [0u8; 16];
    let len = reopened
        .get_attr(file, b"acorn.filetype", &mut value)
        .unwrap()
        .unwrap();
    assert_eq!(&value[..len], b"aff");
}

#[test]
fn a_member_path_that_climbs_out_is_left_out() {
    let archive = tar(&[(b"../escape", b"x"), (b"inside", b"y")]);
    let attachment = MemoryAttachment::new(&archive, false, b"archive");
    let mut fs = open(&attachment);
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [b"inside".to_vec()]);
}

#[test]
fn a_stored_member_changed_in_part_or_cut_short_keeps_to_its_check() {
    let archive = foreign_zip(&[(b"a", b"payload bytes", Packing::Stored)]);
    // Cut short, the bytes left read through without their whole CRC.
    let attachment = MemoryAttachment::new(&archive, true, b"archive");
    let mut fs = open(&attachment);
    let file = walk(&mut fs, &[b"a"]);
    fs.truncate(file, 7).unwrap();
    assert_eq!(contents(&mut fs, file), b"payload");
    fs.flush().unwrap();
    let mut reopened = open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"archive",
    ));
    let file = walk(&mut reopened, &[b"a"]);
    assert_eq!(contents(&mut reopened, file), b"payload");
    // Damaged bytes are never given a fresh check by a change in part.
    let mut damaged = archive.clone();
    damaged[31] ^= 0xFF;
    let attachment = MemoryAttachment::new(&damaged, true, b"archive");
    let mut fs = open(&attachment);
    let file = walk(&mut fs, &[b"a"]);
    assert_eq!(fs.write_at(file, 0, b"P"), Err(DriverError::MediumError));
}

#[test]
fn a_zip_keeps_a_members_mode_but_never_an_owner_it_has_no_place_for() {
    let attachment = MemoryAttachment::new(&empty_zip(), true, b"archive");
    let mut fs = open(&attachment);
    let root = fs.root();
    let file = fs
        .create(root, b"f", NodeKind::RegularFile, &file_record())
        .unwrap();
    let born = fs.security(file).unwrap();
    assert_eq!((born.mode, born.uid, born.gid), (0o640, 0, 0));
    assert_eq!(fs.records_ownership(file), Ok(false));
    assert_eq!(
        fs.set_security(file, NodeSecurity::new(0o600, 1000, 100)),
        Err(DriverError::Unsupported)
    );
    fs.set_security(file, NodeSecurity::new(0o600, 0, 0))
        .unwrap();
    fs.flush().unwrap();
    let mut reopened = open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"archive",
    ));
    let file = walk(&mut reopened, &[b"f"]);
    assert_eq!(reopened.security(file).unwrap().mode, 0o600);
}

#[test]
fn a_tar_keeps_a_members_owner() {
    let attachment = MemoryAttachment::new(&tar(&[(b"f", b"x")]), true, b"archive");
    let mut fs = open(&attachment);
    let file = walk(&mut fs, &[b"f"]);
    assert_eq!(fs.records_ownership(file), Ok(true));
    fs.set_security(file, NodeSecurity::new(0o640, 2000, 200))
        .unwrap();
    fs.flush().unwrap();
    let mut reopened = open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"archive",
    ));
    let file = walk(&mut reopened, &[b"f"]);
    let kept = reopened.security(file).unwrap();
    assert_eq!((kept.mode, kept.uid, kept.gid), (0o640, 2000, 200));
}

#[test]
fn an_archive_whose_members_cannot_all_be_served_is_never_written_back() {
    // A damaged header part way: what follows it would be lost.
    let mut damaged = tar(&[(b"first", b"1")]);
    // Its two zero blocks, which would end it, taken off.
    damaged.truncate(damaged.len() - 1024);
    let mut bogus = [b'?'; 512];
    bogus[..4].copy_from_slice(b"junk");
    damaged.extend_from_slice(&bogus);
    damaged.extend_from_slice(&tar(&[(b"second", b"2")]));
    let mut fs = open(&MemoryAttachment::new(&damaged, true, b"archive"));
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::Damaged));
    let root = fs.root();
    assert_eq!(
        fs.create(root, b"new", NodeKind::RegularFile, &file_record()),
        Err(DriverError::ReadOnly)
    );
    // A member left out for its path.
    let fs = open(&MemoryAttachment::new(
        &foreign_zip(&[
            (b"../escape", b"x", Packing::Stored),
            (b"kept", b"y", Packing::Stored),
        ]),
        true,
        b"archive",
    ));
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::UnsupportedFeature));
}

#[test]
fn an_encrypted_member_is_kept_as_it_is_and_never_renamed() {
    // Coded shorter than it decodes, so only its coded length copies it.
    let secret = b"ciphertext ".repeat(64);
    let mut archive = foreign_zip(&[
        (b"secret", &secret, Packing::Deflated),
        (b"plain", b"text", Packing::Stored),
    ]);
    // The first local header's coded length, and the bytes after its name.
    let packed = usize::try_from(u32::from_le_bytes(archive[18..22].try_into().unwrap())).unwrap();
    let coded = archive[30 + 6..30 + 6 + packed].to_vec();
    assert!(packed < secret.len());
    // The encryption flag, in the local header and the central record.
    archive[6] |= 1;
    let central = archive
        .windows(4)
        .position(|window| window == [0x50, 0x4B, 0x01, 0x02])
        .unwrap();
    archive[central + 8] |= 1;
    let attachment = MemoryAttachment::new(&archive, true, b"archive");
    let mut fs = open(&attachment);
    let root = fs.root();
    let secret = walk(&mut fs, &[b"secret"]);
    let mut buf = [0u8; 16];
    assert_eq!(
        fs.read_at(secret, 0, &mut buf),
        Err(DriverError::PermissionDenied)
    );
    assert_eq!(
        fs.rename(root, b"secret", root, b"open"),
        Err(DriverError::PermissionDenied)
    );
    assert_eq!(
        fs.set_security(secret, NodeSecurity::new(0o600, 0, 0)),
        Err(DriverError::PermissionDenied)
    );
    fs.rename(root, b"plain", root, b"renamed").unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    assert!(written
        .windows(coded.len())
        .any(|window| window == coded.as_slice()));
    let mut reopened = open(&MemoryAttachment::new(&written, false, b"archive"));
    let renamed = walk(&mut reopened, &[b"renamed"]);
    assert_eq!(contents(&mut reopened, renamed), b"text");
    walk(&mut reopened, &[b"secret"]);
}

#[test]
fn directories_made_past_the_depth_bound_are_refused_and_the_archive_still_commits() {
    let depth = crate::tree::MAX_DEPTH;
    let attachment = MemoryAttachment::new(&empty_zip(), true, b"archive");
    let mut fs = open(&attachment);
    let mut at = fs.root();
    for _ in 0..depth {
        at = fs
            .create(at, b"d", NodeKind::Directory, &dir_record())
            .unwrap();
    }
    assert_eq!(
        fs.create(at, b"d", NodeKind::Directory, &dir_record()),
        Err(DriverError::LengthOutOfRange)
    );
    let leaf = fs
        .create(at, b"leaf", NodeKind::RegularFile, &file_record())
        .unwrap();
    fs.write_at(leaf, 0, b"deep").unwrap();
    fs.flush().unwrap();

    let mut reopened = open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"archive",
    ));
    let mut path = vec![&b"d"[..]; depth];
    path.push(b"leaf");
    let leaf = walk(&mut reopened, &path);
    assert_eq!(contents(&mut reopened, leaf), b"deep");
}

#[test]
fn a_change_whose_commit_fails_stands_and_every_flush_after_says_so() {
    let original = tar(&[(b"a", b"1")]);
    let attachment = MemoryAttachment::new(&original, true, b"a.tar");
    let mut fs = open(&attachment);
    attachment.fill();
    let root = fs.root();
    let made = fs.create(root, b"made", NodeKind::RegularFile, &file_record());
    assert!(made.is_ok(), "the change is made: {made:?}");
    assert_eq!(fs.lookup(root, b"made"), made);
    for _ in 0..2 {
        assert_eq!(fs.flush(), Err(DriverError::NoSpace));
    }
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::CommitFailed));
    assert_eq!(
        fs.create(root, b"more", NodeKind::RegularFile, &file_record()),
        Err(DriverError::ReadOnly)
    );
    assert_eq!(attachment.asked().commits, 0);
    assert_eq!(attachment.archive(), original);
}

/// What `attachment`'s archive holds once it is opened again.
fn reopened(attachment: &MemoryAttachment) -> crate::fs::ArchiveFs<tairix_fsserve::MemoryAccess> {
    open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"archive",
    ))
}

/// A zip member keeps a modification time to the second and no access time;
/// a tar member keeps both to the nanosecond, its access time recorded from
/// the change on; and what each keeps is what reads back once committed.
#[test]
fn a_member_keeps_the_times_its_record_holds_and_no_others() {
    use tairix_abi::time::Time64;
    use tairix_abi::{StampChange, TimesChange, TimesKept};
    let change = |accessed, modified, nearest| TimesChange {
        accessed,
        modified,
        nearest,
    };
    let in_1950 = Time64::from_secs(-631_152_000);
    let between_seconds = Time64::new(-631_152_000, 7).expect("canonical");

    let attachment = MemoryAttachment::new(&empty_zip(), true, b"archive");
    let mut fs = open(&attachment);
    let root = fs.root();
    let file = fs
        .create(root, b"f", NodeKind::RegularFile, &file_record())
        .unwrap();
    assert_eq!(
        fs.set_times(
            file,
            change(StampChange::Keep, StampChange::At(between_seconds), false)
        ),
        Err(DriverError::OutOfRange),
        "a zip keeps whole seconds"
    );
    assert_eq!(
        fs.set_times(
            file,
            change(StampChange::At(in_1950), StampChange::At(in_1950), false)
        ),
        Err(DriverError::OutOfRange),
        "a zip keeps no access time"
    );
    assert_eq!(
        fs.set_times(
            file,
            change(StampChange::At(in_1950), StampChange::At(in_1950), true)
        ),
        Ok(TimesKept::Nearest)
    );
    fs.flush().unwrap();
    let mut zip = reopened(&attachment);
    let file = walk(&mut zip, &[b"f"]);
    let times = zip.node_info(file).unwrap().times;
    assert_eq!(
        (times.modified, times.accessed),
        (in_1950, Time64::UNIX_EPOCH)
    );

    let attachment = MemoryAttachment::new(&tar(&[(b"t", b"body")]), true, b"archive");
    let mut fs = open(&attachment);
    let file = walk(&mut fs, &[b"t"]);
    assert_eq!(
        fs.set_times(
            file,
            change(
                StampChange::At(between_seconds),
                StampChange::At(between_seconds),
                false
            )
        ),
        Ok(TimesKept::Exactly)
    );
    fs.flush().unwrap();
    let mut tar = reopened(&attachment);
    let file = walk(&mut tar, &[b"t"]);
    let times = tar.node_info(file).unwrap().times;
    assert_eq!(
        (times.modified, times.accessed),
        (between_seconds, between_seconds)
    );
    assert_eq!(contents(&mut tar, file), b"body");
}
