//! Deterministic fuzz harness for the `lib/partition` table parsers
//! (a parser of untrusted on-disk bytes).
//!
//! A partition table is read off a disk that is outside TAIRiX's trust
//! boundary: a flashed SD card, a USB stick, or an attacker-supplied
//! image. A corrupt MBR signature, an overlapping or out-of-range extent,
//! a forged GPT header, a CRC that does not match, an entries-LBA that
//! escapes the device — all must be **rejected**, never trusted
//! (fail closed). Per ("every parser of untrusted
//! input ... has a fuzz target") the read path is driven here against
//! arbitrary disks, with a single invariant:
//!
//! * feeding any byte image to [`tairix_partition::parse_partition_table`]
//!   (and the lower [`tairix_partition::mbr::parse`]) never panics and never reads out of
//!   bounds — the parser returns a validated [`tairix_partition::PartitionTable`]
//!   or a [`tairix_partition::PartitionError`]. The run
//!   aborting *is* the failure.
//!
//! Beside it, any table [`tairix_partition::mbr::encode_entries`] writes,
//! for any entries and geometry, reads back as the extents it was given.
//!
//! TAIRiX pulls in no external fuzz runner: a
//! per-run-seeded `Prng` mutates valid seed images (real MBRs from
//! [`tairix_partition::mbr::encode`] and, with CHS addresses and an active
//! partition, [`tairix_partition::mbr::encode_entries`], and a CRC-correct
//! GPT) and feeds pure noise. A plain `cargo test` runs the fixed [`SMOKE_ITERATIONS`] sweep;
//! `cargo xtask fuzz` extends the loop to a wall-clock budget.

use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::DriverError;
use tairix_fuzzseed::Prng;
use tairix_partition::{gpt, mbr, parse_partition_table, Partition, PartitionType};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 20_000;

/// An in-memory [`Block`] over a byte vector, mirroring the unit-test
/// mock; reads/writes fail closed on an out-of-range span.
struct VecBlock {
    data: Vec<u8>,
    block_size: u32,
}

impl VecBlock {
    fn new(data: Vec<u8>, block_size: u32) -> Self {
        Self { data, block_size }
    }

    fn span(&self, lba: u64, len: usize) -> Result<(usize, usize), DriverError> {
        let bs = self.block_size as usize;
        if bs == 0 || len == 0 || !len.is_multiple_of(bs) {
            return Err(DriverError::BufferTooSmall);
        }
        let start = usize::try_from(lba)
            .ok()
            .and_then(|l| l.checked_mul(bs))
            .ok_or(DriverError::LengthOutOfRange)?;
        let end = start
            .checked_add(len)
            .ok_or(DriverError::LengthOutOfRange)?;
        if end > self.data.len() {
            return Err(DriverError::LengthOutOfRange);
        }
        Ok((start, end))
    }
}

impl Block for VecBlock {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: self.block_size,
            block_count: self.data.len() as u64 / u64::from(self.block_size),
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let (start, end) = self.span(lba, buf.len())?;
        buf.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        let (start, end) = self.span(lba, buf.len())?;
        self.data[start..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

/// A real, well-formed MBR disk image (sector 0 + slack) as a seed.
fn mbr_image() -> Vec<u8> {
    let parts = [
        Partition {
            ty: PartitionType::FatBoot,
            start_lba: 2048,
            block_count: 4096,
        },
        Partition {
            ty: PartitionType::ARXFSRoot,
            start_lba: 6144,
            block_count: 4096,
        },
    ];
    let sector = mbr::encode(&parts).expect("seed MBR encodes");
    let mut img = vec![0u8; 512 * 64];
    img[..512].copy_from_slice(&sector);
    img
}

/// A DOS disk's MBR, its one active FAT16 partition addressed by CHS, as a
/// seed.
fn dos_image() -> Vec<u8> {
    let geometry = mbr::Geometry::new(4, 26);
    let entry = mbr::Entry {
        type_byte: mbr::PART_TYPE_FAT16_SMALL,
        active: true,
        start_lba: 26,
        sectors: 63_934,
    };
    let sector = mbr::encode_entries(&[entry], geometry).expect("seed MBR encodes");
    let mut img = vec![0u8; 512 * 64];
    img[..512].copy_from_slice(&sector);
    img
}

/// Entries the writer may or may not take, for any geometry: what it writes
/// reads back as those extents, classified by their type bytes. Whether it
/// wrote a table.
fn written_tables_read_back(rng: &mut Prng) -> bool {
    let geometry = mbr::Geometry::new(rng.next_u8(), rng.next_u8() & 0x3f);
    let count = 1 + rng.below(mbr::MAX_PRIMARY_PARTITIONS);
    let mut entries = Vec::with_capacity(count);
    for slot in 0..count {
        let start_lba = if rng.below(4) == 0 {
            rng.next_u32()
        } else {
            1 + u32::try_from(slot).expect("four") * 0x4000_0000 + rng.next_u32() % 0x1000_0000
        };
        let sectors = if rng.below(4) == 0 {
            rng.next_u32()
        } else {
            1 + rng.next_u32() % 0x1000_0000
        };
        entries.push(mbr::Entry {
            type_byte: rng.next_u8(),
            active: rng.below(3) == 0,
            start_lba,
            sectors,
        });
    }
    let Ok(sector) = mbr::encode_entries(&entries, geometry) else {
        return false;
    };
    let table = mbr::parse(&sector).expect("a written table reads back");
    assert_eq!(table.partitions().len(), entries.len());
    for (part, entry) in table.partitions().iter().zip(&entries) {
        assert_eq!(part.start_lba, u64::from(entry.start_lba));
        assert_eq!(part.block_count, u64::from(entry.sectors));
        assert_eq!(part.ty, mbr::classify(entry.type_byte));
    }
    true
}

/// A CRC-correct GPT disk image as a seed (protective MBR + header +
/// entry array, one `ARXFS` root entry). Kept small so the mutation loop
/// clones it cheaply.
fn gpt_image() -> Vec<u8> {
    let bs = 512usize;
    let num_entries = 32u32;
    let blocks = 16usize;
    let mut img = vec![0u8; bs * blocks];

    // Protective MBR.
    img[mbr::PARTITION_TABLE_OFFSET + 4] = 0xee;
    img[bs - 2] = mbr::MBR_SIGNATURE[0];
    img[bs - 1] = mbr::MBR_SIGNATURE[1];

    // Entry array from LBA 2.
    let region_len = num_entries as usize * gpt::ENTRY_LEN;
    let mut region = vec![0u8; region_len];
    region[0..16].copy_from_slice(&gpt::TYPE_GUID_ARXFS_ROOT);
    region[16] = 1;
    region[32..40].copy_from_slice(&12u64.to_le_bytes());
    region[40..48].copy_from_slice(&14u64.to_le_bytes());
    let entries_crc = tairix_crc32::checksum(&region);
    img[2 * bs..2 * bs + region_len].copy_from_slice(&region);

    // Primary header at LBA 1.
    let hdr_off = bs;
    img[hdr_off..hdr_off + 8].copy_from_slice(&gpt::HEADER_SIGNATURE);
    img[hdr_off + 8..hdr_off + 12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    img[hdr_off + 12..hdr_off + 16].copy_from_slice(&92u32.to_le_bytes());
    img[hdr_off + 72..hdr_off + 80].copy_from_slice(&2u64.to_le_bytes());
    img[hdr_off + 80..hdr_off + 84].copy_from_slice(&num_entries.to_le_bytes());
    img[hdr_off + 84..hdr_off + 88]
        .copy_from_slice(&u32::try_from(gpt::ENTRY_LEN).expect("fits").to_le_bytes());
    img[hdr_off + 88..hdr_off + 92].copy_from_slice(&entries_crc.to_le_bytes());
    let header_crc = tairix_crc32::checksum(&img[hdr_off..hdr_off + 92]);
    img[hdr_off + 16..hdr_off + 20].copy_from_slice(&header_crc.to_le_bytes());

    img
}

/// Parse `bytes` as a disk at both common logical-block sizes and drain
/// the result: must never panic, whatever the image.
fn exercise_never_panics(bytes: &[u8]) {
    for bs in [512u32, 4096u32] {
        if bytes.len() < bs as usize {
            continue;
        }
        // Truncate to a whole number of blocks.
        let usable = bytes.len() - (bytes.len() % bs as usize);
        let mut dev = VecBlock::new(bytes[..usable].to_vec(), bs);
        if let Ok(table) = parse_partition_table(&mut dev) {
            // Touch every accessor a caller would.
            let _ = table.first_of_type(PartitionType::FatBoot);
            let _ = table.first_of_type(PartitionType::ARXFSRoot);
            for p in table.partitions() {
                let _ = (p.ty, p.start_lba, p.block_count);
            }
        }
    }
    // The raw MBR sector parser, fed the first 512 bytes directly; and any
    // table it accepts, grown to a medium of any size, stays one it accepts.
    if bytes.len() >= mbr::MBR_SECTOR_LEN {
        let head = &bytes[..mbr::MBR_SECTOR_LEN];
        let parsed = mbr::parse(head);
        let entries = mbr::entries(head);
        assert_eq!(
            parsed.as_ref().map(|table| table.partitions().len()).ok(),
            entries
                .as_ref()
                .map(|entries| entries.as_slice().len())
                .ok(),
            "the entries are the table's"
        );
        if let Ok(record) = mbr::ebr(head) {
            assert_ne!(record.next, Some(0));
            assert!(record.logical.is_none_or(|entry| entry.sectors > 0));
        }
        let mut sector = [0u8; mbr::MBR_SECTOR_LEN];
        sector.copy_from_slice(&bytes[..mbr::MBR_SECTOR_LEN]);
        let medium = u64::from_le_bytes(sector[440..448].try_into().expect("eight bytes"));
        if let Ok(Some(grown)) = mbr::grow_root(&mut sector, medium) {
            let table = mbr::parse(&sector).expect("a grown table is a valid table");
            assert_eq!(table.first_of_type(PartitionType::ARXFSRoot), Some(grown));
        }
    }
}

#[test]
fn parsing_any_partition_table_never_panics() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let corpus = [mbr_image(), dos_image(), gpt_image()];

    let mut rng = Prng::new(tairix_fuzzseed::start(
        "parsing_any_partition_table_never_panics",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));

    let mut iteration: u64 = 0;
    let mut written: u64 = 0;
    loop {
        // 1. A real disk image with a handful of bytes flipped at random,
        //    hammering the signature, type bytes, LBAs, header, and CRCs.
        let template = rng.pick(&corpus);
        let mut mutated = template.clone();
        let flips = rng.at_most(24);
        for _ in 0..flips {
            if mutated.is_empty() {
                break;
            }
            let pos = rng.below(mutated.len());
            mutated[pos] ^= rng.next_u8();
        }
        exercise_never_panics(&mutated);

        // 2. A truncation of a real image, driving the bounds checks.
        let keep = rng.at_most(template.len());
        exercise_never_panics(&template[..keep]);

        // 3. Pure noise of an arbitrary length.
        let nlen = rng.at_most(9000);
        let mut noise = vec![0u8; nlen];
        rng.fill(&mut noise);
        exercise_never_panics(&noise);

        // 4. A table written from arbitrary entries.
        written += u64::from(written_tables_read_back(&mut rng));

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
    // About a quarter of the drawn entry sets are tables.
    assert!(
        written * 10 >= iteration,
        "{written} of {iteration} entry sets written"
    );
}
