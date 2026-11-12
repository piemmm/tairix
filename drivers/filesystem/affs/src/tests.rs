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
use tairix_abi::driver::filesystem::listing::{first_listed, listed};
use tairix_abi::driver::filesystem::{
    DirVisit, FilesystemAttrs, FilesystemRead, FilesystemSecurity, FilesystemStats,
    FilesystemWrite, NameMatching, NodeId, NodeKind, NodeSecurity, Unpinned, WriteRefusal,
};
use tairix_abi::driver::GrantingHost;
use tairix_abi::time::Time64;
use tairix_abi::{CapabilityId, DriverError, StampChange, TimesChange, TimesKept};
use tairix_fsprobe::affs::{
    be32, put_be32, root_block, DateStamp, Offsets, BITMAP_VALID, ST_LINKDIR, ST_SOFTLINK, T_HEADER,
};
use tairix_fuzzseed::Prng;
use tairix_writeback::TestVolumeHost;

use crate::{Affs, FormatOptions, SPARE_BLOCKS};

mod label;

const RECORD: NodeSecurity = NodeSecurity::new(0o644, 0, 0);

/// Every request this binary makes, metered per thread, so a test can bound
/// what a change holds or have the heap refuse it.
#[global_allocator]
static ALLOC: tairix_fuzzseed::meter::Metered = tairix_fuzzseed::meter::Metered;

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

#[test]
fn register_needs_the_load_capability() {
    let loading = GrantingHost {
        granted: &[CapabilityId::DRV_LOAD],
    };
    assert!(crate::register(&loading).is_ok());
    assert_eq!(
        crate::register(&GrantingHost { granted: &[] }).err(),
        Some(DriverError::PermissionDenied)
    );
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

#[test]
fn a_soft_link_read_on_a_short_heap_is_refused() {
    use tairix_fuzzseed::meter::refusing_after;
    let mut fs = floppy(1);
    let root = fs.root();
    for (name, target) in [
        (&b"absolute"[..], "/top/x"),
        (b"parents", "../../far"),
        (b"inner", "a/../b"),
    ] {
        let link = fs
            .create_link(root, name, target.as_bytes(), &RECORD)
            .expect("link");
        let mut granted = 0;
        loop {
            let mut out = [0u8; 64];
            match refusing_after(granted, || fs.read_link(link, &mut out)) {
                Ok(len) => {
                    assert_eq!(&out[..len], target.as_bytes());
                    break;
                }
                Err(err) => assert_eq!(err, DriverError::OutOfMemory, "{target}, {granted}"),
            }
            granted += 1;
        }
    }
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
    tairix_fsprobe::affs::seal(block, tairix_fsprobe::affs::CHECKSUM);
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

/// A header's one date stamp takes a modification time to the tick from
/// 1978 on, an access time only as that stamp, and nothing finer than a tick
/// unless the nearest is asked; the change is on the medium.
#[test]
fn set_times_sets_a_headers_one_date_stamp() {
    let mut fs = floppy(1);
    fs.set_volume_host(
        TestClock::volume(),
        TestClock::at(Some(Time64::from_secs(1_700_000_000))),
    );
    let root = fs.root();
    let file = make(&mut fs, root, "dated", b"x");
    let in_2200 = Time64::new(7_258_118_400, 980_000_000).expect("a time");
    let change = |accessed, modified, nearest| TimesChange {
        accessed,
        modified,
        nearest,
    };
    assert_eq!(
        fs.set_times(
            file,
            change(StampChange::At(in_2200), StampChange::At(in_2200), false)
        ),
        Ok(TimesKept::Exactly)
    );
    assert_eq!(fs.node_info(file).expect("info").times.modified, in_2200);
    let between_ticks = Time64::new(1_000_000_000, 1).expect("a time");
    assert_eq!(
        fs.set_times(
            file,
            change(StampChange::Keep, StampChange::At(between_ticks), false)
        ),
        Err(DriverError::OutOfRange)
    );
    assert_eq!(
        fs.set_times(
            file,
            change(StampChange::At(between_ticks), StampChange::Keep, false)
        ),
        Err(DriverError::OutOfRange),
        "an access time apart from the stamp"
    );
    let before_1978 = Time64::from_secs(0);
    assert_eq!(
        fs.set_times(
            file,
            change(StampChange::Keep, StampChange::At(before_1978), true)
        ),
        Ok(TimesKept::Nearest)
    );
    let mut fs = check_fs(fs);
    let file = fs.lookup(fs.root(), b"dated").expect("found");
    assert_eq!(
        fs.node_info(file).expect("info").times.modified,
        Time64::new(252_460_800, 20_000_000).expect("a time"),
        "1978's first tick: the zero stamp is none"
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

/// A write of nothing changes nothing, at or past the end of a file alike.
#[test]
fn an_empty_write_past_the_end_leaves_the_size_alone() {
    let mut fs = floppy(1);
    let root = fs.root();
    let empty = make(&mut fs, root, "empty", &[]);
    fs.write_all(empty, 24_062, &[]).expect("nothing to write");
    assert_eq!(fs.node_info(empty).expect("info").size, 0);
    let short = make(&mut fs, root, "short", b"abc");
    fs.write_all(short, 3, &[]).expect("nothing to write");
    fs.write_all(short, 9_000, &[]).expect("nothing to write");
    assert_eq!(read_whole(&mut fs, short), b"abc");
    drop(check_fs(fs));
}

/// A file made on a volume with no block left for its header is refused, and
/// leaves no entry for a later lookup or removal to find.
#[test]
fn a_create_on_a_full_volume_is_refused_and_leaves_no_entry() {
    let mut fs = floppy(1);
    let root = fs.root();
    let filler = make(&mut fs, root, "filler", &[]);
    let mut blocks = fs.stats().expect("stats").free_blocks as usize;
    while blocks > 0 {
        match fs.write_all(filler, 0, &vec![7u8; blocks * 512]) {
            Ok(()) => break,
            Err(DriverError::NoSpace) => blocks -= 1,
            Err(err) => panic!("filling: {err:?}"),
        }
    }
    let mut made = 0;
    let refused = loop {
        match fs.create(
            root,
            alloc::format!("f{made}").as_bytes(),
            NodeKind::RegularFile,
            &RECORD,
        ) {
            Ok(_) => made += 1,
            Err(err) => break err,
        }
    };
    assert_eq!(refused, DriverError::NoSpace);
    let name = alloc::format!("f{made}");
    assert_eq!(fs.lookup(root, name.as_bytes()), Err(DriverError::NotFound));
    assert_eq!(fs.remove(root, name.as_bytes()), Err(DriverError::NotFound));
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
    tairix_fsprobe::affs::seal(block, tairix_fsprobe::affs::CHECKSUM);
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
    tairix_fsprobe::affs::seal(block, tairix_fsprobe::affs::CHECKSUM);
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
    // A name filed in the same bucket walks the looping chain to its end.
    let twin = (0..10_000)
        .map(|n| alloc::format!("t{n}"))
        .find(|name| bucket_of(name) == bucket_of("a"))
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

/// Write `data` at `offset` to the root's file `name`, made first where
/// absent, as `model` records the file: a volume with no room for a new
/// file's header or for the data leaves both as they were, and a write of
/// nothing changes nothing, past the end too. Whether the file exists now.
fn write_modelled(
    fs: &mut Affs<MemDisk>,
    model: &mut BTreeMap<String, Vec<u8>>,
    (name, offset, data): (&str, usize, &[u8]),
    step: u32,
) -> bool {
    let root = fs.root();
    let file = match fs.lookup(root, name.as_bytes()) {
        Ok(file) => file,
        Err(_) => match fs.create(root, name.as_bytes(), NodeKind::RegularFile, &RECORD) {
            Ok(file) => file,
            Err(DriverError::NoSpace) => return false,
            Err(err) => panic!("step {step}: making {name}: {err:?}"),
        },
    };
    let entry = model.entry(String::from(name)).or_default();
    match fs.write_all(file, offset as u64, data) {
        Ok(()) if data.is_empty() => {}
        Ok(()) => {
            if entry.len() < offset + data.len() {
                entry.resize(offset + data.len(), 0);
            }
            entry[offset..offset + data.len()].copy_from_slice(data);
        }
        Err(DriverError::NoSpace) => {}
        Err(err) => panic!("step {step}: writing {name}: {err:?}"),
    }
    true
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
                    write_modelled(&mut fs, &mut model, (&name, offset, &data), step);
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

/// The bucket a volume of 512-byte blocks that folds ASCII files `name` in.
fn bucket_of(name: &str) -> usize {
    crate::name::Name::encode(name.as_bytes())
        .expect("a name")
        .bucket(
            NameMatching::AsciiCaseInsensitive,
            Offsets::new(512).table_len(),
        )
}

/// `count` names `{prefix}{n}` such a volume files in the bucket of
/// `{prefix}0`.
fn names_in_one_bucket(prefix: &str, count: usize) -> Vec<String> {
    let wanted = bucket_of(&alloc::format!("{prefix}0"));
    (0..)
        .map(|n| alloc::format!("{prefix}{n}"))
        .filter(|name| bucket_of(name) == wanted)
        .take(count)
        .collect()
}

#[test]
fn removing_a_hard_link_filed_after_its_file_keeps_their_bucket_whole() {
    assert!(BUCKET_64
        .iter()
        .all(|name| bucket_of(name) == bucket_of("3")));
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
    let seal = |block: &mut [u8]| tairix_fsprobe::affs::seal(block, tairix_fsprobe::affs::CHECKSUM);
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
    assert_eq!(read_whole(&mut fs, file), pattern(160 * 512, 4));
}

/// `file` read whole, in as few calls as the driver answers it in.
fn read_whole<B: Block>(fs: &mut Affs<B>, file: NodeId) -> Vec<u8> {
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
    let wanted = bucket_of("l0");
    let links = names_in_one_bucket("l", 12);
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

/// A change to a file holding `old`, answering what it then holds.
type Change<'a> = &'a dyn Fn(&mut Affs<Recorded>, NodeId, &[u8]) -> Vec<u8>;

/// Change `f`, `before` bytes on a fresh floppy, by `change`, then replay
/// every prefix of the change's writes onto the volume as it was: a device
/// that took only those writes reopens and reads `f` as it was or as it
/// became, never as damage.
fn every_cut_reads_old_or_new(dos_type: u8, before: usize, change: Change<'_>) {
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
    let new = change(&mut fs, file, &old);
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

/// The file grown by data written from its end to `after` bytes.
fn appended(fs: &mut Affs<Recorded>, file: NodeId, old: &[u8], after: usize) -> Vec<u8> {
    let tail = pattern(after - old.len(), 7);
    fs.write_all(file, old.len() as u64, &tail).expect("grows");
    [old, tail.as_slice()].concat()
}

/// The file cut, or grown with zeros, to `after` bytes.
fn truncated(fs: &mut Affs<Recorded>, file: NodeId, old: &[u8], after: usize) -> Vec<u8> {
    fs.truncate(file, after as u64).expect("truncates");
    let mut new = old.to_vec();
    new.resize(after, 0);
    new
}

/// The file grown by ten bytes written at `at`, past its end.
fn written_past_the_end(fs: &mut Affs<Recorded>, file: NodeId, old: &[u8], at: usize) -> Vec<u8> {
    fs.write_all(file, at as u64, b"0123456789").expect("grows");
    let mut new = old.to_vec();
    new.resize(at, 0);
    new.extend_from_slice(b"0123456789");
    new
}

#[test]
fn every_state_a_growth_cut_short_leaves_reads_the_file_as_it_was_or_became() {
    // Past the header's 72 pointers into a second extension block.
    every_cut_reads_old_or_new(1, 100 * 512, &|fs, file, old| {
        appended(fs, file, old, 200 * 512 + 100)
    });
    every_cut_reads_old_or_new(0, 100 * 488 + 7, &|fs, file, old| {
        appended(fs, file, old, 200 * 488)
    });
}

#[test]
fn every_state_a_cut_left_part_way_reads_the_file_as_it_was_or_became() {
    // Into the header's table, and into the first extension block.
    every_cut_reads_old_or_new(1, 200 * 512, &|fs, file, old| {
        truncated(fs, file, old, 50 * 512 + 5)
    });
    every_cut_reads_old_or_new(1, 200 * 512, &|fs, file, old| {
        truncated(fs, file, old, 80 * 512)
    });
    every_cut_reads_old_or_new(0, 200 * 488 + 9, &|fs, file, old| {
        truncated(fs, file, old, 80 * 488 + 3)
    });
}

/// The zeros a growth lays down move in runs, crossing two extension blocks,
/// and every state they leave part-way reads as the old file or the new.
#[test]
fn every_state_a_zero_growth_cut_short_leaves_reads_the_file_as_it_was_or_became() {
    every_cut_reads_old_or_new(1, 100 * 512, &|fs, file, old| {
        truncated(fs, file, old, 250 * 512)
    });
    every_cut_reads_old_or_new(1, 100 * 512, &|fs, file, old| {
        written_past_the_end(fs, file, old, 250 * 512)
    });
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
    for damaged_extension in [true, false] {
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
        // Reopened, so the free space is not yet counted.
        let expected = if damaged_extension {
            if let Some(block) = device.data.get_mut(&extension) {
                block[100] ^= 1;
            }
            // Found before the removal writes.
            DriverError::BadMagic
        } else {
            // The header's zeroing fails, once the file's blocks are given
            // back to the bitmap the change leaves part-way.
            device.fail_write_in(2, DriverError::DeviceFault);
            DriverError::DeviceFault
        };
        let mut fs = Affs::open(device).expect("reopens");
        let root = fs.root();
        assert_eq!(fs.remove(root, b"f"), Err(expected));
        assert_eq!(fs.write_refusal(), Some(WriteRefusal::Damaged));
        faulty_device(&mut fs).log.clear();
        fs.stats().expect("stats");
        assert!(
            !faulty_device(&mut fs).wrote(),
            "counting a damaged volume's space writes nothing to it"
        );
    }
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
    // The directory, its bucket chains, the file each link names, and the
    // file's chain of links once: walked per entry, it would be read again
    // for every link.
    assert!(
        reads <= 1 + 3 * (links + 1),
        "{reads} reads for {links} links"
    );
}

/// File an empty file in `dir` under the raw `recorded` name, as a volume
/// another writer made may hold it.
fn plant(fs: &mut Affs<MemDisk>, dir: NodeId, recorded: &[u8]) -> NodeId {
    let name = crate::name::Name::recorded(recorded).expect("a recorded name");
    fs.change(|fs| {
        let mut dir = fs.directory(dir)?;
        let block = fs.allocate(None)?;
        let buf = fs.disk.buffer()?;
        let mut entry = crate::header::Header::new(
            block,
            crate::header::Kind::File,
            buf,
            fs.at,
            &name,
            dir.block,
            fs.now(),
        );
        fs.file_entry(&mut dir, &mut entry)?;
        fs.write_header(&mut dir)?;
        Ok(NodeId::from_raw(block))
    })
    .expect("planted")
}

/// An entry a volume records under a name no new entry may take is still
/// found, read, renamed and removed by that name, and reached through a link
/// to it; only a name a change gives is held to the rules.
#[test]
fn an_entry_recorded_under_a_name_no_new_entry_may_take_is_still_reached() {
    for recorded in [&b"\x01Hidden"[..], b"Demo\x9b", b"a:b"] {
        let mut fs = floppy(3);
        let root = fs.root();
        let dir = mkdir(&mut fs, root, "planted");
        let planted = plant(&mut fs, dir, recorded);
        let spelled: String = recorded.iter().map(|&b| char::from(b)).collect();
        assert_eq!(names(&mut fs, dir), core::slice::from_ref(&spelled));
        assert_eq!(fs.lookup(dir, spelled.as_bytes()), Ok(planted));
        fs.write_all(planted, 0, b"payload").expect("written");
        assert_eq!(read_all(&mut fs, planted), b"payload");
        assert_eq!(
            fs.create(dir, spelled.as_bytes(), NodeKind::RegularFile, &RECORD)
                .err(),
            Some(DriverError::OutOfRange)
        );
        assert_eq!(
            fs.link(root, spelled.as_bytes(), planted).err(),
            Some(DriverError::OutOfRange)
        );
        // A link target spells no `:`, AmigaDOS's volume separator.
        if !recorded.contains(&b':') {
            let via = fs
                .create_link(dir, b"via", spelled.as_bytes(), &RECORD)
                .expect("a link to it");
            let mut target = [0u8; 64];
            let len = fs.read_link(via, &mut target).expect("reads");
            assert_eq!(
                fs.lookup(dir, &target[..len]),
                Ok(planted),
                "through the link"
            );
            fs.remove(dir, b"via").expect("the link goes");
        }
        assert_eq!(
            fs.rename(dir, spelled.as_bytes(), dir, spelled.as_bytes()),
            Ok(())
        );
        fs.rename(dir, spelled.as_bytes(), root, b"moved")
            .expect("renamed to a name it may take");
        assert_eq!(fs.lookup(root, b"moved"), Ok(planted));
        assert_eq!(
            fs.rename(root, b"moved", dir, spelled.as_bytes()).err(),
            Some(DriverError::OutOfRange)
        );
        let second = plant(&mut fs, dir, recorded);
        fs.remove(dir, spelled.as_bytes())
            .expect("removed by its name");
        assert_eq!(
            fs.lookup(dir, spelled.as_bytes()).err(),
            Some(DriverError::NotFound)
        );
        let _ = second;
        fs.remove(root, b"planted")
            .expect("the emptied directory goes");
        check_fs(fs);
    }
}

#[test]
fn a_volume_name_no_volume_may_take_is_refused_as_out_of_range() {
    let long = [b'n'; 31];
    for name in [&long[..], b"", b"a:b", b"tab\there"] {
        let formatted = Affs::format(
            MemDisk::new(901_120),
            &FormatOptions {
                dos_type: 3,
                block_size: 512,
                name,
                created: created(),
            },
        );
        assert_eq!(formatted.err(), Some(DriverError::OutOfRange), "{name:?}");
    }
}

/// A pinned file that has lost its last name, on `fs`, `bytes` of `seed`.
fn orphan_on(fs: &mut Affs<Faulty>, name: &str, seed: u8) -> NodeId {
    let root = fs.root();
    let file = fs
        .create(root, name.as_bytes(), NodeKind::RegularFile, &RECORD)
        .expect("create");
    fs.write_all(file, 0, &pattern(4000, seed)).expect("write");
    fs.pin(file).expect("pin");
    fs.remove(root, name.as_bytes()).expect("unlink");
    file
}

/// An orphan whose free fails at its last unpin is still counted, so the root
/// stays marked invalid for AmigaDOS's validator to reclaim its blocks.
#[test]
fn an_orphan_whose_free_fails_is_left_for_amigados_to_validate() {
    let mut fs = faulty(901_120);
    let temp = orphan_on(&mut fs, "temp", 3);
    faulty_device(&mut fs).fail_read = Some(temp.raw());
    assert_eq!(fs.unpin(temp), Err(DriverError::DeviceFault));
    assert_eq!(fs.write_refusal(), None, "nothing was written");
    faulty_device(&mut fs).fail_read = None;
    let root = fs.root();
    fs.create(root, b"after", NodeKind::RegularFile, &RECORD)
        .expect("the next change goes ahead");
    fs.flush().expect("commits");
    let fs = Affs::open(fs.into_device()).expect("reopens");
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::NeedsCheck));

    let mut fs = faulty(901_120);
    let one = orphan_on(&mut fs, "one", 4);
    let two = orphan_on(&mut fs, "two", 5);
    faulty_device(&mut fs).fail_read = Some(one.raw().min(two.raw()));
    assert_eq!(fs.release_pins(), Err(DriverError::DeviceFault));
    faulty_device(&mut fs).fail_read = None;
    fs.flush().expect("commits");
    let fs = Affs::open(fs.into_device()).expect("reopens");
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::NeedsCheck));
}

/// `fs`'s image with block `block` edited by `edit` and sealed again.
fn resealed(fs: Affs<MemDisk>, block: usize, edit: impl FnOnce(&mut [u8], Offsets)) -> Vec<u8> {
    let (mut data, bs) = image(fs);
    let at = Offsets::new(bs);
    let region = &mut data[block * bs..(block + 1) * bs];
    edit(region, at);
    tairix_fsprobe::affs::seal(region, tairix_fsprobe::affs::CHECKSUM);
    data
}

/// A change on `data`, opened afresh, is refused as damage, the volume is
/// written no more, and nothing was written.
fn refused_as_damage(data: &[u8], change: impl FnOnce(&mut Affs<MemDisk>) -> DriverError) {
    let mut fs = Affs::open(MemDisk {
        data: data.to_vec(),
        block_size: 512,
    })
    .expect("opens");
    assert_eq!(change(&mut fs), DriverError::BadMagic);
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::Damaged));
    let (after, _) = image(fs);
    assert!(after == data, "nothing was written");
}

/// A bitmap that names the root, one block twice, or leaves a page the volume
/// needs unnamed is damage, found before anything is written.
#[test]
fn a_bitmap_block_named_as_the_root_twice_or_after_a_gap_is_damage_and_nothing_is_written() {
    let create = |fs: &mut Affs<MemDisk>| {
        let root = fs.root();
        fs.create(root, b"new", NodeKind::RegularFile, &RECORD)
            .expect_err("refused")
    };
    let mut floppy = floppy(1);
    let root = floppy.root();
    make(&mut floppy, root, "keep", b"kept");
    refused_as_damage(
        &resealed(floppy, 880, |block, at| put_be32(block, at.bm_pages(), 880)),
        create,
    );
    let four_mib = || {
        let mut fs = fresh(1, 512, 4 << 20);
        let root = fs.root();
        make(&mut fs, root, "keep", b"kept");
        fs
    };
    let root_block = (4 << 20) / 512 / 2;
    refused_as_damage(
        &resealed(four_mib(), root_block, |block, at| {
            let first = be32(block, at.bm_pages());
            put_be32(block, at.bm_pages() + 4, first);
        }),
        create,
    );
    refused_as_damage(
        &resealed(four_mib(), root_block, |block, at| {
            put_be32(block, at.bm_pages() + 4, 0);
            put_be32(block, at.bm_pages() + 12, 8000);
        }),
        create,
    );
}

/// A file whose data pointer names the root or a bitmap block never has its
/// data written over it, nor its removal free it.
#[test]
fn a_file_naming_the_root_or_a_bitmap_block_as_its_data_is_never_written_over_or_freed() {
    for named in [880u32, 881] {
        let mut fs = floppy(1);
        let root = fs.root();
        make(&mut fs, root, "keep", b"kept");
        let victim = make(&mut fs, root, "victim", &[0x11; 512]);
        let data = resealed(fs, victim.raw() as usize, |block, at| {
            put_be32(
                block,
                tairix_fsprobe::affs::TABLE + 4 * (at.table_len() - 1),
                named,
            );
            put_be32(block, 16, named);
        });
        refused_as_damage(&data, |fs| {
            fs.write_at(victim, 0, &[0xAB; 512]).expect_err("refused")
        });
        let mut removing = Affs::open(MemDisk {
            data: data.clone(),
            block_size: 512,
        })
        .expect("opens");
        let root = removing.root();
        assert_eq!(removing.remove(root, b"victim"), Err(DriverError::BadMagic));
        assert_eq!(removing.write_refusal(), Some(WriteRefusal::Damaged));
        let (after, bs) = image(removing);
        let bit = u64::from(named) - 2;
        let page = &after[881 * bs..882 * bs];
        let long = be32(page, 4 + 4 * (bit / 32) as usize);
        assert_eq!(long >> (bit % 32) & 1, 0, "{named} is still marked in use");
        let mut fs = Affs::open(MemDisk {
            data,
            block_size: 512,
        })
        .expect("opens");
        let root = fs.root();
        let keep = fs.lookup(root, b"keep").expect("keep");
        assert_eq!(read_all(&mut fs, keep), b"kept");
    }
}

/// Bytes of a file one data block of a `dos_type` volume of 512-byte blocks
/// carries.
fn data_size(dos_type: u8) -> usize {
    if dos_type & 1 == 1 {
        512
    } else {
        512 - 24
    }
}

/// The extension blocks of the file whose header is `header` in `image`, as
/// many as its size counts, by their links.
fn extensions_of(image: &[u8], bs: usize, header: u64) -> Vec<u64> {
    let at = Offsets::new(bs);
    let block = |n: u64| &image[n as usize * bs..(n as usize + 1) * bs];
    let ds = if image[3] & 1 == 1 { bs } else { bs - 24 } as u64;
    let table = at.table_len() as u64;
    let head = block(header);
    let blocks = u64::from(be32(head, at.byte_size())).div_ceil(ds);
    let mut next = be32(head, at.extension());
    (0..blocks.saturating_sub(table).div_ceil(table))
        .map(|_| {
            let this = u64::from(next);
            next = be32(block(this), at.extension());
            this
        })
        .collect()
}

/// The reads `log` holds of any of `blocks`.
fn reads_of(log: &[Op], blocks: &[u64]) -> usize {
    log.iter()
        .filter(|op| matches!(op, Op::Read(lba) if blocks.contains(lba)))
        .count()
}

#[test]
fn appending_reads_only_the_extension_block_it_fills() {
    let mut fs = floppy(1);
    let root = fs.root();
    let mut model = pattern(900 * 512, 2);
    let file = make(&mut fs, root, "grows", &model);
    let mut fs = Recorded::over(fs);
    let append = |fs: &mut Affs<Recorded>, model: &mut Vec<u8>, seed: u8| {
        let data = pattern(8 * 512, seed);
        fs.write_all(file, model.len() as u64, &data)
            .expect("appends");
        model.extend_from_slice(&data);
    };
    // The first walks the cold chain once.
    append(&mut fs, &mut model, 0);
    logged(&mut fs);
    for seed in 1..=20 {
        append(&mut fs, &mut model, seed);
    }
    let log = logged(&mut fs);
    assert_eq!(read_whole(&mut fs, file), model);
    let data = fs.into_device().disk.data;
    check(&data, 512);
    let reads = reads_of(&log, &extensions_of(&data, 512, file.raw()));
    assert!(reads <= 2 * 20, "{reads} extension reads for 20 appends");
}

#[test]
fn an_overwrite_reads_no_extension_block() {
    let mut fs = floppy(1);
    let root = fs.root();
    let file = make(&mut fs, root, "kept", &pattern(900 * 512, 3));
    let mut fs = Recorded::over(fs);
    logged(&mut fs);
    fs.write_all(file, 0, &[9]).expect("overwrites");
    let log = logged(&mut fs);
    let mut first = [0u8; 1];
    assert_eq!(fs.read_at(file, 0, &mut first), Ok(1));
    assert_eq!(first, [9]);
    let data = fs.into_device().disk.data;
    let extensions = extensions_of(&data, 512, file.raw());
    assert_eq!(extensions.len(), 12);
    assert_eq!(reads_of(&log, &extensions), 0);
}

#[test]
fn two_files_read_in_turn_walk_neither_chain_again() {
    for dos_type in [0, 1] {
        let mut fs = floppy(dos_type);
        let root = fs.root();
        let bodies = [
            pattern(648 * data_size(dos_type), 4),
            pattern(648 * data_size(dos_type), 5),
        ];
        let files = [
            make(&mut fs, root, "a", &bodies[0]),
            make(&mut fs, root, "b", &bodies[1]),
        ];
        let mut fs = Recorded::over(fs);
        logged(&mut fs);
        let mut read = [vec![0u8; bodies[0].len()], vec![0u8; bodies[1].len()]];
        let mut calls = 0;
        for at in (0..bodies[0].len()).step_by(4096) {
            for (&file, out) in files.iter().zip(read.iter_mut()) {
                let end = (at + 4096).min(out.len());
                assert_eq!(fs.read_at(file, at as u64, &mut out[at..end]), Ok(end - at));
                calls += 1;
            }
        }
        let log = logged(&mut fs);
        assert!(read == bodies, "DOS\\{dos_type}: both read back");
        let data = fs.into_device().disk.data;
        let extensions: Vec<u64> = files
            .iter()
            .flat_map(|file| extensions_of(&data, 512, file.raw()))
            .collect();
        assert_eq!(extensions.len(), 16);
        let reads = reads_of(&log, &extensions);
        assert!(
            reads <= calls,
            "DOS\\{dos_type}: {reads} extension reads for {calls} calls"
        );
    }
}

#[test]
fn a_copy_within_the_volume_walks_neither_side_s_chain() {
    let mut fs = floppy(1);
    let root = fs.root();
    let body = pattern(648 * 512, 6);
    let from = make(&mut fs, root, "from", &body);
    let to = make(&mut fs, root, "to", b"");
    let mut fs = Recorded::over(fs);
    logged(&mut fs);
    let mut chunk = vec![0u8; 4096];
    let mut calls = 0;
    for at in (0..body.len()).step_by(chunk.len()) {
        let got = fs.read_at(from, at as u64, &mut chunk).expect("reads");
        fs.write_all(to, at as u64, &chunk[..got]).expect("writes");
        calls += 2;
    }
    let log = logged(&mut fs);
    assert_eq!(read_whole(&mut fs, to), body);
    let data = fs.into_device().disk.data;
    check(&data, 512);
    let extensions: Vec<u64> = [from, to]
        .iter()
        .flat_map(|file| extensions_of(&data, 512, file.raw()))
        .collect();
    let reads = reads_of(&log, &extensions);
    assert!(reads <= calls, "{reads} extension reads for {calls} calls");
}

#[test]
fn a_sequential_read_reads_each_extension_block_once() {
    let mut fs = floppy(1);
    let root = fs.root();
    let body = pattern(648 * 512, 7);
    let file = make(&mut fs, root, "stream", &body);
    let mut fs = Recorded::over(fs);
    logged(&mut fs);
    let mut out = vec![0u8; body.len()];
    for (n, chunk) in out.chunks_mut(4096).enumerate() {
        assert_eq!(fs.read_at(file, (n * 4096) as u64, chunk), Ok(chunk.len()));
    }
    let log = logged(&mut fs);
    assert_eq!(out, body);
    let data = fs.into_device().disk.data;
    let extensions = extensions_of(&data, 512, file.raw());
    assert_eq!(extensions.len(), 8);
    for extension in extensions {
        assert_eq!(
            reads_of(&log, &[extension]),
            1,
            "extension block {extension}"
        );
    }
}

/// More files than the driver keeps chains for, long enough to span many
/// extension blocks, written, cut, removed and made again, every change read
/// back and the volume read cold beside it: a chain held across a change that
/// it no longer describes reads the wrong blocks, or writes them.
#[test]
fn a_model_of_changes_across_many_long_files_is_kept_and_checks_out() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "affs-long-model",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    for dos_type in [0, 1] {
        let mut fs = fresh(dos_type, 512, 4 << 20);
        let root = fs.root();
        let mut model: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let compare = |fs: &mut Affs<MemDisk>, model: &BTreeMap<String, Vec<u8>>, step: u32| {
            for (name, data) in model {
                let file = fs.lookup(root, name.as_bytes()).expect("in the model");
                assert!(
                    read_whole(fs, file) == *data,
                    "DOS\\{dos_type} step {step}: {name}"
                );
            }
        };
        for step in 0..300u32 {
            let name = alloc::format!("long{}", rng.below(16));
            let changed = match rng.below(5) {
                0 | 1 => {
                    let offset = rng.below(600_000);
                    let data = pattern(rng.below(64_000), step as u8);
                    write_modelled(&mut fs, &mut model, (&name, offset, &data), step)
                }
                2 => match fs.lookup(root, name.as_bytes()) {
                    Ok(file) => {
                        let size = rng.below(700_000);
                        match fs.truncate(file, size as u64) {
                            Ok(()) => model.entry(name.clone()).or_default().resize(size, 0),
                            Err(DriverError::NoSpace) => {}
                            Err(err) => panic!("DOS\\{dos_type} step {step}: {err:?}"),
                        }
                        true
                    }
                    Err(_) => false,
                },
                3 => {
                    let removed = fs.remove(root, name.as_bytes());
                    assert_eq!(
                        removed.is_ok(),
                        model.remove(&name).is_some(),
                        "step {step}"
                    );
                    false
                }
                _ => false,
            };
            if changed {
                let file = fs.lookup(root, name.as_bytes()).expect("in the model");
                assert!(
                    read_whole(&mut fs, file) == model[&name],
                    "DOS\\{dos_type} step {step}: {name} as changed"
                );
            }
            // Another file's window, so chains are reached out of turn and
            // dropped.
            if let Some((other, data)) = model.iter().nth(rng.below(model.len().max(1))) {
                let file = fs.lookup(root, other.as_bytes()).expect("in the model");
                let at = rng.below(data.len().max(1));
                let end = (at + 4096).min(data.len());
                let mut window = vec![0u8; end - at];
                assert_eq!(fs.read_at(file, at as u64, &mut window), Ok(end - at));
                assert!(
                    window == data[at..end],
                    "DOS\\{dos_type} step {step}: {other}"
                );
            }
            if step % 50 == 49 {
                let mut cold = Affs::open(MemDisk {
                    data: fs.disk.dev.device_mut().data.clone(),
                    block_size: 512,
                })
                .expect("opens cold");
                compare(&mut cold, &model, step);
            }
        }
        let mut fs = check_fs(fs);
        compare(&mut fs, &model, 300);
        let mut listed = names(&mut fs, root);
        listed.sort();
        assert_eq!(listed, model.keys().cloned().collect::<Vec<_>>());
    }
}

#[test]
fn a_cut_holds_no_list_of_what_it_frees() {
    use tairix_fuzzseed::meter::{metered, refusing_above};
    for dos_type in [0, 1] {
        let mut fs = fresh(dos_type, 512, 32 << 20);
        let root = fs.root();
        let free = fs.stats().expect("stats").free_blocks;
        let body = pattern(40_000 * data_size(dos_type), 8);
        let file = make(&mut fs, root, "big", &body);
        // Every bitmap page held, so a cut's own allocations are all it makes.
        fs.stats().expect("stats");
        for size in [10_000 * data_size(dos_type) + 7, 0] {
            let (cut, metering) =
                metered(|| refusing_above(16 * 1024, || fs.truncate(file, size as u64)));
            assert_eq!(cut, Ok(()), "DOS\\{dos_type} cut to {size}");
            assert!(metering.peak < 16 * 1024, "DOS\\{dos_type}: {metering:?}");
            assert!(read_whole(&mut fs, file) == body[..size], "reads as cut");
        }
        assert_eq!(
            fs.stats().expect("stats").free_blocks,
            free - 1,
            "all but the header given back"
        );
        drop(check_fs(fs));
    }
}

/// A floppy holding one file of `blocks` data blocks whose extension block
/// `from` links back to its first, sealed as a writer would have left it.
fn looped(dos_type: u8, blocks: usize, from: usize) -> (Vec<u8>, NodeId) {
    let mut fs = floppy(dos_type);
    let root = fs.root();
    make(&mut fs, root, "keep", b"kept");
    let file = make(
        &mut fs,
        root,
        "looped",
        &pattern(blocks * data_size(dos_type), 9),
    );
    let extensions = extensions_of(&fs.disk.dev.device_mut().data, 512, file.raw());
    let first = crate::header::low32(extensions[0]);
    let data = resealed(fs, extensions[from] as usize, |block, at| {
        put_be32(block, at.extension(), first);
    });
    (data, file)
}

/// A chain the size counts four extension blocks of, the third linking back
/// to the first, passes the first twice within those four before a loop
/// check can see it: walked on to its end, it never ends. Every change that
/// frees or fills what the chain holds is refused before it writes.
#[test]
fn a_chain_that_returns_to_a_block_it_passed_is_damage_to_every_change() {
    for dos_type in [0, 1] {
        let ds = data_size(dos_type) as u64;
        let (data, file) = looped(dos_type, 318, 2);
        refused_as_damage(&data, |fs| {
            fs.truncate(file, 200 * ds).expect_err("a cut refused")
        });
        refused_as_damage(&data, |fs| {
            fs.write_at(file, 318 * ds, b"x")
                .expect_err("a growth refused")
        });
        let mut fs = Affs::open(MemDisk {
            data,
            block_size: 512,
        })
        .expect("opens");
        let root = fs.root();
        assert_eq!(fs.remove(root, b"looped"), Err(DriverError::BadMagic));
        assert_eq!(fs.write_refusal(), Some(WriteRefusal::Damaged));
        // A chain whose freed part links back into what a cut keeps.
        let (data, file) = looped(dos_type, 250, 1);
        refused_as_damage(&data, |fs| {
            fs.truncate(file, 144 * ds).expect_err("a cut refused")
        });
    }
}

#[test]
fn a_growth_whose_chain_has_no_room_to_grow_still_lands() {
    let mut fs = fresh(1, 512, 8 << 20);
    let root = fs.root();
    // 128 extension blocks: the chain the growth linked fills its room.
    let mut body = pattern((72 + 128 * 72) * 512, 10);
    let file = make(&mut fs, root, "long", &body);
    let tail = [0x5A; 512];
    let grown = tairix_fuzzseed::meter::refusing_above(1023, || {
        fs.write_all(file, body.len() as u64, &tail)
    });
    assert_eq!(grown, Ok(()));
    body.extend_from_slice(&tail);
    // Grown again before any read could learn the link the cache lost.
    fs.write_all(file, body.len() as u64, &tail)
        .expect("grows again");
    body.extend_from_slice(&tail);
    assert_eq!(fs.write_refusal(), None);
    assert!(read_whole(&mut fs, file) == body, "reads back grown");
    drop(check_fs(fs));
}

#[test]
fn the_chain_kept_follows_a_cut_and_goes_with_its_file() {
    let mut fs = floppy(1);
    let root = fs.root();
    let file = make(&mut fs, root, "long", &pattern(500 * 512, 11));
    let on_medium = |fs: &mut Affs<MemDisk>| -> Vec<u32> {
        extensions_of(&fs.disk.dev.device_mut().data, 512, file.raw())
            .into_iter()
            .map(crate::header::low32)
            .collect()
    };
    assert_eq!(
        fs.chains.held(file.raw()),
        Some(on_medium(&mut fs)),
        "linked as it grew"
    );
    fs.truncate(file, 200 * 512).expect("cuts");
    let kept = on_medium(&mut fs);
    assert_eq!(kept.len(), 2);
    assert_eq!(fs.chains.held(file.raw()), Some(kept), "cut with it");
    fs.remove(root, b"long").expect("removes");
    assert_eq!(fs.chains.held(file.raw()), None, "gone with its header");
}

#[test]
fn a_growth_by_truncate_writes_its_zeros_in_runs() {
    let mut fs = floppy(1);
    let root = fs.root();
    let file = make(&mut fs, root, "zeros", b"");
    let mut fs = Recorded::over(fs);
    logged(&mut fs);
    fs.truncate(file, 300 * 512).expect("grows");
    fs.flush().expect("commits");
    let writes = writes_logged(&mut fs).len();
    assert!(writes <= 30, "{writes} writes");
    assert!(
        read_whole(&mut fs, file) == vec![0; 300 * 512],
        "reads as zeros"
    );
    check(&fs.into_device().disk.data, 512);
}

#[test]
fn a_write_past_the_end_writes_the_gap_in_runs() {
    let mut fs = floppy(1);
    let root = fs.root();
    let file = make(&mut fs, root, "gap", b"");
    let mut fs = Recorded::over(fs);
    logged(&mut fs);
    fs.write_all(file, 200 * 512, b"0123456789").expect("grows");
    fs.flush().expect("commits");
    let writes = writes_logged(&mut fs).len();
    assert!(writes <= 25, "{writes} writes");
    let mut expected = vec![0; 200 * 512];
    expected.extend_from_slice(b"0123456789");
    assert!(
        read_whole(&mut fs, file) == expected,
        "reads as the gap and the data"
    );
    check(&fs.into_device().disk.data, 512);
}

/// The reads `log` holds.
fn reads(log: &[Op]) -> usize {
    log.iter().filter(|op| matches!(op, Op::Read(_))).count()
}

#[test]
fn a_listing_reads_each_entry_of_a_bucket_once() {
    let mut fs = floppy(1);
    let root = fs.root();
    for name in names_in_one_bucket("e", 12) {
        make(&mut fs, root, &name, b"");
    }
    let mut fs = Recorded::over(fs);
    logged(&mut fs);
    assert_eq!(listed(&mut fs, root, 0, b"").expect("lists").len(), 12);
    let reads = reads(&logged(&mut fs));
    assert!(
        reads <= 1 + 12,
        "{reads} reads: the directory, then each entry once"
    );
}

#[test]
fn a_listing_resumed_an_entry_at_a_time_walks_its_bucket_once() {
    let mut fs = floppy(1);
    let root = fs.root();
    let mut names = names_in_one_bucket("r", 200);
    for name in &names {
        make(&mut fs, root, name, b"");
    }
    let mut fs = Recorded::over(fs);
    logged(&mut fs);
    let mut seen = Vec::new();
    let (mut cursor, mut after) = (0, Vec::new());
    while let Some((entry, name)) = first_listed(&mut fs, root, cursor, &after).expect("lists") {
        cursor = entry.next_cursor;
        after.clone_from(&name);
        seen.push(String::from_utf8(name).expect("utf-8"));
    }
    seen.sort();
    names.sort();
    assert_eq!(seen, names);
    let reads = reads(&logged(&mut fs));
    assert!(reads <= 2 * 200 + 2, "{reads} reads");
}

/// A listing stopping on the first entry of a later bucket, past the one it
/// handed over, is resumed at that entry from its predecessor's cursor, as a
/// served listing whose reply filled is: neither the bucket it stopped in nor
/// the one before is walked again.
#[test]
fn a_listing_resumed_at_the_entry_it_stopped_on_walks_no_bucket_again() {
    let mut fs = floppy(1);
    let root = fs.root();
    let mut buckets = Vec::new();
    let names: Vec<String> = (0..)
        .map(|n| alloc::format!("s{n}"))
        .filter(|name| {
            let bucket = bucket_of(name);
            let fresh = !buckets.contains(&bucket);
            buckets.push(bucket);
            fresh
        })
        .take(6)
        .collect();
    for name in &names {
        make(&mut fs, root, name, b"");
    }
    let mut fs = Recorded::over(fs);
    let mut taken = None;
    fs.read_dir(root, 0, b"", &mut |entry, name| {
        if taken.is_some() {
            return DirVisit::Stop;
        }
        taken = Some((*entry, name.to_vec()));
        DirVisit::Take
    })
    .expect("lists");
    let (first, first_name) = taken.expect("an entry");
    logged(&mut fs);
    let rest = listed(&mut fs, root, first.next_cursor, &first_name).expect("lists");
    assert_eq!(rest.len(), names.len() - 1);
    let reads = reads(&logged(&mut fs));
    assert_eq!(
        reads,
        1 + names.len() - 2,
        "the directory, then each bucket past the one stopped in"
    );
}

#[test]
fn listings_of_two_directories_in_turn_each_hand_every_entry_over_once() {
    let mut fs = floppy(1);
    let root = fs.root();
    let dirs = [mkdir(&mut fs, root, "one"), mkdir(&mut fs, root, "two")];
    let names = names_in_one_bucket("t", 30);
    for dir in dirs {
        for name in &names {
            make(&mut fs, dir, name, b"");
        }
    }
    let mut cursors = [(0, Vec::new()), (0, Vec::new())];
    let mut seen = [Vec::new(), Vec::new()];
    let mut done = [false; 2];
    while done.contains(&false) {
        for side in 0..2 {
            if done[side] {
                continue;
            }
            let (cursor, after) = &cursors[side];
            match first_listed(&mut fs, dirs[side], *cursor, after).expect("lists") {
                Some((entry, name)) => {
                    seen[side].push(String::from_utf8(name.clone()).expect("utf-8"));
                    cursors[side] = (entry.next_cursor, name);
                }
                None => done[side] = true,
            }
        }
    }
    let mut want = names.clone();
    want.sort();
    for mut side in seen {
        side.sort();
        assert_eq!(side, want);
    }
    // Resumed after an entry a change removed, each entry left after it is
    // handed over once.
    let (first, first_name) = first_listed(&mut fs, dirs[0], 0, b"")
        .expect("lists")
        .expect("an entry");
    let (second, second_name) = first_listed(&mut fs, dirs[0], first.next_cursor, &first_name)
        .expect("lists")
        .expect("another");
    fs.remove(dirs[0], &second_name).expect("removes");
    let rest = listed(&mut fs, dirs[0], second.next_cursor, &second_name).expect("lists");
    assert_eq!(rest.len(), names.len() - 2);
    assert!(rest
        .iter()
        .all(|(_, name)| *name != first_name && *name != second_name));
}

#[test]
fn a_damaged_soft_link_fails_only_the_call_that_reaches_it() {
    let mut fs = floppy(1);
    let root = fs.root();
    make(&mut fs, root, BUCKET_64[0], b"");
    // A soft link storing no target, which a writer never leaves.
    let mut fs = forge_link(fs, BUCKET_64[1].as_bytes(), ST_SOFTLINK, 0, b"");
    let cursor = (bucket_of(BUCKET_64[0]) as u64) << 32;
    let (entry, name) = first_listed(&mut fs, root, cursor, b"")
        .expect("the file before it is handed over")
        .expect("an entry");
    assert_eq!(name, BUCKET_64[0].as_bytes());
    assert_eq!(
        first_listed(&mut fs, root, entry.next_cursor, &name).err(),
        Some(DriverError::BadMagic)
    );
}

#[test]
fn a_listing_keeps_only_the_bucket_it_stopped_in() {
    let mut fs = floppy(1);
    let root = fs.root();
    let names = names_in_one_bucket("k", 6);
    for name in &names {
        make(&mut fs, root, name, b"");
    }
    let (listing, metering) = tairix_fuzzseed::meter::metered(|| {
        listed(&mut fs, root, 0, b"").map(|listing| listing.len())
    });
    assert_eq!(listing, Ok(names.len()));
    assert!(
        fs.listing.is_none(),
        "a listing run to its end keeps nothing"
    );
    assert_eq!(metering.retained, 0);
    let bucket = bucket_of(&names[0]);
    first_listed(&mut fs, root, (bucket as u64) << 32, b"")
        .expect("lists")
        .expect("an entry");
    assert!(fs
        .listing
        .as_ref()
        .is_some_and(|held| held.is(root.raw(), bucket)));
    make(&mut fs, root, "after", b"");
    assert!(fs.listing.is_none(), "a change drops it");
}

#[test]
fn security_reads_only_the_node_s_own_header() {
    let mut fs = floppy(1);
    let root = fs.root();
    let file = make(&mut fs, root, "named", b"x");
    for n in 0..40 {
        fs.link(root, alloc::format!("l{n}").as_bytes(), file)
            .expect("link");
    }
    let mut fs = Recorded::over(fs);
    logged(&mut fs);
    assert_eq!(
        fs.security(file),
        Ok(NodeSecurity::unrecorded(NodeKind::RegularFile))
    );
    let reads = reads(&logged(&mut fs));
    assert!(reads <= 1, "{reads} reads");
}

/// A change a shortage meets, given the payload it writes.
type Shortened<'a> = &'a dyn Fn(&mut Affs<MemDisk>, &[u8]) -> Result<(), DriverError>;

/// A change a shortage meets, by name.
type Named = (
    &'static str,
    fn(&mut Affs<MemDisk>, &[u8]) -> Result<(), DriverError>,
);

/// Run `change` on a volume `prepare` makes under every count of granted
/// allocations short of what it needs: each is a refusal — `OutOfMemory`,
/// nothing written, the volume still writable — after which the same change
/// goes ahead; once enough are granted it goes ahead at once. The volume
/// checks out either way.
fn every_shortage_is_a_refusal(
    what: &str,
    prepare: &dyn Fn() -> Affs<MemDisk>,
    change: Shortened<'_>,
    payload_blocks: usize,
) {
    use tairix_fuzzseed::meter::refusing_after;
    // What a change writes is the caller's, made before any shortage.
    let payload = pattern(payload_blocks * 512, 9);
    for granted in 0.. {
        assert!(granted < 4096, "{what}: never went ahead");
        let mut fs = prepare();
        let before = fs.disk.dev.device().data.clone();
        let outcome = refusing_after(granted, || change(&mut fs, &payload));
        if let Err(err) = outcome {
            assert_eq!(err, DriverError::OutOfMemory, "{what}, {granted} granted");
            assert_eq!(
                fs.write_refusal(),
                None,
                "{what}, {granted} granted: a shortage is no damage"
            );
            assert!(
                fs.disk.dev.device().data == before,
                "{what}, {granted} granted: a refusal writes nothing"
            );
            change(&mut fs, &payload)
                .unwrap_or_else(|err| panic!("{what}: {err:?} once memory is there"));
        }
        fs.flush().expect("commits");
        drop(check_fs(fs));
        if outcome.is_ok() {
            return;
        }
    }
}

/// A floppy holding a file `"3"`, a 200-block file `long` and an empty
/// directory `moved` in a directory `d`, a file `full` of 72 and one `rows`
/// of 144 blocks, and an empty directory `e` with a hard link to it in `d`.
fn shortage_floppy(dos_type: u8, deferred: bool) -> Affs<MemDisk> {
    let mut fs = floppy(dos_type);
    let root = fs.root();
    let ds = data_size(dos_type);
    make(&mut fs, root, BUCKET_64[0], b"three");
    let dir = mkdir(&mut fs, root, "d");
    make(&mut fs, dir, "long", &pattern(200 * ds, 1));
    mkdir(&mut fs, dir, "moved");
    make(&mut fs, root, "full", &pattern(72 * ds, 2));
    make(&mut fs, root, "rows", &pattern(144 * ds, 3));
    let empty = mkdir(&mut fs, root, "e");
    let (mut fs, _) = forge_dir_link(fs, dir, b"elink", empty);
    if deferred {
        open_transaction(&mut fs);
    }
    fs
}

/// Defer `fs`'s commits to a volume host, with a transaction already open, as
/// a later change finds one.
fn open_transaction(fs: &mut Affs<MemDisk>) {
    let host = TestVolumeHost::leaked(1_000);
    fs.set_volume_host(TestVolumeHost::volume(), host);
    let root = fs.root();
    make(fs, root, "opened", b"x");
}

/// A floppy crowding every bucket a rename over a linked name walks. Blocks
/// are taken in turn, so each filler, made first, files ahead of what comes
/// later in its bucket: `d`'s file to move behind one, and in `e`, behind two,
/// a file a link in `p` names (its link behind a filler there) and a link to
/// `r` that another link to it was made after.
fn crowded_floppy(dos_type: u8, deferred: bool) -> Affs<MemDisk> {
    let mut fs = floppy(dos_type);
    let root = fs.root();
    let from = mkdir(&mut fs, root, "d");
    let into = mkdir(&mut fs, root, "e");
    let linking = mkdir(&mut fs, root, "p");
    let [ey0, ey1, ey2] = crowded("ey");
    let [ez0, ez1, ez2] = crowded("ez");
    let [pl0, pl1, _] = crowded("pl");
    let [dx0, dx1, _] = crowded("dx");
    for (dir, filler) in [
        (into, &ey0),
        (into, &ey1),
        (into, &ez0),
        (into, &ez1),
        (linking, &pl0),
        (from, &dx0),
    ] {
        make(&mut fs, dir, filler, b"0");
    }
    let named = make(&mut fs, into, &ey2, b"y");
    fs.link(linking, pl1.as_bytes(), named)
        .expect("the file's link");
    let linked = make(&mut fs, root, "r", b"r");
    fs.link(into, ez2.as_bytes(), linked)
        .expect("the occupant link");
    fs.link(root, b"rlink", linked)
        .expect("a link made after it");
    make(&mut fs, from, &dx1, b"x");
    if deferred {
        open_transaction(&mut fs);
    }
    fs
}

/// Three names filed in one bucket, `{prefix}`'s.
fn crowded(prefix: &str) -> [String; 3] {
    let mut names = names_in_one_bucket(prefix, 3).into_iter();
    core::array::from_fn(|_| names.next().expect("three names in one bucket"))
}

/// A rename over a name a link shares, every bucket it walks crowded, is
/// refused before it writes whatever its working set: once written, a change
/// takes only the spares it holds.
#[test]
fn a_rename_over_a_linked_name_in_crowded_buckets_is_refused_before_it_writes() {
    // Spelled before any shortage: the meter refuses this test's own heap too.
    let (dx, ey, ez) = (crowded("dx"), crowded("ey"), crowded("ez"));
    let over_file = |fs: &mut Affs<MemDisk>, _: &[u8]| {
        let (d, e) = (look(fs, &["d"])?, look(fs, &["e"])?);
        fs.rename(d, dx[1].as_bytes(), e, ey[2].as_bytes())
    };
    let over_link = |fs: &mut Affs<MemDisk>, _: &[u8]| {
        let (d, e) = (look(fs, &["d"])?, look(fs, &["e"])?);
        fs.rename(d, dx[1].as_bytes(), e, ez[2].as_bytes())
    };
    let changes: [(&str, Shortened<'_>); 2] = [
        ("a rename over a file a link names", &over_file),
        ("a rename over a hard link", &over_link),
    ];
    for dos_type in [0, 1] {
        for deferred in [false, true] {
            for (what, change) in changes {
                every_shortage_is_a_refusal(
                    &alloc::format!(
                        "DOS\\{dos_type}{}: {what}",
                        if deferred { ", deferred" } else { "" }
                    ),
                    &|| crowded_floppy(dos_type, deferred),
                    change,
                    1,
                );
            }
        }
    }
}

/// [`shortage_floppy`] with `d/long` held and its name removed: an orphan
/// its last unpin frees.
fn orphan_floppy(dos_type: u8, deferred: bool) -> Affs<MemDisk> {
    let mut fs = shortage_floppy(dos_type, deferred);
    let long = look(&mut fs, &["d", "long"]).expect("long");
    let dir = look(&mut fs, &["d"]).expect("d");
    fs.pin(long).expect("pinned");
    fs.remove(dir, b"long").expect("unnamed");
    fs
}

fn look(fs: &mut Affs<MemDisk>, path: &[&str]) -> Result<NodeId, DriverError> {
    path.iter()
        .try_fold(fs.root(), |dir, name| fs.lookup(dir, name.as_bytes()))
}

/// Every kind of change, by name, made with the payload it writes on a
/// volume [`shortage_floppy`] makes.
fn changes_a_shortage_meets() -> [Named; 15] {
    [
        ("a create filed after another", |fs, _| {
            let root = fs.root();
            fs.create(
                root,
                BUCKET_64[1].as_bytes(),
                NodeKind::RegularFile,
                &RECORD,
            )
            .map(|_| ())
        }),
        ("a link", |fs, _| {
            let three = look(fs, &[BUCKET_64[0]])?;
            let root = fs.root();
            fs.link(root, BUCKET_64[1].as_bytes(), three)
        }),
        ("a growth into a first extension", |fs, _| {
            let full = look(fs, &["full"])?;
            let at = fs.node_info(full)?.size;
            fs.write_all(full, at, b"x")
        }),
        ("an append past a full extension", |fs, _| {
            let rows = look(fs, &["rows"])?;
            let at = fs.node_info(rows)?.size;
            fs.write_all(rows, at, b"x")
        }),
        ("an overwrite across extensions", |fs, payload| {
            let long = look(fs, &["d", "long"])?;
            fs.write_all(long, 1000, payload)
        }),
        ("a cut into the header", |fs, _| {
            let long = look(fs, &["d", "long"])?;
            fs.truncate(long, 50 * 512 + 5)
        }),
        ("a cut into an extension", |fs, _| {
            let long = look(fs, &["d", "long"])?;
            fs.truncate(long, 80 * 512)
        }),
        ("a growth over a gap", |fs, _| {
            let full = look(fs, &["full"])?;
            fs.truncate(full, 300 * 512)
        }),
        ("a removal", |fs, _| {
            let dir = look(fs, &["d"])?;
            fs.remove(dir, b"long")
        }),
        ("a rename across directories over a file", |fs, _| {
            let dir = look(fs, &["d"])?;
            let root = fs.root();
            fs.rename(root, b"full", dir, b"long")
        }),
        ("a rename over a directory a link names", |fs, _| {
            let root = fs.root();
            let dir = look(fs, &["d"])?;
            fs.rename(dir, b"moved", root, b"e")
        }),
        ("a removal promoting a link", |fs, _| {
            let root = fs.root();
            fs.remove(root, b"e")
        }),
        ("a directory", |fs, _| {
            let root = fs.root();
            fs.create(root, b"made", NodeKind::Directory, &RECORD)
                .map(|_| ())
        }),
        ("a soft link", |fs, _| {
            let root = fs.root();
            fs.create_link(root, b"soft", b"d/long", &RECORD)
                .map(|_| ())
        }),
        ("a comment", |fs, _| {
            let three = look(fs, &[BUCKET_64[0]])?;
            fs.set_attr(three, b"amiga.comment", b"noted")
        }),
    ]
}

/// A heap that runs short partway through a change refuses the change before
/// it writes, never leaving the volume part-way: whatever the change, and
/// whether its transaction opens with it or is open already.
#[test]
fn every_change_a_shortage_meets_is_refused_before_it_writes() {
    for dos_type in [0, 1] {
        for deferred in [false, true] {
            for (what, change) in changes_a_shortage_meets() {
                every_shortage_is_a_refusal(
                    &alloc::format!(
                        "DOS\\{dos_type}{}: {what}",
                        if deferred { ", deferred" } else { "" }
                    ),
                    &|| shortage_floppy(dos_type, deferred),
                    &change,
                    150,
                );
            }
        }
    }
}

#[test]
fn a_commit_allocates_nothing() {
    let mut fs = shortage_floppy(1, true);
    let root = fs.root();
    make(&mut fs, root, "pending", &pattern(3000, 4));
    let committed = tairix_fuzzseed::meter::refusing_above(0, || fs.flush());
    assert_eq!(committed, Ok(()));
    assert_eq!(fs.write_refusal(), None);
    drop(check_fs(fs));
}

/// An orphan whose free at its last unpin meets a shortage is refused before
/// it writes, and stays owed its free: the release of the mount's pins frees
/// it, and the volume ends validated.
#[test]
fn an_orphan_whose_free_meets_a_shortage_is_freed_when_the_pins_are_released() {
    use tairix_fuzzseed::meter::refusing_after;
    let mut refused = 0;
    for granted in 0.. {
        assert!(granted < 4096, "never freed");
        let mut fs = orphan_floppy(1, false);
        let orphan = (&fs.held)
            .into_iter()
            .next()
            .map(|(block, _)| NodeId::from_raw(block))
            .expect("held");
        let before = fs.disk.dev.device().data.clone();
        match refusing_after(granted, || fs.unpin(orphan)) {
            Ok(Unpinned::Freed) => {
                drop(check_fs(fs));
                assert!(refused > 0, "a shortage was met first");
                return;
            }
            Ok(other) => panic!("{other:?}"),
            Err(err) => {
                assert_eq!(err, DriverError::DeviceFault, "{granted} granted");
                assert_eq!(fs.write_refusal(), None, "{granted} granted");
                assert!(fs.disk.dev.device().data == before, "{granted} granted");
                fs.release_pins().expect("freed once memory is there");
                fs.flush().expect("commits");
                drop(check_fs(fs));
                refused += 1;
            }
        }
    }
}

/// A walk hands each extension block's buffer back as it steps on, so one
/// made once a change has written leaves the spares as it found them.
#[test]
fn a_walk_once_a_change_has_written_takes_no_buffer_from_the_heap() {
    use tairix_fuzzseed::meter::refusing_above;
    let mut fs = fresh(0, 512, 4 << 20);
    let root = fs.root();
    make(&mut fs, root, "big", &pattern(3000 * data_size(0), 6));
    let (data, _) = image(fs);
    let mut fs = Affs::open(MemDisk {
        data,
        block_size: 512,
    })
    .expect("reopens");
    assert!(fs.extension_blocks(3000) > 2 * SPARE_BLOCKS as u64);
    let big = look(&mut fs, &["big"]).expect("big");
    let header = fs.header(big.raw()).expect("its header");
    fs.disk.stock().expect("the spares");
    fs.disk.spending = true;
    let mut held = None;
    let mut buf = [0u8; 16];
    let last = 2999 * data_size(0) as u64;
    // Refused a block buffer, though not the chain's own growth.
    let read = refusing_above(511, || fs.read_file(&header, last, &mut buf, &mut held));
    assert_eq!(read, Ok(16), "the cold chain walked on spares alone");
    assert_eq!(buf, pattern(3000 * data_size(0), 6)[last as usize..][..16]);
}

/// A read outside any change keeps no buffer it walks the chain with, so on a
/// heap running short it is refused at every request it makes, never stopped
/// by a spare grown to keep one.
#[test]
fn a_read_across_a_files_extensions_keeps_no_buffer_and_is_refused_on_a_short_heap() {
    use tairix_fuzzseed::meter::refusing_after;
    let payload = pattern(300 * data_size(1), 3);
    let mut fs = fresh(1, 512, 4 << 20);
    let root = fs.root();
    make(&mut fs, root, "long", &payload);
    let (data, _) = image(fs);
    // In the third extension block, so the walk reads two on the way.
    let at = 250 * data_size(1);
    let mut granted = 0;
    loop {
        // Reopened for every grant, so each read walks the chain cold.
        let mut fs = Affs::open(MemDisk {
            data: data.clone(),
            block_size: 512,
        })
        .expect("reopens");
        assert!(fs.extension_blocks(300) > 3);
        let long = look(&mut fs, &["long"]).expect("long");
        let mut out = [0u8; 64];
        match refusing_after(granted, || fs.read_at(long, at as u64, &mut out)) {
            Ok(len) => {
                assert_eq!(out[..], payload[at..][..len]);
                assert!(fs.disk.spare.is_empty(), "a read keeps no buffer");
                break;
            }
            Err(err) => assert_eq!(err, DriverError::OutOfMemory, "{granted}"),
        }
        granted += 1;
    }
}

/// A long file's walks hand each extension block's buffer back, so a change
/// crossing far more extension blocks than are held in reserve still takes
/// none from the heap once it has written.
#[test]
fn a_change_across_a_long_file_takes_nothing_from_the_heap_once_it_writes() {
    let changes: [Named; 3] = [
        ("a removal", |fs, _| {
            let root = fs.root();
            fs.remove(root, b"big")
        }),
        ("a cut", |fs, _| {
            let big = look(fs, &["big"])?;
            fs.truncate(big, 100 * 512)
        }),
        ("an overwrite", |fs, payload| {
            let big = look(fs, &["big"])?;
            fs.write_all(big, 0, payload)
        }),
    ];
    for dos_type in [0, 1] {
        let prepare = || {
            let mut fs = fresh(dos_type, 512, 4 << 20);
            let root = fs.root();
            make(
                &mut fs,
                root,
                "big",
                &pattern(3000 * data_size(dos_type), 6),
            );
            // Reopened, so every chain is walked cold.
            let (data, _) = image(fs);
            Affs::open(MemDisk {
                data,
                block_size: 512,
            })
            .expect("reopens")
        };
        for (what, change) in changes {
            every_shortage_is_a_refusal(
                &alloc::format!("DOS\\{dos_type}: {what} of a long file"),
                &prepare,
                &change,
                2500,
            );
        }
    }
}
