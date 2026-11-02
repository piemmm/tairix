//! Compressed archives: tars gzip'd and bzip2'd as GNU tar writes them, and
//! lone compressed files — read, written back, and read anywhere through the
//! places their writer noted.

extern crate std;

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{
    FilesystemAttrs, FilesystemRead, FilesystemWrite, NodeKind, NodeSecurity,
};
use tairix_abi::volume::FormatName;
use tairix_abi::DriverError;

use super::*;
use crate::test_support::{contents, names, open, try_open, walk, MemoryAccess, MemoryAttachment};

/// `two.tar.gz`: `hello.txt` and `dir/notes,fff`, as GNU tar 1.35 wrote it
/// and gzip 1.12 compressed it.
const TWO_TAR_GZ: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0xed, 0xd4, 0xb1, 0x0a, 0xc2, 0x30,
    0x14, 0x85, 0xe1, 0xcc, 0x3e, 0x45, 0xdc, 0x8b, 0xa6, 0x69, 0x93, 0x3c, 0x80, 0x8b, 0x4e, 0x82,
    0x7d, 0x02, 0xa1, 0x2d, 0x0a, 0xc5, 0x42, 0x1b, 0xb1, 0x8f, 0x6f, 0xda, 0xad, 0x82, 0x63, 0x23,
    0xd2, 0xff, 0x5b, 0xee, 0x25, 0x4b, 0x86, 0xcb, 0x39, 0xb7, 0xaa, 0x69, 0xda, 0x9d, 0x1f, 0xbc,
    0x58, 0x8e, 0x0a, 0xac, 0xcd, 0xa7, 0x19, 0x7c, 0x4e, 0xa5, 0x52, 0x2b, 0xd2, 0xcc, 0x2a, 0xed,
    0xac, 0xca, 0xa7, 0xf7, 0xb0, 0x18, 0x21, 0x95, 0x88, 0xe0, 0xd9, 0xfb, 0x6b, 0x27, 0xa5, 0x58,
    0xa9, 0xe3, 0x78, 0xff, 0x44, 0xbe, 0xda, 0xae, 0x29, 0xb7, 0x1b, 0x81, 0x95, 0x29, 0xef, 0xdd,
    0x7e, 0xe9, 0x3f, 0xc6, 0x88, 0x3b, 0x67, 0xbe, 0xe7, 0x3f, 0xec, 0xf3, 0xfc, 0x5b, 0xa3, 0xb5,
    0x90, 0x86, 0xfc, 0x47, 0xb9, 0xff, 0xa3, 0xf5, 0x55, 0x9f, 0xd4, 0x75, 0xfd, 0xb3, 0xfe, 0x37,
    0xf3, 0xfb, 0xa7, 0x2a, 0xd3, 0x8e, 0xfe, 0x8f, 0xe1, 0x72, 0x2a, 0x0e, 0xf2, 0x5c, 0x48, 0x5f,
    0x0d, 0x9e, 0xfa, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xe0, 0x3f,
    0xbd, 0x01, 0x11, 0x61, 0x9a, 0x25, 0x00, 0x28, 0x00, 0x00,
];

/// The same tar, as bzip2 1.0.8 compressed it.
const TWO_TAR_BZ2: &[u8] = &[
    0x42, 0x5a, 0x68, 0x39, 0x31, 0x41, 0x59, 0x26, 0x53, 0x59, 0x16, 0xc0, 0x5d, 0x3a, 0x00, 0x00,
    0xb8, 0x5f, 0x81, 0xca, 0x90, 0xe0, 0x05, 0xff, 0x80, 0x08, 0x60, 0x98, 0x84, 0x67, 0x65, 0x9e,
    0xc0, 0x00, 0x00, 0x84, 0x08, 0x20, 0x00, 0x92, 0x0c, 0x91, 0xa5, 0x3d, 0x21, 0x90, 0x34, 0x1e,
    0xa6, 0x8d, 0x07, 0xa9, 0x91, 0xea, 0x36, 0x50, 0x6d, 0x4a, 0x9e, 0x90, 0xf5, 0x1b, 0x4d, 0x4d,
    0x07, 0xa8, 0xc8, 0xc8, 0xc8, 0x19, 0x01, 0xfa, 0xec, 0x68, 0x8a, 0xdb, 0x96, 0x03, 0x4e, 0xc8,
    0x06, 0x78, 0x90, 0x29, 0xa5, 0xb3, 0x72, 0x74, 0x11, 0xa0, 0x8c, 0xa6, 0x70, 0xf0, 0x42, 0x61,
    0x2d, 0x71, 0x75, 0x06, 0x06, 0x43, 0x85, 0x92, 0xbf, 0x0c, 0x5b, 0x3a, 0x7a, 0x9c, 0x0a, 0x46,
    0x49, 0x0f, 0xba, 0xf8, 0x24, 0x52, 0x58, 0xc9, 0x66, 0x1c, 0x71, 0x1c, 0x2d, 0xbd, 0xc0, 0x3c,
    0xbc, 0x30, 0xd6, 0x9c, 0x95, 0x51, 0xc5, 0xa0, 0x34, 0x2e, 0x71, 0x52, 0xa1, 0x6b, 0x6b, 0xa4,
    0x74, 0x32, 0x2e, 0x8a, 0x11, 0x03, 0x2a, 0x66, 0xc2, 0x4e, 0x1e, 0x87, 0x5c, 0x2f, 0x1d, 0x24,
    0x34, 0x8c, 0x50, 0x4f, 0x2f, 0xc7, 0x21, 0x17, 0xcc, 0xd0, 0x62, 0x56, 0x13, 0xa1, 0x65, 0xb5,
    0x40, 0x82, 0x07, 0xf1, 0x77, 0x24, 0x53, 0x85, 0x09, 0x01, 0x6c, 0x05, 0xd3, 0xa0,
];

/// `notes.txt`, as `gzip -k` compressed it: its header naming it and
/// stamping it 2021-06-01 12:00:00 UTC.
const NAMED_GZ: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x08, 0x40, 0x21, 0xb6, 0x60, 0x02, 0x03, 0x6e, 0x6f, 0x74, 0x65, 0x73, 0x2e,
    0x74, 0x78, 0x74, 0x00, 0xf3, 0x2a, 0x2d, 0x2e, 0x51, 0xc8, 0xcf, 0x4b, 0x55, 0x48, 0xcb, 0xcc,
    0x49, 0xd5, 0x51, 0x48, 0xce, 0xcf, 0x2d, 0x28, 0x4a, 0x2d, 0x2e, 0x4e, 0x4d, 0x51, 0x48, 0xcc,
    0x01, 0x0a, 0xeb, 0x71, 0x01, 0x00, 0xb3, 0x66, 0xe3, 0x2f, 0x21, 0x00, 0x00, 0x00,
];

/// The same file, as `gzip -n` compressed it: no name, no time.
const UNNAMED_GZ: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0xf3, 0x2a, 0x2d, 0x2e, 0x51, 0xc8,
    0xcf, 0x4b, 0x55, 0x48, 0xcb, 0xcc, 0x49, 0xd5, 0x51, 0x48, 0xce, 0xcf, 0x2d, 0x28, 0x4a, 0x2d,
    0x2e, 0x4e, 0x4d, 0x51, 0x48, 0xcc, 0x01, 0x0a, 0xeb, 0x71, 0x01, 0x00, 0xb3, 0x66, 0xe3, 0x2f,
    0x21, 0x00, 0x00, 0x00,
];

/// The same file, as bzip2 1.0.8 compressed it.
const NOTES_BZ2: &[u8] = &[
    0x42, 0x5a, 0x68, 0x39, 0x31, 0x41, 0x59, 0x26, 0x53, 0x59, 0x8e, 0xfc, 0x74, 0xa8, 0x00, 0x00,
    0x03, 0xd5, 0x80, 0x00, 0x10, 0x40, 0x05, 0x00, 0x10, 0x2f, 0x27, 0xde, 0x00, 0x20, 0x00, 0x22,
    0x26, 0x46, 0x80, 0x34, 0x6d, 0x21, 0x4c, 0x26, 0x9a, 0x03, 0x4c, 0x49, 0xda, 0x97, 0x06, 0x4b,
    0x52, 0x46, 0xe1, 0x80, 0xad, 0xd6, 0x79, 0x79, 0x70, 0xe3, 0x3b, 0x84, 0x8f, 0x8b, 0xb9, 0x22,
    0x9c, 0x28, 0x48, 0x47, 0x7e, 0x3a, 0x54, 0x00,
];

/// Ten thousand two hundred and forty zeros — an empty tar — as `gzip -n`
/// compressed them.
const ZEROS_GZ: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0xed, 0xc1, 0x01, 0x0d, 0x00, 0x00,
    0x00, 0xc2, 0xa0, 0xf7, 0x4f, 0x6d, 0x0e, 0x37, 0xa0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x80, 0x37, 0x03, 0x9a, 0xde, 0x1d, 0x27, 0x00, 0x28, 0x00, 0x00,
];

const NOTES: &[u8] = b"Just one file, compressed alone.\n";

fn file_record() -> NodeSecurity {
    NodeSecurity::new(0o644, 1000, 100)
}

/// Text that compresses as text does: words drawn by a fixed generator.
fn prose(len: usize) -> Vec<u8> {
    const WORDS: [&[u8]; 8] = [
        b"archive ",
        b"member ",
        b"stream ",
        b"block ",
        b"window ",
        b"place ",
        b"tar\n",
        b"zip ",
    ];
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(WORDS[(state % 8) as usize]);
        out.extend_from_slice(&state.to_le_bytes()[..1]);
    }
    out.truncate(len);
    out
}

#[test]
fn gnu_tars_compressed_either_way_read_as_their_members() {
    for (archive, name, format) in [
        (TWO_TAR_GZ, &b"two.tar.gz"[..], FormatName::TarGzip),
        (TWO_TAR_BZ2, b"two.tar.bz2", FormatName::TarBzip2),
    ] {
        let attachment = MemoryAttachment::new(archive, false, name);
        let mut fs = open(&attachment);
        assert_eq!(fs.format_name(), format);
        let root = fs.root();
        assert_eq!(
            names(&mut fs, root),
            [b"dir".to_vec(), b"hello.txt".to_vec()]
        );
        let hello = walk(&mut fs, &[b"hello.txt"]);
        assert_eq!(contents(&mut fs, hello), b"Hello, world!\n");
        let notes = walk(&mut fs, &[b"dir", b"notes,fff"]);
        assert_eq!(contents(&mut fs, notes), b"RISC OS text\n");
    }
}

#[test]
fn a_compressed_tar_written_back_stays_compressed_and_reads_again() {
    for (archive, name, magic) in [
        (TWO_TAR_GZ, &b"two.tar.gz"[..], &b"\x1f\x8b"[..]),
        (TWO_TAR_BZ2, b"two.tar.bz2", b"BZh9"),
    ] {
        let attachment = MemoryAttachment::new(archive, true, name);
        let mut fs = open(&attachment);
        let root = fs.root();
        let added = fs
            .create(root, b"added.txt", NodeKind::RegularFile, &file_record())
            .unwrap();
        fs.write_all(added, 0, b"written here\n").unwrap();
        fs.rename(root, b"hello.txt", root, b"greeting.txt")
            .unwrap();
        fs.flush().unwrap();
        let written = attachment.archive();
        assert!(written.starts_with(magic), "{written:02x?}");
        // Read through the index the writer noted, then from a fresh open.
        let greeting = walk(&mut fs, &[b"greeting.txt"]);
        assert_eq!(contents(&mut fs, greeting), b"Hello, world!\n");
        let again = MemoryAttachment::new(&written, false, name);
        let mut reopened = open(&again);
        let root = reopened.root();
        assert_eq!(
            names(&mut reopened, root),
            [
                b"added.txt".to_vec(),
                b"dir".to_vec(),
                b"greeting.txt".to_vec()
            ]
        );
        let added = walk(&mut reopened, &[b"added.txt"]);
        assert_eq!(contents(&mut reopened, added), b"written here\n");
        let notes = walk(&mut reopened, &[b"dir", b"notes,fff"]);
        assert_eq!(contents(&mut reopened, notes), b"RISC OS text\n");
    }
}

/// The stream `packing` writes of `plain`, committed as the archive, and the
/// index its writer noted.
fn written(packing: &Packing, plain: &[u8]) -> (Regions<MemoryAccess>, Stream) {
    let attachment = MemoryAttachment::new(&[], true, b"archive");
    let mut regions = Regions::new(attachment.access(), 0);
    let mut sink = Sink::new(&regions).unwrap();
    let mut writer = Writer::new(packing, &mut sink).unwrap();
    for piece in plain.chunks(7919) {
        writer.write(piece).unwrap();
    }
    let index = writer.finish().unwrap();
    let len = sink.finish().unwrap();
    regions.commit(len).unwrap();
    (regions, index)
}

/// Read `plain` back through `stream` from scattered offsets, last first.
fn reads_anywhere(regions: &Regions<MemoryAccess>, stream: &mut Stream, plain: &[u8]) {
    let len = plain.len() as u64;
    let mut offsets: Vec<u64> = (0..24).map(|at| len * at / 24).collect();
    offsets.reverse();
    offsets.push(len - 1);
    offsets.push(1);
    for at in offsets {
        let mut buf = vec![0u8; 5000];
        let got = stream.read(regions, at, &mut buf).unwrap();
        let start = usize::try_from(at).unwrap();
        let end = (start + 5000).min(plain.len());
        assert_eq!(got, end - start, "at {at}");
        assert_eq!(&buf[..got], &plain[start..end], "at {at}");
    }
}

#[test]
fn a_long_stream_reads_anywhere_through_the_places_its_writer_noted() {
    let plain = prose(9 << 20);
    let gzip = Packing::Gzip {
        name: None,
        mtime: 0,
    };
    let (regions, mut index) = written(&gzip, &plain);
    assert!(index.points.len() >= 3, "a place every four megabytes");
    reads_anywhere(&regions, &mut index, &plain);
    // A reader opening the same archive notes its own places as it decodes.
    let mut opened = Stream::new(Compression::Gzip).unwrap();
    assert_eq!(opened.measure(&regions).unwrap(), plain.len() as u64);
    assert!(opened.points.len() >= 3);
    reads_anywhere(&regions, &mut opened, &plain);
}

#[test]
fn a_bzip2_stream_reads_anywhere_from_each_block() {
    let plain = prose(1 << 20);
    let (regions, mut index) = written(&Packing::Bzip2 { level: 1 }, &plain);
    assert!(index.points.len() >= 10, "every block is a place");
    reads_anywhere(&regions, &mut index, &plain);
    let mut opened = Stream::new(Compression::Bzip2).unwrap();
    assert_eq!(opened.measure(&regions).unwrap(), plain.len() as u64);
    assert_eq!(opened.points.len(), index.points.len());
    reads_anywhere(&regions, &mut opened, &plain);
}

#[test]
fn a_bounded_index_lets_every_other_place_go_and_widens_the_stretch() {
    let mut stream = Stream::new(Compression::Bzip2).unwrap();
    for block in 1..=(BZIP2_POINTS as u64 * 2) {
        stream
            .push(Point::Block {
                decoded: block * 1000,
                bit: block * 8000,
                level: 1,
            })
            .unwrap();
    }
    assert!(stream.points.len() <= BZIP2_POINTS);
    assert!(stream.span >= 1000);
    assert_eq!(
        stream.points[0].decoded(),
        0,
        "the stream's start is always a place"
    );
    assert!(stream
        .points
        .windows(2)
        .all(|pair| pair[0].decoded() < pair[1].decoded()));
    // A place no further on than the last is passed over.
    let count = stream.points.len();
    stream.push(Point::Head { decoded: 0, at: 0 }).unwrap();
    assert_eq!(stream.points.len(), count);
}

#[test]
fn gzip_members_one_after_another_read_as_one_stream() {
    let mut archive = NAMED_GZ.to_vec();
    archive.extend_from_slice(UNNAMED_GZ);
    let attachment = MemoryAttachment::new(&archive, false, b"twice.gz");
    let mut fs = open(&attachment);
    let file = walk(&mut fs, &[b"notes.txt"]);
    assert_eq!(contents(&mut fs, file), [NOTES, NOTES].concat());
}

#[test]
fn zeros_after_the_stream_are_padding_and_anything_else_is_refused() {
    for (archive, name) in [
        (TWO_TAR_GZ, &b"two.tar.gz"[..]),
        (TWO_TAR_BZ2, b"two.tar.bz2"),
    ] {
        let mut padded = archive.to_vec();
        padded.resize(archive.len() + 8192, 0);
        let mut fs = open(&MemoryAttachment::new(&padded, false, name));
        let hello = walk(&mut fs, &[b"hello.txt"]);
        assert_eq!(contents(&mut fs, hello), b"Hello, world!\n");
        let mut trailed = archive.to_vec();
        trailed.extend_from_slice(b"trailing junk");
        assert_eq!(
            try_open(&MemoryAttachment::new(&trailed, false, name)).err(),
            Some(DriverError::BadMagic)
        );
    }
}

#[test]
fn a_stream_whose_check_fails_is_refused_when_opened() {
    let mut gzip = NAMED_GZ.to_vec();
    let crc = gzip.len() - 8;
    gzip[crc] ^= 1;
    assert_eq!(
        try_open(&MemoryAttachment::new(&gzip, false, b"notes.gz")).err(),
        Some(DriverError::MediumError)
    );
    let mut bzip2 = TWO_TAR_BZ2.to_vec();
    // The block's CRC follows its magic.
    bzip2[10] ^= 1;
    assert_eq!(
        try_open(&MemoryAttachment::new(&bzip2, false, b"two.tar.bz2")).err(),
        Some(DriverError::MediumError)
    );
}

#[test]
fn an_empty_tar_is_told_from_a_file_of_zeros_by_its_name() {
    let mut fs = open(&MemoryAttachment::new(ZEROS_GZ, false, b"empty.tgz"));
    assert_eq!(fs.format_name(), FormatName::TarGzip);
    let root = fs.root();
    assert!(names(&mut fs, root).is_empty());
    let mut fs = open(&MemoryAttachment::new(ZEROS_GZ, false, b"zeros.gz"));
    assert_eq!(fs.format_name(), FormatName::Gzip);
    let file = walk(&mut fs, &[b"zeros"]);
    assert_eq!(contents(&mut fs, file), vec![0u8; 10240]);
}

#[test]
fn a_lone_file_is_named_by_its_header_else_by_its_archive() {
    type Case = (&'static [u8], &'static [u8], &'static [u8], FormatName);
    let cases: [Case; 4] = [
        (NAMED_GZ, b"anything.gz", b"notes.txt", FormatName::Gzip),
        (UNNAMED_GZ, b"notes.txt.gz", b"notes.txt", FormatName::Gzip),
        (NOTES_BZ2, b"notes.txt.bz2", b"notes.txt", FormatName::Bzip2),
        (NOTES_BZ2, b"notes", b"notes.out", FormatName::Bzip2),
    ];
    for (archive, attached, file, format) in cases {
        let mut fs = open(&MemoryAttachment::new(archive, false, attached));
        assert_eq!(fs.format_name(), format);
        let root = fs.root();
        assert_eq!(names(&mut fs, root), [file.to_vec()]);
        let node = walk(&mut fs, &[file]);
        assert_eq!(contents(&mut fs, node), NOTES);
    }
    let mut fs = open(&MemoryAttachment::new(NAMED_GZ, false, b"notes.txt.gz"));
    let node = walk(&mut fs, &[b"notes.txt"]);
    assert_eq!(
        fs.node_info(node).unwrap().times.modified.secs(),
        1_622_548_800
    );
}

#[test]
fn a_lone_gzip_file_is_written_and_renamed_and_its_header_follows() {
    let attachment = MemoryAttachment::new(UNNAMED_GZ, true, b"notes.txt.gz");
    let mut fs = open(&attachment);
    let file = walk(&mut fs, &[b"notes.txt"]);
    fs.truncate(file, 0).unwrap();
    fs.write_all(file, 0, b"Rewritten.\n").unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    assert_eq!(
        written[3] & 0x08,
        0,
        "the archive's own name still names the file"
    );
    let mut reopened = open(&MemoryAttachment::new(&written, false, b"notes.txt.gz"));
    let file = walk(&mut reopened, &[b"notes.txt"]);
    assert_eq!(contents(&mut reopened, file), b"Rewritten.\n");

    let root = fs.root();
    fs.rename(root, b"notes.txt", root, b"caf\xc3\xa9.txt")
        .unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    assert_eq!(written[3] & 0x08, 0x08);
    assert_eq!(&written[10..19], b"caf\xe9.txt\0", "recorded in ISO 8859-1");
    let mut reopened = open(&MemoryAttachment::new(&written, false, b"notes.txt.gz"));
    let root = reopened.root();
    assert_eq!(names(&mut reopened, root), [b"caf\xc3\xa9.txt".to_vec()]);
    assert_eq!(
        fs.rename(root, b"caf\xc3\xa9.txt", root, "\u{263a}".as_bytes()),
        Err(DriverError::LengthOutOfRange),
        "a name gzip's header cannot spell"
    );
}

#[test]
fn a_lone_files_archive_holds_that_file_and_no_more() {
    let attachment = MemoryAttachment::new(NOTES_BZ2, true, b"notes.txt.bz2");
    let mut fs = open(&attachment);
    let root = fs.root();
    let file = walk(&mut fs, &[b"notes.txt"]);
    assert_eq!(
        fs.create(root, b"more", NodeKind::RegularFile, &file_record()),
        Err(DriverError::Unsupported)
    );
    assert_eq!(fs.remove(root, b"notes.txt"), Err(DriverError::Unsupported));
    assert_eq!(
        fs.rename(root, b"notes.txt", root, b"other"),
        Err(DriverError::Unsupported)
    );
    assert_eq!(
        fs.set_attr(file, b"acorn.filetype", b"fff"),
        Err(DriverError::Unsupported)
    );
    fs.write_all(file, 0, b"JUST").unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    assert!(written.starts_with(b"BZh9"), "the stream keeps its level");
    let mut reopened = open(&MemoryAttachment::new(&written, false, b"notes.txt.bz2"));
    let file = walk(&mut reopened, &[b"notes.txt"]);
    assert_eq!(
        contents(&mut reopened, file),
        [&b"JUST"[..], &NOTES[4..]].concat()
    );
}
