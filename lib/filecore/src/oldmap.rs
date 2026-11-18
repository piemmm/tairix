//! Old-map free-space engine (ADFS S/M/L/D and old-map hard discs).
//!
//! The first two 256-byte sectors of an old-map volume hold the
//! free-space map: sector 0 lists the start sector of each free area,
//! sector 1 the matching lengths, and each sector ends in an
//! add-with-carry checksum byte. Every object on an old-map volume is a
//! single contiguous run of 256-byte sectors, so allocation is a
//! first-fit search of this list and freeing merges the released run
//! back in. Reference: J.G. Harston, "Acorn 8-Bit ADFS Filesystem
//! Structure" (mdfs.net).
//!
//! The engine keeps the raw 512-byte map image and edits only the fields
//! it owns — the free-area entries, the end pointer, the checksums, and the
//! disc name when the disc is retitled — so the boot option, disc id, and
//! the Level 3 fileserver partition pointers round-trip byte-exact. A
//! retitle writes the name over the low bytes of those pointers, which its
//! bytes share.

use crate::bytes::{get_u24, put_u24};
use crate::record::{
    old_map_checksum, old_map_sectors, validate_old_map, DISC_SECTORS, END_POINTER, MAX_FREE_AREAS,
    NAME_LEN, OLD_BOOT_OPTION, OLD_DISC_ID, OLD_MAP_SIZE, OLD_NAME_BYTES,
};
use crate::{Disc, Fault, Result, Structure};

/// What a map failing its checks is.
const BROKEN: Fault = Fault::Broken(Structure::Map);

/// The old-map free-space engine.
pub struct OldMap {
    raw: [u8; OLD_MAP_SIZE],
    disc_sectors: u32,
}

impl OldMap {
    /// Read and validate the map from sectors 0–1 of `volume`.
    ///
    /// # Errors
    ///
    /// * The map broken if either checksum fails, or the end pointer or an
    ///   entry is structurally invalid.
    /// * The disc's failure.
    pub fn load<D: Disc>(disc: &mut D) -> Result<Self, D> {
        let mut raw = [0u8; OLD_MAP_SIZE];
        disc.read(0, &mut raw).map_err(crate::Error::Disc)?;
        validate_old_map(&raw)?;
        Ok(Self {
            disc_sectors: old_map_sectors(&raw),
            raw,
        })
    }

    /// Write the map back to sectors 0–1 of `volume`, refreshing both
    /// checksums.
    ///
    /// # Errors
    ///
    /// * The disc's failure.
    pub fn store<D: Disc>(&mut self, disc: &mut D) -> Result<(), D> {
        {
            let (sector0, sector1) = self.raw.split_at_mut(256);
            sector0[255] = old_map_checksum(sector0);
            sector1[255] = old_map_checksum(sector1);
        }
        disc.write(0, &self.raw).map_err(crate::Error::Disc)
    }

    /// Total sectors on the volume.
    #[must_use]
    pub fn disc_sectors(&self) -> u32 {
        self.disc_sectors
    }

    /// The `*Opt 4` boot option.
    #[must_use]
    pub const fn boot_option(&self) -> u8 {
        self.raw[OLD_BOOT_OPTION]
    }

    /// The boot option set, for [`Self::store`] to write and reseal.
    pub fn set_boot_option(&mut self, option: u8) {
        self.raw[OLD_BOOT_OPTION] = option;
    }

    /// Each free area in turn, as its byte offset and length.
    pub fn free_areas(&self, mut visit: impl FnMut(u64, u64)) {
        for index in 0..self.area_count() {
            let (start, len) = self.entry(index);
            visit(
                u64::from(start) * crate::record::OLD_SECTOR_SIZE,
                u64::from(len) * crate::record::OLD_SECTOR_SIZE,
            );
        }
    }

    /// Whether `sector` lies in a free area.
    #[must_use]
    pub fn is_free(&self, sector: u32) -> bool {
        (0..self.area_count()).any(|index| {
            let (start, len) = self.entry(index);
            sector >= start && sector - start < len
        })
    }

    /// The disc's name, read across the two sectors.
    #[must_use]
    pub fn name(&self) -> [u8; NAME_LEN] {
        let mut name = [0u8; NAME_LEN];
        for (byte, &at) in name.iter_mut().zip(OLD_NAME_BYTES.iter()) {
            *byte = self.raw[at];
        }
        name
    }

    /// Spell `name` across the two sectors a character at a time, for
    /// [`Self::store`] to write and reseal.
    pub fn set_name(&mut self, name: [u8; NAME_LEN]) {
        for (&byte, &at) in name.iter().zip(OLD_NAME_BYTES.iter()) {
            self.raw[at] = byte;
        }
    }

    /// The disc id moved on by one, for [`Self::store`] to write and reseal.
    pub fn bump_disc_id(&mut self) {
        let id = u16::from_le_bytes([self.raw[OLD_DISC_ID], self.raw[OLD_DISC_ID + 1]]);
        self.raw[OLD_DISC_ID..OLD_DISC_ID + 2].copy_from_slice(&id.wrapping_add(1).to_le_bytes());
    }

    /// Number of live free-area entries.
    fn area_count(&self) -> usize {
        usize::from(self.raw[END_POINTER]) / 3
    }

    /// The `index`-th free area as `(start_sector, sector_count)`.
    fn entry(&self, index: usize) -> (u32, u32) {
        (
            get_u24(&self.raw[..256], index * 3),
            get_u24(&self.raw[256..], index * 3),
        )
    }

    fn set_entry(&mut self, index: usize, start: u32, len: u32) {
        put_u24(&mut self.raw[..256], index * 3, start);
        put_u24(&mut self.raw[256..512], index * 3, len);
    }

    /// Remove the `index`-th free area, shifting the tail down.
    fn remove_entry(&mut self, index: usize) {
        let count = self.area_count();
        for i in index..count - 1 {
            let (start, len) = self.entry(i + 1);
            self.set_entry(i, start, len);
        }
        self.set_entry(count - 1, 0, 0);
        // At most 82 areas, so the pointer byte never truncates.
        self.raw[END_POINTER] = u8::try_from((count - 1) * 3).unwrap_or(0);
    }

    /// Insert a free area at `index`, shifting the tail up. The caller
    /// has verified the map is not full.
    fn insert_entry(&mut self, index: usize, start: u32, len: u32) {
        let count = self.area_count();
        let mut i = count;
        while i > index {
            let (s, l) = self.entry(i - 1);
            self.set_entry(i, s, l);
            i -= 1;
        }
        self.set_entry(index, start, len);
        // At most 82 areas, so the pointer byte never truncates.
        self.raw[END_POINTER] = u8::try_from((count + 1) * 3).unwrap_or(0);
    }

    /// Allocate `sectors` contiguous sectors, first-fit, returning the
    /// start sector.
    ///
    /// # Errors
    ///
    /// * [`Fault::DiscFull`] if no free area is large enough.
    pub fn allocate(&mut self, sectors: u32) -> core::result::Result<u32, Fault> {
        if sectors == 0 {
            return Err(Fault::DiscFull);
        }
        for index in 0..self.area_count() {
            let (start, len) = self.entry(index);
            if len >= sectors {
                if len == sectors {
                    self.remove_entry(index);
                } else {
                    self.set_entry(index, start + sectors, len - sectors);
                }
                return Ok(start);
            }
        }
        Err(Fault::DiscFull)
    }

    /// Grow an allocation ending at `end_sector` by `extra` sectors if a
    /// free area starts exactly there, consuming the space. Returns
    /// whether the in-place extension happened.
    pub fn try_extend(&mut self, end_sector: u32, extra: u32) -> bool {
        if extra == 0 {
            return true;
        }
        for index in 0..self.area_count() {
            let (start, len) = self.entry(index);
            if start == end_sector && len >= extra {
                if len == extra {
                    self.remove_entry(index);
                } else {
                    self.set_entry(index, start + extra, len - extra);
                }
                return true;
            }
        }
        false
    }

    /// Return the run `[start, start + sectors)` to the free list,
    /// merging with adjacent free areas.
    ///
    /// # Errors
    ///
    /// * [`Fault::Compaction`] if the freed run cannot be merged and the map
    ///   already holds its maximum number of areas.
    /// * The map broken if the run overlaps an existing free area or leaves
    ///   the disc — the caller handed back space the map says is already
    ///   free, which means the disc is corrupt.
    pub fn free_span(&mut self, start: u32, sectors: u32) -> core::result::Result<(), Fault> {
        if sectors == 0 {
            return Ok(());
        }
        let end = start.checked_add(sectors).ok_or(BROKEN)?;
        if end > self.disc_sectors {
            return Err(BROKEN);
        }
        // Find the insertion point keeping the list sorted by start.
        let count = self.area_count();
        let mut index = 0;
        while index < count && self.entry(index).0 < start {
            index += 1;
        }
        // Overlap with either neighbour is corruption, not a merge.
        if index > 0 {
            let (prev_start, prev_len) = self.entry(index - 1);
            if prev_start + prev_len > start {
                return Err(BROKEN);
            }
        }
        if index < count && end > self.entry(index).0 {
            return Err(BROKEN);
        }
        let merges_prev = index > 0 && {
            let (prev_start, prev_len) = self.entry(index - 1);
            prev_start + prev_len == start
        };
        let merges_next = index < count && self.entry(index).0 == end;
        match (merges_prev, merges_next) {
            (true, true) => {
                let (prev_start, prev_len) = self.entry(index - 1);
                let (_, next_len) = self.entry(index);
                self.set_entry(index - 1, prev_start, prev_len + sectors + next_len);
                self.remove_entry(index);
            }
            (true, false) => {
                let (prev_start, prev_len) = self.entry(index - 1);
                self.set_entry(index - 1, prev_start, prev_len + sectors);
            }
            (false, true) => {
                let (_, next_len) = self.entry(index);
                self.set_entry(index, start, next_len + sectors);
            }
            (false, false) => {
                if count == MAX_FREE_AREAS {
                    return Err(Fault::Compaction);
                }
                self.insert_entry(index, start, sectors);
            }
        }
        Ok(())
    }

    /// Total free sectors.
    #[must_use]
    pub fn free_sectors(&self) -> u64 {
        let mut total = 0u64;
        for index in 0..self.area_count() {
            total += u64::from(self.entry(index).1);
        }
        total
    }

    /// Build a fresh map image for a volume of `disc_sectors` sectors
    /// whose free space is the single run `[first_free, disc_sectors)`.
    #[must_use]
    pub fn initialise(disc_sectors: u32, first_free: u32, disc_id: u16) -> Self {
        let mut map = Self {
            raw: [0u8; OLD_MAP_SIZE],
            disc_sectors,
        };
        put_u24(&mut map.raw[..256], 0, first_free);
        put_u24(&mut map.raw[256..512], 0, disc_sectors - first_free);
        put_u24(&mut map.raw, DISC_SECTORS, disc_sectors);
        map.raw[OLD_DISC_ID..OLD_DISC_ID + 2].copy_from_slice(&disc_id.to_le_bytes());
        map.raw[END_POINTER] = 3;
        map
    }
}
