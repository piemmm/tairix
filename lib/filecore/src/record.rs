//! The disc record and the geometry it gives, the boot block, the map's
//! checksums, and the old map's checks.
//!
//! A new-map disc carries a 60-byte disc record describing its geometry and
//! allocation map: at byte 4 of the first map zone on E-class floppies, and
//! inside the checksummed boot block at byte `0xC00` on F-class floppies and
//! hard discs. An old-map disc has none: its free-space map fills sectors
//! 0–1, each ending in a checksum, with the root directory after it.

use crate::bytes::{get_u24, get_u32};
use crate::{Fault, Structure};

/// The sizes a disc's sectors may have, as powers of two: 256 to 1024 bytes.
pub const LOG2SECSIZE: core::ops::RangeInclusive<u8> = 8..=10;

/// The map-bit sizes a record may give: from 64 bytes, the F format's, to
/// 64 KiB.
pub const LOG2BPMB: core::ops::RangeInclusive<u8> = 6..=16;

/// What a disc record refused on a structural check is.
const BROKEN_RECORD: Fault = Fault::Broken(Structure::Record);

/// What an old map refused on a structural check is.
const BROKEN_MAP: Fault = Fault::Broken(Structure::Map);

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

/// Size of the header opening every map zone: its check byte, the free-space
/// link and the cross-check byte (`u32` form first, as for the record).
pub const ZONE_HEADER_SIZE_U32: u32 = 4;

/// Size of the header opening every map zone.
pub const ZONE_HEADER_SIZE: usize = ZONE_HEADER_SIZE_U32 as usize;

/// The zone header byte holding the zone's [`zone_check`].
pub const ZONE_CHECK_BYTE: usize = 0;

/// The bit the zone header's free-space link starts at.
pub const FREELINK_BIT: u32 = 8;

/// The most bits a free-space link spans; the header link's stop bit follows
/// its field.
pub const LINK_BITS_MAX: u32 = 15;

/// The zone header byte whose values across every zone XOR to
/// [`CROSS_CHECK_SUM`].
pub const CROSS_CHECK_BYTE: usize = 3;

/// What every zone's cross-check bytes XOR to.
pub const CROSS_CHECK_SUM: u8 = 0xFF;

/// Map bits the disc record takes in zone 0, after the zone header.
pub const DISC_RECORD_BITS: u32 = DISC_RECORD_SIZE_U32 * 8;

/// Disc byte address of an E-class volume's bare disc record: in map zone 0,
/// after the zone header.
pub const BARE_RECORD_OFFSET: u64 = ZONE_HEADER_SIZE_U32 as u64;

/// Width of a free-space link for fragment ids `idlen` bits long: an id's
/// width, at most [`LINK_BITS_MAX`].
#[must_use]
pub fn link_bits(idlen: u8) -> u32 {
    u32::from(idlen).min(LINK_BITS_MAX)
}

/// The bit of zone `zone`'s sector its fragment stream starts at: after the
/// zone header, and in zone 0 after the disc record too.
#[must_use]
pub const fn zone_start_bit(zone: u32) -> u32 {
    let after_header = ZONE_HEADER_SIZE_U32 * 8;
    if zone == 0 {
        after_header + DISC_RECORD_BITS
    } else {
        after_header
    }
}

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

/// The largest old-map floppy, the D format's 800 KiB; a larger old-map volume
/// is a hard disc's.
pub const OLD_MAP_FLOPPY_MAX: u64 = 800 * 1024;

/// Maximum number of old-map free-area entries (82 three-byte entries fill
/// bytes `0x00..0xF6`).
pub const MAX_FREE_AREAS: usize = 82;

/// Byte offset of the free-space end pointer within the old map image.
pub const END_POINTER: usize = 0x1FE;

/// Byte offset of the total-sector count within the old map image.
pub const DISC_SECTORS: usize = 0x0FC;

/// Byte offset of the disc identifier within the old map image.
pub const OLD_DISC_ID: usize = 0x1FB;

/// Byte offset of the `*Opt 4` boot option within the old map image.
pub const OLD_BOOT_OPTION: usize = 0x1FD;

/// Bits 29–31 of an old-map start or length entry must be zero; the top
/// three bits of the 8-bit API's sector addresses selected the drive.
pub const ENTRY_LIMIT: u32 = 1 << 29;

/// The two markers a fixed directory may carry after its sequence byte.
pub const DIR_MARKERS: [&[u8; 4]; 2] = [b"Hugo", b"Nick"];

/// Where the old map's ten-byte disc name lives: characters alternate between
/// sector 0 (`0x0F7..0x0FC`) and sector 1 (`0x1F6..0x1FB`), first character
/// in sector 0.
pub const OLD_NAME_BYTES: [usize; 10] = [
    0x0F7, 0x1F6, 0x0F8, 0x1F7, 0x0F9, 0x1F8, 0x0FA, 0x1F9, 0x0FB, 0x1FA,
];

/// The longest disc name either map flavour records.
pub const NAME_LEN: usize = 10;

/// The most UTF-8 bytes a disc name spells: up to three for each of its RISC
/// OS Latin-1 characters.
pub const NAME_UTF8_MAX: usize = 3 * NAME_LEN;

/// One of a new map's two copies.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MapCopy {
    /// The copy `FileCore` reads while every one of its zones checks.
    First,
    /// The copy after it, which `FileCore` falls back to.
    Second,
}

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
            root: get_u32(raw, 0x0C),
            disc_size: u64::from(get_u32(raw, 0x10)) | u64::from(get_u32(raw, 0x24)) << 32,
            disc_id: u16::from_le_bytes([raw[0x14], raw[0x15]]),
            disc_name,
            disc_type: get_u32(raw, 0x20),
            log2sharesize: raw[0x28] & 0x0F,
            big_flag: raw[0x29] & 1 != 0,
            format_version: get_u32(raw, 0x2C),
            root_size: get_u32(raw, 0x30),
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
    /// `FileCore` volume cannot carry. A record it admits has a well-defined
    /// zone geometry: every zone's stream lies inside its sector, and zone 0's
    /// holds at least one fragment.
    ///
    /// # Errors
    ///
    /// The record broken, on any structural violation.
    pub fn validate(&self) -> Result<(), Fault> {
        if !LOG2SECSIZE.contains(&self.log2secsize) {
            return Err(BROKEN_RECORD);
        }
        if !LOG2BPMB.contains(&self.log2bpmb) {
            return Err(BROKEN_RECORD);
        }
        if self.idlen < self.least_idlen() || self.idlen > self.most_idlen() {
            return Err(BROKEN_RECORD);
        }
        if self.nzones == 0 {
            return Err(BROKEN_RECORD);
        }
        // The spare leaves room for the zone header, and zone 0's map bits
        // hold this record and one whole fragment after it: every zone's
        // place on the disc, the map's among them, counts from there.
        let map_bits = (8u32 << self.log2secsize).checked_sub(u32::from(self.zone_spare));
        if u32::from(self.zone_spare) < ZONE_HEADER_SIZE_U32 * 8
            || map_bits.is_none_or(|bits| bits <= DISC_RECORD_BITS + u32::from(self.idlen))
        {
            return Err(BROKEN_RECORD);
        }
        if self.format_version > 1 {
            return Err(BROKEN_RECORD);
        }
        if self.disc_size == 0 || !self.disc_size.is_multiple_of(1u64 << self.log2secsize) {
            return Err(BROKEN_RECORD);
        }
        if self.disc_size >> self.log2secsize > u64::from(u32::MAX) {
            return Err(BROKEN_RECORD);
        }
        // The root must name a real fragment, never free space or the defect
        // list.
        if self.root >> 8 < FRAG_ROOT {
            return Err(BROKEN_RECORD);
        }
        Ok(())
    }

    /// The shortest fragment id: a free fragment's link reaches any bit of
    /// its zone's sector.
    const fn least_idlen(&self) -> u8 {
        self.log2secsize + 3
    }

    /// The longest fragment id the directory format's disc addresses carry:
    /// 16 bits in a fixed directory's three bytes, 19 in a big directory's.
    const fn most_idlen(&self) -> u8 {
        if self.format_version != 0 {
            19
        } else {
            16
        }
    }

    /// Whether `other` puts the map where this record does: the same sector
    /// and map-bit sizes, zones, spare bits, id length and disc size.
    #[must_use]
    pub fn locates_map_as(&self, other: &Self) -> bool {
        (
            self.log2secsize,
            self.log2bpmb,
            self.nzones,
            self.zone_spare,
            self.idlen,
            self.disc_size,
        ) == (
            other.log2secsize,
            other.log2bpmb,
            other.nzones,
            other.zone_spare,
            other.idlen,
            other.disc_size,
        )
    }

    /// The zones and id length a map of this sector size, map-bit size and
    /// spare needs to describe `disc_size` bytes: the fewest zones that
    /// cover it, and the shortest id that numbers every fragment those zones
    /// can hold, as `FileCore` lays a new disc out.
    ///
    /// # Errors
    ///
    /// [`Fault::Unsupported`] where no id length the directory format
    /// carries numbers them, or more zones than a record counts are needed.
    pub fn fit_zones(&mut self) -> Result<(), Fault> {
        let zone_bits =
            u64::from((8u32 << self.log2secsize).saturating_sub(u32::from(self.zone_spare)));
        let first = zone_bits.saturating_sub(u64::from(DISC_RECORD_BITS));
        let blocks = self.disc_size.div_ceil(self.bytes_per_map_bit());
        if zone_bits == 0 || first == 0 {
            return Err(Fault::Unsupported);
        }
        let zones = 1 + blocks.saturating_sub(first).div_ceil(zone_bits);
        self.nzones = u16::try_from(zones).map_err(|_| Fault::Unsupported)?;
        for idlen in self.least_idlen()..=self.most_idlen() {
            let ids_per_zone = zone_bits / (u64::from(idlen) + 1);
            if zones * ids_per_zone <= 1 << idlen {
                self.idlen = idlen;
                return Ok(());
            }
        }
        Err(Fault::Unsupported)
    }

    /// The most bytes a map of this sector size, spare, map bit and
    /// directory format describes: as many zones as its longest id numbers
    /// the fragments of, and a record counts.
    #[must_use]
    pub fn most_described(&self) -> u64 {
        let zone_bits =
            u64::from((8u32 << self.log2secsize).saturating_sub(u32::from(self.zone_spare)));
        let first = zone_bits.saturating_sub(u64::from(DISC_RECORD_BITS));
        let idlen = self.most_idlen();
        let ids_per_zone = (zone_bits / (u64::from(idlen) + 1)).max(1);
        let zones = ((1u64 << idlen) / ids_per_zone).clamp(1, u64::from(u16::MAX));
        (first + (zones - 1) * zone_bits) * self.bytes_per_map_bit()
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

    /// The fewest map bits a fragment takes: its id's, and the bit that ends
    /// it.
    #[must_use]
    pub fn least_fragment_bits(&self) -> u32 {
        u32::from(self.idlen) + 1
    }

    /// The bytes of the smallest fragment.
    #[must_use]
    pub fn least_fragment(&self) -> u64 {
        u64::from(self.least_fragment_bits()) * self.bytes_per_map_bit()
    }

    /// The map bits an object of `bytes` is allocated: whole sectors, as
    /// RISC OS 5 allocates them, and no fewer than a fragment takes.
    #[must_use]
    pub fn allocation_bits(&self, bytes: u64) -> u64 {
        let per_sector = (self.sector_size() / self.bytes_per_map_bit()).max(1);
        bytes
            .div_ceil(self.bytes_per_map_bit())
            .max(u64::from(self.least_fragment_bits()))
            .next_multiple_of(per_sector)
    }

    /// The bytes of one share granule, the unit objects sharing a fragment
    /// are placed in.
    #[must_use]
    pub fn share_granule(&self) -> u64 {
        1u64 << (u32::from(self.log2sharesize) + u32::from(self.log2secsize))
    }

    /// The bytes in front of the object at `indaddr` in its fragment: its
    /// share offset less one in granules, none for a fragment of its own.
    #[must_use]
    pub fn share_base(&self, indaddr: u32) -> u64 {
        match indaddr & 0xFF {
            0 => 0,
            offset => u64::from(offset - 1) * self.share_granule(),
        }
    }

    /// Map bits carried by each zone sector.
    #[must_use]
    pub fn zone_bits(&self) -> u32 {
        (8u32 << self.log2secsize) - u32::from(self.zone_spare)
    }

    /// Fragment ids homed in each zone.
    #[must_use]
    pub fn ids_per_zone(&self) -> u32 {
        self.zone_bits() / self.least_fragment_bits()
    }

    /// The map bits zone `zone`'s fragment stream describes: a zone's map
    /// bits, less the disc record's in zone 0. Defined for a record
    /// [`validate`](Self::validate) admits.
    #[must_use]
    pub fn zone_stream_bits(&self, zone: u32) -> u32 {
        ZONE_HEADER_SIZE_U32 * 8 + self.zone_bits() - zone_start_bit(zone)
    }

    /// The first disc block, in map bits, zone `zone`'s stream describes;
    /// for `zone == nzones`, where a zone after the last would begin, so the
    /// blocks the whole map can describe. Defined for a record
    /// [`validate`](Self::validate) admits.
    #[must_use]
    pub fn zone_start_block(&self, zone: u32) -> u64 {
        if zone == 0 {
            0
        } else {
            u64::from(zone) * u64::from(self.zone_bits()) - u64::from(DISC_RECORD_BITS)
        }
    }

    /// Disc byte address of the first map zone: the map sits in the middle
    /// zone of the disc, which on a single-zone volume is sector 0. Defined
    /// for a record [`validate`](Self::validate) admits.
    #[must_use]
    pub fn map_offset(&self) -> u64 {
        self.zone_start_block(u32::from(self.nzones >> 1)) * self.bytes_per_map_bit()
    }

    /// Size of the whole map in bytes (`nzones` sectors).
    #[must_use]
    pub fn map_size(&self) -> u64 {
        u64::from(self.nzones) * self.sector_size()
    }

    /// Disc byte address of zone `zone` in the map's `copy`: `FileCore`
    /// keeps the map twice, the second copy straight after the first.
    #[must_use]
    pub fn zone_offset(&self, copy: MapCopy, zone: u32) -> u64 {
        let copy_start = match copy {
            MapCopy::First => 0,
            MapCopy::Second => self.map_size(),
        };
        self.map_offset() + copy_start + u64::from(zone) * self.sector_size()
    }

    /// Whether the record's reserved tail is clear, as every genuine record's
    /// is: an all-zero block checksums, but no real record is all zero.
    #[must_use]
    pub fn tail_clear(raw: &[u8; DISC_RECORD_SIZE]) -> bool {
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

/// Place `record` in the boot block `block` and seal it with its
/// [`boot_block_checksum`].
pub fn seal_boot_block(block: &mut [u8; BOOT_BLOCK_SIZE], record: &DiscRecord) {
    block[DISC_RECORD_IN_BOOT_BLOCK..DISC_RECORD_IN_BOOT_BLOCK + DISC_RECORD_SIZE]
        .copy_from_slice(&record.encode());
    block[BOOT_BLOCK_SIZE - 1] = boot_block_checksum(block);
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

/// The disc's size in sectors, as the old map `map` records it.
#[must_use]
pub fn old_map_sectors(map: &[u8; OLD_MAP_SIZE]) -> u32 {
    get_u24(map, DISC_SECTORS)
}

/// Validate a raw old map image: both sector checksums, the recorded disc
/// size, the end pointer, and every free area entry.
///
/// A disc larger than its device is not refused here: an image short of its
/// final, free tracks is common, and its driver serves it read-only.
///
/// # Errors
///
/// The map broken, on any structural violation.
pub fn validate_old_map(map: &[u8; OLD_MAP_SIZE]) -> Result<(), Fault> {
    let (sector0, sector1) = map.split_at(256);
    if old_map_checksum(sector0) != sector0[255] || old_map_checksum(sector1) != sector1[255] {
        return Err(BROKEN_MAP);
    }
    let disc_sectors = old_map_sectors(map);
    if disc_sectors == 0 || disc_sectors >= ENTRY_LIMIT {
        return Err(BROKEN_MAP);
    }
    let end = usize::from(map[END_POINTER]);
    if end % 3 != 0 || end > MAX_FREE_AREAS * 3 {
        return Err(BROKEN_MAP);
    }
    for index in 0..end / 3 {
        let start = get_u24(map, index * 3);
        let len = get_u24(map, 0x100 + index * 3);
        if start >= ENTRY_LIMIT || len >= ENTRY_LIMIT {
            return Err(BROKEN_MAP);
        }
        if len == 0 || start.checked_add(len).is_none_or(|e| e > disc_sectors) {
            return Err(BROKEN_MAP);
        }
    }
    Ok(())
}

fn low_byte(value: u32) -> u8 {
    value.to_le_bytes()[0]
}

#[cfg(test)]
mod tests;
