//! The partition model: DOS 4.0's sizes and shares, where partitions are
//! made and how the records that hold them read back.

use super::*;
use alloc::vec;

/// The test fixed disk's shape: 64 cylinders of two 17-sector tracks.
const SMALL: Shape = Shape {
    cylinders: 64,
    heads: 2,
    sectors: 17,
};

fn blank() -> Disk {
    let base = crate::dos::boot::master_boot_record().unwrap();
    Disk::read(SMALL, &[0; MBR_SECTOR_LEN], &base)
}

fn extent(start: u16, end: u16) -> Extent {
    Extent { start, end }
}

/// The disk `writes` leave on a blank image of `SMALL`'s size.
fn image_of(writes: &[(u32, [u8; MBR_SECTOR_LEN])]) -> Vec<u8> {
    let mut image = vec![0u8; usize::from(SMALL.cylinders) * 34 * MBR_SECTOR_LEN];
    apply(&mut image, writes);
    image
}

/// `writes` made to `image`.
fn apply(image: &mut [u8], writes: &[(u32, [u8; MBR_SECTOR_LEN])]) {
    for (lba, sector) in writes {
        let at = usize::try_from(*lba).unwrap() * MBR_SECTOR_LEN;
        image[at..at + MBR_SECTOR_LEN].copy_from_slice(sector);
    }
}

/// `image` read as `FDISK` reads a disk: its master boot record, then its
/// chain record by record.
fn read_back(image: &[u8]) -> Disk {
    let sector = |lba: u32| -> [u8; MBR_SECTOR_LEN] {
        let at = usize::try_from(lba).unwrap() * MBR_SECTOR_LEN;
        image[at..at + MBR_SECTOR_LEN].try_into().unwrap()
    };
    let mut disk = Disk::read(SMALL, &sector(0), &[0; MBR_SECTOR_LEN]);
    let mut next = disk
        .extended()
        .map(|extended| SMALL.sector_of(extended.start));
    while let Some(at) = next {
        next = disk.take_record(at, &sector(at));
    }
    disk
}

#[test]
fn a_shape_is_read_from_function_08h_and_sizes_round_as_dos_4_rounds_them() {
    assert_eq!(Shape::reported(0x3F11, 1), SMALL);
    assert_eq!(
        Shape::reported(0xFFFF, 15),
        Shape {
            cylinders: 1024,
            heads: 16,
            sectors: 63
        },
        "CL's top bits the cylinder's"
    );
    assert_eq!(Shape::reported(0xFF3F, 15).cylinders, 256);
    assert_eq!(SMALL.mbytes(64), 1, "1.0625 Mbytes, rounded down");
    assert_eq!(SMALL.mbytes(48), 1, "0.797, a half rounded up");
    assert_eq!(SMALL.mbytes(0), 0);
    assert_eq!(SMALL.cylinders_for(1), 61, "2,048 sectors in 121 tracks");
    let big = Shape {
        cylinders: 1024,
        heads: 16,
        sectors: 63,
    };
    assert_eq!(
        big.cylinders_for(32),
        64,
        "the largest size under 65,536 sectors"
    );
    assert_eq!(big.cylinders_for(31), 63);
    assert_eq!(big.mbytes(1024), 504);
    assert_eq!(
        percent(1, 3),
        34,
        "a remainder of half the whole, rounded down, rounds up"
    );
    assert_eq!(percent(2, 4), 50);
    assert_eq!(percent(64, 64), 100);
    assert_eq!(percent(5, 0), 0);
    assert_eq!(
        percent(700, 1024),
        68,
        "no sixteen-bit overflow past 655 cylinders"
    );
    assert_eq!(cylinders_of_percent(50, 63), 32);
    assert_eq!(cylinders_of_percent(100, 1024), 1024);
}

#[test]
fn a_disk_with_no_signature_starts_from_the_master_boot_record_given() {
    let disk = blank();
    assert_eq!(disk.primaries().count(), 0);
    assert_eq!(
        disk.record.0[..2],
        crate::dos::boot::master_boot_record().unwrap()[..2],
        "the code to start it"
    );
    assert_eq!(disk.free_space(false, &[]), Some(extent(0, 63)));
    assert_eq!(
        disk.free_space(true, &[]),
        Some(extent(1, 63)),
        "cylinder 0 holds the record"
    );
    assert!(disk.writes().is_empty(), "nothing changed, nothing written");
}

#[test]
fn a_primary_partition_starts_on_cylinder_0s_second_track_and_is_cleared_when_written() {
    let mut disk = blank();
    assert_eq!(disk.make_primary(extent(0, 31), true), Some(0));
    let made = disk.primary(0).unwrap();
    assert_eq!(
        (made.type_byte, made.active, made.extent),
        (NEW_TYPE, true, Some(extent(0, 31)))
    );
    let writes = disk.writes();
    assert_eq!(writes.len(), 2, "{writes:?}");
    let table = mbr::entries(&writes[0].1).unwrap();
    assert_eq!(
        table.as_slice()[0],
        Entry {
            type_byte: NEW_TYPE,
            active: true,
            start_lba: 17,
            sectors: 32 * 34 - 17
        }
    );
    assert_eq!(
        writes[0].1[PARTITION_TABLE_OFFSET + 1..PARTITION_TABLE_OFFSET + 4],
        [1, 1, 0],
        "head 1, sector 1"
    );
    assert_eq!(
        writes[1],
        (17, [0; MBR_SECTOR_LEN]),
        "no older volume found in it"
    );
    assert_eq!(disk.free_space(true, &[]), Some(extent(32, 63)));
}

#[test]
fn free_space_at_the_disks_start_ends_before_the_next_partition() {
    let mut disk = blank();
    let primary = Entry {
        type_byte: 0x01,
        active: false,
        start_lba: 5 * 34,
        sectors: 59 * 34,
    };
    disk.record.0[PARTITION_TABLE_OFFSET..PARTITION_TABLE_OFFSET + PARTITION_ENTRY_LEN]
        .copy_from_slice(&mbr::encode_slot(&primary, SMALL.geometry()).unwrap());
    assert_eq!(disk.free_space(false, &[]), Some(extent(0, 4)));
    assert_eq!(
        disk.free_space(true, &[]),
        Some(extent(1, 4)),
        "four cylinders, never the next one's first"
    );
    assert_eq!(disk.make_extended(extent(1, 4)), Some(1));
    assert_eq!(
        disk.free_space(false, &[]),
        Some(extent(0, 0)),
        "cylinder 0's second track is left"
    );
}

#[test]
fn logical_drives_chain_in_order_and_read_back_as_the_driver_reads_them() {
    let mut disk = blank();
    disk.make_primary(extent(0, 15), true).unwrap();
    disk.make_extended(extent(16, 63)).unwrap();
    assert_eq!(disk.logical_space(&[]), Some(extent(16, 63)));
    disk.make_logical(extent(40, 63)).unwrap();
    assert_eq!(disk.logical_space(&[]), Some(extent(16, 39)));
    assert_eq!(
        disk.make_logical(extent(16, 39)),
        Some(0),
        "placed in the order the drives lie"
    );
    assert_eq!(disk.logical_space(&[]), None);
    let read = read_back(&image_of(&disk.writes()));
    let drives: Vec<(u16, u16, u8)> = read
        .drives()
        .map(|drive| (drive.record, drive.end, drive.type_byte()))
        .collect();
    assert_eq!(drives, [(16, 39, NEW_TYPE), (40, 63, NEW_TYPE)]);
    assert_eq!(read.logical[0].slot, disk.logical[0].slot);
    let first = mbr::ebr(&image_of(&disk.writes())[16 * 34 * 512..17 * 34 * 512]).unwrap();
    assert_eq!(
        first.logical.map(|entry| entry.start_lba),
        Some(17),
        "the drive after its record's track"
    );
    assert_eq!(
        first.next,
        Some(24 * 34),
        "the link counts from the extended partition's start"
    );
}

#[test]
fn a_chain_whose_drives_are_all_deleted_is_written_empty() {
    let mut disk = blank();
    disk.make_primary(extent(0, 15), true).unwrap();
    disk.make_extended(extent(16, 63)).unwrap();
    disk.make_logical(extent(16, 39)).unwrap();
    disk.make_logical(extent(40, 63)).unwrap();
    let mut image = image_of(&disk.writes());
    let mut read = read_back(&image);
    assert_eq!(
        (read.changed, read.chain_changed),
        (0, false),
        "read, not changed"
    );
    read.logical[0].deleted = true;
    assert_eq!(read.drives().count(), 1);
    read.sweep();
    apply(&mut image, &read.writes());
    let after: Vec<u16> = read_back(&image)
        .drives()
        .map(|drive| drive.record)
        .collect();
    assert_eq!(after, [40], "a head record links to the drive left");
    read.logical[0].deleted = true;
    read.sweep();
    let writes = read.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, 16 * 34);
    assert_eq!(
        mbr::ebr(&writes[0].1).map(|record| (record.logical, record.next)),
        Ok((None, None))
    );
}

#[test]
fn cylinders_found_bad_are_left_out_and_the_largest_run_chosen_again() {
    let disk = blank();
    assert_eq!(
        disk.free_space(false, &[extent(10, 12)]),
        Some(extent(13, 63))
    );
    assert_eq!(
        disk.free_space(false, &[extent(0, 40)]),
        Some(extent(41, 63))
    );
    assert_eq!(disk.free_space(false, &[extent(0, 63)]), None);
    let mut divided = blank();
    divided.make_extended(extent(1, 63)).unwrap();
    assert_eq!(
        divided.logical_space(&[extent(1, 50)]),
        Some(extent(51, 63))
    );
}

#[test]
fn a_new_extended_partition_writes_its_chain_empty() {
    let mut disk = blank();
    disk.make_extended(extent(1, 63)).unwrap();
    let writes = disk.writes();
    assert!(
        writes.iter().any(|(lba, sector)| *lba == 34
            && mbr::ebr(sector)
                == Ok(mbr::Ebr {
                    logical: None,
                    next: None
                })),
        "{writes:?}"
    );
}

#[test]
fn one_entry_is_made_active_and_only_those_changed_are_marked() {
    let mut disk = blank();
    disk.make_primary(extent(0, 31), false).unwrap();
    disk.make_extended(extent(32, 63)).unwrap();
    disk.changed = 0;
    disk.set_active(0);
    assert_eq!(disk.changed, 0b01);
    assert_eq!(disk.active().map(|primary| primary.slot), Some(0));
    disk.set_active(0);
    assert_eq!(disk.changed, 0b01, "no change, no mark");
}

#[test]
fn a_disk_stored_in_memory_loads_as_it_was() {
    let mut disk = blank();
    disk.make_primary(extent(0, 15), true).unwrap();
    disk.make_extended(extent(16, 63)).unwrap();
    disk.make_logical(extent(16, 63)).unwrap();
    disk.logical[0].label.copy_from_slice(b"DATA       ");
    disk.logical[0].system.copy_from_slice(b"FAT12   ");
    disk.logical[0].deleted = true;
    assert_eq!(Disk::load(&disk.store()), disk);
}

#[test]
fn letters_go_to_every_primary_partition_then_each_disks_logical_drives() {
    let mut first = blank();
    first.make_primary(extent(0, 15), true).unwrap();
    first.make_extended(extent(16, 63)).unwrap();
    first.make_logical(extent(16, 39)).unwrap();
    first.make_logical(extent(40, 63)).unwrap();
    let mut second = blank();
    second.make_primary(extent(0, 31), false).unwrap();
    second.make_extended(extent(32, 63)).unwrap();
    second.make_logical(extent(32, 63)).unwrap();
    let disks = [first, second];
    assert_eq!(
        (primary_letter(&disks, 0), primary_letter(&disks, 1)),
        (b'C', b'D')
    );
    assert_eq!(
        (
            first_logical_letter(&disks, 0),
            first_logical_letter(&disks, 1)
        ),
        (b'E', b'G')
    );
    assert_eq!(letters_used(&disks), 5);
    let [mut first, second] = disks;
    first.clear(0);
    let disks = [first, second];
    assert_eq!(
        primary_letter(&disks, 1),
        b'C',
        "a disk without one takes no letter"
    );
}

#[test]
fn a_runs_system_area_is_sized_as_dos_4_sizes_it() {
    assert_eq!(
        system_tracks(SMALL, extent(0, 63), true),
        17,
        "boot, FATs, root and the system's files"
    );
    assert_eq!(system_tracks(SMALL, extent(0, 63), false), 3);
    let big = Shape {
        cylinders: 1024,
        heads: 16,
        sectors: 63,
    };
    assert_eq!(
        system_tracks(big, extent(1, 1023), false),
        33,
        "two FATs of 1,006 sectors and the rest"
    );
}

#[test]
fn kinds_are_named_and_only_volumes_start() {
    assert_eq!([0x01, 0x04, 0x06, 0x0E].map(Kind::of), [Kind::Dos; 4]);
    assert_eq!([0x05, 0x0F].map(Kind::of), [Kind::Extended; 2]);
    assert_eq!(
        [0x02, 0x75, 0xFF, 0x83].map(Kind::of),
        [Kind::Xenix, Kind::PcIx, Kind::Table, Kind::Other]
    );
    assert!(Kind::Other.startable() && Kind::Xenix.startable() && Kind::Dos.startable());
    assert!(!Kind::Extended.startable() && !Kind::Table.startable());
}

#[test]
fn a_chain_that_turns_back_ends() {
    let mut disk = blank();
    disk.make_extended(extent(1, 63)).unwrap();
    let drive = Entry {
        type_byte: 0x06,
        active: false,
        start_lba: 17,
        sectors: 17,
    };
    let back = Entry {
        type_byte: 0x05,
        active: false,
        start_lba: 0x7FFF,
        sectors: 34,
    };
    let mut record = mbr::encode_ebr(
        Some(&mbr::encode_slot(&drive, None).unwrap()),
        Some(&mbr::encode_slot(&back, None).unwrap()),
    );
    assert_eq!(
        disk.take_record(34 * 10, &record),
        Some(34 + 0x7FFF),
        "onward"
    );
    record[PARTITION_TABLE_OFFSET + PARTITION_ENTRY_LEN + 8
        ..PARTITION_TABLE_OFFSET + PARTITION_ENTRY_LEN + 12]
        .copy_from_slice(&34u32.to_le_bytes());
    assert_eq!(
        disk.take_record(34 * 10, &record),
        None,
        "a link back to an earlier record"
    );
    for _ in disk.logical.len()..MAX_LOGICAL - 1 {
        disk.logical.push(Logical::default());
    }
    assert_eq!(
        disk.take_record(
            34 * 10,
            &mbr::encode_ebr(
                Some(&mbr::encode_slot(&drive, None).unwrap()),
                Some(&mbr::encode_slot(&back, None).unwrap())
            )
        ),
        None,
        "no room past the last drive"
    );
}

#[test]
fn a_size_takes_its_own_cylinders_and_the_whole_run_once_it_reaches_it() {
    let st225 = Shape {
        cylinders: 615,
        heads: 4,
        sectors: 17,
    };
    let run = extent(100, 614);
    assert_eq!(st225.mbytes(run.cylinders()), 17);
    assert_eq!(
        sized(st225, run, Size::Mbytes(5)),
        Some(extent(100, 250)),
        "5 Mbytes in whole cylinders, rounded up"
    );
    assert_eq!(
        sized(st225, run, Size::Mbytes(17)),
        Some(run),
        "the run's own size is the run, not its cylinders recounted"
    );
    assert_eq!(sized(st225, run, Size::Mbytes(40)), Some(run));
    let half = Size::Share {
        share: 50,
        whole: 615,
    };
    assert_eq!(
        sized(st225, run, half),
        Some(extent(100, 407)),
        "half the disk's cylinders, rounded up"
    );
    assert_eq!(
        sized(
            st225,
            run,
            Size::Share {
                share: 84,
                whole: 615
            }
        ),
        Some(run),
        "the share the run was offered as"
    );
    assert_eq!(
        sized(
            st225,
            extent(600, 614),
            Size::Share {
                share: 1,
                whole: 15
            }
        ),
        Some(extent(600, 600))
    );
    assert_eq!(sized(st225, run, Size::Mbytes(0)), None);
    assert_eq!(
        sized(
            st225,
            run,
            Size::Share {
                share: 0,
                whole: 615
            }
        ),
        None
    );
}
