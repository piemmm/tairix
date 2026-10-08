//! A picture file drawn as its own content (`plans/FILES-INTERACTION.md`
//! FI12).
//!
//! A thumbnail is the request's own tier, so it falls to the file's class
//! picture whenever it will not serve. The file is streamed to the injected
//! rasteriser rather than read whole here, and the rasteriser decodes it in the
//! parser sandbox, fitted to the slot, so neither the read nor the decode costs
//! more than the file and the slot.

use alloc::string::String;

use tairix_abi::fs::FileId;
use tairix_abi::time::Time64;
use tairix_raster::Surface;

use crate::artwork::{ArtworkRasteriser, ArtworkReader};
use crate::picture::Artwork;

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
/// Keyed by the version of the file its listing named as well as its path, so
/// a changed or replaced file is decoded afresh rather than served the picture
/// another held.
#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct Thumbnail {
    /// The file's absolute path.
    pub path: String,
    /// The version of the file the listing named.
    pub stamp: DocumentStamp,
    /// How its format is read.
    pub reading: Reading,
}

/// Which version of which file: what a listing reports of a file, and what an
/// open of it must report for a byte of it to be read.
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq, PartialOrd, Ord)]
pub struct DocumentStamp {
    /// Its length.
    pub size: u64,
    /// Its modification time.
    pub modified: Time64,
    /// The file it is.
    pub id: FileId,
    /// The version of its data, `0` where the volume keeps none.
    pub content_gen: u64,
}

/// A file opened to stream a thumbnail from.
pub trait ArtworkDocument {
    /// What the open handle reported of the file when it was opened.
    fn stamp(&self) -> DocumentStamp;

    /// What the open handle reports of the file now, or `None` when it cannot
    /// say — which a caller asking whether a read saw one version takes as no.
    fn restamp(&mut self) -> Option<DocumentStamp> {
        None
    }

    /// Fill `into` from the file at `offset`, answering how many bytes were
    /// read, or `None` when the read fails.
    fn read_at(&mut self, offset: u64, into: &mut [u8]) -> Option<usize>;
}

/// The picture of `thumbnail` at `side` pixels, framed at the bounds the
/// rasteriser fitted it to, or `None` when the file is gone, past
/// [`MAX_THUMBNAIL_BYTES`], no longer what its listing described, or refused
/// by the rasteriser.
pub(crate) fn render_thumbnail<R, D>(
    reader: &mut R,
    rasteriser: &mut D,
    thumbnail: &Thumbnail,
    side: u32,
) -> Option<Artwork>
where
    R: ArtworkReader + ?Sized,
    D: ArtworkRasteriser + ?Sized,
{
    // A listing that named no file names nothing an open could be checked
    // against.
    if side == 0 || thumbnail.stamp.id.is_none() || thumbnail.stamp.size > MAX_THUMBNAIL_BYTES {
        return None;
    }
    let mut document = reader.open(&thumbnail.path)?;
    // A file changed or replaced since it was listed is not the picture this
    // key names; its next listing asks under its new stamp.
    if document.stamp() != thumbnail.stamp {
        return None;
    }
    let fitted = rasteriser.thumbnail(side, thumbnail.reading, &mut *document)?;
    let expected = (side as usize)
        .checked_mul(side as usize)
        .and_then(|area| area.checked_mul(4))?;
    if fitted.pixels.len() != expected {
        return None;
    }
    Artwork::framed(
        Surface::from_rgba8(side, side, &fitted.pixels)?,
        fitted.bounds,
    )
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
            stamp: stamp_of(&stat),
        })
    }
}

/// The version of the file `stat` describes.
#[cfg(feature = "rt")]
fn stamp_of(stat: &tairix_abi::fs::FileStat) -> DocumentStamp {
    DocumentStamp {
        size: stat.size,
        modified: stat.times.modified,
        id: stat.id,
        content_gen: stat.content_gen,
    }
}

#[cfg(feature = "rt")]
impl ArtworkDocument for RtDocument {
    fn stamp(&self) -> DocumentStamp {
        self.stamp
    }

    fn restamp(&mut self) -> Option<DocumentStamp> {
        self.file.stat().ok().map(|stat| stamp_of(&stat))
    }

    fn read_at(&mut self, offset: u64, into: &mut [u8]) -> Option<usize> {
        self.file.read_at(offset, into).ok()
    }
}

/// The production [`StoreFile`](crate::store::StoreFile): the blob the
/// app-data service delegated, read and written at an offset.
#[cfg(feature = "rt")]
impl crate::store::StoreFile for tairix_rt::File {
    fn read_exact_at(&mut self, offset: u64, into: &mut [u8]) -> bool {
        let mut done = 0;
        while done < into.len() {
            let Some(at) = offset.checked_add(done as u64) else {
                return false;
            };
            match tairix_rt::File::read_at(self, at, &mut into[done..]) {
                Ok(0) | Err(_) => return false,
                Ok(read) => done += read,
            }
        }
        true
    }

    fn write_all_at(&mut self, offset: u64, from: &[u8]) -> bool {
        let mut done = 0;
        while done < from.len() {
            let Some(at) = offset.checked_add(done as u64) else {
                return false;
            };
            match tairix_rt::File::write_at(self, at, &from[done..]) {
                Ok(0) | Err(_) => return false,
                Ok(written) => done += written,
            }
        }
        true
    }

    fn set_len(&mut self, len: u64) -> bool {
        self.truncate(len).is_ok()
    }

    fn byte_len(&mut self) -> Option<u64> {
        tairix_rt::File::stat(self).ok().map(|stat| stat.size)
    }
}

#[cfg(test)]
#[path = "thumbnail_tests.rs"]
mod tests;
