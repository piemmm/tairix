//! The disc record, the zone check and the boot block's checksum, held to the
//! structures RISC OS 5's `RamFS` was recorded laying down.

use super::{
    boot_block_checksum, zone_check, DiscRecord, BOOT_BLOCK_SIZE, DISC_RECORD_IN_BOOT_BLOCK,
    DISC_RECORD_SIZE, ZONE_CHECK_BYTE, ZONE_HEADER_SIZE,
};
use crate::testkit::ram_disc_zone;

#[test]
fn the_record_of_a_ram_disc_risc_os_5_formatted_reads_and_checks() {
    let zone = ram_disc_zone();
    let mut raw = [0u8; DISC_RECORD_SIZE];
    raw.copy_from_slice(&zone[ZONE_HEADER_SIZE..ZONE_HEADER_SIZE + DISC_RECORD_SIZE]);
    let record = DiscRecord::parse(&raw);
    assert_eq!(record.validate(), Ok(()));
    assert_eq!(
        (
            record.log2secsize,
            record.secspertrack,
            record.heads,
            record.density
        ),
        (10, 128, 1, 1)
    );
    assert_eq!(
        (
            record.idlen,
            record.log2bpmb,
            record.nzones,
            record.zone_spare
        ),
        (13, 7, 1, 32)
    );
    assert_eq!(
        (record.root, record.disc_size, record.disc_id),
        (0x301, 0x4_0000, 0x21F9)
    );
    assert_eq!(&record.disc_name[..8], b"RamDisc0");
    assert_eq!(
        (record.format_version, record.root_size, record.big_flag),
        (1, 0x800, false)
    );
    assert!(DiscRecord::tail_clear(&raw));
    assert_eq!(record.encode(), raw, "a record encodes back to its bytes");
}

#[test]
fn a_zone_risc_os_5_wrote_carries_its_own_check() {
    let zone = ram_disc_zone();
    assert_eq!(zone_check(&zone), zone[ZONE_CHECK_BYTE]);
    let mut changed = zone;
    changed[700] ^= 0x10;
    assert_ne!(
        zone_check(&changed),
        zone[ZONE_CHECK_BYTE],
        "a changed bit is caught"
    );
}

#[test]
fn a_boot_block_risc_os_5_wrote_carries_its_own_checksum() {
    let mut block = [0u8; BOOT_BLOCK_SIZE];
    block[3] = 0x20;
    for (at, byte) in [
        (0, 0x0A),
        (1, 0x80),
        (2, 0x01),
        (3, 0x01),
        (4, 0x0D),
        (5, 0x07),
        (9, 0x02),
        (10, 0x20),
        (12, 0x01),
        (13, 0x46),
        (14, 0x02),
        (18, 0x10),
        (44, 0x01),
    ] {
        block[DISC_RECORD_IN_BOOT_BLOCK + at] = byte;
    }
    block[BOOT_BLOCK_SIZE - 1] = 0x3D;
    assert_eq!(
        boot_block_checksum(&block),
        0x3D,
        "the 1 MiB RAM disc's boot block"
    );
    let mut raw = [0u8; DISC_RECORD_SIZE];
    raw.copy_from_slice(
        &block[DISC_RECORD_IN_BOOT_BLOCK..DISC_RECORD_IN_BOOT_BLOCK + DISC_RECORD_SIZE],
    );
    let record = DiscRecord::parse(&raw);
    assert_eq!(record.validate(), Ok(()));
    assert_eq!(
        (
            record.nzones,
            record.root,
            record.disc_size,
            record.root_size
        ),
        (2, 0x2_4601, 0x10_0000, 0)
    );
    assert_eq!(record.map_offset(), 0xF_0000, "the map in the middle zone");
}
