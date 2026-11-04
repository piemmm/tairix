//! The disc title: the name a new map's disc record keeps, in zone 0 of both
//! map copies and in the boot block, and the ten bytes an old map spells
//! across its two sectors.

use super::*;
use std::string::String;
use tairix_abi::driver::filesystem::{FilesystemLabel, VolumeTitle, WriteRefusal};
use tairix_fsprobe::adfs::{
    boot_block_checksum, old_map_checksum, validate_old_map, zone_check, DiscRecord, MapCopy,
    DISC_RECORD_IN_BOOT_BLOCK, OLD_MAP_SIZE, OLD_NAME_BYTES, ZONE_CHECK_BYTE, ZONE_HEADER_SIZE,
};
use tairix_fsprobe::{ProbedVolume, Recognised};

/// An old-map and a new-map floppy, for what both maps share.
const BOTH_MAPS: [(AdfsVariant, usize); 2] =
    [(AdfsVariant::L, 640 * 1024), (AdfsVariant::F, 1600 * 1024)];

fn title(text: &str) -> VolumeTitle {
    VolumeTitle::new(text).expect("a title")
}

fn label_of(fs: &mut Adfs<MemDisk>) -> String {
    String::from(fs.label().expect("a title").as_str())
}

/// The volume the probe reads from `disk`.
fn probed(disk: MemDisk) -> ProbedVolume {
    let mut dev = ByteDevice::new(disk).expect("a device");
    match tairix_fsprobe::recognise(&mut dev) {
        Ok(Some(Recognised::Volume(volume))) => volume,
        other => panic!("no ADFS volume: {other:?}"),
    }
}

/// The disc name `disk` records, as its bytes lie on the disc, and the disk.
fn recorded_name(disk: MemDisk) -> (Vec<u8>, MemDisk) {
    let mut dev = ByteDevice::new(disk).expect("a device");
    let layout = tairix_fsprobe::adfs::identify(&mut dev)
        .expect("readable")
        .expect("an ADFS volume");
    let (name, len) = layout.name();
    (name[..len].to_vec(), dev.into_inner())
}

/// The disc record zone 0 of `copy` embeds in `image`, its check byte held.
fn zone_record(image: &[u8], record: &DiscRecord, copy: MapCopy) -> DiscRecord {
    let at = usize::try_from(record.zone_offset(copy, 0)).expect("fits");
    let len = usize::try_from(record.sector_size()).expect("fits");
    let zone = &image[at..at + len];
    assert_eq!(zone_check(zone), zone[ZONE_CHECK_BYTE], "{copy:?}");
    let raw = zone[ZONE_HEADER_SIZE..ZONE_HEADER_SIZE + DISC_RECORD_SIZE]
        .try_into()
        .expect("a record");
    DiscRecord::parse(raw)
}

/// A title reads back from the mount, a remount and the volume probe on
/// every variant.
#[test]
fn a_title_reads_back_from_the_mount_a_remount_and_the_probe() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        fs.set_label(&title("Archive")).expect("retitled");
        assert_eq!(label_of(&mut fs), "Archive", "{variant:?}");
        let mut fs = Adfs::open(fs.into_device()).expect("remounts");
        assert_eq!(label_of(&mut fs), "Archive", "{variant:?}");
        assert_eq!(probed(fs.into_device()).label(), b"Archive", "{variant:?}");
    }
}

/// A title is spelt on the disc in RISC OS's Latin-1, its own characters in
/// `0x80`–`0x9F` among them, and read back as the same text by the driver
/// and the volume probe alike.
#[test]
fn a_title_is_spelt_in_risc_os_latin_1() {
    for (variant, bytes) in BOTH_MAPS {
        let mut fs = fresh(variant, bytes);
        fs.set_label(&title("Café€")).expect("retitled");
        let mut fs = Adfs::open(fs.into_device()).expect("remounts");
        assert_eq!(label_of(&mut fs), "Café€", "{variant:?}");
        let (recorded, disk) = recorded_name(fs.into_device());
        assert_eq!(recorded, b"Caf\xE9\x80", "{variant:?}");
        assert_eq!(probed(disk).label(), "Café€".as_bytes(), "{variant:?}");
    }
}

/// A new map's title lands in the disc record zone 0 of both map copies
/// embeds, each still checking, and in the boot block of a volume that has
/// one, resealed.
#[test]
fn a_new_map_title_lands_in_both_map_copies_and_the_boot_block() {
    for (variant, bytes) in VARIANTS {
        if variant.is_old_map() {
            continue;
        }
        let mut fs = fresh(variant, bytes);
        fs.set_label(&title("Disc1")).expect("retitled");
        let Backing::New { map, boot_block } = &fs.backing else {
            panic!("{variant:?} has a new map");
        };
        let (record, boot_block) = (map.record, *boot_block);
        let image = fs.into_device().data;
        for copy in [MapCopy::First, MapCopy::Second] {
            let kept = zone_record(&image, &record, copy);
            assert_eq!(&kept.disc_name, b"Disc1     ", "{variant:?}");
        }
        if boot_block {
            let at = usize::try_from(BOOT_BLOCK_OFFSET).expect("fits");
            let block: &[u8; BOOT_BLOCK_SIZE] =
                image[at..at + BOOT_BLOCK_SIZE].try_into().expect("a block");
            assert_eq!(boot_block_checksum(block), block[BOOT_BLOCK_SIZE - 1]);
            let raw = block[DISC_RECORD_IN_BOOT_BLOCK..][..DISC_RECORD_SIZE]
                .try_into()
                .expect("a record");
            assert_eq!(&DiscRecord::parse(raw).disc_name, b"Disc1     ");
        }
    }
}

/// An old map's title is spelt across its two sectors a character at a
/// time, both resealed, and nothing else on the disc moves.
#[test]
fn an_old_map_title_spans_both_sectors_and_reseals_them() {
    // Each sector's last byte is its checksum.
    let sums = [OLD_MAP_SIZE / 2 - 1, OLD_MAP_SIZE - 1];
    for (variant, bytes) in VARIANTS {
        if !variant.is_old_map() {
            continue;
        }
        let mut fs = fresh(variant, bytes);
        let before = fs.volume.device().data.clone();
        fs.set_label(&title("Games")).expect("retitled");
        let after = fs.into_device().data;
        let map: &[u8; OLD_MAP_SIZE] = after[..OLD_MAP_SIZE].try_into().expect("a map");
        assert_eq!(validate_old_map(map), Ok(()), "{variant:?}");
        let name: Vec<u8> = OLD_NAME_BYTES.iter().map(|&at| after[at]).collect();
        assert_eq!(name, b"Games     ", "{variant:?}");
        let stray = (0..after.len()).find(|&at| {
            after[at] != before[at] && !OLD_NAME_BYTES.contains(&at) && !sums.contains(&at)
        });
        assert_eq!(stray, None, "{variant:?}");
    }
}

/// A title the disc cannot hold is refused and nothing written: an eleventh
/// character, a space, RISC OS's specials, and a character its Latin-1
/// lacks.
#[test]
fn a_title_the_disc_cannot_hold_is_refused_and_nothing_written() {
    for (variant, bytes) in BOTH_MAPS {
        let mut fs = fresh(variant, bytes);
        let before = fs.volume.device().data.clone();
        for (text, refusal) in [
            ("ElevenChars", DriverError::LengthOutOfRange),
            ("Two Words", DriverError::OutOfRange),
            ("Disc.1", DriverError::OutOfRange),
            ("Disc:1", DriverError::OutOfRange),
            ("$Root", DriverError::OutOfRange),
            ("Ωmega", DriverError::OutOfRange),
        ] {
            let refused = fs.set_label(&title(text));
            assert_eq!(refused, Err(refusal), "{variant:?}: {text}");
        }
        assert_eq!(fs.volume.device().data, before, "{variant:?}");
        fs.set_label(&title("TenLetters")).expect("ten fit");
        assert_eq!(label_of(&mut fs), "TenLetters", "{variant:?}");
    }
}

/// The empty title clears the name, which then reads back as none.
#[test]
fn the_empty_title_clears_the_name() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        fs.set_label(&title("Named")).expect("named");
        fs.set_label(&title("")).expect("cleared");
        let mut fs = Adfs::open(fs.into_device()).expect("remounts");
        assert_eq!(label_of(&mut fs), "", "{variant:?}");
        assert!(probed(fs.into_device()).label().is_empty(), "{variant:?}");
    }
}

/// A volume served read-only refuses a title and is left as it was.
#[test]
fn a_volume_served_read_only_refuses_a_title() {
    for (variant, bytes) in BOTH_MAPS {
        let mut disk = fresh(variant, bytes).into_device();
        disk.data.truncate(bytes - 5 * 1024);
        let image = disk.data.clone();
        let mut fs = Adfs::open(disk).expect("opens");
        assert_eq!(fs.write_refusal(), Some(WriteRefusal::Truncated));
        let named = label_of(&mut fs);
        let refused = fs.set_label(&title("Kept"));
        assert_eq!(refused, Err(DriverError::ReadOnly), "{variant:?}");
        assert_eq!(label_of(&mut fs), named, "{variant:?}");
        assert_eq!(fs.into_device().data, image, "{variant:?}");
    }
}

/// A disc name holding a byte RISC OS's Latin-1 leaves undefined ends there,
/// read alike by the driver and the volume probe, and the disc is retitled
/// all the same.
#[test]
fn a_name_ending_at_an_undefined_byte_reads_alike_and_is_retitled() {
    let mut disk = fresh(AdfsVariant::L, 640 * 1024).into_device();
    for (&at, &byte) in OLD_NAME_BYTES.iter().zip(b"Disc\x831    ") {
        disk.data[at] = byte;
    }
    disk.data[255] = old_map_checksum(&disk.data[..256]);
    disk.data[511] = old_map_checksum(&disk.data[256..512]);
    let copy = MemDisk {
        data: disk.data.clone(),
        ..MemDisk::new(0, 512)
    };
    assert_eq!(probed(copy).label(), b"Disc");
    let mut fs = Adfs::open(disk).expect("opens");
    assert_eq!(label_of(&mut fs), "Disc");
    fs.set_label(&title("Work")).expect("retitled");
    assert_eq!(label_of(&mut fs), "Work");
}

/// A retitle changes no part of the disc's identity, which what formatting
/// fixed makes: a remount and the volume probe find the one it had, on every
/// variant.
#[test]
fn a_retitle_keeps_the_identity() {
    for (variant, bytes) in VARIANTS {
        let mut fs = fresh(variant, bytes);
        let identity = fs.volume_identity();
        fs.set_label(&title("Renamed")).expect("retitled");
        let fs = Adfs::open(fs.into_device()).expect("remounts");
        assert_eq!(fs.volume_identity(), identity, "{variant:?}");
        assert_eq!(probed(fs.into_device()).identity, identity, "{variant:?}");
    }
}

/// A disk taking `writes` more writes, then failing every one.
struct Faulting {
    disk: MemDisk,
    writes: usize,
}

impl Block for Faulting {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        self.disk.geometry()
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.disk.read_blocks(lba, buf)
    }

    fn write_blocks(&mut self, lba: u64, data: &[u8]) -> Result<(), DriverError> {
        let Some(left) = self.writes.checked_sub(1) else {
            return Err(DriverError::DeviceFault);
        };
        self.writes = left;
        self.disk.write_blocks(lba, data)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

/// A retitle failing part-way keeps the name the map held, rather than leave
/// the refused one for the next change to the disc record to write out, and
/// leaves the volume refusing every later change, damaged.
#[test]
fn a_retitle_failing_part_way_keeps_its_name_and_damages_the_volume() {
    let disk = fresh(AdfsVariant::F, 1600 * 1024).into_device();
    let mut fs = Adfs::open(Faulting { disk, writes: 1 }).expect("opens");
    let held = |fs: &Adfs<Faulting>| match &fs.backing {
        Backing::New { map, .. } => map.record.disc_name,
        Backing::Old { .. } => panic!("F has a new map"),
    };
    let named = held(&fs);
    assert_eq!(fs.set_label(&title("Torn")), Err(DriverError::DeviceFault));
    assert_eq!(held(&fs), named, "the refused name is not held");
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::Damaged));
    let root = fs.root();
    assert_eq!(
        fs.create(root, b"Later", NodeKind::RegularFile, &FILE_RECORD),
        Err(DriverError::ReadOnly)
    );
}
