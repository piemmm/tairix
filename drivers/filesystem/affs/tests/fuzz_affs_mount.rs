//! Deterministic fuzz harness for the AmigaDOS mount, decode and change
//! paths.
//!
//! [`Affs::open`] recognises the boot block and the root, and the read
//! surface then decodes hash chains, headers, extension chains, OFS data
//! blocks, links and the bitmap from a backing store that, on a real system,
//! came from anywhere: a downloaded `.adf`, a disk written by another
//! machine. The single invariant: no image makes the driver panic, loop or
//! read outside the volume — it opens a valid volume and refuses everything
//! else — and a volume that opens is walked, read and changed, every call
//! answering a `Result`, and still opens once changed.
//!
//! A per-run-seeded `Prng` mutates real formatted images, one OFS and one
//! FFS, carrying files long enough to need extension blocks, a nested
//! directory, hard links — one filed after its file in its bucket — soft
//! links and comments; a structured sweep flips each byte. Every metadata
//! block is checksummed, so a flipped byte almost never gets past the
//! checksum: the sealed mode shapes a header, extension, OFS data or bitmap
//! block — its pointers aimed at other blocks, itself and its ancestors, its
//! counts, kinds and lengths at and past their bounds — and seals it again, so
//! the structural checks behind the checksum are what meet it. A volume past
//! the root's 25 bitmap blocks carries the bitmap's extension blocks into the
//! sealed mode. A plain `cargo test` runs a quick seed-driven smoke sample;
//! the time-limited soak (`TAIRIX_FUZZ_BUDGET_SECS`) sweeps byte positions in
//! a seeded full-coverage order until the budget elapses.

use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::driver::filesystem::{
    DirEntry, DirVisit, FilesystemAttrs, FilesystemRead, FilesystemStats, FilesystemWrite, NodeId,
    NodeKind, NodeSecurity,
};
use tairix_abi::DriverError;
use tairix_drv_fs_affs::{Affs, FormatOptions};
use tairix_fsprobe::affs::{
    be32, put_be32, root_block, seal, DateStamp, Offsets, BITMAP_CHECKSUM, BITMAP_PAGES, CHECKSUM,
    ST_FILE, ST_LINKDIR, ST_LINKFILE, ST_ROOT, ST_SOFTLINK, ST_USERDIR, TABLE, T_DATA, T_HEADER,
    T_LIST,
};
use tairix_fuzzseed::Prng;

use std::time::Instant;

const BLOCK_SIZE: u32 = 512;

const RECORD: NodeSecurity = NodeSecurity::new(0o644, 0, 0);

/// The volume each base image is formatted on: room for its files and small
/// enough that a sweep of every byte stays affordable.
const VOLUME_BYTES: usize = 160 * 1024;

/// A volume needing more bitmap blocks than the root holds, so its bitmap
/// chains extension blocks.
const EXTENDED_VOLUME_BYTES: usize = 56 * 1024 * 1024;

/// The most entries a walk takes from one directory.
const LISTED_MAX: usize = 4096;

/// The file reads a walk makes again once it has read the rest, to find a
/// chain kept across other files' reads reading differently.
const READS_AGAIN: usize = 32;

/// PRNG-image count for the quick smoke pass (no budget set).
const SMOKE_ITERATIONS: u64 = 128;

/// Sealed mutations per small base image in the smoke pass.
const SMOKE_SEALED: u64 = 256;

/// Sealed mutations of the extended volume in the smoke pass: each costs
/// two copies of a 56 MiB image.
const SMOKE_SEALED_EXTENDED: u64 = 6;

/// Seed-driven single-byte-flip samples per base image in the smoke pass.
const SMOKE_FLIP_SAMPLES: u64 = 192;

/// In-RAM device over a byte image.
struct MemBlock {
    store: Vec<u8>,
}

impl Block for MemBlock {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: BLOCK_SIZE,
            block_count: (self.store.len() as u64) / u64::from(BLOCK_SIZE),
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let start = usize::try_from(lba)
            .ok()
            .and_then(|lba| lba.checked_mul(BLOCK_SIZE as usize))
            .ok_or(DriverError::LengthOutOfRange)?;
        let end = start
            .checked_add(buf.len())
            .filter(|&end| end <= self.store.len())
            .ok_or(DriverError::LengthOutOfRange)?;
        buf.copy_from_slice(&self.store[start..end]);
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        let start = usize::try_from(lba)
            .ok()
            .and_then(|lba| lba.checked_mul(BLOCK_SIZE as usize))
            .ok_or(DriverError::LengthOutOfRange)?;
        let end = start
            .checked_add(buf.len())
            .filter(|&end| end <= self.store.len())
            .ok_or(DriverError::LengthOutOfRange)?;
        self.store[start..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

/// Walk every reachable directory (bounded), resolving each name, reading
/// each file and link and probing the metadata surfaces. Each directory is
/// listed again an entry a call, and some file reads are made again once the
/// rest are done: where both answers are whole, they must agree.
fn walk(fs: &mut Affs<MemBlock>) {
    let mut stack = vec![(fs.root(), 0u32)];
    let mut visits = 0u32;
    let mut read: Vec<(NodeId, u64, Vec<u8>)> = Vec::new();
    while let Some((dir, depth)) = stack.pop() {
        visits += 1;
        if visits > 256 {
            break;
        }
        let mut entries = Vec::new();
        let mut whole = true;
        let listing = fs.read_dir(dir, 0, &[], &mut |entry, name| {
            if entries.len() >= LISTED_MAX {
                whole = false;
                return DirVisit::Stop;
            }
            entries.push((*entry, name.to_vec()));
            DirVisit::Take
        });
        if listing.is_ok() && whole {
            listed_again_an_entry_a_call(fs, dir, &entries);
        }
        let _ = fs.parent(dir);
        for (entry, name) in entries {
            let _ = fs.lookup(dir, &name);
            let _ = fs.node_info(entry.node);
            let _ = fs.get_attr(entry.node, b"amiga.protection", &mut [0u8; 16]);
            let _ = fs.get_attr(entry.node, b"amiga.comment", &mut [0u8; 80]);
            let _ = fs.list_attr(entry.node, 0, &mut [0u8; 32]);
            match entry.info.kind {
                NodeKind::Directory if depth < 8 => stack.push((entry.node, depth + 1)),
                NodeKind::RegularFile => {
                    for at in [0, entry.info.size.saturating_sub(4096)] {
                        let mut buf = vec![0u8; 4096];
                        if let Ok(got) = fs.read_at(entry.node, at, &mut buf) {
                            buf.truncate(got);
                            if read.len() < READS_AGAIN {
                                read.push((entry.node, at, buf));
                            }
                        }
                    }
                }
                NodeKind::Symlink => {
                    let _ = fs.read_link(entry.node, &mut [0u8; 512]);
                }
                NodeKind::Directory => {}
            }
        }
    }
    for (node, at, first) in read {
        let mut again = vec![0u8; 4096];
        if let Ok(got) = fs.read_at(node, at, &mut again) {
            assert!(
                again[..got] == first[..],
                "a read made again after others reads differently"
            );
        }
    }
}

/// List `dir` again, stopping at each entry and resuming from its cursor, for
/// at most one entry more than `listed`, which one call handed over whole:
/// run to its end, it hands over the same entries in the same order.
fn listed_again_an_entry_a_call(
    fs: &mut Affs<MemBlock>,
    dir: NodeId,
    listed: &[(DirEntry, Vec<u8>)],
) {
    let mut names: Vec<Vec<u8>> = Vec::new();
    let (mut cursor, mut after) = (0, Vec::new());
    while names.len() <= listed.len() {
        let mut next = None;
        let resumed = fs.read_dir(dir, cursor, &after, &mut |entry, name| {
            next = Some((entry.next_cursor, name.to_vec()));
            DirVisit::Stop
        });
        match (resumed, next) {
            (Ok(()), Some((at, name))) => {
                cursor = at;
                after.clone_from(&name);
                names.push(name);
            }
            (Ok(()), None) => {
                assert!(
                    names.iter().eq(listed.iter().map(|(_, name)| name)),
                    "a listing resumed an entry at a time hands over what one call does"
                );
                return;
            }
            (Err(_), _) => return,
        }
    }
}

/// Change the volume every way the kernel can: cut and grow a long file, create,
/// write past a gap, link, rename, truncate, comment, remove — a pinned file
/// across its removal included — and flush.
fn change(fs: &mut Affs<MemBlock>) {
    let root = fs.root();
    // Two files read in turn, then the long one cut into its second extension
    // block, grown again past a gap and read, so its kept chain is walked,
    // shortened and lengthened.
    if let (Ok(big), Ok(one)) = (fs.lookup(root, b"big"), fs.lookup(root, b"one")) {
        let mut buf = [0u8; 4096];
        for at in [0, 60_000, 4_096, 110_000] {
            let _ = fs.read_at(big, at, &mut buf);
            let _ = fs.read_at(one, 0, &mut buf);
        }
        let _ = fs.truncate(big, 75_000);
        let _ = fs.write_all(big, 110_000, &[0x5C; 2_000]);
        let _ = fs.read_at(big, 100_000, &mut buf);
    }
    if let Ok(file) = fs.create(root, b"New", NodeKind::RegularFile, &RECORD) {
        let _ = fs.write_all(file, 5_000, &[0x5A; 1_500]);
        let _ = fs.link(root, b"NewLink", file);
        let _ = fs.rename(root, b"New", root, b"Renamed");
        let _ = fs.truncate(file, 100);
        let _ = fs.set_attr(file, b"amiga.comment", b"fuzz");
        if let Ok(pinned) = fs.pin(file) {
            let _ = fs.remove(root, b"Renamed");
            let _ = fs.read_at(pinned, 0, &mut [0u8; 64]);
            let _ = fs.unpin(pinned);
        }
    }
    // An overwrite in place, through every data pointer the file has.
    if let Ok(one) = fs.lookup(root, b"one") {
        let _ = fs.write_all(one, 0, &[0x3C; 700]);
    }
    let _ = fs.remove(root, b"one");
    // A link filed right after its file in one bucket, and one elsewhere.
    let _ = fs.remove(root, b"{");
    if let Ok(sub) = fs.lookup(root, b"Sub") {
        let _ = fs.remove(sub, b"hard");
    }
    let _ = fs.rename(root, b"Sub", root, b"Moved");
    let _ = fs.create_link(root, b"Soft", b"../x", &RECORD);
    let _ = fs.flush();
    let _ = fs.stats();
}

/// The single invariant over one image.
fn exercise(image: &[u8]) {
    let open = || {
        Affs::open(MemBlock {
            store: image.to_vec(),
        })
    };
    if let Ok(mut fs) = open() {
        walk(&mut fs);
        let _ = fs.stats();
    }
    if let Ok(mut fs) = open() {
        change(&mut fs);
        walk(&mut fs);
        assert!(
            Affs::open(fs.into_device()).is_ok(),
            "a change left a volume that opened unopenable"
        );
    }
}

/// A populated base image of type `dos_type`, `bytes` long.
fn formatted_image(dos_type: u8, bytes: usize) -> Vec<u8> {
    let mut fs = Affs::format(
        MemBlock {
            store: vec![0u8; bytes],
        },
        &FormatOptions {
            dos_type,
            block_size: BLOCK_SIZE,
            name: b"Fuzz",
            created: DateStamp::default(),
        },
    )
    .expect("format a blank fuzz device");
    let root = fs.root();
    let sub = fs
        .create(root, b"Sub", NodeKind::Directory, &RECORD)
        .expect("create dir");
    for (dir, name, len) in [
        (root, &b"one"[..], 700),
        // Three extension blocks on either format.
        (root, b"big", 120_000),
        (sub, b"three", 1_200),
    ] {
        let file = fs
            .create(dir, name, NodeKind::RegularFile, &RECORD)
            .expect("create file");
        fs.write_all(file, 0, &vec![0xA5u8; len]).expect("write");
    }
    let big = fs.lookup(root, b"big").expect("big");
    fs.link(sub, b"hard", big).expect("hard link");
    // "3" and "{" hash to one bucket, the link filed after its file.
    let three = fs
        .create(root, b"3", NodeKind::RegularFile, &RECORD)
        .expect("create file");
    fs.write_all(three, 0, b"three").expect("write");
    fs.link(root, b"{", three).expect("hard link");
    fs.create_link(sub, b"soft", b"/big", &RECORD)
        .expect("soft link");
    fs.set_attr(big, b"amiga.comment", b"comment")
        .expect("comment");
    fs.flush().expect("commit");
    fs.into_device().store
}

/// Where a header, extension or OFS data block of `image` keeps its
/// checksum, if `block` is one.
fn sealed_at(image: &[u8], block: usize) -> Option<usize> {
    let bs = BLOCK_SIZE as usize;
    let at = &image[block * bs..(block + 1) * bs];
    matches!(be32(at, 0), T_HEADER | T_LIST | T_DATA)
        .then(|| tairix_fsprobe::affs::sums_to_zero(at))
        .filter(|&sums| sums)
        .map(|_| CHECKSUM)
}

/// The bitmap blocks `image`'s root names, and the bitmap extension blocks
/// that chain the rest: the first summed, the second carrying no checksum.
fn bitmap_blocks(image: &[u8]) -> (Vec<usize>, Vec<usize>) {
    let bs = BLOCK_SIZE as usize;
    let at = Offsets::new(bs);
    let blocks = image.len() / bs;
    let root = usize::try_from(root_block(blocks as u64)).expect("a test volume's root");
    let block = |n: usize| &image[n * bs..(n + 1) * bs];
    let mut pages: Vec<usize> = (0..BITMAP_PAGES)
        .map(|page| be32(block(root), at.bm_pages() + 4 * page) as usize)
        .filter(|&pointer| pointer != 0 && pointer < blocks)
        .collect();
    let mut extensions = Vec::new();
    let mut ext = be32(block(root), at.bm_ext()) as usize;
    while ext != 0 && ext < blocks && !extensions.contains(&ext) {
        extensions.push(ext);
        let longs = bs / 4;
        pages.extend(
            (0..longs - 1)
                .map(|slot| be32(block(ext), 4 * slot) as usize)
                .filter(|&pointer| pointer != 0 && pointer < blocks),
        );
        ext = be32(block(ext), bs - 4) as usize;
    }
    (pages, extensions)
}

/// The metadata blocks of `image`, each with where its checksum lies: none
/// for a bitmap extension block, which carries none.
fn metadata_blocks(image: &[u8]) -> Vec<(usize, Option<usize>)> {
    let (pages, extensions) = bitmap_blocks(image);
    (2..image.len() / BLOCK_SIZE as usize)
        .filter_map(|block| {
            if pages.contains(&block) {
                Some((block, Some(BITMAP_CHECKSUM)))
            } else if extensions.contains(&block) {
                Some((block, None))
            } else {
                sealed_at(image, block).map(|at| (block, Some(at)))
            }
        })
        .collect()
}

/// Shape one field of metadata block `block` as a hostile but well-formed
/// volume might, and seal it again at `sum_at` where it carries a checksum.
fn sealed_mutation(
    image: &mut [u8],
    block: usize,
    sum_at: Option<usize>,
    others: &[(usize, Option<usize>)],
    rng: &mut Prng,
) {
    let bs = BLOCK_SIZE as usize;
    let at = Offsets::new(bs);
    let blocks = image.len() / bs;
    let root = usize::try_from(root_block(blocks as u64)).expect("a test volume's root");
    let aim = |rng: &mut Prng| -> u32 {
        let pick = rng.below(6);
        let target = match pick {
            0 => block,
            1 => root,
            2 => others[rng.below(others.len())].0,
            3 => 2 + rng.below(blocks - 2),
            4 => blocks + rng.below(4),
            _ => rng.below(2),
        };
        u32::try_from(target).unwrap_or(u32::MAX)
    };
    let pointers = [
        at.hash_chain(),
        at.parent(),
        at.extension(),
        at.next_link(),
        at.real_entry(),
        16,
    ];
    let mutations = 1 + rng.below(3);
    let region = &mut image[block * bs..(block + 1) * bs];
    for _ in 0..mutations {
        match rng.below(8) {
            0 | 1 => {
                // The root's bitmap pointers are aimed too, so a bitmap naming
                // a structural block twice, or a gap, meets the driver.
                let field = if block == root && rng.below(2) == 0 {
                    match rng.below(BITMAP_PAGES + 1) {
                        0 => at.bm_ext(),
                        slot => at.bm_pages() + 4 * (slot - 1),
                    }
                } else {
                    pointers[rng.below(pointers.len())]
                };
                let target = aim(rng);
                put_be32(region, field, target);
            }
            2 => {
                let slot = rng.below(at.table_len());
                let target = aim(rng);
                put_be32(region, TABLE + 4 * slot, target);
            }
            3 => {
                let count = u32::try_from(rng.below(at.table_len() + 3)).unwrap_or(0);
                put_be32(region, 8, count);
            }
            4 => {
                let size = match rng.below(3) {
                    0 => u32::MAX - u32::try_from(rng.below(4)).unwrap_or(0),
                    1 => u32::try_from(rng.below(64 * 1024)).unwrap_or(0),
                    _ => rng.next_u32(),
                };
                put_be32(region, at.byte_size(), size);
            }
            5 => {
                let kinds = [
                    ST_ROOT,
                    ST_USERDIR,
                    ST_FILE,
                    ST_SOFTLINK,
                    ST_LINKFILE,
                    ST_LINKDIR,
                    7,
                ];
                put_be32(region, at.sec_type(), kinds[rng.below(kinds.len())]);
            }
            6 => {
                let lengths = [0u8, 1, 30, 31, 79, 80, 255];
                let field = if rng.below(2) == 0 {
                    at.name()
                } else {
                    at.comment()
                };
                region[field] = lengths[rng.below(lengths.len())];
            }
            _ => {
                let long = 4 * rng.below(bs / 4);
                put_be32(region, long, rng.next_u32());
            }
        }
    }
    if let Some(sum_at) = sum_at {
        seal(region, sum_at);
    }
}

/// Run `count` sealed mutations of `base`, and more until `deadline` when
/// one is given.
fn sealed_round(base: &[u8], count: u64, deadline: Option<Instant>, rng: &mut Prng) {
    let metadata = metadata_blocks(base);
    assert!(!metadata.is_empty(), "the base image carries metadata");
    let mut done = 0u64;
    while done < count || tairix_fuzzseed::within_budget(deadline) {
        let mut image = base.to_vec();
        let (block, sum_at) = metadata[rng.below(metadata.len())];
        sealed_mutation(&mut image, block, sum_at, &metadata, rng);
        exercise(&image);
        done += 1;
    }
}

/// The phases a budgeted run shares its budget between: the extended
/// volume's sealed round, then each base image's sealed round, byte sweep
/// and stream.
const BUDGET_PHASES: u32 = 7;

#[test]
fn fuzz_affs_mount() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "fuzz_affs_mount",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let slices = tairix_fuzzseed::budget_slices(deadline, BUDGET_PHASES);
    let extended = formatted_image(1, EXTENDED_VOLUME_BYTES);
    let (pages, extensions) = bitmap_blocks(&extended);
    assert!(
        pages.len() > BITMAP_PAGES && !extensions.is_empty(),
        "the extended volume chains bitmap extension blocks"
    );
    sealed_round(&extended, SMOKE_SEALED_EXTENDED, slices(1), &mut rng);
    drop(extended);
    for (at, dos_type) in (0u32..).zip([0, 3]) {
        let base = formatted_image(dos_type, VOLUME_BYTES);
        sealed_round(&base, SMOKE_SEALED, slices(2 + 3 * at), &mut rng);
        if let Some(sweep_until) = slices(3 + 3 * at) {
            tairix_fuzzseed::budgeted_sweep(base.len(), rng.next_u64(), sweep_until, |i| {
                let mut image = base.clone();
                image[i] ^= 0xFF;
                exercise(&image);
            });
        } else {
            for _ in 0..SMOKE_FLIP_SAMPLES {
                let mut image = base.clone();
                let i = rng.below(base.len());
                image[i] ^= 0xFF;
                exercise(&image);
            }
        }
        let mut iteration: u64 = 0;
        loop {
            let mut image = base.clone();
            let flips = 1 + rng.below(23);
            for _ in 0..flips {
                let pos = rng.below(image.len());
                image[pos] = rng.next_u8();
            }
            exercise(&image);
            if rng.below(8) == 0 {
                let mut noise = vec![0u8; 64 * 1024];
                rng.fill(&mut noise);
                noise[..4].copy_from_slice(b"DOS\x01");
                exercise(&noise);
            }
            iteration += 1;
            if !tairix_fuzzseed::within_budget(slices(4 + 3 * at)) && iteration >= SMOKE_ITERATIONS
            {
                break;
            }
        }
    }
}
