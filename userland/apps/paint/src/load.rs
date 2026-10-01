//! Assembling a document from what the sandboxed decoder answered.
//!
//! Every answer has already been held to the edit decode's bounds by the
//! seam that received it; this turns them into canvases and entries, and
//! refuses a document whose answers do not add up.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_sandbox::imageedit::{EditDocument, EditKept, EditPicture, EditPixels};

use crate::canvas::{CanvasBuilder, CanvasError, Kind, OutOfMemory, Sample};
use crate::document::{Document, Entry, Kept, Origin, Picture, SpriteInfo};

/// Why an entry could not join the document.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// What the entry needs could not be held.
    NoMemory,
    /// What the decoder described does not add up: more entries than the
    /// document declared, or sprites needing more of the file than it holds.
    Unbelieved,
}

/// A document being put together, one entry at a time.
#[derive(Debug)]
pub struct Assembly {
    entries: Vec<Entry>,
    opened: EditDocument,
    /// Bytes of the file the entries so far have not been charged. A sprite
    /// area's sprites lie end to end within its file, so one that would need
    /// more than is left was described by a decoder that cannot be believed,
    /// and is refused before anything is allocated for it.
    unclaimed: u64,
    overrun: bool,
}

impl Assembly {
    /// Room for the entries `opened` says a file `length` bytes long holds.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the room is refused.
    pub fn new(opened: EditDocument, length: usize) -> Result<Self, OutOfMemory> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(opened.count as usize)
            .map_err(|_| OutOfMemory)?;
        Ok(Self {
            entries,
            opened,
            unclaimed: u64::try_from(length).unwrap_or(u64::MAX),
            overrun: false,
        })
    }

    /// A builder for the rows of `picture`, which [`picture`](Self::picture)
    /// takes once they are all in. A sprite is first charged the least of
    /// the file it could be stored in.
    ///
    /// # Errors
    ///
    /// [`Refusal`]: the sprite needs more of the file than is left, or the
    /// picture cannot be held.
    pub fn canvas_for(&mut self, picture: &EditPicture) -> Result<CanvasBuilder, Refusal> {
        if let Some(sprite) = picture.sprite() {
            let stored = sprite
                .mode
                .layout()
                .least_stored_bytes(picture.width(), picture.height());
            self.claim(stored.ok_or(Refusal::Unbelieved)?)?;
        }
        let (kind, fill) = match picture.pixels() {
            EditPixels::Rgba => (Kind::Rgba, Sample::Rgba([0; 4])),
            EditPixels::Indexed {
                depth,
                palette,
                plane,
            } => {
                let palette =
                    tairix_util::fallible::collected(palette.len(), palette.iter().copied())
                        .ok_or(Refusal::NoMemory)?;
                (
                    Kind::Indexed {
                        depth: *depth,
                        palette,
                        masked: *plane,
                    },
                    Sample::Index(0, u8::MAX),
                )
            }
        };
        CanvasBuilder::new(picture.width(), picture.height(), kind, fill).map_err(|err| match err {
            CanvasError::OutOfMemory => Refusal::NoMemory,
            CanvasError::BadSize | CanvasError::BadPalette => Refusal::Unbelieved,
        })
    }

    /// Charge the kept sprite `kept` describes, before its bytes are
    /// fetched.
    ///
    /// # Errors
    ///
    /// [`Refusal::Unbelieved`] when it is longer than is left of the file.
    pub fn claim_kept(&mut self, kept: &EditKept) -> Result<(), Refusal> {
        self.claim(u64::from(kept.length))
    }

    fn claim(&mut self, bytes: u64) -> Result<(), Refusal> {
        self.unclaimed = self
            .unclaimed
            .checked_sub(bytes)
            .ok_or(Refusal::Unbelieved)?;
        Ok(())
    }

    /// Add the picture `built` from `picture`'s rows.
    pub fn picture(&mut self, built: CanvasBuilder, picture: &EditPicture) {
        let sprite = picture.sprite().map(|sprite| SpriteInfo {
            name: sprite.name,
            mode: sprite.mode,
            palette: sprite.palette.clone(),
            masked: sprite.masked,
        });
        self.push(Entry::Picture(Picture {
            canvas: built.finish(),
            sprite,
        }));
    }

    /// Add the sprite `kept` describes as `bytes`, exactly as the file held
    /// them.
    pub fn kept(&mut self, kept: &EditKept, bytes: Vec<u8>) {
        self.push(Entry::Kept(Kept {
            name: kept.name,
            reason: kept.reason,
            bytes: Arc::new(bytes),
        }));
    }

    /// Keep `entry`; one past the count the document declared is not the
    /// document it said it was, so it is dropped and
    /// [`finish`](Self::finish) refuses the whole.
    fn push(&mut self, entry: Entry) {
        if self.entries.len() < self.opened.count as usize {
            self.entries.push(entry);
        } else {
            self.overrun = true;
        }
    }

    /// The document, once it holds every entry it said it would and no more.
    #[must_use]
    pub fn finish(self) -> Option<Document> {
        if self.overrun || self.entries.len() != self.opened.count as usize {
            return None;
        }
        Document::of(
            self.entries,
            Origin::Read(self.opened.format),
            self.opened.unkept,
        )
    }
}

#[cfg(test)]
#[path = "load_tests.rs"]
mod tests;
