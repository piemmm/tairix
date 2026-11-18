//! A new disc's layout for every `FileCore` format: the free-space map (old
//! or new), the boot block where the format carries one, and the root
//! directory in the format's directory form.

use crate::bigdir::{BigDir, DirStore, BIG_DIR_GRAIN};
use crate::dir::{FixedDir, FixedFormat, NEW_DIR_SIZE, NEW_DIR_SIZE_U32};
use crate::newmap::{bits_set, bits_set_field, MAX_ZONE_BYTES};
use crate::oldmap::OldMap;
use crate::record::{
    link_bits, seal_boot_block, zone_check, zone_start_bit, DiscRecord, MapCopy, BOOT_BLOCK_OFFSET,
    BOOT_BLOCK_SIZE, CROSS_CHECK_BYTE, CROSS_CHECK_SUM, DIR_MARKERS, DISC_RECORD_SIZE, FRAG_BAD,
    FRAG_ROOT, FREELINK_BIT, LINK_BITS_MAX, LOG2BPMB, NAME_LEN, OLD_SECTOR_SIZE, ZONE_CHECK_BYTE,
    ZONE_HEADER_SIZE,
};
use crate::{Disc, Error, Fault, Result, Structure};

/// Disc id stamped into freshly formatted volumes.
const FORMAT_DISC_ID: u16 = 0x5253; // "RS"

/// The map bit RISC OS 5's RAM disc lays out with: 128 bytes.
const RAM_DISC_LOG2BPMB: u8 = 7;

/// The largest share unit a record gives, in log2 sectors: its four bits.
const MOST_LOG2SHARESIZE: u8 = 15;

/// Byte length of the boot area reserved at the start of a multi-zone
/// disc (the boot block sits inside it at `0xC00`).
const BOOT_AREA_BYTES: u64 = 0x1000;

/// The ADFS on-disc format variants the driver formats and mounts.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Variant {
    /// 160 KiB, old map, 1280-byte `Hugo` directories.
    S,
    /// 320 KiB, old map, 1280-byte `Hugo` directories.
    M,
    /// 640 KiB, old map, 1280-byte `Hugo` directories.
    L,
    /// 800 KiB, old map, 2048-byte directories.
    D,
    /// 800 KiB, new map, 2048-byte directories.
    E,
    /// 800 KiB, new map, big directories (E+).
    EPlus,
    /// 1600 KiB, new map with boot block, 2048-byte directories.
    F,
    /// 1600 KiB, new map with boot block, big directories (F+).
    FPlus,
    /// New-map hard disc (boot block; size taken from the device),
    /// 2048-byte directories.
    HardDisc,
    /// New-map hard disc with big directories.
    HardDiscPlus,
    /// RISC OS 5's RAM disc: 1 KiB sectors, 128-byte map bits, big
    /// directories, sized by the device, its boot block keeping no identity.
    RamDisc,
}

impl Variant {
    /// Total disc size in bytes, or `None` for a hard disc (sized by
    /// the device).
    #[must_use]
    pub fn fixed_size(self) -> Option<u64> {
        match self {
            Self::S => Some(160 * 1024),
            Self::M => Some(320 * 1024),
            Self::L => Some(640 * 1024),
            Self::D | Self::E | Self::EPlus => Some(800 * 1024),
            Self::F | Self::FPlus => Some(1600 * 1024),
            Self::HardDisc | Self::HardDiscPlus | Self::RamDisc => None,
        }
    }

    /// Whether the variant uses the old (sector 0–1) free-space map.
    #[must_use]
    pub fn is_old_map(self) -> bool {
        matches!(self, Self::S | Self::M | Self::L | Self::D)
    }

    /// Whether the variant uses big (E+/F+) directories.
    #[must_use]
    pub fn is_big_dir(self) -> bool {
        matches!(
            self,
            Self::EPlus | Self::FPlus | Self::HardDiscPlus | Self::RamDisc
        )
    }
}

/// Build the disc record describing a fresh new-map disc of this
/// variant (`device_bytes` sizes a hard disc).
///
/// Floppy geometry uses the authentic `FileCore` parameters, under
/// which every zone's coverage fits the disc exactly (E) or leaves a
/// tail that is marked as a defect fragment (F). A hard disc derives
/// its zone count from the device size.
///
/// # Errors
///
/// * [`Fault::Unsupported`] for an old-map variant.
/// * [`Fault::DiscFull`] if the disc is too small for the
///   variant.
pub fn new_map_record(
    variant: Variant,
    device_bytes: u64,
) -> core::result::Result<DiscRecord, Fault> {
    let format_version = u32::from(variant.is_big_dir());
    let mut record = DiscRecord {
        log2secsize: 10,
        secspertrack: 5,
        heads: 2,
        density: 2,
        idlen: 15,
        log2bpmb: 7,
        skew: 1,
        bootoption: 0,
        lowsector: 0,
        nzones: 1,
        zone_spare: 0x520,
        root: 0,
        disc_size: 0,
        disc_id: 0,
        disc_name: *b"TAIRiX    ",
        disc_type: 0,
        log2sharesize: 0,
        big_flag: false,
        format_version,
        root_size: if format_version != 0 {
            BIG_DIR_GRAIN
        } else {
            0
        },
    };
    match variant {
        Variant::E | Variant::EPlus => {}
        Variant::F | Variant::FPlus => {
            record.secspertrack = 10;
            record.density = 4;
            record.log2bpmb = 6;
            record.nzones = 4;
            record.zone_spare = 0x640;
        }
        Variant::HardDisc | Variant::HardDiscPlus => {
            record.secspertrack = 63;
            record.heads = 16;
            record.density = 0;
            record.log2secsize = 9;
            record.zone_spare = 32;
            record.disc_size = device_bytes & !(record.sector_size() - 1);
            // The smallest map bit, no smaller than a sector, whose map numbers
            // every fragment the disc can hold; a device larger than any map
            // describes holds a disc as large as the largest does.
            let fitted = (record.log2secsize..=*LOG2BPMB.end()).find_map(|log2bpmb| {
                let mut fitted = DiscRecord { log2bpmb, ..record };
                fitted.fit_zones().ok().map(|()| fitted)
            });
            record = if let Some(fitted) = fitted {
                fitted
            } else {
                let mut largest = DiscRecord {
                    log2bpmb: *LOG2BPMB.end(),
                    ..record
                };
                largest.disc_size =
                    largest.most_described().min(largest.disc_size) & !(largest.sector_size() - 1);
                largest.fit_zones()?;
                largest
            };
        }
        Variant::RamDisc => {
            record.secspertrack = 128;
            record.heads = 1;
            record.density = 1;
            record.skew = 0;
            record.log2secsize = 10;
            record.zone_spare = 32;
            record.disc_size = device_bytes & !(record.sector_size() - 1);
            // 128-byte map bits wherever an id numbers the fragments, as at
            // every size RISC OS 5 was recorded at; a larger disc takes the
            // next map bit up that does.
            record = (RAM_DISC_LOG2BPMB..=*LOG2BPMB.end())
                .find_map(|log2bpmb| {
                    let mut fitted = DiscRecord { log2bpmb, ..record };
                    fitted.fit_zones().ok().map(|()| fitted)
                })
                .ok_or(Fault::Unsupported)?;
        }
        _ => return Err(Fault::Unsupported),
    }
    let size = match variant.fixed_size() {
        Some(fixed) => fixed,
        None => device_bytes,
    };
    if size > device_bytes {
        return Err(Fault::DiscFull);
    }
    // Round down to whole sectors, then trim so the map's coverage
    // excess is either zero or a representable defect fragment.
    let mut disc_size = size & !(record.sector_size() - 1);
    let coverage = record.zone_start_block(u32::from(record.nzones));
    loop {
        let bits = disc_size >> record.log2bpmb;
        if bits == 0 {
            return Err(Fault::DiscFull);
        }
        let excess = coverage.saturating_sub(bits);
        if bits <= coverage && (excess == 0 || excess >= u64::from(record.least_fragment_bits())) {
            break;
        }
        disc_size -= record.sector_size();
    }
    record.disc_size = disc_size;
    Ok(record)
}

/// A [`DirStore`] window over a fixed byte range of the disc, used
/// while formatting (the root's location is known before the map is
/// mountable).
struct VolumeWindow<'a, D: Disc> {
    disc: &'a mut D,
    base: u64,
    len: u32,
}

impl<D: Disc> DirStore for VolumeWindow<'_, D> {
    type Error = D::Error;

    fn read_at(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), D> {
        if u64::from(offset) + buf.len() as u64 > u64::from(self.len) {
            return Err(Fault::Broken(Structure::Object).into());
        }
        self.disc
            .read(self.base + u64::from(offset), buf)
            .map_err(Error::Disc)
    }

    fn write_at(&mut self, offset: u32, data: &[u8]) -> Result<(), D> {
        if u64::from(offset) + data.len() as u64 > u64::from(self.len) {
            return Err(Fault::Broken(Structure::Object).into());
        }
        self.disc
            .write(self.base + u64::from(offset), data)
            .map_err(Error::Disc)
    }
}

/// Format `disc` as an old-map variant (S/M/L/D).
fn format_old_map<D: Disc>(disc: &mut D, variant: Variant) -> Result<(), D> {
    let size = variant.fixed_size().ok_or(Fault::Unsupported)?;
    if size > disc.capacity() {
        return Err(Fault::DiscFull.into());
    }
    // A floppy's sector count is far below `u32`.
    let disc_sectors = u32::try_from(size / OLD_SECTOR_SIZE).unwrap_or(0);
    let (format, root_sector) = match variant {
        Variant::D => (FixedFormat::New, 4u32),
        _ => (FixedFormat::Old, 2u32),
    };
    let root_offset = u64::from(root_sector) * OLD_SECTOR_SIZE;
    let root_sectors = u32::try_from(format.size() as u64 / OLD_SECTOR_SIZE).unwrap_or(0);
    // Zero the gap between the map and the root (a D-format disc is
    // recognised by the zero bytes at the start of sector 2).
    disc.zero(2 * OLD_SECTOR_SIZE, root_offset - 2 * OLD_SECTOR_SIZE)
        .map_err(Error::Disc)?;
    let mut map = OldMap::initialise(disc_sectors, root_sector + root_sectors, FORMAT_DISC_ID);
    map.store(disc)?;
    let root = FixedDir::initialise(format, *DIR_MARKERS[1], b"$", root_sector);
    disc.write(root_offset, &root.data[..format.size()])
        .map_err(Error::Disc)
}

/// Format `disc` as a new-map variant (E/E+/F/F+/hard disc).
fn format_new_map<D: Disc>(disc: &mut D, variant: Variant) -> Result<(), D> {
    let mut record = new_map_record(variant, disc.capacity())?;
    record.disc_id = FORMAT_DISC_ID;
    let boot = if variant == Variant::RamDisc {
        BootRecord::Blank
    } else {
        BootRecord::Whole
    };
    lay_out(disc, record, boot).map(drop)
}

/// What a multi-zone disc's boot block holds of the disc record.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BootRecord {
    /// The map's record, whole.
    Whole,
    /// The map's record with no identity — no id, name or root size — which
    /// finds the map as well: RISC OS 5's RAM disc writes it so.
    Blank,
}

/// Lay out an empty new-map disc described by `record` on `disc`: the map
/// in the middle zone, twice, behind a boot block on a multi-zone disc; and
/// the root directory, which shares the map's fragment in the fixed format
/// and in the big one is an object of its own, numbered as the first id the
/// map's zone homes. The record laid out, its root placed.
///
/// # Errors
///
/// * [`Fault::DiscFull`] where the disc cannot hold the map and root.
/// * The record broken where it describes no disc.
/// * The disc's failure.
pub fn lay_out<D: Disc>(
    disc: &mut D,
    mut record: DiscRecord,
    boot: BootRecord,
) -> Result<DiscRecord, D> {
    let big = record.format_version != 0;
    let map_offset = record.map_offset();
    let map_bytes = record.map_size();
    let bpmb = record.bytes_per_map_bit();
    let min_frag_bytes = record.least_fragment();
    let mut system_blocks = (2 * map_bytes).div_ceil(bpmb).max(min_frag_bytes / bpmb);
    let root_size = if big {
        record.root_size
    } else {
        NEW_DIR_SIZE_U32
    };
    let root_blocks = u64::from(root_size)
        .div_ceil(bpmb)
        .max(min_frag_bytes / bpmb);
    let root_id = if big {
        (record.ids_per_zone() * u32::from(record.nzones >> 1)).max(FRAG_ROOT + 1)
    } else {
        FRAG_ROOT
    };
    if big {
        record.root = root_id << 8 | 1;
    } else {
        // The root shares the map's fragment, at most 254 share units in: a
        // map longer than that many sectors takes a larger unit, the root
        // starting on one.
        let behind = system_blocks * bpmb;
        let share = (0..=MOST_LOG2SHARESIZE)
            .find(|&share| behind.div_ceil(record.sector_size() << share) < 0xFF)
            .ok_or(Fault::DiscFull)?;
        let unit = record.sector_size() << share;
        let units = behind.div_ceil(unit);
        system_blocks = (units * unit).div_ceil(bpmb);
        record.log2sharesize = share;
        record.root = FRAG_ROOT << 8 | u32::try_from(units + 1).map_err(|_| Fault::DiscFull)?;
    }
    let root_offset = map_offset + system_blocks * bpmb;
    record.validate()?;
    // The boot area, like every fragment, must span at least one legal
    // fragment; the padding just reserves a little more of the disc's start.
    let boot_blocks = if record.nzones > 1 {
        BOOT_AREA_BYTES.div_ceil(bpmb).max(min_frag_bytes / bpmb)
    } else {
        0
    };
    if root_offset + u64::from(root_size) > record.disc_size {
        return Err(Fault::DiscFull.into());
    }
    let layout = MapLayout {
        boot_blocks,
        system_start: map_offset / bpmb,
        // A fixed-directory root belongs to the system fragment.
        system_blocks: if big {
            system_blocks
        } else {
            system_blocks + root_blocks
        },
        root_blocks: if big { root_blocks } else { 0 },
        root_id,
    };
    write_fresh_map(disc, &record, &layout)?;
    if record.nzones > 1 {
        let copy = match boot {
            BootRecord::Whole => record,
            BootRecord::Blank => DiscRecord {
                disc_id: 0,
                disc_name: [0; NAME_LEN],
                root_size: 0,
                ..record
            },
        };
        write_boot_block(disc, &copy)?;
    }
    if big {
        let mut window = VolumeWindow {
            disc,
            base: root_offset,
            len: root_size,
        };
        BigDir::initialise(&mut window, root_size, b"$", record.root)?;
    } else {
        let root = FixedDir::initialise(FixedFormat::New, *DIR_MARKERS[1], b"$", record.root);
        disc.write(root_offset, &root.data[..NEW_DIR_SIZE])
            .map_err(Error::Disc)?;
    }
    Ok(record)
}

/// The allocated extents a fresh disc carries, in disc blocks.
struct MapLayout {
    /// Boot-area blocks at the start of the disc (fragment 2).
    boot_blocks: u64,
    /// First block of the map area (fragment 2, and the root behind it).
    system_start: u64,
    /// Blocks of the map area (including a fixed-directory root).
    system_blocks: u64,
    /// Blocks of a big-directory root, directly after the system extent;
    /// `0` on fixed-directory volumes.
    root_blocks: u64,
    /// The big-directory root's fragment id.
    root_id: u32,
}

/// Write every map zone of a fresh disc to both map copies: the
/// allocated system extents, one free fragment per gap, and the trailing
/// defect fragment covering map bits past the end of the disc.
fn write_fresh_map<D: Disc>(
    disc: &mut D,
    record: &DiscRecord,
    layout: &MapLayout,
) -> Result<(), D> {
    // The sector size was validated against `MAX_ZONE_BYTES`.
    let sector = usize::try_from(record.sector_size()).unwrap_or(MAX_ZONE_BYTES);
    let idlen = u32::from(record.idlen);
    let total_blocks = record.disc_size / record.bytes_per_map_bit();
    for zone in 0..u32::from(record.nzones) {
        let mut buf = [0u8; MAX_ZONE_BYTES];
        let start_bit = zone_start_bit(zone);
        let start_block = record.zone_start_block(zone);
        let covered = record.zone_stream_bits(zone);
        let physical_end = start_bit + covered;
        let logical_blocks = total_blocks
            .saturating_sub(start_block)
            .min(u64::from(covered));
        let logical_end = start_bit + bit_offset(logical_blocks);
        // Zone 0 embeds the disc record, and its cross-check byte alone makes
        // every zone's XOR to the sum.
        if zone == 0 {
            buf[ZONE_HEADER_SIZE..ZONE_HEADER_SIZE + DISC_RECORD_SIZE]
                .copy_from_slice(&record.encode());
            buf[CROSS_CHECK_BYTE] = CROSS_CHECK_SUM;
        }
        // Collect this zone's allocated segments in block order. Every
        // segment arrives at least a fragment's least bits long (the layout
        // sizes them so), and abutting segments are legal — adjacent
        // fragments are self-delimiting.
        let min_bits = u64::from(record.least_fragment_bits());
        let mut segments = [(0u64, 0u64, 0u32); 3];
        let mut segment_count = 0;
        for (seg_start, seg_len, id) in [
            (0u64, layout.boot_blocks, FRAG_ROOT),
            (layout.system_start, layout.system_blocks, FRAG_ROOT),
            (
                layout.system_start + layout.system_blocks,
                layout.root_blocks,
                layout.root_id,
            ),
        ] {
            if seg_len == 0 {
                continue;
            }
            let lo = seg_start.max(start_block);
            let hi = (seg_start + seg_len).min(start_block + logical_blocks);
            if lo < hi {
                // A segment split across zones must leave a legal
                // fragment in each; the layout keeps extents whole-zone
                // sized or well inside one zone, so a short piece here
                // is a layout bug caught closed.
                if hi - lo < min_bits {
                    return Err(Fault::DiscFull.into());
                }
                segments[segment_count] = (lo - start_block, hi - lo, id);
                segment_count += 1;
            }
        }
        // Lay the stream: gaps become free fragments (linked from the
        // zone header), segments become allocated fragments.
        let mut prev_link_pos: Option<u32> = None;
        let mut cursor = start_bit;
        let place_free =
            |buf: &mut [u8; MAX_ZONE_BYTES], prev: &mut Option<u32>, from: u32, to: u32| {
                if to > from {
                    link_free(buf, record, *prev, from);
                    bits_set(buf, to - 1, true);
                    *prev = Some(from);
                }
            };
        for &(rel_start, len, id) in &segments[..segment_count] {
            let seg_bit = start_bit + bit_offset(rel_start);
            // A free gap smaller than a legal fragment cannot exist.
            if seg_bit != cursor && seg_bit - cursor < bit_offset(min_bits) {
                return Err(Fault::DiscFull.into());
            }
            place_free(&mut buf, &mut prev_link_pos, cursor, seg_bit);
            bits_set_field(&mut buf, seg_bit, idlen, id);
            bits_set(&mut buf, seg_bit + bit_offset(len) - 1, true);
            cursor = seg_bit + bit_offset(len);
        }
        // The trailing free gap, and the defect run covering map bits
        // past the disc end. A too-small trailing gap is folded into
        // whichever neighbour keeps every fragment legal.
        let mut defect_start = logical_end;
        if logical_end > cursor && u64::from(logical_end - cursor) < min_bits {
            if physical_end > logical_end {
                // The defect run absorbs the sliver of real blocks.
                defect_start = cursor;
            } else if segment_count > 0 {
                // The last allocated fragment absorbs it as slack.
                bits_set(&mut buf, cursor - 1, false);
                bits_set(&mut buf, logical_end - 1, true);
                cursor = logical_end;
            } else {
                return Err(Fault::DiscFull.into());
            }
        }
        place_free(
            &mut buf,
            &mut prev_link_pos,
            cursor,
            defect_start.min(logical_end),
        );
        if physical_end > defect_start.min(logical_end).max(cursor) {
            let at = defect_start.min(logical_end).max(cursor);
            if physical_end - at < bit_offset(min_bits) {
                return Err(Fault::DiscFull.into());
            }
            bits_set_field(&mut buf, at, idlen, FRAG_BAD);
            bits_set(&mut buf, physical_end - 1, true);
        }
        buf[ZONE_CHECK_BYTE] = zone_check(&buf[..sector]);
        for copy in [MapCopy::First, MapCopy::Second] {
            disc.write(record.zone_offset(copy, zone), &buf[..sector])
                .map_err(Error::Disc)?;
        }
    }
    Ok(())
}

/// A map-bit offset as `u32`: every zone is a single sector, so its
/// bit offsets stay far below `u32` by construction.
fn bit_offset(bits: u64) -> u32 {
    u32::try_from(bits).unwrap_or(u32::MAX)
}

/// Point the free-list link at `prev` (a free fragment's bit, or the
/// zone header) at the free fragment starting at `bit`, writing the
/// header's stop bit when the link is the header's.
fn link_free(buf: &mut [u8; MAX_ZONE_BYTES], record: &DiscRecord, prev: Option<u32>, bit: u32) {
    let width = link_bits(record.idlen);
    match prev {
        None => {
            // The header's link field carries its own stop bit at its top.
            bits_set_field(buf, FREELINK_BIT, width, bit - FREELINK_BIT);
            bits_set(buf, FREELINK_BIT + LINK_BITS_MAX, true);
        }
        Some(pos) => bits_set_field(buf, pos, width, bit - pos),
    }
}

/// Write the checksummed boot block embedding the disc record.
fn write_boot_block<D: Disc>(disc: &mut D, record: &DiscRecord) -> Result<(), D> {
    let mut block = [0u8; BOOT_BLOCK_SIZE];
    seal_boot_block(&mut block, record);
    disc.write(BOOT_BLOCK_OFFSET, &block).map_err(Error::Disc)
}

/// Format `disc` as `variant`, laying out an empty, valid filesystem.
///
/// # Errors
///
/// * [`Fault::DiscFull`] if the disc is too small.
/// * The disc's failure.
pub fn format_volume<D: Disc>(disc: &mut D, variant: Variant) -> Result<(), D> {
    if variant.is_old_map() {
        format_old_map(disc, variant)
    } else {
        format_new_map(disc, variant)
    }
}

#[cfg(test)]
mod tests;
