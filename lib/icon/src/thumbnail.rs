//! A picture file drawn as its own content (`plans/FILES-INTERACTION.md`
//! FI12).
//!
//! A thumbnail is the request's own tier, so it falls to the file's class
//! picture whenever it will not serve. The file is streamed to the injected
//! rasteriser rather than read whole here, and the rasteriser decodes it in the
//! parser sandbox, fitted to the slot, so neither the read nor the decode costs
//! more than the file and the slot.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::fs::FileId;
use tairix_abi::time::Time64;
use tairix_raster::Surface;

use crate::artwork::{ArtworkRasteriser, ArtworkReader};

/// Largest picture file a thumbnail is drawn from, in bytes.
///
/// A fixed bound on untrusted input, not a capacity: a thumbnail is decoration
/// produced for every picture on screen, so a file past it draws its class
/// picture rather than costing a read and an upload of its whole length.
pub const MAX_THUMBNAIL_BYTES: u64 = 32 << 20;

/// How a thumbnail's file says what format it is.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reading {
    /// By the signature its bytes open with.
    Signature,
    /// As a RISC OS sprite area, which carries no signature and is read as one
    /// only because its name says it is.
    Sprite,
}

/// The file a thumbnail pictures, as its listing described it.
///
/// Keyed by the file's identity, size and modification time as well as its
/// path, so a changed or replaced file is decoded afresh rather than served
/// the picture another held.
#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct Thumbnail {
    /// The file's absolute path.
    pub path: String,
    /// Its length when it was listed.
    pub size: u64,
    /// Its modification time when it was listed.
    pub modified: Time64,
    /// The file it was when it was listed.
    pub id: FileId,
    /// How its format is read.
    pub reading: Reading,
}

/// What an open file reports of itself, checked against its listing before a
/// byte of it is read.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DocumentStamp {
    /// Its length.
    pub size: u64,
    /// Its modification time.
    pub modified: Time64,
    /// The file it is.
    pub id: FileId,
}

/// A file opened to stream a thumbnail from.
pub trait ArtworkDocument {
    /// What the open handle reports of the file.
    fn stamp(&self) -> DocumentStamp;

    /// Fill `into` from the file at `offset`, answering how many bytes were
    /// read, or `None` when the read fails.
    fn read_at(&mut self, offset: u64, into: &mut [u8]) -> Option<usize>;
}

/// The picture of `thumbnail` at `side` pixels, or `None` when the file is
/// gone, past [`MAX_THUMBNAIL_BYTES`], no longer what its listing described,
/// or refused by the rasteriser.
pub(crate) fn render_thumbnail<R, D>(
    reader: &mut R,
    rasteriser: &mut D,
    thumbnail: &Thumbnail,
    side: u32,
) -> Option<Surface>
where
    R: ArtworkReader + ?Sized,
    D: ArtworkRasteriser + ?Sized,
{
    // A listing that named no file names nothing an open could be checked
    // against.
    if side == 0 || thumbnail.id.is_none() || thumbnail.size > MAX_THUMBNAIL_BYTES {
        return None;
    }
    let mut document = reader.open(&thumbnail.path)?;
    // A file changed or replaced since it was listed is not the picture this
    // key names; its next listing asks under its new stamp.
    let stamp = document.stamp();
    if stamp.id != thumbnail.id
        || stamp.size != thumbnail.size
        || stamp.modified != thumbnail.modified
    {
        return None;
    }
    let pixels: Vec<u8> = rasteriser.thumbnail(side, thumbnail.reading, &mut *document)?;
    let expected = (side as usize)
        .checked_mul(side as usize)
        .and_then(|area| area.checked_mul(4))?;
    if pixels.len() != expected {
        return None;
    }
    Surface::from_rgba8(side, side, &pixels)
}

/// The production [`ArtworkDocument`]: a regular file opened for reading
/// under the caller's own identity, read at an offset.
///
/// One implementation for every surface that draws thumbnails, so the open and
/// the positional read are not re-derived beside each.
#[cfg(feature = "rt")]
pub struct RtDocument {
    file: tairix_rt::File,
    stamp: DocumentStamp,
}

#[cfg(feature = "rt")]
impl RtDocument {
    /// Open the file at `path`, or `None` when it is missing, unreadable, not
    /// a regular file, or a symbolic link: a name swapped for a link after it
    /// was listed is never followed.
    #[must_use]
    pub fn open(path: &str) -> Option<Self> {
        let flags = tairix_abi::fs::OpenFlags::READ.union(tairix_abi::fs::OpenFlags::NO_FOLLOW);
        let file = tairix_rt::File::open(path.as_bytes(), flags).ok()?;
        let stat = file.stat().ok()?;
        (stat.kind == tairix_abi::fs::FileKind::Regular).then_some(Self {
            file,
            stamp: DocumentStamp {
                size: stat.size,
                modified: stat.times.modified,
                id: stat.id,
            },
        })
    }
}

#[cfg(feature = "rt")]
impl ArtworkDocument for RtDocument {
    fn stamp(&self) -> DocumentStamp {
        self.stamp
    }

    fn read_at(&mut self, offset: u64, into: &mut [u8]) -> Option<usize> {
        self.file.read_at(offset, into).ok()
    }
}

#[cfg(test)]
#[path = "thumbnail_tests.rs"]
mod tests;
