//! Host tests: every variant formatted, read and written in memory, the tree
//! and bitmap checked against each other by a walker of the raw image that
//! shares nothing with the driver but the format's constants, and the
//! damaged and hostile cases refused.

// Test volumes are a few MiB, so every offset, size and block number fits a
// usize on every target, and a pattern byte is its index's low byte.
#![allow(clippy::cast_possible_truncation)]

extern crate std;

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::driver::filesystem::clock::TestClock;
use tairix_abi::driver::filesystem::listing::listed;
use tairix_abi::driver::filesystem::{
    DirVisit, FilesystemAttrs, FilesystemRead, FilesystemSecurity, FilesystemStats,
    FilesystemWrite, NameMatching, NodeId, NodeKind, NodeSecurity, Unpinned, WriteRefusal,
};
use tairix_abi::time::Time64;
use tairix_abi::{CapabilityId, DriverError, DriverHost, DriverKind};
use tairix_fsprobe::affs::{
    be32, put_be32, root_block, DateStamp, Offsets, BITMAP_VALID, ST_LINKDIR, ST_SOFTLINK, T_HEADER,
};
use tairix_fuzzseed::Prng;
use tairix_writeback::TestVolumeHost;

use crate::{Affs, FormatOptions};

const RECORD: NodeSecurity = NodeSecurity::new(0o644, 0, 0);

/// An in-memory block device.
struct MemDisk {
    data: Vec<u8>,
    block_size: u32,
}

impl MemDisk {
    fn new(bytes: usize) -> Self {
        Self {
            data: vec![0; bytes],
            block_size: 512,
        }
    }
}

impl Block for MemDisk {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: self.block_size,
            block_count: self.data.len() as u64 / u64::from(self.block_size),
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let at = (lba * u64::from(self.block_size)) as usize;
        let src = self
            .data
            .get(at..at + buf.len())
            .ok_or(DriverError::LengthOutOfRange)?;
        buf.copy_from_slice(src);
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, data: &[u8]) -> Result<(), DriverError> {
        let at = (lba * u64::from(self.block_size)) as usize;
        self.data
            .get_mut(at..at + data.len())
            .ok_or(DriverError::LengthOutOfRange)?
            .copy_from_slice(data);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

/// The variants formatted: type, block size and the device size.
const VARIANTS: [(u8, u32, usize); 7] = [
    (0, 512, 901_120),
    (1, 512, 901_120),
    (2, 512, 1_802_240),
    (3, 512, 1_802_240),
    (1, 1024, 2 * 1024 * 1024),
    (3, 4096, 4 * 1024 * 1024),
    // Past 25 bitmap blocks: the bitmap's extension blocks are walked.
    (1, 512, 64 * 1024 * 1024),
];

fn created() -> DateStamp {
    DateStamp {
        days: 9000,
        mins: 720,
        ticks: 10,
    }
}

fn fresh(dos_type: u8, block_size: u32, bytes: usize) -> Affs<MemDisk> {
    Affs::format(
        MemDisk::new(bytes),
        &FormatOptions {
            dos_type,
            block_size,
            name: b"Work",
            created: created(),
        },
    )
    .unwrap_or_else(|err| panic!("DOS\\{dos_type} at {block_size}: {err:?}"))
}

fn floppy(dos_type: u8) -> Affs<MemDisk> {
    fresh(dos_type, 512, 901_120)
}

fn make(fs: &mut Affs<MemDisk>, dir: NodeId, name: &str, data: &[u8]) -> NodeId {
    let file = fs
        .create(dir, name.as_bytes(), NodeKind::RegularFile, &RECORD)
        .expect("create");
    if !data.is_empty() {
        fs.write_all(file, 0, data).expect("write");
    }
    file
}

fn mkdir(fs: &mut Affs<MemDisk>, dir: NodeId, name: &str) -> NodeId {
    fs.create(dir, name.as_bytes(), NodeKind::Directory, &RECORD)
        .expect("mkdir")
}

fn read_all(fs: &mut Affs<MemDisk>, file: NodeId) -> Vec<u8> {
    let size = fs.node_info(file).expect("info").size as usize;
    let mut out = vec![0; size];
    let mut at = 0;
    // An odd window, so reads straddle every block boundary.
    while at < size {
        let end = (at + 333).min(size);
        let got = fs
            .read_at(file, at as u64, &mut out[at..end])
            .expect("read");
        assert!(got > 0, "short read at {at} of {size}");
        at += got;
    }
    out
}

fn names(fs: &mut Affs<MemDisk>, dir: NodeId) -> Vec<String> {
    let mut names: Vec<String> = listed(fs, dir, 0, b"")
        .expect("listing")
        .into_iter()
        .map(|(_, name)| String::from_utf8(name).expect("utf-8"))
        .collect();
    names.sort();
    names
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|at| (at as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn image(fs: Affs<MemDisk>) -> (Vec<u8>, usize) {
    let block_size = fs.layout.block_size as usize;
    (fs.into_device().data, block_size)
}

/// Walk `image` the way a validator does — every header from the root, every
/// data and extension block a file names, every bitmap block the root names
/// — and check the bitmap marks exactly those blocks used and is marked
/// valid. Shares nothing with the driver but the format's offsets.
fn check(image: &[u8], bs: usize) {
    let used = filed_blocks(image, bs);
    check_bitmap(image, bs, used);
}

/// The root, and every header, data and file extension block filed beneath it.
fn filed_blocks(image: &[u8], bs: usize) -> BTreeSet<u64> {
    let at = Offsets::new(bs);
    let blocks = (image.len() / bs) as u64;
    let block = |n: u64| &image[n as usize * bs..(n as usize + 1) * bs];
    let root = root_block(blocks);
    let mut used = BTreeSet::new();
    used.insert(root);
    let mut dirs = vec![root];
    let tl = at.table_len();
    let fast = image[3] & 1 == 1;
    let ds = if fast { bs } else { bs - 24 } as u64;
    while let Some(dir) = dirs.pop() {
        let header = block(dir);
        for slot in 0..tl {
            let mut next = be32(header, 24 + 4 * slot);
            let mut guard = 0;
            while next != 0 {
                guard += 1;
                assert!(guard < 100_000, "a chain loops");
                let entry = block(u64::from(next));
                assert_eq!(be32(entry, 0), T_HEADER);
                assert!(tairix_fsprobe::affs::sums_to_zero(entry));
                assert_eq!(
                    u64::from(be32(entry, at.parent())),
                    dir,
                    "filed where it lives"
                );
                assert!(used.insert(u64::from(next)), "a header filed twice");
                match be32(entry, at.sec_type()) {
                    2 => dirs.push(u64::from(next)),
                    0xFFFF_FFFD => {
                        let size = u64::from(be32(entry, at.byte_size()));
                        let count = size.div_ceil(ds);
                        let mut table = entry;
                        let mut ext = be32(entry, at.extension());
                        for index in 0..count {
                            let within = index % tl as u64;
                            if index > 0 && within == 0 {
                                assert_ne!(ext, 0, "the extension chain is short");
                                assert!(used.insert(u64::from(ext)), "an extension used twice");
                                table = block(u64::from(ext));
                                ext = be32(table, at.extension());
                            }
                            let pointer = be32(table, 24 + 4 * (tl - 1 - within as usize));
                            assert!(used.insert(u64::from(pointer)), "a data block used twice");
                        }
                    }
                    _ => {}
                }
                next = be32(entry, at.hash_chain());
            }
        }
    }
    used
}

/// Check the bitmap is validated, sealed, and marks free exactly the blocks
/// not in `used` and the bitmap's own blocks, and none past the volume.
fn check_bitmap(image: &[u8], bs: usize, mut used: BTreeSet<u64>) {
    let at = Offsets::new(bs);
    let blocks = (image.len() / bs) as u64;
    let block = |n: u64| &image[n as usize * bs..(n as usize + 1) * bs];
    let header = block(root_block(blocks));
    assert_eq!(
        be32(header, at.bm_flag()),
        BITMAP_VALID,
        "the bitmap is validated"
    );
    let bits = (bs as u64 / 4 - 1) * 32;
    let pages = (blocks - 2).div_ceil(bits);
    let mut pointers = Vec::new();
    for page in 0..25.min(pages) as usize {
        pointers.push(u64::from(be32(header, at.bm_pages() + 4 * page)));
    }
    let mut ext = be32(header, at.bm_ext());
    while ext != 0 && (pointers.len() as u64) < pages {
        used.insert(u64::from(ext));
        let ext_block = block(u64::from(ext));
        for slot in 0..(bs / 4 - 1) {
            if (pointers.len() as u64) < pages {
                pointers.push(u64::from(be32(ext_block, 4 * slot)));
            }
        }
        ext = be32(ext_block, bs - 4);
    }
    for &pointer in &pointers {
        used.insert(pointer);
        assert!(
            tairix_fsprobe::affs::sums_to_zero(block(pointer)),
            "bitmap checksum"
        );
    }
    let free = |bit: u64| {
        let page = block(pointers[(bit / bits) as usize]);
        let long = be32(page, 4 + 4 * ((bit % bits) / 32) as usize);
        long & (1 << (bit % 32)) != 0
    };
    for n in 2..blocks {
        let free = free(n - 2);
        assert_eq!(
            free,
            !used.contains(&n),
            "block {n}: bitmap says free={free}"
        );
    }
    for bit in blocks - 2..pages * bits {
        assert!(
            !free(bit),
            "bit {bit} lies past the volume, so is never free"
        );
    }
}

fn check_fs(fs: Affs<MemDisk>) -> Affs<MemDisk> {
    let (data, bs) = image(fs);
    check(&data, bs);
    Affs::open(MemDisk {
        data,
        block_size: 512,
    })
    .expect("reopens")
}

struct MockHost;

impl DriverHost for MockHost {
    fn has_capability(&self, cap: CapabilityId) -> bool {
        cap == CapabilityId::DRV_LOAD
    }

    fn kind(&self) -> DriverKind {
        DriverKind::UserSpace
    }
}

#[test]
fn register_needs_the_load_capability() {
    assert!(crate::register(&MockHost).is_ok());
}

#[test]
fn every_variant_formats_opens_and_checks_out_empty() {
    for (dos_type, block_size, bytes) in VARIANTS {
        let mut fs = fresh(dos_type, block_size, bytes);
        let root = fs.root();
        assert!(names(&mut fs, root).is_empty());
        let stats = fs.stats().expect("stats");
        assert_eq!(stats.block_size, block_size);
        let blocks = bytes as u64 / u64::from(block_size);
        assert_eq!(stats.total_blocks, blocks - 2);
        assert!(stats.free_blocks > 0 && stats.free_blocks < stats.total_blocks);
        assert_eq!(fs.write_refusal(), None);
        let expected = if dos_type >= 2 {
            NameMatching::Latin1CaseInsensitive
        } else {
            NameMatching::AsciiCaseInsensitive
        };
        assert_eq!(fs.name_matching(), expected);
        let fs = check_fs(fs);
        drop(fs);
    }
}

#[test]
fn files_round_trip_across_every_block_and_extension_boundary() {
    for (dos_type, block_size, bytes) in VARIANTS.into_iter().take(6) {
        let mut fs = fresh(dos_type, block_size, bytes);
        let root = fs.root();
        let ds = if dos_type & 1 == 1 {
            block_size as usize
        } else {
            block_size as usize - 24
        };
        let tl = block_size as usize / 4 - 56;
        let sizes: Vec<usize> = [
            0,
            1,
            ds - 1,
            ds,
            ds + 1,
            tl * ds,
            tl * ds + 1,
            3 * tl * ds + 7,
        ]
        .into_iter()
        .filter(|&size| size * 4 < bytes)
        .collect();
        for (at, &size) in sizes.iter().enumerate() {
            let name = alloc::format!("file{at}");
            let data = pattern(size, at as u8);
            let file = make(&mut fs, root, &name, &data);
            assert_eq!(read_all(&mut fs, file), data, "{name} of {size} bytes");
            let info = fs.node_info(file).expect("info");
            assert_eq!(info.size, size as u64);
            assert_eq!(info.kind, NodeKind::RegularFile);
            let blocks = size.div_ceil(ds);
            let exts = blocks.saturating_sub(tl).div_ceil(tl);
            assert_eq!(
                info.allocated,
                ((blocks + exts) * block_size as usize) as u64
            );
        }
        let mut fs = check_fs(fs);
        let root = fs.root();
        for (at, &size) in sizes.iter().enumerate() {
            let file = fs
                .lookup(root, alloc::format!("FILE{at}").as_bytes())
                .expect("found again");
            assert_eq!(read_all(&mut fs, file), pattern(size, at as u8));
        }
    }
}

#[test]
fn overwrites_appends_sparse_writes_and_truncates_keep_the_model() {
    for dos_type in [0, 1] {
        let mut fs = floppy(dos_type);
        let root = fs.root();
        let file = make(&mut fs, root, "data", &pattern(1000, 1));
        let mut model = pattern(1000, 1);
        let mut apply = |fs: &mut Affs<MemDisk>, offset: usize, data: &[u8]| {
            fs.write_all(file, offset as u64, data).expect("write");
            if model.len() < offset + data.len() {
                model.resize(offset + data.len(), 0);
            }
            model[offset..offset + data.len()].copy_from_slice(data);
            assert_eq!(read_all(fs, file), model, "after a write at {offset}");
        };
        apply(&mut fs, 10, &pattern(50, 2));
        apply(&mut fs, 990, &pattern(100, 3));
        apply(&mut fs, 60_000, &pattern(700, 4));
        apply(&mut fs, 0, &pattern(70_000, 5));
        // What a shrink cut off reads back as zeros when the file grows again.
        for size in [70_001u64, 35_000, 488, 487, 1, 0, 45_000] {
            fs.truncate(file, size).expect("truncate");
            model.resize(size as usize, 0);
            assert_eq!(read_all(&mut fs, file), model, "truncated to {size}");
        }
        drop(check_fs(fs));
    }
}

#[test]
fn a_file_grown_over_its_old_tail_reads_zeros_there() {
    let mut fs = floppy(1);
    let root = fs.root();
    let file = make(&mut fs, root, "tail", &[0xAA; 600]);
    fs.truncate(file, 100).expect("shrink");
    fs.truncate(file, 600).expect("grow");
    let back = read_all(&mut fs, file);
    assert_eq!(&back[..100], &[0xAA; 100]);
    assert!(back[100..].iter().all(|&b| b == 0), "no stale tail");
}

#[test]
fn removing_files_and_directories_gives_their_space_back() {
    let mut fs = floppy(1);
    let root = fs.root();
    let before = fs.stats().expect("stats").free_blocks;
    let dir = mkdir(&mut fs, root, "Dir");
    let file = make(&mut fs, dir, "big", &pattern(200_000, 9));
    assert_eq!(fs.remove(root, b"dir"), Err(DriverError::DirectoryNotEmpty));
    fs.remove(dir, b"BIG").expect("remove the file");
    assert_eq!(
        fs.node_info(file),
        Err(DriverError::NotFound),
        "its id names nothing"
    );
    fs.remove(root, b"Dir").expect("remove the directory");
    assert_eq!(fs.stats().expect("stats").free_blocks, before);
    assert_eq!(fs.remove(root, b"Dir"), Err(DriverError::NotFound));
    drop(check_fs(fs));
}

#[test]
fn names_are_checked_and_fold_as_the_volume_folds_them() {
    let mut ascii = floppy(1);
    let root = ascii.root();
    make(&mut ascii, root, "Résumé", b"x");
    assert!(
        ascii.lookup(root, "RÉSUMÉ".as_bytes()).is_err(),
        "only ASCII folds"
    );
    assert!(ascii.lookup(root, "rÉsumÉ".as_bytes()).is_err());
    assert!(ascii.lookup(root, "résumé".as_bytes()).is_ok());
    let mut intl = floppy(3);
    let root = intl.root();
    make(&mut intl, root, "Résumé", b"x");
    assert!(
        intl.lookup(root, "RÉSUMÉ".as_bytes()).is_ok(),
        "Latin-1 folds"
    );
    assert_eq!(names(&mut intl, root), ["Résumé"]);
    for (bad, err) in [
        ("a:b", DriverError::OutOfRange),
        ("€uro", DriverError::OutOfRange),
        ("tab\there", DriverError::OutOfRange),
        ("", DriverError::LengthOutOfRange),
        (
            "0123456789012345678901234567890",
            DriverError::LengthOutOfRange,
        ),
    ] {
        assert_eq!(
            intl.create(root, bad.as_bytes(), NodeKind::RegularFile, &RECORD),
            Err(err),
            "{bad:?}"
        );
    }
    let longest = "012345678901234567890123456789";
    make(&mut intl, root, longest, b"");
    assert_eq!(
        intl.create(root, b"resume", NodeKind::RegularFile, &RECORD)
            .map(|_| ()),
        Ok(())
    );
    assert_eq!(
        intl.create(root, "RÉSUMÉ".as_bytes(), NodeKind::RegularFile, &RECORD),
        Err(DriverError::AlreadyExists)
    );
}

#[test]
fn a_listing_hands_each_entry_over_once_however_often_it_stops() {
    let mut fs = floppy(1);
    let root = fs.root();
    let mut expected = BTreeSet::new();
    for n in 0..200 {
        let name = alloc::format!("entry-{n}");
        make(&mut fs, root, &name, &[]);
        expected.insert(name);
    }
    let mut seen = BTreeSet::new();
    let mut cursor = 0;
    let mut after = Vec::new();
    loop {
        // Take one entry and stop at the next, which is left unread.
        let mut taken = None;
        fs.read_dir(root, cursor, &after, &mut |entry, name| {
            if taken.is_some() {
                return DirVisit::Stop;
            }
            taken = Some((entry.next_cursor, name.to_vec()));
            DirVisit::Take
        })
        .expect("listing");
        let Some((next, name)) = taken else { break };
        assert!(
            seen.insert(String::from_utf8(name.clone()).expect("utf-8")),
            "seen twice"
        );
        cursor = next;
        after = name;
    }
    assert_eq!(seen, expected);
}

#[test]
fn a_listing_keeps_its_place_across_changes_around_it() {
    let mut fs = floppy(1);
    let root = fs.root();
    for n in 0..100 {
        make(&mut fs, root, &alloc::format!("keep-{n}"), &[]);
    }
    let mut seen = Vec::new();
    let mut cursor = 0;
    let mut round = 0;
    loop {
        let mut next = None;
        let mut count = 0;
        fs.read_dir(root, cursor, b"", &mut |entry, name| {
            if count == 7 {
                return DirVisit::Stop;
            }
            count += 1;
            seen.push(String::from_utf8(name.to_vec()).expect("utf-8"));
            next = Some(entry.next_cursor);
            DirVisit::Take
        })
        .expect("listing");
        let Some(next) = next else { break };
        cursor = next;
        round += 1;
        make(&mut fs, root, &alloc::format!("added-{round}"), &[]);
    }
    for n in 0..100 {
        let name = alloc::format!("keep-{n}");
        assert_eq!(seen.iter().filter(|s| **s == name).count(), 1, "{name}");
    }
}

#[test]
fn renames_move_replace_and_refuse_as_posix_asks() {
    let mut fs = floppy(3);
    let root = fs.root();
    let top = mkdir(&mut fs, root, "a");
    let inner = mkdir(&mut fs, top, "b");
    let first = make(&mut fs, root, "f", b"first");
    let second = make(&mut fs, top, "g", b"second");
    // Within a directory, and a change of case alone.
    fs.rename(root, b"f", root, b"F2").expect("rename");
    fs.rename(root, b"f2", root, b"f2").expect("respell");
    assert_eq!(names(&mut fs, root), ["a", "f2"]);
    // Across directories, the id kept.
    fs.rename(root, b"f2", inner, b"moved").expect("move");
    assert_eq!(fs.lookup(inner, b"moved"), Ok(first));
    // A file replaces a file.
    fs.rename(inner, b"moved", top, b"g").expect("replace");
    let replaced = fs.lookup(top, b"g").expect("g");
    assert_eq!(read_all(&mut fs, replaced), b"first");
    assert_eq!(fs.node_info(second), Err(DriverError::NotFound));
    // A directory may not go inside itself, nor over a file.
    assert_eq!(
        fs.rename(root, b"a", inner, b"loop"),
        Err(DriverError::DirectoryCycle)
    );
    assert_eq!(
        fs.rename(top, b"b", top, b"g"),
        Err(DriverError::Unsupported)
    );
    let other = mkdir(&mut fs, root, "c");
    make(&mut fs, other, "inside", b"");
    assert_eq!(
        fs.rename(top, b"b", root, b"c"),
        Err(DriverError::DirectoryNotEmpty)
    );
    fs.remove(other, b"inside").expect("empty c");
    fs.rename(top, b"b", root, b"c")
        .expect("a directory over an empty one");
    assert_eq!(fs.parent(inner), Ok(root));
    assert_eq!(fs.lookup(root, b"c"), Ok(inner));
    assert_eq!(
        fs.rename(root, b"nothing", root, b"x"),
        Err(DriverError::NotFound)
    );
    drop(check_fs(fs));
}

#[test]
fn a_volume_written_with_no_host_is_validated_after_every_change() {
    let mut fs = floppy(1);
    let root = fs.root();
    make(&mut fs, root, "one", b"1");
    let (data, bs) = image(fs);
    check(&data, bs);
}

#[test]
fn a_deferred_transaction_leaves_the_bitmap_invalid_until_its_commit() {
    let mut fs = floppy(1);
    let host = TestVolumeHost::leaked(1_000);
    fs.set_volume_host(TestVolumeHost::volume(), host);
    let root = fs.root();
    make(&mut fs, root, "deferred", &pattern(5000, 7));
    assert!(
        host.reported().is_some(),
        "the transaction's deadline is reported"
    );
    let on_disk = |fs: &Affs<MemDisk>| {
        let image = &fs.disk.dev.device().data;
        let root = 880 * 512;
        be32(&image[root..root + 512], Offsets::new(512).bm_flag())
    };
    assert_eq!(on_disk(&fs), 0, "marked invalid while open");
    fs.flush().expect("commit");
    assert_eq!(on_disk(&fs), BITMAP_VALID);
    assert_eq!(host.reported(), None, "nothing held after the commit");
    let (data, bs) = image(fs);
    check(&data, bs);
}

#[test]
fn an_unvalidated_volume_opens_read_only() {
    let mut fs = floppy(1);
    let host = TestVolumeHost::leaked(1_000);
    fs.set_volume_host(TestVolumeHost::volume(), host);
    let root = fs.root();
    make(&mut fs, root, "lost", b"never committed");
    // The process stops before the commit.
    let (data, _) = image(fs);
    let mut fs = Affs::open(MemDisk {
        data,
        block_size: 512,
    })
    .expect("opens");
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::NeedsCheck));
    let root = fs.root();
    assert_eq!(
        fs.create(root, b"x", NodeKind::RegularFile, &RECORD),
        Err(DriverError::ReadOnly)
    );
    assert!(fs.lookup(root, b"lost").is_ok(), "still readable");
}

#[test]
fn directory_cache_volumes_read_only_and_long_name_volumes_not_at_all() {
    let mut fs = floppy(1);
    let root = fs.root();
    make(&mut fs, root, "cached", b"payload");
    let (mut data, _) = image(fs);
    data[3] = 5;
    let mut fs = Affs::open(MemDisk {
        data: data.clone(),
        block_size: 512,
    })
    .expect("opens");
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::UnsupportedFeature));
    let root = fs.root();
    let file = fs.lookup(root, b"cached").expect("found");
    assert_eq!(read_all(&mut fs, file), b"payload");
    assert_eq!(fs.remove(root, b"cached"), Err(DriverError::ReadOnly));
    data[3] = 7;
    assert_eq!(
        Affs::open(MemDisk {
            data,
            block_size: 512
        })
        .err(),
        Some(DriverError::Unsupported)
    );
}

#[test]
fn hard_links_share_a_file_and_the_newest_takes_its_place_when_it_goes() {
    let mut fs = floppy(1);
    let root = fs.root();
    let dir = mkdir(&mut fs, root, "dir");
    let file = make(&mut fs, root, "real", b"shared bytes");
    fs.link(dir, b"alias", file).expect("link");
    fs.link(root, b"second", file).expect("another");
    assert_eq!(fs.node_info(file).expect("info").nlink, 3);
    assert_eq!(fs.lookup(dir, b"alias"), Ok(file), "a link is its file");
    fs.write_all(file, 0, b"SHARED").expect("write");
    let via = fs.lookup(dir, b"alias").expect("alias");
    assert_eq!(read_all(&mut fs, via), b"SHARED bytes");
    // The real name goes: the newest link takes over, under the same id.
    fs.remove(root, b"real").expect("remove the original");
    assert_eq!(fs.lookup(root, b"second"), Ok(file));
    assert_eq!(fs.node_info(file).expect("info").nlink, 2);
    fs.remove(dir, b"alias").expect("remove a link");
    assert_eq!(fs.node_info(file).expect("info").nlink, 1);
    assert_eq!(names(&mut fs, root), ["dir", "second"]);
    assert_eq!(
        fs.link(root, b"dirlink", dir),
        Err(DriverError::Unsupported),
        "a directory takes no hard link"
    );
    let mut fs = check_fs(fs);
    let root = fs.root();
    let file = fs.lookup(root, b"second").expect("kept");
    assert_eq!(read_all(&mut fs, file), b"SHARED bytes");
}

#[test]
fn soft_links_read_in_posix_spelling() {
    let mut fs = floppy(1);
    let root = fs.root();
    for (target, stored) in [
        ("a/b", &b"a/b"[..]),
        ("/top/x", b":top/x"),
        ("../up", b"/up"),
        ("a/../b", b"a//b"),
        ("../../far", b"//far"),
    ] {
        let name = alloc::format!("link{}", target.len());
        let link = fs
            .create_link(root, name.as_bytes(), target.as_bytes(), &RECORD)
            .expect("link");
        let header = fs.node(link).expect("header");
        assert_eq!(header.link_target(), stored, "{target} stored");
        let mut out = [0u8; 64];
        let len = fs.read_link(link, &mut out).expect("read");
        assert_eq!(&out[..len], target.as_bytes());
        assert_eq!(fs.node_info(link).expect("info").kind, NodeKind::Symlink);
        fs.remove(root, name.as_bytes()).expect("unlink");
    }
    assert_eq!(
        fs.create_link(root, b"bad", "SYS:c".as_bytes(), &RECORD),
        Err(DriverError::OutOfRange),
        "a colon names a volume"
    );
}

/// Turn the entry `name` of `dir` into a link of `sec_type` naming `real` or
/// storing `target`, sealing it as AmigaDOS would have written it.
fn forge_link(
    fs: Affs<MemDisk>,
    name: &[u8],
    sec_type: u32,
    real: u64,
    target: &[u8],
) -> Affs<MemDisk> {
    let mut fs = fs;
    let root = fs.root();
    let id = fs
        .create(root, name, NodeKind::RegularFile, &RECORD)
        .expect("placeholder");
    let (mut data, bs) = image(fs);
    let at = Offsets::new(bs);
    let block = &mut data[id.raw() as usize * bs..(id.raw() as usize + 1) * bs];
    put_be32(block, at.sec_type(), sec_type);
    put_be32(block, at.real_entry(), crate::header::low32(real));
    block[24..24 + target.len()].copy_from_slice(target);
    put_be32(block, 20, 0);
    let sum = tairix_fsprobe::affs::checksum(block, 20);
    put_be32(block, 20, sum);
    Affs::open(MemDisk {
        data,
        block_size: 512,
    })
    .expect("reopens")
}

#[test]
fn a_volume_named_soft_link_and_a_directory_link_resolve_within_the_volume() {
    let mut fs = floppy(1);
    let root = fs.root();
    let docs = mkdir(&mut fs, root, "Docs");
    let deep = mkdir(&mut fs, docs, "Deep");
    let fs = forge_link(fs, b"here", ST_SOFTLINK, 0, b"Work:Docs/Deep");
    let fs = forge_link(fs, b"there", ST_SOFTLINK, 0, b"SYS:C/Dir");
    let mut fs = forge_link(fs, b"dirlink", ST_LINKDIR, deep.raw(), b"");
    let root = fs.root();
    let mut out = [0u8; 64];
    for (name, expected) in [
        (&b"here"[..], &b"/Docs/Deep"[..]),
        (b"there", b"SYS:C/Dir"),
        (b"dirlink", b"/Docs/Deep"),
    ] {
        let link = fs.lookup(root, name).expect("found");
        assert_eq!(fs.node_info(link).expect("info").kind, NodeKind::Symlink);
        let len = fs.read_link(link, &mut out).expect("read");
        assert_eq!(&out[..len], expected);
    }
}

#[test]
fn a_held_file_outlives_its_name_and_is_freed_at_its_last_unpin() {
    let mut fs = floppy(1);
    let root = fs.root();
    let before = fs.stats().expect("stats").free_blocks;
    let file = make(&mut fs, root, "held", &pattern(3000, 3));
    assert_eq!(fs.pin(file), Ok(file));
    assert_eq!(fs.pin(file), Ok(file));
    fs.remove(root, b"held").expect("unlink");
    assert!(fs.lookup(root, b"held").is_err());
    assert_eq!(fs.node_info(file).expect("info").nlink, 0);
    assert_eq!(read_all(&mut fs, file), pattern(3000, 3));
    fs.write_all(file, 0, b"still writable").expect("write");
    assert_eq!(fs.unpin(file), Ok(Unpinned::Held));
    assert_eq!(fs.unpin(file), Ok(Unpinned::Freed));
    assert_eq!(fs.unpin(file), Err(DriverError::NotFound));
    assert_eq!(fs.stats().expect("stats").free_blocks, before);
    drop(check_fs(fs));
}

#[test]
fn a_held_directory_that_loses_its_name_takes_no_entries() {
    let mut fs = floppy(1);
    let root = fs.root();
    let dir = mkdir(&mut fs, root, "gone");
    fs.pin(dir).expect("pin");
    fs.remove(root, b"gone").expect("rmdir");
    assert_eq!(fs.lookup(dir, b"x"), Err(DriverError::NotFound));
    assert_eq!(fs.parent(dir), Err(DriverError::NotFound));
    assert_eq!(
        fs.create(dir, b"x", NodeKind::RegularFile, &RECORD),
        Err(DriverError::NotFound)
    );
    assert!(listed(&mut fs, dir, 0, b"").expect("lists").is_empty());
    fs.release_pins().expect("release");
    assert_eq!(fs.node_info(dir), Err(DriverError::NotFound));
    drop(check_fs(fs));
}

#[test]
fn protection_and_comments_are_amiga_attributes() {
    let mut fs = floppy(1);
    let root = fs.root();
    let file = make(&mut fs, root, "script", b"echo");
    let mut out = [0u8; 80];
    let len = fs
        .get_attr(file, b"amiga.protection", &mut out)
        .expect("get")
        .expect("carried");
    assert_eq!(&out[..len], b"--------");
    fs.set_attr(file, b"amiga.protection", b"-s--rwed")
        .expect("set");
    let len = fs
        .get_attr(file, b"amiga.protection", &mut out)
        .expect("get")
        .expect("carried");
    assert_eq!(&out[..len], b"-s--rwed");
    assert_eq!(fs.get_attr(file, b"amiga.comment", &mut out), Ok(None));
    fs.set_attr(file, b"amiga.comment", b"Startup")
        .expect("comment");
    let mut keys = Vec::new();
    let mut key = [0u8; 64];
    while let Some(len) = fs
        .list_attr(file, keys.len() as u64, &mut key)
        .expect("list")
    {
        keys.push(key[..len].to_vec());
    }
    assert_eq!(
        keys,
        [b"amiga.protection".to_vec(), b"amiga.comment".to_vec()]
    );
    assert_eq!(
        fs.set_attr(file, b"amiga.comment", &[b'x'; 80]),
        Err(DriverError::LengthOutOfRange)
    );
    assert_eq!(
        fs.set_attr(file, b"acorn.filetype", b"fff"),
        Err(DriverError::Unsupported)
    );
    assert_eq!(
        fs.remove_attr(file, b"amiga.protection"),
        Err(DriverError::Unsupported)
    );
    fs.remove_attr(file, b"amiga.comment")
        .expect("drop the comment");
    assert_eq!(fs.get_attr(file, b"amiga.comment", &mut out), Ok(None));
    assert_eq!(fs.get_attr(root, b"amiga.protection", &mut out), Ok(None));
    assert_eq!(
        fs.set_attr(file, b"no namespace", b"x"),
        Err(DriverError::OutOfRange)
    );
    let mut fs = check_fs(fs);
    let root = fs.root();
    let file = fs.lookup(root, b"script").expect("found");
    let len = fs
        .get_attr(file, b"amiga.protection", &mut out)
        .expect("get")
        .expect("kept");
    assert_eq!(&out[..len], b"-s--rwed");
}

#[test]
fn an_object_is_stamped_with_the_hosts_wall_clock_to_the_tick() {
    let mut fs = floppy(1);
    let when = Time64::new(1_700_000_000, 123_456_789).expect("a time");
    fs.set_volume_host(TestClock::volume(), TestClock::at(Some(when)));
    let root = fs.root();
    let file = make(&mut fs, root, "dated", b"x");
    let times = fs.node_info(file).expect("info").times;
    assert_eq!(times.modified.secs(), 1_700_000_000);
    assert_eq!(times.modified.subsec_nanos(), 120_000_000, "whole ticks");
    assert_eq!(times.created, times.modified);
    let root_times = fs.node_info(root).expect("info").times;
    assert_eq!(
        root_times.created,
        tairix_fsmeta::preset::amiga::datestamp_to_time64(9000, 720, 10).expect("a stamp")
    );
    let mut unknown = floppy(1);
    unknown.set_volume_host(TestClock::volume(), TestClock::at(None));
    let root = unknown.root();
    let file = make(&mut unknown, root, "undated", b"x");
    assert_eq!(
        unknown.node_info(file).expect("info").times.modified,
        Time64::UNIX_EPOCH
    );
}

#[test]
fn a_full_volume_refuses_the_write_and_stays_consistent() {
    let mut fs = floppy(1);
    let root = fs.root();
    let free = fs.stats().expect("stats").free_blocks;
    let file = make(&mut fs, root, "filler", &[]);
    let too_much = vec![7u8; (free as usize + 10) * 512];
    assert_eq!(fs.write_at(file, 0, &too_much), Err(DriverError::NoSpace));
    assert_eq!(
        fs.node_info(file).expect("info").size,
        0,
        "nothing half-written"
    );
    let fits = vec![7u8; (free as usize - 40) * 512];
    fs.write_all(file, 0, &fits).expect("what fits");
    drop(check_fs(fs));
}

#[test]
fn security_is_unrecorded_and_unsettable() {
    let mut fs = floppy(1);
    let root = fs.root();
    let file = make(&mut fs, root, "f", b"");
    assert_eq!(
        fs.security(file),
        Ok(NodeSecurity::unrecorded(NodeKind::RegularFile))
    );
    assert_eq!(fs.records_ownership(file), Ok(false));
    assert_eq!(fs.set_security(file, RECORD), Err(DriverError::Unsupported));
}

/// Flip `byte` of block `block` of `fs`'s image and reopen it, the damage
/// left unsealed.
fn damaged(fs: Affs<MemDisk>, block: u64, byte: usize) -> Affs<MemDisk> {
    let (mut data, bs) = image(fs);
    data[block as usize * bs + byte] ^= 0x01;
    Affs::open(MemDisk {
        data,
        block_size: 512,
    })
    .expect("the root still opens")
}

#[test]
fn damaged_structures_are_refused_never_followed() {
    // A header whose checksum fails.
    let mut fs = floppy(0);
    let root = fs.root();
    let file = make(&mut fs, root, "victim", &pattern(2000, 1));
    let mut fs = damaged(fs, file.raw(), 100);
    let root = fs.root();
    assert_eq!(fs.lookup(root, b"victim"), Err(DriverError::BadMagic));
    assert_eq!(
        listed(&mut fs, root, 0, b"").err(),
        Some(DriverError::BadMagic)
    );
    // An OFS data block out of sequence.
    let mut fs = floppy(0);
    let root = fs.root();
    let file = make(&mut fs, root, "data", &pattern(2000, 1));
    let first = {
        let header = fs.node(file).expect("header");
        u64::from(header.table(Offsets::new(512).table_len() - 1))
    };
    let (mut data, _) = image(fs);
    let block = &mut data[first as usize * 512..(first as usize + 1) * 512];
    put_be32(block, 8, 9);
    put_be32(block, 20, 0);
    let sum = tairix_fsprobe::affs::checksum(block, 20);
    put_be32(block, 20, sum);
    let mut fs = Affs::open(MemDisk {
        data,
        block_size: 512,
    })
    .expect("opens");
    let mut out = [0u8; 100];
    assert_eq!(fs.read_at(file, 0, &mut out), Err(DriverError::BadMagic));
}

#[test]
fn a_hash_chain_that_loops_ends_the_walk_as_damage() {
    let mut fs = floppy(1);
    let root = fs.root();
    let first = make(&mut fs, root, "a", b"");
    let (mut data, bs) = image(fs);
    // The entry's chain names itself.
    let block = &mut data[first.raw() as usize * bs..(first.raw() as usize + 1) * bs];
    put_be32(
        block,
        Offsets::new(bs).hash_chain(),
        crate::header::low32(first.raw()),
    );
    put_be32(block, 20, 0);
    let sum = tairix_fsprobe::affs::checksum(block, 20);
    put_be32(block, 20, sum);
    let mut fs = Affs::open(MemDisk {
        data,
        block_size: 512,
    })
    .expect("opens");
    let root = fs.root();
    assert_eq!(
        listed(&mut fs, root, 0, b"").err(),
        Some(DriverError::BadMagic)
    );
    let bucket = |name: &str| {
        crate::name::Name::encode(name.as_bytes())
            .expect("a name")
            .bucket(
                NameMatching::AsciiCaseInsensitive,
                Offsets::new(512).table_len(),
            )
    };
    // A name filed in the same bucket walks the looping chain to its end.
    let twin = (0..10_000)
        .map(|n| alloc::format!("t{n}"))
        .find(|name| bucket(name) == bucket("a"))
        .expect("a name sharing the bucket");
    assert_eq!(fs.lookup(root, twin.as_bytes()), Err(DriverError::BadMagic));
}

#[test]
fn a_damaged_bitmap_block_stops_the_allocation() {
    let mut fs = floppy(1);
    let root = fs.root();
    make(&mut fs, root, "a", b"x");
    let bitmap = u64::from(
        fs.node(root)
            .expect("root")
            .long_at(Offsets::new(512).bm_pages()),
    );
    let mut fs = damaged(fs, bitmap, 200);
    let root = fs.root();
    assert_eq!(
        fs.create(root, b"b", NodeKind::RegularFile, &RECORD),
        Err(DriverError::BadMagic)
    );
    assert_eq!(
        fs.write_refusal(),
        Some(WriteRefusal::Damaged),
        "no more writes"
    );
}

#[test]
fn a_model_of_random_changes_is_kept_and_checks_out() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "affs-model",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    for dos_type in [0, 3] {
        let mut fs = floppy(dos_type);
        let root = fs.root();
        let mut model: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for step in 0..400u32 {
            let name = alloc::format!("f{}", rng.below(24));
            match rng.below(6) {
                0 | 1 => {
                    let offset = rng.below(30_000);
                    let len = rng.below(9_000);
                    let data = pattern(len, step as u8);
                    let file = match fs.lookup(root, name.as_bytes()) {
                        Ok(file) => file,
                        Err(_) => make(&mut fs, root, &name, &[]),
                    };
                    let entry = model.entry(name.clone()).or_default();
                    match fs.write_all(file, offset as u64, &data) {
                        Ok(()) => {
                            if entry.len() < offset + len {
                                entry.resize(offset + len, 0);
                            }
                            entry[offset..offset + len].copy_from_slice(&data);
                        }
                        Err(DriverError::NoSpace) => {}
                        Err(err) => panic!("step {step}: {err:?}"),
                    }
                }
                2 => {
                    if let Ok(file) = fs.lookup(root, name.as_bytes()) {
                        let size = rng.below(40_000);
                        if fs.truncate(file, size as u64).is_ok() {
                            model.entry(name.clone()).or_default().resize(size, 0);
                        }
                    }
                }
                3 => {
                    let removed = fs.remove(root, name.as_bytes());
                    assert_eq!(
                        removed.is_ok(),
                        model.remove(&name).is_some(),
                        "step {step}"
                    );
                }
                4 => {
                    let to = alloc::format!("f{}", rng.below(24));
                    if fs
                        .rename(root, name.as_bytes(), root, to.as_bytes())
                        .is_ok()
                        && to != name
                    {
                        let moved = model.remove(&name).expect("renamed what the model has");
                        model.insert(to, moved);
                    }
                }
                _ => {
                    if model.contains_key(&name) {
                        let file = fs.lookup(root, name.as_bytes()).expect("in the model");
                        assert_eq!(read_all(&mut fs, file), model[&name], "step {step}: {name}");
                    }
                }
            }
        }
        let mut names_now = names(&mut fs, root);
        names_now.sort();
        assert_eq!(names_now, model.keys().cloned().collect::<Vec<_>>());
        let mut fs = check_fs(fs);
        let root = fs.root();
        for (name, data) in &model {
            let file = fs.lookup(root, name.as_bytes()).expect("kept");
            assert_eq!(&read_all(&mut fs, file), data, "{name} after a reopen");
        }
    }
}

/// What a [`Recorded`] device was asked to do.
#[derive(Clone, Debug, PartialEq)]
enum Op {
    Read(u64),
    Write(u64, Vec<u8>),
    Flush,
}

/// A device that records every read, write and flush asked of it.
struct Recorded {
    disk: MemDisk,
    log: Vec<Op>,
}

impl Recorded {
    /// `fs`'s volume, reopened over a recording device.
    fn over(fs: Affs<MemDisk>) -> Affs<Self> {
        let (data, _) = image(fs);
        Affs::open(Self {
            disk: MemDisk {
                data,
                block_size: 512,
            },
            log: Vec::new(),
        })
        .expect("reopens")
    }
}

impl Block for Recorded {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        self.disk.geometry()
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.log.push(Op::Read(lba));
        self.disk.read_blocks(lba, buf)
    }

    fn write_blocks(&mut self, lba: u64, data: &[u8]) -> Result<(), DriverError> {
        self.log.push(Op::Write(lba, data.to_vec()));
        self.disk.write_blocks(lba, data)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.log.push(Op::Flush);
        self.disk.flush()
    }
}

/// The ops logged since `fs`'s log was last taken.
fn logged(fs: &mut Affs<Recorded>) -> Vec<Op> {
    core::mem::take(&mut fs.disk.dev.device_mut().log)
}

/// A one-character name every volume files in the bucket `"3"` is in.
const BUCKET_64: [&str; 3] = ["3", "{", "\u{C3}"];

#[test]
fn removing_a_hard_link_filed_after_its_file_keeps_their_bucket_whole() {
    let bucket = |name: &str| {
        crate::name::Name::encode(name.as_bytes())
            .expect("a name")
            .bucket(
                NameMatching::AsciiCaseInsensitive,
                Offsets::new(512).table_len(),
            )
    };
    assert!(BUCKET_64.iter().all(|name| bucket(name) == bucket("3")));
    let mut fs = floppy(1);
    let root = fs.root();
    let file = make(&mut fs, root, BUCKET_64[0], b"real");
    fs.link(root, BUCKET_64[1].as_bytes(), file).expect("link");
    assert!(
        fs.node(file).expect("header").hash_chain() != 0,
        "the link is filed after its file in one chain"
    );
    fs.remove(root, BUCKET_64[1].as_bytes())
        .expect("unlink the link");
    assert_eq!(names(&mut fs, root), [BUCKET_64[0]]);
    assert_eq!(
        fs.lookup(root, BUCKET_64[2].as_bytes()),
        Err(DriverError::NotFound),
        "the bucket's chain ends where it should"
    );
    assert_eq!(fs.node_info(file).expect("info").nlink, 1);
    drop(check_fs(fs));
}

/// Turn the entry `name`, created in `dir`, into a hard link to the directory
/// `real` heading its chain of links, sealed as AmigaDOS would have written
/// both.
fn forge_dir_link(
    fs: Affs<MemDisk>,
    dir: NodeId,
    name: &[u8],
    real: NodeId,
) -> (Affs<MemDisk>, NodeId) {
    let mut fs = fs;
    let id = fs
        .create(dir, name, NodeKind::RegularFile, &RECORD)
        .expect("placeholder");
    let (mut data, bs) = image(fs);
    let at = Offsets::new(bs);
    let seal = |block: &mut [u8]| {
        put_be32(block, 20, 0);
        let sum = tairix_fsprobe::affs::checksum(block, 20);
        put_be32(block, 20, sum);
    };
    let link = &mut data[id.raw() as usize * bs..(id.raw() as usize + 1) * bs];
    put_be32(link, at.sec_type(), ST_LINKDIR);
    put_be32(link, at.real_entry(), crate::header::low32(real.raw()));
    seal(link);
    let target = &mut data[real.raw() as usize * bs..(real.raw() as usize + 1) * bs];
    put_be32(target, at.next_link(), crate::header::low32(id.raw()));
    seal(target);
    let fs = Affs::open(MemDisk {
        data,
        block_size: 512,
    })
    .expect("reopens");
    (fs, id)
}

#[test]
fn a_directory_link_filed_in_the_directory_it_names_is_removed_cleanly() {
    let mut fs = floppy(1);
    let root = fs.root();
    let dir = mkdir(&mut fs, root, "Loop");
    let (mut fs, link) = forge_dir_link(fs, dir, b"self", dir);
    assert_eq!(fs.node_info(link).expect("info").kind, NodeKind::Symlink);
    fs.remove(dir, b"self").expect("unlink the link");
    assert!(names(&mut fs, dir).is_empty());
    assert_eq!(fs.node(dir).expect("header").next_link(), 0, "no link left");
    drop(check_fs(fs));
}

#[test]
fn a_held_directory_link_keeps_its_block_until_its_last_unpin() {
    let mut fs = floppy(1);
    let root = fs.root();
    let deep = mkdir(&mut fs, root, "Deep");
    let (mut fs, link) = forge_dir_link(fs, root, b"Link", deep);
    fs.pin(link)
        .expect("pin the link, as a NO_FOLLOW open does");
    fs.remove(root, b"Link").expect("unlink");
    assert_eq!(fs.node_info(link).expect("still answers").nlink, 0);
    for n in 0..8 {
        let made = make(&mut fs, root, &alloc::format!("new{n}"), b"x");
        assert_ne!(made, link, "a held node's block is given to no other");
    }
    assert_eq!(fs.unpin(link), Ok(Unpinned::Freed));
    drop(check_fs(fs));
}

#[test]
fn a_held_newest_link_of_a_promoted_directory_is_freed_at_its_last_unpin() {
    let mut fs = floppy(1);
    let root = fs.root();
    let real = mkdir(&mut fs, root, "Real");
    let elsewhere = mkdir(&mut fs, root, "Elsewhere");
    let (mut fs, link) = forge_dir_link(fs, elsewhere, b"Alias", real);
    fs.pin(link).expect("pin");
    fs.remove(root, b"Real")
        .expect("its own name goes; the link takes its place");
    assert_eq!(
        fs.lookup(elsewhere, b"Alias"),
        Ok(real),
        "promoted under its id"
    );
    assert_eq!(fs.node_info(link).expect("still answers").nlink, 0);
    let made = make(&mut fs, root, "after", b"x");
    assert_ne!(made, link);
    assert_eq!(fs.unpin(link), Ok(Unpinned::Freed));
    drop(check_fs(fs));
}

#[test]
fn the_root_is_marked_valid_only_after_a_flush_behind_the_rest_of_the_commit() {
    let mut fs = Recorded::over(floppy(1));
    logged(&mut fs);
    let root = fs.root();
    let file = fs
        .create(root, b"ordered", NodeKind::RegularFile, &RECORD)
        .expect("create");
    fs.write_all(file, 0, &pattern(9000, 2)).expect("write");
    let log = logged(&mut fs);
    let bm_flag = Offsets::new(512).bm_flag();
    let valid_at = log
        .iter()
        .rposition(
            |op| matches!(op, Op::Write(880, data) if be32(&data[..512], bm_flag) == BITMAP_VALID),
        )
        .expect("the commit marks the root valid");
    let ahead = log[..valid_at]
        .iter()
        .rfind(|op| !matches!(op, Op::Read(_)));
    assert_eq!(ahead, Some(&Op::Flush), "a barrier ahead of the root");
    assert_eq!(log.last(), Some(&Op::Flush), "and one behind it");
    let opened_at = log
        .iter()
        .rposition(|op| matches!(op, Op::Write(880, data) if be32(&data[..512], bm_flag) == 0))
        .expect("the transaction marked the root invalid");
    assert_eq!(
        log[opened_at + 1],
        Op::Flush,
        "before anything else is written"
    );
    assert!(
        log[opened_at + 1..valid_at].iter().all(
            |op| !matches!(op, Op::Write(880, data) if be32(&data[..512], bm_flag) == BITMAP_VALID)
        ),
        "nothing in between says valid"
    );
}

#[test]
fn a_new_extension_block_is_written_before_anything_points_at_it() {
    let mut fs = Recorded::over(floppy(1));
    let root = fs.root();
    let file = fs
        .create(root, b"long", NodeKind::RegularFile, &RECORD)
        .expect("create");
    logged(&mut fs);
    // Past the header's table and the first extension's.
    fs.write_all(file, 0, &pattern(160 * 512, 4))
        .expect("write");
    let log = logged(&mut fs);
    let header = fs.node(file).expect("header");
    let first = u64::from(header.extension());
    let second = {
        let mut buf = vec![0u8; 512];
        fs.disk.read(first, &mut buf).expect("read");
        u64::from(be32(&buf, Offsets::new(512).extension()))
    };
    assert!(first != 0 && second != 0, "two extension blocks");
    let written = |block: u64| {
        log.iter()
            .position(|op| matches!(op, Op::Write(lba, _) if *lba == block))
            .expect("written")
    };
    let linked = log
        .iter()
        .position(|op| {
            matches!(op, Op::Write(lba, data)
                if *lba == first && u64::from(be32(data, Offsets::new(512).extension())) == second)
        })
        .expect("the first links the second");
    assert!(
        written(second) < linked,
        "the target is on the medium first"
    );
    let header_written = log
        .iter()
        .rposition(|op| matches!(op, Op::Write(lba, _) if *lba == file.raw()))
        .expect("the header is written");
    assert!(written(first) < header_written, "and so is the header's");
    assert_eq!(read_all_recorded(&mut fs, file), pattern(160 * 512, 4));
}

fn read_all_recorded(fs: &mut Affs<Recorded>, file: NodeId) -> Vec<u8> {
    let size = fs.node_info(file).expect("info").size as usize;
    let mut out = vec![0; size];
    let mut at = 0;
    while at < size {
        let got = fs.read_at(file, at as u64, &mut out[at..]).expect("read");
        assert!(got > 0);
        at += got;
    }
    out
}

#[test]
fn a_refused_or_idle_change_writes_nothing_and_opens_no_transaction() {
    let mut fs = Recorded::over(floppy(1));
    let host = TestVolumeHost::leaked(1_000);
    fs.set_volume_host(TestVolumeHost::volume(), host);
    let root = fs.root();
    let file = fs
        .create(root, b"kept", NodeKind::RegularFile, &RECORD)
        .expect("create");
    fs.write_all(file, 0, b"twelve bytes").expect("write");
    fs.flush().expect("commit");
    assert_eq!(host.reported(), None);
    logged(&mut fs);
    let free = fs.stats().expect("stats").free_blocks as usize;
    let refusals = [
        fs.truncate(file, 12),
        fs.create(root, b"KEPT", NodeKind::RegularFile, &RECORD)
            .map(drop),
        fs.remove(root, b"absent"),
        fs.write_at(file, 0, &vec![1u8; (free + 4) * 512]).map(drop),
        fs.rename(root, b"kept", root, b"kept"),
    ];
    assert_eq!(
        refusals,
        [
            Ok(()),
            Err(DriverError::AlreadyExists),
            Err(DriverError::NotFound),
            Err(DriverError::NoSpace),
            Ok(()),
        ]
    );
    let log = logged(&mut fs);
    assert!(
        log.iter().all(|op| matches!(op, Op::Read(_))),
        "nothing written or flushed: {log:?}"
    );
    assert_eq!(host.reported(), None, "no transaction left open");
}

#[test]
fn an_orphan_keeps_the_bitmap_unvalidated_until_its_last_unpin() {
    let mut fs = floppy(1);
    let root = fs.root();
    let file = make(&mut fs, root, "orphan", &pattern(4000, 9));
    fs.pin(file).expect("pin");
    fs.remove(root, b"orphan").expect("unlink");
    let bm_flag = |fs: &Affs<MemDisk>| {
        let image = &fs.disk.dev.device().data;
        be32(&image[880 * 512..881 * 512], Offsets::new(512).bm_flag())
    };
    assert_eq!(
        bm_flag(&fs),
        0,
        "blocks no name reaches: a crash is revalidated"
    );
    // The process stops before the unpin: AmigaDOS validates it, reclaiming them.
    let (data, _) = image(fs);
    let reopened = Affs::open(MemDisk {
        data,
        block_size: 512,
    })
    .expect("opens");
    assert_eq!(reopened.write_refusal(), Some(WriteRefusal::NeedsCheck));
    let mut fs = floppy(1);
    let root = fs.root();
    let file = make(&mut fs, root, "orphan", &pattern(4000, 9));
    fs.pin(file).expect("pin");
    fs.remove(root, b"orphan").expect("unlink");
    assert_eq!(fs.unpin(file), Ok(Unpinned::Freed));
    assert_eq!(
        bm_flag(&fs),
        BITMAP_VALID,
        "valid again once they are freed"
    );
    drop(check_fs(fs));
}

#[test]
fn a_growing_file_takes_the_block_after_its_last_not_a_gap_behind_it() {
    let mut fs = floppy(1);
    let root = fs.root();
    let file = make(&mut fs, root, "grows", &[1u8; 512]);
    make(&mut fs, root, "between", &[2u8; 512]);
    fs.write_all(file, 512, &[3u8; 512])
        .expect("a second block");
    fs.remove(root, b"between")
        .expect("leaves a gap behind the file's end");
    let last = |fs: &mut Affs<MemDisk>| {
        let header = fs.node(file).expect("header");
        let blocks = header.high_seq() as usize;
        u64::from(header.table(Offsets::new(512).table_len() - blocks))
    };
    let before = last(&mut fs);
    fs.write_all(file, 1024, &[4u8; 512])
        .expect("a third block");
    assert_eq!(last(&mut fs), before + 1, "grown on from its end");
    drop(check_fs(fs));
}

#[test]
fn a_sequential_read_reads_its_data_and_not_its_metadata_again() {
    let mut fs = floppy(1);
    let root = fs.root();
    let body = pattern(128 * 512, 6);
    let file = make(&mut fs, root, "stream", &body);
    let mut fs = Recorded::over(fs);
    logged(&mut fs);
    let mut out = vec![0u8; body.len()];
    let mut calls = 0;
    for chunk in out.chunks_mut(4096) {
        let at = calls * 4096;
        assert_eq!(fs.read_at(file, at as u64, chunk), Ok(chunk.len()));
        calls += 1;
    }
    assert_eq!(out, body);
    let reads = logged(&mut fs)
        .iter()
        .filter(|op| matches!(op, Op::Read(_)))
        .count();
    // Each call's run of data, the header and the extension once, and the one
    // run the extension block splits.
    assert!(reads <= calls + 3, "{reads} reads for {calls} calls");
}

#[test]
fn a_listing_that_stops_early_resolves_only_what_it_handed_over() {
    let mut fs = floppy(1);
    let root = fs.root();
    let real = make(&mut fs, root, "target", b"x");
    let bucket = |name: &str| {
        crate::name::Name::encode(name.as_bytes())
            .expect("a name")
            .bucket(
                NameMatching::AsciiCaseInsensitive,
                Offsets::new(512).table_len(),
            )
    };
    let wanted = bucket("l0");
    let links: Vec<String> = (0..2000)
        .map(|n| alloc::format!("l{n}"))
        .filter(|name| bucket(name) == wanted)
        .take(12)
        .collect();
    for name in &links {
        fs.link(root, name.as_bytes(), real).expect("link");
    }
    let mut fs = Recorded::over(fs);
    logged(&mut fs);
    let cursor = (wanted as u64) << 32;
    let mut handed = 0;
    let root = fs.root();
    fs.read_dir(root, cursor, b"", &mut |_, _| {
        handed += 1;
        DirVisit::Stop
    })
    .expect("lists");
    assert_eq!(handed, 1);
    let reads = logged(&mut fs)
        .iter()
        .filter(|op| matches!(op, Op::Read(_)))
        .count();
    // The directory, its bucket's chain once, and the one entry handed over
    // with the file it names and that file's chain of links, which its count
    // of names is.
    assert!(reads <= 2 + 2 * (links.len() + 1), "{reads} reads");
}

/// The writes `fs` logged since its log was last taken, in order.
fn writes_logged(fs: &mut Affs<Recorded>) -> Vec<(u64, Vec<u8>)> {
    logged(fs)
        .into_iter()
        .filter_map(|op| match op {
            Op::Write(lba, data) => Some((lba, data)),
            Op::Read(_) | Op::Flush => None,
        })
        .collect()
}

/// Grow or cut `f`, `before` bytes on a fresh floppy, to `after`, then
/// replay every prefix of the change's writes onto the volume as it was: a
/// device that took only those writes reopens and reads `f` as it was or as
/// it became, never as damage.
fn every_cut_reads_old_or_new(dos_type: u8, before: usize, after: usize) {
    let mut fs = floppy(dos_type);
    let root = fs.root();
    let old = pattern(before, 3);
    make(&mut fs, root, "f", &old);
    let (start, _) = image(fs);
    let mut fs = Recorded::over(
        Affs::open(MemDisk {
            data: start.clone(),
            block_size: 512,
        })
        .expect("reopens"),
    );
    let root = fs.root();
    let file = fs.lookup(root, b"f").expect("f");
    logged(&mut fs);
    let new = if after > before {
        let tail = pattern(after - before, 7);
        fs.write_all(file, before as u64, &tail).expect("grows");
        [old.as_slice(), tail.as_slice()].concat()
    } else {
        fs.truncate(file, after as u64).expect("cuts");
        old[..after].to_vec()
    };
    fs.flush().expect("commits");
    let writes = writes_logged(&mut fs);
    assert!(writes.len() > 3, "the change takes several writes");
    for cut in 0..=writes.len() {
        let mut data = start.clone();
        for (lba, bytes) in &writes[..cut] {
            let at = *lba as usize * 512;
            data[at..at + bytes.len()].copy_from_slice(bytes);
        }
        let mut fs = Affs::open(MemDisk {
            data,
            block_size: 512,
        })
        .unwrap_or_else(|err| panic!("cut after {cut} writes: {err:?}"));
        let root = fs.root();
        let file = fs.lookup(root, b"f").expect("still named");
        let size = fs.node_info(file).expect("info").size as usize;
        let mut read = vec![0; size];
        let mut at = 0;
        while at < size {
            let got = fs
                .read_at(file, at as u64, &mut read[at..])
                .unwrap_or_else(|err| {
                    panic!("cut after {cut} of {} writes: {err:?}", writes.len())
                });
            assert!(got > 0, "cut after {cut}: short read at {at}");
            at += got;
        }
        assert!(
            read == old || read == new,
            "cut after {cut} of {} writes reads {} bytes, neither the old file nor the new",
            writes.len(),
            read.len()
        );
    }
}

#[test]
fn every_state_a_growth_cut_short_leaves_reads_the_file_as_it_was_or_became() {
    // Past the header's 72 pointers into a second extension block.
    every_cut_reads_old_or_new(1, 100 * 512, 200 * 512 + 100);
    every_cut_reads_old_or_new(0, 100 * 488 + 7, 200 * 488);
}

#[test]
fn every_state_a_cut_left_part_way_reads_the_file_as_it_was_or_became() {
    // Into the header's table, and into the first extension block.
    every_cut_reads_old_or_new(1, 200 * 512, 50 * 512 + 5);
    every_cut_reads_old_or_new(1, 200 * 512, 80 * 512);
    every_cut_reads_old_or_new(0, 200 * 488 + 9, 80 * 488 + 3);
}

/// A sparse device that logs what it is asked and fails the write it is told
/// to, or every read of one block.
struct Faulty {
    blocks: u64,
    data: BTreeMap<u64, Vec<u8>>,
    log: Vec<Op>,
    writes: usize,
    fail_write: Option<(usize, DriverError)>,
    fail_read: Option<u64>,
}

impl Faulty {
    fn new(bytes: u64) -> Self {
        Self {
            blocks: bytes / 512,
            data: BTreeMap::new(),
            log: Vec::new(),
            writes: 0,
            fail_write: None,
            fail_read: None,
        }
    }

    /// Fail the `nth` write from now, counting from zero, with `err`.
    fn fail_write_in(&mut self, nth: usize, err: DriverError) {
        self.fail_write = Some((self.writes + nth, err));
    }

    fn wrote(&self) -> bool {
        self.log.iter().any(|op| matches!(op, Op::Write(..)))
    }

    fn flushed(&self) -> bool {
        self.log.iter().any(|op| matches!(op, Op::Flush))
    }
}

impl Block for Faulty {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: 512,
            block_count: self.blocks,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.log.push(Op::Read(lba));
        let count = (buf.len() / 512) as u64;
        if self
            .fail_read
            .is_some_and(|bad| (lba..lba + count).contains(&bad))
        {
            return Err(DriverError::DeviceFault);
        }
        if lba + count > self.blocks {
            return Err(DriverError::LengthOutOfRange);
        }
        for (at, chunk) in (lba..).zip(buf.chunks_mut(512)) {
            match self.data.get(&at) {
                Some(block) => chunk.copy_from_slice(block),
                None => chunk.fill(0),
            }
        }
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, data: &[u8]) -> Result<(), DriverError> {
        let nth = self.writes;
        self.writes += 1;
        if let Some((fail, err)) = self.fail_write {
            if fail == nth {
                return Err(err);
            }
        }
        self.log.push(Op::Write(lba, data.to_vec()));
        if lba + (data.len() / 512) as u64 > self.blocks {
            return Err(DriverError::LengthOutOfRange);
        }
        for (at, chunk) in (lba..).zip(data.chunks(512)) {
            self.data.insert(at, chunk.to_vec());
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.log.push(Op::Flush);
        Ok(())
    }
}

fn faulty(bytes: u64) -> Affs<Faulty> {
    Affs::format(
        Faulty::new(bytes),
        &FormatOptions {
            dos_type: 1,
            block_size: 512,
            name: b"Work",
            created: created(),
        },
    )
    .expect("formats")
}

fn faulty_device(fs: &mut Affs<Faulty>) -> &mut Faulty {
    fs.disk.dev.device_mut()
}

#[test]
fn a_change_whose_write_fails_part_way_is_never_committed_valid() {
    for err in [
        DriverError::NoSpace,
        DriverError::ReadOnly,
        DriverError::DeviceFault,
    ] {
        let mut fs = faulty(901_120);
        let root = fs.root();
        let file = fs
            .create(root, b"f", NodeKind::RegularFile, &RECORD)
            .expect("create");
        fs.write_all(file, 0, &pattern(10 * 512, 1)).expect("write");
        // The third write of the growth fails, after its first two landed.
        faulty_device(&mut fs).fail_write_in(2, err);
        assert_eq!(
            fs.write_all(file, 10 * 512, &pattern(100 * 512, 2)),
            Err(err)
        );
        assert_eq!(
            fs.write_refusal(),
            Some(WriteRefusal::Damaged),
            "{err:?} part-way is damage"
        );
        assert_eq!(
            fs.create(root, b"g", NodeKind::RegularFile, &RECORD),
            Err(DriverError::ReadOnly),
            "nothing more is written"
        );
        faulty_device(&mut fs).log.clear();
        assert_eq!(fs.flush(), Ok(()));
        let device = faulty_device(&mut fs);
        assert!(device.flushed(), "what the device took is forced out");
        assert!(!device.wrote(), "nothing is written to finish the change");
        let fs = Affs::open(fs.into_device()).expect("reopens");
        assert_eq!(
            fs.write_refusal(),
            Some(WriteRefusal::NeedsCheck),
            "the part-way change left the bitmap marked invalid"
        );
    }
}

#[test]
fn a_change_refused_before_it_writes_leaves_the_volume_writable() {
    let mut fs = faulty(901_120);
    let root = fs.root();
    let dir = mkdir_on(&mut fs, root, "d");
    faulty_device(&mut fs).fail_read = Some(dir.raw());
    assert_eq!(
        fs.create(dir, b"f", NodeKind::RegularFile, &RECORD),
        Err(DriverError::DeviceFault)
    );
    assert_eq!(fs.write_refusal(), None, "nothing was written");
    faulty_device(&mut fs).fail_read = None;
    let file = fs
        .create(dir, b"f", NodeKind::RegularFile, &RECORD)
        .expect("the next change goes ahead");
    fs.write_all(file, 0, b"kept").expect("write");
    fs.flush().expect("commits");
    let fs = Affs::open(fs.into_device()).expect("reopens");
    assert_eq!(fs.write_refusal(), None, "committed valid");
}

fn mkdir_on<B: Block>(fs: &mut Affs<B>, dir: NodeId, name: &str) -> NodeId {
    fs.create(dir, name.as_bytes(), NodeKind::Directory, &RECORD)
        .expect("mkdir")
}

#[test]
fn a_damaged_volume_writes_nothing_more_even_to_count_its_space() {
    // More bitmap pages than the cache holds, so counting the space evicts.
    let mut fs = faulty(256 * 1024 * 1024);
    let root = fs.root();
    let file = fs
        .create(root, b"f", NodeKind::RegularFile, &RECORD)
        .expect("create");
    fs.write_all(file, 0, &pattern(100 * 512, 4))
        .expect("write");
    fs.flush().expect("commits");
    let extension = u64::from(fs.node(file).expect("header").extension());
    let mut device = fs.into_device();
    if let Some(block) = device.data.get_mut(&extension) {
        block[100] ^= 1;
    }
    let mut fs = Affs::open(device).expect("reopens");
    let root = fs.root();
    // The header's 72 data blocks are given back, then the damaged
    // extension is found: the change stops part-way.
    assert_eq!(fs.remove(root, b"f"), Err(DriverError::BadMagic));
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::Damaged));
    faulty_device(&mut fs).log.clear();
    fs.stats().expect("stats");
    assert!(
        !faulty_device(&mut fs).wrote(),
        "counting a damaged volume's space writes nothing to it"
    );
}

#[test]
fn listing_a_directory_of_links_walks_their_file_s_chain_once() {
    let mut fs = floppy(1);
    let root = fs.root();
    let dir = mkdir(&mut fs, root, "d");
    let file = make(&mut fs, dir, "real", b"x");
    let links = 40;
    for n in 0..links {
        fs.link(dir, alloc::format!("l{n}").as_bytes(), file)
            .expect("link");
    }
    let mut fs = Recorded::over(fs);
    let dir = fs.lookup(fs.root(), b"d").expect("d");
    logged(&mut fs);
    let names = listed(&mut fs, dir, 0, b"").expect("lists");
    assert_eq!(names.len(), links + 1);
    assert!(names
        .iter()
        .all(|(_, name)| name.as_slice() == b"real" || name.starts_with(b"l")));
    let reads = logged(&mut fs)
        .iter()
        .filter(|op| matches!(op, Op::Read(_)))
        .count();
    // The bucket chains, each entry again as it is handed over and the file it
    // names, and the file's chain of links once: walked per entry, it would
    // be read again for every link.
    assert!(
        reads <= 1 + 4 * (links + 1),
        "{reads} reads for {links} links"
    );
}
