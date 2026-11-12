// Test archives are a few kilobytes, so every length fits the narrower field
// it is written to.
#![allow(clippy::cast_possible_truncation)]

extern crate std;

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::volume::VolumeFsType;
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;

use super::{
    arc_entry_holds, arcfs_header, bzip2_level, rar5_vint, rar_version, recognise,
    tar_header_holds, tar_number, RarVersion, ZipEnd, ARCFS_HEADER_LEN, ARCFS_MAGIC,
    RAR4_SIGNATURE, RAR5_SIGNATURE, TAR_BLOCK,
};

const BLOCK: usize = 512;

/// A file as the kernel presents an archive: whole blocks, the last one
/// running past the file's bytes into `padding`.
struct File {
    bytes: Vec<u8>,
}

impl File {
    fn new(content: &[u8], padding: u8) -> Self {
        let mut bytes = content.to_vec();
        bytes.resize(content.len().div_ceil(BLOCK) * BLOCK, padding);
        Self { bytes }
    }
}

impl Block for File {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: BLOCK as u32,
            block_count: (self.bytes.len() / BLOCK) as u64,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let at = usize::try_from(lba).unwrap() * BLOCK;
        let part = self
            .bytes
            .get(at..at + buf.len())
            .ok_or(DriverError::LengthOutOfRange)?;
        buf.copy_from_slice(part);
        Ok(())
    }

    fn write_blocks(&mut self, _lba: u64, _buf: &[u8]) -> Result<(), DriverError> {
        Err(DriverError::Unsupported)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

fn recognised(content: &[u8]) -> Option<VolumeFsType> {
    let mut dev = ByteDevice::new(File::new(content, 0)).unwrap();
    recognise(&mut dev, content.len() as u64)
        .unwrap()
        .map(|volume| volume.fstype)
}

fn zip_end(content: &[u8]) -> Option<ZipEnd> {
    let mut dev = ByteDevice::new(File::new(content, 0)).unwrap();
    ZipEnd::find(&mut dev, content.len() as u64).unwrap()
}

fn le16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn le32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn le64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// A zip of one stored member `name` holding `data`, its offsets counted from
/// its own start, with `comment`.
fn zip(name: &[u8], data: &[u8], comment: &[u8]) -> Vec<u8> {
    let crc = tairix_crc32::checksum(data);
    let mut out = Vec::new();
    le32(&mut out, 0x0403_4b50);
    for value in [20u16, 0, 0, 0, 0] {
        le16(&mut out, value);
    }
    le32(&mut out, crc);
    le32(&mut out, data.len() as u32);
    le32(&mut out, data.len() as u32);
    le16(&mut out, name.len() as u16);
    le16(&mut out, 0);
    out.extend_from_slice(name);
    out.extend_from_slice(data);
    let central = out.len();
    le32(&mut out, 0x0201_4b50);
    for value in [20u16, 20, 0, 0, 0, 0] {
        le16(&mut out, value);
    }
    le32(&mut out, crc);
    le32(&mut out, data.len() as u32);
    le32(&mut out, data.len() as u32);
    le16(&mut out, name.len() as u16);
    for value in [0u16, 0, 0, 0] {
        le16(&mut out, value);
    }
    le32(&mut out, 0);
    le32(&mut out, 0);
    out.extend_from_slice(name);
    let central_len = out.len() - central;
    le32(&mut out, 0x0605_4b50);
    for value in [0u16, 0, 1, 1] {
        le16(&mut out, value);
    }
    le32(&mut out, central_len as u32);
    le32(&mut out, central as u32);
    le16(&mut out, comment.len() as u16);
    out.extend_from_slice(comment);
    out
}

#[test]
fn a_zip_is_known_by_the_end_record_that_closes_it() {
    let archive = zip(b"hello.txt", b"hello, world", b"a comment");
    assert_eq!(recognised(&archive), Some(VolumeFsType::Zip));
    let end = zip_end(&archive).unwrap();
    assert_eq!(end.entries, 1);
    assert_eq!(end.base, 0);
    assert!(!end.split);
    assert_eq!(
        &archive[end.comment.start as usize..end.comment.end as usize],
        b"a comment"
    );
    let empty = zip_end(&archive[..0]);
    assert_eq!(empty, None);
}

#[test]
fn an_empty_zip_is_its_end_record_alone() {
    let mut archive = Vec::new();
    le32(&mut archive, 0x0605_4b50);
    archive.resize(22, 0);
    assert_eq!(recognised(&archive), Some(VolumeFsType::Zip));
    assert_eq!(zip_end(&archive).unwrap().entries, 0);
}

#[test]
fn a_self_extractor_counts_its_offsets_after_its_program() {
    let mut archive = vec![0x4d; 1000];
    archive.extend_from_slice(&zip(b"a", b"b", b""));
    let end = zip_end(&archive).unwrap();
    assert_eq!(end.base, 1000);
    assert_eq!(end.directory, 1000 + 30 + 1 + 1);
}

#[test]
fn bytes_after_the_end_record_or_a_directory_out_of_place_are_no_zip() {
    let mut trailing = zip(b"a", b"b", b"");
    trailing.push(0);
    assert_eq!(zip_end(&trailing), None);
    let mut misplaced = zip(b"a", b"b", b"");
    let at = misplaced.len() - 6;
    misplaced[at..at + 4].copy_from_slice(&5000u32.to_le_bytes());
    assert_eq!(zip_end(&misplaced), None);
}

#[test]
fn an_end_record_in_the_last_part_block_is_found_and_padding_never_read() {
    // The archive ends mid-block; an end record planted in the padding past
    // its last byte must not be taken for one.
    let archive = zip(b"name", &[7; 700], b"");
    assert_ne!(archive.len() % BLOCK, 0);
    let mut file = File::new(&archive, 0);
    let pad = archive.len();
    let fake = zip(b"x", b"", b"");
    let room = file.bytes.len() - pad;
    let fake_tail = &fake[fake.len() - 22..];
    if room >= 22 {
        file.bytes[pad..pad + 22].copy_from_slice(fake_tail);
    }
    let mut dev = ByteDevice::new(file).unwrap();
    let end = ZipEnd::find(&mut dev, archive.len() as u64)
        .unwrap()
        .unwrap();
    assert_eq!(end.entries, 1);
    assert_eq!(end.comment.end, archive.len() as u64);
}

#[test]
fn a_zip64_end_states_what_its_end_record_cannot() {
    let mut archive = zip(b"a", b"b", b"");
    let end_at = archive.len() - 22;
    let central = u32::from_le_bytes(archive[end_at + 16..end_at + 20].try_into().unwrap());
    let central_len = u32::from_le_bytes(archive[end_at + 12..end_at + 16].try_into().unwrap());
    archive.truncate(end_at);
    let zip64_at = archive.len() as u64;
    le32(&mut archive, 0x0606_4b50);
    le64(&mut archive, 44);
    le16(&mut archive, 45);
    le16(&mut archive, 45);
    le32(&mut archive, 0);
    le32(&mut archive, 0);
    le64(&mut archive, 1);
    le64(&mut archive, 1);
    le64(&mut archive, u64::from(central_len));
    le64(&mut archive, u64::from(central));
    le32(&mut archive, 0x0706_4b50);
    le32(&mut archive, 0);
    le64(&mut archive, zip64_at);
    le32(&mut archive, 1);
    le32(&mut archive, 0x0605_4b50);
    for value in [0u16, 0, 0xFFFF, 0xFFFF] {
        le16(&mut archive, value);
    }
    le32(&mut archive, 0xFFFF_FFFF);
    le32(&mut archive, 0xFFFF_FFFF);
    le16(&mut archive, 0);
    let end = zip_end(&archive).unwrap();
    assert_eq!(end.entries, 1);
    assert!(end.zip64);
    assert_eq!(end.end, zip64_at);
    assert_eq!(end.directory, u64::from(central));
    assert_eq!(recognised(&archive), Some(VolumeFsType::Zip));
    // Without the locator the wide fields are taken at their word, and name
    // no directory.
    let mut lost = archive.clone();
    let locator = lost.len() - 22 - 20;
    lost[locator] = 0;
    assert_eq!(zip_end(&lost), None);
}

#[test]
fn a_count_of_65535_with_no_zip64_record_is_taken_at_its_word() {
    let mut archive = zip(b"a", b"b", b"");
    let end_at = archive.len() - 22;
    for field in [8, 10] {
        archive[end_at + field..end_at + field + 2].copy_from_slice(&0xFFFFu16.to_le_bytes());
    }
    let end = zip_end(&archive).unwrap();
    assert_eq!(end.entries, 0xFFFF);
    assert!(!end.zip64);
    assert_eq!(end.end, end_at as u64);
}

/// A ustar header naming `name`, its checksum summed as `signed` says.
fn tar_header(name: &[u8], signed: bool) -> [u8; TAR_BLOCK] {
    let mut block = [0u8; TAR_BLOCK];
    block[..name.len()].copy_from_slice(name);
    block[100..107].copy_from_slice(b"0000644");
    block[108..115].copy_from_slice(b"0001750");
    block[116..123].copy_from_slice(b"0001750");
    block[124..135].copy_from_slice(b"00000000013");
    block[136..147].copy_from_slice(b"14727463270");
    block[156] = b'0';
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    block[148..156].fill(b' ');
    let sum: i64 = if signed {
        block.iter().map(|&b| i64::from(b.cast_signed())).sum()
    } else {
        block.iter().map(|&b| i64::from(b)).sum()
    };
    let field = std::format!("{sum:06o}\0 ");
    block[148..156].copy_from_slice(field.as_bytes());
    block
}

#[test]
fn a_tar_is_known_by_a_header_whose_checksum_holds() {
    let header = tar_header(b"notes.txt", false);
    assert!(tar_header_holds(&header));
    let mut archive = header.to_vec();
    archive.extend_from_slice(&[0; 1536]);
    assert_eq!(recognised(&archive), Some(VolumeFsType::Tar));
    let mut broken = header;
    broken[0] ^= 1;
    assert!(!tar_header_holds(&broken));
    assert!(
        !tar_header_holds(&[0; TAR_BLOCK]),
        "an empty block names nothing"
    );
}

#[test]
fn an_old_writers_signed_checksum_holds_too() {
    let header = tar_header(b"caf\xe9", true);
    assert!(tar_header_holds(&header));
}

#[test]
fn a_tar_number_is_octal_or_base_256() {
    assert_eq!(tar_number(b"  0644 \0"), Some(0o644));
    assert_eq!(tar_number(b"00000000013\0"), Some(11));
    assert_eq!(tar_number(&[0x80, 0, 0, 0, 1, 0, 0, 0]), Some(1 << 24));
    assert_eq!(tar_number(b"0x10"), None);
    assert_eq!(tar_number(b"12 3"), None, "a digit after the end");
}

#[test]
fn a_gzip_member_is_recognised_by_its_header() {
    let mut member = vec![0x1f, 0x8b, 8, 0x08, 1, 2, 3, 4, 0, 3];
    member.extend_from_slice(b"report.txt\0");
    member.extend_from_slice(&[3, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(recognised(&member), Some(VolumeFsType::Gzip));
    member[3] |= 0x20;
    assert_eq!(recognised(&member), None);
}

#[test]
fn a_bzip2_stream_says_its_block_size() {
    assert_eq!(bzip2_level(b"BZh91AY&SY\0\0"), Some(9));
    assert_eq!(bzip2_level(b"BZh1\x17\x72\x45\x38\x50\x90"), Some(1));
    assert_eq!(bzip2_level(b"BZh01AY&SY"), None);
    assert_eq!(bzip2_level(b"BZh9XAY&SY"), None);
    let mut stream = b"BZh91AY&SY".to_vec();
    stream.resize(40, 0);
    assert_eq!(recognised(&stream), Some(VolumeFsType::Bzip2));
}

#[test]
fn a_rar_signature_needs_a_main_header_whose_checksum_holds() {
    let mut rar4 = RAR4_SIGNATURE.to_vec();
    let fields = [0x73u8, 0, 0, 13, 0, 0, 0, 0, 0, 0, 0];
    let crc = (tairix_crc32::checksum(&fields) & 0xFFFF) as u16;
    rar4.extend_from_slice(&crc.to_le_bytes());
    rar4.extend_from_slice(&fields);
    assert_eq!(rar_version(&rar4), Some(RarVersion::Rar4));
    assert_eq!(recognised(&rar4), Some(VolumeFsType::Rar));
    let mut broken = rar4.clone();
    broken[8] ^= 1;
    assert_eq!(rar_version(&broken), None);

    // RAR 1.5 and 2 embed the archive comment in the main header, outside
    // what its checksum covers.
    let mut commented = RAR4_SIGNATURE.to_vec();
    let mut fields = vec![0x73u8, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let comment = b"\x00\x00\x75\x00\x00\x0f\x00\x02\x00\x0f\x30\x00\x00hi";
    fields[3..5].copy_from_slice(&u16::try_from(13 + comment.len()).unwrap().to_le_bytes());
    let crc = (tairix_crc32::checksum(&fields) & 0xFFFF) as u16;
    commented.extend_from_slice(&crc.to_le_bytes());
    commented.extend_from_slice(&fields);
    commented.extend_from_slice(comment);
    assert_eq!(rar_version(&commented), Some(RarVersion::Rar4));

    let mut rar5 = RAR5_SIGNATURE.to_vec();
    let body = [3u8, 1, 0, 0];
    let crc = tairix_crc32::checksum(&body);
    rar5.extend_from_slice(&crc.to_le_bytes());
    rar5.extend_from_slice(&body);
    assert_eq!(rar_version(&rar5), Some(RarVersion::Rar5));
    // Encrypted headers open with the encryption header.
    let mut encrypted = RAR5_SIGNATURE.to_vec();
    let body = [3u8, 4, 0, 0];
    encrypted.extend_from_slice(&tairix_crc32::checksum(&body).to_le_bytes());
    encrypted.extend_from_slice(&body);
    assert_eq!(rar_version(&encrypted), Some(RarVersion::Rar5));
    let mut file_first = RAR5_SIGNATURE.to_vec();
    let body = [3u8, 2, 0, 0];
    file_first.extend_from_slice(&tairix_crc32::checksum(&body).to_le_bytes());
    file_first.extend_from_slice(&body);
    assert_eq!(rar_version(&file_first), None);
    let mut other = rar5.clone();
    let at = rar5.len() - 3;
    other[at] = 2;
    assert_eq!(rar_version(&other), None, "a main header is type 1");
}

#[test]
fn a_rar5_integer_is_seven_bits_a_byte() {
    assert_eq!(rar5_vint(&[0x05]), Some((5, 1)));
    assert_eq!(rar5_vint(&[0x80 | 0x01, 0x02]), Some((0x101, 2)));
    assert_eq!(rar5_vint(&[0x80; 10]), None, "never ended");
    assert_eq!(
        rar5_vint(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]),
        Some((u64::MAX, 10))
    );
    assert_eq!(
        rar5_vint(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x02]),
        None,
        "past 64 bits"
    );
}

#[test]
fn an_arcfs_header_places_its_table_before_its_data() {
    let mut archive = ARCFS_MAGIC.to_vec();
    archive.extend_from_slice(&72u32.to_le_bytes());
    archive.extend_from_slice(&((ARCFS_HEADER_LEN + 72) as u32).to_le_bytes());
    archive.resize(ARCFS_HEADER_LEN + 72, 0);
    assert!(arcfs_header(&archive, archive.len() as u64).is_some());
    assert_eq!(recognised(&archive), Some(VolumeFsType::ArcFs));
    let mut ragged = archive.clone();
    ragged[8] = 71;
    assert!(arcfs_header(&ragged, ragged.len() as u64).is_none());
    assert!(arcfs_header(&archive, 100).is_none(), "data past the file");
}

#[test]
fn a_spark_header_names_a_member_that_fits() {
    let mut archive = vec![0x1A, 0x82];
    let mut name = [0u8; 13];
    name[..6].copy_from_slice(b"ReadMe");
    archive.extend_from_slice(&name);
    archive.extend_from_slice(&5u32.to_le_bytes());
    archive.extend_from_slice(&[0; 8]);
    archive.extend_from_slice(&5u32.to_le_bytes());
    archive.extend_from_slice(&[0; 12]);
    archive.extend_from_slice(b"hello");
    archive.extend_from_slice(&[0x1A, 0x80]);
    assert!(arc_entry_holds(&archive, archive.len() as u64));
    assert_eq!(recognised(&archive), Some(VolumeFsType::Spark));
    assert!(!arc_entry_holds(&archive, 30), "a member past the file");
    let mut unknown = archive.clone();
    unknown[1] = 0x8A;
    assert!(!arc_entry_holds(&unknown, unknown.len() as u64));
    assert_eq!(recognised(&[0x1A, 0x80]), Some(VolumeFsType::Spark));
}

#[test]
fn nothing_else_is_an_archive() {
    assert_eq!(recognised(b"plain text, nothing more"), None);
    assert_eq!(recognised(&[0; 4096]), None);
}
