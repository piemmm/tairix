//! Deterministic fuzz harness for the ADFS mount / decode path.
//!
//! [`Adfs::open`] identifies and validates whichever ADFS variant a
//! device carries — the old free-space map, a bare E-class disc record,
//! or a boot block — and the read surface then decodes directories
//! (fixed and big), the allocation map, and per-entry metadata, all
//! from a backing store that, on a real system, may have been written
//! by anything. The single invariant:
//!
//! * `open` never panics for any device contents — it returns `Ok` for
//!   a genuinely valid volume and `Err` (fail closed) for everything
//!   else — and a volume that mounts is walked (directories, lookups,
//!   attributes, stats) and has a file driven through a pin without
//!   panicking either.
//!
//! TAIRiX pulls in no external fuzz runner: a per-run-seeded `Prng` draws
//! pseudo-random images, and a structured sweep flips bytes of real
//! formatted images — one per map flavour (old map, a bare disc record,
//! big directories, a boot block over four zones) — to hammer the checksum
//! and structural validation. Half of each base's stream shapes the disc
//! record's geometry at its edges and seals it again behind its boot-block
//! checksum or zone check, so the validation of the geometry is reached
//! rather than the checksum in front of it. A plain `cargo test` runs a
//! quick seed-driven smoke sample; the time-limited soak
//! (`TAIRIX_FUZZ_BUDGET_SECS`) gives each base an equal share of the
//! budget, half to sweeping byte positions in a seeded full-coverage order
//! and half to the PRNG stream.

use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::driver::filesystem::{
    DirVisit, FilesystemAttrs, FilesystemRead, FilesystemStats, FilesystemWrite, NodeKind,
    NodeSecurity,
};
use tairix_abi::DriverError;
use tairix_drv_fs_adfs::{Adfs, AdfsVariant};
use tairix_fsprobe::adfs::{
    boot_block_checksum, zone_check, DiscRecord, BOOT_BLOCK_OFFSET, BOOT_BLOCK_SIZE,
    DISC_RECORD_BITS, DISC_RECORD_IN_BOOT_BLOCK, DISC_RECORD_SIZE, ZONE_CHECK_BYTE,
    ZONE_HEADER_SIZE,
};
use tairix_fuzzseed::Prng;

const BLOCK_SIZE: u32 = 512;

// What the harness's creations ask for, which ADFS has nowhere to keep.
const FILE_RECORD: NodeSecurity = NodeSecurity::new(0o644, 0, 0);
const DIR_RECORD: NodeSecurity = NodeSecurity::new(0o755, 0, 0);

/// Where a base's disc record lies, which a shaped record is sealed behind.
#[derive(Copy, Clone)]
enum RecordAt {
    /// An old map carries none.
    Nowhere,
    /// In map zone 0, after the zone header, under the zone's check.
    Bare,
    /// In the boot block, under its checksum.
    BootBlock,
}

/// The smallest volume of each map flavour keeps the sweep affordable.
const BASES: [(AdfsVariant, usize, RecordAt); 4] = [
    (AdfsVariant::S, 160 * 1024, RecordAt::Nowhere),
    (AdfsVariant::E, 800 * 1024, RecordAt::Bare),
    (AdfsVariant::EPlus, 800 * 1024, RecordAt::Bare),
    (AdfsVariant::F, 1600 * 1024, RecordAt::BootBlock),
];

/// PRNG-image count for the quick smoke pass (no budget set).
const SMOKE_ITERATIONS: u64 = 128;

/// Seed-driven single-byte-flip samples per base image in the smoke
/// pass; the soak sweeps positions to its wall-clock budget instead.
const SMOKE_FLIP_SAMPLES: u64 = 128;

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
        let bs = BLOCK_SIZE as usize;
        if buf.is_empty() || !buf.len().is_multiple_of(bs) {
            return Err(DriverError::BufferTooSmall);
        }
        let start = usize::try_from(lba)
            .ok()
            .and_then(|l| l.checked_mul(bs))
            .ok_or(DriverError::LengthOutOfRange)?;
        let end = start
            .checked_add(buf.len())
            .ok_or(DriverError::LengthOutOfRange)?;
        if end > self.store.len() {
            return Err(DriverError::LengthOutOfRange);
        }
        buf.copy_from_slice(&self.store[start..end]);
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        let bs = BLOCK_SIZE as usize;
        if buf.is_empty() || !buf.len().is_multiple_of(bs) {
            return Err(DriverError::BufferTooSmall);
        }
        let start = usize::try_from(lba)
            .ok()
            .and_then(|l| l.checked_mul(bs))
            .ok_or(DriverError::LengthOutOfRange)?;
        let end = start
            .checked_add(buf.len())
            .ok_or(DriverError::LengthOutOfRange)?;
        if end > self.store.len() {
            return Err(DriverError::LengthOutOfRange);
        }
        self.store[start..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

/// Walk every reachable directory (bounded), resolving each decoded
/// name and probing the metadata surfaces. Every call must return a
/// `Result`, never panic, for any device contents; the visit and depth
/// budgets stop a fuzzed image driving the walk forever.
fn walk(fs: &mut Adfs<MemBlock>) {
    let mut stack = vec![(fs.root(), 0u32)];
    let mut visits = 0u32;
    while let Some((dir, depth)) = stack.pop() {
        visits += 1;
        if visits > 1024 {
            break;
        }
        // A fuzzed image may describe any directory; the entry budget bounds
        // one listing however it is shaped.
        let mut entries = Vec::new();
        let _ = fs.read_dir(dir, 0, &[], &mut |entry, name| {
            if entries.len() > 16_384 {
                return DirVisit::Stop;
            }
            entries.push((*entry, name.to_vec()));
            DirVisit::Take
        });
        let _ = fs.parent(dir);
        for (entry, name) in entries {
            let _ = fs.lookup(dir, &name);
            let _ = fs.node_info(entry.node);
            let _ = fs.parent(entry.node);
            let _ = fs.get_attr(entry.node, b"acorn.attr", &mut [0u8; 16]);
            if matches!(entry.info.kind, NodeKind::Directory) && depth < 8 {
                stack.push((entry.node, depth + 1));
            }
        }
    }
}

/// Create a file and drive it through a pin, as the kernel holds an open
/// file across its removal: pin, write, read back, remove, unpin.
fn pin_round_trip(fs: &mut Adfs<MemBlock>) {
    let root = fs.root();
    let Ok(file) = fs.create(root, b"Pinned", NodeKind::RegularFile, &FILE_RECORD) else {
        return;
    };
    let Ok(pinned) = fs.pin(file) else {
        return;
    };
    if fs.write_all(pinned, 0, b"pinned body").is_ok() {
        let _ = fs.read_at(pinned, 0, &mut [0u8; 11]);
    }
    let _ = fs.remove(root, b"Pinned");
    let _ = fs.node_info(pinned);
    let _ = fs.unpin(pinned);
}

/// The single invariant: opening an arbitrary image returns a
/// `Result`, never panics; a mounted volume is walked and re-opened.
fn exercise(image: Vec<u8>) {
    let dev = MemBlock { store: image };
    if let Ok(mut fs) = Adfs::open(dev) {
        walk(&mut fs);
        let _ = fs.stats();
        pin_round_trip(&mut fs);
    }
}

/// One of `edges`, or what `otherwise` draws, as the stream picks.
fn edge_or<T: Copy>(rng: &mut Prng, edges: &[T], otherwise: impl FnOnce(&mut Prng) -> T) -> T {
    if rng.below(2) == 0 {
        *rng.pick(edges)
    } else {
        otherwise(rng)
    }
}

/// Shape one to three of the disc record's fields in `image` at its edges,
/// or at random, and seal the record again where it lies, so what decides is
/// the validation of its geometry. An edge that does not exist for the record
/// as shaped is a random value instead: the harness's own arithmetic never
/// overflows.
fn shape_record(image: &mut [u8], at: RecordAt, rng: &mut Prng) {
    let boot_block = usize::try_from(BOOT_BLOCK_OFFSET).expect("the boot block's offset fits");
    let offset = match at {
        RecordAt::Nowhere => return,
        RecordAt::Bare => ZONE_HEADER_SIZE,
        RecordAt::BootBlock => boot_block + DISC_RECORD_IN_BOOT_BLOCK,
    };
    let Some(raw) = image
        .get(offset..offset + DISC_RECORD_SIZE)
        .and_then(|raw| <[u8; DISC_RECORD_SIZE]>::try_from(raw).ok())
    else {
        return;
    };
    let mut record = DiscRecord::parse(&raw);
    for _ in 0..=rng.below(3) {
        match rng.below(9) {
            0 => record.log2secsize = edge_or(rng, &[7, 8, 9, 10, 11], Prng::next_u8),
            1 => {
                let at = record.log2secsize;
                let edges = [at.saturating_add(2), at.saturating_add(3), 15, 16, 19, 20];
                record.idlen = edge_or(rng, &edges, Prng::next_u8);
            }
            2 => record.log2bpmb = edge_or(rng, &[5, 6, 7, 16, 17], Prng::next_u8),
            3 => {
                let [low, high] = record.nzones.to_le_bytes();
                let low = edge_or(rng, &[0, 1, 2, 3, 0xFF], |_| low);
                let high = edge_or(rng, &[0, 1, 2, 3, 0xFF], |_| high);
                record.nzones = u16::from_le_bytes([low, high]);
            }
            4 => record.root = edge_or(rng, &[0, 0x1FF, 0x200, u32::MAX], Prng::next_u32),
            5 => record.disc_size = rng.next_u64() >> rng.below(64),
            6 => record.format_version = edge_or(rng, &[0, 1, 2], Prng::next_u32),
            _ => record.zone_spare = shaped_spare(&record, rng),
        }
    }
    image[offset..offset + DISC_RECORD_SIZE].copy_from_slice(&record.encode());
    match at {
        RecordAt::Nowhere => {}
        RecordAt::Bare => {
            if (8..=10).contains(&record.log2secsize) {
                let sector = 1usize << record.log2secsize;
                if let Some(zone) = image.get_mut(..sector) {
                    zone[ZONE_CHECK_BYTE] = zone_check(zone);
                }
            }
        }
        RecordAt::BootBlock => {
            if let Some(block) = image
                .get_mut(boot_block..boot_block + BOOT_BLOCK_SIZE)
                .and_then(|block| <&mut [u8; BOOT_BLOCK_SIZE]>::try_from(block).ok())
            {
                block[BOOT_BLOCK_SIZE - 1] = boot_block_checksum(block);
            }
        }
    }
}

/// A zone spare at the edges of what `record`'s own sector size and id
/// width admit — an exact fit for the record and one fragment, one bit short,
/// a zone 0 smaller than the record, the zone header's edge — or a common or
/// random value.
fn shaped_spare(record: &DiscRecord, rng: &mut Prng) -> u16 {
    let sector_bits = (7..=11)
        .contains(&record.log2secsize)
        .then(|| 8u32.checked_shl(u32::from(record.log2secsize)))
        .flatten();
    let idlen = u32::from(record.idlen);
    let edge = |from: Option<u32>| from.and_then(|bits| u16::try_from(bits).ok());
    let edges = sector_bits.map(|bits| {
        let record_bits = bits.checked_sub(DISC_RECORD_BITS);
        [
            edge(record_bits.and_then(|left| left.checked_sub(idlen + 1))),
            edge(record_bits.and_then(|left| left.checked_sub(idlen))),
            edge(record_bits.and_then(|left| left.checked_add(1))),
            edge(bits.checked_sub(33 + idlen)),
            edge(bits.checked_sub(32 + idlen)),
            Some(0),
            Some(32),
            Some(0x520),
            Some(0x640),
        ]
    });
    edges
        .and_then(|edges| *rng.pick(&edges))
        .unwrap_or_else(|| rng.next_u16())
}

/// A populated base image of `variant`: files, a nested directory, and
/// typed metadata, so the sweeps spend their time near genuinely valid
/// structures rather than pure noise.
fn formatted_image(variant: AdfsVariant, bytes: usize) -> Vec<u8> {
    let mut fs = Adfs::format(
        MemBlock {
            store: vec![0u8; bytes],
        },
        variant,
    )
    .expect("format a blank fuzz device");
    let root = fs.root();
    fs.create(root, b"Sub", NodeKind::Directory, &DIR_RECORD)
        .expect("create dir");
    let sub = fs.lookup(root, b"Sub").expect("dir resolves");
    for (dir, name) in [(root, &b"one"[..]), (root, b"two"), (sub, b"three")] {
        let file = fs
            .create(dir, name, NodeKind::RegularFile, &FILE_RECORD)
            .expect("create file");
        fs.write_at(file, 0, &[0xA5u8; 700]).expect("write");
    }
    let node = fs.lookup(root, b"one").expect("file resolves");
    fs.set_attr(node, b"acorn.filetype", b"fff")
        .expect("type the file");
    fs.into_device().store
}

#[test]
fn fuzz_adfs_mount() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);

    // Draw and log the seed up front so every sampled byte position and
    // every PRNG image below replays exactly from the logged value.
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "fuzz_adfs_mount",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));

    // Each base gets its share of the budget, half to the sweep and half to
    // the stream, so no base is starved by the one before it.
    let halves = u32::try_from(2 * BASES.len()).expect("a handful of bases");
    let slices = tairix_fuzzseed::budget_slices(deadline, halves);
    for (at, (variant, bytes, record_at)) in (0u32..).zip(BASES) {
        let base = formatted_image(variant, bytes);
        let (sweep_until, stream_until) = (slices(2 * at + 1), slices(2 * at + 2));
        // Structured single-byte sweep over a valid image, probing the
        // checksum/structure rejection near genuinely valid data. The soak
        // visits positions in a seeded full-coverage order and stops at its
        // slice of the budget — the nightly budget flips every byte, a short
        // budget probes a reproducible spread on time — while the smoke pass
        // samples a fixed number of positions.
        if let Some(until) = sweep_until {
            tairix_fuzzseed::budgeted_sweep(base.len(), rng.next_u64(), until, |i| {
                let mut image = base.clone();
                image[i] ^= 0xFF;
                exercise(image);
            });
        } else {
            for _ in 0..SMOKE_FLIP_SAMPLES {
                let mut image = base.clone();
                let i = rng.below(base.len());
                image[i] ^= 0xFF;
                exercise(image);
            }
        }

        // PRNG sweep: multi-byte mutations of the valid image, every other
        // one with its disc record shaped at its edges and sealed again, plus
        // the occasional pure-noise image.
        let mut iteration: u64 = 0;
        loop {
            let mut image = base.clone();
            if iteration % 2 == 1 {
                shape_record(&mut image, record_at, &mut rng);
            } else {
                let flips = 1 + rng.below(23);
                for _ in 0..flips {
                    let pos = rng.below(image.len());
                    image[pos] = rng.next_u8();
                }
            }
            exercise(image);

            if rng.below(8) == 0 {
                let mut noise = vec![0u8; 64 * 1024];
                rng.fill(&mut noise);
                exercise(noise);
            }

            iteration += 1;
            if !tairix_fuzzseed::within_budget(stream_until) && iteration >= SMOKE_ITERATIONS {
                break;
            }
        }
    }
}
