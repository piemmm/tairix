//! The formats DOS lays down, each by the parameter block it is formatted
//! with: a diskette's — the BPB a drive's standard medium has, that a DOS 1
//! diskette's media byte stands for, and that `FORMAT` writes for a size —
//! and a fixed disk partition's, by its size; and the fields of such a
//! block, as a driver hands it to DOS.

use tairix_fat::boot::{Bpb, Kind, DOS_BPB};
use tairix_fat::format::{self, Medium, FIXED_MEDIA};
use tairix_fat::name;
use tairix_partition::mbr;

/// A BPB's fields in the form a block driver hands DOS, as offsets into it:
/// the boot sector's, from its bytes-per-sector field on.
pub mod bpb_at {
    use tairix_fat::boot::at;

    const fn of(field: usize) -> usize {
        field - at::BYTES_PER_SECTOR
    }

    pub const BYTES_PER_SECTOR: usize = of(at::BYTES_PER_SECTOR);
    pub const SECTORS_PER_CLUSTER: usize = of(at::SECTORS_PER_CLUSTER);
    pub const FATS: usize = of(at::FATS);
    pub const TOTAL: usize = of(at::TOTAL_SECTORS_16);
    pub const MEDIA: usize = of(at::MEDIA);
    pub const FAT: usize = of(at::FAT_SECTORS_16);
    pub const SECTORS: usize = of(at::SECTORS_PER_TRACK);
    pub const HEADS: usize = of(at::HEADS);
    pub const HIDDEN: usize = of(at::HIDDEN_SECTORS);
    pub const BIG_TOTAL: usize = of(at::TOTAL_SECTORS_32);
}

/// A BPB's 16-bit field at `at`.
#[must_use]
pub fn word_at(bpb: &[u8; DOS_BPB], at: usize) -> u16 {
    u16::from_le_bytes([bpb[at], bpb[at + 1]])
}

/// A BPB's 32-bit field at `at`.
#[must_use]
pub fn dword_at(bpb: &[u8; DOS_BPB], at: usize) -> u32 {
    u32::from_le_bytes([bpb[at], bpb[at + 1], bpb[at + 2], bpb[at + 3]])
}

/// A BPB's total sectors, from whichever field holds them.
#[must_use]
pub fn total_of(bpb: &[u8; DOS_BPB]) -> u32 {
    match word_at(bpb, bpb_at::TOTAL) {
        0 => dword_at(bpb, bpb_at::BIG_TOTAL),
        small => u32::from(small),
    }
}

/// A diskette format, by its capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Diskette {
    /// 5¼", one side of 40 tracks of 8 sectors.
    K160,
    /// 5¼", one side of 40 tracks of 9 sectors.
    K180,
    /// 5¼", two sides of 40 tracks of 8 sectors.
    K320,
    /// 5¼", two sides of 40 tracks of 9 sectors.
    K360,
    /// 3½", two sides of 80 tracks of 9 sectors.
    K720,
    /// 5¼" high density, two sides of 80 tracks of 15 sectors.
    K1200,
    /// 3½" high density, two sides of 80 tracks of 18 sectors.
    K1440,
}

/// What a format's BPB holds beyond what every diskette's does.
struct Layout {
    cluster: u8,
    root: u16,
    cylinders: u16,
    heads: u16,
    sectors: u16,
    media: u8,
    fat: u32,
}

impl Diskette {
    const fn layout(self) -> Layout {
        let (cluster, root, cylinders, heads, sectors, media, fat) = match self {
            Self::K160 => (1, 64, 40, 1, 8, 0xFE, 1),
            Self::K180 => (1, 64, 40, 1, 9, 0xFC, 2),
            Self::K320 => (2, 112, 40, 2, 8, 0xFF, 1),
            Self::K360 => (2, 112, 40, 2, 9, 0xFD, 2),
            Self::K720 => (2, 112, 80, 2, 9, 0xF9, 3),
            Self::K1200 => (1, 224, 80, 2, 15, 0xF9, 7),
            Self::K1440 => (1, 224, 80, 2, 18, 0xF0, 9),
        };
        Layout {
            cluster,
            root,
            cylinders,
            heads,
            sectors,
            media,
            fat,
        }
    }

    /// The format's parameter block.
    #[must_use]
    pub fn bpb(self) -> [u8; DOS_BPB] {
        self.plan().dos()
    }

    /// An unlabelled volume of the format numbered `serial`, as its boot
    /// sector records it.
    #[must_use]
    pub fn volume(self, serial: u32) -> Bpb {
        Bpb {
            serial: Some(serial),
            label: Some(name::label_field(name::NO_LABEL)),
            ..self.plan()
        }
    }

    fn plan(self) -> Bpb {
        let shape = self.layout();
        Bpb {
            kind: Kind::Fat12,
            bytes_per_sector: 512,
            sectors_per_cluster: shape.cluster,
            reserved_sectors: 1,
            fats: 2,
            root_entries: shape.root,
            total_sectors: u32::from(shape.cylinders)
                * u32::from(shape.heads)
                * u32::from(shape.sectors),
            media: shape.media,
            fat_sectors: shape.fat,
            sectors_per_track: shape.sectors,
            heads: shape.heads,
            hidden_sectors: 0,
            root_cluster: 0,
            fsinfo_sector: 0,
            backup_boot_sector: 0,
            drive: 0,
            serial: None,
            label: None,
        }
    }

    /// Its cylinders.
    #[must_use]
    pub const fn cylinders(self) -> u16 {
        self.layout().cylinders
    }

    /// The format a DOS 1 diskette's media byte `media` stands for: one of
    /// the four DOS 1 wrote, whose boot sectors carry no BPB.
    #[must_use]
    pub const fn of_media(media: u8) -> Option<Self> {
        match media {
            0xFE => Some(Self::K160),
            0xFC => Some(Self::K180),
            0xFF => Some(Self::K320),
            0xFD => Some(Self::K360),
            _ => None,
        }
    }

    /// The standard medium of a floppy drive of INT 13h type `kind`; a drive
    /// of no type the BIOS names is taken for a 360 KiB one.
    #[must_use]
    pub const fn of_drive(kind: u8) -> Self {
        match kind {
            2 => Self::K1200,
            3 => Self::K720,
            4 => Self::K1440,
            _ => Self::K360,
        }
    }
}

/// Whether DOS uses a partition of `type_byte`: one of a FAT12 or FAT16
/// volume.
#[must_use]
pub const fn is_dos(type_byte: u8) -> bool {
    matches!(
        type_byte,
        mbr::PART_TYPE_FAT12
            | mbr::PART_TYPE_FAT16_SMALL
            | mbr::PART_TYPE_FAT16_CHS
            | mbr::PART_TYPE_FAT16_LBA
    )
}

/// The most sectors of a partition DOS 4.0 gives a FAT12 volume.
pub const FAT12_MOST: u32 = 32_680;

/// The BPB DOS 4.0 formats a partition of `total` sectors from `start` with,
/// on a disk of `heads` heads and `sectors` sectors a track: the cluster
/// and table kind its table gives the size, a hard disk's root, and a table
/// sized for the clusters. A partition past what sixteen bits of cluster
/// number and 32 KiB clusters reach has none.
#[must_use]
pub fn partition(start: u32, total: u32, heads: u8, sectors: u8) -> Option<[u8; DOS_BPB]> {
    let (kind, cluster) = if total <= FAT12_MOST {
        (Kind::Fat12, 8)
    } else {
        match total {
            ..=0x4_0000 => (Kind::Fat16, 4),
            0x4_0001..=0x8_0000 => (Kind::Fat16, 8),
            0x8_0001..=0x10_0000 => (Kind::Fat16, 16),
            0x10_0001..=0x20_0000 => (Kind::Fat16, 32),
            0x20_0001..=0x40_0000 => (Kind::Fat16, 64),
            _ => return None,
        }
    };
    let medium = Medium {
        sectors: total,
        bytes_per_sector: 512,
        sectors_per_track: u16::from(sectors),
        heads: u16::from(heads),
        hidden_sectors: start,
        media: FIXED_MEDIA,
    };
    let shape = format::Shape {
        kind: Some(kind),
        sectors_per_cluster: Some(cluster),
        root_entries: 512,
        serial: 0,
        label: None,
    };
    format::plan(&medium, &shape).ok().map(|bpb| bpb.dos())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_format_is_a_fat12_volume_of_its_capacity() {
        for (diskette, kib) in [
            (Diskette::K160, 160),
            (Diskette::K180, 180),
            (Diskette::K320, 320),
            (Diskette::K360, 360),
            (Diskette::K720, 720),
            (Diskette::K1200, 1200),
            (Diskette::K1440, 1440),
        ] {
            let bpb = Bpb::from_dos(&diskette.bpb()).unwrap();
            assert_eq!(bpb.kind, Kind::Fat12, "{diskette:?}");
            assert_eq!(bpb.total_sectors * 512, kib * 1024, "{diskette:?}");
            let per_cylinder = u32::from(bpb.heads) * u32::from(bpb.sectors_per_track);
            assert_eq!(
                bpb.total_sectors / per_cylinder,
                u32::from(diskette.cylinders()),
                "{diskette:?}"
            );
        }
    }

    #[test]
    fn dos_1_media_bytes_name_the_four_formats_it_wrote() {
        assert_eq!(
            Diskette::of_media(0xFD).map(Diskette::bpb),
            Some(Diskette::K360.bpb())
        );
        assert_eq!(Diskette::of_media(0xFE), Some(Diskette::K160));
        assert_eq!(
            Diskette::of_media(0xF9),
            None,
            "a 720 KiB diskette always has its BPB"
        );
        assert_eq!(Diskette::of_drive(4), Diskette::K1440);
        assert_eq!(Diskette::of_drive(9), Diskette::K360);
    }

    #[test]
    fn a_partition_takes_the_cluster_its_size_does() {
        let small = Bpb::from_dos(&partition(17, 2159, 2, 17).unwrap()).unwrap();
        assert_eq!(
            (
                small.kind,
                small.sectors_per_cluster,
                small.root_entries,
                small.hidden_sectors
            ),
            (Kind::Fat12, 8, 512, 17)
        );
        let thirty = Bpb::from_dos(&partition(26, 63_934, 4, 26).unwrap()).unwrap();
        assert_eq!(
            (thirty.kind, thirty.sectors_per_cluster, thirty.media),
            (Kind::Fat16, 4, 0xF8)
        );
        let big = Bpb::from_dos(&partition(63, 0x30_0000, 255, 63).unwrap()).unwrap();
        assert_eq!(
            (big.kind, big.sectors_per_cluster, big.total_sectors),
            (Kind::Fat16, 64, 0x30_0000)
        );
        assert_eq!(partition(63, 0x40_0001, 255, 63), None, "past 2 GiB");
        assert_eq!(
            partition(1, 20, 2, 17),
            None,
            "too small for a hard disk's root"
        );
    }
}
