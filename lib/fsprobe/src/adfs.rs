//! Acorn ADFS / RISC OS `FileCore` recognition, shared by the probe and the
//! ADFS driver so the two agree on what an ADFS volume is and which identity
//! it carries.
//!
//! A new-map volume carries a 60-byte *disc record* describing its geometry
//! and allocation map: at byte 4 of the first map zone on E-class floppies,
//! and inside the checksummed boot block at byte `0xC00` on F-class floppies
//! and hard discs. An old-map volume has none: its free-space map fills
//! sectors 0–1, each ending in a checksum, with the root directory after it.
//! References: J.G. Harston, "Acorn 8-Bit ADFS Filesystem Structure"
//! (mdfs.net), and the RISC OS PRM `FileCore` formats chapter.

use tairix_abi::driver::block::Block;
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;

use crate::{content_identity, read_present};

/// Size of the on-disc disc record in bytes (`u32` form first so the byte
/// form derives from it losslessly).
pub const DISC_RECORD_SIZE_U32: u32 = 60;

/// Size of the on-disc disc record in bytes.
pub const DISC_RECORD_SIZE: usize = DISC_RECORD_SIZE_U32 as usize;

/// Disc byte address of the boot block on F-class and hard-disc volumes.
pub const BOOT_BLOCK_OFFSET: u64 = 0xC00;

/// Offset of the disc record inside the boot block.
pub const DISC_RECORD_IN_BOOT_BLOCK: usize = 0x1C0;

/// Size of the checksummed boot block.
pub const BOOT_BLOCK_SIZE: usize = 512;

/// Disc byte address of an E-class volume's bare disc record: byte 4 of map
/// zone 0, after the zone header.
pub const BARE_RECORD_OFFSET: u64 = 4;

/// The reserved "defect" fragment id.
pub const FRAG_BAD: u32 = 1;
/// The fragment id carrying the map itself and the root directory.
pub const FRAG_ROOT: u32 = 2;

/// Old-map logical sector size in bytes, fixed by the format.
pub const OLD_SECTOR_SIZE_U32: u32 = 256;

/// Old-map logical sector size in bytes, fixed by the format.
pub const OLD_SECTOR_SIZE: u64 = OLD_SECTOR_SIZE_U32 as u64;

/// Size of the raw two-sector old map image.
pub const OLD_MAP_SIZE: usize = 512;

/// Maximum number of old-map free-area entries (82 three-byte entries fill
/// bytes `0x00..0xF6`).
pub const MAX_FREE_AREAS: usize = 82;

/// Byte offset of the free-space end pointer within the old map image.
pub const END_POINTER: usize = 0x1FE;

/// Byte offset of the total-sector count within the old map image.
pub const DISC_SECTORS: usize = 0x0FC;

/// Byte offset of the disc identifier within the old map image.
pub const OLD_DISC_ID: usize = 0x1FB;

/// Bits 29–31 of an old-map start or length entry must be zero; the top
/// three bits of the 8-bit API's sector addresses selected the drive.
pub const ENTRY_LIMIT: u32 = 1 << 29;

/// The two markers a fixed directory may carry after its sequence byte.
pub const DIR_MARKERS: [&[u8; 4]; 2] = [b"Hugo", b"Nick"];

/// Where the old map's ten-byte disc name lives: characters alternate between
/// sector 0 (`0x0F7..0x0FC`) and sector 1 (`0x1F6..0x1FB`), first character
/// in sector 0.
const OLD_NAME_BYTES: [usize; 10] = [
    0x0F7, 0x1F6, 0x0F8, 0x1F7, 0x0F9, 0x1F8, 0x0FA, 0x1F9, 0x0FB, 0x1FA,
];

/// The longest disc name either map flavour records.
pub const NAME_LEN: usize = 10;

/// A decoded new-map disc record.
///
/// Field names follow the RISC OS PRM. Multi-byte fields are little-endian
/// on disc; `nzones` combines the low byte at offset 9 with the high byte at
/// offset `0x2A`, and `disc_size` combines the 32-bit low word with the
/// 32-bit high word at `0x24`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DiscRecord {
    /// Log2 of the sector size in bytes (8, 9, or 10).
    pub log2secsize: u8,
    /// Sectors per track (physical geometry; informational here).
    pub secspertrack: u8,
    /// Head count (physical geometry; informational here).
    pub heads: u8,
    /// Recording density code (physical geometry; informational here).
    pub density: u8,
    /// Fragment-id length in map bits.
    pub idlen: u8,
    /// Log2 of the bytes described by one map bit.
    pub log2bpmb: u8,
    /// Track-to-track sector skew (physical geometry; informational here).
    pub skew: u8,
    /// `*OPT 4` boot option.
    pub bootoption: u8,
    /// Lowest numbered sector id on a track (physical geometry).
    pub lowsector: u8,
    /// Number of map zones (low and high bytes combined).
    pub nzones: u16,
    /// Non-map bits at the end of each zone sector.
    pub zone_spare: u16,
    /// Indirect disc address of the root directory.
    pub root: u32,
    /// Disc size in bytes.
    pub disc_size: u64,
    /// Disc identifier, cycled by `FileCore` on mutation: never an identity.
    pub disc_id: u16,
    /// Disc name, space padded.
    pub disc_name: [u8; NAME_LEN],
    /// Filetype presented for the disc as a whole (informational).
    pub disc_type: u32,
    /// Log2 of the share granularity, in sectors, for shared fragments.
    pub log2sharesize: u8,
    /// The "big disc" flag (RISC OS 3.6+ addressing).
    pub big_flag: bool,
    /// Directory format: `0` = fixed 2048-byte new directories, `1` =
    /// variable-length big directories (E+/F+).
    pub format_version: u32,
    /// Root directory size in bytes (big directories only).
    pub root_size: u32,
}

impl DiscRecord {
    /// Decode a disc record from its 60-byte on-disc form.
    #[must_use]
    pub fn parse(raw: &[u8; DISC_RECORD_SIZE]) -> Self {
        let mut disc_name = [0u8; NAME_LEN];
        disc_name.copy_from_slice(&raw[0x16..0x20]);
        Self {
            log2secsize: raw[0],
            secspertrack: raw[1],
            heads: raw[2],
            density: raw[3],
            idlen: raw[4],
            log2bpmb: raw[5],
            skew: raw[6],
            bootoption: raw[7],
            lowsector: raw[8],
            nzones: u16::from(raw[9]) | u16::from(raw[0x2A]) << 8,
            zone_spare: u16::from_le_bytes([raw[0x0A], raw[0x0B]]),
            root: u32_at(raw, 0x0C),
            disc_size: u64::from(u32_at(raw, 0x10)) | u64::from(u32_at(raw, 0x24)) << 32,
            disc_id: u16::from_le_bytes([raw[0x14], raw[0x15]]),
            disc_name,
            disc_type: u32_at(raw, 0x20),
            log2sharesize: raw[0x28] & 0x0F,
            big_flag: raw[0x29] & 1 != 0,
            format_version: u32_at(raw, 0x2C),
            root_size: u32_at(raw, 0x30),
        }
    }

    /// Encode the record into its 60-byte on-disc form.
    #[must_use]
    pub fn encode(&self) -> [u8; DISC_RECORD_SIZE] {
        let mut raw = [0u8; DISC_RECORD_SIZE];
        raw[0] = self.log2secsize;
        raw[1] = self.secspertrack;
        raw[2] = self.heads;
        raw[3] = self.density;
        raw[4] = self.idlen;
        raw[5] = self.log2bpmb;
        raw[6] = self.skew;
        raw[7] = self.bootoption;
        raw[8] = self.lowsector;
        let [nzones_low, nzones_high] = self.nzones.to_le_bytes();
        raw[9] = nzones_low;
        raw[0x0A..0x0C].copy_from_slice(&self.zone_spare.to_le_bytes());
        raw[0x0C..0x10].copy_from_slice(&self.root.to_le_bytes());
        let size = self.disc_size.to_le_bytes();
        raw[0x10..0x14].copy_from_slice(&size[..4]);
        raw[0x14..0x16].copy_from_slice(&self.disc_id.to_le_bytes());
        raw[0x16..0x20].copy_from_slice(&self.disc_name);
        raw[0x20..0x24].copy_from_slice(&self.disc_type.to_le_bytes());
        raw[0x24..0x28].copy_from_slice(&size[4..]);
        raw[0x28] = self.log2sharesize & 0x0F;
        raw[0x29] = u8::from(self.big_flag);
        raw[0x2A] = nzones_high;
        raw[0x2C..0x30].copy_from_slice(&self.format_version.to_le_bytes());
        raw[0x30..0x34].copy_from_slice(&self.root_size.to_le_bytes());
        raw
    }

    /// Structurally validate the record, failing closed on anything a genuine
    /// `FileCore` volume cannot carry.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] on any structural violation.
    pub fn validate(&self) -> Result<(), DriverError> {
        // Sector size must be 256, 512, or 1024 bytes.
        if !(8..=10).contains(&self.log2secsize) {
            return Err(DriverError::BadMagic);
        }
        // One map bit covers at least 64 bytes (the F format's value) and at
        // most 64 KiB.
        if !(6..=16).contains(&self.log2bpmb) {
            return Err(DriverError::BadMagic);
        }
        // A fragment id spans at least a sector's worth of map bits plus the
        // three reserved ids, and is bounded by the 3-byte indirect disc
        // address (16 bits) — 19 bits with big directories.
        let max_idlen = if self.format_version != 0 { 19 } else { 16 };
        if self.idlen < self.log2secsize + 3 || self.idlen > max_idlen {
            return Err(DriverError::BadMagic);
        }
        if self.nzones == 0 {
            return Err(DriverError::BadMagic);
        }
        // A spare consuming the whole zone sector leaves no map.
        let zone_bits = 8u32 << self.log2secsize;
        if u32::from(self.zone_spare) + 32 + u32::from(self.idlen) >= zone_bits {
            return Err(DriverError::BadMagic);
        }
        if self.format_version > 1 {
            return Err(DriverError::BadMagic);
        }
        if self.disc_size == 0 || !self.disc_size.is_multiple_of(1u64 << self.log2secsize) {
            return Err(DriverError::BadMagic);
        }
        if self.disc_size >> self.log2secsize > u64::from(u32::MAX) {
            return Err(DriverError::BadMagic);
        }
        // The root must name a real fragment, never free space or the defect
        // list.
        if self.root >> 8 < FRAG_ROOT {
            return Err(DriverError::BadMagic);
        }
        Ok(())
    }

    /// Bytes described by one map bit.
    #[must_use]
    pub fn bytes_per_map_bit(&self) -> u64 {
        1u64 << self.log2bpmb
    }

    /// Sector size in bytes.
    #[must_use]
    pub fn sector_size(&self) -> u64 {
        1u64 << self.log2secsize
    }

    /// Map bits carried by each zone sector.
    #[must_use]
    pub fn zone_bits(&self) -> u32 {
        (8u32 << self.log2secsize) - u32::from(self.zone_spare)
    }

    /// Fragment ids homed in each zone.
    #[must_use]
    pub fn ids_per_zone(&self) -> u32 {
        self.zone_bits() / (u32::from(self.idlen) + 1)
    }

    /// Disc byte address of the first map zone: the map sits in the middle
    /// zone of the disc, which on a single-zone volume is sector 0.
    #[must_use]
    pub fn map_offset(&self) -> u64 {
        let half_zones = u64::from(self.nzones >> 1);
        let mut map_bits = half_zones * u64::from(self.zone_bits());
        if self.nzones > 1 {
            map_bits -= (DISC_RECORD_SIZE as u64) * 8;
        }
        map_bits * self.bytes_per_map_bit()
    }

    /// Size of the whole map in bytes (`nzones` sectors).
    #[must_use]
    pub fn map_size(&self) -> u64 {
        u64::from(self.nzones) * self.sector_size()
    }

    /// Whether the record's reserved tail is clear, as every genuine record's
    /// is: an all-zero block checksums, but no real record is all zero.
    fn tail_clear(raw: &[u8; DISC_RECORD_SIZE]) -> bool {
        raw[0x34..0x3C].iter().all(|&b| b == 0)
    }
}

/// The boot-block checksum: an end-around-carry sum of bytes `0..=510`,
/// stored in byte 511, the carry folded in before each addition and the
/// final carry discarded, as `FileCore`'s `ADC` loop computes it.
#[must_use]
pub fn boot_block_checksum(block: &[u8; BOOT_BLOCK_SIZE]) -> u8 {
    let mut sum: u32 = 0;
    for &byte in block[..BOOT_BLOCK_SIZE - 1].iter().rev() {
        sum = (sum & 0xFF) + (sum >> 8);
        sum += u32::from(byte);
    }
    low_byte(sum)
}

/// The check byte of one new-map zone sector (`ZoneCheck`): `FileCore`'s
/// 4-lane carry-propagating byte sum, in which byte 0 — the check byte
/// itself — takes no part.
///
/// `map` is one whole zone sector, whose length is a multiple of four; any
/// other length is no zone and checks as `0`.
#[must_use]
pub fn zone_check(map: &[u8]) -> u8 {
    if map.len() < 8 || !map.len().is_multiple_of(4) {
        return 0;
    }
    let (mut v0, mut v1, mut v2, mut v3) = (0u32, 0u32, 0u32, 0u32);
    let mut i = map.len() - 4;
    while i > 0 {
        v0 += u32::from(map[i]) + (v3 >> 8);
        v3 &= 0xFF;
        v1 += u32::from(map[i + 1]) + (v0 >> 8);
        v0 &= 0xFF;
        v2 += u32::from(map[i + 2]) + (v1 >> 8);
        v1 &= 0xFF;
        v3 += u32::from(map[i + 3]) + (v2 >> 8);
        v2 &= 0xFF;
        i -= 4;
    }
    v0 += v3 >> 8;
    v1 += u32::from(map[1]) + (v0 >> 8);
    v2 += u32::from(map[2]) + (v1 >> 8);
    v3 += u32::from(map[3]) + (v2 >> 8);
    low_byte(v0 ^ v1 ^ v2 ^ v3)
}

/// The checksum closing one old-map sector: start at 255 and add bytes 254
/// down to 0, folding each carry into the next addition.
#[must_use]
pub fn old_map_checksum(sector: &[u8]) -> u8 {
    let mut sum: u32 = 255;
    for &byte in sector[..255].iter().rev() {
        if sum > 255 {
            sum = (sum + 1) & 0xFF;
        }
        sum += u32::from(byte);
    }
    low_byte(sum)
}

/// The 3-byte little-endian old-map value at `at`.
#[must_use]
pub fn old_map_u24(map: &[u8; OLD_MAP_SIZE], at: usize) -> u32 {
    u32::from_le_bytes([map[at], map[at + 1], map[at + 2], 0])
}

/// Validate a raw old map image: both sector checksums, the recorded disc
/// size against the device's `device_bytes`, the end pointer, and every free
/// area entry.
///
/// # Errors
///
/// [`DriverError::BadMagic`] on any structural violation.
pub fn validate_old_map(map: &[u8; OLD_MAP_SIZE], device_bytes: u64) -> Result<(), DriverError> {
    let (sector0, sector1) = map.split_at(256);
    if old_map_checksum(sector0) != sector0[255] || old_map_checksum(sector1) != sector1[255] {
        return Err(DriverError::BadMagic);
    }
    let disc_sectors = old_map_u24(map, DISC_SECTORS);
    if disc_sectors == 0
        || u64::from(disc_sectors) * OLD_SECTOR_SIZE > device_bytes
        || disc_sectors >= ENTRY_LIMIT
    {
        return Err(DriverError::BadMagic);
    }
    let end = usize::from(map[END_POINTER]);
    if end % 3 != 0 || end > MAX_FREE_AREAS * 3 {
        return Err(DriverError::BadMagic);
    }
    for index in 0..end / 3 {
        let start = old_map_u24(map, index * 3);
        let len = old_map_u24(map, 0x100 + index * 3);
        if start >= ENTRY_LIMIT || len >= ENTRY_LIMIT {
            return Err(DriverError::BadMagic);
        }
        if len == 0 || start.checked_add(len).is_none_or(|e| e > disc_sectors) {
            return Err(DriverError::BadMagic);
        }
    }
    Ok(())
}

/// How an ADFS volume is laid out, as recognition found it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Layout {
    /// A new-map volume, described by its disc record.
    NewMap {
        /// The validated disc record.
        record: DiscRecord,
        /// Whether the record came from a boot block, whose copy a writer
        /// keeps in step.
        boot_block: bool,
    },
    /// An old-map volume.
    OldMap {
        /// Whether its directories are the 2048-byte form (the D format)
        /// rather than the 1280-byte one.
        large_directories: bool,
        /// The root directory's first 256-byte sector.
        root_sector: u32,
        /// The disc's size in 256-byte sectors, as its map records it.
        disc_sectors: u32,
        /// The disc's name, as its map records it.
        name: [u8; NAME_LEN],
    },
}

impl Layout {
    /// The volume's stable identity.
    ///
    /// ADFS records no unique identifier — the disc id is cycled on mutation
    /// — so the identity is a digest of the facts no ordinary write changes:
    /// the geometry, the root's address, the size and the name. Two discs
    /// formatted alike and named alike share it, as two FAT volumes sharing a
    /// serial and label do.
    #[must_use]
    pub fn identity(&self) -> [u8; 16] {
        match self {
            Self::NewMap { record, .. } => {
                let mut stable = record.encode();
                // The disc id, boot option and a big root's size change under
                // ordinary use; the rest is fixed when the disc is formatted.
                stable[0x14..0x16].fill(0);
                stable[7] = 0;
                stable[0x30..0x34].fill(0);
                content_identity(b"tairix.adfs.newmap", &[&stable])
            }
            Self::OldMap {
                large_directories,
                disc_sectors,
                name,
                ..
            } => {
                let (name, len) = trimmed(*name);
                content_identity(
                    b"tairix.adfs.oldmap",
                    &[
                        &[u8::from(*large_directories)],
                        &disc_sectors.to_le_bytes(),
                        &name[..len],
                    ],
                )
            }
        }
    }

    /// The volume's disc name, trimmed of padding: up to [`NAME_LEN`] bytes.
    #[must_use]
    pub fn name(&self) -> ([u8; NAME_LEN], usize) {
        match self {
            Self::NewMap { record, .. } => trimmed(record.disc_name),
            Self::OldMap { name, .. } => trimmed(*name),
        }
    }
}

/// Recognise an ADFS volume on `dev`, in `FileCore`'s order: the checksummed
/// boot block (F-class and hard discs), then a bare disc record at byte 4
/// whose zone check holds (E-class), then a checksummed old map whose root
/// directory carries its marker (S/M/L/D). `None` when none of them holds —
/// never a guess.
///
/// # Errors
///
/// Only a device fault: a structure that lies beyond the device is simply
/// absent.
pub fn identify<B: Block>(dev: &mut ByteDevice<B>) -> Result<Option<Layout>, DriverError> {
    if let Some(record) = boot_block_record(dev)? {
        return Ok(Some(Layout::NewMap {
            record,
            boot_block: true,
        }));
    }
    if let Some(record) = bare_record(dev)? {
        return Ok(Some(Layout::NewMap {
            record,
            boot_block: false,
        }));
    }
    old_map(dev)
}

/// The disc record a valid boot block carries.
fn boot_block_record<B: Block>(dev: &mut ByteDevice<B>) -> Result<Option<DiscRecord>, DriverError> {
    let mut block = [0u8; BOOT_BLOCK_SIZE];
    if !read_present(dev, BOOT_BLOCK_OFFSET, &mut block)? {
        return Ok(None);
    }
    if boot_block_checksum(&block) != block[BOOT_BLOCK_SIZE - 1] {
        return Ok(None);
    }
    let mut raw = [0u8; DISC_RECORD_SIZE];
    raw.copy_from_slice(
        &block[DISC_RECORD_IN_BOOT_BLOCK..DISC_RECORD_IN_BOOT_BLOCK + DISC_RECORD_SIZE],
    );
    let record = DiscRecord::parse(&raw);
    if record.validate().is_err() || !DiscRecord::tail_clear(&raw) {
        return Ok(None);
    }
    Ok(Some(record))
}

/// The bare disc record of a single-zone (E-class) volume, accepted only when
/// zone 0's check byte holds over the sector that carries it.
fn bare_record<B: Block>(dev: &mut ByteDevice<B>) -> Result<Option<DiscRecord>, DriverError> {
    let mut raw = [0u8; DISC_RECORD_SIZE];
    if !read_present(dev, BARE_RECORD_OFFSET, &mut raw)? {
        return Ok(None);
    }
    let record = DiscRecord::parse(&raw);
    if record.validate().is_err() || record.nzones != 1 || !DiscRecord::tail_clear(&raw) {
        return Ok(None);
    }
    let mut zone = [0u8; 1024];
    let zone = &mut zone[..usize::try_from(record.sector_size()).unwrap_or(1024)];
    if !read_present(dev, 0, zone)? || zone_check(zone) != zone[0] {
        return Ok(None);
    }
    Ok(Some(record))
}

/// A checksummed old map whose root directory carries a marker.
fn old_map<B: Block>(dev: &mut ByteDevice<B>) -> Result<Option<Layout>, DriverError> {
    let mut map = [0u8; OLD_MAP_SIZE];
    if !read_present(dev, 0, &mut map)? || validate_old_map(&map, dev.capacity()).is_err() {
        return Ok(None);
    }
    // Five zero bytes at sector 2 mark a large-sector (D) volume, whose root
    // sits at sector 4; otherwise the 1280-byte root is at sector 2.
    let mut lead = [0u8; 5];
    if !read_present(dev, 2 * OLD_SECTOR_SIZE, &mut lead)? {
        return Ok(None);
    }
    let large_directories = lead == [0u8; 5];
    let root_sector = if large_directories { 4 } else { 2 };
    let mut header = [0u8; 5];
    if !read_present(dev, u64::from(root_sector) * OLD_SECTOR_SIZE, &mut header)?
        || !DIR_MARKERS.iter().any(|marker| header[1..5] == **marker)
    {
        return Ok(None);
    }
    let mut name = [0u8; NAME_LEN];
    for (slot, &at) in name.iter_mut().zip(OLD_NAME_BYTES.iter()) {
        *slot = map[at];
    }
    Ok(Some(Layout::OldMap {
        large_directories,
        root_sector,
        disc_sectors: old_map_u24(&map, DISC_SECTORS),
        name,
    }))
}

/// A name field with its trailing spaces, NULs and control padding removed.
/// `FileCore` pads names with spaces, and an old map that was never named
/// holds zeros, or the low bytes of the Level 3 partition fields it overlaps.
fn trimmed(name: [u8; NAME_LEN]) -> ([u8; NAME_LEN], usize) {
    let printable = name
        .iter()
        .position(|&b| b < 0x20 || b == 0x7F)
        .unwrap_or(NAME_LEN);
    let len = name[..printable]
        .iter()
        .rposition(|&b| b != b' ')
        .map_or(0, |last| last + 1);
    (name, len)
}

fn u32_at(raw: &[u8; DISC_RECORD_SIZE], at: usize) -> u32 {
    u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]])
}

fn low_byte(value: u32) -> u8 {
    value.to_le_bytes()[0]
}
