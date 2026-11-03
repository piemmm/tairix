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

use tairix_abi::driver::block::Block;
use tairix_abi::DriverError;
use tairix_bytedev::ByteDevice;
use tairix_fsprobe::adfs::{
    link_bits, seal_boot_block, zone_check, zone_start_bit, DiscRecord, MapCopy, BOOT_BLOCK_OFFSET,
    BOOT_BLOCK_SIZE, CROSS_CHECK_BYTE, CROSS_CHECK_SUM, DISC_RECORD_SIZE, FRAG_ROOT, FREELINK_BIT,
    ZONE_CHECK_BYTE, ZONE_HEADER_SIZE,
};

/// Largest map sector (zone) size: `log2secsize` is at most 10.
pub const MAX_ZONE_BYTES: usize = 1024;

/// Most zones a volume may carry. This bounds the driver's zone-scan
/// loops against a hostile disc record; a genuine `FileCore` volume of
/// the maximum 2 TiB size stays far below it.
pub const MAX_ZONES: usize = 512;

/// The new-map allocation engine.
///
/// The engine re-reads and re-writes one zone sector at a time through
/// an on-stack buffer, so its memory footprint is one zone regardless of
/// volume size.
pub struct NewMap {
    /// The validated disc record (from zone 0 or the boot block).
    pub record: DiscRecord,
    /// The copy the engine reads: the first whose every zone checks.
    reads: MapCopy,
    /// The other copy, when it checks too but records something else: an
    /// object the copy read lacks is looked for there, and the volume is
    /// never written, which could allocate over what only it records.
    recovers: Option<MapCopy>,
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
    /// [`DriverError::BadMagic`] if a fragment runs past the zone end
    /// (no stop bit before the map boundary).
    fn next(&mut self, staged: &Zone) -> Result<Option<Frag>, DriverError> {
        if self.bit >= staged.end_bit {
            return Ok(None);
        }
        let bit = self.bit;
        let id = staged.field(bit, self.idlen);
        let end = staged
            .next_set_bit(bit + self.idlen, staged.end_bit)
            .ok_or(DriverError::BadMagic)?;
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
    /// Attach to a volume whose validated disc record is `record`, reading
    /// the first map copy whose every zone checks and whose cross-check holds,
    /// as `FileCore` does.
    ///
    /// # Errors
    ///
    /// * [`DriverError::BadMagic`] if the record is inconsistent or leaves
    ///   no room for both copies, or neither copy checks.
    /// * [`DriverError::LengthOutOfRange`] for a zone of the copy read past
    ///   the end of a short device.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block read.
    pub fn open<B: Block>(
        volume: &mut ByteDevice<B>,
        record: DiscRecord,
    ) -> Result<Self, DriverError> {
        record.validate()?;
        if usize::from(record.nzones) > MAX_ZONES || record.sector_size() > MAX_ZONE_BYTES as u64 {
            return Err(DriverError::BadMagic);
        }
        let maps_end = record
            .map_size()
            .checked_mul(2)
            .and_then(|both| both.checked_add(record.map_offset()))
            .ok_or(DriverError::BadMagic)?;
        if maps_end > record.disc_size {
            return Err(DriverError::BadMagic);
        }
        let mut map = Self {
            record,
            reads: MapCopy::First,
            recovers: None,
        };
        match map.copy_checks(volume, MapCopy::First) {
            Ok(()) => {
                if map.copy_checks(volume, MapCopy::Second).is_ok() && !map.copies_agree(volume)? {
                    map.recovers = Some(MapCopy::Second);
                }
            }
            Err(DriverError::BadMagic) => {
                map.copy_checks(volume, MapCopy::Second)?;
                map.reads = MapCopy::Second;
            }
            Err(err) => return Err(err),
        }
        Ok(map)
    }

    /// Whether both copies check yet record different maps, which leaves
    /// the volume read-only.
    #[must_use]
    pub fn copies_disagree(&self) -> bool {
        self.recovers.is_some()
    }

    /// Check every zone of `copy` and its map-wide cross-check.
    fn copy_checks<B: Block>(
        &self,
        volume: &mut ByteDevice<B>,
        copy: MapCopy,
    ) -> Result<(), DriverError> {
        let mut crosscheck = 0u8;
        for zone in 0..u32::from(self.record.nzones) {
            crosscheck ^= self.load_zone_of(volume, copy, zone)?.bytes[CROSS_CHECK_BYTE];
        }
        if crosscheck != CROSS_CHECK_SUM {
            return Err(DriverError::BadMagic);
        }
        Ok(())
    }

    /// Whether the two copies, both of which check, hold the same bytes.
    fn copies_agree<B: Block>(&self, volume: &mut ByteDevice<B>) -> Result<bool, DriverError> {
        for zone in 0..u32::from(self.record.nzones) {
            let first = self.load_zone_of(volume, MapCopy::First, zone)?;
            let second = self.load_zone_of(volume, MapCopy::Second, zone)?;
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

    /// Home zone of `frag_id` (the root fragment lives in the middle
    /// zone alongside the map).
    fn home_zone(&self, frag_id: u32) -> Result<u32, DriverError> {
        let zone = if frag_id == FRAG_ROOT {
            u32::from(self.record.nzones) >> 1
        } else {
            frag_id / self.record.ids_per_zone()
        };
        if zone >= u32::from(self.record.nzones) {
            return Err(DriverError::BadMagic);
        }
        Ok(zone)
    }

    /// Locate the fragment run of `frag_id` holding map-bit offset
    /// `map_offset` within the object, returning the run's disc byte
    /// address and remaining length from that offset.
    fn lookup_run<B: Block>(
        &self,
        volume: &mut ByteDevice<B>,
        frag_id: u32,
        map_offset: u64,
    ) -> Result<FragmentRun, DriverError> {
        let nzones = u32::from(self.record.nzones);
        for copy in self.copies() {
            let mut offset = map_offset;
            let mut seen = false;
            let mut zone = self.home_zone(frag_id)?;
            for _ in 0..nzones {
                let staged = self.load_zone_of(volume, copy, zone)?;
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
        Err(DriverError::BadMagic)
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
    ) -> Result<Option<FragmentRun>, DriverError> {
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
                }));
            }
            *map_offset -= length;
        }
        Ok(None)
    }

    fn walker(&self, staged: &Zone) -> Walker {
        Walker::new(staged, u32::from(self.record.idlen), self.link_bits())
    }

    /// Smallest legal fragment size in map bits.
    fn min_frag_bits(&self) -> u32 {
        u32::from(self.record.idlen) + 1
    }

    /// Map bits needed to hold `bytes` (at least one whole fragment).
    fn bits_for(&self, bytes: u64) -> u64 {
        let bpmb = self.record.bytes_per_map_bit();
        bytes.div_ceil(bpmb).max(u64::from(self.min_frag_bits()))
    }

    /// Locate the run backing byte `byte_offset` of the object at
    /// `indaddr`, returning `(disc_byte, run_bytes_available)`.
    ///
    /// The low byte of `indaddr` is the share offset: `N` places the
    /// object `(N - 1) << log2sharesize` sectors into its fragment.
    ///
    /// # Errors
    ///
    /// * [`DriverError::BadMagic`] if the fragment id is unknown, the
    ///   offset is past the object's allocation, or the map is corrupt.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block read.
    pub fn locate<B: Block>(
        &self,
        volume: &mut ByteDevice<B>,
        indaddr: u32,
        byte_offset: u64,
    ) -> Result<(u64, u64), DriverError> {
        let frag_id = indaddr >> 8;
        let share = indaddr & 0xFF;
        let share_bytes = if share == 0 {
            0
        } else {
            (u64::from(share) - 1)
                << (u32::from(self.record.log2sharesize) + u32::from(self.record.log2secsize))
        };
        let total = share_bytes
            .checked_add(byte_offset)
            .ok_or(DriverError::BadMagic)?;
        let bpmb = self.record.bytes_per_map_bit();
        let sub = total % bpmb;
        let run = self.lookup_run(volume, frag_id, total / bpmb)?;
        Ok((run.disc_byte + sub, run.len - sub))
    }

    /// Total bytes of map allocation carried by fragment id `frag_id`, in
    /// the first copy that records it.
    pub fn object_allocated_bytes<B: Block>(
        &self,
        volume: &mut ByteDevice<B>,
        frag_id: u32,
    ) -> Result<u64, DriverError> {
        for copy in self.copies() {
            let mut bits = 0u64;
            for zone in 0..u32::from(self.record.nzones) {
                let staged = self.load_zone_of(volume, copy, zone)?;
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

    /// Total free bytes on the volume.
    pub fn free_bytes<B: Block>(&self, volume: &mut ByteDevice<B>) -> Result<u64, DriverError> {
        let mut bits = 0u64;
        for zone in 0..u32::from(self.record.nzones) {
            let staged = self.load_zone(volume, zone)?;
            let mut walker = self.walker(&staged);
            while let Some(frag) = walker.next(&staged)? {
                if frag.is_free {
                    bits += u64::from(frag.end + 1 - frag.bit);
                }
            }
        }
        Ok(bits * self.record.bytes_per_map_bit())
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
        need = need.max(self.min_frag_bits()).min(total);
        if total - need < self.min_frag_bits() {
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
    fn take_fragments<B: Block>(
        &self,
        volume: &mut ByteDevice<B>,
        frag_id: u32,
        home: u32,
        first_min_bit: u32,
        zones_to_scan: u32,
        total_bits: u64,
    ) -> Result<(), DriverError> {
        let nzones = u32::from(self.record.nzones);
        let mut remaining = total_bits;
        for i in 0..zones_to_scan.min(nzones) {
            let zone = (home + i) % nzones;
            let min_bit = if i == 0 { first_min_bit } else { 0 };
            loop {
                if remaining == 0 {
                    return Ok(());
                }
                let mut staged = self.load_zone(volume, zone)?;
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
                self.store_zone(volume, zone, &mut staged)?;
                remaining = remaining.saturating_sub(u64::from(taken));
            }
        }
        if remaining == 0 {
            Ok(())
        } else {
            Err(DriverError::NoSpace)
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
    /// * [`DriverError::NotFound`] if the map carries no such fragment.
    /// * [`DriverError::BadMagic`] on map corruption.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block access.
    pub fn extend_object<B: Block>(
        &self,
        volume: &mut ByteDevice<B>,
        frag_id: u32,
        new_bytes: u64,
    ) -> Result<bool, DriverError> {
        let bpmb = self.record.bytes_per_map_bit();
        let needed_bits = new_bytes
            .div_ceil(bpmb)
            .max(u64::from(self.min_frag_bits()));
        let nzones = u32::from(self.record.nzones);
        let home = self.home_zone(frag_id)?;
        // Find the object's last fragment in scan order, and its total.
        let mut current_bits = 0u64;
        let mut last: Option<(u32, u32, Frag)> = None;
        for i in 0..nzones {
            let zone = (home + i) % nzones;
            let staged = self.load_zone(volume, zone)?;
            let mut walker = self.walker(&staged);
            while let Some(frag) = walker.next(&staged)? {
                if !frag.is_free && frag.id == frag_id {
                    current_bits += u64::from(frag.end + 1 - frag.bit);
                    last = Some((i, zone, frag));
                }
            }
        }
        let Some((last_order, last_zone, last_frag)) = last else {
            return Err(DriverError::NotFound);
        };
        if current_bits >= needed_bits {
            return Ok(true);
        }
        let mut remaining = needed_bits - current_bits;
        // Absorb free space directly following the last fragment.
        let mut staged = self.load_zone(volume, last_zone)?;
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
            if size - take < self.min_frag_bits() {
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
            self.store_zone(volume, last_zone, &mut staged)?;
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
        match self.take_fragments(
            volume, frag_id, last_zone, resume_bit, zones_left, remaining,
        ) {
            Ok(()) => Ok(true),
            Err(DriverError::NoSpace) => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// Shrink the object `frag_id` so that it keeps only `new_bytes` of
    /// allocation: the boundary fragment is trimmed (its remainder
    /// joining the free list) and every later fragment is freed.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] if the map carries no such fragment.
    /// * [`DriverError::BadMagic`] on map corruption.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block access.
    pub fn shrink_object<B: Block>(
        &self,
        volume: &mut ByteDevice<B>,
        frag_id: u32,
        new_bytes: u64,
    ) -> Result<(), DriverError> {
        let needed = self.bits_for(new_bytes);
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
                let mut staged = self.load_zone(volume, zone)?;
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
                    self.store_zone(volume, zone, &mut staged)?;
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
                let keep = keep_wanted.max(self.min_frag_bits());
                if u64::from(keep) >= len || len - u64::from(keep) < u64::from(self.min_frag_bits())
                {
                    // The tail cannot stand alone as a free fragment;
                    // the slack stays allocated.
                    acc += len;
                    cursor = frag.end + 1;
                    continue;
                }
                self.trim_allocated(&mut staged, &frag, keep)?;
                self.store_zone(volume, zone, &mut staged)?;
                acc = needed;
                cursor = frag.bit + keep;
            }
        }
        if seen {
            Ok(())
        } else {
            Err(DriverError::NotFound)
        }
    }

    /// Shorten the allocated fragment `frag` to `keep` bits, turning the
    /// remainder into free space (merged with a directly following free
    /// fragment where present).
    fn trim_allocated(&self, staged: &mut Zone, frag: &Frag, keep: u32) -> Result<(), DriverError> {
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
    /// grow), refreshing zone 0's check byte and, when the volume
    /// carries one, the boot block copy.
    ///
    /// # Errors
    ///
    /// * [`DriverError::DeviceFault`] on an unrecoverable block access.
    /// * [`DriverError::BadMagic`] if zone 0 no longer validates.
    pub fn set_root_size<B: Block>(
        &mut self,
        volume: &mut ByteDevice<B>,
        boot_block: bool,
        new_size: u32,
    ) -> Result<(), DriverError> {
        self.record.root_size = new_size;
        let mut staged = self.load_zone(volume, 0)?;
        let encoded = self.record.encode();
        staged.bytes[ZONE_HEADER_SIZE..ZONE_HEADER_SIZE + DISC_RECORD_SIZE]
            .copy_from_slice(&encoded);
        self.store_zone(volume, 0, &mut staged)?;
        if boot_block {
            let mut block = [0u8; BOOT_BLOCK_SIZE];
            volume.read(BOOT_BLOCK_OFFSET, &mut block)?;
            seal_boot_block(&mut block, &self.record);
            volume.write(BOOT_BLOCK_OFFSET, &block)?;
        }
        Ok(())
    }

    /// Find an unused fragment id homed in `zone`, scanning the whole
    /// map for ids already in use there.
    ///
    /// # Errors
    ///
    /// * [`DriverError::DeviceFault`] / [`DriverError::BadMagic`] from
    ///   the underlying zone reads.
    fn unused_id_in_zone<B: Block>(
        &self,
        volume: &mut ByteDevice<B>,
        zone: u32,
    ) -> Result<Option<u32>, DriverError> {
        let ids_per_zone = self.record.ids_per_zone();
        let first = zone * ids_per_zone;
        let mut used = [0u8; (MAX_ZONE_BYTES * 8).div_ceil(8)];
        for scan in 0..u32::from(self.record.nzones) {
            let staged = self.load_zone(volume, scan)?;
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

    /// Allocate a fresh object of `bytes`, fragmenting across zones
    /// where no single free fragment is large enough, and return its
    /// indirect disc address (share offset 0).
    ///
    /// # Errors
    ///
    /// * [`DriverError::NoSpace`] if the volume lacks the space or a
    ///   free fragment id.
    /// * [`DriverError::DeviceFault`] / [`DriverError::BadMagic`] from
    ///   the underlying zone access.
    pub fn allocate_object<B: Block>(
        &self,
        volume: &mut ByteDevice<B>,
        bytes: u64,
    ) -> Result<u32, DriverError> {
        let need = self.bits_for(bytes);
        let nzones = u32::from(self.record.nzones);
        for zone in 0..nzones {
            let Some(frag_id) = self.unused_id_in_zone(volume, zone)? else {
                continue;
            };
            return match self.take_fragments(volume, frag_id, zone, 0, nzones, need) {
                Ok(()) => Ok(frag_id << 8),
                Err(DriverError::NoSpace) => {
                    // Reclaim whatever the partial walk allocated; every
                    // taken bit carries the id, so the cleanup is total.
                    match self.free_object(volume, frag_id) {
                        Ok(()) | Err(DriverError::NotFound) => Err(DriverError::NoSpace),
                        Err(err) => Err(err),
                    }
                }
                Err(err) => Err(err),
            };
        }
        Err(DriverError::NoSpace)
    }

    /// Free every fragment of `frag_id`, merging with neighbouring free
    /// fragments and keeping each zone's free list sorted.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] if the map carries no such fragment.
    /// * [`DriverError::BadMagic`] on map corruption.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block access.
    pub fn free_object<B: Block>(
        &self,
        volume: &mut ByteDevice<B>,
        frag_id: u32,
    ) -> Result<(), DriverError> {
        let mut freed_any = false;
        for zone in 0..u32::from(self.record.nzones) {
            // Convert one fragment per pass; rescan until none remain.
            loop {
                let mut staged = self.load_zone(volume, zone)?;
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
                self.store_zone(volume, zone, &mut staged)?;
                freed_any = true;
            }
        }
        if freed_any {
            Ok(())
        } else {
            Err(DriverError::NotFound)
        }
    }

    /// Convert the allocated fragment `frag` to free space in `staged`,
    /// merging with the free fragments either side where adjacent.
    fn free_one(&self, staged: &mut Zone, frag: &Frag) -> Result<(), DriverError> {
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
    /// the sector, and the first disc block it describes.
    fn zone_geometry(&self, zone: u32) -> (u32, u32, u64) {
        let start_bit = zone_start_bit(zone);
        let start_block = self.record.zone_start_block(zone);
        let mut stream = self.record.zone_stream_bits(zone);
        // The last zone covers only what remains of the disc.
        if zone == u32::from(self.record.nzones) - 1 {
            let remaining =
                (self.record.disc_size >> self.record.log2bpmb).saturating_sub(start_block);
            stream = u32::try_from(remaining).map_or(stream, |remaining| remaining.min(stream));
        }
        (start_bit, start_bit + stream, start_block)
    }

    /// Load and verify zone `zone` of the copy the engine reads.
    fn load_zone<B: Block>(
        &self,
        volume: &mut ByteDevice<B>,
        zone: u32,
    ) -> Result<Zone, DriverError> {
        self.load_zone_of(volume, self.reads, zone)
    }

    /// Load and verify zone `zone` of `copy`.
    fn load_zone_of<B: Block>(
        &self,
        volume: &mut ByteDevice<B>,
        copy: MapCopy,
        zone: u32,
    ) -> Result<Zone, DriverError> {
        // The sector size was validated against `MAX_ZONE_BYTES`.
        let sector = usize::try_from(self.record.sector_size()).unwrap_or(MAX_ZONE_BYTES);
        let mut bytes = [0u8; MAX_ZONE_BYTES];
        volume.read(self.record.zone_offset(copy, zone), &mut bytes[..sector])?;
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
            return Err(DriverError::BadMagic);
        }
        Ok(staged)
    }

    /// Write zone `zone` back to both copies, refreshing its check byte, so
    /// either stands for the map as `FileCore` expects.
    fn store_zone<B: Block>(
        &self,
        volume: &mut ByteDevice<B>,
        zone: u32,
        staged: &mut Zone,
    ) -> Result<(), DriverError> {
        staged.bytes[ZONE_CHECK_BYTE] = zone_check(&staged.bytes[..staged.len]);
        for copy in [MapCopy::First, MapCopy::Second] {
            volume.write(
                self.record.zone_offset(copy, zone),
                &staged.bytes[..staged.len],
            )?;
        }
        Ok(())
    }
}
