//! Assembling a document from what the sandboxed decoder answered.
//!
//! Every answer has already been held to the edit decode's bounds by the
//! seam that received it; this turns them into canvases and entries, and
//! refuses a document whose answers do not add up. A layered document's
//! layers become the layers of its one picture, each laid on a canvas of the
//! picture's size where the file placed it.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_sandbox::imageedit::{EditDocument, EditKept, EditKind, EditPicture, EditPixels};

use crate::canvas::{CanvasBuilder, CanvasError, Kind, Sample};
use crate::document::{Document, Entry, Kept, Layer, Origin, Picture, SpriteInfo, MOST_LAYERS};
use crate::save::SaveSettings;
use crate::shape::Bounds;

/// Why an entry could not join the document.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// What the entry needs could not be held.
    NoMemory,
    /// What the decoder described does not add up: more entries than the
    /// document declared, or sprites needing more of the file than it holds.
    Unbelieved,
}

/// A picture's rows being written into its canvas: where they land, and how
/// long each must be.
#[derive(Debug)]
pub struct Rows {
    built: CanvasBuilder,
    at: (i64, i64),
    width: usize,
}

impl Rows {
    /// Write the picture's row `y`, as [`CanvasBuilder::row`] takes one: a
    /// row of another length is ignored.
    pub fn row(&mut self, y: u32, samples: &[u8], mask: &[u8]) {
        if samples.len() == self.width {
            self.built
                .row_at((self.at.0, self.at.1 + i64::from(y)), samples, mask);
        }
    }
}

/// A document being put together, one entry at a time.
#[derive(Debug)]
pub struct Assembly {
    entries: Vec<Entry>,
    /// A layered document's layers so far, the bottom first: its one entry
    /// once they are all in.
    layers: Vec<Layer>,
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
    /// [`Refusal`]: the room is refused, or a layered document has more
    /// layers than a picture holds or no canvas to lay them on.
    pub fn new(opened: EditDocument, length: usize) -> Result<Self, Refusal> {
        let count = opened.count as usize;
        let layered = opened.kind == EditKind::Layers;
        if layered && (count > MOST_LAYERS || opened.canvas.is_none()) {
            return Err(Refusal::Unbelieved);
        }
        let (mut entries, mut layers) = (Vec::new(), Vec::new());
        let reserved = if layered {
            entries
                .try_reserve_exact(1)
                .and(layers.try_reserve_exact(count))
        } else {
            entries.try_reserve_exact(count)
        };
        reserved.map_err(|_| Refusal::NoMemory)?;
        Ok(Self {
            entries,
            layers,
            opened,
            unclaimed: u64::try_from(length).unwrap_or(u64::MAX),
            overrun: false,
        })
    }

    /// A builder for the rows of `picture`, which [`picture`](Self::picture)
    /// takes once they are all in. A sprite is first charged the least of
    /// the file it could be stored in; a layer is laid where its document
    /// places it on its canvas.
    ///
    /// # Errors
    ///
    /// [`Refusal`]: the sprite needs more of the file than is left, a layer
    /// arrives other than as a layered document's, or the picture cannot be
    /// held.
    pub fn canvas_for(&mut self, picture: &EditPicture) -> Result<Rows, Refusal> {
        let canvas = match (self.opened.canvas, picture.layer()) {
            (Some(canvas), Some(layer)) if self.opened.kind == EditKind::Layers => {
                Some((canvas, layer.at))
            }
            (None, None) if self.opened.kind != EditKind::Layers => None,
            _ => return Err(Refusal::Unbelieved),
        };
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
        let refused = |err| match err {
            CanvasError::OutOfMemory => Refusal::NoMemory,
            CanvasError::BadSize | CanvasError::BadPalette => Refusal::Unbelieved,
        };
        let width = picture.width() as usize * picture.pixels().sample_bytes();
        let Some(((across, down), (x, y))) = canvas else {
            let built = CanvasBuilder::new(picture.width(), picture.height(), kind, fill)
                .map_err(refused)?;
            return Ok(Rows {
                built,
                at: (0, 0),
                width,
            });
        };
        let at = (i64::from(x), i64::from(y));
        let area = Bounds {
            x0: at.0,
            y0: at.1,
            x1: at.0 + i64::from(picture.width()),
            y1: at.1 + i64::from(picture.height()),
        };
        let built = CanvasBuilder::within(across, down, kind, fill, area).map_err(refused)?;
        Ok(Rows { built, at, width })
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

    /// Add the picture `rows` were built into from `picture`'s; a layer
    /// joins those of the document's one picture.
    ///
    /// # Errors
    ///
    /// [`Refusal::NoMemory`] when a layer's name cannot be held.
    pub fn picture(&mut self, rows: Rows, picture: &EditPicture) -> Result<(), Refusal> {
        let canvas = rows.built.finish();
        if let Some(layer) = picture.layer() {
            if self.layers.len() >= self.opened.count as usize {
                self.overrun = true;
                return Ok(());
            }
            let mut name = String::new();
            name.try_reserve_exact(layer.name.len())
                .map_err(|_| Refusal::NoMemory)?;
            name.push_str(&layer.name);
            self.layers.push(Layer {
                canvas,
                name,
                opacity: layer.opacity,
                visible: layer.visible,
            });
            return Ok(());
        }
        let sprite = picture.sprite().map(|sprite| SpriteInfo {
            name: sprite.name,
            mode: sprite.mode,
            palette: sprite.palette.clone(),
            masked: sprite.masked,
        });
        let mut made = Picture::plain(canvas);
        made.sprite = sprite;
        made.density = picture.density();
        self.push(Entry::Picture(made));
        Ok(())
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
    pub fn finish(mut self) -> Option<Document> {
        if self.opened.kind == EditKind::Layers {
            if self.overrun || self.layers.len() != self.opened.count as usize {
                return None;
            }
            // The topmost layer is the one painted on, as an editor opens one.
            let top = self.layers.len().checked_sub(1)?;
            let picture = Picture::layered(core::mem::take(&mut self.layers), top)?;
            self.entries.push(Entry::Picture(picture));
        } else if self.overrun || self.entries.len() != self.opened.count as usize {
            return None;
        }
        Document::of(
            self.entries,
            Origin::Read(self.opened.format),
            self.opened.unkept,
            SaveSettings::as_written(self.opened.written),
        )
    }
}

#[cfg(test)]
#[path = "load_tests.rs"]
mod tests;
