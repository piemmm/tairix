//! The boot sector and its BIOS parameter block: what a volume says of its
//! own shape, checked against what the format allows, and the layout of the
//! volume's regions that follows from it.
//!
//! A boot sector shaped for FAT32 — no fixed root directory and no 16-bit
//! FAT size — is FAT32, however few clusters it has, as Linux reads the
//! small FAT32 volumes `mkfs.fat` makes. Any other is FAT12 or FAT16 by its
//! count of clusters, at the thresholds the specification sets. The count is
//! bounded by what the allocation table can describe, so no reading of a
//! volume reaches past its table.

use crate::{le16, le32, put16, put32};

/// The bytes of the boot sector the parameter block lies in, whatever the
/// sector size.
pub const BOOT_SECTOR: usize = 512;
/// The boot sector's closing signature.
pub const SIGNATURE: [u8; 2] = [0x55, 0xAA];
/// The extended boot signature that declares a serial, a label and a type.
pub const EXTENDED: u8 = 0x29;
/// The older extended boot signature that declares a serial alone.
pub const EXTENDED_SERIAL_ONLY: u8 = 0x28;
/// The fewest clusters a FAT16 volume counts; below them a volume is FAT12.
pub const FAT16_CLUSTERS: u32 = 4085;
/// The fewest clusters a FAT32 volume should count; a FAT16 volume counts
/// fewer.
pub const FAT32_CLUSTERS: u32 = 65_525;
/// The most of a root directory's head a label entry is looked for in:
/// formatters write it first, so this bounds the read, not the format.
pub const LABEL_WINDOW: usize = 4096;

/// The boot sector's fields, by the specification's names.
pub mod at {
    /// `BS_jmpBoot`: the jump over the parameter block to the boot code.
    pub const JUMP: usize = 0;
    /// `BS_OEMName`: eight bytes naming the system that formatted it.
    pub const OEM: usize = 3;
    /// `BPB_BytsPerSec`.
    pub const BYTES_PER_SECTOR: usize = 11;
    /// `BPB_SecPerClus`.
    pub const SECTORS_PER_CLUSTER: usize = 13;
    /// `BPB_RsvdSecCnt`.
    pub const RESERVED_SECTORS: usize = 14;
    /// `BPB_NumFATs`.
    pub const FATS: usize = 16;
    /// `BPB_RootEntCnt`: the fixed root directory's entries, 0 on FAT32.
    pub const ROOT_ENTRIES: usize = 17;
    /// `BPB_TotSec16`.
    pub const TOTAL_SECTORS_16: usize = 19;
    /// `BPB_Media`.
    pub const MEDIA: usize = 21;
    /// `BPB_FATSz16`: one FAT's sectors, 0 on FAT32.
    pub const FAT_SECTORS_16: usize = 22;
    /// `BPB_SecPerTrk`.
    pub const SECTORS_PER_TRACK: usize = 24;
    /// `BPB_NumHeads`.
    pub const HEADS: usize = 26;
    /// `BPB_HiddSec`: the sectors before the volume on its disk.
    pub const HIDDEN_SECTORS: usize = 28;
    /// `BPB_TotSec32`.
    pub const TOTAL_SECTORS_32: usize = 32;
    /// FAT12 and FAT16's `BS_DrvNum`.
    pub const DRIVE: usize = 36;
    /// FAT12 and FAT16's `BS_BootSig`.
    pub const EXTENDED: usize = 38;
    /// FAT12 and FAT16's `BS_VolID`.
    pub const SERIAL: usize = 39;
    /// FAT12 and FAT16's `BS_VolLab`.
    pub const LABEL: usize = 43;
    /// FAT12 and FAT16's `BS_FilSysType`.
    pub const TYPE: usize = 54;
    /// Where FAT12 and FAT16's boot code begins.
    pub const CODE: usize = 62;
    /// `BPB_FATSz32`.
    pub const FAT_SECTORS_32: usize = 36;
    /// `BPB_ExtFlags`.
    pub const FLAGS_32: usize = 40;
    /// `BPB_FSVer`.
    pub const VERSION_32: usize = 42;
    /// `BPB_RootClus`.
    pub const ROOT_CLUSTER: usize = 44;
    /// `BPB_FSInfo`.
    pub const FSINFO: usize = 48;
    /// `BPB_BkBootSec`.
    pub const BACKUP_BOOT: usize = 50;
    /// FAT32's `BS_DrvNum`.
    pub const DRIVE_32: usize = 64;
    /// FAT32's `BS_BootSig`.
    pub const EXTENDED_32: usize = 66;
    /// FAT32's `BS_VolID`.
    pub const SERIAL_32: usize = 67;
    /// FAT32's `BS_VolLab`.
    pub const LABEL_32: usize = 71;
    /// FAT32's `BS_FilSysType`.
    pub const TYPE_32: usize = 82;
    /// Where FAT32's boot code begins.
    pub const CODE_32: usize = 90;
    /// The closing signature.
    pub const SIGNATURE: usize = 510;
}

/// Which allocation table a volume has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Twelve-bit entries.
    Fat12,
    /// Sixteen-bit entries.
    Fat16,
    /// Thirty-two-bit entries, of which 28 count.
    Fat32,
}

impl Kind {
    /// The bits one entry takes in the table.
    #[must_use]
    pub const fn entry_bits(self) -> u64 {
        match self {
            Self::Fat12 => 12,
            Self::Fat16 => 16,
            Self::Fat32 => 32,
        }
    }

    /// The most data clusters the kind can count: FAT12 and FAT16 up to the
    /// thresholds that type a volume, FAT32 up to the reserved values of its
    /// 28 bits.
    #[must_use]
    pub const fn max_clusters(self) -> u32 {
        match self {
            Self::Fat12 => FAT16_CLUSTERS - 1,
            Self::Fat16 => FAT32_CLUSTERS - 1,
            Self::Fat32 => 0x0FFF_FFF5,
        }
    }

    /// The file system type the extended parameter block names.
    #[must_use]
    pub const fn type_name(self) -> &'static [u8; 8] {
        match self {
            Self::Fat12 => b"FAT12   ",
            Self::Fat16 => b"FAT16   ",
            Self::Fat32 => b"FAT32   ",
        }
    }
}

/// A volume's parameters, as its boot sector records them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bpb {
    /// Which allocation table it has.
    pub kind: Kind,
    /// Bytes per sector: 512, 1024, 2048 or 4096.
    pub bytes_per_sector: u16,
    /// Sectors per cluster: a power of two.
    pub sectors_per_cluster: u8,
    /// Sectors before the first FAT, never 0.
    pub reserved_sectors: u16,
    /// Copies of the FAT: one or two.
    pub fats: u8,
    /// The fixed root directory's entries; 0 on FAT32.
    pub root_entries: u16,
    /// The volume's sectors.
    pub total_sectors: u32,
    /// The media descriptor.
    pub media: u8,
    /// One FAT's sectors, never 0.
    pub fat_sectors: u32,
    /// Sectors per track of the disk's INT 13h geometry; 0 for none.
    pub sectors_per_track: u16,
    /// Heads of that geometry; 0 for none.
    pub heads: u16,
    /// Sectors before the volume on its disk.
    pub hidden_sectors: u32,
    /// FAT32's root directory's first cluster; 0 on FAT12 and FAT16.
    pub root_cluster: u32,
    /// FAT32's `FSInfo` sector; 0 on FAT12 and FAT16.
    pub fsinfo_sector: u16,
    /// FAT32's backup boot sector; 0 on FAT12 and FAT16.
    pub backup_boot_sector: u16,
    /// The INT 13h drive number recorded for it.
    pub drive: u8,
    /// The volume serial, where the extended boot signature declares one.
    pub serial: Option<u32>,
    /// The label's copy in the boot sector, where the extended boot
    /// signature declares one.
    pub label: Option<[u8; 11]>,
}

/// Bytes of the parameter block a DOS block driver hands DOS: the boot
/// sector's from its bytes-per-sector field through its 32-bit total.
pub const DOS_BPB: usize = 25;

/// The serial, and the label where it carries one, that the extended boot
/// signature at `at` ([`at::EXTENDED`] or [`at::EXTENDED_32`]) declares;
/// read whether or not the parameter block around it is one of a volume.
#[must_use]
pub fn declared(boot: &[u8; BOOT_SECTOR], at: usize) -> (Option<u32>, Option<[u8; 11]>) {
    let signature = boot.get(at).copied();
    let serial = matches!(signature, Some(EXTENDED | EXTENDED_SERIAL_ONLY))
        .then(|| le32(boot, at + 1))
        .flatten();
    let label = (signature == Some(EXTENDED))
        .then(|| boot.get(at + 5..at + 16)?.try_into().ok())
        .flatten();
    (serial, label)
}

impl Bpb {
    /// The FAT12 or FAT16 parameter block `bpb` records, in the form a DOS
    /// block driver hands it over; `None` for one no such volume carries.
    #[must_use]
    pub fn from_dos(bpb: &[u8; DOS_BPB]) -> Option<Self> {
        let mut boot = [0u8; BOOT_SECTOR];
        boot[at::BYTES_PER_SECTOR..at::BYTES_PER_SECTOR + DOS_BPB].copy_from_slice(bpb);
        boot[at::SIGNATURE..at::SIGNATURE + 2].copy_from_slice(&SIGNATURE);
        Self::parse(&boot).filter(|parsed| parsed.kind != Kind::Fat32)
    }

    /// The boot sector's parameter bytes from its bytes-per-sector field
    /// through its 32-bit total: the form a DOS block driver hands over, as
    /// [`Bpb::from_dos`] reads it. The total takes the 16-bit field when it
    /// fits there, but never on FAT32.
    #[must_use]
    pub fn dos(&self) -> [u8; DOS_BPB] {
        let fat32 = self.kind == Kind::Fat32;
        let mut bpb = [0u8; DOS_BPB];
        let field = |at: usize| at - at::BYTES_PER_SECTOR;
        put16(&mut bpb, field(at::BYTES_PER_SECTOR), self.bytes_per_sector);
        bpb[field(at::SECTORS_PER_CLUSTER)] = self.sectors_per_cluster;
        put16(&mut bpb, field(at::RESERVED_SECTORS), self.reserved_sectors);
        bpb[field(at::FATS)] = self.fats;
        put16(&mut bpb, field(at::ROOT_ENTRIES), self.root_entries);
        let total_16 = u16::try_from(self.total_sectors).ok().filter(|_| !fat32);
        put16(&mut bpb, field(at::TOTAL_SECTORS_16), total_16.unwrap_or(0));
        bpb[field(at::MEDIA)] = self.media;
        let fat_16 = u16::try_from(self.fat_sectors).ok().filter(|_| !fat32);
        put16(&mut bpb, field(at::FAT_SECTORS_16), fat_16.unwrap_or(0));
        put16(
            &mut bpb,
            field(at::SECTORS_PER_TRACK),
            self.sectors_per_track,
        );
        put16(&mut bpb, field(at::HEADS), self.heads);
        put32(&mut bpb, field(at::HIDDEN_SECTORS), self.hidden_sectors);
        let total_32 = if total_16.is_some() {
            0
        } else {
            self.total_sectors
        };
        put32(&mut bpb, field(at::TOTAL_SECTORS_32), total_32);
        bpb
    }

    /// The parameter block `boot` records, or `None` for one no FAT volume
    /// carries: no signature, a sector or cluster size outside the format,
    /// no reserved sectors, a FAT count other than one or two, an empty FAT,
    /// no sectors past its system area, a FAT16-shaped block with too many
    /// clusters for sixteen bits, or a FAT32 root outside its clusters.
    #[must_use]
    pub fn parse(boot: &[u8; BOOT_SECTOR]) -> Option<Self> {
        if boot[at::SIGNATURE..at::SIGNATURE + 2] != SIGNATURE {
            return None;
        }
        let bytes_per_sector = le16(boot, at::BYTES_PER_SECTOR)?;
        let sectors_per_cluster = boot[at::SECTORS_PER_CLUSTER];
        let reserved_sectors = le16(boot, at::RESERVED_SECTORS)?;
        let fats = boot[at::FATS];
        let root_entries = le16(boot, at::ROOT_ENTRIES)?;
        let total_16 = le16(boot, at::TOTAL_SECTORS_16)?;
        let fat_16 = le16(boot, at::FAT_SECTORS_16)?;
        let total_32 = le32(boot, at::TOTAL_SECTORS_32)?;
        let sized = bytes_per_sector.is_power_of_two()
            && (512..=4096).contains(&bytes_per_sector)
            && sectors_per_cluster.is_power_of_two()
            && reserved_sectors != 0
            && (1..=2).contains(&fats);
        if !sized {
            return None;
        }
        let fat32 = root_entries == 0 && fat_16 == 0;
        let (fat_sectors, root_cluster, fsinfo_sector, backup_boot_sector, extended) = if fat32 {
            (
                le32(boot, at::FAT_SECTORS_32)?,
                le32(boot, at::ROOT_CLUSTER)?,
                le16(boot, at::FSINFO)?,
                le16(boot, at::BACKUP_BOOT)?,
                at::EXTENDED_32,
            )
        } else {
            (u32::from(fat_16), 0, 0, 0, at::EXTENDED)
        };
        let total_sectors = if total_16 != 0 {
            u32::from(total_16)
        } else {
            total_32
        };
        let (serial, label) = declared(boot, extended);
        let mut bpb = Self {
            kind: Kind::Fat32,
            bytes_per_sector,
            sectors_per_cluster,
            reserved_sectors,
            fats,
            root_entries,
            total_sectors,
            media: boot[at::MEDIA],
            fat_sectors,
            sectors_per_track: le16(boot, at::SECTORS_PER_TRACK)?,
            heads: le16(boot, at::HEADS)?,
            hidden_sectors: le32(boot, at::HIDDEN_SECTORS)?,
            root_cluster,
            fsinfo_sector,
            backup_boot_sector,
            drive: boot[if fat32 { at::DRIVE_32 } else { at::DRIVE }],
            serial,
            label,
        };
        if fat_sectors == 0 || u64::from(total_sectors) <= bpb.first_data_sector() {
            return None;
        }
        let by_size = bpb.clusters_by_size();
        bpb.kind = match (fat32, by_size) {
            (true, _) => Kind::Fat32,
            (false, n) if n < FAT16_CLUSTERS => Kind::Fat12,
            (false, n) if n < FAT32_CLUSTERS => Kind::Fat16,
            _ => return None,
        };
        let usable = bpb.clusters() >= 1;
        let root = !fat32 || (2..=bpb.max_cluster()).contains(&root_cluster);
        (usable && root).then_some(bpb)
    }

    /// The fixed root directory's sectors; none on FAT32.
    #[must_use]
    pub fn root_sectors(&self) -> u32 {
        let bytes = u32::from(self.root_entries) * 32;
        bytes.div_ceil(u32::from(self.bytes_per_sector))
    }

    /// The first sector of the data region, where cluster 2 begins.
    #[must_use]
    pub fn first_data_sector(&self) -> u64 {
        u64::from(self.reserved_sectors)
            + u64::from(self.fats) * u64::from(self.fat_sectors)
            + u64::from(self.root_sectors())
    }

    /// The clusters the sectors past the system area hold.
    fn clusters_by_size(&self) -> u32 {
        let data = u64::from(self.total_sectors).saturating_sub(self.first_data_sector());
        let clusters = data / u64::from(self.sectors_per_cluster.max(1));
        u32::try_from(clusters).unwrap_or(u32::MAX)
    }

    /// The data clusters the volume has: those its sectors hold, as many as
    /// its allocation table can describe and its kind can count.
    #[must_use]
    pub fn clusters(&self) -> u32 {
        let entries = u64::from(self.fat_sectors) * u64::from(self.bytes_per_sector) * 8
            / self.kind.entry_bits();
        let described = u32::try_from(entries.saturating_sub(2)).unwrap_or(u32::MAX);
        self.clusters_by_size()
            .min(described)
            .min(self.kind.max_clusters())
    }

    /// The highest data cluster's number: clusters run from 2 to it.
    #[must_use]
    pub fn max_cluster(&self) -> u32 {
        self.clusters() + 1
    }

    /// The bytes of a sector.
    #[must_use]
    pub fn sector_bytes(&self) -> u64 {
        u64::from(self.bytes_per_sector)
    }

    /// The bytes of a cluster.
    #[must_use]
    pub fn cluster_bytes(&self) -> u64 {
        u64::from(self.sectors_per_cluster) * self.sector_bytes()
    }

    /// Where the first FAT begins, in bytes.
    #[must_use]
    pub fn fat_start(&self) -> u64 {
        u64::from(self.reserved_sectors) * self.sector_bytes()
    }

    /// The bytes of one FAT.
    #[must_use]
    pub fn fat_bytes(&self) -> u64 {
        u64::from(self.fat_sectors) * self.sector_bytes()
    }

    /// Where the fixed root directory begins, in bytes: just past the FATs.
    #[must_use]
    pub fn root_start(&self) -> u64 {
        self.fat_start() + u64::from(self.fats) * self.fat_bytes()
    }

    /// The bytes of the fixed root directory; none on FAT32.
    #[must_use]
    pub fn root_bytes(&self) -> u64 {
        u64::from(self.root_sectors()) * self.sector_bytes()
    }

    /// Where the data region begins, in bytes.
    #[must_use]
    pub fn data_start(&self) -> u64 {
        self.first_data_sector() * self.sector_bytes()
    }

    /// Where cluster `cluster` begins, in bytes, if the volume has it.
    #[must_use]
    pub fn cluster_start(&self, cluster: u32) -> Option<u64> {
        (2..=self.max_cluster())
            .contains(&cluster)
            .then(|| self.data_start() + u64::from(cluster - 2) * self.cluster_bytes())
    }

    /// Where the volume's label entry is looked for: the head of its root
    /// directory — the root's first cluster on FAT32, the fixed region
    /// otherwise — at most [`LABEL_WINDOW`] of it; the byte it starts at and
    /// its length.
    #[must_use]
    pub fn label_window(&self) -> Option<(u64, usize)> {
        let (start, len) = match self.kind {
            Kind::Fat32 => (self.cluster_start(self.root_cluster)?, self.cluster_bytes()),
            Kind::Fat12 | Kind::Fat16 => (self.root_start(), self.root_bytes()),
        };
        Some((
            start,
            usize::try_from(len)
                .unwrap_or(LABEL_WINDOW)
                .min(LABEL_WINDOW),
        ))
    }

    /// The clusters whose data lies wholly within a device of
    /// `device_bytes`: a volume on a device smaller than it says is used no
    /// further than the device goes.
    #[must_use]
    pub fn clusters_within(&self, device_bytes: u64) -> u32 {
        let past = device_bytes.saturating_sub(self.data_start()) / self.cluster_bytes().max(1);
        self.clusters().min(u32::try_from(past).unwrap_or(u32::MAX))
    }
}

#[cfg(test)]
#[path = "boot_tests.rs"]
mod tests;
