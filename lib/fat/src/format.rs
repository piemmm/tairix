//! A new volume: the parameters it gets on a medium — kind, cluster size,
//! table size — the boot sector that records them, and what its system area
//! holds.
//!
//! The kind follows the volume's size unless one is asked for: FAT12 below
//! 16 MiB, where DOS lays FAT12 partitions, FAT16 to 2 GiB, FAT32 beyond.
//! FAT16's cluster sizes are the specification's table, FAT32's the
//! thresholds `mkfs.fat` applies, and FAT12 takes the smallest cluster that
//! keeps it under its count. The table is sized exactly: as small as maps
//! every cluster that fits after it. Whatever the request, the result
//! counts the clusters its kind is typed by, so every reader agrees on it.
//!
//! A system area is all zeros but for a few sectors. Its boot sector is
//! written last, so no reader takes a half-made volume for one.

use crate::boot::{
    at, Bpb, Kind, BOOT_SECTOR, DOS_BPB, EXTENDED, FAT16_CLUSTERS, FAT32_CLUSTERS, SIGNATURE,
};
use crate::fsinfo::FsInfo;
use crate::name::{label_field, NO_LABEL};
use crate::{put16, put32, table};

/// The sectors before a FAT12 or FAT16 volume's first FAT.
const RESERVED: u16 = 1;
/// The sectors before a FAT32 volume's first FAT: room for the boot
/// sector, `FSInfo` and their backups.
const RESERVED_32: u16 = 32;
/// FAT32's `FSInfo` sector, and its backup boot sector, which the backup
/// `FSInfo` follows.
const FSINFO: u16 = 1;
const BACKUP_BOOT: u16 = 6;
/// Copies of the FAT a new volume has.
const FATS: u8 = 2;
/// A FAT12 or FAT16 hard disk's root directory entries when none are
/// asked for.
const ROOT_ENTRIES: u16 = 512;
/// The media descriptor of a fixed disk.
pub const FIXED_MEDIA: u8 = 0xF8;
/// The BIOS's drive number for the first fixed disk, which a fixed disk's
/// boot sector records; each further one is the next.
pub const FIXED_DRIVE: u8 = 0x80;
/// Below this size a volume is FAT12, to this size FAT16, when no kind is
/// asked for.
const FAT12_BELOW: u64 = 16 << 20;
const FAT16_UPTO: u64 = 2 << 30;
/// FAT16's cluster sizes by volume size, as the specification tabulates
/// them for 512-byte sectors: to so many sectors, so many sectors a
/// cluster.
const FAT16_CLUSTERS_BY_SIZE: [(u64, u64); 6] = [
    (32_680, 2),
    (262_144, 4),
    (524_288, 8),
    (1_048_576, 16),
    (2_097_152, 32),
    (4_194_304, 64),
];

/// What a volume is laid on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Medium {
    /// Its sectors.
    pub sectors: u32,
    /// The bytes of each.
    pub bytes_per_sector: u16,
    /// Sectors per track of its INT 13h geometry; 0 for none.
    pub sectors_per_track: u16,
    /// Heads of that geometry; 0 for none.
    pub heads: u16,
    /// Sectors before it on its disk.
    pub hidden_sectors: u32,
    /// Its media descriptor.
    pub media: u8,
}

/// What is asked of a new volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    /// Its kind, or one chosen by its size.
    pub kind: Option<Kind>,
    /// Sectors per cluster, or as many as its size chooses.
    pub sectors_per_cluster: Option<u8>,
    /// The fixed root directory's entries, on FAT12 and FAT16; 0 for a
    /// hard disk's 512.
    pub root_entries: u16,
    /// Its serial.
    pub serial: u32,
    /// Its label, if any.
    pub label: Option<[u8; 11]>,
}

/// Why no volume could be planned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatError {
    /// A sector size no FAT volume has.
    SectorSize,
    /// A cluster size no FAT volume has.
    ClusterSize,
    /// Too few sectors for the kind asked for, or for any.
    TooSmall,
    /// Too many sectors for the kind asked for.
    TooLarge,
}

/// The parameter block a new volume gets on `medium`.
pub fn plan(medium: &Medium, shape: &Shape) -> Result<Bpb, FormatError> {
    let bps = medium.bytes_per_sector;
    if !bps.is_power_of_two() || !(512..=4096).contains(&bps) {
        return Err(FormatError::SectorSize);
    }
    let bytes = u64::from(medium.sectors) * u64::from(bps);
    let kind = shape.kind.unwrap_or(if bytes < FAT12_BELOW {
        Kind::Fat12
    } else if bytes <= FAT16_UPTO {
        Kind::Fat16
    } else {
        Kind::Fat32
    });
    let (reserved, root_entries) = match kind {
        Kind::Fat32 => (RESERVED_32, 0),
        _ if shape.root_entries == 0 => (RESERVED, ROOT_ENTRIES),
        _ => (RESERVED, shape.root_entries),
    };
    let mut bpb = Bpb {
        kind,
        bytes_per_sector: bps,
        sectors_per_cluster: 1,
        reserved_sectors: reserved,
        fats: FATS,
        root_entries,
        total_sectors: medium.sectors,
        media: medium.media,
        fat_sectors: 1,
        sectors_per_track: medium.sectors_per_track,
        heads: medium.heads,
        hidden_sectors: medium.hidden_sectors,
        root_cluster: if kind == Kind::Fat32 { 2 } else { 0 },
        fsinfo_sector: if kind == Kind::Fat32 { FSINFO } else { 0 },
        backup_boot_sector: if kind == Kind::Fat32 { BACKUP_BOOT } else { 0 },
        drive: if medium.media == FIXED_MEDIA {
            FIXED_DRIVE
        } else {
            0x00
        },
        serial: Some(shape.serial),
        label: Some(shape.label.unwrap_or_else(|| label_field(NO_LABEL))),
    };
    let clusters = match shape.sectors_per_cluster {
        Some(spc) if spc.is_power_of_two() => size_table(&mut bpb, spc)?,
        Some(_) => return Err(FormatError::ClusterSize),
        None => match kind {
            Kind::Fat12 => fat12_cluster(&mut bpb)?,
            Kind::Fat16 => size_table(&mut bpb, fat16_cluster(bytes, bps))?,
            Kind::Fat32 => size_table(&mut bpb, fat32_cluster(bytes, bps))?,
        },
    };
    let (fewest, most) = match kind {
        Kind::Fat12 => (1, FAT16_CLUSTERS - 1),
        Kind::Fat16 => (FAT16_CLUSTERS, FAT32_CLUSTERS - 1),
        Kind::Fat32 => (FAT32_CLUSTERS, kind.max_clusters()),
    };
    if clusters < u64::from(fewest) {
        return Err(FormatError::TooSmall);
    }
    if clusters > u64::from(most) {
        return Err(FormatError::TooLarge);
    }
    Ok(bpb)
}

/// Sets `bpb`'s cluster size to `spc` and its table to the smallest that
/// maps every cluster after it; returns their count.
fn size_table(bpb: &mut Bpb, spc: u8) -> Result<u64, FormatError> {
    bpb.sectors_per_cluster = spc;
    let bps = u64::from(bpb.bytes_per_sector);
    let fixed = u64::from(bpb.reserved_sectors) + u64::from(bpb.root_sectors());
    let fats = u64::from(bpb.fats);
    let mut fat_sectors = 1u64;
    loop {
        let data = u64::from(bpb.total_sectors)
            .checked_sub(fixed + fats * fat_sectors)
            .ok_or(FormatError::TooSmall)?;
        let clusters = data / u64::from(spc);
        let needed = ((clusters + 2) * bpb.kind.entry_bits())
            .div_ceil(8)
            .div_ceil(bps)
            .max(1);
        if needed <= fat_sectors {
            bpb.fat_sectors = u32::try_from(fat_sectors).map_err(|_| FormatError::TooLarge)?;
            return Ok(clusters);
        }
        fat_sectors = needed;
    }
}

/// FAT12's cluster: the smallest that keeps the volume under FAT12's count.
fn fat12_cluster(bpb: &mut Bpb) -> Result<u64, FormatError> {
    let mut spc = 1u8;
    loop {
        let clusters = size_table(bpb, spc)?;
        if clusters < u64::from(FAT16_CLUSTERS) {
            return Ok(clusters);
        }
        spc = spc.checked_mul(2).ok_or(FormatError::TooLarge)?;
    }
}

/// FAT16's cluster for a volume of `bytes`, from the specification's table.
fn fat16_cluster(bytes: u64, bps: u16) -> u8 {
    let sectors_512 = bytes / 512;
    let spc_512 = FAT16_CLUSTERS_BY_SIZE
        .iter()
        .find(|&&(upto, _)| sectors_512 <= upto)
        .map_or(64, |&(_, spc)| spc);
    in_sectors(spc_512 * 512, bps)
}

/// FAT32's cluster for a volume of `bytes`, at the thresholds `mkfs.fat`
/// applies.
fn fat32_cluster(bytes: u64, bps: u16) -> u8 {
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    let cluster_bytes = match bytes {
        b if b <= 64 * MIB => 512,
        b if b <= 128 * MIB => 1024,
        b if b <= 256 * MIB => 2048,
        b if b <= 8 * GIB => 4096,
        b if b <= 16 * GIB => 8192,
        b if b <= 32 * GIB => 16_384,
        _ => 32_768,
    };
    in_sectors(cluster_bytes, bps)
}

/// `cluster_bytes` as sectors of `bps`: at least one, at most 128.
fn in_sectors(cluster_bytes: u64, bps: u16) -> u8 {
    u8::try_from((cluster_bytes / u64::from(bps)).clamp(1, 128)).unwrap_or(128)
}

/// The boot sector recording `bpb`, named `oem`, its jump landing on `code`
/// — boot code, cut short where it does not fit.
#[must_use]
pub fn boot_sector(bpb: &Bpb, oem: &[u8; 8], code: &[u8]) -> [u8; BOOT_SECTOR] {
    let fat32 = bpb.kind == Kind::Fat32;
    let code_at = if fat32 { at::CODE_32 } else { at::CODE };
    let mut boot = [0; BOOT_SECTOR];
    boot[at::JUMP] = 0xEB;
    boot[at::JUMP + 1] = u8::try_from(code_at - 2).unwrap_or(0);
    boot[at::JUMP + 2] = 0x90;
    boot[at::OEM..at::OEM + 8].copy_from_slice(oem);
    boot[at::BYTES_PER_SECTOR..at::BYTES_PER_SECTOR + DOS_BPB].copy_from_slice(&bpb.dos());
    let drive = if fat32 {
        put32(&mut boot, at::FAT_SECTORS_32, bpb.fat_sectors);
        put32(&mut boot, at::ROOT_CLUSTER, bpb.root_cluster);
        put16(&mut boot, at::FSINFO, bpb.fsinfo_sector);
        put16(&mut boot, at::BACKUP_BOOT, bpb.backup_boot_sector);
        at::DRIVE_32
    } else {
        at::DRIVE
    };
    boot[drive] = bpb.drive;
    boot[drive + 2] = EXTENDED;
    put32(&mut boot, drive + 3, bpb.serial.unwrap_or(0));
    boot[drive + 7..drive + 18]
        .copy_from_slice(&bpb.label.unwrap_or_else(|| label_field(NO_LABEL)));
    boot[drive + 18..drive + 26].copy_from_slice(bpb.kind.type_name());
    let room = at::SIGNATURE - code_at;
    let code = &code[..code.len().min(room)];
    boot[code_at..code_at + code.len()].copy_from_slice(code);
    boot[at::SIGNATURE..at::SIGNATURE + 2].copy_from_slice(&SIGNATURE);
    boot
}

/// The sectors a new volume's system area takes: the reserved sectors, the
/// FATs, the fixed root, and on FAT32 its root directory's cluster.
#[must_use]
pub fn system_sectors(bpb: &Bpb) -> u64 {
    let root_cluster = if bpb.kind == Kind::Fat32 {
        u64::from(bpb.sectors_per_cluster)
    } else {
        0
    };
    bpb.first_data_sector() + root_cluster
}

/// The system area's sectors that hold more than zeros, but for the boot
/// sector: `FSInfo` and its backups on FAT32, then each FAT's first sector.
pub fn marked_sectors(bpb: &Bpb) -> impl Iterator<Item = u64> + '_ {
    let backups = (bpb.kind == Kind::Fat32).then(|| {
        let backup = u64::from(bpb.backup_boot_sector);
        [u64::from(bpb.fsinfo_sector), backup, backup + 1]
    });
    let fats = (0..u64::from(bpb.fats))
        .map(|fat| u64::from(bpb.reserved_sectors) + fat * u64::from(bpb.fat_sectors));
    backups.into_iter().flatten().chain(fats)
}

/// What sector `sector` of a new volume holds, written into `out` — a
/// sector long — given its `boot` sector: zeros, but for the boot sector
/// and its backup, `FSInfo` and its backup, and each FAT's first entries.
pub fn system_sector(bpb: &Bpb, boot: &[u8; BOOT_SECTOR], sector: u64, out: &mut [u8]) {
    out.fill(0);
    let fat32 = bpb.kind == Kind::Fat32;
    let backup = u64::from(bpb.backup_boot_sector);
    let place = |out: &mut [u8], bytes: &[u8]| {
        let len = out.len().min(bytes.len());
        out[..len].copy_from_slice(&bytes[..len]);
    };
    if sector == 0 || fat32 && sector == backup {
        place(out, boot);
    } else if fat32 && (sector == u64::from(bpb.fsinfo_sector) || sector == backup + 1) {
        place(out, &FsInfo::UNKNOWN.encode());
    } else if (0..u64::from(bpb.fats))
        .any(|fat| sector == u64::from(bpb.reserved_sectors) + fat * u64::from(bpb.fat_sectors))
    {
        let [media, end] = table::head(bpb.kind, bpb.media);
        table::set(bpb.kind, out, 0, media);
        table::set(bpb.kind, out, 1, end);
        if fat32 {
            table::set(bpb.kind, out, bpb.root_cluster, table::end(bpb.kind));
        }
    }
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
