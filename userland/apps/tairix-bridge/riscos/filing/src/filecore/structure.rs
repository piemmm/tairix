//! What `FileCore_FloppyStructure` lays the start of a floppy out as, as RISC
//! OS 5 lays it out.
//!
//! The map is old where R2's bit 6 or 7 asks; the root takes the old 1280-byte
//! layout where bit 7 does, and is marked `Hugo` where bit 6 does, `Nick`
//! otherwise. Its parent is the record's root, its name and title `$`, zero
//! padded, and the record is written as given but for a fresh disc id.
//!
//! A new map is laid as though the disc had one zone: zone 0's sector, its
//! copy straight after it, then the root, their size the answer in R3. Its
//! fragments run through the map's bits as one stream — zone 0's from its
//! disc record on, each later zone's sector from its first bit, each zone but
//! the last as many as a zone describes — so a disc of more zones has their
//! bits laid where the copy and the root then fall, and past them. The
//! system fragment covers the copies and the root; the defects follow in the
//! list's order, each sector widened to the least fragment and joined to a
//! defect within a fragment of it, one behind those laid skipped; free space
//! runs on to the disc's end, and a disc ending in zone 0 has the rest of
//! that sector as a defect. Zone 0's sector and each zone's before the last
//! are cleared whole first, the last zone's through the disc's end, and bit 1
//! of the byte where a zone after the last would begin is cleared.

use alloc::vec::Vec;

use tairix_filecore::defect::FIRST_END;
use tairix_filecore::dir::{FixedDir, FixedFormat, NameEnd, NEW_DIR_SIZE};
use tairix_filecore::oldmap::OldMap;
use tairix_filecore::record::{
    link_bits, zone_check, DiscRecord, CROSS_CHECK_BYTE, CROSS_CHECK_SUM, DIR_MARKERS,
    DISC_RECORD_SIZE, FRAG_BAD, FRAG_ROOT, FREELINK_BIT, LINK_BITS_MAX, LOG2BPMB, LOG2SECSIZE,
    OLD_SECTOR_SIZE, ZONE_CHECK_BYTE, ZONE_HEADER_SIZE,
};
use tairix_filecore::{Disc, Error, Fault};

/// R2's bit asking for an old map.
pub(super) const OLD_MAP: u32 = 1 << 6;
/// R2's bit asking for old directories.
pub(super) const OLD_DIRECTORIES: u32 = 1 << 7;

/// The first bit of zone 0 a fragment may take: past its header and the disc
/// record.
const ZONE_0_STREAM: u64 = ((ZONE_HEADER_SIZE + DISC_RECORD_SIZE) * 8) as u64;

/// A floppy's start, laid out.
pub(super) struct Laid {
    /// The bytes, from the buffer's start, and which of them were written.
    pub(super) image: Image,
    /// What R3 answers.
    pub(super) size: u32,
    /// The byte bit 1 is cleared in, past the image.
    pub(super) cleared: Option<u32>,
    /// A defect lay where the map and root do: FileCore's "Defect", the
    /// structure left as far as it was laid.
    pub(super) refused: bool,
}

/// The structure for the record `record`, R2's `flags` and the defects
/// `defects`, a disc of the id `disc_id`.
///
/// # Errors
///
/// [`Fault::BadDefect`] for a record no structure can be laid from;
/// [`Fault::NoMemory`] where the host has no room for it.
pub(super) fn floppy(
    record: &DiscRecord,
    flags: u32,
    defects: &[u32],
    disc_id: u16,
) -> Result<Laid, Fault> {
    if flags & (OLD_MAP | OLD_DIRECTORIES) == 0 {
        new_map(record, defects, disc_id)
    } else {
        old_map(record, flags, disc_id)
    }
}

/// An old map's floppy: the map, then the root.
fn old_map(record: &DiscRecord, flags: u32, disc_id: u16) -> Result<Laid, Fault> {
    let format = if flags & OLD_DIRECTORIES == 0 {
        FixedFormat::New
    } else {
        FixedFormat::Old
    };
    let marker = *DIR_MARKERS[usize::from(flags & OLD_MAP == 0)];
    let root_at = match format {
        FixedFormat::Old => 2 * OLD_SECTOR_SIZE,
        FixedFormat::New => 4 * OLD_SECTOR_SIZE,
    };
    let end = root_at + format.size() as u64;
    let sectors =
        u32::try_from(record.disc_size / OLD_SECTOR_SIZE).map_err(|_| Fault::BadDefect)?;
    let first_free = u32::try_from(end / OLD_SECTOR_SIZE).unwrap_or(u32::MAX);
    if sectors < first_free {
        return Err(Fault::BadDefect);
    }
    let mut image = Image::new(end)?;
    image.zero(0, end);
    let mut map = OldMap::initialise(sectors, first_free, disc_id);
    map.set_name(record.disc_name);
    map.set_boot_option(record.bootoption);
    map.store(&mut image).map_err(|error| match error {
        Error::Fault(fault) | Error::Disc(fault) => fault,
    })?;
    let root = FixedDir::initialise(format, marker, b"$", record.root, NameEnd::Zeros);
    image.put(root_at, &root.data[..format.size()]);
    Ok(Laid {
        image,
        size: u32::try_from(end).unwrap_or(u32::MAX),
        cleared: None,
        refused: false,
    })
}

/// A new map's floppy, laid as one zone's.
fn new_map(record: &DiscRecord, defects: &[u32], disc_id: u16) -> Result<Laid, Fault> {
    let zones = u64::from(record.nzones);
    // A floppy's record numbers its zones in a byte.
    if zones == 0
        || zones > u64::from(u8::MAX)
        || !LOG2SECSIZE.contains(&record.log2secsize)
        || !LOG2BPMB.contains(&record.log2bpmb)
        || record.idlen == 0
    {
        return Err(Fault::BadDefect);
    }
    let sector = record.sector_size();
    let bpmb = record.bytes_per_map_bit();
    let size = 2 * sector + NEW_DIR_SIZE as u64;
    let stream = Stream::new(record);
    let blocks = record.disc_size / bpmb;
    // More disc than its zones describe is no floppy's, and would size the
    // image by the record rather than by its map.
    if blocks > stream.capacity() {
        return Err(Fault::BadDefect);
    }
    let extent = stream.extent(blocks).max(size);
    let mut image = Image::new(extent.max(zones * sector))?;
    image.zero(0, 2 * sector);
    for zone in 1..zones.saturating_sub(1) {
        image.zero(zone * sector, sector);
    }
    if zones > 1 {
        let last = (zones - 1) * sector;
        image.zero(last, extent.saturating_sub(last).min(sector));
    }
    let written = DiscRecord { disc_id, ..*record };
    image.put(ZONE_HEADER_SIZE as u64, &written.encode());
    image.set_bit(u64::from(FREELINK_BIT + LINK_BITS_MAX), true);
    image.put(CROSS_CHECK_BYTE as u64, &[CROSS_CHECK_SUM]);
    let idlen = u32::from(record.idlen);
    let system = size.div_ceil(bpmb);
    let mut writer = Fragments {
        image: &mut image,
        stream: &stream,
        idlen,
    };
    writer.fragment(0, system, FRAG_ROOT);
    let Some(runs) = sweep(record, defects, system, blocks) else {
        return Ok(Laid {
            image,
            size: u32::try_from(size).unwrap_or(u32::MAX),
            cleared: None,
            refused: true,
        });
    };
    let mut writer = Fragments {
        image: &mut image,
        stream: &stream,
        idlen,
    };
    let link = link_bits(record.idlen);
    let mut next_free = None;
    for run in runs.iter().rev() {
        if run.defect {
            writer.fragment(run.start, run.end, FRAG_BAD);
        } else {
            let gap = next_free.map_or(0, |next: u64| next - run.start);
            writer.free(run.start, run.end, gap, link);
            next_free = Some(run.start);
        }
    }
    if let Some(first) = next_free {
        let at = stream.bit(first) - u64::from(FREELINK_BIT);
        writer.field(u64::from(FREELINK_BIT), link, at);
    }
    if zones == 1 {
        let tail = ZONE_0_STREAM + blocks;
        if tail < sector * 8 {
            writer.raw_fragment(tail, sector * 8, FRAG_BAD);
        }
    }
    let byte = zones * sector;
    let cleared = if byte < image.len() {
        image.clear_bit(byte * 8 + 1);
        None
    } else {
        u32::try_from(byte).ok()
    };
    let check = zone_check(image.slice(0, sector));
    image.put(ZONE_CHECK_BYTE as u64, &[check]);
    image.copy(0, sector, sector);
    let root = FixedDir::initialise(
        FixedFormat::New,
        *DIR_MARKERS[1],
        b"$",
        record.root,
        NameEnd::Zeros,
    );
    image.put(2 * sector, &root.data[..NEW_DIR_SIZE]);
    Ok(Laid {
        image,
        size: u32::try_from(size).unwrap_or(u32::MAX),
        cleared,
        refused: false,
    })
}

/// A run of the stream, in disc blocks.
struct Run {
    start: u64,
    end: u64,
    defect: bool,
}

/// The free space and defects from block `from` to the disc's end at
/// `blocks`, as `FileCore_FloppyStructure` sweeps the list `defects`; none
/// where a defect lies before `from`, in the map and root.
fn sweep(record: &DiscRecord, defects: &[u32], from: u64, blocks: u64) -> Option<Vec<Run>> {
    let sector = record.sector_size();
    let bpmb = record.bytes_per_map_bit();
    let span = (sector / bpmb).max(1);
    let fewest = u64::from(record.least_fragment_bits());
    let mut runs: Vec<Run> = Vec::new();
    let mut cursor = from;
    for &defect in defects.iter().take_while(|&&word| word < FIRST_END) {
        let block = (u64::from(defect) - u64::from(defect) % sector) / bpmb;
        if block < from {
            return None;
        }
        if block < cursor || block >= blocks {
            continue;
        }
        if let Some(last) = runs.last_mut().filter(|run| run.defect) {
            if block - cursor < fewest {
                last.end = (block + span).min(blocks);
                cursor = last.end;
                continue;
            }
        }
        let want = span.max(fewest).next_multiple_of(span);
        let (mut start, mut end) = (block, block + want);
        if end > blocks {
            end = (block + span).min(blocks);
            start = end.saturating_sub(want).max(cursor);
        }
        if start > cursor && start - cursor < fewest {
            start = cursor;
        }
        if start > cursor {
            runs.push(Run {
                start: cursor,
                end: start,
                defect: false,
            });
        }
        runs.push(Run {
            start,
            end,
            defect: true,
        });
        cursor = end;
    }
    if cursor < blocks {
        match runs.last_mut().filter(|run| run.defect) {
            Some(last) if blocks - cursor < fewest => last.end = blocks,
            _ => runs.push(Run {
                start: cursor,
                end: blocks,
                defect: false,
            }),
        }
    }
    Some(runs)
}

/// Where the map's stream of fragment bits falls in the image: zone 0's
/// sector from its disc record on, each later zone's from its first bit, each
/// zone before the last as many bits as a zone describes, the last to its
/// sector's end.
struct Stream {
    sector_bits: u64,
    zone_bits: u64,
    zones: u64,
}

impl Stream {
    fn new(record: &DiscRecord) -> Self {
        let sector_bits = record.sector_size() * 8;
        Self {
            sector_bits,
            zone_bits: sector_bits.saturating_sub(u64::from(record.zone_spare)),
            zones: u64::from(record.nzones),
        }
    }

    /// Where zone `zone`'s stream starts in its sector, and the blocks it
    /// describes there.
    fn span(&self, zone: u64) -> (u64, u64) {
        let start = if zone == 0 { ZONE_0_STREAM } else { 0 };
        let end = if zone + 1 == self.zones {
            self.sector_bits
        } else {
            self.zone_bits
        };
        (start, end.saturating_sub(start))
    }

    /// The image bit block `block` of the stream falls at.
    fn bit(&self, block: u64) -> u64 {
        let mut left = block;
        for zone in 0..self.zones {
            let (start, room) = self.span(zone);
            if left < room || zone + 1 == self.zones {
                return zone * self.sector_bits + start + left;
            }
            left -= room;
        }
        block
    }

    /// The blocks the stream describes before it runs past the last zone's
    /// sector.
    fn capacity(&self) -> u64 {
        (0..self.zones).map(|zone| self.span(zone).1).sum()
    }

    /// The bytes the stream reaches for a disc of `blocks` blocks.
    fn extent(&self, blocks: u64) -> u64 {
        blocks
            .checked_sub(1)
            .map_or(0, |last| self.bit(last) / 8 + 1)
    }
}

/// Fragments written into the image through the stream.
struct Fragments<'a> {
    image: &'a mut Image,
    stream: &'a Stream,
    idlen: u32,
}

impl Fragments<'_> {
    /// Blocks `[start, end)` of the stream a fragment of `id`.
    fn fragment(&mut self, start: u64, end: u64, id: u32) {
        for i in 0..u64::from(self.idlen) {
            self.image
                .set_bit(self.stream.bit(start + i), id >> i & 1 != 0);
        }
        self.image.set_bit(self.stream.bit(end - 1), true);
    }

    /// Blocks `[start, end)` of the stream free, its link `link` bits wide
    /// pointing `gap` blocks on.
    fn free(&mut self, start: u64, end: u64, gap: u64, link: u32) {
        for i in 0..u64::from(link) {
            self.image
                .set_bit(self.stream.bit(start + i), gap >> i & 1 != 0);
        }
        self.image.set_bit(self.stream.bit(end - 1), true);
    }

    /// Image bits `[start, end)` a fragment of `id`, past the stream.
    fn raw_fragment(&mut self, start: u64, end: u64, id: u32) {
        for i in 0..u64::from(self.idlen) {
            self.image.set_bit(start + i, id >> i & 1 != 0);
        }
        self.image.set_bit(end - 1, true);
    }

    /// The `width`-bit field at image bit `at` set to `value`.
    fn field(&mut self, at: u64, width: u32, value: u64) {
        for i in 0..u64::from(width) {
            self.image.set_bit(at + i, value >> i & 1 != 0);
        }
    }
}

/// The structure's bytes, and which of them are written.
pub(super) struct Image {
    bytes: Vec<u8>,
    written: Vec<bool>,
}

impl Image {
    /// Nothing written yet, in `len` bytes.
    fn new(len: u64) -> Result<Self, Fault> {
        let len = usize::try_from(len).map_err(|_| Fault::NoMemory)?;
        let mut bytes = Vec::new();
        let mut written = Vec::new();
        bytes.try_reserve_exact(len).map_err(|_| Fault::NoMemory)?;
        written
            .try_reserve_exact(len)
            .map_err(|_| Fault::NoMemory)?;
        bytes.resize(len, 0);
        written.resize(len, false);
        Ok(Self { bytes, written })
    }

    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn slice(&self, at: u64, len: u64) -> &[u8] {
        let at = usize::try_from(at)
            .unwrap_or(usize::MAX)
            .min(self.bytes.len());
        let end = at.saturating_add(usize::try_from(len).unwrap_or(usize::MAX));
        &self.bytes[at..end.min(self.bytes.len())]
    }

    /// `bytes` written at `at`, as far as the image reaches.
    fn put(&mut self, at: u64, bytes: &[u8]) {
        let Ok(at) = usize::try_from(at) else { return };
        for (i, &byte) in bytes.iter().enumerate() {
            if let (Some(slot), Some(mark)) =
                (self.bytes.get_mut(at + i), self.written.get_mut(at + i))
            {
                *slot = byte;
                *mark = true;
            }
        }
    }

    /// `len` bytes at `at` written zero.
    fn zero(&mut self, at: u64, len: u64) {
        let Ok(at) = usize::try_from(at) else { return };
        let end = at
            .saturating_add(usize::try_from(len).unwrap_or(usize::MAX))
            .min(self.bytes.len());
        if at < end {
            self.bytes[at..end].fill(0);
            self.written[at..end].fill(true);
        }
    }

    /// Image bit `bit` set or cleared, its byte written.
    fn set_bit(&mut self, bit: u64, value: bool) {
        let Ok(at) = usize::try_from(bit / 8) else {
            return;
        };
        if let (Some(byte), Some(mark)) = (self.bytes.get_mut(at), self.written.get_mut(at)) {
            let mask = 1 << (bit % 8);
            if value {
                *byte |= mask;
            } else {
                *byte &= !mask;
            }
            *mark = true;
        }
    }

    /// Image bit `bit` cleared.
    fn clear_bit(&mut self, bit: u64) {
        self.set_bit(bit, false);
    }

    /// The `len` bytes at `from` written again at `to`.
    fn copy(&mut self, from: u64, len: u64, to: u64) {
        let (Ok(from), Ok(len), Ok(to)) = (
            usize::try_from(from),
            usize::try_from(len),
            usize::try_from(to),
        ) else {
            return;
        };
        if from + len <= self.bytes.len() && to + len <= self.bytes.len() {
            self.bytes.copy_within(from..from + len, to);
            self.written[to..to + len].fill(true);
        }
    }

    /// Each run of written bytes: where it starts, and its bytes.
    pub(super) fn written_runs(&self) -> impl Iterator<Item = (usize, &[u8])> {
        let mut at = 0;
        core::iter::from_fn(move || {
            while self.written.get(at) == Some(&false) {
                at += 1;
            }
            let start = at;
            while self.written.get(at) == Some(&true) {
                at += 1;
            }
            (start < at).then(|| (start, &self.bytes[start..at]))
        })
    }
}

/// The image as a disc an old map is stored on.
impl Disc for Image {
    type Error = Fault;

    fn capacity(&self) -> u64 {
        self.len()
    }

    fn read(&mut self, at: u64, out: &mut [u8]) -> Result<(), Fault> {
        let source = self.slice(at, out.len() as u64);
        out[..source.len()].copy_from_slice(source);
        out[source.len()..].fill(0);
        Ok(())
    }

    fn write(&mut self, at: u64, bytes: &[u8]) -> Result<(), Fault> {
        self.put(at, bytes);
        Ok(())
    }

    fn zero(&mut self, at: u64, length: u64) -> Result<(), Fault> {
        Image::zero(self, at, length);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Fault> {
        Ok(())
    }
}
