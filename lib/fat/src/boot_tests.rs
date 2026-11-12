use super::*;
use crate::{put16, put32};

/// A boot sector with the parameter block given, and its signature.
#[allow(
    clippy::too_many_arguments,
    reason = "one argument a field, as the format lays them"
)]
fn boot(
    bps: u16,
    spc: u8,
    reserved: u16,
    fats: u8,
    root: u16,
    total: u32,
    fat: u32,
    fat32: bool,
) -> [u8; 512] {
    let mut boot = [0u8; 512];
    put16(&mut boot, at::BYTES_PER_SECTOR, bps);
    boot[at::SECTORS_PER_CLUSTER] = spc;
    put16(&mut boot, at::RESERVED_SECTORS, reserved);
    boot[at::FATS] = fats;
    put16(&mut boot, at::ROOT_ENTRIES, root);
    match u16::try_from(total) {
        Ok(total) if !fat32 => put16(&mut boot, at::TOTAL_SECTORS_16, total),
        _ => put32(&mut boot, at::TOTAL_SECTORS_32, total),
    }
    if fat32 {
        put32(&mut boot, at::FAT_SECTORS_32, fat);
        put32(&mut boot, at::ROOT_CLUSTER, 2);
    } else {
        put16(&mut boot, at::FAT_SECTORS_16, u16::try_from(fat).unwrap());
    }
    boot[at::SIGNATURE..].copy_from_slice(&SIGNATURE);
    boot
}

fn floppy_1440() -> [u8; 512] {
    boot(512, 1, 1, 2, 224, 2880, 9, false)
}

#[test]
fn a_high_density_floppy_is_fat12_with_its_regions_where_dos_puts_them() {
    let bpb = Bpb::parse(&floppy_1440()).unwrap();
    assert_eq!(bpb.kind, Kind::Fat12);
    assert_eq!(bpb.root_sectors(), 14);
    assert_eq!(bpb.first_data_sector(), 33);
    assert_eq!(bpb.clusters(), 2847);
    assert_eq!(
        (bpb.fat_start(), bpb.root_start(), bpb.data_start()),
        (512, 19 * 512, 33 * 512)
    );
    assert_eq!(bpb.cluster_start(2), Some(33 * 512));
    assert_eq!(bpb.cluster_start(2848), Some((33 + 2846) * 512));
    assert_eq!(bpb.cluster_start(2849), None);
    assert_eq!(bpb.cluster_start(1), None);
}

#[test]
fn the_kind_follows_the_cluster_count_unless_the_block_is_shaped_for_fat32() {
    let fat16 = boot(512, 4, 1, 2, 512, 63_960, 63, false);
    let bpb = Bpb::parse(&fat16).unwrap();
    assert_eq!(bpb.kind, Kind::Fat16);
    assert!(bpb.clusters() >= FAT16_CLUSTERS);
    let small_fat32 = boot(512, 1, 1, 1, 0, 16, 1, true);
    let bpb = Bpb::parse(&small_fat32).unwrap();
    assert_eq!(
        (bpb.kind, bpb.clusters()),
        (Kind::Fat32, 14),
        "as Linux reads mkfs.fat's small volumes"
    );
    let crowded = boot(512, 1, 1, 2, 512, 70_000, 255, false);
    assert_eq!(
        Bpb::parse(&crowded),
        None,
        "too many clusters for sixteen bits"
    );
}

#[test]
fn the_cluster_count_is_bounded_by_what_the_table_describes() {
    let short_fat = boot(512, 1, 1, 2, 224, 2880, 1, false);
    let bpb = Bpb::parse(&short_fat).unwrap();
    assert_eq!(
        bpb.clusters(),
        512 * 8 / 12 - 2,
        "one sector of twelve-bit entries"
    );
    assert_eq!(bpb.data_start(), 17 * 512);
    assert_eq!(
        bpb.clusters_within(bpb.data_start() + 10 * 512 + 1),
        10,
        "a device cut short"
    );
}

#[test]
fn a_boot_sector_no_fat_volume_carries_is_refused() {
    let good = floppy_1440();
    let mut unsigned = good;
    unsigned[511] = 0;
    assert_eq!(Bpb::parse(&unsigned), None);
    for (at, value) in [
        (at::BYTES_PER_SECTOR, 513u16),
        (at::BYTES_PER_SECTOR, 256),
        (at::RESERVED_SECTORS, 0),
        (at::FAT_SECTORS_16, 0),
        (at::TOTAL_SECTORS_16, 30),
    ] {
        let mut bad = good;
        put16(&mut bad, at, value);
        assert_eq!(Bpb::parse(&bad), None, "{at} = {value}");
    }
    for (at, value) in [
        (at::SECTORS_PER_CLUSTER, 3u8),
        (at::SECTORS_PER_CLUSTER, 0),
        (at::FATS, 0),
        (at::FATS, 3),
    ] {
        let mut bad = good;
        bad[at] = value;
        assert_eq!(Bpb::parse(&bad), None, "{at} = {value}");
    }
    let mut rootless = boot(512, 1, 32, 2, 0, 200_000, 1_600, true);
    assert!(Bpb::parse(&rootless).is_some());
    for root in [0, 1, 0x0FFF_FFFF] {
        put32(&mut rootless, at::ROOT_CLUSTER, root);
        assert_eq!(Bpb::parse(&rootless), None, "root cluster {root}");
    }
}

#[test]
fn the_extended_signature_declares_the_serial_and_label() {
    let mut boot = floppy_1440();
    assert_eq!(
        (
            Bpb::parse(&boot).unwrap().serial,
            Bpb::parse(&boot).unwrap().label
        ),
        (None, None)
    );
    boot[at::EXTENDED] = EXTENDED;
    put32(&mut boot, at::SERIAL, 0x1234_5678);
    boot[at::LABEL..at::LABEL + 11].copy_from_slice(b"DOS DISK   ");
    let bpb = Bpb::parse(&boot).unwrap();
    assert_eq!(
        (bpb.serial, bpb.label),
        (Some(0x1234_5678), Some(*b"DOS DISK   "))
    );
    boot[at::EXTENDED] = EXTENDED_SERIAL_ONLY;
    let bpb = Bpb::parse(&boot).unwrap();
    assert_eq!((bpb.serial, bpb.label), (Some(0x1234_5678), None));
}

#[test]
fn a_declared_identity_is_read_without_a_volume_around_it() {
    let mut boot = [0u8; BOOT_SECTOR];
    assert_eq!(declared(&boot, at::EXTENDED), (None, None));
    boot[at::EXTENDED] = EXTENDED;
    put32(&mut boot, at::SERIAL, 0x0BAD_F00D);
    boot[at::LABEL..at::LABEL + 11].copy_from_slice(b"NO BPB HERE");
    assert_eq!(Bpb::parse(&boot), None, "no parameter block");
    assert_eq!(
        declared(&boot, at::EXTENDED),
        (Some(0x0BAD_F00D), Some(*b"NO BPB HERE"))
    );
    assert_eq!(
        declared(&boot, at::EXTENDED_32),
        (None, None),
        "FAT32's place holds nothing"
    );
    assert_eq!(
        declared(&boot, BOOT_SECTOR),
        (None, None),
        "past the sector"
    );
}

#[test]
fn the_label_is_looked_for_at_the_head_of_the_root() {
    let floppy = Bpb::parse(&floppy_1440()).unwrap();
    assert_eq!(
        floppy.label_window(),
        Some((19 * 512, 4096)),
        "the fixed root, 7 KiB of it a window's worth"
    );
    let fat32 = Bpb::parse(&boot(512, 8, 32, 2, 0, 200_000, 1_600, true)).unwrap();
    assert_eq!(
        fat32.label_window(),
        Some(((32 + 3_200) * 512, 4096)),
        "the root's first cluster"
    );
    let small = Bpb::parse(&boot(512, 1, 1, 1, 0, 16, 1, true)).unwrap();
    assert_eq!(
        small.label_window(),
        Some((2 * 512, 512)),
        "a cluster smaller than the window"
    );
}

#[test]
fn a_dos_parameter_block_reads_as_its_boot_sector_does() {
    let mut bpb = [0u8; DOS_BPB];
    bpb[0..2].copy_from_slice(&512u16.to_le_bytes());
    bpb[2] = 2;
    bpb[3..5].copy_from_slice(&1u16.to_le_bytes());
    bpb[5] = 2;
    bpb[6..8].copy_from_slice(&112u16.to_le_bytes());
    bpb[8..10].copy_from_slice(&720u16.to_le_bytes());
    bpb[10] = 0xFD;
    bpb[11..13].copy_from_slice(&2u16.to_le_bytes());
    let parsed = Bpb::from_dos(&bpb).unwrap();
    assert_eq!(
        (parsed.kind, parsed.clusters(), parsed.media),
        (Kind::Fat12, 354, 0xFD)
    );
    bpb[6..8].copy_from_slice(&0u16.to_le_bytes());
    bpb[11..13].copy_from_slice(&0u16.to_le_bytes());
    assert_eq!(Bpb::from_dos(&bpb), None, "a FAT32 shape has no DOS form");
}

#[test]
fn a_parameter_block_writes_the_dos_form_it_was_read_from() {
    let mut sector = boot(512, 4, 1, 2, 512, 70_000, 69, false);
    put16(&mut sector, at::SECTORS_PER_TRACK, 17);
    put16(&mut sector, at::HEADS, 4);
    put32(&mut sector, at::HIDDEN_SECTORS, 17);
    let bpb = Bpb::parse(&sector).unwrap();
    let dos = bpb.dos();
    assert_eq!(
        dos[..],
        sector[at::BYTES_PER_SECTOR..at::BYTES_PER_SECTOR + DOS_BPB]
    );
    assert_eq!(
        &dos[8..10],
        &[0, 0],
        "a total past sixteen bits takes the 32-bit field"
    );
    assert_eq!(Bpb::from_dos(&dos), Some(bpb));
    let small = Bpb::parse(&floppy_1440()).unwrap();
    assert_eq!(u16::from_le_bytes([small.dos()[8], small.dos()[9]]), 2880);
    assert_eq!(
        &small.dos()[21..25],
        &[0; 4],
        "one that fits takes the 16-bit field alone"
    );
    let fat32 = Bpb::parse(&boot(512, 1, 32, 2, 0, 70_000, 600, true)).unwrap();
    assert_eq!(
        (&fat32.dos()[8..10], &fat32.dos()[11..13]),
        (&[0u8, 0][..], &[0u8, 0][..]),
        "FAT32 leaves the 16-bit fields clear"
    );
}
