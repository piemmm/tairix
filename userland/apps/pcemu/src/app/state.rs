//! What the store keeps of a machine beside its record and its disks: the
//! last screen it showed, compressed, which its card in the library window is
//! drawn from, and, while it is suspended, its snapshot and when it was
//! suspended.
//!
//! A header names both parts and is checked on its own, and each part is
//! checked on its own, so a card's screen is read without reading the
//! snapshot behind it.

use alloc::vec::Vec;
use core::ops::Range;

use crate::link::layout::PIXEL;
use crate::link::most_kept;
use crate::video::LARGEST_FRAME;

const MAGIC: u32 = u32::from_le_bytes(*b"PCST");
const VERSION: u16 = 2;
/// Magic, version, the frame's dots and lines, the screen's and the
/// snapshot's lengths, whether and when it was suspended, and the header's
/// own checksum.
pub const HEAD: usize = 4 + 2 + 2 + 2 + 4 + 4 + 1 + 8 + 4;
const CRC: usize = 4;

/// A machine's last screen: `width` by `height` dots of red, green, blue and
/// alpha.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastScreen {
    /// Its dots.
    pub width: u32,
    /// Its lines.
    pub height: u32,
    /// Its pixels.
    pub pixels: Vec<u8>,
}

/// A suspended machine's snapshot, as its worker kept it, and when it was
/// suspended by the host's calendar, in seconds since 1970, if the host
/// knew.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suspended {
    /// The snapshot.
    pub kept: Vec<u8>,
    /// When it was suspended.
    pub at: Option<i64>,
}

/// What a state's header says of the parts behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Head {
    width: u32,
    height: u32,
    screen: usize,
    kept: usize,
    at: Option<i64>,
}

impl Head {
    /// Whether the machine is suspended: a snapshot lies behind its screen.
    #[must_use]
    pub const fn suspended(&self) -> bool {
        self.kept > 0
    }

    /// Where the screen and its checksum lie in the state.
    #[must_use]
    pub const fn screen_span(&self) -> Range<usize> {
        HEAD..HEAD + self.screen + CRC
    }

    /// The bytes of the whole state.
    #[must_use]
    pub const fn size(&self) -> usize {
        HEAD + self.screen + CRC + if self.kept > 0 { self.kept + CRC } else { 0 }
    }
}

/// Why a state was not kept or not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateError {
    /// Not a frame any monitor shows, a snapshot larger than any, or a
    /// state cut short or corrupted.
    Malformed,
    /// The heap would not hold it.
    OutOfMemory,
}

/// Bytes a frame of `width` by `height` dots takes, for a frame some monitor
/// shows.
fn frame_bytes(width: u32, height: u32) -> Option<usize> {
    let (most_w, most_h) = LARGEST_FRAME;
    if width == 0 || height == 0 || width > most_w || height > most_h {
        return None;
    }
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(PIXEL)
}

/// The most bytes a state's screen takes: the largest frame, stored however
/// badly it compresses.
fn most_screen() -> usize {
    tairix_compress::max_compressed_len(frame_bytes(LARGEST_FRAME.0, LARGEST_FRAME.1).unwrap_or(0))
}

/// The most bytes a state blob holds: the largest screen and the largest
/// snapshot.
#[must_use]
pub fn most_bytes() -> usize {
    HEAD + most_screen() + CRC + most_kept() + CRC
}

/// The state of a machine whose last screen was `screen`, and which is
/// `suspended` with that snapshot, if it is.
///
/// # Errors
///
/// [`StateError::Malformed`] for a frame no monitor shows, pixels not its
/// size, or a snapshot empty or larger than any; [`StateError::OutOfMemory`].
pub fn encode(screen: &LastScreen, suspended: Option<&Suspended>) -> Result<Vec<u8>, StateError> {
    let malformed = StateError::Malformed;
    let len = frame_bytes(screen.width, screen.height).ok_or(malformed)?;
    if screen.pixels.len() != len {
        return Err(malformed);
    }
    let kept = suspended.map_or(0, |suspended| suspended.kept.len());
    if suspended.is_some() && !(1..=most_kept()).contains(&kept) {
        return Err(malformed);
    }
    let bound = tairix_compress::max_compressed_len(len);
    let total = HEAD + bound + CRC + if kept > 0 { kept + CRC } else { 0 };
    let mut out = Vec::new();
    out.try_reserve_exact(total)
        .map_err(|_| StateError::OutOfMemory)?;
    out.resize(HEAD + bound, 0);
    let packed =
        tairix_compress::compress(&screen.pixels, &mut out[HEAD..]).map_err(|_| malformed)?;
    out.truncate(HEAD + packed);
    let sum = tairix_crc32::checksum(&out[HEAD..]);
    out.extend_from_slice(&sum.to_le_bytes());
    if let Some(suspended) = suspended {
        out.extend_from_slice(&suspended.kept);
        out.extend_from_slice(&tairix_crc32::checksum(&suspended.kept).to_le_bytes());
    }
    let dims = |n: u32| u16::try_from(n).map_err(|_| malformed);
    let lengths = |n: usize| u32::try_from(n).map_err(|_| malformed);
    let at = suspended.and_then(|suspended| suspended.at);
    let mut head = [0u8; HEAD];
    head[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    head[4..6].copy_from_slice(&VERSION.to_le_bytes());
    head[6..8].copy_from_slice(&dims(screen.width)?.to_le_bytes());
    head[8..10].copy_from_slice(&dims(screen.height)?.to_le_bytes());
    head[10..14].copy_from_slice(&lengths(packed)?.to_le_bytes());
    head[14..18].copy_from_slice(&lengths(kept)?.to_le_bytes());
    head[18] = u8::from(at.is_some());
    head[19..27].copy_from_slice(&at.unwrap_or(0).to_le_bytes());
    let sum = tairix_crc32::checksum(&head[..HEAD - CRC]);
    head[HEAD - CRC..].copy_from_slice(&sum.to_le_bytes());
    out[..HEAD].copy_from_slice(&head);
    Ok(out)
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The part `section` holds, its checksum last, if it checks out.
fn checked(section: &[u8]) -> Option<&[u8]> {
    let len = section.len().checked_sub(CRC)?;
    let (body, sum) = section.split_at(len);
    (u32_at(sum, 0) == Some(tairix_crc32::checksum(body))).then_some(body)
}

/// What the header that `bytes` begins with says.
///
/// # Errors
///
/// [`StateError::Malformed`] for a header cut short, corrupted or of another
/// version, or one naming parts no state could hold.
pub fn head(bytes: &[u8]) -> Result<Head, StateError> {
    let malformed = StateError::Malformed;
    let head = checked(bytes.get(..HEAD).ok_or(malformed)?).ok_or(malformed)?;
    if u32_at(head, 0) != Some(MAGIC) || u16_at(head, 4) != Some(VERSION) {
        return Err(malformed);
    }
    let width = u32::from(u16_at(head, 6).ok_or(malformed)?);
    let height = u32::from(u16_at(head, 8).ok_or(malformed)?);
    let length = |at: usize| -> Result<usize, StateError> {
        usize::try_from(u32_at(head, at).ok_or(malformed)?).map_err(|_| malformed)
    };
    let (screen, kept) = (length(10)?, length(14)?);
    frame_bytes(width, height).ok_or(malformed)?;
    if screen > most_screen() || kept > most_kept() {
        return Err(malformed);
    }
    let at = match head[18] {
        0 => None,
        1 => Some(i64::from_le_bytes(
            head[19..27].try_into().map_err(|_| malformed)?,
        )),
        _ => return Err(malformed),
    };
    Ok(Head {
        width,
        height,
        screen,
        kept,
        at,
    })
}

/// The last screen the part of a state at `head`'s
/// [`screen_span`](Head::screen_span) holds.
///
/// # Errors
///
/// [`StateError::Malformed`] for a part that does not check out or does not
/// hold the frame its header says; [`StateError::OutOfMemory`].
pub fn screen(head: &Head, section: &[u8]) -> Result<LastScreen, StateError> {
    let malformed = StateError::Malformed;
    if section.len() != head.screen + CRC {
        return Err(malformed);
    }
    let packed = checked(section).ok_or(malformed)?;
    let len = frame_bytes(head.width, head.height).ok_or(malformed)?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(len)
        .map_err(|_| StateError::OutOfMemory)?;
    pixels.resize(len, 0);
    match tairix_compress::decompress(packed, &mut pixels) {
        Ok(n) if n == len => Ok(LastScreen {
            width: head.width,
            height: head.height,
            pixels,
        }),
        _ => Err(malformed),
    }
}

/// The whole of a state `bytes` holds: its last screen, and its snapshot if
/// it is suspended.
///
/// # Errors
///
/// [`StateError::Malformed`] for anything but a whole state that checks out;
/// [`StateError::OutOfMemory`].
pub fn decode(bytes: &[u8]) -> Result<(LastScreen, Option<Suspended>), StateError> {
    let malformed = StateError::Malformed;
    let head = head(bytes)?;
    if bytes.len() != head.size() {
        return Err(malformed);
    }
    let shown = screen(&head, &bytes[head.screen_span()])?;
    if !head.suspended() {
        return Ok((shown, None));
    }
    let body = checked(&bytes[head.screen_span().end..]).ok_or(malformed)?;
    let mut kept = Vec::new();
    kept.try_reserve_exact(body.len())
        .map_err(|_| StateError::OutOfMemory)?;
    kept.extend_from_slice(body);
    Ok((shown, Some(Suspended { kept, at: head.at })))
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
