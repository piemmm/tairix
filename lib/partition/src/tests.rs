//! Unit tests for the scheme-neutral partition model, the MBR and GPT
//! parsers, and the partition-window [`Block`] adapter.

use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::gpt::{self, ENTRY_LEN, TYPE_GUID_ARXFS_ROOT, TYPE_GUID_EFI_SYSTEM};
use crate::mbr::{self, MbrError, MBR_SECTOR_LEN};
use tairix_abi::blkio::{BlkDeviceClass, BlkTransport};
use tairix_abi::driver::block::BlockClass;
use tairix_abi::driver::block::{
    Block, BlockGeometry, DeviceHealth, DiscardCapability, HealthSnapshot,
};
use tairix_abi::DriverError;

/// A disk on an ATA bus not known to rotate or not: served as rotational.
const DISK: BlkMedium = BlkMedium::of_transport(BlkTransport::Ata);

/// An in-memory [`Block`] device over a byte vector, for building and
/// parsing disk images in tests.
struct VecBlock {
    data: Vec<u8>,
    block_size: u32,
}

impl VecBlock {
    fn new(block_size: u32, block_count: u64) -> Self {
        let len = block_size as usize * usize::try_from(block_count).expect("fits");
        Self {
            data: vec![0u8; len],
            block_size,
        }
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

    fn put(&mut self, lba: u64, bytes: &[u8]) {
        let start = usize::try_from(lba).expect("fits") * self.block_size as usize;
        self.data[start..start + bytes.len()].copy_from_slice(bytes);
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

fn boot(start: u32, sectors: u32) -> Partition {
    Partition {
        ty: PartitionType::FatBoot,
        start_lba: u64::from(start),
        block_count: u64::from(sectors),
    }
}

fn root(start: u32, sectors: u32) -> Partition {
    Partition {
        ty: PartitionType::ARXFSRoot,
        start_lba: u64::from(start),
        block_count: u64::from(sectors),
    }
}

fn system(start: u32, sectors: u32) -> Partition {
    Partition {
        ty: PartitionType::ARXFSSystem,
        start_lba: u64::from(start),
        block_count: u64::from(sectors),
    }
}

// ----------------------------------------------------------------------
// MBR
// ----------------------------------------------------------------------

#[test]
fn mbr_round_trips_the_boot_and_root_layout() {
    let parts = [boot(2048, 65536), root(67584, 1_000_000)];
    let sector = mbr::encode(&parts).expect("encodes");
    let table = mbr::parse(&sector).expect("parses");

    assert_eq!(table.partitions().len(), 2);
    assert_eq!(table.first_of_type(PartitionType::FatBoot), Some(parts[0]));
    assert_eq!(
        table.first_of_type(PartitionType::ARXFSRoot),
        Some(parts[1])
    );
    assert_eq!(table.first_of_type(PartitionType::Other), None);
}

#[test]
fn mbr_round_trips_the_three_partition_design_b_layout() {
    // The design-B image: FAT boot + read-only `/System` + encrypted data
    // root, each located by role and distinct on the wire (`plans/PI.md`).
    let parts = [
        system(67584, 65536),
        boot(2048, 65536),
        root(133_120, 65536),
    ];
    let sector = mbr::encode(&parts).expect("encodes");

    // The `/System` entry carries its own distinct type byte, separate from
    // the data root's.
    assert_eq!(sector[446 + 4], mbr::PART_TYPE_ARXFS_SYSTEM);
    assert_ne!(mbr::PART_TYPE_ARXFS_SYSTEM, mbr::PART_TYPE_ARXFS);

    let table = mbr::parse(&sector).expect("parses");
    assert_eq!(table.partitions().len(), 3);
    assert_eq!(table.first_of_type(PartitionType::FatBoot), Some(parts[1]));
    assert_eq!(
        table.first_of_type(PartitionType::ARXFSSystem),
        Some(parts[0])
    );
    assert_eq!(
        table.first_of_type(PartitionType::ARXFSRoot),
        Some(parts[2])
    );
}

#[test]
fn mbr_type_byte_round_trips_every_representable_role() {
    for ty in [
        PartitionType::FatBoot,
        PartitionType::ARXFSSystem,
        PartitionType::ARXFSRoot,
    ] {
        let byte = mbr::type_byte_for(ty).expect("representable role");
        assert_eq!(mbr::classify(byte), ty);
    }
    // `Other` folds every foreign type byte together, so no single byte
    // represents it.
    assert_eq!(mbr::type_byte_for(PartitionType::Other), None);
}

#[test]
fn mbr_encode_refuses_an_unrepresentable_role_instead_of_dropping_it() {
    // Encoding an `Other` partition once wrote the *unused* type byte,
    // silently dropping the partition from the table; it must be refused
    // whole instead.
    let parts = [
        boot(2048, 4096),
        Partition {
            ty: PartitionType::Other,
            start_lba: 8192,
            block_count: 4096,
        },
    ];
    assert_eq!(mbr::encode(&parts), Err(MbrError::UnrepresentableRole));
}

#[test]
fn gpt_classifies_the_system_guid_distinctly_from_the_root_guid() {
    assert_eq!(
        gpt::classify(&gpt::TYPE_GUID_ARXFS_SYSTEM),
        PartitionType::ARXFSSystem
    );
    assert_eq!(
        gpt::classify(&TYPE_GUID_ARXFS_ROOT),
        PartitionType::ARXFSRoot
    );
    assert_ne!(gpt::TYPE_GUID_ARXFS_SYSTEM, TYPE_GUID_ARXFS_ROOT);
}

#[test]
fn mbr_encode_rejects_overlap() {
    let parts = [boot(2048, 4096), root(4096, 4096)];
    assert_eq!(mbr::encode(&parts), Err(MbrError::Overlap));
}

#[test]
fn mbr_encode_rejects_empty_and_sector_zero_and_too_many() {
    assert_eq!(mbr::encode(&[]), Err(MbrError::NoPartitions));
    assert_eq!(
        mbr::encode(&[boot(0, 4096)]),
        Err(MbrError::CoversMbrSector)
    );
    assert_eq!(mbr::encode(&[boot(2048, 0)]), Err(MbrError::EmptyPartition));
    let five = [
        boot(2048, 16),
        root(2064, 16),
        boot(2080, 16),
        root(2096, 16),
        boot(2112, 16),
    ];
    assert_eq!(mbr::encode(&five), Err(MbrError::TooManyPartitions));
}

#[test]
fn mbr_encode_rejects_a_partition_too_large_for_32_bit_fields() {
    let huge = Partition {
        ty: PartitionType::ARXFSRoot,
        start_lba: u64::from(u32::MAX) + 1,
        block_count: 64,
    };
    assert_eq!(mbr::encode(&[huge]), Err(MbrError::ExtentTooLarge));
}

#[test]
fn mbr_parse_rejects_a_missing_signature() {
    let mut sector = mbr::encode(&[root(2048, 4096)]).unwrap();
    sector[MBR_SECTOR_LEN - 1] = 0x00;
    assert_eq!(mbr::parse(&sector), Err(MbrError::BadSignature));
}

#[test]
fn mbr_parse_rejects_a_short_sector() {
    let short = [0u8; MBR_SECTOR_LEN - 1];
    assert_eq!(mbr::parse(&short), Err(MbrError::ShortSector));
}

#[test]
fn mbr_parse_rejects_overlapping_on_disk_entries() {
    // Hand-author a table the encoder would never produce: two
    // overlapping entries. The parser must reject it whole.
    let mut sector = [0u8; MBR_SECTOR_LEN];
    for (i, (start, sectors)) in [(2048u32, 4096u32), (4096, 4096)].iter().enumerate() {
        let base = mbr::PARTITION_TABLE_OFFSET + i * mbr::PARTITION_ENTRY_LEN;
        sector[base + 4] = mbr::PART_TYPE_ARXFS;
        sector[base + 8..base + 12].copy_from_slice(&start.to_le_bytes());
        sector[base + 12..base + 16].copy_from_slice(&sectors.to_le_bytes());
    }
    sector[MBR_SECTOR_LEN - 2] = mbr::MBR_SIGNATURE[0];
    sector[MBR_SECTOR_LEN - 1] = mbr::MBR_SIGNATURE[1];
    assert_eq!(mbr::parse(&sector), Err(MbrError::Overlap));
}

/// The Seagate ST-238R's geometry: 4 heads of 26 sectors a track.
fn st238() -> mbr::Geometry {
    mbr::Geometry::new(4, 26).expect("a geometry")
}

#[test]
fn a_geometry_counts_what_a_chs_address_holds() {
    assert_eq!(mbr::Geometry::new(0, 17), None);
    assert_eq!(mbr::Geometry::new(4, 0), None);
    assert_eq!(mbr::Geometry::new(4, 64), None);
    assert!(mbr::Geometry::new(255, 63).is_some());
}

#[test]
fn an_lba_packs_as_head_sector_and_ten_bit_cylinder() {
    let geometry = st238();
    assert_eq!(geometry.chs(0), [0, 1, 0], "cylinder 0, head 0, sector 1");
    assert_eq!(geometry.chs(26), [1, 1, 0], "the second track");
    // 63959 = 614 * 104 + 3 * 26 + 25: cylinder 614 (0x266), head 3, sector 26.
    assert_eq!(geometry.chs(63_959), [3, 0x1a | 0x80, 0x66]);
    // Past the 1,024 cylinders, the last address there is.
    let past = 1024 * 104;
    assert!(geometry.reaches(past - 1));
    assert!(!geometry.reaches(past));
    assert_eq!(geometry.chs(past), [3, 0x1a | 0xc0, 0xff]);
    assert_eq!(geometry.chs(past - 1), geometry.chs(past));
    let large = mbr::Geometry::new(255, 63).expect("a geometry");
    assert_eq!(large.chs(u32::MAX), [254, 0xff, 0xff]);
}

/// The one active FAT16 partition a DOS-era 30 MB disk holds, from its
/// second track to its end.
fn dos_disk() -> mbr::Entry {
    mbr::Entry {
        type_byte: mbr::fat_type(tairix_fat::boot::Kind::Fat16, 26, 63_934, Some(st238())),
        active: true,
        start_lba: 26,
        sectors: 63_934,
    }
}

#[test]
fn a_dos_table_records_chs_addresses_and_its_active_partition() {
    let sector = mbr::encode_entries(&[dos_disk()], Some(st238())).expect("encodes");
    let entry = &sector[mbr::PARTITION_TABLE_OFFSET..][..mbr::PARTITION_ENTRY_LEN];
    assert_eq!(entry[0], 0x80, "active");
    assert_eq!(
        &entry[1..4],
        &[1, 1, 0],
        "starts at cylinder 0, head 1, sector 1"
    );
    assert_eq!(entry[4], mbr::PART_TYPE_FAT16_SMALL);
    assert_eq!(&entry[5..8], &[3, 0x1a | 0x80, 0x66], "ends at 614/3/26");
    assert_eq!(&entry[8..12], &26u32.to_le_bytes());
    assert_eq!(&entry[12..16], &63_934u32.to_le_bytes());
    let table = mbr::parse(&sector).expect("parses");
    assert_eq!(table.partitions().len(), 1);
    assert_eq!(table.partitions()[0].start_lba, 26);
}

#[test]
fn a_table_for_no_geometry_takes_the_use_lba_form() {
    let sector = mbr::encode_entries(&[dos_disk()], None).expect("encodes");
    let entry = &sector[mbr::PARTITION_TABLE_OFFSET..][..mbr::PARTITION_ENTRY_LEN];
    assert_eq!(&entry[1..4], &[0xff; 3]);
    assert_eq!(&entry[5..8], &[0xff; 3]);
    let role = mbr::encode(&[boot(2048, 65536)]).expect("encodes");
    assert_eq!(role[mbr::PARTITION_TABLE_OFFSET], 0, "no role is active");
}

#[test]
fn entries_that_are_no_table_are_refused() {
    let unused = mbr::Entry {
        type_byte: mbr::PART_TYPE_UNUSED,
        ..dos_disk()
    };
    assert_eq!(
        mbr::encode_entries(&[unused], Some(st238())),
        Err(MbrError::UnusedType)
    );
    let second = mbr::Entry {
        start_lba: 70_000,
        sectors: 1000,
        ..dos_disk()
    };
    assert_eq!(
        mbr::encode_entries(&[dos_disk(), second], None),
        Err(MbrError::SeveralActive)
    );
    let overlapping = mbr::Entry {
        active: false,
        start_lba: 30,
        ..dos_disk()
    };
    assert_eq!(
        mbr::encode_entries(&[dos_disk(), overlapping], None),
        Err(MbrError::Overlap)
    );
}

#[test]
fn a_fat_partition_takes_the_type_dos_gives_it() {
    use tairix_fat::boot::Kind;
    let geometry = Some(st238());
    assert_eq!(
        mbr::fat_type(Kind::Fat12, 26, 20_000, geometry),
        mbr::PART_TYPE_FAT12
    );
    assert_eq!(
        mbr::fat_type(Kind::Fat16, 26, 65_510, geometry),
        mbr::PART_TYPE_FAT16_SMALL,
        "ends on sector 65,535"
    );
    assert_eq!(
        mbr::fat_type(Kind::Fat16, 26, 65_511, geometry),
        mbr::PART_TYPE_FAT16_CHS
    );
    assert_eq!(
        mbr::fat_type(Kind::Fat16, 40_000, 30_000, geometry),
        mbr::PART_TYPE_FAT16_CHS,
        "small, but past where early DOS numbers sectors"
    );
    let past = 1024 * 104;
    assert_eq!(
        mbr::fat_type(Kind::Fat16, 26, past - 26, geometry),
        mbr::PART_TYPE_FAT16_CHS
    );
    assert_eq!(
        mbr::fat_type(Kind::Fat16, 26, past - 25, geometry),
        mbr::PART_TYPE_FAT16_LBA
    );
    assert_eq!(
        mbr::fat_type(Kind::Fat16, 26, 1000, None),
        mbr::PART_TYPE_FAT16_LBA
    );
    assert_eq!(
        mbr::fat_type(Kind::Fat32, 26, 70_000, geometry),
        mbr::PART_TYPE_FAT32_CHS
    );
    assert_eq!(
        mbr::fat_type(Kind::Fat32, 2048, 70_000, None),
        mbr::PART_TYPE_FAT32_LBA
    );
}

#[test]
fn entries_keep_type_bytes_and_the_active_flag() {
    let extended = mbr::Entry {
        type_byte: mbr::PART_TYPE_EXTENDED_CHS,
        active: false,
        start_lba: 70_000,
        sectors: 30_000,
    };
    let sector = mbr::encode_entries(&[dos_disk(), extended], Some(st238())).expect("encodes");
    let entries = mbr::entries(&sector).expect("reads");
    assert_eq!(entries.as_slice(), &[dos_disk(), extended]);
    assert!(mbr::is_extended(entries.as_slice()[1].type_byte));
    assert_eq!(mbr::entries(&sector[..100]), Err(MbrError::ShortSector));
}

/// An EBR whose first entry is `logical` and whose second links to `next`.
fn ebr_sector(logical: Option<(u8, u32, u32)>, next: Option<u32>) -> [u8; MBR_SECTOR_LEN] {
    let mut sector = [0u8; MBR_SECTOR_LEN];
    let mut put = |slot: usize, type_byte: u8, start: u32, sectors: u32| {
        let at = mbr::PARTITION_TABLE_OFFSET + slot * mbr::PARTITION_ENTRY_LEN;
        sector[at + 4] = type_byte;
        sector[at + 8..at + 12].copy_from_slice(&start.to_le_bytes());
        sector[at + 12..at + 16].copy_from_slice(&sectors.to_le_bytes());
    };
    if let Some((type_byte, start, sectors)) = logical {
        put(0, type_byte, start, sectors);
    }
    if let Some(start) = next {
        put(1, mbr::PART_TYPE_EXTENDED_CHS, start, 2000);
    }
    sector[510..].copy_from_slice(&mbr::MBR_SIGNATURE);
    sector
}

#[test]
fn an_ebr_names_its_logical_drive_and_the_next_record() {
    let read = mbr::ebr(&ebr_sector(Some((0x06, 26, 1000)), Some(5000))).expect("an EBR");
    assert_eq!(
        read.logical.map(|entry| (entry.type_byte, entry.start_lba)),
        Some((0x06, 26))
    );
    assert_eq!(read.next, Some(5000));
    let last = mbr::ebr(&ebr_sector(Some((0x01, 26, 100)), None)).expect("the last");
    assert_eq!(last.next, None);
    assert_eq!(
        mbr::ebr(&ebr_sector(Some((0x06, 26, 1000)), Some(0))),
        Err(MbrError::CoversMbrSector),
        "a link back to the first record loops"
    );
    assert_eq!(
        mbr::ebr(&ebr_sector(Some((0x06, 26, 0)), None)),
        Err(MbrError::EmptyPartition)
    );
    let mut unsigned = ebr_sector(None, None);
    unsigned[511] = 0;
    assert_eq!(mbr::ebr(&unsigned), Err(MbrError::BadSignature));
}

#[test]
fn a_slot_holds_its_entry_and_an_ebr_reads_back_its_two_slots() {
    let geometry = mbr::Geometry::new(2, 17);
    let logical = mbr::Entry {
        type_byte: 0x06,
        active: false,
        start_lba: 17,
        sectors: 3 * 2 * 17 - 17,
    };
    let slot = mbr::encode_slot(&logical, geometry).expect("a slot");
    assert_eq!(slot[1..4], [1, 1, 0], "head 1, sector 1 of cylinder 0");
    assert_eq!(
        slot[5..8],
        [1, 17, 2],
        "the last head and sector of cylinder 2"
    );
    let link = mbr::Entry {
        type_byte: mbr::PART_TYPE_EXTENDED_CHS,
        active: false,
        start_lba: 102,
        sectors: 102,
    };
    let link = mbr::encode_slot(&link, geometry).expect("a link");
    let read = mbr::ebr(&mbr::encode_ebr(Some(&slot), Some(&link))).expect("an EBR");
    assert_eq!((read.logical, read.next), (Some(logical), Some(102)));
    let emptied = mbr::ebr(&mbr::encode_ebr(None, None)).expect("an emptied chain");
    assert_eq!((emptied.logical, emptied.next), (None, None));
    let refused = |entry| mbr::encode_slot(&entry, geometry);
    assert_eq!(
        refused(mbr::Entry {
            sectors: 0,
            ..logical
        }),
        Err(MbrError::EmptyPartition)
    );
    assert_eq!(
        refused(mbr::Entry {
            type_byte: 0,
            ..logical
        }),
        Err(MbrError::UnusedType)
    );
    assert_eq!(
        refused(mbr::Entry {
            start_lba: u32::MAX,
            ..logical
        }),
        Err(MbrError::LbaOverflow)
    );
}

// ----------------------------------------------------------------------
// GPT
// ----------------------------------------------------------------------

/// Build a GPT disk image (protective MBR + primary header + entry array)
/// with the given partitions, computing both CRCs with the shared
/// `tairix_crc32` so the parser is exercised against a self-consistent
/// image (the standard way to test a parser absent an in-tree encoder).
fn build_gpt(
    block_size: u32,
    block_count: u64,
    num_entries: u32,
    parts: &[([u8; 16], u64, u64)],
) -> VecBlock {
    let bs = block_size as usize;
    let mut dev = VecBlock::new(block_size, block_count);

    // Protective MBR (LBA 0): one 0xEE entry plus the signature.
    let mut pmbr = vec![0u8; bs];
    pmbr[mbr::PARTITION_TABLE_OFFSET + 4] = 0xee;
    pmbr[bs - 2] = mbr::MBR_SIGNATURE[0];
    pmbr[bs - 1] = mbr::MBR_SIGNATURE[1];
    dev.put(0, &pmbr);

    // Entry array (from LBA 2).
    let region_len = num_entries as usize * ENTRY_LEN;
    let mut region = vec![0u8; region_len];
    for (i, (guid, first, last)) in parts.iter().enumerate() {
        let base = i * ENTRY_LEN;
        region[base..base + 16].copy_from_slice(guid);
        // Unique partition GUID (off 16..32): just the index, enough to be
        // non-zero/distinct.
        region[base + 16] = u8::try_from(i).expect("few test parts").wrapping_add(1);
        region[base + 32..base + 40].copy_from_slice(&first.to_le_bytes());
        region[base + 40..base + 48].copy_from_slice(&last.to_le_bytes());
    }
    let entries_crc = tairix_crc32::checksum(&region);
    // Write the region across consecutive blocks from LBA 2.
    let entries_lba = 2u64;
    for (i, chunk) in region.chunks(bs).enumerate() {
        dev.put(entries_lba + i as u64, chunk);
    }

    // Primary header (LBA 1).
    let mut hdr = vec![0u8; bs];
    hdr[0..8].copy_from_slice(&gpt::HEADER_SIGNATURE);
    hdr[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes()); // revision 1.0
    hdr[12..16].copy_from_slice(&92u32.to_le_bytes()); // header size
    hdr[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    hdr[80..84].copy_from_slice(&num_entries.to_le_bytes());
    hdr[84..88].copy_from_slice(&u32::try_from(ENTRY_LEN).expect("fits").to_le_bytes());
    hdr[88..92].copy_from_slice(&entries_crc.to_le_bytes());
    let header_crc = tairix_crc32::checksum(&hdr[..92]);
    hdr[16..20].copy_from_slice(&header_crc.to_le_bytes());
    dev.put(1, &hdr);

    dev
}

#[test]
fn gpt_parses_boot_and_root_partitions() {
    let parts = [
        (TYPE_GUID_EFI_SYSTEM, 2048u64, 67583u64),
        (TYPE_GUID_ARXFS_ROOT, 67584, 200_000),
    ];
    let mut dev = build_gpt(512, 262_144, 128, &parts);

    let table = parse_partition_table(&mut dev).expect("parses GPT");
    assert_eq!(table.partitions().len(), 2);
    let bootp = table.first_of_type(PartitionType::FatBoot).expect("boot");
    assert_eq!(bootp.start_lba, 2048);
    assert_eq!(bootp.block_count, 67583 - 2048 + 1);
    let rootp = table.first_of_type(PartitionType::ARXFSRoot).expect("root");
    assert_eq!(rootp.start_lba, 67584);
    assert_eq!(rootp.block_count, 200_000 - 67584 + 1);
}

#[test]
fn gpt_parses_with_a_4k_logical_block() {
    let parts = [(TYPE_GUID_ARXFS_ROOT, 256u64, 50_000u64)];
    let mut dev = build_gpt(4096, 65_536, 128, &parts);
    let table = parse_partition_table(&mut dev).expect("parses 4k GPT");
    assert_eq!(
        table
            .first_of_type(PartitionType::ARXFSRoot)
            .unwrap()
            .start_lba,
        256
    );
}

#[test]
fn gpt_rejects_a_corrupt_header_crc() {
    let parts = [(TYPE_GUID_ARXFS_ROOT, 2048u64, 4096u64)];
    let mut dev = build_gpt(512, 8192, 128, &parts);
    // Flip a header byte the CRC covers (the entries-LBA field).
    let mut hdr = [0u8; 512];
    dev.read_blocks(1, &mut hdr).unwrap();
    hdr[72] ^= 0xff;
    dev.write_blocks(1, &hdr).unwrap();
    // Detection now fails -> falls back to MBR (the protective MBR has no
    // ARXFS entry), so no scheme yields a usable table.
    assert!(matches!(
        parse_partition_table(&mut dev),
        Err(PartitionError::Mbr(_))
    ));
}

#[test]
fn gpt_rejects_a_corrupt_entries_crc() {
    let parts = [(TYPE_GUID_ARXFS_ROOT, 2048u64, 4096u64)];
    let mut dev = build_gpt(512, 8192, 128, &parts);
    // Corrupt the entry array (LBA 2) without touching the header.
    let mut blk = [0u8; 512];
    dev.read_blocks(2, &mut blk).unwrap();
    blk[40] ^= 0x01;
    dev.write_blocks(2, &blk).unwrap();
    assert_eq!(
        parse_partition_table(&mut dev),
        Err(PartitionError::Gpt(gpt::GptError::EntriesCrc))
    );
}

#[test]
fn gpt_rejects_an_extent_past_the_device() {
    let parts = [(TYPE_GUID_ARXFS_ROOT, 2048u64, 999_999u64)];
    let mut dev = build_gpt(512, 8192, 128, &parts);
    assert_eq!(
        parse_partition_table(&mut dev),
        Err(PartitionError::Gpt(gpt::GptError::BadExtent))
    );
}

// ----------------------------------------------------------------------
// Scheme dispatch
// ----------------------------------------------------------------------

#[test]
fn dispatch_reads_an_mbr_disk() {
    let parts = [boot(2048, 4096), root(6144, 4096)];
    let sector = mbr::encode(&parts).unwrap();
    let mut dev = VecBlock::new(512, 16_384);
    dev.put(0, &sector);

    let table = parse_partition_table(&mut dev).expect("parses MBR via dispatch");
    assert_eq!(
        table.first_of_type(PartitionType::ARXFSRoot),
        Some(parts[1])
    );
}

#[test]
fn dispatch_reports_no_scheme_on_a_blank_disk() {
    let mut dev = VecBlock::new(512, 64);
    assert_eq!(
        parse_partition_table(&mut dev),
        Err(PartitionError::Mbr(MbrError::BadSignature))
    );
}

// ----------------------------------------------------------------------
// PartitionBlock window
// ----------------------------------------------------------------------

#[test]
fn window_translates_and_bounds_accesses() {
    let mut dev = VecBlock::new(512, 64);
    // Mark block 10 (the window's first block) with a sentinel.
    dev.put(10, &[0xab; 512]);

    let mut win = PartitionBlock::new(dev, 10, 4).expect("window fits");
    assert_eq!(win.block_count(), 4);
    assert_eq!(win.geometry().unwrap().block_count, 4);

    let mut buf = [0u8; 512];
    win.read_blocks(0, &mut buf).expect("reads window LBA 0");
    assert_eq!(buf, [0xab; 512]);

    // A read past the window end is refused.
    let mut over = [0u8; 512];
    assert_eq!(
        win.read_blocks(4, &mut over),
        Err(DriverError::LengthOutOfRange)
    );
}

#[test]
fn device_mut_reaches_a_block_outside_the_window_view() {
    let dev = VecBlock::new(512, 64);
    let mut win = PartitionBlock::new(dev, 10, 4).expect("window fits");

    // Block 0 lies below the window's start (device block 10), so it is
    // outside anything the window's own addressing can reach.
    win.device_mut()
        .write_blocks(0, &[0xcd; 512])
        .expect("writes below the window through the whole device");

    // The window's own view is unaffected: its LBA 0 still maps to device
    // block 10, still zeroed.
    let mut in_window = [0u8; 512];
    win.read_blocks(0, &mut in_window)
        .expect("reads window LBA 0");
    assert_eq!(in_window, [0u8; 512]);

    // The write landed on the device, reachable only by going around the
    // window.
    let mut below_window = [0u8; 512];
    win.device_mut()
        .read_blocks(0, &mut below_window)
        .expect("reads the device directly");
    assert_eq!(below_window, [0xcd; 512]);
}

#[test]
fn window_rejects_an_extent_past_the_device() {
    let dev = VecBlock::new(512, 64);
    assert_eq!(
        PartitionBlock::new(dev, 60, 8).map(|_| ()).unwrap_err(),
        DriverError::LengthOutOfRange
    );
}

#[test]
fn window_from_partition_uses_the_extent() {
    let dev = VecBlock::new(512, 1024);
    let part = root(64, 128);
    let win = PartitionBlock::from_partition(dev, &part).expect("window");
    assert_eq!(win.block_count(), 128);
}

/// A device that reports both a class and real health telemetry, so a test
/// can tell a forwarded answer from the trait's "no telemetry" default.
struct TelemetryBlock {
    inner: VecBlock,
}

impl Block for TelemetryBlock {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        self.inner.geometry()
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.inner.read_blocks(lba, buf)
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        self.inner.write_blocks(lba, buf)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.inner.flush()
    }

    fn medium(&self) -> Option<BlkMedium> {
        Some(DISK)
    }

    fn disc_layout(&self) -> Option<DiscLayout> {
        Some(DiscLayout {
            last_session_start: 300,
            last_recorded: Some(900),
        })
    }

    fn backing_availability(&self) -> MountAvailability {
        MountAvailability::Recovering
    }

    fn device_health(&self) -> Result<DeviceHealth, DriverError> {
        Ok(DeviceHealth::Available(HealthSnapshot {
            power_on_hours: 0,
            unsafe_shutdowns: 0,
            media_errors: 7,
            reallocated_sectors: 3,
            pending_sectors: 0,
            uncorrectable_sectors: 0,
            crc_errors: 0,
            percentage_used: 0,
            available_spare: 100,
            temperature_kelvin: 300,
            critical_warning: true,
        }))
    }
}

/// A device that supports discard at `granularity` and records the ranges
/// it is asked to discard, so a window's translation is observable.
struct DiscardingBlock {
    inner: VecBlock,
    granularity: u64,
    discarded: Vec<(u64, u64)>,
}

impl DiscardingBlock {
    fn new(granularity: u64) -> Self {
        Self {
            inner: VecBlock::new(512, 64),
            granularity,
            discarded: Vec::new(),
        }
    }
}

impl Block for DiscardingBlock {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        self.inner.geometry()
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.inner.read_blocks(lba, buf)
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        self.inner.write_blocks(lba, buf)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.inner.flush()
    }

    fn discard_capability(&self) -> Result<DiscardCapability, DriverError> {
        Ok(DiscardCapability {
            supported: true,
            granularity_blocks: self.granularity,
            max_blocks_per_request: 32,
        })
    }

    fn discard(&mut self, lba: u64, blocks: u64) -> Result<(), DriverError> {
        self.discarded.push((lba, blocks));
        Ok(())
    }
}

#[test]
fn window_forwards_discard_translated_into_the_disks_blocks() {
    // Nearly every filesystem is mounted on a partition, so a window that
    // inherited the "no discard" default would silently withhold every trim
    // from the hardware — the same trap the class/health forwarding avoids.
    let mut win = PartitionBlock::new(DiscardingBlock::new(4), 8, 16).expect("window fits");

    let capability = win.discard_capability().expect("capability forwards");
    assert!(
        capability.supported,
        "an aligned window reports the disk's discard support"
    );
    assert_eq!(capability.granularity_blocks, 4);
    assert_eq!(capability.max_blocks_per_request, 32);

    win.discard(4, 8).expect("an in-window discard is accepted");
    assert_eq!(
        win.device_mut().discarded,
        vec![(12, 8)],
        "the window's LBA is translated by its start block, never passed raw"
    );
}

#[test]
fn window_refuses_a_discard_reaching_outside_itself() {
    // A discard destroys the range's contents, so the containment check is
    // the same one a write gets: a window must never name a neighbouring
    // partition's blocks.
    let mut win = PartitionBlock::new(DiscardingBlock::new(1), 8, 16).expect("window fits");

    assert_eq!(win.discard(12, 8), Err(DriverError::LengthOutOfRange));
    assert_eq!(win.discard(u64::MAX, 2), Err(DriverError::LengthOutOfRange));
    assert!(
        win.device_mut().discarded.is_empty(),
        "a refused discard reaches the device not at all"
    );
}

#[test]
fn a_misaligned_window_withdraws_discard_support() {
    // The window starts at block 9, which is not a multiple of the device's
    // 4-block granularity. Reporting that granularity anyway would make a
    // caller that aligned its range correctly produce a device-misaligned
    // request, so support is withdrawn rather than promised falsely.
    let win = PartitionBlock::new(DiscardingBlock::new(4), 9, 16).expect("window fits");

    assert!(
        !win.discard_capability()
            .expect("capability is answered")
            .supported
    );
}

#[test]
fn window_reports_the_underlying_disks_class_and_health() {
    // A window is a range of the same physical disk, so both the patience it
    // is owed and its failing-drive telemetry are the disk's answers. If the
    // window inherited the trait defaults instead, a filesystem on a
    // partition — nearly every filesystem — would be served the unclassified
    // I/O budget and would see a dying drive as having no telemetry at all,
    // silently disabling the scrub scheduling those counters drive.
    let dev = TelemetryBlock {
        inner: VecBlock::new(512, 64),
    };
    let win = PartitionBlock::new(dev, 10, 4).expect("window fits");

    assert_eq!(win.medium(), Some(DISK));
    assert_eq!(win.device_class(), BlkDeviceClass::Rotational);
    let DeviceHealth::Available(snapshot) = win.device_health().expect("health forwards") else {
        panic!("the window must not swallow the disk's telemetry");
    };
    assert_eq!(snapshot.media_errors, 7);
    assert_eq!(snapshot.reallocated_sectors, 3);
    assert!(snapshot.critical_warning);
    assert_eq!(
        win.backing_availability(),
        MountAvailability::Recovering,
        "a window that reported the available default would have a filesystem \
         spend discretionary I/O on a disk that is being given a chance to \
         come back"
    );
}

#[test]
fn only_a_window_from_the_discs_start_reports_its_layout() {
    // The drive counts its sessions from the disc's first block, so a window
    // that starts anywhere else would send a filesystem to the wrong blocks.
    let whole = PartitionBlock::new(
        TelemetryBlock {
            inner: VecBlock::new(512, 64),
        },
        0,
        64,
    )
    .expect("window fits");
    assert_eq!(
        whole.disc_layout(),
        Some(DiscLayout {
            last_session_start: 300,
            last_recorded: Some(900),
        })
    );
    let inner = PartitionBlock::new(
        TelemetryBlock {
            inner: VecBlock::new(512, 64),
        },
        10,
        4,
    )
    .expect("window fits");
    assert_eq!(inner.disc_layout(), None);
}

#[test]
fn only_a_missing_signature_is_an_absent_scheme() {
    assert!(PartitionError::NoScheme.is_absent());
    assert!(PartitionError::Mbr(MbrError::BadSignature).is_absent());
    for present in [
        PartitionError::Mbr(MbrError::Overlap),
        PartitionError::Mbr(MbrError::CoversMbrSector),
        PartitionError::TooManyPartitions,
        PartitionError::Device(DriverError::DeviceFault),
    ] {
        assert!(!present.is_absent(), "{present:?} describes something");
    }
}

#[test]
fn table_push_fails_closed_past_the_cap() {
    let mut table = PartitionTable::empty();
    for _ in 0..MAX_PARTITIONS {
        table.push(root(1, 1)).expect("within cap");
    }
    assert_eq!(
        table.push(root(1, 1)),
        Err(PartitionError::TooManyPartitions)
    );
}

// ----------------------------------------------------------------------
// Growing the root to the medium
// ----------------------------------------------------------------------

/// A disk of `disk_blocks` holding the three-partition layout an image
/// writes, the root ending at `root_end`.
fn image_on(disk_blocks: u64, root_end: u32) -> VecBlock {
    let mut disk = VecBlock::new(512, disk_blocks);
    let parts = [
        boot(2048, 2048),
        system(4096, 4096),
        root(8192, root_end - 8192),
    ];
    let mut sector = mbr::encode(&parts).expect("encodes");
    sector[440..444].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    disk.put(0, &sector);
    disk
}

#[test]
fn the_root_grows_to_the_end_of_a_larger_medium() {
    let mut disk = image_on(65_536, 16_384);
    let before = disk.data[..MBR_SECTOR_LEN].to_vec();
    let grown = grow_root_to_medium(&mut disk).expect("grows");
    assert_eq!(grown, Some(root(8192, 65_536 - 8192)));
    let table = parse_partition_table(&mut disk).expect("the table still parses");
    assert_eq!(
        table.first_of_type(PartitionType::ARXFSRoot),
        Some(root(8192, 65_536 - 8192))
    );
    // Every byte but the root entry's end is as it was: the disk signature,
    // the other entries, the boot code.
    let after = &disk.data[..MBR_SECTOR_LEN];
    let entry = mbr::PARTITION_TABLE_OFFSET + 2 * mbr::PARTITION_ENTRY_LEN;
    for (offset, (old, new)) in before.iter().zip(after).enumerate() {
        let in_end =
            (entry + 5..entry + 8).contains(&offset) || (entry + 12..entry + 16).contains(&offset);
        if !in_end {
            assert_eq!(old, new, "byte {offset} changed");
        }
    }
}

#[test]
fn a_root_that_fills_its_medium_is_left_alone() {
    let mut disk = image_on(16_384, 16_384);
    let before = disk.data.clone();
    assert_eq!(grow_root_to_medium(&mut disk), Ok(None));
    assert_eq!(disk.data, before, "nothing was written");
}

#[test]
fn a_root_with_a_partition_after_it_is_left_alone() {
    let mut disk = VecBlock::new(512, 65_536);
    let parts = [boot(2048, 2048), root(4096, 4096), system(16_384, 4096)];
    disk.put(0, &mbr::encode(&parts).expect("encodes"));
    let before = disk.data.clone();
    assert_eq!(grow_root_to_medium(&mut disk), Ok(None));
    assert_eq!(disk.data, before);
}

#[test]
fn a_root_grows_no_further_than_the_mbr_can_address() {
    let mut sector = mbr::encode(&[boot(2048, 2048), root(8192, 8192)]).expect("encodes");
    let grown = mbr::grow_root(&mut sector, u64::MAX).expect("grows");
    assert_eq!(grown, Some(root(8192, u32::MAX - 8192)));
    let table = mbr::parse(&sector).expect("the grown table is still valid");
    assert_eq!(
        table.first_of_type(PartitionType::ARXFSRoot),
        Some(root(8192, u32::MAX - 8192))
    );
}
