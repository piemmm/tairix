use super::*;

fn medium(sectors: u32, media: u8, geometry: (u16, u16)) -> Medium {
    Medium {
        sectors,
        bytes_per_sector: 512,
        sectors_per_track: geometry.0,
        heads: geometry.1,
        hidden_sectors: 0,
        media,
    }
}

fn shape(kind: Option<Kind>, spc: Option<u8>, root: u16) -> Shape {
    Shape {
        kind,
        sectors_per_cluster: spc,
        root_entries: root,
        serial: 0x1234_ABCD,
        label: None,
    }
}

/// `bpb` written as a boot sector and read back.
fn round_trip(bpb: &Bpb) -> Bpb {
    Bpb::parse(&boot_sector(bpb, b"TAIRIX  ", &[0xF4])).unwrap()
}

#[test]
fn floppies_get_the_tables_dos_gives_them() {
    let high = plan(
        &medium(2880, 0xF0, (18, 2)),
        &shape(Some(Kind::Fat12), Some(1), 224),
    )
    .unwrap();
    assert_eq!(
        (high.kind, high.fat_sectors, high.clusters(), high.drive),
        (Kind::Fat12, 9, 2847, 0)
    );
    assert_eq!(round_trip(&high), high);
    let double = plan(
        &medium(720, 0xFD, (9, 2)),
        &shape(Some(Kind::Fat12), Some(2), 112),
    )
    .unwrap();
    assert_eq!((double.fat_sectors, double.clusters()), (2, 354));
}

#[test]
fn a_disk_takes_its_kind_and_cluster_from_its_size() {
    let st238 = plan(&medium(63_934, FIXED_MEDIA, (26, 4)), &shape(None, None, 0)).unwrap();
    assert_eq!(
        (
            st238.kind,
            st238.sectors_per_cluster,
            st238.root_entries,
            st238.drive
        ),
        (Kind::Fat16, 4, 512, 0x80)
    );
    assert_eq!(round_trip(&st238), st238);
    let small = plan(&medium(20_000, FIXED_MEDIA, (17, 4)), &shape(None, None, 0)).unwrap();
    assert_eq!(small.kind, Kind::Fat12);
    assert!(small.clusters() < FAT16_CLUSTERS);
    let large = plan(
        &medium(131_072, FIXED_MEDIA, (0, 0)),
        &shape(Some(Kind::Fat32), None, 0),
    )
    .unwrap();
    assert_eq!(
        (
            large.reserved_sectors,
            large.sectors_per_cluster,
            large.root_cluster
        ),
        (32, 1, 2)
    );
    assert_eq!(round_trip(&large), large);
}

#[test]
fn a_request_the_size_cannot_meet_is_refused() {
    let floppy = medium(2880, 0xF0, (18, 2));
    assert_eq!(
        plan(&floppy, &shape(Some(Kind::Fat32), None, 0)),
        Err(FormatError::TooSmall)
    );
    assert_eq!(
        plan(&floppy, &shape(Some(Kind::Fat16), Some(1), 0)),
        Err(FormatError::TooSmall)
    );
    let disk = medium(400_000, FIXED_MEDIA, (0, 0));
    assert_eq!(
        plan(&disk, &shape(Some(Kind::Fat12), Some(1), 0)),
        Err(FormatError::TooLarge)
    );
    assert_eq!(
        plan(&disk, &shape(None, Some(3), 0)),
        Err(FormatError::ClusterSize)
    );
    assert_eq!(
        plan(&medium(10, 0xF0, (0, 0)), &shape(None, None, 0)),
        Err(FormatError::TooSmall)
    );
    let odd = Medium {
        bytes_per_sector: 513,
        ..floppy
    };
    assert_eq!(
        plan(&odd, &shape(None, None, 0)),
        Err(FormatError::SectorSize)
    );
}

#[test]
fn the_boot_sector_records_every_field_where_the_kind_keeps_it() {
    let bpb = plan(
        &medium(131_072, FIXED_MEDIA, (0, 0)),
        &shape(Some(Kind::Fat32), None, 0),
    )
    .unwrap();
    let boot = boot_sector(&bpb, b"TAIRIX  ", &[0xFA; 1000]);
    assert_eq!(&boot[..3], &[0xEB, 0x58, 0x90]);
    assert_eq!(&boot[at::TYPE_32..at::TYPE_32 + 8], b"FAT32   ");
    assert_eq!(&boot[at::LABEL_32..at::LABEL_32 + 11], b"NO NAME    ");
    assert_eq!(
        (boot[at::EXTENDED_32], boot[at::DRIVE_32]),
        (EXTENDED, 0x80)
    );
    assert_eq!(boot[at::CODE_32 + 419], 0xFA, "code to the signature");
    assert_eq!(&boot[510..], &SIGNATURE);
    let floppy = plan(
        &medium(2880, 0xF0, (18, 2)),
        &shape(Some(Kind::Fat12), Some(1), 224),
    )
    .unwrap();
    let boot = boot_sector(&floppy, b"TAIRIX  ", &[]);
    assert_eq!(boot[1], 0x3C);
    assert_eq!(&boot[at::TYPE..at::TYPE + 8], b"FAT12   ");
    assert_eq!(
        u16::from_le_bytes([boot[at::TOTAL_SECTORS_16], boot[at::TOTAL_SECTORS_16 + 1]]),
        2880
    );
}

/// Every sector a new volume's system area holds, laid out in memory.
fn laid_out(bpb: &Bpb) -> std::vec::Vec<u8> {
    let boot = boot_sector(bpb, b"TAIRIX  ", &[]);
    let bps = usize::from(bpb.bytes_per_sector);
    let sectors = usize::try_from(system_sectors(bpb)).unwrap();
    let mut image = std::vec![0u8; sectors * bps];
    for (n, sector) in image.chunks_exact_mut(bps).enumerate() {
        system_sector(bpb, &boot, u64::try_from(n).unwrap(), sector);
    }
    image
}

extern crate std;

#[test]
fn a_new_system_area_reads_back_as_an_empty_volume_of_its_kind() {
    let fat32 = plan(
        &medium(131_072, FIXED_MEDIA, (0, 0)),
        &shape(Some(Kind::Fat32), None, 0),
    )
    .unwrap();
    let image = laid_out(&fat32);
    assert_eq!(Bpb::parse(image[..512].try_into().unwrap()), Some(fat32));
    assert_eq!(
        &image[6 * 512..6 * 512 + 512],
        &image[..512],
        "the backup boot sector"
    );
    assert_eq!(
        FsInfo::decode(image[512..1024].try_into().unwrap()),
        Some(FsInfo::UNKNOWN)
    );
    let marked: std::vec::Vec<u64> = marked_sectors(&fat32).collect();
    assert_eq!(&marked[..3], &[1, 6, 7]);
    for fat in 0..u64::from(fat32.fats) {
        let start = usize::try_from(fat32.fat_start() + fat * fat32.fat_bytes()).unwrap();
        let table_bytes = &image[start..];
        assert_eq!(table::get(Kind::Fat32, table_bytes, 0), Some(0x0FFF_FFF8));
        assert_eq!(
            table::get(Kind::Fat32, table_bytes, 2),
            Some(table::end(Kind::Fat32)),
            "the root's chain"
        );
        assert_eq!(table::get(Kind::Fat32, table_bytes, 3), Some(0));
    }
    let floppy = plan(
        &medium(2880, 0xF0, (18, 2)),
        &shape(Some(Kind::Fat12), Some(1), 224),
    )
    .unwrap();
    let image = laid_out(&floppy);
    assert_eq!(&image[512..515], &[0xF0, 0xFF, 0xFF]);
    assert_eq!(
        image.len(),
        33 * 512,
        "the system area ends where the data begins"
    );
}
