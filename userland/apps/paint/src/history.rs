//! Undo and redo.
//!
//! Every step is a swap: undoing one puts its contents in the document and
//! takes what they replace, which is then the step that redoes it. What a
//! step holds is only what left the document, so the bytes charged to the
//! history are exactly the bytes nothing else holds.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_image::Rgba8;
use tairix_reclaim::PressureBand;

use crate::canvas::{charge, OutOfMemory, Tile};
use crate::document::{Entry, Layer, Picture, Shown, SpriteInfo};
use crate::shape::Bounds;

/// One undoable change.
#[derive(Debug)]
pub enum Step {
    /// Tiles of layer `layer` of entry `entry` as they stood, each with
    /// its index.
    Tiles {
        /// The entry written.
        entry: usize,
        /// The layer of it written.
        layer: usize,
        /// The tiles to put back.
        tiles: Vec<(usize, Arc<Tile>)>,
    },
    /// Entry `entry`'s whole picture as it stood.
    Picture {
        /// The entry replaced.
        entry: usize,
        /// The picture to put back.
        picture: Picture,
    },
    /// Entry `entry`'s palette and sprite details as they stood.
    Details {
        /// The entry changed.
        entry: usize,
        /// The palette to put back, for a palette picture.
        palette: Option<Vec<Rgba8>>,
        /// The sprite details to put back.
        sprite: Option<SpriteInfo>,
    },
    /// An entry was added at `index`.
    Inserted {
        /// Where.
        index: usize,
    },
    /// `entry` was removed from `index`.
    Removed {
        /// Where it was.
        index: usize,
        /// The entry.
        entry: Entry,
    },
    /// The entry at `from` was moved to `to`.
    Moved {
        /// Where it was.
        from: usize,
        /// Where it went.
        to: usize,
    },
    /// A layer was added to entry `entry` at `layer`.
    LayerInserted {
        /// The entry.
        entry: usize,
        /// Where.
        layer: usize,
    },
    /// `removed` was taken from entry `entry`'s layers at `layer`.
    LayerRemoved {
        /// The entry.
        entry: usize,
        /// Where it was.
        layer: usize,
        /// The layer.
        removed: Layer,
    },
    /// Entry `entry`'s layer at `from` was moved to `to`.
    LayerMoved {
        /// The entry.
        entry: usize,
        /// Where it was.
        from: usize,
        /// Where it went.
        to: usize,
    },
    /// How entry `entry`'s layer `layer` showed.
    LayerShown {
        /// The entry.
        entry: usize,
        /// The layer.
        layer: usize,
        /// How it showed.
        shown: Shown,
    },
}

/// What applying a step changed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Applied {
    /// The entry to show.
    pub entry: usize,
    /// What of it changed.
    pub damage: Damage,
}

/// What of an entry a change reached.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Damage {
    /// These pixels.
    Area(Bounds),
    /// The whole picture, which may have changed size or kind.
    Whole,
    /// The layers — how many, their order, how each shows, or which is
    /// painted on — the picture's size and kind kept.
    Layers,
    /// The list of entries itself.
    List,
}

impl Step {
    /// Bytes this step alone holds.
    fn bytes(&self) -> usize {
        match self {
            Self::Tiles { tiles, .. } => tiles.iter().map(|(_, tile)| charge(tile)).sum(),
            Self::Picture { picture, .. } => picture.charged_bytes(),
            Self::Details { palette, .. } => {
                palette.as_ref().map_or(0, Vec::len) * core::mem::size_of::<Rgba8>()
            }
            Self::Removed { entry, .. } => entry.charged_bytes(),
            Self::LayerRemoved { removed, .. } => {
                removed.canvas.charged_bytes() + removed.name.len()
            }
            Self::LayerShown { shown, .. } => shown.name.len(),
            Self::Inserted { .. }
            | Self::Moved { .. }
            | Self::LayerInserted { .. }
            | Self::LayerMoved { .. } => 0,
        }
    }

    /// Hold the room putting this step into `entries` takes, so that applying
    /// it cannot then fail for memory.
    fn reserve(&self, entries: &mut Vec<Entry>) -> Result<(), OutOfMemory> {
        match self {
            Self::Removed { .. } => entries.try_reserve(1).map_err(|_| OutOfMemory),
            Self::LayerRemoved { entry, .. } => match entries.get_mut(*entry) {
                Some(Entry::Picture(picture)) => picture.reserve_layer(),
                _ => Ok(()),
            },
            _ => Ok(()),
        }
    }

    /// Put this step into `entries`, its room held already, answering the
    /// step that takes it back out and what it changed; `None` for a step
    /// that no longer fits them, which is dropped.
    fn apply(self, entries: &mut Vec<Entry>) -> Option<(Self, Applied)> {
        let whole = |entry| Applied {
            entry,
            damage: Damage::Whole,
        };
        let list = |entry| Applied {
            entry,
            damage: Damage::List,
        };
        match self {
            Self::Tiles {
                entry,
                layer,
                tiles,
            } => swap_tiles(entries, (entry, layer), tiles),
            Self::Picture { entry, picture } => {
                let held = picture_of(entries, entry)?;
                let picture = core::mem::replace(held, picture);
                Some((Self::Picture { entry, picture }, whole(entry)))
            }
            Self::Details {
                entry,
                palette,
                sprite,
            } => {
                let held = picture_of(entries, entry)?;
                let palette = match palette {
                    Some(palette) => Some(held.canvas_mut().swap_palette(palette)?),
                    None => None,
                };
                let sprite = core::mem::replace(&mut held.sprite, sprite);
                let step = Self::Details {
                    entry,
                    palette,
                    sprite,
                };
                Some((step, whole(entry)))
            }
            Self::Inserted { index } => {
                if index >= entries.len() || entries.len() == 1 {
                    return None;
                }
                let entry = entries.remove(index);
                let shown = index.min(entries.len() - 1);
                Some((Self::Removed { index, entry }, list(shown)))
            }
            Self::Removed { index, entry } => {
                if index > entries.len() {
                    return None;
                }
                entries.insert(index, entry);
                Some((Self::Inserted { index }, list(index)))
            }
            Self::Moved { from, to } => {
                if from >= entries.len() || to >= entries.len() {
                    return None;
                }
                let entry = entries.remove(from);
                entries.insert(to, entry);
                Some((Self::Moved { from: to, to: from }, list(to)))
            }
            step => step.apply_to_layers(entries),
        }
    }

    /// Put this step, one changing a picture's layers, into `entries`, as
    /// [`apply`](Self::apply) does.
    fn apply_to_layers(self, entries: &mut [Entry]) -> Option<(Self, Applied)> {
        let layers = |entry| Applied {
            entry,
            damage: Damage::Layers,
        };
        match self {
            Self::LayerInserted { entry, layer } => {
                let removed = picture_of(entries, entry)?.remove_layer(layer).ok()?;
                let step = Self::LayerRemoved {
                    entry,
                    layer,
                    removed,
                };
                Some((step, layers(entry)))
            }
            Self::LayerRemoved {
                entry,
                layer,
                removed,
            } => {
                picture_of(entries, entry)?
                    .insert_layer(layer, removed)
                    .ok()?;
                Some((Self::LayerInserted { entry, layer }, layers(entry)))
            }
            Self::LayerMoved { entry, from, to } => {
                picture_of(entries, entry)?.move_layer(from, to).ok()?;
                let step = Self::LayerMoved {
                    entry,
                    from: to,
                    to: from,
                };
                Some((step, layers(entry)))
            }
            Self::LayerShown {
                entry,
                layer,
                shown,
            } => {
                let shown = picture_of(entries, entry)?.show_layer(layer, shown).ok()?;
                let step = Self::LayerShown {
                    entry,
                    layer,
                    shown,
                };
                Some((step, layers(entry)))
            }
            _ => None,
        }
    }
}

/// The picture entry `entry` holds, unless it is kept as its bytes.
fn picture_of(entries: &mut [Entry], entry: usize) -> Option<&mut Picture> {
    match entries.get_mut(entry) {
        Some(Entry::Picture(picture)) => Some(picture),
        _ => None,
    }
}

/// Why an undo or a redo changed nothing.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Unapplied {
    /// There was nothing to take back or put back.
    Nothing,
    /// The memory for what it hands back was refused: the step waits where
    /// it was, and the document is as it was.
    NoMemory,
    /// The step no longer fitted the document, which a history that records
    /// every change never leaves, and was dropped.
    Stale,
}

/// Swap `tiles` into layer `layer` of entry `entry`, each exchanged in place
/// for the tile it replaces, so the step that takes them back out needs no
/// memory of its own. Tiles that do not fit their slots, every one checked
/// first, change nothing.
fn swap_tiles(
    entries: &mut [Entry],
    (entry, layer): (usize, usize),
    mut tiles: Vec<(usize, Arc<Tile>)>,
) -> Option<(Step, Applied)> {
    let picture = picture_of(entries, entry)?;
    let painted = picture.active();
    let canvas = &mut picture.layers_mut().get_mut(layer)?.canvas;
    if !tiles.iter().all(|(index, tile)| canvas.fits(*index, tile)) {
        return None;
    }
    let mut bounds: Option<Bounds> = None;
    for (index, tile) in &mut tiles {
        let area = canvas.tile_rect(*index).bounds();
        bounds = Some(bounds.map_or(area, |held| held.union(&area)));
        *tile = canvas.replace_tile(*index, Arc::clone(tile));
    }
    // A change to another layer than the one painted on paints on it again.
    let damage = match bounds {
        Some(area) if painted == layer => Damage::Area(area),
        _ => Damage::Layers,
    };
    picture.set_active(layer);
    let step = Step::Tiles {
        entry,
        layer,
        tiles,
    };
    Some((step, Applied { entry, damage }))
}

/// A step and the bytes it was charged when it was made.
#[derive(Debug)]
struct Held {
    step: Step,
    bytes: usize,
}

/// The history of one document.
#[derive(Debug)]
pub struct History {
    undo: VecDeque<Held>,
    redo: Vec<Held>,
    /// How many steps deep the undo stack was when the document last matched
    /// its file; `None` once that state can no longer be reached.
    saved: Option<usize>,
    bytes: usize,
    /// The memory-pressure band last told, which bounds what the history
    /// holds beside the document from then on.
    band: PressureBand,
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    /// An empty history of a document that matches its file.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            undo: VecDeque::new(),
            redo: Vec::new(),
            saved: Some(0),
            bytes: 0,
            band: PressureBand::Normal,
        }
    }

    /// Make room for the next step, so that a change made once this has
    /// answered can always be recorded. Memory refused costs the history
    /// first — every redo, then the oldest steps — before the change.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when nothing is left to give up and the stack still
    /// cannot grow: the change should not be made.
    pub fn reserve(&mut self) -> Result<(), OutOfMemory> {
        while self.undo.try_reserve(1).is_err() {
            if !self.redo.is_empty() {
                self.drop_redo();
            } else if !self.forget_oldest() {
                return Err(OutOfMemory);
            }
        }
        Ok(())
    }

    /// Record `step`: undoing it puts it back. Every redo is forgotten, and
    /// under memory pressure the oldest steps go until the history fits what
    /// the band allows beside a document of `document_bytes`.
    ///
    /// A step recorded without room reserved for it, and refused, leaves the
    /// document changed with nothing to name the change: its file's state can
    /// then no longer be told apart, and the document reads as changed.
    pub fn record(&mut self, step: Step, document_bytes: impl FnOnce() -> usize) {
        self.drop_redo();
        if self.reserve().is_err() {
            self.saved = None;
            return;
        }
        let bytes = step.bytes();
        self.bytes += bytes;
        self.undo.push_back(Held { step, bytes });
        if let Some(allowed) = allowance(self.band, document_bytes()) {
            self.trim(allowed);
        }
    }

    /// Forget the oldest step, answering whether there was one.
    fn forget_oldest(&mut self) -> bool {
        let Some(oldest) = self.undo.pop_front() else {
            return false;
        };
        self.bytes = self.bytes.saturating_sub(oldest.bytes);
        self.saved = self.saved.and_then(|depth| depth.checked_sub(1));
        true
    }

    /// Forget every redo, then the oldest steps, until the history holds no
    /// more than `allowed`.
    fn trim(&mut self, allowed: usize) {
        self.drop_redo();
        // What a step alone holds grows as what shared its tiles lets go, so
        // each is charged again before the total is believed.
        for held in &mut self.undo {
            held.bytes = held.step.bytes();
        }
        self.bytes = self.undo.iter().map(|held| held.bytes).sum();
        while self.bytes > allowed && self.forget_oldest() {}
    }

    /// Forget every redo, and the file's state with them if it lay there.
    fn drop_redo(&mut self) {
        if self.saved.is_some_and(|depth| depth > self.undo.len()) {
            self.saved = None;
        }
        let freed: usize = self.redo.iter().map(|held| held.bytes).sum();
        self.bytes = self.bytes.saturating_sub(freed);
        self.redo.clear();
    }

    /// Undo the newest step in `entries`, answering what it changed.
    ///
    /// # Errors
    ///
    /// [`Unapplied`]: nothing to undo, or no memory for it — the document and
    /// both stacks unchanged.
    pub fn undo(&mut self, entries: &mut Vec<Entry>) -> Result<Applied, Unapplied> {
        self.redo.try_reserve(1).map_err(|_| Unapplied::NoMemory)?;
        let held = self.undo.back().ok_or(Unapplied::Nothing)?;
        held.step
            .reserve(entries)
            .map_err(|OutOfMemory| Unapplied::NoMemory)?;
        let held = self.undo.pop_back().ok_or(Unapplied::Nothing)?;
        let (inverse, applied) = self.swap(held, entries)?;
        self.redo.push(inverse);
        Ok(applied)
    }

    /// Redo the newest undone step in `entries`, answering what it changed.
    ///
    /// # Errors
    ///
    /// [`Unapplied`], as [`undo`](Self::undo) answers it.
    pub fn redo(&mut self, entries: &mut Vec<Entry>) -> Result<Applied, Unapplied> {
        self.undo.try_reserve(1).map_err(|_| Unapplied::NoMemory)?;
        let held = self.redo.last().ok_or(Unapplied::Nothing)?;
        held.step
            .reserve(entries)
            .map_err(|OutOfMemory| Unapplied::NoMemory)?;
        let held = self.redo.pop().ok_or(Unapplied::Nothing)?;
        let (inverse, applied) = self.swap(held, entries)?;
        self.undo.push_back(inverse);
        Ok(applied)
    }

    /// Apply `held`, its room held already, answering its inverse charged at
    /// what it holds. A step that no longer applies is lost, and the file's
    /// state with it, since the depth it was saved at no longer names it.
    fn swap(&mut self, held: Held, entries: &mut Vec<Entry>) -> Result<(Held, Applied), Unapplied> {
        self.bytes = self.bytes.saturating_sub(held.bytes);
        let Some((step, applied)) = held.step.apply(entries) else {
            self.saved = None;
            return Err(Unapplied::Stale);
        };
        let bytes = step.bytes();
        self.bytes += bytes;
        Ok((Held { step, bytes }, applied))
    }

    /// Whether there is anything to undo.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether there is anything to redo.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Whether the document differs from its file.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.saved != Some(self.undo.len())
    }

    /// The document now matches its file.
    pub fn mark_saved(&mut self) {
        self.saved = Some(self.undo.len());
    }

    /// The file now holds a state this history can no longer name.
    pub fn forget_saved(&mut self) {
        self.saved = None;
    }

    /// Bytes the history alone holds.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    /// Hold the history from now on to what `band` allows beside a
    /// document of `document_bytes` — the oldest steps forgotten first, and
    /// every redo once memory is short at all: what pressure costs the
    /// history before it costs the document.
    pub fn adopt_pressure(&mut self, band: PressureBand, document_bytes: usize) {
        self.band = band;
        if let Some(allowed) = allowance(band, document_bytes) {
            self.trim(allowed);
        }
    }

    /// How many steps can be undone.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.undo.len()
    }
}

/// What the history may hold at `band` beside a document of `document_bytes`:
/// all it likes at normal pressure, twice the document at mild, once at
/// moderate, and nothing beyond.
const fn allowance(band: PressureBand, document_bytes: usize) -> Option<usize> {
    match band {
        PressureBand::Normal => None,
        PressureBand::Mild => Some(document_bytes.saturating_mul(2)),
        PressureBand::Moderate => Some(document_bytes),
        PressureBand::Severe | PressureBand::Critical => Some(0),
    }
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
