//! Which way up a stored raster is, and where that puts each of its pixels.
//!
//! TIFF's `Orientation` tag and the EXIF attribute a JPEG carries in its
//! `APP1` segment are the same tag number with the same eight values, so the
//! map from a stored position to the position it presents at is one
//! definition both formats read rather than two that can drift apart.
//!
//! Applying an orientation is a permutation of positions: nothing is done to
//! a pixel, so a decoder places each one as it produces it and never holds a
//! second copy of the picture.

use crate::density::Density;
use crate::ifd::{self, Endian, ENTRY_LEN, LONG, MAGIC, SHORT};

/// The `Orientation` tag, in both formats that carry one.
pub(crate) const TAG_ORIENTATION: u16 = 274;

/// The marker segment identifier that introduces an EXIF attribute block.
pub(crate) const EXIF_IDENTIFIER: &[u8] = b"Exif\0\0";

/// Which edges of the picture the stored raster's first row and first column
/// lie along, as tag 274 numbers the eight possibilities.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Orientation(u32);

impl Orientation {
    /// The stored raster is already the picture: first row at the top, first
    /// column on the left.
    pub const IDENTITY: Self = Self(1);

    /// The orientation tag 274 spells as `code`, or `None` for a value
    /// outside the eight it defines.
    #[must_use]
    pub const fn from_tag(code: u32) -> Option<Self> {
        if matches!(code, 1..=8) {
            Some(Self(code))
        } else {
            None
        }
    }

    /// The value tag 274 spells it as.
    #[must_use]
    pub const fn tag(self) -> u32 {
        self.0
    }

    /// Whether the picture's axes are the stored raster's two swapped.
    #[must_use]
    pub const fn transposes(self) -> bool {
        self.0 >= 5
    }

    /// The map from a `stored` raster's coordinates onto the picture's, a
    /// pixel to the unit and the first row at the top: where each stored
    /// pixel is placed, over the continuous plane, for a writer that keeps
    /// the raster as it is stored and turns it as it is drawn.
    #[must_use]
    pub fn to_picture(self, stored: (u32, u32)) -> tairix_raster::Affine {
        let (width, height) = self.picture_size(stored.0, stored.1);
        let (across, down) = (f64::from(width), f64::from(height));
        // Where the stored raster's x and y axes point in the picture, and
        // where its origin lands.
        let (x_axis, y_axis, origin) = match self.0 {
            2 => ((-1.0, 0.0), (0.0, 1.0), (across, 0.0)),
            3 => ((-1.0, 0.0), (0.0, -1.0), (across, down)),
            4 => ((1.0, 0.0), (0.0, -1.0), (0.0, down)),
            5 => ((0.0, 1.0), (1.0, 0.0), (0.0, 0.0)),
            6 => ((0.0, 1.0), (-1.0, 0.0), (across, 0.0)),
            7 => ((0.0, -1.0), (-1.0, 0.0), (across, down)),
            8 => ((0.0, -1.0), (1.0, 0.0), (0.0, down)),
            _ => ((1.0, 0.0), (0.0, 1.0), (0.0, 0.0)),
        };
        tairix_raster::Affine {
            a: x_axis.0,
            b: x_axis.1,
            c: y_axis.0,
            d: y_axis.1,
            e: origin.0,
            f: origin.1,
        }
    }

    /// The picture a `width`×`height` stored raster presents as.
    #[must_use]
    pub const fn picture_size(self, width: u32, height: u32) -> (u32, u32) {
        if self.transposes() {
            (height, width)
        } else {
            (width, height)
        }
    }

    /// The density of the picture a raster stated at `density` presents as.
    pub(crate) const fn picture_density(self, density: Density) -> Density {
        if self.transposes() {
            density.transposed()
        } else {
            density
        }
    }

    /// Where the stored pixel at `(x, y)` lands, given the **picture's**
    /// `width` and `height` — the stored raster's two swapped whenever this
    /// orientation transposes.
    ///
    /// Saturating, so a coordinate outside the picture yields an edge
    /// position rather than wrapping or panicking; callers bound their own
    /// loops by the geometry they validated, so that never arises.
    pub(crate) const fn place(self, x: u32, y: u32, width: u32, height: u32) -> (u32, u32) {
        let last_x = width.saturating_sub(1);
        let last_y = height.saturating_sub(1);
        match self.0 {
            2 => (last_x.saturating_sub(x), y),
            3 => (last_x.saturating_sub(x), last_y.saturating_sub(y)),
            4 => (x, last_y.saturating_sub(y)),
            5 => (y, x),
            6 => (last_x.saturating_sub(y), x),
            7 => (last_x.saturating_sub(y), last_y.saturating_sub(x)),
            8 => (y, last_y.saturating_sub(x)),
            _ => (x, y),
        }
    }
}

/// The orientation an EXIF attribute block states, if it states a usable one.
///
/// `payload` is a marker segment's contents, including the `Exif\0\0`
/// identifier that introduces one.
///
/// Metadata is advisory, so this answers `None` for a block that is absent,
/// truncated, malformed, or carries a value outside the eight the tag
/// defines, and the picture is then used as stored. That is deliberately
/// unlike TIFF, where the same tag sits in the directory describing the
/// pixels being decoded and a bad value means the file cannot be read at
/// all: here a camera's malformed metadata must not cost a reader the
/// photograph.
pub(crate) fn from_exif(payload: &[u8]) -> Option<Orientation> {
    let header = payload.strip_prefix(EXIF_IDENTIFIER)?;
    let endian = Endian::of(header)?;
    if endian.u16(header, 2)? != MAGIC {
        return None;
    }

    // Offsets are relative to the start of the TIFF header, which is where
    // `header` begins.
    let directory = usize::try_from(endian.u32(header, 4)?).ok()?;
    let count = usize::from(endian.u16(header, directory)?);
    let entries = directory.checked_add(2)?;
    // An entry count is a declaration, not a guarantee: walk only the
    // entries the segment actually holds.
    let available = header.len().saturating_sub(entries) / ENTRY_LEN;

    (0..count.min(available)).find_map(|index| {
        let entry = ifd::entry(header, endian, entries + index * ENTRY_LEN)?;
        if entry.tag != TAG_ORIENTATION || entry.count != 1 {
            return None;
        }
        let values = entry.values?;
        let value = match entry.kind {
            SHORT => u32::from(endian.u16(header, values.at)?),
            LONG => endian.u32(header, values.at)?,
            _ => return None,
        };
        Orientation::from_tag(value)
    })
}

#[cfg(test)]
#[path = "orientation_tests.rs"]
mod tests;
