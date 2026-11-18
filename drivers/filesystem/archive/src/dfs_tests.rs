//! DFS disc images, built here as DFS lays them out — catalogues in descending
//! order of first sector, sides alternating track by track in a `.dsd` —
//! read, and written back with only what changed rewritten.

extern crate std;

use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{
    FilesystemAttrs, FilesystemRead, FilesystemWrite, NodeId, NodeKind, NodeSecurity, WriteRefusal,
};
use tairix_abi::volume::FormatName;
use tairix_abi::DriverError;
use tairix_dfs::catalogue::{Catalogue, Entry, CATALOGUE_LEN};
use tairix_dfs::image::{Layout, SECTOR_LEN, TRACK_LEN};

use crate::test_support::{contents, names, open, walk, MemoryAttachment};
use crate::ArchiveFs;

/// One file of a side being built.
#[derive(Clone, Copy)]
struct File<'a> {
    dir: u8,
    name: &'a [u8],
    load: u32,
    exec: u32,
    data: &'a [u8],
    start: u16,
    locked: bool,
}

const fn file<'a>(dir: u8, name: &'a [u8], data: &'a [u8], start: u16) -> File<'a> {
    File {
        dir,
        name,
        load: 0x3_1900,
        exec: 0x3_8023,
        data,
        start,
        locked: false,
    }
}

/// A side of `sectors` sectors holding `files`, its catalogue titled and
/// cycled as given, and every sector no file covers filled with `free`.
fn side_with(sectors: u16, files: &[File<'_>], free: u8) -> Vec<u8> {
    let mut bytes = vec![free; usize::from(sectors) * SECTOR_LEN];
    let mut entries: Vec<Entry> = files
        .iter()
        .map(|f| {
            let mut name = [b' '; 7];
            name[..f.name.len()].copy_from_slice(f.name);
            Entry {
                name,
                dir: f.dir,
                locked: f.locked,
                load: f.load,
                exec: f.exec,
                length: u32::try_from(f.data.len()).unwrap(),
                start: f.start,
            }
        })
        .collect();
    entries.sort_by_key(|entry| core::cmp::Reverse(entry.start));
    for f in files {
        let at = usize::from(f.start) * SECTOR_LEN;
        bytes[at..at + f.data.len()].copy_from_slice(f.data);
        let tail = (at + f.data.len()).next_multiple_of(SECTOR_LEN);
        bytes[at + f.data.len()..tail].fill(0);
    }
    let mut catalogue = Catalogue::blank(sectors);
    catalogue.title = *b"TEST DISC   ";
    catalogue.cycle = 0x41;
    catalogue.boot = 3;
    catalogue.set_files(&entries).unwrap();
    catalogue.write((&mut bytes[..CATALOGUE_LEN]).try_into().unwrap());
    bytes
}

fn side(sectors: u16, files: &[File<'_>]) -> Vec<u8> {
    side_with(sectors, files, 0xE5)
}

/// Two sides as a `.dsd` holds them, track by track.
fn dsd(first: &[u8], second: &[u8]) -> Vec<u8> {
    let mut image = Vec::new();
    for (a, b) in first.chunks(TRACK_LEN).zip(second.chunks(TRACK_LEN)) {
        image.extend_from_slice(a);
        image.extend_from_slice(b);
    }
    image
}

/// `side`'s bytes as `image`, laid out as `layout`, holds them.
fn side_of(image: &[u8], layout: Layout, side: usize) -> Vec<u8> {
    (0..layout.side_len(side, image.len()))
        .map(|offset| image[layout.image_offset(side, offset)])
        .collect()
}

fn catalogue_of(side: &[u8]) -> Catalogue {
    Catalogue::read(side[..CATALOGUE_LEN].try_into().unwrap()).unwrap()
}

fn record() -> NodeSecurity {
    NodeSecurity::new(0o644, 0, 0)
}

fn writable(
    image: &[u8],
    name: &[u8],
) -> (
    Rc<MemoryAttachment>,
    ArchiveFs<crate::test_support::MemoryAccess>,
) {
    let attachment = MemoryAttachment::new(image, true, name);
    let mut fs = open(&attachment);
    let host = tairix_writeback::TestVolumeHost::leaked(1);
    fs.set_volume_host(tairix_writeback::TestVolumeHost::volume(), host);
    (attachment, fs)
}

fn attr(fs: &mut impl FilesystemAttrs, node: NodeId, key: &[u8]) -> Vec<u8> {
    let mut value = [0u8; 16];
    let len = fs.get_attr(node, key, &mut value).unwrap().unwrap();
    value[..len].to_vec()
}

const CODE: &[u8] = b"A machine-code game, long enough to cross a sector or two. ";

fn sample() -> Vec<u8> {
    side(
        800,
        &[
            file(b'$', b"!BOOT", b"*RUN ELITE\r", 2),
            file(b'$', b"ELITE", &CODE.repeat(10), 3),
            File {
                locked: true,
                load: 0x1900,
                exec: 0x1900,
                ..file(b'B', b"DATA", b"data", 6)
            },
            file(b'$', b"A/B", b"slash", 7),
        ],
    )
}

#[test]
fn an_ssd_reads_as_its_files_full_names() {
    let mut fs = open(&MemoryAttachment::new(&sample(), false, b"Elite.ssd"));
    assert_eq!(fs.format_name(), FormatName::Dfs);
    let root = fs.root();
    assert_eq!(
        names(&mut fs, root),
        [&b"$.!BOOT"[..], b"$.A.B", b"$.ELITE", b"B.DATA"]
    );
    let elite = walk(&mut fs, &[b"$.ELITE"]);
    assert_eq!(contents(&mut fs, elite), CODE.repeat(10));
    assert_eq!(attr(&mut fs, elite, b"acorn.loadaddr"), b"ffff1900");
    assert_eq!(attr(&mut fs, elite, b"acorn.execaddr"), b"ffff8023");
    assert_eq!(attr(&mut fs, elite, b"acorn.attr"), b"RW/");
    let data = walk(&mut fs, &[b"b.data"]);
    assert_eq!(contents(&mut fs, data), b"data");
    assert_eq!(attr(&mut fs, data, b"acorn.loadaddr"), b"00001900");
    assert_eq!(attr(&mut fs, data, b"acorn.attr"), b"RWL/");
    let slash = walk(&mut fs, &[b"$.A.B"]);
    assert_eq!(contents(&mut fs, slash), b"slash");
}

#[test]
fn a_dsd_serves_each_side_as_a_drive() {
    let long = CODE.repeat(100);
    let first = side(400, &[file(b'$', b"LONG", &long, 5)]);
    let second = side(400, &[file(b'$', b"OTHER", b"the second side", 2)]);
    let mut fs = open(&MemoryAttachment::new(
        &dsd(&first, &second),
        false,
        b"Two.DSD",
    ));
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [&b"Drive 0"[..], b"Drive 2"]);
    // The file crosses tracks, each a run of its own in the image.
    let long_file = walk(&mut fs, &[b"Drive 0", b"$.LONG"]);
    assert_eq!(contents(&mut fs, long_file), long);
    let mut middle = [0u8; 700];
    let got = fs.read_at(long_file, 2000, &mut middle).unwrap();
    assert_eq!(&middle[..got], &long[2000..2000 + got]);
    let other = walk(&mut fs, &[b"Drive 2", b"$.OTHER"]);
    assert_eq!(contents(&mut fs, other), b"the second side");
}

#[test]
fn a_dsd_with_no_second_catalogue_serves_its_first_side() {
    let first = side(400, &[file(b'$', b"ONLY", b"one side", 2)]);
    let mut fs = open(&MemoryAttachment::new(
        &dsd(&first, &vec![0; first.len()]),
        false,
        b"Half.dsd",
    ));
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [&b"Drive 0"[..]]);
}

#[test]
fn an_image_named_nothing_is_double_sided_only_past_its_first_side() {
    let first = side(400, &[file(b'$', b"ONE", b"1", 2)]);
    let second = side(400, &[file(b'$', b"TWO", b"2", 2)]);
    let mut double = open(&MemoryAttachment::new(&dsd(&first, &second), false, b""));
    let root = double.root();
    assert_eq!(names(&mut double, root), [&b"Drive 0"[..], b"Drive 2"]);
    let mut single = open(&MemoryAttachment::new(&first, false, b""));
    let root = single.root();
    assert_eq!(names(&mut single, root), [&b"$.ONE"[..]]);
}

#[test]
fn bytes_past_the_image_s_end_read_as_unrecorded_zeros() {
    let mut image = side(400, &[file(b'$', b"CUT", &[0x55; 600], 2)]);
    image.truncate(2 * SECTOR_LEN + 300);
    let mut fs = open(&MemoryAttachment::new(&image, false, b"cut.ssd"));
    let cut = walk(&mut fs, &[b"$.CUT"]);
    let read = contents(&mut fs, cut);
    assert_eq!(read.len(), 600);
    assert_eq!(&read[..300], &[0x55; 300][..]);
    assert!(read[300..].iter().all(|&byte| byte == 0));
}

#[test]
fn a_new_file_takes_the_first_gap_and_leaves_every_other_sector_as_it_was() {
    let image = side(
        400,
        &[
            file(b'$', b"LOW", &[1; 300], 2),
            file(b'$', b"HIGH", &[2; 10], 20),
        ],
    );
    let (attachment, mut fs) = writable(&image, b"gap.ssd");
    let root = fs.root();
    let added = fs
        .create(root, b"$.NEW", NodeKind::RegularFile, &record())
        .unwrap();
    fs.write_all(added, 0, &[3; 1000]).unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    assert_eq!(written.len(), image.len(), "the image keeps its length");
    let catalogue = catalogue_of(&written);
    assert_eq!(catalogue.cycle, 0x42, "the cycle steps in BCD");
    assert_eq!(catalogue.title, *b"TEST DISC   ");
    assert_eq!(catalogue.boot, 3);
    let starts: Vec<(u8, u16)> = catalogue
        .files()
        .iter()
        .map(|entry| (entry.name[0], entry.start))
        .collect();
    // Highest first; the new file in the first gap that holds it.
    assert_eq!(starts, [(b'H', 20), (b'N', 4), (b'L', 2)]);
    let new_at = 4 * SECTOR_LEN;
    assert_eq!(&written[new_at..new_at + 1000], &[3; 1000][..]);
    assert!(written[new_at + 1000..8 * SECTOR_LEN]
        .iter()
        .all(|&b| b == 0));
    // Free sectors keep what they held, and so do unchanged files.
    assert_eq!(&written[8 * SECTOR_LEN..], &image[8 * SECTOR_LEN..]);
    assert_eq!(
        &written[2 * SECTOR_LEN..4 * SECTOR_LEN],
        &image[2 * SECTOR_LEN..4 * SECTOR_LEN]
    );
    assert!(
        attachment.asked().copied > 0,
        "unchanged sectors are the kernel's to copy"
    );
}

#[test]
fn a_file_written_again_goes_back_where_it_was_if_it_fits() {
    let image = side(
        400,
        &[
            file(b'$', b"FIRST", &[1; 600], 2),
            file(b'$', b"NEXT", &[2; 10], 10),
        ],
    );
    let (attachment, mut fs) = writable(&image, b"home.ssd");
    let first = walk(&mut fs, &[b"$.FIRST"]);
    fs.truncate(first, 0).unwrap();
    fs.write_all(first, 0, &[9; 2000]).unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    let catalogue = catalogue_of(&written);
    let entry = catalogue
        .files()
        .iter()
        .find(|e| e.name[0] == b'F')
        .unwrap();
    assert_eq!(entry.start, 2, "eight sectors still fit before NEXT");
    assert_eq!(entry.length, 2000);
    assert_eq!(
        &written[2 * SECTOR_LEN..2 * SECTOR_LEN + 2000],
        &[9; 2000][..]
    );
}

#[test]
fn a_grown_file_that_no_longer_fits_moves_to_the_first_gap_that_holds_it() {
    let image = side(
        400,
        &[
            file(b'$', b"FIRST", &[1; 600], 2),
            file(b'$', b"NEXT", &[2; 10], 5),
        ],
    );
    let (attachment, mut fs) = writable(&image, b"grow.ssd");
    let first = walk(&mut fs, &[b"$.FIRST"]);
    fs.write_all(first, 600, &[7; 1000]).unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    let catalogue = catalogue_of(&written);
    let entry = catalogue
        .files()
        .iter()
        .find(|e| e.name[0] == b'F')
        .unwrap();
    assert_eq!(entry.start, 6);
    let at = 6 * SECTOR_LEN;
    assert_eq!(&written[at..at + 600], &[1; 600][..]);
    assert_eq!(&written[at + 600..at + 1600], &[7; 1000][..]);
    // Its old sectors are free now, and keep what they held.
    assert_eq!(
        &written[2 * SECTOR_LEN..5 * SECTOR_LEN],
        &image[2 * SECTOR_LEN..5 * SECTOR_LEN]
    );
}

#[test]
fn a_side_is_compacted_only_when_no_gap_holds_a_file() {
    // Two sectors free before MID and three after it: four fit only once
    // the side is compacted.
    let image = side(
        10,
        &[
            file(b'$', b"LOW", &[1; 256], 2),
            file(b'$', b"MID", &[2; 512], 5),
        ],
    );
    let (attachment, mut fs) = writable(&image, b"tight.ssd");
    let root = fs.root();
    let added = fs
        .create(root, b"$.FOUR", NodeKind::RegularFile, &record())
        .unwrap();
    fs.write_all(added, 0, &[3; 4 * SECTOR_LEN]).unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    let catalogue = catalogue_of(&written);
    let starts: Vec<(u8, u16)> = catalogue
        .files()
        .iter()
        .map(|entry| (entry.name[0], entry.start))
        .collect();
    assert_eq!(starts, [(b'F', 5), (b'M', 3), (b'L', 2)]);
    assert_eq!(&written[3 * SECTOR_LEN..5 * SECTOR_LEN], &[2; 512][..]);
    assert_eq!(
        &written[5 * SECTOR_LEN..9 * SECTOR_LEN],
        &[3; 4 * SECTOR_LEN][..]
    );
}

#[test]
fn a_catalogue_holding_the_same_entries_is_left_as_it_was() {
    let image = sample();
    let (attachment, mut fs) = writable(&image, b"same.ssd");
    let elite = walk(&mut fs, &[b"$.ELITE"]);
    fs.write_all(elite, 5, b"PATCHED").unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    assert_eq!(&written[..CATALOGUE_LEN], &image[..CATALOGUE_LEN]);
    let mut expected = CODE.repeat(10);
    expected[5..12].copy_from_slice(b"PATCHED");
    let at = 3 * SECTOR_LEN;
    assert_eq!(&written[at..at + expected.len()], &expected[..]);
}

#[test]
fn a_side_holds_thirty_one_files_and_its_own_sectors() {
    let (_, mut fs) = writable(&side(40, &[]), b"full.ssd");
    let root = fs.root();
    for n in 0..31 {
        let name = std::format!("$.F{n}");
        fs.create(root, name.as_bytes(), NodeKind::RegularFile, &record())
            .unwrap();
    }
    assert_eq!(
        fs.create(root, b"$.MORE", NodeKind::RegularFile, &record()),
        Err(DriverError::NoSpace)
    );
    let f0 = walk(&mut fs, &[b"$.F0"]);
    // Forty sectors, two of them the catalogue's.
    assert_eq!(
        fs.write_all(f0, 0, &[0; 38 * SECTOR_LEN + 1]),
        Err(DriverError::NoSpace)
    );
    fs.write_all(f0, 0, &[0; 38 * SECTOR_LEN]).unwrap();
    fs.flush().unwrap();
}

#[test]
fn names_are_dfs_names() {
    let (attachment, mut fs) = writable(&sample(), b"names.ssd");
    let root = fs.root();
    for (name, refusal) in [
        (&b"ELITE"[..], DriverError::OutOfRange),
        (b"$.EIGHTCHR", DriverError::LengthOutOfRange),
        (b"$.A*B", DriverError::OutOfRange),
        (b"$.A B", DriverError::OutOfRange),
        (b"AB.C", DriverError::OutOfRange),
        (b"$.", DriverError::OutOfRange),
    ] {
        assert_eq!(
            fs.create(root, name, NodeKind::RegularFile, &record()),
            Err(refusal),
            "{name:?}"
        );
    }
    assert_eq!(
        fs.create(root, b"$.DIR", NodeKind::Directory, &record()),
        Err(DriverError::Unsupported)
    );
    // Names compare as DFS compares them, letters in either case.
    assert_eq!(
        fs.create(root, b"$.elite", NodeKind::RegularFile, &record()),
        Err(DriverError::AlreadyExists)
    );
    fs.rename(root, b"$.ELITE", root, b"G.Game.1").unwrap();
    fs.flush().unwrap();
    let mut reopened = open(&MemoryAttachment::new(
        &attachment.archive(),
        false,
        b"names.ssd",
    ));
    let game = walk(&mut reopened, &[b"G.Game.1"]);
    assert_eq!(contents(&mut reopened, game), CODE.repeat(10));
    let catalogue = catalogue_of(&attachment.archive());
    let entry = catalogue.files().iter().find(|e| e.dir == b'G').unwrap();
    assert_eq!(&entry.name, b"Game/1 ");
    assert_eq!(entry.start, 3, "a rename moves no bytes");
}

#[test]
fn addresses_and_the_lock_are_attributes_a_write_leaves_alone() {
    let (attachment, mut fs) = writable(&sample(), b"attrs.ssd");
    let elite = walk(&mut fs, &[b"$.ELITE"]);
    // An I/O-processor address reads as a RISC OS typed file's would, but a
    // write stamps no time into it.
    fs.write_all(elite, 0, b"x").unwrap();
    assert_eq!(attr(&mut fs, elite, b"acorn.loadaddr"), b"ffff1900");
    assert_eq!(
        fs.set_attr(elite, b"acorn.loadaddr", b"00ff2000"),
        Err(DriverError::OutOfRange)
    );
    assert_eq!(
        fs.set_attr(elite, b"acorn.attr", b"L/"),
        Err(DriverError::OutOfRange)
    );
    assert_eq!(
        fs.set_attr(elite, b"acorn.attr", b"RWLP/"),
        Err(DriverError::OutOfRange)
    );
    fs.set_attr(elite, b"acorn.loadaddr", b"ffff2000").unwrap();
    fs.set_attr(elite, b"acorn.execaddr", b"00002011").unwrap();
    fs.set_attr(elite, b"acorn.attr", b"RWL/").unwrap();
    fs.flush().unwrap();
    let catalogue = catalogue_of(&attachment.archive());
    let entry = catalogue
        .files()
        .iter()
        .find(|e| e.name[0] == b'E')
        .unwrap();
    assert_eq!(
        (entry.load, entry.exec, entry.locked),
        (0x3_2000, 0x2011, true)
    );
}

#[test]
fn a_name_keeps_the_top_bits_it_was_read_with_until_renamed() {
    let mut image = side(400, &[file(b'$', b"TOP", b"protected", 2)]);
    image[8] |= 0x80;
    let (attachment, mut fs) = writable(&image, b"top.ssd");
    let top = walk(&mut fs, &[b"$.TOP"]);
    fs.write_all(top, 0, b"P").unwrap();
    fs.flush().unwrap();
    assert_eq!(&attachment.archive()[8..11], b"\xD4OP");
    let root = fs.root();
    fs.rename(root, b"$.TOP", root, b"$.TIP").unwrap();
    fs.flush().unwrap();
    assert_eq!(&attachment.archive()[8..11], b"TIP");
}

#[test]
fn a_file_renamed_to_the_other_side_takes_its_bytes() {
    let first = side(400, &[file(b'$', b"MOVER", &CODE.repeat(60), 7)]);
    let second = side(400, &[]);
    let (attachment, mut fs) = writable(&dsd(&first, &second), b"move.dsd");
    let from = walk(&mut fs, &[b"Drive 0"]);
    let to = walk(&mut fs, &[b"Drive 2"]);
    fs.rename(from, b"$.MOVER", to, b"$.MOVED").unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    let second = side_of(&written, Layout::Double, 1);
    let catalogue = catalogue_of(&second);
    assert_eq!(catalogue.files().len(), 1);
    let entry = catalogue.files()[0];
    assert_eq!(entry.start, 2);
    let at = 2 * SECTOR_LEN;
    let moved = CODE.repeat(60);
    assert_eq!(&second[at..at + moved.len()], &moved[..]);
    assert!(catalogue_of(&side_of(&written, Layout::Double, 0))
        .files()
        .is_empty());
}

#[test]
fn a_side_s_folder_is_neither_removed_nor_renamed() {
    let first = side(400, &[]);
    let (_, mut fs) = writable(&dsd(&first, &side(400, &[])), b"fixed.dsd");
    let root = fs.root();
    assert_eq!(fs.remove(root, b"Drive 2"), Err(DriverError::Unsupported));
    assert!(fs.rename(root, b"Drive 2", root, b"Drive 3").is_err());
    assert_eq!(
        fs.create(root, b"$.ROOT", NodeKind::RegularFile, &record()),
        Err(DriverError::Unsupported)
    );
}

#[test]
fn a_name_no_served_name_can_spell_is_left_out_and_kept() {
    let mut image = side(
        400,
        &[file(b'$', b"KEPT", b"k", 2), file(b'$', b"DOTX", b"d", 3)],
    );
    // DOTX holds the higher first sector, so it is the catalogue's first.
    image[8 + 1] = b'.';
    let mut fs = open(&MemoryAttachment::new(&image, true, b"dot.ssd"));
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [&b"$.KEPT"[..]]);
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::UnsupportedFeature));
}

#[test]
fn a_watford_second_catalogue_leaves_the_disc_unwritten() {
    let mut image = side(400, &[file(b'$', b"FILE", b"f", 4)]);
    image[CATALOGUE_LEN..CATALOGUE_LEN + 8].fill(0xAA);
    let fs = open(&MemoryAttachment::new(&image, true, b"watford.ssd"));
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::UnsupportedFeature));
    // The mark inside a file's bytes is that file's, not a catalogue.
    let mut covered = side(400, &[file(b'$', b"FILE", &[0xAA; 8], 2)]);
    covered[CATALOGUE_LEN..CATALOGUE_LEN + 8].fill(0xAA);
    let fs = open(&MemoryAttachment::new(&covered, true, b"plain.ssd"));
    assert_eq!(fs.write_refusal(), None);
}

#[test]
fn an_image_grows_only_to_the_last_sector_written() {
    let mut image = side(400, &[file(b'$', b"A", b"a", 2)]);
    image.truncate(3 * SECTOR_LEN);
    let (attachment, mut fs) = writable(&image, b"short.ssd");
    let root = fs.root();
    let added = fs
        .create(root, b"$.B", NodeKind::RegularFile, &record())
        .unwrap();
    fs.write_all(added, 0, &[4; 300]).unwrap();
    fs.flush().unwrap();
    let written = attachment.archive();
    assert_eq!(written.len(), 5 * SECTOR_LEN);
    assert_eq!(
        &written[3 * SECTOR_LEN..3 * SECTOR_LEN + 300],
        &[4; 300][..]
    );
}

#[test]
fn a_file_of_no_bytes_starts_where_dfs_starts_one() {
    let (attachment, mut fs) = writable(&side(400, &[file(b'$', b"A", &[1; 256], 2)]), b"zero.ssd");
    let root = fs.root();
    fs.create(root, b"$.EMPTY", NodeKind::RegularFile, &record())
        .unwrap();
    fs.flush().unwrap();
    let catalogue = catalogue_of(&attachment.archive());
    let starts: Vec<(u8, u16, u32)> = catalogue
        .files()
        .iter()
        .map(|entry| (entry.name[0], entry.start, entry.end()))
        .collect();
    // A file ending later is listed ahead of one starting with it.
    assert_eq!(starts, [(b'A', 2, 3), (b'E', 2, 2)]);
}
