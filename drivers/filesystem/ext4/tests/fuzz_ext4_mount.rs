//! Deterministic fuzz harness for the ext4 mount, journal replay and walk.
//!
//! A removable volume is untrusted input the kernel parses: its superblock,
//! descriptors, bitmaps, inodes, extent trees, block maps, directories (linear
//! and hashed), attribute blocks, orphan lists and journal. Opening one
//! writable replays a journal it left needing recovery and finishes its
//! orphans, so a crafted log drives writes. The invariant: `open` and
//! `open_read_only` never panic or hang whatever the device holds, and a
//! volume that mounts is walked, written, committed and mounted again
//! without either.
//!
//! The bases are a volume `Ext4::format` lays, with a file fragmented enough
//! for its extent tree to hold a node below the inode, three e2fsprogs-made
//! ones — a journaled volume, lazily initialised groups under `metadata_csum`,
//! and an ACL in an attribute block — and the journaled one with a transaction
//! in its log that writes the superblock home again. A plain `cargo test` runs
//! a seed-driven smoke sample; under `TAIRIX_FUZZ_BUDGET_SECS` each base's
//! metadata bytes are swept in a seeded order and the random stream runs to
//! the same budget.
//!
//! Half the stream is shaped rather than random: whole values a parser must
//! refuse or bound, set in coordination where one alone is harmless, in a
//! superblock — home, or the logged copy a replay writes home — or an extent
//! header, the superblock's checksum re-sealed so a checksummed volume reaches
//! the value instead of refusing the checksum. A byte flip almost never
//! produces these.

#[path = "support/sparse.rs"]
mod support;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Instant;

use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::driver::filesystem::{
    DirVisit, FilesystemRead, FilesystemSecurity, FilesystemStats, FilesystemWrite, NodeId,
    NodeKind, NodeSecurity,
};
use tairix_abi::DriverError;
use tairix_drv_fs_ext4::Ext4;
use tairix_fuzzseed::Prng;

const SECTOR_BYTES: u32 = 512;
const SECTOR: usize = SECTOR_BYTES as usize;

/// The volume `Ext4::format` lays for the first base: its one group, the
/// least it lays.
const FORMATTED_BYTES: usize = 8 << 20;

const JOURNALED: &[u8] = include_bytes!("fixtures/journaled.sparse");
const UNINIT: &[u8] = include_bytes!("fixtures/uninit.sparse");
const ACL_BLOCK: &[u8] = include_bytes!("fixtures/acl_block.sparse");

/// Random-stream inputs per base in the smoke pass (no budget set).
const SMOKE_ITERATIONS: u64 = 48;

/// The writes each fragmented file grows by, and their spacing.
const FRAGMENT_WRITES: u64 = 12;
const FRAGMENT_BYTES: u64 = 4096;

/// Seed-driven single-byte flips per base in the smoke pass.
const SMOKE_FLIP_SAMPLES: u64 = 48;

const SUPERBLOCK_HOME: usize = 1024;
const SUPERBLOCK_LEN: usize = 1024;
const SB_FEATURE_COMPAT: usize = 0x5C;
const SB_FEATURE_INCOMPAT: usize = 0x60;
const SB_FEATURE_RO_COMPAT: usize = 0x64;
const SB_CHECKSUM: usize = 0x3FC;
const RO_COMPAT_METADATA_CSUM: u32 = 0x400;
const EXTENT_MAGIC: u16 = 0xF30A;
const JOURNAL_MAGIC: u32 = 0xC03B_3998;
const JOURNAL_INODE: usize = 8;

/// A superblock field — `width` bytes at `at` — and values worth putting in
/// it: zero, one, the format's limits and just past them, the widest.
struct Field {
    at: usize,
    width: usize,
    values: &'static [u64],
}

const SUPERBLOCK_FIELDS: &[Field] = &[
    // Inode count; block count, low and high words.
    Field {
        at: 0x00,
        width: 4,
        values: &[0, 1, 0xFFFF_FFFF],
    },
    Field {
        at: 0x04,
        width: 4,
        values: &[0, 1, 64, 0xFFFF_FFFF],
    },
    Field {
        at: 0x150,
        width: 4,
        values: &[0, 1, 0xFFFF_FFFF],
    },
    // First data block; block and cluster size shifts.
    Field {
        at: 0x14,
        width: 4,
        values: &[0, 1, 2],
    },
    Field {
        at: 0x18,
        width: 4,
        values: &[0, 1, 2, 3, 6, 21, 32],
    },
    Field {
        at: 0x1C,
        width: 4,
        values: &[0, 1, 2, 16, 20, 21, 22, 31, 32],
    },
    // Blocks, clusters and inodes per group.
    Field {
        at: 0x20,
        width: 4,
        values: &[0, 1, 8, 8192, 32_768, 0xFFFF_FFFF],
    },
    Field {
        at: 0x24,
        width: 4,
        values: &[0, 1, 8, 8192, 0xFFFF_FFFF],
    },
    Field {
        at: 0x28,
        width: 4,
        values: &[0, 1, 8, 0xFFFF_FFFF],
    },
    // First ordinary inode; inode record size.
    Field {
        at: 0x54,
        width: 4,
        values: &[0, 1, 2, 11, 0xFFFF_FFFF],
    },
    Field {
        at: 0x58,
        width: 2,
        values: &[0, 64, 128, 129, 256, 1024, 0xFFFF],
    },
    // Reserved descriptor blocks; journal inode; descriptor size; first meta_bg.
    Field {
        at: 0xCE,
        width: 2,
        values: &[0, 1, 0xFFFF],
    },
    Field {
        at: 0xE0,
        width: 4,
        values: &[0, 1, 2, 7, 8, 11, 0xFFFF_FFFF],
    },
    Field {
        at: 0xFE,
        width: 2,
        values: &[0, 16, 32, 63, 64, 96, 128, 1024, 0xFFFF],
    },
    Field {
        at: 0x104,
        width: 4,
        values: &[0, 1, 0xFFFF_FFFF],
    },
];

/// The feature words, and the bits worth turning over in each.
const FEATURE_BITS: &[(usize, &[u32])] = &[
    (SB_FEATURE_COMPAT, &[0x4, 0x200]),
    (
        SB_FEATURE_INCOMPAT,
        &[0x4, 0x10, 0x80, 0x100, 0x200, 0x1000, 0x8000, 0x1_0000],
    ),
    (
        SB_FEATURE_RO_COMPAT,
        &[0x2, 0x8, 0x10, 0x20, 0x40, 0x200, RO_COMPAT_METADATA_CSUM],
    ),
];

/// The base image every input starts from, shared: an input's changes live
/// in its device's overlay, so a large base is never copied per input.
struct Base {
    image: Rc<Vec<u8>>,
    /// Where the sectors worth mutating start: the ones that hold anything.
    hot: Vec<usize>,
    /// Where each extent header the image holds starts.
    extent_headers: Vec<usize>,
    /// Where each superblock a mount reads sits: home, and a logged copy a
    /// replay writes home.
    superblocks: Vec<usize>,
    /// Whether it mounts only writable, its log replayed first.
    needs_replay: bool,
}

/// A device reading `base` beneath the sectors this input changed or wrote.
#[derive(Clone)]
struct CowBlock {
    base: Rc<Vec<u8>>,
    overlay: Rc<RefCell<BTreeMap<usize, [u8; SECTOR]>>>,
}

impl CowBlock {
    fn over(base: &Base) -> Self {
        Self {
            base: Rc::clone(&base.image),
            overlay: Rc::new(RefCell::new(BTreeMap::new())),
        }
    }

    fn sector(&self, index: usize) -> [u8; SECTOR] {
        if let Some(changed) = self.overlay.borrow().get(&index) {
            return *changed;
        }
        let mut out = [0u8; SECTOR];
        out.copy_from_slice(&self.base[index * SECTOR..(index + 1) * SECTOR]);
        out
    }

    fn set_byte(&self, at: usize, value: u8) {
        let index = at / SECTOR;
        let mut sector = self.sector(index);
        sector[at % SECTOR] = value;
        self.overlay.borrow_mut().insert(index, sector);
    }

    fn byte(&self, at: usize) -> u8 {
        self.sector(at / SECTOR)[at % SECTOR]
    }

    fn bytes(&self, at: usize, len: usize) -> Vec<u8> {
        (at..at + len).map(|i| self.byte(i)).collect()
    }

    fn put_bytes(&self, mut at: usize, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let (index, within) = (at / SECTOR, at % SECTOR);
            let take = (SECTOR - within).min(bytes.len());
            let mut sector = self.sector(index);
            sector[within..within + take].copy_from_slice(&bytes[..take]);
            self.overlay.borrow_mut().insert(index, sector);
            at += take;
            bytes = &bytes[take..];
        }
    }

    fn span(&self, lba: u64, len: usize) -> Result<core::ops::Range<usize>, DriverError> {
        if len == 0 || !len.is_multiple_of(SECTOR) {
            return Err(DriverError::BufferTooSmall);
        }
        let first = usize::try_from(lba).map_err(|_| DriverError::LengthOutOfRange)?;
        let end = first
            .checked_add(len / SECTOR)
            .filter(|&end| end <= self.base.len() / SECTOR)
            .ok_or(DriverError::LengthOutOfRange)?;
        Ok(first..end)
    }
}

impl Block for CowBlock {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: SECTOR_BYTES,
            block_count: (self.base.len() / SECTOR) as u64,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let span = self.span(lba, buf.len())?;
        for (index, out) in span.zip(buf.as_chunks_mut::<SECTOR>().0) {
            *out = self.sector(index);
        }
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        let span = self.span(lba, buf.len())?;
        let mut overlay = self.overlay.borrow_mut();
        for (index, data) in span.zip(buf.as_chunks::<SECTOR>().0) {
            overlay.insert(index, *data);
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

/// Visit every directory reachable from the root, bounded, reading what each
/// entry describes.
fn walk<F: FilesystemRead + FilesystemSecurity>(fs: &mut F) {
    let mut stack = vec![(fs.root(), 0u32)];
    let mut visits = 0u32;
    let mut buf = [0u8; 4096];
    while let Some((dir, depth)) = stack.pop() {
        visits += 1;
        if visits > 256 {
            break;
        }
        let mut entries = Vec::new();
        let _ = fs.read_dir(dir, 0, &[], &mut |entry, name| {
            if entries.len() >= 2048 {
                return DirVisit::Stop;
            }
            entries.push((*entry, name.to_vec()));
            DirVisit::Take
        });
        for (entry, name) in entries {
            let _ = fs.lookup(dir, &name);
            let _ = fs.node_info(entry.node);
            let _ = fs.security(entry.node);
            match entry.info.kind {
                NodeKind::Directory if depth < 8 => stack.push((entry.node, depth + 1)),
                NodeKind::RegularFile => {
                    let _ = fs.read_at(entry.node, 0, &mut buf);
                    let _ = fs.read_at(entry.node, entry.info.size.saturating_sub(1), &mut buf);
                }
                NodeKind::Symlink => {
                    let _ = fs.read_link(entry.node, &mut buf);
                }
                NodeKind::Directory => {}
            }
        }
    }
}

/// What a writable mount is put through: a create, a write that grows a file,
/// a directory made and listed, a rename, a mode change, a truncate and the
/// removals — every refusal an answer, not a fault.
fn write_through(fs: &mut Ext4<CowBlock>) {
    let root = fs.root();
    let _ = fs.create(root, b"fz", NodeKind::RegularFile);
    let _ = fs.write_at(root, b"fz", 0, &[0x5A; 6000]);
    let _ = fs.create(root, b"fzdir", NodeKind::Directory);
    if let Ok(dir) = fs.lookup(root, b"fzdir") {
        let _ = fs.create(dir, b"inner", NodeKind::RegularFile);
        let _ = fs.rename(dir, b"inner", root, b"moved");
        let _ = fs.remove(root, b"fzdir");
    }
    if let Ok(file) = fs.lookup(root, b"fz") {
        let _ = fs.set_security(file, NodeSecurity::new(0o600, 0, 0));
    }
    let _ = fs.truncate(root, b"fz", 100);
    let _ = fs.remove(root, b"fz");
    let _ = fs.remove(root, b"moved");
    let _ = fs.write_at(
        root,
        b"fragA",
        FRAGMENT_WRITES * FRAGMENT_BYTES,
        &[0x3C; 3000],
    );
    let _ = fs.stats();
    let _ = fs.flush();
}

/// One shaped input: coordinated values in a superblock or an extent header.
fn shape(device: &CowBlock, base: &Base, rng: &mut Prng) {
    if base.extent_headers.is_empty() || rng.below(2) == 0 {
        let at = base.superblocks[rng.below(base.superblocks.len())];
        shape_superblock(device, at, rng);
    } else {
        let at = base.extent_headers[rng.below(base.extent_headers.len())];
        shape_extent_header(device, at, rng);
    }
}

/// Set one to three superblock fields or feature bits at `at`, then re-seal
/// the checksum a `metadata_csum` superblock carries.
fn shape_superblock(device: &CowBlock, at: usize, rng: &mut Prng) {
    let mut sb = device.bytes(at, SUPERBLOCK_LEN);
    for _ in 0..=rng.below(3) {
        if rng.below(3) == 0 {
            let (word, bits) = FEATURE_BITS[rng.below(FEATURE_BITS.len())];
            let flipped = le32(&sb, word) ^ bits[rng.below(bits.len())];
            sb[word..word + 4].copy_from_slice(&flipped.to_le_bytes());
        } else {
            let field = &SUPERBLOCK_FIELDS[rng.below(SUPERBLOCK_FIELDS.len())];
            let value = if rng.below(4) == 0 {
                rng.next_u64()
            } else {
                field.values[rng.below(field.values.len())]
            };
            sb[field.at..field.at + field.width]
                .copy_from_slice(&value.to_le_bytes()[..field.width]);
        }
    }
    if le32(&sb, SB_FEATURE_RO_COMPAT) & RO_COMPAT_METADATA_CSUM != 0 {
        let sum = tairix_crc32c::update(0xFFFF_FFFF, &sb[..SB_CHECKSUM]);
        sb[SB_CHECKSUM..].copy_from_slice(&sum.to_le_bytes());
    }
    device.put_bytes(at, &sb);
}

/// Set an extent header's entry count, capacity or depth to an edge — none,
/// one, the root's four, one past it, a block's worth, the widest — or claim
/// one entry more than the node holds.
fn shape_extent_header(device: &CowBlock, at: usize, rng: &mut Prng) {
    const COUNTS: [u16; 6] = [0, 1, 4, 5, 340, 0xFFFF];
    let (entries, capacity, depth) = (at + 2, at + 4, at + 6);
    if rng.below(4) == 0 {
        let held = u16::from_le_bytes([device.byte(capacity), device.byte(capacity + 1)]);
        device.put_bytes(entries, &held.wrapping_add(1).to_le_bytes());
        return;
    }
    for _ in 0..=rng.below(2) {
        let field = [entries, capacity, depth][rng.below(3)];
        device.put_bytes(field, &COUNTS[rng.below(COUNTS.len())].to_le_bytes());
    }
}

fn le32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(buf[at..at + 4].try_into().expect("four bytes"))
}

fn be32(buf: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(buf[at..at + 4].try_into().expect("four bytes"))
}

/// The journaled volume with one transaction in its log that writes the
/// superblock's block home again, so a shaped edit of that logged copy
/// reaches the replay with whatever geometry it describes. The log is the
/// unchecksummed kind this volume's journal keeps, so nothing in it needs
/// re-sealing.
fn replaying(journaled: Vec<u8>) -> Base {
    let mut image = journaled;
    let sb = &image[SUPERBLOCK_HOME..SUPERBLOCK_HOME + SUPERBLOCK_LEN];
    let bs = 1024usize << le32(sb, 0x18);
    let inode_size = usize::from(u16::from_le_bytes([sb[0x58], sb[0x59]]));
    let desc_at = (usize::try_from(le32(sb, 0x14)).expect("small") + 1) * bs;
    let table = usize::try_from(le32(&image, desc_at + 8)).expect("small");
    let inode = table * bs + (JOURNAL_INODE - 1) * inode_size;
    let root = &image[inode + 0x28..inode + 0x28 + 60];
    assert_eq!(u16::from_le_bytes([root[0], root[1]]), EXTENT_MAGIC);
    assert_eq!(u16::from_le_bytes([root[6], root[7]]), 0, "a flat journal");
    let extents: Vec<(u64, u64, u64)> = (0..usize::from(u16::from_le_bytes([root[2], root[3]])))
        .map(|k| {
            let e = &root[12 + 12 * k..24 + 12 * k];
            let logical = u64::from(le32(e, 0));
            let len = u64::from(u16::from_le_bytes([e[4], e[5]]));
            let phys = (u64::from(u16::from_le_bytes([e[6], e[7]])) << 32) | u64::from(le32(e, 8));
            (logical, len, phys)
        })
        .collect();
    let byte_of = |logical: u64| {
        let (start, _, phys) = extents
            .iter()
            .find(|(start, len, _)| (*start..start + len).contains(&logical))
            .expect("the journal maps it");
        usize::try_from(phys + logical - start).expect("small") * bs
    };
    let jsb = byte_of(0);
    let (first, sequence) = (
        u64::from(be32(&image, jsb + 0x14)),
        be32(&image, jsb + 0x18),
    );
    let uuid: [u8; 16] = image[jsb + 0x30..jsb + 0x40]
        .try_into()
        .expect("sixteen bytes");
    let home = SUPERBLOCK_HOME / bs;

    let mut descriptor = vec![0u8; bs];
    descriptor[0..4].copy_from_slice(&JOURNAL_MAGIC.to_be_bytes());
    descriptor[4..8].copy_from_slice(&1u32.to_be_bytes());
    descriptor[8..12].copy_from_slice(&sequence.to_be_bytes());
    descriptor[12..16].copy_from_slice(&u32::try_from(home).expect("small").to_be_bytes());
    // The last tag, its own UUID following it.
    descriptor[16..20].copy_from_slice(&8u32.to_be_bytes());
    descriptor[20..36].copy_from_slice(&uuid);
    let logged = image[home * bs..(home + 1) * bs].to_vec();
    let mut commit = vec![0u8; bs];
    commit[0..4].copy_from_slice(&JOURNAL_MAGIC.to_be_bytes());
    commit[4..8].copy_from_slice(&2u32.to_be_bytes());
    commit[8..12].copy_from_slice(&sequence.to_be_bytes());
    for (logical, block) in [
        (first, &descriptor),
        (first + 1, &logged),
        (first + 2, &commit),
    ] {
        let at = byte_of(logical);
        image[at..at + bs].copy_from_slice(block);
    }
    image[jsb + 0x1C..jsb + 0x20]
        .copy_from_slice(&u32::try_from(first).expect("small").to_be_bytes());
    let logged_superblock = byte_of(first + 1) + SUPERBLOCK_HOME % bs;
    let mut base = Base::over(image);
    base.superblocks.push(logged_superblock);
    base.needs_replay = true;
    base
}

fn exercise(device: &CowBlock) {
    if let Ok(mut fs) = Ext4::open_read_only(device.clone()) {
        walk(&mut fs);
        let _ = fs.stats();
    }
    let written = device.clone();
    let Ok(mut fs) = Ext4::open(written.clone()) else {
        return;
    };
    walk(&mut fs);
    if fs.write_refusal().is_none() {
        write_through(&mut fs);
    }
    if fs.into_block().is_ok() {
        if let Ok(mut again) = Ext4::open_read_only(written) {
            walk(&mut again);
        }
    }
}

fn formatted() -> Base {
    let device = vec![0u8; FORMATTED_BYTES];
    let block = CowBlock {
        base: Rc::new(device),
        overlay: Rc::new(RefCell::new(BTreeMap::new())),
    };
    let mut fs = Ext4::format(block.clone(), 64, [0x3C; 16]).expect("format the fuzz volume");
    let root = fs.root();
    fs.create(root, b"Sub", NodeKind::Directory)
        .expect("create a directory");
    let sub: NodeId = fs.lookup(root, b"Sub").expect("the directory resolves");
    for (dir, name, len) in [
        (root, &b"one"[..], 700),
        (root, b"two", 40_000),
        (sub, b"three", 1),
    ] {
        fs.create(dir, name, NodeKind::RegularFile)
            .expect("create a file");
        fs.write_at(dir, name, 0, &vec![0xA5; len]).expect("write");
    }
    for i in 0..40u8 {
        let name = [b'n', b'0' + i / 10, b'0' + i % 10];
        fs.create(sub, &name, NodeKind::RegularFile)
            .expect("fill the directory");
    }
    // Two files grown in turn, so neither run continues the last and each
    // maps more extents than the inode holds.
    for name in [&b"fragA"[..], b"fragB"] {
        fs.create(root, name, NodeKind::RegularFile)
            .expect("create a fragmented file");
    }
    for i in 0..FRAGMENT_WRITES {
        for name in [&b"fragA"[..], b"fragB"] {
            fs.write_at(root, name, i * FRAGMENT_BYTES, &[0xC3; 4096])
                .expect("grow a fragmented file");
        }
    }
    let device = fs.into_block().expect("commit the fuzz volume");
    let mut image = (*device.base).clone();
    for (index, sector) in device.overlay.borrow().iter() {
        image[index * SECTOR..(index + 1) * SECTOR].copy_from_slice(sector);
    }
    let base = Base::over(image);
    assert!(
        base.extent_headers
            .iter()
            .any(|&at| u16::from_le_bytes([base.image[at + 6], base.image[at + 7]]) > 0),
        "a fragmented file's tree holds a node below its inode"
    );
    base
}

impl Base {
    fn over(image: Vec<u8>) -> Self {
        let hot: Vec<usize> = (0..image.len())
            .step_by(SECTOR)
            .filter(|&at| image[at..at + SECTOR].iter().any(|&b| b != 0))
            .collect();
        let extent_headers = hot
            .iter()
            .flat_map(|&sector| (sector..sector + SECTOR).step_by(4))
            .filter(|&at| u16::from_le_bytes([image[at], image[at + 1]]) == EXTENT_MAGIC)
            .collect();
        Self {
            image: Rc::new(image),
            hot,
            extent_headers,
            superblocks: vec![SUPERBLOCK_HOME],
            needs_replay: false,
        }
    }

    /// The `i`th byte of the sectors that hold anything.
    fn hot_byte(&self, i: usize) -> usize {
        self.hot[i / SECTOR] + i % SECTOR
    }

    /// How many bytes the sectors that hold anything span.
    fn hot_len(&self) -> usize {
        self.hot.len() * SECTOR
    }
}

#[test]
fn fuzz_ext4_mount() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "fuzz_ext4_mount",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let bases = [
        formatted(),
        Base::over(support::sparse(JOURNALED)),
        Base::over(support::sparse(UNINIT)),
        Base::over(support::sparse(ACL_BLOCK)),
        replaying(support::sparse(JOURNALED)),
    ];
    // Each base gets its share of the budget, half to the sweep and half to
    // the stream, so the journal bases are never starved by the first.
    let start = Instant::now();
    let halves = u32::try_from(2 * bases.len()).expect("a handful of bases");
    let share = deadline.map(|deadline| deadline.saturating_duration_since(start) / halves);
    for (at, base) in bases.iter().enumerate() {
        let slice = |half: u32| {
            share.map(|share| start + share * (2 * u32::try_from(at).expect("few bases") + half))
        };
        let (sweep_until, stream_until) = (slice(1), slice(2));
        // A base that does not mount would make every input below vacuous,
        // and one whose log does not replay would never reach the replay.
        let mounts = if base.needs_replay {
            Ext4::open_read_only(CowBlock::over(base)).is_err()
                && Ext4::open(CowBlock::over(base)).is_ok()
        } else {
            Ext4::open_read_only(CowBlock::over(base)).is_ok()
        };
        assert!(mounts, "every base mounts as it stands");
        exercise(&CowBlock::over(base));
        if let Some(deadline) = sweep_until {
            tairix_fuzzseed::budgeted_sweep(base.hot_len(), rng.next_u64(), deadline, |i| {
                let at = base.hot_byte(i);
                let device = CowBlock::over(base);
                device.set_byte(at, device.byte(at) ^ 0xFF);
                exercise(&device);
            });
        } else {
            for _ in 0..SMOKE_FLIP_SAMPLES {
                let device = CowBlock::over(base);
                let at = base.hot_byte(rng.below(base.hot_len()));
                device.set_byte(at, device.byte(at) ^ 0xFF);
                exercise(&device);
            }
        }
        let mut iteration = 0u64;
        loop {
            let device = CowBlock::over(base);
            if iteration % 2 == 1 {
                shape(&device, base, &mut rng);
            } else {
                for _ in 0..=rng.below(24) {
                    let at = base.hot_byte(rng.below(base.hot_len()));
                    device.set_byte(at, rng.next_u8());
                }
            }
            exercise(&device);
            iteration += 1;
            if !tairix_fuzzseed::within_budget(stream_until) && iteration >= SMOKE_ITERATIONS {
                break;
            }
        }
    }
}
