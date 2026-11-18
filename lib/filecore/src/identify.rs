//! Where a disc's structures lie, found as `FileCore` finds them: the
//! checksummed boot block (F-class and hard discs), a bare disc record whose
//! zone check holds (E-class), or a checksummed old map whose root carries its
//! marker (S/M/L/D).

use tairix_fsmeta::preset::acorn::latin1_char;

use crate::record::{
    boot_block_checksum, old_map_sectors, validate_old_map, zone_check, DiscRecord, MapCopy,
    BARE_RECORD_OFFSET, BOOT_BLOCK_OFFSET, BOOT_BLOCK_SIZE, DIR_MARKERS, DISC_RECORD_IN_BOOT_BLOCK,
    DISC_RECORD_SIZE, NAME_LEN, NAME_UTF8_MAX, OLD_MAP_SIZE, OLD_NAME_BYTES, OLD_SECTOR_SIZE,
    ZONE_CHECK_BYTE, ZONE_HEADER_SIZE,
};
use crate::{Disc, Error, Result};

/// How a disc is laid out, as identification found it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Layout {
    /// A new-map disc, described by its disc record.
    NewMap {
        /// The validated disc record: the map's own where the map was found
        /// through a boot block, whose copy need only find it.
        record: DiscRecord,
        /// The boot block's own copy, where a boot block found the map: a
        /// writer keeps it in step, and it may leave the identity blank.
        boot: Option<DiscRecord>,
    },
    /// An old-map disc.
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
    /// The disc's name, trimmed of padding: up to [`NAME_LEN`] bytes.
    #[must_use]
    pub fn name(&self) -> ([u8; NAME_LEN], usize) {
        match self {
            Self::NewMap { record, .. } => trimmed(record.disc_name),
            Self::OldMap { name, .. } => trimmed(*name),
        }
    }

    /// The disc's name in UTF-8, read from RISC OS's Latin-1.
    #[must_use]
    pub fn label(&self) -> ([u8; NAME_UTF8_MAX], usize) {
        let (name, len) = self.name();
        let mut label = [0u8; NAME_UTF8_MAX];
        let mut used = 0;
        for ch in name[..len].iter().filter_map(|&byte| latin1_char(byte)) {
            used += ch.encode_utf8(&mut label[used..]).len();
        }
        (label, used)
    }
}

/// The layout `disc` carries, in `FileCore`'s order; `None` when no structure
/// holds — never a guess.
///
/// # Errors
///
/// Only the disc's failure: a structure lying past the disc's end is simply
/// absent.
pub fn identify<D: Disc>(disc: &mut D) -> Result<Option<Layout>, D> {
    if let Some(boot) = boot_block_record(disc)? {
        return Ok(Some(Layout::NewMap {
            record: map_record(disc, boot)?,
            boot: Some(boot),
        }));
    }
    if let Some(record) = bare_record(disc)? {
        return Ok(Some(Layout::NewMap { record, boot: None }));
    }
    old_map(disc)
}

/// `out.len()` bytes at `at`, or `false` where they lie past the disc's end.
fn read_present<D: Disc>(disc: &mut D, at: u64, out: &mut [u8]) -> Result<bool, D> {
    let fits = at
        .checked_add(out.len() as u64)
        .is_some_and(|end| end <= disc.capacity());
    if !fits {
        return Ok(false);
    }
    disc.read(at, out).map_err(Error::Disc)?;
    Ok(true)
}

/// The disc record a valid boot block carries.
fn boot_block_record<D: Disc>(disc: &mut D) -> Result<Option<DiscRecord>, D> {
    let mut block = [0u8; BOOT_BLOCK_SIZE];
    if !read_present(disc, BOOT_BLOCK_OFFSET, &mut block)?
        || boot_block_checksum(&block) != block[BOOT_BLOCK_SIZE - 1]
    {
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

/// The record zone 0 of the first map copy whose zone checks holds, where it
/// puts the map where `found` does: the disc's name and id live there, and a
/// boot block's copy may leave them blank. `found` itself otherwise.
fn map_record<D: Disc>(disc: &mut D, found: DiscRecord) -> Result<DiscRecord, D> {
    let mut zone = [0u8; 1024];
    let sector = usize::try_from(found.sector_size()).unwrap_or(zone.len());
    let Some(zone) = zone.get_mut(..sector) else {
        return Ok(found);
    };
    for copy in [MapCopy::First, MapCopy::Second] {
        if !read_present(disc, found.zone_offset(copy, 0), zone)?
            || zone_check(zone) != zone[ZONE_CHECK_BYTE]
        {
            continue;
        }
        let mut raw = [0u8; DISC_RECORD_SIZE];
        raw.copy_from_slice(&zone[ZONE_HEADER_SIZE..ZONE_HEADER_SIZE + DISC_RECORD_SIZE]);
        let record = DiscRecord::parse(&raw);
        if record.validate().is_ok() && record.locates_map_as(&found) {
            return Ok(record);
        }
    }
    Ok(found)
}

/// The bare disc record of a single-zone (E-class) disc, accepted only when
/// zone 0's check byte holds over the sector that carries it.
fn bare_record<D: Disc>(disc: &mut D) -> Result<Option<DiscRecord>, D> {
    let mut raw = [0u8; DISC_RECORD_SIZE];
    if !read_present(disc, BARE_RECORD_OFFSET, &mut raw)? {
        return Ok(None);
    }
    let record = DiscRecord::parse(&raw);
    if record.validate().is_err() || record.nzones != 1 || !DiscRecord::tail_clear(&raw) {
        return Ok(None);
    }
    let mut zone = [0u8; 1024];
    let zone = &mut zone[..usize::try_from(record.sector_size()).unwrap_or(1024)];
    if !read_present(disc, 0, zone)? || zone_check(zone) != zone[ZONE_CHECK_BYTE] {
        return Ok(None);
    }
    Ok(Some(record))
}

/// A checksummed old map whose root directory carries a marker.
fn old_map<D: Disc>(disc: &mut D) -> Result<Option<Layout>, D> {
    let mut map = [0u8; OLD_MAP_SIZE];
    if !read_present(disc, 0, &mut map)? || validate_old_map(&map).is_err() {
        return Ok(None);
    }
    // Five zero bytes at sector 2 mark a large-sector (D) disc, whose root
    // sits at sector 4; otherwise the 1280-byte root is at sector 2.
    let mut lead = [0u8; 5];
    if !read_present(disc, 2 * OLD_SECTOR_SIZE, &mut lead)? {
        return Ok(None);
    }
    let large_directories = lead == [0u8; 5];
    let root_sector = if large_directories { 4 } else { 2 };
    let mut header = [0u8; 5];
    if !read_present(disc, u64::from(root_sector) * OLD_SECTOR_SIZE, &mut header)?
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
        disc_sectors: old_map_sectors(&map),
        name,
    }))
}

/// A name field with its trailing spaces removed, ending at its first byte no
/// name holds: a NUL or other control byte, or one RISC OS's Latin-1 leaves
/// undefined. `FileCore` pads names with spaces, its RAM disc with NULs, and an
/// old map that was never named holds zeros, or the low bytes of the Level 3
/// partition fields it overlaps.
#[must_use]
pub fn trimmed(name: [u8; NAME_LEN]) -> ([u8; NAME_LEN], usize) {
    let held = name
        .iter()
        .position(|&b| latin1_char(b).is_none())
        .unwrap_or(NAME_LEN);
    let len = name[..held]
        .iter()
        .rposition(|&b| b != b' ')
        .map_or(0, |last| last + 1);
    (name, len)
}

#[cfg(test)]
mod tests;
