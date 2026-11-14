//! `ArcFS` archives, built here as `ArcFS` lays them out — deleted entries,
//! `FileData` blocks, a CRC left unrecorded — read, and written back with
//! the table afresh and every unchanged file's coded bytes copied.

extern crate std;

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{
    FilesystemAttrs, FilesystemRead, FilesystemWrite, NodeKind, NodeSecurity, WriteRefusal,
};
use tairix_abi::volume::FormatName;
use tairix_abi::DriverError;

use crate::test_support::{contents, names, open, walk, MemoryAttachment};

/// One entry of a table being built.
enum Item<'a> {
    Dir(&'a [u8]),
    End,
    Deleted,
    /// Info byte, name, coded bytes, decoded length, attribute word.
    File(u8, &'a [u8], Vec<u8>, usize, u32),
}

const LOAD: u32 = 0xFFFF_FD12;
const EXEC: u32 = 0x3456_7890;

/// An archive of `items`, pre-order, the root's end appended; each file's
/// data led by a `FileData` block where `blocks`.
fn arcfs(items: &[Item<'_>], blocks: bool) -> Vec<u8> {
    let count = items.len() + 1;
    let data_at = 96 + 36 * count;
    let mut table = vec![0u8; 36 * count];
    let mut data = Vec::new();
    let mut dirs = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let entry = &mut table[36 * index..36 * index + 36];
        match item {
            Item::Dir(name) => {
                entry[0] = 0x82;
                entry[1..=name.len()].copy_from_slice(name);
                entry[12..16].fill(0xFF);
                entry[16..20].copy_from_slice(&0xFFFF_FF12u32.to_le_bytes());
                entry[20..24].copy_from_slice(&EXEC.to_le_bytes());
                entry[24] = 0x33;
                entry[28..32].fill(0xFF);
                dirs.push(index);
            }
            Item::End => {
                let dir = dirs.pop().unwrap();
                let next = u32::try_from(36 * (index + 1)).unwrap() | 1 << 31;
                table[36 * dir + 32..36 * dir + 36].copy_from_slice(&next.to_le_bytes());
            }
            Item::Deleted => entry[0] = 0x01,
            Item::File(info, name, coded, len, attr) => {
                if blocks {
                    let mut block = vec![0u8; 36];
                    block[..8].copy_from_slice(b"FileData");
                    data.extend_from_slice(&block);
                }
                entry[0] = *info;
                entry[1..=name.len()].copy_from_slice(name);
                entry[12..16].copy_from_slice(&u32::try_from(*len).unwrap().to_le_bytes());
                entry[16..20].copy_from_slice(&LOAD.to_le_bytes());
                entry[20..24].copy_from_slice(&EXEC.to_le_bytes());
                entry[24..28].copy_from_slice(&attr.to_le_bytes());
                entry[28..32].copy_from_slice(&u32::try_from(coded.len()).unwrap().to_le_bytes());
                entry[32..36].copy_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
                data.extend_from_slice(coded);
            }
        }
    }
    let mut out = vec![0u8; 96];
    out[..8].copy_from_slice(b"Archive\0");
    out[8..12].copy_from_slice(&u32::try_from(table.len()).unwrap().to_le_bytes());
    out[12..16].copy_from_slice(&u32::try_from(data_at).unwrap().to_le_bytes());
    out[16..20].copy_from_slice(&40u32.to_le_bytes());
    out[20..24].copy_from_slice(&100u32.to_le_bytes());
    out.extend_from_slice(&table);
    out.extend_from_slice(&data);
    out
}

/// A stored file's attribute word: its CRC, no width, read and write.
fn stored_attr(data: &[u8]) -> u32 {
    u32::from(tairix_crc16::checksum(data)) << 16 | 0x33
}

/// The dynamic LZW of `plain`, as `ArcFS` compresses, and its attribute word.
fn compressed(plain: &[u8], bits: u8) -> (Vec<u8>, u32) {
    struct Gathered(Vec<u8>);
    impl tairix_compress::Output for Gathered {
        fn put(&mut self, bytes: &[u8]) -> bool {
            self.0.extend_from_slice(bytes);
            true
        }
    }
    let mut encoder =
        tairix_compress::arc::Encoder::new(vec![0u32; tairix_compress::arc::ENCODER_WORK], bits)
            .unwrap();
    let mut out = Gathered(Vec::new());
    encoder.write(plain, &mut out).unwrap();
    encoder.finish(&mut out).unwrap();
    (
        out.0,
        u32::from(tairix_crc16::checksum(plain)) << 16 | u32::from(bits) << 8 | 0x33,
    )
}

const TEXT: &[u8] = b"An ArcFS archive's text, compressed as ArcFS compresses it. ";

fn sample(blocks: bool) -> Vec<u8> {
    let text = TEXT.repeat(20);
    let (coded, attr) = compressed(&text, 12);
    arcfs(
        &[
            Item::Dir(b"!Docs"),
            Item::File(0xFF, b"Guide", coded, text.len(), attr),
            Item::Deleted,
            Item::File(
                0x82,
                b"Read/me",
                b"stored".to_vec(),
                6,
                stored_attr(b"stored"),
            ),
            Item::End,
            // A CRC of zero is none recorded.
            Item::File(0x82, b"Unchecked", b"no crc".to_vec(), 6, 0x33),
        ],
        blocks,
    )
}

#[test]
fn an_arcfs_archive_reads_as_its_directories_and_files() {
    for blocks in [false, true] {
        let mut fs = open(&MemoryAttachment::new(
            &sample(blocks),
            false,
            b"sample.arc",
        ));
        assert_eq!(fs.format_name(), FormatName::ArcFs);
        let root = fs.root();
        assert_eq!(
            names(&mut fs, root),
            [b"!Docs".to_vec(), b"Unchecked".to_vec()]
        );
        let docs = walk(&mut fs, &[b"!Docs"]);
        assert_eq!(
            names(&mut fs, docs),
            [b"Guide".to_vec(), b"Read.me".to_vec()]
        );
        let guide = walk(&mut fs, &[b"!Docs", b"Guide"]);
        assert_eq!(contents(&mut fs, guide), TEXT.repeat(20));
        let readme = walk(&mut fs, &[b"!Docs", b"Read.me"]);
        assert_eq!(contents(&mut fs, readme), b"stored");
        let unchecked = walk(&mut fs, &[b"Unchecked"]);
        assert_eq!(contents(&mut fs, unchecked), b"no crc");
        let mut value = [0u8; 16];
        let len = fs
            .get_attr(readme, b"acorn.loadaddr", &mut value)
            .unwrap()
            .unwrap();
        assert_eq!(&value[..len], b"fffffd12");
    }
}

#[test]
fn a_file_whose_check_fails_is_refused() {
    let items = [Item::File(
        0x82,
        b"Bad",
        b"bytes".to_vec(),
        5,
        stored_attr(b"other"),
    )];
    let mut fs = open(&MemoryAttachment::new(
        &arcfs(&items, false),
        false,
        b"bad.arc",
    ));
    let bad = walk(&mut fs, &[b"Bad"]);
    let mut buf = [0u8; 8];
    assert_eq!(fs.read_at(bad, 0, &mut buf), Err(DriverError::MediumError));
}

#[test]
fn the_table_is_written_afresh_and_unchanged_bytes_copied() {
    let attachment = MemoryAttachment::new(&sample(true), true, b"sample.arc");
    let mut fs = open(&attachment);
    let host = tairix_writeback::TestVolumeHost::leaked(1);
    fs.set_volume_host(tairix_writeback::TestVolumeHost::volume(), host);
    let root = fs.root();
    let added = fs
        .create(
            root,
            b"added",
            NodeKind::RegularFile,
            &NodeSecurity::new(0o644, 0, 0),
        )
        .unwrap();
    fs.write_all(added, 0, &TEXT.repeat(50)).unwrap();
    let docs = walk(&mut fs, &[b"!Docs"]);
    fs.rename(docs, b"Read.me", root, b"Moved").unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    assert_eq!(&written[16..28], &[40, 0, 0, 0, 100, 0, 0, 0, 0, 0, 0, 0]);
    assert!(
        attachment.asked().copied > 0,
        "unchanged coded bytes are the kernel's to copy"
    );

    let mut reopened = open(&MemoryAttachment::new(&written, false, b"sample.arc"));
    let root = reopened.root();
    // Sorted as RISC OS sorts: by name, its letters' case aside.
    assert_eq!(
        names(&mut reopened, root),
        [
            b"!Docs".to_vec(),
            b"added".to_vec(),
            b"Moved".to_vec(),
            b"Unchecked".to_vec()
        ]
    );
    let added = walk(&mut reopened, &[b"added"]);
    assert_eq!(contents(&mut reopened, added), TEXT.repeat(50));
    let moved = walk(&mut reopened, &[b"Moved"]);
    assert_eq!(contents(&mut reopened, moved), b"stored");
    let guide = walk(&mut reopened, &[b"!Docs", b"Guide"]);
    assert_eq!(contents(&mut reopened, guide), TEXT.repeat(20));
    let docs = walk(&mut reopened, &[b"!Docs"]);
    assert_eq!(names(&mut reopened, docs), [b"Guide".to_vec()]);
    // The table names each directory's next object past its end.
    let order: Vec<u8> = written[96..96 + 36 * 6]
        .chunks(36)
        .map(|entry| entry[0])
        .collect();
    assert_eq!(order, [0x82, 0xFF, 0x00, 0xFF, 0x82, 0x82, 0x00][..6]);
}

/// An entry named as RISC OS could not have named it is left out, and the
/// archive is not written back, which would lose it.
#[test]
fn an_entry_named_as_no_risc_os_name_can_be_is_left_out_and_kept() {
    for name in [&b"wild*card"[..], b"has space", b"colon:"] {
        let bytes = arcfs(
            &[
                Item::File(0x82, b"Kept", b"kept".to_vec(), 4, stored_attr(b"kept")),
                Item::File(0x82, name, b"x".to_vec(), 1, stored_attr(b"x")),
            ],
            false,
        );
        let mut fs = open(&MemoryAttachment::new(&bytes, true, b"odd.arc"));
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
fn a_name_longer_than_an_entry_holds_is_refused() {
    let mut fs = open(&MemoryAttachment::new(&sample(false), true, b"sample.arc"));
    let root = fs.root();
    assert_eq!(
        fs.create(
            root,
            b"Elevenchars",
            NodeKind::RegularFile,
            &NodeSecurity::new(0o644, 0, 0)
        ),
        Err(DriverError::LengthOutOfRange)
    );
    assert!(fs
        .create(
            root,
            b"Tenchars!!",
            NodeKind::RegularFile,
            &NodeSecurity::new(0o644, 0, 0)
        )
        .is_ok());
}

#[test]
fn a_member_past_the_roots_end_leaves_the_archive_unwritten() {
    let mut archive = arcfs(
        &[
            Item::File(0x82, b"a", b"1".to_vec(), 1, stored_attr(b"1")),
            Item::Deleted,
            Item::File(0x82, b"b", b"2".to_vec(), 1, stored_attr(b"2")),
        ],
        false,
    );
    // The deleted entry made an end, which closes the root early.
    archive[96 + 36] = 0;
    let mut fs = open(&MemoryAttachment::new(&archive, true, b"stray.arc"));
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [b"a".to_vec()]);
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::Damaged));
}
