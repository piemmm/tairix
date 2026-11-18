//! New discs laid out as RISC OS 5 lays them out: its RAM disc at every size
//! it was recorded at, bit for bit where a zone was recorded whole, and hard
//! discs of any size the format can describe.

use super::{lay_out, new_map_record, BootRecord, Variant};
use crate::newmap::NewMap;
use crate::record::{
    DiscRecord, MapCopy, BOOT_BLOCK_OFFSET, BOOT_BLOCK_SIZE, DISC_RECORD_IN_BOOT_BLOCK,
    DISC_RECORD_SIZE, NAME_LEN, ZONE_HEADER_SIZE,
};
use crate::testkit::{ram_disc_zone, MemDisc, SparseDisc};
use crate::volume::{Backing, Volume};
use crate::Disc;

/// RISC OS 5's RAM disc of `size` bytes, named and numbered as it named and
/// numbered the one whose zone was recorded.
fn ram_disc(size: u64) -> DiscRecord {
    let mut record = new_map_record(Variant::RamDisc, size).expect("a RAM disc's record");
    record.disc_name = *b"RamDisc0\0\0";
    record.disc_id = 0x21F9;
    record.disc_type = 0x2014_8C18;
    record
}

/// `record` laid out on a disc of its size, opened.
fn laid_out(record: DiscRecord) -> (DiscRecord, Volume<MemDisc>) {
    let mut disc = MemDisc::new(usize::try_from(record.disc_size).expect("a test size"));
    let record = lay_out(&mut disc, record, BootRecord::Blank).expect("laid out");
    let map = NewMap::open(&mut disc, record).expect("its map");
    let boot_block = record.nzones > 1;
    let volume = Volume::open(disc, Backing::New { map, boot_block }).expect("its root");
    (record, volume)
}

#[test]
fn a_quarter_megabyte_ram_disc_is_laid_out_bit_for_bit_as_risc_os_5_lays_it_out() {
    let (record, mut volume) = laid_out(ram_disc(0x4_0000));
    assert_eq!((record.nzones, record.idlen, record.root), (1, 13, 0x301));
    let mut zone = [0u8; 1024];
    for copy in [0, 0x400] {
        volume.disc_mut().read(copy, &mut zone).expect("a zone");
        assert_eq!(zone, ram_disc_zone(), "the copy at {copy:#X}");
    }
}

/// The sizes RISC OS 5's RAM disc was recorded at, each with its zones, id
/// length, root fragment and free bytes.
const RECORDED: [(u64, u16, u8, u32, u64); 14] = [
    (0x4_0000, 1, 13, 3, 0x3_F000),
    (0x8_0000, 1, 13, 3, 0x7_F000),
    (0x10_0000, 2, 13, 0x246, 0xF_D800),
    (0x18_0000, 2, 13, 0x246, 0x17_D800),
    (0x20_0000, 3, 13, 0x246, 0x1F_D000),
    (0x30_0000, 4, 13, 0x48C, 0x2F_C800),
    (0x40_0000, 5, 13, 0x48C, 0x3F_C000),
    (0x50_0000, 6, 13, 0x6D2, 0x4F_B800),
    (0x80_0000, 9, 13, 0x918, 0x7F_A000),
    (0xC0_0000, 13, 13, 0xDA4, 0xBF_8000),
    (0x100_0000, 17, 14, 0x1100, 0xFF_6000),
    (0x140_0000, 21, 14, 0x1540, 0x13F_4000),
    (0x180_0000, 25, 14, 0x1980, 0x17F_2000),
    (0x200_0000, 33, 15, 0x1FE0, 0x1FE_E000),
];

#[test]
fn every_recorded_ram_disc_has_risc_os_5s_zones_root_and_free_space() {
    for (size, nzones, idlen, root, free) in RECORDED {
        let (record, mut volume) = laid_out(ram_disc(size));
        assert_eq!(
            (record.nzones, record.idlen, record.root),
            (nzones, idlen, root << 8 | 1),
            "{size:#X}"
        );
        let space = volume.space().expect("its space");
        assert_eq!(u64::from(space.unit) * space.free, free, "{size:#X}");
    }
}

#[test]
fn a_multi_zone_ram_disc_keeps_its_identity_in_the_map_alone() {
    let (record, mut volume) = laid_out(ram_disc(0x10_0000));
    assert_eq!(record.map_offset(), 0xF_0000, "the map in the middle zone");
    let mut block = [0u8; BOOT_BLOCK_SIZE];
    volume
        .disc_mut()
        .read(BOOT_BLOCK_OFFSET, &mut block)
        .expect("the boot block");
    let mut raw = [0u8; DISC_RECORD_SIZE];
    raw.copy_from_slice(
        &block[DISC_RECORD_IN_BOOT_BLOCK..DISC_RECORD_IN_BOOT_BLOCK + DISC_RECORD_SIZE],
    );
    let copy = DiscRecord::parse(&raw);
    assert_eq!(
        (copy.disc_id, copy.disc_name, copy.root_size),
        (0, [0; NAME_LEN], 0),
        "the boot block's copy only finds the map"
    );
    assert!(copy.locates_map_as(&record));
    let (mut disc, _) = volume.detach();
    let map = NewMap::open(&mut disc, copy).expect("found through the boot block");
    assert_eq!(
        map.record, record,
        "the identity read from the map's own record"
    );
}

#[test]
fn a_first_change_moves_the_disc_id_on_once_a_mount() {
    let (record, mut volume) = laid_out(ram_disc(0x4_0000));
    let id = |volume: &mut Volume<MemDisc>| {
        let mut zone = [0u8; 1024];
        volume.disc_mut().read(0, &mut zone).expect("zone 0");
        u16::from_le_bytes([zone[24], zone[25]])
    };
    assert_eq!(id(&mut volume), record.disc_id);
    let first = volume.allocate(4096).expect("an object");
    assert_eq!(
        id(&mut volume),
        record.disc_id + 1,
        "moved on by the first change"
    );
    let (disc, mount) = volume.detach();
    let mut volume = Volume::attach(disc, mount);
    volume.allocate(4096).expect("another");
    volume.free(first, 4096).expect("freed");
    assert_eq!(
        id(&mut volume),
        record.disc_id + 1,
        "once a mount, however attached"
    );
    let (mut disc, _) = volume.detach();
    let mut zone = [0u8; 1024];
    disc.read(0, &mut zone).expect("zone 0");
    let mut raw = [0u8; DISC_RECORD_SIZE];
    raw.copy_from_slice(&zone[ZONE_HEADER_SIZE..ZONE_HEADER_SIZE + DISC_RECORD_SIZE]);
    let map = NewMap::open(&mut disc, DiscRecord::parse(&raw)).expect("its map");
    let mut volume = Volume::open(
        disc,
        Backing::New {
            map,
            boot_block: false,
        },
    )
    .expect("again");
    volume.allocate(4096).expect("a third");
    assert_eq!(
        id(&mut volume),
        record.disc_id + 2,
        "and again at the next mount's"
    );
}

#[test]
fn a_hard_disc_of_any_size_the_format_describes_is_laid_out_whole() {
    const GIB: u64 = 1 << 30;
    for (size, variant) in [
        (20 << 20, Variant::HardDisc),
        (GIB, Variant::HardDisc),
        (2 * GIB, Variant::HardDiscPlus),
        (64 * GIB, Variant::HardDiscPlus),
        (600 * GIB, Variant::HardDiscPlus),
        (1 << 40, Variant::HardDiscPlus),
    ] {
        let record = new_map_record(variant, size).expect("a record");
        assert!(
            record.disc_size <= size && record.disc_size > size / 2,
            "{size:#X}"
        );
        let ids = u64::from(record.nzones) * u64::from(record.ids_per_zone());
        assert!(
            ids <= 1 << record.idlen,
            "{size:#X}: every fragment numbered"
        );
        let mut disc = SparseDisc::new(size);
        let record = lay_out(&mut disc, record, BootRecord::Whole)
            .unwrap_or_else(|error| panic!("{size:#X} laid out: {error:?} {record:?}"));
        let map = NewMap::open(&mut disc, record).expect("its map");
        let mut volume = Volume::open(
            disc,
            Backing::New {
                map,
                boot_block: true,
            },
        )
        .expect("its root");
        let object = volume.allocate(1 << 20).expect("room for a megabyte");
        assert!(object >> 8 > 2, "{size:#X}");
        assert_eq!(
            map_copies_agree(&mut volume, &record),
            Ok(true),
            "{size:#X}"
        );
    }
}

/// Whether both copies of the map hold the same zones.
fn map_copies_agree<D: Disc>(
    volume: &mut Volume<D>,
    record: &DiscRecord,
) -> Result<bool, D::Error> {
    let sector = usize::try_from(record.sector_size()).unwrap_or(0);
    let (mut first, mut second) = ([0u8; 1024], [0u8; 1024]);
    for zone in 0..u32::from(record.nzones) {
        let disc = volume.disc_mut();
        disc.read(
            record.zone_offset(MapCopy::First, zone),
            &mut first[..sector],
        )?;
        disc.read(
            record.zone_offset(MapCopy::Second, zone),
            &mut second[..sector],
        )?;
        if first[..sector] != second[..sector] {
            return Ok(false);
        }
    }
    Ok(true)
}
