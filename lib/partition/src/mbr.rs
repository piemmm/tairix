//! Classic MBR (master boot record) partition scheme.
//!
//! An MBR disk carries its table in the first 512-byte sector: four
//! 16-byte primary-partition entries at offset [`PARTITION_TABLE_OFFSET`]
//! and the `0x55AA` signature at the end. LBAs and sector counts are
//! 32-bit, so an MBR addresses at most 2 TiB of 512-byte sectors; past
//! that a disk must use [`crate::gpt`].
//!
//! This module both writes a table (the image author, [`encode`]; a DOS
//! disk's author, [`encode_entries`]) and reads one back (the boot path,
//! [`parse`]); both enforce the same extent invariants through one shared
//! validator (`validate_extents`), so a table this module writes is always
//! one it will read back, and a malformed/hostile table is rejected whole at
//! both ends.
//!
//! An entry records its extent twice: by LBA, and by the cylinder, head and
//! sector a BIOS reaches it through INT 13h, which DOS and its boot records
//! read. A table for a disk with a [`Geometry`] carries those addresses; one
//! for any other disk carries the all-ones "use LBA" form instead.

use crate::{Partition, PartitionTable, PartitionType};
use tairix_fat::boot::Kind;

/// MBR partition-type byte for a FAT32 partition addressed via LBA.
///
/// The platform firmware scans the
/// four primary entries for a FAT partition and reads the firmware files
/// from it; [`encode`] writes the boot partition with this type.
pub const PART_TYPE_FAT32_LBA: u8 = 0x0c;

/// MBR partition-type byte for a FAT32 partition addressed via CHS, also
/// classified as [`PartitionType::FatBoot`] on parse.
pub const PART_TYPE_FAT32_CHS: u8 = 0x0b;

/// MBR partition-type byte for a FAT12 partition.
pub const PART_TYPE_FAT12: u8 = 0x01;

/// MBR partition-type byte for a FAT16 partition of fewer than 65,536
/// sectors, the only FAT16 type DOS before 3.31 reads.
pub const PART_TYPE_FAT16_SMALL: u8 = 0x04;

/// MBR partition-type byte for a FAT16 partition of 65,536 sectors or more
/// addressed via CHS.
pub const PART_TYPE_FAT16_CHS: u8 = 0x06;

/// MBR partition-type byte for a FAT16 partition addressed via LBA.
pub const PART_TYPE_FAT16_LBA: u8 = 0x0e;

/// MBR partition-type byte for the encrypted `ARXFS` data-root partition.
///
/// `0x7f` is the de-facto "reserved for individual or local use" type for
/// a filesystem without an assigned identifier. `ARXFS` volumes are
/// self-identifying (superblock magic + checksums), so the type byte is a
/// routing hint, never a trusted input.
pub const PART_TYPE_ARXFS: u8 = 0x7f;

/// MBR partition-type byte for the read-only, signed-bundle `ARXFS`
/// `/System` partition (the design-B pre-unlock store, `plans/PI.md`).
///
/// `0x7e` is a sibling "reserved for individual or local use" type,
/// distinct from [`PART_TYPE_ARXFS`] so the boot path tells the read-only
/// `/System` volume apart from the data root by role. Like every type byte it
/// is a routing hint, never a trusted input: the volume's own superblock still
/// decides how it opens, and its bundles are still verified against the load
/// gate's trust anchor.
pub const PART_TYPE_ARXFS_SYSTEM: u8 = 0x7e;

/// A partition entry with this type byte is unused; it is skipped on read
/// rather than treated as a (malformed) zero-length partition.
pub const PART_TYPE_UNUSED: u8 = 0x00;

/// Size of the MBR sector, in bytes: a boot sector's.
pub const MBR_SECTOR_LEN: usize = tairix_fat::boot::BOOT_SECTOR;

/// Byte offset of the first 16-byte primary-partition entry in the MBR
/// sector. The 446 bytes before it hold the code a PC's BIOS starts the
/// disk with; TAIRiX's own boot does not use it.
pub const PARTITION_TABLE_OFFSET: usize = 446;

/// Bytes per primary-partition entry.
pub const PARTITION_ENTRY_LEN: usize = 16;

/// Number of primary-partition entries an MBR holds.
pub const MAX_PRIMARY_PARTITIONS: usize = 4;

/// The two boot-signature bytes at the end of the MBR sector, as every
/// boot sector ends.
pub const MBR_SIGNATURE: [u8; 2] = tairix_fat::boot::SIGNATURE;

/// An entry's status byte when it is the active partition.
pub const ACTIVE: u8 = 0x80;

/// The CHS address an entry records where the disk has no geometry: the
/// all-ones "use LBA" form.
const USE_LBA: [u8; 3] = [0xff; 3];

/// The highest cylinder a CHS address names.
pub const MAX_CYLINDER: u32 = 1023;
/// The most sectors a track a CHS address counts.
pub const MAX_SECTORS: u8 = 63;

/// The heads and sectors per track a BIOS addresses a disk by through
/// INT 13h, which an entry's CHS addresses count in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Geometry {
    heads: u8,
    sectors: u8,
}

impl Geometry {
    /// `heads` heads of `sectors` sectors a track, or `None` outside what a
    /// CHS address counts: 1 to 255 heads, 1 to 63 sectors.
    #[must_use]
    pub const fn new(heads: u8, sectors: u8) -> Option<Self> {
        if heads == 0 || sectors == 0 || sectors > MAX_SECTORS {
            None
        } else {
            Some(Self { heads, sectors })
        }
    }

    /// Whether a CHS address reaches `lba`: whether it lies within the first
    /// 1,024 cylinders.
    #[must_use]
    pub const fn reaches(self, lba: u32) -> bool {
        lba / self.per_cylinder() <= MAX_CYLINDER
    }

    /// `lba` as an entry's CHS address holds it: the head, the sector with
    /// the cylinder's top two bits above it, and the cylinder's low byte. An
    /// address past the 1,024 cylinders records the last one there is, as DOS
    /// and Windows record it.
    #[must_use]
    pub fn chs(self, lba: u32) -> [u8; 3] {
        let (heads, sectors) = (u32::from(self.heads), u32::from(self.sectors));
        let (cylinder, head, sector) = if self.reaches(lba) {
            (
                lba / self.per_cylinder(),
                lba / sectors % heads,
                lba % sectors + 1,
            )
        } else {
            (MAX_CYLINDER, heads - 1, sectors)
        };
        let [head, ..] = head.to_le_bytes();
        let [sector, ..] = sector.to_le_bytes();
        let [low, high, ..] = cylinder.to_le_bytes();
        [head, sector | high << 6, low]
    }

    /// The sectors of one cylinder.
    const fn per_cylinder(self) -> u32 {
        self.heads as u32 * self.sectors as u32
    }
}

/// Why an MBR sector could not be encoded or parsed.
///
/// The same invariants are enforced on both ends (one
/// `validate_extents`); the encoder rejects an authoring defect and the
/// parser rejects a malformed/hostile on-disk table, so every error here
/// can arise from either path.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MbrError {
    /// The MBR sector buffer is shorter than [`MBR_SECTOR_LEN`].
    ShortSector,
    /// The `0x55AA` boot signature is absent (not an MBR sector).
    BadSignature,
    /// More than [`MAX_PRIMARY_PARTITIONS`] extents were supplied to the
    /// encoder, or more present entries were parsed than fit.
    TooManyPartitions,
    /// No (non-empty) partition was supplied to the encoder, or none was
    /// present in the parsed table.
    NoPartitions,
    /// A present partition has zero length.
    EmptyPartition,
    /// A present partition starts at sector 0 (covering the MBR itself).
    CoversMbrSector,
    /// A partition's `start_lba + sectors` overflows the 32-bit LBA space.
    LbaOverflow,
    /// Two present partitions overlap.
    Overlap,
    /// A [`Partition`] handed to [`encode`] has an LBA or block count that
    /// does not fit MBR's 32-bit fields (use [`crate::gpt`] for such a
    /// disk).
    ExtentTooLarge,
    /// A [`Partition`] handed to [`encode`] has a role no MBR type byte
    /// represents ([`PartitionType::Other`] — a many-to-one
    /// classification): encoding it would silently drop the partition
    /// from the table, so it is refused instead.
    UnrepresentableRole,
    /// An [`Entry`] handed to [`encode_entries`] has the unused type byte,
    /// which would drop it from the table.
    UnusedType,
    /// More than one [`Entry`] handed to [`encode_entries`] is active; a
    /// boot record starts exactly one.
    SeveralActive,
}

/// A primary partition as an MBR entry records it, in [`MBR_SECTOR_LEN`]-byte
/// LBA sectors.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    /// Its type byte.
    pub type_byte: u8,
    /// Whether it is the active partition, the one a boot record starts.
    pub active: bool,
    /// Its first sector.
    pub start_lba: u32,
    /// Its sectors.
    pub sectors: u32,
}

/// An entry recording no partition.
const NO_ENTRY: Entry = Entry {
    type_byte: PART_TYPE_UNUSED,
    active: false,
    start_lba: 0,
    sectors: 0,
};

/// Classify an MBR type byte into the scheme-neutral [`PartitionType`].
#[must_use]
pub fn classify(type_byte: u8) -> PartitionType {
    match type_byte {
        PART_TYPE_FAT32_LBA | PART_TYPE_FAT32_CHS => PartitionType::FatBoot,
        PART_TYPE_ARXFS_SYSTEM => PartitionType::ARXFSSystem,
        PART_TYPE_ARXFS => PartitionType::ARXFSRoot,
        _ => PartitionType::Other,
    }
}

/// The MBR type byte [`encode`] writes for a scheme-neutral role, or
/// `None` for [`PartitionType::Other`]: `classify` folds every foreign
/// type byte into `Other`, so no single byte can represent it — and the
/// unused byte would make the entry *absent*, silently dropping the
/// partition from the encoded table. An unrepresentable role is refused
/// at [`encode`] instead (fail closed, never silent data loss).
#[must_use]
pub fn type_byte_for(ty: PartitionType) -> Option<u8> {
    match ty {
        PartitionType::FatBoot => Some(PART_TYPE_FAT32_LBA),
        PartitionType::ARXFSSystem => Some(PART_TYPE_ARXFS_SYSTEM),
        PartitionType::ARXFSRoot => Some(PART_TYPE_ARXFS),
        PartitionType::Other => None,
    }
}

/// The type byte DOS gives a FAT partition of `kind` holding `sectors` from
/// `start_lba` on a disk `geometry` addresses: FAT12; FAT16 wholly within
/// the disk's first 65,536 sectors in the type early DOS reads, its sector
/// numbers being sixteen bits, and elsewhere in its own; FAT32; and FAT16
/// and FAT32 in their LBA types where CHS does not reach the partition's
/// end, as on a disk with no geometry.
#[must_use]
pub fn fat_type(kind: Kind, start_lba: u32, sectors: u32, geometry: Option<Geometry>) -> u8 {
    let last = start_lba.saturating_add(sectors.saturating_sub(1));
    let reached = geometry.is_some_and(|geometry| geometry.reaches(last));
    match kind {
        Kind::Fat12 => PART_TYPE_FAT12,
        Kind::Fat16 if !reached => PART_TYPE_FAT16_LBA,
        Kind::Fat16 if u16::try_from(last).is_ok() => PART_TYPE_FAT16_SMALL,
        Kind::Fat16 => PART_TYPE_FAT16_CHS,
        Kind::Fat32 if reached => PART_TYPE_FAT32_CHS,
        Kind::Fat32 => PART_TYPE_FAT32_LBA,
    }
}

/// Validate raw extents against the shared MBR invariants: between one and
/// [`MAX_PRIMARY_PARTITIONS`] partitions, each non-empty, none covering
/// the MBR sector, none wrapping the 32-bit LBA space, and no two
/// overlapping.
fn validate_extents(parts: &[Entry]) -> Result<(), MbrError> {
    if parts.is_empty() {
        return Err(MbrError::NoPartitions);
    }
    if parts.len() > MAX_PRIMARY_PARTITIONS {
        return Err(MbrError::TooManyPartitions);
    }
    for (i, p) in parts.iter().enumerate() {
        if p.sectors == 0 {
            return Err(MbrError::EmptyPartition);
        }
        if p.start_lba == 0 {
            return Err(MbrError::CoversMbrSector);
        }
        if p.start_lba.checked_add(p.sectors).is_none() {
            return Err(MbrError::LbaOverflow);
        }
        let p_start = u64::from(p.start_lba);
        let p_end = p_start + u64::from(p.sectors);
        for q in &parts[..i] {
            let q_start = u64::from(q.start_lba);
            let q_end = q_start + u64::from(q.sectors);
            if p_start < q_end && q_start < p_end {
                return Err(MbrError::Overlap);
            }
        }
    }
    Ok(())
}

/// Convert a scheme-neutral [`Partition`] to an MBR entry, failing closed
/// if its 64-bit LBA/length does not fit MBR's 32-bit fields.
fn entry_from_partition(part: &Partition) -> Result<Entry, MbrError> {
    let start_lba = u32::try_from(part.start_lba).map_err(|_| MbrError::ExtentTooLarge)?;
    let sectors = u32::try_from(part.block_count).map_err(|_| MbrError::ExtentTooLarge)?;
    let type_byte = type_byte_for(part.ty).ok_or(MbrError::UnrepresentableRole)?;
    Ok(Entry {
        type_byte,
        active: false,
        start_lba,
        sectors,
    })
}

/// Encode `parts` (one to [`MAX_PRIMARY_PARTITIONS`] primary partitions)
/// as a full [`MBR_SECTOR_LEN`]-byte MBR sector, for a disk with no
/// geometry. No partition is active: nothing in TAIRiX consumes the flag.
///
/// # Errors
///
/// An [`MbrError`] on any invalid, overlapping, or too-large extent.
pub fn encode(parts: &[Partition]) -> Result<[u8; MBR_SECTOR_LEN], MbrError> {
    if parts.len() > MAX_PRIMARY_PARTITIONS {
        return Err(MbrError::TooManyPartitions);
    }
    let mut entries = [NO_ENTRY; MAX_PRIMARY_PARTITIONS];
    for (slot, part) in entries.iter_mut().zip(parts.iter()) {
        *slot = entry_from_partition(part)?;
    }
    encode_entries(&entries[..parts.len()], None)
}

/// Encode `entries` (one to [`MAX_PRIMARY_PARTITIONS`] primary partitions)
/// as a full [`MBR_SECTOR_LEN`]-byte MBR sector, each extent's CHS
/// addresses those of `geometry`, or the "use LBA" form for none.
///
/// # Errors
///
/// An [`MbrError`] on any invalid, overlapping or unused-typed entry, or
/// for more than one active.
pub fn encode_entries(
    entries: &[Entry],
    geometry: Option<Geometry>,
) -> Result<[u8; MBR_SECTOR_LEN], MbrError> {
    validate_extents(entries)?;
    if entries
        .iter()
        .any(|entry| entry.type_byte == PART_TYPE_UNUSED)
    {
        return Err(MbrError::UnusedType);
    }
    if entries.iter().filter(|entry| entry.active).count() > 1 {
        return Err(MbrError::SeveralActive);
    }
    let mut sector = [0u8; MBR_SECTOR_LEN];
    for (i, entry) in entries.iter().enumerate() {
        let base = PARTITION_TABLE_OFFSET + i * PARTITION_ENTRY_LEN;
        sector[base..base + PARTITION_ENTRY_LEN].copy_from_slice(&encode_slot(entry, geometry)?);
    }
    sector[MBR_SECTOR_LEN - 2..].copy_from_slice(&MBR_SIGNATURE);
    Ok(sector)
}

/// `entry` as an MBR's or EBR's slot holds it: its active flag, its first
/// and last sectors as CHS addresses of `geometry` (the "use LBA" form for
/// none), its type byte, and its extent in LBA sectors.
///
/// # Errors
///
/// [`MbrError::UnusedType`] for the unused type byte, which would read as
/// no entry, [`MbrError::EmptyPartition`] for an empty extent and
/// [`MbrError::LbaOverflow`] for one that wraps.
pub fn encode_slot(
    entry: &Entry,
    geometry: Option<Geometry>,
) -> Result<[u8; PARTITION_ENTRY_LEN], MbrError> {
    if entry.type_byte == PART_TYPE_UNUSED {
        return Err(MbrError::UnusedType);
    }
    let last = entry
        .sectors
        .checked_sub(1)
        .ok_or(MbrError::EmptyPartition)?;
    let last = entry
        .start_lba
        .checked_add(last)
        .ok_or(MbrError::LbaOverflow)?;
    let chs = |lba| geometry.map_or(USE_LBA, |geometry| geometry.chs(lba));
    let mut slot = [0u8; PARTITION_ENTRY_LEN];
    slot[0] = if entry.active { ACTIVE } else { 0 };
    slot[1..4].copy_from_slice(&chs(entry.start_lba));
    slot[4] = entry.type_byte;
    slot[5..8].copy_from_slice(&chs(last));
    slot[8..12].copy_from_slice(&entry.start_lba.to_le_bytes());
    slot[12..16].copy_from_slice(&entry.sectors.to_le_bytes());
    Ok(slot)
}

/// The extended boot record [`ebr`] reads back: `logical`, the slot of its
/// logical drive, first, and `next`, the slot linking the next record,
/// second, either left empty for none, and no code before them, as DOS
/// writes them.
#[must_use]
pub fn encode_ebr(
    logical: Option<&[u8; PARTITION_ENTRY_LEN]>,
    next: Option<&[u8; PARTITION_ENTRY_LEN]>,
) -> [u8; MBR_SECTOR_LEN] {
    let mut sector = [0u8; MBR_SECTOR_LEN];
    for (i, slot) in [logical, next].into_iter().enumerate() {
        if let Some(slot) = slot {
            let base = PARTITION_TABLE_OFFSET + i * PARTITION_ENTRY_LEN;
            sector[base..base + PARTITION_ENTRY_LEN].copy_from_slice(slot);
        }
    }
    sector[MBR_SECTOR_LEN - 2..].copy_from_slice(&MBR_SIGNATURE);
    sector
}

/// Extend the data root's entry in the MBR `sector` to end at `disk_blocks`,
/// when it is the partition nearest the disk's end and the disk reaches past
/// it, leaving every other byte as it was.
///
/// Answers the grown extent, or `None` when there is nothing to grow: no
/// root, a partition after it, or no room past it. An MBR addresses at most
/// `u32::MAX` sectors, so on a larger disk the root ends there.
///
/// # Errors
///
/// An [`MbrError`] if `sector` is not a valid MBR.
pub fn grow_root(
    sector: &mut [u8; MBR_SECTOR_LEN],
    disk_blocks: u64,
) -> Result<Option<Partition>, MbrError> {
    let table = parse(sector)?;
    let Some(root) = table.first_of_type(PartitionType::ARXFSRoot) else {
        return Ok(None);
    };
    if table
        .partitions()
        .iter()
        .any(|other| other.start_lba > root.start_lba)
    {
        return Ok(None);
    }
    let grown = disk_blocks
        .min(u64::from(u32::MAX))
        .saturating_sub(root.start_lba);
    if grown <= root.block_count {
        return Ok(None);
    }
    let slot = (0..MAX_PRIMARY_PARTITIONS)
        .find(|&i| sector[PARTITION_TABLE_OFFSET + i * PARTITION_ENTRY_LEN + 4] == PART_TYPE_ARXFS)
        .ok_or(MbrError::NoPartitions)?;
    let entry =
        &mut sector[PARTITION_TABLE_OFFSET + slot * PARTITION_ENTRY_LEN..][..PARTITION_ENTRY_LEN];
    // The new end lies past any CHS address, so it takes the "use LBA" form.
    entry[5..8].copy_from_slice(&USE_LBA);
    let sectors = u32::try_from(grown).map_err(|_| MbrError::ExtentTooLarge)?;
    entry[12..16].copy_from_slice(&sectors.to_le_bytes());
    Ok(Some(Partition {
        block_count: grown,
        ..root
    }))
}

/// `true` if `sector` is at least [`MBR_SECTOR_LEN`] bytes and carries the
/// `0x55AA` boot signature.
#[must_use]
pub fn has_signature(sector: &[u8]) -> bool {
    sector.len() >= MBR_SECTOR_LEN
        && sector[MBR_SECTOR_LEN - 2] == MBR_SIGNATURE[0]
        && sector[MBR_SECTOR_LEN - 1] == MBR_SIGNATURE[1]
}

/// Parse an MBR partition table out of `sector`, fail-closed.
///
/// `sector` is read off an untrusted device, so it is fully validated
/// before any extent is trusted: the buffer
/// must be at least [`MBR_SECTOR_LEN`] bytes and carry the `0x55AA`
/// signature, every entry whose type byte is not [`PART_TYPE_UNUSED`] is
/// collected, and the present extents must satisfy `validate_extents`.
/// A table that violates any invariant is rejected **whole**.
///
/// # Errors
///
/// An [`MbrError`]: [`MbrError::ShortSector`] for a short buffer,
/// [`MbrError::BadSignature`] for a missing signature, or the first
/// extent invariant `validate_extents` rejects.
pub fn parse(sector: &[u8]) -> Result<PartitionTable, MbrError> {
    let entries = entries(sector)?;
    let mut table = PartitionTable::empty();
    for p in entries.as_slice() {
        // Cannot exceed MAX_PRIMARY_PARTITIONS <= MAX_PARTITIONS.
        table
            .push(Partition {
                ty: classify(p.type_byte),
                start_lba: u64::from(p.start_lba),
                block_count: u64::from(p.sectors),
            })
            .map_err(|_| MbrError::TooManyPartitions)?;
    }
    Ok(table)
}

/// The present entries of an MBR, in entry order, validated as [`parse`]
/// validates them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Entries {
    entries: [Entry; MAX_PRIMARY_PARTITIONS],
    len: usize,
}

impl Entries {
    /// The entries.
    #[must_use]
    pub fn as_slice(&self) -> &[Entry] {
        &self.entries[..self.len]
    }
}

/// The entry in slot `slot` of `sector`, or `None` where it is unused.
fn entry_at(sector: &[u8], slot: usize) -> Option<Entry> {
    let base = PARTITION_TABLE_OFFSET + slot * PARTITION_ENTRY_LEN;
    let raw = sector.get(base..base + PARTITION_ENTRY_LEN)?;
    let word = |at: usize| u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]);
    (raw[4] != PART_TYPE_UNUSED).then(|| Entry {
        type_byte: raw[4],
        active: raw[0] == ACTIVE,
        start_lba: word(8),
        sectors: word(12),
    })
}

/// The entries of the MBR `sector` with their type bytes and active flags,
/// fail-closed as [`parse`] is.
///
/// # Errors
///
/// An [`MbrError`] as [`parse`] gives one.
pub fn entries(sector: &[u8]) -> Result<Entries, MbrError> {
    if sector.len() < MBR_SECTOR_LEN {
        return Err(MbrError::ShortSector);
    }
    if !has_signature(sector) {
        return Err(MbrError::BadSignature);
    }
    let mut out = Entries {
        entries: [NO_ENTRY; MAX_PRIMARY_PARTITIONS],
        len: 0,
    };
    for entry in (0..MAX_PRIMARY_PARTITIONS).filter_map(|slot| entry_at(sector, slot)) {
        out.entries[out.len] = entry;
        out.len += 1;
    }
    validate_extents(out.as_slice())?;
    Ok(out)
}

/// MBR partition-type byte for an extended partition addressed via CHS,
/// whose logical drives a chain of EBRs records.
pub const PART_TYPE_EXTENDED_CHS: u8 = 0x05;

/// MBR partition-type byte for an extended partition addressed via LBA.
pub const PART_TYPE_EXTENDED_LBA: u8 = 0x0f;

/// Whether `type_byte` names an extended partition.
#[must_use]
pub const fn is_extended(type_byte: u8) -> bool {
    matches!(type_byte, PART_TYPE_EXTENDED_CHS | PART_TYPE_EXTENDED_LBA)
}

/// An extended boot record: the logical drive it describes, its start
/// relative to the record itself, and where the next record lies, relative
/// to the extended partition's start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ebr {
    /// The logical drive, if the record has one.
    pub logical: Option<Entry>,
    /// The next record's offset in the extended partition, if one follows.
    pub next: Option<u32>,
}

/// The extended boot record `sector`: its first entry the logical drive,
/// its second the link to the next record. Each entry's offsets have their
/// own base, so they are not checked against each other; an empty logical
/// drive or a link at offset 0, which would loop, is refused.
///
/// # Errors
///
/// [`MbrError::ShortSector`] or [`MbrError::BadSignature`] for a sector no
/// record is, [`MbrError::EmptyPartition`] for an empty logical drive, and
/// [`MbrError::CoversMbrSector`] for a link back to the chain's first record.
pub fn ebr(sector: &[u8]) -> Result<Ebr, MbrError> {
    if sector.len() < MBR_SECTOR_LEN {
        return Err(MbrError::ShortSector);
    }
    if !has_signature(sector) {
        return Err(MbrError::BadSignature);
    }
    let logical = entry_at(sector, 0).filter(|entry| !is_extended(entry.type_byte));
    if logical.is_some_and(|entry| entry.sectors == 0 || entry.start_lba == 0) {
        return Err(MbrError::EmptyPartition);
    }
    let next = entry_at(sector, 1)
        .filter(|entry| is_extended(entry.type_byte))
        .map(|entry| entry.start_lba);
    if next == Some(0) {
        return Err(MbrError::CoversMbrSector);
    }
    Ok(Ebr { logical, next })
}
