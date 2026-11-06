//! Speculative work — the picture as an operation would leave it, the
//! histograms the panes read, the Layers pane's thumbnails and the composite
//! beneath a layer whose opacity is changing — and the overlay what lands is
//! gathered into.
//!
//! It runs on Paint's own preview worker, never on the ordered queue its saves
//! are written on, so no save waits behind it. The worker's queue holds a job
//! running and one waiting for each window, the newest winning
//! ([`supersedes`], [`PREVIEW_ROOM`]). A window's [`Ticket`] is bumped with
//! each sample of a setting and each move of the picture; a job carries the
//! value it was asked under and asks between blocks whether it still holds, so
//! a stale preview is abandoned rather than finished; a job gives way the same
//! way to the queue's work that wants the cores — an Apply's completion, a
//! merge, a flatten — which [`serve`]'s caller says.
//!
//! What lands gathers into an [`Overlay`]: the tiles worked out so far for one
//! operation over one state of the layer, those in view asked first
//! ([`Overlay::next_batch`]). Apply adopts a complete overlay as one step, and
//! an incomplete one is completed on the ordered queue
//! ([`tiled::complete`]).

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use tairix_parallel::JobRunner;
use tairix_util::fallible;

use crate::canvas::{Canvas, OutOfMemory, Tile};
use crate::document::Layer;
use crate::histogram::Histogram;
use crate::mask::Mask;
use crate::op::{Halted, Op};
use crate::shape::Bounds;
use crate::tiled;

/// The room the preview worker's queue holds for each window: a job running,
/// one waiting, and an answer landed but not yet collected.
pub const PREVIEW_ROOM: usize = 3;

/// A window's preview ticket: the one count every job it asks carries.
///
/// Relaxed throughout: the ticket publishes nothing, a stale read costs one
/// block more, and every answer is checked against the ticket on the loop.
#[derive(Debug, Default)]
pub struct Ticket(Arc<AtomicU64>);

impl Ticket {
    /// A ticket no job has been asked under.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Abandon every job asked under the count it held: a setting sampled, or
    /// the picture moved.
    pub fn bump(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    /// What a job asked now carries.
    #[must_use]
    pub fn stamp(&self) -> Stamp {
        Stamp {
            ticket: Arc::clone(&self.0),
            value: self.0.load(Ordering::Relaxed),
        }
    }

    /// Whether `value`, an answer's, is the ticket's still: no sample has
    /// abandoned the job it answers.
    #[must_use]
    pub fn holds(&self, value: u64) -> bool {
        self.0.load(Ordering::Relaxed) == value
    }
}

/// The count of its window's ticket a job was asked under.
#[derive(Debug)]
pub struct Stamp {
    ticket: Arc<AtomicU64>,
    value: u64,
}

impl Stamp {
    /// Whether the ticket still holds it.
    #[must_use]
    pub fn current(&self) -> bool {
        self.ticket.load(Ordering::Relaxed) == self.value
    }

    /// The count.
    #[must_use]
    pub const fn value(&self) -> u64 {
        self.value
    }
}

/// What a preview job works out.
#[derive(Debug)]
pub enum PreviewWork {
    /// Tiles `tiles` of the layer `source` as `op` leaves them, held to `clip`.
    Tiles {
        /// The layer painted on, as it stood.
        source: Canvas,
        /// What it is worked through.
        op: Op,
        /// The selection it is held to, if one is.
        clip: Option<Mask>,
        /// The tiles wanted, the first first.
        tiles: Vec<usize>,
    },
    /// The histogram of `canvas`, held to `clip`.
    Histogram {
        /// The layer painted on, as it stood.
        canvas: Canvas,
        /// The selection it is held to, if one is.
        clip: Option<Mask>,
    },
    /// The Layers pane's thumbnails, each `side` pixels square, of layers
    /// whose pixels are `aspect` across to down.
    LayerThumbnails {
        /// The square's side.
        side: u32,
        /// A pixel's shape, across to down.
        aspect: (u32, u32),
        /// Each layer's revision and its pixels as they stood.
        layers: Vec<(u64, Canvas)>,
    },
    /// The layers beneath the one whose opacity is changing, laid together.
    Under {
        /// The layers, the bottom first, as they stood.
        layers: Vec<Layer>,
    },
}

/// One preview job: the window it is for, the window's own count of it, the
/// stamp it was asked under, and its work.
#[derive(Debug)]
pub struct PreviewJob {
    /// The window that asked.
    pub window: u64,
    /// The window's own count of what it asked, which the answer carries back.
    pub job: u64,
    /// The stamp it was asked under.
    pub stamp: Stamp,
    /// What it works out.
    pub work: PreviewWork,
}

/// Each layer's revision and, where one could be drawn, its thumbnail's
/// pixels.
pub type Thumbnails = Vec<(u64, Option<Vec<u8>>)>;

/// What a preview job came to.
#[derive(Debug)]
pub enum Previewed {
    /// The tiles, each as it would stand.
    Tiles(Result<Vec<(usize, Arc<Tile>)>, Halted>),
    /// The histogram.
    Histogram(Result<Histogram, Halted>),
    /// The Layers pane's thumbnails: each layer's revision and, where one
    /// could be drawn, its thumbnail `side` pixels square of straight-alpha
    /// RGBA8; none for a layer the job was abandoned before, and given up
    /// where that was every layer.
    LayerThumbnails {
        /// The square's side.
        side: u32,
        /// Each layer's revision and its pixels.
        drawn: Result<Thumbnails, Halted>,
    },
    /// The layers beneath, laid together.
    Under(Result<Canvas, Halted>),
}

/// A preview job's answer: the window, the job and the ticket's count it was
/// asked under, and what it came to.
#[derive(Debug)]
pub struct PreviewAnswer {
    /// The window that asked.
    pub window: u64,
    /// The window's own count of the job it answers.
    pub job: u64,
    /// The ticket's count it was asked under.
    pub ticket: u64,
    /// What it came to.
    pub previewed: Previewed,
}

/// Carry out `job` across `runner`: the preview worker's whole work — the
/// tiles a block at a time, a histogram a band of rows, the thumbnails a layer
/// each and the composite a tile each. A job whose stamp no longer holds, or
/// that `yielding` says must give way to the queue's work that wants the
/// cores, stops at the next block, band, thumbnail or tile it would start.
#[must_use]
pub fn serve(
    job: PreviewJob,
    runner: &dyn JobRunner,
    yielding: &(dyn Fn() -> bool + Sync),
) -> PreviewAnswer {
    let PreviewJob {
        window,
        job,
        stamp,
        work,
    } = job;
    let wanted = || stamp.current() && !yielding();
    let previewed = match work {
        PreviewWork::Tiles {
            source,
            op,
            clip,
            tiles,
        } => Previewed::Tiles(tiled::run(
            &source,
            &op,
            clip.as_ref(),
            &tiles,
            runner,
            &wanted,
        )),
        PreviewWork::Histogram { canvas, clip } => {
            Previewed::Histogram(Histogram::of(&canvas, clip.as_ref(), runner, &wanted))
        }
        PreviewWork::LayerThumbnails {
            side,
            aspect,
            layers,
        } => Previewed::LayerThumbnails {
            side,
            drawn: thumbnails(&layers, aspect, side, runner, &wanted),
        },
        PreviewWork::Under { layers } => {
            Previewed::Under(crate::compose::laid_together(&layers, runner, &wanted))
        }
    };
    PreviewAnswer {
        window,
        job,
        ticket: stamp.value(),
        previewed,
    }
}

/// A layer's place among a job's, and its thumbnail once the job reached it.
type Drawn = (usize, Result<Option<Vec<u8>>, Halted>);

/// The thumbnails of `layers`, each `side` pixels square of pixels `aspect`
/// across to down, a layer each across `runner` while `wanted` holds; none for
/// a layer the job was abandoned before.
///
/// # Errors
///
/// [`Halted::Abandoned`] where it was abandoned before every layer, and
/// [`Halted::OutOfMemory`] where their list cannot be held.
fn thumbnails(
    layers: &[(u64, Canvas)],
    aspect: (u32, u32),
    side: u32,
    runner: &dyn JobRunner,
    wanted: &(dyn Fn() -> bool + Sync),
) -> Result<Thumbnails, Halted> {
    let unreached = (0..layers.len()).map(|at| (at, Err(Halted::Abandoned)));
    let mut drawn: Vec<Drawn> =
        fallible::collected(layers.len(), unreached).ok_or(Halted::OutOfMemory)?;
    tairix_parallel::for_each(runner, &mut drawn, &|(at, made): &mut Drawn| {
        if wanted() {
            let canvas = layers.get(*at).map(|(_, canvas)| canvas);
            *made = Ok(canvas
                .and_then(|canvas| crate::thumb::of_canvas(canvas, aspect, side))
                .map(|(pixels, _)| pixels));
        }
    });
    let reached = drawn
        .into_iter()
        .filter_map(|(at, made)| Some((layers.get(at)?.0, made.ok()?)));
    let landed = fallible::collected(layers.len(), reached).ok_or(Halted::OutOfMemory)?;
    if landed.is_empty() && !layers.is_empty() {
        return Err(Halted::Abandoned);
    }
    Ok(landed)
}

/// Whether `job` takes the place of `waiting`, a job not yet taken: each
/// window's newest does.
#[must_use]
pub fn supersedes(job: &PreviewJob, waiting: &PreviewJob) -> bool {
    job.window == waiting.window
}

/// What one tile of an overlay holds.
#[derive(Clone, Debug)]
enum Slot {
    /// The operation does not write it.
    Unwanted,
    /// It is still to be worked out.
    Missing,
    /// As the operation leaves it.
    Done(Arc<Tile>),
}

/// The layer as one operation would leave one state of it, as far as that has
/// been worked out.
#[derive(Debug)]
pub struct Overlay<B> {
    basis: B,
    op: Op,
    slots: Vec<Slot>,
    /// The layer with what has been worked out in place.
    shown: Canvas,
    /// How many tiles the operation writes, and how many of them are still to
    /// be worked out.
    wanted: usize,
    missing: usize,
}

impl<B: Copy + Eq> Overlay<B> {
    /// An overlay of `op` over `source` as it stands at `basis`, held to
    /// `clip`, with nothing yet worked out.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] where its room cannot be had.
    pub fn new(
        basis: B,
        op: Op,
        source: &Canvas,
        clip: Option<&Mask>,
    ) -> Result<Self, OutOfMemory> {
        let wanted = tiled::tiles_for(source, clip)?;
        let mut slots = fallible::filled(source.tile_count(), Slot::Unwanted).ok_or(OutOfMemory)?;
        for &index in &wanted {
            if let Some(slot) = slots.get_mut(index) {
                *slot = Slot::Missing;
            }
        }
        Ok(Self {
            basis,
            op,
            slots,
            shown: source.try_clone()?,
            wanted: wanted.len(),
            missing: wanted.len(),
        })
    }

    /// Whether it shows `op` over the layer as it stands at `basis`.
    #[must_use]
    pub fn shows(&self, basis: B, op: &Op) -> bool {
        self.basis == basis && self.op == *op
    }

    /// The state of the layer it was made over.
    #[must_use]
    pub fn basis(&self) -> B {
        self.basis
    }

    /// The operation it shows.
    #[must_use]
    pub const fn op(&self) -> &Op {
        &self.op
    }

    /// Whether every tile the operation writes has been worked out.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.missing == 0
    }

    /// An overlay of `op` over the same state of the layer, still showing what
    /// this one shows until its own tiles land: what a sample of a setting
    /// starts from, so the picture keeps the last preview rather than falling
    /// back to the layer between samples, and nothing is allocated.
    #[must_use]
    pub fn followed_by(mut self, op: Op) -> Self {
        for slot in &mut self.slots {
            if matches!(slot, Slot::Done(_)) {
                *slot = Slot::Missing;
            }
        }
        self.op = op;
        self.missing = self.wanted;
        self
    }

    /// Take in tiles a job worked out, each put in place over the layer; one
    /// it does not want, holds already, or that does not fit is passed over.
    pub fn land(&mut self, tiles: Vec<(usize, Arc<Tile>)>) {
        for (index, tile) in tiles {
            let Some(slot) = self.slots.get_mut(index) else {
                continue;
            };
            if matches!(slot, Slot::Missing) && self.shown.fits(index, &tile) {
                self.shown.replace_tile(index, Arc::clone(&tile));
                *slot = Slot::Done(tile);
                self.missing = self.missing.saturating_sub(1);
            }
        }
    }

    /// The tiles to ask for next: the missing ones meeting `visible`, else
    /// every missing one, in reading order.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] where the list cannot be held.
    pub fn next_batch(&self, visible: Bounds) -> Result<Vec<usize>, OutOfMemory> {
        let missing = |index: &usize| matches!(self.slots.get(*index), Some(Slot::Missing));
        let seen = |index: &usize| {
            let bounds = self.shown.tile_rect(*index).bounds();
            !bounds.intersection(&visible).is_empty()
        };
        let every = 0..self.slots.len();
        let shown = every.clone().filter(missing).filter(seen).count();
        let batch = if shown > 0 {
            fallible::collected(shown, every.filter(missing).filter(seen))
        } else {
            fallible::collected(self.missing, every.filter(missing))
        };
        batch.ok_or(OutOfMemory)
    }

    /// The layer with what has been worked out in place: what the paint
    /// draws.
    #[must_use]
    pub const fn shown(&self) -> &Canvas {
        &self.shown
    }

    /// The tiles worked out, in reading order: what Apply adopts once the
    /// overlay is complete, and what the ordered queue completes it from
    /// otherwise.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] where the list cannot be held.
    pub fn done(&self) -> Result<Vec<(usize, Arc<Tile>)>, OutOfMemory> {
        let done = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| match slot {
                Slot::Done(tile) => Some((index, Arc::clone(tile))),
                Slot::Unwanted | Slot::Missing => None,
            });
        fallible::collected(self.wanted.saturating_sub(self.missing), done).ok_or(OutOfMemory)
    }
}

#[cfg(test)]
#[path = "preview_tests.rs"]
mod tests;
