//! Spark and ARC archives, built here as their archivers lay them out: read
//! — nested directories, RISC OS names and words, plain ARC members among
//! them — written back with every unchanged record copied, and refused where
//! the format has no place for a change.

extern crate std;

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{
    FilesystemAttrs, FilesystemRead, FilesystemSecurity, FilesystemWrite, NodeKind, NodeSecurity,
    WriteRefusal,
};
use tairix_abi::volume::FormatName;
use tairix_abi::DriverError;

use crate::test_support::{contents, names, open, walk, MemoryAttachment};

/// A typed load word: filetype `filetype`, the stamp's top byte `high`.
const fn typed(filetype: u32, high: u32) -> u32 {
    0xFFF0_0000 | filetype << 8 | high
}

/// The stamp of 2024-05-06 07:08:09.10 UTC, in centiseconds since 1900.
const STAMP: u64 = 3_924_016_089 * 100 + 10;

/// A member's record: its header, the RISC OS words where `words`, then its
/// coded bytes.
fn record(
    kind: u8,
    name: &[u8],
    coded: &[u8],
    len: usize,
    crc: u16,
    words: Option<(u32, u32, u8)>,
) -> Vec<u8> {
    let mut out = vec![0x1A, kind];
    let mut field = [0u8; 13];
    field[..name.len()].copy_from_slice(name);
    out.extend_from_slice(&field);
    out.extend_from_slice(&u32::try_from(coded.len()).unwrap().to_le_bytes());
    // 2024-05-06 07:08:08.
    out.extend_from_slice(&(44u16 << 9 | 5 << 5 | 6).to_le_bytes());
    out.extend_from_slice(&(7u16 << 11 | 8 << 5 | 4).to_le_bytes());
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&u32::try_from(len).unwrap().to_le_bytes());
    if let Some((load, exec, attr)) = words {
        out.extend_from_slice(&load.to_le_bytes());
        out.extend_from_slice(&exec.to_le_bytes());
        out.extend_from_slice(&u32::from(attr).to_le_bytes());
    }
    out.extend_from_slice(coded);
    out
}

/// A file stored as it is, typed as `filetype`.
fn stored(name: &[u8], data: &[u8], filetype: u32) -> Vec<u8> {
    let words = (
        typed(filetype, u32::try_from(STAMP >> 32).unwrap()),
        u32::try_from(STAMP & 0xFFFF_FFFF).unwrap(),
        0x33,
    );
    record(
        0x82,
        name,
        data,
        data.len(),
        tairix_crc16::checksum(data),
        Some(words),
    )
}

/// RLE90, as Spark packs: a run of three or more as the byte, the marker and
/// its count, the marker escaped.
fn pack(plain: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < plain.len() {
        let byte = plain[at];
        let run = plain[at..]
            .iter()
            .take(255)
            .take_while(|&&next| next == byte)
            .count();
        if byte == 0x90 {
            out.extend_from_slice(&[0x90, 0]);
            at += 1;
        } else if run >= 3 {
            out.extend_from_slice(&[byte, 0x90, u8::try_from(run).unwrap()]);
            at += run;
        } else {
            out.push(byte);
            at += 1;
        }
    }
    out
}

/// A directory: a nested archive of `members`, ended as Spark ends one.
fn directory(name: &[u8], members: &[Vec<u8>]) -> Vec<u8> {
    let nested = archive(members);
    let words = (
        typed(0xDDC, u32::try_from(STAMP >> 32).unwrap()),
        u32::try_from(STAMP & 0xFFFF_FFFF).unwrap(),
        0x33,
    );
    record(0x82, name, &nested, nested.len(), 0, Some(words))
}

fn archive(members: &[Vec<u8>]) -> Vec<u8> {
    let mut out = members.concat();
    out.extend_from_slice(&[0x1A, 0x80]);
    out
}

const README: &[u8] = b"A Spark archive, read and written.\n";

/// `!App` holding a packed `!Run` and a `ReadMe/txt`; `Notes` beside it, and
/// a plain ARC member `DOS.TXT` with a DOS name.
fn sample() -> Vec<u8> {
    let run = b"Run <Obey$Dir>.!RunImage\n\n\n\n\n\n\n\n\n\n\n";
    let packed = pack(run);
    let words = (
        typed(0xFEB, u32::try_from(STAMP >> 32).unwrap()),
        u32::try_from(STAMP & 0xFFFF_FFFF).unwrap(),
        0x33,
    );
    let app = directory(
        b"!App",
        &[
            record(
                0x83,
                b"!Run",
                &packed,
                run.len(),
                tairix_crc16::checksum(run),
                Some(words),
            ),
            stored(b"ReadMe/txt", README, 0xFFF),
        ],
    );
    let dos = record(
        0x02,
        b"DOS.TXT",
        b"dos",
        3,
        tairix_crc16::checksum(b"dos"),
        None,
    );
    archive(&[app, stored(b"Notes\xA0One", b"hard space", 0xFFF), dos])
}

fn file_record() -> NodeSecurity {
    NodeSecurity::new(0o644, 1000, 100)
}

#[test]
fn a_spark_archive_reads_as_its_nested_directories_and_risc_os_names() {
    let mut fs = open(&MemoryAttachment::new(&sample(), false, b"sample.spk"));
    assert_eq!(fs.format_name(), FormatName::Spark);
    let root = fs.root();
    assert_eq!(
        names(&mut fs, root),
        [
            b"!App".to_vec(),
            b"DOS.TXT".to_vec(),
            "Notes\u{a0}One".as_bytes().to_vec()
        ]
    );
    let app = walk(&mut fs, &[b"!App"]);
    assert_eq!(fs.node_info(app).unwrap().kind, NodeKind::Directory);
    let readme = walk(&mut fs, &[b"!App", b"ReadMe.txt"]);
    assert_eq!(contents(&mut fs, readme), README);
    let run = walk(&mut fs, &[b"!App", b"!Run"]);
    assert!(contents(&mut fs, run).starts_with(b"Run <Obey$Dir>"));
    let mut value = [0u8; 16];
    let len = fs
        .get_attr(run, b"acorn.filetype", &mut value)
        .unwrap()
        .unwrap();
    assert_eq!(&value[..len], b"feb");
    // The stamp is the member's modification time, to the centisecond.
    let modified = fs.node_info(readme).unwrap().times.modified;
    assert_eq!(
        (modified.secs(), modified.subsec_nanos()),
        (
            i64::try_from(STAMP / 100).unwrap() - 2_208_988_800,
            100_000_000
        )
    );
    // A plain ARC member's DOS stamp, with no words to say otherwise.
    let dos = walk(&mut fs, &[b"DOS.TXT"]);
    assert_eq!(contents(&mut fs, dos), b"dos");
    assert_eq!(
        fs.get_attr(dos, b"acorn.filetype", &mut value).unwrap(),
        None
    );
    // Names are RISC OS's, matched without regard to case.
    assert_eq!(fs.lookup(root, b"!app").unwrap(), app);
}

#[test]
fn unchanged_records_are_copied_whole_and_changes_written_afresh() {
    let attachment = MemoryAttachment::new(&sample(), true, b"sample.spk");
    let mut fs = open(&attachment);
    let host = tairix_writeback::TestVolumeHost::leaked(1);
    fs.set_volume_host(tairix_writeback::TestVolumeHost::volume(), host);
    let root = fs.root();
    let added = fs
        .create(root, b"Added", NodeKind::RegularFile, &file_record())
        .unwrap();
    let body = b"compressed by the writer, ".repeat(400);
    fs.write_all(added, 0, &body).unwrap();
    fs.rename(root, b"DOS.TXT", root, b"Dos.txt").unwrap();
    let notes = walk(&mut fs, &["Notes\u{a0}One".as_bytes()]);
    fs.set_attr(notes, b"acorn.filetype", b"ffd").unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    // `!App` and everything in it is copied whole, its records untouched.
    let app = &sample()[..sample().windows(4).position(|w| w == b"Note").unwrap() - 2];
    assert!(written.windows(app.len()).any(|window| window == app));
    assert!(attachment.asked().copied >= app.len() as u64);

    let mut reopened = open(&MemoryAttachment::new(&written, false, b"sample.spk"));
    let added = walk(&mut reopened, &[b"Added"]);
    assert_eq!(contents(&mut reopened, added), body);
    assert!(
        reopened.node_info(added).unwrap().allocated < body.len() as u64 / 4,
        "compressed"
    );
    let dos = walk(&mut reopened, &[b"Dos.txt"]);
    assert_eq!(contents(&mut reopened, dos), b"dos");
    let notes = walk(&mut reopened, &["Notes\u{a0}One".as_bytes()]);
    let mut value = [0u8; 16];
    let len = reopened
        .get_attr(notes, b"acorn.filetype", &mut value)
        .unwrap()
        .unwrap();
    assert_eq!(&value[..len], b"ffd");
    assert_eq!(contents(&mut reopened, notes), b"hard space");
    let readme = walk(&mut reopened, &[b"!App", b"ReadMe.txt"]);
    assert_eq!(contents(&mut reopened, readme), README);
}

#[test]
fn a_member_removed_from_a_directory_has_it_written_afresh() {
    let attachment = MemoryAttachment::new(&sample(), true, b"sample.spk");
    let mut fs = open(&attachment);
    let app = walk(&mut fs, &[b"!App"]);
    fs.remove(app, b"!Run").unwrap();
    fs.flush().unwrap();
    let mut reopened = open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"sample.spk",
    ));
    let app = walk(&mut reopened, &[b"!App"]);
    assert_eq!(names(&mut reopened, app), [b"ReadMe.txt".to_vec()]);
    let readme = walk(&mut reopened, &[b"!App", b"ReadMe.txt"]);
    assert_eq!(contents(&mut reopened, readme), README);
}

/// A member named as RISC OS could not have named it is left out, and the
/// archive is not written back, which would lose it.
#[test]
fn a_member_named_as_no_risc_os_name_can_be_is_left_out_and_kept() {
    for name in [&b"wild*card"[..], b"has space", b"colon:", b"a.b"] {
        let bytes = archive(&[stored(b"Kept", b"kept", 0xFFF), stored(name, b"x", 0xFFF)]);
        let mut fs = open(&MemoryAttachment::new(&bytes, true, b"odd.spk"));
        let root = fs.root();
        assert_eq!(names(&mut fs, root), [b"Kept".to_vec()], "{name:?}");
        assert_eq!(
            fs.write_refusal(),
            Some(WriteRefusal::UnsupportedFeature),
            "{name:?}"
        );
    }
}

#[test]
fn what_the_format_has_no_place_for_is_refused() {
    let attachment = MemoryAttachment::new(&sample(), true, b"sample.spk");
    let mut fs = open(&attachment);
    let root = fs.root();
    for refused in [
        &b"Thirteen-long"[..],
        b"has space",
        b"dollar$",
        "\u{263a}".as_bytes(),
    ] {
        assert_eq!(
            fs.create(root, refused, NodeKind::RegularFile, &file_record()),
            Err(DriverError::LengthOutOfRange),
            "{refused:?}"
        );
    }
    assert_eq!(
        fs.create_link(root, b"Link", b"Notes", &file_record()),
        Err(DriverError::Unsupported)
    );
    let notes = walk(&mut fs, &["Notes\u{a0}One".as_bytes()]);
    assert_eq!(
        fs.set_security(notes, NodeSecurity::new(0o600, 0, 0)),
        Err(DriverError::Unsupported)
    );
    assert_eq!(fs.records_ownership(notes), Ok(false));
    assert_eq!(
        fs.security(notes).unwrap().mode,
        NodeSecurity::unrecorded(NodeKind::RegularFile).mode
    );
    assert_eq!(
        fs.write_at(notes, u64::from(u32::MAX), b"x"),
        Err(DriverError::LengthOutOfRange)
    );
}

#[test]
fn a_member_no_method_here_reads_is_listed_and_kept_but_not_rewritten() {
    // A PAK-crushed member, and an ARC 5 crunched one garbled by a password.
    let crushed = record(0x0A, b"CRUSHED.DAT", b"opaque", 64, 0, None);
    let garbled = record(0x08, b"SECRET.TXT", b"\x44garbled", 64, 0, None);
    let archive = archive(&[stored(b"Plain", b"plain", 0xFFF), crushed, garbled]);
    let attachment = MemoryAttachment::new(&archive, true, b"old.arc");
    let mut fs = open(&attachment);
    let root = fs.root();
    let crushed = walk(&mut fs, &[b"CRUSHED.DAT"]);
    let mut buf = [0u8; 8];
    assert_eq!(
        fs.read_at(crushed, 0, &mut buf),
        Err(DriverError::Unsupported)
    );
    let garbled = walk(&mut fs, &[b"SECRET.TXT"]);
    assert_eq!(
        fs.read_at(garbled, 0, &mut buf),
        Err(DriverError::PermissionDenied)
    );
    // Its record would have to be written whole, and it cannot be.
    assert_eq!(
        fs.rename(root, b"SECRET.TXT", root, b"OPEN.TXT"),
        Err(DriverError::PermissionDenied)
    );
    // A change elsewhere copies both records as they were.
    fs.create(root, b"New", NodeKind::RegularFile, &file_record())
        .unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    assert!(written.windows(6).any(|window| window == b"opaque"));
    assert!(written.windows(8).any(|window| window == b"\x44garbled"));
}

#[test]
fn an_archive_with_records_that_cannot_be_written_back_opens_read_only() {
    // ARC 6's note record, which no member is.
    let note = record(0x14, b"", b"a comment", 9, 0, None);
    let fs = open(&MemoryAttachment::new(
        &archive(&[stored(b"Kept", b"kept", 0xFFF), note]),
        true,
        b"note.arc",
    ));
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::UnsupportedFeature));
    // A damaged header part way through ends what can be believed.
    let mut damaged = archive(&[stored(b"First", b"first", 0xFFF)]);
    damaged.truncate(damaged.len() - 2);
    damaged.extend_from_slice(b"\x1B\x82garbage");
    let mut fs = open(&MemoryAttachment::new(&damaged, true, b"damaged.spk"));
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::Damaged));
    let first = walk(&mut fs, &[b"First"]);
    assert_eq!(contents(&mut fs, first), b"first");
}

#[test]
fn a_datestamp_set_is_the_members_time_through_a_commit() {
    let attachment = MemoryAttachment::new(&sample(), true, b"sample.spk");
    let mut fs = open(&attachment);
    let notes = walk(&mut fs, &["Notes\u{a0}One".as_bytes()]);
    fs.set_attr(notes, b"acorn.datestamp", b"3a1b2c3d4e")
        .unwrap();
    let dated = tairix_fsmeta::preset::acorn::centiseconds_to_time64(0x3a_1b2c_3d4e).unwrap();
    assert_eq!(fs.node_info(notes).unwrap().times.modified, dated);
    fs.flush().unwrap();
    let mut reopened = open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"sample.spk",
    ));
    let notes = walk(&mut reopened, &["Notes\u{a0}One".as_bytes()]);
    let mut value = [0u8; 16];
    let len = reopened
        .get_attr(notes, b"acorn.datestamp", &mut value)
        .unwrap()
        .unwrap();
    assert_eq!(&value[..len], b"3a1b2c3d4e");
    assert_eq!(reopened.node_info(notes).unwrap().times.modified, dated);
}

/// A write to a typed member is stamped to the centisecond the member's
/// words hold, whatever finer reading the clock gives.
///
/// Regression: the clock's reading went to the words as it was, and any
/// nanoseconds past a centisecond made every such write fail.
#[test]
fn a_typed_member_written_under_a_fine_clock_is_stamped_to_its_centisecond() {
    let attachment = MemoryAttachment::new(&sample(), true, b"sample.spk");
    let mut fs = open(&attachment);
    let clock = tairix_abi::time::Time64::new(1_700_000_000, 123_456_789).expect("canonical");
    fs.set_volume_host(
        tairix_abi::driver::filesystem::clock::TestClock::volume(),
        tairix_abi::driver::filesystem::clock::TestClock::at(Some(clock)),
    );
    let notes = walk(&mut fs, &["Notes\u{a0}One".as_bytes()]);
    fs.set_attr(notes, b"acorn.filetype", b"ffd").unwrap();
    fs.write_at(notes, 0, b"typed")
        .expect("a typed member takes the write");
    assert_eq!(
        fs.node_info(notes).unwrap().times.modified,
        tairix_abi::time::Time64::new(1_700_000_000, 120_000_000).expect("canonical")
    );
}
