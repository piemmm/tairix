//! D64 images: a 1541 disk's 256-byte sectors in order, track by track — 21
//! sectors a track on tracks 1 to 17, 19 to 24, 18 to 30 and 17 beyond — 35
//! tracks or 40, and after them, in some images, a byte a sector naming the
//! error the drive reported reading it.
//!
//! A track is recorded as the 1541 formats one: each sector a SYNC, its
//! header, a gap, a SYNC and its data block, a gap filling the revolution
//! evenly after each; the disk's ID taken from its BAM. An error code is
//! recorded as the fault that makes the drive report it.

use alloc::vec::Vec;
use core::iter::repeat_n;

use super::layout::{
    bam, BAM_SECTOR, DIRECTORY_TRACK, FIRST_DIRECTORY_SECTOR, NAME_BYTES, PADDING, TRACKS,
};
use super::recording::{
    job, DATA_GCR, DATA_MARK, FILL, FILL_FIRST, FILL_FIRST_TRACK_1, GAP, HEADER_GAP, HEADER_GCR,
    HEADER_MARK, HEADER_PADDING, SECTOR_RECORDING, SYNC, SYNC_BYTES,
};
use super::{gcr, DiskError, Format, Surface, Track, HALF_TRACKS};
use crate::machine::c64::drive::head;

/// Bytes in a sector.
pub const SECTOR_BYTES: usize = 256;

/// Sectors on `track`, counted from 1.
#[must_use]
pub const fn sectors(track: u8) -> u8 {
    match track {
        0..=17 => 21,
        18..=24 => 19,
        25..=30 => 18,
        _ => 17,
    }
}

/// The zone `track` is written at, 3 outermost.
#[must_use]
pub const fn zone(track: u8) -> u8 {
    match track {
        0..=17 => 3,
        18..=24 => 2,
        25..=30 => 1,
        _ => 0,
    }
}

/// Bytes a revolution holds at `zone`'s rate, its last cells too few for a
/// byte left over.
#[must_use]
pub const fn revolution_bytes(zone: u8) -> usize {
    (head::REVOLUTION / head::cell_cycles(zone) / 8) as usize
}

/// Sectors on the tracks before `track`.
fn sectors_before(track: u8) -> usize {
    (1..track).map(|t| usize::from(sectors(t))).sum()
}

/// A header's bytes as far as its ID, and a data block's as far as its
/// checksum: what follows is padding a write's end can spoil.
const HEADER_READ: usize = 6;
const DATA_READ: usize = 258;

/// The faults an error code is recorded as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    /// Read without error.
    None,
    /// 20: the header's mark is not $08.
    NoHeader,
    /// 21: no SYNC before the sector's blocks.
    NoSync,
    /// 22: the data block's mark is not $07.
    NoData,
    /// 23: the data block's checksum is wrong.
    DataChecksum,
    /// 27: the header's checksum is wrong.
    HeaderChecksum,
    /// 29: the header carries another disk's ID.
    WrongId,
}

impl Fault {
    /// The fault an image's error byte names.
    const fn from_code(code: u8) -> Self {
        match code {
            job::NO_HEADER => Self::NoHeader,
            job::NO_SYNC => Self::NoSync,
            job::NO_DATA_BLOCK => Self::NoData,
            job::DATA_CHECKSUM => Self::DataChecksum,
            job::HEADER_CHECKSUM => Self::HeaderChecksum,
            job::ID_MISMATCH => Self::WrongId,
            _ => Self::None,
        }
    }
}

/// The XOR of `bytes`, as a block's checksum.
fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |sum, byte| sum ^ byte)
}

/// Track `track`'s recording of `data`, its sectors' bytes in order, with
/// their error codes `codes` if the image has them.
pub(super) fn record(
    track: u8,
    data: &[u8],
    codes: Option<&[u8]>,
    id: [u8; 2],
) -> Result<Track, DiskError> {
    let zone = zone(track);
    let capacity = revolution_bytes(zone);
    let count = usize::from(sectors(track));
    let gap = capacity.saturating_sub(count * SECTOR_RECORDING) / count;
    let mut ring = Vec::with_capacity(capacity);
    for (sector, block) in data.as_chunks::<SECTOR_BYTES>().0.iter().enumerate() {
        let fault = codes.map_or(Fault::None, |codes| Fault::from_code(codes[sector]));
        let sector = u8::try_from(sector).map_err(|_| DiskError::Malformed)?;
        let sync = repeat_n(
            if fault == Fault::NoSync { GAP } else { SYNC },
            usize::from(SYNC_BYTES),
        );
        let header_id = if fault == Fault::WrongId {
            [id[0] ^ 0xFF, id[1]]
        } else {
            id
        };
        let mut header = [
            HEADER_MARK,
            0,
            sector,
            track,
            header_id[1],
            header_id[0],
            HEADER_PADDING,
            HEADER_PADDING,
        ];
        header[1] = checksum(&header[2..6]);
        match fault {
            Fault::NoHeader => header[0] = 0x00,
            Fault::HeaderChecksum => header[1] ^= 0xFF,
            _ => {}
        }
        ring.extend(sync.clone());
        gcr::encode(&header, &mut ring);
        ring.extend(repeat_n(GAP, usize::from(HEADER_GAP)));
        ring.extend(sync);
        // The mark, the sector, its checksum, two zeros.
        let mut block_bytes = [0; SECTOR_BYTES + 4];
        block_bytes[0] = if fault == Fault::NoData {
            0x00
        } else {
            DATA_MARK
        };
        block_bytes[1..=SECTOR_BYTES].copy_from_slice(block);
        let sum = checksum(block);
        block_bytes[SECTOR_BYTES + 1] = if fault == Fault::DataChecksum {
            sum ^ 0xFF
        } else {
            sum
        };
        gcr::encode(&block_bytes, &mut ring);
        ring.extend(repeat_n(GAP, gap));
    }
    ring.resize(capacity, GAP);
    Track::new(ring, zone)
}

/// The largest D64: forty tracks, with a byte of error code a sector.
pub const MAX_IMAGE: usize = 197_376;

/// A 35-track D64 as the 1541's DOS formats a disk named `name` (its first
/// sixteen bytes) with `id`: the BAM and an empty directory, and every other
/// block `$4B` then `$01`s, track 1's opening with `$00`.
#[must_use]
pub fn formatted(name: &[u8], id: [u8; 2]) -> Vec<u8> {
    let mut image = alloc::vec![0; sectors_before(TRACKS + 1) * SECTOR_BYTES];
    for track in 1..=TRACKS {
        let first = if track == 1 {
            FILL_FIRST_TRACK_1
        } else {
            FILL_FIRST
        };
        for sector in 0..sectors(track) {
            if track == DIRECTORY_TRACK && sector <= FIRST_DIRECTORY_SECTOR {
                continue;
            }
            let at = (sectors_before(track) + usize::from(sector)) * SECTOR_BYTES;
            image[at] = first;
            image[at + 1..at + SECTOR_BYTES].fill(FILL);
        }
    }
    let at = |field: u8| sectors_before(DIRECTORY_TRACK) * SECTOR_BYTES + usize::from(field);
    image[at(0)] = DIRECTORY_TRACK;
    image[at(1)] = FIRST_DIRECTORY_SECTOR;
    image[at(bam::VERSION_AT)] = bam::VERSION;
    for track in 1..=TRACKS {
        let entry = at(bam::TRACK_ENTRY_BYTES * track);
        // The BAM's and the directory's sectors, the directory track's first.
        let used = if track == DIRECTORY_TRACK {
            FIRST_DIRECTORY_SECTOR + 1
        } else {
            0
        };
        image[entry] = sectors(track) - used;
        for sector in used..sectors(track) {
            image[entry + 1 + usize::from(sector / 8)] |= 1 << (sector % 8);
        }
    }
    let name = &name[..name.len().min(usize::from(NAME_BYTES))];
    image[at(bam::NAME)..at(bam::ID)].fill(PADDING);
    image[at(bam::NAME)..][..name.len()].copy_from_slice(name);
    image[at(bam::ID)..at(bam::ID) + 2].copy_from_slice(&id);
    image[at(bam::ID) + 2] = PADDING;
    image[at(bam::DOS_TYPE_AT)..at(bam::DOS_TYPE_AT) + 2].copy_from_slice(&bam::DOS_TYPE);
    image[at(bam::DOS_TYPE_AT) + 2..at(bam::HEADER_END)].fill(PADDING);
    let directory =
        (sectors_before(DIRECTORY_TRACK) + usize::from(FIRST_DIRECTORY_SECTOR)) * SECTOR_BYTES;
    image[directory + 1] = 0xFF;
    image
}

/// A D64 image's disk.
///
/// # Errors
///
/// [`DiskError::BadSize`] for a length no D64 has.
pub fn decode(image: &[u8]) -> Result<Surface, DiskError> {
    let (tracks, with_errors) = match image.len() {
        174_848 => (35, false),
        175_531 => (35, true),
        196_608 => (40, false),
        MAX_IMAGE => (40, true),
        _ => return Err(DiskError::BadSize),
    };
    let total = sectors_before(tracks + 1);
    let (data, codes) = image.split_at(total * SECTOR_BYTES);
    let id_at = sectors_before(DIRECTORY_TRACK) * SECTOR_BYTES + usize::from(bam::ID);
    let id = [data[id_at], data[id_at + 1]];
    let mut rings = alloc::vec![None; HALF_TRACKS];
    for track in 1..=tracks {
        let first = sectors_before(track);
        let count = usize::from(sectors(track));
        let sectors = &data[first * SECTOR_BYTES..(first + count) * SECTOR_BYTES];
        let codes = with_errors.then(|| &codes[first..first + count]);
        rings[usize::from(track - 1) * 2] = Some(record(track, sectors, codes, id)?);
    }
    Surface::new(rings, Format::D64 { tracks })
}

/// The first `count` bytes of the block beginning at bit `start` of
/// `ring`, `len` bytes of GCR long, decoded.
fn read_block(ring: &Track, start: usize, len: usize, count: usize) -> Option<Vec<u8>> {
    let gcr: Vec<u8> = (0..len)
        .map(|byte| {
            (0..8).fold(0u8, |value, bit| {
                (value << 1) | u8::from(ring.bit(start + byte * 8 + bit))
            })
        })
        .collect();
    gcr::decode_prefix(&gcr, count)
}

/// Where each block of `ring` begins: the bit ending each run of at least
/// ten ones, within one revolution.
fn block_starts(ring: &Track) -> Vec<usize> {
    let len = ring.len();
    let mut starts = Vec::new();
    let mut ones = 0;
    for position in 0..len + 16 {
        if ring.bit(position) {
            ones += 1;
            continue;
        }
        if ones >= 10 && position >= 16 {
            starts.push(position % len);
        }
        ones = 0;
    }
    starts.sort_unstable();
    starts.dedup();
    starts
}

/// Track `track`'s sectors as `ring` records them, if each decodes once
/// with its header, every header carrying `id`, and the ring holds no other
/// block: a header and its data a sector.
fn read_track(ring: &Track, track: u8, id: [u8; 2]) -> Option<Vec<[u8; SECTOR_BYTES]>> {
    let count = usize::from(sectors(track));
    let mut sectors: Vec<Option<[u8; SECTOR_BYTES]>> = alloc::vec![None; count];
    let starts = block_starts(ring);
    if starts.len() != 2 * count {
        return None;
    }
    for (i, &start) in starts.iter().enumerate() {
        let Some(header) = read_block(ring, start, usize::from(HEADER_GCR), HEADER_READ) else {
            continue;
        };
        if header[0] != HEADER_MARK
            || header[1] != checksum(&header[2..6])
            || header[3] != track
            || [header[5], header[4]] != id
        {
            continue;
        }
        let next = starts[(i + 1) % starts.len()];
        let block = read_block(ring, next, usize::from(DATA_GCR), DATA_READ)?;
        if block[0] != DATA_MARK || block[257] != checksum(&block[1..257]) {
            return None;
        }
        let slot = sectors.get_mut(usize::from(header[2]))?;
        if slot.is_some() {
            return None;
        }
        let mut data = [0; SECTOR_BYTES];
        data.copy_from_slice(&block[1..257]);
        *slot = Some(data);
    }
    sectors.into_iter().collect()
}

/// `surface` as a D64 of `tracks` tracks, if it records nothing but those
/// tracks' sectors, every one decoding, and its BAM names the ID its headers
/// carry, which a D64's headers are made from.
#[must_use]
pub fn encode(surface: &Surface, tracks: u8) -> Option<Vec<u8>> {
    let bam_ring = surface.track(usize::from(DIRECTORY_TRACK - 1) * 2)?;
    // The BAM's own header carries the ID the rest must.
    let id = block_starts(bam_ring).into_iter().find_map(|start| {
        let header = read_block(bam_ring, start, usize::from(HEADER_GCR), HEADER_READ)?;
        (header[0] == HEADER_MARK && header[2] == BAM_SECTOR && header[3] == DIRECTORY_TRACK)
            .then_some([header[5], header[4]])
    })?;
    let mut image = Vec::with_capacity(sectors_before(tracks + 1) * SECTOR_BYTES);
    for (index, ring) in surface.tracks().iter().enumerate() {
        let track = u8::try_from(index / 2 + 1).ok()?;
        match ring {
            Some(ring) if index % 2 == 0 && track <= tracks => {
                for sector in read_track(ring, track, id)? {
                    image.extend_from_slice(&sector);
                }
            }
            Some(_) => return None,
            None if index % 2 == 0 && track <= tracks => return None,
            None => {}
        }
    }
    let id_at = sectors_before(DIRECTORY_TRACK) * SECTOR_BYTES + usize::from(bam::ID);
    (image.get(id_at..id_at + 2)? == id).then_some(image)
}
