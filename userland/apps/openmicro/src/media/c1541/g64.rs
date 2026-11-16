//! G64 images: each half-track's GCR recording as it is, so copy protection
//! and anything else no sector dump holds survives.
//!
//! The image opens with `GCR-1541`, a version (0), the half-tracks it holds
//! and the longest track's size; then a table of each half-track's offset (0
//! for none) and one of its zone, each four bytes, little-endian; each track
//! its length in two bytes, then its bytes. A zone table entry past 3 points
//! at a zone a byte; the track is taken at its standard zone, as every
//! track here is one ring at one rate.

use alloc::vec::Vec;

use super::{d64, DiskError, Format, Surface, Track, HALF_TRACKS, MAX_TRACK_BYTES};

/// The signature an image opens with.
const SIGNATURE: &[u8; 8] = b"GCR-1541";
/// The header: signature, version, half-tracks, longest track.
const HEADER: usize = 12;
/// The size every track slot is written at, unless a track is longer.
const SLOT_BYTES: usize = 7928;
/// The largest image decoded: every half-track the longest a ring may be,
/// as [`encode`] writes it.
pub const MAX_IMAGE: usize = HEADER + 8 * HALF_TRACKS + HALF_TRACKS * (2 + MAX_TRACK_BYTES);

/// The field of `N` bytes at `at`, taken without adding to an offset a file
/// gave, which a narrow `usize` could not hold.
fn field<const N: usize>(bytes: &[u8], at: usize) -> Result<&[u8], DiskError> {
    bytes
        .get(at..)
        .and_then(|rest| rest.get(..N))
        .ok_or(DiskError::Malformed)
}

fn le16(bytes: &[u8], at: usize) -> Result<usize, DiskError> {
    let field = field::<2>(bytes, at)?;
    Ok(usize::from(u16::from_le_bytes([field[0], field[1]])))
}

fn le32(bytes: &[u8], at: usize) -> Result<usize, DiskError> {
    let field = field::<4>(bytes, at)?;
    let value = u32::from_le_bytes([field[0], field[1], field[2], field[3]]);
    usize::try_from(value).map_err(|_| DiskError::Malformed)
}

/// The standard zone of half-track `half_track`.
fn standard_zone(half_track: usize) -> u8 {
    d64::zone(u8::try_from(half_track / 2 + 1).unwrap_or(u8::MAX))
}

/// A G64 image's disk.
///
/// # Errors
///
/// [`DiskError::BadSignature`] for another signature or version, and
/// [`DiskError::Malformed`] for an image past [`MAX_IMAGE`], or a table or
/// track outside the image or its bounds.
pub fn decode(image: &[u8]) -> Result<Surface, DiskError> {
    if image.get(..SIGNATURE.len()) != Some(SIGNATURE) || image.get(8) != Some(&0) {
        return Err(DiskError::BadSignature);
    }
    if image.len() > MAX_IMAGE {
        return Err(DiskError::Malformed);
    }
    let count = usize::from(*image.get(9).ok_or(DiskError::Malformed)?);
    let longest = le16(image, 10)?;
    if count > HALF_TRACKS || longest > MAX_TRACK_BYTES {
        return Err(DiskError::Malformed);
    }
    let mut tracks = alloc::vec![None; HALF_TRACKS];
    for (half_track, slot) in tracks.iter_mut().enumerate().take(count) {
        let offset = le32(image, HEADER + 4 * half_track)?;
        if offset == 0 {
            continue;
        }
        let zone_entry = le32(image, HEADER + 4 * (count + half_track))?;
        let len = le16(image, offset)?;
        if len > longest {
            return Err(DiskError::Malformed);
        }
        let bytes = image
            .get(offset..)
            .and_then(|rest| rest.get(2..)?.get(..len))
            .ok_or(DiskError::Malformed)?;
        let zone = u8::try_from(zone_entry)
            .ok()
            .filter(|&zone| zone <= 3)
            .unwrap_or_else(|| standard_zone(half_track));
        *slot = Some(Track::new(bytes.to_vec(), zone)?);
    }
    Surface::new(tracks, Format::G64)
}

/// `surface` as a G64 image.
#[must_use]
pub fn encode(surface: &Surface) -> Vec<u8> {
    let tracks = surface.tracks();
    let slot = tracks
        .iter()
        .flatten()
        .map(|track| track.bytes().len())
        .fold(SLOT_BYTES, usize::max);
    let tables = HEADER + 8 * HALF_TRACKS;
    let mut image = Vec::with_capacity(tables + tracks.iter().flatten().count() * (slot + 2));
    image.extend_from_slice(SIGNATURE);
    image.push(0);
    image.push(u8::try_from(HALF_TRACKS).unwrap_or(u8::MAX));
    image.extend(u16::try_from(slot).unwrap_or(u16::MAX).to_le_bytes());
    let mut offset = tables;
    let mut offsets = Vec::with_capacity(HALF_TRACKS);
    for track in tracks {
        offsets.push(if track.is_some() { offset } else { 0 });
        if track.is_some() {
            offset += 2 + slot;
        }
    }
    for at in &offsets {
        image.extend(u32::try_from(*at).unwrap_or(u32::MAX).to_le_bytes());
    }
    for (half_track, track) in tracks.iter().enumerate() {
        let zone = track
            .as_ref()
            .map_or_else(|| standard_zone(half_track), Track::zone);
        image.extend(u32::from(zone).to_le_bytes());
    }
    for track in tracks.iter().flatten() {
        let bytes = track.bytes();
        image.extend(u16::try_from(bytes.len()).unwrap_or(u16::MAX).to_le_bytes());
        image.extend_from_slice(bytes);
        image.resize(image.len() + slot - bytes.len(), 0);
    }
    image
}
