//! ADFS driver unit tests.
//!
//! Every variant is formatted in memory with [`Adfs::format`],
//! exercised through the filesystem traits (round-trips, growth,
//! truncation, renames, metadata), and then deliberately corrupted —
//! map checksums, directory check bytes, zone checks, the cross-check,
//! the boot block, and big-directory structures — to prove the driver
//! fails closed rather than trusting a damaged volume.

extern crate std;

use super::*;
use std::vec;
use std::vec::Vec;
use tairix_abi::driver::block::BlockGeometry;
use tairix_abi::driver::filesystem::clock::TestClock;
use tairix_abi::driver::filesystem::listing::listed;
use tairix_abi::DriverKind;

// What a test's creations ask for, which ADFS has nowhere to keep.
const FILE_RECORD: NodeSecurity = NodeSecurity::new(0o644, 0, 0);
const DIR_RECORD: NodeSecurity = NodeSecurity::new(0o755, 0, 0);
const LINK_RECORD: NodeSecurity = NodeSecurity::new(0o777, 0, 0);

/// In-memory block device backing the test volumes, counting the reads it
/// serves so a test can bound what an operation costs.
struct MemDisk {
    data: Vec<u8>,
    block_size: u32,
    reads: std::sync::Arc<core::sync::atomic::AtomicUsize>,
}

impl MemDisk {
    fn new(bytes: usize, block_size: u32) -> Self {
        Self {
            data: vec![0; bytes],
            block_size,
            reads: std::sync::Arc::default(),
        }
    }
}

impl Block for MemDisk {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: self.block_size,
            block_count: (self.data.len() as u64) / u64::from(self.block_size),
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.reads
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let at = usize::try_from(lba * u64::from(self.block_size))
            .map_err(|_| DriverError::LengthOutOfRange)?;
        let end = at
            .checked_add(buf.len())
            .ok_or(DriverError::LengthOutOfRange)?;
        if end > self.data.len() {
            return Err(DriverError::LengthOutOfRange);
        }
        buf.copy_from_slice(&self.data[at..end]);
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, data: &[u8]) -> Result<(), DriverError> {
        let at = usize::try_from(lba * u64::from(self.block_size))
            .map_err(|_| DriverError::LengthOutOfRange)?;
        let end = at
            .checked_add(data.len())
            .ok_or(DriverError::LengthOutOfRange)?;
        if end > self.data.len() {
            return Err(DriverError::LengthOutOfRange);
        }
        self.data[at..end].copy_from_slice(data);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

struct MockHost {
    drv_load: bool,
}

impl DriverHost for MockHost {
    fn has_capability(&self, cap: CapabilityId) -> bool {
        matches!(cap, CapabilityId::DRV_LOAD if self.drv_load)
    }

    fn kind(&self) -> DriverKind {
        DriverKind::UserSpace
    }
}

/// Every supported variant with the device size the tests give it (a
/// hard disc gets a 4 MiB device).
const VARIANTS: [(AdfsVariant, usize); 10] = [
    (AdfsVariant::S, 160 * 1024),
    (AdfsVariant::M, 320 * 1024),
    (AdfsVariant::L, 640 * 1024),
    (AdfsVariant::D, 800 * 1024),
    (AdfsVariant::E, 800 * 1024),
    (AdfsVariant::EPlus, 800 * 1024),
    (AdfsVariant::F, 1600 * 1024),
    (AdfsVariant::FPlus, 1600 * 1024),
    (AdfsVariant::HardDisc, 4 * 1024 * 1024),
    (AdfsVariant::HardDiscPlus, 4 * 1024 * 1024),
];

fn fresh(variant: AdfsVariant, bytes: usize) -> Adfs<MemDisk> {
    match Adfs::format(MemDisk::new(bytes, 512), variant) {
        Ok(fs) => fs,
        Err(err) => panic!("{variant:?}: format failed: {err:?}"),
    }
}

fn make(fs: &mut Adfs<MemDisk>, dir: NodeId, name: &[u8], data: &[u8]) -> NodeId {
    let file = fs
        .create(dir, name, NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    if !data.is_empty() {
        assert_eq!(fs.write_at(file, 0, data).expect("write"), data.len());
    }
    fs.lookup(dir, name).expect("created file resolves")
}

fn read_all(fs: &mut Adfs<MemDisk>, node: NodeId) -> Vec<u8> {
    let info = fs.node_info(node).expect("node info");
    let mut out = vec![0u8; usize::try_from(info.size).expect("size fits")];
    // Read through a small window to exercise offset handling.
    let mut done = 0usize;
    while done < out.len() {
        let take = (out.len() - done).min(333);
        let got = fs
            .read_at(node, done as u64, &mut out[done..done + take])
            .expect("read");
        assert!(got > 0, "unexpected EOF at {done}");
        done += got;
    }
    out
}

/// Collect `(name, is_dir, size)` for every entry of `dir`.
fn list(fs: &mut Adfs<MemDisk>, dir: NodeId) -> Vec<(Vec<u8>, bool, u64)> {
    listed(fs, dir, 0, &[])
        .expect("read_dir")
        .into_iter()
        .map(|(entry, name)| {
            (
                name,
                entry.info.kind == NodeKind::Directory,
                entry.info.size,
            )
        })
        .collect()
}

/// A listing resumed by cursor while the sorted directory shifts under it —
/// two names inserted before the resume point and one removed after it —
/// lists every lasting entry exactly once.
#[test]
fn a_listing_resumed_across_shifts_lists_each_lasting_entry_once() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = fs.root();
        for name in [&b"Bravo"[..], b"Delta", b"Echo", b"Golf", b"Hotel"] {
            fs.create(root, name, NodeKind::RegularFile, &FILE_RECORD)
                .expect("create");
        }
        let head = listed(&mut fs, root, 0, &[]).expect("read_dir");
        let (cursor_entry, after) = &head[1];
        assert_eq!(after, b"Delta", "{variant:?}: sorted");
        for name in [&b"Alpha"[..], b"Charlie"] {
            fs.create(root, name, NodeKind::RegularFile, &FILE_RECORD)
                .expect("create");
        }
        fs.remove(root, b"Golf").expect("remove");
        let rest: Vec<Vec<u8>> = listed(&mut fs, root, cursor_entry.next_cursor, after)
            .expect("read_dir")
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        assert_eq!(rest, [&b"Echo"[..], b"Hotel"], "{variant:?}");
    }
}

#[test]
fn sparse_writes_zero_fill_the_gap() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let sparse = make(&mut fs, root, b"Sparse", b"head");
        // Writing past the end zero-fills between the old end and the
        // write offset.
        assert_eq!(fs.write_at(sparse, 5000, b"tail").expect("sparse"), 4);
        let node = fs.lookup(root, b"Sparse").expect("node");
        let body = read_all(&mut fs, node);
        assert_eq!(&body[..4], b"head", "{variant:?}");
        assert!(body[4..5000].iter().all(|&b| b == 0), "{variant:?}: gap");
        assert_eq!(&body[5000..], b"tail", "{variant:?}");
    }
}

#[test]
fn truncate_grows_shrinks_and_frees() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let body: Vec<u8> = (0..20_000u32).map(|i| (i % 241) as u8).collect();
        let file = make(&mut fs, root, b"File", &body);
        let free_full = fs.stats().expect("stats").free_blocks;

        // Shrink: the survivor keeps its data and space returns.
        fs.truncate(file, 700).expect("shrink");
        let node = fs.lookup(root, b"File").expect("node");
        assert_eq!(read_all(&mut fs, node), &body[..700], "{variant:?}");
        assert!(
            fs.stats().expect("stats").free_blocks > free_full,
            "{variant:?}: shrink frees space"
        );

        // Grow: the tail is zero-filled.
        fs.truncate(node, 2000).expect("grow");
        let node = fs.lookup(root, b"File").expect("node");
        let grown = read_all(&mut fs, node);
        assert_eq!(&grown[..700], &body[..700], "{variant:?}");
        assert!(grown[700..].iter().all(|&b| b == 0), "{variant:?}: zeros");

        // Truncate to nothing releases the whole allocation.
        fs.truncate(node, 0).expect("empty");
        let node = fs.lookup(root, b"File").expect("node");
        let info = fs.node_info(node).expect("info");
        assert_eq!((info.size, info.allocated), (0, 0), "{variant:?}");

        // A directory cannot be truncated.
        let dir = fs
            .create(root, b"Dir", NodeKind::Directory, &DIR_RECORD)
            .expect("mkdir");
        assert_eq!(
            fs.truncate(dir, 0),
            Err(DriverError::Unsupported),
            "{variant:?}"
        );
    }
}

#[test]
fn remove_reclaims_space_and_names() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let baseline = fs.stats().expect("stats").free_blocks;
        make(&mut fs, root, b"Doomed", &vec![7u8; 30_000]);
        let sub = fs
            .create(root, b"Nest", NodeKind::Directory, &DIR_RECORD)
            .expect("dir");
        make(&mut fs, sub, b"child", b"x");

        // A populated directory refuses removal; its child does not.
        assert_eq!(
            fs.remove(root, b"Nest"),
            Err(DriverError::DirectoryNotEmpty),
            "{variant:?}"
        );
        fs.remove(sub, b"child").expect("remove child");
        fs.remove(root, b"Nest").expect("remove emptied dir");
        fs.remove(root, b"Doomed").expect("remove file");
        assert_eq!(fs.remove(root, b"Doomed"), Err(DriverError::NotFound));

        // Every block returns and the names are reusable.
        assert_eq!(
            fs.stats().expect("stats").free_blocks,
            baseline,
            "{variant:?}: all space reclaimed"
        );
        make(&mut fs, root, b"Doomed", b"fresh");
        let node = fs.lookup(root, b"doomed").expect("reused name");
        assert_eq!(read_all(&mut fs, node), b"fresh", "{variant:?}");
    }
}

#[test]
fn rename_moves_replaces_and_guards_cycles() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let one = fs
            .create(root, b"One", NodeKind::Directory, &DIR_RECORD)
            .expect("dir");
        let two = fs
            .create(root, b"Two", NodeKind::Directory, &DIR_RECORD)
            .expect("dir");
        make(&mut fs, one, b"file", b"payload");

        // A plain rename within a directory.
        fs.rename(one, b"file", one, b"named").expect("rename");
        assert_eq!(fs.lookup(one, b"file"), Err(DriverError::NotFound));
        let node = fs.lookup(one, b"named").expect("renamed");
        assert_eq!(read_all(&mut fs, node), b"payload");

        // A move across directories.
        fs.rename(one, b"named", two, b"moved").expect("move");
        let node = fs.lookup(two, b"moved").expect("moved");
        assert_eq!(read_all(&mut fs, node), b"payload");

        // Replacement frees the victim's space and keeps the mover's data.
        make(&mut fs, two, b"victim", &vec![3u8; 9000]);
        let before = fs.stats().expect("stats").free_blocks;
        fs.rename(two, b"moved", two, b"victim").expect("replace");
        assert!(
            fs.stats().expect("stats").free_blocks > before,
            "{variant:?}: replaced data freed"
        );
        let node = fs.lookup(two, b"victim").expect("kept");
        assert_eq!(read_all(&mut fs, node), b"payload", "{variant:?}");
        assert_eq!(fs.lookup(two, b"moved"), Err(DriverError::NotFound));

        // A directory move updates the child's parent linkage, and a
        // directory can never move into its own subtree.
        fs.rename(root, b"One", two, b"Inner").expect("move dir");
        let inner = fs.lookup(two, b"Inner").expect("inner");
        make(&mut fs, inner, b"leaf", b"leaf body");
        assert_eq!(
            fs.rename(root, b"Two", inner, b"Loop"),
            Err(DriverError::DirectoryCycle),
            "{variant:?}: cycle refused"
        );

        // Kind mismatches are refused.
        assert_eq!(
            fs.rename(two, b"victim", two, b"Inner"),
            Err(DriverError::Unsupported),
            "{variant:?}: file over directory"
        );

        // A destination naming the entry itself, spelled differently, is a
        // re-spelling; spelled the same, nothing changes.
        fs.rename(two, b"victim", two, b"VICTIM").expect("re-spell");
        let names: Vec<Vec<u8>> = list(&mut fs, two)
            .into_iter()
            .map(|(name, _, _)| name)
            .collect();
        assert!(
            names.contains(&b"VICTIM".to_vec()),
            "{variant:?}: {names:?}"
        );
        assert!(
            !names.contains(&b"victim".to_vec()),
            "{variant:?}: {names:?}"
        );
        let node = fs.lookup(two, b"victim").expect("found by either spelling");
        assert_eq!(read_all(&mut fs, node), b"payload", "{variant:?}");
        fs.rename(two, b"VICTIM", two, b"VICTIM").expect("no-op");
    }
}

/// Replacing a sibling whose name differs only in case leaves the moved
/// object under the spelling asked for.
#[test]
fn a_replacing_rename_takes_the_spelling_asked_for() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        make(&mut fs, root, b"a", b"mover");
        make(&mut fs, root, b"b", b"victim");
        fs.rename(root, b"a", root, b"B").expect("replace");
        let names: Vec<Vec<u8>> = list(&mut fs, root)
            .into_iter()
            .map(|(name, _, _)| name)
            .collect();
        assert_eq!(names, [b"B".to_vec()], "{variant:?}");
        let node = fs.lookup(root, b"b").expect("kept");
        assert_eq!(read_all(&mut fs, node), b"mover", "{variant:?}");
    }
}

#[test]
fn growth_survives_a_blocking_neighbour() {
    // Force the relocation path: A is pinned behind B, so growing A
    // cannot extend in place.
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let a: Vec<u8> = (0..4000u32).map(|i| (i % 197) as u8).collect();
        make(&mut fs, root, b"A", &a);
        make(&mut fs, root, b"B", &vec![0xBB; 4000]);
        let grown: Vec<u8> = (0..90_000u32).map(|i| (i % 193) as u8).collect();
        let node = fs.lookup(root, b"A").expect("A");
        assert_eq!(
            fs.write_at(node, 0, &grown).expect("grow A"),
            grown.len(),
            "{variant:?}"
        );
        let node = fs.lookup(root, b"A").expect("A");
        assert_eq!(read_all(&mut fs, node), grown, "{variant:?}: A grown");
        let node = fs.lookup(root, b"B").expect("B");
        assert_eq!(
            read_all(&mut fs, node),
            vec![0xBB; 4000],
            "{variant:?}: B intact"
        );
    }
}

#[test]
fn filling_the_volume_fails_closed_and_recovers() {
    let (variant, bytes) = VARIANTS[0]; // The smallest volume (S).
    let mut fs = fresh(variant, bytes);
    let root = FilesystemRead::root(&fs);
    let chunk = vec![0x5A; 32 * 1024];
    let mut created = 0u32;
    loop {
        let mut name = *b"Fill0";
        name[4] = b'0' + (created % 10) as u8;
        let file = fs
            .create(root, &name, NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
        match fs.write_at(file, 0, &chunk) {
            Ok(len) => assert_eq!(len, chunk.len()),
            Err(DriverError::NoSpace) => break,
            Err(other) => panic!("unexpected error: {other:?}"),
        }
        created += 1;
        assert!(created < 10, "volume never filled");
    }
    // The volume still validates and recovers space on delete.
    let mut name = *b"Fill0";
    name[4] = b'0';
    fs.remove(root, &name).expect("remove");
    make(&mut fs, root, b"After", b"still works");
    let node = fs.lookup(root, b"After").expect("post-full create");
    assert_eq!(read_all(&mut fs, node), b"still works");
}

#[test]
fn invalid_names_are_rejected() {
    for (variant, bytes) in [VARIANTS[0], VARIANTS[4], VARIANTS[5]] {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        for bad in [
            &b"has space"[..],
            b"dot.name",
            b"star*",
            b"hash#",
            b"quo\"te",
        ] {
            assert_eq!(
                fs.create(root, bad, NodeKind::RegularFile, &FILE_RECORD),
                Err(DriverError::OutOfRange),
                "{variant:?}: {bad:?}"
            );
        }
        assert_eq!(
            fs.create(root, b"", NodeKind::RegularFile, &FILE_RECORD),
            Err(DriverError::LengthOutOfRange),
            "{variant:?}: empty name"
        );
        let long = [b'x'; 300];
        assert_eq!(
            fs.create(root, &long, NodeKind::RegularFile, &FILE_RECORD),
            Err(DriverError::LengthOutOfRange),
            "{variant:?}: oversized name"
        );
        if fs.is_big_dir() {
            fs.create(
                root,
                b"ALongBigDirectoryName",
                NodeKind::RegularFile,
                &FILE_RECORD,
            )
            .expect("big directories take long names");
        } else {
            assert_eq!(
                fs.create(root, b"ElevenChars", NodeKind::RegularFile, &FILE_RECORD),
                Err(DriverError::LengthOutOfRange),
                "{variant:?}: fixed directories cap names at ten bytes"
            );
        }
    }
}

#[test]
fn big_directories_grow_and_survive_remount() {
    for (variant, bytes) in [VARIANTS[5], VARIANTS[7], VARIANTS[9]] {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        // Enough long-named entries to overflow the initial 2048-byte
        // directory several times over.
        let count = 120u32;
        for i in 0..count {
            let mut name = *b"AVeryLongObjectName-000";
            name[20] = b'0' + (i / 100 % 10) as u8;
            name[21] = b'0' + (i / 10 % 10) as u8;
            name[22] = b'0' + (i % 10) as u8;
            make(&mut fs, root, &name, &i.to_le_bytes());
        }
        assert_eq!(list(&mut fs, root).len(), count as usize, "{variant:?}");
        // The grown root (its size now lives in the disc record)
        // survives a remount, and every entry still resolves.
        let mut fs = Adfs::open(fs.volume.into_inner()).expect("remount");
        let root = FilesystemRead::root(&fs);
        for i in 0..count {
            let mut name = *b"AVeryLongObjectName-000";
            name[20] = b'0' + (i / 100 % 10) as u8;
            name[21] = b'0' + (i / 10 % 10) as u8;
            name[22] = b'0' + (i % 10) as u8;
            let node = fs.lookup(root, &name).expect("entry survives");
            assert_eq!(read_all(&mut fs, node), i.to_le_bytes(), "{variant:?}");
        }
        // Removal keeps the heap and pointers coherent.
        for i in (0..count).step_by(2) {
            let mut name = *b"AVeryLongObjectName-000";
            name[20] = b'0' + (i / 100 % 10) as u8;
            name[21] = b'0' + (i / 10 % 10) as u8;
            name[22] = b'0' + (i % 10) as u8;
            fs.remove(root, &name).expect("remove");
        }
        assert_eq!(list(&mut fs, root).len(), count as usize / 2, "{variant:?}");
    }
}

/// Format a variant, populate it lightly, and hand back the raw image.
fn image_of(variant: AdfsVariant, bytes: usize) -> Vec<u8> {
    let mut fs = fresh(variant, bytes);
    let root = FilesystemRead::root(&fs);
    make(&mut fs, root, b"File", b"survivor");
    fs.create(root, b"Dir", NodeKind::Directory, &DIR_RECORD)
        .expect("dir");
    fs.volume.into_inner().data
}

/// Reopen an image, asserting the driver rejects it as corrupt.
fn assert_rejected(variant: AdfsVariant, data: Vec<u8>, what: &str) {
    let device = MemDisk {
        data,
        block_size: 512,
        reads: std::sync::Arc::default(),
    };
    match Adfs::open(device) {
        Err(DriverError::BadMagic) => {}
        other => panic!(
            "{variant:?}: {what}: expected BadMagic, got {other:?}",
            other = other.map(|_| "Ok")
        ),
    }
}

#[test]
fn corrupt_old_map_checksums_are_rejected() {
    for variant in [
        AdfsVariant::S,
        AdfsVariant::M,
        AdfsVariant::L,
        AdfsVariant::D,
    ] {
        let bytes = usize::try_from(variant.fixed_size().expect("floppy")).expect("fits");
        // Sector 0 checksum (free-space starts).
        let mut data = image_of(variant, bytes);
        data[0x40] ^= 0x01;
        assert_rejected(variant, data, "sector 0 corruption");
        // Sector 1 checksum (free-space lengths).
        let mut data = image_of(variant, bytes);
        data[0x140] ^= 0x01;
        assert_rejected(variant, data, "sector 1 corruption");
        // A free-space entry pointing past the disc.
        let mut data = image_of(variant, bytes);
        data[0x00] = 0xFF;
        data[0x01] = 0xFF;
        data[0x02] = 0x1F;
        assert_rejected(variant, data, "out-of-range free area");
    }
}

#[test]
fn corrupt_directories_are_rejected() {
    // The root's check byte, marker, and sequence bytes are validated
    // on every load; damage to any of them refuses the volume.
    for (variant, offset) in [
        (AdfsVariant::S, 0x200usize), // root at sector 2
        (AdfsVariant::D, 0x400),      // root at sector 4
    ] {
        let bytes = usize::try_from(variant.fixed_size().expect("floppy")).expect("fits");
        // Marker byte.
        let mut data = image_of(variant, bytes);
        data[offset + 1] = b'X';
        assert_rejected(variant, data, "root marker");
        // A name byte inside a live entry breaks the check byte.
        let mut data = image_of(variant, bytes);
        data[offset + 5] ^= 0x01;
        assert_rejected(variant, data, "root entry bytes");
        // Mismatched head/tail sequence numbers.
        let mut data = image_of(variant, bytes);
        data[offset] = data[offset].wrapping_add(1);
        assert_rejected(variant, data, "sequence mismatch");
    }
}

#[test]
fn corrupt_new_map_zones_are_rejected() {
    // E: single zone at the disc start.
    let bytes = 800 * 1024;
    let mut data = image_of(AdfsVariant::E, bytes);
    data[0x40] ^= 0x01; // Map bits: breaks the zone check.
    assert_rejected(AdfsVariant::E, data, "zone check");

    // The cross-check byte of zone 0 must XOR with the others to 0xFF.
    let mut data = image_of(AdfsVariant::E, bytes);
    let map_at = 0usize;
    data[map_at + 3] ^= 0x10;
    assert_rejected(AdfsVariant::E, data, "cross-check");

    // A disc record claiming an impossible geometry.
    let mut data = image_of(AdfsVariant::E, bytes);
    data[4] = 42; // log2secsize
    assert_rejected(AdfsVariant::E, data, "impossible disc record");
}

#[test]
fn corrupt_boot_blocks_fall_back_and_fail_closed() {
    // F carries its disc record in the boot block; damaging the record
    // while fixing the checksum must still be rejected (zone checks),
    // and damaging the checksum makes the volume unrecognisable.
    let bytes = 1600 * 1024;
    let mut data = image_of(AdfsVariant::F, bytes);
    data[0xC00 + 0x1FF] ^= 0xFF; // Checksum byte.
    assert_rejected(AdfsVariant::F, data, "boot block checksum");

    let mut data = image_of(AdfsVariant::F, bytes);
    // Point the root at the free-space id, re-fixing the checksum so
    // only the record's content is wrong.
    data[0xC00 + 0x1C0 + 0x0C] = 0x00;
    data[0xC00 + 0x1C0 + 0x0D] = 0x00;
    let mut block = [0u8; 512];
    block.copy_from_slice(&data[0xC00..0xE00]);
    block[511] = 0;
    let sum = tairix_fsprobe::adfs::boot_block_checksum(&block);
    data[0xC00 + 0x1FF] = sum;
    assert_rejected(AdfsVariant::F, data, "hostile disc record");
}

#[test]
fn corrupt_big_directories_are_rejected() {
    let bytes = 800 * 1024;
    let base = image_of(AdfsVariant::EPlus, bytes);
    // Find the root directory ("SBPr") in the image.
    let root_at = base
        .windows(4)
        .position(|w| w == b"SBPr")
        .expect("root header present")
        - 4;

    // Header marker.
    let mut data = base.clone();
    data[root_at + 4] = b'Z';
    assert_rejected(AdfsVariant::EPlus, data, "big dir start marker");

    // Version bytes must be zero.
    let mut data = base.clone();
    data[root_at + 1] = 9;
    assert_rejected(AdfsVariant::EPlus, data, "big dir version");

    // An entry count far past what the directory can hold.
    let mut data = base.clone();
    data[root_at + 16] = 0xFF;
    data[root_at + 17] = 0xFF;
    assert_rejected(AdfsVariant::EPlus, data, "big dir entry count");

    // A name heap byte breaks the check byte.
    let mut data = base.clone();
    data[root_at + 0x1C] ^= 0x01;
    assert_rejected(AdfsVariant::EPlus, data, "big dir name heap");

    // Tail marker ("oven" at the directory end).
    let oven_at = base
        .windows(4)
        .position(|w| w == b"oven")
        .expect("root tail present");
    let mut data = base.clone();
    data[oven_at] = b'!';
    assert_rejected(AdfsVariant::EPlus, data, "big dir tail marker");
}

#[test]
fn corrupt_entry_pointers_fail_closed_at_use() {
    // An entry whose indirect address points at the free-space id: the
    // volume opens (the directory is structurally intact only if the
    // check byte matches, so rewrite it), and using the entry fails
    // closed instead of reading arbitrary bytes.
    let mut fs = fresh(AdfsVariant::E, 800 * 1024);
    let root = FilesystemRead::root(&fs);
    make(&mut fs, root, b"File", b"data");
    // Forge the entry through the driver's own directory codec so the
    // check byte stays valid.
    let (index, mut object) = fs
        .dir_lookup(node_addr(root), b"File")
        .expect("io")
        .expect("found");
    object.indaddr = 0x00F0 << 8; // An id the map never allocated.
    fs.dir_update_at(node_addr(root), index, &object)
        .expect("forge");
    let node = fs.lookup(root, b"File").expect("entry resolves");
    assert_eq!(
        fs.read_at(node, 0, &mut [0u8; 16]),
        Err(DriverError::BadMagic),
        "dangling indirect address fails closed"
    );
}

/// `get_attr` into a `Vec`, growing the buffer to the returned length.
fn attr(fs: &mut Adfs<MemDisk>, node: NodeId, key: &[u8]) -> Option<Vec<u8>> {
    let mut buf = [0u8; 64];
    fs.get_attr(node, key, &mut buf)
        .expect("get_attr")
        .map(|len| buf[..len].to_vec())
}

/// `fs`, stamping `wall` from now on, or no time.
fn at(mut fs: Adfs<MemDisk>, wall: Option<Time64>) -> Adfs<MemDisk> {
    fs.set_volume_host(TestClock::volume(), TestClock::at(wall));
    fs
}

/// `time` to the centisecond RISC OS stamps hold.
fn to_centisecond(time: Time64) -> Time64 {
    Time64::new(time.secs(), time.subsec_nanos() / 10_000_000 * 10_000_000).expect("canonical")
}

#[test]
fn a_new_object_is_typed_and_stamped_and_a_write_restamps_it() {
    let made = Time64::new(1_792_067_697, 123_456_789).expect("canonical");
    let later = Time64::new(1_792_067_797, 987_654_321).expect("canonical");
    for (variant, bytes) in VARIANTS {
        let mut fs = at(fresh(variant, bytes), Some(made));
        let root = FilesystemRead::root(&fs);
        fs.create(root, b"File", NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
        let node = fs.lookup(root, b"File").expect("found");
        assert_eq!(
            attr(&mut fs, node, b"acorn.filetype").as_deref(),
            Some(&b"ffd"[..])
        );
        assert_eq!(
            fs.node_info(node).expect("info").times.modified,
            to_centisecond(made),
            "{variant:?}"
        );
        fs.create(root, b"Dir", NodeKind::Directory, &DIR_RECORD)
            .expect("mkdir");
        let dir = fs.lookup(root, b"Dir").expect("found");
        assert_eq!(
            fs.node_info(dir).expect("info").times.modified,
            to_centisecond(made),
            "{variant:?}"
        );

        let mut fs = at(fs, Some(later));
        let node = fs.lookup(root, b"File").expect("found");
        fs.write_at(node, 0, b"body").expect("write");
        let node = fs.lookup(root, b"File").expect("found");
        assert_eq!(
            fs.node_info(node).expect("info").times.modified,
            to_centisecond(later),
            "{variant:?}"
        );

        let mut fs = at(fs, Some(made));
        fs.rename(root, b"File", root, b"Moved").expect("rename");
        let node = fs.lookup(root, b"Moved").expect("found");
        assert_eq!(
            fs.node_info(node).expect("info").times.modified,
            to_centisecond(later),
            "a rename moves the name, never the stamp ({variant:?})"
        );
    }
}

#[test]
fn an_untyped_objects_addresses_survive_a_write() {
    let (variant, bytes) = VARIANTS[0];
    let mut fs = at(
        fresh(variant, bytes),
        Some(Time64::from_secs(1_792_067_697)),
    );
    let root = FilesystemRead::root(&fs);
    let node = make(&mut fs, root, b"Image", b"code");
    fs.remove_attr(node, b"acorn.filetype").expect("untype");
    let node = fs.lookup(root, b"Image").expect("found");
    fs.set_attr(node, b"acorn.loadaddr", b"00008000")
        .expect("load");
    let node = fs.lookup(root, b"Image").expect("found");
    fs.set_attr(node, b"acorn.execaddr", b"00008000")
        .expect("exec");
    fs.write_at(node, 0, b"more code").expect("write");
    let node = fs.lookup(root, b"Image").expect("found");
    for key in [&b"acorn.loadaddr"[..], b"acorn.execaddr"] {
        assert_eq!(attr(&mut fs, node, key).as_deref(), Some(&b"00008000"[..]));
    }
}

#[test]
fn with_no_time_to_give_an_adfs_stamp_is_zero_and_reads_as_none() {
    let (variant, bytes) = VARIANTS[0];
    let mut fs = at(fresh(variant, bytes), None);
    let root = FilesystemRead::root(&fs);
    let node = make(&mut fs, root, b"File", b"body");
    assert_eq!(
        attr(&mut fs, node, b"acorn.datestamp").as_deref(),
        Some(&b"0000000000"[..])
    );
    assert_eq!(
        fs.node_info(node).expect("info").times,
        NodeTimes::default()
    );

    // 2255 is past what the 40-bit stamp holds: no stamp, never a wrapped one.
    let mut fs = at(fs, Some(Time64::from_secs(9_000_000_000)));
    fs.write_at(node, 0, b"more").expect("write");
    let node = fs.lookup(root, b"File").expect("found");
    assert_eq!(
        attr(&mut fs, node, b"acorn.datestamp").as_deref(),
        Some(&b"0000000000"[..])
    );
}

#[test]
fn acorn_metadata_round_trips() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let node = make(&mut fs, root, b"Typed", b"body");

        // A fresh file is Data, as RISC OS makes one, and with no clock to
        // stamp it from it carries the zero stamp, which reads as no time.
        assert_eq!(
            attr(&mut fs, node, b"acorn.loadaddr").as_deref(),
            Some(&b"fffffd00"[..])
        );
        assert_eq!(
            attr(&mut fs, node, b"acorn.filetype").as_deref(),
            Some(&b"ffd"[..])
        );
        assert_eq!(
            attr(&mut fs, node, b"acorn.datestamp").as_deref(),
            Some(&b"0000000000"[..])
        );
        assert_eq!(
            attr(&mut fs, node, b"acorn.attr").as_deref(),
            Some(&b"RW/"[..])
        );
        assert_eq!(
            fs.node_info(node).expect("info").times,
            NodeTimes::default()
        );

        // Typing the file, then stamping it, round-trips exactly.
        fs.set_attr(node, b"acorn.filetype", b"ffb")
            .expect("filetype");
        fs.set_attr(node, b"acorn.datestamp", b"00a1b2c3d4")
            .expect("stamp");
        assert_eq!(
            attr(&mut fs, node, b"acorn.filetype").as_deref(),
            Some(&b"ffb"[..])
        );
        assert_eq!(
            attr(&mut fs, node, b"acorn.datestamp").as_deref(),
            Some(&b"00a1b2c3d4"[..]),
            "{variant:?}"
        );
        // The stamp now decodes as the node's times and listing stamp.
        let stamp = acorn::centiseconds_to_time64(0x00_A1B2_C3D4).expect("in range");
        assert_eq!(fs.node_info(node).expect("info").times.modified, stamp);

        // Attribute letters round-trip within the format's storable set.
        fs.set_attr(node, b"acorn.attr", b"RWL/r").expect("attr");
        assert_eq!(
            attr(&mut fs, node, b"acorn.attr").as_deref(),
            Some(&b"RWL/r"[..])
        );
        // The directory bit cannot be forged onto a file.
        assert_eq!(
            fs.set_attr(node, b"acorn.attr", b"RWD/"),
            Err(DriverError::OutOfRange),
            "{variant:?}"
        );

        // Raw addresses are settable on untyped objects and reported
        // exactly.
        fs.remove_attr(node, b"acorn.filetype").expect("untype");
        assert_eq!(attr(&mut fs, node, b"acorn.filetype"), None);
        fs.set_attr(node, b"acorn.loadaddr", b"0000eafe")
            .expect("load");
        fs.set_attr(node, b"acorn.execaddr", b"0000eb00")
            .expect("exec");
        assert_eq!(
            attr(&mut fs, node, b"acorn.loadaddr").as_deref(),
            Some(&b"0000eafe"[..])
        );
        assert_eq!(
            attr(&mut fs, node, b"acorn.execaddr").as_deref(),
            Some(&b"0000eb00"[..])
        );

        // Enumeration lists exactly the present keys.
        let mut keys = Vec::new();
        let mut index = 0u64;
        let mut key_buf = [0u8; 32];
        while let Some(len) = fs.list_attr(node, index, &mut key_buf).expect("list") {
            keys.push(key_buf[..len].to_vec());
            index += 1;
        }
        assert_eq!(
            keys,
            vec![
                b"acorn.loadaddr".to_vec(),
                b"acorn.execaddr".to_vec(),
                b"acorn.attr".to_vec()
            ],
            "{variant:?}"
        );

        // Foreign namespaces have nowhere to live; malformed keys and
        // values fail closed.
        assert_eq!(attr(&mut fs, node, b"user.comment"), None);
        assert_eq!(
            fs.set_attr(node, b"user.comment", b"x"),
            Err(DriverError::Unsupported)
        );
        assert_eq!(
            fs.get_attr(node, b"not-a-key", &mut [0u8; 8]),
            Err(DriverError::OutOfRange)
        );
        assert_eq!(
            fs.set_attr(node, b"acorn.filetype", b"zz"),
            Err(DriverError::OutOfRange)
        );
        assert_eq!(
            fs.get_attr(node, b"acorn.loadaddr", &mut [0u8; 4]),
            Err(DriverError::BufferTooSmall)
        );
    }
}

#[test]
fn register_requires_drv_load() {
    assert!(register(&MockHost { drv_load: true }).is_ok());
    assert_eq!(
        register(&MockHost { drv_load: false }),
        Err(DriverError::PermissionDenied)
    );
}

#[test]
fn format_open_roundtrip_every_variant() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        assert!(node_is_dir(root), "{variant:?}: root is a directory");
        // A fresh volume has an empty root and sane space accounting.
        assert!(list(&mut fs, root).is_empty(), "{variant:?}: empty root");
        let stats = fs.stats().expect("stats");
        assert!(stats.total_blocks > 0, "{variant:?}: total space");
        assert!(
            stats.free_blocks > 0 && stats.free_blocks <= stats.total_blocks,
            "{variant:?}: free within total"
        );
        assert_eq!(stats.avail_blocks, stats.free_blocks);

        // Create, write, and read back a file whose body crosses the
        // staging-buffer and allocation-unit boundaries.
        let body: Vec<u8> = (0..9000u32).map(|i| (i * 31 % 251) as u8).collect();
        let node = make(&mut fs, root, b"Data", &body);
        assert_eq!(read_all(&mut fs, node), body, "{variant:?}: body");
        let info = fs.node_info(node).expect("info");
        assert_eq!(info.size, body.len() as u64);
        assert!(
            info.allocated >= info.size,
            "{variant:?}: allocation covers the data"
        );

        // Lookups are case-insensitive, misses fail closed.
        assert_eq!(fs.lookup(root, b"dAtA").expect("case fold"), node);
        assert_eq!(fs.lookup(root, b"Other"), Err(DriverError::NotFound));

        // The listing reflects the file, and reopening the same device
        // sees the identical volume.
        let listed = list(&mut fs, root);
        assert_eq!(listed, vec![(b"Data".to_vec(), false, body.len() as u64)]);
        let device = fs.volume.into_inner();
        let mut reopened = Adfs::open(device).expect("reopen");
        let node = reopened
            .lookup(FilesystemRead::root(&reopened), b"Data")
            .expect("file survives remount");
        assert_eq!(read_all(&mut reopened, node), body, "{variant:?}: remount");
    }
}

#[test]
fn directories_nest_and_list_sorted() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let sub = fs
            .create(root, b"Sub", NodeKind::Directory, &DIR_RECORD)
            .expect("mkdir");
        assert!(node_is_dir(sub));
        make(&mut fs, sub, b"inner", b"inner body");
        // Insertion keeps the case-insensitive sort order.
        make(&mut fs, root, b"zz", b"");
        make(&mut fs, root, b"AA", b"");
        let names: Vec<Vec<u8>> = list(&mut fs, root).into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(
            names,
            vec![b"AA".to_vec(), b"Sub".to_vec(), b"zz".to_vec()],
            "{variant:?}: sorted listing"
        );
        // The nested file resolves through its directory.
        let inner = fs.lookup(sub, b"INNER").expect("nested lookup");
        assert_eq!(read_all(&mut fs, inner), b"inner body");
        // A directory cannot be read as a file, nor a file listed.
        assert_eq!(
            fs.read_at(sub, 0, &mut [0u8; 8]),
            Err(DriverError::Unsupported),
            "{variant:?}: read_at on dir"
        );
        let file = fs.lookup(sub, b"inner").expect("file");
        assert_eq!(listed(&mut fs, file, 0, &[]), Err(DriverError::Unsupported));
    }
}

#[test]
fn adfs_has_no_link_of_either_kind_and_reports_one_name() {
    // FileCore has no link object type at all: a directory entry carries the
    // object's own load/exec/length, so a second name would be a second
    // object. Both link calls refuse rather than approximating one.
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let file = make(&mut fs, root, b"File", b"body");
        assert_eq!(
            fs.link(root, b"Alias", file),
            Err(DriverError::Unsupported),
            "{variant:?}"
        );
        assert_eq!(
            fs.create_link(root, b"Alias", b"$.File", &LINK_RECORD),
            Err(DriverError::Unsupported),
            "{variant:?}"
        );
        assert_eq!(fs.lookup(root, b"Alias"), Err(DriverError::NotFound));
        assert_eq!(fs.node_info(file).expect("stat").nlink, 1);
        assert_eq!(fs.node_info(root).expect("stat").nlink, 1);
    }
}

/// A stat reads the one directory holding the node, never the tree: a
/// volume whose every directory is wide costs a deep node's stat no more
/// than listing its parent does.
#[test]
fn a_stat_reads_its_parent_alone() {
    let disk = MemDisk::new(1600 * 1024, 512);
    let reads = std::sync::Arc::clone(&disk.reads);
    let mut fs = Adfs::format(disk, AdfsVariant::FPlus).expect("format");
    let root = FilesystemRead::root(&fs);
    let mut deep = root;
    for level in 0..6u8 {
        for wide in 0..12u8 {
            make(&mut fs, deep, &[b'F', b'a' + level, b'a' + wide], b"x");
        }
        deep = fs
            .create(
                deep,
                &[b'D', b'a' + level],
                NodeKind::Directory,
                &DIR_RECORD,
            )
            .expect("dir");
    }
    let target = make(&mut fs, deep, b"Target", b"payload");
    let device = fs.volume.into_inner();
    let mut fs = Adfs::open(device).expect("reopen");
    reads.store(0, core::sync::atomic::Ordering::Relaxed);
    let _ = list(&mut fs, deep);
    let listing = reads.swap(0, core::sync::atomic::Ordering::Relaxed);
    let info = fs.node_info(target).expect("stat");
    let stat = reads.load(core::sync::atomic::Ordering::Relaxed);
    assert_eq!(info.size, 7);
    assert!(
        stat <= listing + 8,
        "a stat read {stat} blocks; listing the parent reads {listing}"
    );
}

/// A directory's node is its address and its parent's, so growing it keeps
/// the node a listing of it was bound to.
#[test]
fn a_directory_keeps_its_node_as_it_grows() {
    let mut fs = fresh(AdfsVariant::EPlus, 800 * 1024);
    let root = FilesystemRead::root(&fs);
    let dir = fs
        .create(root, b"Grows", NodeKind::Directory, &DIR_RECORD)
        .expect("dir");
    let before = fs.lookup(root, b"Grows").expect("found");
    assert_eq!(before, dir);
    for i in 0..120u32 {
        let name = std::format!("Entry{i:03}");
        fs.create(dir, name.as_bytes(), NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
    }
    assert_eq!(fs.lookup(root, b"Grows").expect("found"), before);
    assert_eq!(list(&mut fs, dir).len(), 120);
}

/// Objects with no allocation share address 0, so each is told apart by its
/// name: two empty files are two nodes, and setting one's metadata leaves
/// the other's alone.
#[test]
fn empty_files_are_distinct_nodes() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let first = make(&mut fs, root, b"Empty1", b"");
        let second = make(&mut fs, root, b"Empty2", b"");
        assert_ne!(first, second, "{variant:?}");
        let set = acorn::addr_to_value(0x1234_5678);
        let before = attr(&mut fs, second, b"acorn.loadaddr");
        assert_ne!(before, Some(set.to_vec()), "{variant:?}");
        fs.set_attr(first, b"acorn.loadaddr", &set).expect("set");
        assert_eq!(
            attr(&mut fs, first, b"acorn.loadaddr"),
            Some(set.to_vec()),
            "{variant:?}"
        );
        assert_eq!(
            attr(&mut fs, second, b"acorn.loadaddr"),
            before,
            "{variant:?}"
        );
        assert_eq!(fs.read_at(second, 0, &mut [0u8; 4]), Ok(0));
    }
}

/// An empty file is named by its name's digest until a write gives it an
/// allocation, and by that address from then on: the write moves its node.
#[test]
fn a_first_write_moves_an_empty_files_node_to_its_address() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let empty = fs
            .create(root, b"Grows", NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
        fs.write_at(empty, 0, b"body").expect("write");
        let written = fs.lookup(root, b"Grows").expect("found");
        assert_ne!(written, empty, "{variant:?}");
        assert_eq!(
            fs.node_info(empty),
            Err(DriverError::NotFound),
            "{variant:?}"
        );
        assert_eq!(
            fs.write_at(empty, 0, b"x"),
            Err(DriverError::NotFound),
            "{variant:?}"
        );
        assert_eq!(read_all(&mut fs, written), b"body", "{variant:?}");
        assert_eq!(
            fs.node_info(written).expect("info").ino,
            written.raw(),
            "{variant:?}"
        );
    }
}

/// Two empty siblings whose names share a digest share a node, which cannot
/// tell them apart: a write by it is refused rather than guessed.
#[test]
fn a_node_two_empty_siblings_share_takes_no_write() {
    let mut seen = std::collections::HashMap::new();
    let (first, second) = (0u32..)
        .map(|i| std::format!("N{i:05X}").into_bytes())
        .find_map(|name| {
            seen.insert(name_digest(&name), name.clone())
                .map(|other| (other, name))
        })
        .expect("names whose digests collide");
    let (variant, bytes) = VARIANTS[0];
    let mut fs = fresh(variant, bytes);
    let root = FilesystemRead::root(&fs);
    let a = fs
        .create(root, &first, NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let b = fs
        .create(root, &second, NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    assert_eq!(a, b, "one node names both");
    assert_eq!(fs.write_at(a, 0, b"which?"), Err(DriverError::Unsupported));
    assert_eq!(fs.truncate(a, 1), Err(DriverError::Unsupported));
    assert_eq!(
        fs.pin(a),
        Err(DriverError::Unsupported),
        "nor held for either"
    );
    assert_eq!(fs.read_at(a, 0, &mut [0u8; 4]), Ok(0), "both hold no bytes");
}

/// A directory's parent is the directory whose entry holds it, the root's is
/// the root, and a removed directory has none.
#[test]
fn a_directorys_parent_is_the_directory_holding_its_entry() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        assert_eq!(fs.parent(root), Ok(root), "{variant:?}");
        let outer = fs
            .create(root, b"Outer", NodeKind::Directory, &DIR_RECORD)
            .expect("mkdir");
        let inner = fs
            .create(outer, b"Inner", NodeKind::Directory, &DIR_RECORD)
            .expect("mkdir");
        assert_eq!(fs.parent(outer), Ok(root), "{variant:?}");
        assert_eq!(fs.parent(inner), Ok(outer), "{variant:?}");
        let file = make(&mut fs, outer, b"File", b"x");
        assert_eq!(
            fs.parent(file),
            Err(DriverError::Unsupported),
            "{variant:?}"
        );
        fs.remove(outer, b"Inner").expect("rmdir");
        assert_eq!(fs.parent(inner), Err(DriverError::NotFound), "{variant:?}");
    }
}

/// `FileCore` folds names to upper case, so `_` sorts after every letter.
#[test]
fn names_sort_as_filecore_folds_them() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        make(&mut fs, root, b"A_B", b"1");
        make(&mut fs, root, b"AB", b"2");
        make(&mut fs, root, b"aa", b"3");
        let names: Vec<Vec<u8>> = list(&mut fs, root).into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(
            names,
            [b"aa".to_vec(), b"AB".to_vec(), b"A_B".to_vec()],
            "{variant:?}"
        );
    }
}

/// RISC OS finds names by binary search and reports a directory out of
/// order as broken; so does the driver, rather than resume a listing wrongly
/// in it.
#[test]
fn a_directory_out_of_order_is_rejected() {
    let mut fs = fresh(AdfsVariant::E, 800 * 1024);
    let root = FilesystemRead::root(&fs);
    make(&mut fs, root, b"QQQQ1", b"1");
    make(&mut fs, root, b"QQQQ2", b"2");
    let image = fs.volume.into_inner().data;
    let first = image
        .windows(5)
        .position(|w| w == b"QQQQ1")
        .expect("entry present");
    let dir_start = first - dir::ENTRIES_OFFSET;
    let check_at = dir_start + dir::NEW_DIR_SIZE - 1;
    let unchecked = |mut data: Vec<u8>| {
        data[check_at] = 0;
        data
    };
    let in_order = MemDisk {
        data: unchecked(image.clone()),
        block_size: 512,
        reads: std::sync::Arc::default(),
    };
    assert!(
        Adfs::open(in_order).is_ok(),
        "an unchecked sorted directory opens"
    );
    let mut swapped = unchecked(image);
    let (a, b) = swapped[first..first + 2 * dir::ENTRY_SIZE].split_at_mut(dir::ENTRY_SIZE);
    a.swap_with_slice(b);
    assert_rejected(AdfsVariant::E, swapped, "entries out of order");
}

/// A fragment two files share is freed with the last of them: the other
/// sharers can only be its parent's entries, which is all that is read.
#[test]
fn a_shared_fragment_is_freed_with_its_last_sharer() {
    let mut fs = fresh(AdfsVariant::E, 800 * 1024);
    let root = FilesystemRead::root(&fs);
    make(&mut fs, root, b"Holder", b"holder bytes");
    make(&mut fs, root, b"Sharer", b"");
    let free_before = fs.stats().expect("stats").free_blocks;
    let (holder_at, mut holder) = fs
        .dir_lookup(node_addr(root), b"Holder")
        .expect("io")
        .expect("found");
    let fragment = holder.indaddr & !0xFF;
    holder.indaddr = fragment | 1;
    fs.dir_update_at(node_addr(root), holder_at, &holder)
        .expect("share");
    let (sharer_at, mut sharer) = fs
        .dir_lookup(node_addr(root), b"Sharer")
        .expect("io")
        .expect("found");
    sharer.indaddr = fragment | 2;
    sharer.size = 4;
    fs.dir_update_at(node_addr(root), sharer_at, &sharer)
        .expect("share");
    fs.remove(root, b"Sharer").expect("remove a sharer");
    assert_eq!(
        fs.stats().expect("stats").free_blocks,
        free_before,
        "the holder still uses the fragment"
    );
    fs.remove(root, b"Holder").expect("remove the last sharer");
    assert!(fs.stats().expect("stats").free_blocks > free_before);
}

/// On a new-format directory the attribute byte is `FileCore`'s: public
/// read and write are bits 4 and 5 there, not the canonical encoding's 5 and
/// 6. A file a RISC OS machine made publicly readable must read back as `r`,
/// and one set publicly writable here must be so on the disc RISC OS reads.
#[test]
fn public_access_bits_land_where_filecore_keeps_them() {
    for (variant, bytes) in [
        (AdfsVariant::E, 800 * 1024),
        (AdfsVariant::F, 1600 * 1024),
        (AdfsVariant::HardDisc, 4 * 1024 * 1024),
    ] {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let node = make(&mut fs, root, b"Shared", b"body");
        fs.set_attr(node, b"acorn.attr", b"RW/rw").expect("attr");
        let mut image = fs.volume.into_inner().data;
        let entry = image
            .windows(7)
            .position(|w| w == b"Shared\r")
            .unwrap_or_else(|| panic!("{variant:?}: the entry is on the disc"));
        assert_eq!(image[entry + 25], 0b0011_0011, "{variant:?}: R W r w");

        // A byte a RISC OS machine wrote: owner read, locked, public read.
        // The entry is the root's first, so the directory opens five bytes
        // before it; a zero check byte is one the format does not check.
        image[entry + 25] = 0b0001_0101;
        image[entry - 5 + dir::NEW_DIR_SIZE - 1] = 0;
        let mut disk = MemDisk::new(0, 512);
        disk.data = image;
        let mut fs = Adfs::open(disk).expect("reopen");
        let root = FilesystemRead::root(&fs);
        let node = fs.lookup(root, b"Shared").expect("lookup");
        assert_eq!(
            attr(&mut fs, node, b"acorn.attr").as_deref(),
            Some(&b"RL/r"[..]),
            "{variant:?}"
        );
        // The execute-only bit has no place in this format.
        assert_eq!(
            fs.set_attr(node, b"acorn.attr", b"RE/"),
            Err(DriverError::OutOfRange),
            "{variant:?}"
        );
    }
}

/// The volume's free blocks.
fn free_blocks(fs: &mut Adfs<MemDisk>) -> u64 {
    fs.stats().expect("stats").free_blocks
}

/// A pinned object is reported under its token, which lookups and listings
/// answer in place of its own id, while `stat` goes on reporting its own
/// number. The root is held under its own id, which never changes.
#[test]
fn a_pinned_object_is_reported_under_its_token() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let file = make(&mut fs, root, b"File", b"body");
        let dir = fs
            .create(root, b"Dir", NodeKind::Directory, &DIR_RECORD)
            .expect("mkdir");
        let pinned = fs.pin(file).expect("pin");
        let held = fs.pin(dir).expect("pin");
        assert_ne!(pinned, file, "{variant:?}");
        assert_ne!(held, dir, "{variant:?}");
        assert_eq!(fs.lookup(root, b"FILE"), Ok(pinned), "{variant:?}");
        assert_eq!(fs.lookup(root, b"dir"), Ok(held), "{variant:?}");
        let entries = listed(&mut fs, root, 0, &[]).expect("lists");
        let reported: Vec<(NodeId, u64)> = entries
            .iter()
            .map(|(entry, _)| (entry.node, entry.info.ino))
            .collect();
        assert_eq!(
            reported,
            [(held, dir.raw()), (pinned, file.raw())],
            "{variant:?}"
        );
        assert_eq!(fs.node_info(pinned).expect("info").ino, file.raw());
        let inner = fs
            .create(held, b"Inner", NodeKind::Directory, &DIR_RECORD)
            .expect("mkdir through the token");
        assert_eq!(fs.parent(inner), Ok(held), "{variant:?}");
        assert_eq!(fs.pin(root), Ok(root), "{variant:?}");
        assert_eq!(fs.parent(held), Ok(root), "{variant:?}");
        assert_eq!(fs.unpin(root), Ok(Unpinned::Released), "{variant:?}");
    }
}

/// Holds count: every hold of one object answers one id, whichever id it
/// was taken by, and each is released by one unpin.
#[test]
fn holds_count_and_every_hold_answers_one_id() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let file = make(&mut fs, root, b"File", b"body");
        let pinned = fs.pin(file).expect("pin");
        assert_eq!(fs.pin(file), Ok(pinned), "{variant:?}");
        assert_eq!(fs.pin(pinned), Ok(pinned), "{variant:?}");
        assert_eq!(fs.unpin(pinned), Ok(Unpinned::Held), "{variant:?}");
        assert_eq!(fs.unpin(pinned), Ok(Unpinned::Held), "{variant:?}");
        assert_eq!(fs.unpin(pinned), Ok(Unpinned::Released), "{variant:?}");
        assert_eq!(fs.unpin(pinned), Err(DriverError::NotFound), "{variant:?}");
        assert_eq!(fs.lookup(root, b"File"), Ok(file), "{variant:?}");
        for _ in 0..2 {
            assert_eq!(fs.pin(root), Ok(root), "{variant:?}");
        }
        assert_eq!(fs.unpin(root), Ok(Unpinned::Held), "{variant:?}");
        assert_eq!(fs.unpin(root), Ok(Unpinned::Released), "{variant:?}");
        assert_eq!(fs.unpin(root), Err(DriverError::NotFound), "{variant:?}");
    }
}

/// A hold nothing took cannot be released, and nothing but a live object
/// can be held.
#[test]
fn an_unpin_with_no_hold_is_not_found() {
    let (variant, bytes) = VARIANTS[4];
    let mut fs = fresh(variant, bytes);
    let root = FilesystemRead::root(&fs);
    let file = make(&mut fs, root, b"File", b"body");
    let forged = token_node(99, false);
    for never in [file, forged, NodeId::NONE] {
        assert_eq!(fs.unpin(never), Err(DriverError::NotFound), "{never:?}");
    }
    let pinned = fs.pin(file).expect("pin");
    assert_eq!(
        fs.unpin(file),
        Err(DriverError::NotFound),
        "a hold is released by the id it answered"
    );
    assert_eq!(fs.unpin(pinned), Ok(Unpinned::Released));
    for dead in [forged, NodeId::NONE] {
        assert_eq!(fs.pin(dead), Err(DriverError::NotFound), "{dead:?}");
    }
    fs.remove(root, b"File").expect("remove");
    assert_eq!(fs.pin(file), Err(DriverError::NotFound));
}

/// A removal deletes a pinned file's entry but keeps its fragments: the
/// file reads, writes, truncates and changes its metadata through its token,
/// stats with no name, and holds its space until the last unpin frees it.
#[test]
fn a_pinned_file_outlives_its_name_until_its_last_unpin() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let empty = free_blocks(&mut fs);
        let file = make(&mut fs, root, b"Kept", &vec![7u8; 3000]);
        let pinned = fs.pin(file).expect("pin");
        let held = free_blocks(&mut fs);
        fs.remove(root, b"Kept").expect("remove");
        assert_eq!(fs.lookup(root, b"Kept"), Err(DriverError::NotFound));
        assert_eq!(
            free_blocks(&mut fs),
            held,
            "{variant:?}: the fragments are kept"
        );

        let info = fs.node_info(pinned).expect("info");
        assert_eq!((info.nlink, info.size), (0, 3000), "{variant:?}");
        assert_eq!(read_all(&mut fs, pinned), vec![7u8; 3000], "{variant:?}");
        fs.write_at(pinned, 3000, &vec![9u8; 6000]).expect("write");
        let body = read_all(&mut fs, pinned);
        assert_eq!(body.len(), 9000, "{variant:?}");
        assert!(body[3000..].iter().all(|&b| b == 9), "{variant:?}");
        fs.truncate(pinned, 100).expect("truncate");
        assert_eq!(fs.node_info(pinned).expect("info").size, 100);
        assert_eq!(fs.security(pinned).map(|record| record.mode), Ok(0o644));
        fs.set_attr(pinned, b"acorn.filetype", b"fff")
            .expect("type");
        assert_eq!(
            attr(&mut fs, pinned, b"acorn.filetype").as_deref(),
            Some(&b"fff"[..]),
            "{variant:?}"
        );

        assert_eq!(fs.unpin(pinned), Ok(Unpinned::Freed), "{variant:?}");
        assert_eq!(free_blocks(&mut fs), empty, "{variant:?}: freed");
    }
}

/// Once its last unpin has freed it, a nameless object is gone to every
/// call, and so is a removed object's own id, an empty one's included.
#[test]
fn an_unpinned_nameless_object_is_not_found() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let file = make(&mut fs, root, b"File", b"body");
        let pinned = fs.pin(file).expect("pin");
        fs.remove(root, b"File").expect("remove");
        assert_eq!(fs.unpin(pinned), Ok(Unpinned::Freed), "{variant:?}");
        for gone in [pinned, file] {
            assert_eq!(
                fs.node_info(gone),
                Err(DriverError::NotFound),
                "{variant:?}"
            );
            assert_eq!(
                fs.read_at(gone, 0, &mut [0u8; 4]),
                Err(DriverError::NotFound),
                "{variant:?}"
            );
            assert_eq!(
                fs.security(gone).map(|_| ()),
                Err(DriverError::NotFound),
                "{variant:?}"
            );
            assert_eq!(fs.write_at(gone, 0, b"x"), Err(DriverError::NotFound));
            assert_eq!(fs.truncate(gone, 0), Err(DriverError::NotFound));
        }
        let empty = fs
            .create(root, b"Empty", NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
        fs.remove(root, b"Empty").expect("remove");
        assert_eq!(
            fs.read_at(empty, 0, &mut [0u8; 4]),
            Err(DriverError::NotFound),
            "{variant:?}"
        );
    }
}

/// A pinned object keeps its token through the writes that move its own id
/// and through renames of it and of its directory; a pinned directory keeps
/// its token through a move, and parents answer it.
#[test]
fn a_pinned_object_keeps_its_id_through_writes_and_renames() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let outer = fs
            .create(root, b"Outer", NodeKind::Directory, &DIR_RECORD)
            .expect("mkdir");
        let file = fs
            .create(outer, b"Grows", NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
        let pinned = fs.pin(file).expect("pin");
        let dir = fs.pin(outer).expect("pin");
        fs.write_at(pinned, 0, b"first")
            .expect("a first allocation");
        let (_, object) = fs
            .dir_lookup(node_addr(outer), b"Grows")
            .expect("io")
            .expect("found");
        assert_ne!(
            child_node(&object, node_addr(outer)),
            file,
            "{variant:?}: its own id moved"
        );
        assert_eq!(fs.lookup(dir, b"Grows"), Ok(pinned), "{variant:?}");
        make(&mut fs, dir, b"Block", &vec![0xBB; 4000]);
        let body: Vec<u8> = (0..9000u32).map(|i| (i % 199) as u8).collect();
        fs.write_at(pinned, 0, &body)
            .expect("grow past a neighbour");
        fs.rename(dir, b"Grows", dir, b"Renamed").expect("rename");
        fs.rename(root, b"Outer", root, b"Moved")
            .expect("rename the directory");
        assert_eq!(fs.lookup(root, b"Moved"), Ok(dir), "{variant:?}");
        assert_eq!(fs.lookup(dir, b"Renamed"), Ok(pinned), "{variant:?}");
        let other = fs
            .create(root, b"Other", NodeKind::Directory, &DIR_RECORD)
            .expect("mkdir");
        fs.rename(dir, b"Renamed", other, b"Here").expect("move");
        assert_eq!(fs.lookup(other, b"Here"), Ok(pinned), "{variant:?}");
        assert_eq!(fs.lookup(dir, b"Renamed"), Err(DriverError::NotFound));
        fs.rename(root, b"Moved", other, b"Inner")
            .expect("move the directory");
        assert_eq!(fs.lookup(other, b"Inner"), Ok(dir), "{variant:?}");
        assert_eq!(fs.parent(dir), Ok(other), "{variant:?}");
        assert_eq!(read_all(&mut fs, pinned), body, "{variant:?}");
        assert_eq!(fs.unpin(pinned), Ok(Unpinned::Released), "{variant:?}");
        assert_eq!(fs.unpin(dir), Ok(Unpinned::Released), "{variant:?}");
        assert_ne!(
            fs.lookup(other, b"Here"),
            Ok(pinned),
            "{variant:?}: unpinned, it answers its own id"
        );
    }
}

/// A rename replacing a pinned file deletes its entry as a removal does: the
/// pin keeps the replaced file's data until its last unpin frees it.
#[test]
fn a_rename_over_a_pinned_file_keeps_it_for_the_pin() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let victim = make(&mut fs, root, b"B", &vec![2u8; 5000]);
        make(&mut fs, root, b"A", b"mover");
        let pinned = fs.pin(victim).expect("pin");
        let free = free_blocks(&mut fs);
        fs.rename(root, b"A", root, b"B").expect("replace");
        assert_eq!(
            free_blocks(&mut fs),
            free,
            "{variant:?}: the replaced data is kept"
        );
        assert_eq!(read_all(&mut fs, pinned), vec![2u8; 5000], "{variant:?}");
        assert_eq!(fs.node_info(pinned).expect("info").nlink, 0);
        let named = fs.lookup(root, b"B").expect("found");
        assert_ne!(named, pinned, "{variant:?}");
        assert_eq!(read_all(&mut fs, named), b"mover", "{variant:?}");
        assert_eq!(fs.unpin(pinned), Ok(Unpinned::Freed), "{variant:?}");
        assert!(
            free_blocks(&mut fs) > free,
            "{variant:?}: freed at the last unpin"
        );
    }
}

/// A removed directory a pin holds keeps its allocation but takes no
/// entries: a lookup, a listing, its parent and a creation or move into it
/// find nothing, it stats with no name, and its last unpin frees it.
#[test]
fn a_removed_pinned_directory_takes_no_entries() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let file = make(&mut fs, root, b"File", b"body");
        let empty = free_blocks(&mut fs);
        let dir = fs
            .create(root, b"Gone", NodeKind::Directory, &DIR_RECORD)
            .expect("mkdir");
        let pinned = fs.pin(dir).expect("pin");
        let held = free_blocks(&mut fs);
        fs.remove(root, b"Gone").expect("rmdir");
        assert_eq!(
            free_blocks(&mut fs),
            held,
            "{variant:?}: its allocation is kept"
        );
        assert_eq!(fs.lookup(root, b"Gone"), Err(DriverError::NotFound));
        assert_eq!(
            fs.create(pinned, b"New", NodeKind::RegularFile, &FILE_RECORD),
            Err(DriverError::NotFound),
            "{variant:?}"
        );
        assert_eq!(
            fs.create(pinned, b"New", NodeKind::Directory, &DIR_RECORD),
            Err(DriverError::NotFound),
            "{variant:?}"
        );
        assert_eq!(
            fs.rename(root, b"File", pinned, b"File"),
            Err(DriverError::NotFound),
            "{variant:?}"
        );
        assert_eq!(fs.lookup(root, b"File"), Ok(file), "the refused move moved");
        assert_eq!(
            fs.link(pinned, b"Alias", file),
            Err(DriverError::Unsupported),
            "ADFS has no links at all"
        );
        assert_eq!(fs.lookup(pinned, b"Any"), Err(DriverError::NotFound));
        assert_eq!(listed(&mut fs, pinned, 0, &[]), Ok(Vec::new()));
        assert_eq!(fs.parent(pinned), Err(DriverError::NotFound));
        let info = fs.node_info(pinned).expect("info");
        assert_eq!((info.kind, info.nlink), (NodeKind::Directory, 0));
        assert_eq!(fs.unpin(pinned), Ok(Unpinned::Freed), "{variant:?}");
        assert_eq!(free_blocks(&mut fs), empty, "{variant:?}: freed");
        assert_eq!(fs.node_info(pinned), Err(DriverError::NotFound));
    }
}

/// Releasing every hold frees each nameless object held, and leaves a named
/// one as it was.
#[test]
fn release_pins_frees_every_nameless_object() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let empty = free_blocks(&mut fs);
        let file = make(&mut fs, root, b"File", &vec![1u8; 3000]);
        let dir = fs
            .create(root, b"Dir", NodeKind::Directory, &DIR_RECORD)
            .expect("mkdir");
        let named = make(&mut fs, root, b"Named", b"named body");
        let full = free_blocks(&mut fs);
        let held = [fs.pin(file), fs.pin(dir), fs.pin(named), fs.pin(root)]
            .map(|pinned| pinned.expect("pin"));
        fs.remove(root, b"File").expect("remove");
        fs.remove(root, b"Dir").expect("rmdir");
        assert_eq!(free_blocks(&mut fs), full, "{variant:?}");
        assert_eq!(fs.release_pins(), Ok(()), "{variant:?}");
        assert!(free_blocks(&mut fs) > full, "{variant:?}: freed");
        for released in held {
            assert_eq!(
                fs.unpin(released),
                Err(DriverError::NotFound),
                "{variant:?}"
            );
        }
        assert_eq!(fs.lookup(root, b"Named"), Ok(named), "{variant:?}");
        assert_eq!(read_all(&mut fs, named), b"named body", "{variant:?}");
        fs.remove(root, b"Named").expect("remove");
        assert_eq!(free_blocks(&mut fs), empty, "{variant:?}");
    }
}

/// No pin outlives the mount: a removed file still pinned when the volume
/// goes away leaves its fragments allocated and no entry naming them, for
/// the format's own checker to reclaim.
#[test]
fn a_removed_pinned_files_fragments_are_left_to_the_checker_when_the_mount_ends() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let empty = free_blocks(&mut fs);
        let file = make(&mut fs, root, b"Open", &vec![5u8; 3000]);
        fs.pin(file).expect("pin");
        fs.remove(root, b"Open").expect("remove");
        let mut fs = Adfs::open(fs.into_device()).expect("remount");
        let root = FilesystemRead::root(&fs);
        assert!(
            free_blocks(&mut fs) < empty,
            "{variant:?}: the fragments leak"
        );
        assert!(list(&mut fs, root).is_empty(), "{variant:?}");
    }
}

/// A pinned file sharing a fragment takes one of its own as it loses its
/// name, so the fragment goes with its last named sharer while the pin still
/// reads the file.
#[test]
fn a_pinned_file_sharing_a_fragment_takes_its_own_as_it_loses_its_name() {
    let mut fs = fresh(AdfsVariant::E, 800 * 1024);
    let root = FilesystemRead::root(&fs);
    let dir = node_addr(root);
    make(&mut fs, root, b"Holder", b"holder bytes");
    make(&mut fs, root, b"Sharer", b"");
    let (holder_at, mut holder) = fs.dir_lookup(dir, b"Holder").expect("io").expect("found");
    let fragment = holder.indaddr & !0xFF;
    holder.indaddr = fragment | 1;
    fs.dir_update_at(dir, holder_at, &holder).expect("share");
    let (sharer_at, mut sharer) = fs.dir_lookup(dir, b"Sharer").expect("io").expect("found");
    sharer.indaddr = fragment | 2;
    sharer.size = 4;
    fs.dir_update_at(dir, sharer_at, &sharer).expect("share");
    fs.object_write(fragment | 2, 0, b"mine")
        .expect("the sharer's bytes");
    let Backing::New { map, .. } = &fs.backing else {
        panic!("a new-map volume");
    };
    let (shared_byte, _) = map
        .locate(&mut fs.volume, fragment | 2, 0)
        .expect("located");

    let node = fs.lookup(root, b"Sharer").expect("found");
    let pinned = fs.pin(node).expect("pin");
    fs.remove(root, b"Sharer").expect("remove");
    let number = token_number(pinned).expect("a token");
    let Ok(Kept::Nameless { object, owned, .. }) = fs.pins.kept(number) else {
        panic!("kept with no name");
    };
    assert_eq!(object.indaddr & 0xFF, 0, "a fragment of its own");
    assert!(owned);
    let free = free_blocks(&mut fs);
    fs.remove(root, b"Holder")
        .expect("remove the last named sharer");
    assert!(free_blocks(&mut fs) > free, "the shared fragment is freed");
    fs.volume
        .write(shared_byte, &[0xEE; 4])
        .expect("reuse its bytes");
    let mut buf = [0u8; 4];
    assert_eq!(fs.read_at(pinned, 0, &mut buf), Ok(4));
    assert_eq!(&buf, b"mine");
    assert_eq!(fs.unpin(pinned), Ok(Unpinned::Freed));
}

/// ADFS keeps no owner, group or mode, so the record an object is created
/// with has nowhere to go: the object reports the record every object does,
/// and its access bits are those RISC OS gives a new object.
#[test]
fn a_new_object_reports_the_record_every_object_does() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let root = FilesystemRead::root(&fs);
        let asked = NodeSecurity::new(0o600, 1000, 100);
        let file = fs
            .create(root, b"File", NodeKind::RegularFile, &asked)
            .expect("create");
        let dir = fs
            .create(root, b"Dir", NodeKind::Directory, &asked)
            .expect("mkdir");
        let sec = fs.security(file).expect("security");
        assert_eq!((sec.mode, sec.uid, sec.gid), (0o644, 0, 0), "{variant:?}");
        let sec = fs.security(dir).expect("security");
        assert_eq!((sec.mode, sec.uid, sec.gid), (0o755, 0, 0), "{variant:?}");
        assert_eq!(
            attr(&mut fs, file, b"acorn.attr").as_deref(),
            Some(&b"RW/"[..]),
            "{variant:?}"
        );
    }
}
