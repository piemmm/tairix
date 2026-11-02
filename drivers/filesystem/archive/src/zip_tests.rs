//! zip members as other archivers code them: bzip2 decoded, and a method
//! this driver does not decode kept whole, its record's version needed with
//! it, through a rewrite.

extern crate std;

use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{
    FilesystemAttrs, FilesystemRead, FilesystemSecurity, FilesystemWrite, NodeKind, NodeSecurity,
    WriteRefusal,
};
use tairix_abi::time::Time64;
use tairix_abi::DriverError;
use tairix_fsmeta::preset::acorn;
use tairix_fuzzseed::meter::metered;

use crate::data::{Archived, Check, Coding, Lead};
use crate::region::Regions;
use crate::test_support::{
    contents, empty_zip, foreign_zip, names, open, walk, MemoryAttachment, Packing,
};
use crate::zip::located;

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// What a written archive's records say of one member.
#[derive(Debug, Eq, PartialEq)]
struct Said {
    /// The version needed, in the local header and the central record.
    needed: (u16, u16),
    method: u16,
    coded: Vec<u8>,
}

/// What the records of `archive`, which has no comment, say of `name`.
fn said(archive: &[u8], name: &[u8]) -> Said {
    let end = archive.len() - 22;
    assert_eq!(le32(archive, end), 0x0605_4b50);
    let mut at = usize::try_from(le32(archive, end + 16)).unwrap();
    for _ in 0..le16(archive, end + 10) {
        let name_len = usize::from(le16(archive, at + 28));
        let skip =
            name_len + usize::from(le16(archive, at + 30)) + usize::from(le16(archive, at + 32));
        if &archive[at + 46..at + 46 + name_len] == name {
            let local = usize::try_from(le32(archive, at + 42)).unwrap();
            let packed = usize::try_from(le32(archive, at + 20)).unwrap();
            let data = local
                + 30
                + usize::from(le16(archive, local + 26))
                + usize::from(le16(archive, local + 28));
            return Said {
                needed: (le16(archive, local + 4), le16(archive, at + 6)),
                method: le16(archive, at + 10),
                coded: archive[data..data + packed].to_vec(),
            };
        }
        at += 46 + skip;
    }
    std::panic!("no member {name:?}")
}

const TEXT: &[u8] = b"A member another archiver compressed with bzip2, block by block. ";

#[test]
fn a_bzip2_member_reads_from_anywhere_and_keeps_its_coding_through_a_rewrite() {
    // Past one block at the level the builder writes.
    let text = TEXT.repeat(3000);
    let archive = foreign_zip(&[
        (b"long.txt", &text, Packing::Bzip2),
        (b"tiny", b"t", Packing::Bzip2),
    ]);
    let coded = said(&archive, b"long.txt").coded;
    let attachment = MemoryAttachment::new(&archive, true, b"archive.zip");
    let mut fs = open(&attachment);
    let long = walk(&mut fs, &[b"long.txt"]);
    assert_eq!(contents(&mut fs, long), text);
    let mut buf = [0u8; 100];
    let got = fs.read_at(long, 150_000, &mut buf).unwrap();
    assert_eq!(&buf[..got], &text[150_000..150_100]);
    let tiny = walk(&mut fs, &[b"tiny"]);
    assert_eq!(contents(&mut fs, tiny), b"t");

    let root = fs.root();
    let added = fs
        .create(
            root,
            b"added",
            NodeKind::RegularFile,
            &NodeSecurity::new(0o644, 0, 0),
        )
        .unwrap();
    fs.write_at(added, 0, b"new").unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    assert_eq!(
        said(&written, b"long.txt"),
        Said {
            needed: (46, 46),
            method: 12,
            coded
        }
    );
    assert_eq!(said(&written, b"added").needed, (20, 20));
    let mut reopened = open(&MemoryAttachment::new(&written, false, b"archive.zip"));
    let long = walk(&mut reopened, &[b"long.txt"]);
    assert_eq!(contents(&mut reopened, long), text);
}

#[test]
fn a_damaged_bzip2_member_is_refused() {
    let text = TEXT.repeat(50);
    let mut archive = foreign_zip(&[(b"member", &text, Packing::Bzip2)]);
    // Into the block, past the stream header and the block magic.
    let data = 30 + b"member".len();
    archive[data + 20] ^= 0x10;
    let mut fs = open(&MemoryAttachment::new(&archive, false, b"archive.zip"));
    let member = walk(&mut fs, &[b"member"]);
    let mut buf = std::vec![0u8; text.len()];
    assert_eq!(
        fs.read_at(member, 0, &mut buf),
        Err(DriverError::MediumError)
    );
}

#[test]
fn a_member_in_a_method_not_decoded_keeps_its_record_through_a_rewrite() {
    // LZMA, which APPNOTE says needs 6.3 to extract.
    let lzma = Packing::Raw {
        method: 14,
        needed: 63,
        coded: b"coded as another archiver left it",
    };
    let archive = foreign_zip(&[
        (b"lzma", b"what it decodes to", lzma),
        (b"plain", b"text", Packing::Stored),
    ]);
    let attachment = MemoryAttachment::new(&archive, true, b"archive.zip");
    let mut fs = open(&attachment);
    let member = walk(&mut fs, &[b"lzma"]);
    let mut buf = [0u8; 32];
    assert_eq!(
        fs.read_at(member, 0, &mut buf),
        Err(DriverError::Unsupported)
    );

    let root = fs.root();
    fs.remove(root, b"plain").unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    assert_eq!(
        said(&written, b"lzma"),
        Said {
            needed: (63, 63),
            method: 14,
            coded: b"coded as another archiver left it".to_vec()
        }
    );
    let mut reopened = open(&MemoryAttachment::new(&written, false, b"archive.zip"));
    let root = reopened.root();
    assert_eq!(names(&mut reopened, root), [b"lzma".to_vec()]);
}

/// Where the end record of `zip` starts.
fn end_record(zip: &[u8]) -> usize {
    zip.windows(4)
        .rposition(|window| window == [0x50, 0x4B, 0x05, 0x06])
        .unwrap()
}

/// Where the first central record of `zip` starts.
fn central_record(zip: &[u8]) -> usize {
    zip.windows(4)
        .position(|window| window == [0x50, 0x4B, 0x01, 0x02])
        .unwrap()
}

/// A file named `name` added to the root of `fs`, then committed.
fn add_and_commit<F: FilesystemRead + FilesystemWrite>(fs: &mut F, name: &[u8]) {
    let root = fs.root();
    let file = fs
        .create(
            root,
            name,
            NodeKind::RegularFile,
            &NodeSecurity::new(0o644, 0, 0),
        )
        .unwrap();
    fs.write_all(file, 0, b"added").unwrap();
    fs.flush().unwrap();
}

/// The directory is read by its own length, so a count the end record gets
/// wrong loses no member; one no wrap explains leaves the archive unwritten.
#[test]
fn every_record_the_directory_holds_is_read_whatever_the_end_record_counts() {
    let mut zip = foreign_zip(&[
        (b"first", b"1", Packing::Stored),
        (b"second", b"2", Packing::Stored),
    ]);
    let end = end_record(&zip);
    for field in [8, 10] {
        zip[end + field..end + field + 2].copy_from_slice(&1u16.to_le_bytes());
    }
    let mut fs = open(&MemoryAttachment::new(&zip, true, b"archive.zip"));
    let root = fs.root();
    assert_eq!(
        names(&mut fs, root),
        [b"first".to_vec(), b"second".to_vec()]
    );
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::Damaged));
}

/// `zip`, its end record replaced by a ZIP64 end record counting `entries`,
/// that record's locator, and an end record deferring to both.
fn as_zip64(zip: &[u8], entries: u64) -> Vec<u8> {
    let end = end_record(zip);
    let directory_len = u64::from(le32(zip, end + 12));
    let directory = u64::from(le32(zip, end + 16));
    let mut out = zip[..end].to_vec();
    let record = out.len() as u64;
    out.extend_from_slice(&0x0606_4b50u32.to_le_bytes());
    out.extend_from_slice(&44u64.to_le_bytes());
    out.extend_from_slice(&[45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    for field in [entries, entries, directory_len, directory] {
        out.extend_from_slice(&field.to_le_bytes());
    }
    out.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&record.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&[0xFF; 12]);
    out.extend_from_slice(&[0, 0]);
    out
}

/// Sixteen bits count 65537 records as one; a ZIP64 count saying so is
/// wrong.
#[test]
fn only_a_sixteen_bit_count_may_wrap() {
    let spelled: Vec<Vec<u8>> = (0..=0x1_0000u32)
        .map(|n| std::format!("{n}").into_bytes())
        .collect();
    let members: Vec<(&[u8], &[u8], Packing<'_>)> = spelled
        .iter()
        .map(|name| (name.as_slice(), &b""[..], Packing::Stored))
        .collect();
    let wrapped = foreign_zip(&members);
    let fs = open(&MemoryAttachment::new(&wrapped, true, b"archive.zip"));
    assert_eq!(fs.write_refusal(), None);
    let mut fs = open(&MemoryAttachment::new(
        &as_zip64(&wrapped, 1),
        true,
        b"archive.zip",
    ));
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::Damaged));
    let root = fs.root();
    assert_eq!(names(&mut fs, root).len(), 0x1_0001);
}

/// A member whose record puts its data past the archive's end is left out.
#[test]
fn a_member_whose_data_lies_past_the_archive_is_left_out() {
    let mut zip = foreign_zip(&[
        (b"long", b"0123456789", Packing::Stored),
        (b"kept", b"k", Packing::Stored),
    ]);
    let central = central_record(&zip);
    zip[central + 20..central + 24].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
    let mut fs = open(&MemoryAttachment::new(&zip, true, b"archive.zip"));
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [b"kept".to_vec()]);
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::UnsupportedFeature));
}

/// A local header whose name and extra field push its member's bytes past
/// the archive's end places them nowhere.
#[test]
fn a_local_header_pushing_its_data_past_the_archive_is_refused() {
    let mut zip = foreign_zip(&[(b"pushed", b"0123456789", Packing::Stored)]);
    zip[28..30].copy_from_slice(&0xFFFFu16.to_le_bytes());
    let attachment = MemoryAttachment::new(&zip, false, b"archive.zip");
    let regions = Regions::new(attachment.access(), zip.len() as u64);
    let archived = Archived {
        at: 0,
        lead: Lead::ZipLocal,
        len: 10,
        coding: Coding::Stored,
        check: Check::Crc32(tairix_crc32::checksum(b"0123456789")),
    };
    assert_eq!(located(&regions, archived), Err(DriverError::MediumError));
    zip[28..30].copy_from_slice(&0u16.to_le_bytes());
    let attachment = MemoryAttachment::new(&zip, false, b"archive.zip");
    let regions = Regions::new(attachment.access(), zip.len() as u64);
    assert_eq!(located(&regions, archived).map(|at| at.at), Ok(36));
}

/// A name its record flags as UTF-8 that is none leaves its member out, not
/// the archive unopened.
#[test]
fn a_member_named_in_broken_utf8_is_left_out() {
    let mut zip = foreign_zip(&[
        (b"bad\x80", b"x", Packing::Stored),
        (b"good", b"y", Packing::Stored),
    ]);
    let central = central_record(&zip);
    zip[central + 9] |= 0x08;
    let mut fs = open(&MemoryAttachment::new(&zip, true, b"archive.zip"));
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [b"good".to_vec()]);
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::UnsupportedFeature));
}

/// A self-extractor's program ahead of the archive is kept by every commit,
/// not only the first.
#[test]
fn a_self_extractors_program_survives_every_commit() {
    let program = b"#!/bin/sh\nexec unzip \"$0\"\n".repeat(8);
    let mut archive = program.clone();
    archive.extend_from_slice(&foreign_zip(&[(b"inside", b"zipped", Packing::Stored)]));
    let attachment = MemoryAttachment::new(&archive, true, b"setup.zip");
    let mut fs = open(&attachment);
    for name in [b"one", b"two"] {
        add_and_commit(&mut fs, name);
        assert!(attachment.archive().starts_with(&program));
    }
    let mut reopened = open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"setup.zip",
    ));
    let inside = walk(&mut reopened, &[b"inside"]);
    assert_eq!(contents(&mut reopened, inside), b"zipped");
}

/// A member kept whole, for a method this driver cannot read, keeps its name
/// through a rewrite: written in UTF-8, and flagged so.
#[test]
fn a_kept_members_code_page_437_name_is_flagged_utf8_when_written_back() {
    let unknown = Packing::Raw {
        method: 99,
        needed: 63,
        coded: b"coded",
    };
    let zip = foreign_zip(&[(b"caf\x82", b"coded", unknown)]);
    let attachment = MemoryAttachment::new(&zip, true, b"archive.zip");
    let mut fs = open(&attachment);
    add_and_commit(&mut fs, b"other");
    let mut reopened = open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"archive.zip",
    ));
    let root = reopened.root();
    assert_eq!(
        names(&mut reopened, root),
        ["caf\u{e9}".as_bytes().to_vec(), b"other".to_vec()]
    );
}

/// Whether `bytes` holds `part` anywhere.
fn holds(bytes: &[u8], part: &[u8]) -> bool {
    bytes.windows(part.len()).any(|window| window == part)
}

/// The one-member `zip`, its central record given `extra`, `comment`, the
/// maker `made_by` and the attributes `external`.
fn with_central(zip: &[u8], extra: &[u8], comment: &[u8], made_by: u16, external: u32) -> Vec<u8> {
    let central = central_record(zip);
    let name_len = usize::from(le16(zip, central + 28));
    let mut record = zip[central..central + 46 + name_len].to_vec();
    record[4..6].copy_from_slice(&made_by.to_le_bytes());
    record[30..32].copy_from_slice(&u16::try_from(extra.len()).unwrap().to_le_bytes());
    record[32..34].copy_from_slice(&u16::try_from(comment.len()).unwrap().to_le_bytes());
    record[38..42].copy_from_slice(&external.to_le_bytes());
    record.extend_from_slice(extra);
    record.extend_from_slice(comment);
    let mut end = zip[end_record(zip)..].to_vec();
    end[12..16].copy_from_slice(&u32::try_from(record.len()).unwrap().to_le_bytes());
    [&zip[..central], &record, &end].concat()
}

/// An extra field of `id` holding `data`.
fn extra_field(id: u16, data: &[u8]) -> Vec<u8> {
    let len = u16::try_from(data.len()).unwrap();
    [&id.to_le_bytes()[..], &len.to_le_bytes(), data].concat()
}

#[test]
fn a_changed_member_keeps_what_its_central_record_carried() {
    // Reserved, then its one attribute: three FILETIMEs.
    let mut times = alloc::vec![0, 0, 0, 0, 1, 0, 24, 0];
    times.extend_from_slice(&[7; 24]);
    let ntfs = extra_field(0x000a, &times);
    let unix = extra_field(0x7875, &[1, 4, 0xe8, 3, 0, 0, 4, 0xe8, 3, 0, 0]);
    // Made on MS-DOS, read-only and archived, recording no mode.
    let zip = with_central(
        &foreign_zip(&[(b"file", b"data", Packing::Stored)]),
        &[ntfs.as_slice(), &unix].concat(),
        b"a comment",
        0x0014,
        0x21,
    );
    let attachment = MemoryAttachment::new(&zip, true, b"archive.zip");
    let mut fs = open(&attachment);
    let root = fs.root();
    fs.rename(root, b"file", root, b"renamed").unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    let central = central_record(&written);
    assert_eq!(le16(&written, central + 4), 0x0014, "still made on MS-DOS");
    assert_eq!(le32(&written, central + 38), 0x21);
    for kept in [&b"a comment"[..], &ntfs, &unix] {
        assert!(holds(&written[central..], kept));
    }
    let renamed = walk(&mut fs, &[b"renamed"]);
    fs.write_all(renamed, 0, b"more").unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    let central = central_record(&written);
    assert!(
        !holds(&written[central..], &ntfs),
        "the times it gave are not the member's"
    );
    assert!(holds(&written[central..], &unix));
    assert!(holds(&written[central..], b"a comment"));
    fs.set_security(renamed, NodeSecurity::new(0o600, 0, 0))
        .unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    let central = central_record(&written);
    assert_eq!(
        le16(&written, central + 4) >> 8,
        3,
        "made on Unix, for its mode"
    );
    assert_eq!(le32(&written, central + 38), 0o100_600 << 16 | 0x21);
    let mut reopened = open(&MemoryAttachment::new(&written, false, b"archive.zip"));
    let renamed = walk(&mut reopened, &[b"renamed"]);
    assert_eq!(reopened.security(renamed).unwrap().mode, 0o600);
    assert_eq!(contents(&mut reopened, renamed), b"more");
}

/// A one-member zip whose extended timestamp is `word`, beside the MS-DOS
/// date `dos_date`.
fn stamped(word: u32, dos_date: u16) -> Vec<u8> {
    let mut data = alloc::vec![1];
    data.extend_from_slice(&word.to_le_bytes());
    let mut zip = with_central(
        &foreign_zip(&[(b"dated", b"x", Packing::Stored)]),
        &extra_field(0x5455, &data),
        b"",
        0x0314,
        0o100_644 << 16,
    );
    let central = central_record(&zip);
    zip[central + 14..central + 16].copy_from_slice(&dos_date.to_le_bytes());
    zip
}

/// The MS-DOS date of `day`/`month`/`year`.
fn dos_date(year: u16, month: u16, day: u16) -> u16 {
    (year - 1980) << 9 | month << 5 | day
}

#[test]
fn an_extended_timestamp_holds_times_either_side_of_the_signed_range() {
    // A year before 1970, and a day in 2039, with the MS-DOS dates a writer
    // would give them: held at 1980, and the day itself.
    let year_before = -31_536_000i32;
    let in_2039 = 2_200_000_000u32;
    for (word, dos_date, secs) in [
        (
            year_before.cast_unsigned(),
            dos_date(1980, 1, 1),
            i64::from(year_before),
        ),
        (in_2039, dos_date(2039, 9, 18), i64::from(in_2039)),
    ] {
        let attachment = MemoryAttachment::new(&stamped(word, dos_date), true, b"archive.zip");
        let mut fs = open(&attachment);
        let dated = walk(&mut fs, &[b"dated"]);
        assert_eq!(
            fs.node_info(dated).unwrap().times.modified,
            Time64::from_secs(secs)
        );
        let root = fs.root();
        fs.rename(root, b"dated", root, b"renamed").unwrap();
        fs.flush().unwrap();
        let mut reopened = open(&MemoryAttachment::new(
            &attachment.archive(),
            false,
            b"archive.zip",
        ));
        let renamed = walk(&mut reopened, &[b"renamed"]);
        assert_eq!(
            reopened.node_info(renamed).unwrap().times.modified,
            Time64::from_secs(secs)
        );
    }
}

#[test]
fn a_change_to_a_root_no_zip_records_is_refused() {
    let mut fs = open(&MemoryAttachment::new(&empty_zip(), true, b"archive.zip"));
    let root = fs.root();
    assert_eq!(
        fs.set_security(root, NodeSecurity::new(0o700, 0, 0)),
        Err(DriverError::Unsupported)
    );
}

/// The `acorn.datestamp` value `fs` gives `node`.
fn datestamp<F: FilesystemAttrs>(
    fs: &mut F,
    node: tairix_abi::driver::filesystem::NodeId,
) -> Vec<u8> {
    let mut value = [0u8; 16];
    let len = fs
        .get_attr(node, b"acorn.datestamp", &mut value)
        .unwrap()
        .unwrap();
    value[..len].to_vec()
}

/// The datestamp value of `time`.
fn stamp_of(time: Time64) -> Vec<u8> {
    let centiseconds = acorn::time64_to_centiseconds(time).unwrap();
    acorn::datestamp_to_value(centiseconds).unwrap().to_vec()
}

#[test]
fn a_typed_members_stamp_follows_a_change_of_its_bytes() {
    // Typed as Text, stamped 2024-05-06 07:08:09.10.
    let stamp: u64 = 3_924_016_089 * 100 + 10;
    let load = 0xFFF0_0000 | 0xFFF << 8 | u32::try_from(stamp >> 32).unwrap();
    let exec = u32::try_from(stamp & 0xFFFF_FFFF).unwrap();
    let mut words = b"ARC0".to_vec();
    for word in [load, exec, 0x03, 0] {
        words.extend_from_slice(&word.to_le_bytes());
    }
    let zip = with_central(
        &foreign_zip(&[(b"text", b"old", Packing::Stored)]),
        &extra_field(0x4341, &words),
        b"",
        0x0314,
        0o100_644 << 16,
    );
    let attachment = MemoryAttachment::new(&zip, true, b"archive.zip");
    let mut fs = open(&attachment);
    let text = walk(&mut fs, &[b"text"]);
    fs.write_all(text, 0, b"new").unwrap();
    fs.flush().unwrap();
    let mut reopened = open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"archive.zip",
    ));
    let text = walk(&mut reopened, &[b"text"]);
    let modified = reopened.node_info(text).unwrap().times.modified;
    assert_eq!(datestamp(&mut reopened, text), stamp_of(modified));
}

#[test]
fn a_member_typed_afresh_is_dated_as_the_member_is() {
    let mut fs = open(&MemoryAttachment::new(
        &foreign_zip(&[(b"plain", b"x", Packing::Stored)]),
        true,
        b"archive.zip",
    ));
    let plain = walk(&mut fs, &[b"plain"]);
    let modified = fs.node_info(plain).unwrap().times.modified;
    fs.set_attr(plain, b"acorn.filetype", b"fff").unwrap();
    assert_eq!(datestamp(&mut fs, plain), stamp_of(modified));
    assert_eq!(fs.node_info(plain).unwrap().times.modified, modified);
}

#[test]
fn a_risc_os_attribute_a_zip_has_no_place_for_is_refused() {
    let mut fs = open(&MemoryAttachment::new(&empty_zip(), true, b"archive.zip"));
    let root = fs.root();
    let file = fs
        .create(
            root,
            b"f",
            NodeKind::RegularFile,
            &NodeSecurity::new(0o644, 0, 0),
        )
        .unwrap();
    assert_eq!(
        fs.set_attr(file, b"acorn.attr", b"RWP/r"),
        Err(DriverError::OutOfRange)
    );
    fs.set_attr(file, b"acorn.attr", b"RWL/r").unwrap();
}

#[test]
fn a_member_shorter_than_a_bzip2_block_is_decoded_in_room_its_size() {
    let target = b"../elsewhere/target";
    let coded = tairix_test_archive_image::bzip2(target, 9).unwrap();
    let link = Packing::Raw {
        method: 12,
        needed: 46,
        coded: &coded,
    };
    let zip = with_central(
        &foreign_zip(&[(b"link", target, link)]),
        b"",
        b"",
        0x0314,
        0o120_777 << 16,
    );
    let attachment = MemoryAttachment::new(&zip, false, b"archive.zip");
    // Its target is read as the archive opens.
    let (mut fs, metering) = metered(|| open(&attachment));
    assert!(
        metering.largest < 1 << 20,
        "{} bytes at once",
        metering.largest
    );
    let link = walk(&mut fs, &[b"link"]);
    let mut out = [0u8; 64];
    let len = fs.read_link(link, &mut out).unwrap();
    assert_eq!(&out[..len], target);
}

#[test]
fn a_held_member_no_read_reaches_never_stops_a_commit() {
    let mut zip = foreign_zip(&[
        (b"secret", b"ciphertext", Packing::Stored),
        (b"plain", b"text", Packing::Stored),
    ]);
    // Encrypted, in its local header and its central record.
    zip[6] |= 1;
    let central = central_record(&zip);
    zip[central + 8] |= 1;
    let attachment = MemoryAttachment::new(&zip, true, b"archive.zip");
    let mut fs = open(&attachment);
    let root = fs.root();
    let secret = walk(&mut fs, &[b"secret"]);
    let held = fs.pin(secret).unwrap();
    fs.remove(root, b"secret").unwrap();
    assert_eq!(fs.flush(), Ok(()));
    assert_eq!(fs.write_refusal(), None);
    assert_eq!(
        fs.read_at(held, 0, &mut [0u8; 4]),
        Err(DriverError::PermissionDenied)
    );
    let mut reopened = open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"archive.zip",
    ));
    let root = reopened.root();
    assert_eq!(names(&mut reopened, root), [b"plain".to_vec()]);
}

#[test]
fn a_held_member_damaged_where_it_lies_never_stops_a_commit() {
    let text = b"the quick brown fox jumps ".repeat(64);
    let mut zip = foreign_zip(&[
        (b"damaged", &text, Packing::Deflated),
        (b"plain", b"text", Packing::Stored),
    ]);
    // Its CRC broken where both records give it.
    zip[14] ^= 0xFF;
    let central = central_record(&zip);
    zip[central + 16] ^= 0xFF;
    let attachment = MemoryAttachment::new(&zip, true, b"archive.zip");
    let mut fs = open(&attachment);
    let root = fs.root();
    let damaged = walk(&mut fs, &[b"damaged"]);
    let held = fs.pin(damaged).unwrap();
    fs.remove(root, b"damaged").unwrap();
    assert_eq!(fs.flush(), Ok(()));
    assert_eq!(fs.write_refusal(), None);
    assert_eq!(
        fs.read_at(held, 0, &mut [0u8; 16]),
        Err(DriverError::MediumError)
    );
    let mut reopened = open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"archive.zip",
    ));
    let root = reopened.root();
    assert_eq!(names(&mut reopened, root), [b"plain".to_vec()]);
}
