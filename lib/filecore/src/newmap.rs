//! New-map allocation engine (ADFS E/E+/F/F+ and new-map hard discs).
//!
//! The map is a set of zones, one sector each. A zone starts with a
//! 4-byte header — check byte, 15-bit free-space link, cross-check byte —
//! followed by a bitstream of variable-length fragments: an `idlen`-bit
//! fragment id, zero or more clear bits, and a set stop bit. Each map bit
//! stands for `1 << log2bpmb` bytes of disc. Zone 0 additionally embeds
//! the 60-byte disc record directly after its header. Free fragments form
//! a per-zone linked list: the header link and each free fragment's id
//! field hold the bit distance to the next free fragment.
//!
//! The map is held twice, the second copy straight after the first. The
//! engine reads the first copy whose every zone checks and writes both.
//!
//! Reference: the RISC OS PRM `FileCore` formats chapter.

use alloc::vec::Vec;

use crate::record::{
    link_bits, seal_boot_block, zone_check, zone_start_bit, DiscRecord, MapCopy, BOOT_BLOCK_OFFSET,
    BOOT_BLOCK_SIZE, CROSS_CHECK_BYTE, CROSS_CHECK_SUM, DISC_RECORD_SIZE, FRAG_BAD, FRAG_ROOT,
    FREELINK_BIT, NAME_LEN, ZONE_CHECK_BYTE, ZONE_HEADER_SIZE,
};
use crate::{Disc, Error, Fault, Result, Structure};

/// What a map failing its checks is.
const BROKEN: Fault = Fault::Broken(Structure::Map);

/// What an address naming the disc's own structures as an object is: only a
/// broken directory hands one over.
const NOT_AN_OBJECT: Fault = Fault::Broken(Structure::Directory);

/// Refuses `frag_id` as a fragment an object may grow, shrink or free.
fn own_fragment(frag_id: u32) -> core::result::Result<(), Fault> {
    if NewMap::is_object_fragment(frag_id) {
        Ok(())
    } else {
        Err(NOT_AN_OBJECT)
    }
}

/// Largest map sector (zone) size: `log2secsize` is at most 10.
pub const MAX_ZONE_BYTES: usize = 1024;

/// The new-map allocation engine.
///
/// The engine re-reads and re-writes one zone sector at a time through
/// an on-stack buffer, so its memory footprint is one zone regardless of
/// disc size.
pub struct NewMap {
    /// The validated disc record (from zone 0 or the boot block).
    pub record: DiscRecord,
    /// The copy the engine reads: the first whose every zone checks.
    reads: MapCopy,
    /// The other copy, when it checks too but records something else: an
    /// object the copy read lacks is looked for there, and the disc is
    /// never written, which could allocate over what only it records.
    recovers: Option<MapCopy>,
}

/// What mapping a defect out found.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MappedOut {
    /// It is mapped out now.
    Done,
    /// A defect held it already, or no zone describes it.
    Already,
    /// The disc's own structures or an object hold it.
    InUse,
}

/// Where compaction may move a fragment.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Moves {
    /// A file's: down or up.
    Anywhere,
    /// A directory's: up only.
    Up,
}

/// The most of a disc copied at once while a fragment moves.
const COPY_CHUNK: usize = 4096;

/// `length` bytes of `disc` copied from `from` to `to`, which do not
/// overlap, a chunk at a time.
fn copy_bytes<D: Disc>(disc: &mut D, from: u64, to: u64, length: u64) -> Result<(), D> {
    let mut chunk = [0u8; COPY_CHUNK];
    let mut done = 0;
    while done < length {
        let take = usize::try_from((length - done).min(COPY_CHUNK as u64)).unwrap_or(COPY_CHUNK);
        disc.read(from + done, &mut chunk[..take])
            .map_err(Error::Disc)?;
        disc.write(to + done, &chunk[..take]).map_err(Error::Disc)?;
        done += take as u64;
    }
    Ok(())
}

/// One zone staged in memory.
struct Zone {
    bytes: [u8; MAX_ZONE_BYTES],
    len: usize,
    /// First fragment bit of this zone's stream.
    start_bit: u32,
    /// One past the last valid map bit of this zone's stream.
    end_bit: u32,
    /// Disc block (map-bit units) where this zone's coverage starts.
    start_block: u64,
}

impl Zone {
    fn bits(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// Read the bit at `bit` (little-endian bit order).
    fn bit(&self, bit: u32) -> bool {
        bits_get(&self.bytes[..self.len], bit)
    }

    fn set_bit(&mut self, bit: u32, value: bool) {
        bits_set(&mut self.bytes[..self.len], bit, value);
    }

    /// Read `count` bits (at most 19 plus slack; bounded by 25) starting
    /// at `bit`.
    fn field(&self, bit: u32, count: u32) -> u32 {
        bits_field(&self.bytes[..self.len], bit, count)
    }

    fn set_field(&mut self, bit: u32, count: u32, value: u32) {
        bits_set_field(&mut self.bytes[..self.len], bit, count, value);
    }

    /// Find the next set bit at or after `from`, up to `limit`.
    fn next_set_bit(&self, from: u32, limit: u32) -> Option<u32> {
        let mut bit = from;
        while bit < limit {
            if self.bit(bit) {
                return Some(bit);
            }
            bit += 1;
        }
        None
    }
}

/// Read the bit at `bit` of a little-endian bitstream.
pub(crate) fn bits_get(buf: &[u8], bit: u32) -> bool {
    let byte = (bit / 8) as usize;
    byte < buf.len() && buf[byte] & (1 << (bit % 8)) != 0
}

/// Set or clear the bit at `bit` of a little-endian bitstream.
pub(crate) fn bits_set(buf: &mut [u8], bit: u32, value: bool) {
    let byte = (bit / 8) as usize;
    if byte < buf.len() {
        if value {
            buf[byte] |= 1 << (bit % 8);
        } else {
            buf[byte] &= !(1 << (bit % 8));
        }
    }
}

/// Read a `count`-bit little-endian field starting at `bit`.
pub(crate) fn bits_field(buf: &[u8], bit: u32, count: u32) -> u32 {
    let mut value: u64 = 0;
    let first = (bit / 8) as usize;
    for i in 0..4 {
        let byte = first + i;
        if byte < buf.len() {
            value |= u64::from(buf[byte]) << (8 * i);
        }
    }
    // The mask keeps at most 32 bits, so the narrowing never truncates.
    u32::try_from((value >> (bit % 8)) & ((1u64 << count) - 1)).unwrap_or(0)
}

/// Write a `count`-bit little-endian field starting at `bit`.
pub(crate) fn bits_set_field(buf: &mut [u8], bit: u32, count: u32, value: u32) {
    for i in 0..count {
        bits_set(buf, bit + i, value & (1 << i) != 0);
    }
}

/// A fragment of an object within one zone, in disc terms.
#[derive(Copy, Clone)]
struct FragmentRun {
    /// Disc byte address of the fragment's data.
    disc_byte: u64,
    /// Fragment length in bytes.
    len: u64,
    /// The map bits of the fragment ahead of where it was looked up.
    into: u64,
}

/// One map fragment yielded by [`Walker`].
#[derive(Copy, Clone)]
struct Frag {
    /// First map bit of the fragment (its id field).
    bit: u32,
    /// The fragment's stop bit.
    end: u32,
    /// The raw id field (a fragment id, or a free-list link).
    id: u32,
    /// Whether the fragment is on the zone's free list.
    is_free: bool,
    /// The free-list predecessor: a free fragment's bit, or `None` for
    /// the zone-header link.
    prev_free: Option<u32>,
}

/// Sequential fragment walker over one staged zone.
struct Walker {
    bit: u32,
    next_free: u32,
    prev_free: Option<u32>,
    idlen: u32,
    link_bits: u32,
}

impl Walker {
    fn new(staged: &Zone, idlen: u32, link_bits: u32) -> Self {
        let link = staged.field(FREELINK_BIT, link_bits);
        Self {
            bit: staged.start_bit,
            next_free: if link == 0 { 0 } else { FREELINK_BIT + link },
            prev_free: None,
            idlen,
            link_bits,
        }
    }

    /// Yield the next fragment, or `None` at the end of the zone.
    ///
    /// # Errors
    ///
    /// The map broken if a fragment runs past the zone end
    /// (no stop bit before the map boundary).
    fn next(&mut self, staged: &Zone) -> core::result::Result<Option<Frag>, Fault> {
        if self.bit >= staged.end_bit {
            return Ok(None);
        }
        let bit = self.bit;
        let id = staged.field(bit, self.idlen);
        let end = staged
            .next_set_bit(bit + self.idlen, staged.end_bit)
            .ok_or(BROKEN)?;
        let is_free = bit == self.next_free;
        let prev_free = self.prev_free;
        if is_free {
            let link = staged.field(bit, self.link_bits);
            self.next_free = if link == 0 { 0 } else { bit + link };
            self.prev_free = Some(bit);
        }
        self.bit = end + 1;
        Ok(Some(Frag {
            bit,
            end,
            id,
            is_free,
            prev_free,
        }))
    }
}

impl NewMap {
    /// Attach to a disc whose validated disc record is `record`, reading
    /// the first map copy whose every zone checks and whose cross-check holds,
    /// as `FileCore` does.
    ///
    /// # Errors
    ///
    /// * The map broken if the record is inconsistent or leaves
    ///   no room for both copies, or neither copy checks, or the map
    ///   describes anything but a defect past the disc's end.
    /// * The disc's failure for a zone of the copy past the end of a short
    ///   disc.
    /// * The disc's failure.
    pub fn open<D: Disc>(disc: &mut D, record: DiscRecord) -> Result<Self, D> {
        record.validate()?;
        if record.sector_size() > MAX_ZONE_BYTES as u64 {
            return Err(BROKEN.into());
        }
        let maps_end = record
            .map_size()
            .checked_mul(2)
            .and_then(|both| both.checked_add(record.map_offset()))
            .ok_or(BROKEN)?;
        if maps_end > record.disc_size {
            return Err(BROKEN.into());
        }
        let mut map = Self {
            record,
            reads: MapCopy::First,
            recovers: None,
        };
        match map.copy_checks(disc, MapCopy::First) {
            Ok(()) => {
                if map.copy_checks(disc, MapCopy::Second).is_ok() && !map.copies_agree(disc)? {
                    map.recovers = Some(MapCopy::Second);
                }
            }
            Err(Error::Fault(BROKEN)) => {
                map.copy_checks(disc, MapCopy::Second)?;
                map.reads = MapCopy::Second;
            }
            Err(err) => return Err(err),
        }
        map.adopt_zone_record(disc)?;
        map.check_past_disc(disc)?;
        Ok(map)
    }

    /// Refuses a map that leaves anything but a defect where the disc ends,
    /// which would give out space no sector holds. The block the end falls
    /// in counts as the disc's.
    fn check_past_disc<D: Disc>(&self, disc: &mut D) -> Result<(), D> {
        let end = self
            .record
            .disc_size
            .div_ceil(self.record.bytes_per_map_bit());
        for zone in 0..u32::from(self.record.nzones) {
            let (start_bit, end_bit, start_block) = self.zone_geometry(zone);
            let within = end.saturating_sub(start_block);
            if within >= u64::from(end_bit - start_bit) {
                continue;
            }
            // Below the zone's stream, so it fits the bit numbering.
            let limit = start_bit + u32::try_from(within).unwrap_or(0);
            let staged = self.load_zone(disc, zone)?;
            let mut walker = self.walker(&staged);
            while let Some(frag) = walker.next(&staged)? {
                if frag.end >= limit && (frag.is_free || frag.id != FRAG_BAD) {
                    return Err(BROKEN.into());
                }
            }
        }
        Ok(())
    }

    /// The disc record as zone 0 of the copy read holds it, which `FileCore`
    /// takes as the disc's: a boot block's copy need only find the map, and
    /// may leave the identity blank. A zone record that would put the map
    /// elsewhere is a broken map.
    fn adopt_zone_record<D: Disc>(&mut self, disc: &mut D) -> Result<(), D> {
        let staged = self.load_zone(disc, 0)?;
        let mut raw = [0u8; DISC_RECORD_SIZE];
        raw.copy_from_slice(&staged.bytes[ZONE_HEADER_SIZE..ZONE_HEADER_SIZE + DISC_RECORD_SIZE]);
        let zone = DiscRecord::parse(&raw);
        if zone.validate().is_err() || !zone.locates_map_as(&self.record) {
            return Err(BROKEN.into());
        }
        self.record = zone;
        Ok(())
    }

    /// The disc id moved on by one in the map's record, as `FileCore` moves
    /// it at the first change it makes to a disc it has mounted, so the disc
    /// reads as changed wherever it was known before.
    ///
    /// # Errors
    ///
    /// The disc's failure, or the map broken.
    pub fn bump_disc_id<D: Disc>(&mut self, disc: &mut D) -> Result<(), D> {
        let record = DiscRecord {
            disc_id: self.record.disc_id.wrapping_add(1),
            ..self.record
        };
        self.store_record(disc, false, record)
    }

    /// Whether both copies check yet record different maps, which leaves
    /// the disc read-only.
    #[must_use]
    pub fn copies_disagree(&self) -> bool {
        self.recovers.is_some()
    }

    /// Check every zone of `copy` and its map-wide cross-check.
    fn copy_checks<D: Disc>(&self, disc: &mut D, copy: MapCopy) -> Result<(), D> {
        let mut crosscheck = 0u8;
        for zone in 0..u32::from(self.record.nzones) {
            crosscheck ^= self.load_zone_of(disc, copy, zone)?.bytes[CROSS_CHECK_BYTE];
        }
        if crosscheck != CROSS_CHECK_SUM {
            return Err(BROKEN.into());
        }
        Ok(())
    }

    /// Whether the two copies, both of which check, hold the same bytes.
    fn copies_agree<D: Disc>(&self, disc: &mut D) -> Result<bool, D> {
        for zone in 0..u32::from(self.record.nzones) {
            let first = self.load_zone_of(disc, MapCopy::First, zone)?;
            let second = self.load_zone_of(disc, MapCopy::Second, zone)?;
            if first.bytes[..first.len] != second.bytes[..second.len] {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// The copy read, then the one recovered from.
    fn copies(&self) -> impl Iterator<Item = MapCopy> {
        core::iter::once(self.reads).chain(self.recovers)
    }

    /// Width of a free-space link field.
    fn link_bits(&self) -> u32 {
        link_bits(self.record.idlen)
    }

    /// Whether an object may grow, shrink or free fragment `frag_id`: free
    /// space, the defects and the map's fragment are no entry's.
    #[must_use]
    pub const fn is_object_fragment(frag_id: u32) -> bool {
        frag_id > FRAG_ROOT
    }

    /// Whether `indaddr` may address an object at all: never the defects,
    /// nor the whole of the map's fragment, which the map begins.
    #[must_use]
    pub const fn may_address_object(indaddr: u32) -> bool {
        match indaddr >> 8 {
            FRAG_BAD => false,
            FRAG_ROOT => indaddr & 0xFF != 0,
            _ => true,
        }
    }

    /// The zone the object at `indaddr` is homed in; the central zone, the
    /// map's, for one the map cannot place.
    #[must_use]
    pub fn zone_of(&self, indaddr: u32) -> u32 {
        self.home_zone(indaddr >> 8)
            .unwrap_or(u32::from(self.record.nzones) >> 1)
    }

    /// Home zone of `frag_id` (the root fragment lives in the middle
    /// zone alongside the map).
    fn home_zone(&self, frag_id: u32) -> core::result::Result<u32, Fault> {
        let zone = if frag_id == FRAG_ROOT {
            u32::from(self.record.nzones) >> 1
        } else {
            frag_id / self.record.ids_per_zone()
        };
        if zone >= u32::from(self.record.nzones) {
            return Err(BROKEN);
        }
        Ok(zone)
    }

    /// Locate the fragment run of `frag_id` holding map-bit offset
    /// `map_offset` within the object, returning the run's disc byte
    /// address and remaining length from that offset.
    fn lookup_run<D: Disc>(
        &self,
        disc: &mut D,
        frag_id: u32,
        map_offset: u64,
    ) -> Result<FragmentRun, D> {
        let nzones = u32::from(self.record.nzones);
        for copy in self.copies() {
            let mut offset = map_offset;
            let mut seen = false;
            let mut zone = self.home_zone(frag_id)?;
            for _ in 0..nzones {
                let staged = self.load_zone_of(disc, copy, zone)?;
                if let Some(run) = self.scan_zone(&staged, frag_id, &mut offset, &mut seen)? {
                    return Ok(run);
                }
                zone = (zone + 1) % nzones;
            }
            // A copy recording the object at all is the whole truth about
            // it; stitching another copy's fragments onto it would not be.
            if seen {
                break;
            }
        }
        Err(BROKEN.into())
    }

    /// Scan one zone for `frag_id`, consuming `map_offset` bits of the
    /// object as earlier fragments are passed and noting in `seen` that the
    /// zone holds one.
    fn scan_zone(
        &self,
        staged: &Zone,
        frag_id: u32,
        map_offset: &mut u64,
        seen: &mut bool,
    ) -> core::result::Result<Option<FragmentRun>, Fault> {
        let mut walker = self.walker(staged);
        while let Some(frag) = walker.next(staged)? {
            if frag.is_free || frag.id != frag_id {
                continue;
            }
            *seen = true;
            let length = u64::from(frag.end + 1 - frag.bit);
            if *map_offset < length {
                let block =
                    staged.start_block + u64::from(frag.bit - staged.start_bit) + *map_offset;
                return Ok(Some(FragmentRun {
                    disc_byte: block * self.record.bytes_per_map_bit(),
                    len: (length - *map_offset) * self.record.bytes_per_map_bit(),
                    into: *map_offset,
                }));
            }
            *map_offset -= length;
        }
        Ok(None)
    }

    fn walker(&self, staged: &Zone) -> Walker {
        Walker::new(staged, u32::from(self.record.idlen), self.link_bits())
    }

    /// Locate the run backing byte `byte_offset` of the object at
    /// `indaddr`, returning `(disc_byte, run_bytes_available)`.
    ///
    /// The low byte of `indaddr` is the share offset: `N` places the
    /// object `(N - 1) << log2sharesize` sectors into its fragment.
    ///
    /// # Errors
    ///
    /// * The map broken if the fragment id is unknown, the
    ///   offset is past the object's allocation, or the map is corrupt.
    /// * The directory broken for an address in the disc's own structures:
    ///   the defects, or the map's fragment anywhere but from the root on,
    ///   in the piece the root begins in.
    /// * The disc's failure.
    pub fn locate<D: Disc>(
        &self,
        disc: &mut D,
        indaddr: u32,
        byte_offset: u64,
    ) -> Result<(u64, u64), D> {
        let frag_id = indaddr >> 8;
        if !Self::may_address_object(indaddr) {
            return Err(NOT_AN_OBJECT.into());
        }
        let total = self
            .record
            .share_base(indaddr)
            .checked_add(byte_offset)
            .ok_or(BROKEN)?;
        let bpmb = self.record.bytes_per_map_bit();
        let sub = total % bpmb;
        let run = self.lookup_run(disc, frag_id, total / bpmb)?;
        if frag_id == FRAG_ROOT {
            // The map, and on a disc with a boot block the boot area, share
            // the root's fragment.
            let root = self.record.share_base(self.record.root);
            let piece = (total / bpmb - run.into) * bpmb;
            if self.record.root >> 8 != FRAG_ROOT || total < root || piece > root {
                return Err(NOT_AN_OBJECT.into());
            }
        }
        Ok((run.disc_byte + sub, run.len - sub))
    }

    /// Total bytes of map allocation carried by fragment id `frag_id`, in
    /// the first copy that records it.
    pub fn object_allocated_bytes<D: Disc>(&self, disc: &mut D, frag_id: u32) -> Result<u64, D> {
        for copy in self.copies() {
            let mut bits = 0u64;
            for zone in 0..u32::from(self.record.nzones) {
                let staged = self.load_zone_of(disc, copy, zone)?;
                let mut walker = self.walker(&staged);
                while let Some(frag) = walker.next(&staged)? {
                    if !frag.is_free && frag.id == frag_id {
                        bits += u64::from(frag.end + 1 - frag.bit);
                    }
                }
            }
            if bits != 0 {
                return Ok(bits * self.record.bytes_per_map_bit());
            }
        }
        Ok(0)
    }

    /// Total free bytes on the disc.
    pub fn free_bytes<D: Disc>(&self, disc: &mut D) -> Result<u64, D> {
        let mut bits = 0u64;
        for zone in 0..u32::from(self.record.nzones) {
            let staged = self.load_zone(disc, zone)?;
            let mut walker = self.walker(&staged);
            while let Some(frag) = walker.next(&staged)? {
                if frag.is_free {
                    bits += u64::from(frag.end + 1 - frag.bit);
                }
            }
        }
        Ok(bits * self.record.bytes_per_map_bit())
    }

    /// Each free area of the disc in turn, as its byte offset and length.
    ///
    /// # Errors
    ///
    /// The map broken, or the disc's failure.
    pub fn free_areas<D: Disc>(
        &self,
        disc: &mut D,
        mut visit: impl FnMut(u64, u64),
    ) -> Result<(), D> {
        let bpmb = self.record.bytes_per_map_bit();
        for zone in 0..u32::from(self.record.nzones) {
            let staged = self.load_zone(disc, zone)?;
            let mut walker = self.walker(&staged);
            while let Some(frag) = walker.next(&staged)? {
                if frag.is_free {
                    let block = staged.start_block + u64::from(frag.bit - staged.start_bit);
                    visit(block * bpmb, u64::from(frag.end + 1 - frag.bit) * bpmb);
                }
            }
        }
        Ok(())
    }

    /// Point the free-list predecessor `prev_free` (a free fragment's
    /// bit, or the zone header) at `target` (`0` = end of list).
    fn set_pred_link(&self, staged: &mut Zone, prev_free: Option<u32>, target: u32) {
        let pos = prev_free.unwrap_or(FREELINK_BIT);
        let value = if target == 0 { 0 } else { target - pos };
        staged.set_field(pos, self.link_bits(), value);
    }

    /// Rewrite `[bit, end]` as one free fragment linking to `next`
    /// (`0` = end of list).
    fn write_free_fragment(&self, staged: &mut Zone, bit: u32, end: u32, next: u32) {
        for i in bit..=end {
            staged.set_bit(i, false);
        }
        let link = if next == 0 { 0 } else { next - bit };
        staged.set_field(bit, self.link_bits(), link);
        staged.set_bit(end, true);
    }

    /// Rewrite `[bit, bit + bits - 1]` as an allocated fragment of
    /// `frag_id`.
    fn write_allocated_fragment(&self, staged: &mut Zone, bit: u32, bits: u32, frag_id: u32) {
        for i in bit..bit + bits {
            staged.set_bit(i, false);
        }
        staged.set_field(bit, u32::from(self.record.idlen), frag_id);
        staged.set_bit(bit + bits - 1, true);
    }

    /// Carve `need` map bits for `frag_id` from the free fragment `frag`
    /// in `staged`, splitting off any usable remainder. Returns the bits
    /// actually taken (rounded up to the whole fragment when the
    /// remainder could not stand alone).
    fn carve(&self, staged: &mut Zone, frag: &Frag, mut need: u32, frag_id: u32) -> u32 {
        let total = frag.end + 1 - frag.bit;
        let link = staged.field(frag.bit, self.link_bits());
        let next_free = if link == 0 { 0 } else { frag.bit + link };
        need = need.max(self.record.least_fragment_bits()).min(total);
        if total - need < self.record.least_fragment_bits() {
            // The remainder cannot stand alone; take the whole fragment.
            need = total;
        }
        self.write_allocated_fragment(staged, frag.bit, need, frag_id);
        if need < total {
            let rem_bit = frag.bit + need;
            self.write_free_fragment(staged, rem_bit, frag.end, next_free);
            self.set_pred_link(staged, frag.prev_free, rem_bit);
        } else {
            self.set_pred_link(staged, frag.prev_free, next_free);
        }
        need
    }

    /// Greedily allocate `total_bits` of map space to `frag_id`, walking
    /// zones in lookup scan order from `home`, taking free fragments (or
    /// pieces of them) as encountered. In the first scanned zone only
    /// free fragments starting at or after `first_min_bit` are taken, so
    /// an extension never allocates *before* the object's last fragment.
    ///
    /// On `NoSpace` some fragments may already carry `frag_id`; the
    /// caller owns cleanup (freeing the id, or relocating and then
    /// freeing it), which is total by construction because every
    /// allocated bit carries the id.
    fn take_fragments<D: Disc>(
        &self,
        disc: &mut D,
        frag_id: u32,
        home: u32,
        first_min_bit: u32,
        zones_to_scan: u32,
        total_bits: u64,
    ) -> Result<(), D> {
        let nzones = u32::from(self.record.nzones);
        let mut remaining = total_bits;
        for i in 0..zones_to_scan.min(nzones) {
            let zone = (home + i) % nzones;
            let min_bit = if i == 0 { first_min_bit } else { 0 };
            loop {
                if remaining == 0 {
                    return Ok(());
                }
                let mut staged = self.load_zone(disc, zone)?;
                let mut walker = self.walker(&staged);
                let mut found = None;
                while let Some(frag) = walker.next(&staged)? {
                    if frag.is_free && frag.bit >= min_bit {
                        found = Some(frag);
                        break;
                    }
                }
                let Some(frag) = found else { break };
                let size = frag.end + 1 - frag.bit;
                let want = u32::try_from(remaining.min(u64::from(size))).unwrap_or(size);
                let taken = self.carve(&mut staged, &frag, want, frag_id);
                self.store_zone(disc, zone, &mut staged)?;
                remaining = remaining.saturating_sub(u64::from(taken));
            }
        }
        if remaining == 0 {
            Ok(())
        } else {
            Err(Fault::DiscFull.into())
        }
    }

    /// Grow the object `frag_id` so that its allocation covers
    /// `new_bytes`, first absorbing free space directly after its last
    /// fragment and then appending same-id fragments later in scan
    /// order. Returns `false` when the growth could not be completed —
    /// the caller then relocates the object and frees this id, which
    /// also reclaims any partial growth (every appended bit carries the
    /// id).
    ///
    /// # Errors
    ///
    /// * [`Fault::NoFragment`] if the map carries no such fragment.
    /// * The directory broken for a fragment no object owns.
    /// * The map broken on map corruption.
    /// * The disc's failure.
    pub fn extend_object<D: Disc>(
        &self,
        disc: &mut D,
        frag_id: u32,
        new_bytes: u64,
    ) -> Result<bool, D> {
        own_fragment(frag_id)?;
        let needed_bits = self.record.allocation_bits(new_bytes);
        let nzones = u32::from(self.record.nzones);
        let home = self.home_zone(frag_id)?;
        // Find the object's last fragment in scan order, and its total.
        let mut current_bits = 0u64;
        let mut last: Option<(u32, u32, Frag)> = None;
        for i in 0..nzones {
            let zone = (home + i) % nzones;
            let staged = self.load_zone(disc, zone)?;
            let mut walker = self.walker(&staged);
            while let Some(frag) = walker.next(&staged)? {
                if !frag.is_free && frag.id == frag_id {
                    current_bits += u64::from(frag.end + 1 - frag.bit);
                    last = Some((i, zone, frag));
                }
            }
        }
        let Some((last_order, last_zone, last_frag)) = last else {
            return Err(Fault::NoFragment.into());
        };
        if current_bits >= needed_bits {
            return Ok(true);
        }
        let mut remaining = needed_bits - current_bits;
        // Absorb free space directly following the last fragment.
        let mut staged = self.load_zone(disc, last_zone)?;
        let mut walker = self.walker(&staged);
        let mut succ = None;
        while let Some(frag) = walker.next(&staged)? {
            if frag.is_free && frag.bit == last_frag.end + 1 {
                succ = Some(frag);
                break;
            }
        }
        let mut resume_bit = last_frag.end + 1;
        if let Some(free) = succ {
            let size = free.end + 1 - free.bit;
            let mut take = u32::try_from(remaining.min(u64::from(size))).unwrap_or(size);
            if size - take < self.record.least_fragment_bits() {
                take = size;
            }
            let link = staged.field(free.bit, self.link_bits());
            let next_free = if link == 0 { 0 } else { free.bit + link };
            // Grow the allocated fragment over the absorbed bits.
            staged.set_bit(last_frag.end, false);
            for i in free.bit..free.bit + take {
                staged.set_bit(i, false);
            }
            staged.set_bit(last_frag.end + take, true);
            if take < size {
                let rem_bit = free.bit + take;
                self.write_free_fragment(&mut staged, rem_bit, free.end, next_free);
                self.set_pred_link(&mut staged, free.prev_free, rem_bit);
            } else {
                self.set_pred_link(&mut staged, free.prev_free, next_free);
            }
            self.store_zone(disc, last_zone, &mut staged)?;
            remaining = remaining.saturating_sub(u64::from(take));
            resume_bit = last_frag.end + take + 1;
        }
        if remaining == 0 {
            return Ok(true);
        }
        // Append fresh same-id fragments strictly after the last one in
        // scan order: only the zones from the last fragment's to the end
        // of the scan may hold them, or lookup would reorder the data.
        let zones_left = nzones - last_order;
        match self.take_fragments(disc, frag_id, last_zone, resume_bit, zones_left, remaining) {
            Ok(()) => Ok(true),
            Err(Error::Fault(Fault::DiscFull)) => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// Shrink the object `frag_id` so that it keeps only `new_bytes` of
    /// allocation: the boundary fragment is trimmed (its remainder
    /// joining the free list) and every later fragment is freed.
    ///
    /// # Errors
    ///
    /// * [`Fault::NoFragment`] if the map carries no such fragment.
    /// * The directory broken for a fragment no object owns.
    /// * The map broken on map corruption.
    /// * The disc's failure.
    pub fn shrink_object<D: Disc>(
        &self,
        disc: &mut D,
        frag_id: u32,
        new_bytes: u64,
    ) -> Result<(), D> {
        own_fragment(frag_id)?;
        let needed = self.record.allocation_bits(new_bytes);
        let nzones = u32::from(self.record.nzones);
        let home = self.home_zone(frag_id)?;
        let mut acc = 0u64;
        let mut seen = false;
        for i in 0..nzones {
            let zone = (home + i) % nzones;
            // Fragments are visited in bit order; a mutation never moves
            // an unvisited fragment, so the cursor stays valid.
            let mut cursor = 0u32;
            loop {
                let mut staged = self.load_zone(disc, zone)?;
                let mut walker = self.walker(&staged);
                let mut target = None;
                while let Some(frag) = walker.next(&staged)? {
                    if !frag.is_free && frag.id == frag_id && frag.bit >= cursor {
                        target = Some(frag);
                        break;
                    }
                }
                let Some(frag) = target else { break };
                seen = true;
                let len = u64::from(frag.end + 1 - frag.bit);
                if acc >= needed {
                    // Entirely past the kept extent: free the fragment.
                    self.free_one(&mut staged, &frag)?;
                    self.store_zone(disc, zone, &mut staged)?;
                    cursor = frag.bit;
                    continue;
                }
                if acc + len <= needed {
                    acc += len;
                    cursor = frag.end + 1;
                    continue;
                }
                // The boundary fragment: keep the head, free the tail.
                // The boundary difference is under one fragment's bits.
                let keep_wanted = u32::try_from(needed - acc).unwrap_or(u32::MAX);
                let keep = keep_wanted.max(self.record.least_fragment_bits());
                if u64::from(keep) >= len
                    || len - u64::from(keep) < u64::from(self.record.least_fragment_bits())
                {
                    // The tail cannot stand alone as a free fragment;
                    // the slack stays allocated.
                    acc += len;
                    cursor = frag.end + 1;
                    continue;
                }
                self.trim_allocated(&mut staged, &frag, keep)?;
                self.store_zone(disc, zone, &mut staged)?;
                acc = needed;
                cursor = frag.bit + keep;
            }
        }
        if seen {
            Ok(())
        } else {
            Err(Fault::NoFragment.into())
        }
    }

    /// Shorten the allocated fragment `frag` to `keep` bits, turning the
    /// remainder into free space (merged with a directly following free
    /// fragment where present).
    fn trim_allocated(
        &self,
        staged: &mut Zone,
        frag: &Frag,
        keep: u32,
    ) -> core::result::Result<(), Fault> {
        // Locate the free-list neighbours of the remainder.
        let mut walker = self.walker(staged);
        let mut pred: Option<Frag> = None;
        let mut succ: Option<Frag> = None;
        while let Some(other) = walker.next(staged)? {
            if !other.is_free {
                continue;
            }
            if other.bit < frag.bit {
                pred = Some(other);
            } else {
                succ = Some(other);
                break;
            }
        }
        let rem_bit = frag.bit + keep;
        let (rem_end, next) = match &succ {
            Some(s) if s.bit == frag.end + 1 => {
                let link = staged.field(s.bit, self.link_bits());
                (s.end, if link == 0 { 0 } else { s.bit + link })
            }
            Some(s) => (frag.end, s.bit),
            None => (frag.end, 0),
        };
        // Shorten the allocated fragment.
        staged.set_bit(frag.end, false);
        staged.set_bit(frag.bit + keep - 1, true);
        self.write_free_fragment(staged, rem_bit, rem_end, next);
        self.set_pred_link(staged, pred.as_ref().map(|p| p.bit), rem_bit);
        Ok(())
    }

    /// Rewrite the disc record's root directory size (big directories
    /// grow), refreshing zone 0's check byte and, when the disc
    /// carries one, the boot block copy.
    ///
    /// # Errors
    ///
    /// * The disc's failure.
    /// * The map broken if zone 0 no longer validates.
    pub fn set_root_size<D: Disc>(
        &mut self,
        disc: &mut D,
        boot_block: bool,
        new_size: u32,
    ) -> Result<(), D> {
        let record = DiscRecord {
            root_size: new_size,
            ..self.record
        };
        self.store_record(disc, boot_block, record)
    }

    /// Rewrite the disc record's boot option, as [`Self::set_root_size`]
    /// rewrites its root size.
    ///
    /// # Errors
    ///
    /// * The disc's failure.
    /// * The map broken if zone 0 no longer validates.
    pub fn set_boot_option<D: Disc>(
        &mut self,
        disc: &mut D,
        boot_block: bool,
        option: u8,
    ) -> Result<(), D> {
        let record = DiscRecord {
            bootoption: option,
            ..self.record
        };
        self.store_record(disc, boot_block, record)
    }

    /// The id of the fragment holding map block `block`: `None` for free
    /// space and for a block past the map's last zone.
    ///
    /// # Errors
    ///
    /// The disc's failure, or the map broken.
    pub fn id_at<D: Disc>(&self, disc: &mut D, block: u64) -> Result<Option<u32>, D> {
        let Some((zone, bit)) = self.place(block) else {
            return Ok(None);
        };
        let staged = self.load_zone(disc, zone)?;
        let mut walker = self.walker(&staged);
        while let Some(frag) = walker.next(&staged)? {
            if bit <= frag.end {
                return Ok((!frag.is_free).then_some(frag.id));
            }
        }
        Ok(None)
    }

    /// The zone describing map block `block`, and the bit there that does.
    fn place(&self, block: u64) -> Option<(u32, u32)> {
        (0..u32::from(self.record.nzones)).find_map(|zone| {
            let (start_bit, end_bit, start_block) = self.zone_geometry(zone);
            let into = block.checked_sub(start_block)?;
            let into = u32::try_from(into)
                .ok()
                .filter(|&into| into < end_bit - start_bit)?;
            Some((zone, start_bit + into))
        })
    }

    /// Map blocks `[block, block + blocks)` out of use as a defect, as RISC
    /// OS 5 maps one out of free space: widened to the least fragment,
    /// forward where the free space runs on and backward where it ends,
    /// unless it meets a defect, which it joins; and taking with it any
    /// sliver of free space left either side too small to stand alone.
    ///
    /// # Errors
    ///
    /// The disc's failure, or the map broken.
    pub fn map_out<D: Disc>(&self, disc: &mut D, block: u64, blocks: u64) -> Result<MappedOut, D> {
        let Some((zone, bit)) = self.place(block) else {
            return Ok(MappedOut::Already);
        };
        let mut staged = self.load_zone(disc, zone)?;
        let mut walker = self.walker(&staged);
        let mut before = None;
        let mut holder = None;
        while let Some(frag) = walker.next(&staged)? {
            if bit <= frag.end {
                holder = Some(frag);
                break;
            }
            before = Some(frag);
        }
        let Some(free) = holder.filter(|frag| frag.is_free) else {
            return Ok(match holder {
                Some(frag) if frag.id != FRAG_BAD => MappedOut::InUse,
                _ => MappedOut::Already,
            });
        };
        let after = walker.next(&staged)?;
        let is_defect = |frag: Option<Frag>| frag.is_some_and(|f| !f.is_free && f.id == FRAG_BAD);
        let fewest = self.record.least_fragment_bits();
        let stop = free.end + 1;
        let span = u32::try_from(blocks).unwrap_or(u32::MAX).max(1);
        let end = bit.saturating_add(span).min(stop);
        let (mut lo, mut hi) = (bit, end);
        let meets = (bit == free.bit && is_defect(before)) || (end == stop && is_defect(after));
        if !meets {
            // Whole sectors, as every allocation is.
            let want = span.max(fewest).next_multiple_of(span);
            if bit + want <= stop {
                hi = bit + want;
            } else {
                lo = end.saturating_sub(want).max(free.bit);
                hi = (lo + want).min(stop);
            }
        }
        if lo > free.bit && lo - free.bit < fewest {
            lo = free.bit;
        }
        if hi < stop && stop - hi < fewest {
            hi = stop;
        }
        let first = match before {
            Some(frag) if lo == free.bit && is_defect(before) => frag.bit,
            _ => lo,
        };
        let last = match after {
            Some(frag) if hi == stop && is_defect(after) => frag.end + 1,
            _ => hi,
        };
        self.take_from(&mut staged, &free, lo..hi, first..last, FRAG_BAD);
        self.store_zone(disc, zone, &mut staged)?;
        Ok(MappedOut::Done)
    }

    /// `taken` of the free fragment `free` made part of a fragment of `id`
    /// spanning `fragment`, which runs past `taken` only into neighbours it
    /// joins; what is left of `free` either side stays on the free list.
    fn take_from(
        &self,
        staged: &mut Zone,
        free: &Frag,
        taken: core::ops::Range<u32>,
        fragment: core::ops::Range<u32>,
        id: u32,
    ) {
        let link = staged.field(free.bit, self.link_bits());
        let next_free = if link == 0 { 0 } else { free.bit + link };
        let left = taken.start > free.bit;
        let right = taken.end <= free.end;
        self.write_allocated_fragment(staged, fragment.start, fragment.end - fragment.start, id);
        match (left, right) {
            (true, true) => {
                self.write_free_fragment(staged, free.bit, taken.start - 1, taken.end);
                self.write_free_fragment(staged, taken.end, free.end, next_free);
            }
            (true, false) => {
                self.write_free_fragment(staged, free.bit, taken.start - 1, next_free);
            }
            (false, true) => {
                self.write_free_fragment(staged, taken.end, free.end, next_free);
                self.set_pred_link(staged, free.prev_free, taken.end);
            }
            (false, false) => self.set_pred_link(staged, free.prev_free, next_free),
        }
    }

    /// Each zone's free space gathered together as RISC OS 5's `*Compact`
    /// gathers it: whether anything moved. Only a fragment of one piece
    /// moves, keeping its id, so no directory changes, and a directory never
    /// moves down. Until a zone's free space is one run, `H` its lowest: the
    /// topmost file between `H` and the last free run moves to `H`'s start
    /// where it fits leaving no sliver; else the file just above `H` slides down into it where it
    /// fits; else the fragment just above `H`, unless it is that topmost
    /// file, moves to the end of the zone's last free run where it fits
    /// there; else the zone is left. `directories` holds, ascending, every
    /// fragment id a directory lies in.
    ///
    /// # Errors
    ///
    /// [`Fault::NoMemory`] where the host has no room to list the map's
    /// fragments; the disc's failure, or the map broken.
    pub fn compact<D: Disc>(&self, disc: &mut D, directories: &[u32]) -> Result<bool, D> {
        let pieces = self.piece_counts(disc)?;
        let kind = |frag: &Frag| {
            let alone = pieces
                .binary_search_by_key(&frag.id, |&(id, _)| id)
                .is_ok_and(|at| pieces[at].1 == 1);
            (!frag.is_free && frag.id > FRAG_ROOT && alone).then(|| {
                if directories.binary_search(&frag.id).is_ok() {
                    Moves::Up
                } else {
                    Moves::Anywhere
                }
            })
        };
        let mut frags = Vec::new();
        let mut moved = false;
        for zone in 0..u32::from(self.record.nzones) {
            loop {
                let staged = self.load_zone(disc, zone)?;
                frags.clear();
                let mut walker = self.walker(&staged);
                while let Some(frag) = walker.next(&staged)? {
                    frags.try_reserve(1).map_err(|_| Fault::NoMemory)?;
                    frags.push(frag);
                }
                let Some((piece, to)) = self.compaction_move(&frags, &kind) else {
                    break;
                };
                self.relocate(disc, zone, staged, &piece, to)?;
                moved = true;
            }
        }
        Ok(moved)
    }

    /// The next move [`Self::compact`] makes in a zone of `frags`, in bit
    /// order: the fragment and the bit it moves to; none once the zone's
    /// free space is one run or nothing can move.
    fn compaction_move(
        &self,
        frags: &[Frag],
        kind: &impl Fn(&Frag) -> Option<Moves>,
    ) -> Option<(Frag, u32)> {
        let lowest = frags.iter().position(|frag| frag.is_free)?;
        let hole = frags[lowest];
        let last = *frags.iter().rev().find(|frag| frag.is_free)?;
        if last.bit == hole.bit {
            return None;
        }
        let size = |frag: &Frag| frag.end + 1 - frag.bit;
        let fewest = self.record.least_fragment_bits();
        let leaves = |room: u32, taking: u32, joins: bool| {
            taking <= room && (room == taking || room - taking >= fewest || joins)
        };
        // A file moved past the last free run has been moved for good.
        let topmost = frags[lowest + 1..]
            .iter()
            .rev()
            .find(|frag| frag.bit < last.bit && kind(frag) == Some(Moves::Anywhere))
            .copied();
        if let Some(top) = topmost {
            if leaves(size(&hole), size(&top), top.bit == hole.end + 1) {
                return Some((top, hole.bit));
            }
        }
        let next = *frags.get(lowest + 1)?;
        match kind(&next) {
            Some(Moves::Anywhere) if size(&next) <= size(&hole) => Some((next, hole.bit)),
            Some(_)
                if topmost.is_none_or(|top| top.bit != next.bit)
                    && leaves(size(&last), size(&next), next.end + 1 == last.bit) =>
            {
                Some((next, last.end + 1 - size(&next)))
            }
            _ => None,
        }
    }

    /// The one-piece fragment `piece` of zone `zone`, staged as `staged`,
    /// moved to start at bit `to` of free space: its bytes copied, then the
    /// map rewritten, its id kept.
    fn relocate<D: Disc>(
        &self,
        disc: &mut D,
        zone: u32,
        mut staged: Zone,
        piece: &Frag,
        to: u32,
    ) -> Result<(), D> {
        let bpmb = self.record.bytes_per_map_bit();
        let bits = piece.end + 1 - piece.bit;
        let byte = |bit: u32| (staged.start_block + u64::from(bit - staged.start_bit)) * bpmb;
        copy_bytes(disc, byte(piece.bit), byte(to), u64::from(bits) * bpmb)?;
        self.free_one(&mut staged, piece)?;
        let mut walker = self.walker(&staged);
        let free = loop {
            match walker.next(&staged)? {
                Some(frag) if frag.is_free && to >= frag.bit && to + bits <= frag.end + 1 => {
                    break frag;
                }
                Some(_) => {}
                None => return Err(BROKEN.into()),
            }
        };
        self.take_from(&mut staged, &free, to..to + bits, to..to + bits, piece.id);
        self.store_zone(disc, zone, &mut staged)
    }

    /// Each fragment id the map holds, ascending, with how many pieces it
    /// has.
    fn piece_counts<D: Disc>(&self, disc: &mut D) -> Result<Vec<(u32, u32)>, D> {
        let mut ids = Vec::new();
        for zone in 0..u32::from(self.record.nzones) {
            let staged = self.load_zone(disc, zone)?;
            let mut walker = self.walker(&staged);
            while let Some(frag) = walker.next(&staged)? {
                if !frag.is_free {
                    ids.try_reserve(1).map_err(|_| Fault::NoMemory)?;
                    ids.push(frag.id);
                }
            }
        }
        ids.sort_unstable();
        let mut counts: Vec<(u32, u32)> = Vec::new();
        for id in ids {
            match counts.last_mut() {
                Some((held, pieces)) if *held == id => *pieces += 1,
                _ => {
                    counts.try_reserve(1).map_err(|_| Fault::NoMemory)?;
                    counts.push((id, 1));
                }
            }
        }
        Ok(counts)
    }

    /// Each defect the map holds, as its first map block and how many it
    /// spans, in disc order, until `visit` declines another.
    ///
    /// # Errors
    ///
    /// The disc's failure, or the map broken.
    pub fn defects<D: Disc>(
        &self,
        disc: &mut D,
        mut visit: impl FnMut(u64, u64) -> bool,
    ) -> Result<(), D> {
        for zone in 0..u32::from(self.record.nzones) {
            let staged = self.load_zone(disc, zone)?;
            let mut walker = self.walker(&staged);
            while let Some(frag) = walker.next(&staged)? {
                if !frag.is_free && frag.id == FRAG_BAD {
                    let block = staged.start_block + u64::from(frag.bit - staged.start_bit);
                    if !visit(block, u64::from(frag.end + 1 - frag.bit)) {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    /// Rewrite the disc record's disc name, as [`Self::set_root_size`]
    /// rewrites its root size.
    ///
    /// # Errors
    ///
    /// * The disc's failure.
    /// * The map broken if zone 0 no longer validates.
    pub fn set_disc_name<D: Disc>(
        &mut self,
        disc: &mut D,
        boot_block: bool,
        name: [u8; NAME_LEN],
    ) -> Result<(), D> {
        let record = DiscRecord {
            disc_name: name,
            ..self.record
        };
        self.store_record(disc, boot_block, record)
    }

    /// Write `record` into zone 0 of both copies, its check byte refreshed,
    /// and, when `boot_block`, into the boot block, resealed, and hold it as
    /// the map's once it is written: a write that fails leaves the map
    /// holding the record it had.
    fn store_record<D: Disc>(
        &mut self,
        disc: &mut D,
        boot_block: bool,
        record: DiscRecord,
    ) -> Result<(), D> {
        let mut staged = self.load_zone(disc, 0)?;
        let encoded = record.encode();
        staged.bytes[ZONE_HEADER_SIZE..ZONE_HEADER_SIZE + DISC_RECORD_SIZE]
            .copy_from_slice(&encoded);
        self.store_zone(disc, 0, &mut staged)?;
        if boot_block {
            let mut block = [0u8; BOOT_BLOCK_SIZE];
            disc.read(BOOT_BLOCK_OFFSET, &mut block)
                .map_err(Error::Disc)?;
            seal_boot_block(&mut block, &record);
            disc.write(BOOT_BLOCK_OFFSET, &block).map_err(Error::Disc)?;
        }
        self.record = record;
        Ok(())
    }

    /// Find an unused fragment id homed in `zone`, scanning the whole
    /// map for ids already in use there.
    ///
    /// # Errors
    ///
    /// * The disc's failure, or the map broken, from
    ///   the underlying zone reads.
    fn unused_id_in_zone<D: Disc>(&self, disc: &mut D, zone: u32) -> Result<Option<u32>, D> {
        let ids_per_zone = self.record.ids_per_zone();
        let first = zone * ids_per_zone;
        let mut used = [0u8; (MAX_ZONE_BYTES * 8).div_ceil(8)];
        for scan in 0..u32::from(self.record.nzones) {
            let staged = self.load_zone(disc, scan)?;
            let mut walker = self.walker(&staged);
            while let Some(frag) = walker.next(&staged)? {
                if !frag.is_free && frag.id >= first && frag.id < first + ids_per_zone {
                    let rel = (frag.id - first) as usize;
                    used[rel / 8] |= 1 << (rel % 8);
                }
            }
        }
        let id_mask = (1u32 << self.record.idlen) - 1;
        for rel in 0..ids_per_zone {
            let id = first + rel;
            // Ids 0-2 are reserved (free space, defects, the root), and
            // every id must fit the idlen field.
            if id <= FRAG_ROOT || id > id_mask {
                continue;
            }
            if used[(rel / 8) as usize] & (1 << (rel % 8)) == 0 {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Allocate a fresh object of `bytes`, from zone `home` on, fragmenting
    /// across zones where no single free fragment is large enough, and
    /// return its indirect disc address (share offset 0).
    ///
    /// # Errors
    ///
    /// * [`Fault::DiscFull`] if the disc lacks the space, [`Fault::MapFull`]
    ///   if it has no free fragment id.
    /// * The disc's failure, or the map broken, from
    ///   the underlying zone access.
    pub fn allocate_object<D: Disc>(&self, disc: &mut D, bytes: u64, home: u32) -> Result<u32, D> {
        let need = self.record.allocation_bits(bytes);
        let nzones = u32::from(self.record.nzones);
        for zone in (0..nzones).map(|i| (home + i) % nzones) {
            let Some(frag_id) = self.unused_id_in_zone(disc, zone)? else {
                continue;
            };
            return match self.take_fragments(disc, frag_id, zone, 0, nzones, need) {
                Ok(()) => Ok(frag_id << 8),
                Err(Error::Fault(Fault::DiscFull)) => {
                    // Reclaim whatever the partial walk allocated; every
                    // taken bit carries the id, so the cleanup is total.
                    match self.free_object(disc, frag_id) {
                        Ok(()) | Err(Error::Fault(Fault::NoFragment)) => {
                            Err(Fault::DiscFull.into())
                        }
                        Err(err) => Err(err),
                    }
                }
                Err(err) => Err(err),
            };
        }
        Err(Fault::MapFull.into())
    }

    /// Free every fragment of `frag_id`, merging with neighbouring free
    /// fragments and keeping each zone's free list sorted.
    ///
    /// # Errors
    ///
    /// * [`Fault::NoFragment`] if the map carries no such fragment.
    /// * The directory broken for a fragment no object owns.
    /// * The map broken on map corruption.
    /// * The disc's failure.
    pub fn free_object<D: Disc>(&self, disc: &mut D, frag_id: u32) -> Result<(), D> {
        own_fragment(frag_id)?;
        let mut freed_any = false;
        for zone in 0..u32::from(self.record.nzones) {
            // Convert one fragment per pass; rescan until none remain.
            loop {
                let mut staged = self.load_zone(disc, zone)?;
                let mut walker = self.walker(&staged);
                let mut target = None;
                while let Some(frag) = walker.next(&staged)? {
                    if !frag.is_free && frag.id == frag_id {
                        target = Some(frag);
                        break;
                    }
                }
                let Some(frag) = target else { break };
                self.free_one(&mut staged, &frag)?;
                self.store_zone(disc, zone, &mut staged)?;
                freed_any = true;
            }
        }
        if freed_any {
            Ok(())
        } else {
            Err(Fault::NoFragment.into())
        }
    }

    /// Convert the allocated fragment `frag` to free space in `staged`,
    /// merging with the free fragments either side where adjacent.
    fn free_one(&self, staged: &mut Zone, frag: &Frag) -> core::result::Result<(), Fault> {
        // Find the free fragments either side of `frag` in the list
        // (which is kept sorted by position).
        let mut walker = self.walker(staged);
        let mut pred: Option<Frag> = None;
        let mut succ: Option<Frag> = None;
        while let Some(other) = walker.next(staged)? {
            if !other.is_free {
                continue;
            }
            if other.bit < frag.bit {
                pred = Some(other);
            } else {
                succ = Some(other);
                break;
            }
        }
        let merge_prev = pred.as_ref().is_some_and(|p| p.end + 1 == frag.bit);
        let merge_next = succ.as_ref().is_some_and(|s| s.bit == frag.end + 1);
        let bit = match (&pred, merge_prev) {
            (Some(p), true) => p.bit,
            _ => frag.bit,
        };
        let (end, next) = match (&succ, merge_next) {
            (Some(s), true) => {
                // Absorb the following free fragment, inheriting its link.
                let link = staged.field(s.bit, self.link_bits());
                (s.end, if link == 0 { 0 } else { s.bit + link })
            }
            (Some(s), false) => (frag.end, s.bit),
            (None, _) => (frag.end, 0),
        };
        // The link that must point at the merged fragment: the
        // predecessor of whatever fragment now starts the span.
        let pred_link = if merge_prev {
            pred.as_ref().and_then(|p| p.prev_free)
        } else {
            pred.as_ref().map(|p| p.bit)
        };
        self.write_free_fragment(staged, bit, end, next);
        self.set_pred_link(staged, pred_link, bit);
        Ok(())
    }

    /// Zone-stream geometry for `zone`: where its stream starts and ends in
    /// the sector, and the first disc block it describes. The last zone's
    /// stream runs on past the disc, through the defect that covers what
    /// lies beyond it, which the defect at the disc's end may join.
    fn zone_geometry(&self, zone: u32) -> (u32, u32, u64) {
        let start_bit = zone_start_bit(zone);
        let stream = self.record.zone_stream_bits(zone);
        (
            start_bit,
            start_bit + stream,
            self.record.zone_start_block(zone),
        )
    }

    /// Load and verify zone `zone` of the copy the engine reads.
    fn load_zone<D: Disc>(&self, disc: &mut D, zone: u32) -> Result<Zone, D> {
        self.load_zone_of(disc, self.reads, zone)
    }

    /// Load and verify zone `zone` of `copy`.
    fn load_zone_of<D: Disc>(&self, disc: &mut D, copy: MapCopy, zone: u32) -> Result<Zone, D> {
        // The sector size was validated against `MAX_ZONE_BYTES`.
        let sector = usize::try_from(self.record.sector_size()).unwrap_or(MAX_ZONE_BYTES);
        let mut bytes = [0u8; MAX_ZONE_BYTES];
        disc.read(self.record.zone_offset(copy, zone), &mut bytes[..sector])
            .map_err(Error::Disc)?;
        let (start_bit, end_bit, start_block) = self.zone_geometry(zone);
        let staged = Zone {
            bytes,
            len: sector,
            start_bit,
            end_bit,
            start_block,
        };
        // A zeroed sector checks, yet every genuine zone holds a fragment's
        // stop bit: a blanked copy is no copy.
        if zone_check(staged.bits()) != staged.bytes[ZONE_CHECK_BYTE]
            || staged.bits().iter().all(|&byte| byte == 0)
        {
            return Err(BROKEN.into());
        }
        Ok(staged)
    }

    /// Write zone `zone` back to both copies, refreshing its check byte, so
    /// either stands for the map as `FileCore` expects.
    fn store_zone<D: Disc>(&self, disc: &mut D, zone: u32, staged: &mut Zone) -> Result<(), D> {
        staged.bytes[ZONE_CHECK_BYTE] = zone_check(&staged.bytes[..staged.len]);
        for copy in [MapCopy::First, MapCopy::Second] {
            disc.write(
                self.record.zone_offset(copy, zone),
                &staged.bytes[..staged.len],
            )
            .map_err(Error::Disc)?;
        }
        Ok(())
    }
}
