//! Disks as a 1541 sees them: 84 half-tracks, each a ring of GCR recording
//! a revolution long at the rate its zone was written at — 307,692 bits a
//! second at the outermost, 250,000 at the innermost, 300 revolutions a
//! minute — and nothing recorded on the half-tracks between whole ones.
//!
//! D64 images, the 1541's sector dumps (35 or 40 tracks, with or without a
//! byte of error code a sector), are recorded as the drive formats a disk,
//! their errors written in. G64 images carry each half-track's recording as
//! it is. A disk written since it was loaded goes back as a D64 where every
//! sector still decodes, and as a G64 otherwise.

pub mod d64;
pub mod g64;
pub mod gcr;
pub mod layout;
pub mod recording;

use alloc::vec;
use alloc::vec::Vec;

use crate::snapshot::{self, Reader, SnapshotError, Writer};

/// Half-tracks a 1541's head can reach: tracks 1 to 42.
pub const HALF_TRACKS: usize = 84;

/// The longest ring of recording a disk may carry, in bytes: a revolution at
/// the outermost zone's rate with nearly a fifth to spare for a slow-spinning
/// mastering drive.
pub const MAX_TRACK_BYTES: usize = 0x2400;

/// Why an image was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskError {
    /// Not a length any D64 has.
    BadSize,
    /// Not a G64: its signature or version is wrong.
    BadSignature,
    /// A field out of its bounds, or a table pointing outside the image.
    Malformed,
}

/// One half-track's ring of recording, a bit a cell, the first bit the
/// first byte's top bit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    bytes: Vec<u8>,
    /// The zone it was written at, 0 innermost to 3 outermost.
    zone: u8,
}

impl Track {
    /// A ring of `bytes`, written at `zone`.
    ///
    /// # Errors
    ///
    /// [`DiskError::Malformed`] for an empty ring, one past
    /// [`MAX_TRACK_BYTES`], or a zone past 3.
    pub fn new(bytes: Vec<u8>, zone: u8) -> Result<Self, DiskError> {
        if bytes.is_empty() || bytes.len() > MAX_TRACK_BYTES || zone > 3 {
            return Err(DiskError::Malformed);
        }
        Ok(Self { bytes, zone })
    }

    /// Bits in the ring.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.bytes.len() * 8
    }

    /// Whether the ring holds nothing; never, by construction.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The zone the ring was written at.
    #[must_use]
    pub const fn zone(&self) -> u8 {
        self.zone
    }

    /// Bit `index`, wrapping round the ring.
    #[must_use]
    pub fn bit(&self, index: usize) -> bool {
        let index = index % self.len();
        self.bytes[index / 8] & (0x80 >> (index % 8)) != 0
    }

    /// A revolution of nothing at `zone`'s density.
    fn blank(zone: u8) -> Self {
        Self {
            bytes: vec![0; d64::revolution_bytes(zone)],
            zone,
        }
    }

    /// The ring as a write at `zone` leaves it: a revolution at that zone's
    /// density, each cell holding what the old ring held at the same angle.
    fn respace(&mut self, zone: u8) {
        let mut respaced = Self::blank(zone);
        let (from, to) = (self.len(), respaced.len());
        for cell in 0..to {
            if self.bit(cell * from / to) {
                respaced.set_bit(cell, true);
            }
        }
        *self = respaced;
    }

    /// Cells from `start` round the ring to the next bit set, `start`'s
    /// own counting as none; `None` for a ring with no bit set. Whole bytes
    /// of zeros are passed over at once.
    #[must_use]
    pub fn cells_to_set_bit(&self, start: usize) -> Option<usize> {
        let start = start % self.len();
        let (first, offset) = (start / 8, start % 8);
        let rest = self.bytes.get(first)? & (0xFF >> offset);
        if rest != 0 {
            return Some(rest.leading_zeros() as usize - offset);
        }
        let count = self.bytes.len();
        (1..=count).find_map(|step| {
            let byte = self.bytes.get((first + step) % count)?;
            (*byte != 0).then(|| step * 8 + byte.leading_zeros() as usize - offset)
        })
    }

    /// Set bit `index`, wrapping round the ring.
    pub fn set_bit(&mut self, index: usize, value: bool) {
        let index = index % self.len();
        let mask = 0x80 >> (index % 8);
        if value {
            self.bytes[index / 8] |= mask;
        } else {
            self.bytes[index / 8] &= !mask;
        }
    }

    /// The ring's bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// What an image was loaded from, which decides how it is written back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// A D64 of this many tracks.
    D64 {
        /// 35 or 40.
        tracks: u8,
    },
    /// A G64.
    G64,
}

/// A disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Surface {
    tracks: Vec<Option<Track>>,
    /// The write-protect notch is covered.
    pub write_protected: bool,
    format: Format,
    /// A track has been written since the disk was loaded or last saved.
    dirty: bool,
}

impl Surface {
    /// A disk of these half-tracks, loaded from `format`.
    ///
    /// # Errors
    ///
    /// [`DiskError::Malformed`] for other than [`HALF_TRACKS`] half-tracks.
    pub fn new(tracks: Vec<Option<Track>>, format: Format) -> Result<Self, DiskError> {
        if tracks.len() != HALF_TRACKS {
            return Err(DiskError::Malformed);
        }
        Ok(Self {
            tracks,
            write_protected: false,
            format,
            dirty: false,
        })
    }

    /// A disk with nothing recorded on it.
    #[must_use]
    pub fn blank() -> Self {
        Self {
            tracks: vec![None; HALF_TRACKS],
            write_protected: false,
            format: Format::D64 { tracks: 35 },
            dirty: false,
        }
    }

    /// Half-track `half_track`'s recording, if any.
    #[must_use]
    pub fn track(&self, half_track: usize) -> Option<&Track> {
        self.tracks.get(half_track).and_then(Option::as_ref)
    }

    /// Half-track `half_track`'s recording, to write: a half-track never
    /// recorded gets a ring of `zone`'s length, blank.
    pub fn track_to_write(&mut self, half_track: usize, zone: u8) -> Option<&mut Track> {
        let slot = self.tracks.get_mut(half_track)?;
        self.dirty = true;
        let zone = zone.min(3);
        let track = slot.get_or_insert_with(|| Track::blank(zone));
        if track.zone != zone {
            track.respace(zone);
        }
        Some(track)
    }

    /// Whether a track has been written since the disk was loaded or saved.
    #[must_use]
    pub const fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The image the disk is saved as: a D64 of its own track count while
    /// every sector decodes, a G64 otherwise.
    #[must_use]
    pub fn encode(&self) -> (Format, Vec<u8>) {
        if let Format::D64 { tracks } = self.format {
            if let Some(image) = d64::encode(self, tracks) {
                return (self.format, image);
            }
        }
        (Format::G64, g64::encode(self))
    }

    /// Mark the disk saved as it now stands.
    pub const fn mark_saved(&mut self) {
        self.dirty = false;
    }

    /// Every half-track, innermost last.
    pub(crate) fn tracks(&self) -> &[Option<Track>] {
        &self.tracks
    }
}

impl snapshot::Save for Track {
    fn save(&self, w: &mut Writer) {
        w.bytes(&self.bytes);
        w.put(&self.zone);
    }
}

impl snapshot::Load for Track {
    fn load(r: &mut Reader<'_>) -> Result<Self, SnapshotError> {
        let bytes = r.bytes_at_most(MAX_TRACK_BYTES)?.to_vec();
        Self::new(bytes, r.get()?).map_err(|_| SnapshotError::Malformed)
    }
}

/// What a snapshot records a G64 as; a D64 is its track count.
const G64_TAG: u8 = 0;

impl snapshot::Save for Format {
    fn save(&self, w: &mut Writer) {
        w.put(&match self {
            Self::D64 { tracks } => *tracks,
            Self::G64 => G64_TAG,
        });
    }
}

impl snapshot::Load for Format {
    fn load(r: &mut Reader<'_>) -> Result<Self, SnapshotError> {
        match r.get::<u8>()? {
            G64_TAG => Ok(Self::G64),
            tracks @ (35 | 40) => Ok(Self::D64 { tracks }),
            _ => Err(SnapshotError::Malformed),
        }
    }
}

impl snapshot::Save for Surface {
    fn save(&self, w: &mut Writer) {
        for track in &self.tracks {
            w.put(track);
        }
        w.put(&self.write_protected);
        w.put(&self.format);
        w.put(&self.dirty);
    }
}

impl snapshot::Load for Surface {
    fn load(r: &mut Reader<'_>) -> Result<Self, SnapshotError> {
        let mut tracks = Vec::with_capacity(HALF_TRACKS);
        for _ in 0..HALF_TRACKS {
            tracks.push(r.get::<Option<Track>>()?);
        }
        Ok(Self {
            tracks,
            write_protected: r.get()?,
            format: r.get()?,
            dirty: r.get()?,
        })
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
