extern crate std;

use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{
    FilesystemAttrs, FilesystemRead, FilesystemSecurity, FilesystemWrite, NodeKind, NodeSecurity,
    WriteRefusal,
};
use tairix_abi::time::Time64;
use tairix_abi::DriverError;
use tairix_fuzzseed::meter::metered;

use super::{pax_time, ustar_split};
use crate::test_support::{
    contents, names, open, pax_records, seal_tar_header, tar, tar_header, tar_of, try_open, walk,
    MemoryAttachment,
};

fn record() -> NodeSecurity {
    NodeSecurity::new(0o644, 1000, 1000)
}

/// Whether `bytes` holds `part` anywhere.
fn holds(bytes: &[u8], part: &[u8]) -> bool {
    bytes.windows(part.len()).any(|window| window == part)
}

/// A file named `name` added to the root of `fs`, then committed.
fn add_and_commit<F: FilesystemRead + FilesystemWrite>(fs: &mut F, name: &[u8]) {
    let root = fs.root();
    let file = fs
        .create(root, name, NodeKind::RegularFile, &record())
        .unwrap();
    fs.write_all(file, 0, b"added").unwrap();
    fs.flush().unwrap();
}

#[test]
fn a_long_path_splits_where_both_halves_fit_and_the_name_names_something() {
    let dir = [b'd'; 60];
    let mut path = alloc::vec::Vec::new();
    for _ in 0..2 {
        path.extend_from_slice(&dir);
        path.push(b'/');
    }
    // `d60/d60/`: the trailing slash is no cut.
    let (prefix, name) = ustar_split(&path).expect("splits");
    assert_eq!(prefix, &dir[..]);
    assert_eq!(name.len(), 61);
    let mut long = alloc::vec::Vec::new();
    long.extend_from_slice(&[b'x'; 101]);
    assert_eq!(ustar_split(&long), None, "no slash to cut at");
}

#[test]
fn a_negative_pax_time_counts_its_fraction_back_from_its_whole_seconds() {
    let time = |secs, nanos| Time64::new(secs, nanos).unwrap();
    assert_eq!(pax_time(b"1.25"), Ok(time(1, 250_000_000)));
    assert_eq!(pax_time(b"-1.25"), Ok(time(-2, 750_000_000)));
    // Its whole seconds parse as zero, yet it is before the epoch.
    assert_eq!(pax_time(b"-0.5"), Ok(time(-1, 500_000_000)));
    assert_eq!(pax_time(b"-9223372036854775808"), Ok(time(i64::MIN, 0)));
    // No second comes before the earliest to count back from.
    assert_eq!(
        pax_time(b"-9223372036854775808.5"),
        Err(DriverError::BadMagic)
    );
}

#[test]
fn a_member_whose_size_runs_past_any_offset_is_refused() {
    let mut header = tar_header(b"huge", b'0', 0);
    // Base 256, as GNU tar writes a size octal cannot hold.
    header[124] = 0x80;
    header[125..128].fill(0);
    header[128..136].copy_from_slice(&(u64::MAX - 100).to_be_bytes());
    seal_tar_header(&mut header);
    let archive = tar_of(&[(header, &[])]);
    let refused = try_open(&MemoryAttachment::new(&archive, false, b"huge.tar"));
    assert_eq!(refused.err(), Some(DriverError::BadMagic));
}

/// The global header `git archive` writes, naming the commit.
fn git_comment() -> Vec<u8> {
    pax_records(&[(b"comment", b"0123456789abcdef0123456789abcdef01234567")])
}

#[test]
fn a_global_header_saying_nothing_of_any_member_is_kept_ahead_of_them_all() {
    let comment = git_comment();
    let archive = tar_of(&[
        (
            tar_header(b"pax_global_header", b'g', comment.len()),
            &comment,
        ),
        (tar_header(b"first", b'0', 1), b"1"),
    ]);
    let attachment = MemoryAttachment::new(&archive, true, b"git.tar");
    let mut fs = open(&attachment);
    assert_eq!(fs.write_refusal(), None);
    let root = fs.root();
    fs.rename(root, b"first", root, b"renamed").unwrap();
    fs.flush().unwrap();
    add_and_commit(&mut fs, b"second");
    let written = attachment.archive();
    assert_eq!(
        &written[..1024],
        &archive[..1024],
        "the global header first"
    );
    let mut reopened = open(&MemoryAttachment::new(&written, false, b"git.tar"));
    let root = reopened.root();
    assert_eq!(
        names(&mut reopened, root),
        [b"renamed".to_vec(), b"second".to_vec()]
    );
}

#[test]
fn a_global_header_saying_something_of_members_is_read_and_never_written_back() {
    let records = pax_records(&[(b"mtime", b"86400")]);
    let archive = tar_of(&[
        (tar_header(b"first", b'0', 1), b"1"),
        (tar_header(b"global", b'g', records.len()), &records),
        (tar_header(b"second", b'0', 1), b"2"),
    ]);
    let mut fs = open(&MemoryAttachment::new(&archive, true, b"archive.tar"));
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::UnsupportedFeature));
    let first = walk(&mut fs, &[b"first"]);
    let second = walk(&mut fs, &[b"second"]);
    let day = Time64::from_secs(86_400);
    assert_eq!(fs.node_info(second).unwrap().times.modified, day);
    assert_ne!(fs.node_info(first).unwrap().times.modified, day);
}

#[test]
fn anything_after_the_end_marker_leaves_the_archive_unwritten() {
    let mut archive = tar(&[(b"a", b"1")]);
    archive.resize(20 * 512, 0);
    let fs = open(&MemoryAttachment::new(&archive, true, b"a.tar"));
    assert_eq!(fs.write_refusal(), None, "zeros pad a record");
    archive.extend_from_slice(&tar(&[(b"appended", b"2")]));
    let mut fs = open(&MemoryAttachment::new(&archive, true, b"a.tar"));
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::UnsupportedFeature));
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [b"a".to_vec()]);
}

#[test]
fn a_tar_of_a_directory_records_its_root_as_dot_slash() {
    let mut dot = tar_header(b"./", b'5', 0);
    dot[100..107].copy_from_slice(b"0000700");
    seal_tar_header(&mut dot);
    let archive = tar_of(&[(dot, &[]), (tar_header(b"./a", b'0', 1), b"1")]);
    let attachment = MemoryAttachment::new(&archive, true, b"dir.tar");
    let mut fs = open(&attachment);
    assert_eq!(fs.write_refusal(), None);
    let root = fs.root();
    assert_eq!(fs.security(root).unwrap().mode & 0o7777, 0o700);
    add_and_commit(&mut fs, b"b");
    assert_eq!(&attachment.archive()[..512], &archive[..512]);
    fs.set_security(root, NodeSecurity::new(0o750, 1000, 1000))
        .unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    assert_eq!(&written[..3], b"./\0");
    let mut reopened = open(&MemoryAttachment::new(&written, false, b"dir.tar"));
    let root = reopened.root();
    assert_eq!(reopened.security(root).unwrap().mode & 0o7777, 0o750);
    assert_eq!(names(&mut reopened, root), [b"a".to_vec(), b"b".to_vec()]);
}

#[test]
fn a_root_changed_where_no_member_recorded_it_is_recorded_from_then_on() {
    let attachment = MemoryAttachment::new(&tar(&[(b"a", b"1")]), true, b"a.tar");
    let mut fs = open(&attachment);
    let root = fs.root();
    fs.set_security(root, NodeSecurity::new(0o700, 0, 0))
        .unwrap();
    fs.flush().unwrap();
    let mut reopened = open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"a.tar",
    ));
    let root = reopened.root();
    assert_eq!(reopened.security(root).unwrap().mode & 0o7777, 0o700);
}

#[test]
fn a_path_two_members_name_serves_the_later_and_is_never_written_back() {
    let archive = tar(&[(b"twice", b"first"), (b"twice", b"second")]);
    let mut fs = open(&MemoryAttachment::new(&archive, true, b"a.tar"));
    let twice = walk(&mut fs, &[b"twice"]);
    assert_eq!(contents(&mut fs, twice), b"second");
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::UnsupportedFeature));
}

#[test]
fn a_member_gnu_tar_stored_sparse_is_left_out() {
    let records = pax_records(&[
        (b"GNU.sparse.major", b"1"),
        (b"GNU.sparse.minor", b"0"),
        (b"GNU.sparse.name", b"sparse"),
        (b"GNU.sparse.realsize", b"1048576"),
    ]);
    let archive = tar_of(&[
        (
            tar_header(b"PaxHeaders/sparse", b'x', records.len()),
            &records,
        ),
        (tar_header(b"GNUSparseFile.0/sparse", b'0', 3), b"map"),
        (tar_header(b"plain", b'0', 1), b"p"),
    ]);
    let mut fs = open(&MemoryAttachment::new(&archive, true, b"a.tar"));
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [b"plain".to_vec()]);
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::UnsupportedFeature));
}

#[test]
fn a_changed_member_keeps_the_records_it_carried_and_its_owners_names() {
    let records = pax_records(&[
        (b"SCHILY.xattr.user.note", b"kept"),
        (b"comment", b"also kept"),
        (b"uname", b"alice"),
    ]);
    let mut header = tar_header(b"file", b'0', 4);
    header[265..270].copy_from_slice(b"alice");
    header[297..302].copy_from_slice(b"staff");
    seal_tar_header(&mut header);
    let archive = tar_of(&[
        (tar_header(b"gone", b'0', 4), b"gone"),
        (
            tar_header(b"PaxHeaders/file", b'x', records.len()),
            &records,
        ),
        (header, b"data"),
    ]);
    let attachment = MemoryAttachment::new(&archive, true, b"a.tar");
    let mut fs = open(&attachment);
    // Copied whole to where the member before it was, so the change after
    // reads them where they moved to.
    let root = fs.root();
    fs.remove(root, b"gone").unwrap();
    fs.flush().unwrap();
    fs.rename(root, b"file", root, b"renamed").unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    assert!(holds(&written, b"SCHILY.xattr.user.note=kept\n"));
    assert!(holds(&written, b"comment=also kept\n"));
    assert!(holds(&written, b"uname=alice\n"));
    assert!(holds(&written, b"alice\0"));
    assert!(holds(&written, b"staff\0"));
    let renamed = walk(&mut fs, &[b"renamed"]);
    fs.set_security(renamed, NodeSecurity::new(0o644, 2000, 2000))
        .unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    assert!(holds(&written, b"SCHILY.xattr.user.note=kept\n"));
    assert!(
        !holds(&written, b"alice"),
        "names a new owner does not have"
    );
    assert!(!holds(&written, b"staff"));
    let mut reopened = open(&MemoryAttachment::new(&written, false, b"a.tar"));
    let renamed = walk(&mut reopened, &[b"renamed"]);
    assert_eq!(contents(&mut reopened, renamed), b"data");
}

#[test]
fn a_global_header_is_read_through_not_copied_into_every_member() {
    let link = alloc::vec![b'l'; 60_000];
    let records = pax_records(&[(b"linkpath", &link)]);
    let spelt: Vec<Vec<u8>> = (0..200)
        .map(|n| std::format!("member{n}").into_bytes())
        .collect();
    let mut entries = alloc::vec![(
        tar_header(b"global", b'g', records.len()),
        records.as_slice()
    )];
    entries.extend(
        spelt
            .iter()
            .map(|name| (tar_header(name, b'0', 1), &b"x"[..])),
    );
    let attachment = MemoryAttachment::new(&tar_of(&entries), false, b"a.tar");
    let (fs, metering) = metered(|| open(&attachment));
    assert!(
        metering.requested < 2 << 20,
        "{} bytes asked for",
        metering.requested
    );
    drop(fs);
}

#[test]
fn a_tar_keeps_every_risc_os_attribute_bit() {
    let attachment = MemoryAttachment::new(&tar(&[(b"f", b"x")]), true, b"a.tar");
    let mut fs = open(&attachment);
    let file = walk(&mut fs, &[b"f"]);
    fs.set_attr(file, b"acorn.attr", b"RWP/re").unwrap();
    fs.flush().unwrap();
    let mut reopened = open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"a.tar",
    ));
    let file = walk(&mut reopened, &[b"f"]);
    let mut value = [0u8; 16];
    let len = reopened
        .get_attr(file, b"acorn.attr", &mut value)
        .unwrap()
        .unwrap();
    assert_eq!(&value[..len], b"RWP/re");
}
